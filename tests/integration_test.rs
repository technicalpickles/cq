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
fn tools_summary() {
    let env = setup_env(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&env)
        .arg("tools")
        .assert()
        .success()
        .stdout(predicate::str::contains("Bash"));
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
fn sql_raw_query() {
    let env = setup_env(&["simple_session.jsonl"]);
    cq_cmd(&env)
        .args(["sql", "SELECT count(*) AS n FROM tool_calls"])
        .assert()
        .success();
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
    // Progress messages go to stderr
    assert!(
        stderr.contains("Indexed") || stderr.contains("Cache up to date"),
        "Expected progress on stderr, got: {stderr}"
    );
    // stdout should not have progress messages
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
