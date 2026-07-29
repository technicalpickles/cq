use anyhow::Result;
use duckdb::types::Value;
use serde_json;

use crate::style;

pub enum OutputFormat {
    Default,
    Table,
    Json,
}

pub fn print_results(
    stmt: &mut duckdb::Statement,
    params: &[&dyn duckdb::types::ToSql],
    format: &OutputFormat,
    wide: bool,
) -> Result<()> {
    let mut rows_iter = stmt.query(params)?;

    // Get column names from the executed result set
    let column_names: Vec<String> = rows_iter
        .as_ref()
        .expect("query returned no result set")
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let column_count = column_names.len();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        let values: Vec<Value> = (0..column_count)
            .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
            .collect();
        rows.push(values);
    }

    let max_width = if wide { 0 } else { 120 };

    match format {
        OutputFormat::Json => print_json(&column_names, &rows),
        _ => print_light_table_output(&column_names, &rows, max_width),
    }
}

/// A single context-window row curated down to the 4 columns `cq messages`/`cq tools`
/// show in their normal (non-context) output, formatted the same way
/// `messages::render_oneline` formats them: `session_id` short-id'd, `type` as-is,
/// `timestamp` relativized, `text` truncated unless `wide`.
struct CuratedContextRow {
    cells: [String; 4],
    is_match: bool,
    group: Option<i64>,
}

/// Raw text extraction for a DuckDB `Value`, matching the `val_str` helper duplicated
/// across `src/commands/*.rs`: `Text` unwraps, `Null` becomes an empty string (so callers
/// can apply their own `style::null_display()` fallback), anything else falls back to
/// its debug form.
fn raw_string(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Null => String::new(),
        other => format!("{:?}", other),
    }
}

/// Look up a required column's index by name, failing loudly (rather than silently
/// mis-curating) if the context query's shape ever drifts from `ContextSqlBuilder`'s
/// documented `session_id, uuid, type, timestamp, text, model, tool_count, project,
/// match_kind, match_group` output.
fn required_column_index(names: &[String], name: &str) -> Result<usize> {
    names.iter().position(|c| c == name).ok_or_else(|| {
        anyhow::anyhow!(
            "curate_context_rows: expected column '{name}' not found in context query result (columns: {names:?})"
        )
    })
}

/// Walk a context-window result set once and curate each row down to the 4 columns
/// `cq messages`/`cq tools` show normally, applying the same formatting
/// `messages::render_oneline` uses. Shared by both the TTY (`print_context_rows`) and
/// `--table` (`print_context_table`) renderers so Default and Table context output stay
/// in lockstep.
fn curate_context_rows(
    stmt: &mut duckdb::Statement,
    params: &[&dyn duckdb::types::ToSql],
    wide: bool,
) -> Result<Vec<CuratedContextRow>> {
    let mut rows_iter = stmt.query(params)?;
    let column_names: Vec<String> = rows_iter
        .as_ref()
        .expect("query returned no result set")
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let session_idx = required_column_index(&column_names, "session_id")?;
    let type_idx = required_column_index(&column_names, "type")?;
    let timestamp_idx = required_column_index(&column_names, "timestamp")?;
    let text_idx = required_column_index(&column_names, "text")?;
    let kind_idx = column_names.iter().position(|c| c == "match_kind");
    let group_idx = column_names.iter().position(|c| c == "match_group");

    let ncols = column_names.len();
    let mut out_rows: Vec<CuratedContextRow> = Vec::new();

    while let Some(row) = rows_iter.next()? {
        let values: Vec<Value> = (0..ncols)
            .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
            .collect();

        let group = group_idx.and_then(|i| match &values[i] {
            Value::BigInt(n) => Some(*n),
            Value::Int(n) => Some(*n as i64),
            Value::HugeInt(n) => i64::try_from(*n).ok(),
            _ => None,
        });
        let is_match = kind_idx
            .map(|i| matches!(&values[i], Value::Text(s) if s == "match"))
            .unwrap_or(true);

        let session_id = raw_string(&values[session_idx]);
        let msg_type = raw_string(&values[type_idx]);
        let timestamp = raw_string(&values[timestamp_idx]);
        let text = raw_string(&values[text_idx]);

        let session_cell = if session_id.is_empty() {
            style::null_display().to_string()
        } else {
            style::short_id(&session_id, 8)
        };
        let type_cell = if msg_type.is_empty() {
            style::null_display().to_string()
        } else {
            msg_type
        };
        let timestamp_cell = if timestamp.is_empty() {
            style::null_display().to_string()
        } else {
            style::relative_time(&timestamp)
        };
        let text_cell = if text.is_empty() {
            style::null_display().to_string()
        } else if wide {
            text
        } else {
            style::truncate(&text, 60)
        };

        out_rows.push(CuratedContextRow {
            cells: [session_cell, type_cell, timestamp_cell, text_cell],
            is_match,
            group,
        });
    }

    Ok(out_rows)
}

