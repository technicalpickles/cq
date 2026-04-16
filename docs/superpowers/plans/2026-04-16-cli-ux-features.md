# cq CLI UX Features Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add --wide (TTY-aware truncation), --fields on messages/sessions, --count-by aggregation, session --timeline, and align all existing error messages and help text to a consistent UX standard.

**Architecture:** Each feature threads through the existing command pattern: main.rs parses args, commands build SQL + params, output renders. The --wide flag adds a boolean to the render pipeline. --fields and --count-by add query modes to existing commands. Timeline is a new query path in sessions.rs. A shared validation module handles field/column validation with consistent error messages.

**Tech Stack:** Rust, clap 4 (derive), DuckDB, std::io::IsTerminal

**Spec:** `docs/superpowers/specs/2026-04-16-cli-ux-features-design.md`
**UX conventions:** `docs/cli-ux-conventions.md`

---

## File Structure

| File | Responsibility | Tasks |
|------|---------------|-------|
| `src/main.rs` | CLI arg definitions, dispatch | 1, 2, 3, 4, 5, 6 |
| `src/output.rs` | Shared rendering with width param | 2 |
| `src/style.rs` | Truncation helper | 2 |
| `src/scope.rs` | Error message alignment | 1 |
| `src/commands/mod.rs` | Shared validation helpers | 3 |
| `src/commands/schema.rs` | Error message alignment | 1 |
| `src/commands/tools.rs` | --count-by, --wide threading | 2, 4 |
| `src/commands/messages.rs` | --fields, --count-by, --wide | 2, 3, 4 |
| `src/commands/sessions.rs` | --fields, --count-by, --wide, --timeline | 2, 3, 4, 5 |
| `tests/integration_test.rs` | CLI integration tests | 1, 2, 3, 4, 5 |

---

### Task 1: Help text and error message alignment (Feature 0)

**Files:**
- Modify: `src/main.rs:11-110` (CLI arg help strings)
- Modify: `src/scope.rs:6-24` (session ID validation error)
- Modify: `src/scope.rs:47-63` (since parsing errors)
- Modify: `src/commands/schema.rs:12-25` (unknown view error)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write tests for improved error messages**

Add to `tests/integration_test.rs`:

```rust
#[test]
fn since_invalid_shows_format_hint() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--since", "bogus", "sessions"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Invalid duration 'bogus'"),
        "Should show full invalid input, got: {stderr}"
    );
    assert!(
        stderr.contains("7d, 24h, 30m"),
        "Should show format examples, got: {stderr}"
    );
}

#[test]
fn since_bad_unit_shows_valid_units() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--since", "7x", "sessions"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("'x'") && stderr.contains("'7x'"),
        "Should show bad unit and full input, got: {stderr}"
    );
    assert!(
        stderr.contains("d (days)"),
        "Should show valid units with descriptions, got: {stderr}"
    );
}

#[test]
fn session_invalid_shows_hint() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--session", "not-a-uuid", "sessions"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cq sessions"),
        "Should hint to run 'cq sessions', got: {stderr}"
    );
}

#[test]
fn schema_unknown_view_error_format() {
    Command::cargo_bin("cq").unwrap()
        .args(["schema", "bogus"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown view 'bogus'"))
        .stderr(predicate::str::contains("Valid views:"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test integration_test since_invalid_shows_format_hint since_bad_unit_shows_valid_units session_invalid_shows_hint schema_unknown_view_error_format 2>&1 | tail -20`
Expected: 4 failures

- [ ] **Step 3: Fix scope.rs error messages**

In `src/scope.rs`, replace the `since_timestamp` method's error handling:

```rust
    pub fn since_timestamp(&self) -> Result<Option<DateTime<Utc>>> {
        let since = match &self.since {
            Some(s) => s,
            None => return Ok(None),
        };

        let len = since.len();
        if len < 2 {
            return Err(anyhow!(
                "Invalid duration '{}'\nExpected format: <number><unit> (e.g. 7d, 24h, 30m)",
                since
            ));
        }

        let (num_str, unit) = since.split_at(len - 1);
        let _num: i64 = num_str.parse().map_err(|_| {
            anyhow!(
                "Invalid duration '{}'\nExpected format: <number><unit> (e.g. 7d, 24h, 30m)",
                since
            )
        })?;

        let duration = match unit {
            "d" => Duration::days(_num),
            "h" => Duration::hours(_num),
            "m" => Duration::minutes(_num),
            _ => {
                return Err(anyhow!(
                    "Unknown duration unit '{}' in '{}'\nValid units: d (days), h (hours), m (minutes)",
                    unit, since
                ))
            }
        };

        Ok(Some(Utc::now() - duration))
    }
```

In `src/scope.rs`, update `validate_session_id` to add a hint:

```rust
pub fn validate_session_id(id: &str) -> Result<()> {
    let parts: Vec<&str> = id.split('-').collect();
    let valid = parts.len() == 5
        && parts[0].len() == 8
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() == 4
        && parts[4].len() == 12
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit()));

    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "'{}' is not a valid session ID\nExpected UUID format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx\nHint: Run 'cq sessions' to find session IDs",
            id
        ))
    }
}
```

- [ ] **Step 4: Fix schema.rs error message**

In `src/commands/schema.rs`, update `print_view` to use process::exit(1) and the error template:

