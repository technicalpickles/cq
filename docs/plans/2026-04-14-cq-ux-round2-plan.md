# cq UX Round 2: Output Format Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace comfy-table output with per-command formatters (oneline, bar chart, light table), add color via owo-colors, and add --table/--no-color flags.

**Architecture:** New `style` module provides shared formatting (color, padding, truncation, time). Each command owns its rendering logic and dispatches based on `OutputFormat`. The generic `print_results` in output.rs switches from comfy-table to light table style for `cq sql`.

**Tech Stack:** Rust, owo-colors 4, DuckDB, clap 4

**Spec:** `docs/specs/2026-04-14-cq-ux-round2-design.md`

---

## File Structure

```
src/
├── style.rs         # NEW: color, truncation, time formatting, column alignment, bar rendering
├── output.rs        # MODIFY: remove comfy-table, use style:: for light table in print_results
├── main.rs          # MODIFY: add --table/--no-color flags, wire color override
├── lib.rs           # MODIFY: add pub mod style
├── commands/
│   ├── sessions.rs  # MODIFY: add oneline/table renderers, fetch rows as structs
│   ├── tools.rs     # MODIFY: add bar chart/oneline/table renderers
│   └── messages.rs  # MODIFY: add oneline/table renderers
Cargo.toml           # MODIFY: add owo-colors, remove comfy-table
tests/
└── integration_test.rs  # MODIFY: update assertions for new output format
```

---

## Task 1: Add owo-colors and update OutputFormat enum

Foundation: add the color dep, expand `OutputFormat`, add new CLI flags. No rendering changes yet.

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/output.rs:1-9`
- Modify: `src/main.rs:9-38,87-94`
- Modify: `src/lib.rs`

- [ ] **Step 1: Update Cargo.toml**

Add owo-colors, remove comfy-table:

```toml
[dependencies]
duckdb = { version = "1", features = ["bundled", "json"] }
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = "0.4"
anyhow = "1"
dirs = "5"
owo-colors = { version = "4", features = ["supports-colors"] }
```

Remove the `comfy-table = "7"` line.

- [ ] **Step 2: Update OutputFormat enum in output.rs**

Replace the existing `OutputFormat` enum. Remove the `use comfy_table` import.

```rust
// src/output.rs - top of file
use anyhow::Result;
use duckdb::types::Value;
use serde_json;

pub enum OutputFormat {
    Default,
    Table,
    Json,
}
```

- [ ] **Step 3: Add --table and --no-color flags to main.rs**

Update the `Cli` struct:

```rust
#[derive(Parser)]
#[command(name = "cq", about = "Query AI agent session transcripts with SQL")]
struct Cli {
    /// Scope to a project (substring match)
    #[arg(short = 'p', long, global = true)]
    project: Option<String>,

    /// Scope to a session (prefix match)
    #[arg(short = 's', long, global = true)]
    session: Option<String>,

    /// Time filter (e.g. 7d, 24h, 30m)
    #[arg(long, global = true)]
    since: Option<String>,

    /// Force full reindex of session files
    #[arg(long, global = true)]
    reindex: bool,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Output as aligned table with header
    #[arg(long, global = true)]
    table: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    /// Maximum number of results
    #[arg(long, global = true, default_value_t = 50)]
    limit: usize,

