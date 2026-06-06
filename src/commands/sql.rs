use anyhow::Result;
use duckdb::Connection;

use crate::output::{self, OutputFormat};

pub fn run(conn: &Connection, query: &str, format: &OutputFormat, wide: bool) -> Result<()> {
    let mut stmt = conn.prepare(query)?;
    output::print_results(&mut stmt, &[], format, wide)
}

/// Detect the two recurring DuckDB errors that come from doing date math against
/// cq's VARCHAR timestamp columns, and return a one-line fix hint.
///
/// Both stem from the same root cause: `timestamp`, `started_at`, `ended_at` and
/// friends are stored as ISO 8601 strings (VARCHAR), not native TIMESTAMP:
///
///   - `-(TIMESTAMP WITH TIME ZONE, INTERVAL)` binder error from
///     `now() - INTERVAL N DAY`. This one is also a DuckDB regression: the
///     overload existed before a 1.x bump tightened TIMESTAMPTZ arithmetic
///     (see the pin in Cargo.toml).
///   - `Cannot compare values of type VARCHAR and type TIMESTAMP` from
///     comparing a column directly against a `TIMESTAMP '...'` literal.
///
/// The match strings are the verbatim phrasings DuckDB emits on the pinned
/// version (=1.10501.0). If you bump the pin, re-run the tests below: a reworded
/// DuckDB error makes the hint silently stop appearing (the safe failure
/// direction, but worth knowing). The binder case anchors on the full operator
/// signature so an unrelated error that merely echoes the offending query text
/// (DuckDB echoes the SQL) can't trip it.
///
/// Returns `None` for any other error so we never editorialize on unrelated SQL.
pub fn timestamp_error_hint(message: &str) -> Option<&'static str> {
    let now_minus_interval = message.contains("-(TIMESTAMP WITH TIME ZONE, INTERVAL)");
    let varchar_timestamp_compare = message.contains("Cannot compare values of type")
        && message.contains("VARCHAR")
        && message.contains("TIMESTAMP");

    if now_minus_interval || varchar_timestamp_compare {
        // Note: deliberately does not suggest --since. It only filters the
        // sessions/messages/tools subcommands; `cq sql` runs the query verbatim
        // and ignores scope flags entirely.
        Some(
            "Hint: timestamp columns are VARCHAR ISO strings. Compare them as strings \
             (timestamp > '2026-06-01') or cast with now()::TIMESTAMP. For recency windows, \
             the sessions/messages/tools commands take --since.",
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::timestamp_error_hint;

    #[test]
    fn hints_on_now_minus_interval_binder_error() {
        let msg = "Binder Error: No function matches the given name and argument types \
                   '-(TIMESTAMP WITH TIME ZONE, INTERVAL)'. You might need to add explicit \
                   type casts.";
        assert!(timestamp_error_hint(msg).is_some());
    }

    #[test]
    fn hints_on_varchar_timestamp_comparison() {
        let msg = "Binder Error: Cannot compare values of type VARCHAR and type TIMESTAMP - \
                   an explicit cast is required";
        assert!(timestamp_error_hint(msg).is_some());
    }

    #[test]
    fn no_hint_on_unrelated_error() {
        let msg = "Catalog Error: Table with name foo does not exist!";
        assert!(timestamp_error_hint(msg).is_none());
    }
}
