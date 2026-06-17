//! Prototype integration test: read opencode's SQLite DB via DuckDB, emit cq-shaped output.
//!
//! Run:
//!   cargo test opencode_spike -- --nocapture --include-ignored
//!
//! Requires ~/.local/share/opencode/opencode.db (ignored if absent).
//! Also requires the DuckDB sqlite extension (downloaded on first run; needs network).

use duckdb::Connection;

fn opencode_db_path() -> std::path::PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".local/share/opencode/opencode.db")
}

fn setup_opencode_connection() -> Connection {
    let db_path = opencode_db_path();
    let conn = Connection::open_in_memory().expect("open in-memory DuckDB");

    conn.execute_batch("INSTALL sqlite; LOAD sqlite;")
        .expect("install/load sqlite extension");

    conn.execute_batch(&format!(
        "ATTACH '{}' AS oc (TYPE sqlite, READ_ONLY);",
        db_path.display()
    ))
    .expect("attach opencode.db");

    conn
}

fn create_views(conn: &Connection) {
    // sessions: one row per top-level session
    conn.execute_batch(
        "CREATE OR REPLACE VIEW oc_sessions AS
        SELECT
            s.id                                    AS session_id,
            s.directory                             AS project,
            'opencode'                              AS source,
            epoch_ms(s.time_created)::VARCHAR       AS started_at,
            epoch_ms(s.time_updated)::VARCHAR       AS ended_at,
            (SELECT COUNT(*) FROM oc.message m WHERE m.session_id = s.id) AS message_count,
            (SELECT COUNT(*) FROM oc.part p
             WHERE p.session_id = s.id
               AND json_extract_string(p.data, '$.type') = 'tool') AS tool_call_count,
            (SELECT COUNT(*) FROM oc.message m
             WHERE m.session_id = s.id
               AND json_extract_string(m.data, '$.role') = 'user') AS user_message_count,
            (SELECT COUNT(*) FROM oc.session sub WHERE sub.parent_id = s.id) AS subagent_count,
            s.title AS first_user_message
        FROM oc.session s
        WHERE s.parent_id IS NULL
        ORDER BY s.time_created DESC",
    ).expect("create oc_sessions");

    // messages: one row per message
    conn.execute_batch(
        "CREATE OR REPLACE VIEW oc_messages AS
        SELECT
            m.session_id,
            s.directory                                         AS project,
            'opencode'                                          AS source,
            m.id                                               AS uuid,
            json_extract_string(m.data, '$.parentID')          AS parent_uuid,
            json_extract_string(m.data, '$.role')              AS type,
            epoch_ms(m.time_created)::VARCHAR                   AS timestamp,
            (SELECT json_extract_string(p.data, '$.text')
             FROM oc.part p
             WHERE p.message_id = m.id
               AND json_extract_string(p.data, '$.type') = 'text'
             LIMIT 1)                                           AS text,
            (SELECT COUNT(*) FROM oc.part p
             WHERE p.message_id = m.id
               AND json_extract_string(p.data, '$.type') = 'tool') AS tool_count,
            COALESCE(
                json_extract_string(m.data, '$.modelID'),
                json_extract_string(m.data, '$.model.modelID')
            )                                                   AS model,
            NULL::VARCHAR                                       AS agent_id,
            false                                               AS is_sidechain,
            json_extract_string(m.data, '$.agent')             AS agent_type,
            NULL::VARCHAR                                       AS workflow_id
        FROM oc.message m
        JOIN oc.session s ON s.id = m.session_id
        ORDER BY m.time_created",
    ).expect("create oc_messages");

    // tool_calls: one row per tool invocation
    conn.execute_batch(
        "CREATE OR REPLACE VIEW oc_tool_calls AS
        SELECT
            p.session_id,
            s.directory                                         AS project,
            'opencode'                                          AS source,
            p.message_id                                        AS message_uuid,
            json_extract_string(p.data, '$.callID')            AS tool_use_id,
            json_extract_string(p.data, '$.tool')              AS name,
            json_extract(p.data, '$.state.input')              AS input,
            epoch_ms(p.time_created)::VARCHAR                   AS timestamp,
            NULL::VARCHAR                                       AS agent_id,
            false                                               AS is_sidechain,
            json_extract_string(oc_msg.data, '$.agent')        AS agent_type,
            NULL::VARCHAR                                       AS workflow_id
        FROM oc.part p
        JOIN oc.message oc_msg ON oc_msg.id = p.message_id
        JOIN oc.session s ON s.id = p.session_id
        WHERE json_extract_string(p.data, '$.type') = 'tool'
        ORDER BY p.time_created",
    ).expect("create oc_tool_calls");

    // tool_results: same part rows, different columns
    conn.execute_batch(
        "CREATE OR REPLACE VIEW oc_tool_results AS
        SELECT
            p.session_id,
            s.directory                                                  AS project,
            'opencode'                                                   AS source,
            json_extract_string(p.data, '$.callID')                     AS tool_use_id,
            (json_extract_string(p.data, '$.state.status') != 'completed'
             AND json_extract_string(p.data, '$.state.status') IS NOT NULL)
                                                                         AS is_error,
            json_extract_string(p.data, '$.state.output')              AS content,
            NULL::VARCHAR                                                AS agent_id,
            false                                                        AS is_sidechain,
            json_extract_string(oc_msg.data, '$.agent')                 AS agent_type,
            NULL::VARCHAR                                                AS workflow_id
        FROM oc.part p
        JOIN oc.message oc_msg ON oc_msg.id = p.message_id
        JOIN oc.session s ON s.id = p.session_id
        WHERE json_extract_string(p.data, '$.type') = 'tool'
        ORDER BY p.time_created",
    ).expect("create oc_tool_results");
}

