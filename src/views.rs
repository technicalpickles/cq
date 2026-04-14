use anyhow::{Context, Result};
use duckdb::Connection;
use std::path::PathBuf;

/// SQL expression to extract and decode the project path from a filename.
/// Input: filename column from read_json (e.g. "/path/to/-Users-josh-pickleton/sess.jsonl")
/// Output: decoded path (e.g. "/Users/josh/pickleton")
const PROJECT_EXPR: &str =
    "'/' || replace(regexp_extract(filename, '.*/([^/]+)/[^/]+$', 1)[2:], '-', '/')";

/// Register all queryable views against the given JSONL transcript files.
///
/// Creates four views:
/// - `messages`: one row per user/assistant turn
/// - `tool_calls`: one row per tool_use block (from assistant messages)
/// - `tool_results`: one row per tool_result block (from user messages with array content)
/// - `sessions`: aggregated session-level metrics
///
/// When `files` is empty, creates empty views with the correct schema so queries
/// don't error.
pub fn register_views(conn: &Connection, files: &[PathBuf]) -> Result<()> {
    if files.is_empty() {
        return register_empty_views(conn);
    }

    let file_list = build_file_list(files);

    register_raw_view(conn, &file_list)?;
    register_messages_view(conn)?;
    register_tool_calls_view(conn)?;
    register_tool_results_view(conn)?;
    register_sessions_view(conn)?;

    Ok(())
}

/// Build a DuckDB list literal from file paths: ['path1', 'path2', ...]
fn build_file_list(files: &[PathBuf]) -> String {
    let paths: Vec<String> = files
        .iter()
        .map(|p| format!("'{}'", p.display().to_string().replace('\'', "''")))
        .collect();
    format!("[{}]", paths.join(", "))
}

/// Create the raw view that reads JSONL files with DuckDB's auto-schema inference.
///
/// DuckDB with `records=false` produces a `json` column typed as a STRUCT
/// (with auto-inferred nested fields) plus a `filename` column for the source file.
/// The JSON extension must be bundled (via the `json` cargo feature on duckdb).
fn register_raw_view(conn: &Connection, file_list: &str) -> Result<()> {
    let sql = format!(
        "CREATE VIEW raw_records AS
        SELECT json, filename
        FROM read_json({file_list}, format='newline_delimited', records=false, filename=true, union_by_name=true, ignore_errors=true)"
    );
    conn.execute_batch(&sql)
        .context("Failed to create raw_records view")?;
    Ok(())
}

