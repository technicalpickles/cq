# Smart Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace CQ's always-reindex behavior with smart sync: mtime-based change detection, file locking for write contention, and three sync modes (auto/force/skip).

**Architecture:** Add a `SyncMode` enum and `SyncScope` enum that flow from CLI flags through `db::setup_connection()` into `indexer::sync()`. The indexer gains a fast-path mtime check against `last_sync_at` stored in `cache_meta`, and wraps writes in a file lock. When the lock is busy during auto-sync, the indexer skips the sync and returns a flag so the caller can report it.

**Tech Stack:** Rust, DuckDB, fs2 (file locking), clap (CLI flags)

---

### Task 1: Add fs2 dependency

**Files:**
- Modify: `Cargo.toml:11` (dependencies section)

- [ ] **Step 1: Add fs2 to dependencies**

Add `fs2` to the `[dependencies]` section of `Cargo.toml`:

```toml
fs2 = "0.4"
```

Place it after the `duckdb` line. The dependencies section should look like:

```toml
[dependencies]
duckdb = { version = "1", features = ["bundled", "json"] }
fs2 = "0.4"
clap = { version = "4", features = ["derive"] }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: Compiles successfully, fs2 is downloaded and resolved.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add fs2 dependency for file locking"
```

---

### Task 2: Add `last_sync_at` to cache schema

**Files:**
- Modify: `src/cache.rs:6` (SCHEMA_VERSION), `src/cache.rs:63-96` (rebuild function), `src/cache.rs:71-88` (CREATE TABLE statements)

- [ ] **Step 1: Write the failing test**

Add to `tests/cache_test.rs`:

```rust
#[test]
fn cache_meta_has_last_sync_at() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();

    // last_sync_at should exist and default to 0
    let last_sync: i64 = conn
        .query_row("SELECT last_sync_at FROM cache_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(last_sync, 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test cache_meta_has_last_sync_at -- --nocapture`
Expected: FAIL with "column last_sync_at does not exist" or similar.

- [ ] **Step 3: Bump SCHEMA_VERSION and add last_sync_at column**

In `src/cache.rs`, change line 6:

```rust
pub const SCHEMA_VERSION: i32 = 2;
```

In the `rebuild` function, change the `CREATE TABLE cache_meta` statement to:

```rust
    conn.execute_batch(
        "CREATE TABLE cache_meta (
            version INTEGER NOT NULL,
            last_sync_at BIGINT NOT NULL DEFAULT 0
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
```

- [ ] **Step 4: Add read/write helpers for last_sync_at**

Add these public functions to `src/cache.rs`:

```rust
/// Read the last_sync_at timestamp from cache_meta.
/// Returns 0 if no value is stored (first run).
pub fn last_sync_at(conn: &Connection) -> Result<i64> {
    let ts: i64 = conn
        .query_row("SELECT last_sync_at FROM cache_meta LIMIT 1", [], |r| r.get(0))
        .with_context(|| "Failed to read last_sync_at")?;
    Ok(ts)
}

/// Update the last_sync_at timestamp in cache_meta.
pub fn set_last_sync_at(conn: &Connection, ts: i64) -> Result<()> {
    conn.execute("UPDATE cache_meta SET last_sync_at = ?", [ts])?;
    Ok(())
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test cache_meta_has_last_sync_at -- --nocapture`
Expected: PASS

- [ ] **Step 6: Run all tests to check for regressions**

Run: `cargo test`
Expected: All tests pass. The version bump triggers a rebuild on existing databases, which is the correct behavior.

- [ ] **Step 7: Commit**

```bash
git add src/cache.rs tests/cache_test.rs
git commit -m "feat: add last_sync_at to cache_meta schema (v2)"
```

---

### Task 3: Add `SyncMode` enum and `--no-reindex` flag

**Files:**
- Modify: `src/db.rs:13-15` (DbOptions)
- Modify: `src/main.rs:26-28` (CLI flags)

- [ ] **Step 1: Replace `reindex: bool` with `SyncMode` in db.rs**

Replace the `DbOptions` struct in `src/db.rs`:

