use anyhow::Result;
use duckdb::Connection;

use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    tool_name: Option<&str>,
    grep: Option<&str>,
    errors_only: bool,
    format: &OutputFormat,
    limit: usize,
) -> Result<()> {
    // Summary mode: no filters specified
    if tool_name.is_none() && grep.is_none() && !errors_only {
        return run_summary(conn, scope, format);
    }

    let mut conditions = vec!["1=1".to_string()];
    let mut params: Vec<Box<dyn duckdb::types::ToSql>> = Vec::new();

    if let Some(project) = &scope.project {
        conditions.push("tc.project ILIKE ?".to_string());
        params.push(Box::new(format!("%{project}%")));
    }

    if let Some(session) = &scope.session {
        conditions.push("tc.session_id = ?".to_string());
        params.push(Box::new(session.clone()));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("tc.timestamp >= '{formatted}'"));
    }

    if let Some(name) = tool_name {
        conditions.push("tc.name = ?".to_string());
        params.push(Box::new(name.to_string()));
    }

    if let Some(pattern) = grep {
        conditions.push("CAST(tc.input AS VARCHAR) ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }

    let where_clause = conditions.join(" AND ");

    let sql = if errors_only {
        format!(
            "SELECT tc.session_id, tc.project, tc.name, tc.tool_use_id, tc.timestamp, CAST(tc.input AS VARCHAR) AS input
             FROM tool_calls tc
             JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
             WHERE {where_clause}
             AND tr.is_error = true
             ORDER BY tc.timestamp DESC
             LIMIT {limit}"
        )
    } else {
        format!(
            "SELECT tc.session_id, tc.project, tc.name, tc.tool_use_id, tc.timestamp, CAST(tc.input AS VARCHAR) AS input
             FROM tool_calls tc
             WHERE {where_clause}
             ORDER BY tc.timestamp DESC
             LIMIT {limit}"
        )
    };

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, &param_refs, format)
}

fn run_summary(conn: &Connection, scope: &QueryScope, format: &OutputFormat) -> Result<()> {
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

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT name, COUNT(*) AS count
         FROM tool_calls
         WHERE {where_clause}
         GROUP BY name
         ORDER BY count DESC"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, &param_refs, format)
}
