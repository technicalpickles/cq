use anyhow::Result;
use duckdb::types::Value;
use duckdb::Connection;

use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;
use crate::style;

struct HookSummaryRow {
    hook_event: String,
    count: i64,
}

struct HookDetailRow {
    session_id: String,
    hook_event: String,
    hook_name: String,
    content: String,
}

fn val_str(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Null => String::new(),
        other => format!("{:?}", other),
    }
}

fn val_i64(v: &Value) -> i64 {
    match v {
        Value::TinyInt(n) => *n as i64,
        Value::SmallInt(n) => *n as i64,
        Value::Int(n) => *n as i64,
        Value::BigInt(n) => *n,
        _ => 0,
    }
}

const VALID_COUNT_BY_COLUMNS: &[&str] = &["hook_event", "hook_name", "session_id", "project"];

#[allow(clippy::too_many_arguments)]
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    event: Option<&str>,
    grep: &[String],
    count_by: Option<&str>,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    // Dispatch to count-by mode
    if let Some(col) = count_by {
        let resolved = super::validate_count_by(col, VALID_COUNT_BY_COLUMNS, "hooks");
        return run_count_by(conn, scope, event, grep, &resolved, format, wide);
    }

    // Summary mode: no filters specified
    if event.is_none() && grep.is_empty() {
        return run_summary(conn, scope, format, wide);
    }

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

    if let Some(source) = &scope.source {
        conditions.push("source = ?".to_string());
        params.push(Box::new(source.clone()));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("timestamp >= '{formatted}'"));
    }

    if let Some(e) = event {
        conditions.push("hook_event = ?".to_string());
        params.push(Box::new(e.to_string()));
    }

    if let Some(clause) = super::grep_where("content", grep) {
        conditions.push(clause);
        params.extend(super::grep_params(grep));
    }

    let where_clause = conditions.join(" AND ");
    let limit_clause = super::limit_clause(limit);
    let offset_clause = super::offset_clause(offset);
    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    // JSON gets full column set for scripting; display gets only what's shown
    if matches!(format, OutputFormat::Json) {
        let sql = format!(
            "SELECT session_id, project, source, harness, timestamp, hook_event, hook_name, attachment_type, content, content_size
             FROM hook_events
             WHERE {where_clause}
             ORDER BY timestamp DESC
             {limit_clause}
             {offset_clause}"
        );
        let mut stmt = conn.prepare(&sql)?;
        return output::print_results(&mut stmt, &param_refs, format, wide);
    }

    let sql = format!(
        "SELECT session_id, hook_event, hook_name, content
         FROM hook_events
         WHERE {where_clause}
         ORDER BY timestamp DESC
         {limit_clause}
         {offset_clause}"
    );

    let mut stmt = conn.prepare(&sql)?;

    let mut rows_iter = stmt.query(&param_refs[..])?;
    let mut detail_rows: Vec<HookDetailRow> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        let values: Vec<Value> = (0..4)
            .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
            .collect();
        detail_rows.push(HookDetailRow {
            session_id: val_str(&values[0]),
            hook_event: val_str(&values[1]),
            hook_name: val_str(&values[2]),
            content: val_str(&values[3]),
        });
    }

    if detail_rows.is_empty() {
        if let Some(session) = &scope.session {
            super::print_session_not_found(session);
        } else {
            let mut extras: Vec<&str> = Vec::new();
            if !grep.is_empty() {
                extras.push("--grep");
            }
            if event.is_some() {
                extras.push("[event]");
            }
            super::print_no_results(scope, &extras);
        }
        return Ok(());
    }

    match format {
        OutputFormat::Table => render_detail_table(&detail_rows, wide),
        _ => render_detail_oneline(&detail_rows, wide),
    }

    super::print_truncation_hint(
        conn,
        "hook_events",
        &where_clause,
        &param_refs,
        detail_rows.len(),
        limit,
    );

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_count_by(
    conn: &Connection,
    scope: &QueryScope,
    event: Option<&str>,
    grep: &[String],
    column: &str,
    format: &OutputFormat,
    wide: bool,
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

    if let Some(source) = &scope.source {
        conditions.push("source = ?".to_string());
        params.push(Box::new(source.clone()));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("timestamp >= '{formatted}'"));
    }

    if let Some(e) = event {
        conditions.push("hook_event = ?".to_string());
        params.push(Box::new(e.to_string()));
    }

    if let Some(clause) = super::grep_where("content", grep) {
        conditions.push(clause);
        params.extend(super::grep_params(grep));
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT {column}, COUNT(*) AS count
         FROM hook_events
         WHERE {where_clause}
         GROUP BY {column}
         ORDER BY count DESC"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    match format {
        OutputFormat::Json => output::print_results(&mut stmt, &param_refs, format, wide),
        OutputFormat::Table => output::print_results(&mut stmt, &param_refs, format, wide),
        _ => {
            let mut rows_iter = stmt.query(&param_refs[..])?;
            let mut chart_rows: Vec<(String, i64)> = Vec::new();
            while let Some(row) = rows_iter.next()? {
                let label = row
                    .get::<_, Value>(0)
                    .map(|v| val_str(&v))
                    .unwrap_or_default();
                let count = row.get::<_, Value>(1).map(|v| val_i64(&v)).unwrap_or(0);
                chart_rows.push((label, count));
            }

            if chart_rows.is_empty() {
                super::print_no_results(scope, &[]);
                return Ok(());
            }

            super::render_bar_chart(&chart_rows);
            Ok(())
        }
    }
}

