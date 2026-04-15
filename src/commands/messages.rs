use anyhow::Result;
use duckdb::Connection;
use duckdb::types::Value;

use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;
use crate::style;

struct MessageRow {
    session_id: String,
    msg_type: String,
    timestamp: String,
    text: String,
}

fn val_str(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Null => String::new(),
        other => format!("{:?}", other),
    }
}

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
    let limit_clause = super::limit_clause(limit);

    let sql = format!(
        "SELECT session_id, type, timestamp, text
         FROM messages
         WHERE {where_clause}
         ORDER BY timestamp DESC
         {limit_clause}"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    match format {
        OutputFormat::Json => output::print_results(&mut stmt, &param_refs, format),
        _ => {
            let mut rows_iter = stmt.query(&param_refs[..])?;
            let mut message_rows: Vec<MessageRow> = Vec::new();
            while let Some(row) = rows_iter.next()? {
                let values: Vec<Value> = (0..4)
                    .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
                    .collect();
                message_rows.push(MessageRow {
                    session_id: val_str(&values[0]),
                    msg_type: val_str(&values[1]),
                    timestamp: val_str(&values[2]),
                    text: val_str(&values[3]),
                });
            }

            match format {
                OutputFormat::Table => render_table(&message_rows),
                _ => render_oneline(&message_rows),
            }

            Ok(())
        }
    }
}

fn render_oneline(rows: &[MessageRow]) {
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

        let msg_type = if r.msg_type.is_empty() {
            style::null_display().to_string()
        } else {
            r.msg_type.clone()
        };

        let time_ago = if r.timestamp.is_empty() {
            style::null_display().to_string()
        } else {
            style::relative_time(&r.timestamp)
        };

        let text = if r.text.is_empty() {
            style::null_display().to_string()
        } else {
            style::truncate(&r.text, 60)
        };

        vec![session_id, msg_type, time_ago, text]
    }).collect();

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
        let cols: Vec<String> = row.iter().enumerate().map(|(i, cell)| {
            let padded = if i == ncols - 1 {
                cell.clone()
            } else {
                style::pad_right(cell, widths[i])
            };
            match i {
                0 => style::color(&padded, style::Color::Secondary),
                1 => style::color(&padded, style::Color::Primary),
                2 => style::color(&padded, style::Color::Dim),
                _ => padded, // last column, no color
            }
        }).collect();
        println!("{}", cols.join("  "));
    }
}

fn render_table(rows: &[MessageRow]) {
    if rows.is_empty() {
        eprintln!("No results.");
        return;
    }

    let headers = ["session_id", "type", "timestamp", "text"];

    let string_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        let session_id = if r.session_id.is_empty() {
            style::null_display().to_string()
        } else {
            style::short_id(&r.session_id, 8)
        };

        let msg_type = if r.msg_type.is_empty() {
            style::null_display().to_string()
        } else {
            r.msg_type.clone()
        };

        let timestamp = if r.timestamp.is_empty() {
            style::null_display().to_string()
        } else {
            style::relative_time(&r.timestamp)
        };

        let text = if r.text.is_empty() {
            style::null_display().to_string()
        } else {
            style::truncate(&r.text, 60)
        };

        vec![session_id, msg_type, timestamp, text]
    }).collect();

    style::print_light_table(&headers, &string_rows);
}
