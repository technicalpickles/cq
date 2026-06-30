use crate::provider::{TranscriptProvider, View};
use anyhow::{Context, Result};
use duckdb::Connection;
use std::path::PathBuf;

/// SQL expression to get the project path. Uses cwd from file_registry if
/// available, falls back to decoding the directory name from source_file.
const PROJECT_EXPR: &str = "COALESCE(
        (SELECT fr.cwd FROM file_registry fr WHERE fr.file_path = source_file),
        '/' || replace(regexp_extract(source_file, '.*/([^/]+)/[^/]+$', 1)[2:], '-', '/')
    )";

/// SQL expression for the subagent identity (NULL for main-loop rows).
const AGENT_ID_EXPR: &str = "json_extract_string(json, '$.agentId')";

/// SQL expression for the sidechain flag (always non-null; false for main loop).
const IS_SIDECHAIN_EXPR: &str =
    "COALESCE(CAST(json_extract(json, '$.isSidechain') AS BOOLEAN), false)";

/// SQL expression for the workflow run id, parsed from the file path.
/// NULL for plain subagents and main-loop rows.
const WORKFLOW_ID_EXPR: &str =
    "NULLIF(regexp_extract(source_file, 'workflows/(wf_[^/]+)/', 1), '')";

/// SQL expression for the subagent type, read from the sidecar meta.json at
/// index time and stored in file_registry. NULL for main-loop rows.
const AGENT_TYPE_EXPR: &str =
    "(SELECT fr.agent_type FROM file_registry fr WHERE fr.file_path = source_file)";

/// SQL expression for the source name, read from file_registry at index time.
const SOURCE_EXPR: &str =
    "(SELECT fr.source FROM file_registry fr WHERE fr.file_path = source_file)";

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
        // No raw_records source; compose with no active providers -> empty views.
        return compose_views(conn, &[]);
    }

    let file_list = build_file_list(files);
    register_raw_view(conn, &file_list)?;
    register_derived_views(conn)
}

