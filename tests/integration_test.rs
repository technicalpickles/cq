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
    // schema doesn't need DB connection, no CQ_PROJECTS_DIR needed
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
        .stdout(predicate::str::contains("\u{2588}")); // bar chart block char
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
        .stdout(predicate::str::contains("\u{2500}")); // header separator
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
        .stdout(predicate::str::contains("\u{2500}")); // header separator
}

#[test]
fn sql_raw_query() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["sql", "SELECT count(*) AS n FROM tool_calls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("n"))
        .stdout(predicate::str::contains("\u{2500}")); // light table format separator
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
        stderr.contains("Loaded 0 files"),
        "Expected 'Loaded 0 files' on stderr, got: {stderr}"
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
    // Progress messages go to stderr
    assert!(
        stderr.contains("Synced") || stderr.contains("Loaded"),
        "Expected progress on stderr, got: {stderr}"
    );
    // stdout should not have progress messages
    assert!(!stdout.contains("Synced"), "Progress message leaked to stdout: {stdout}");
    assert!(!stdout.contains("Loaded"), "Progress message leaked to stdout: {stdout}");
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
    // Use a fresh Command WITHOUT NO_COLOR env var, then pass --no-color flag
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.env("CQ_PROJECTS_DIR", env.projects.path());
    cmd.env("CQ_CACHE_DIR", env.cache.path());
    // Explicitly unset NO_COLOR so we're testing the flag, not the env var
    cmd.env_remove("NO_COLOR");

    let output = cmd
        .args(["--no-color", "tools"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("\x1b["),
        "Expected no ANSI escape codes in output, got: {stdout}"
    );
}

#[test]
fn projects_default_oneline() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .arg("projects")
        .assert()
        .success()
        .stdout(predicate::str::contains("myproject"))
        .stdout(predicate::str::contains("msgs"))
        .stdout(predicate::str::contains("tools"));
}

#[test]
fn projects_table() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .args(["--table", "projects"])
        .assert()
        .success()
        .stdout(predicate::str::contains("project"))
        .stdout(predicate::str::contains("sessions"))
        .stdout(predicate::str::contains("\u{2500}"));
}

#[test]
fn projects_json() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "projects"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert!(!parsed.is_empty());
    let first = &parsed[0];
    assert!(first.get("project").is_some());
    assert!(first.get("sessions").is_some());
    assert!(first.get("messages").is_some());
    assert!(first.get("tools").is_some());
    assert!(first.get("skills").is_some());
    assert!(first.get("skill_count").is_some());
}

#[test]
fn projects_skills_flag() {
    let env = setup_env(&["multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .args(["projects", "--skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sanitation")) // skill from fixture
        .stdout(predicate::str::contains("\u{2514}")); // └ prefix for skill line
}

#[test]
fn projects_json_includes_skill_names() {
    let env = setup_env(&["multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "projects"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let first = &parsed[0];
    let skills = first["skills"].as_array().unwrap();
    assert!(skills.iter().any(|s| s.as_str() == Some("sanitation")));
}

#[test]
fn help_shows_projects_command() {
    Command::cargo_bin("cq").unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("projects"));
}

#[test]
fn tools_fields_extracts_command() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "Bash", "--fields", "command"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should show the extracted command, not raw JSON
    assert!(stdout.contains("ls"), "Expected extracted command in output, got: {stdout}");
    assert!(!stdout.contains("{\"command\""), "Should not contain raw JSON, got: {stdout}");
}

#[test]
fn tools_fields_multiple() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["tools", "Bash", "--fields", "command,description"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List files"));
}

#[test]
fn tools_fields_table_format() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["--table", "tools", "Bash", "--fields", "command"])
        .assert()
        .success()
        .stdout(predicate::str::contains("command")) // header
        .stdout(predicate::str::contains("\u{2500}")); // separator
}

#[test]
fn tools_fields_json_format() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "tools", "Bash", "--fields", "command"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert!(!parsed.is_empty());
    assert!(parsed[0].get("command").is_some(), "JSON should have 'command' field");
}

