# cq Boot Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Sub-second startup for common queries by persisting parsed session data and only re-parsing changed files.

**Architecture:** Persistent DuckDB file at `~/.cache/cq/index.duckdb` with a `file_registry` table (mtime+size change detection, cwd for project paths) and a `raw_records` table (parsed JSON). Views stay as SQL views on top of the persisted data. A `--reindex` flag forces full rebuild.

**Tech Stack:** Rust, DuckDB (persistent mode), `dirs` crate for cache path.

**Spec:** `docs/specs/2026-04-14-cq-boot-cache-design.md`

---

## File Structure

```
src/
├── cache.rs              # NEW: Cache DB lifecycle (open, version check, tables)
├── indexer.rs             # NEW: File scanning, diffing, incremental insert/delete
├── views.rs              # MODIFY: json_extract migration, register_derived_views
├── db.rs                 # MODIFY: Use cache path instead of in-memory
├── main.rs               # MODIFY: Add --reindex flag
├── claude_provider.rs    # MODIFY: discover_all_files (no scope filtering)
├── provider.rs           # MODIFY: Add discover_all_files to trait
├── lib.rs                # MODIFY: Add cache, indexer modules
└── ...
tests/
├── views_test.rs         # MODIFY: Tests still work with json_extract views
├── integration_test.rs   # MODIFY: Tests use CQ_CACHE_DIR env var
└── cache_test.rs         # NEW: Cache + indexer integration tests
```

---

## Task 1: Migrate Views to json_extract

Switch all view SQL from struct dot notation (`json.sessionId`) to `json_extract_string`/`json_extract`. This decouples views from DuckDB's auto-inferred STRUCT typing, which is required for the persistent JSON column.

Also rename the `filename` column reference to `source_file` for consistency with the cache table schema.

**Files:**
- Modify: `src/views.rs`
- Test: `tests/views_test.rs` (existing tests, should pass without changes)

- [ ] **Step 0: Verify json_extract works on STRUCT columns**

Before committing to the migration, verify that `json_extract_string` works on DuckDB's STRUCT-typed columns (produced by `read_json` with `records=false`). If it doesn't, we'd need a `CAST(json AS JSON)` wrapper.

Run against real data:
```bash
cargo run -- sql "SELECT json_extract_string(json, '$.sessionId') AS extracted, json.sessionId AS struct_access FROM raw_records LIMIT 1"
```

Expected: Both columns return the same session ID value. If `json_extract_string` errors on STRUCT input, add `CAST(json AS JSON)` in the raw_records view before proceeding.

- [ ] **Step 1: Update PROJECT_EXPR to use source_file**

In `src/views.rs:8-9`, change `filename` to `source_file`:

```rust
const PROJECT_EXPR: &str =
    "'/' || replace(regexp_extract(source_file, '.*/([^/]+)/[^/]+$', 1)[2:], '-', '/')";
```

- [ ] **Step 2: Update register_raw_view to alias filename as source_file**

In `src/views.rs:51-59`, add the alias:

```rust
fn register_raw_view(conn: &Connection, file_list: &str) -> Result<()> {
    let sql = format!(
        "CREATE VIEW raw_records AS
        SELECT json, filename AS source_file
        FROM read_json({file_list}, format='newline_delimited', records=false, filename=true, union_by_name=true, ignore_errors=true)"
    );
    conn.execute_batch(&sql)
        .context("Failed to create raw_records view")?;
    Ok(())
}
```

- [ ] **Step 3: Migrate register_messages_view to json_extract**

Replace `src/views.rs:70-116` with:

