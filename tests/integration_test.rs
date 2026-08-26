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
    codex_sessions: TempDir,
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
    let codex_sessions = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    TestEnv {
        projects,
        codex_sessions,
        cache,
    }
}

fn cq_cmd(env: &TestEnv) -> Command {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.env("CQ_PROJECTS_DIR", env.projects.path());
    cmd.env("CQ_CACHE_DIR", env.cache.path());
    cmd.env("CQ_CODEX_SESSIONS_DIR", env.codex_sessions.path());
    // Isolate from any real cenv envs on the host so only CQ_PROJECTS_DIR is indexed.
    cmd.env("CENV_BASE", env.cache.path().join("no-such-cenv-base"));
    // Test commands should exercise their chosen runtime explicitly, rather
    // than inheriting the Codex session that happens to run the test suite.
    cmd.env_remove("CODEX_SESSION_ID");
    cmd.env_remove("CODEX_THREAD_ID");
    cmd.env("NO_COLOR", "1");
    cmd
}

fn setup_codex_env() -> TestEnv {
    let env = setup_env(&[]);
    let rollout_dir = env.codex_sessions.path().join("2026/08/26");
    std::fs::create_dir_all(&rollout_dir).unwrap();
    std::fs::copy(
        fixture_path("codex_session.jsonl"),
        rollout_dir.join("rollout-2026-08-26T14-00-00-session.jsonl"),
    )
    .unwrap();
    env
}

fn setup_mixed_harness_env() -> TestEnv {
    let env = setup_env(&["simple_session.jsonl", "hook_events_session.jsonl"]);
    let rollout_dir = env.codex_sessions.path().join("2026/08/26");
    std::fs::create_dir_all(&rollout_dir).unwrap();
    std::fs::copy(
        fixture_path("codex_session.jsonl"),
        rollout_dir.join("rollout-2026-08-26T14-00-00-session.jsonl"),
    )
    .unwrap();
    env
}

#[test]
fn help_shows_commands() {
    Command::cargo_bin("cq")
        .unwrap()
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
    Command::cargo_bin("cq")
        .unwrap()
        .arg("schema")
        .assert()
        .success()
        .stdout(predicate::str::contains("tool_calls"))
        .stdout(predicate::str::contains("messages"));
}

