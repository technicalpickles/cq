use anyhow::{Context, Result};
use duckdb::Connection;

use crate::cache;

pub const SEARCH_TABLE: &str = "cq_fts_messages";
pub const SEARCH_SCHEMA: &str = "fts_main_cq_fts_messages";

/// Ensure the lazily-built message search table and DuckDB FTS index cover the
/// latest completed transcript sync.
pub fn prepare(conn: &Connection) -> Result<()> {
    load_extension(conn)?;

    let last_sync_at = cache::last_sync_at(conn)?;
    let fts_sync_at = cache::fts_sync_at(conn)?;
    if fts_sync_at == last_sync_at && search_objects_exist(conn)? {
        return Ok(());
    }

    // DuckDB's FTS indexes do not track mutations to their input table. Build a
    // fresh physical snapshot from the composed messages view, then index it.
    conn.execute_batch(&format!(
        "DROP SCHEMA IF EXISTS {SEARCH_SCHEMA} CASCADE;
         DROP TABLE IF EXISTS {SEARCH_TABLE};
         CREATE TABLE {SEARCH_TABLE} AS
         SELECT
             CAST(ROW_NUMBER() OVER (
                 ORDER BY harness, COALESCE(source, ''), session_id,
                          timestamp, COALESCE(uuid, '')
             ) AS VARCHAR) AS document_id,
             session_id, project, source, harness, uuid, type, timestamp, text
         FROM messages
         WHERE text IS NOT NULL AND text != ''"
    ))
    .context("Failed to materialize messages for full-text search")?;

    conn.execute_batch(&format!(
        "PRAGMA create_fts_index(
            'main.{SEARCH_TABLE}', 'document_id', 'text',
            stemmer = 'porter', stopwords = 'english', overwrite = 1
        )"
    ))
    .context("Failed to build the full-text search index")?;

    cache::set_fts_sync_at(conn, last_sync_at)?;
    Ok(())
}

fn load_extension(conn: &Connection) -> Result<()> {
    if conn.execute_batch("LOAD fts").is_ok() {
        return Ok(());
    }

    conn.execute_batch("INSTALL fts; LOAD fts").with_context(|| {
        "Failed to install DuckDB's fts extension (the first `cq search` requires network access)"
    })
}

fn search_objects_exist(conn: &Connection) -> Result<bool> {
    let table_exists: bool = conn.query_row(
        "SELECT count(*) > 0
         FROM information_schema.tables
         WHERE table_schema = 'main' AND table_name = ?",
        [SEARCH_TABLE],
        |row| row.get(0),
    )?;
    let schema_exists: bool = conn.query_row(
        "SELECT count(*) > 0
         FROM information_schema.schemata
         WHERE schema_name = ?",
        [SEARCH_SCHEMA],
        |row| row.get(0),
    )?;
    Ok(table_exists && schema_exists)
}