#[test]
fn truncation_hint_shown_on_stderr() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "Bash", "--limit", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Showing 1 of"),
        "Expected truncation hint on stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("--limit 0"),
        "Expected --limit 0 suggestion on stderr, got: {stderr}"
    );
}

#[test]
fn no_truncation_hint_when_all_results_shown() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "Bash", "--limit", "100"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Showing"),
        "Should not show truncation hint when all results fit, got: {stderr}"
    );
}

#[test]
fn no_truncation_hint_in_json_mode() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "tools", "Bash", "--limit", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Showing"),
        "Should not show truncation hint in JSON mode, got: {stderr}"
    );
}

#[test]
fn auto_scope_to_current_project() {
    // Create two project dirs with different sessions.
    // The directory names must match the cwd embedded in the fixture files:
    //   simple_session.jsonl has cwd "/Users/test/myproject" -> dir "-Users-test-myproject"
    //   multi_tool_session.jsonl has cwd "/Users/test/webapp" -> dir "-Users-test-webapp"
    let projects = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let project_a = projects.path().join("-Users-test-myproject");
    let project_b = projects.path().join("-Users-test-webapp");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    std::fs::copy(fixture_path("simple_session.jsonl"), project_a.join("sess-a.jsonl")).unwrap();
    std::fs::copy(fixture_path("multi_tool_session.jsonl"), project_b.join("sess-b.jsonl")).unwrap();

    let env = TestEnv { projects, cache };

    // Run from "myproject" dir (matches simple_session.jsonl's cwd), should auto-scope
    let output = cq_cmd(&env)
        .env("PWD", "/Users/test/myproject")
        .arg("sessions")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // sess-001 is in myproject, sess-002 is in webapp
    assert!(stdout.contains("sess-001"), "Should show myproject session (sess-001), got: {stdout}");
    assert!(!stdout.contains("sess-002"), "Should not show webapp session (sess-002), got: {stdout}");
    assert!(stderr.contains("Scoped to"), "Should show scope notice, got: {stderr}");
}

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

#[test]
fn no_results_shows_suggestions() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--project", "nonexistent", "sessions"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No results"),
        "Should show no results, got: {stderr}"
    );
    assert!(
        stderr.contains("--project"),
        "Should mention active filter in suggestion, got: {stderr}"
    );
}

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

#[test]
fn session_not_found_errors() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--session", "00000000-0000-0000-0000-000000000000", "sessions"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Session 00000000") && stderr.contains("not found"),
        "Should show session-not-found message, got: {stderr}"
    );
}

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

#[test]
fn all_flag_overrides_auto_scope() {
    let projects = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let project_a = projects.path().join("-Users-test-myproject");
    let project_b = projects.path().join("-Users-test-webapp");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    std::fs::copy(fixture_path("simple_session.jsonl"), project_a.join("sess-a.jsonl")).unwrap();
    std::fs::copy(fixture_path("multi_tool_session.jsonl"), project_b.join("sess-b.jsonl")).unwrap();

    let env = TestEnv { projects, cache };

    // Run with --all from myproject dir, should show sessions from BOTH projects
    let output = cq_cmd(&env)
        .env("PWD", "/Users/test/myproject")
        .args(["--all", "sessions"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Scoped to"), "Should not show scope notice with --all, got: {stderr}");
    // Both sessions should appear
    assert!(stdout.contains("sess-001"), "Should show myproject session with --all, got: {stdout}");
    assert!(stdout.contains("sess-002"), "Should show webapp session with --all, got: {stdout}");
}

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
        "Should show invalid input in quotes, got: {stderr}"
    );
    assert!(
        stderr.contains("7d, 24h, 30m"),
        "Should show example formats, got: {stderr}"
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
        stderr.contains("'x'"),
        "Should show the bad unit in quotes, got: {stderr}"
    );
    assert!(
        stderr.contains("'7x'"),
        "Should show the full input in quotes, got: {stderr}"
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
        stderr.contains("not a valid session ID"),
        "Should still mention invalid session ID, got: {stderr}"
    );
    assert!(
        stderr.contains("cq sessions"),
        "Should hint to run 'cq sessions', got: {stderr}"
    );
}

