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
            source TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )",
    )
    .unwrap();

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
            source TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )",
    )
    .unwrap();

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
        .query_row("SELECT DISTINCT session_id FROM messages", [], |r| r.get(0))
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
        .query_row("SELECT tool_use_id FROM tool_calls LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(id, "toolu_001");
}

#[test]
fn tool_calls_has_message_uuid() {
    let conn = setup_db("simple_session.jsonl");
    let uuid: String = conn
        .query_row("SELECT message_uuid FROM tool_calls LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(uuid, "a1");
}

// ---- advisor() tool calls ----
// advisor() invocations use server_tool_use / advisor_tool_result content blocks
// instead of the standard tool_use / tool_result pair, and the result block lives
// in an assistant-type record rather than a user-type one.

#[test]
fn tool_calls_finds_advisor_server_tool_use() {
    let conn = setup_db("advisor_session.jsonl");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE name = 'advisor'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn sessions_tool_call_count_includes_advisor() {
    let conn = setup_db("advisor_session.jsonl");
    let tool_count: i64 = conn
        .query_row(
            "SELECT tool_count FROM messages WHERE uuid = 'a1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tool_count, 1, "advisor() call should count in tool_count");

    let session_total: i64 = conn
        .query_row("SELECT tool_call_count FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        session_total, 1,
        "sessions.tool_call_count should agree with tool_calls' row count"
    );
}

#[test]
fn tool_results_finds_advisor_tool_result() {
    let conn = setup_db("advisor_session.jsonl");
    let content: String = conn
        .query_row(
            "SELECT content FROM tool_results WHERE tool_use_id = 'srvtoolu_adv001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(content, "Looks solid. Ship it.");
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
        .query_row("SELECT tool_use_id FROM tool_results LIMIT 1", [], |r| {
            r.get(0)
        })
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
    assert_eq!(
        count, 2,
        "mixed_types.jsonl has 6 records but only 2 are user/assistant"
    );
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
        .query_row("SELECT COUNT(DISTINCT session_id) FROM sessions", [], |r| {
            r.get(0)
        })
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
    assert_eq!(
        count, 5,
        "all main-loop and sidechain messages are queryable"
    );

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

#[test]
fn sessions_counts_exclude_sidechains() {
    let conn = setup_db("mixed_sidechain_session.jsonl");
    let (msgs, tools, users, subs): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT message_count, tool_call_count, user_message_count, subagent_count
             FROM sessions WHERE session_id = 'sess-mix'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(msgs, 2, "only main-loop messages counted");
    assert_eq!(tools, 1, "only the main-loop Task call counted");
    assert_eq!(users, 1, "only the main-loop user turn counted");
    assert_eq!(subs, 1, "one distinct subagent (agentAAA)");

    let first: String = conn
        .query_row(
            "SELECT first_user_message FROM sessions WHERE session_id = 'sess-mix'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(first, "run the analysis");
}

// ---- workflow_id extraction ----

#[test]
fn workflow_id_extracted_from_path() {
    let conn = setup_db("subagents/workflows/wf_testrun/agent-wf1.jsonl");
    let wf: Option<String> = conn
        .query_row(
            "SELECT workflow_id FROM tool_calls WHERE name = 'Glob'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(wf.as_deref(), Some("wf_testrun"));
}

#[test]
fn workflow_id_null_for_non_workflow() {
    let conn = setup_db("multi_tool_session.jsonl");
    let nulls: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE workflow_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nulls, 0);
}

// ---- agent_type propagation ----

#[test]
fn agent_type_flows_from_registry() {
    use std::path::PathBuf;
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT,
            file_size BIGINT,
            cwd TEXT,
            agent_type TEXT,
            source TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )",
    )
    .unwrap();

    let wf_path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("subagents/workflows/wf_testrun/agent-wf1.jsonl");
    let wf_str = wf_path.display().to_string();

    // Register the subagent's agent_type as the indexer would.
    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type)
         VALUES (?, 0, 0, NULL, 'Explore')",
        [&wf_str],
    )
    .unwrap();

    cq::views::register_views(&conn, &[wf_path]).unwrap();

    let at: Option<String> = conn
        .query_row(
            "SELECT agent_type FROM tool_calls WHERE name = 'Glob'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(at.as_deref(), Some("Explore"));
}

// ---- cross-cwd subagent de-duplication ----

#[test]
fn sessions_single_row_across_cwds() {
    use std::path::PathBuf;
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT,
            file_size BIGINT,
            cwd TEXT,
            agent_type TEXT,
            source TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )",
    )
    .unwrap();

    let main_path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("simple_session.jsonl");
    let sub_path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("subagent_other_cwd.jsonl");

    // Main session file's home is /Users/test/myproject; the subagent ran in /Users/other/repo.
    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type)
         VALUES (?, 0, 0, '/Users/test/myproject', NULL)",
        [&main_path.display().to_string()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type)
         VALUES (?, 0, 0, '/Users/other/repo', 'Explore')",
        [&sub_path.display().to_string()],
    )
    .unwrap();

    cq::views::register_views(&conn, &[main_path, sub_path]).unwrap();

    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE session_id = 'sess-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        rows, 1,
        "one row per session even when subagents ran in another cwd"
    );

    let (project, subs): (String, i64) = conn
        .query_row(
            "SELECT project, subagent_count FROM sessions WHERE session_id = 'sess-001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        project, "/Users/test/myproject",
        "project is the main-loop home, not the subagent cwd"
    );
    assert_eq!(subs, 1, "the other-cwd subagent is still counted");
}

