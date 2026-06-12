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
- `--source <NAME>` - Scope to a named source (`main`, or a cenv env name); use `--all` to span every source. Every row carries a `source` column.
- `--all` - Show all projects and all sources (disable auto-scoping)
- `--since <DURATION>` - Time filter (e.g. `7d`, `24h`, `30m`); applies to `sessions`/`tools`/`messages`, not `cq sql` (raw SQL ignores scope flags)
- `--json` - Machine-readable JSON output
- `--table` - Aligned table with header
- `--limit <N>` - Max results (default 50, 0 for unlimited)
- `--offset <N>` - Skip first N results (for pagination)

## Subcommand Flags

- **sessions**: `--grep` (filter by content)
- **tools**: `--grep` (filter inputs), `--errors` (errors only), `--fields` (extract input fields as columns)
- **messages**: `--type user|assistant`, `--grep`

## View Schemas

**sessions**: session_id, project, source, started_at, ended_at, message_count, tool_call_count, user_message_count, subagent_count, first_user_message (counts are main-loop only)

**messages**: session_id, project, source, uuid, parent_uuid, type, timestamp, text, tool_count, model, agent_id, is_sidechain, agent_type, workflow_id

**tool_calls**: session_id, project, source, message_uuid, tool_use_id, name, input (JSON), timestamp, agent_id, is_sidechain, agent_type, workflow_id

**tool_results**: session_id, project, source, tool_use_id, is_error, content, agent_id, is_sidechain, agent_type, workflow_id

Subagent rows carry the parent `session_id`. Filter with `WHERE NOT is_sidechain` (main loop), `WHERE is_sidechain` (subagents), `WHERE agent_type = 'Explore'`, or `WHERE workflow_id IS NOT NULL`.

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
- Do not suppress stderr on a cq pipe (`cq ... --json 2>/dev/null | ...`). If the SQL errors, the error goes to stderr and stdout is empty, so the downstream consumer (e.g. `python3 -c json.load`) dies with a confusing `Expecting value: line 1 column 1` instead of the real cq error. Let stderr through, or redirect to a temp file and check it.
- The convenience subcommands (`sessions`, `tools`, `messages`) cover most needs. Reach for `cq sql` when you need joins or aggregations across views.
- `--since` filters the convenience subcommands (`sessions`, `tools`, `messages`), but NOT `cq sql`: raw SQL runs verbatim and ignores `--since`/`--project`/`--session`. On `cq sql`, filter time with a string comparison instead, e.g. `WHERE timestamp >= '2026-05-28'`. cq uses DuckDB, not SQLite, so SQLite functions like `datetime()` will not work. Do NOT reach for `WHERE timestamp > now() - INTERVAL N DAY`: it errors (see Tips below).
- `cq` auto-scopes to the current directory's project. The scope hint shows which path is being matched.
- Use `--project <name>` to query a different project (substring match, searches all project directories).
- Use `--all` to disable auto-scoping entirely and query across all projects.
- When searching for work done in a different repo (e.g. karafka sessions while in pickleton), use `--project <name>` or `--all`. Auto-scoping only matches sessions from the current directory's project.
- `cq projects` always shows all projects regardless of auto-scoping, so you can see what's available.

## Tips for `cq sql`

- Use `<>` instead of `!=` in SQL queries. The shell can mangle `!=` into `\!=`, causing DuckDB parse errors.
- Avoid shell metacharacters in query strings where possible. Single-quote the SQL string to prevent expansion.

### Timestamps are strings

The `timestamp`, `started_at`, and `ended_at` columns are `VARCHAR` ISO 8601 strings (e.g. `2026-05-21T17:48:17.818Z`), not DuckDB timestamps. The format is fixed-width UTC, so lexical order equals chronological order. That means most of what you want already works on the strings:

- **Time windows:** filter on the string directly, e.g. `WHERE timestamp >= '2026-05-28'` (lexical order is chronological). `--since` does NOT apply to `cq sql`; it only filters the `sessions`/`tools`/`messages` subcommands.
- **Explicit ranges:** compare against a string literal, e.g. `WHERE timestamp >= '2026-05-28'`. Comparing against a `TIMESTAMP '...'` literal fails with `Cannot compare VARCHAR and TIMESTAMP`.
- **Sort / bucket by day:** `ORDER BY timestamp` works; `substr(timestamp, 1, 10)` gives the date for `GROUP BY`.

What does NOT work on the raw columns:

- `now() - INTERVAL N DAY` errors with `No function matches '-(TIMESTAMP WITH TIME ZONE, INTERVAL)'`. Compare against a string literal, or cast with `now()::TIMESTAMP - INTERVAL N DAY`. cq surfaces this hint automatically when the query fails.
- `strftime(...)`, `date_trunc(...)`, `extract(...)` need a real timestamp. For genuine per-row date math (e.g. correlating events within a window), cast inline: `timestamp::TIMESTAMP`.

## Error Recovery

When a query fails, returns unexpected results, or a column/view doesn't exist:

1. Run `cq schema` to check the current view definitions and example queries. This skill's inline schemas may be out of date with the installed version of cq.
2. Do not guess at column names or view structures. Check `cq schema` first.
3. If `cq schema` shows different schemas than what's documented here, trust `cq schema` and use what it shows.

If cq fails with `Cannot open file ".../cq/index.duckdb": Operation not permitted`, the cache DB is outside the agent sandbox's writable paths. Retry the command with the sandbox disabled (Claude Code: `dangerouslyDisableSandbox: true`).
