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

const VALID_FIELDS: &[&str] = &[
    "session_id", "project", "type", "timestamp", "text", "model", "tool_count",
];

const VALID_COUNT_BY_COLUMNS: &[&str] = &["type", "session_id", "project"];

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    msg_type: Option<&str>,
    grep: Option<&str>,
    fields: Option<&[&str]>,
    count_by: Option<&str>,
    ctx: Option<super::ContextWindow>,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    // Check for conflicting flags
    super::check_count_by_fields_conflict(count_by, fields);
    super::check_count_by_context_conflict(count_by, ctx);

    // Dispatch to context mode
    if let Some(window) = ctx {
        return run_with_context(conn, scope, msg_type, grep, window, format, limit, wide);
    }

    // Dispatch to count-by mode
    if let Some(col) = count_by {
        let resolved = super::validate_count_by(col, VALID_COUNT_BY_COLUMNS, "messages");
        return run_count_by(conn, scope, msg_type, grep, &resolved, format, wide);
    }

    // Validate and resolve fields if specified
    if let Some(field_list) = fields {
        let resolved = super::validate_fields(field_list, VALID_FIELDS, "messages");
        let resolved_refs: Vec<&str> = resolved.iter().map(|s| s.as_str()).collect();
        return run_with_fields(conn, scope, msg_type, grep, &resolved_refs, format, limit, offset, wide);
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
    let offset_clause = super::offset_clause(offset);

    let sql = format!(
        "SELECT session_id, type, timestamp, text
         FROM messages
         WHERE {where_clause}
         ORDER BY timestamp DESC
         {limit_clause}
         {offset_clause}"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    match format {
        OutputFormat::Json => output::print_results(&mut stmt, &param_refs, format, wide),
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

            if message_rows.is_empty() {
                if scope.session.is_some() {
                    super::print_session_not_found(scope.session.as_ref().unwrap());
                } else {
                    let mut extras: Vec<&str> = Vec::new();
                    if msg_type.is_some() { extras.push("--type"); }
                    if grep.is_some() { extras.push("--grep"); }
                    super::print_no_results(&scope, &extras);
                }
                return Ok(());
            }

            match format {
                OutputFormat::Table => render_table(&message_rows, wide),
                _ => render_oneline(&message_rows, wide),
            }

            super::print_truncation_hint(
                conn,
                "messages",
                &where_clause,
                &param_refs,
                message_rows.len(),
                limit,
            );

            Ok(())
        }
    }
}

fn run_with_context(
    conn: &Connection,
    scope: &QueryScope,
    msg_type: Option<&str>,
    grep: Option<&str>,
    window: super::ContextWindow,
    format: &OutputFormat,
    match_limit: usize,
    wide: bool,
) -> Result<()> {
    // Build scope WHERE conditions (used in both `ordered` CTE and inside matches_subquery).
    let mut scope_conditions = vec!["1=1".to_string()];

    if let Some(_project) = &scope.project {
        scope_conditions.push("project ILIKE ?".to_string());
    }
    if let Some(_session) = &scope.session {
        scope_conditions.push("session_id = ?".to_string());
    }
    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        scope_conditions.push(format!("timestamp >= '{formatted}'"));
    }
    let scope_where = scope_conditions.join(" AND ");

    // Build match-level conditions for the matches_subquery.
    let mut match_conditions = vec!["1=1".to_string()];

    if let Some(_t) = msg_type {
        match_conditions.push("type = ?".to_string());
    }
    if let Some(_pattern) = grep {
        match_conditions.push("text ILIKE ?".to_string());
    }
    let match_where = match_conditions.join(" AND ");

    // matches_subquery projects session_id + message_uuid and filters by scope + match conditions.
    let matches_subquery = format!(
        "SELECT session_id, uuid AS message_uuid FROM messages WHERE {scope_where} AND {match_where}"
    );

    let builder = super::ContextSqlBuilder {
        window,
        matches_subquery: &matches_subquery,
        ordered_scope_where: &scope_where,
        match_limit,
    };
    let sql = builder.build();

    // Param order matches the SQL generation order:
    //   1. scope_params for the `ordered` CTE WHERE clause
    //   2. scope_params again (duplicated because matches_subquery embeds scope_where inline)
    //   3. match_params for the matches_subquery additional conditions
    let mut all_params: Vec<Box<dyn duckdb::types::ToSql>> = Vec::new();
    // scope params appear twice: once for `ordered` CTE, once inside matches_subquery's embedded WHERE
    all_params.extend(super::build_scope_params(scope));
    all_params.extend(super::build_scope_params(scope));
    // then match-level params (type, grep)
    if let Some(t) = msg_type {
        all_params.push(Box::new(t.to_string()));
    }
    if let Some(pattern) = grep {
        all_params.push(Box::new(format!("%{pattern}%")));
    }

    let param_refs: Vec<&dyn duckdb::types::ToSql> = all_params.iter().map(|p| p.as_ref()).collect();

    match format {
        OutputFormat::Json => {
            let mut stmt = conn.prepare(&sql)?;
            output::print_results(&mut stmt, &param_refs, format, wide)
        }
        OutputFormat::Table => {
            let mut stmt = conn.prepare(&sql)?;
            output::print_results(&mut stmt, &param_refs, format, wide)
        }
        OutputFormat::Default => {
            let mut stmt = conn.prepare(&sql)?;
            output::print_context_rows(&mut stmt, &param_refs, wide)
        }
    }
}

fn run_with_fields(
    conn: &Connection,
    scope: &QueryScope,
    msg_type: Option<&str>,
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
    let offset_clause = super::offset_clause(offset);

    // Build SELECT with only requested columns
    let select_cols = field_list.join(", ");

    let sql = format!(
        "SELECT {select_cols}
         FROM messages
         WHERE {where_clause}
         ORDER BY timestamp DESC
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
    msg_type: Option<&str>,
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
        "SELECT {column}, COUNT(*) AS count
         FROM messages
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

fn val_i64(v: &Value) -> i64 {
    match v {
        Value::TinyInt(n) => *n as i64,
        Value::SmallInt(n) => *n as i64,
        Value::Int(n) => *n as i64,
        Value::BigInt(n) => *n,
        _ => 0,
    }
}

fn render_oneline(rows: &[MessageRow], wide: bool) {
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
        } else if wide {
            r.text.clone()
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

fn render_table(rows: &[MessageRow], wide: bool) {
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
        } else if wide {
            r.text.clone()
        } else {
            style::truncate(&r.text, 60)
        };

        vec![session_id, msg_type, timestamp, text]
    }).collect();

    style::print_light_table(&headers, &string_rows);
}