    #[command(subcommand)]
    command: Command,
}
```

Update the format construction and add color override in `main()`:

```rust
fn main() -> Result<()> {
    let cli = Cli::parse();

    // Disable color if --no-color or NO_COLOR env var
    if cli.no_color || std::env::var("NO_COLOR").is_ok() {
        owo_colors::set_override(false);
    }

    // --json wins over --table
    let format = if cli.json {
        OutputFormat::Json
    } else if cli.table {
        OutputFormat::Table
    } else {
        OutputFormat::Default
    };

    // ... rest unchanged
```

Add the `use owo_colors` import to main.rs (only needed for `set_override`; the actual coloring happens in style.rs).

- [ ] **Step 4: Add `pub mod style;` to lib.rs**

```rust
pub mod cache;
pub mod indexer;
pub mod scope;
pub mod provider;
pub mod claude_provider;
pub mod views;
pub mod db;
pub mod output;
pub mod style;
pub mod commands;
```

- [ ] **Step 5: Create empty style.rs placeholder**

```rust
// src/style.rs
// Formatting helpers for cq output. Populated in Task 2.
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo build`

Expected: Compiles with warnings about unused `OutputFormat::Default` and `OutputFormat::Table` variants, and unused `style` module. No errors.

- [ ] **Step 7: Commit**

```
git add Cargo.toml src/output.rs src/main.rs src/lib.rs src/style.rs
git commit -m "refactor: add owo-colors, expand OutputFormat, add --table/--no-color flags"
```

---

## Task 2: Build the style module

All shared formatting helpers. Pure functions, easy to unit test.

**Files:**
- Create: `src/style.rs`

- [ ] **Step 1: Write unit tests for style helpers**

Add tests at the bottom of `src/style.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world, this is long", 10), "hello w...");
    }

    #[test]
    fn test_null_display() {
        assert_eq!(null_display(), "-");
    }

    #[test]
    fn test_short_id_8() {
        assert_eq!(short_id("c82e9d4c-4344-4022-a275-be14733e377e", 8), "c82e9d4c");
    }

    #[test]
    fn test_short_id_0() {
        assert_eq!(short_id("c82e9d4c-4344-4022-a275-be14733e377e", 0), "");
    }

    #[test]
    fn test_short_id_full() {
        let full = "c82e9d4c-4344-4022-a275-be14733e377e";
        assert_eq!(short_id(full, 36), full);
    }

    #[test]
    fn test_relative_time_minutes() {
        let now = chrono::Utc::now();
        let ts = (now - chrono::Duration::minutes(16))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        let result = relative_time(&ts);
        assert!(result == "16m ago" || result == "15m ago" || result == "17m ago",
                "Expected ~16m ago, got: {result}");
    }

    #[test]
    fn test_relative_time_hours() {
        let now = chrono::Utc::now();
        let ts = (now - chrono::Duration::hours(3))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        assert_eq!(relative_time(&ts), "3h ago");
    }

    #[test]
    fn test_relative_time_days() {
        let now = chrono::Utc::now();
        let ts = (now - chrono::Duration::days(5))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        assert_eq!(relative_time(&ts), "5d ago");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration_mins(16), "16m");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration_mins(150), "2h30m");
    }

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration_mins(0), "<1m");
    }

    #[test]
    fn test_align_columns() {
        let rows = vec![
            vec!["short".to_string(), "a".to_string()],
            vec!["longer text".to_string(), "bb".to_string()],
        ];
        let result = align_columns(&rows);
        assert_eq!(result[0], "short        a ");
        assert_eq!(result[1], "longer text  bb");
    }

    #[test]
    fn test_bar() {
        let result = bar(50, 100, 20);
        assert_eq!(result.len(), 10);
        assert!(result.chars().all(|c| c == '█'));
    }

    #[test]
    fn test_bar_minimum() {
        let result = bar(1, 10000, 30);
        assert_eq!(result.len(), 1 * '█'.len_utf8());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test style`

Expected: Compilation errors (functions don't exist yet).

- [ ] **Step 3: Implement style.rs**

```rust
// src/style.rs
use chrono::{DateTime, Utc};
use owo_colors::{OwoColorize, Stream::Stdout};

/// Semantic color roles for cq output
pub enum Color {
    Primary,    // blue: project names, tool names
    Secondary,  // yellow: session IDs
    Dim,        // dimmed: timestamps, counts, metadata
    Bar,        // green: bar chart fill
}

/// Apply semantic color. Automatically respects NO_COLOR, --no-color, and TTY detection.
pub fn color(text: &str, role: Color) -> String {
    match role {
        Color::Primary => text.if_supports_color(Stdout, |t| t.blue()).to_string(),
        Color::Secondary => text.if_supports_color(Stdout, |t| t.yellow()).to_string(),
        Color::Dim => text.if_supports_color(Stdout, |t| t.dimmed()).to_string(),
        Color::Bar => text.if_supports_color(Stdout, |t| t.green()).to_string(),
    }
}

/// Truncate text to max chars, appending "..." if truncated.
pub fn truncate(s: &str, max: usize) -> String {
    if max < 4 {
        return s.chars().take(max).collect();
    }
    if s.len() <= max {
        s.to_string()
    } else {
        let mut result: String = s.chars().take(max - 3).collect();
        result.push_str("...");
        result
    }
}

/// Display string for null values.
pub fn null_display() -> &'static str {
    "-"
}

/// First `len` characters of a UUID/session ID.
pub fn short_id(id: &str, len: usize) -> String {
    id.chars().take(len).collect()
}

