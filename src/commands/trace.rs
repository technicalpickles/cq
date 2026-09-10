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
/// runs, not runtime failures worth threading through `?`.
fn parse_bound(bound: &str, session_start_ms: i64) -> i64 {
    if let Some(rest) = bound.strip_prefix('+') {
        if !rest.is_empty() {
            let (num, unit) = rest.split_at(rest.len() - 1);
            if let Ok(n) = num.parse::<i64>() {
                let ms = match unit {
                    "s" => Some(n * 1_000),
                    "m" => Some(n * 60_000),
                    "h" => Some(n * 3_600_000),
                    _ => None,
                };
                if let Some(ms) = ms {
                    return session_start_ms + ms;
                }
                eprintln!("Error: Unknown window unit '{unit}' in '{bound}'");
                eprintln!("Valid units: s, m, h");
                std::process::exit(1);
            }
        }
        eprintln!("Error: Invalid window offset '{bound}'");
        eprintln!("Expected format: +<number><unit> (e.g. +12m, +90s)");
        std::process::exit(1);
    }

    match chrono::DateTime::parse_from_rfc3339(bound) {
        Ok(dt) => dt.timestamp_millis(),
        Err(_) => {
            eprintln!("Error: Invalid window bound '{bound}'");
            eprintln!("Expected an offset (+12m) or an ISO timestamp");
            std::process::exit(1);
        }
    }
}
