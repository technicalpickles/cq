use anyhow::Result;
use duckdb::Connection;

use crate::full_text;
use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;

#[allow(clippy::too_many_arguments)]
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    query: &str,
    msg_type: Option<&str>,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    full_text::prepare(conn)?;

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
    let ranked_sql = format!(
        "SELECT session_id, type, timestamp, text, score
         FROM (
             SELECT session_id, project, source, harness, type, timestamp, text,
                    {schema}.match_bm25(document_id, ?) AS score
             FROM {table}
             WHERE {where_clause}
         ) ranked
         WHERE score IS NOT NULL",
        schema = full_text::SEARCH_SCHEMA,
        table = full_text::SEARCH_TABLE,
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let count: i64 = conn.query_row(
        &format!("SELECT count(*) FROM ({ranked_sql}) __cq_count"),
        &param_refs[..],
        |row| row.get(0),
    )?;
    if count == 0 {
        if let Some(session) = &scope.session {
            super::print_session_not_found(session);
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
    let sql = format!(
        "SELECT session_id, type, timestamp, text, ROUND(score, 4) AS score
         FROM ({ranked_sql}) matches
         ORDER BY score DESC, timestamp DESC
         {limit_clause}
         {offset_clause}"
    );
    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, &param_refs, format, wide)?;

    if !matches!(format, OutputFormat::Json) && limit > 0 && count as usize > limit + offset {
        eprintln!(
            "{}",
            crate::style::hint(&format!(
                "Showing {} of {} results. Use --limit 0 for all.",
                limit,
                count.saturating_sub(offset as i64)
            ))
        );
    }
    Ok(())
}
