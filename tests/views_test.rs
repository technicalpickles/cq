use duckdb::Connection;
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn setup_db(fixture: &str) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    let path = fixture_path(fixture);

    // Create file_registry for PROJECT_EXPR cwd lookup (empty = uses fallback decode)
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT,
            file_size BIGINT,
            cwd TEXT,
            agent_type TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )"
    ).unwrap();

    cq::views::register_views(&conn, &[path]).unwrap();
    conn
}

fn setup_db_multi(fixtures: &[&str]) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    let paths: Vec<PathBuf> = fixtures.iter().map(|f| fixture_path(f)).collect();

    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT,
            file_size BIGINT,
            cwd TEXT,
            agent_type TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )"
    ).unwrap();

    cq::views::register_views(&conn, &paths).unwrap();
    conn
}

// ---- messages view ----

#[test]
fn messages_view_returns_user_and_assistant() {
    let conn = setup_db("simple_session.jsonl");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 4);
}

#[test]
fn messages_view_extracts_text() {
    let conn = setup_db("simple_session.jsonl");
    let text: String = conn
        .query_row(
            "SELECT text FROM messages WHERE type = 'user' ORDER BY timestamp LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(text, "list the files");
}

#[test]
fn messages_view_extracts_assistant_text() {
    let conn = setup_db("simple_session.jsonl");
    let text: String = conn
        .query_row(
            "SELECT text FROM messages WHERE type = 'assistant' ORDER BY timestamp LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(text, "Let me list the files.");
}

#[test]
fn messages_view_tool_count() {
    let conn = setup_db("simple_session.jsonl");
    // First assistant message has 1 tool_use (Bash)
    let tool_count: i64 = conn
        .query_row(
            "SELECT tool_count FROM messages WHERE uuid = 'a1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tool_count, 1);

    // Second assistant message has 0 tool_use (text only)
    let tool_count: i64 = conn
        .query_row(
            "SELECT tool_count FROM messages WHERE uuid = 'a2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tool_count, 0);
}

#[test]
fn messages_view_model() {
    let conn = setup_db("simple_session.jsonl");
    let model: String = conn
        .query_row(
            "SELECT model FROM messages WHERE type = 'assistant' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(model, "claude-sonnet-4-20250514");
}

#[test]
fn messages_view_session_id() {
    let conn = setup_db("simple_session.jsonl");
    let session_id: String = conn
        .query_row(
            "SELECT DISTINCT session_id FROM messages",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(session_id, "sess-001");
}

// ---- tool_calls view ----

#[test]
fn tool_calls_view_finds_bash() {
    let conn = setup_db("simple_session.jsonl");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE name = 'Bash'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn tool_calls_view_multiple_tools() {
    let conn = setup_db("multi_tool_session.jsonl");
    let mut stmt = conn
        .prepare("SELECT DISTINCT name FROM tool_calls ORDER BY name")
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(names, vec!["Bash", "Grep", "Read", "Skill"]);
}

#[test]
fn tool_calls_input_queryable() {
    let conn = setup_db("multi_tool_session.jsonl");
    let cmd: String = conn
        .query_row(
            "SELECT json_extract_string(input, '$.command') FROM tool_calls WHERE name = 'Bash' ORDER BY timestamp LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cmd, "docker ps");
}

#[test]
fn tool_calls_has_tool_use_id() {
    let conn = setup_db("simple_session.jsonl");
    let id: String = conn
        .query_row(
            "SELECT tool_use_id FROM tool_calls LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(id, "toolu_001");
}

#[test]
fn tool_calls_has_message_uuid() {
    let conn = setup_db("simple_session.jsonl");
    let uuid: String = conn
        .query_row(
            "SELECT message_uuid FROM tool_calls LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(uuid, "a1");
}

// ---- tool_results view ----

#[test]
fn tool_results_view_finds_errors() {
    let conn = setup_db("error_session.jsonl");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_results WHERE is_error = true",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn tool_results_error_content() {
    let conn = setup_db("error_session.jsonl");
    let content: String = conn
        .query_row(
            "SELECT content FROM tool_results WHERE is_error = true",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(content.contains("mismatched types"));
}

#[test]
fn tool_results_non_error() {
    let conn = setup_db("simple_session.jsonl");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_results WHERE is_error = false",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "simple_session has 1 non-error tool result");
}

#[test]
fn tool_results_has_tool_use_id() {
    let conn = setup_db("simple_session.jsonl");
    let id: String = conn
        .query_row(
            "SELECT tool_use_id FROM tool_results LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(id, "toolu_001");
}

// ---- mixed types filtering ----

#[test]
fn mixed_types_filtered_from_messages() {
    let conn = setup_db("mixed_types.jsonl");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "mixed_types.jsonl has 6 records but only 2 are user/assistant");
}

// ---- sessions view ----

#[test]
fn sessions_view_aggregates() {
    let conn = setup_db("simple_session.jsonl");

    let session_id: String = conn
        .query_row("SELECT session_id FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(session_id, "sess-001");

    let msg_count: i64 = conn
        .query_row("SELECT message_count FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(msg_count, 4);

    let tool_count: i64 = conn
        .query_row("SELECT tool_call_count FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tool_count, 1);

    let user_count: i64 = conn
        .query_row("SELECT user_message_count FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(user_count, 2);

    let first_msg: String = conn
        .query_row("SELECT first_user_message FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(first_msg, "list the files");
}

#[test]
fn sessions_view_timestamps() {
    let conn = setup_db("simple_session.jsonl");
    let started: String = conn
        .query_row("SELECT started_at FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(started, "2026-04-13T10:00:00.000Z");

    let ended: String = conn
        .query_row("SELECT ended_at FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ended, "2026-04-13T10:00:07.000Z");
}

// ---- multi-file support ----

#[test]
fn multi_file_sessions() {
    let conn = setup_db_multi(&["simple_session.jsonl", "error_session.jsonl"]);
    let count: i64 = conn
        .query_row("SELECT COUNT(DISTINCT session_id) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "two files should produce two sessions");
}

// ---- subagent tagging ----

#[test]
fn messages_tag_sidechain_rows() {
    let conn = setup_db("mixed_sidechain_session.jsonl");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 5, "all main-loop and sidechain messages are queryable");

    let (is_side, agent): (bool, Option<String>) = conn
        .query_row(
            "SELECT is_sidechain, agent_id FROM messages WHERE uuid = 'su1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(is_side);
    assert_eq!(agent.as_deref(), Some("agentAAA"));

    let (is_side, agent): (bool, Option<String>) = conn
        .query_row(
            "SELECT is_sidechain, agent_id FROM messages WHERE uuid = 'mu1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(!is_side);
    assert_eq!(agent, None);
}

#[test]
fn tool_calls_tag_sidechain_rows() {
    let conn = setup_db("mixed_sidechain_session.jsonl");
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 3, "Task + Read + Grep");

    let main_only: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE NOT is_sidechain",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(main_only, 1, "only the Task call is main-loop");

    let agent: Option<String> = conn
        .query_row(
            "SELECT agent_id FROM tool_calls WHERE name = 'Read'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(agent.as_deref(), Some("agentAAA"));
}

#[test]
fn tool_results_tag_sidechain_rows() {
    let conn = setup_db("mixed_sidechain_session.jsonl");
    let is_side: bool = conn
        .query_row(
            "SELECT is_sidechain FROM tool_results WHERE tool_use_id = 'toolu_s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(is_side);
}

// ---- empty files ----

#[test]
fn empty_files_no_error() {
    let conn = Connection::open_in_memory().unwrap();
    cq::views::register_views(&conn, &[]).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tool_results", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