#[test]
fn wide_flag_shows_full_values() {
    let env = setup_env(&["long_values_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--wide", "tools", "Bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The long command should NOT be truncated (no "..." at end of the input column)
    // The fixture has a command longer than 60 chars, which would normally be truncated
    assert!(
        stdout.contains("sort -rn"),
        "With --wide, full command should be visible, got: {stdout}"
    );
    assert!(
        !stdout.contains("..."),
        "With --wide, output should not be truncated, got: {stdout}"
    );
}

#[test]
fn default_truncates_long_values() {
    let env = setup_env(&["long_values_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "Bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The long command from the fixture should be truncated in default mode
    // (assert_cmd runs as a piped process, but we test that the default code path works)
    // Note: when piped (not a TTY), wide is automatically enabled, so the output won't be truncated.
    // This test just confirms the command works without errors in default mode.
    assert!(
        stdout.contains("find"),
        "Should show tool input content, got: {stdout}"
    );
}

#[test]
fn wide_flag_in_help() {
    Command::cargo_bin("cq").unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--wide"));
}

#[test]
fn wide_flag_with_table_format() {
    let env = setup_env(&["long_values_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--wide", "--table", "tools", "Bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sort -rn"),
        "With --wide --table, full command should be visible, got: {stdout}"
    );
}

#[test]
fn wide_flag_sessions() {
    let env = setup_env(&["long_values_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--wide", "sessions"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The first_user_message is long, should not be truncated with --wide
    assert!(
        stdout.contains("truncation width"),
        "With --wide, full first_user_message should be visible, got: {stdout}"
    );
}

#[test]
fn wide_flag_messages() {
    let env = setup_env(&["long_values_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--wide", "messages"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The user message text is long, should not be truncated with --wide
    assert!(
        stdout.contains("truncation width"),
        "With --wide, full message text should be visible, got: {stdout}"
    );
}

#[test]
fn wide_flag_sql() {
    let env = setup_env(&["long_values_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--wide", "sql", "SELECT name, CAST(input AS VARCHAR) AS input FROM tool_calls LIMIT 1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sort -rn"),
        "With --wide sql, full values should be visible, got: {stdout}"
    );
}

#[test]
fn schema_unknown_view_error_format() {
    let output = Command::cargo_bin("cq").unwrap()
        .args(["schema", "bogus"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "Should exit with failure for unknown view");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown view 'bogus'"),
        "Should show unknown view with input in quotes, got: {stderr}"
    );
    assert!(
        stderr.contains("Valid views:"),
        "Should list valid views, got: {stderr}"
    );
}

// --- messages --fields tests ---

#[test]
fn messages_fields_text() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "--fields", "text"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("list the files"),
        "Should show message text, got: {stdout}"
    );
    // session_id should NOT appear as a column when only text is requested
    assert!(
        !stdout.contains("sess-001"),
        "Should not show session_id when only text field requested, got: {stdout}"
    );
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
        stderr.contains("'bogus'"),
        "Should show the invalid field name in quotes, got: {stderr}"
    );
    assert!(
        stderr.contains("Valid fields:"),
        "Should list valid fields, got: {stderr}"
    );
    assert!(
        stderr.contains("cq schema messages"),
        "Should hint to run 'cq schema messages', got: {stderr}"
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
    assert!(parsed[0].get("text").is_some(), "JSON should have 'text' field, got: {}", parsed[0]);
    assert!(parsed[0].get("type").is_some(), "JSON should have 'type' field, got: {}", parsed[0]);
    // Should NOT have fields that weren't requested
    assert!(parsed[0].get("session_id").is_none(), "JSON should not have 'session_id' when not requested, got: {}", parsed[0]);
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
    // "session" should resolve to session_id and show the ID
    assert!(
        stdout.contains("sess-001"),
        "Should show session_id (alias 'session' resolved), got: {stdout}"
    );
    assert!(
        stdout.contains("list the files"),
        "Should show text, got: {stdout}"
    );
}

// --- sessions --fields tests ---

#[test]
fn sessions_fields_session_id() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["sessions", "--fields", "session_id"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sess-001"),
        "Should show session_id, got: {stdout}"
    );
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
        stderr.contains("'bogus'"),
        "Should show the invalid field name in quotes, got: {stderr}"
    );
    assert!(
        stderr.contains("cq schema sessions"),
        "Should hint to run 'cq schema sessions', got: {stderr}"
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
    assert!(
        stdout.contains("sess-001"),
        "Should show session_id, got: {stdout}"
    );
    assert!(
        stdout.contains("list the files"),
        "Should show first_user_message, got: {stdout}"
    );
}

// --- --count-by tests ---

#[test]
fn tools_count_by_name() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "--count-by", "name"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should produce bar chart with tool names
    assert!(stdout.contains("Bash"), "Should show Bash tool name, got: {stdout}");
    assert!(stdout.contains("\u{2588}"), "Should show bar chart blocks, got: {stdout}");
}

#[test]
fn tools_count_by_session() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "--count-by", "session"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should produce bar chart with session IDs
    assert!(stdout.contains("sess-001"), "Should show session ID, got: {stdout}");
    assert!(stdout.contains("sess-002"), "Should show session ID, got: {stdout}");
    assert!(stdout.contains("\u{2588}"), "Should show bar chart blocks, got: {stdout}");
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
    assert!(
        stderr.contains("'bogus'"),
        "Should show invalid column in quotes, got: {stderr}"
    );
    assert!(
        stderr.contains("Valid columns:"),
        "Should list valid columns, got: {stderr}"
    );
}

#[test]
fn tools_count_by_with_filter() {
    let env = setup_env(&["simple_session.jsonl", "error_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "--errors", "--count-by", "session"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // error_session.jsonl has sess-003 with an error
    assert!(stdout.contains("sess-003"), "Should show error session, got: {stdout}");
    // simple_session.jsonl sess-001 has no errors, should not appear
    assert!(!stdout.contains("sess-001"), "Should not show non-error session, got: {stdout}");
}

#[test]
fn messages_count_by_type() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "--count-by", "type"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("user"), "Should show user type, got: {stdout}");
    assert!(stdout.contains("assistant"), "Should show assistant type, got: {stdout}");
    assert!(stdout.contains("\u{2588}"), "Should show bar chart blocks, got: {stdout}");
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
    assert!(parsed[0].get("name").is_some(), "JSON should have 'name' key, got: {}", parsed[0]);
    assert!(parsed[0].get("count").is_some(), "JSON should have 'count' key, got: {}", parsed[0]);
}

#[test]
fn count_by_and_fields_conflict() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "Bash", "--count-by", "name", "--fields", "command"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--count-by") && stderr.contains("--fields"),
        "Should mention both flags in error, got: {stderr}"
    );
}

// --- subagent recursive indexing tests ---

fn write_file(path: &std::path::Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn setup_subagent_env() -> TestEnv {
    let projects = TempDir::new().unwrap();
    let proj = projects.path().join("-Users-test-myproject");
    let sess = "11111111-1111-1111-1111-111111111111";

    // Top-level main-loop session.
    write_file(&proj.join(format!("{sess}.jsonl")),
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}]},\"uuid\":\"a1\",\"parentUuid\":null,\"isSidechain\":false,\"timestamp\":\"2026-05-01T10:00:00.000Z\",\"sessionId\":\"11111111-1111-1111-1111-111111111111\",\"cwd\":\"/Users/test/myproject\"}\n");

    // Plain subagent.
    let sub = proj.join(sess).join("subagents");
    write_file(&sub.join("agent-aaa.jsonl"),
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m2\",\"role\":\"assistant\",\"model\":\"claude-haiku-4-5\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t2\",\"name\":\"Read\",\"input\":{\"file_path\":\"/x\"}}]},\"uuid\":\"a2\",\"parentUuid\":null,\"isSidechain\":true,\"agentId\":\"aaa\",\"timestamp\":\"2026-05-01T10:00:01.000Z\",\"sessionId\":\"11111111-1111-1111-1111-111111111111\",\"cwd\":\"/Users/test/myproject\"}\n");
    std::fs::write(sub.join("agent-aaa.meta.json"), "{\"agentType\":\"general-purpose\"}").unwrap();

    // Workflow subagent + journal ledger (must be excluded).
    let wf = sub.join("workflows").join("wf_xyz");
    write_file(&wf.join("agent-bbb.jsonl"),
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m3\",\"role\":\"assistant\",\"model\":\"claude-haiku-4-5\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t3\",\"name\":\"Glob\",\"input\":{\"pattern\":\"*\"}}]},\"uuid\":\"a3\",\"parentUuid\":null,\"isSidechain\":true,\"agentId\":\"bbb\",\"timestamp\":\"2026-05-01T10:00:02.000Z\",\"sessionId\":\"11111111-1111-1111-1111-111111111111\",\"cwd\":\"/Users/test/myproject\"}\n");
    std::fs::write(wf.join("agent-bbb.meta.json"), "{\"agentType\":\"Explore\"}").unwrap();
    std::fs::write(wf.join("journal.jsonl"), "{\"type\":\"started\",\"key\":\"v2:abc\",\"agentId\":\"bbb\"}\n").unwrap();

    TestEnv { projects, cache: TempDir::new().unwrap() }
}