/// Convert an ISO timestamp to a relative time string like "16m ago", "2h ago", "3d ago".
pub fn relative_time(iso_ts: &str) -> String {
    let parsed = DateTime::parse_from_rfc3339(iso_ts)
        .or_else(|_| DateTime::parse_from_str(iso_ts, "%Y-%m-%dT%H:%M:%S%.fZ"))
        .map(|dt| dt.with_timezone(&Utc));

    let ts = match parsed {
        Ok(ts) => ts,
        Err(_) => return iso_ts.to_string(), // fallback: return raw
    };

    let delta = Utc::now() - ts;
    let mins = delta.num_minutes();

    if mins < 1 {
        "just now".to_string()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 1440 {
        format!("{}h ago", mins / 60)
    } else {
        format!("{}d ago", mins / 1440)
    }
}

/// Format a duration in minutes as "16m", "2h30m", or "<1m".
pub fn format_duration_mins(mins: i64) -> String {
    if mins < 1 {
        "<1m".to_string()
    } else if mins < 60 {
        format!("{mins}m")
    } else {
        let h = mins / 60;
        let m = mins % 60;
        if m > 0 {
            format!("{h}h{m}m")
        } else {
            format!("{h}h")
        }
    }
}

/// Pad a string to `width` with trailing spaces.
pub fn pad_right(s: &str, width: usize) -> String {
    if s.len() >= width {
        s.to_string()
    } else {
        format!("{s:<width$}")
    }
}

/// Pad a string to `width` with leading spaces.
pub fn pad_left(s: &str, width: usize) -> String {
    if s.len() >= width {
        s.to_string()
    } else {
        format!("{s:>width$}")
    }
}

/// Align a table of string rows into padded, double-space-separated lines.
/// Each inner Vec is a row of cell strings.
pub fn align_columns(rows: &[Vec<String>]) -> Vec<String> {
    if rows.is_empty() {
        return vec![];
    }
    let col_count = rows[0].len();
    let mut widths = vec![0usize; col_count];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_count && cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }
    rows.iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, cell)| pad_right(cell, widths[i]))
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect()
}

/// Print a light table: header row, unicode separator, then data rows.
/// `headers` and each row in `rows` must have the same length.
pub fn print_light_table(headers: &[&str], rows: &[Vec<String>]) {
    if rows.is_empty() {
        eprintln!("No results.");
        return;
    }
    let col_count = headers.len();
    let mut widths = vec![0usize; col_count];
    for (i, h) in headers.iter().enumerate() {
        widths[i] = h.len();
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_count && cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }
    // Header
    let header_line: String = headers
        .iter()
        .enumerate()
        .map(|(i, h)| pad_right(h, widths[i]))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{}", color(&header_line, Color::Dim));

    // Separator
    let sep: String = widths
        .iter()
        .map(|w| "─".repeat(*w))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{}", color(&sep, Color::Dim));

    // Data rows
    for row in rows {
        let line: String = row
            .iter()
            .enumerate()
            .map(|(i, cell)| pad_right(cell, widths[i]))
            .collect::<Vec<_>>()
            .join("  ");
        println!("{line}");
    }
}

