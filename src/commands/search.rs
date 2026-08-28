use anyhow::Result;
use duckdb::Connection;

use crate::db::SyncMode;
use crate::full_text;
use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;

#[allow(clippy::too_many_arguments)]
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    query: &str,
    msg_type: Option<&str>,
    all_matches: bool,
    sync_mode: SyncMode,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    full_text::prepare(conn, sync_mode)?;

    let mut conditions = vec!["1=1".to_string()];
    let mut params: Vec<Box<dyn duckdb::types::ToSql>> = vec![Box::new(query.to_string())];

    if let Some(project) = &scope.project {
        conditions.push("project ILIKE ?".to_string());
        params.push(Box::new(format!("%{project}%")));
    }
    if let Some(session) = &scope.session {
        conditions.push("session_id = ?".to_string());
        params.push(Box::new(session.clone()));
    }
    if let Some(source) = &scope.source {
        conditions.push(crate::scope::source_filter_sql(""));
        params.push(Box::new(source.clone()));
    }
    if let Some(harness) = &scope.harness {
        conditions.push(crate::scope::harness_filter_sql(""));
        params.push(Box::new(harness.clone()));
    }
    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("timestamp >= '{formatted}'"));
    }
    if let Some(msg_type) = msg_type {
        conditions.push("type = ?".to_string());
        params.push(Box::new(msg_type.to_string()));
    }

    let where_clause = conditions.join(" AND ");
    let scored_sql = format!(
        "SELECT session_id, type, timestamp, text,
                {schema}.match_bm25(document_id, ?) AS score
         FROM {table}
         WHERE {where_clause}",
        schema = full_text::SEARCH_SCHEMA,
        table = full_text::SEARCH_TABLE,
    );

    // The index is message-level, so a session that discussed a topic at length
    // can crowd every other session off the page. Collapse to that session's
    // best-scoring message by default and carry the passage count alongside it,
    // so the signal that would otherwise be lost stays visible.
    let ranked_sql = if all_matches {
        format!("SELECT * FROM ({scored_sql}) s WHERE s.score IS NOT NULL")
    } else {
        format!(
            "SELECT session_id, type, timestamp, text, score, match_count
             FROM (
                 SELECT s.*,
                        ROW_NUMBER() OVER (PARTITION BY s.session_id
                                           ORDER BY s.score DESC, s.timestamp DESC) AS rn,
                        COUNT(*) OVER (PARTITION BY s.session_id) AS match_count
                 FROM ({scored_sql}) s
                 WHERE s.score IS NOT NULL
             ) collapsed
             WHERE rn = 1"
        )
    };

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let count: i64 = conn.query_row(
        &format!("SELECT count(*) FROM ({ranked_sql}) __cq_count"),
        &param_refs[..],
        |row| row.get(0),
    )?;
    if count == 0 {
        let missing_session = if let Some(session) = &scope.session {
            !session_exists(conn, session)?
        } else {
            false
        };
        if missing_session {
            super::print_session_not_found(
                scope
                    .session
                    .as_deref()
                    .expect("missing session was checked"),
            );
        } else {
            let mut extras = Vec::new();
            if msg_type.is_some() {
                extras.push("--type");
            }
            super::print_no_results(scope, &extras);
        }
        if matches!(format, OutputFormat::Json) {
            println!("[]");
        }
        return Ok(());
    }

    let limit_clause = super::limit_clause(limit);
    let offset_clause = super::offset_clause(offset);
    let match_count_column = if all_matches {
        ""
    } else {
        ", match_count AS matches"
    };
    let sql = format!(
        "SELECT session_id, type, timestamp, text,
                ROUND(score, 4) AS score{match_count_column}
         FROM ({ranked_sql}) ranked
         ORDER BY score DESC, timestamp DESC
         {limit_clause}
         {offset_clause}"
    );
    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, &param_refs, format, wide)?;

    if !matches!(format, OutputFormat::Json) && limit > 0 && count as usize > limit + offset {
        let unit = if all_matches { "messages" } else { "sessions" };
        eprintln!(
            "{}",
            crate::style::hint(&format!(
                "Showing {} of {} matching {}. Use --limit 0 for all.",
                limit,
                count.saturating_sub(offset as i64),
                unit
            ))
        );
    }
    Ok(())
}

/// A scoped search can return no matches even when the session itself exists.
/// Check the live messages view rather than the FTS snapshot so a newly-synced
/// session is not mistaken for a missing one while the search index is stale.
fn session_exists(conn: &Connection, session_id: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM messages WHERE session_id = ?)",
        [session_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}
