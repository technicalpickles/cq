use anyhow::Result;
use duckdb::types::Value;
use duckdb::Connection;

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

struct ToolFieldsRow {
    session_id: String,
    name: String,
    fields: Vec<String>,
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

const VALID_COUNT_BY_COLUMNS: &[&str] = &["name", "session_id", "project"];

#[allow(clippy::too_many_arguments)]
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    tool_name: Option<&str>,
    grep: &[String],
    result_grep: &[String],
    errors_only: bool,
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
    super::check_fields_context_conflict(fields, ctx);
    if !result_grep.is_empty() && ctx.is_some() {
        eprintln!(
            "Error: --result-grep cannot be used with -A, -B, or -C\n\
             Use --errors or --grep for context-window searches; --result-grep is detail-mode only"
        );
        std::process::exit(1);
    }

    // Dispatch to context mode
    if let Some(window) = ctx {
        return run_with_context(
            conn,
            scope,
            tool_name,
            grep,
            errors_only,
            window,
            format,
            limit,
            wide,
        );
    }

    // Dispatch to count-by mode
    if let Some(col) = count_by {
        let resolved = super::validate_count_by(col, VALID_COUNT_BY_COLUMNS, "tools");
        return run_count_by(
            conn,
            scope,
            tool_name,
            grep,
            result_grep,
            errors_only,
            &resolved,
            format,
            wide,
        );
    }

    // Summary mode: no filters specified (and no fields requested)
    if tool_name.is_none()
        && grep.is_empty()
        && result_grep.is_empty()
        && !errors_only
        && fields.is_none()
    {
        return run_summary(conn, scope, format, wide);
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

    if let Some(source) = &scope.source {
        conditions.push(crate::scope::source_filter_sql("tc."));
        params.push(Box::new(source.clone()));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("tc.timestamp >= '{formatted}'"));
    }

    if let Some(name) = tool_name {
        conditions.push("tc.name = ?".to_string());
        params.push(Box::new(name.to_string()));
    }

    if let Some(clause) = super::grep_where("CAST(tc.input AS VARCHAR)", grep) {
        conditions.push(clause);
        params.extend(super::grep_params(grep));
    }

    if errors_only {
        conditions.push("tr.is_error = true".to_string());
    }

    if let Some(clause) = super::grep_where("tr.content", result_grep) {
        conditions.push(clause);
        params.extend(super::grep_params(result_grep));
    }

    // tr.is_error / tr.content above only resolve when tool_results is joined.
    let needs_results_join = errors_only || !result_grep.is_empty();
    let results_join_clause = if needs_results_join {
        "JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id"
    } else {
        ""
    };

    let where_clause = conditions.join(" AND ");
    let limit_clause = super::limit_clause(limit);
    let offset_clause = super::offset_clause(offset);

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    // When --fields is specified, use extracted columns instead of raw input
    if let Some(field_list) = fields {
        return run_with_fields(
            conn,
            scope,
            &where_clause,
            &param_refs,
            field_list,
            needs_results_join,
            format,
            limit,
            offset,
            wide,
        );
    }

    // JSON gets full column set for scripting; display gets only what's shown
    if matches!(format, OutputFormat::Json) {
        let sql = format!(
            "SELECT tc.session_id, tc.project, tc.name, tc.tool_use_id, tc.timestamp, CAST(tc.input AS VARCHAR) AS input
             FROM tool_calls tc
             {results_join_clause}
             WHERE {where_clause}
             ORDER BY tc.timestamp DESC
             {limit_clause}
             {offset_clause}"
        );
        let mut stmt = conn.prepare(&sql)?;
        return output::print_results(&mut stmt, &param_refs, format, wide);
    }

