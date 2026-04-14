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

    if let Some(project) = &scope.project {
        let escaped = project.replace('\'', "''");
        conditions.push(format!("project ILIKE '%{escaped}%'"));
    }

    if let Some(session) = &scope.session {
        let escaped = session.replace('\'', "''");
        conditions.push(format!("session_id = '{escaped}'"));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("started_at >= '{formatted}'"));
    }

    if let Some(pattern) = grep {
        let escaped = pattern.replace('\'', "''");
        conditions.push(format!("first_user_message ILIKE '%{escaped}%'"));
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT session_id, project, started_at, ended_at, message_count, tool_call_count, first_user_message
         FROM sessions
         WHERE {where_clause}
         ORDER BY started_at DESC
         LIMIT {limit}"
    );

    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, format)
}
