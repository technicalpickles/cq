use anyhow::Result;
use duckdb::Connection;
use duckdb::types::Value;

use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;
use crate::style;

struct ToolSummaryRow {
    name: String,
    count: i64,
}

struct ToolDetailRow {
    session_id: String,
    name: String,
    input: String,
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

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    // JSON gets full column set for scripting; display gets only what's shown
    if matches!(format, OutputFormat::Json) {
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
        return output::print_results(&mut stmt, &param_refs, format);
    }

    let sql = if errors_only {
        format!(
            "SELECT tc.session_id, tc.name, CAST(tc.input AS VARCHAR) AS input
             FROM tool_calls tc
             JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
             WHERE {where_clause}
             AND tr.is_error = true
             ORDER BY tc.timestamp DESC
             LIMIT {limit}"
        )
    } else {
        format!(
            "SELECT tc.session_id, tc.name, CAST(tc.input AS VARCHAR) AS input
             FROM tool_calls tc
             WHERE {where_clause}
             ORDER BY tc.timestamp DESC
             LIMIT {limit}"
        )
    };

    let mut stmt = conn.prepare(&sql)?;

    let mut rows_iter = stmt.query(&param_refs[..])?;
    let mut detail_rows: Vec<ToolDetailRow> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        let values: Vec<Value> = (0..3)
            .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
            .collect();
        detail_rows.push(ToolDetailRow {
            session_id: val_str(&values[0]),
            name: val_str(&values[1]),
            input: val_str(&values[2]),
        });
    }

    if detail_rows.is_empty() {
        eprintln!("No results.");
        return Ok(());
    }

    match format {
        OutputFormat::Table => render_detail_table(&detail_rows),
        _ => render_detail_oneline(&detail_rows),
    }

    Ok(())
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

    match format {
        OutputFormat::Json => output::print_results(&mut stmt, &param_refs, format),
        _ => {
            let mut rows_iter = stmt.query(&param_refs[..])?;
            let mut summary_rows: Vec<ToolSummaryRow> = Vec::new();
            while let Some(row) = rows_iter.next()? {
                let values: Vec<Value> = (0..2)
                    .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
                    .collect();
                summary_rows.push(ToolSummaryRow {
                    name: val_str(&values[0]),
                    count: val_i64(&values[1]),
                });
            }

            match format {
                OutputFormat::Table => render_summary_table(&summary_rows),
                _ => render_bar_chart(&summary_rows),
            }

            Ok(())
        }
    }
}

fn render_bar_chart(rows: &[ToolSummaryRow]) {
    if rows.is_empty() {
        eprintln!("No results.");
        return;
    }

    let max_count = rows.iter().map(|r| r.count).max().unwrap_or(1);
    let name_width = rows.iter().map(|r| r.name.len()).max().unwrap_or(0);
    let count_width = rows.iter().map(|r| r.count.to_string().len()).max().unwrap_or(0);

    for row in rows {
        let name_padded = style::pad_right(&row.name, name_width);
        let bar_str = style::bar(row.count, max_count, 30);
        let count_str = row.count.to_string();
        let count_padded = style::pad_left(&count_str, count_width);

        println!(
            "{}  {}  {}",
            style::color(&name_padded, style::Color::Primary),
            style::color(&bar_str, style::Color::Bar),
            style::color(&count_padded, style::Color::Dim),
        );
    }
}

fn render_summary_table(rows: &[ToolSummaryRow]) {
    let headers = ["name", "count"];
    let string_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        vec![r.name.clone(), r.count.to_string()]
    }).collect();
    style::print_light_table(&headers, &string_rows);
}

fn render_detail_oneline(rows: &[ToolDetailRow]) {
    if rows.is_empty() {
        eprintln!("No results.");
        return;
    }

    // Build plain text rows (no color) for width calculation
    let plain_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        let session_id = if r.session_id.is_empty() {
            style::null_display().to_string()
        } else {
            style::short_id(&r.session_id, 8)
        };

        let name = if r.name.is_empty() {
            style::null_display().to_string()
        } else {
            r.name.clone()
        };

        let input = if r.input.is_empty() {
            style::null_display().to_string()
        } else {
            style::truncate(&r.input, 60)
        };

        vec![session_id, name, input]
    }).collect();

    // Calculate column widths from plain text
    let ncols = 3;
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
        let cols: Vec<String> = row.iter().enumerate().map(|(i, cell)| {
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
        }).collect();
        println!("{}", cols.join("  "));
    }
}

fn render_detail_table(rows: &[ToolDetailRow]) {
    let headers = ["session", "tool", "input"];
    let string_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        let session_id = if r.session_id.is_empty() {
            style::null_display().to_string()
        } else {
            style::short_id(&r.session_id, 8)
        };

        let name = if r.name.is_empty() {
            style::null_display().to_string()
        } else {
            r.name.clone()
        };

        let input = if r.input.is_empty() {
            style::null_display().to_string()
        } else {
            style::truncate(&r.input, 60)
        };

        vec![session_id, name, input]
    }).collect();
    style::print_light_table(&headers, &string_rows);
}