```rust
fn register_messages_view(conn: &Connection) -> Result<()> {
    let sql = format!("CREATE VIEW messages AS
        WITH string_msgs AS (
            SELECT
                json_extract_string(json, '$.sessionId') AS session_id,
                {PROJECT_EXPR} AS project,
                json_extract_string(json, '$.uuid') AS uuid,
                json_extract_string(json, '$.parentUuid') AS parent_uuid,
                json_extract_string(json, '$.type') AS type,
                json_extract_string(json, '$.timestamp') AS timestamp,
                json_extract_string(json, '$.message.content') AS text,
                CAST(0 AS BIGINT) AS tool_count,
                json_extract_string(json, '$.message.model') AS model
            FROM raw_records
            WHERE json_extract_string(json, '$.type') IN ('user', 'assistant')
            AND json_type(json_extract(json, '$.message.content')) = 'VARCHAR'
        ),
        array_msgs AS (
            SELECT
                json_extract_string(json, '$.sessionId') AS session_id,
                {PROJECT_EXPR} AS project,
                json_extract_string(json, '$.uuid') AS uuid,
                json_extract_string(json, '$.parentUuid') AS parent_uuid,
                json_extract_string(json, '$.type') AS type,
                json_extract_string(json, '$.timestamp') AS timestamp,
                (SELECT json_extract_string(item, '$.text')
                 FROM (SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item)
                 WHERE json_extract_string(item, '$.type') = 'text'
                 LIMIT 1) AS text,
                CASE WHEN json_extract_string(json, '$.type') = 'assistant' THEN
                    (SELECT COUNT(*)
                     FROM (SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item)
                     WHERE json_extract_string(item, '$.type') = 'tool_use')
                ELSE CAST(0 AS BIGINT)
                END AS tool_count,
                json_extract_string(json, '$.message.model') AS model
            FROM raw_records
            WHERE json_extract_string(json, '$.type') IN ('user', 'assistant')
            AND json_type(json_extract(json, '$.message.content')) = 'ARRAY'
        )
        SELECT * FROM string_msgs
        UNION ALL
        SELECT * FROM array_msgs");
    conn.execute_batch(&sql)
        .context("Failed to create messages view")?;
    Ok(())
}
```

- [ ] **Step 4: Migrate register_tool_calls_view to json_extract**

Replace `src/views.rs:122-142`:

```rust
fn register_tool_calls_view(conn: &Connection) -> Result<()> {
    let sql = format!("CREATE VIEW tool_calls AS
        SELECT
            json_extract_string(json, '$.sessionId') AS session_id,
            {PROJECT_EXPR} AS project,
            json_extract_string(json, '$.uuid') AS message_uuid,
            json_extract_string(item, '$.id') AS tool_use_id,
            json_extract_string(item, '$.name') AS name,
            json_extract(item, '$.input') AS input,
            json_extract_string(json, '$.timestamp') AS timestamp
        FROM raw_records,
        LATERAL (
            SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item
        )
        WHERE json_extract_string(json, '$.type') = 'assistant'
        AND json_type(json_extract(json, '$.message.content')) = 'ARRAY'
        AND json_extract_string(item, '$.type') = 'tool_use'");
    conn.execute_batch(&sql)
        .context("Failed to create tool_calls view")?;
    Ok(())
}
```

- [ ] **Step 5: Migrate register_tool_results_view to json_extract**

Replace `src/views.rs:148-166`:

```rust
fn register_tool_results_view(conn: &Connection) -> Result<()> {
    let sql = format!("CREATE VIEW tool_results AS
        SELECT
            json_extract_string(json, '$.sessionId') AS session_id,
            {PROJECT_EXPR} AS project,
            json_extract_string(item, '$.tool_use_id') AS tool_use_id,
            COALESCE(CAST(json_extract(item, '$.is_error') AS BOOLEAN), false) AS is_error,
            json_extract_string(item, '$.content') AS content
        FROM raw_records,
        LATERAL (
            SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item
        )
        WHERE json_extract_string(json, '$.type') = 'user'
        AND json_type(json_extract(json, '$.message.content')) = 'ARRAY'
        AND json_extract_string(item, '$.type') = 'tool_result'");
    conn.execute_batch(&sql)
        .context("Failed to create tool_results view")?;
    Ok(())
}
```

- [ ] **Step 6: Run all tests**

Run: `cargo test`
Expected: All 46 tests pass. The json_extract calls should produce identical results to the struct dot notation when working with `read_json(..., records=false)` since DuckDB supports both access patterns on STRUCT types.

- [ ] **Step 7: Commit**

```
feat: migrate view SQL from struct dot notation to json_extract

Decouples views from DuckDB's auto-inferred STRUCT typing.
Required for persistent cache where json is stored as opaque JSON type.
Also renames filename -> source_file for consistency with cache schema.
```

---

## Task 2: Extract register_derived_views