    let sql = format!(
        "SELECT tc.session_id, tc.name, CAST(tc.input AS VARCHAR) AS input
         FROM tool_calls tc
         {results_join_clause}
         WHERE {where_clause}
         ORDER BY tc.timestamp DESC
         {limit_clause}
         {offset_clause}"
    );

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
        if let Some(session) = &scope.session {
            super::print_session_not_found(session);
        } else {
            let mut extras: Vec<&str> = Vec::new();
            if !grep.is_empty() {
                extras.push("--grep");
            }
            if !result_grep.is_empty() {
                extras.push("--result-grep");
            }
            if errors_only {
                extras.push("--errors");
            }
            if tool_name.is_some() {
                extras.push("[name]");
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
        if needs_results_join {
            "tool_calls tc JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id"
        } else {
            "tool_calls tc"
        },
        &where_clause,
        &param_refs,
        detail_rows.len(),
        limit,
    );

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_with_context(
    conn: &Connection,
    scope: &crate::scope::QueryScope,
    tool_name: Option<&str>,
    grep: &[String],
    errors_only: bool,
    window: super::ContextWindow,
    format: &OutputFormat,
    match_limit: usize,
    wide: bool,
) -> Result<()> {
    // Scope conditions for the `ordered` CTE, over `messages`.
    let mut scope_conditions = vec!["1=1".to_string()];
    if scope.project.is_some() {
        scope_conditions.push("project ILIKE ?".to_string());
    }
    if scope.session.is_some() {
        scope_conditions.push("session_id = ?".to_string());
    }
    if scope.source.is_some() {
        scope_conditions.push(crate::scope::source_filter_sql(""));
    }
    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        scope_conditions.push(format!("timestamp >= '{formatted}'"));
    }
    let scope_where = scope_conditions.join(" AND ");

    // Tool-match conditions: scope + tool name + grep, all against the tool_calls view.
    let mut tool_conditions = vec!["1=1".to_string()];
    // Initialize tool_params from scope (project + session), then add tool-specific params.
    let mut tool_params = super::build_scope_params(scope);
    if scope.project.is_some() {
        tool_conditions.push("tc.project ILIKE ?".to_string());
    }
    if scope.session.is_some() {
        tool_conditions.push("tc.session_id = ?".to_string());
    }
    if scope.source.is_some() {
        tool_conditions.push(crate::scope::source_filter_sql("tc."));
    }
    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        tool_conditions.push(format!("tc.timestamp >= '{formatted}'"));
    }
    if let Some(name) = tool_name {
        tool_conditions.push("tc.name = ?".to_string());
        tool_params.push(Box::new(name.to_string()));
    }
    if let Some(clause) = super::grep_where("CAST(tc.input AS VARCHAR)", grep) {
        tool_conditions.push(clause);
        tool_params.extend(super::grep_params(grep));
    }
    let tool_where = tool_conditions.join(" AND ");

    let errors_join = if errors_only {
        "JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id AND tr.is_error = true"
    } else {
        ""
    };

    // Materialize matches to a temp table. This avoids embedding the tool match SQL
    // twice (once in the context builder, once in the JSON enrichment join).
    // Apply match_limit here so both the context window and the enrichment JOIN see
    // the same capped set of tool calls.
    let temp_limit_clause = if match_limit > 0 {
        format!("LIMIT {match_limit}")
    } else {
        String::new()
    };
    let create_temp_sql = format!(
        "CREATE OR REPLACE TEMP TABLE cq_ctx_matches AS \
         SELECT tc.session_id, tc.message_uuid, tc.name, CAST(tc.input AS VARCHAR) AS input, \
                tc.tool_use_id, tc.timestamp \
         FROM tool_calls tc {errors_join} \
         WHERE {tool_where} \
         ORDER BY tc.timestamp \
         {temp_limit_clause}"
    );
    let tool_param_refs: Vec<&dyn duckdb::types::ToSql> =
        tool_params.iter().map(|p| p.as_ref()).collect();
    conn.execute(&create_temp_sql, &tool_param_refs[..])?;

    // Check for empty results before building the context SQL.
    let match_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM cq_ctx_matches", [], |r| r.get(0))?;
    if match_count == 0 {
        if let Some(session) = &scope.session {
            super::print_session_not_found(session);
        } else {
            let mut extras: Vec<&str> = Vec::new();
            if tool_name.is_some() {
                extras.push("[name]");
            }
            if !grep.is_empty() {
                extras.push("--grep");
            }
            if errors_only {
                extras.push("--errors");
            }
            super::print_no_results(scope, &extras);
        }
        return Ok(());
    }

    // Build the context SQL. `matches_subquery` references the temp table -- no params needed.
    // match_limit was already applied when building the temp table, so pass 0 here (unlimited).
    let matches_subquery = "SELECT session_id, message_uuid FROM cq_ctx_matches".to_string();
    let builder = super::ContextSqlBuilder {
        window,
        matches_subquery: &matches_subquery,
        ordered_scope_where: &scope_where,
        match_limit: 0,
    };
    let sql = builder.build();

