use anyhow::Result;
use duckdb::Connection;

use crate::output::OutputFormat;
use crate::scope::QueryScope;
use crate::trace;

/// Which renderer `cq trace` dispatches to. `--json` bypasses both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceOutput {
    /// Terminal waterfall (default).
    Waterfall,
    /// Chrome Trace Event JSON on stdout (`--perfetto`).
    Perfetto,
}

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    format: &OutputFormat,
    output: TraceOutput,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<()> {
    let session_id = match scope.session.as_deref() {
        Some(id) => id,
        None => {
            // A trace is one session's shape; there's nothing to render across
            // sessions, so this is required rather than defaulted.
            eprintln!("Error: cq trace requires --session");
            eprintln!("Usage: cq trace --session <id>");
            eprintln!("Hint: Run 'cq sessions' to find session IDs");
            std::process::exit(1);
        }
    };

    let mut spans = trace::spans(conn, session_id)?;

    // No spans at all (as opposed to a window that happens to be empty) means
    // the session id itself didn't match anything real -- same convention
    // every other command follows for an unknown --session.
    if spans.is_empty() {
        super::print_session_not_found(session_id);
        return Ok(());
    }

    // `--from`/`--to` window the trace to a slice of the session. Offsets
    // (`+12m`) are relative to the session's own first span, so compute that
    // before any windowing narrows what "session start" would even mean.
    let window = if from.is_some() || to.is_some() {
        let session_start_ms = spans
            .iter()
            .map(|s| trace::epoch_ms(&s.start))
            .min()
            .unwrap_or(0);
        let from_ms = from
            .map(|bound| parse_bound(bound, session_start_ms))
            .unwrap_or(i64::MIN);
        let to_ms = to
            .map(|bound| parse_bound(bound, session_start_ms))
            .unwrap_or(i64::MAX);
        Some((from_ms, to_ms))
    } else {
        None
    };

    if let Some((from_ms, to_ms)) = window {
        // Overlap, not containment: a span that started before the window and
        // finished inside it (or vice versa) still touched this slice of time.
        spans.retain(|s| trace::epoch_ms(&s.end) >= from_ms && trace::epoch_ms(&s.start) <= to_ms);
    }

    // --json is the machine-readable escape hatch and wins over the renderer:
    // an agent asking for span rows doesn't want a waterfall or a trace file.
    // It returns before querying gaps, which only the renderers consume.
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&spans)?);
        return Ok(());
    }

    let mut gaps = trace::gaps(conn, session_id)?;
    if let Some((from_ms, to_ms)) = window {
        gaps.retain(|g| trace::epoch_ms(&g.start) >= from_ms && trace::epoch_ms(&g.end) <= to_ms);
    }

    match output {
        TraceOutput::Waterfall => trace::waterfall::render(&spans, &gaps),
        TraceOutput::Perfetto => trace::perfetto::emit(&spans, &gaps, session_id),
    }
}

