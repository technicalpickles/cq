use anyhow::Result;
use comfy_table::{ContentArrangement, Table};
use duckdb::types::Value;
use serde_json;

pub enum OutputFormat {
    Table,
    Json,
}

pub fn print_results(stmt: &mut duckdb::Statement, format: &OutputFormat) -> Result<()> {
    let column_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    let rows: Vec<Vec<Value>> = stmt
        .query_map([], |row| {
            let values: Vec<Value> = (0..column_names.len())
                .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
                .collect();
            Ok(values)
        })?
        .collect::<Result<Vec<_>, _>>()?;

    match format {
        OutputFormat::Table => print_table(&column_names, &rows),
        OutputFormat::Json => print_json(&column_names, &rows),
    }
}

fn value_to_string(v: &Value) -> String {
    let s = match v {
        Value::Null => "NULL".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::TinyInt(n) => n.to_string(),
        Value::SmallInt(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Text(s) => s.clone(),
        other => format!("{:?}", other),
    };

    if s.len() > 120 {
        format!("{}...", &s[..120])
    } else {
        s
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
        Value::Float(f) => serde_json::Number::from_f64(*f as f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Double(d) => serde_json::Number::from_f64(*d)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Text(s) => serde_json::Value::String(s.clone()),
        other => serde_json::Value::String(format!("{:?}", other)),
    }
}

fn print_table(column_names: &[String], rows: &[Vec<Value>]) -> Result<()> {
    if rows.is_empty() {
        println!("No results.");
        return Ok(());
    }

    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(column_names);

    for row in rows {
        let cells: Vec<String> = row.iter().map(value_to_string).collect();
        table.add_row(cells);
    }

    println!("{table}");
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
