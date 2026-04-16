# cq

**INT. TERMINAL**

Hundreds of Claude Code sessions. Thousands of tool calls.
You've never looked at the logs.

```
$ cq tools
Read             ██████████████████████████████  1847
Bash             ████████████████████████████    1623
Edit             ███████████████                  982
Write            █████                            341
Grep             ████                             298
```

*1600 Bash calls. What am I even running?*

**TIGHT ON** the commands.

```
$ cq tools Bash --fields command --limit 5
c82e9d4c  Bash  cargo test
c82e9d4c  Bash  git diff --stat
bfc27bd2  Bash  docker compose up -d
bfc27bd2  Bash  git commit -m "fix: resolve session timeout"
a1f3e890  Bash  psql -c "SELECT count(*) FROM users"
```

*Wait, I set up a git commit skill for that. Is it even firing? The commits would still go through Bash either way...*

**CUT TO** Claude, investigating a hunch.

```
$ cq sql "
WITH commit_sessions AS (
  SELECT DISTINCT session_id FROM tool_calls
  WHERE name = 'Bash'
    AND json_extract_string(input, '$.command') LIKE '%git commit%'
),
skill_sessions AS (
  SELECT DISTINCT session_id FROM tool_calls
  WHERE name = 'Skill'
    AND json_extract_string(input, '$.skill') LIKE '%commit%'
)
SELECT
  (SELECT count(*) FROM commit_sessions) as total_sessions,
  (SELECT count(*) FROM skill_sessions) as used_skill,
  (SELECT count(*) FROM commit_sessions)
    - (SELECT count(*) FROM skill_sessions) as bypassed
" --since 7d
```

```
total_sessions  used_skill  bypassed
──────────────  ──────────  ────────
           168          16       152
```

**152 sessions. The skill was right there. Nobody called it.**

---

**TITLE CARD:** SQL for your Claude Code sessions.

cq loads Claude Code's JSONL session transcripts into an in-memory [DuckDB](https://duckdb.org/) instance and gives you SQL views to query against. Built-in commands handle the common stuff, and `cq sql` lets you run whatever you want.

## Install

Requires [Rust](https://rustup.rs/).

```bash
cargo install --git https://github.com/technicalpickles/cq
```

## Quick start

```bash
cq sessions                              # your recent sessions
cq tools                                 # tool usage, ranked
cq messages --grep "docker" --since 7d   # search your history
cq sql "SELECT count(*) FROM messages"   # run anything
```

Run `cq schema --examples` for a query cookbook.

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

- **sessions** - one row per session with timestamps, message counts, tool counts
- **messages** - one row per conversation turn (user or assistant)
- **tool_calls** - one row per tool invocation, with input as queryable JSON
- **tool_results** - one row per tool response, with an error flag

Run `cq schema` for full column details.

## Use cases

For deeper examples of what you can dig up, see [docs/use-cases.md](docs/use-cases.md). Skill activation gaps, silent failures that look fine from the outside, context budget analysis across tool calls.

## For agents

`cq schema` and `cq schema --examples` are designed to be consumed by AI agents building their own queries. Pair with `--json` for machine-readable output.

## License

MIT