Split `register_views` so the cache path can create derived views without recreating raw_records.

**Files:**
- Modify: `src/views.rs`

- [ ] **Step 1: Create register_derived_views function**

Add after `register_views` in `src/views.rs`:

```rust
/// Register only the derived views (messages, tool_calls, tool_results, sessions).
/// Assumes raw_records already exists (either as a view from read_json or as a
/// persistent table from the cache).
pub fn register_derived_views(conn: &Connection) -> Result<()> {
    register_messages_view(conn)?;
    register_tool_calls_view(conn)?;
    register_tool_results_view(conn)?;
    register_sessions_view(conn)?;
    Ok(())
}
```

- [ ] **Step 2: Refactor register_views to call register_derived_views**

```rust
pub fn register_views(conn: &Connection, files: &[PathBuf]) -> Result<()> {
    if files.is_empty() {
        return register_empty_views(conn);
    }

    let file_list = build_file_list(files);
    register_raw_view(conn, &file_list)?;
    register_derived_views(conn)?;

    Ok(())
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: All tests pass (pure refactor, no behavior change).

- [ ] **Step 4: Commit**

```
refactor: extract register_derived_views for cache reuse
```

---

## Task 3: Cache Module

Create `src/cache.rs` with persistent DB lifecycle: open/create, schema versioning, table creation.

**Files:**
- Create: `src/cache.rs`
- Modify: `src/lib.rs` (add module)

- [ ] **Step 1: Write test for cache creation**

Create `tests/cache_test.rs`:

```rust
use tempfile::TempDir;

fn cache_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[test]
fn creates_tables_on_first_open() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path()).unwrap();

    // Verify tables exist
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM information_schema.tables WHERE table_name IN ('cache_meta', 'file_registry', 'raw_records')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 3);
}

#[test]
fn version_check_passes_on_current() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path()).unwrap();
    drop(conn);

    // Second open should succeed without rebuilding
    let conn = cq::cache::open(dir.path()).unwrap();
    let version: i32 = conn
        .query_row("SELECT version FROM cache_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, cq::cache::SCHEMA_VERSION);
}

#[test]
fn version_mismatch_triggers_rebuild() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path()).unwrap();

    // Tamper with version
    conn.execute("UPDATE cache_meta SET version = 0", []).unwrap();

    // Insert a row that should disappear after rebuild
    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size) VALUES ('ghost.jsonl', 0, 0)",
        [],
    ).unwrap();
    drop(conn);

    let conn = cq::cache::open(dir.path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "rebuild should clear all data");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test cache`
Expected: Compile error, `cq::cache` doesn't exist yet.

- [ ] **Step 3: Implement cache.rs**

Create `src/cache.rs`:

```rust
use std::path::Path;
use anyhow::{Context, Result};
use duckdb::Connection;
use duckdb::OptionalExt;

pub const SCHEMA_VERSION: i32 = 1;

/// Open or create the cache database. Creates tables if missing,
/// rebuilds if schema version mismatches.
pub fn open(cache_dir: &Path) -> Result<Connection> {
    std::fs::create_dir_all(cache_dir)
        .context("Failed to create cache directory")?;

    let db_path = cache_dir.join("index.duckdb");
    let conn = Connection::open(&db_path)
        .context("Failed to open cache database")?;

    if needs_rebuild(&conn)? {
        rebuild(&conn)?;
    }

    Ok(conn)
}

/// Determine the cache directory path.
/// Uses CQ_CACHE_DIR env var if set, otherwise ~/.cache/cq.
pub fn cache_dir() -> Result<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("CQ_CACHE_DIR") {
        return Ok(std::path::PathBuf::from(dir));
    }
    let cache = dirs::cache_dir().context("Could not determine cache directory")?;
    Ok(cache.join("cq"))
}

fn needs_rebuild(conn: &Connection) -> Result<bool> {
    // Check if cache_meta table exists
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM information_schema.tables WHERE table_name = 'cache_meta'",
            [],
            |r| r.get(0),
        )?;

    if !table_exists {
        return Ok(true);
    }

    // Check version (handle empty table gracefully)
    let version: Option<i32> = conn
        .query_row(
            "SELECT version FROM cache_meta LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;

    match version {
        Some(v) if v == SCHEMA_VERSION => Ok(false),
        _ => Ok(true),
    }
}