    // Only scope params remain for the context SQL.
    let scope_params = super::build_scope_params(scope);
    let scope_param_refs: Vec<&dyn duckdb::types::ToSql> =
        scope_params.iter().map(|p| p.as_ref()).collect();

    match format {
        OutputFormat::Json => {
            // Heterogeneous JSON: wrap context SQL, LEFT JOIN temp table to enrich match rows.
            // Safe to interpolate: ContextSqlBuilder generates SQL from trusted fragments
            // (matches_subquery is a literal, ordered_scope_where is built from hardcoded conditions),
            // and all user input is bound via ? placeholders in scope_params.
            let wrapped = format!(
                "WITH ctx AS ({sql}) \
                 SELECT ctx.session_id, ctx.uuid, ctx.type, ctx.timestamp, ctx.text, \
                        ctx.model, ctx.tool_count, ctx.project, ctx.match_kind, ctx.match_group, \
                        m.name AS tool_name, m.input AS tool_input, m.tool_use_id \
                 FROM ctx \
                 LEFT JOIN cq_ctx_matches m \
                   ON ctx.match_kind = 'match' AND ctx.uuid = m.message_uuid \
                 ORDER BY ctx.session_id, ctx.timestamp"
            );
            let mut stmt = conn.prepare(&wrapped)?;
            output::print_results(&mut stmt, &scope_param_refs, format, wide)
        }
        OutputFormat::Table => {
            let mut stmt = conn.prepare(&sql)?;
            output::print_context_table(&mut stmt, &scope_param_refs, wide)
        }
        OutputFormat::Default => {
            let mut stmt = conn.prepare(&sql)?;
            output::print_context_rows(&mut stmt, &scope_param_refs, wide)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_with_fields(
    conn: &Connection,
    scope: &QueryScope,
    where_clause: &str,
    params: &[&dyn duckdb::types::ToSql],
    field_list: &[&str],
    needs_results_join: bool,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    // Build SELECT columns: session_id, name, then each field extracted from input JSON
    let field_columns: Vec<String> = field_list
        .iter()
        .map(|f| format!("json_extract_string(tc.input, '$.{f}') AS \"{f}\""))
        .collect();
    let field_select = field_columns.join(", ");

    let limit_clause = super::limit_clause(limit);
    let offset_clause = super::offset_clause(offset);

    // is_error/content filters (if any) are already embedded in where_clause by the caller.
    let join_clause = if needs_results_join {
        "JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id"
    } else {
        ""
    };

    // JSON mode: include extra metadata columns
    if matches!(format, OutputFormat::Json) {
        let sql = format!(
            "SELECT tc.session_id, tc.project, tc.name, tc.timestamp, {field_select}
             FROM tool_calls tc
             {join_clause}
             WHERE {where_clause}
             ORDER BY tc.timestamp DESC
             {limit_clause}
             {offset_clause}"
        );
        let mut stmt = conn.prepare(&sql)?;
        return output::print_results(&mut stmt, params, format, wide);
    }

    let sql = format!(
        "SELECT tc.session_id, tc.name, {field_select}
         FROM tool_calls tc
         {join_clause}
         WHERE {where_clause}
         ORDER BY tc.timestamp DESC
         {limit_clause}
         {offset_clause}"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(params)?;
    let num_fields = field_list.len();
    let total_cols = 2 + num_fields; // session_id, name, then fields
    let mut rows: Vec<ToolFieldsRow> = Vec::new();

    while let Some(row) = rows_iter.next()? {
        let values: Vec<Value> = (0..total_cols)
            .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
            .collect();
        rows.push(ToolFieldsRow {
            session_id: val_str(&values[0]),
            name: val_str(&values[1]),
            fields: values[2..].iter().map(val_str).collect(),
        });
    }

    if rows.is_empty() {
        if let Some(session) = &scope.session {
            super::print_session_not_found(session);
        } else {
            let mut extras: Vec<&str> = Vec::new();
            if needs_results_join {
                extras.push("--errors/--result-grep");
            }
            super::print_no_results(scope, &extras);
        }
        return Ok(());
    }

    match format {
        OutputFormat::Table => render_fields_table(&rows, field_list, wide),
        _ => render_fields_oneline(&rows, field_list, wide),
    }

    super::print_truncation_hint(
        conn,
        if needs_results_join {
            "tool_calls tc JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id"
        } else {
            "tool_calls tc"
        },
        where_clause,
        params,
        rows.len(),
        limit,
    );

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_count_by(
    conn: &Connection,
    scope: &QueryScope,
    tool_name: Option<&str>,
    grep: &[String],
    result_grep: &[String],
    errors_only: bool,
    column: &str,
    format: &OutputFormat,
    wide: bool,
) -> Result<()> {
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

    if let Some(source) = &scope.source {
        conditions.push(crate::scope::source_filter_sql("tc."));
        params.push(Box::new(source.clone()));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("tc.timestamp >= '{formatted}'"));
    }

    if let Some(name) = tool_name {
        conditions.push("tc.name = ?".to_string());
        params.push(Box::new(name.to_string()));
    }

    if let Some(clause) = super::grep_where("CAST(tc.input AS VARCHAR)", grep) {
        conditions.push(clause);
        params.extend(super::grep_params(grep));
    }

    if errors_only {
        conditions.push("tr.is_error = true".to_string());
    }

    if let Some(clause) = super::grep_where("tr.content", result_grep) {
        conditions.push(clause);
        params.extend(super::grep_params(result_grep));
    }

    let where_clause = conditions.join(" AND ");
    let join_clause = if errors_only || !result_grep.is_empty() {
        "JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id"
    } else {
        ""
    };

    let sql = format!(
        "SELECT tc.{column}, COUNT(*) AS count
         FROM tool_calls tc
         {join_clause}
         WHERE {where_clause}
         GROUP BY tc.{column}
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
        conditions.push(crate::scope::source_filter_sql(""));
        params.push(Box::new(source.clone()));
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
        OutputFormat::Json => output::print_results(&mut stmt, &param_refs, format, wide),
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
                        .map(|r| (r.name.clone(), r.count))
                        .collect();
                    super::render_bar_chart(&chart_rows);
                }
            }

            Ok(())
        }
    }
}

fn render_summary_table(rows: &[ToolSummaryRow]) {
    let headers = ["name", "count"];
    let string_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| vec![r.name.clone(), r.count.to_string()])
        .collect();
    style::print_light_table(&headers, &string_rows);
}