```rust
/// Controls how the indexer decides whether to sync.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncMode {
    /// Check mtimes, try-lock, skip if busy. The default.
    Auto,
    /// Force full sync, wait for lock.
    Force,
    /// Skip sync entirely, use cached data.
    Skip,
}

pub struct DbOptions {
    pub sync_mode: SyncMode,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self { sync_mode: SyncMode::Auto }
    }
}
```

- [ ] **Step 2: Update setup_connection to pass sync_mode**

In `src/db.rs`, update `setup_connection` to pass `Force` as the force_rebuild flag to `cache::open`:

```rust
pub fn setup_connection(projects_dir: &std::path::Path, options: &DbOptions) -> Result<DbSetup> {
    let cache_dir = cache::cache_dir()?;
    let force_rebuild = options.sync_mode == SyncMode::Force;
    let conn = cache::open(&cache_dir, force_rebuild)?;

    let stats = indexer::sync(&conn, projects_dir)?;
    let file_count = stats.added + stats.changed;

    views::register_derived_views(&conn)?;

    Ok(DbSetup { conn, file_count, total_files: stats.total })
}
```

- [ ] **Step 3: Add `--no-reindex` flag to CLI**

In `src/main.rs`, replace the `reindex` field in the `Cli` struct:

```rust
    /// Force full reindex of session files
    #[arg(long, global = true, conflicts_with = "no_reindex")]
    reindex: bool,

    /// Skip sync entirely, use cached data
    #[arg(long, global = true, conflicts_with = "reindex")]
    no_reindex: bool,
```

- [ ] **Step 4: Update main.rs to construct SyncMode**

In `src/main.rs`, replace the `DbOptions` construction (around line 205):

```rust
    let sync_mode = if cli.reindex {
        db::SyncMode::Force
    } else if cli.no_reindex {
        db::SyncMode::Skip
    } else {
        db::SyncMode::Auto
    };

    let options = db::DbOptions {
        sync_mode,
        ..Default::default()
    };
```

- [ ] **Step 5: Verify it compiles and tests pass**

Run: `cargo test`
Expected: All tests pass. Behavior is unchanged since we haven't wired the new modes into the indexer yet.

- [ ] **Step 6: Commit**

```bash
git add src/db.rs src/main.rs
git commit -m "feat: add SyncMode enum and --no-reindex flag"
```

---

### Task 4: Add SyncScope enum

**Files:**
- Create: `src/sync_scope.rs`
- Modify: `src/lib.rs:1` (add module)

- [ ] **Step 1: Create sync_scope module**

Create `src/sync_scope.rs`:

```rust
use std::path::PathBuf;

/// Controls which project directories the indexer checks.
#[derive(Debug, Clone)]
pub enum SyncScope {
    /// Scan all project directories. Default for unscoped queries.
    All,
    /// Scan specific project directories only.
    Projects(Vec<PathBuf>),
    /// Check a single specific file only.
    File(PathBuf),
}
```

- [ ] **Step 2: Add module to lib.rs**

In `src/lib.rs`, add after the `pub mod indexer;` line:

```rust
pub mod sync_scope;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: Compiles successfully.

- [ ] **Step 4: Commit**

```bash
git add src/sync_scope.rs src/lib.rs
git commit -m "feat: add SyncScope enum for targeted sync"
```

---

### Task 5: Implement mtime-based fast path in indexer

**Files:**
- Modify: `src/indexer.rs:19-70` (sync function)

- [ ] **Step 1: Write the failing test**

Add to `tests/cache_test.rs`:

```rust
#[test]
fn auto_sync_skips_when_nothing_changed() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    // First sync indexes files
    let result = cq::indexer::sync(
        &conn,
        projects.path(),
        cq::db::SyncMode::Auto,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    ).unwrap();
    assert_eq!(result.stats.added, 1);
    assert!(!result.skipped);

    // Second sync with no changes should skip (fast path)
    let result = cq::indexer::sync(
        &conn,
        projects.path(),
        cq::db::SyncMode::Auto,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    ).unwrap();
    assert_eq!(result.stats.added, 0);
    assert_eq!(result.stats.changed, 0);
    assert_eq!(result.stats.removed, 0);
}

