use crate::provider::{ProjectInfo, TranscriptProvider, View};
use crate::scope::QueryScope;
use crate::views;
use anyhow::{Context, Result};
use duckdb::Connection;
use std::path::{Path, PathBuf};

/// Reads the JSONL rollout transcripts written by Codex. The session root is
/// derived from `CODEX_HOME` when set, and can be explicitly overridden for
/// testing or nonstandard storage with `CQ_CODEX_SESSIONS_DIR`.
pub struct CodexProvider {
    sessions_dir: PathBuf,
}

impl CodexProvider {
    pub fn new() -> Result<Self> {
        let sessions_dir = resolve_sessions_dir(
            std::env::var_os("CQ_CODEX_SESSIONS_DIR").map(PathBuf::from),
            std::env::var_os("CODEX_HOME").map(PathBuf::from),
            dirs::home_dir(),
        )?;
        Ok(Self { sessions_dir })
    }

    pub fn with_sessions_dir(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }
}

/// Resolve the Codex transcript directory with explicit cq configuration
/// taking precedence over Codex's own home-directory setting.
fn resolve_sessions_dir(
    cq_sessions_dir: Option<PathBuf>,
    codex_home: Option<PathBuf>,
    home_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(dir) = cq_sessions_dir {
        return Ok(dir);
    }
    if let Some(home) = codex_home {
        return Ok(home.join("sessions"));
    }
    Ok(home_dir
        .context("Could not determine home directory")?
        .join(".codex")
        .join("sessions"))
}

impl TranscriptProvider for CodexProvider {
    fn name(&self) -> &str {
        "codex"
    }

    fn discover_files(&self, _scope: &QueryScope) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        collect_jsonl_paths(&self.sessions_dir, &mut files)?;
        Ok(files)
    }

    fn register_views(&self, _conn: &Connection, _files: &[PathBuf]) -> Result<()> {
        // Production views are composed in db::setup_connection over the
        // persistent raw_records cache. This method remains for trait symmetry.
        Ok(())
    }

    fn list_projects(&self) -> Result<Vec<ProjectInfo>> {
        Ok(Vec::new())
    }

    fn prepare(&self, _conn: &Connection) -> Result<bool> {
        Ok(self.sessions_dir.is_dir())
    }

    fn contribute_view_sql(&self, view: View) -> Option<String> {
        match view {
            View::Messages => Some(views::codex_messages_sql()),
            View::ToolCalls => Some(views::codex_tool_calls_sql()),
            View::ToolResults => Some(views::codex_tool_results_sql()),
            View::HookEvents => views::codex_hook_events_sql(),
            View::Sessions => Some(views::codex_sessions_sql()),
        }
    }
}

fn collect_jsonl_paths(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_jsonl_paths(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            files.push(path);
        }
    }
    files.sort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache;
    use crate::db::SyncMode;
    use crate::sync_scope::SyncScope;
    use crate::views;
    use tempfile::TempDir;

    #[test]
    fn session_directory_uses_explicit_cq_override_first() {
        let cq_override = PathBuf::from("/cq-sessions");
        let resolved = resolve_sessions_dir(
            Some(cq_override.clone()),
            Some(PathBuf::from("/codex-home")),
            Some(PathBuf::from("/user-home")),
        )
        .unwrap();
        assert_eq!(resolved, cq_override);
    }

    #[test]
    fn session_directory_derives_from_codex_home() {
        let resolved = resolve_sessions_dir(
            None,
            Some(PathBuf::from("/custom-codex")),
            Some(PathBuf::from("/user-home")),
        )
        .unwrap();
        assert_eq!(resolved, PathBuf::from("/custom-codex/sessions"));
    }

    #[test]
    fn session_directory_falls_back_to_default_codex_home() {
        let resolved = resolve_sessions_dir(None, None, Some(PathBuf::from("/user-home"))).unwrap();
        assert_eq!(resolved, PathBuf::from("/user-home/.codex/sessions"));
    }

    #[test]
    fn discovers_rollout_files_recursively() {
        let root = TempDir::new().unwrap();
        let nested = root.path().join("2026/08/26");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("rollout-a.jsonl"), "{}\n").unwrap();
        std::fs::write(nested.join("notes.txt"), "x").unwrap();

        let provider = CodexProvider::with_sessions_dir(root.path().to_path_buf());
        let files = provider
            .discover_files(&QueryScope::new(None, None, None))
            .unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("rollout-a.jsonl"));
    }

    #[test]
    fn maps_codex_records_to_cq_views() {
        let sessions = TempDir::new().unwrap();
        let rollout_dir = sessions.path().join("2026/08/26");
        std::fs::create_dir_all(&rollout_dir).unwrap();
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_session.jsonl");
        std::fs::copy(&fixture, rollout_dir.join("rollout-session.jsonl")).unwrap();

        let cache_dir = TempDir::new().unwrap();
        let conn = cache::open(cache_dir.path(), true).unwrap();
        crate::indexer::sync_sources(
            &conn,
            &[("codex".to_string(), sessions.path().to_path_buf())],
            SyncMode::Force,
            SyncScope::All,
            cache_dir.path(),
        )
        .unwrap();

        let provider = CodexProvider::with_sessions_dir(sessions.path().to_path_buf());
        views::compose_views(&conn, &[&provider]).unwrap();

        let session_id: String = conn
            .query_row("SELECT session_id FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(session_id, "019a1b2c-3d4e-7f80-9a0b-1c2d3e4f5a6b");

        let project: String = conn
            .query_row("SELECT project FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(project, "/Users/test/codex-project");

        let model: String = conn
            .query_row(
                "SELECT model FROM messages WHERE uuid = 'msg-assistant-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(model, "gpt-5-codex");

        let tools: i64 = conn
            .query_row("SELECT count(*) FROM tool_calls", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tools, 2);

        let session_tools: i64 = conn
            .query_row("SELECT tool_call_count FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(session_tools, 2);

        let messages: i64 = conn
            .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            messages, 2,
            "developer and reasoning records are not messages"
        );

        let event_records: i64 = conn
            .query_row(
                "SELECT count(*) FROM raw_records WHERE json_extract_string(json, '$.type') = 'event_msg'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_records, 3);

        let result: String = conn
            .query_row(
                "SELECT content FROM tool_results WHERE tool_use_id = 'call-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(result, "Cargo.toml\\nsrc");
    }
}