/// Render a proportional bar of `█` characters.
pub fn bar(value: i64, max_value: i64, max_width: usize) -> String {
    if max_value == 0 {
        return String::new();
    }
    let len = ((value as f64 / max_value as f64) * max_width as f64).round() as usize;
    let len = len.max(1); // minimum 1 char
    "█".repeat(len)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test style`

Expected: All 16 tests pass.

- [ ] **Step 5: Commit**

```
git add src/style.rs
git commit -m "feat: add style module with color, truncation, time, and layout helpers"
```

---

## Task 3: Rewrite output.rs (drop comfy-table)

Replace the comfy-table table renderer with light table style. Keep JSON renderer. `cq sql` uses this generic path.

**Files:**
- Modify: `src/output.rs`

- [ ] **Step 1: Rewrite output.rs**

```rust
// src/output.rs
use anyhow::Result;
use duckdb::types::Value;
use serde_json;

use crate::style;

pub enum OutputFormat {
    Default,
    Table,
    Json,
}

/// Convert a DuckDB Value to a display string. NULL becomes "-".
pub fn value_to_string(v: &Value) -> String {
    let s = match v {
        Value::Null => return style::null_display().to_string(),
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
    style::truncate(&s, 120)
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

/// Generic result printer for cq sql and fallback paths.
/// Uses light table for Default and Table formats, JSON for Json.
pub fn print_results(
    stmt: &mut duckdb::Statement,
    params: &[&dyn duckdb::types::ToSql],
    format: &OutputFormat,
) -> Result<()> {
    let mut rows_iter = stmt.query(params)?;

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

    match format {
        OutputFormat::Json => print_json(&column_names, &rows),
        _ => print_light_table(&column_names, &rows),
    }
}

fn print_light_table(column_names: &[String], rows: &[Vec<Value>]) -> Result<()> {
    let headers: Vec<&str> = column_names.iter().map(|s| s.as_str()).collect();
    let string_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(value_to_string).collect())
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
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build`

Expected: Compiles. No comfy-table references remain.

- [ ] **Step 3: Run existing tests**

Run: `cargo test`

Expected: Most pass. Integration tests that assert on table border characters (`+------+`) will fail. That's expected and gets fixed in Task 7.

- [ ] **Step 4: Commit**

```
git add src/output.rs
git commit -m "refactor: replace comfy-table with light table renderer in output.rs"
```

---

## Task 4: Add per-command rendering to sessions.rs

Sessions gets oneline (default) and table renderers. The command fetches rows, converts to structs, then dispatches to the right renderer.

**Files:**
- Modify: `src/commands/sessions.rs`

- [ ] **Step 1: Rewrite sessions.rs**

```rust
// src/commands/sessions.rs
use anyhow::Result;
use duckdb::types::Value;
use duckdb::Connection;

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

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
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
        conditions.push(format!("started_at >= '{formatted}'"));
    }

    if let Some(pattern) = grep {
        conditions.push("first_user_message ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT session_id, project, started_at, ended_at, message_count, tool_call_count, first_user_message
         FROM sessions
         WHERE {where_clause}
         ORDER BY started_at DESC
         LIMIT {limit}"
    );

    // For JSON, use the generic output path
    if matches!(format, OutputFormat::Json) {
        let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        return output::print_results(&mut stmt, &param_refs, format);
    }

    // Fetch rows into structs for custom rendering
    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(&*param_refs)?;

    let mut rows: Vec<SessionRow> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        rows.push(SessionRow {
            session_id: row.get::<_, Value>(0).map(|v| val_str(&v)).unwrap_or_default(),
            project: row.get::<_, Value>(1).map(|v| val_str(&v)).unwrap_or_default(),
            started_at: row.get::<_, Value>(2).map(|v| val_str(&v)).unwrap_or_default(),
            ended_at: row.get::<_, Value>(3).map(|v| val_str(&v)).unwrap_or_default(),
            message_count: row.get::<_, Value>(4).map(|v| val_i64(&v)).unwrap_or(0),
            tool_call_count: row.get::<_, Value>(5).map(|v| val_i64(&v)).unwrap_or(0),
            first_user_message: row.get::<_, Value>(6).map(|v| val_str(&v)).unwrap_or_default(),
        });
    }

    if rows.is_empty() {
        eprintln!("No results.");
        return Ok(());
    }

    match format {
        OutputFormat::Default => render_oneline(&rows),
        OutputFormat::Table => render_table(&rows),
        OutputFormat::Json => unreachable!(),
    }

    Ok(())
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
        Value::BigInt(n) => *n,
        Value::Int(n) => *n as i64,
        Value::TinyInt(n) => *n as i64,
        Value::SmallInt(n) => *n as i64,
        _ => 0,
    }
}

fn duration_mins(started: &str, ended: &str) -> i64 {
    let parse = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .or_else(|_| chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ"))
            .ok()
    };
    match (parse(started), parse(ended)) {
        (Some(s), Some(e)) => (e - s).num_minutes(),
        _ => 0,
    }
}

fn render_oneline(rows: &[SessionRow]) {
    // Build plain rows for alignment, then apply color after padding.
    // Color codes contain ANSI escapes that mess up width calculation,
    // so we pad first, color second.
    let plain_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let msg = if r.first_user_message.is_empty() {
                style::null_display().to_string()
            } else {
                style::truncate(&r.first_user_message, 60)
            };
            vec![
                style::relative_time(&r.started_at),
                project_leaf(&r.project),
                style::short_id(&r.session_id, 8),
                style::format_duration_mins(duration_mins(&r.started_at, &r.ended_at)),
                format!("{}msg", r.message_count),
                format!("{}t", r.tool_call_count),
                msg,
            ]
        })
        .collect();

    // Calculate widths from plain text
    let col_count = plain_rows[0].len();
    let mut widths = vec![0usize; col_count];
    for row in &plain_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    // Print with color applied to padded cells
    for row in &plain_rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let padded = style::pad_right(cell, widths[i]);
                match i {
                    0 => style::color(&padded, style::Color::Dim),      // time ago
                    1 => style::color(&padded, style::Color::Primary),   // project
                    2 => style::color(&padded, style::Color::Secondary), // session ID
                    3 => style::color(&padded, style::Color::Dim),       // duration
                    4 => style::color(&padded, style::Color::Dim),       // msg count
                    5 => style::color(&padded, style::Color::Dim),       // tool count
                    _ => padded,                                          // message
                }
            })
            .collect();
        println!("{}", cells.join("  "));
    }
}

