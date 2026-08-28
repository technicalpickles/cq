use anyhow::{Context, Result};
use chrono::Duration;
use duckdb::Connection;

use crate::cache;
use crate::db::SyncMode;
use crate::scope;

pub const SEARCH_TABLE: &str = "cq_fts_messages";
pub const SEARCH_SCHEMA: &str = "fts_main_cq_fts_messages";

/// How far the search index may lag behind the transcripts before `cq search`
/// rebuilds it, overridable with `CQ_FTS_MAX_AGE`.
///
/// Rebuilding costs multiples of an entire normal cq command, and usage is
/// bursty: the median gap between consecutive cq invocations in a session is
/// about 16 seconds, so always-refresh turns one exploration into a string of
/// rebuilds. Serving slightly stale results and saying so fits cq's
/// stale-but-available default better. See
/// `docs/notes/2026-08-27-full-text-search-evaluation.md`.
const DEFAULT_MAX_AGE: &str = "5m";

/// Ensure the lazily-built message search table and DuckDB FTS index are fresh
/// enough to query, and report on stderr when they are not.
pub fn prepare(conn: &Connection, mode: SyncMode) -> Result<()> {
    load_extension(conn)?;

    let exists = search_objects_exist(conn)?;
    let last_sync_at = cache::last_sync_at(conn)?;

    if exists && cache::fts_sync_at(conn)? == last_sync_at {
        return Ok(());
    }

    if !exists {
        // Nothing to serve, so there is no stale-but-available option here.
        if mode == SyncMode::Skip {
            anyhow::bail!(
                "No search index exists yet, and --no-reindex forbids building one.\n\
                 Hint: run the same search without --no-reindex to build it."
            );
        }
        return rebuild(conn, last_sync_at);
    }

    match mode {
        // Explicit beats smart: --reindex forces the rebuild outright rather
        // than relying on the freshness comparison happening to fail.
        SyncMode::Force => rebuild(conn, last_sync_at),
        // --no-reindex promises to skip expensive work, so it skips this too.
        SyncMode::Skip => report_staleness(conn),
        SyncMode::Auto => {
            let age = now_ns().saturating_sub(cache::fts_built_at(conn)?);
            if age >= max_age()?.num_nanoseconds().unwrap_or(i64::MAX) {
                rebuild(conn, last_sync_at)
            } else {
                report_staleness(conn)
            }
        }
    }
}

fn max_age() -> Result<Duration> {
    match std::env::var("CQ_FTS_MAX_AGE") {
        Ok(value) if !value.is_empty() => scope::parse_duration(&value)
            .with_context(|| format!("Invalid CQ_FTS_MAX_AGE '{value}'")),
        _ => scope::parse_duration(DEFAULT_MAX_AGE),
    }
}

fn rebuild(conn: &Connection, last_sync_at: i64) -> Result<()> {
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

    cache::set_fts_built(conn, last_sync_at, now_ns())?;
    Ok(())
}

/// Tell the caller the index is behind, and how far.
///
/// The sharper warning keys off whether the caller's own session has content
/// the index lacks, not off whether that session turned up in the results. A
/// stale index means the caller's recent messages are missing, so the failure
/// worth warning about is the false negative, where a matching message is
/// simply absent and inspecting the results reveals nothing.
fn report_staleness(conn: &Connection) -> Result<()> {
    let age = humanize(now_ns().saturating_sub(cache::fts_built_at(conn)?));

    let own_session_ahead = match scope::active_claude_session() {
        Some(session_id) => session_is_ahead(conn, &session_id)?,
        None => false,
    };

    let mut message = format!("Search index is {age} behind.");
    if own_session_ahead {
        message.push_str(" Your current session has messages that are not in it yet.");
    }
    message.push_str(" Hint: --reindex rebuilds it.");
    eprintln!("{}", crate::style::hint(&message));
    Ok(())
}

/// True when the given session has messages newer than anything indexed for it.
/// Cheap because transcript sync stays on Auto, so `messages` is current even
/// when the snapshot is not.
fn session_is_ahead(conn: &Connection, session_id: &str) -> Result<bool> {
    let ahead: bool = conn.query_row(
        &format!(
            "SELECT COALESCE(
                 (SELECT max(timestamp) FROM messages WHERE session_id = ?)
                 > COALESCE(
                     (SELECT max(timestamp) FROM {SEARCH_TABLE} WHERE session_id = ?), ''),
                 false)"
        ),
        [session_id, session_id],
        |row| row.get(0),
    )?;
    Ok(ahead)
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn humanize(nanos: i64) -> String {
    let secs = nanos / 1_000_000_000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_picks_a_coarse_unit() {
        assert_eq!(humanize(5 * 1_000_000_000), "5s");
        assert_eq!(humanize(4 * 60 * 1_000_000_000), "4m");
        assert_eq!(humanize(3 * 3_600 * 1_000_000_000), "3h");
        assert_eq!(humanize(2 * 86_400 * 1_000_000_000), "2d");
    }

    #[test]
    fn max_age_defaults_to_five_minutes() {
        // Guard the default rather than the env override, which would race
        // other tests sharing this process's environment.
        assert_eq!(
            scope::parse_duration(DEFAULT_MAX_AGE).unwrap(),
            Duration::minutes(5)
        );
    }
}