fn run_summary(
    conn: &Connection,
    scope: &QueryScope,
    format: &OutputFormat,
    wide: bool,
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

    if let Some(source) = &scope.source {
        conditions.push("source = ?".to_string());
        params.push(Box::new(source.clone()));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("timestamp >= '{formatted}'"));
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT hook_event, COUNT(*) AS count
         FROM hook_events
         WHERE {where_clause}
         GROUP BY hook_event
         ORDER BY count DESC"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    match format {
        OutputFormat::Json => output::print_results(&mut stmt, &param_refs, format, wide),
        _ => {
            let mut rows_iter = stmt.query(&param_refs[..])?;
            let mut summary_rows: Vec<HookSummaryRow> = Vec::new();
            while let Some(row) = rows_iter.next()? {
                let values: Vec<Value> = (0..2)
                    .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
                    .collect();
                summary_rows.push(HookSummaryRow {
                    hook_event: val_str(&values[0]),
                    count: val_i64(&values[1]),
                });
            }

            if summary_rows.is_empty() {
                if let Some(session) = &scope.session {
                    super::print_session_not_found(session);
                } else {
                    super::print_no_results(scope, &[]);
                }
                return Ok(());
            }

            match format {
                OutputFormat::Table => render_summary_table(&summary_rows),
                _ => {
                    let chart_rows: Vec<(String, i64)> = summary_rows
                        .iter()
                        .map(|r| (r.hook_event.clone(), r.count))
                        .collect();
                    super::render_bar_chart(&chart_rows);
                }
            }

            Ok(())
        }
    }
}

fn render_summary_table(rows: &[HookSummaryRow]) {
    let headers = ["hook_event", "count"];
    let string_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| vec![r.hook_event.clone(), r.count.to_string()])
        .collect();
    style::print_light_table(&headers, &string_rows);
}

fn render_detail_oneline(rows: &[HookDetailRow], wide: bool) {
    // Build plain text rows (no color) for width calculation
    let plain_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let session_id = if r.session_id.is_empty() {
                style::null_display().to_string()
            } else {
                style::short_id(&r.session_id, 8)
            };

            let hook_event = if r.hook_event.is_empty() {
                style::null_display().to_string()
            } else {
                r.hook_event.clone()
            };

            let hook_name = if r.hook_name.is_empty() {
                style::null_display().to_string()
            } else {
                r.hook_name.clone()
            };

            let content = if r.content.is_empty() {
                style::null_display().to_string()
            } else if wide {
                r.content.clone()
            } else {
                style::truncate(&r.content, 60)
            };

            vec![session_id, hook_event, hook_name, content]
        })
        .collect();

    // Calculate column widths from plain text
    let ncols = 4;
    let mut widths = vec![0usize; ncols];
    for row in &plain_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    // Print each row with color applied after padding
    for row in &plain_rows {
        let cols: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let padded = if i == ncols - 1 {
                    cell.clone()
                } else {
                    style::pad_right(cell, widths[i])
                };
                match i {
                    0 => style::color(&padded, style::Color::Secondary),
                    1 => style::color(&padded, style::Color::Primary),
                    _ => padded,
                }
            })
            .collect();
        println!("{}", cols.join("  "));
    }
}

fn render_detail_table(rows: &[HookDetailRow], wide: bool) {
    let headers = ["session", "hook_event", "hook_name", "content"];
    let string_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let session_id = if r.session_id.is_empty() {
                style::null_display().to_string()
            } else {
                style::short_id(&r.session_id, 8)
            };

            let hook_event = if r.hook_event.is_empty() {
                style::null_display().to_string()
            } else {
                r.hook_event.clone()
            };

            let hook_name = if r.hook_name.is_empty() {
                style::null_display().to_string()
            } else {
                r.hook_name.clone()
            };

            let content = if r.content.is_empty() {
                style::null_display().to_string()
            } else if wide {
                r.content.clone()
            } else {
                style::truncate(&r.content, 60)
            };

            vec![session_id, hook_event, hook_name, content]
        })
        .collect();
    style::print_light_table(&headers, &string_rows);
}