fn render_table(rows: &[SessionRow]) {
    let headers = &["started", "project", "session_id", "dur", "msgs", "tools", "first_user_message"];
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let msg = if r.first_user_message.is_empty() {
                style::null_display().to_string()
            } else {
                style::truncate(&r.first_user_message, 60)
            };
            vec![
                style::relative_time(&r.started_at),
                project_leaf(&r.project),
                style::short_id(&r.session_id, 8),
                style::format_duration_mins(duration_mins(&r.started_at, &r.ended_at)),
                r.message_count.to_string(),
                r.tool_call_count.to_string(),
                msg,
            ]
        })
        .collect();
    style::print_light_table(headers, &table_rows);
}

/// Extract the last path component as the project display name.
fn project_leaf(project: &str) -> String {
    project
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(project)
        .to_string()
}
```

- [ ] **Step 2: Test manually**

Run: `cargo run -- sessions --limit 5`

Expected: One-line format with color, relative timestamps, short IDs.

Run: `cargo run -- --table sessions --limit 5`

Expected: Light table with header separator.

Run: `cargo run -- --json sessions --limit 5`

Expected: JSON array (unchanged).

- [ ] **Step 3: Run tests**

Run: `cargo test`

Expected: Compiles. Some integration tests may need updating (Task 7).

- [ ] **Step 4: Commit**

```
git add src/commands/sessions.rs
git commit -m "feat: add oneline and light table renderers for sessions"
```

---

## Task 5: Add per-command rendering to tools.rs

Tools summary gets bar chart (default) and table. Tool detail gets oneline (default) and table.

**Files:**
- Modify: `src/commands/tools.rs`

- [ ] **Step 1: Rewrite tools.rs**

```rust
// src/commands/tools.rs
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
    project: String,
    name: String,
    timestamp: String,
    input: String,
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
    if tool_name.is_none() && grep.is_none() && !errors_only {
        return run_summary(conn, scope, format, limit);
    }
    run_detail(conn, scope, tool_name, grep, errors_only, format, limit)
}

fn run_summary(
    conn: &Connection,
    scope: &QueryScope,
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

    let where_clause = conditions.join(" AND ");

    let sql = format!(
        "SELECT name, COUNT(*) AS count
         FROM tool_calls
         WHERE {where_clause}
         GROUP BY name
         ORDER BY count DESC
         LIMIT {limit}"
    );

    if matches!(format, OutputFormat::Json) {
        let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        return output::print_results(&mut stmt, &param_refs, format);
    }

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(&*param_refs)?;

    let mut rows: Vec<ToolSummaryRow> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        rows.push(ToolSummaryRow {
            name: row.get::<_, Value>(0).map(|v| val_str(&v)).unwrap_or_default(),
            count: row.get::<_, Value>(1).map(|v| val_i64(&v)).unwrap_or(0),
        });
    }

    if rows.is_empty() {
        eprintln!("No results.");
        return Ok(());
    }

    match format {
        OutputFormat::Default => render_bar_chart(&rows),
        OutputFormat::Table => render_summary_table(&rows),
        OutputFormat::Json => unreachable!(),
    }

    Ok(())
}

