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
        "hook_events" => Some(HOOK_EVENTS_SCHEMA),
        "sessions" => Some(SESSIONS_SCHEMA),
        _ => None,
    };
    match section {
        Some(s) => println!("{}", s),
        None => {
            eprintln!("Error: Unknown view '{}'\nValid views: messages, tool_calls, tool_results, hook_events, sessions", name);
            std::process::exit(1);
        }
    }
}

const MESSAGES_SCHEMA: &str = r#"messages
--------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name (directory containing the JSONL file)
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  uuid                VARCHAR   Message UUID
  parent_uuid         VARCHAR   Parent message UUID
  type                VARCHAR   'user' or 'assistant'
  timestamp           VARCHAR   ISO 8601 timestamp string
  text                VARCHAR   Text content of the message (first text block for assistant)
  tool_count          BIGINT    Number of tool calls in this message (assistant only)
  model               VARCHAR   Model used (assistant messages only)
  agent_id            VARCHAR   Subagent id; NULL for main-loop rows
  is_sidechain        BOOLEAN   true if this row is from a subagent
  agent_type          VARCHAR   Subagent type from meta.json (e.g. 'Explore'); NULL for main loop
  workflow_id         VARCHAR   Workflow run id (wf_...) if spawned by a workflow, else NULL"#;

const TOOL_CALLS_SCHEMA: &str = r#"tool_calls
----------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  message_uuid        VARCHAR   UUID of the containing assistant message
  tool_use_id         VARCHAR   Unique tool use ID (matches tool_results.tool_use_id)
  name                VARCHAR   Tool name (e.g. 'Bash', 'Read', 'Edit', 'Skill')
  input               JSON      Tool input as JSON object
  timestamp           VARCHAR   ISO 8601 timestamp string
  agent_id            VARCHAR   Subagent id; NULL for main-loop rows
  is_sidechain        BOOLEAN   true if this row is from a subagent
  agent_type          VARCHAR   Subagent type from meta.json (e.g. 'Explore'); NULL for main loop
  workflow_id         VARCHAR   Workflow run id (wf_...) if spawned by a workflow, else NULL

  Note: query input fields with json_extract_string(input, '$.field_name')
  Example: json_extract_string(input, '$.command') for Bash commands
  Note: advisor() calls appear here with name = 'advisor'"#;

const TOOL_RESULTS_SCHEMA: &str = r#"tool_results
------------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  tool_use_id         VARCHAR   Matches tool_calls.tool_use_id
  is_error            BOOLEAN   true if the tool call returned an error
  content             VARCHAR   Tool result content (text); advisor() results are unwrapped from {type, text}
  agent_id            VARCHAR   Subagent id; NULL for main-loop rows
  is_sidechain        BOOLEAN   true if this row is from a subagent
  agent_type          VARCHAR   Subagent type from meta.json (e.g. 'Explore'); NULL for main loop
  workflow_id         VARCHAR   Workflow run id (wf_...) if spawned by a workflow, else NULL"#;

