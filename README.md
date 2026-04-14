# cq

Query AI agent session transcripts with SQL. Built on DuckDB.

Currently supports Claude Code transcripts (`~/.claude/projects/`). Architected for future agent providers.

## Install

```bash
cargo install --path .
```

## Usage

```bash
# List recent sessions
cq sessions
cq sessions --project pickleton --since 7d

# Query tool usage
cq tools                                 # summary (counts by tool name)
cq tools Bash                            # all Bash invocations
cq tools Skill --grep "sanitation"       # filter on input content
cq tools --errors                        # tool calls that returned errors

# Search messages
cq messages --type user --grep "docker"

# Raw SQL (the real power)
cq sql "SELECT name, count(*) AS n FROM tool_calls GROUP BY 1 ORDER BY 2 DESC"
cq sql "SELECT json_extract_string(input, '$.command') AS cmd FROM tool_calls WHERE name='Bash' LIMIT 10"

# View schemas and examples
cq schema                    # all views
cq schema tool_calls         # one view with column details
cq schema --examples         # common query cookbook
```

## Common flags

| Flag | Short | Description |
|------|-------|-------------|
| `--project <name>` | `-p` | Scope to a project (substring match) |
| `--session <id>` | `-s` | Scope to a session (UUID prefix match) |
| `--since <duration>` | | Time filter: `7d`, `24h`, `30m` |
| `--json` | | Output as JSON instead of table |
| `--limit <n>` | | Max results (default: 50) |

## Views

Four SQL views are available for querying:

- **messages**: one row per conversation turn (user/assistant)
- **tool_calls**: one row per tool invocation with input as JSON
- **tool_results**: one row per tool response with error flag
- **sessions**: aggregated session metrics

Run `cq schema` for full column details. Run `cq schema --examples` for a query cookbook.

## For agents

`cq schema` and `cq schema --examples` are designed to be read by agents to construct queries. Use `--json` for machine-readable output.