/// Register the derived views over an existing `raw_records`. Composes from the
/// Claude provider (the only provider that reads `raw_records`).
pub fn register_derived_views(conn: &Connection) -> Result<()> {
    let claude = crate::claude_provider::ClaudeProvider::new_with_base(std::path::PathBuf::new());
    let providers: [&dyn TranscriptProvider; 1] = [&claude];
    compose_views(conn, &providers)
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
/// (with auto-inferred nested fields) plus a `filename` column (aliased to `source_file`).
/// The JSON extension must be bundled (via the `json` cargo feature on duckdb).
fn register_raw_view(conn: &Connection, file_list: &str) -> Result<()> {
    let sql = format!(
        "CREATE OR REPLACE VIEW raw_records AS
        SELECT json, filename AS source_file
        FROM read_json({file_list}, format='newline_delimited', records=false, filename=true, union_by_name=true, ignore_errors=true)"
    );
    conn.execute_batch(&sql)
        .context("Failed to create raw_records view")?;
    Ok(())
}

/// The Claude `messages` view body (SELECT only, no CREATE VIEW wrapper).
pub fn claude_messages_sql() -> String {
    format!("WITH string_msgs AS (
            SELECT
                json_extract_string(json, '$.sessionId') AS session_id,
                {PROJECT_EXPR} AS project,
                {SOURCE_EXPR} AS source,
                'claude' AS harness,
                json_extract_string(json, '$.uuid') AS uuid,
                json_extract_string(json, '$.parentUuid') AS parent_uuid,
                json_extract_string(json, '$.type') AS type,
                json_extract_string(json, '$.timestamp') AS timestamp,
                json_extract_string(json, '$.message.content') AS text,
                CAST(0 AS BIGINT) AS tool_count,
                json_extract_string(json, '$.message.model') AS model,
                {AGENT_ID_EXPR} AS agent_id,
                {IS_SIDECHAIN_EXPR} AS is_sidechain,
                {AGENT_TYPE_EXPR} AS agent_type,
                {WORKFLOW_ID_EXPR} AS workflow_id
            FROM raw_records
            WHERE json_extract_string(json, '$.type') IN ('user', 'assistant')
            AND json_type(json_extract(json, '$.message.content')) = 'VARCHAR'
        ),
        array_msgs AS (
            SELECT
                json_extract_string(json, '$.sessionId') AS session_id,
                {PROJECT_EXPR} AS project,
                {SOURCE_EXPR} AS source,
                'claude' AS harness,
                json_extract_string(json, '$.uuid') AS uuid,
                json_extract_string(json, '$.parentUuid') AS parent_uuid,
                json_extract_string(json, '$.type') AS type,
                json_extract_string(json, '$.timestamp') AS timestamp,
                (SELECT json_extract_string(item, '$.text')
                 FROM (SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item)
                 WHERE json_extract_string(item, '$.type') = 'text'
                 LIMIT 1) AS text,
                CASE WHEN json_extract_string(json, '$.type') = 'assistant' THEN
                    (SELECT COUNT(*)
                     FROM (SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item)
                     WHERE json_extract_string(item, '$.type') = 'tool_use')
                ELSE CAST(0 AS BIGINT)
                END AS tool_count,
                json_extract_string(json, '$.message.model') AS model,
                {AGENT_ID_EXPR} AS agent_id,
                {IS_SIDECHAIN_EXPR} AS is_sidechain,
                {AGENT_TYPE_EXPR} AS agent_type,
                {WORKFLOW_ID_EXPR} AS workflow_id
            FROM raw_records
            WHERE json_extract_string(json, '$.type') IN ('user', 'assistant')
            AND json_type(json_extract(json, '$.message.content')) = 'ARRAY'
        )
        SELECT * FROM string_msgs
        UNION ALL
        SELECT * FROM array_msgs")
}

/// The Claude `tool_calls` view body.
pub fn claude_tool_calls_sql() -> String {
    format!(
        "SELECT
            json_extract_string(json, '$.sessionId') AS session_id,
            {PROJECT_EXPR} AS project,
            {SOURCE_EXPR} AS source,
            'claude' AS harness,
            json_extract_string(json, '$.uuid') AS message_uuid,
            json_extract_string(item, '$.id') AS tool_use_id,
            json_extract_string(item, '$.name') AS name,
            json_extract(item, '$.input') AS input,
            json_extract_string(json, '$.timestamp') AS timestamp,
            {AGENT_ID_EXPR} AS agent_id,
            {IS_SIDECHAIN_EXPR} AS is_sidechain,
            {AGENT_TYPE_EXPR} AS agent_type,
            {WORKFLOW_ID_EXPR} AS workflow_id
        FROM raw_records,
        LATERAL (
            SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item
        )
        WHERE json_extract_string(json, '$.type') = 'assistant'
        AND json_type(json_extract(json, '$.message.content')) = 'ARRAY'
        AND json_extract_string(item, '$.type') = 'tool_use'"
    )
}

/// The Claude `tool_results` view body.
pub fn claude_tool_results_sql() -> String {
    format!(
        "SELECT
            json_extract_string(json, '$.sessionId') AS session_id,
            {PROJECT_EXPR} AS project,
            {SOURCE_EXPR} AS source,
            'claude' AS harness,
            json_extract_string(item, '$.tool_use_id') AS tool_use_id,
            COALESCE(CAST(json_extract(item, '$.is_error') AS BOOLEAN), false) AS is_error,
            json_extract_string(item, '$.content') AS content,
            {AGENT_ID_EXPR} AS agent_id,
            {IS_SIDECHAIN_EXPR} AS is_sidechain,
            {AGENT_TYPE_EXPR} AS agent_type,
            {WORKFLOW_ID_EXPR} AS workflow_id
        FROM raw_records,
        LATERAL (
            SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item
        )
        WHERE json_extract_string(json, '$.type') = 'user'
        AND json_type(json_extract(json, '$.message.content')) = 'ARRAY'
        AND json_extract_string(item, '$.type') = 'tool_result'"
    )
}

/// The Claude `sessions` view body. Aggregates over the (possibly multi-provider)
/// `messages` view, filtered to Claude rows so it never scoops up another harness's
/// messages once `messages` becomes a UNION.
pub fn claude_sessions_sql() -> String {
    "SELECT
            session_id,
            COALESCE(
                MAX(project) FILTER (WHERE NOT is_sidechain),
                MAX(project)
            ) AS project,
            COALESCE(
                MAX(source) FILTER (WHERE NOT is_sidechain),
                MAX(source)
            ) AS source,
            'claude' AS harness,
            MIN(timestamp) FILTER (WHERE NOT is_sidechain) AS started_at,
            MAX(timestamp) FILTER (WHERE NOT is_sidechain) AS ended_at,
            COUNT(*) FILTER (WHERE NOT is_sidechain) AS message_count,
            CAST(COALESCE(SUM(tool_count) FILTER (WHERE NOT is_sidechain), 0) AS BIGINT) AS tool_call_count,
            COUNT(*) FILTER (WHERE type = 'user' AND NOT is_sidechain) AS user_message_count,
            COUNT(DISTINCT agent_id) AS subagent_count,
            (SELECT text FROM messages m2
             WHERE m2.session_id = m1.session_id
             AND m2.harness = 'claude'
             AND m2.type = 'user'
             AND NOT m2.is_sidechain
             AND m2.text IS NOT NULL
             AND m2.text != ''
             AND m2.text NOT LIKE '<%'
             AND m2.text NOT LIKE 'Base directory for this skill%'
             AND m2.text NOT LIKE '#%'
             ORDER BY m2.timestamp LIMIT 1) AS first_user_message
        FROM messages m1
        WHERE harness = 'claude'
        GROUP BY session_id".to_string()
}

/// The empty-view body (correct schema, zero rows) for one view. Used when no
/// active provider contributes to that view.
fn empty_view_sql(view: View) -> &'static str {
    match view {
        View::Messages => {
            "SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS source,
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS uuid,
            NULL::VARCHAR AS parent_uuid,
            NULL::VARCHAR AS type,
            NULL::VARCHAR AS timestamp,
            NULL::VARCHAR AS text,
            CAST(0 AS BIGINT) AS tool_count,
            NULL::VARCHAR AS model,
            NULL::VARCHAR AS agent_id,
            false AS is_sidechain,
            NULL::VARCHAR AS agent_type,
            NULL::VARCHAR AS workflow_id
        WHERE 1=0"
        }
        View::ToolCalls => {
            "SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS source,
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS message_uuid,
            NULL::VARCHAR AS tool_use_id,
            NULL::VARCHAR AS name,
            NULL::JSON AS input,
            NULL::VARCHAR AS timestamp,
            NULL::VARCHAR AS agent_id,
            false AS is_sidechain,
            NULL::VARCHAR AS agent_type,
            NULL::VARCHAR AS workflow_id
        WHERE 1=0"
        }
        View::ToolResults => {
            "SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS source,
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS tool_use_id,
            false AS is_error,
            NULL::VARCHAR AS content,
            NULL::VARCHAR AS agent_id,
            false AS is_sidechain,
            NULL::VARCHAR AS agent_type,
            NULL::VARCHAR AS workflow_id
        WHERE 1=0"
        }
        View::Sessions => {
            "SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS source,
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS started_at,
            NULL::VARCHAR AS ended_at,
            CAST(0 AS BIGINT) AS message_count,
            CAST(0 AS BIGINT) AS tool_call_count,
            CAST(0 AS BIGINT) AS user_message_count,
            CAST(0 AS BIGINT) AS subagent_count,
            NULL::VARCHAR AS first_user_message
        WHERE 1=0"
        }
    }
}

/// Compose the four views from the active providers' contributions. Each
/// contribution is wrapped in parens and `UNION ALL`ed. A view with no
/// contributors falls back to the empty-view schema. Views are created in
/// `View::ALL` order so `sessions` (which reads `messages`) comes last.
pub fn compose_views(conn: &Connection, providers: &[&dyn TranscriptProvider]) -> Result<()> {
    for view in View::ALL {
        let parts: Vec<String> = providers
            .iter()
            .filter_map(|p| p.contribute_view_sql(view))
            .map(|body| format!("({body})"))
            .collect();
        let body = if parts.is_empty() {
            empty_view_sql(view).to_string()
        } else {
            parts.join("\nUNION ALL\n")
        };
        let sql = format!("CREATE OR REPLACE VIEW {} AS {body}", view.name());
        conn.execute_batch(&sql)
            .with_context(|| format!("Failed to create {} view", view.name()))?;
    }
    Ok(())
}