fn rebuild(conn: &Connection) -> Result<()> {
    // Drop existing tables if they exist
    conn.execute_batch(
        "DROP TABLE IF EXISTS raw_records;
         DROP TABLE IF EXISTS file_registry;
         DROP TABLE IF EXISTS cache_meta;"
    )?;

    conn.execute_batch(
        "CREATE TABLE cache_meta (
            version INTEGER NOT NULL
        );

        CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT NOT NULL,
            file_size BIGINT NOT NULL,
            cwd TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        );

        CREATE TABLE raw_records (
            source_file TEXT NOT NULL,
            json JSON NOT NULL
        );"
    )?;

    conn.execute(
        "INSERT INTO cache_meta (version) VALUES (?)",
        [SCHEMA_VERSION],
    )?;

    Ok(())
}
```

- [ ] **Step 4: Add module to lib.rs**

Add `pub mod cache;` to `src/lib.rs`.

- [ ] **Step 5: Run tests**

Run: `cargo test cache`
Expected: All 3 cache tests pass.

- [ ] **Step 6: Commit**

```
feat: add cache module with persistent DB lifecycle
```

---

## Task 4: Indexer Module

File scanning, diffing against registry, incremental insert/delete, and cwd extraction.

**Files:**
- Create: `src/indexer.rs`
- Modify: `src/lib.rs` (add module)

- [ ] **Step 1: Write test for scan and diff**

Add to `tests/cache_test.rs`:

```rust
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Create a fake projects dir with one project containing fixture files.
fn setup_projects(fixtures: &[&str]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_dir).unwrap();
    for fixture in fixtures {
        let src = fixture_path(fixture);
        let dest = project_dir.join(fixture);
        std::fs::copy(&src, &dest).unwrap();
    }
    tmp
}

#[test]
fn index_new_files() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path()).unwrap();

    let stats = cq::indexer::sync(&conn, projects.path()).unwrap();
    assert_eq!(stats.added, 1);
    assert_eq!(stats.removed, 0);
    assert_eq!(stats.changed, 0);

    // raw_records should have rows
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM raw_records", [], |r| r.get(0))
        .unwrap();
    assert!(count > 0, "raw_records should have rows after indexing");

    // file_registry should have the file
    let reg_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(reg_count, 1);
}

#[test]
fn no_changes_is_noop() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path()).unwrap();

    cq::indexer::sync(&conn, projects.path()).unwrap();
    let stats = cq::indexer::sync(&conn, projects.path()).unwrap();
    assert_eq!(stats.added, 0);
    assert_eq!(stats.removed, 0);
    assert_eq!(stats.changed, 0);
}

#[test]
fn detects_deleted_files() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl", "error_session.jsonl"]);
    let conn = cq::cache::open(cache.path()).unwrap();

    cq::indexer::sync(&conn, projects.path()).unwrap();

    // Delete one file
    let project_dir = projects.path().join("-Users-test-myproject");
    std::fs::remove_file(project_dir.join("error_session.jsonl")).unwrap();

    let stats = cq::indexer::sync(&conn, projects.path()).unwrap();
    assert_eq!(stats.removed, 1);

    let reg_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(reg_count, 1, "only simple_session.jsonl should remain");
}

