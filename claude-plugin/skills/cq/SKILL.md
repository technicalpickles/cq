---
name: cq
description: Query past Claude Code sessions using the cq CLI (SQL over session transcripts via DuckDB). You MUST use this skill whenever the user asks about their Claude session history, tool usage, errors, or past commands. This includes questions like "what tools have I used", "how many sessions today", "what was that command I ran", "show me errors", "which files have I been editing", "what skills get invoked", or any question about what happened in previous Claude Code sessions. Also use when doing meta-work (skill development, plugin analysis, workflow review) where querying past session data would be informative. If the user mentions sessions, transcripts, tool calls, or wants to recall something from a past conversation, this is the skill to use.
user_invocable: true
---

# cq: Query Claude Code Sessions

`cq` indexes Claude Code session transcripts into DuckDB and exposes them as SQL views. It reads JSONL files from `~/.claude/projects/`, caches them incrementally, and lets you query sessions, messages, tool calls, and tool results.

## Subcommands

| Command | Purpose |
|---------|---------|
| `cq sessions` | List sessions with metadata |
| `cq tools [NAME]` | Query tool calls, optionally filtered by tool name |
| `cq messages` | Query user/assistant messages |
| `cq projects` | Summarize projects by session/message/tool counts |
| `cq sql "<QUERY>"` | Run raw SQL against the views |
| `cq schema` | View schemas and example queries (source of truth) |

## Global Flags

- `--project <NAME>` - Substring match on project name
- `--session <ID>` - Session ID (full UUID required, validates format)
- `--all` - Show all projects (disable auto-scoping to current directory)
- `--since <DURATION>` - Time filter (e.g. `7d`, `24h`, `30m`)
- `--json` - Machine-readable JSON output
- `--table` - Aligned table with header
- `--limit <N>` - Max results (default 50, 0 for unlimited)
- `--offset <N>` - Skip first N results (for pagination)

## Subcommand Flags

- **sessions**: `--grep` (filter by content)
- **tools**: `--grep` (filter inputs), `--errors` (errors only), `--fields` (extract input fields as columns)
- **messages**: `--type user|assistant`, `--grep`

## View Schemas

**sessions**: session_id, project, started_at, ended_at, message_count, tool_call_count, user_message_count, first_user_message

**messages**: session_id, project, uuid, parent_uuid, type, timestamp, text, tool_count, model

**tool_calls**: session_id, project, message_uuid, tool_use_id, name, input (JSON), timestamp

**tool_results**: session_id, project, tool_use_id, is_error, content

## Querying JSON Input Fields

Tool call inputs are stored as JSON. Extract specific fields with:

```sql
json_extract_string(input, '$.command')    -- Bash commands
json_extract_string(input, '$.file_path')  -- Read/Edit targets
json_extract_string(input, '$.skill')      -- Skill invocations
json_extract_string(input, '$.pattern')    -- Glob/Grep patterns
```

## Working With cq

- Use `--json` when parsing output programmatically or piping to other tools.
- The convenience subcommands (`sessions`, `tools`, `messages`) cover most needs. Reach for `cq sql` when you need joins or aggregations across views.
- `--since` applies to all subcommands including `cq sql`. Use it instead of writing time-filter clauses in SQL. cq uses DuckDB, not SQLite, so SQLite functions like `datetime()` will not work.
- `cq` auto-scopes to the current directory's project. The scope hint shows which path is being matched.
- Use `--project <name>` to query a different project (substring match, searches all project directories).
- Use `--all` to disable auto-scoping entirely and query across all projects.
- When searching for work done in a different repo (e.g. karafka sessions while in pickleton), use `--project <name>` or `--all`. Auto-scoping only matches sessions from the current directory's project.
- `cq projects` always shows all projects regardless of auto-scoping, so you can see what's available.

## Tips for `cq sql`

- Use `<>` instead of `!=` in SQL queries. The shell can mangle `!=` into `\!=`, causing DuckDB parse errors.
- Avoid shell metacharacters in query strings where possible. Single-quote the SQL string to prevent expansion.

## Error Recovery

When a query fails, returns unexpected results, or a column/view doesn't exist:

1. Run `cq schema` to check the current view definitions and example queries. This skill's inline schemas may be out of date with the installed version of cq.
2. Do not guess at column names or view structures. Check `cq schema` first.
3. If `cq schema` shows different schemas than what's documented here, trust `cq schema` and use what it shows.
