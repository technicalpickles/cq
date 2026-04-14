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

    if let Some(project) = &scope.project {
        let escaped = project.replace('\'', "''");
        conditions.push(format!("tc.project = '{escaped}'"));
    }

    if let Some(session) = &scope.session {
        let escaped = session.replace('\'', "''");
        conditions.push(format!("tc.session_id = '{escaped}'"));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("tc.timestamp >= '{formatted}'"));
    }

    if let Some(name) = tool_name {
        let escaped = name.replace('\'', "''");
        conditions.push(format!("tc.name = '{escaped}'"));
    }

    if let Some(pattern) = grep {
        let escaped = pattern.replace('\'', "''");
        conditions.push(format!("CAST(tc.input AS VARCHAR) ILIKE '%{escaped}%'"));
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

    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, format)
}

fn run_summary(conn: &Connection, scope: &QueryScope, format: &OutputFormat) -> Result<()> {
    let mut conditions = vec!["1=1".to_string()];

    if let Some(project) = &scope.project {
        let escaped = project.replace('\'', "''");
        conditions.push(format!("project = '{escaped}'"));
    }

    if let Some(session) = &scope.session {
        let escaped = session.replace('\'', "''");
        conditions.push(format!("session_id = '{escaped}'"));
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

    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, format)
}
