//! Chrome Trace Event JSON emitter, for loading a session trace into Perfetto
//! (or Firefox Profiler, or Speedscope).

use crate::trace::{Gap, Span};
use anyhow::Result;

/// Chrome Trace JSON emitter. Implemented in Task 7.
pub fn emit(_spans: &[Span], _gaps: &[Gap], _session_id: &str) -> Result<()> {
    Ok(())
}
