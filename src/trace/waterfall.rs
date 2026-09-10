//! Terminal waterfall renderer for a session trace.

use crate::trace::{Gap, Span};
use anyhow::Result;

/// Terminal waterfall renderer. Implemented in Task 5.
pub fn render(_spans: &[Span], _gaps: &[Gap]) -> Result<()> {
    Ok(())
}
