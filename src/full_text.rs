use anyhow::{Context, Result};
use chrono::Duration;
use duckdb::Connection;

use crate::cache;
use crate::db::SyncMode;
use crate::scope;

const SEARCH_TABLES: [&str; 2] = ["cq_fts_messages_0", "cq_fts_messages_1"];
const SEARCH_SCHEMAS: [&str; 2] = ["fts_main_cq_fts_messages_0", "fts_main_cq_fts_messages_1"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchIndex {
    pub table: &'static str,
    pub schema: &'static str,
    slot: usize,
}

impl SearchIndex {
    fn for_slot(slot: usize) -> Result<Self> {
        match (SEARCH_TABLES.get(slot), SEARCH_SCHEMAS.get(slot)) {
            (Some(&table), Some(&schema)) => Ok(Self {
                table,
                schema,
                slot,
            }),
            _ => anyhow::bail!("Invalid full-text search slot {slot}"),
        }
    }

    fn other(self) -> Self {
        Self::for_slot(1 - self.slot).expect("full-text search has exactly two slots")
    }
}

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
pub fn prepare(conn: &Connection, mode: SyncMode) -> Result<SearchIndex> {
    load_extension(conn)?;

    let active = SearchIndex::for_slot(cache::fts_slot(conn)?)?;
    let fts_sync_at = cache::fts_sync_at(conn)?;
    // fts_sync_at stays at -1 until a complete build atomically selects its
    // generation. Partial objects from a failed first build are never usable.
    let exists = fts_sync_at >= 0 && search_objects_exist(conn, active)?;
    let last_sync_at = cache::last_sync_at(conn)?;

    if exists && fts_sync_at == last_sync_at {
        return Ok(active);
    }

    if !exists {
        // Nothing to serve, so there is no stale-but-available option here.
        if mode == SyncMode::Skip {
            anyhow::bail!(
                "No search index exists yet, and --no-reindex forbids building one.\n\
                 Hint: run the same search without --no-reindex to build it."
            );
        }
        return rebuild(conn, last_sync_at, active, false);
    }

    match mode {
        // Explicit beats smart: --reindex forces the rebuild outright rather
        // than relying on the freshness comparison happening to fail.
        SyncMode::Force => rebuild(conn, last_sync_at, active, true),
        // --no-reindex promises to skip expensive work, so it skips this too.
        SyncMode::Skip => {
            report_staleness(conn, active)?;
            Ok(active)
        }
        SyncMode::Auto => {
            let age = now_ns().saturating_sub(cache::fts_built_at(conn)?);
            if age >= max_age()?.num_nanoseconds().unwrap_or(i64::MAX) {
                rebuild(conn, last_sync_at, active, true)
            } else {
                report_staleness(conn, active)?;
                Ok(active)
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

fn rebuild(
    conn: &Connection,
    last_sync_at: i64,
    active: SearchIndex,
    active_exists: bool,
) -> Result<SearchIndex> {
    // DuckDB's FTS indexes do not track mutations to their input table. Build a
    // fresh physical snapshot from the composed messages view, then index it.
    // Refreshes target the inactive generation, so any failure leaves the
    // completed active generation untouched and available to --no-reindex.
    let target = if active_exists {
        active.other()
    } else {
        active
    };

    conn.execute_batch(&format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE;
         DROP TABLE IF EXISTS {table};
         CREATE TABLE {table} AS
         SELECT
             CAST(ROW_NUMBER() OVER (
                 ORDER BY harness, COALESCE(source, ''), session_id,
                          timestamp, COALESCE(uuid, '')
             ) AS VARCHAR) AS document_id,
             session_id, project, source, harness, uuid, type, timestamp, text
         FROM messages
         WHERE text IS NOT NULL AND text != ''",
        schema = target.schema,
        table = target.table,
    ))
    .context("Failed to materialize messages for full-text search")?;

    conn.execute_batch(&format!(
        "PRAGMA create_fts_index(
            'main.{table}', 'document_id', 'text',
            stemmer = 'porter', stopwords = 'english', overwrite = 1
        )",
        table = target.table,
    ))
    .context("Failed to build the full-text search index")?;

    // This one statement is the swap. If it fails, cache_meta still points at
    // the old completed generation; the new objects are merely inactive and
    // will be replaced on the next rebuild attempt.
    cache::set_fts_built(conn, last_sync_at, now_ns(), target.slot)?;
    Ok(target)
}

/// Tell the caller the index is behind, and how far.
///
/// The sharper warning keys off whether the caller's own session has content
/// the index lacks, not off whether that session turned up in the results. A
/// stale index means the caller's recent messages are missing, so the failure
/// worth warning about is the false negative, where a matching message is
/// simply absent and inspecting the results reveals nothing.
fn report_staleness(conn: &Connection, active: SearchIndex) -> Result<()> {
    let age = humanize(now_ns().saturating_sub(cache::fts_built_at(conn)?));

    let own_session_ahead = match scope::active_claude_session() {
        Some(session_id) => session_is_ahead(conn, &session_id, active)?,
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
fn session_is_ahead(conn: &Connection, session_id: &str, active: SearchIndex) -> Result<bool> {
    let ahead: bool = conn.query_row(
        &format!(
            "SELECT COALESCE(
                 (SELECT max(timestamp) FROM messages WHERE session_id = ?)
                 > COALESCE(
                     (SELECT max(timestamp) FROM {table} WHERE session_id = ?), ''),
                 false)",
            table = active.table,
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

fn search_objects_exist(conn: &Connection, index: SearchIndex) -> Result<bool> {
    let table_exists: bool = conn.query_row(
        "SELECT count(*) > 0
         FROM information_schema.tables
         WHERE table_schema = 'main' AND table_name = ?",
        [index.table],
        |row| row.get(0),
    )?;
    let schema_exists: bool = conn.query_row(
        "SELECT count(*) > 0
         FROM information_schema.schemata
         WHERE schema_name = ?",
        [index.schema],
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

    #[test]
    fn failed_rebuild_preserves_previous_snapshot() {
        let conn = Connection::open_in_memory().unwrap();
        let active = SearchIndex::for_slot(0).unwrap();
        conn.execute_batch(&format!(
            "CREATE SCHEMA {schema};
             CREATE TABLE {table} (marker VARCHAR);
             INSERT INTO {table} VALUES ('old snapshot');
             CREATE TABLE {schema}.sentinel (value INTEGER);",
            schema = active.schema,
            table = active.table,
        ))
        .unwrap();

        let error = rebuild(&conn, 0, active, true).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Failed to materialize messages for full-text search"),
            "unexpected error: {error:#}"
        );
        assert!(
            search_objects_exist(&conn, active).unwrap(),
            "failed rebuild removed the previous search objects"
        );
        let marker: String = conn
            .query_row(&format!("SELECT marker FROM {}", active.table), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(marker, "old snapshot");
    }
}
