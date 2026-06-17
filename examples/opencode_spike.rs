//! Prototype: query opencode's SQLite DB via DuckDB and emit cq-shaped output.
//!
//! Run with:
//!   cargo run --example opencode_spike
//!   cargo run --example opencode_spike -- --messages
//!   cargo run --example opencode_spike -- --tools
//!
//! Requires the DuckDB sqlite extension (downloaded on first run; needs network).
//! The opencode DB at ~/.local/share/opencode/opencode.db is opened read-only.

use anyhow::{Context, Result};
use cq::output::{self, OutputFormat};
use duckdb::Connection;
use std::env;
use std::path::PathBuf;

fn opencode_db_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".local/share/opencode/opencode.db")
}

fn setup(db_path: &PathBuf) -> Result<Connection> {
    let conn = Connection::open_in_memory().context("open in-memory DuckDB")?;

    // Install the SQLite scanner extension (downloads once to ~/.duckdb/extensions/).
    // If this fails with a network error, set DUCKDB_EXTENSION_DIR or pre-download.
    conn.execute_batch("INSTALL sqlite; LOAD sqlite;")
        .context("install/load sqlite extension")?;

    // Attach the opencode DB read-only so we never accidentally write to it.
    conn.execute_batch(&format!(
        "ATTACH '{}' AS oc (TYPE sqlite, READ_ONLY);",
        db_path.display()
    ))
    .context("attach opencode.db")?;

    Ok(conn)
}

// --- view definitions ---

/// cq sessions equivalent: one row per top-level opencode session.
///
/// Mapping notes vs cq's sessions view:
/// - project  = session.directory (the worktree path)
/// - started_at/ended_at: epoch_ms(bigint)::VARCHAR, format is "YYYY-MM-DD HH:MM:SS.mmm"
///   (cq uses ISO 8601 "YYYY-MM-DDTHH:MM:SSZ"; real integration would normalize)
/// - first_user_message = session.title (opencode sets this to the user's initial prompt)
/// - subagent_count = child sessions (parent_id != NULL), not in-message agent_id
///   (opencode's sub-agent model differs from CC: each sub-agent IS a session)
fn create_oc_sessions_view(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE OR REPLACE VIEW oc_sessions AS
        SELECT
            s.id                                    AS session_id,
            s.directory                             AS project,
            'opencode'                              AS source,
            epoch_ms(s.time_created)::VARCHAR       AS started_at,
            epoch_ms(s.time_updated)::VARCHAR       AS ended_at,
            (SELECT COUNT(*)
             FROM oc.message m
             WHERE m.session_id = s.id)             AS message_count,
            (SELECT COUNT(*)
             FROM oc.part p
             WHERE p.session_id = s.id
               AND json_extract_string(p.data, '$.type') = 'tool')
                                                    AS tool_call_count,
            (SELECT COUNT(*)
             FROM oc.message m
             WHERE m.session_id = s.id
               AND json_extract_string(m.data, '$.role') = 'user')
                                                    AS user_message_count,
            (SELECT COUNT(*)
             FROM oc.session sub
             WHERE sub.parent_id = s.id)            AS subagent_count,
            s.title                                 AS first_user_message
        FROM oc.session s
        WHERE s.parent_id IS NULL
        ORDER BY s.time_created DESC",
    )
    .context("create oc_sessions view")?;
    Ok(())
}

/// cq messages equivalent.
///
/// Mapping notes:
/// - uuid/parent_uuid use opencode's message.id / data.parentID (different ID namespace)
/// - type maps from data.role: 'user' or 'assistant'
/// - text is extracted from parts with type='text' (not stored in message.data directly)
/// - model: assistant messages store modelID at top level; user messages store it nested
/// - is_sidechain: false for all rows here (sub-sessions are separate sessions in opencode)
/// - agent_type: data.agent field ('build', 'general', etc.)
fn create_oc_messages_view(conn: &Connection) -> Result<()> {
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
            (SELECT COUNT(*)
             FROM oc.part p
             WHERE p.message_id = m.id
               AND json_extract_string(p.data, '$.type') = 'tool')
                                                                AS tool_count,
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
    )
    .context("create oc_messages view")?;
    Ok(())
}

