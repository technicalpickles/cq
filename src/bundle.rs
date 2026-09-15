//! Core logic for `cq bundle`: packaging one session's raw transcript files
//! (plus best-effort persistedOutputPath sidecars) into a zip. Kept free of
//! any DuckDB dependency so the logic here is unit-testable without a
//! database -- `commands/bundle.rs` is the only place that touches
//! `Connection`.
//!
//! See `docs/specs/2026-09-15-session-bundle-design.md`.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
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

/// Scan one JSONL file for `toolUseResult.persistedOutputPath` pointers.
/// Unparseable lines are skipped, not errors -- a bundle is a best-effort
/// artifact over real transcripts, which already tolerate the same
/// unparseable-line reality every other cq command works around (see
/// `docs/session-storage.md`). Returns paths deduplicated and sorted.
pub fn scan_persisted_output_paths(file: &Path) -> Result<Vec<String>> {
    let handle = File::open(file)
        .with_context(|| format!("Failed to open {} for sidecar scan", file.display()))?;
    let mut found = BTreeSet::new();
    for line in BufReader::new(handle).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(path) = value
            .get("toolUseResult")
            .and_then(|v| v.get("persistedOutputPath"))
            .and_then(|v| v.as_str())
        {
            found.insert(path.to_string());
        }
    }
    Ok(found.into_iter().collect())
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

    #[test]
    fn scan_finds_persisted_output_path() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("session.jsonl");
        std::fs::write(
            &jsonl,
            "{\"type\":\"user\",\"toolUseResult\":{\"persistedOutputPath\":\"/tmp/out.txt\"}}\n\
             {\"type\":\"assistant\"}\n",
        )
        .unwrap();
        assert_eq!(
            scan_persisted_output_paths(&jsonl).unwrap(),
            vec!["/tmp/out.txt".to_string()]
        );
    }

    #[test]
    fn scan_skips_unparseable_and_blank_lines() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("session.jsonl");
        std::fs::write(&jsonl, "not json at all\n\n").unwrap();
        assert_eq!(
            scan_persisted_output_paths(&jsonl).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn scan_dedupes_repeated_paths() {
        let dir = TempDir::new().unwrap();
        let jsonl = dir.path().join("session.jsonl");
        std::fs::write(
            &jsonl,
            "{\"toolUseResult\":{\"persistedOutputPath\":\"/tmp/out.txt\"}}\n\
             {\"toolUseResult\":{\"persistedOutputPath\":\"/tmp/out.txt\"}}\n",
        )
        .unwrap();
        assert_eq!(
            scan_persisted_output_paths(&jsonl).unwrap(),
            vec!["/tmp/out.txt".to_string()]
        );
    }
}