#[test]
fn detects_changed_files() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path()).unwrap();

    cq::indexer::sync(&conn, projects.path()).unwrap();

    // Append to the file to change mtime and size
    let project_dir = projects.path().join("-Users-test-myproject");
    let file_path = project_dir.join("simple_session.jsonl");
    let mut f = std::fs::OpenOptions::new().append(true).open(&file_path).unwrap();
    use std::io::Write;
    writeln!(f, "{{}}").unwrap();

    let stats = cq::indexer::sync(&conn, projects.path()).unwrap();
    assert_eq!(stats.changed, 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test cache`
Expected: Compile error, `cq::indexer` doesn't exist yet.

- [ ] **Step 3: Implement indexer.rs**

Create `src/indexer.rs`:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use duckdb::Connection;

#[derive(Debug, Default)]
pub struct SyncStats {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

struct FileInfo {
    mtime_ns: i64,
    file_size: i64,
}

/// Sync the cache with the filesystem. Scans all JSONL files under
/// projects_dir, diffs against file_registry, and updates raw_records.
pub fn sync(conn: &Connection, projects_dir: &Path) -> Result<SyncStats> {
    let disk_files = scan_filesystem(projects_dir)?;
    let registry = load_registry(conn)?;

    let mut stats = SyncStats::default();
    let mut to_add: Vec<PathBuf> = Vec::new();
    let mut to_remove: Vec<String> = Vec::new();

    // Find new and changed files
    for (path, info) in &disk_files {
        let path_str = path.to_string_lossy().to_string();
        match registry.get(&path_str) {
            None => {
                to_add.push(path.clone());
                stats.added += 1;
            }
            Some(reg) => {
                if reg.mtime_ns != info.mtime_ns || reg.file_size != info.file_size {
                    to_remove.push(path_str);
                    to_add.push(path.clone());
                    stats.changed += 1;
                }
            }
        }
    }

    // Find deleted files
    for path_str in registry.keys() {
        let path = PathBuf::from(path_str);
        if !disk_files.contains_key(&path) {
            to_remove.push(path_str.clone());
            stats.removed += 1;
        }
    }

    // Apply removals
    for path_str in &to_remove {
        conn.execute("DELETE FROM raw_records WHERE source_file = ?", [path_str])?;
        conn.execute("DELETE FROM file_registry WHERE file_path = ?", [path_str])?;
    }

    // Apply additions
    if !to_add.is_empty() {
        index_files(conn, &to_add)?;
    }

    Ok(stats)
}

/// Scan the filesystem for all JSONL files under the projects directory.
fn scan_filesystem(projects_dir: &Path) -> Result<HashMap<PathBuf, FileInfo>> {
    let mut files = HashMap::new();

    if !projects_dir.exists() {
        return Ok(files);
    }

    for project_entry in std::fs::read_dir(projects_dir)?.filter_map(|e| e.ok()) {
        if !project_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        for file_entry in std::fs::read_dir(project_entry.path())?
            .filter_map(|e| e.ok())
        {
            let path = file_entry.path();
            if path.extension().map(|e| e == "jsonl").unwrap_or(false) && path.is_file() {
                if let Ok(metadata) = std::fs::metadata(&path) {
                    let mtime_ns = metadata
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as i64)
                        .unwrap_or(0);
                    files.insert(
                        path,
                        FileInfo {
                            mtime_ns,
                            file_size: metadata.len() as i64,
                        },
                    );
                }
            }
        }
    }

    Ok(files)
}

/// Load the current file registry from the database.
fn load_registry(conn: &Connection) -> Result<HashMap<String, FileInfo>> {
    let mut stmt = conn.prepare(
        "SELECT file_path, mtime_ns, file_size FROM file_registry"
    )?;
    let mut registry = HashMap::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let mtime_ns: i64 = row.get(1)?;
        let file_size: i64 = row.get(2)?;
        registry.insert(path, FileInfo { mtime_ns, file_size });
    }
    Ok(registry)
}

/// Parse JSONL files with DuckDB's read_json and insert into raw_records.
/// Also extracts cwd and registers files in file_registry.
fn index_files(conn: &Connection, files: &[PathBuf]) -> Result<()> {
    for file in files {
        let path_str = file.to_string_lossy().to_string();
        let escaped = path_str.replace('\'', "''");

        // Insert raw records from this file
        let insert_sql = format!(
            "INSERT INTO raw_records (source_file, json)
             SELECT '{escaped}', json
             FROM read_json('{escaped}', format='newline_delimited', records=false, ignore_errors=true)"
        );
        conn.execute_batch(&insert_sql)
            .with_context(|| format!("Failed to index {path_str}"))?;

        // Extract cwd from first record that has one
        let cwd: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT json_extract_string(json, '$.cwd')
                     FROM raw_records
                     WHERE source_file = '{escaped}'
                     AND json_extract_string(json, '$.cwd') IS NOT NULL
                     LIMIT 1"
                ),
                [],
                |r| r.get(0),
            )
            .ok();

        // Get file metadata for registry
        let metadata = std::fs::metadata(file)?;
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let file_size = metadata.len() as i64;

        conn.execute(
            "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd) VALUES (?, ?, ?, ?)",
            duckdb::params![path_str, mtime_ns, file_size, cwd],
        )?;
    }

    Ok(())
}
```

- [ ] **Step 4: Add module to lib.rs**

Add `pub mod indexer;` to `src/lib.rs`.

- [ ] **Step 5: Run tests**

Run: `cargo test cache`
Expected: All 7 cache/indexer tests pass (3 cache + 4 indexer).

- [ ] **Step 6: Commit**

```
feat: add indexer module with incremental file sync
```

---

## Task 5: Wire Up Cache in db.rs and main.rs

Connect the cache and indexer to the main boot path. Add `--reindex` flag.

**Files:**
- Modify: `src/db.rs`
- Modify: `src/main.rs`
- Modify: `src/views.rs` (update PROJECT_EXPR for cwd)

- [ ] **Step 1: Update db.rs to use cache**

Replace `src/db.rs` entirely:

```rust
use anyhow::Result;
use duckdb::Connection;
use crate::cache;
use crate::indexer;
use crate::views;

pub struct DbSetup {
    pub conn: Connection,
    pub file_count: usize,
}

pub struct DbOptions {
    pub reindex: bool,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self { reindex: false }
    }
}

/// Set up a DuckDB connection with views registered.
///
/// Uses the persistent cache for fast incremental startup. Falls back
/// to in-memory mode if the projects dir is empty.
pub fn setup_connection(projects_dir: &std::path::Path, options: &DbOptions) -> Result<DbSetup> {
    let cache_dir = cache::cache_dir()?;
    let conn = cache::open(&cache_dir, options.reindex)?;

    let stats = indexer::sync(&conn, projects_dir)?;
    let file_count = stats.added + stats.changed;

    views::register_derived_views(&conn)?;

    Ok(DbSetup { conn, file_count })
}
```

- [ ] **Step 2: Update cache::open to accept reindex flag**

Modify `src/cache.rs` to add the reindex parameter:

```rust
pub fn open(cache_dir: &Path, force_rebuild: bool) -> Result<Connection> {
    std::fs::create_dir_all(cache_dir)
        .context("Failed to create cache directory")?;

    let db_path = cache_dir.join("index.duckdb");
    let conn = Connection::open(&db_path)
        .context("Failed to open cache database")?;

    if force_rebuild || needs_rebuild(&conn)? {
        rebuild(&conn)?;
    }

    Ok(conn)
}
```

Update the cache tests to pass `false` for the new parameter.

- [ ] **Step 3: Update PROJECT_EXPR for cwd lookup**

In `src/views.rs`, change the PROJECT_EXPR to use file_registry.cwd with fallback:

```rust
/// SQL expression to get the project path. Uses cwd from file_registry if
/// available, falls back to decoding the directory name from source_file.
const PROJECT_EXPR: &str =
    "COALESCE(
        (SELECT fr.cwd FROM file_registry fr WHERE fr.file_path = source_file),
        '/' || replace(regexp_extract(source_file, '.*/([^/]+)/[^/]+$', 1)[2:], '-', '/')
    )";
```

This subquery is evaluated per row but is a simple primary key lookup, so it's fast.

- [ ] **Step 4: Add --reindex flag to main.rs**

In `src/main.rs`, add the flag to Cli struct:

```rust
/// Force full reindex of session files
#[arg(long, global = true)]
reindex: bool,
```

Update the setup_connection call:

```rust
let provider = ClaudeProvider::new()?;

let options = db::DbOptions {
    reindex: cli.reindex,
    ..Default::default()
};

let start = std::time::Instant::now();
let db_setup = db::setup_connection(provider.base_dir(), &options)?;
let elapsed = start.elapsed();
if db_setup.file_count > 0 {
    eprintln!("Indexed {} files in {:.1}s", db_setup.file_count, elapsed.as_secs_f64());
} else {
    eprintln!("Cache up to date ({:.1}s)", elapsed.as_secs_f64());
}
```

Note: the progress message changes. "Scanned" becomes "Indexed" when files were actually processed, or "Cache up to date" when nothing changed.

- [ ] **Step 5: Remove provider dependency from db::setup_connection**

The `db::setup_connection` now takes `projects_dir: &Path` directly instead of a `&dyn TranscriptProvider`. The provider's `discover_files` and `register_views` methods are no longer used by the main path. The provider is still used for `base_dir()` and `list_projects()`.

Remove `provider` and `scope` parameters, update the call in main.rs. The `TranscriptProvider` trait's `register_views` and `discover_files` methods can stay for testing purposes.

- [ ] **Step 6: Run tests**

Run: `cargo test`
Expected: Some tests may fail due to changed db.rs API. Fix in next step.

- [ ] **Step 7: Update integration tests for cache**

In `tests/integration_test.rs`, update `cq_cmd` to set `CQ_CACHE_DIR`:

```rust
fn cq_cmd(tmp: &TempDir) -> Command {
    let cache = TempDir::new().unwrap();
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.env("CQ_PROJECTS_DIR", tmp.path());
    cmd.env("CQ_CACHE_DIR", cache.path());
    cmd
}
```

Note: since `cache` is a local variable that gets dropped, we need to use a persistent temp dir. Better approach: create a fixture that returns both dirs:

```rust
struct TestEnv {
    projects: TempDir,
    cache: TempDir,
}

fn setup_env(fixtures: &[&str]) -> TestEnv {
    let projects = TempDir::new().unwrap();
    let project_dir = projects.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_dir).unwrap();
    for fixture in fixtures {
        let src = fixture_path(fixture);
        let dest = project_dir.join(fixture);
        std::fs::copy(&src, &dest).unwrap();
    }
    let cache = TempDir::new().unwrap();
    TestEnv { projects, cache }
}

fn cq_cmd(env: &TestEnv) -> Command {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.env("CQ_PROJECTS_DIR", env.projects.path());
    cmd.env("CQ_CACHE_DIR", env.cache.path());
    cmd
}
```

Update all integration tests to use `TestEnv`. The `no_files_no_error` test needs its own empty `TestEnv`.

Also update the `progress_on_stderr_not_stdout` test: the stderr message changes from "Scanned" to "Indexed" (when files were processed) or "Cache up to date" (when nothing changed). Update the assertion to match:

```rust
assert!(
    stderr.contains("Indexed") || stderr.contains("Cache up to date"),
    "Expected progress on stderr, got: {stderr}"
);
```

- [ ] **Step 8: Update views_test.rs for source_file column**

The views_test.rs uses `register_views` with in-memory connections. Since `PROJECT_EXPR` now references `file_registry`, tests need a file_registry table too. Add a helper:

```rust
fn setup_db(fixture: &str) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    let path = fixture_path(fixture);

    // Create file_registry for PROJECT_EXPR fallback
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT,
            file_size BIGINT,
            cwd TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )"
    ).unwrap();

    cq::views::register_views(&conn, &[path]).unwrap();
    conn
}
```

- [ ] **Step 9: Run all tests**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 10: Commit**

```
feat: wire cache into boot path with --reindex flag

Startup now uses persistent DuckDB cache. On first run, indexes all
files (~3.6s). Subsequent runs diff mtime+size and re-parse only
changed files (~200ms with no changes). --reindex forces full rebuild.

Project paths now use cwd from session data instead of lossy directory
name decoding.
```

---

## Task 6: Verify End-to-End Performance

Manual verification that the cache delivers the expected speedup.

**Files:** None (verification only)

- [ ] **Step 1: Clean cache and do cold start**

```bash
rm -rf ~/.cache/cq
time cargo run --release -- tools Skill 2>&1 | tail -5
```

Expected: ~3.6s (same as before, building the cache).

- [ ] **Step 2: Warm start with no changes**

```bash
time cargo run --release -- tools Skill 2>&1 | tail -5
```

Expected: Under 1s. stderr says "Cache up to date".

- [ ] **Step 3: Test --reindex**

```bash
time cargo run --release -- tools Skill --reindex 2>&1 | tail -5
```

Expected: ~3.6s, full rebuild.

- [ ] **Step 4: Test project path accuracy**

```bash
cargo run --release -- sessions --limit 5
```

Expected: project column shows actual paths (with underscores, dots preserved) instead of the old lossy decode.

- [ ] **Step 5: Commit any fixes**

If any issues found during verification, fix and commit.