/// cq tool_calls equivalent.
///
/// Mapping notes:
/// - tool_use_id = part.data.callID (opencode's per-call identifier)
/// - name = part.data.tool (e.g. 'bash', 'read', 'write', 'task')
/// - input = part.data.state.input (JSON object)
/// - IMPORTANT: in opencode, tool call + result live in the SAME part row.
///   cq splits them into tool_calls and tool_results. The callID is the join key.
fn create_oc_tool_calls_view(conn: &Connection) -> Result<()> {
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
    )
    .context("create oc_tool_calls view")?;
    Ok(())
}

/// cq tool_results equivalent.
///
/// Same part rows as oc_tool_calls; different columns surfaced.
/// is_error: true when state.status != 'completed'.
/// content: state.output (the stdout/result text).
fn create_oc_tool_results_view(conn: &Connection) -> Result<()> {
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
    )
    .context("create oc_tool_results view")?;
    Ok(())
}

fn print_section(conn: &Connection, title: &str, sql: &str) -> Result<()> {
    println!("\n=== {} ===", title);
    let mut stmt = conn.prepare(sql)?;
    output::print_results(&mut stmt, &[], &OutputFormat::Default, false)?;
    Ok(())
}

fn main() -> Result<()> {
    let db_path = opencode_db_path();
    if !db_path.exists() {
        eprintln!(
            "opencode.db not found at {}\nInstall opencode and start a session first.",
            db_path.display()
        );
        std::process::exit(1);
    }

    let args: Vec<String> = env::args().collect();
    let show_messages = args.iter().any(|a| a == "--messages");
    let show_tools = args.iter().any(|a| a == "--tools");

    eprintln!("Connecting to {} ...", db_path.display());
    let conn = setup(&db_path)?;

    create_oc_sessions_view(&conn)?;
    create_oc_messages_view(&conn)?;
    create_oc_tool_calls_view(&conn)?;
    create_oc_tool_results_view(&conn)?;

    // Sessions (always shown)
    print_section(
        &conn,
        "sessions (top-level, newest first)",
        "SELECT session_id, project, source, started_at, ended_at,
                message_count, tool_call_count, user_message_count,
                subagent_count, first_user_message
         FROM oc_sessions
         LIMIT 10",
    )?;

    // Summary stats
    print_section(
        &conn,
        "summary stats",
        "SELECT
            (SELECT COUNT(*) FROM oc_sessions)        AS sessions,
            (SELECT COUNT(*) FROM oc_messages)        AS messages,
            (SELECT COUNT(*) FROM oc_tool_calls)      AS tool_calls,
            (SELECT COUNT(DISTINCT name) FROM oc_tool_calls) AS distinct_tools",
    )?;

    // Tool usage breakdown
    print_section(
        &conn,
        "tool call counts by name",
        "SELECT name, COUNT(*) AS calls
         FROM oc_tool_calls
         GROUP BY name
         ORDER BY calls DESC",
    )?;

    if show_messages {
        print_section(
            &conn,
            "messages (newest session)",
            "SELECT session_id, uuid, type, timestamp, model, agent_type,
                    tool_count, text
             FROM oc_messages
             WHERE session_id = (SELECT session_id FROM oc_sessions LIMIT 1)
             ORDER BY timestamp",
        )?;
    }

    if show_tools {
        print_section(
            &conn,
            "tool_calls (newest session)",
            "SELECT session_id, message_uuid, tool_use_id, name, timestamp, agent_type
             FROM oc_tool_calls
             WHERE session_id = (SELECT session_id FROM oc_sessions LIMIT 1)
             ORDER BY timestamp",
        )?;

        print_section(
            &conn,
            "tool_results (newest session, first 5)",
            "SELECT session_id, tool_use_id, is_error, agent_type, content
             FROM oc_tool_results
             WHERE session_id = (SELECT session_id FROM oc_sessions LIMIT 1)
             ORDER BY session_id, tool_use_id
             LIMIT 5",
        )?;
    }

    Ok(())
}
