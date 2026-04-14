# cq UX Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the four highest-impact UX issues in cq: silent 18s startup, encoded project paths, noisy first_user_message, and stderr/stdout separation.

**Architecture:** All changes are localized. Startup feedback goes in `main.rs` and `db.rs`. Project path decoding is a SQL change in `views.rs`. Message noise filtering is a SQL WHERE clause addition in `views.rs`. stderr separation touches `main.rs` and `output.rs`.

**Tech Stack:** Rust, DuckDB SQL, stderr for progress/errors.

**Spec:** @dev-tools:designing-clis (Feedback principle: anything >1s needs progress indication)

---

## File Structure

```
src/
├── main.rs          # Add stderr progress messages around db setup
├── db.rs            # Return file count so main can report it
├── views.rs         # Decode project paths in SQL, filter first_user_message noise
├── output.rs        # Ensure "No results." goes to stderr not stdout
└── commands/
    └── (no changes)
```

---

## Task 1: Startup Feedback

Show a progress message on stderr while DuckDB is loading files. Users currently see nothing for 9-18 seconds.

**Files:**
- Modify: `src/db.rs:11-16`
- Modify: `src/main.rs:100-101`

- [ ] **Step 1: Update db.rs to return file count**

Change `setup_connection` to return the connection and file count so main.rs can report it.

```rust
// src/db.rs
use std::time::Instant;
use anyhow::Result;
use duckdb::Connection;
use crate::provider::TranscriptProvider;
use crate::scope::QueryScope;

pub struct DbSetup {
    pub conn: Connection,
    pub file_count: usize,
}

pub fn setup_connection(provider: &dyn TranscriptProvider, scope: &QueryScope) -> Result<DbSetup> {
    let files = provider.discover_files(scope)?;
    let file_count = files.len();
    let conn = Connection::open_in_memory()?;
    provider.register_views(&conn, &files)?;
    Ok(DbSetup { conn, file_count })
}
```

- [ ] **Step 2: Update main.rs to show progress on stderr**

```rust
// In main(), replace the db setup block:
    let provider = ClaudeProvider::new()?;

    // Show scanning feedback on stderr (single line, no carriage return tricks)
    let start = std::time::Instant::now();
    let db_setup = db::setup_connection(&provider, &scope)?;
    let elapsed = start.elapsed();
    eprintln!("Scanned {} files in {:.1}s", db_setup.file_count, elapsed.as_secs_f64());

    let conn = db_setup.conn;
```

Add `use std::time::Instant;` at the top of main.rs (or just inline `std::time::Instant`).

Update all `&conn` references below to use the new variable name (they already say `conn` so this should just work).

- [ ] **Step 3: Build and test manually**

Run: `cd ~/workspace/cq && cargo build`
Run: `cargo run -- tools --limit 3`
Expected: see "Scanned N files in X.Xs" on stderr, then the table on stdout.

Run: `cargo run -- tools --limit 3 2>/dev/null`
Expected: only the table, no progress messages.

- [ ] **Step 4: Run existing tests**