const HOOK_EVENTS_SCHEMA: &str = r#"hook_events
-----------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  timestamp           VARCHAR   ISO 8601 timestamp string
  hook_event          VARCHAR   Hook event name (e.g. 'SessionStart', 'PreToolUse', 'PostToolUse')
  hook_name           VARCHAR   Specific hook identifier (e.g. 'SessionStart:startup', 'PreToolUse:Bash')
  attachment_type     VARCHAR   'hook_success' or 'hook_additional_context'
  content             VARCHAR   Injected text or stdout; one row per plugin for hook_additional_context
  content_size        BIGINT    Byte length of content

  Note: one row per hook_additional_context array element (fanned out per plugin's SessionStart contribution)"#;

const SESSIONS_SCHEMA: &str = r#"sessions
--------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  started_at          VARCHAR   Timestamp of first message
  ended_at            VARCHAR   Timestamp of last message
  message_count       BIGINT    Total messages in session
  tool_call_count     BIGINT    Total tool calls in session
  user_message_count  BIGINT    Number of user turns
  subagent_count      BIGINT    Distinct subagents spawned in this session
  first_user_message  VARCHAR   Text of the first user message"#;

const EXAMPLE_QUERIES: &str = r#"Example Queries
===============

Ranked full-text message search (built-in command):
  cq search "dependency migration" --since 30d --type user

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

Subagent tool calls in a session:
  SELECT agent_type, name, COUNT(*) AS count
  FROM tool_calls
  WHERE session_id = 'SESSION_ID' AND is_sidechain
  GROUP BY agent_type, name
  ORDER BY count DESC;

Recent sessions:
  SELECT session_id, project, started_at, message_count, tool_call_count, first_user_message
  FROM sessions
  ORDER BY started_at DESC
  LIMIT 10;

SessionStart injection sizes by plugin:
  SELECT hook_name, content_size
  FROM hook_events
  WHERE attachment_type = 'hook_additional_context'
  ORDER BY content_size DESC;"#;

const SCHEMA_DOCS: &str = r#"cq Views Schema
===============

messages
--------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name (directory containing the JSONL file)
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  uuid                VARCHAR   Message UUID
  parent_uuid         VARCHAR   Parent message UUID
  type                VARCHAR   'user' or 'assistant'
  timestamp           VARCHAR   ISO 8601 timestamp string
  text                VARCHAR   Text content of the message (first text block for assistant)
  tool_count          BIGINT    Number of tool calls in this message (assistant only)
  model               VARCHAR   Model used (assistant messages only)
  agent_id            VARCHAR   Subagent id; NULL for main-loop rows
  is_sidechain        BOOLEAN   true if this row is from a subagent
  agent_type          VARCHAR   Subagent type from meta.json (e.g. 'Explore'); NULL for main loop
  workflow_id         VARCHAR   Workflow run id (wf_...) if spawned by a workflow, else NULL

tool_calls
----------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  message_uuid        VARCHAR   UUID of the containing assistant message
  tool_use_id         VARCHAR   Unique tool use ID (matches tool_results.tool_use_id)
  name                VARCHAR   Tool name (e.g. 'Bash', 'Read', 'Edit', 'Skill')
  input               JSON      Tool input as JSON object
  timestamp           VARCHAR   ISO 8601 timestamp string
  agent_id            VARCHAR   Subagent id; NULL for main-loop rows
  is_sidechain        BOOLEAN   true if this row is from a subagent
  agent_type          VARCHAR   Subagent type from meta.json (e.g. 'Explore'); NULL for main loop
  workflow_id         VARCHAR   Workflow run id (wf_...) if spawned by a workflow, else NULL

  Note: query input fields with json_extract_string(input, '$.field_name')
  Example: json_extract_string(input, '$.command') for Bash commands
  Note: advisor() calls appear here with name = 'advisor'

tool_results
------------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  tool_use_id         VARCHAR   Matches tool_calls.tool_use_id
  is_error            BOOLEAN   true if the tool call returned an error
  content             VARCHAR   Tool result content (text); advisor() results are unwrapped from {type, text}
  agent_id            VARCHAR   Subagent id; NULL for main-loop rows
  is_sidechain        BOOLEAN   true if this row is from a subagent
  agent_type          VARCHAR   Subagent type from meta.json (e.g. 'Explore'); NULL for main loop
  workflow_id         VARCHAR   Workflow run id (wf_...) if spawned by a workflow, else NULL

hook_events
-----------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  timestamp           VARCHAR   ISO 8601 timestamp string
  hook_event          VARCHAR   Hook event name (e.g. 'SessionStart', 'PreToolUse', 'PostToolUse')
  hook_name           VARCHAR   Specific hook identifier (e.g. 'SessionStart:startup', 'PreToolUse:Bash')
  attachment_type     VARCHAR   'hook_success' or 'hook_additional_context'
  content             VARCHAR   Injected text or stdout; one row per plugin for hook_additional_context
  content_size        BIGINT    Byte length of content

  Note: one row per hook_additional_context array element (fanned out per plugin's SessionStart contribution)

sessions
--------
  session_id          VARCHAR   Session identifier
  project             VARCHAR   Project name
  source              VARCHAR   Source the transcript came from (`main` or a cenv env name)
  harness             VARCHAR   The tool that produced the transcript (claude, codex, opencode)
  started_at          VARCHAR   Timestamp of first message
  ended_at            VARCHAR   Timestamp of last message
  message_count       BIGINT    Total messages in session
  tool_call_count     BIGINT    Total tool calls in session
  user_message_count  BIGINT    Number of user turns
  subagent_count      BIGINT    Distinct subagents spawned in this session
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

Subagent tool calls in a session:
  SELECT agent_type, name, COUNT(*) AS count
  FROM tool_calls
  WHERE session_id = 'SESSION_ID' AND is_sidechain
  GROUP BY agent_type, name
  ORDER BY count DESC;

Recent sessions:
  SELECT session_id, project, started_at, message_count, tool_call_count, first_user_message
  FROM sessions
  ORDER BY started_at DESC
  LIMIT 10;

SessionStart injection sizes by plugin:
  SELECT hook_name, content_size
  FROM hook_events
  WHERE attachment_type = 'hook_additional_context'
  ORDER BY content_size DESC;
"#;
