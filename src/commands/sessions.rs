use anyhow::Result;
use duckdb::Connection;

use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    grep: Option<&str>,
    format: &OutputFormat,
    limit: usize,
) -> Result<()> {
    let mut conditions = vec!["1=1".to_string()];
    let mut params: Vec<Box<dyn duckdb::types::ToSql>> = Vec::new();

    if let Some(project) = &scope.project {
        conditions.push("project ILIKE ?".to_string());
        params.push(Box::new(format!("%{project}%")));
    }

    if let Some(session) = &scope.session {
        conditions.push("session_id = ?".to_string());
        params.push(Box::new(session.clone()));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("started_at >= '{formatted}'"));
    }

    if let Some(pattern) = grep {
        conditions.push("first_user_message ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT session_id, project, started_at, ended_at, message_count, tool_call_count, first_user_message
         FROM sessions
         WHERE {where_clause}
         ORDER BY started_at DESC
         LIMIT {limit}"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, &param_refs, format)
}
