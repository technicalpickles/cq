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
cq search "dependency migration"         # ranked full-text search across messages
cq tools                                 # tool usage, ranked
cq messages --grep "docker" --since 7d   # search your history
cq tools --errors --result-grep "ECONNREFUSED"  # what failed, and why
cq hooks                                 # hook events, ranked (SessionStart, PreToolUse, ...)
cq sql "SELECT count(*) FROM messages"   # run anything
```

Run `cq schema --examples` for a query cookbook.

`cq search` uses BM25 relevance ranking with stemming, so a query such as
`cq search "dependency migrations"` can match messages containing “migrate a
dependency.” Results show the best-scoring message per session with a `matches`
count of how many messages in that session hit; `--all-matches` returns every
matching passage instead. This is lexical search rather than semantic search:
related ideas expressed with wholly different vocabulary still require
embeddings.

The first search downloads DuckDB's official `fts` extension into the cq cache
and builds a search index, which takes a while on a large corpus. After that the
index is allowed to lag up to five minutes behind your transcripts, because
rebuilding it costs several times a normal query and searches tend to come in
bursts. When cq serves a stale index it says so on stderr. Use `--reindex` to
rebuild on demand, `CQ_FTS_MAX_AGE` to change the window (`0s` refreshes on
every search, `1h` is lazier), and `--no-reindex` to skip the refresh entirely.

## Common flags

| Flag | Short | Description |
|------|-------|-------------|
| `--project <name>` | `-p` | Scope to a project (substring match) |
| `--session <id>` | `-s` | Scope to a session (UUID prefix match) |
| `--since <duration>` | | Time filter: `7d`, `24h`, `30m` |
| `--all` | | Remove inferred current-context scope (project, source, harness) |
| `--harness <name>` | | Target one harness (`claude` or `codex`; cannot combine with `--source`) |
| `--json` | | JSON output instead of tables; does not change scope |
| `--table` | | Aligned table with headers |
| `--no-color` | | Disable colored output |
| `--limit <n>` | | Max results (default: 50, 0 for unlimited) |
| `--offset <n>` | | Skip first N results |
| `--type <type>` | | Filter message results (`user` or `assistant`; messages, search) |
| `--all-matches` | | Every matching message instead of the best per session (search) |
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

Codex transcripts are discovered recursively from `$CODEX_HOME/sessions` (default: `~/.codex/sessions/`). Set `CQ_CODEX_SESSIONS_DIR` to override that location for cq only. Codex rows have `harness = 'codex'` and no `source`.

Built-in commands select the active harness by default: `harness = 'codex'` inside a Codex session and `harness = 'claude'` everywhere else. Codex selection skips Claude's automatic source scope. Use `--all` to span harnesses, or `--harness claude` / `--harness codex` to choose one explicitly. `--source` selects Claude rows only, so it cannot be combined with `--harness`. `cq sql` is raw SQL and ignores all scope flags.

## Views

Six SQL views, all queryable with `cq sql`:

- **sessions** - one row per session with timestamps, message counts, tool counts (main-loop only), plus a `subagent_count`
- **messages** - one row per conversation turn (user or assistant)
- **tool_calls** - one row per tool invocation, with input as queryable JSON
- **tool_results** - one row per tool response, with an error flag
- **hook_events** - one row per hook injection - SessionStart context, PreToolUse/PostToolUse output - fanned out per plugin for `hook_additional_context` records
- **agents** - one row per subagent lane (Claude-only), with its type, description, parent tool call, and spawn depth

Every view includes a `harness` column (`claude` or `codex`). Subagent activity is indexed for Claude Code: `messages`, `tool_calls`, and `tool_results` carry `is_sidechain`, `agent_id`, `agent_type`, and `workflow_id` so you can include, exclude, or focus subagents. `cq sessions` stays main-loop-only.

Run `cq schema` for full column details.

## Trace

`cq trace --session <id>` renders a session as a terminal waterfall: one row per lane (main loop first, then subagents by first activity), duration bars scaled to your terminal width, and a header that always states the time-per-column scale. Tool execution is often a minority of wall clock: one real session ran 344.9 minutes with only 169.3 minutes (49%) inside tool spans, the rest spent waiting on you or on the model.

```
$ cq trace --session a1b2c3d4
7 spans  3 lanes  1.7 min wall  [72 cols = 1.4 s/col]
1.0 min of tool work across 3 lanes   blocked on you 1.0 min
main                  4 ████████████████████████████                          ██
sub1                  2       ███████████████
sub2                  1          █
```

Use `--from`/`--to` to zoom into a slice of the session, as an offset from session start (`+12m`, `+90s`, `+2h`) or an absolute ISO timestamp:

```bash
cq trace --session <id> --from +12m --to +17m
```

Add `--format <FORMAT>` to emit a trace file on stdout instead of the waterfall `[valid: waterfall, perfetto, firefox-profiler]`:

```bash
cq trace --session <id> --format firefox-profiler > profile.json
cq trace --session <id> --format perfetto > trace.json
```

`--format firefox-profiler` emits Firefox Profiler's native processed-profile JSON directly — open it at [profiler.firefox.com](https://profiler.firefox.com/) for real per-category colors in the Marker Chart (Firefox Profiler's own Chrome Trace importer grays every marker out). `--format perfetto` emits Chrome Trace Event JSON, for opening in [Perfetto](https://ui.perfetto.dev/) or Speedscope; it also still loads in Firefox Profiler, just without native colors. The old `--perfetto` boolean flag is a deprecated alias for `--format perfetto` and still works, but new scripts should use `--format`.

Add `--open` to skip the file entirely and jump straight to the browser:

```bash
cq trace --session <id> --open                    # implies --format firefox-profiler
cq trace --session <id> --format perfetto --open
```

`--open` serves the trace from a local httpd and launches your OS browser straight to Firefox Profiler or Perfetto, instead of printing or saving JSON — the httpd only binds `127.0.0.1`, and the hosted viewer's own network traffic is just its static JS/CSS, so the trace data itself never leaves your machine unless you explicitly click "Share" inside the viewer. It's not valid with `--format waterfall` or `--json`, since neither produces a file a browser can load. The server runs in the foreground and blocks the terminal until you hit Ctrl-C; that's intentional, not a hang. Without `--open`, running `--format firefox-profiler`/`--format perfetto` on an interactive terminal writes to a deterministic tmp file and prints its path instead of dumping raw JSON at you; piped or redirected output is unaffected.

The local server binds port 9001 by default (`--port` overrides it, and is only valid alongside `--open`) — that's not arbitrary: `ui.perfetto.dev`'s own Content-Security-Policy only allows local connections to that exact port. Firefox Profiler has no such restriction, but both formats default to 9001 anyway so there's one port to remember. If something else already holds 9001, pass `--port` to pick a free one.

`--open --format firefox-profiler` opens your OS default browser, which needs to be Firefox or Chrome: Safari has its own limitation that blocks importing local profiles into Firefox Profiler entirely (a Safari restriction, not a cq bug), and refuses with an on-page error instead of loading the trace.

Tool spans and "blocked on you" gaps carry a compact `args.detail` string too (the tool's input, or the message that ended the gap, truncated at 200 chars) — Firefox Profiler renders it straight into the Marker Chart, Marker Table, and tooltip with no click needed; Perfetto shows the full `args` object regardless.

The global `--json` flag returns span rows instead of either renderer: one object per paired tool call, with `lane`, `duration_ms`, and `is_error`.

## Bundle

`cq bundle --session <id>` packages one session's raw transcript files -- the
main JSONL, any subagent JSONL (including nested workflow agents) and their
`.meta.json` sidecars, and best-effort copies of any `persistedOutputPath`
sidecars the records point to -- into a single zip with a `manifest.json`.
Useful for archiving a session before it rotates out of
`~/.claude/projects/`, attaching one to a bug report, or handing raw JSONL to
another tool without re-deriving cq's own file discovery by hand.

```
$ cq bundle --session a1b2c3d4
Wrote ./session-a1b2c3d4-0000-4000-8000-000000000001.zip (5 files, 0 sidecars, 3214 bytes)
```

```
cq bundle --session <id> -o ~/Desktop/session.zip   # explicit output path
```

## Use cases

For deeper examples of what you can dig up, see [docs/use-cases.md](docs/use-cases.md). Skill activation gaps, silent failures that look fine from the outside, context budget analysis across tool calls.

## Use with Claude Code

cq ships a Claude Code plugin that teaches Claude when and how to query your session history. Install it from the [`pickled-claude-plugins`](https://github.com/technicalpickles/pickled-claude-plugins) marketplace and Claude will reach for cq automatically when you ask about past sessions.

See [`claude-plugin/README.md`](claude-plugin/README.md) for details.

## For agents

`cq schema` and `cq schema --examples` are designed to be consumed by AI agents building their own queries. Pair with `--json` for machine-readable output.

## License

MIT