#[test]
fn sessions_filter_by_source() {
    use std::path::PathBuf;
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT,
            file_size BIGINT,
            cwd TEXT,
            agent_type TEXT,
            source TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )",
    )
    .unwrap();

    // Two distinct sessions, each tagged with a different source.
    let main_path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("simple_session.jsonl");
    let pinwheel_path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("multi_tool_session.jsonl");

    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type, source)
         VALUES (?, 0, 0, '/Users/test/myproject', NULL, 'main')",
        [&main_path.display().to_string()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type, source)
         VALUES (?, 0, 0, '/Users/test/pinwheel', NULL, 'pinwheel')",
        [&pinwheel_path.display().to_string()],
    )
    .unwrap();

    cq::views::register_views(&conn, &[main_path, pinwheel_path]).unwrap();

    // Both sources are present across all sessions.
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 2, "both sessions are indexed");

    // The source = ? predicate (as built by the command scope WHERE sites) restricts to one.
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE source = ?",
            duckdb::params!["pinwheel"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 1,
        "source filter restricts sessions to the matching source"
    );

    let main_n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE source = ?",
            duckdb::params!["main"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(main_n, 1, "the other source is excluded by the filter");
}

// ---- tool_calls source filtering (summary mode) ----

#[test]
fn tool_calls_filter_by_source() {
    use std::path::PathBuf;
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT,
            file_size BIGINT,
            cwd TEXT,
            agent_type TEXT,
            source TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )",
    )
    .unwrap();

    // simple_session.jsonl has 1 tool call; multi_tool_session.jsonl has 4.
    // Tag them with different sources to verify source filtering on tool_calls.
    let main_path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("simple_session.jsonl");
    let pinwheel_path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("multi_tool_session.jsonl");

    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type, source)
         VALUES (?, 0, 0, '/Users/test/myproject', NULL, 'main')",
        [&main_path.display().to_string()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type, source)
         VALUES (?, 0, 0, '/Users/test/pinwheel', NULL, 'pinwheel')",
        [&pinwheel_path.display().to_string()],
    )
    .unwrap();

    cq::views::register_views(&conn, &[main_path, pinwheel_path]).unwrap();

    // Baseline: all tool calls present across both sources.
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        total, 5,
        "1 from simple_session + 4 from multi_tool_session"
    );

    // Source filter restricts to pinwheel's 4 tool calls.
    let pinwheel_n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE source = ?",
            duckdb::params!["pinwheel"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        pinwheel_n, 4,
        "source='pinwheel' returns only pinwheel tool calls"
    );

    // Source filter restricts to main's 1 tool call.
    let main_n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE source = ?",
            duckdb::params!["main"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(main_n, 1, "source='main' returns only main tool calls");

    // Nonexistent source returns 0 (matches run_summary behavior with --source nonexistent).
    let none_n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE source = ?",
            duckdb::params!["nonexistent"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(none_n, 0, "nonexistent source returns no tool calls");
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

#[test]
fn views_expose_claude_harness() {
    let conn = setup_db("simple_session.jsonl");
    for view in ["messages", "tool_calls", "tool_results", "sessions"] {
        // Every non-empty view's rows are tagged harness='claude'.
        let distinct: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM (SELECT DISTINCT harness FROM {view} WHERE harness IS NOT NULL)"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            distinct <= 1,
            "{view} should have at most one harness value"
        );
        let claude: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {view} WHERE harness = 'claude'"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        let total: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {view}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(claude, total, "all {view} rows should be harness='claude'");
    }
}

#[test]
fn empty_views_have_harness_column() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY, mtime_ns BIGINT, file_size BIGINT,
            cwd TEXT, agent_type TEXT, source TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )",
    )
    .unwrap();
    cq::views::register_views(&conn, &[]).unwrap();
    // Selecting harness from each empty view must not error (column exists).
    for view in ["messages", "tool_calls", "tool_results", "sessions"] {
        let n: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {view} WHERE harness IS NULL"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "{view} should be empty");
    }
}
