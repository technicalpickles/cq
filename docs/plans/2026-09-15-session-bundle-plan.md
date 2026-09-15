# Session bundle export (`cq bundle`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `cq bundle --session <id> [-o <path>]`, packaging one Claude session's raw transcript files (main JSONL, subagent JSONL, `.meta.json` sidecars, best-effort `persistedOutputPath` sidecars) into a self-contained zip with a `manifest.json`.

**Architecture:** A DB-free core (`src/bundle.rs`) holds the pure logic — mapping a discovered file to its zip-internal path, finding a subagent's `.meta.json` sidecar, scanning a JSONL file for `persistedOutputPath` pointers, and writing the zip + manifest. A thin CLI layer (`src/commands/bundle.rs`) does the DB/filesystem plumbing: requiring `--session`, resolving the session's files via the existing `ClaudeProvider::discover_files`, pulling session metadata from the `sessions` view, and reporting the result. This mirrors the existing `trace.rs` (model) / `commands/trace.rs` (CLI) split.

**Tech Stack:** Rust, `zip` crate (new dependency), existing `duckdb`/`clap`/`serde_json`/`anyhow` stack, `assert_cmd` for integration tests.

**Reference:** `docs/specs/2026-09-15-session-bundle-design.md`

---

### Task 1: Add the `zip` dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the dependency**

Add to the `[dependencies]` section of `Cargo.toml` (after `dirs = "5"`):

```toml
zip = "2"
```

- [ ] **Step 2: Verify it resolves and builds**

Run: `cargo build`
Expected: succeeds, `Cargo.lock` gains `zip` and its transitive deps (`crc32fast`, `flate2` or similar). If the build fails because `CompressionMethod::Deflated` isn't available, change the dependency line to:

```toml
zip = { version = "2", features = ["deflate"] }
```

