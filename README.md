# cq

SQL for your AI agent sessions.

Claude Code stores every conversation as JSONL transcripts in `~/.claude/projects/`. cq loads them into an in-memory [DuckDB](https://duckdb.org/) instance and gives you four SQL views to query against: `sessions`, `messages`, `tool_calls`, and `tool_results`. Built-in commands handle the common stuff, and `cq sql` lets you run whatever you want.

## What can you do with it?

Find out which tools you actually use:

```bash
$ cq tools
 name             | calls | pct
──────────────────┼───────┼─────
 Read             |  1847 | ████████████████████ 28.5%
 Bash             |  1623 | █████████████████ 25.0%
 Edit             |   982 | ██████████ 15.1%
 ...
```

Search your conversation history:

```bash
$ cq messages --type user --grep "docker" --since 7d
```

See what commands you've been running:

```bash
$ cq tools Bash --fields command --limit 10
```

Find tool calls that errored:

```bash
$ cq tools --errors --since 24h
```

See activity across all your projects:

```bash
$ cq projects --all
```

Or just write SQL directly, because sometimes that's the move:

```bash
$ cq sql "SELECT name, count(*) n FROM tool_calls GROUP BY 1 ORDER BY 2 DESC LIMIT 10"
```

`cq schema` shows all available views and columns. `cq schema --examples` has a query cookbook to get you started.

## Install

Requires [Rust](https://rustup.rs/).

```bash
cargo install --git https://github.com/technicalpickles/cq
```

## Common flags

| Flag | Short | Description |
|------|-------|-------------|
| `--project <name>` | `-p` | Scope to a project (substring match) |
| `--session <id>` | `-s` | Scope to a session (UUID prefix match) |
| `--since <duration>` | | Time filter: `7d`, `24h`, `30m` |
| `--all` | | Show all projects (disable auto-scoping) |
| `--json` | | JSON output instead of tables |
| `--table` | | Aligned table with headers |
| `--no-color` | | Disable colored output |
| `--limit <n>` | | Max results (default: 50, 0 for unlimited) |
| `--offset <n>` | | Skip first N results |

## Views

Four SQL views, all queryable with `cq sql`:

- **sessions**: one row per session with timestamps, message counts, tool counts
- **messages**: one row per conversation turn (user or assistant)
- **tool_calls**: one row per tool invocation, with input as queryable JSON
- **tool_results**: one row per tool response, with an error flag

Run `cq schema` for full column details.

**Tip:** The `project` column in SQL contains full decoded paths (e.g. `/Users/alice/myproject`), while built-in commands show short names. When filtering in raw SQL, use `LIKE` instead of `=`:

```sql
WHERE project LIKE '%myproject'
```

## Use cases

For deeper examples of what you can dig up, see [docs/use-cases.md](docs/use-cases.md). Things like finding skills that never fire, auditing which tool calls burn the most context, and catching silent failures that look fine from the outside.

## For agents

`cq schema` and `cq schema --examples` are designed to be consumed by AI agents building their own queries. Pair with `--json` for machine-readable output.

## License

MIT