#[test]
#[ignore]
fn opencode_spike_sessions() {
    let db_path = opencode_db_path();
    if !db_path.exists() {
        eprintln!("SKIP: {} not found", db_path.display());
        return;
    }

    let conn = setup_opencode_connection();
    create_views(&conn);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM oc_sessions", [], |r| r.get(0))
        .expect("count sessions");
    println!("Top-level sessions: {}", count);
    assert!(count > 0, "expected at least one session");

    // Print summary
    let mut stmt = conn
        .prepare(
            "SELECT session_id, project, started_at, message_count,
                    tool_call_count, user_message_count, subagent_count,
                    first_user_message
             FROM oc_sessions LIMIT 5",
        )
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    println!(
        "\n{:<32} {:<40} {:<26} {:>5} {:>6} {:>5} {:>5}",
        "session_id", "project", "started_at", "msgs", "tools", "user", "subs"
    );
    println!("{}", "-".repeat(125));
    while let Some(row) = rows.next().unwrap() {
        let session_id: String = row.get(0).unwrap_or_default();
        let project: String = row.get(1).unwrap_or_default();
        let started_at: String = row.get(2).unwrap_or_default();
        let message_count: i64 = row.get(3).unwrap_or(0);
        let tool_call_count: i64 = row.get(4).unwrap_or(0);
        let user_message_count: i64 = row.get(5).unwrap_or(0);
        let subagent_count: i64 = row.get(6).unwrap_or(0);
        // Truncate long fields for readability
        let sid = &session_id[..session_id.len().min(32)];
        let proj = if project.len() > 40 { &project[project.len() - 40..] } else { &project };
        println!(
            "{:<32} {:<40} {:<26} {:>5} {:>6} {:>5} {:>5}",
            sid, proj, &started_at[..started_at.len().min(26)],
            message_count, tool_call_count, user_message_count, subagent_count
        );
    }
}

