//! `cq bundle`: package one Claude session's raw transcript files into a
//! zip. See `docs/specs/2026-09-15-session-bundle-design.md`.

use crate::bundle;
use crate::claude_provider::ClaudeProvider;
use crate::provider::TranscriptProvider;
use crate::scope::QueryScope;
use anyhow::{Context, Result};
use duckdb::Connection;
use std::path::Path;

/// `scope.session` must already be the full resolved session id -- `main.rs`
/// requires `--session` and validates its UUID shape before any command runs,
/// same precondition `cq trace` relies on.
pub fn run(
    conn: &Connection,
    provider: &ClaudeProvider,
    scope: &QueryScope,
    session_id: &str,
    output: &Path,
) -> Result<()> {
    let files = provider
        .discover_files(scope)?
        .into_iter()
        .filter(|file| file_belongs_to_session(file, session_id))
        .collect::<Vec<_>>();
    if files.is_empty() {
        super::print_session_not_found(session_id);
        return Ok(());
    }

    let meta = fetch_session_meta(conn, session_id)?;
    let summary = bundle::write_bundle(session_id, meta, &files, output)
        .with_context(|| format!("Failed to write bundle to {}", output.display()))?;

    for missing in &summary.sidecars_missing {
        eprintln!(
            "{}",
            crate::style::hint(&format!("Sidecar not found, skipped: {missing}"))
        );
    }
    eprintln!(
        "Wrote {} ({} files, {} sidecars, {} bytes)",
        output.display(),
        summary.files_written,
        summary.sidecars_included.len(),
        summary.bytes_written
    );
    Ok(())
}

fn file_belongs_to_session(file: &Path, session_id: &str) -> bool {
    if file
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == format!("{session_id}.jsonl"))
    {
        return true;
    }

    let components = file
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    components
        .windows(2)
        .any(|window| window[0] == session_id && window[1] == "subagents")
}

fn fetch_session_meta(conn: &Connection, session_id: &str) -> Result<bundle::SessionMeta> {
    let sql = "SELECT project, source, harness, started_at, ended_at
        FROM sessions WHERE harness = 'claude' AND session_id = ?";
    let mut stmt = conn
        .prepare(sql)
        .context("preparing session metadata query")?;
    let mut rows = stmt
        .query_map([session_id], |row| {
            Ok(bundle::SessionMeta {
                project: row.get(0)?,
                source: row.get(1)?,
                harness: row.get(2)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
            })
        })
        .context("running session metadata query")?;

    match rows.next() {
        Some(meta) => Ok(meta?),
        None => {
            eprintln!(
                "{}",
                crate::style::hint(
                    "Session metadata not found in the index (try --reindex); manifest fields will be empty"
                )
            );
            Ok(bundle::SessionMeta::default())
        }
    }
}
