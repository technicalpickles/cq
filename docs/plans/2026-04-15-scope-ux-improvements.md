# Scope UX Improvements

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Improve cq's scope feedback so users understand what's being queried and get actionable errors when things don't match.

**Architecture:** Four targeted changes to existing code: (1) validate `--session` as UUID with clear errors, (2) make `cq projects` always unscoped, (3) show the actual path in auto-scope hints, (4) update the skill docs. All changes are in existing files, no new modules.

**Tech Stack:** Rust, clap, DuckDB, existing test infrastructure (assert_cmd + tempfile)

---

### Task 1: Validate `--session` as UUID format

`--session` takes a session ID. Claude Code uses full UUIDs (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`), and doesn't support short prefixes. Currently, passing a non-UUID silently produces empty results. We want: format validation before querying, and a "not found" error after querying.

**Files:**
- Modify: `src/scope.rs` (add UUID validation)
- Modify: `src/main.rs:132-147` (validate early, before DB setup)
- Modify: `src/commands/sessions.rs:120-125` (session-not-found error)
- Modify: `src/commands/messages.rs:91-97` (session-not-found error)
- Modify: `src/commands/tools.rs:67-69` (session-not-found error)
- Test: `tests/integration_test.rs` (new tests)

- [ ] **Step 1: Write failing test for invalid session format**

In `tests/integration_test.rs`, add:

```rust
#[test]
fn session_invalid_format_errors() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--session", "not-a-uuid", "sessions"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a valid session ID"),
        "Should error on invalid UUID format, got: {stderr}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test integration_test session_invalid_format_errors`
Expected: FAIL (currently exits 0 with silent empty results)

- [ ] **Step 3: Add UUID validation to scope.rs**

In `src/scope.rs`, add a validation method:

```rust
/// Validate that a session ID looks like a UUID.
/// Format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx (8-4-4-4-12 hex chars)
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
            "'{}' is not a valid session ID. Expected UUID format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
            id
        ))
    }
}
```

- [ ] **Step 4: Call validation early in main.rs**

In `src/main.rs`, after parsing args but before DB setup (around line 132), add validation:

```rust
// Validate --session format before doing any work
if let Some(ref session) = cli.session {
    if let Err(e) = cq::scope::validate_session_id(session) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --test integration_test session_invalid_format_errors`
Expected: PASS

- [ ] **Step 6: Write failing test for valid UUID not found**

```rust
#[test]
fn session_not_found_errors() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--session", "00000000-0000-0000-0000-000000000000", "sessions"])
        .output()
        .unwrap();
    // Should still succeed (exit 0) but show a specific "not found" message
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Session 00000000") && stderr.contains("not found"),
        "Should show session-not-found message, got: {stderr}"
    );
}
```

- [ ] **Step 7: Run test to verify it fails**

Run: `cargo test --test integration_test session_not_found_errors`
Expected: FAIL (currently shows generic "No results.")

- [ ] **Step 8: Add session-not-found handling to commands/mod.rs**

In `src/commands/mod.rs`, add a helper for session-specific "not found":

```rust
/// Print "Session <id> not found." when --session is specified but no results match.
pub fn print_session_not_found(session_id: &str) {
    let short = &session_id[..std::cmp::min(8, session_id.len())];
    eprintln!(
        "Session {}... not found.",
        short
    );
}
```

- [ ] **Step 9: Use session-not-found in sessions.rs**

In `src/commands/sessions.rs`, replace lines 120-125:

```rust
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
```

- [ ] **Step 10: Use session-not-found in messages.rs**

In `src/commands/messages.rs`, replace lines 91-97:

```rust
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
```

- [ ] **Step 11: Use session-not-found in tools.rs (detail mode)**

In `src/commands/tools.rs`, replace lines 157-164:

```rust
if detail_rows.is_empty() {
    if scope.session.is_some() {
        super::print_session_not_found(scope.session.as_ref().unwrap());
    } else {
        let mut extras: Vec<&str> = Vec::new();
        if grep.is_some() { extras.push("--grep"); }
        if errors_only { extras.push("--errors"); }
        if tool_name.is_some() { extras.push("[name]"); }
        super::print_no_results(&scope, &extras);
    }
    return Ok(());
}
```

Also in `run_summary` (line 338-339):

