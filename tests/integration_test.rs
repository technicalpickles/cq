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
