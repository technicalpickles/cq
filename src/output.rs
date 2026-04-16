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

/// Render context-bearing rows for TTY (Default) output.
/// Drops `match_kind` and `match_group` columns from the visible output.
/// Dims rows where `match_kind != 'match'`.
/// Prints `--` separator line when `match_group` changes between consecutive rows.
///
/// Expects the input statement to include both `match_kind` (text) and
/// `match_group` (integer) columns. Column names and positions are detected at
/// runtime so this works for both message-shaped and tools-shaped queries.
pub fn print_context_rows(
    stmt: &mut duckdb::Statement,
    params: &[&dyn duckdb::types::ToSql],
    wide: bool,
) -> anyhow::Result<()> {
    let mut rows_iter = stmt.query(params)?;
    let column_names: Vec<String> = rows_iter
        .as_ref()
        .expect("query returned no result set")
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let kind_idx = column_names.iter().position(|c| c == "match_kind");
    let group_idx = column_names.iter().position(|c| c == "match_group");

    // Indices to display (everything except the two metadata columns).
    let display_indices: Vec<usize> = (0..column_names.len())
        .filter(|i| Some(*i) != kind_idx && Some(*i) != group_idx)
        .collect();

    let max_width = if wide { 0 } else { 120 };

    struct OutRow {
        cells: Vec<String>,
        is_match: bool,
        group: Option<i64>,
    }
    let mut out_rows: Vec<OutRow> = Vec::new();

    while let Some(row) = rows_iter.next()? {
        let values: Vec<Value> = (0..column_names.len())
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

        let cells: Vec<String> = display_indices
            .iter()
            .map(|&i| value_to_string(&values[i], max_width))
            .collect();

        out_rows.push(OutRow { cells, is_match, group });
    }

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

fn print_light_table_output(column_names: &[String], rows: &[Vec<Value>], max_width: usize) -> Result<()> {
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