```rust
fn print_view(name: &str) {
    let section = match name {
        "messages" => Some(MESSAGES_SCHEMA),
        "tool_calls" => Some(TOOL_CALLS_SCHEMA),
        "tool_results" => Some(TOOL_RESULTS_SCHEMA),
        "sessions" => Some(SESSIONS_SCHEMA),
        _ => None,
    };
    match section {
        Some(s) => println!("{}", s),
        None => {
            eprintln!(
                "Error: Unknown view '{}'\nValid views: messages, tool_calls, tool_results, sessions",
                name
            );
            std::process::exit(1);
        }
    }
}
```

- [ ] **Step 5: Update help strings in main.rs**

In `src/main.rs`, update existing arg help strings:

```rust
    /// Scope to a project (substring match, e.g. 'myproject')
    #[arg(short = 'p', long, global = true)]
    project: Option<String>,

    /// Scope to a session by UUID (prefix match supported)
    #[arg(short = 's', long, global = true)]
    session: Option<String>,
```

In the `Tools` command:

```rust
    /// Filter to a specific tool name (run 'cq tools' to see available names)
    name: Option<String>,

    /// Extract specific input fields as columns (comma-separated; fields depend on the tool, see 'cq schema tool_calls')
    #[arg(long, value_delimiter = ',')]
    fields: Option<Vec<String>>,
```

In the `Messages` command:

```rust
    /// Filter by message type [valid: user, assistant]
    #[arg(long = "type", name = "type")]
    msg_type: Option<String>,
```

In the `Schema` command:

```rust
    /// Show documentation for a specific view [valid: messages, tool_calls, tool_results, sessions]
    name: Option<String>,
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test integration_test since_invalid_shows_format_hint since_bad_unit_shows_valid_units session_invalid_shows_hint schema_unknown_view_error_format 2>&1 | tail -20`
Expected: 4 passing

- [ ] **Step 7: Run full test suite to check for regressions**

Run: `cargo test 2>&1 | tail -20`
Expected: All tests pass. The existing `session_invalid_format_errors` test checks for "not a valid session ID" which is still present in the new message.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs src/scope.rs src/commands/schema.rs tests/integration_test.rs
git commit -m "feat(ux): align help text and error messages to consistent template

Improve discoverability by listing valid values in --help for constrained
flags (--type, schema [NAME], tools [NAME], tools --fields). Align error
messages for --since, --session, and schema to the standard template:
what went wrong, what's valid, how to fix."
```

---

### Task 2: --wide flag and TTY-aware truncation (Feature 1)

**Files:**
- Modify: `src/main.rs` (add --wide flag, compute effective wide, pass to commands)
- Modify: `src/output.rs:44-65` (value_to_string accepts width)
- Modify: `src/commands/tools.rs:44-54,403-453,456-497` (thread wide to run + render functions)
- Modify: `src/commands/messages.rs:24-32,125-183` (thread wide to run + render functions)
- Modify: `src/commands/sessions.rs:56-63,153-223` (thread wide to run + render functions)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write tests for --wide behavior**

Add to `tests/integration_test.rs`:

```rust
#[test]
fn wide_flag_shows_full_values() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--wide", "tools", "Bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // With --wide, the full JSON input should not be truncated with "..."
    // The simple_session fixture has short inputs, so check the command is fully visible
    assert!(
        stdout.contains("\"command\":\"ls\""),
        "Expected full input in --wide mode, got: {stdout}"
    );
}

