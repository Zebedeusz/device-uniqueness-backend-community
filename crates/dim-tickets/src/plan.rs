//! Pure batch-planning core for the dim-tickets writer — a faithful port of
//! the legacy `planBatchProcessing` / `computeRetryDelay` /
//! `parseForceBatchResults` decisions.
//!
//! Semantics preserved exactly:
//! - **Exhausted first**: `retry_count >= max_retries` → mark `FAILED`,
//!   regardless of status.
//! - **Orphans are recovered AND re-batched**: a due `SUBMITTING`/`SUBMITTED`
//!   row (its `retry_at` lease expired — a crashed or errored earlier
//!   submission) is reset to `PENDING` and included in this tick's batch.
//! - **One lease per batch**: `retry_at = now + delay(1 + max retry_count in
//!   batch)`. While the submission is in flight the rows are not due; if the
//!   writer dies the lease expires and the rows come back as orphans.
//! - **Backoff is un-jittered**: `min(base · 2^min(attempt, max_exponent), max)`.
//! - **`force_batch` item mapping is positional**: the chain emits one
//!   `Utility.ItemCompleted` / `Utility.ItemFailed` per inner call in call
//!   order; no events at all means the whole batch failed.

use crate::tickets::Dim;

/// A due row as the planner sees it (already decoded; malformed rows are
/// logged and skipped upstream, as legacy did).
#[derive(Debug, Clone)]
pub struct DueTicket {
    /// SS58 address (the row key and the on-chain ticket).
    pub ticket: String,
    /// Which DIM pallet the ticket registers under.
    pub dim: Dim,
    /// Whether the row was in flight (`SUBMITTING`/`SUBMITTED`) when fetched.
    pub in_flight: bool,
    /// Failed submission attempts so far.
    pub retry_count: i32,
}

/// The tick's plan: three disjoint actions in execution order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BatchPlan {
    /// Tickets whose retries are exhausted → `FAILED`.
    pub exhausted: Vec<String>,
    /// Orphaned in-flight tickets → reset to `PENDING` (then re-batched).
    pub recovered: Vec<String>,
    /// The submission batch, in fetch order (= on-chain call order).
    pub batch: Vec<(String, Dim)>,
    /// Lease delay for the batch's `retry_at`, ms from now.
    pub lease_delay_ms: u64,
}

/// The legacy exponential backoff: `min(base · 2^min(attempt, max_exponent), max)`.
pub fn retry_delay_ms(base_ms: u64, max_ms: u64, max_exponent: u32) -> impl Fn(i32) -> u64 + Copy {
    move |attempt: i32| {
        let exponent = attempt.clamp(0, max_exponent as i32) as u32;
        base_ms
            .saturating_mul(2u64.saturating_pow(exponent))
            .min(max_ms)
    }
}

/// Plan one writer tick over the due rows (fetch order preserved).
pub fn plan_batch(
    tickets: &[DueTicket],
    max_retries: i32,
    delay_ms: impl Fn(i32) -> u64,
) -> BatchPlan {
    let mut plan = BatchPlan::default();
    let mut max_retry_count = 0;

    for ticket in tickets {
        if ticket.retry_count >= max_retries {
            plan.exhausted.push(ticket.ticket.clone());
            continue;
        }
        if ticket.in_flight {
            plan.recovered.push(ticket.ticket.clone());
        }
        max_retry_count = max_retry_count.max(ticket.retry_count);
        plan.batch.push((ticket.ticket.clone(), ticket.dim));
    }

    if !plan.batch.is_empty() {
        plan.lease_delay_ms = delay_ms(max_retry_count + 1);
    }
    plan
}

/// Positional split of a finalized `force_batch`: `items` is the ordered
/// sequence of `Utility.ItemCompleted` (`true`) / `Utility.ItemFailed`
/// (`false`) events; index = inner-call index.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BatchSplit {
    /// Inner-call indices that completed.
    pub completed: Vec<usize>,
    /// Inner-call indices that failed.
    pub failed: Vec<usize>,
    /// Data-mismatch warnings (event/call count disagreement).
    pub warnings: Vec<String>,
}

