//! Log subscriber installation, shared by every deployed process.
//!
//! One entry point, so the shape switches in one place. `LOG_FORMAT=json` emits
//! one JSON object per event, which is what makes the aggregated logs queryable
//! by field (`reservation_id`, `tx_hash`, …) rather than grep-only; anything
//! else keeps the text format local runs are read in.
//!
//! The service, environment, and container are deliberately absent from the
//! body: the log shipper labels them from Docker metadata
//! (`observability/alloy-config.alloy`), and two sources for one fact drift.

use tracing_subscriber::EnvFilter;

/// How events are rendered to stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// One JSON object per event; for aggregation (Loki).
    Json,
    /// Human-readable single lines; the local default.
    Text,
}

/// Install the process-wide log subscriber, honouring `RUST_LOG` (default
/// `info`) and `LOG_FORMAT` (`json` for aggregation, anything else for text).
///
/// Call once, first in `main`, before [`crate::metrics::spawn`] — that path
/// logs its own outcome.
pub fn init(service: &'static str) {
    let raw = std::env::var("LOG_FORMAT").ok();
    let format = parse_format(raw.as_deref());
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    match format {
        // `flatten_event` lifts fields to the top level, so a query reads
        // `reservation_id`, not `fields.reservation_id`. The span list is
        // dropped because subxt's would dominate every line.
        Format::Json => tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(false)
            .with_env_filter(filter())
            .init(),
        Format::Text => tracing_subscriber::fmt().with_env_filter(filter()).init(),
    }
    if let Some(raw) = raw.as_deref().filter(|raw| unrecognized(raw)) {
        tracing::warn!(
            service,
            log_format = raw,
            "unrecognized LOG_FORMAT; logging as text"
        );
    }
}

/// The pure half of [`init`]: only an explicit `json` opts into JSON, so a typo
/// degrades to readable text rather than to no logs at all.
fn parse_format(raw: Option<&str>) -> Format {
    match raw.map(|raw| raw.trim().to_ascii_lowercase()) {
        Some(raw) if raw == "json" => Format::Json,
        _ => Format::Text,
    }
}

/// Whether a set `LOG_FORMAT` is neither documented value, and so worth a
/// warning — an operator who meant JSON should not have to guess.
fn unrecognized(raw: &str) -> bool {
    !matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "json" | "text" | ""
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_json_selects_json() {
        for on in ["json", "JSON", " json "] {
            assert_eq!(parse_format(Some(on)), Format::Json, "{on:?}");
        }
        for off in ["text", "pretty", "", "yes", "1"] {
            assert_eq!(parse_format(Some(off)), Format::Text, "{off:?}");
        }
        assert_eq!(parse_format(None), Format::Text, "unset is text");
    }

    #[test]
    fn only_undocumented_values_warn() {
        for quiet in ["json", "text", "", "  "] {
            assert!(!unrecognized(quiet), "{quiet:?} should not warn");
        }
        for loud in ["jsn", "structured", "1"] {
            assert!(unrecognized(loud), "{loud:?} should warn");
        }
    }
}
