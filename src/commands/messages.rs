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
        conditions.push(format!("timestamp >= '{formatted}'"));
    }

    if let Some(t) = msg_type {
        conditions.push("type = ?".to_string());
        params.push(Box::new(t.to_string()));
    }

    if let Some(pattern) = grep {
        conditions.push("text ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT session_id, type, timestamp, text
         FROM messages
         WHERE {where_clause}
         ORDER BY timestamp DESC
         LIMIT {limit}"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, &param_refs, format)
}
