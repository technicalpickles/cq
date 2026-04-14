use anyhow::Result;
use duckdb::Connection;
use crate::provider::TranscriptProvider;
use crate::scope::QueryScope;

pub struct DbSetup {
    pub conn: Connection,
    pub file_count: usize,
}

/// Set up an in-memory DuckDB connection with views registered against
/// discovered transcript files.
///
/// Uses the provider to discover files matching the scope, then registers
/// all queryable views (messages, tool_calls, tool_results, sessions).
pub fn setup_connection(provider: &dyn TranscriptProvider, scope: &QueryScope) -> Result<DbSetup> {
    let files = provider.discover_files(scope)?;
    let file_count = files.len();
    let conn = Connection::open_in_memory()?;
    provider.register_views(&conn, &files)?;
    Ok(DbSetup { conn, file_count })
}
