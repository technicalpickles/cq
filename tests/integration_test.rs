use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
use tempfile::TempDir;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Set up a temp directory that mimics the Claude projects structure.
/// Copies fixture files into a project subdirectory.
fn setup_fake_projects(fixtures: &[&str]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_dir).unwrap();

    for fixture in fixtures {
        let src = fixture_path(fixture);
        let dest = project_dir.join(fixture);
        std::fs::copy(&src, &dest).unwrap();
    }

    tmp
}

fn cq_cmd(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("cq").unwrap();
    cmd.env("CQ_PROJECTS_DIR", tmp.path());
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
    let tmp = setup_fake_projects(&["simple_session.jsonl", "multi_tool_session.jsonl"]);
    cq_cmd(&tmp)
        .arg("tools")
        .assert()
        .success()
        .stdout(predicate::str::contains("Bash"));
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
    let output = cq_cmd(&tmp).arg("tools").output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Scanned"), "should not print scan message with 0 files");
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
fn progress_on_stderr_not_stdout() {
    let tmp = setup_fake_projects(&["simple_session.jsonl"]);
    let output = cq_cmd(&tmp)
        .arg("sessions")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Progress messages go to stderr
    assert!(stderr.contains("Scanned"), "Expected 'Scanned' on stderr, got: {stderr}");
    // stdout should not have progress messages
    assert!(!stdout.contains("Scanned"), "Progress message leaked to stdout: {stdout}");
}

#[test]
fn messages_command() {
    let tmp = setup_fake_projects(&["simple_session.jsonl"]);
    cq_cmd(&tmp)
        .arg("messages")
        .assert()
        .success()
        .stdout(predicate::str::contains("list the files"));
}

#[test]
fn project_filter() {
    let tmp = setup_fake_projects(&["simple_session.jsonl"]);
    cq_cmd(&tmp)
        .args(["--project", "myproject", "sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sess-001"));
}
