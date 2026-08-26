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

**TITLE CARD:** SQL for your AI coding sessions.

cq indexes Claude Code and Codex JSONL session transcripts into a local [DuckDB](https://duckdb.org/) cache at `~/.cache/cq/` and gives you SQL views to query against. Incremental sync keeps it fresh on each run, so you only pay the full-parse cost once. Built-in commands handle the common stuff, and `cq sql` lets you run whatever you want.

## Install

### Prebuilt binary

Grab the archive for your platform from the [latest release](https://github.com/technicalpickles/cq/releases/latest), extract it, and put `cq` on your `PATH`. Builds are published for macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64).

### From source

Requires [Rust](https://rustup.rs/).

```bash
cargo install --git https://github.com/technicalpickles/cq
```

## Quick start

```bash
cq sessions                              # your recent sessions
cq tools                                 # tool usage, ranked
cq messages --grep "docker" --since 7d   # search your history
cq tools --errors --result-grep "ECONNREFUSED"  # what failed, and why
cq hooks                                 # hook events, ranked (SessionStart, PreToolUse, ...)
cq sql "SELECT count(*) FROM messages"   # run anything
```

Run `cq schema --examples` for a query cookbook.

## Common flags

| Flag | Short | Description |
|------|-------|-------------|
| `--project <name>` | `-p` | Scope to a project (substring match) |
| `--session <id>` | `-s` | Scope to a session (UUID prefix match) |
| `--since <duration>` | | Time filter: `7d`, `24h`, `30m` |
| `--all` | | Disable automatic project, source, and harness scoping; span every harness |
| `--harness <name>` | | Target one harness (`claude` or `codex`; cannot combine with `--source`) |
| `--json` | | JSON output instead of tables |
| `--table` | | Aligned table with headers |
| `--no-color` | | Disable colored output |
| `--limit <n>` | | Max results (default: 50, 0 for unlimited) |
| `--offset <n>` | | Skip first N results |
| `--version` | `-V` | Print the cq version |
| `-A N` | | Show N messages after each match (messages, tools) |
| `-B N` | | Show N messages before each match (messages, tools) |
| `-C N` | | Shorthand for `-A N -B N` (messages, tools) |

## Claude sources

cq indexes multiple transcript **sources**: the main config dir (`~/.claude/projects`, or `$CQ_PROJECTS_DIR`) plus every cenv env's `projects/` (discovered under `$CENV_BASE`, default `~/.local/share/cenv`). A cenv env is one kind of source; cq never shells out to cenv.

When Claude is the active harness, cq scopes to the **active** Claude source (the one matching `$CLAUDE_CONFIG_DIR`, else `main`), mirroring how it auto-scopes to the current directory. Use `--all` to span every source and `--source <name>` to target one. Every row carries a `source` column; compose with `--since` to weigh results by age.

| Flag | What it does |
|------|-------------|
| _(default)_ | Scope to the active source |
| `--source <name>` | Target one source by name (e.g. `main`, or a cenv env name) |
| `--all` | Span all sources |

## Codex sessions

Codex transcripts are discovered recursively from `~/.codex/sessions/`. Set `CQ_CODEX_SESSIONS_DIR` to point cq at a different session root. Codex rows have `harness = 'codex'` and no `source`.

Built-in commands select the active harness by default: `harness = 'codex'` inside a Codex session and `harness = 'claude'` everywhere else. Codex selection skips Claude's automatic source scope. Use `--all` to span harnesses, or `--harness claude` / `--harness codex` to choose one explicitly. `--source` selects Claude rows only, so it cannot be combined with `--harness`. `cq sql` is raw SQL and ignores all scope flags.

## Views

Five SQL views, all queryable with `cq sql`:

- **sessions** - one row per session with timestamps, message counts, tool counts (main-loop only), plus a `subagent_count`
- **messages** - one row per conversation turn (user or assistant)
- **tool_calls** - one row per tool invocation, with input as queryable JSON
- **tool_results** - one row per tool response, with an error flag
- **hook_events** - one row per hook injection - SessionStart context, PreToolUse/PostToolUse output - fanned out per plugin for `hook_additional_context` records

Every view includes a `harness` column (`claude` or `codex`). Subagent activity is indexed for Claude Code: `messages`, `tool_calls`, and `tool_results` carry `is_sidechain`, `agent_id`, `agent_type`, and `workflow_id` so you can include, exclude, or focus subagents. `cq sessions` stays main-loop-only.

Run `cq schema` for full column details.

## Use cases

For deeper examples of what you can dig up, see [docs/use-cases.md](docs/use-cases.md). Skill activation gaps, silent failures that look fine from the outside, context budget analysis across tool calls.

## Use with Claude Code

cq ships a Claude Code plugin that teaches Claude when and how to query your session history. Install it from the [`pickled-claude-plugins`](https://github.com/technicalpickles/pickled-claude-plugins) marketplace and Claude will reach for cq automatically when you ask about past sessions.

See [`claude-plugin/README.md`](claude-plugin/README.md) for details.

## For agents

`cq schema` and `cq schema --examples` are designed to be consumed by AI agents building their own queries. Pair with `--json` for machine-readable output.

## License

MIT
