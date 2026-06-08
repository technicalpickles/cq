use anyhow::Result;
use duckdb::Connection;
use duckdb::types::Value;

use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;
use crate::style;

struct SessionRow {
    session_id: String,
    project: String,
    started_at: String,
    ended_at: String,
    message_count: i64,
    tool_call_count: i64,
    first_user_message: String,
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

fn project_leaf(project: &str) -> String {
    project.split('/').filter(|s| !s.is_empty()).last().unwrap_or(project).to_string()
}

fn duration_mins(started: &str, ended: &str) -> i64 {
    use chrono::DateTime;
    let s = DateTime::parse_from_rfc3339(started)
        .or_else(|_| DateTime::parse_from_str(started, "%Y-%m-%dT%H:%M:%S%.f%z"));
    let e = DateTime::parse_from_rfc3339(ended)
        .or_else(|_| DateTime::parse_from_str(ended, "%Y-%m-%dT%H:%M:%S%.f%z"));
    match (s, e) {
        (Ok(s), Ok(e)) => {
            let diff = e.signed_duration_since(s);
            diff.num_minutes().max(0)
        }
        _ => 0,
    }
}

const VALID_FIELDS: &[&str] = &[
    "session_id", "project", "started_at", "ended_at", "message_count",
    "tool_call_count", "user_message_count", "first_user_message",
];

const VALID_COUNT_BY_COLUMNS: &[&str] = &["project"];

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    grep: Option<&str>,
    fields: Option<&[&str]>,
    count_by: Option<&str>,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
    timeline: bool,
) -> Result<()> {
    if timeline {
        if scope.session.is_none() {
            eprintln!("Error: --timeline requires --session");
            eprintln!("Usage: cq sessions --session <id> --timeline");
            eprintln!("Hint: Run 'cq sessions' to find session IDs");
            std::process::exit(1);
        }
        return run_timeline(conn, scope, format, wide);
    }

    // Check for conflicting flags
    super::check_count_by_fields_conflict(count_by, fields);

    // Dispatch to count-by mode
    if let Some(col) = count_by {
        let resolved = super::validate_count_by(col, VALID_COUNT_BY_COLUMNS, "sessions");
        return run_count_by(conn, scope, grep, &resolved, format, wide);
    }

    // Validate and resolve fields if specified
    if let Some(field_list) = fields {
        let resolved = super::validate_fields(field_list, VALID_FIELDS, "sessions");
        let resolved_refs: Vec<&str> = resolved.iter().map(|s| s.as_str()).collect();
        return run_with_fields(conn, scope, grep, &resolved_refs, format, limit, offset, wide);
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
        conditions.push(format!("started_at >= '{formatted}'"));
    }

    if let Some(pattern) = grep {
        conditions.push("first_user_message ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }

    let where_clause = conditions.join(" AND ");
    let limit_clause = super::limit_clause(limit);
    let offset_clause = super::offset_clause(offset);

    let sql = format!(
        "SELECT session_id, project, started_at, ended_at, message_count, tool_call_count, first_user_message
         FROM sessions
         WHERE {where_clause}
         ORDER BY started_at DESC
         {limit_clause}
         {offset_clause}"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    match format {
        OutputFormat::Json => output::print_results(&mut stmt, &param_refs, format, wide),
        _ => {
            let mut rows_iter = stmt.query(&param_refs[..])?;
            let mut session_rows: Vec<SessionRow> = Vec::new();
            while let Some(row) = rows_iter.next()? {
                let values: Vec<Value> = (0..7)
                    .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
                    .collect();
                session_rows.push(SessionRow {
                    session_id: val_str(&values[0]),
                    project: val_str(&values[1]),
                    started_at: val_str(&values[2]),
                    ended_at: val_str(&values[3]),
                    message_count: val_i64(&values[4]),
                    tool_call_count: val_i64(&values[5]),
                    first_user_message: val_str(&values[6]),
                });
            }

            if session_rows.is_empty() {
                if scope.session.is_some() {
                    super::print_session_not_found(scope.session.as_ref().unwrap());
                } else {
                    let mut extras: Vec<&str> = Vec::new();
                    if grep.is_some() { extras.push("--grep"); }
                    super::print_no_results(&scope, &extras);
                }
                return Ok(());
            }

            match format {
                OutputFormat::Table => render_table(&session_rows, wide),
                _ => render_oneline(&session_rows, wide),
            }

            super::print_truncation_hint(
                conn,
                "sessions",
                &where_clause,
                &param_refs,
                session_rows.len(),
                limit,
            );

            Ok(())
        }
    }
}

fn run_with_fields(
    conn: &Connection,
    scope: &QueryScope,
    grep: Option<&str>,
    field_list: &[&str],
    format: &OutputFormat,
    limit: usize,
    offset: usize,
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
        conditions.push(format!("started_at >= '{formatted}'"));
    }

    if let Some(pattern) = grep {
        conditions.push("first_user_message ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }

    let where_clause = conditions.join(" AND ");
    let limit_clause = super::limit_clause(limit);
    let offset_clause = super::offset_clause(offset);

    // Build SELECT with only requested columns
    let select_cols = field_list.join(", ");

    let sql = format!(
        "SELECT {select_cols}
         FROM sessions
         WHERE {where_clause}
         ORDER BY started_at DESC
         {limit_clause}
         {offset_clause}"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    output::print_results(&mut stmt, &param_refs, format, wide)
}

fn run_count_by(
    conn: &Connection,
    scope: &QueryScope,
    grep: Option<&str>,
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
        conditions.push(format!("started_at >= '{formatted}'"));
    }

    if let Some(pattern) = grep {
        conditions.push("first_user_message ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT {column}, COUNT(*) AS count
         FROM sessions
         WHERE {where_clause}
         GROUP BY {column}
         ORDER BY count DESC"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    match format {
        OutputFormat::Json | OutputFormat::Table => {
            output::print_results(&mut stmt, &param_refs, format, wide)
        }
        _ => {
            let mut rows_iter = stmt.query(&param_refs[..])?;
            let mut chart_rows: Vec<(String, i64)> = Vec::new();
            while let Some(row) = rows_iter.next()? {
                let label = row.get::<_, Value>(0)
                    .map(|v| val_str(&v))
                    .unwrap_or_default();
                let count = row.get::<_, Value>(1)
                    .map(|v| val_i64(&v))
                    .unwrap_or(0);
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

fn render_oneline(rows: &[SessionRow], wide: bool) {
    // Build plain text rows (no color) for width calculation
    let plain_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        let time_ago = if r.started_at.is_empty() {
            style::null_display().to_string()
        } else {
            style::relative_time(&r.started_at)
        };

        let project = if r.project.is_empty() {
            style::null_display().to_string()
        } else {
            project_leaf(&r.project)
        };

        let session_id = if r.session_id.is_empty() {
            style::null_display().to_string()
        } else {
            style::short_id(&r.session_id, 8)
        };

        let duration = if r.ended_at.is_empty() || r.started_at.is_empty() {
            style::null_display().to_string()
        } else {
            style::format_duration_mins(duration_mins(&r.started_at, &r.ended_at))
        };

        let msg_count = r.message_count.to_string();
        let tool_count = r.tool_call_count.to_string();

        let first_msg = if r.first_user_message.is_empty() {
            style::null_display().to_string()
        } else if wide {
            r.first_user_message.clone()
        } else {
            style::truncate(&r.first_user_message, 60)
        };

        vec![time_ago, project, session_id, duration, msg_count, tool_count, first_msg]
    }).collect();

    // Calculate column widths from plain text
    let ncols = 7;
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
                0 => style::color(&padded, style::Color::Dim),
                1 => style::color(&padded, style::Color::Primary),
                2 => style::color(&padded, style::Color::Secondary),
                3 => style::color(&padded, style::Color::Dim),
                4 => style::color(&padded, style::Color::Dim),
                5 => style::color(&padded, style::Color::Dim),
                _ => padded, // last column, no color
            }
        }).collect();
        println!("{}", cols.join("  "));
    }
}

fn render_table(rows: &[SessionRow], wide: bool) {
    let headers = ["started", "project", "session_id", "dur", "msgs", "tools", "first_user_message"];

    let string_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        let started = if r.started_at.is_empty() {
            style::null_display().to_string()
        } else {
            style::relative_time(&r.started_at)
        };

        let project = if r.project.is_empty() {
            style::null_display().to_string()
        } else {
            project_leaf(&r.project)
        };

        let session_id = if r.session_id.is_empty() {
            style::null_display().to_string()
        } else {
            style::short_id(&r.session_id, 8)
        };

        let duration = if r.ended_at.is_empty() || r.started_at.is_empty() {
            style::null_display().to_string()
        } else {
            style::format_duration_mins(duration_mins(&r.started_at, &r.ended_at))
        };

        let msg_count = r.message_count.to_string();
        let tool_count = r.tool_call_count.to_string();

        let first_msg = if r.first_user_message.is_empty() {
            style::null_display().to_string()
        } else if wide {
            r.first_user_message.clone()
        } else {
            style::truncate(&r.first_user_message, 60)
        };

        vec![started, project, session_id, duration, msg_count, tool_count, first_msg]
    }).collect();

    style::print_light_table(&headers, &string_rows);
}