#[test]
fn default_truncates_long_values() {
    let env = setup_env(&["multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "Bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Default mode should truncate (the fixture has inputs that fit in 60 chars,
    // but verify the truncation mechanism is active by checking value_to_string path)
    // This is more of a smoke test; the unit test in style.rs covers truncation logic
    assert!(!stdout.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test integration_test wide_flag 2>&1 | tail -10`
Expected: FAIL (--wide flag not recognized)

- [ ] **Step 3: Add --wide flag to CLI args in main.rs**

Add to the `Cli` struct:

```rust
    /// Show full column values without truncation
    #[arg(long, global = true)]
    wide: bool,
```

Compute effective wide after parsing, before dispatch:

```rust
    use std::io::IsTerminal;
    let wide = cli.wide || !std::io::stdout().is_terminal();
```

- [ ] **Step 4: Update output::value_to_string to accept max width**

In `src/output.rs`, change `value_to_string` signature and body:

```rust
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
```

Update `print_light_table_output` to pass `max_width` (use 120 for default, 0 for wide):

```rust
fn print_light_table_output(column_names: &[String], rows: &[Vec<Value>], max_width: usize) -> Result<()> {
    let headers: Vec<&str> = column_names.iter().map(|s| s.as_str()).collect();
    let string_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|v| value_to_string(v, max_width)).collect())
        .collect();
    style::print_light_table(&headers, &string_rows);
    Ok(())
}
```

Update `print_results` to accept `wide: bool` and pass through:

```rust
pub fn print_results(
    stmt: &mut duckdb::Statement,
    params: &[&dyn duckdb::types::ToSql],
    format: &OutputFormat,
    wide: bool,
) -> Result<()> {
```

And in the match:

```rust
    match format {
        OutputFormat::Json => print_json(&column_names, &rows),
        _ => {
            let max_width = if wide { 0 } else { 120 };
            print_light_table_output(&column_names, &rows, max_width)
        }
    }
```

- [ ] **Step 5: Thread wide through all command run() functions**

Update each command's `run()` function signature to accept `wide: bool`, and pass it to render functions and `output::print_results` calls.

**tools.rs** `run()`: Add `wide: bool` parameter. Pass to `output::print_results(..., wide)` calls (lines ~123, ~148). Pass to `render_detail_oneline`, `render_fields_oneline`, etc. In render functions, change `style::truncate(&r.input, 60)` to `if wide { r.input.clone() } else { style::truncate(&r.input, 60) }`. Same for `style::truncate(val, 80)` in fields rendering.

Also update `run_with_fields` and `run_summary` to accept and pass `wide`.

**messages.rs** `run()`: Add `wide: bool` parameter. Pass to `output::print_results(..., wide)`. In `render_oneline`, change `style::truncate(&r.text, 60)` to `if wide { r.text.clone() } else { style::truncate(&r.text, 60) }`.

**sessions.rs** `run()`: Add `wide: bool` parameter. Pass to `output::print_results(..., wide)`. In `render_oneline`, change `style::truncate(&r.first_user_message, 60)` to `if wide { r.first_user_message.clone() } else { style::truncate(&r.first_user_message, 60) }`.

**projects.rs**: Also needs `wide` threaded through for consistency. Check its render functions for truncation calls.

- [ ] **Step 6: Update main.rs dispatch to pass wide**

```rust
    match cli.command {
        Command::Sessions { grep } => {
            sessions::run(&conn, &scope, grep.as_deref(), &format, cli.limit, cli.offset, wide)?;
        }
        Command::Tools { name, grep, errors, fields } => {
            let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
            tools::run(&conn, &scope, name.as_deref(), grep.as_deref(), errors, field_refs.as_deref(), &format, cli.limit, cli.offset, wide)?;
        }
        Command::Messages { msg_type, grep } => {
            messages::run(&conn, &scope, msg_type.as_deref(), grep.as_deref(), &format, cli.limit, cli.offset, wide)?;
        }
        Command::Projects { skills } => {
            projects::run(&conn, &scope, skills, &format, cli.limit, cli.offset, wide)?;
        }
        Command::Sql { query } => {
            sql::run(&conn, &query, &format, wide)?;
        }
        Command::Schema { .. } => unreachable!(),
    }
```

- [ ] **Step 7: Update sql.rs to accept wide**

`sql::run` calls `output::print_results`. Update it to accept and pass `wide: bool`.

- [ ] **Step 8: Run tests**

Run: `cargo test 2>&1 | tail -20`
Expected: All tests pass (including new --wide test and all existing tests).

- [ ] **Step 9: Commit**

```bash
git add src/main.rs src/output.rs src/commands/tools.rs src/commands/messages.rs src/commands/sessions.rs src/commands/projects.rs src/commands/sql.rs tests/integration_test.rs
git commit -m "feat: add --wide flag and TTY-aware truncation

When stdout is not a terminal (piped), output full column values
automatically. --wide flag forces full output in terminal mode.
Truncation widths are now parameterized through the render pipeline."
```

---

### Task 3: --fields on messages and sessions (Feature 2)

**Files:**
- Modify: `src/main.rs` (add --fields to Messages and Sessions commands)
- Modify: `src/commands/mod.rs` (add shared field validation helper)
- Modify: `src/commands/messages.rs` (add field validation + run_with_fields)
- Modify: `src/commands/sessions.rs` (add field validation + run_with_fields)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write tests for --fields on messages**

Add to `tests/integration_test.rs`:

```rust
#[test]
fn messages_fields_text() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "--fields", "text"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("list the files"), "Should show message text, got: {stdout}");
    // Should not show session_id or type columns in default mode
    assert!(!stdout.contains("sess-001"), "Should not show session_id when --fields text, got: {stdout}");
}

#[test]
fn messages_fields_invalid() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "--fields", "bogus"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown field 'bogus'"),
        "Should show error with invalid field name, got: {stderr}"
    );
    assert!(
        stderr.contains("Valid fields:"),
        "Should list valid fields, got: {stderr}"
    );
    assert!(
        stderr.contains("cq schema messages"),
        "Should hint to cq schema, got: {stderr}"
    );
}

#[test]
fn messages_fields_json() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "messages", "--fields", "text,type"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert!(!parsed.is_empty());
    assert!(parsed[0].get("text").is_some(), "JSON should have 'text' field");
    assert!(parsed[0].get("type").is_some(), "JSON should have 'type' field");
}

#[test]
fn messages_fields_session_alias() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "--fields", "session,text"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sess-001"), "Should resolve 'session' alias to session_id, got: {stdout}");
}
```

- [ ] **Step 2: Write tests for --fields on sessions**

Add to `tests/integration_test.rs`:

```rust
#[test]
fn sessions_fields_session_id() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["sessions", "--fields", "session_id"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sess-001"), "Should show session_id, got: {stdout}");
    // Should not show other columns like project or first_user_message
    assert!(!stdout.contains("list the files"), "Should not show first_user_message, got: {stdout}");
}

