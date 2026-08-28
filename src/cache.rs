use anyhow::{Context, Result};
use duckdb::Connection;
use duckdb::OptionalExt;
use std::path::Path;

pub const SCHEMA_VERSION: i32 = 6;

/// Open or create the cache database. Creates tables if missing,
/// rebuilds if schema version mismatches or force_rebuild is true.
pub fn open(cache_dir: &Path, force_rebuild: bool) -> Result<Connection> {
    std::fs::create_dir_all(cache_dir).context("Failed to create cache directory")?;

    let db_path = cache_dir.join("index.duckdb");
    let conn = Connection::open(&db_path).context("Failed to open cache database")?;

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

    if force_rebuild || needs_rebuild(&conn)? {
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
    let table_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM information_schema.tables WHERE table_name = 'cache_meta'",
        [],
        |r| r.get(0),
    )?;

    if !table_exists {
        return Ok(true);
    }

    // Check version (handle empty table gracefully)
    let version: Option<i32> = conn
        .query_row("SELECT version FROM cache_meta LIMIT 1", [], |r| r.get(0))
        .optional()?;

    match version {
        Some(v) if v == SCHEMA_VERSION => Ok(false),
        _ => Ok(true),
    }
}

fn rebuild(conn: &Connection) -> Result<()> {
    // Drop existing tables if they exist
    conn.execute_batch(
        "DROP SCHEMA IF EXISTS fts_main_cq_fts_messages CASCADE;
         DROP SCHEMA IF EXISTS fts_main_cq_fts_messages_0 CASCADE;
         DROP SCHEMA IF EXISTS fts_main_cq_fts_messages_1 CASCADE;
         DROP TABLE IF EXISTS cq_fts_messages;
         DROP TABLE IF EXISTS cq_fts_messages_0;
         DROP TABLE IF EXISTS cq_fts_messages_1;
         DROP TABLE IF EXISTS raw_records;
         DROP TABLE IF EXISTS file_registry;
         DROP TABLE IF EXISTS cache_meta;",
    )?;

    conn.execute_batch(
        // fts_sync_at answers \"has the data changed since we indexed?\" and
        // fts_built_at answers \"how old is the index?\". The staleness window
        // needs both: a rebuild is only worth doing when data actually moved,
        // and only once the index has aged past the window. fts_slot selects
        // which completed physical snapshot and FTS schema search should use.
        "CREATE TABLE cache_meta (
            version INTEGER NOT NULL,
            last_sync_at BIGINT NOT NULL DEFAULT 0,
            fts_sync_at BIGINT NOT NULL DEFAULT -1,
            fts_built_at BIGINT NOT NULL DEFAULT 0,
            fts_slot INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT NOT NULL,
            file_size BIGINT NOT NULL,
            cwd TEXT,
            agent_type TEXT,
            source TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        );

        CREATE TABLE raw_records (
            source_file TEXT NOT NULL,
            json JSON NOT NULL
        );",
    )?;

    conn.execute(
        "INSERT INTO cache_meta (version) VALUES (?)",
        [SCHEMA_VERSION],
    )?;

    Ok(())
}

/// Read the last_sync_at timestamp from cache_meta.
/// Returns 0 if no value is stored (first run).
pub fn last_sync_at(conn: &Connection) -> Result<i64> {
    let ts: i64 = conn
        .query_row("SELECT last_sync_at FROM cache_meta LIMIT 1", [], |r| {
            r.get(0)
        })
        .with_context(|| "Failed to read last_sync_at")?;
    Ok(ts)
}

/// Update the last_sync_at timestamp in cache_meta.
pub fn set_last_sync_at(conn: &Connection, ts: i64) -> Result<()> {
    conn.execute("UPDATE cache_meta SET last_sync_at = ?", [ts])?;
    Ok(())
}

/// Read the transcript sync generation covered by the persisted FTS index.
pub fn fts_sync_at(conn: &Connection) -> Result<i64> {
    let ts: i64 = conn
        .query_row("SELECT fts_sync_at FROM cache_meta LIMIT 1", [], |r| {
            r.get(0)
        })
        .with_context(|| "Failed to read fts_sync_at")?;
    Ok(ts)
}

/// Mark one completed FTS generation as active and covering the given transcript
/// sync. Updating all three fields in one statement makes the generation switch
/// atomic from the search command's perspective.
pub fn set_fts_built(conn: &Connection, sync_at: i64, built_at: i64, slot: usize) -> Result<()> {
    conn.execute(
        "UPDATE cache_meta SET fts_sync_at = ?, fts_built_at = ?, fts_slot = ?",
        duckdb::params![sync_at, built_at, slot as i32],
    )?;
    Ok(())
}

/// The completed FTS generation that search should query.
pub fn fts_slot(conn: &Connection) -> Result<usize> {
    let slot: i32 = conn
        .query_row("SELECT fts_slot FROM cache_meta LIMIT 1", [], |r| r.get(0))
        .with_context(|| "Failed to read fts_slot")?;
    match slot {
        0 | 1 => Ok(slot as usize),
        _ => anyhow::bail!("Invalid full-text search slot {slot}"),
    }
}

/// Wall-clock time the persisted FTS index was last built, in nanoseconds since
/// the epoch. Returns 0 when no index has been built.
pub fn fts_built_at(conn: &Connection) -> Result<i64> {
    let ts: i64 = conn
        .query_row("SELECT fts_built_at FROM cache_meta LIMIT 1", [], |r| {
            r.get(0)
        })
        .with_context(|| "Failed to read fts_built_at")?;
    Ok(ts)
}