/// Extract HH:MM:SS from an ISO 8601 timestamp string.
fn extract_time(ts: &str) -> String {
    // Try to find a T separator and extract the time portion
    if let Some(t_pos) = ts.find('T') {
        let after_t = &ts[t_pos + 1..];
        // Take up to 8 chars (HH:MM:SS), stopping at dot or Z
        let time_part: String = after_t
            .chars()
            .take_while(|c| *c != '.' && *c != 'Z' && *c != '+' && *c != '-')
            .collect();
        if time_part.len() >= 8 {
            return time_part[..8].to_string();
        }
        return time_part;
    }
    ts.to_string()
}

fn run_timeline(
    conn: &Connection,
    scope: &QueryScope,
    format: &OutputFormat,
    wide: bool,
) -> Result<()> {
    let session_id = scope.session.as_ref().unwrap();

    let sql = "SELECT event, timestamp, name, detail FROM (
        SELECT 'call' AS event, tc.timestamp, tc.name,
               CAST(tc.input AS VARCHAR) AS detail
        FROM tool_calls tc
        WHERE tc.session_id = ?
        UNION ALL
        SELECT 'result' AS event, tc.timestamp, tc.name,
               CASE WHEN tr.is_error THEN 'error' ELSE 'ok' END
               || ' (' || CAST(LENGTH(COALESCE(tr.content, '')) AS VARCHAR) || ' bytes)' AS detail
        FROM tool_calls tc
        JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
        WHERE tc.session_id = ?
    ) timeline
    ORDER BY timestamp, CASE WHEN event = 'call' THEN 0 ELSE 1 END";

    let params: Vec<Box<dyn duckdb::types::ToSql>> = vec![
        Box::new(session_id.clone()),
        Box::new(session_id.clone()),
    ];
    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn.prepare(sql)?;

    match format {
        OutputFormat::Json => {
            output::print_results(&mut stmt, &param_refs, format, wide)
        }
        OutputFormat::Table => {
            let mut rows_iter = stmt.query(&param_refs[..])?;
            let mut rows: Vec<Vec<String>> = Vec::new();
            while let Some(row) = rows_iter.next()? {
                let event = row.get::<_, duckdb::types::Value>(0)
                    .map(|v| val_str(&v)).unwrap_or_default();
                let timestamp = row.get::<_, duckdb::types::Value>(1)
                    .map(|v| val_str(&v)).unwrap_or_default();
                let name = row.get::<_, duckdb::types::Value>(2)
                    .map(|v| val_str(&v)).unwrap_or_default();
                let detail = row.get::<_, duckdb::types::Value>(3)
                    .map(|v| val_str(&v)).unwrap_or_default();

                let time = extract_time(&timestamp);
                let detail_display = if wide {
                    detail
                } else {
                    style::truncate(&detail, 80)
                };

                rows.push(vec![time, event, name, detail_display]);
            }

            if rows.is_empty() {
                super::print_session_not_found(session_id);
                return Ok(());
            }

            let headers = ["time", "event", "tool", "detail"];
            style::print_light_table(&headers, &rows);
            Ok(())
        }
        OutputFormat::Default => {
            let mut rows_iter = stmt.query(&param_refs[..])?;

            let mut plain_rows: Vec<Vec<String>> = Vec::new();
            while let Some(row) = rows_iter.next()? {
                let event = row.get::<_, duckdb::types::Value>(0)
                    .map(|v| val_str(&v)).unwrap_or_default();
                let timestamp = row.get::<_, duckdb::types::Value>(1)
                    .map(|v| val_str(&v)).unwrap_or_default();
                let name = row.get::<_, duckdb::types::Value>(2)
                    .map(|v| val_str(&v)).unwrap_or_default();
                let detail = row.get::<_, duckdb::types::Value>(3)
                    .map(|v| val_str(&v)).unwrap_or_default();

                let time = extract_time(&timestamp);
                let detail_display = if wide {
                    detail
                } else {
                    style::truncate(&detail, 80)
                };

                plain_rows.push(vec![time, event, name, detail_display]);
            }

            if plain_rows.is_empty() {
                super::print_session_not_found(session_id);
                return Ok(());
            }

            // Calculate column widths
            let ncols = 4;
            let mut widths = vec![0usize; ncols];
            for row in &plain_rows {
                for (i, cell) in row.iter().enumerate() {
                    if cell.len() > widths[i] {
                        widths[i] = cell.len();
                    }
                }
            }

            // Print with color
            for row in &plain_rows {
                let cols: Vec<String> = row.iter().enumerate().map(|(i, cell)| {
                    let padded = if i == ncols - 1 {
                        cell.clone()
                    } else {
                        style::pad_right(cell, widths[i])
                    };
                    match i {
                        0 => style::color(&padded, style::Color::Dim),      // time
                        1 => style::color(&padded, style::Color::Primary),   // event (call/result)
                        2 => style::color(&padded, style::Color::Primary),   // tool name
                        3 => style::color(&padded, style::Color::Secondary), // detail
                        _ => padded,
                    }
                }).collect();
                println!("{}", cols.join("  "));
            }

            Ok(())
        }
    }
}
