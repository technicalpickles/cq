use anyhow::Result;
use duckdb::Connection;

use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    msg_type: Option<&str>,
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
        conditions.push(format!("timestamp >= '{formatted}'"));
    }

    if let Some(t) = msg_type {
        let escaped = t.replace('\'', "''");
        conditions.push(format!("type = '{escaped}'"));
    }

    if let Some(pattern) = grep {
        let escaped = pattern.replace('\'', "''");
        conditions.push(format!("text ILIKE '%{escaped}%'"));
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT session_id, type, timestamp, text
         FROM messages
         WHERE {where_clause}
         ORDER BY timestamp DESC
         LIMIT {limit}"
    );

    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, format)
}
