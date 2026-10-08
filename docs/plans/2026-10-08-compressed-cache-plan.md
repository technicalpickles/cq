# Compressed Cache Implementation Plan

> **Superseded in one place:** this plan assumed nothing else could have the cache file open during a rebuild. Code review proved that wrong: once `open` drops its connection, nothing holds the file. The shipped `cache::open` serializes recreation through `index.lock` and re-checks once the lock is held, so the Task 2 snippet below is the pre-fix shape. `src/cache.rs` and docs/adr/0002-recreate-cache-file-on-rebuild.md are the current story.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `index.duckdb` use DuckDB's current storage format so `raw_records.json` is compressed, and recreate the file on rebuild so it stops carrying dead free blocks.

**Architecture:** `cache::open` opens the DB with `storage_compatibility_version = 'latest'`. Any time a rebuild is needed (schema version mismatch, `--reindex`, or a file stamped with an older storage version), cq deletes the file and creates a fresh one instead of dropping tables in place. A pinned `STORAGE_VERSION_TAG` constant plus a test makes a future DuckDB bump that changes the storage format fail loudly, so it's a conscious update rather than a silent one.

**Tech Stack:** Rust, `duckdb` crate `=1.10501.0` (DuckDB 1.5.1, bundled), `tempfile` + `assert_cmd` for tests.

---

## Background (read this first)

Measured on 2026-10-08 against a real 6.1 GB cache (2.9 GB of source JSONL, 780k records):

| | Size |
|---|---|
| `raw_records.json`, `Uncompressed` | ~4 GiB (16,161 × 256 KiB blocks) |
| Free blocks never returned to the OS | ~1.6 GiB (6,714 blocks) |
| FTS tables + everything else | ~100 MB |

