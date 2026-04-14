use anyhow::Result;
use duckdb::Connection;
use crate::provider::TranscriptProvider;
use crate::scope::QueryScope;

/// Set up an in-memory DuckDB connection with views registered against
/// discovered transcript files.
///
/// Uses the provider to discover files matching the scope, then registers
/// all queryable views (messages, tool_calls, tool_results, sessions).
pub fn setup_connection(provider: &dyn TranscriptProvider, scope: &QueryScope) -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    let files = provider.discover_files(scope)?;
    provider.register_views(&conn, &files)?;
    Ok(conn)
}