fn render_detail_oneline(rows: &[ToolDetailRow], wide: bool) {
    // Build plain text rows (no color) for width calculation
    let plain_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
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
            } else if wide {
                r.input.clone()
            } else {
                style::truncate(&r.input, 60)
            };

            vec![session_id, name, input]
        })
        .collect();

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

fn render_fields_oneline(rows: &[ToolFieldsRow], field_names: &[&str], wide: bool) {
    let ncols = 2 + field_names.len(); // session_id, name, then fields
    let plain_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let mut cols = vec![
                if r.session_id.is_empty() {
                    style::null_display().to_string()
                } else {
                    style::short_id(&r.session_id, 8)
                },
                if r.name.is_empty() {
                    style::null_display().to_string()
                } else {
                    r.name.clone()
                },
            ];
            for val in &r.fields {
                cols.push(if val.is_empty() {
                    style::null_display().to_string()
                } else if wide {
                    val.clone()
                } else {
                    style::truncate(val, 80)
                });
            }
            cols
        })
        .collect();

    let mut widths = vec![0usize; ncols];
    for row in &plain_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

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

fn render_fields_table(rows: &[ToolFieldsRow], field_names: &[&str], wide: bool) {
    let mut headers: Vec<&str> = vec!["session", "tool"];
    headers.extend_from_slice(field_names);

    let string_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let mut cols = vec![
                if r.session_id.is_empty() {
                    style::null_display().to_string()
                } else {
                    style::short_id(&r.session_id, 8)
                },
                if r.name.is_empty() {
                    style::null_display().to_string()
                } else {
                    r.name.clone()
                },
            ];
            for val in &r.fields {
                cols.push(if val.is_empty() {
                    style::null_display().to_string()
                } else if wide {
                    val.clone()
                } else {
                    style::truncate(val, 80)
                });
            }
            cols
        })
        .collect();

    style::print_light_table(&headers, &string_rows);
}

fn render_detail_table(rows: &[ToolDetailRow], wide: bool) {
    let headers = ["session", "tool", "input"];
    let string_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
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
            } else if wide {
                r.input.clone()
            } else {
                style::truncate(&r.input, 60)
            };

            vec![session_id, name, input]
        })
        .collect();
    style::print_light_table(&headers, &string_rows);
}