#[test]
fn force_sync_always_scans() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    // First sync
    cq::indexer::sync(
        &conn,
        projects.path(),
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    ).unwrap();

    // Force sync always scans even with no changes
    let result = cq::indexer::sync(
        &conn,
        projects.path(),
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    ).unwrap();
    // Stats reflect what the scan found (no new files, but it did scan)
    assert!(!result.skipped);
}

#[test]
fn skip_sync_returns_immediately() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    let result = cq::indexer::sync(
        &conn,
        projects.path(),
        cq::db::SyncMode::Skip,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    ).unwrap();
    assert!(result.skipped);
    assert_eq!(result.stats.added, 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test auto_sync_skips force_sync_always skip_sync_returns -- --nocapture`
Expected: FAIL because `sync()` doesn't accept the new parameters yet.

- [ ] **Step 3: Update SyncStats to SyncResult and add mtime/lock logic**

Replace the contents of `src/indexer.rs` with:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use duckdb::Connection;
use fs2::FileExt;

use crate::cache;
use crate::db::SyncMode;
use crate::sync_scope::SyncScope;

#[derive(Debug, Default)]
pub struct SyncStats {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub total: usize,
}

/// Result of a sync operation, including whether it was skipped.
#[derive(Debug)]
pub struct SyncResult {
    pub stats: SyncStats,
    /// True if the sync was skipped (Skip mode or lock busy).
    pub skipped: bool,
    /// True if the sync was skipped specifically because the lock was busy.
    pub lock_busy: bool,
}

struct FileInfo {
    mtime_ns: i64,
    file_size: i64,
}

/// Sync the cache with the filesystem.
///
/// Behavior depends on `mode`:
/// - `Auto`: check directory mtimes, try-lock, skip if nothing changed or lock busy
/// - `Force`: always scan, wait for lock with timeout
/// - `Skip`: return immediately without touching the database
pub fn sync(
    conn: &Connection,
    projects_dir: &Path,
    mode: SyncMode,
    scope: SyncScope,
    cache_dir: &Path,
) -> Result<SyncResult> {
    // Skip mode: return immediately
    if mode == SyncMode::Skip {
        return Ok(SyncResult {
            stats: SyncStats::default(),
            skipped: true,
            lock_busy: false,
        });
    }

    // Auto mode: check if anything changed via directory mtimes
    if mode == SyncMode::Auto {
        let last_sync = cache::last_sync_at(conn)?;
        let max_mtime = max_dir_mtime(projects_dir, &scope)?;
        if max_mtime <= last_sync {
            // Nothing changed, skip the full scan
            let total = count_registry(conn)?;
            return Ok(SyncResult {
                stats: SyncStats { total, ..Default::default() },
                skipped: false,
                lock_busy: false,
            });
        }
    }

    // Acquire file lock
    let lock_path = cache_dir.join("index.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .context("Failed to open lock file")?;

    let lock_acquired = match mode {
        SyncMode::Auto => {
            // Non-blocking try
            lock_file.try_lock_exclusive().is_ok()
        }
        SyncMode::Force => {
            // Blocking with timeout (5 seconds)
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if lock_file.try_lock_exclusive().is_ok() {
                    break true;
                }
                if std::time::Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        SyncMode::Skip => unreachable!(),
    };

    if !lock_acquired {
        if mode == SyncMode::Force {
            anyhow::bail!("index locked by another process after 5s, try again shortly");
        }
        // Auto mode: use cached data
        let total = count_registry(conn)?;
        return Ok(SyncResult {
            stats: SyncStats { total, ..Default::default() },
            skipped: true,
            lock_busy: true,
        });
    }

    // Lock acquired, do the full scan
    let result = do_sync(conn, projects_dir, &scope);

    // Update last_sync_at on success
    if let Ok(ref sync_result) = result {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        cache::set_last_sync_at(conn, now_ns)?;
        let _ = sync_result; // used above
    }

    // Release lock (drop does this, but be explicit)
    let _ = lock_file.unlock();

    let stats = result?;
    Ok(SyncResult {
        stats,
        skipped: false,
        lock_busy: false,
    })
}

/// The actual sync logic, extracted so the lock wraps it cleanly.
fn do_sync(conn: &Connection, projects_dir: &Path, scope: &SyncScope) -> Result<SyncStats> {
    let disk_files = scan_filesystem(projects_dir, scope)?;
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

    // Find deleted files (only check files within scope)
    for path_str in registry.keys() {
        let path = PathBuf::from(path_str);
        if in_scope(&path, projects_dir, scope) && !disk_files.contains_key(&path) {
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

    stats.total = if matches!(scope, SyncScope::All) {
        // For All scope, count everything in the registry after sync
        count_registry(conn)?
    } else {
        disk_files.len()
    };

    Ok(stats)
}

/// Check if a file path falls within the given sync scope.
fn in_scope(path: &Path, projects_dir: &Path, scope: &SyncScope) -> bool {
    match scope {
        SyncScope::All => path.starts_with(projects_dir),
        SyncScope::Projects(dirs) => dirs.iter().any(|d| path.starts_with(d)),
        SyncScope::File(f) => path == f,
    }
}

/// Get the maximum mtime across project directories within scope.
fn max_dir_mtime(projects_dir: &Path, scope: &SyncScope) -> Result<i64> {
    let dirs_to_check: Vec<PathBuf> = match scope {
        SyncScope::All => {
            if !projects_dir.exists() {
                return Ok(0);
            }
            std::fs::read_dir(projects_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect()
        }
        SyncScope::Projects(dirs) => dirs.clone(),
        SyncScope::File(f) => {
            // For a single file, just stat the file itself
            if let Ok(meta) = std::fs::metadata(f) {
                let mtime = meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                return Ok(mtime);
            }
            return Ok(0);
        }
    };

    let mut max_mtime: i64 = 0;
    for dir in &dirs_to_check {
        if let Ok(meta) = std::fs::metadata(dir) {
            let mtime = meta.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            if mtime > max_mtime {
                max_mtime = mtime;
            }
        }
    }

    Ok(max_mtime)
}

/// Count total files in the registry.
fn count_registry(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_registry", [], |r| r.get(0)
    )?;
    Ok(count as usize)
}

/// Scan the filesystem for JSONL files, scoped to the given SyncScope.
fn scan_filesystem(projects_dir: &Path, scope: &SyncScope) -> Result<HashMap<PathBuf, FileInfo>> {
    match scope {
        SyncScope::All => scan_all(projects_dir),
        SyncScope::Projects(dirs) => {
            let mut files = HashMap::new();
            for dir in dirs {
                let dir_files = scan_directory(dir)?;
                files.extend(dir_files);
            }
            Ok(files)
        }
        SyncScope::File(path) => {
            let mut files = HashMap::new();
            if path.is_file() {
                if let Ok(metadata) = std::fs::metadata(path) {
                    let mtime_ns = metadata.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as i64)
                        .unwrap_or(0);
                    files.insert(path.clone(), FileInfo {
                        mtime_ns,
                        file_size: metadata.len() as i64,
                    });
                }
            }
            Ok(files)
        }
    }
}

/// Scan all project directories under the base projects_dir.
fn scan_all(projects_dir: &Path) -> Result<HashMap<PathBuf, FileInfo>> {
    let mut files = HashMap::new();

    if !projects_dir.exists() {
        return Ok(files);
    }

    for project_entry in std::fs::read_dir(projects_dir)?.filter_map(|e| e.ok()) {
        if !project_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir_files = scan_directory(&project_entry.path())?;
        files.extend(dir_files);
    }

    Ok(files)
}

/// Scan a single directory for JSONL files.
fn scan_directory(dir: &Path) -> Result<HashMap<PathBuf, FileInfo>> {
    let mut files = HashMap::new();

    if !dir.exists() {
        return Ok(files);
    }

    for file_entry in std::fs::read_dir(dir)?.filter_map(|e| e.ok()) {
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

    Ok(files)
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
             SELECT '{escaped}', CAST(json AS JSON)
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

- [ ] **Step 4: Run the new tests**

Run: `cargo test auto_sync_skips force_sync_always skip_sync_returns -- --nocapture`
Expected: PASS

- [ ] **Step 5: Run all tests, fix any callers of old sync() signature**

Run: `cargo test`
Expected: Compilation errors in `tests/cache_test.rs` and `src/db.rs` because `sync()` now takes more arguments. Fix those in the next task.

---

### Task 6: Update all callers of indexer::sync()

**Files:**
- Modify: `src/db.rs:26-36` (setup_connection)
- Modify: `tests/cache_test.rs` (all test functions that call sync)

- [ ] **Step 1: Update db::setup_connection**

Replace `setup_connection` in `src/db.rs`:

```rust
use crate::sync_scope::SyncScope;

/// Set up a DuckDB connection with views registered.
///
/// Uses the persistent cache for fast incremental startup.
pub fn setup_connection(
    projects_dir: &std::path::Path,
    options: &DbOptions,
    scope: SyncScope,
) -> Result<DbSetup> {
    let cache_dir = cache::cache_dir()?;
    let force_rebuild = options.sync_mode == SyncMode::Force;
    let conn = cache::open(&cache_dir, force_rebuild)?;

    let result = indexer::sync(&conn, projects_dir, options.sync_mode, scope, &cache_dir)?;
    let file_count = result.stats.added + result.stats.changed;

    views::register_derived_views(&conn)?;

    Ok(DbSetup {
        conn,
        file_count,
        total_files: result.stats.total,
        skipped: result.skipped,
        lock_busy: result.lock_busy,
    })
}
```

- [ ] **Step 2: Update DbSetup to include sync info**

In `src/db.rs`, update the `DbSetup` struct:

```rust
pub struct DbSetup {
    pub conn: Connection,
    pub file_count: usize,
    pub total_files: usize,
    /// True if sync was skipped (Skip mode or lock busy).
    pub skipped: bool,
    /// True if sync was skipped because the lock was busy.
    pub lock_busy: bool,
}
```

- [ ] **Step 3: Update main.rs to pass SyncScope and handle new DbSetup fields**

In `src/main.rs`, update the `setup_connection` call (around line 211):

```rust
    let start = std::time::Instant::now();
    let db_setup = db::setup_connection(provider.base_dir(), &options, cq::sync_scope::SyncScope::All)?;
    let elapsed = start.elapsed();
    if db_setup.lock_busy {
        eprintln!("index busy, using cached data (re-run with --reindex to force)");
    } else if db_setup.skipped {
        // --no-reindex: silence
    } else if db_setup.file_count > 0 {
        eprintln!("Synced {} new files ({} total, {:.1}s)", db_setup.file_count, db_setup.total_files, elapsed.as_secs_f64());
    } else {
        eprintln!("Loaded {} files ({:.1}s)", db_setup.total_files, elapsed.as_secs_f64());
    }
```

- [ ] **Step 4: Update cache_test.rs callers**

Update existing tests in `tests/cache_test.rs` that call `sync()` to use the new signature. For existing tests that don't care about sync mode, use `SyncMode::Force` and `SyncScope::All` to match the old behavior. For example, update `index_new_files`:

```rust
#[test]
fn index_new_files() {
    let cache = cache_dir();
    let projects = setup_projects(&["simple_session.jsonl"]);
    let conn = cq::cache::open(cache.path(), false).unwrap();

    let result = cq::indexer::sync(
        &conn,
        projects.path(),
        cq::db::SyncMode::Force,
        cq::sync_scope::SyncScope::All,
        cache.path(),
    ).unwrap();
    assert_eq!(result.stats.added, 1);
    assert_eq!(result.stats.removed, 0);
    assert_eq!(result.stats.changed, 0);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM raw_records", [], |r| r.get(0))
        .unwrap();
    assert!(count > 0, "raw_records should have rows after indexing");

    let reg_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_registry", [], |r| r.get(0))
        .unwrap();
    assert_eq!(reg_count, 1);
}
```

Apply the same pattern to `no_changes_is_noop`, `detects_deleted_files`, and `detects_changed_files`: change `cq::indexer::sync(&conn, projects.path())` to `cq::indexer::sync(&conn, projects.path(), cq::db::SyncMode::Force, cq::sync_scope::SyncScope::All, cache.path())` and access stats via `.stats.field` instead of `.field`.

- [ ] **Step 5: Run all tests**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/db.rs src/main.rs src/indexer.rs tests/cache_test.rs
git commit -m "feat: wire SyncMode and SyncScope through to indexer"
```

---

### Task 7: Derive SyncScope from CLI flags

**Files:**
- Modify: `src/main.rs` (between scope construction and setup_connection call)
- Modify: `src/claude_provider.rs` (add helper to resolve project path to directory)

- [ ] **Step 1: Add project_dir_for_query to ClaudeProvider**

Add this method to the `impl ClaudeProvider` block in `src/claude_provider.rs`:

```rust
    /// Given a project query string (as passed to --project), return the
    /// matching project directories on disk. Used by SyncScope::Projects.
    pub fn project_dirs_for_query(&self, query: &str) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if !self.base_dir.exists() {
            return dirs;
        }
        if let Ok(entries) = std::fs::read_dir(&self.base_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let dir_name = entry.file_name().to_string_lossy().to_string();
                if self.matches_project(&dir_name, query) {
                    dirs.push(entry.path());
                }
            }
        }
        dirs
    }
```

- [ ] **Step 2: Derive SyncScope in main.rs**

In `src/main.rs`, replace the hardcoded `SyncScope::All` with scope derivation. Place this before the `setup_connection` call:

```rust
    let sync_scope = if cli.reindex {
        // --reindex always scans everything
        cq::sync_scope::SyncScope::All
    } else if let Some(ref p) = scope.project {
        let dirs = provider.project_dirs_for_query(p);
        if dirs.is_empty() {
            cq::sync_scope::SyncScope::All
        } else {
            cq::sync_scope::SyncScope::Projects(dirs)
        }
    } else {
        cq::sync_scope::SyncScope::All
    };

    let start = std::time::Instant::now();
    let db_setup = db::setup_connection(provider.base_dir(), &options, sync_scope)?;
```

- [ ] **Step 3: Run all tests**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/claude_provider.rs
git commit -m "feat: derive SyncScope from --project flag"
```

---

### Task 8: Add integration tests for sync modes

**Files:**
- Modify: `tests/integration_test.rs`

- [ ] **Step 1: Read current integration tests for patterns**

Read `tests/integration_test.rs` to understand the `assert_cmd` patterns used.

- [ ] **Step 2: Add integration tests for --no-reindex and --reindex**

Add to `tests/integration_test.rs`:

```rust
#[test]
fn no_reindex_skips_sync() {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.arg("--no-reindex").arg("sessions").arg("--limit").arg("1");
    cmd.assert().success();
    // stderr should NOT contain "Synced" or "Loaded"
    cmd.assert().stderr(predicates::str::contains("Synced").not());
}

#[test]
fn reindex_and_no_reindex_conflict() {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.arg("--reindex").arg("--no-reindex").arg("sessions");
    cmd.assert().failure();
    cmd.assert().stderr(predicates::str::contains("cannot be used with"));
}
```

- [ ] **Step 3: Run the new integration tests**

Run: `cargo test no_reindex_skips reindex_and_no_reindex -- --nocapture`
Expected: PASS

- [ ] **Step 4: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add tests/integration_test.rs
git commit -m "test: add integration tests for --no-reindex and flag conflicts"
```

---

### Task 9: Update CLI help text

**Files:**
- Modify: `src/main.rs:26-28` (flag help strings)

- [ ] **Step 1: Update help text for sync flags**

In `src/main.rs`, update the help text for the reindex flags to follow the CLI UX conventions:

```rust
    /// Force full reindex of session files (waits for lock if index is busy)
    #[arg(long, global = true, conflicts_with = "no_reindex")]
    reindex: bool,

    /// Skip sync entirely, use cached data (fastest, no lock contention)
    #[arg(long, global = true, conflicts_with = "reindex")]
    no_reindex: bool,
```

- [ ] **Step 2: Verify help output looks right**

Run: `cargo run -- --help`
Expected: Both `--reindex` and `--no-reindex` appear with clear descriptions.

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "docs: update help text for sync mode flags"
```