#[test]
fn sessions_fields_invalid() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["sessions", "--fields", "bogus"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown field 'bogus'"),
        "Should show error with invalid field name, got: {stderr}"
    );
    assert!(
        stderr.contains("cq schema sessions"),
        "Should hint to cq schema sessions, got: {stderr}"
    );
}

#[test]
fn sessions_fields_multiple() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["sessions", "--fields", "session_id,first_user_message"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sess-001"), "Should show session_id, got: {stdout}");
    assert!(stdout.contains("list the files"), "Should show first_user_message, got: {stdout}");
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test integration_test messages_fields sessions_fields 2>&1 | tail -20`
Expected: All fail (--fields not recognized on messages/sessions)

- [ ] **Step 4: Add shared field validation to commands/mod.rs**

In `src/commands/mod.rs`, add:

```rust
/// Validate field names against a list of valid fields.
/// Resolves aliases (e.g. "session" -> "session_id").
/// Returns resolved field names or prints an error and exits.
pub fn validate_fields(fields: &[&str], valid_fields: &[&str], command_name: &str) -> Vec<String> {
    let aliases: std::collections::HashMap<&str, &str> = [
        ("session", "session_id"),
    ].into_iter().collect();

    let mut resolved = Vec::new();
    for field in fields {
        let canonical = aliases.get(field).copied().unwrap_or(field);
        if valid_fields.contains(&canonical) {
            resolved.push(canonical.to_string());
        } else {
            eprintln!(
                "Error: Unknown field '{}' for {}\nValid fields: {}\nHint: Run 'cq schema {}' for field descriptions",
                field,
                command_name,
                valid_fields.join(", "),
                command_name,
            );
            std::process::exit(1);
        }
    }
    resolved
}
```

- [ ] **Step 5: Add --fields to Messages command in main.rs**

In the `Messages` enum variant:

```rust
    Messages {
        /// Filter by message type [valid: user, assistant]
        #[arg(long = "type", name = "type")]
        msg_type: Option<String>,

        /// Filter messages by content
        #[arg(long)]
        grep: Option<String>,

        /// Extract specific columns (comma-separated) [valid: session_id, project, type, timestamp, text, model, tool_count]
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
    },
```

Update the dispatch:

```rust
        Command::Messages { msg_type, grep, fields } => {
            let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
            messages::run(&conn, &scope, msg_type.as_deref(), grep.as_deref(), field_refs.as_deref(), &format, cli.limit, cli.offset, wide)?;
        }
```

- [ ] **Step 6: Implement --fields in messages.rs**

Update `run()` signature to accept `fields: Option<&[&str]>`. At the top of `run()`, validate fields and branch:

```rust
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    msg_type: Option<&str>,
    grep: Option<&str>,
    fields: Option<&[&str]>,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    const VALID_FIELDS: &[&str] = &[
        "session_id", "project", "type", "timestamp", "text", "model", "tool_count",
    ];

    if let Some(field_list) = fields {
        let resolved = super::validate_fields(field_list, VALID_FIELDS, "messages");
        return run_with_fields(conn, scope, msg_type, grep, &resolved, format, limit, offset, wide);
    }
    // ... existing code
```

Add `run_with_fields`:

```rust
fn run_with_fields(
    conn: &Connection,
    scope: &QueryScope,
    msg_type: Option<&str>,
    grep: Option<&str>,
    fields: &[String],
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

    let select_cols = fields.join(", ");
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
```

- [ ] **Step 7: Add --fields to Sessions command in main.rs**

In the `Sessions` enum variant:

```rust
    Sessions {
        /// Filter sessions by content
        #[arg(long)]
        grep: Option<String>,

        /// Extract specific columns (comma-separated) [valid: session_id, project, started_at, ended_at, message_count, tool_call_count, user_message_count, first_user_message]
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
    },
```

Update the dispatch:

```rust
        Command::Sessions { grep, fields } => {
            let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
            sessions::run(&conn, &scope, grep.as_deref(), field_refs.as_deref(), &format, cli.limit, cli.offset, wide)?;
        }
```

- [ ] **Step 8: Implement --fields in sessions.rs**

Same pattern as messages.rs. Update `run()` signature, validate, branch:

```rust
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    grep: Option<&str>,
    fields: Option<&[&str]>,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    const VALID_FIELDS: &[&str] = &[
        "session_id", "project", "started_at", "ended_at",
        "message_count", "tool_call_count", "user_message_count", "first_user_message",
    ];

    if let Some(field_list) = fields {
        let resolved = super::validate_fields(field_list, VALID_FIELDS, "sessions");
        return run_with_fields(conn, scope, grep, &resolved, format, limit, offset, wide);
    }
    // ... existing code
```

Add `run_with_fields` (same structure as messages, querying from `sessions` view):

```rust
fn run_with_fields(
    conn: &Connection,
    scope: &QueryScope,
    grep: Option<&str>,
    fields: &[String],
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
        conditions.push(format!("started_at >= '{formatted}'"));
    }
    if let Some(pattern) = grep {
        conditions.push("first_user_message ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }

    let where_clause = conditions.join(" AND ");
    let limit_clause = super::limit_clause(limit);
    let offset_clause = super::offset_clause(offset);

    let select_cols = fields.join(", ");
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
```

- [ ] **Step 9: Run tests**

Run: `cargo test 2>&1 | tail -20`
Expected: All tests pass.

- [ ] **Step 10: Commit**

```bash
git add src/main.rs src/commands/mod.rs src/commands/messages.rs src/commands/sessions.rs tests/integration_test.rs
git commit -m "feat: add --fields to messages and sessions commands

Column selection for flat-column commands. --fields text extracts just
the text column, --fields session_id enables piping to other commands.
Shared validation in commands/mod.rs with consistent error messages and
session alias support."
```

---

### Task 4: --count-by aggregation (Feature 3)

**Files:**
- Modify: `src/main.rs` (add --count-by to Tools, Messages, Sessions)
- Modify: `src/commands/mod.rs` (add validate_count_by, shared bar chart rendering)
- Modify: `src/commands/tools.rs` (add count-by path)
- Modify: `src/commands/messages.rs` (add count-by path)
- Modify: `src/commands/sessions.rs` (add count-by path)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write tests for --count-by**

Add to `tests/integration_test.rs`:

```rust
#[test]
fn tools_count_by_name() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .args(["tools", "--count-by", "name"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Bash"))
        .stdout(predicate::str::contains("\u{2588}")); // bar chart
}

#[test]
fn tools_count_by_session() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .args(["tools", "--count-by", "session"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sess-001"))
        .stdout(predicate::str::contains("\u{2588}"));
}

#[test]
fn tools_count_by_invalid() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "--count-by", "bogus"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Unknown count-by column 'bogus'"), "got: {stderr}");
    assert!(stderr.contains("Valid columns:"), "got: {stderr}");
}

#[test]
fn tools_count_by_with_filter() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl", "error_session.jsonl"]);
    cq_cmd(&env)
        .args(["tools", "--errors", "--count-by", "session"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sess-003")); // error_session has the error
}

#[test]
fn messages_count_by_type() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["messages", "--count-by", "type"])
        .assert()
        .success()
        .stdout(predicate::str::contains("user"))
        .stdout(predicate::str::contains("assistant"));
}

#[test]
fn count_by_json_output() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "tools", "--count-by", "name"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert!(!parsed.is_empty());
    assert!(parsed[0].get("name").is_some());
    assert!(parsed[0].get("count").is_some());
}

#[test]
fn count_by_and_fields_conflict() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "--count-by", "name", "--fields", "command"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot be used together"), "got: {stderr}");
}

#[test]
fn sessions_count_by_project() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .args(["sessions", "--count-by", "project"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\u{2588}"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test integration_test count_by 2>&1 | tail -10`
Expected: All fail

- [ ] **Step 3: Add shared count-by validation and bar chart rendering to commands/mod.rs**

```rust
/// Validate a --count-by column name. Returns the SQL column expression.
/// Resolves aliases (e.g. "session" -> "session_id").
pub fn validate_count_by(column: &str, valid_columns: &[&str], command_name: &str) -> String {
    let aliases: std::collections::HashMap<&str, &str> = [
        ("session", "session_id"),
    ].into_iter().collect();

    let canonical = aliases.get(column).copied().unwrap_or(column);
    if valid_columns.contains(&canonical) {
        canonical.to_string()
    } else {
        // Show the friendly names in the error (not the SQL names)
        let display_names: Vec<&str> = valid_columns.iter().map(|c| {
            // Reverse lookup: if canonical is "session_id", show "session"
            if *c == "session_id" { "session" } else { c }
        }).collect();
        eprintln!(
            "Error: Unknown count-by column '{}' for {}\nValid columns: {}",
            column, command_name, display_names.join(", ")
        );
        std::process::exit(1);
    }
}

/// Check that --count-by and --fields are not both specified.
pub fn check_count_by_fields_conflict(count_by: Option<&str>, fields: Option<&[&str]>) {
    if count_by.is_some() && fields.is_some() {
        eprintln!(
            "Error: --count-by and --fields cannot be used together\n--count-by aggregates rows into counts; --fields selects columns from detail rows"
        );
        std::process::exit(1);
    }
}

/// Render a bar chart for count-by results. Shared across commands.
pub fn render_bar_chart(rows: &[(String, i64)]) {
    if rows.is_empty() {
        eprintln!("No results.");
        return;
    }
    let max_count = rows.iter().map(|r| r.1).max().unwrap_or(1);
    let name_width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let count_width = rows.iter().map(|r| r.1.to_string().len()).max().unwrap_or(0);

    for (name, count) in rows {
        let name_padded = style::pad_right(name, name_width);
        let bar_str = style::bar(*count, max_count, 30);
        let count_str = count.to_string();
        let count_padded = style::pad_left(&count_str, count_width);

        println!(
            "{}  {}  {}",
            style::color(&name_padded, style::Color::Primary),
            style::color(&bar_str, style::Color::Bar),
            style::color(&count_padded, style::Color::Dim),
        );
    }
}
```

- [ ] **Step 4: Add --count-by to CLI args in main.rs**

Add to `Tools`:

```rust
        /// Aggregate rows into counts by column [valid: name, session, project]
        #[arg(long = "count-by")]
        count_by: Option<String>,
```

Add to `Messages`:

```rust
        /// Aggregate rows into counts by column [valid: type, session, project]
        #[arg(long = "count-by")]
        count_by: Option<String>,
```

Add to `Sessions`:

```rust
        /// Aggregate rows into counts by column [valid: project]
        #[arg(long = "count-by")]
        count_by: Option<String>,
```

Update all dispatch arms to pass `count_by.as_deref()`.

- [ ] **Step 5: Implement --count-by in tools.rs**

Update `run()` to accept `count_by: Option<&str>`. At the top, check conflict and dispatch:

```rust
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    tool_name: Option<&str>,
    grep: Option<&str>,
    errors_only: bool,
    fields: Option<&[&str]>,
    count_by: Option<&str>,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    super::check_count_by_fields_conflict(count_by, fields);

    if let Some(col) = count_by {
        let sql_col = super::validate_count_by(col, &["name", "session_id", "project"], "tools");
        return run_count_by(conn, scope, tool_name, grep, errors_only, &sql_col, format);
    }
    // ... existing code
```

Add `run_count_by`:

```rust
fn run_count_by(
    conn: &Connection,
    scope: &QueryScope,
    tool_name: Option<&str>,
    grep: Option<&str>,
    errors_only: bool,
    sql_col: &str,
    format: &OutputFormat,
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
    let join_clause = if errors_only {
        "JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id"
    } else {
        ""
    };
    let error_filter = if errors_only { "AND tr.is_error = true" } else { "" };

    // Qualify column with tc. prefix
    let qualified_col = format!("tc.{sql_col}");

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    if matches!(format, OutputFormat::Json) {
        let sql = format!(
            "SELECT {qualified_col} AS \"{sql_col}\", COUNT(*) AS count
             FROM tool_calls tc
             {join_clause}
             WHERE {where_clause}
             {error_filter}
             GROUP BY {qualified_col}
             ORDER BY count DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        return output::print_results(&mut stmt, &param_refs, format, false);
    }

    let sql = format!(
        "SELECT {qualified_col}, COUNT(*) AS count
         FROM tool_calls tc
         {join_clause}
         WHERE {where_clause}
         {error_filter}
         GROUP BY {qualified_col}
         ORDER BY count DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(&param_refs[..])?;
    let mut chart_rows: Vec<(String, i64)> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        let name = val_str(&row.get::<_, duckdb::types::Value>(0).unwrap_or(duckdb::types::Value::Null));
        let count = val_i64(&row.get::<_, duckdb::types::Value>(1).unwrap_or(duckdb::types::Value::Null));
        chart_rows.push((name, count));
    }

    match format {
        OutputFormat::Table => {
            let headers = [sql_col, "count"];
            let string_rows: Vec<Vec<String>> = chart_rows.iter().map(|(n, c)| {
                vec![n.clone(), c.to_string()]
            }).collect();
            crate::style::print_light_table(&headers, &string_rows);
        }
        _ => super::render_bar_chart(&chart_rows),
    }

    Ok(())
}
```

- [ ] **Step 6: Implement --count-by in messages.rs**

Same pattern. Update `run()` to accept `count_by: Option<&str>`, check conflict, dispatch. Valid columns: `["type", "session_id", "project"]`.

The `run_count_by` function queries `messages` table with the GROUP BY pattern. No join needed (messages don't have an errors_only filter).

- [ ] **Step 7: Implement --count-by in sessions.rs**

Same pattern. Valid columns: `["project"]` only.

The `run_count_by` function queries `sessions` table with GROUP BY project.

- [ ] **Step 8: Run tests**

Run: `cargo test 2>&1 | tail -20`
Expected: All pass.

- [ ] **Step 9: Commit**

```bash
git add src/main.rs src/commands/mod.rs src/commands/tools.rs src/commands/messages.rs src/commands/sessions.rs tests/integration_test.rs
git commit -m "feat: add --count-by aggregation to tools, messages, and sessions

Switches to aggregation mode with bar chart output. Supports all
existing filters as WHERE conditions. Shared validation and rendering
in commands/mod.rs. Mutually exclusive with --fields."
```

---

### Task 5: Session timeline (Feature 4)

**Files:**
- Modify: `src/main.rs` (add --timeline flag to Sessions)
- Modify: `src/commands/sessions.rs` (add run_timeline)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write tests for --timeline**

Add to `tests/integration_test.rs`:

```rust
#[test]
fn sessions_timeline_shows_events() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["--session", "sess-0010-0000-0000-000000000000", "sessions", "--timeline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("call"))
        .stdout(predicate::str::contains("Bash"))
        .stdout(predicate::str::contains("result"));
}

#[test]
fn sessions_timeline_requires_session() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["sessions", "--timeline"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--timeline requires --session"), "got: {stderr}");
    assert!(stderr.contains("cq sessions"), "Should hint to find sessions, got: {stderr}");
}

#[test]
fn sessions_timeline_shows_errors() {
    let env = setup_env(&["error_session.jsonl"]);
    cq_cmd(&env)
        .args(["--session", "sess-0030-0000-0000-000000000000", "sessions", "--timeline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("error"));
}

#[test]
fn sessions_timeline_json() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "--session", "sess-0010-0000-0000-000000000000", "sessions", "--timeline"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert!(!parsed.is_empty());
    assert!(parsed[0].get("event").is_some());
    assert!(parsed[0].get("name").is_some());
}
```

**Important:** The fixture session IDs are `sess-001`, `sess-002`, `sess-003` which are not valid UUIDs. The session ID validation will reject them. We need to either:
(a) Update fixtures to use valid UUIDs, or
(b) Use `--session` prefix match with the fixtures as-is.

Since `--session` validation requires UUID format, we need to update fixtures. Create fixture session IDs that are valid UUIDs. Update the test session IDs to match. Actually, looking at the existing tests, `session_invalid_format_errors` tests bad format, and the existing passing tests use `sess-001` with `--session` which means... let me re-check.

Actually, the existing tests like `sessions_list` check for `sess-001` in stdout without using `--session` flag. The test `session_not_found_errors` uses a valid UUID `00000000-0000-0000-0000-000000000000`. So the `--session` flag requires valid UUID format, but the fixtures use non-UUID session IDs. The timeline tests need to use the `--session` flag, so we need valid UUID session IDs in the fixtures.

Let me revise: We need a new fixture or updated fixtures with UUID session IDs for timeline testing.

- [ ] **Step 2: Create a timeline test fixture**

Create `tests/fixtures/timeline_session.jsonl` with UUID session IDs:

```jsonl
{"type":"user","message":{"role":"user","content":"build the project"},"uuid":"u1","parentUuid":null,"isSidechain":false,"timestamp":"2026-04-13T14:02:00.000Z","sessionId":"aaaa0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"assistant","message":{"id":"msg_001","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"I'll read the config first."},{"type":"tool_use","id":"toolu_100","name":"Read","input":{"file_path":"/src/main.rs"}}],"stop_reason":"tool_use","usage":{"input_tokens":100,"output_tokens":30}},"uuid":"a1","parentUuid":"u1","isSidechain":false,"timestamp":"2026-04-13T14:02:01.000Z","sessionId":"aaaa0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_100","content":"fn main() { println!(\"hello\"); }"}]},"uuid":"u2","parentUuid":"a1","isSidechain":false,"timestamp":"2026-04-13T14:02:02.000Z","sessionId":"aaaa0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"assistant","message":{"id":"msg_002","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"tool_use","id":"toolu_101","name":"Bash","input":{"command":"cargo build","description":"Build project"}}],"stop_reason":"tool_use","usage":{"input_tokens":200,"output_tokens":40}},"uuid":"a2","parentUuid":"u2","isSidechain":false,"timestamp":"2026-04-13T14:02:03.000Z","sessionId":"aaaa0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_101","content":"error[E0308]: mismatched types","is_error":true}]},"uuid":"u3","parentUuid":"a2","isSidechain":false,"timestamp":"2026-04-13T14:02:10.000Z","sessionId":"aaaa0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
```

- [ ] **Step 3: Update timeline tests to use the new fixture**

```rust
#[test]
fn sessions_timeline_shows_events() {
    let env = setup_env(&["timeline_session.jsonl"]);
    cq_cmd(&env)
        .args(["--session", "aaaa0000-0000-0000-0000-000000000001", "sessions", "--timeline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("call"))
        .stdout(predicate::str::contains("Read"))
        .stdout(predicate::str::contains("Bash"))
        .stdout(predicate::str::contains("result"));
}

#[test]
fn sessions_timeline_requires_session() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["sessions", "--timeline"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--timeline requires --session"), "got: {stderr}");
    assert!(stderr.contains("cq sessions"), "Should hint to find sessions, got: {stderr}");
}

#[test]
fn sessions_timeline_shows_errors() {
    let env = setup_env(&["timeline_session.jsonl"]);
    cq_cmd(&env)
        .args(["--session", "aaaa0000-0000-0000-0000-000000000001", "sessions", "--timeline"])
        .assert()
        .success()
        .stdout(predicate::str::contains("error"));
}

#[test]
fn sessions_timeline_json() {
    let env = setup_env(&["timeline_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "--session", "aaaa0000-0000-0000-0000-000000000001", "sessions", "--timeline"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert!(!parsed.is_empty());
    assert!(parsed[0].get("event").is_some());
    assert!(parsed[0].get("name").is_some());
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test --test integration_test sessions_timeline 2>&1 | tail -10`
Expected: All fail

- [ ] **Step 5: Add --timeline flag to Sessions in main.rs**

```rust
    Sessions {
        /// Filter sessions by content
        #[arg(long)]
        grep: Option<String>,

        /// Extract specific columns (comma-separated) [valid: session_id, project, started_at, ended_at, message_count, tool_call_count, user_message_count, first_user_message]
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,

        /// Aggregate rows into counts by column [valid: project]
        #[arg(long = "count-by")]
        count_by: Option<String>,

        /// Show chronological tool call timeline (requires --session)
        #[arg(long)]
        timeline: bool,
    },
```

Update the dispatch to pass `timeline`:

```rust
        Command::Sessions { grep, fields, count_by, timeline } => {
            let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
            sessions::run(&conn, &scope, grep.as_deref(), field_refs.as_deref(), count_by.as_deref(), timeline, &format, cli.limit, cli.offset, wide)?;
        }
```

- [ ] **Step 6: Implement --timeline in sessions.rs**

Update `run()` to accept `timeline: bool`. Add validation and dispatch at the top:

```rust
    if timeline {
        if scope.session.is_none() {
            eprintln!(
                "Error: --timeline requires --session\nUsage: cq sessions --session <id> --timeline\nHint: Run 'cq sessions' to find session IDs"
            );
            std::process::exit(1);
        }
        return run_timeline(conn, scope, format, limit, offset, wide);
    }
```

Add `run_timeline`:

```rust
struct TimelineRow {
    event: String,
    timestamp: String,
    name: String,
    detail: String,
}

fn run_timeline(
    conn: &Connection,
    scope: &QueryScope,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    let session = scope.session.as_ref().unwrap();
    let limit_clause = super::limit_clause(limit);
    let offset_clause = super::offset_clause(offset);

    let sql = format!(
        "SELECT event, timestamp, name, detail FROM (
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
        ORDER BY timestamp, CASE WHEN event = 'call' THEN 0 ELSE 1 END
        {limit_clause}
        {offset_clause}"
    );

    let params: Vec<Box<dyn duckdb::types::ToSql>> = vec![
        Box::new(session.clone()),
        Box::new(session.clone()),
    ];
    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    if matches!(format, OutputFormat::Json) {
        let mut stmt = conn.prepare(&sql)?;
        return output::print_results(&mut stmt, &param_refs, format, wide);
    }

    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(&param_refs[..])?;
    let mut timeline_rows: Vec<TimelineRow> = Vec::new();
    while let Some(row) = rows_iter.next()? {
        let values: Vec<duckdb::types::Value> = (0..4)
            .map(|i| row.get::<_, duckdb::types::Value>(i).unwrap_or(duckdb::types::Value::Null))
            .collect();
        timeline_rows.push(TimelineRow {
            event: val_str(&values[0]),
            timestamp: val_str(&values[1]),
            name: val_str(&values[2]),
            detail: val_str(&values[3]),
        });
    }

    if timeline_rows.is_empty() {
        super::print_session_not_found(session);
        return Ok(());
    }

    match format {
        OutputFormat::Table => render_timeline_table(&timeline_rows, wide),
        _ => render_timeline_oneline(&timeline_rows, wide),
    }

    Ok(())
}

fn extract_time(timestamp: &str) -> String {
    // Extract HH:MM:SS from ISO timestamp
    if let Some(t_pos) = timestamp.find('T') {
        let time_part = &timestamp[t_pos + 1..];
        if let Some(dot_pos) = time_part.find('.') {
            return time_part[..dot_pos].to_string();
        }
        if let Some(z_pos) = time_part.find('Z') {
            return time_part[..z_pos].to_string();
        }
        return time_part.to_string();
    }
    timestamp.to_string()
}

fn render_timeline_oneline(rows: &[TimelineRow], wide: bool) {
    let plain_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        let time = extract_time(&r.timestamp);
        let detail = if wide {
            r.detail.clone()
        } else {
            style::truncate(&r.detail, 80)
        };
        vec![time, r.event.clone(), r.name.clone(), detail]
    }).collect();

    let ncols = 4;
    let mut widths = vec![0usize; ncols];
    for row in &plain_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    for row in &plain_rows {
        let cols: Vec<String> = row.iter().enumerate().map(|(i, cell)| {
            let padded = if i == ncols - 1 {
                cell.clone()
            } else {
                style::pad_right(cell, widths[i])
            };
            match i {
                0 => style::color(&padded, style::Color::Dim),
                1 => {
                    if cell == "call" {
                        style::color(&padded, style::Color::Primary)
                    } else {
                        style::color(&padded, style::Color::Secondary)
                    }
                }
                2 => style::color(&padded, style::Color::Primary),
                _ => {
                    if padded.starts_with("error") {
                        style::color(&padded, style::Color::Secondary)
                    } else {
                        padded
                    }
                }
            }
        }).collect();
        println!("{}", cols.join("  "));
    }
}

fn render_timeline_table(rows: &[TimelineRow], wide: bool) {
    let headers = ["time", "event", "tool", "detail"];
    let string_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        let detail = if wide {
            r.detail.clone()
        } else {
            style::truncate(&r.detail, 80)
        };
        vec![extract_time(&r.timestamp), r.event.clone(), r.name.clone(), detail]
    }).collect();
    style::print_light_table(&headers, &string_rows);
}
```

- [ ] **Step 7: Run tests**

Run: `cargo test 2>&1 | tail -20`
Expected: All tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs src/commands/sessions.rs tests/fixtures/timeline_session.jsonl tests/integration_test.rs
git commit -m "feat: add session timeline view

cq sessions --session <id> --timeline shows chronological interleaved
tool calls and results with timestamps, tool names, and detail snippets.
Errors are highlighted. Supports --json, --table, and --wide."
```

---

### Task 6: Update beans to in-progress

- [ ] **Step 1: Mark all four beans as in-progress**

```bash
pt beans update gt-u9ff -s in-progress
pt beans update gt-ilzd -s in-progress
pt beans update gt-r4mh -s in-progress
pt beans update gt-m6bl -s in-progress
```

- [ ] **Step 2: Run final full test suite**

```bash
cargo test 2>&1
```

Expected: All tests pass (existing + new).

- [ ] **Step 3: Manual smoke test**

```bash
cq tools --wide
cq tools --count-by session
cq messages --fields text --type user
cq sessions --fields session_id
cq sessions --count-by project
# Pick a real session ID from the output:
cq sessions --session <id> --timeline
```

Verify output looks reasonable for each.