- **Why uncompressed:** DuckDB files default to storage compatibility `v0.10.2` (tag `v1.0.0+`) even on DuckDB 1.5.1, for backwards compatibility. That format disables the ZSTD and DICT_FSST string codecs added in DuckDB 1.2/1.3. Median record is 2 KB, p90 7 KB, p99 59 KB, which is too big for the older dictionary/FSST paths, so they fall through to `Uncompressed`.
- **Why it never fixes itself:** storage version is fixed when the file is created. `cache::rebuild` drops tables inside the existing file, so `--reindex` and schema bumps keep the old format and the high-water-mark file size.
- **Proof the fix works:** copying `raw_records` into a DB attached with `STORAGE_VERSION 'v1.5.0'` took 5 s and produced 440 MiB (ZSTD, ~9× smaller). A full-scan query ran in 0.62 s vs 0.67–0.80 s on the old file.
- **Deleting the file is safe today:** cq mirrors JSONL on disk. `do_sync` deletes rows for files that no longer exist, so nothing in the cache is unrecoverable. If [#64](https://github.com/technicalpickles/cq/issues/64) (retain pruned sessions) ever lands, the recreate path in Task 2 must carry the retained data forward. Note that in the ADR.
- **Concurrency:** DuckDB holds an exclusive OS lock on the file for a read-write connection, so a second `cq` process fails at `Connection::open` ("Failed to open cache database") while one is running. That means when `cache::open` decides to rebuild, no other process has the file open. This is existing behavior and out of scope here.

Useful diagnostics (run with `cq --no-reindex sql "..."`):

```sql
-- storage format of the open cache
SELECT tags['storage_version'] FROM duckdb_databases() WHERE database_name = current_database();
-- per-column codec
SELECT column_name, compression, count(*) FROM pragma_storage_info('raw_records') GROUP BY ALL;
-- used vs free blocks
PRAGMA database_size;
```

## File Structure

- Modify: `src/cache.rs`. Split `open` into `connect` (config + extension dir), add `storage_version`, `STORAGE_VERSION_TAG`, and `remove_database`. Rename `rebuild` to `create_schema` and drop its `DROP ...` statements, since it now always runs against a fresh file.
- Modify: `tests/cache_test.rs`. Add storage-version, legacy-file, force-rebuild, and compression tests.
- Create: `docs/adr/0002-recreate-cache-file-on-rebuild.md`
- Modify: `CLAUDE.md`, the `cache.rs` line in Architecture and the "Persistent cache + incremental sync" Key pattern.

Work in a fresh worktree off `origin/main` (the local `worktrees/main` was 10+ commits behind when this was written; `cache.rs`, `db.rs`, `indexer.rs`, and `tests/cache_test.rs` were identical to `origin/main`).

---

### Task 1: Open new cache files at the latest storage version

**Files:**
- Modify: `src/cache.rs:1-35`
- Test: `tests/cache_test.rs`

- [ ] **Step 1: Write the failing tests**

Append to `tests/cache_test.rs`:

```rust
#[test]
fn fresh_cache_uses_current_storage_version() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();

    assert_eq!(
        cq::cache::storage_version(&conn).unwrap().as_deref(),
        Some(cq::cache::STORAGE_VERSION_TAG),
        "a new cache file should use DuckDB's latest storage format; if a DuckDB \
         upgrade changed the tag, update STORAGE_VERSION_TAG (existing caches \
         will be recreated once on first open)"
    );
}

#[test]
fn large_json_records_are_compressed() {
    let dir = cache_dir();
    let conn = cq::cache::open(dir.path(), false).unwrap();

    // ~11 KB per record, the size range that stayed Uncompressed under the
    // v0.10.2 storage format.
    conn.execute_batch(
        "INSERT INTO raw_records (source_file, json)
         SELECT 'big.jsonl', json_object('i', i, 'pad', repeat('lorem ipsum dolor ', 600))
         FROM range(2000) r(i);
         CHECKPOINT;",
    )
    .unwrap();

    let uncompressed: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_storage_info('raw_records')
             WHERE column_name = 'json' AND segment_type <> 'VALIDITY'
               AND compression = 'Uncompressed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(uncompressed, 0, "raw_records.json should not be stored Uncompressed");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test cache_test -- fresh_cache_uses_current_storage_version large_json_records_are_compressed`
Expected: compile error, `cannot find function storage_version` / `cannot find value STORAGE_VERSION_TAG` in `cq::cache`.

- [ ] **Step 3: Implement `connect`, `storage_version`, and the constant**

In `src/cache.rs`, change the imports and replace the top of the file through the end of `open` with:

```rust
use anyhow::{Context, Result};
use duckdb::{Config, Connection};
use duckdb::OptionalExt;
use std::path::Path;

pub const SCHEMA_VERSION: i32 = 7;

/// The `storage_version` tag DuckDB stamps on a file this build creates.
/// Storage format is fixed when a file is created, so a cache with any other
/// tag is recreated on open. If a DuckDB upgrade changes the tag, the
/// `fresh_cache_uses_current_storage_version` test fails; update this and
/// existing caches get recreated once in the newer format.
pub const STORAGE_VERSION_TAG: &str = "v1.5.0+";

/// Open or create the cache database. Creates tables if missing,
/// rebuilds if schema version mismatches or force_rebuild is true.
pub fn open(cache_dir: &Path, force_rebuild: bool) -> Result<Connection> {
    std::fs::create_dir_all(cache_dir).context("Failed to create cache directory")?;

    let db_path = cache_dir.join("index.duckdb");
    let conn = connect(cache_dir, &db_path)?;

    if force_rebuild || needs_rebuild(&conn)? {
        rebuild(&conn)?;
    }

    Ok(conn)
}

fn connect(cache_dir: &Path, db_path: &Path) -> Result<Connection> {
    // DuckDB defaults new files to the v0.10.2 storage format for backwards
    // compatibility, which disables the ZSTD/DICT_FSST string codecs and
    // leaves raw_records.json uncompressed. The setting only affects files
    // created by this connection; existing files keep their format.
    let config = Config::default()
        .with("storage_compatibility_version", "latest")
        .context("Failed to configure cache database")?;
    let conn =
        Connection::open_with_flags(db_path, config).context("Failed to open cache database")?;

    // Keep optional DuckDB extensions alongside cq's cache instead of writing
    // into the user's global ~/.duckdb directory. The FTS extension is fetched
    // lazily, only when `cq search` is used for the first time.
    let extension_dir = std::env::var("CQ_DUCKDB_EXTENSION_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| cache_dir.join("extensions"));
    std::fs::create_dir_all(&extension_dir)
        .context("Failed to create DuckDB extension directory")?;
    let escaped_extension_dir = extension_dir.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!(
        "SET extension_directory = '{escaped_extension_dir}'"
    ))
    .context("Failed to configure DuckDB extension directory")?;

    Ok(conn)
}

/// The storage format tag of the open cache file, e.g. `v1.5.0+`.
pub fn storage_version(conn: &Connection) -> Result<Option<String>> {
    conn.query_row(
        "SELECT tags['storage_version'] FROM duckdb_databases()
         WHERE database_name = current_database()",
        [],
        |r| r.get(0),
    )
    .optional()
    .context("Failed to read cache storage version")
    .map(Option::flatten)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test cache_test`
Expected: all pass, including the two new tests. If `fresh_cache_uses_current_storage_version` fails with a tag other than `v1.5.0+`, the crate version changed. Use the tag it reports and recheck the compression test.

- [ ] **Step 5: Commit**

```bash
git add src/cache.rs tests/cache_test.rs
git commit -m "perf(cache): create cache files at DuckDB's latest storage version"
```

---

### Task 2: Recreate the file on rebuild, including legacy-format files

**Files:**
- Modify: `src/cache.rs` (`open`, `needs_rebuild`, `rebuild` → `create_schema`, new `remove_database`)
- Test: `tests/cache_test.rs`

- [ ] **Step 1: Write the failing tests**

Append to `tests/cache_test.rs`:

```rust
/// Write a cache file in the given storage format whose cache_meta otherwise
/// passes the schema-version check, plus a marker table that only survives
/// if the file is NOT recreated.
fn write_cache_file(dir: &std::path::Path, storage: &str) {
    let config = duckdb::Config::default()
        .with("storage_compatibility_version", storage)
        .unwrap();
    let conn = duckdb::Connection::open_with_flags(dir.join("index.duckdb"), config).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE cache_meta (version INTEGER NOT NULL);
         INSERT INTO cache_meta VALUES ({});
         CREATE TABLE marker (x INTEGER);",
        cq::cache::SCHEMA_VERSION
    ))
    .unwrap();
}

fn marker_exists(conn: &duckdb::Connection) -> bool {
    conn.query_row(
        "SELECT count(*) > 0 FROM information_schema.tables WHERE table_name = 'marker'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn legacy_storage_file_is_recreated() {
    let dir = cache_dir();
    write_cache_file(dir.path(), "v0.10.2");

    let conn = cq::cache::open(dir.path(), false).unwrap();
    assert!(!marker_exists(&conn), "legacy-format file should be recreated");
    assert_eq!(
        cq::cache::storage_version(&conn).unwrap().as_deref(),
        Some(cq::cache::STORAGE_VERSION_TAG)
    );
}

#[test]
fn current_storage_file_is_kept() {
    let dir = cache_dir();
    write_cache_file(dir.path(), "latest");

    let conn = cq::cache::open(dir.path(), false).unwrap();
    assert!(marker_exists(&conn), "a current-format file at the current schema version must not be recreated");
}

#[test]
fn force_rebuild_recreates_file() {
    let dir = cache_dir();
    write_cache_file(dir.path(), "latest");

    let conn = cq::cache::open(dir.path(), true).unwrap();
    assert!(!marker_exists(&conn), "--reindex should start from a fresh file");
}
```

`current_storage_file_is_kept` is the control. It proves the storage-version check, not something else, is what recreates the legacy file. It passes before and after the change. Note that `cache_meta` here lacks the real columns; `open` never reads them, and the test only checks the marker.

- [ ] **Step 2: Run the tests to verify the right ones fail**

Run: `cargo test --test cache_test -- storage_file force_rebuild`
Expected: `legacy_storage_file_is_recreated` FAILS (marker survives, tag is `v1.0.0+`). `force_rebuild_recreates_file` FAILS (today's `rebuild` drops a fixed list of tables, so `marker` survives). `current_storage_file_is_kept` PASSES.

- [ ] **Step 3: Implement recreate-on-rebuild**

In `src/cache.rs`, replace `open` with:

```rust
/// Open or create the cache database. Recreates the file from scratch when
/// the schema version or storage format is out of date, or force_rebuild is
/// true. Recreating (not dropping tables in place) is what moves an old file
/// to the current storage format and returns its free blocks to the OS.
/// Safe because the cache only mirrors transcript files that are still on
/// disk; see docs/adr/0002-recreate-cache-file-on-rebuild.md.
pub fn open(cache_dir: &Path, force_rebuild: bool) -> Result<Connection> {
    std::fs::create_dir_all(cache_dir).context("Failed to create cache directory")?;

    let db_path = cache_dir.join("index.duckdb");
    let mut conn = connect(cache_dir, &db_path)?;

    if force_rebuild || needs_rebuild(&conn)? {
        // DuckDB holds an exclusive lock on the file while `conn` is open,
        // so no other cq process has it open at this point.
        drop(conn);
        remove_database(&db_path)?;
        conn = connect(cache_dir, &db_path)?;
        create_schema(&conn)?;
    }

    Ok(conn)
}
```

Add a storage check at the top of `needs_rebuild`:

```rust
fn needs_rebuild(conn: &Connection) -> Result<bool> {
    if storage_version(conn)?.as_deref() != Some(STORAGE_VERSION_TAG) {
        return Ok(true);
    }

    // Check if cache_meta table exists
    // ... rest unchanged ...
```

Add `remove_database` below `connect`:

```rust
/// Delete the cache file and its write-ahead log, if present.
fn remove_database(db_path: &Path) -> Result<()> {
    let wal_path = db_path.with_extension("duckdb.wal");
    for path in [db_path, wal_path.as_path()] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("Failed to remove {}", path.display()))
            }
        }
    }
    Ok(())
}
```

Rename `fn rebuild(conn: &Connection)` to `fn create_schema(conn: &Connection)` and delete its first `conn.execute_batch("DROP SCHEMA ... DROP TABLE IF EXISTS cache_meta;")?;` block. The `CREATE TABLE` batch and the `INSERT INTO cache_meta` stay exactly as they are.

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: all pass. `version_mismatch_triggers_rebuild` still passes because a schema-version mismatch now recreates the file, which also clears `file_registry`. If anything in `tests/integration_test.rs` relied on `--reindex` keeping non-cq tables around, it'll show up here.

- [ ] **Step 5: Lint**

Run: `cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/cache.rs tests/cache_test.rs
git commit -m "perf(cache): recreate the cache file on rebuild instead of dropping tables

Storage format is fixed when a DuckDB file is created, so dropping tables in
place kept old caches on the v0.10.2 format (raw_records.json uncompressed)
and never returned free blocks. Recreating moves them to the current format
on first open."
```

---

### Task 3: Record the decision and update docs

**Files:**
- Create: `docs/adr/0002-recreate-cache-file-on-rebuild.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Write the ADR**

Create `docs/adr/0002-recreate-cache-file-on-rebuild.md`:

```markdown
# Recreate the cache file on rebuild, at the latest storage format

**Status:** accepted

cq opens `index.duckdb` with `storage_compatibility_version = 'latest'`, and any rebuild (schema version mismatch, `--reindex`, or a file whose `storage_version` tag isn't `STORAGE_VERSION_TAG`) deletes the file and creates a fresh one instead of dropping tables in place.

## Why

- DuckDB defaults new files to the v0.10.2 storage format for backwards compatibility. That format disables the ZSTD and DICT_FSST string codecs, so `raw_records.json` (median record 2 KB, p99 59 KB) was stored uncompressed. On a real 6.1 GB cache, moving to the current format cut that column from ~4 GiB to 440 MiB with no read-time regression.
- Storage format is fixed at file creation, and DuckDB doesn't return free blocks to the OS. Dropping tables in place kept both problems forever; recreating fixes both.
- It's safe because the cache only mirrors transcript files still on disk (`do_sync` deletes rows for files that are gone), so a rebuild loses nothing.

## Consequences

- DuckDB CLIs older than 1.5 can't open the cache file. Only cq reads it.
- A DuckDB upgrade that changes the storage tag makes `fresh_cache_uses_current_storage_version` fail. Updating `STORAGE_VERSION_TAG` recreates every existing cache once, which costs one full reindex.
- If cq starts retaining sessions whose transcripts were pruned ([#64](https://github.com/technicalpickles/cq/issues/64)), a rebuild would destroy them. That work has to carry retained data across the recreate (or store it outside `index.duckdb`) before it ships.
```

- [ ] **Step 2: Update CLAUDE.md**

In the Architecture tree, change the `cache.rs` line to:

```
cache.rs          Persistent DuckDB cache at ~/.cache/cq/index.duckdb; schema + storage-format versioning, recreate-on-rebuild
```

In Key patterns, append to the end of the "**Persistent cache + incremental sync.**" bullet:

```
Rebuilds delete and recreate the file at DuckDB's latest storage format rather than dropping tables, because storage format is fixed at creation and the old default left `raw_records.json` uncompressed (docs/adr/0002-recreate-cache-file-on-rebuild.md).
```

Then check the `docs/cli-ux-conventions.md` "Keeping docs in sync" table. The "Sync / cache / scope behavior" row points to CLAUDE.md Key patterns (done) and `docs/design-principles.md` only if a default changes. No user-facing default changes here, so nothing else should need to move.

- [ ] **Step 3: Commit**

```bash
git add docs/adr/0002-recreate-cache-file-on-rebuild.md CLAUDE.md
git commit -m "docs: ADR for recreating the cache file at the latest storage format"
```

---

### Task 4: Verify on real data

No code changes. This produces the before/after numbers for the PR description.

- [ ] **Step 1: Record the baseline**

```bash
ls -la ~/Library/Caches/cq/index.duckdb
cq --no-reindex --table sql "PRAGMA database_size"
```

Expected (as of 2026-10-08): ~6.1 GB file, `used_blocks` ~16.6k, `free_blocks` ~6.7k.

- [ ] **Step 2: Build into a scratch cache and time it**

```bash
cargo build --release
# reuse the already-downloaded FTS extension instead of fetching it again
export CQ_DUCKDB_EXTENSION_DIR=~/Library/Caches/cq/extensions
time CQ_CACHE_DIR="$TMPDIR/cq-scratch" ./target/release/cq --all sessions --limit 1
ls -la "$TMPDIR/cq-scratch/index.duckdb"
CQ_CACHE_DIR="$TMPDIR/cq-scratch" ./target/release/cq --no-reindex --table sql \
  "SELECT column_name, compression, count(*) FROM pragma_storage_info('raw_records') WHERE column_name='json' GROUP BY ALL"
```

Expected: no `Uncompressed` rows for `json`. The file should land under ~1 GB before FTS. Record the wall time of the first full index.

- [ ] **Step 3: Check search builds and works in the scratch cache**

```bash
time CQ_CACHE_DIR="$TMPDIR/cq-scratch" ./target/release/cq --all search "storage version" --limit 3
ls -la "$TMPDIR/cq-scratch/index.duckdb"
```

Expected: results come back. Record the file size with FTS built.

- [ ] **Step 4: Spot-check query speed against the old cache**

Run each command twice and keep the second timing:

```bash
Q="SELECT count(*) FILTER (json_extract_string(json,'\$.type')='assistant') FROM raw_records"
time cq --no-reindex sql "$Q"
time CQ_CACHE_DIR="$TMPDIR/cq-scratch" ./target/release/cq --no-reindex sql "$Q"
```

Expected: the compressed cache is the same speed or faster (0.62 s vs 0.67–0.80 s in the 2026-10-08 measurement).

- [ ] **Step 5: Upgrade the real cache**

```bash
time ./target/release/cq --all sessions --limit 1   # first open sees v1.0.0+, recreates, reindexes
ls -la ~/Library/Caches/cq/index.duckdb
cq --no-reindex --table sql "SELECT tags['storage_version'] FROM duckdb_databases() WHERE database_name = current_database()"
```

Expected: tag `v1.5.0+`, file roughly the scratch size. Clean up with `rm -rf "$TMPDIR/cq-scratch"`.

- [ ] **Step 6: Open the PR**

Use the `git:pull-request` skill. Title: `perf(cache): compress the cache by recreating it at DuckDB's latest storage format`. Body: the before/after table from Steps 1–5 plus a link to [#64](https://github.com/technicalpickles/cq/issues/64) for the retention interaction. The PR title lint requires a conventional-commit prefix, and `perf:` gets a patch release from release-please.

---

## Follow-ups (not in this plan)

- **Free-block growth between rebuilds.** Re-indexing a changed session file deletes and reinserts all its rows, and FTS generations alternate. Freed blocks get reused, but the file never shrinks below its high-water mark. After this ships, watch `PRAGMA database_size` for a week or two before deciding whether cq needs a periodic compaction (copy into a fresh file) or append-only indexing for growing session files.
- **Concurrent cq invocations** fail at `Connection::open` because DuckDB locks the file exclusively. This is pre-existing, unrelated to compression, and worth its own issue if it bites.