fn run_detail(
    conn: &Connection,
    scope: &QueryScope,
    tool_name: Option<&str>,
    grep: Option<&str>,
    errors_only: bool,
    format: &OutputFormat,
    limit: usize,
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

    let sql = if errors_only {
        format!(
            "SELECT tc.session_id, tc.project, tc.name, tc.timestamp, CAST(tc.input AS VARCHAR) AS input
             FROM tool_calls tc
             JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
             WHERE {where_clause}
             AND tr.is_error = true
             ORDER BY tc.timestamp DESC
             LIMIT {limit}"
        )
    } else {
        format!(
            "SELECT tc.session_id, tc.project, tc.name, tc.timestamp, CAST(tc.input AS VARCHAR) AS input
             FROM tool_calls tc
             WHERE {where_clause}
             ORDER BY tc.timestamp DESC
             LIMIT {limit}"
        )
    };

    if matches!(format, OutputFormat::Json) {
        let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        return output::print_results(&mut stmt, &param_refs, format);
    }

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(&*param_refs)?;

    let mut rows: Vec<ToolDetailRow> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        rows.push(ToolDetailRow {
            session_id: row.get::<_, Value>(0).map(|v| val_str(&v)).unwrap_or_default(),
            project: row.get::<_, Value>(1).map(|v| val_str(&v)).unwrap_or_default(),
            name: row.get::<_, Value>(2).map(|v| val_str(&v)).unwrap_or_default(),
            timestamp: row.get::<_, Value>(3).map(|v| val_str(&v)).unwrap_or_default(),
            input: row.get::<_, Value>(4).map(|v| val_str(&v)).unwrap_or_default(),
        });
    }

    if rows.is_empty() {
        eprintln!("No results.");
        return Ok(());
    }

    match format {
        OutputFormat::Default => render_detail_oneline(&rows),
        OutputFormat::Table => render_detail_table(&rows),
        OutputFormat::Json => unreachable!(),
    }

    Ok(())
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
        Value::BigInt(n) => *n,
        Value::Int(n) => *n as i64,
        Value::TinyInt(n) => *n as i64,
        Value::SmallInt(n) => *n as i64,
        _ => 0,
    }
}

fn render_bar_chart(rows: &[ToolSummaryRow]) {
    let max_count = rows.iter().map(|r| r.count).max().unwrap_or(1);
    let name_width = rows.iter().map(|r| r.name.len()).max().unwrap_or(4);
    let count_width = rows.iter().map(|r| r.count.to_string().len()).max().unwrap_or(1);
    let bar_width = 30;

    for row in rows {
        let name = style::color(&style::pad_right(&row.name, name_width), style::Color::Primary);
        let bar_str = style::bar(row.count, max_count, bar_width);
        let bar_colored = style::color(&bar_str, style::Color::Bar);
        let count = style::color(&style::pad_left(&row.count.to_string(), count_width), style::Color::Dim);
        println!("{name}  {bar_colored}  {count}");
    }
}

fn render_summary_table(rows: &[ToolSummaryRow]) {
    let headers = &["name", "count"];
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| vec![r.name.clone(), r.count.to_string()])
        .collect();
    style::print_light_table(headers, &table_rows);
}

fn render_detail_oneline(rows: &[ToolDetailRow]) {
    let plain_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let input = if r.input.is_empty() {
                style::null_display().to_string()
            } else {
                style::truncate(&r.input, 60)
            };
            vec![
                style::short_id(&r.session_id, 8),
                r.name.clone(),
                input,
            ]
        })
        .collect();

    let col_count = plain_rows[0].len();
    let mut widths = vec![0usize; col_count];
    for row in &plain_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    for row in &plain_rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let padded = style::pad_right(cell, widths[i]);
                match i {
                    0 => style::color(&padded, style::Color::Secondary), // session ID
                    1 => style::color(&padded, style::Color::Primary),   // tool name
                    _ => padded,                                          // input
                }
            })
            .collect();
        println!("{}", cells.join("  "));
    }
}

fn render_detail_table(rows: &[ToolDetailRow]) {
    let headers = &["session", "tool", "input"];
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let input = if r.input.is_empty() {
                style::null_display().to_string()
            } else {
                style::truncate(&r.input, 60)
            };
            vec![
                style::short_id(&r.session_id, 8),
                r.name.clone(),
                input,
            ]
        })
        .collect();
    style::print_light_table(headers, &table_rows);
}
```

Note: The detail SQL drops `tool_use_id` from the SELECT since it's not displayed.

- [ ] **Step 2: Test manually**

Run: `cargo run -- tools`

Expected: Bar chart with colored tool names, green bars, dim counts.

Run: `cargo run -- --table tools`

Expected: Light table with name/count.

Run: `cargo run -- tools Bash --limit 5`

Expected: One-line format with session ID, tool name, input.

- [ ] **Step 3: Commit**

```
git add src/commands/tools.rs
git commit -m "feat: add bar chart and oneline renderers for tools command"
```

---

## Task 6: Add per-command rendering to messages.rs

**Files:**
- Modify: `src/commands/messages.rs`

- [ ] **Step 1: Rewrite messages.rs**

```rust
// src/commands/messages.rs
use anyhow::Result;
use duckdb::types::Value;
use duckdb::Connection;

