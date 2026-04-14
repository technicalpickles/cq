use std::path::Path;
use anyhow::{Context, Result};
use duckdb::Connection;
use duckdb::OptionalExt;

pub const SCHEMA_VERSION: i32 = 1;

/// Open or create the cache database. Creates tables if missing,
/// rebuilds if schema version mismatches or force_rebuild is true.
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
