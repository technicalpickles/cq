pub fn run(name: Option<&str>, examples: bool) {
    if let Some(view_name) = name {
        print_view(view_name);
    } else if examples {
        println!("{}", EXAMPLE_QUERIES);
    } else {
        println!("{}", SCHEMA_DOCS);
    }
}

fn print_view(name: &str) {
    let section = match name {
        "messages" => Some(MESSAGES_SCHEMA),
        "tool_calls" => Some(TOOL_CALLS_SCHEMA),
        "tool_results" => Some(TOOL_RESULTS_SCHEMA),
        "sessions" => Some(SESSIONS_SCHEMA),
        _ => None,
    };
    match section {
        Some(s) => println!("{}", s),
        None => eprintln!(
            "Unknown view: {name}. Known views: messages, tool_calls, tool_results, sessions"
        ),
    }
}

const MESSAGES_SCHEMA: &str = r#"messages
--------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name (directory containing the JSONL file)
  uuid                VARCHAR   Message UUID
  parent_uuid         VARCHAR   Parent message UUID
  type                VARCHAR   'user' or 'assistant'
  timestamp           VARCHAR   ISO 8601 timestamp string
  text                VARCHAR   Text content of the message (first text block for assistant)
  tool_count          BIGINT    Number of tool calls in this message (assistant only)
  model               VARCHAR   Model used (assistant messages only)"#;

const TOOL_CALLS_SCHEMA: &str = r#"tool_calls
----------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  message_uuid        VARCHAR   UUID of the containing assistant message
  tool_use_id         VARCHAR   Unique tool use ID (matches tool_results.tool_use_id)
  name                VARCHAR   Tool name (e.g. 'Bash', 'Read', 'Edit', 'Skill')
  input               JSON      Tool input as JSON object
  timestamp           VARCHAR   ISO 8601 timestamp string

  Note: query input fields with json_extract_string(input, '$.field_name')
  Example: json_extract_string(input, '$.command') for Bash commands"#;

const TOOL_RESULTS_SCHEMA: &str = r#"tool_results
------------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  tool_use_id         VARCHAR   Matches tool_calls.tool_use_id
  is_error            BOOLEAN   true if the tool call returned an error
  content             VARCHAR   Tool result content (text)"#;

const SESSIONS_SCHEMA: &str = r#"sessions
--------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  started_at          VARCHAR   Timestamp of first message
  ended_at            VARCHAR   Timestamp of last message
  message_count       BIGINT    Total messages in session
  tool_call_count     BIGINT    Total tool calls in session
  user_message_count  BIGINT    Number of user turns
  first_user_message  VARCHAR   Text of the first user message"#;

const EXAMPLE_QUERIES: &str = r#"Example Queries
===============

All Bash commands:
  SELECT session_id, timestamp, json_extract_string(input, '$.command') AS command
  FROM tool_calls
  WHERE name = 'Bash'
  ORDER BY timestamp DESC
  LIMIT 50;

Tool usage frequency:
  SELECT name, COUNT(*) AS count
  FROM tool_calls
  GROUP BY name
  ORDER BY count DESC;

Sessions with errors:
  SELECT DISTINCT tc.session_id, tc.project, tc.timestamp, tc.name
  FROM tool_calls tc
  JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
  WHERE tr.is_error = true
  ORDER BY tc.timestamp DESC;

Skills used:
  SELECT json_extract_string(input, '$.skill') AS skill, COUNT(*) AS count
  FROM tool_calls
  WHERE name = 'Skill'
  GROUP BY skill
  ORDER BY count DESC;

Files read most often:
  SELECT json_extract_string(input, '$.file_path') AS file_path, COUNT(*) AS count
  FROM tool_calls
  WHERE name = 'Read'
  GROUP BY file_path
  ORDER BY count DESC
  LIMIT 20;

Docker-related Bash commands:
  SELECT session_id, timestamp, json_extract_string(input, '$.command') AS command
  FROM tool_calls
  WHERE name = 'Bash'
  AND CAST(input AS VARCHAR) ILIKE '%docker%'
  ORDER BY timestamp DESC;

Recent sessions:
  SELECT session_id, project, started_at, message_count, tool_call_count, first_user_message
  FROM sessions
  ORDER BY started_at DESC
  LIMIT 10;"#;

const SCHEMA_DOCS: &str = r#"cq Views Schema
===============

messages
--------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name (directory containing the JSONL file)
  uuid                VARCHAR   Message UUID
  parent_uuid         VARCHAR   Parent message UUID
  type                VARCHAR   'user' or 'assistant'
  timestamp           VARCHAR   ISO 8601 timestamp string
  text                VARCHAR   Text content of the message (first text block for assistant)
  tool_count          BIGINT    Number of tool calls in this message (assistant only)
  model               VARCHAR   Model used (assistant messages only)

tool_calls
----------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  message_uuid        VARCHAR   UUID of the containing assistant message
  tool_use_id         VARCHAR   Unique tool use ID (matches tool_results.tool_use_id)
  name                VARCHAR   Tool name (e.g. 'Bash', 'Read', 'Edit', 'Skill')
  input               JSON      Tool input as JSON object
  timestamp           VARCHAR   ISO 8601 timestamp string

  Note: query input fields with json_extract_string(input, '$.field_name')
  Example: json_extract_string(input, '$.command') for Bash commands

tool_results
------------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  tool_use_id         VARCHAR   Matches tool_calls.tool_use_id
  is_error            BOOLEAN   true if the tool call returned an error
  content             VARCHAR   Tool result content (text)

sessions
--------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  started_at          VARCHAR   Timestamp of first message
  ended_at            VARCHAR   Timestamp of last message
  message_count       BIGINT    Total messages in session
  tool_call_count     BIGINT    Total tool calls in session
  user_message_count  BIGINT    Number of user turns
  first_user_message  VARCHAR   Text of the first user message


Example Queries
===============

All Bash commands:
  SELECT session_id, timestamp, json_extract_string(input, '$.command') AS command
  FROM tool_calls
  WHERE name = 'Bash'
  ORDER BY timestamp DESC
  LIMIT 50;

Tool usage frequency:
  SELECT name, COUNT(*) AS count
  FROM tool_calls
  GROUP BY name
  ORDER BY count DESC;

Sessions with errors:
  SELECT DISTINCT tc.session_id, tc.project, tc.timestamp, tc.name
  FROM tool_calls tc
  JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
  WHERE tr.is_error = true
  ORDER BY tc.timestamp DESC;

Skills used:
  SELECT json_extract_string(input, '$.skill') AS skill, COUNT(*) AS count
  FROM tool_calls
  WHERE name = 'Skill'
  GROUP BY skill
  ORDER BY count DESC;

Files read most often:
  SELECT json_extract_string(input, '$.file_path') AS file_path, COUNT(*) AS count
  FROM tool_calls
  WHERE name = 'Read'
  GROUP BY file_path
  ORDER BY count DESC
  LIMIT 20;

Docker-related Bash commands:
  SELECT session_id, timestamp, json_extract_string(input, '$.command') AS command
  FROM tool_calls
  WHERE name = 'Bash'
  AND CAST(input AS VARCHAR) ILIKE '%docker%'
  ORDER BY timestamp DESC;

Recent sessions:
  SELECT session_id, project, started_at, message_count, tool_call_count, first_user_message
  FROM sessions
  ORDER BY started_at DESC
  LIMIT 10;
"#;
