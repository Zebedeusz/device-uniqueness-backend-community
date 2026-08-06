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

use std::collections::HashMap;
use std::str::FromStr as _;

use subxt::utils::AccountId32;

use crate::tickets::{Dim, DueRow};

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

/// Decode fetched due rows into planner input. Malformed rows (unknown `dim`
/// literal) are returned separately as `(ticket, stored dim)` pairs for the
/// caller to log and skip, as legacy did.
pub fn decode_due(rows: &[DueRow]) -> (Vec<DueTicket>, Vec<(String, String)>) {
    let mut due = Vec::with_capacity(rows.len());
    let mut skipped = Vec::new();
    for row in rows {
        match Dim::from_str(&row.dim) {
            Ok(dim) => due.push(DueTicket {
                ticket: row.ticket.clone(),
                dim,
                in_flight: row.status == "SUBMITTING" || row.status == "SUBMITTED",
                retry_count: row.retry_count,
            }),
            Err(()) => skipped.push((row.ticket.clone(), row.dim.clone())),
        }
    }
    (due, skipped)
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

/// Parse the planned batch's SS58 keys. A row whose key is not valid SS58 can
/// never submit; it is returned in the second list as `(ticket, error)` for
/// the caller to fail terminally. Both lists preserve batch order.
#[allow(clippy::type_complexity)]
pub fn parse_batch(
    batch: &[(String, Dim)],
) -> (Vec<(String, AccountId32, Dim)>, Vec<(String, String)>) {
    let mut parsed = Vec::with_capacity(batch.len());
    let mut unparseable = Vec::new();
    for (ticket, dim) in batch {
        match AccountId32::from_str(ticket) {
            Ok(account) => parsed.push((ticket.clone(), account, *dim)),
            Err(e) => unparseable.push((ticket.clone(), e.to_string())),
        }
    }
    (parsed, unparseable)
}

/// Re-join the submission batch onto its due rows' retry counts, by ticket.
///
/// Invariant: every batch ticket came out of `due` ([`plan_batch`] and
/// [`parse_batch`] only ever select from it), so a miss is impossible — it is
/// debug-asserted, and release builds preserve the legacy runtime default
/// of 0.
pub fn batch_retry_counts(tickets: &[String], due: &[DueTicket]) -> Vec<i32> {
    let mut counts: HashMap<&str, i32> = HashMap::with_capacity(due.len());
    for ticket in due {
        // First occurrence wins, like the legacy linear `find`.
        counts
            .entry(ticket.ticket.as_str())
            .or_insert(ticket.retry_count);
    }
    tickets
        .iter()
        .map(|ticket| {
            let count = counts.get(ticket.as_str()).copied();
            debug_assert!(
                count.is_some(),
                "batch ticket {ticket} missing from due rows"
            );
            count.unwrap_or(0)
        })
        .collect()
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

/// A failed batch item's re-queue: its incremented count and backoff delay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    /// SS58 address (the row key).
    pub ticket: String,
    /// The incremented retry count to store.
    pub retry_count: i32,
    /// Backoff delay for the item's `retry_at`, ms from now.
    pub delay_ms: u64,
}

/// Project a positional [`BatchSplit`] back onto the batch's tickets:
/// completed indices become registered tickets, failed indices become
/// [`Retry`]s with incremented counts and their backoff delays. Out-of-range
/// indices (an event/call mismatch, already warned by [`split_batch_items`])
/// are skipped, as legacy did.
pub fn apply_split(
    split: &BatchSplit,
    tickets: &[String],
    retry_counts: &[i32],
    delay_ms: impl Fn(i32) -> u64,
) -> (Vec<String>, Vec<Retry>) {
    debug_assert_eq!(tickets.len(), retry_counts.len());
    let registered = split
        .completed
        .iter()
        .filter_map(|&i| tickets.get(i).cloned())
        .collect();
    let retries = split
        .failed
        .iter()
        .filter_map(|&i| {
            let ticket = tickets.get(i)?.clone();
            let retry_count = retry_counts.get(i).copied().unwrap_or(0) + 1;
            Some(Retry {
                ticket,
                retry_count,
                delay_ms: delay_ms(retry_count),
            })
        })
        .collect();
    (registered, retries)
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

    fn due_row(ticket: &str, dim: &str, status: &str, retry_count: i32) -> DueRow {
        DueRow {
            ticket: ticket.to_string(),
            dim: dim.to_string(),
            status: status.to_string(),
            retry_count,
        }
    }

    #[test]
    fn decode_maps_in_flight_statuses_and_keeps_fetch_order() {
        let (due, skipped) = decode_due(&[
            due_row("a", "Game", "PENDING", 0),
            due_row("b", "ProofOfInk", "SUBMITTING", 2),
            due_row("c", "Game", "SUBMITTED", 1),
        ]);
        assert!(skipped.is_empty());
        let decoded: Vec<(&str, Dim, bool, i32)> = due
            .iter()
            .map(|d| (d.ticket.as_str(), d.dim, d.in_flight, d.retry_count))
            .collect();
        assert_eq!(
            decoded,
            vec![
                ("a", Dim::Game, false, 0),
                ("b", Dim::ProofOfInk, true, 2),
                ("c", Dim::Game, true, 1),
            ]
        );
    }

    #[test]
    fn decode_skips_malformed_rows_and_reports_each_one() {
        let (due, skipped) = decode_due(&[
            due_row("a", "Game", "PENDING", 0),
            due_row("b", "NotADim", "PENDING", 0),
            due_row("c", "game", "PENDING", 0), // case-sensitive literal
            due_row("d", "ProofOfInk", "PENDING", 0),
        ]);
        let kept: Vec<&str> = due.iter().map(|d| d.ticket.as_str()).collect();
        assert_eq!(kept, vec!["a", "d"]);
        assert_eq!(
            skipped,
            vec![
                ("b".to_string(), "NotADim".to_string()),
                ("c".to_string(), "game".to_string()),
            ]
        );
    }

    const ALICE: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
    const BOB: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";

    #[test]
    fn parse_batch_splits_unparseable_keys_and_preserves_order() {
        let batch = vec![
            (ALICE.to_string(), Dim::Game),
            ("not-an-address".to_string(), Dim::Game),
            (BOB.to_string(), Dim::ProofOfInk),
        ];
        let (parsed, unparseable) = parse_batch(&batch);
        let kept: Vec<(&str, Dim)> = parsed.iter().map(|(t, _, d)| (t.as_str(), *d)).collect();
        assert_eq!(kept, vec![(ALICE, Dim::Game), (BOB, Dim::ProofOfInk)]);
        // The parsed account is the address's decoded key, not a placeholder.
        assert_eq!(parsed[0].1, ALICE.parse::<AccountId32>().unwrap());
        assert_eq!(unparseable.len(), 1);
        assert_eq!(unparseable[0].0, "not-an-address");
        assert!(!unparseable[0].1.is_empty()); // carries the parser's error text
    }

    #[test]
    fn retry_counts_rejoin_the_batch_in_batch_order() {
        let due = [due("a", false, 3), due("b", true, 0), due("c", false, 7)];
        // The batch is a subset of due, in a different order.
        let tickets = vec!["c".to_string(), "a".to_string()];
        assert_eq!(batch_retry_counts(&tickets, &due), vec![7, 3]);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "missing from due rows")]
    fn retry_counts_debug_assert_the_subset_invariant() {
        batch_retry_counts(&["ghost".to_string()], &[due("a", false, 0)]);
    }

    #[test]
    fn apply_split_projects_indices_and_computes_each_retry() {
        let split = split_batch_items(&[true, false, false], 3);
        let tickets = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let (registered, retries) = apply_split(&split, &tickets, &[0, 0, 4], DELAY);
        assert_eq!(registered, vec!["a"]);
        // Each retry increments its own count and prices its own backoff.
        assert_eq!(
            retries,
            vec![
                Retry {
                    ticket: "b".to_string(),
                    retry_count: 1,
                    delay_ms: DELAY(1),
                },
                Retry {
                    ticket: "c".to_string(),
                    retry_count: 5,
                    delay_ms: DELAY(5),
                },
            ]
        );
    }

    #[test]
    fn apply_split_skips_out_of_range_indices() {
        // 4 events over a 2-call batch: indices 2 and 3 map to no ticket.
        let split = split_batch_items(&[true, false, true, false], 2);
        let tickets = vec!["a".to_string(), "b".to_string()];
        let (registered, retries) = apply_split(&split, &tickets, &[0, 2], DELAY);
        assert_eq!(registered, vec!["a"]);
        assert_eq!(
            retries,
            vec![Retry {
                ticket: "b".to_string(),
                retry_count: 3,
                delay_ms: DELAY(3),
            }]
        );
    }
}