use crate::output::{self, OutputFormat};
use crate::scope::QueryScope;
use crate::style;

struct MessageRow {
    session_id: String,
    msg_type: String,
    timestamp: String,
    text: String,
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

    let sql = format!(
        "SELECT session_id, type, timestamp, text
         FROM messages
         WHERE {where_clause}
         ORDER BY timestamp DESC
         LIMIT {limit}"
    );

    if matches!(format, OutputFormat::Json) {
        let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        return output::print_results(&mut stmt, &param_refs, format);
    }

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(&*param_refs)?;

    let mut rows: Vec<MessageRow> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        rows.push(MessageRow {
            session_id: row.get::<_, Value>(0).map(|v| val_str(&v)).unwrap_or_default(),
            msg_type: row.get::<_, Value>(1).map(|v| val_str(&v)).unwrap_or_default(),
            timestamp: row.get::<_, Value>(2).map(|v| val_str(&v)).unwrap_or_default(),
            text: row.get::<_, Value>(3).map(|v| val_str(&v)).unwrap_or_default(),
        });
    }

    if rows.is_empty() {
        eprintln!("No results.");
        return Ok(());
    }

    match format {
        OutputFormat::Default => render_oneline(&rows),
        OutputFormat::Table => render_table(&rows),
        OutputFormat::Json => unreachable!(),
    }

    Ok(())
}

fn val_str(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Null => String::new(),
        other => format!("{:?}", other),
    }
}

fn render_oneline(rows: &[MessageRow]) {
    let plain_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let text = if r.text.is_empty() {
                style::null_display().to_string()
            } else {
                style::truncate(&r.text, 60)
            };
            vec![
                style::short_id(&r.session_id, 8),
                r.msg_type.clone(),
                style::relative_time(&r.timestamp),
                text,
            ]
        })
        .collect();

    let col_count = plain_rows[0].len();
    let mut widths = vec![0usize; col_count];
    for row in &plain_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    for row in &plain_rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let padded = style::pad_right(cell, widths[i]);
                match i {
                    0 => style::color(&padded, style::Color::Secondary), // session ID
                    1 => style::color(&padded, style::Color::Primary),   // type
                    2 => style::color(&padded, style::Color::Dim),       // timestamp
                    _ => padded,                                          // text
                }
            })
            .collect();
        println!("{}", cells.join("  "));
    }
}

fn render_table(rows: &[MessageRow]) {
    let headers = &["session_id", "type", "timestamp", "text"];
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let text = if r.text.is_empty() {
                style::null_display().to_string()
            } else {
                style::truncate(&r.text, 60)
            };
            vec![
                style::short_id(&r.session_id, 8),
                r.msg_type.clone(),
                style::relative_time(&r.timestamp),
                text,
            ]
        })
        .collect();
    style::print_light_table(headers, &table_rows);
}
```

- [ ] **Step 2: Test manually**

Run: `cargo run -- messages --limit 5`

Expected: One-line format with colored session IDs, type, relative time, text.

- [ ] **Step 3: Commit**

```
git add src/commands/messages.rs
git commit -m "feat: add oneline and light table renderers for messages"
```

---

## Task 7: Update integration tests

The integration tests assert on output content. The format has changed: no more table borders, different column layout.

**Files:**
- Modify: `tests/integration_test.rs`

- [ ] **Step 1: Update test assertions**

The content checks (e.g. `contains("Bash")`, `contains("sess-001")`) should still work since the data is the same. The format-specific assertions need updating. Also add tests for the new flags.

Update `tests/integration_test.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
use tempfile::TempDir;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

struct TestEnv {
    projects: TempDir,
    cache: TempDir,
}

fn setup_env(fixtures: &[&str]) -> TestEnv {
    let projects = TempDir::new().unwrap();
    let project_dir = projects.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_dir).unwrap();
    for fixture in fixtures {
        let src = fixture_path(fixture);
        let dest = project_dir.join(fixture);
        std::fs::copy(&src, &dest).unwrap();
    }
    let cache = TempDir::new().unwrap();
    TestEnv { projects, cache }
}

fn cq_cmd(env: &TestEnv) -> Command {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.env("CQ_PROJECTS_DIR", env.projects.path());
    cmd.env("CQ_CACHE_DIR", env.cache.path());
    // Force no color in tests for predictable output
    cmd.env("NO_COLOR", "1");
    cmd
}