and re-run `cargo build`.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add zip dependency for cq bundle"
```

---

### Task 2: Core path-mapping helpers (`zip_relative_path`, `meta_sidecar_for`)

**Files:**
- Create: `src/bundle.rs`
- Modify: `src/lib.rs:1` (add `pub mod bundle;`)

- [ ] **Step 1: Write the failing tests**

Create `src/bundle.rs`:

```rust
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
```

Add `pub mod bundle;` as the first line of `src/lib.rs` (before `pub mod cache;`).

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib bundle::`
Expected: PASS (this step is written test-first, but the implementation above is already correct — the run just confirms it; if you're following strict TDD, comment out the function bodies first, confirm a compile failure, then restore them).

- [ ] **Step 3: Commit**

```bash
git add src/bundle.rs src/lib.rs
git commit -m "feat: add cq bundle path-mapping helpers"
```

---

### Task 3: `persistedOutputPath` sidecar scanner

**Files:**
- Modify: `src/bundle.rs`

- [ ] **Step 1: Write the failing tests**

Add to `src/bundle.rs`, above the existing `#[cfg(test)]` block:

```rust
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};

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
```

Add these tests inside the existing `mod tests` block:

```rust
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
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib bundle::`
Expected: PASS, 8 tests total (3 path-mapping + 2 meta-sidecar + 3 new scan tests).

- [ ] **Step 3: Commit**

```bash
git add src/bundle.rs
git commit -m "feat: add persistedOutputPath sidecar scanner for cq bundle"
```

---

### Task 4: `write_bundle` — the actual zip writer

**Files:**
- Modify: `src/bundle.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/bundle.rs`, above the existing structs/functions (after the module doc comment, before `zip_relative_path`):

```rust
use serde::Serialize;
use std::io::{Read, Write};

/// Session-level fields pulled from the `sessions` view for the manifest.
/// Field names deliberately match that view's own columns rather than
/// inventing parallel names.
#[derive(Debug, Default, Clone)]
pub struct SessionMeta {
    pub project: Option<String>,
    pub source: Option<String>,
    pub harness: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct Manifest {
    session_id: String,
    project: Option<String>,
    source: Option<String>,
    harness: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    files: Vec<String>,
    sidecars_included: Vec<String>,
    sidecars_missing: Vec<String>,
    cq_version: String,
}

/// What `write_bundle` actually did, for the CLI layer to report.
#[derive(Debug, Default)]
pub struct BundleSummary {
    pub files_written: usize,
    pub sidecars_included: Vec<String>,
    pub sidecars_missing: Vec<String>,
    pub bytes_written: u64,
}
```

Add the test to the `mod tests` block (needs `use std::io::Read;` already available via `use super::*;`):

```rust
    #[test]
    fn write_bundle_zips_files_meta_and_manifest() {
        let src_dir = TempDir::new().unwrap();
        let session_id = "test-session-id";
        let main_file = src_dir.path().join(format!("{session_id}.jsonl"));
        std::fs::write(&main_file, "{\"type\":\"user\"}\n").unwrap();

        let sub_dir = src_dir.path().join(session_id).join("subagents");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let sub_file = sub_dir.join("agent-sub1.jsonl");
        std::fs::write(&sub_file, "{\"type\":\"assistant\"}\n").unwrap();
        std::fs::write(
            sub_dir.join("agent-sub1.meta.json"),
            "{\"agentType\":\"general-purpose\"}",
        )
        .unwrap();

        let out_dir = TempDir::new().unwrap();
        let out_path = out_dir.path().join("bundle.zip");

        let meta = SessionMeta {
            project: Some("myproject".to_string()),
            source: Some("main".to_string()),
            harness: Some("claude".to_string()),
            started_at: Some("2026-09-10T12:00:00Z".to_string()),
            ended_at: Some("2026-09-10T12:05:00Z".to_string()),
        };

        let summary =
            write_bundle(session_id, meta, &[main_file, sub_file], &out_path).unwrap();

        assert_eq!(summary.files_written, 3); // main + subagent + its meta.json
        assert!(summary.sidecars_included.is_empty());
        assert!(summary.sidecars_missing.is_empty());

        let zip_file = std::fs::File::open(&out_path).unwrap();
        let mut archive = zip::ZipArchive::new(zip_file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"main.jsonl".to_string()), "{names:?}");
        assert!(
            names.contains(&"subagents/agent-sub1.jsonl".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"subagents/agent-sub1.meta.json".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"manifest.json".to_string()), "{names:?}");

        let manifest_idx = names.iter().position(|n| n == "manifest.json").unwrap();
        let mut manifest_str = String::new();
        archive
            .by_index(manifest_idx)
            .unwrap()
            .read_to_string(&mut manifest_str)
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();
        assert_eq!(manifest["session_id"], session_id);
        assert_eq!(manifest["project"], "myproject");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib bundle::write_bundle_zips_files_meta_and_manifest`
Expected: FAIL with "cannot find function `write_bundle`" (or similar compile error).

- [ ] **Step 3: Implement `write_bundle`**

Add to `src/bundle.rs`, after `meta_sidecar_for`:

```rust
/// Build the zip at `output`: `files` (already discovered -- main +
/// subagents, `journal.jsonl` already excluded by the caller's discovery
/// step), each file's `.meta.json` sidecar if present, best-effort
/// `persistedOutputPath` sidecars, and `manifest.json`.
pub fn write_bundle(
    session_id: &str,
    meta: SessionMeta,
    files: &[PathBuf],
    output: &Path,
) -> anyhow::Result<BundleSummary> {
    use anyhow::Context;

    let zip_file =
        File::create(output).with_context(|| format!("Failed to create {}", output.display()))?;
    let mut zip = zip::ZipWriter::new(zip_file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut manifest_files = Vec::new();
    let mut sidecar_paths = BTreeSet::new();

    for file in files {
        let zip_path = zip_relative_path(file);
        write_file_entry(&mut zip, &zip_path, file, options)?;
        manifest_files.push(zip_path.clone());

        if let Some(meta_path) = meta_sidecar_for(file) {
            let zip_meta_path = zip_path.replace(".jsonl", ".meta.json");
            write_file_entry(&mut zip, &zip_meta_path, &meta_path, options)?;
            manifest_files.push(zip_meta_path);
        }

        sidecar_paths.extend(scan_persisted_output_paths(file)?);
    }

    let mut sidecars_included = Vec::new();
    let mut sidecars_missing = Vec::new();
    for path in &sidecar_paths {
        let sidecar = Path::new(path);
        if !sidecar.is_file() {
            sidecars_missing.push(path.clone());
            continue;
        }
        // Basenames are assumed unique across one session's sidecars -- true
        // for every persisted-output path seen so far (docs/session-storage.md).
        // Cheap to revisit (e.g. hash-prefix on collision) if that changes.
        let basename = sidecar
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "sidecar".to_string());
        let zip_path = format!("sidecars/{basename}");
        write_file_entry(&mut zip, &zip_path, sidecar, options)?;
        sidecars_included.push(zip_path);
    }

    let manifest = Manifest {
        session_id: session_id.to_string(),
        project: meta.project,
        source: meta.source,
        harness: meta.harness,
        started_at: meta.started_at,
        ended_at: meta.ended_at,
        files: manifest_files.clone(),
        sidecars_included: sidecars_included.clone(),
        sidecars_missing: sidecars_missing.clone(),
        cq_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let manifest_json =
        serde_json::to_vec_pretty(&manifest).context("Failed to serialize manifest.json")?;
    zip.start_file("manifest.json", options)
        .context("Failed to start manifest.json in zip")?;
    zip.write_all(&manifest_json)
        .context("Failed to write manifest.json")?;

    zip.finish().context("Failed to finalize zip")?;
    let bytes_written = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);

    Ok(BundleSummary {
        files_written: manifest_files.len(),
        sidecars_included,
        sidecars_missing,
        bytes_written,
    })
}

fn write_file_entry(
    zip: &mut zip::ZipWriter<File>,
    zip_path: &str,
    source: &Path,
    options: zip::write::SimpleFileOptions,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let mut contents = Vec::new();
    File::open(source)
        .with_context(|| format!("Failed to open {}", source.display()))?
        .read_to_end(&mut contents)
        .with_context(|| format!("Failed to read {}", source.display()))?;
    zip.start_file(zip_path, options)
        .with_context(|| format!("Failed to start {zip_path} in zip"))?;
    zip.write_all(&contents)
        .with_context(|| format!("Failed to write {zip_path} to zip"))?;
    Ok(())
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib bundle::`
Expected: PASS, all bundle unit tests green.

- [ ] **Step 5: Commit**

```bash
git add src/bundle.rs
git commit -m "feat: implement cq bundle zip writer with manifest"
```

---

### Task 5: Wire up the CLI (`cq bundle --session <id> [-o <path>]`)

**Files:**
- Create: `src/commands/bundle.rs`
- Modify: `src/commands/mod.rs` (add `pub mod bundle;`)
- Modify: `src/main.rs` (add `Command::Bundle` variant, dispatch arm, import)

- [ ] **Step 1: Add the module declaration**

In `src/commands/mod.rs`, add `pub mod bundle;` alongside the existing `pub mod trace;` line (keep the list alphabetical: after `pub mod projects;`, before `pub mod schema;`).

- [ ] **Step 2: Write `src/commands/bundle.rs`**

```rust
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
    let files = provider.discover_files(scope)?;
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

fn fetch_session_meta(conn: &Connection, session_id: &str) -> Result<bundle::SessionMeta> {
    let sql = "SELECT project, source, harness, started_at, ended_at
        FROM sessions WHERE harness = 'claude' AND session_id = ?";
    let mut stmt = conn.prepare(sql).context("preparing session metadata query")?;
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
```

- [ ] **Step 3: Add the `Bundle` variant to `Command` in `src/main.rs`**

Add `use std::path::PathBuf;` near the top of `src/main.rs` (after `use std::io::IsTerminal;`).

Add `bundle` to the existing commands import:

```rust
use cq::commands::{bundle, hooks, messages, projects, schema, search, sessions, sql, tools, trace};
```

Add this variant to the `Command` enum (after `Trace { ... }`, before `Sql { query: String }`):

```rust
    /// Package one session's raw transcript files (JSONL + subagents +
    /// best-effort persisted-output sidecars) into a zip, for sharing,
    /// archiving, or feeding another tool
    Bundle {
        /// Output zip path (default: ./session-<id>.zip in the current directory)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
```

- [ ] **Step 4: Add the dispatch arm**

Add this arm to the `match cli.command` block in `src/main.rs` (after the `Command::Trace { ... } => { ... }` arm, before `Command::Sql { query } => { ... }`):

```rust
        Command::Bundle { output } => {
            let session_id = match scope.session.as_deref() {
                Some(id) => id.to_string(),
                None => {
                    eprintln!("Error: cq bundle requires --session");
                    eprintln!("Usage: cq bundle --session <id> [-o <path>]");
                    eprintln!("Hint: Run 'cq sessions' to find session IDs");
                    std::process::exit(1);
                }
            };
            let output_path =
                output.unwrap_or_else(|| PathBuf::from(format!("session-{session_id}.zip")));
            bundle::run(&conn, &provider, &scope, &session_id, &output_path)?;
        }
```

- [ ] **Step 5: Build and smoke-test**

Run: `cargo build`
Expected: succeeds.

Run: `cargo run -- bundle`
Expected: prints `Error: cq bundle requires --session`, `Usage: cq bundle --session <id> [-o <path>]`, `Hint: Run 'cq sessions' to find session IDs`, exits non-zero.

Run: `cargo run -- --session $(cargo run -- sessions --limit 1 --json | python3 -c "import json,sys; print(json.load(sys.stdin)[0]['session_id'])") bundle`
Expected: prints a `Wrote ./session-<id>.zip (...)` line, and the zip exists in the current directory. Delete it afterward (`rm ./session-*.zip`) — this is a manual smoke test, not something to leave behind.

- [ ] **Step 6: Commit**

```bash
git add src/commands/bundle.rs src/commands/mod.rs src/main.rs
git commit -m "feat: add cq bundle CLI command"
```

---

### Task 6: Integration tests

**Files:**
- Create: `tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002.jsonl`
- Create: `tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/journal.jsonl`
- Create: `tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/subagents/agent-plain.jsonl`
- Create: `tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/subagents/agent-plain.meta.json`
- Create: `tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/subagents/workflows/wf_bundle/agent-wf.jsonl`
- Create: `tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/subagents/workflows/wf_bundle/agent-wf.meta.json`
- Modify: `tests/integration_test.rs` (new helpers + tests)

- [ ] **Step 1: Create the fixture tree**

`tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002.jsonl`:

```
{"type": "user", "uuid": "u1", "parentUuid": null, "timestamp": "2026-09-10T12:00:00.000Z", "message": {"role": "user", "content": "go"}, "sessionId": "b2c3d4e5-1111-4000-8000-000000000002", "cwd": "/Users/test/myproject", "version": "2.0.0"}
{"type": "assistant", "uuid": "a1", "parentUuid": "u1", "timestamp": "2026-09-10T12:00:01.000Z", "message": {"role": "assistant", "model": "claude-opus-5", "content": "done"}, "sessionId": "b2c3d4e5-1111-4000-8000-000000000002", "cwd": "/Users/test/myproject", "version": "2.0.0"}
```

`tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/journal.jsonl`:

```
{"type": "workflow_ledger_entry"}
```

`tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/subagents/agent-plain.jsonl`:

```
{"type": "user", "uuid": "s1u1", "parentUuid": null, "timestamp": "2026-09-10T12:00:02.000Z", "message": {"role": "user", "content": "go"}, "sessionId": "b2c3d4e5-1111-4000-8000-000000000002", "cwd": "/Users/test/myproject", "version": "2.0.0", "isSidechain": true, "agentId": "agent-plain"}
```

`tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/subagents/agent-plain.meta.json`:

```json
{"agentType": "general-purpose", "description": "test subagent", "toolUseId": "toolu_1", "spawnDepth": 1}
```

`tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/subagents/workflows/wf_bundle/agent-wf.jsonl`:

```
{"type": "user", "uuid": "w1u1", "parentUuid": null, "timestamp": "2026-09-10T12:00:03.000Z", "message": {"role": "user", "content": "go"}, "sessionId": "b2c3d4e5-1111-4000-8000-000000000002", "cwd": "/Users/test/myproject", "version": "2.0.0", "isSidechain": true, "agentId": "agent-wf"}
```

`tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002/subagents/workflows/wf_bundle/agent-wf.meta.json`:

```json
{"agentType": "workflow-agent", "spawnDepth": 2}
```

- [ ] **Step 2: Add test helpers to `tests/integration_test.rs`**

Add near the other `setup_*` helpers (after `setup_env_tree`):

```rust
/// Recursively copy every file under `src` into `dest`, preserving nested
/// directories -- `setup_env_tree` only copies the flat main file + a single
/// `subagents/` level, which isn't enough for bundle fixtures that need
/// `journal.jsonl` and nested `subagents/workflows/...` to actually exist on
/// disk (the whole point of those tests is proving what bundle excludes).
fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&src_path, &dest_path);
        } else {
            std::fs::copy(&src_path, &dest_path).unwrap();
        }
    }
}

fn setup_bundle_env(session_id: &str) -> TestEnv {
    let env = setup_env(&[]);
    let project_dir = env.projects.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_dir).unwrap();

    std::fs::copy(
        fixture_path(&format!("{session_id}.jsonl")),
        project_dir.join(format!("{session_id}.jsonl")),
    )
    .unwrap();
    copy_dir_recursive(&fixture_path(session_id), &project_dir.join(session_id));
    env
}
```

- [ ] **Step 3: Write the tests**

Add near the end of `tests/integration_test.rs`:

```rust
// ---- cq bundle ----

const BUNDLE_SESSION: &str = "b2c3d4e5-1111-4000-8000-000000000002";

#[test]
fn bundle_requires_session() {
    let env = setup_bundle_env(BUNDLE_SESSION);
    let output = cq_cmd(&env).args(["bundle"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cq bundle requires --session"),
        "got: {stderr}"
    );
    assert!(!output.status.success());
}

#[test]
fn bundle_unknown_session_reports_not_found() {
    let env = setup_bundle_env(BUNDLE_SESSION);
    let out_dir = TempDir::new().unwrap();
    let out_path = out_dir.path().join("bundle.zip");
    let output = cq_cmd(&env)
        .args(["--session", "00000000-0000-4000-8000-000000000000", "bundle", "-o"])
        .arg(&out_path)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found"), "got: {stderr}");
    assert!(!out_path.exists());
}

#[test]
fn bundle_writes_expected_zip_contents() {
    let env = setup_bundle_env(BUNDLE_SESSION);
    let out_dir = TempDir::new().unwrap();
    let out_path = out_dir.path().join("bundle.zip");

    let output = cq_cmd(&env)
        .args(["--session", BUNDLE_SESSION, "bundle", "-o"])
        .arg(&out_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let zip_file = std::fs::File::open(&out_path).unwrap();
    let mut archive = zip::ZipArchive::new(zip_file).unwrap();
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect();

    assert!(names.contains(&"main.jsonl".to_string()), "{names:?}");
    assert!(
        names.contains(&"subagents/agent-plain.jsonl".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"subagents/agent-plain.meta.json".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"subagents/workflows/wf_bundle/agent-wf.jsonl".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"subagents/workflows/wf_bundle/agent-wf.meta.json".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"manifest.json".to_string()), "{names:?}");
    assert!(
        !names.iter().any(|n| n.contains("journal")),
        "journal.jsonl must never appear in a bundle: {names:?}"
    );

    let manifest_idx = names.iter().position(|n| n == "manifest.json").unwrap();
    let mut manifest_str = String::new();
    std::io::Read::read_to_string(&mut archive.by_index(manifest_idx).unwrap(), &mut manifest_str)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();
    assert_eq!(manifest["session_id"], BUNDLE_SESSION);
    assert_eq!(manifest["harness"], "claude");
    assert!(manifest["project"].as_str().unwrap().contains("myproject"));
    assert_eq!(manifest["sidecars_included"].as_array().unwrap().len(), 0);
    assert_eq!(manifest["sidecars_missing"].as_array().unwrap().len(), 0);
}

#[test]
fn bundle_default_output_path_is_session_zip_in_cwd() {
    let env = setup_bundle_env(BUNDLE_SESSION);
    let cwd_dir = TempDir::new().unwrap();

    let output = cq_cmd(&env)
        .args(["--session", BUNDLE_SESSION, "bundle"])
        .current_dir(cwd_dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let expected = cwd_dir.path().join(format!("session-{BUNDLE_SESSION}.zip"));
    assert!(expected.exists(), "expected {expected:?} to exist");
}

#[test]
fn bundle_includes_available_sidecar_and_warns_on_missing() {
    let env = setup_env(&[]);
    let project_dir = env.projects.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_dir).unwrap();

    let sidecar_dir = TempDir::new().unwrap();
    let present_sidecar = sidecar_dir.path().join("present-output.txt");
    std::fs::write(&present_sidecar, "the real tool output").unwrap();
    let missing_sidecar = sidecar_dir.path().join("missing-output.txt"); // never created

    let session_id = "c3d4e5f6-2222-4000-8000-000000000003";
    let record_present = format!(
        "{{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"timestamp\":\"2026-09-10T12:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_1\",\"content\":\"truncated\"}}]}},\"sessionId\":\"{session_id}\",\"cwd\":\"/Users/test/myproject\",\"version\":\"2.0.0\",\"toolUseResult\":{{\"persistedOutputPath\":\"{}\",\"persistedOutputSize\":50000}}}}",
        present_sidecar.display()
    );
    let record_missing = format!(
        "{{\"type\":\"user\",\"uuid\":\"u2\",\"parentUuid\":\"u1\",\"timestamp\":\"2026-09-10T12:00:01.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_2\",\"content\":\"truncated\"}}]}},\"sessionId\":\"{session_id}\",\"cwd\":\"/Users/test/myproject\",\"version\":\"2.0.0\",\"toolUseResult\":{{\"persistedOutputPath\":\"{}\",\"persistedOutputSize\":50000}}}}",
        missing_sidecar.display()
    );
    std::fs::write(
        project_dir.join(format!("{session_id}.jsonl")),
        format!("{record_present}\n{record_missing}\n"),
    )
    .unwrap();

    let out_dir = TempDir::new().unwrap();
    let out_path = out_dir.path().join("bundle.zip");
    let output = cq_cmd(&env)
        .args(["--session", session_id, "bundle", "-o"])
        .arg(&out_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing-output.txt"),
        "expected a warning naming the missing sidecar: {stderr}"
    );

    let zip_file = std::fs::File::open(&out_path).unwrap();
    let mut archive = zip::ZipArchive::new(zip_file).unwrap();
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "sidecars/present-output.txt"),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("missing-output")),
        "{names:?}"
    );

    let manifest_idx = names.iter().position(|n| n == "manifest.json").unwrap();
    let mut manifest_str = String::new();
    std::io::Read::read_to_string(&mut archive.by_index(manifest_idx).unwrap(), &mut manifest_str)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();
    assert_eq!(manifest["sidecars_included"].as_array().unwrap().len(), 1);
    let missing = manifest["sidecars_missing"].as_array().unwrap();
    assert_eq!(missing.len(), 1);
    assert!(missing[0]
        .as_str()
        .unwrap()
        .contains("missing-output.txt"));
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test bundle`
Expected: PASS, 5 integration tests (`bundle_requires_session`, `bundle_unknown_session_reports_not_found`, `bundle_writes_expected_zip_contents`, `bundle_default_output_path_is_session_zip_in_cwd`, `bundle_includes_available_sidecar_and_warns_on_missing`).

- [ ] **Step 5: Run the full test suite**

Run: `cargo test`
Expected: PASS, no regressions in existing tests.

- [ ] **Step 6: Commit**

```bash
git add tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002.jsonl \
        tests/fixtures/b2c3d4e5-1111-4000-8000-000000000002 \
        tests/integration_test.rs
git commit -m "test: add integration coverage for cq bundle"
```

---

### Task 7: Docs

**Files:**
- Modify: `README.md`
- Modify: `CLAUDE.md`
- Modify: `docs/specs/2026-09-15-session-bundle-design.md`

- [ ] **Step 1: Add a "## Bundle" section to `README.md`**

Insert after the existing `## Trace` section (before `## Use cases`):

```markdown
## Bundle

`cq bundle --session <id>` packages one session's raw transcript files -- the
main JSONL, any subagent JSONL (including nested workflow agents) and their
`.meta.json` sidecars, and best-effort copies of any `persistedOutputPath`
sidecars the records point to -- into a single zip with a `manifest.json`.
Useful for archiving a session before it rotates out of
`~/.claude/projects/`, attaching one to a bug report, or handing raw JSONL to
another tool without re-deriving cq's own file discovery by hand.

```
$ cq bundle --session a1b2c3d4
Wrote ./session-a1b2c3d4-0000-4000-8000-000000000001.zip (5 files, 0 sidecars, 3214 bytes)
```

```
cq bundle --session <id> -o ~/Desktop/session.zip   # explicit output path
```
```

- [ ] **Step 2: Update `CLAUDE.md`'s architecture tree**

In the `## Architecture` code block, add `bundle.rs` to the `commands/` list (alphabetically after `context.rs`'s section, before `hooks.rs`):

```
  bundle.rs       `cq bundle`: --session required check, default output path, session metadata query, calls bundle::write_bundle, prints summary/warnings
```

Add a top-level module line (alphabetically first, before `cache.rs`):

```
bundle.rs         Core `cq bundle` logic: file -> zip-path mapping, meta.json sidecar lookup, persistedOutputPath scan, zip writing + manifest (docs/specs/2026-09-15-session-bundle-design.md)
```

- [ ] **Step 3: Mark the spec as implemented**

In `docs/specs/2026-09-15-session-bundle-design.md`, change the header:

```markdown
Status: implemented
Date: 2026-09-15
```

- [ ] **Step 4: Commit**

```bash
git add README.md CLAUDE.md docs/specs/2026-09-15-session-bundle-design.md
git commit -m "docs: document cq bundle"
```