#[test]
#[ignore]
fn opencode_spike_messages() {
    let db_path = opencode_db_path();
    if !db_path.exists() {
        eprintln!("SKIP: {} not found", db_path.display());
        return;
    }

    let conn = setup_opencode_connection();
    create_views(&conn);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM oc_messages", [], |r| r.get(0))
        .expect("count messages");
    println!("Total messages: {}", count);
    assert!(count > 0);

    // Verify required columns exist and have sensible types
    let user_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM oc_messages WHERE type = 'user'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let assistant_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM oc_messages WHERE type = 'assistant'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("  user: {}  assistant: {}", user_count, assistant_count);
    assert!(user_count > 0);
    assert!(assistant_count > 0);

    // Check is_sidechain column type (should always be false)
    let sidechain_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM oc_messages WHERE is_sidechain = true",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("  sidechain rows: {}", sidechain_count);
    assert_eq!(sidechain_count, 0, "all rows should be main-loop (is_sidechain=false)");
}

#[test]
#[ignore]
fn opencode_spike_tool_calls() {
    let db_path = opencode_db_path();
    if !db_path.exists() {
        eprintln!("SKIP: {} not found", db_path.display());
        return;
    }

    let conn = setup_opencode_connection();
    create_views(&conn);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM oc_tool_calls", [], |r| r.get(0))
        .expect("count tool_calls");
    println!("Total tool calls: {}", count);
    assert!(count > 0);

    println!("\nTool usage breakdown:");
    let mut stmt = conn
        .prepare("SELECT name, COUNT(*) AS calls FROM oc_tool_calls GROUP BY name ORDER BY calls DESC")
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let name: String = row.get(0).unwrap_or_default();
        let calls: i64 = row.get(1).unwrap_or(0);
        println!("  {:20} {}", name, calls);
    }

    // tool_calls and tool_results should have same count (same underlying part rows)
    let results_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM oc_tool_results", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, results_count, "tool_calls and tool_results should have same row count");
}

#[test]
#[ignore]
fn opencode_spike_full_demo() {
    let db_path = opencode_db_path();
    if !db_path.exists() {
        eprintln!("SKIP: {} not found", db_path.display());
        return;
    }

    let conn = setup_opencode_connection();
    create_views(&conn);

    println!("=== opencode -> cq schema prototype ===\n");

    // Stats
    let sessions: i64 = conn.query_row("SELECT COUNT(*) FROM oc_sessions", [], |r| r.get(0)).unwrap();
    let messages: i64 = conn.query_row("SELECT COUNT(*) FROM oc_messages", [], |r| r.get(0)).unwrap();
    let tools: i64 = conn.query_row("SELECT COUNT(*) FROM oc_tool_calls", [], |r| r.get(0)).unwrap();
    let distinct_tools: i64 = conn
        .query_row("SELECT COUNT(DISTINCT name) FROM oc_tool_calls", [], |r| r.get(0))
        .unwrap();

    println!("sessions:       {}", sessions);
    println!("messages:       {}", messages);
    println!("tool_calls:     {}", tools);
    println!("distinct tools: {}", distinct_tools);

    println!("\n--- sessions (newest 3) ---");
    let mut stmt = conn
        .prepare(
            "SELECT session_id, project, source, started_at, message_count, tool_call_count, first_user_message
             FROM oc_sessions LIMIT 3",
        )
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let sid: String = row.get(0).unwrap_or_default();
        let project: String = row.get(1).unwrap_or_default();
        let source: String = row.get(2).unwrap_or_default();
        let started: String = row.get(3).unwrap_or_default();
        let msgs: i64 = row.get(4).unwrap_or(0);
        let tools_n: i64 = row.get(5).unwrap_or(0);
        let title: String = row.get(6).unwrap_or_default();
        println!("  {} | {} | {} | msgs={} tools={}", sid, source, started, msgs, tools_n);
        println!("    project: {}", project);
        println!("    title:   {}", &title[..title.len().min(80)]);
    }

    println!("\n--- tool call breakdown ---");
    let mut stmt = conn
        .prepare("SELECT name, COUNT(*) AS calls FROM oc_tool_calls GROUP BY name ORDER BY calls DESC")
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let name: String = row.get(0).unwrap_or_default();
        let calls: i64 = row.get(1).unwrap_or(0);
        println!("  {:20} {}", name, calls);
    }
}
