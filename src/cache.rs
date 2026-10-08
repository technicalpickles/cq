use anyhow::{Context, Result};
use duckdb::OptionalExt;
use duckdb::{Config, Connection};
use std::path::Path;

pub const SCHEMA_VERSION: i32 = 7;

/// The `storage_version` tag DuckDB stamps on files this build creates; a
/// cache with any other tag is recreated on open. Update it when a DuckDB bump
/// fails `fresh_cache_uses_current_storage_version`: left stale, every new
/// file mismatches and the cache is recreated on every open.
pub const STORAGE_VERSION_TAG: &str = "v1.5.0+";

/// Open or create the cache database. Recreates the file from scratch when
/// the schema version or storage format is out of date, or force_rebuild is
/// true. Recreating (not dropping tables in place) is what moves an old file
/// to the current storage format and returns its free blocks to the OS.
/// Safe because the cache only mirrors transcript files that are still on
/// disk; see docs/adr/0002-recreate-cache-file-on-rebuild.md.
///
/// Recreating needs a window with no DuckDB connection open, so it is
/// serialized with other cq processes through `index.lock`. The lock is
/// released before returning, because the indexer takes it again.
pub fn open(cache_dir: &Path, force_rebuild: bool) -> Result<Connection> {
    std::fs::create_dir_all(cache_dir).context("Failed to create cache directory")?;

    let db_path = cache_dir.join("index.duckdb");
    let existed = db_path.exists();
    let conn = connect(cache_dir, &db_path)?;

    // A brand-new file is already at the current storage format.
    if !existed {
        create_schema(&conn)?;
        return Ok(conn);
    }
    if !force_rebuild && !needs_rebuild(&conn)? {
        return Ok(conn);
    }

    // Once `conn` is dropped nothing holds the file, so another cq process
    // could open or recreate it. Take index.lock, and re-check under it in
    // case another process already recreated the file.
    drop(conn);
    let _lock = lock_index(cache_dir)?;
    if !force_rebuild {
        let conn = connect(cache_dir, &db_path)?;
        if !needs_rebuild(&conn)? {
            return Ok(conn);
        }
        drop(conn);
    }
    remove_database(&db_path)?;
    let conn = connect(cache_dir, &db_path)?;
    create_schema(&conn)?;
    Ok(conn)
}

pub fn open_lock_file(cache_dir: &Path) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(cache_dir.join("index.lock"))
        .context("Failed to open lock file")
}

/// Poll for an exclusive lock on `file` for up to 5s. True if acquired.
pub fn wait_for_lock(file: &std::fs::File) -> bool {
    use fs2::FileExt;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if file.try_lock_exclusive().is_ok() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Take the exclusive index lock, waiting up to 5s. Released on drop.
pub fn lock_index(cache_dir: &Path) -> Result<std::fs::File> {
    let file = open_lock_file(cache_dir)?;
    if !wait_for_lock(&file) {
        anyhow::bail!("index locked by another process after 5s, try again shortly");
    }
    Ok(file)
}

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

fn connect(cache_dir: &Path, db_path: &Path) -> Result<Connection> {
    // DuckDB's default v0.10.2 format (tag `v1.0.0+`) disables ZSTD/DICT_FSST,
    // leaving raw_records.json uncompressed. Only affects newly created files.
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
    if storage_version(conn)?.as_deref() != Some(STORAGE_VERSION_TAG) {
        return Ok(true);
    }

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

fn create_schema(conn: &Connection) -> Result<()> {
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
            agent_description TEXT,
            parent_tool_use_id TEXT,
            spawn_depth BIGINT,
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