/// Parse a window bound: `+12m` / `+90s` / `+2h` offset from session start, or
/// an absolute ISO timestamp. Returns epoch milliseconds.
///
/// Exits on invalid input rather than returning a `Result`, matching this
/// codebase's other flag-validation helpers (e.g. `validate_count_by` in
/// `commands/mod.rs`): these are user-input errors caught before any query
/// runs, not runtime failures worth threading through `?`. The exit lives
/// here, at the thin wrapper, so `try_parse_bound` below stays a pure
/// function unit tests can call directly without killing the test process.
fn parse_bound(bound: &str, session_start_ms: i64) -> i64 {
    match try_parse_bound(bound, session_start_ms) {
        Ok(ms) => ms,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}

/// The actual parsing logic behind `parse_bound`, factored out so it can
/// return `Err` instead of exiting -- see the note on `parse_bound`.
fn try_parse_bound(bound: &str, session_start_ms: i64) -> Result<i64, String> {
    if let Some(rest) = bound.strip_prefix('+') {
        // Split on the last *char*, not the last byte: `rest.len() - 1` would
        // land mid-codepoint for non-ASCII input (e.g. "+5é") and panic with
        // "byte index is not a char boundary". `char_indices().last()` finds
        // the last char's byte offset regardless of its width, so the split
        // is always valid; an empty `rest` (nothing after "+") falls through
        // to the same "Invalid window offset" error as any other malformed
        // input, rather than panicking or indexing out of bounds.
        //
        // This mirrors `scope::parse_duration`'s grammar shape (used by
        // `--since`) but isn't shared with it on purpose: the two flag
        // families use disjoint unit vocabularies (s/m/h here vs d/h/m/s
        // there), so unifying them would need a parameterized unit set for
        // little benefit.
        if let Some((split_at, _)) = rest.char_indices().last() {
            let (num, unit) = rest.split_at(split_at);
            if let Ok(n) = num.parse::<i64>() {
                let ms = match unit {
                    "s" => Some(n * 1_000),
                    "m" => Some(n * 60_000),
                    "h" => Some(n * 3_600_000),
                    _ => None,
                };
                if let Some(ms) = ms {
                    return Ok(session_start_ms + ms);
                }
                return Err(format!(
                    "Error: Unknown window unit '{unit}' in '{bound}'\nValid units: s, m, h"
                ));
            }
        }
        return Err(format!(
            "Error: Invalid window offset '{bound}'\nExpected format: +<number><unit> (e.g. +12m, +90s)"
        ));
    }

    match chrono::DateTime::parse_from_rfc3339(bound) {
        Ok(dt) => Ok(dt.timestamp_millis()),
        Err(_) => Err(format!(
            "Error: Invalid window bound '{bound}'\nExpected an offset (+12m) or an ISO timestamp"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_seconds() {
        assert_eq!(try_parse_bound("+90s", 1_000).unwrap(), 1_000 + 90_000);
    }

    #[test]
    fn offset_minutes() {
        assert_eq!(try_parse_bound("+12m", 1_000).unwrap(), 1_000 + 12 * 60_000);
    }

    #[test]
    fn offset_hours() {
        assert_eq!(
            try_parse_bound("+2h", 1_000).unwrap(),
            1_000 + 2 * 3_600_000
        );
    }

    #[test]
    fn absolute_iso_timestamp() {
        let ms = try_parse_bound("2026-09-10T12:00:00Z", 0).unwrap();
        assert_eq!(ms, 1_789_041_600_000);
    }

    #[test]
    fn bad_unit_quotes_input() {
        let err = try_parse_bound("+5x", 0).unwrap_err();
        assert!(
            err.contains("+5x"),
            "expected error to quote '+5x', got: {err}"
        );
    }

    #[test]
    fn bad_number_quotes_input() {
        let err = try_parse_bound("+xm", 0).unwrap_err();
        assert!(
            err.contains("+xm"),
            "expected error to quote '+xm', got: {err}"
        );
    }

    #[test]
    fn malformed_bound_is_an_error() {
        let err = try_parse_bound("garbage", 0).unwrap_err();
        assert!(
            err.contains("garbage"),
            "expected error to quote 'garbage', got: {err}"
        );
    }

    /// Regression test: `"+5é"` used to panic with "byte index 2 is not a
    /// char boundary" because the old implementation split `rest` on a byte
    /// index (`rest.len() - 1`) rather than a char boundary. It must produce
    /// an ordinary error instead.
    #[test]
    fn non_ascii_offset_does_not_panic() {
        let err = try_parse_bound("+5é", 0).unwrap_err();
        assert!(
            err.contains("+5é"),
            "expected error to quote the bad input '+5é', got: {err}"
        );
    }

    #[test]
    fn empty_offset_after_plus_is_an_error() {
        let err = try_parse_bound("+", 0).unwrap_err();
        assert!(err.contains('+'), "expected error to quote '+', got: {err}");
    }
}