Run: `cd ~/workspace/cq && cargo test`
Expected: all 34 tests pass (tests don't go through main.rs, so DbSetup change needs the test code updated if any tests use db::setup_connection directly)

Check: grep for `db::setup_connection` in test files. If any exist, update them to use `.conn` field.

- [ ] **Step 5: Commit**

```
git add src/db.rs src/main.rs
git commit -m "feat: add startup progress feedback on stderr"
```

---

## Task 2: Decode Project Paths

The `project` column currently shows `-Users-josh-nichols-pickleton`. Should show the decoded path. This is a SQL-level change in the views.

**Files:**
- Modify: `src/views.rs:69,84` (messages view, both CTEs)
- Modify: `src/views.rs:119` (tool_calls view)
- Modify: `src/views.rs:144` (tool_results view)
- Modify: `tests/views_test.rs` (update expected project values)
- Modify: `src/commands/sessions.rs:17-19` (project filter now matches decoded path)
- Modify: `src/commands/tools.rs:23-25,78-80` (same)

- [ ] **Step 1: Determine the SQL approach**

DuckDB has `replace()` and `regexp_replace()`. The current project extraction is:

```sql
regexp_extract(filename, '.*/([^/]+)/[^/]+$', 1) AS project
```

This gives the encoded directory name (e.g. `-Users-josh-nichols-pickleton`). To decode, we need to:
1. Strip leading `-`
2. Replace remaining `-` with `/`
3. Prepend `/`

This is lossy (same as ClaudeProvider::decode_path), which is fine for display.

SQL expression:

```sql
'/' || replace(regexp_extract(filename, '.*/([^/]+)/[^/]+$', 1)[2:], '-', '/') AS project
```

DuckDB string indexing: `[2:]` skips the first character (the leading `-`). `replace('-', '/')` does the rest.

- [ ] **Step 2: Create a SQL helper function in views.rs**

Add a Rust constant or helper to avoid repeating the expression in every view:

```rust
/// SQL expression to extract and decode the project path from a filename.
/// Input: filename column from read_json (e.g. "/path/to/-Users-josh-pickleton/sess.jsonl")
/// Output: decoded path (e.g. "/Users/josh/pickleton")
const PROJECT_EXPR: &str = "'/' || replace(regexp_extract(filename, '.*/([^/]+)/[^/]+$', 1)[2:], '-', '/')";
```

- [ ] **Step 3: Update all four views to use the decoded project expression**

Replace every `regexp_extract(filename, '.*/([^/]+)/[^/]+$', 1) AS project` with `{PROJECT_EXPR} AS project` using format strings.

This affects: `register_messages_view` (two CTEs), `register_tool_calls_view`, `register_tool_results_view`. The sessions view derives from messages so it gets it for free.

- [ ] **Step 4: Verify existing tests still pass**

No existing test in `views_test.rs` asserts on the `project` column value, so the decode change shouldn't break anything. Run `cargo test` to confirm. If any test does check project, update the expectation to the decoded form.

- [ ] **Step 5: Update project filtering in commands**

`sessions.rs` and `tools.rs` filter with `project = '{escaped}'`. Since project is now a decoded path like `/Users/josh/pickleton`, the `--project pickleton` flag should use ILIKE substring matching instead of exact match:

```rust
// Change from:
conditions.push(format!("project = '{escaped}'"));
// To:
conditions.push(format!("project ILIKE '%{escaped}%'"));
```

Apply this change in: `sessions.rs`, `tools.rs` (both `run` and `run_summary`), and `messages.rs`.

- [ ] **Step 6: Build and test**

Run: `cd ~/workspace/cq && cargo test`
Run: `cargo run -- sessions --limit 3`
Expected: project column shows `/Users/josh/nichols/pickleton` (or similar decoded path)
Run: `cargo run -- sessions --project pickleton --limit 3`
Expected: still correctly filters

- [ ] **Step 7: Commit**

```
git add src/views.rs src/commands/sessions.rs src/commands/tools.rs src/commands/messages.rs tests/views_test.rs
git commit -m "feat: decode project paths for human-readable output"
```

---

## Task 3: Filter Noise from first_user_message

The sessions view picks up `<command-message>`, `<command-name>`, and other XML-like content as the first user message. Filter these out.

**Files:**
- Modify: `src/views.rs:175-178` (sessions view subquery)

- [ ] **Step 1: Update the sessions view subquery**

The current first_user_message selection:

```sql
(SELECT text FROM messages m2
 WHERE m2.session_id = m1.session_id
 AND m2.type = 'user' AND m2.text IS NOT NULL
 ORDER BY m2.timestamp LIMIT 1) AS first_user_message
```

Add a filter to skip noise:

```sql
(SELECT text FROM messages m2
 WHERE m2.session_id = m1.session_id
 AND m2.type = 'user'
 AND m2.text IS NOT NULL
 AND m2.text != ''
 AND m2.text NOT LIKE '<%'
 AND m2.text NOT LIKE 'Base directory for this skill%'
 AND m2.text NOT LIKE '#%'
 ORDER BY m2.timestamp LIMIT 1) AS first_user_message
```

These filters skip:
- XML/HTML content (`<command-message>`, `<system-reminder>`, etc.)
- Skill preamble lines (`Base directory for this skill`)
- Markdown headers that come from skill content (`#`)

- [ ] **Step 2: Build and test**

Run: `cd ~/workspace/cq && cargo test`
Run: `cargo run -- sessions --limit 10`
Expected: `first_user_message` column shows actual human messages, not XML noise.

- [ ] **Step 3: Commit**

```
git add src/views.rs
git commit -m "fix: filter XML noise from first_user_message in sessions view"
```

---

## Task 4: stderr/stdout Separation

Progress and errors should go to stderr. Data output goes to stdout. This lets `cq tools --json | jq` work without progress messages polluting the JSON.

**Files:**
- Modify: `src/output.rs:79-80` ("No results." should go to stderr)
- Modify: `src/main.rs` (error output)

- [ ] **Step 1: Move "No results." to stderr**

In `src/output.rs`, change the empty-results case in `print_table`:

```rust
// Change from:
println!("No results.");
// To:
eprintln!("No results.");
```

Leave the JSON empty-array case (`println!("[]")`) on stdout since it's valid machine-readable output.

- [ ] **Step 2: Verify error output goes to stderr**

`anyhow` errors from `main() -> Result<()>` already print to stderr by default. Verify by running:

```bash
cargo run -- sql "INVALID SQL" 2>/dev/null
```

Expected: no output (error went to stderr which is suppressed).

```bash
cargo run -- sql "INVALID SQL" 2>&1 | head -1
```

Expected: error message visible.

- [ ] **Step 3: Build and test**

Run: `cd ~/workspace/cq && cargo test`
Run: `cargo run -- --json tools --limit 3 2>/dev/null`
Expected: clean JSON output with no progress messages mixed in.

- [ ] **Step 4: Commit**

```
git add src/output.rs
git commit -m "fix: separate progress/errors (stderr) from data output (stdout)"
```

---

## Task 5: Integration Tests

End-to-end tests using `assert_cmd` to run the binary against fixtures.

**Files:**
- Create: `tests/integration_test.rs`
- Modify: `src/claude_provider.rs` (verify CQ_PROJECTS_DIR env override exists)

- [ ] **Step 1: Verify CQ_PROJECTS_DIR override**

Read `src/claude_provider.rs` and confirm the `new()` method checks `CQ_PROJECTS_DIR` env var. If not present, add it (it was in the plan for Task 3 but verify it landed).

- [ ] **Step 2: Create integration test helpers**

```rust
// tests/integration_test.rs
use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
use tempfile::TempDir;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Set up a fake projects directory with a project containing fixture files
fn setup_fake_projects(fixtures: &[&str]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_dir).unwrap();

    for fixture in fixtures {
        let src = fixture_path(fixture);
        let dest = project_dir.join(fixture.replace("_session", "").replace("_types", ""));
        std::fs::copy(&src, &dest).unwrap();
    }

    tmp
}

fn cq_cmd(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.env("CQ_PROJECTS_DIR", tmp.path());
    cmd
}
```

- [ ] **Step 3: Write tests**

```rust
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
fn tools_summary() {
    let tmp = setup_fake_projects(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&tmp)
        .arg("tools")
        .assert()
        .success()
        .stdout(predicate::str::contains("Bash"));
}

#[test]
fn tools_filter_by_name() {
    let tmp = setup_fake_projects(&["multi_tool_session.jsonl"]);
    cq_cmd(&tmp)
        .args(["tools", "Skill"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sanitation"));
}

#[test]
fn sessions_list() {
    let tmp = setup_fake_projects(&["simple_session.jsonl"]);
    cq_cmd(&tmp)
        .arg("sessions")
        .assert()
        .success()
        .stdout(predicate::str::contains("sess-001"));
}

#[test]
fn sql_raw_query() {
    let tmp = setup_fake_projects(&["simple_session.jsonl"]);
    cq_cmd(&tmp)
        .args(["sql", "SELECT count(*) AS n FROM tool_calls"])
        .assert()
        .success();
}

#[test]
fn json_output() {
    let tmp = setup_fake_projects(&["simple_session.jsonl"]);
    cq_cmd(&tmp)
        .args(["--json", "tools"])
        .assert()
        .success()
        .stdout(predicate::str::contains("["));
}

#[test]
fn no_files_no_error() {
    let tmp = TempDir::new().unwrap();
    cq_cmd(&tmp)
        .arg("tools")
        .assert()
        .success();
}

#[test]
fn progress_on_stderr_not_stdout() {
    let tmp = setup_fake_projects(&["simple_session.jsonl"]);
    let output = cq_cmd(&tmp)
        .arg("sessions")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Progress messages go to stderr
    assert!(stderr.contains("Scanned") || stderr.contains("Scanning"));
    // stdout should not have progress messages
    assert!(!stdout.contains("Scanning"));
}
```

- [ ] **Step 4: Run tests**

Run: `cd ~/workspace/cq && cargo test`
Expected: all tests pass (existing 34 + new integration tests).

- [ ] **Step 5: Commit**

```
git add tests/integration_test.rs
git commit -m "test: add integration tests with assert_cmd"
```