#[test]
fn help_shows_commands() {
    Command::cargo_bin("cq").unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("sessions"))
        .stdout(predicate::str::contains("tools"))
        .stdout(predicate::str::contains("sql"));
}

#[test]
fn schema_shows_views() {
    Command::cargo_bin("cq").unwrap()
        .arg("schema")
        .assert()
        .success()
        .stdout(predicate::str::contains("tool_calls"))
        .stdout(predicate::str::contains("messages"));
}

#[test]
fn schema_examples_shows_sql() {
    Command::cargo_bin("cq").unwrap()
        .args(["schema", "--examples"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SELECT"));
}

#[test]
fn tools_summary_bar_chart() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .arg("tools")
        .assert()
        .success()
        .stdout(predicate::str::contains("Bash"))
        .stdout(predicate::str::contains("█")); // bar chart chars
}

#[test]
fn tools_summary_table() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .args(["--table", "tools"])
        .assert()
        .success()
        .stdout(predicate::str::contains("name"))
        .stdout(predicate::str::contains("count"))
        .stdout(predicate::str::contains("─")); // header separator
}

#[test]
fn sessions_list() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .arg("sessions")
        .assert()
        .success()
        .stdout(predicate::str::contains("sess-001"));
}

#[test]
fn sessions_table() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["--table", "sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("session_id"))
        .stdout(predicate::str::contains("─"));
}

#[test]
fn sql_raw_query() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["sql", "SELECT count(*) AS n FROM tool_calls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("n"))
        .stdout(predicate::str::contains("─"));
}

#[test]
fn json_output() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["--json", "tools"])
        .assert()
        .success()
        .stdout(predicate::str::contains("["));
}

#[test]
fn no_files_no_error() {
    let projects = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let env = TestEnv { projects, cache };
    let output = cq_cmd(&env).arg("tools").output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Cache up to date"),
        "Expected 'Cache up to date' on stderr, got: {stderr}"
    );
}

#[test]
fn tools_filter_by_name() {
    let env = setup_env(&["multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .args(["tools", "Skill"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sanitation"));
}

#[test]
fn progress_on_stderr_not_stdout() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .arg("sessions")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Indexed") || stderr.contains("Cache up to date"),
        "Expected progress on stderr, got: {stderr}"
    );
    assert!(!stdout.contains("Indexed"), "Progress message leaked to stdout: {stdout}");
    assert!(!stdout.contains("Cache up to date"), "Progress message leaked to stdout: {stdout}");
}

#[test]
fn messages_command() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .arg("messages")
        .assert()
        .success()
        .stdout(predicate::str::contains("list the files"));
}

#[test]
fn project_filter() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["--project", "myproject", "sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sess-001"));
}

#[test]
fn no_color_flag() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--no-color", "tools"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should not contain ANSI escape codes
    assert!(!stdout.contains("\x1b["), "Found ANSI codes in --no-color output: {stdout}");
}
```

- [ ] **Step 2: Run all tests**

Run: `cargo test`

Expected: All tests pass.

- [ ] **Step 3: Commit**

```
git add tests/integration_test.rs
git commit -m "test: update integration tests for new output format"
```

---

## Task 8: Clean up and final verification

Remove any leftover comfy-table references. Verify everything works end to end.

**Files:**
- Check: all `src/` files for `comfy_table` references
- Check: `Cargo.lock` updated

- [ ] **Step 1: Search for stale comfy-table references**

Run: `grep -r "comfy" src/ Cargo.toml`

Expected: No matches.

- [ ] **Step 2: Run full test suite**

Run: `cargo test`

Expected: All tests pass (unit + integration + views).

- [ ] **Step 3: Manual smoke test all commands**

Run each of these and verify the output looks correct:

```bash
cargo run -- sessions --limit 5
cargo run -- --table sessions --limit 5
cargo run -- --json sessions --limit 3
cargo run -- tools
cargo run -- --table tools
cargo run -- tools Bash --limit 5
cargo run -- messages --limit 5
cargo run -- --table messages --limit 5
cargo run -- sql "SELECT count(*) FROM messages"
cargo run -- --no-color tools
cargo run -- schema
```

- [ ] **Step 4: Commit any final cleanup**

If anything needed fixing, stage specific files and commit:

```
git add <changed-files>
git commit -m "chore: final cleanup after output format redesign"
```