```rust
if summary_rows.is_empty() {
    if scope.session.is_some() {
        super::print_session_not_found(scope.session.as_ref().unwrap());
    } else {
        super::print_no_results(&scope, &[]);
    }
    return Ok(());
}
```

And in `run_with_fields` (lines 259-264):

```rust
if rows.is_empty() {
    if scope.session.is_some() {
        super::print_session_not_found(scope.session.as_ref().unwrap());
    } else {
        let mut extras: Vec<&str> = Vec::new();
        if errors_only { extras.push("--errors"); }
        super::print_no_results(scope, &extras);
    }
    return Ok(());
}
```

- [ ] **Step 12: Run tests to verify session-not-found passes**

Run: `cargo test --test integration_test session_not_found`
Expected: PASS

- [ ] **Step 13: Add unit test for UUID validation**

In `src/scope.rs` tests module:

```rust
#[test]
fn validate_session_id_valid() {
    assert!(validate_session_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
}

#[test]
fn validate_session_id_short_prefix() {
    assert!(validate_session_id("550e8400").is_err());
}

#[test]
fn validate_session_id_garbage() {
    assert!(validate_session_id("not-a-uuid").is_err());
}

#[test]
fn validate_session_id_empty() {
    assert!(validate_session_id("").is_err());
}
```

- [ ] **Step 14: Run all tests**

Run: `cargo test`
Expected: All pass

- [ ] **Step 15: Commit**

```
feat: validate --session as UUID with clear error messages

--session now validates format upfront and shows "Session xxx... not found"
instead of generic "No results." when a valid UUID has no matching data.
```

---

### Task 2: Make `cq projects` always unscoped

`cq projects` is a discovery command. Scoping it to the current project defeats its purpose, since you'd only ever see one project.

