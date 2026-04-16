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

Or just write SQL directly, because sometimes that's the move:

```bash
$ cq sql "SELECT name, count(*) n FROM tool_calls GROUP BY 1 ORDER BY 2 DESC LIMIT 10"
```

The `schema` command shows all available views and columns, and `schema --examples` has a query cookbook to get you started:

```bash
$ cq schema --examples
```

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
| `--json` | | JSON output instead of tables |
| `--limit <n>` | | Max results (default: 50) |

## Views

Four SQL views, all queryable with `cq sql`:

- **sessions**: one row per session with timestamps, message counts, tool counts
- **messages**: one row per conversation turn (user or assistant)
- **tool_calls**: one row per tool invocation, with input as queryable JSON
- **tool_results**: one row per tool response, with an error flag

Run `cq schema` for full column details.

## Use cases

For deeper examples of what you can dig up, see [docs/use-cases.md](docs/use-cases.md). Things like finding skills that never fire, auditing which tool calls burn the most context, and catching silent failures that look fine from the outside.

## For agents

`cq schema` and `cq schema --examples` are designed to be consumed by AI agents building their own queries. Pair with `--json` for machine-readable output.

## License

MIT
