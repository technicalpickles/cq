use anyhow::Result;
use duckdb::Connection;
use crate::cache;
use crate::indexer;
use crate::views;
use crate::sync_scope::SyncScope;

pub struct DbSetup {
    pub conn: Connection,
    pub file_count: usize,
    pub total_files: usize,
    pub skipped: bool,
    pub lock_busy: bool,
}

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

/// Set up a DuckDB connection with views registered.
///
/// Uses the persistent cache for fast incremental startup.
pub fn setup_connection(
    sources: &[(String, std::path::PathBuf)],
    options: &DbOptions,
    scope: SyncScope,
) -> Result<DbSetup> {
    let cache_dir = cache::cache_dir()?;
    let force_rebuild = options.sync_mode == SyncMode::Force;
    let conn = cache::open(&cache_dir, force_rebuild)?;

    let result = indexer::sync_sources(&conn, sources, options.sync_mode, scope, &cache_dir)?;
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
