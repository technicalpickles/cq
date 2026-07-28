use crate::scope::QueryScope;
use anyhow::Result;
use duckdb::Connection;
use std::path::PathBuf;

pub trait TranscriptProvider {
    fn name(&self) -> &str;
    fn discover_files(&self, scope: &QueryScope) -> Result<Vec<PathBuf>>;
    fn register_views(&self, conn: &Connection, files: &[PathBuf]) -> Result<()>;
    fn list_projects(&self) -> Result<Vec<ProjectInfo>>;

    /// Prepare any provider-specific state and report whether this provider is
    /// active (has data to contribute). Inactive providers are skipped when
    /// composing the views.
    fn prepare(&self, conn: &Connection) -> Result<bool>;

    /// The SELECT body this provider contributes for `view`, or `None` if it
    /// contributes nothing for that view. `views::compose_views` UNION ALLs the
    /// bodies from all active providers.
    fn contribute_view_sql(&self, view: View) -> Option<String>;
}

/// The five SQL views cq exposes. Each active provider contributes a SELECT
/// body per view via [`TranscriptProvider::contribute_view_sql`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Messages,
    ToolCalls,
    ToolResults,
    HookEvents,
    Sessions,
}

impl View {
    /// All views in creation order. `Sessions` comes last because its SQL reads
    /// from the `messages` view, which must already exist. `HookEvents` doesn't
    /// read `messages`, so its position only matters relative to `Sessions`
    /// (before it).
    pub const ALL: [View; 5] = [
        View::Messages,
        View::ToolCalls,
        View::ToolResults,
        View::HookEvents,
        View::Sessions,
    ];

    /// The SQL view name as created in the database.
    pub fn name(&self) -> &'static str {
        match self {
            View::Messages => "messages",
            View::ToolCalls => "tool_calls",
            View::ToolResults => "tool_results",
            View::HookEvents => "hook_events",
            View::Sessions => "sessions",
        }
    }
}

#[derive(Debug)]
pub struct ProjectInfo {
    pub encoded_name: String,
    pub decoded_path: String,
    pub file_count: usize,
}