#[test]
fn indexes_subagent_tool_calls() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args(["sql", "SELECT COUNT(*) FROM tool_calls WHERE is_sidechain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2")); // Read + Glob
}

#[test]
fn excludes_journal_jsonl() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args(["sql", "SELECT COUNT(*) FROM raw_records WHERE source_file LIKE '%journal.jsonl%'"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0"));
}

#[test]
fn workflow_id_visible_end_to_end() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args(["sql", "SELECT workflow_id FROM tool_calls WHERE name = 'Glob'"])
        .assert()
        .success()
        .stdout(predicate::str::contains("wf_xyz"));
}

// --- timeline tests ---

fn timeline_env() -> TestEnv {
    setup_env(&["timeline_session.jsonl"])
}

#[test]
fn tools_help_shows_context_flags() {
    Command::cargo_bin("cq").unwrap()
        .args(["tools", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("-A"))
        .stdout(predicate::str::contains("-B"))
        .stdout(predicate::str::contains("-C"))
        .stdout(predicate::str::contains("messages after each match"))
        .stdout(predicate::str::contains("messages before each match"));
}

#[test]
fn messages_help_shows_context_flags() {
    Command::cargo_bin("cq").unwrap()
        .args(["messages", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("-A"))
        .stdout(predicate::str::contains("-B"))
        .stdout(predicate::str::contains("-C"));
}

#[test]
fn sessions_timeline_shows_events() {
    let env = timeline_env();
    let output = cq_cmd(&env)
        .args(["sessions", "--session", "aaaa0000-0000-0000-0000-000000000001", "--timeline"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("call"), "Should show 'call' events, got: {stdout}");
    assert!(stdout.contains("result"), "Should show 'result' events, got: {stdout}");
    assert!(stdout.contains("Read"), "Should show Read tool, got: {stdout}");
    assert!(stdout.contains("Bash"), "Should show Bash tool, got: {stdout}");
}

#[test]
fn sessions_timeline_requires_session() {
    let env = timeline_env();
    let output = cq_cmd(&env)
        .args(["sessions", "--timeline"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--timeline requires --session"),
        "Should error about requiring --session, got: {stderr}"
    );
    assert!(
        stderr.contains("cq sessions"),
        "Should hint about finding session IDs, got: {stderr}"
    );
}

#[test]
fn sessions_timeline_shows_errors() {
    let env = timeline_env();
    let output = cq_cmd(&env)
        .args(["sessions", "--session", "aaaa0000-0000-0000-0000-000000000001", "--timeline"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("error"),
        "Should show 'error' for failed tool result, got: {stdout}"
    );
}

#[test]
fn sessions_timeline_json() {
    let env = timeline_env();
    let output = cq_cmd(&env)
        .args(["--json", "sessions", "--session", "aaaa0000-0000-0000-0000-000000000001", "--timeline"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .expect(&format!("Should be valid JSON, got: {stdout}"));
    assert!(!parsed.is_empty(), "Should have timeline events");
    assert!(
        parsed[0].get("event").is_some(),
        "JSON should have 'event' key, got: {}",
        parsed[0]
    );
    assert!(
        parsed[0].get("name").is_some(),
        "JSON should have 'name' key, got: {}",
        parsed[0]
    );
}

#[test]
fn no_reindex_skips_sync() {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.arg("--no-reindex").arg("sessions").arg("--limit").arg("1");
    cmd.assert().success();
}

#[test]
fn reindex_and_no_reindex_conflict() {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.arg("--reindex").arg("--no-reindex").arg("sessions");
    cmd.assert().failure();
}

#[test]
fn sessions_count_by_project() {
    let projects = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let project_a = projects.path().join("-Users-test-myproject");
    let project_b = projects.path().join("-Users-test-webapp");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    std::fs::copy(fixture_path("simple_session.jsonl"), project_a.join("sess-a.jsonl")).unwrap();
    std::fs::copy(fixture_path("multi_tool_session.jsonl"), project_b.join("sess-b.jsonl")).unwrap();

    let env = TestEnv { projects, cache };
    let output = cq_cmd(&env)
        .args(["sessions", "--count-by", "project"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("myproject"), "Should show project name, got: {stdout}");
    assert!(stdout.contains("\u{2588}"), "Should show bar chart blocks, got: {stdout}");
}

#[test]
fn tools_context_conflicts_with_count_by() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "-C", "2", "--count-by", "name"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--count-by") && stderr.contains("context"),
        "Should explain conflict between --count-by and context flags, got: {stderr}"
    );
}

#[test]
fn messages_grep_with_context_a_shows_following_messages() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "messages", "--grep", "NEEDLE", "-A", "2"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let rows = parsed.as_array().unwrap();
    assert_eq!(rows.len(), 3, "expected match + 2 after, got {}: {}", rows.len(), stdout);
    assert_eq!(rows[0]["match_kind"], "match");
    assert_eq!(rows[1]["match_kind"], "after");
    assert_eq!(rows[2]["match_kind"], "after");
    assert!(rows[0]["text"].as_str().unwrap().contains("NEEDLE"));
}

#[test]
fn messages_grep_with_context_c_shows_surrounding() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "messages", "--grep", "NEEDLE", "-C", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["match_kind"], "before");
    assert_eq!(rows[1]["match_kind"], "match");
    assert_eq!(rows[2]["match_kind"], "after");
}

#[test]
fn messages_context_does_not_cross_session_boundary() {
    // Two sessions; match is in context_session. -B 10 shouldn't pull anything from simple_session.
    let env = setup_env(&["simple_session.jsonl", "context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "messages", "--grep", "NEEDLE", "-B", "10"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let match_session = rows.iter()
        .find(|r| r["match_kind"] == "match")
        .and_then(|r| r["session_id"].as_str())
        .unwrap()
        .to_string();
    for row in &rows {
        assert_eq!(row["session_id"].as_str().unwrap(), match_session, "cross-session leak: {row}");
    }
}

#[test]
fn tools_with_context_c_shows_surrounding_messages() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "tools", "Read", "-C", "1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    // Read tool is at ord 4; -C 1 gives ords 3, 4, 5 -> 3 rows
    assert_eq!(rows.len(), 3);
    // Match row should carry the tool name.
    let match_row = rows.iter().find(|r| r["match_kind"] == "match").unwrap();
    assert_eq!(match_row["tool_name"], "Read", "match row should carry tool name, got: {match_row}");
    // Context rows are message-shaped; tool_name should be null.
    let context_rows: Vec<_> = rows.iter().filter(|r| r["match_kind"] != "match").collect();
    assert_eq!(context_rows.len(), 2);
    for ctx_row in &context_rows {
        assert!(ctx_row["tool_name"].is_null(), "context row should not have tool_name, got: {ctx_row}");
        assert!(ctx_row["type"].is_string(), "context row should be message-shaped, got: {ctx_row}");
    }
}

#[test]
fn tools_with_context_respects_match_limit() {
    // multi_tool_session.jsonl has multiple tool calls.
    // --limit 1 -C 0 should return exactly 1 match row (grep -m semantics).
    let env = setup_env(&["multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "tools", "--limit", "1", "-C", "0"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let match_count = rows.iter().filter(|r| r["match_kind"] == "match").count();
    assert_eq!(match_count, 1, "expected 1 match with --limit 1, rows: {stdout}");
}

#[test]
fn tools_with_context_non_json_does_not_error() {
    // Task 6 will add a pretty TTY renderer; for now just make sure the Default/Table path
    // doesn't crash.
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "Read", "-C", "1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "non-JSON tools context path should not error, stderr: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn tty_context_hides_match_kind_and_group_columns() {
    let env = setup_env(&["context_session.jsonl"]);
    // Default mode (no --json, no --table) = TTY-style.
    // NO_COLOR=1 via cq_cmd strips ANSI so we can grep plain text.
    let output = cq_cmd(&env)
        .args(["messages", "--grep", "NEEDLE", "-C", "1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);

    let non_blank_lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(non_blank_lines.len(), 3, "expected 3 output rows, got:\n{stdout}");

    // Each row's cells (split by the two-space delimiter print_context_rows uses) should not
    // include any bare `match_kind` or `match_group` column value.
    for line in &non_blank_lines {
        let cells: Vec<&str> = line.split("  ").map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        for cell in &cells {
            assert!(
                !matches!(*cell, "match" | "before" | "after"),
                "TTY output should not include bare match_kind column value '{cell}' in row:\n{line}"
            );
        }
    }
}

#[test]
fn tty_context_single_group_no_separator() {
    // One match, -C 0 -- one group, no `--` separator line.
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "--grep", "NEEDLE", "-A", "0", "-B", "0"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let separator_lines = stdout.lines().filter(|l| l.trim() == "--").count();
    assert_eq!(separator_lines, 0, "single group should not have '--' separator, got:\n{stdout}");
}

#[test]
fn tty_context_non_contiguous_groups_show_separator() {
    // Grep "ne" matches "one" (ord 1), "six NEEDLE" (ord 6), and "nine" (ord 9) in the fixture.
    // With -C 0, these form three non-contiguous groups separated by `--` separators.
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "--grep", "ne", "-A", "0", "-B", "0"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let separator_lines = stdout.lines().filter(|l| l.trim() == "--").count();
    assert_eq!(separator_lines, 2, "three non-contiguous matches should have exactly 2 '--' separators, got:\n{stdout}");
    // Also verify we got three data rows plus two separators (5 total non-blank lines).
    let non_blank: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(non_blank.len(), 5, "expected 3 match rows + 2 separator lines, got:\n{stdout}");
}

#[test]
fn messages_context_empty_result_prints_no_results() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "--grep", "xyz-will-not-match", "-C", "2"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No results"),
        "expected 'No results' in stderr, got: {stderr}"
    );
}

#[test]
fn messages_fields_conflicts_with_context() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "-C", "1", "--fields", "text"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--fields") && stderr.contains("-A"),
        "expected conflict error mentioning --fields and -A, got: {stderr}"
    );
}
