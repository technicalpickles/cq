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
/// Uses the persistent cache for fast incremental startup.
pub fn setup_connection(projects_dir: &std::path::Path, options: &DbOptions) -> Result<DbSetup> {
    let cache_dir = cache::cache_dir()?;
    let conn = cache::open(&cache_dir, options.reindex)?;

    let stats = indexer::sync(&conn, projects_dir)?;
    let file_count = stats.added + stats.changed;

    views::register_derived_views(&conn)?;

    Ok(DbSetup { conn, file_count })
}
