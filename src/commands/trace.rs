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

    let spans = trace::spans(conn, session_id)?;

    // --json is the machine-readable escape hatch and wins over the renderer:
    // an agent asking for span rows doesn't want a waterfall or a trace file.
    // It returns before querying gaps, which only the renderers consume.
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&spans)?);
        return Ok(());
    }

    let gaps = trace::gaps(conn, session_id)?;
    match output {
        TraceOutput::Waterfall => trace::waterfall::render(&spans, &gaps),
        TraceOutput::Perfetto => trace::perfetto::emit(&spans, &gaps, session_id),
    }
}