#[test]
fn schema_examples_shows_sql() {
    Command::cargo_bin("cq")
        .unwrap()
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
fn codex_sessions_and_tools_are_queryable() {
    let env = setup_codex_env();

    cq_cmd(&env)
        .env("CODEX_SESSION_ID", "test-codex-session")
        .arg("sessions")
        .assert()
        .success()
        .stdout(predicate::str::contains("019a1b2c"));

    cq_cmd(&env)
        .args([
            "sql",
            "SELECT harness, project, count(*) AS tools FROM tool_calls GROUP BY harness, project",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("codex"))
        .stdout(predicate::str::contains("codex-project"))
        .stdout(predicate::str::contains("2"));
}

#[test]
fn non_codex_runtime_defaults_builtins_to_claude() {
    let env = setup_mixed_harness_env();

    cq_cmd(&env)
        .arg("sessions")
        .assert()
        .success()
        .stdout(predicate::str::contains("sess-001"))
        .stdout(predicate::str::contains("019a1b2c").not());

    cq_cmd(&env)
        .arg("messages")
        .assert()
        .success()
        .stdout(predicate::str::contains("Let me list the files."))
        .stdout(predicate::str::contains("The project has a Cargo manifest").not());

    cq_cmd(&env)
        .arg("tools")
        .assert()
        .success()
        .stdout(predicate::str::contains("Bash"))
        .stdout(predicate::str::contains("exec_command").not());

    cq_cmd(&env)
        .arg("hooks")
        .assert()
        .success()
        .stdout(predicate::str::contains("SessionStart"));

    cq_cmd(&env)
        .arg("projects")
        .assert()
        .success()
        .stdout(predicate::str::contains("myproject"))
        .stdout(predicate::str::contains("codex-project").not());

    let output = cq_cmd(&env).args(["--json", "sessions"]).output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        !rows.is_empty(),
        "expected Claude rows in JSON output, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        rows.iter()
            .all(|row| row["session_id"].as_str() == Some("sess-001")),
        "JSON output should contain only Claude rows: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn codex_runtime_scopes_to_codex_unless_all_is_requested() {
    let env = setup_env(&["simple_session.jsonl"]);
    let rollout_dir = env.codex_sessions.path().join("2026/08/26");
    std::fs::create_dir_all(&rollout_dir).unwrap();
    std::fs::copy(
        fixture_path("codex_session.jsonl"),
        rollout_dir.join("rollout-2026-08-26T14-00-00-session.jsonl"),
    )
    .unwrap();

    cq_cmd(&env)
        .env("CODEX_THREAD_ID", "test-codex-thread")
        .arg("sessions")
        .assert()
        .success()
        .stdout(predicate::str::contains("019a1b2c"))
        .stdout(predicate::str::contains("sess-001").not());

    cq_cmd(&env)
        .env("CODEX_THREAD_ID", "test-codex-thread")
        .args(["--json", "sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("019a1b2c"))
        .stdout(predicate::str::contains("sess-001").not());

    cq_cmd(&env)
        .env("CODEX_THREAD_ID", "test-codex-thread")
        .args(["--harness", "claude", "sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sess-001"))
        .stdout(predicate::str::contains("019a1b2c").not());

    cq_cmd(&env)
        .env("CODEX_THREAD_ID", "test-codex-thread")
        .args(["--all", "sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("019a1b2c"))
        .stdout(predicate::str::contains("sess-001"));
}

#[test]
fn harness_and_source_cannot_be_combined() {
    let env = setup_env(&[]);
    cq_cmd(&env)
        .args(["--harness", "codex", "--source", "main", "sessions"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn raw_sql_does_not_claim_automatic_scope() {
    let env = setup_mixed_harness_env();
    cq_cmd(&env)
        .env("CODEX_SESSION_ID", "test-codex-session")
        .args(["sql", "SELECT session_id FROM sessions ORDER BY session_id"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sess-001"))
        .stdout(predicate::str::contains("019a1b2c"))
        .stderr(predicate::str::contains("Scoped to harness").not());
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
fn sql_now_minus_interval_shows_timestamp_hint() {
    // `now() - INTERVAL N DAY` errors on the pinned DuckDB (TIMESTAMPTZ
    // arithmetic was tightened). cq should append a hint pointing at the fix.
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["sql", "SELECT now() - INTERVAL 2 DAY"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "Should exit 1 on SQL error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The hint is additive: the original DuckDB error must still be surfaced.
    assert!(
        stderr.contains("Error:") && stderr.contains("INTERVAL"),
        "Should still print the underlying DuckDB error, got: {stderr}"
    );
    assert!(
        stderr.contains("timestamp columns are VARCHAR ISO strings"),
        "Should hint about VARCHAR timestamp columns, got: {stderr}"
    );
    assert!(
        stderr.contains("now()::TIMESTAMP"),
        "Should suggest the cast fix, got: {stderr}"
    );
    // Must not steer raw-SQL users to --since, which cq sql ignores.
    assert!(
        !stderr.contains("Use --since"),
        "Hint should not tell raw-SQL users to use --since, got: {stderr}"
    );
}

#[test]
fn sql_varchar_timestamp_compare_shows_hint() {
    // Comparing a VARCHAR column against a TIMESTAMP literal is the other half
    // of the same gotcha.
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args([
            "sql",
            "SELECT * FROM sessions WHERE started_at >= TIMESTAMP '2026-01-01'",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "Should exit 1 on SQL error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error:") && stderr.contains("Cannot compare"),
        "Should still print the underlying DuckDB error, got: {stderr}"
    );
    assert!(
        stderr.contains("timestamp columns are VARCHAR ISO strings"),
        "Should hint about VARCHAR timestamp columns, got: {stderr}"
    );
}

#[test]
fn sql_unrelated_error_has_no_timestamp_hint() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["sql", "SELECT * FROM no_such_table"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "Should exit 1 on SQL error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Confirm we actually hit the SQL error path (not a vacuous pass).
    assert!(
        stderr.contains("does not exist"),
        "Expected a catalog error for the missing table, got: {stderr}"
    );
    assert!(
        !stderr.contains("timestamp columns are VARCHAR"),
        "Unrelated errors should not get the timestamp hint, got: {stderr}"
    );
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
    let env = TestEnv {
        projects,
        codex_sessions: TempDir::new().unwrap(),
        cache,
    };
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
    let output = cq_cmd(&env).arg("sessions").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Progress messages go to stderr
    assert!(
        stderr.contains("Synced") || stderr.contains("Loaded"),
        "Expected progress on stderr, got: {stderr}"
    );
    // stdout should not have progress messages
    assert!(
        !stdout.contains("Synced"),
        "Progress message leaked to stdout: {stdout}"
    );
    assert!(
        !stdout.contains("Loaded"),
        "Progress message leaked to stdout: {stdout}"
    );
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
    cmd.env(
        "CQ_CODEX_SESSIONS_DIR",
        env.cache.path().join("no-such-codex-sessions"),
    );
    cmd.env("CENV_BASE", env.cache.path().join("no-such-cenv-base"));
    // Explicitly unset NO_COLOR so we're testing the flag, not the env var
    cmd.env_remove("NO_COLOR");

    let output = cmd.args(["--no-color", "tools"]).output().unwrap();

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
    let output = cq_cmd(&env).args(["--json", "projects"]).output().unwrap();
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
    let output = cq_cmd(&env).args(["--json", "projects"]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let first = &parsed[0];
    let skills = first["skills"].as_array().unwrap();
    assert!(skills.iter().any(|s| s.as_str() == Some("sanitation")));
}

#[test]
fn help_shows_projects_command() {
    Command::cargo_bin("cq")
        .unwrap()
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
    assert!(
        stdout.contains("ls"),
        "Expected extracted command in output, got: {stdout}"
    );
    assert!(
        !stdout.contains("{\"command\""),
        "Should not contain raw JSON, got: {stdout}"
    );
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
    assert!(
        parsed[0].get("command").is_some(),
        "JSON should have 'command' field"
    );
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
    std::fs::copy(
        fixture_path("simple_session.jsonl"),
        project_a.join("sess-a.jsonl"),
    )
    .unwrap();
    std::fs::copy(
        fixture_path("multi_tool_session.jsonl"),
        project_b.join("sess-b.jsonl"),
    )
    .unwrap();

    let env = TestEnv {
        projects,
        codex_sessions: TempDir::new().unwrap(),
        cache,
    };

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
    assert!(
        stdout.contains("sess-001"),
        "Should show myproject session (sess-001), got: {stdout}"
    );
    assert!(
        !stdout.contains("sess-002"),
        "Should not show webapp session (sess-002), got: {stdout}"
    );
    assert!(
        stderr.contains("Scoped to"),
        "Should show scope notice, got: {stderr}"
    );

    // JSON changes the output format, not the inferred project scope.
    let output = cq_cmd(&env)
        .env("PWD", "/Users/test/myproject")
        .args(["--json", "sessions"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sess-001"),
        "JSON should show myproject session, got: {stdout}"
    );
    assert!(
        !stdout.contains("sess-002"),
        "JSON should not show webapp session, got: {stdout}"
    );
}

#[test]
fn auto_scope_hint_shows_path() {
    let projects = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let project_a = projects.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::copy(
        fixture_path("simple_session.jsonl"),
        project_a.join("sess-a.jsonl"),
    )
    .unwrap();

    let env = TestEnv {
        projects,
        codex_sessions: TempDir::new().unwrap(),
        cache,
    };

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
        .args([
            "--session",
            "00000000-0000-0000-0000-000000000000",
            "sessions",
        ])
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
    std::fs::copy(
        fixture_path("simple_session.jsonl"),
        project_a.join("sess-a.jsonl"),
    )
    .unwrap();
    std::fs::copy(
        fixture_path("multi_tool_session.jsonl"),
        project_b.join("sess-b.jsonl"),
    )
    .unwrap();

    let env = TestEnv {
        projects,
        codex_sessions: TempDir::new().unwrap(),
        cache,
    };

    // Run from "myproject" dir. projects should still show BOTH projects.
    let output = cq_cmd(&env)
        .env("PWD", "/Users/test/myproject")
        .arg("projects")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("myproject"),
        "Should show myproject, got: {stdout}"
    );
    assert!(
        stdout.contains("webapp"),
        "Should show webapp even when auto-scoped elsewhere, got: {stdout}"
    );
}

#[test]
fn all_flag_overrides_auto_scope() {
    let projects = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    let project_a = projects.path().join("-Users-test-myproject");
    let project_b = projects.path().join("-Users-test-webapp");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    std::fs::copy(
        fixture_path("simple_session.jsonl"),
        project_a.join("sess-a.jsonl"),
    )
    .unwrap();
    std::fs::copy(
        fixture_path("multi_tool_session.jsonl"),
        project_b.join("sess-b.jsonl"),
    )
    .unwrap();

    let env = TestEnv {
        projects,
        codex_sessions: TempDir::new().unwrap(),
        cache,
    };

    // Run with --all from myproject dir, should show sessions from BOTH projects
    let output = cq_cmd(&env)
        .env("PWD", "/Users/test/myproject")
        .args(["--all", "sessions"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Scoped to"),
        "Should not show scope notice with --all, got: {stderr}"
    );
    // Both sessions should appear
    assert!(
        stdout.contains("sess-001"),
        "Should show myproject session with --all, got: {stdout}"
    );
    assert!(
        stdout.contains("sess-002"),
        "Should show webapp session with --all, got: {stdout}"
    );
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
    let output = cq_cmd(&env).args(["tools", "Bash"]).output().unwrap();
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
    Command::cargo_bin("cq")
        .unwrap()
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
    let output = cq_cmd(&env).args(["--wide", "sessions"]).output().unwrap();
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
    let output = cq_cmd(&env).args(["--wide", "messages"]).output().unwrap();
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
        .args([
            "--wide",
            "sql",
            "SELECT name, CAST(input AS VARCHAR) AS input FROM tool_calls LIMIT 1",
        ])
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
    let output = Command::cargo_bin("cq")
        .unwrap()
        .args(["schema", "bogus"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "Should exit with failure for unknown view"
    );
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
    assert!(
        parsed[0].get("text").is_some(),
        "JSON should have 'text' field, got: {}",
        parsed[0]
    );
    assert!(
        parsed[0].get("type").is_some(),
        "JSON should have 'type' field, got: {}",
        parsed[0]
    );
    // Should NOT have fields that weren't requested
    assert!(
        parsed[0].get("session_id").is_none(),
        "JSON should not have 'session_id' when not requested, got: {}",
        parsed[0]
    );
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
    assert!(
        stdout.contains("Bash"),
        "Should show Bash tool name, got: {stdout}"
    );
    assert!(
        stdout.contains("\u{2588}"),
        "Should show bar chart blocks, got: {stdout}"
    );
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
    assert!(
        stdout.contains("sess-001"),
        "Should show session ID, got: {stdout}"
    );
    assert!(
        stdout.contains("sess-002"),
        "Should show session ID, got: {stdout}"
    );
    assert!(
        stdout.contains("\u{2588}"),
        "Should show bar chart blocks, got: {stdout}"
    );
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
    assert!(
        stdout.contains("sess-003"),
        "Should show error session, got: {stdout}"
    );
    // simple_session.jsonl sess-001 has no errors, should not appear
    assert!(
        !stdout.contains("sess-001"),
        "Should not show non-error session, got: {stdout}"
    );
}

#[test]
fn tools_grep_multiple_patterns_matches_any() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "--grep", "zzz-no-match", "--grep", "ls"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Bash"),
        "Second --grep pattern should still match, got: {stdout}"
    );
}

#[test]
fn tools_result_grep_filters_by_result_content() {
    let env = setup_env(&["simple_session.jsonl", "error_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "--result-grep", "E0308"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sess-003"),
        "Should show the session whose tool result matched, got: {stdout}"
    );
    assert!(
        !stdout.contains("sess-001"),
        "Should not show the session with no matching result, got: {stdout}"
    );
}

#[test]
fn tools_result_grep_ands_with_errors() {
    let env = setup_env(&["error_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "--errors", "--result-grep", "no-such-pattern"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No results"),
        "--errors and --result-grep should AND together, got stderr: {stderr}"
    );
}

#[test]
fn tools_result_grep_conflicts_with_context() {
    let env = setup_env(&["error_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "--result-grep", "E0308", "-C", "1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--result-grep"),
        "Should name the conflicting flag, got: {stderr}"
    );
}

#[test]
fn messages_grep_multiple_patterns_matches_any() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args([
            "messages",
            "--grep",
            "zzz-no-match",
            "--grep",
            "list the files",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("list the files"),
        "Second --grep pattern should still match, got: {stdout}"
    );
}

#[test]
fn sessions_grep_multiple_patterns_matches_any() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args([
            "sessions",
            "--grep",
            "zzz-no-match",
            "--grep",
            "list the files",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("list the files"),
        "Second --grep pattern should still match, got: {stdout}"
    );
}

#[test]
fn hooks_grep_multiple_patterns_matches_any() {
    let env = setup_env(&["hook_events_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["hooks", "--grep", "zzz-no-match", "--grep", "superpowers"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("You have superpowers."),
        "Second --grep pattern should still match, got: {stdout}"
    );
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
    assert!(
        stdout.contains("user"),
        "Should show user type, got: {stdout}"
    );
    assert!(
        stdout.contains("assistant"),
        "Should show assistant type, got: {stdout}"
    );
    assert!(
        stdout.contains("\u{2588}"),
        "Should show bar chart blocks, got: {stdout}"
    );
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
    assert!(
        parsed[0].get("name").is_some(),
        "JSON should have 'name' key, got: {}",
        parsed[0]
    );
    assert!(
        parsed[0].get("count").is_some(),
        "JSON should have 'count' key, got: {}",
        parsed[0]
    );
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
    std::fs::write(
        sub.join("agent-aaa.meta.json"),
        "{\"agentType\":\"general-purpose\"}",
    )
    .unwrap();

    // Workflow subagent + journal ledger (must be excluded).
    let wf = sub.join("workflows").join("wf_xyz");
    write_file(&wf.join("agent-bbb.jsonl"),
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m3\",\"role\":\"assistant\",\"model\":\"claude-haiku-4-5\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t3\",\"name\":\"Glob\",\"input\":{\"pattern\":\"*\"}}]},\"uuid\":\"a3\",\"parentUuid\":null,\"isSidechain\":true,\"agentId\":\"bbb\",\"timestamp\":\"2026-05-01T10:00:02.000Z\",\"sessionId\":\"11111111-1111-1111-1111-111111111111\",\"cwd\":\"/Users/test/myproject\"}\n");
    std::fs::write(
        wf.join("agent-bbb.meta.json"),
        "{\"agentType\":\"Explore\"}",
    )
    .unwrap();
    std::fs::write(
        wf.join("journal.jsonl"),
        "{\"type\":\"started\",\"key\":\"v2:abc\",\"agentId\":\"bbb\"}\n",
    )
    .unwrap();

    TestEnv {
        projects,
        codex_sessions: TempDir::new().unwrap(),
        cache: TempDir::new().unwrap(),
    }
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
        .args([
            "sql",
            "SELECT COUNT(*) FROM raw_records WHERE source_file LIKE '%journal.jsonl%'",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0"));
}

#[test]
fn workflow_id_visible_end_to_end() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args([
            "sql",
            "SELECT workflow_id FROM tool_calls WHERE name = 'Glob'",
        ])
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
    Command::cargo_bin("cq")
        .unwrap()
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
    Command::cargo_bin("cq")
        .unwrap()
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
        .args([
            "sessions",
            "--session",
            "aaaa0000-0000-0000-0000-000000000001",
            "--timeline",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("call"),
        "Should show 'call' events, got: {stdout}"
    );
    assert!(
        stdout.contains("result"),
        "Should show 'result' events, got: {stdout}"
    );
    assert!(
        stdout.contains("Read"),
        "Should show Read tool, got: {stdout}"
    );
    assert!(
        stdout.contains("Bash"),
        "Should show Bash tool, got: {stdout}"
    );
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
        .args([
            "sessions",
            "--session",
            "aaaa0000-0000-0000-0000-000000000001",
            "--timeline",
        ])
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
        .args([
            "--json",
            "sessions",
            "--session",
            "aaaa0000-0000-0000-0000-000000000001",
            "--timeline",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("Should be valid JSON, got: {stdout}"));
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
    let projects = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.env("CQ_PROJECTS_DIR", projects.path());
    cmd.env("CQ_CACHE_DIR", cache.path());
    cmd.env(
        "CQ_CODEX_SESSIONS_DIR",
        cache.path().join("no-such-codex-sessions"),
    );
    cmd.env("CENV_BASE", cache.path().join("no-such-cenv-base"));
    cmd.arg("--no-reindex")
        .arg("sessions")
        .arg("--limit")
        .arg("1");
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
    std::fs::copy(
        fixture_path("simple_session.jsonl"),
        project_a.join("sess-a.jsonl"),
    )
    .unwrap();
    std::fs::copy(
        fixture_path("multi_tool_session.jsonl"),
        project_b.join("sess-b.jsonl"),
    )
    .unwrap();

    let env = TestEnv {
        projects,
        codex_sessions: TempDir::new().unwrap(),
        cache,
    };
    let output = cq_cmd(&env)
        .args(["sessions", "--count-by", "project"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("myproject"),
        "Should show project name, got: {stdout}"
    );
    assert!(
        stdout.contains("\u{2588}"),
        "Should show bar chart blocks, got: {stdout}"
    );
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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let rows = parsed.as_array().unwrap();
    assert_eq!(
        rows.len(),
        3,
        "expected match + 2 after, got {}: {}",
        rows.len(),
        stdout
    );
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
    let match_session = rows
        .iter()
        .find(|r| r["match_kind"] == "match")
        .and_then(|r| r["session_id"].as_str())
        .unwrap()
        .to_string();
    for row in &rows {
        assert_eq!(
            row["session_id"].as_str().unwrap(),
            match_session,
            "cross-session leak: {row}"
        );
    }
}

#[test]
fn tools_with_context_c_shows_surrounding_messages() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "tools", "Read", "-C", "1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    // Read tool is at ord 4; -C 1 gives ords 3, 4, 5 -> 3 rows
    assert_eq!(rows.len(), 3);
    // Match row should carry the tool name.
    let match_row = rows.iter().find(|r| r["match_kind"] == "match").unwrap();
    assert_eq!(
        match_row["tool_name"], "Read",
        "match row should carry tool name, got: {match_row}"
    );
    // Context rows are message-shaped; tool_name should be null.
    let context_rows: Vec<_> = rows.iter().filter(|r| r["match_kind"] != "match").collect();
    assert_eq!(context_rows.len(), 2);
    for ctx_row in &context_rows {
        assert!(
            ctx_row["tool_name"].is_null(),
            "context row should not have tool_name, got: {ctx_row}"
        );
        assert!(
            ctx_row["type"].is_string(),
            "context row should be message-shaped, got: {ctx_row}"
        );
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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let match_count = rows.iter().filter(|r| r["match_kind"] == "match").count();
    assert_eq!(
        match_count, 1,
        "expected 1 match with --limit 1, rows: {stdout}"
    );
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
    assert!(
        output.status.success(),
        "non-JSON tools context path should not error, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    let non_blank_lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        non_blank_lines.len(),
        3,
        "expected 3 output rows, got:\n{stdout}"
    );

    // Each row's cells (split by the two-space delimiter print_context_rows uses) should not
    // include any bare `match_kind` or `match_group` column value.
    for line in &non_blank_lines {
        let cells: Vec<&str> = line
            .split("  ")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
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
    assert_eq!(
        separator_lines, 0,
        "single group should not have '--' separator, got:\n{stdout}"
    );
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
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let separator_lines = stdout.lines().filter(|l| l.trim() == "--").count();
    assert_eq!(
        separator_lines, 2,
        "three non-contiguous matches should have exactly 2 '--' separators, got:\n{stdout}"
    );
    // Also verify we got three data rows plus two separators (5 total non-blank lines).
    let non_blank: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        non_blank.len(),
        5,
        "expected 3 match rows + 2 separator lines, got:\n{stdout}"
    );
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

#[test]
fn populates_agent_type_from_meta() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args([
            "sql",
            "SELECT agent_type FROM tool_calls WHERE name = 'Read'",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("general-purpose"));
}

#[test]
fn agent_type_null_for_main_loop() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args([
            "sql",
            "SELECT COUNT(*) FROM tool_calls WHERE name = 'Bash' AND agent_type IS NULL",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1"));
}

// ---- multi-source output (Task 9: source column + per-source grouping/skills) ----

/// Build an env with a main source (CQ_PROJECTS_DIR) plus one cenv source
/// (CENV_BASE/<env_name>/projects). Both carry the SAME project path so we can
/// prove grouping by (source, project) and per-source skill correctness.
///
/// `main_jsonl` and `cenv_jsonl` are raw JSONL bodies written into a project dir
/// named to match their embedded cwd ("/Users/test/myproject").
struct MultiSourceEnv {
    projects: TempDir,
    cenv_base: TempDir,
    cache: TempDir,
    env_name: String,
}

fn setup_multi_source(env_name: &str, main_jsonl: &str, cenv_jsonl: &str) -> MultiSourceEnv {
    let projects = TempDir::new().unwrap();
    let cenv_base = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();

    // main source: CQ_PROJECTS_DIR/-Users-test-myproject/
    let main_proj = projects.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&main_proj).unwrap();
    std::fs::write(main_proj.join("main-sess.jsonl"), main_jsonl).unwrap();

    // cenv source: CENV_BASE/<env_name>/projects/-Users-test-myproject/
    let cenv_proj = cenv_base
        .path()
        .join(env_name)
        .join("projects")
        .join("-Users-test-myproject");
    std::fs::create_dir_all(&cenv_proj).unwrap();
    std::fs::write(cenv_proj.join("cenv-sess.jsonl"), cenv_jsonl).unwrap();

    MultiSourceEnv {
        projects,
        cenv_base,
        cache,
        env_name: env_name.to_string(),
    }
}

fn multi_cmd(env: &MultiSourceEnv) -> Command {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.env("CQ_PROJECTS_DIR", env.projects.path());
    cmd.env("CQ_CACHE_DIR", env.cache.path());
    cmd.env(
        "CQ_CODEX_SESSIONS_DIR",
        env.cache.path().join("no-such-codex-sessions"),
    );
    cmd.env("CENV_BASE", env.cenv_base.path());
    cmd.env_remove("CODEX_SESSION_ID");
    cmd.env_remove("CODEX_THREAD_ID");
    cmd.env("NO_COLOR", "1");
    cmd
}

// A user+assistant pair in /Users/test/myproject with one Skill call.
// Caller supplies a distinct sessionId and skill name.
fn session_with_skill(session_id: &str, skill: &str, tool_id: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"do a thing\"}},\"uuid\":\"u-{session_id}\",\"parentUuid\":null,\"isSidechain\":false,\"timestamp\":\"2026-04-13T10:00:00.000Z\",\"sessionId\":\"{session_id}\",\"cwd\":\"/Users/test/myproject\"}}\n\
         {{\"type\":\"assistant\",\"message\":{{\"id\":\"m-{session_id}\",\"role\":\"assistant\",\"model\":\"claude-opus-4-6\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{tool_id}\",\"name\":\"Skill\",\"input\":{{\"skill\":\"{skill}\"}}}}]}},\"uuid\":\"a-{session_id}\",\"parentUuid\":\"u-{session_id}\",\"isSidechain\":false,\"timestamp\":\"2026-04-13T10:00:05.000Z\",\"sessionId\":\"{session_id}\",\"cwd\":\"/Users/test/myproject\"}}\n"
    )
}

#[test]
fn projects_unscoped_groups_by_source() {
    // Same project path under two sources -> two rows when unscoped (--all).
    let main_body = session_with_skill(
        "11111111-1111-1111-1111-111111111111",
        "sanitation",
        "toolu_m1",
    );
    let cenv_body = session_with_skill(
        "22222222-2222-2222-2222-222222222222",
        "obsidian",
        "toolu_c1",
    );
    let env = setup_multi_source("pinwheel", &main_body, &cenv_body);

    let output = multi_cmd(&env)
        .args(["--json", "--all", "projects"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();

    // myproject appears under both 'main' and 'pinwheel': two rows.
    let myproject_rows: Vec<&serde_json::Value> = parsed
        .iter()
        .filter(|r| r["project"].as_str() == Some("/Users/test/myproject"))
        .collect();
    assert_eq!(
        myproject_rows.len(),
        2,
        "same project path under two sources should produce two rows, got: {stdout}"
    );
    let sources: std::collections::HashSet<&str> = myproject_rows
        .iter()
        .filter_map(|r| r["source"].as_str())
        .collect();
    assert!(
        sources.contains("main"),
        "expected a 'main' source row, got: {sources:?}"
    );
    assert!(
        sources.contains("pinwheel"),
        "expected a 'pinwheel' source row, got: {sources:?}"
    );
}

#[test]
fn projects_scoped_groups_by_project_only() {
    // Scoped to one source -> a single row for the project (no per-source split).
    let main_body = session_with_skill(
        "11111111-1111-1111-1111-111111111111",
        "sanitation",
        "toolu_m1",
    );
    let cenv_body = session_with_skill(
        "22222222-2222-2222-2222-222222222222",
        "obsidian",
        "toolu_c1",
    );
    let env = setup_multi_source("pinwheel", &main_body, &cenv_body);

    let output = multi_cmd(&env)
        .args(["--json", "--source", "main", "projects"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();

    let myproject_rows: Vec<&serde_json::Value> = parsed
        .iter()
        .filter(|r| r["project"].as_str() == Some("/Users/test/myproject"))
        .collect();
    assert_eq!(
        myproject_rows.len(),
        1,
        "scoped to one source the project should be a single row, got: {stdout}"
    );
    assert_eq!(myproject_rows[0]["source"].as_str(), Some("main"));
}

#[test]
fn json_preserves_automatic_source_scope() {
    let main_body = session_with_skill(
        "11111111-1111-1111-1111-111111111111",
        "sanitation",
        "toolu_m1",
    );
    let cenv_body = session_with_skill(
        "22222222-2222-2222-2222-222222222222",
        "obsidian",
        "toolu_c1",
    );
    let env = setup_multi_source("pinwheel", &main_body, &cenv_body);

    let output = multi_cmd(&env)
        .env(
            "CLAUDE_CONFIG_DIR",
            env.cenv_base.path().join(&env.env_name),
        )
        .args(["--json", "sessions"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 1, "expected one active-source row: {rows:?}");
    assert_eq!(rows[0]["source"].as_str(), Some("pinwheel"));
}

#[test]
fn projects_skill_count_is_per_source() {
    // Each source has a distinct Skill call for the same project path.
    // --source main must reflect only main's skill, not the cenv source's.
    let main_body = session_with_skill(
        "11111111-1111-1111-1111-111111111111",
        "sanitation",
        "toolu_m1",
    );
    let cenv_body = session_with_skill(
        "22222222-2222-2222-2222-222222222222",
        "obsidian",
        "toolu_c1",
    );
    let env = setup_multi_source("pinwheel", &main_body, &cenv_body);

    // Scoped to main: skills = ["sanitation"], count 1 (NOT 2, which would mean
    // it counted the cenv source's skill too).
    let output = multi_cmd(&env)
        .args(["--json", "--source", "main", "projects"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let row = parsed
        .iter()
        .find(|r| r["project"].as_str() == Some("/Users/test/myproject"))
        .expect("myproject row");
    assert_eq!(
        row["skill_count"].as_i64(),
        Some(1),
        "main should count only its own skill, got: {stdout}"
    );
    let skills: Vec<&str> = row["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s.as_str())
        .collect();
    assert_eq!(
        skills,
        vec!["sanitation"],
        "main should list only its own skill, got: {skills:?}"
    );

    // Scoped to the cenv source: skills = ["obsidian"].
    let output = multi_cmd(&env)
        .args(["--json", "--source", &env.env_name, "projects"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let row = parsed
        .iter()
        .find(|r| r["project"].as_str() == Some("/Users/test/myproject"))
        .expect("myproject row");
    let skills: Vec<&str> = row["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s.as_str())
        .collect();
    assert_eq!(
        skills,
        vec!["obsidian"],
        "cenv source should list only its own skill, got: {skills:?}"
    );
}

#[test]
fn projects_display_skill_count_per_source() {
    // Default (non-JSON) display: per-source skill counts must not bleed across
    // sources. Each source has exactly one distinct skill for the same project.
    let main_body = session_with_skill(
        "11111111-1111-1111-1111-111111111111",
        "sanitation",
        "toolu_m1",
    );
    let cenv_body = session_with_skill(
        "22222222-2222-2222-2222-222222222222",
        "obsidian",
        "toolu_c1",
    );
    let env = setup_multi_source("pinwheel", &main_body, &cenv_body);

    let output = multi_cmd(&env)
        .args(["--source", "main", "projects", "--skills"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // main's skill appears; the cenv source's skill must NOT.
    assert!(
        stdout.contains("sanitation"),
        "main's skill should show, got: {stdout}"
    );
    assert!(
        !stdout.contains("obsidian"),
        "cenv source's skill must not leak into main, got: {stdout}"
    );
    assert!(
        stdout.contains("1 skills"),
        "main should count exactly 1 skill, got: {stdout}"
    );
}

#[test]
fn sessions_source_column_by_flag() {
    let main_body = session_with_skill(
        "11111111-1111-1111-1111-111111111111",
        "sanitation",
        "toolu_m1",
    );
    let cenv_body = session_with_skill(
        "22222222-2222-2222-2222-222222222222",
        "obsidian",
        "toolu_c1",
    );
    let env = setup_multi_source("pinwheel", &main_body, &cenv_body);

    // --all (unscoped): SOURCE column present in table output.
    let output = multi_cmd(&env)
        .args(["--table", "--all", "sessions"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("source"),
        "SOURCE column should appear under --all, got: {stdout}"
    );
    assert!(
        stdout.contains("main"),
        "main source value should appear, got: {stdout}"
    );
    assert!(
        stdout.contains("pinwheel"),
        "pinwheel source value should appear, got: {stdout}"
    );

    // --source main: SOURCE column omitted (redundant).
    let output = multi_cmd(&env)
        .args(["--table", "--source", "main", "sessions"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Header row is the column names; "source" must not be a header.
    let header_line = stdout
        .lines()
        .find(|l| l.contains("project") && l.contains("session"))
        .unwrap_or("");
    assert!(
        !header_line.contains("source"),
        "SOURCE column should be omitted when scoped, header: {header_line}"
    );
}

#[test]
fn sessions_json_always_includes_source() {
    let main_body = session_with_skill(
        "11111111-1111-1111-1111-111111111111",
        "sanitation",
        "toolu_m1",
    );
    let cenv_body = session_with_skill(
        "22222222-2222-2222-2222-222222222222",
        "obsidian",
        "toolu_c1",
    );
    let env = setup_multi_source("pinwheel", &main_body, &cenv_body);

    // Even scoped to one source, JSON carries the source field.
    let output = multi_cmd(&env)
        .args(["--json", "--source", "main", "sessions"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert!(
        !parsed.is_empty(),
        "expected at least one session, got: {stdout}"
    );
    assert_eq!(
        parsed[0]["source"].as_str(),
        Some("main"),
        "JSON should carry source, got: {stdout}"
    );
}

// ---- cq hooks ----

#[test]
fn hooks_summary_mode_shows_bar_chart() {
    let env = setup_env(&["hook_events_session.jsonl"]);
    cq_cmd(&env)
        .arg("hooks")
        .assert()
        .success()
        .stdout(predicate::str::contains("SessionStart"))
        .stdout(predicate::str::contains("\u{2588}")); // bar chart block char
}

#[test]
fn hooks_filter_by_event() {
    let env = setup_env(&["hook_events_session.jsonl"]);
    cq_cmd(&env)
        .args(["hooks", "PreToolUse"])
        .assert()
        .success()
        .stdout(predicate::str::contains("permissionDecision"));
}

#[test]
fn hooks_grep_filters_content() {
    let env = setup_env(&["hook_events_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["hooks", "--grep", "superpowers"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("You have superpowers."),
        "Expected matching content in output, got: {stdout}"
    );
    assert!(
        !stdout.contains("LSP context"),
        "Should not show non-matching content, got: {stdout}"
    );
}

#[test]
fn hooks_count_by_hook_name() {
    let env = setup_env(&["hook_events_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["hooks", "--count-by", "hook_name"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("SessionStart:startup"),
        "Should show SessionStart:startup, got: {stdout}"
    );
    assert!(
        stdout.contains("PreToolUse:Bash"),
        "Should show PreToolUse:Bash, got: {stdout}"
    );
    assert!(
        stdout.contains("\u{2588}"),
        "Should show bar chart blocks, got: {stdout}"
    );
}

#[test]
fn hooks_json_output_full_columns() {
    let env = setup_env(&["hook_events_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "hooks", "SessionStart"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert!(!parsed.is_empty());
    let first = &parsed[0];
    for field in [
        "session_id",
        "project",
        "source",
        "harness",
        "timestamp",
        "hook_event",
        "hook_name",
        "attachment_type",
        "content",
        "content_size",
    ] {
        assert!(
            first.get(field).is_some(),
            "Expected field '{field}' in JSON output, got: {first}"
        );
    }
}

#[test]
fn hooks_no_results_message() {
    let env = setup_env(&["hook_events_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["hooks", "--grep", "zzznotfound"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No results"),
        "Should show no results, got: {stderr}"
    );
}

#[test]
fn tty_context_curates_to_four_columns() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["messages", "--grep", "NEEDLE", "-C", "1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    let data_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty() && l.trim() != "--")
        .collect();
    assert!(
        !data_lines.is_empty(),
        "expected at least one data row, got:\n{stdout}"
    );

    for line in &data_lines {
        let cells: Vec<&str> = line
            .split("  ")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            cells.len(),
            4,
            "expected 4 curated columns (session_id, type, timestamp, text), got {} in line:\n{line}",
            cells.len()
        );
    }
}

#[test]
fn table_context_hides_match_kind_and_group_columns() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--table", "messages", "--grep", "NEEDLE", "-C", "1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("session_id") && stdout.contains("timestamp") && stdout.contains("text"),
        "expected curated header columns, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("match_kind") && !stdout.contains("match_group"),
        "table context output should not expose match_kind/match_group column names, got:\n{stdout}"
    );

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "--" {
            continue;
        }
        let cells: Vec<&str> = trimmed
            .split("  ")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        for cell in &cells {
            assert!(
                !matches!(*cell, "match" | "before" | "after"),
                "table output should not include bare match_kind column value '{cell}' in line:\n{line}"
            );
        }
    }
}

#[test]
fn table_context_shows_separators_on_group_boundaries() {
    // Grep "ne" matches "one" (ord 1), "six NEEDLE" (ord 6), and "nine" (ord 9) in the fixture.
    // With -A 0 -B 0, these form three non-contiguous groups separated by `--` separators,
    // ported from the TTY equivalent (`tty_context_non_contiguous_groups_show_separator`).
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--table", "messages", "--grep", "ne", "-A", "0", "-B", "0"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let separator_lines = stdout.lines().filter(|l| l.trim() == "--").count();
    assert_eq!(
        separator_lines, 2,
        "three non-contiguous matches should have exactly 2 '--' separators, got:\n{stdout}"
    );
}

#[test]
fn table_context_single_group_no_separator() {
    // One match, -A 0 -B 0 -- one group, no `--` separator line.
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args([
            "--table", "messages", "--grep", "NEEDLE", "-A", "0", "-B", "0",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let separator_lines = stdout.lines().filter(|l| l.trim() == "--").count();
    assert_eq!(
        separator_lines, 0,
        "single group should not have '--' separator, got:\n{stdout}"
    );
}