/// Create the messages view.
///
/// User messages can have string content (human text) or array content (tool results).
/// Assistant messages always have array content (text blocks + tool_use blocks).
///
/// We use UNION ALL to handle both cases separately, avoiding DuckDB's eagerness
/// to evaluate CAST(content AS JSON[]) even inside a CASE WHEN branch where
/// content is a string.
fn register_messages_view(conn: &Connection) -> Result<()> {
    let sql = format!("CREATE VIEW messages AS
        WITH string_msgs AS (
            SELECT
                json.sessionId AS session_id,
                {PROJECT_EXPR} AS project,
                json.uuid AS uuid,
                json.parentUuid AS parent_uuid,
                json.type AS type,
                json.timestamp AS timestamp,
                json_extract_string(json.message, '$.content') AS text,
                CAST(0 AS BIGINT) AS tool_count,
                CAST(json.message.model AS VARCHAR) AS model
            FROM raw_records
            WHERE json.type IN ('user', 'assistant')
            AND json_type(json.message.content) = 'VARCHAR'
        ),
        array_msgs AS (
            SELECT
                json.sessionId AS session_id,
                {PROJECT_EXPR} AS project,
                json.uuid AS uuid,
                json.parentUuid AS parent_uuid,
                json.type AS type,
                json.timestamp AS timestamp,
                (SELECT json_extract_string(item, '$.text')
                 FROM (SELECT UNNEST(CAST(json.message.content AS JSON[])) AS item)
                 WHERE json_extract_string(item, '$.type') = 'text'
                 LIMIT 1) AS text,
                CASE WHEN json.type = 'assistant' THEN
                    (SELECT COUNT(*)
                     FROM (SELECT UNNEST(CAST(json.message.content AS JSON[])) AS item)
                     WHERE json_extract_string(item, '$.type') = 'tool_use')
                ELSE CAST(0 AS BIGINT)
                END AS tool_count,
                CAST(json.message.model AS VARCHAR) AS model
            FROM raw_records
            WHERE json.type IN ('user', 'assistant')
            AND json_type(json.message.content) = 'ARRAY'
        )
        SELECT * FROM string_msgs
        UNION ALL
        SELECT * FROM array_msgs");
    conn.execute_batch(&sql)
        .context("Failed to create messages view")?;
    Ok(())
}

/// Create the tool_calls view.
///
/// Extracts one row per tool_use content block from assistant messages.
/// Uses LATERAL UNNEST to flatten the content array, then filters for tool_use type.
fn register_tool_calls_view(conn: &Connection) -> Result<()> {
    let sql = format!("CREATE VIEW tool_calls AS
        SELECT
            json.sessionId AS session_id,
            {PROJECT_EXPR} AS project,
            json.uuid AS message_uuid,
            json_extract_string(item, '$.id') AS tool_use_id,
            json_extract_string(item, '$.name') AS name,
            json_extract(item, '$.input') AS input,
            json.timestamp AS timestamp
        FROM raw_records,
        LATERAL (
            SELECT UNNEST(CAST(json.message.content AS JSON[])) AS item
        )
        WHERE json.type = 'assistant'
        AND json_type(json.message.content) = 'ARRAY'
        AND json_extract_string(item, '$.type') = 'tool_use'");
    conn.execute_batch(&sql)
        .context("Failed to create tool_calls view")?;
    Ok(())
}

/// Create the tool_results view.
///
/// Extracts one row per tool_result content block from user messages that have
/// array content (i.e. messages carrying tool results back from tool execution).
fn register_tool_results_view(conn: &Connection) -> Result<()> {
    let sql = format!("CREATE VIEW tool_results AS
        SELECT
            json.sessionId AS session_id,
            {PROJECT_EXPR} AS project,
            json_extract_string(item, '$.tool_use_id') AS tool_use_id,
            COALESCE(CAST(json_extract(item, '$.is_error') AS BOOLEAN), false) AS is_error,
            json_extract_string(item, '$.content') AS content
        FROM raw_records,
        LATERAL (
            SELECT UNNEST(CAST(json.message.content AS JSON[])) AS item
        )
        WHERE json.type = 'user'
        AND json_type(json.message.content) = 'ARRAY'
        AND json_extract_string(item, '$.type') = 'tool_result'");
    conn.execute_batch(&sql)
        .context("Failed to create tool_results view")?;
    Ok(())
}

/// Create the sessions view.
///
/// Aggregates from the messages view to provide session-level metrics.
fn register_sessions_view(conn: &Connection) -> Result<()> {
    let sql = "CREATE VIEW sessions AS
        SELECT
            session_id,
            project,
            MIN(timestamp) AS started_at,
            MAX(timestamp) AS ended_at,
            COUNT(*) AS message_count,
            CAST(SUM(tool_count) AS BIGINT) AS tool_call_count,
            COUNT(CASE WHEN type = 'user' THEN 1 END) AS user_message_count,
            (SELECT text FROM messages m2
             WHERE m2.session_id = m1.session_id
             AND m2.type = 'user' AND m2.text IS NOT NULL
             ORDER BY m2.timestamp LIMIT 1) AS first_user_message
        FROM messages m1
        GROUP BY session_id, project";
    conn.execute_batch(sql)
        .context("Failed to create sessions view")?;
    Ok(())
}

/// Register empty views when no files are provided.
/// Uses WHERE 1=0 against a VALUES clause so queries return 0 rows
/// without erroring on missing tables.
fn register_empty_views(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIEW messages AS
        SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS uuid,
            NULL::VARCHAR AS parent_uuid,
            NULL::VARCHAR AS type,
            NULL::VARCHAR AS timestamp,
            NULL::VARCHAR AS text,
            CAST(0 AS BIGINT) AS tool_count,
            NULL::VARCHAR AS model
        WHERE 1=0"
    ).context("Failed to create empty messages view")?;

    conn.execute_batch(
        "CREATE VIEW tool_calls AS
        SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS message_uuid,
            NULL::VARCHAR AS tool_use_id,
            NULL::VARCHAR AS name,
            NULL::JSON AS input,
            NULL::VARCHAR AS timestamp
        WHERE 1=0"
    ).context("Failed to create empty tool_calls view")?;

    conn.execute_batch(
        "CREATE VIEW tool_results AS
        SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS tool_use_id,
            false AS is_error,
            NULL::VARCHAR AS content
        WHERE 1=0"
    ).context("Failed to create empty tool_results view")?;

    conn.execute_batch(
        "CREATE VIEW sessions AS
        SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS started_at,
            NULL::VARCHAR AS ended_at,
            CAST(0 AS BIGINT) AS message_count,
            CAST(0 AS BIGINT) AS tool_call_count,
            CAST(0 AS BIGINT) AS user_message_count,
            NULL::VARCHAR AS first_user_message
        WHERE 1=0"
    ).context("Failed to create empty sessions view")?;

    Ok(())
}
