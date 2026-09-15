//! Core logic for `cq bundle`: packaging one session's raw transcript files
//! (plus best-effort persistedOutputPath sidecars) into a zip. Kept free of
//! any DuckDB dependency so the logic here is unit-testable without a
//! database -- `commands/bundle.rs` is the only place that touches
//! `Connection`.
//!
//! See `docs/specs/2026-09-15-session-bundle-design.md`.

use std::path::{Path, PathBuf};

/// The zip-internal path for one discovered session file: `main.jsonl` for
/// the top-level `<session_id>.jsonl`, or `subagents/...` (preserving
/// nesting, including workflow subdirectories) for anything under a
/// `subagents/` directory.
pub fn zip_relative_path(file: &Path) -> String {
    let components: Vec<&std::ffi::OsStr> = file.components().map(|c| c.as_os_str()).collect();
    match components.iter().position(|c| *c == "subagents") {
        Some(idx) => components[idx..]
            .iter()
            .map(|c| c.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        None => "main.jsonl".to_string(),
    }
}

/// The sibling `<stem>.meta.json` next to a subagent transcript, if it
/// exists. Mirrors the sidecar lookup `indexer::read_agent_meta` uses, but
/// only needs the path -- the raw file gets copied into the bundle as-is,
/// not parsed.
pub fn meta_sidecar_for(file: &Path) -> Option<PathBuf> {
    let stem = file.file_stem()?.to_str()?;
    let meta = file.with_file_name(format!("{stem}.meta.json"));
    meta.exists().then_some(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn zip_relative_path_for_main_file() {
        let path = Path::new("/home/x/.claude/projects/-Users-x-proj/abc-123.jsonl");
        assert_eq!(zip_relative_path(path), "main.jsonl");
    }

    #[test]
    fn zip_relative_path_for_plain_subagent() {
        let path =
            Path::new("/home/x/.claude/projects/-Users-x-proj/abc-123/subagents/agent-sub1.jsonl");
        assert_eq!(zip_relative_path(path), "subagents/agent-sub1.jsonl");
    }

    #[test]
    fn zip_relative_path_for_workflow_subagent() {
        let path = Path::new(
            "/home/x/.claude/projects/-Users-x-proj/abc-123/subagents/workflows/wf_1/agent-wf.jsonl",
        );
        assert_eq!(
            zip_relative_path(path),
            "subagents/workflows/wf_1/agent-wf.jsonl"
        );
    }

    #[test]
    fn meta_sidecar_for_existing_sidecar() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("agent-sub1.jsonl");
        std::fs::write(&jsonl, "{}").unwrap();
        let meta = dir.path().join("agent-sub1.meta.json");
        std::fs::write(&meta, "{}").unwrap();
        assert_eq!(meta_sidecar_for(&jsonl), Some(meta));
    }

    #[test]
    fn meta_sidecar_for_missing_sidecar() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("agent-sub1.jsonl");
        std::fs::write(&jsonl, "{}").unwrap();
        assert_eq!(meta_sidecar_for(&jsonl), None);
    }
}