**Files:**
- Modify: `src/main.rs:134-147` (skip auto-scope for Projects command)
- Test: `tests/integration_test.rs` (new test)

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn projects_always_unscoped() {
    let projects = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let project_a = projects.path().join("-Users-test-myproject");
    let project_b = projects.path().join("-Users-test-webapp");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    std::fs::copy(fixture_path("simple_session.jsonl"), project_a.join("sess-a.jsonl")).unwrap();
    std::fs::copy(fixture_path("multi_tool_session.jsonl"), project_b.join("sess-b.jsonl")).unwrap();

    let env = TestEnv { projects, cache };

    // Run from "myproject" dir. projects should still show BOTH projects.
    let output = cq_cmd(&env)
        .env("PWD", "/Users/test/myproject")
        .arg("projects")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("myproject"), "Should show myproject, got: {stdout}");
    assert!(stdout.contains("webapp"), "Should show webapp even when auto-scoped elsewhere, got: {stdout}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test integration_test projects_always_unscoped`
Expected: FAIL (webapp filtered out by auto-scoping)

- [ ] **Step 3: Skip auto-scoping for Projects command**

In `src/main.rs`, modify the auto-scoping logic (around line 134). The `command` is available from `cli.command`. Check if it's `Command::Projects` and treat it like `--all`:

```rust
let is_projects_cmd = matches!(cli.command, Command::Projects { .. });

let (project, auto_scoped) = if cli.project.is_some() {
    (cli.project, false)
} else if cli.all || cli.json || is_projects_cmd {
    (None, false)
} else {
    match std::env::var("PWD").ok() {
        Some(cwd) => match provider.project_for_cwd(&cwd) {
            Some(project_path) => (Some(project_path), true),
            None => (None, false),
        },
        None => (None, false),
    }
};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test integration_test projects_always_unscoped`
Expected: PASS

- [ ] **Step 5: Verify existing projects tests still pass**

Run: `cargo test --test integration_test projects`
Expected: All projects-related tests pass

- [ ] **Step 6: Commit**

```
fix: make cq projects always show all projects regardless of auto-scoping

projects is a discovery command; scoping it to the current directory
meant you could only see one project, defeating its purpose.
```

---

### Task 3: Show path in auto-scope hint

Change the auto-scope hint from `Scoped to pickleton` to `Scoped to ~/pickleton` so users can see exactly what directory is being matched.

**Files:**
- Modify: `src/main.rs:149-154` (change hint formatting)
- Test: `tests/integration_test.rs` (update existing test)

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn auto_scope_hint_shows_path() {
    let projects = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let project_a = projects.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::copy(fixture_path("simple_session.jsonl"), project_a.join("sess-a.jsonl")).unwrap();

    let env = TestEnv { projects, cache };

    let output = cq_cmd(&env)
        .env("PWD", "/Users/test/myproject")
        .env("HOME", "/Users/test")
        .arg("sessions")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should show path, not just leaf name
    assert!(
        stderr.contains("~/myproject"),
        "Should show ~/myproject in scope hint, got: {stderr}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test integration_test auto_scope_hint_shows_path`
Expected: FAIL (currently shows "Scoped to myproject")

- [ ] **Step 3: Add abbreviate_home helper to style.rs**

In `src/style.rs`:

```rust
/// Replace the home directory prefix with ~ for display.
pub fn abbreviate_home(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path.starts_with(home_str.as_ref()) {
            return format!("~{}", &path[home_str.len()..]);
        }
    }
    path.to_string()
}
```

- [ ] **Step 4: Use path instead of leaf in hint**

In `src/main.rs`, replace lines 149-154:

```rust
if auto_scoped && !cli.json {
    if let Some(ref p) = project {
        let display = cq::style::abbreviate_home(p);
        eprintln!("{}", cq::style::hint(&format!("Scoped to {display} (use --all for everything)")));
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --test integration_test auto_scope_hint_shows_path`
Expected: PASS

- [ ] **Step 6: Update existing auto_scope test**

The `auto_scope_to_current_project` test (line 398) checks for `Scoped to` on stderr. Update the assertion to match the new format. The test sets `PWD=/Users/test/myproject` and `HOME` isn't set in tests, so `abbreviate_home` will fall back to the full path. Update to match either format:

```rust
assert!(
    stderr.contains("Scoped to") && stderr.contains("myproject"),
    "Should show scope notice with project path, got: {stderr}"
);
```

- [ ] **Step 7: Add unit test for abbreviate_home**

In `src/style.rs` tests module:

```rust
#[test]
fn abbreviate_home_with_match() {
    // This test depends on having a home dir, which we always do in test envs
    if let Some(home) = dirs::home_dir() {
        let path = format!("{}/projects/myapp", home.display());
        let result = abbreviate_home(&path);
        assert!(result.starts_with("~/"), "Should start with ~/, got: {result}");
        assert!(result.ends_with("/projects/myapp"), "Should end with path, got: {result}");
    }
}

#[test]
fn abbreviate_home_no_match() {
    let result = abbreviate_home("/some/other/path");
    assert_eq!(result, "/some/other/path");
}
```

- [ ] **Step 8: Run all tests**

Run: `cargo test`
Expected: All pass

- [ ] **Step 9: Commit**

```
feat: show project path in auto-scope hint instead of leaf name

Shows "Scoped to ~/pickleton" instead of "Scoped to pickleton" so
users can see exactly what directory is being matched.
```

---

### Task 4: Rebuild and install

The installed binary is missing `--all` and other recent features.

**Files:** None (build step only)

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: All pass

- [ ] **Step 2: Install**

Run: `cargo install --path .`
Expected: Binary installed to `~/.cargo/bin/cq`

- [ ] **Step 3: Verify --all flag exists**

Run: `cq --help`
Expected: Output includes `--all` flag

- [ ] **Step 4: Verify scope hint shows path**

Run: `cd ~/pickleton && cq sessions --limit 1`
Expected: Hint shows `Scoped to ~/pickleton (use --all for everything)`

---

### Task 5: Update cq skill documentation

Add `--all` to the documented flags and add guidance about cross-project queries.

**Files:**
- Modify: `~/.claude/skills/cq/SKILL.md`

- [ ] **Step 1: Add --all to global flags section**

In the Global Flags section, add:

```markdown
- `--all` - Show all projects (disable auto-scoping to current directory)
```

- [ ] **Step 2: Update "Working With cq" section**

Replace the auto-scoping bullet and add cross-project guidance:

```markdown
- `cq` auto-scopes to the current directory's project. The scope hint shows which path is being matched.
- Use `--project <name>` to query a different project (substring match, searches all project directories).
- Use `--all` to disable auto-scoping entirely and query across all projects.
- When searching for work done in a different repo (e.g. karafka sessions while in pickleton), use `--project <name>` or `--all`. Auto-scoping only matches sessions from the current directory's project.
- `cq projects` always shows all projects regardless of auto-scoping, so you can see what's available.
```

- [ ] **Step 3: Commit**

```
docs: update cq skill with --all flag and cross-project guidance
```