/// Render context-bearing rows for TTY (Default) output.
/// Curates the row down to `session_id, type, timestamp, text` (the same 4 columns
/// `cq messages`'s normal output shows), dropping `match_kind`/`match_group` entirely.
/// Dims rows where `match_kind != 'match'`.
/// Prints `--` separator line when `match_group` changes between consecutive rows.
///
/// Expects the input statement to include `session_id`, `type`, `timestamp`, `text`,
/// `match_kind` (text), and `match_group` (integer) columns — the shape
/// `ContextSqlBuilder::build()` produces. Column names and positions are detected at
/// runtime so this works for both message-shaped and tools-shaped queries.
pub fn print_context_rows(
    stmt: &mut duckdb::Statement,
    params: &[&dyn duckdb::types::ToSql],
    wide: bool,
) -> anyhow::Result<()> {
    let out_rows = curate_context_rows(stmt, params, wide)?;

    let mut prev_group: Option<i64> = None;
    for row in &out_rows {
        if let (Some(prev), Some(this)) = (prev_group, row.group) {
            if this != prev {
                println!("--");
            }
        }
        prev_group = row.group.or(prev_group);

        let line = row.cells.join("  ");
        if row.is_match {
            println!("{line}");
        } else {
            println!("{}", crate::style::color(&line, crate::style::Color::Dim));
        }
    }

    Ok(())
}

/// Render context-bearing rows for `--table` output.
/// Curates to the same 4 columns as `print_context_rows` (and `cq messages`'s normal
/// table output), and adds `--` group-boundary separators to match Default mode's
/// behavior — unlike the old direct `print_results` path, which showed all 10 raw
/// columns (including `match_kind`/`match_group`) with no separators.
///
/// No per-row dim styling is applied here (consistent with every other existing
/// `--table` renderer, which is plain/uncolored); only column curation and separators
/// are added.
pub fn print_context_table(
    stmt: &mut duckdb::Statement,
    params: &[&dyn duckdb::types::ToSql],
    wide: bool,
) -> anyhow::Result<()> {
    let curated_rows = curate_context_rows(stmt, params, wide)?;

    let headers = ["session_id", "type", "timestamp", "text"];
    let ncols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in &curated_rows {
        for (i, cell) in row.cells.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    let (header_line, sep_line) = style::table_header_and_separator(&headers, &widths);
    println!("{header_line}");
    println!("{sep_line}");

    let mut prev_group: Option<i64> = None;
    for row in &curated_rows {
        if let (Some(prev), Some(this)) = (prev_group, row.group) {
            if this != prev {
                println!("--");
            }
        }
        prev_group = row.group.or(prev_group);

        let cells: Vec<String> = row
            .cells
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                if i == ncols - 1 {
                    cell.clone()
                } else {
                    style::pad_right(cell, widths[i])
                }
            })
            .collect();
        println!("{}", cells.join("  "));
    }

    Ok(())
}

pub fn value_to_string(v: &Value, max_width: usize) -> String {
    let s = match v {
        Value::Null => return style::null_display().to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::TinyInt(n) => n.to_string(),
        Value::SmallInt(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::HugeInt(n) => n.to_string(),
        Value::UTinyInt(n) => n.to_string(),
        Value::USmallInt(n) => n.to_string(),
        Value::UInt(n) => n.to_string(),
        Value::UBigInt(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Decimal(d) => d.to_string(),
        Value::Text(s) => s.clone(),
        Value::Enum(s) => s.clone(),
        other => format!("{:?}", other),
    };
    if max_width == 0 {
        s
    } else {
        style::truncate(&s, max_width)
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::TinyInt(n) => serde_json::Value::Number((*n).into()),
        Value::SmallInt(n) => serde_json::Value::Number((*n).into()),
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::BigInt(n) => serde_json::Value::Number((*n).into()),
        Value::HugeInt(n) => {
            // i128 doesn't impl Into<serde_json::Number>, try i64 first
            if let Ok(n64) = i64::try_from(*n) {
                serde_json::Value::Number(n64.into())
            } else {
                serde_json::Value::String(n.to_string())
            }
        }
        Value::UTinyInt(n) => serde_json::Value::Number((*n).into()),
        Value::USmallInt(n) => serde_json::Value::Number((*n).into()),
        Value::UInt(n) => serde_json::Value::Number((*n).into()),
        Value::UBigInt(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f as f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Double(d) => serde_json::Number::from_f64(*d)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Decimal(d) => serde_json::Value::String(d.to_string()),
        Value::Text(s) => {
            // Try to parse as JSON object/array to avoid double-encoding
            if s.starts_with('{') || s.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                    return parsed;
                }
            }
            serde_json::Value::String(s.clone())
        }
        Value::Enum(s) => serde_json::Value::String(s.clone()),
        other => serde_json::Value::String(format!("{:?}", other)),
    }
}

fn print_light_table_output(
    column_names: &[String],
    rows: &[Vec<Value>],
    max_width: usize,
) -> Result<()> {
    let headers: Vec<&str> = column_names.iter().map(|s| s.as_str()).collect();
    let string_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|v| value_to_string(v, max_width)).collect())
        .collect();
    style::print_light_table(&headers, &string_rows);
    Ok(())
}

fn print_json(column_names: &[String], rows: &[Vec<Value>]) -> Result<()> {
    if rows.is_empty() {
        println!("[]");
        return Ok(());
    }

    let json_rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let obj: serde_json::Map<String, serde_json::Value> = column_names
                .iter()
                .zip(row.iter())
                .map(|(name, val)| (name.clone(), value_to_json(val)))
                .collect();
            serde_json::Value::Object(obj)
        })
        .collect();

    println!("{}", serde_json::to_string_pretty(&json_rows)?);
    Ok(())
}