/// Split `force_batch` item events into completed/failed call indices.
pub fn split_batch_items(items: &[bool], call_count: usize) -> BatchSplit {
    if items.is_empty() {
        return BatchSplit {
            completed: Vec::new(),
            failed: (0..call_count).collect(),
            warnings: vec!["No utility events found in transaction".to_string()],
        };
    }

    let mut split = BatchSplit::default();
    for (index, completed) in items.iter().enumerate() {
        if *completed {
            split.completed.push(index);
        } else {
            split.failed.push(index);
        }
    }
    if items.len() != call_count {
        split.warnings.push(format!(
            "Event count mismatch: expected {call_count} calls, found {} events",
            items.len()
        ));
    }
    split
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(ticket: &str, in_flight: bool, retry_count: i32) -> DueTicket {
        DueTicket {
            ticket: ticket.to_string(),
            dim: Dim::Game,
            in_flight,
            retry_count,
        }
    }

    const DELAY: fn(i32) -> u64 = |attempt| 1000 * (attempt as u64 + 100);

    #[test]
    fn backoff_doubles_and_caps() {
        let delay = retry_delay_ms(1_000, 60_000, 10);
        assert_eq!(delay(0), 1_000);
        assert_eq!(delay(1), 2_000);
        assert_eq!(delay(5), 32_000);
        assert_eq!(delay(6), 60_000); // 64s capped at max
        assert_eq!(delay(100), 60_000); // exponent clamped, then capped
        assert_eq!(delay(-1), 1_000); // negative clamps to 0
    }

    #[test]
    fn exhausted_tickets_leave_the_batch() {
        let plan = plan_batch(&[due("a", false, 5), due("b", false, 0)], 5, DELAY);
        assert_eq!(plan.exhausted, vec!["a"]);
        assert!(plan.recovered.is_empty());
        assert_eq!(plan.batch, vec![("b".to_string(), Dim::Game)]);
    }

    #[test]
    fn orphans_are_recovered_and_rebatched_in_fetch_order() {
        let plan = plan_batch(
            &[due("a", false, 0), due("b", true, 2), due("c", false, 1)],
            5,
            DELAY,
        );
        assert_eq!(plan.recovered, vec!["b"]);
        // Fetch order preserved — this IS the on-chain call order.
        let batch: Vec<&str> = plan.batch.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(batch, vec!["a", "b", "c"]);
    }

    #[test]
    fn lease_uses_max_retry_count_in_batch_plus_one() {
        let plan = plan_batch(&[due("a", false, 0), due("b", true, 3)], 5, DELAY);
        assert_eq!(plan.lease_delay_ms, DELAY(4));
    }

    #[test]
    fn exhausted_orphan_fails_without_recovery() {
        let plan = plan_batch(&[due("a", true, 5)], 5, DELAY);
        assert_eq!(plan.exhausted, vec!["a"]);
        assert!(plan.recovered.is_empty());
        assert!(plan.batch.is_empty());
        assert_eq!(plan.lease_delay_ms, 0);
    }

    #[test]
    fn splits_items_positionally() {
        let split = split_batch_items(&[true, false, true], 3);
        assert_eq!(split.completed, vec![0, 2]);
        assert_eq!(split.failed, vec![1]);
        assert!(split.warnings.is_empty());
    }

    #[test]
    fn no_events_means_whole_batch_failed() {
        let split = split_batch_items(&[], 3);
        assert!(split.completed.is_empty());
        assert_eq!(split.failed, vec![0, 1, 2]);
        assert_eq!(split.warnings.len(), 1);
    }

    #[test]
    fn count_mismatch_is_flagged() {
        let split = split_batch_items(&[true, true], 3);
        assert_eq!(split.completed, vec![0, 1]);
        assert!(split.warnings[0].contains("expected 3 calls, found 2 events"));
    }
}
