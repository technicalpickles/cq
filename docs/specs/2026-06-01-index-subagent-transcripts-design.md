# Index subagent transcripts

Status: approved (design)
Date: 2026-06-01

## Problem

cq is blind to all subagent work. It indexes only top-level session transcripts
(`~/.claude/projects/<project>/<session>.jsonl`) and never recurses into the
`<session>/subagents/` subdirectory where subagent transcripts live.

Confirmed against session `725b3651-1211-4a33-a51d-d02d307d8791` (a workflow run):
cq reports the parent session normally (main-loop messages and tool calls only),
but none of the 37 subagents' ~370 tool calls or messages are queryable. The only
trace of subagent work today is the `<task-notification>` user message that
delivers an aggregated result back to the parent main loop.

This affects every kind of subagent, not just PocketFlow workflows. Across
`~/.claude/projects` there are ~595 `agent-*.jsonl` files under 55 parent sessions:
~558 plain Task/Agent subagents at `<session>/subagents/agent-<id>.jsonl`, and ~37
workflow subagents nested at `<session>/subagents/workflows/wf_<id>/agent-<id>.jsonl`.

## On-disk facts

Both kinds of subagent transcript:

- carry `isSidechain: true`
- carry `sessionId` equal to the **parent** session id (their own identity is `agentId`)
- carry `cwd`, so project attribution still works through `file_registry`

Sidecar and ledger files live alongside the transcripts:

- `agent-<id>.meta.json` carries `{"agentType": "...", ...}` (e.g. `Explore`,
  `general-purpose`). Plain subagents also include `description` and `toolUseId`;
  workflow subagents carry only `agentType`.
- Workflow runs add a `journal.jsonl` (a started/result ledger keyed by content
  hash for resume). This is **not** a transcript and must be excluded from indexing.

## Goals

1. Index subagent transcripts so their messages, tool calls, and tool results
   become queryable.
2. Tag every row so a user can include, exclude, or focus subagents at will,
   both through the top-level commands and through raw SQL.
3. Keep the `sessions` overview clean: one row per real session, main-loop counts
   undistorted by subagent volume.
4. Surface `agentType` and the workflow run id where they exist.
5. Keep subagent transcripts fresh in the default (Auto) sync path, not only on
   `--reindex`.

## Non-goals

- No new CLI mode or flag for including/excluding subagents. The tag columns do
  the slicing (see "Why no flag").
- No promotion of rare per-record fields (`slug`, `entrypoint`, `gitBranch`) to
  columns. They remain reachable through the `raw_records` escape hatch.
- No change to how the `<task-notification>` parent message is rendered.

## Data model

### Row-level views

`messages`, `tool_calls`, and `tool_results` each gain four tag columns. One
consistent convention: `NULL` means "not applicable / main loop" for the three
identity columns; `is_sidechain` is the one always-populated boolean.

| Column | Main loop | Subagent | Source |
|---|---|---|---|
| `session_id` | parent | parent (same) | `$.sessionId` (unchanged) |
| `is_sidechain` | `false` | `true` | `COALESCE(CAST($.isSidechain AS BOOLEAN), false)` |
| `agent_id` | `NULL` | `a966b24…` | `$.agentId` |
| `agent_type` | `NULL` | `Explore` / `general-purpose` | sidecar `.meta.json` via `file_registry` |
| `workflow_id` | `NULL` | `wf_bbeb…` or `NULL` | `NULLIF(regexp_extract(source_file, 'workflows/(wf_[^/]+)/', 1), '')` |

Subagents keep the parent `session_id`, so `WHERE session_id = 'P'` returns the
session and all of its subagents, while `... AND NOT is_sidechain` narrows to the
main loop. `agent_id IS NULL` is equivalent to `NOT is_sidechain` in practice; both
are kept because each reads naturally for a different intent (is it a subagent vs
which subagent). `workflow_id` is NULL for plain subagents and for main-loop rows;
it is populated only for workflow-spawned agents.

### sessions view

Counts (`message_count`, `tool_call_count`, `user_message_count`, `first_user_message`,
`started_at`, `ended_at`) are computed over main-loop rows only (`WHERE NOT
is_sidechain`), so the overview stays honest and one-row-per-session.

One new column, `subagent_count` = `COUNT(DISTINCT agent_id)` for that session
(across all rows, sidechain or not). It is a "there is hidden depth here" signal:
a session that fanned out 37 agents reads differently from one that did not,
without inflating the main counts. Detailed per-agent rollups remain a `GROUP BY
agent_id` away in the row-level views.

## SQL surface

Every include/exclude/group decision is expressible without a flag:

```sql
WHERE NOT is_sidechain          -- main loop only (≡ agent_id IS NULL)
WHERE is_sidechain              -- all subagents
WHERE agent_id = 'a966b24…'     -- one specific subagent
WHERE agent_type = 'Explore'    -- all Explore agents
WHERE workflow_id IS NOT NULL   -- only workflow-spawned agents
WHERE workflow_id = 'wf_bbeb…'  -- one workflow run
WHERE session_id = 'P'          -- a session AND all its subagents
WHERE session_id = 'P' AND NOT is_sidechain   -- that session, main loop only
GROUP BY agent_id, agent_type   -- per-agent rollup
```

Rare fields stay reachable through `raw_records`:

```sql
SELECT json_extract_string(json, '$.slug') FROM raw_records WHERE …
```

### Why no flag

There are two kinds of cq query and they want opposite defaults:

- **Content search** (`cq tools Bash`, `cq messages`): the user wants the hit
  regardless of whether it came from the main loop, a Task subagent, or a workflow
  agent. Excluding subagents by default would hide the very thing being searched
  for. So subagent rows are included by default in the row-level commands.
- **Overview** (`cq sessions`): the boundary matters, so `sessions` stays
  main-loop-only.

The main/subagent/workflow split is therefore a dimension you slice on when you
care, not a mode chosen up front. The tag columns provide that slicing, so a
`--include-subagents` flag is unnecessary (YAGNI).

The one visible consequence: a no-filter `cq tools` global summary now reflects all
the work, so the numbers rise (workflow fan-outs especially). That is more
truthful, not a regression, since the work was simply invisible before.

## Discovery and indexing changes

1. **Recurse during scan.** `indexer.rs::scan_directory` and
   `claude_provider.rs::discover_files` collect `*.jsonl` recursively under each
   project directory, excluding any file named `journal.jsonl`. Top-level session
   files and nested `agent-*.jsonl` files both match; `.meta.json` files are
   naturally excluded (wrong extension).
2. **Capture `agent_type`.** `indexer.rs::index_files` reads the sibling
   `agent-<id>.meta.json` for each `agent-*.jsonl` file, parses `agentType`, and
   stores it in a new `file_registry.agent_type` column alongside `cwd`. The view
   exposes it via a subquery on `file_registry`, mirroring `PROJECT_EXPR`.
3. **`workflow_id` is path-derived.** Extracted in the view from `source_file`; no
   indexer change needed.

## Auto-sync freshness

`indexer.rs::max_dir_mtime` currently stats only the top-level project directory.
A subagent file written under `<session>/subagents/` does not bump that mtime, so
the Auto fast-path would skip re-indexing and miss new subagent transcripts until
the next `--reindex`.

Fix: make `max_dir_mtime` take the max mtime over **all** directories in scope
recursively (project dirs, session dirs, `subagents/`, and `workflows/wf_*/`),
rather than special-casing one level. Creating any new transcript file bumps its
immediate parent directory's mtime, so a recursive max uniformly catches a new
session, a new subagent, or a new workflow agent.

### Measured cost (this corpus: 45 project dirs, 534 sessions, 636 agent files)

- Stat'ing all 179 session + `subagents/` dirs: ~3.5 ms.
- A full recursive walk of the entire tree: ~20-30 ms.

The tree is hundreds of directories, not millions, so the deepened walk is
single-digit to low-tens of milliseconds. There is no meaningful budget the
shallow fast-path was protecting; the expensive part of a real sync is the DuckDB
reparse of changed files, which the mtime gate still avoids when nothing changed.

### Append-blindness is pre-existing, not introduced

Measured directory-mtime behavior on this filesystem:

| Action | Bumps parent dir mtime? |
|---|---|
| Append to existing file | no |
| Create a new file | yes (immediate parent only) |
| Create a new subdir | yes (immediate parent only) |

Appending to a file bumps no directory's mtime at any level, so the current
top-level fast-path **already** misses growth of an existing session file; it only
catches newly-created files. The recursive `max_dir_mtime` inherits exactly this
property: it catches a new `agent-*.jsonl` appearing (the common case when a
workflow fans out) and does not catch pure appends, the same as today for the main
session. `--reindex` (Force mode) remains the way to force a full re-parse.

## Cache migration

Adding `file_registry.agent_type` is a schema change. Bump
`cache.rs::SCHEMA_VERSION` from 2 to 3 and add the column to the `CREATE TABLE
file_registry` statement. `rebuild()` already drops and recreates all tables on a
version mismatch, so existing caches rebuild automatically on first run.

## Change surface

Grounded in the "Keeping docs in sync" table in `docs/cli-ux-conventions.md`:

| Area | File(s) | Change |
|---|---|---|
| Scan recursion | `src/indexer.rs`, `src/claude_provider.rs` | recurse, exclude `journal.jsonl` |
| Auto-sync mtime | `src/indexer.rs` | deepen `max_dir_mtime` |
| `agent_type` capture | `src/indexer.rs` | read `.meta.json`, store in registry |
| Cache schema | `src/cache.rs` | add column, bump `SCHEMA_VERSION` |
| Views | `src/views.rs` | add tag columns to row-level + empty views; `subagent_count` + `NOT is_sidechain` in `sessions` |
| View column docs | `src/commands/schema.rs` | document new columns + example query |
| Architecture/patterns | `CLAUDE.md` | note recursion in the indexer/provider description and Key patterns |
| Views bullets | `README.md` | reflect new columns in the Views section |
| Skill schema | `claude-plugin/skills/cq/SKILL.md` | mirror the schema additions |
| Tests | `tests/views_test.rs`, `tests/integration_test.rs`, fixtures | see below |

## Testing

View-level (`tests/views_test.rs`, SQL correctness over a manual file list):

- A `mixed_sidechain_session.jsonl` fixture containing main-loop and sidechain
  records sharing one `session_id`. Assert: all rows queryable in `messages` /
  `tool_calls` / `tool_results`; `agent_id` / `is_sidechain` populate correctly;
  `sessions` counts exclude sidechains; `subagent_count` reflects distinct agents;
  `WHERE agent_id IS NULL` isolates the main loop.
- A fixture placed at a nested `…/subagents/workflows/wf_test…/agent-*.jsonl` path
  to assert `workflow_id` extraction; a plain subagent path to assert NULL.
- A manual `file_registry` row carrying `agent_type` to assert the column flows
  through the view.
- Update the `setup_db` / `setup_db_multi` helpers to add `agent_type` to their
  `file_registry` table (the view references it).

Integration (`tests/integration_test.rs`, real index pipeline):

- A temp projects dir with a top-level session, a `<session>/subagents/agent-*.jsonl`
  + `.meta.json`, a `<session>/subagents/workflows/wf_*/agent-*.jsonl` + `.meta.json`,
  and a `journal.jsonl`. Assert: subagent tool calls appear; `agent_type` is
  populated from `.meta.json`; `journal.jsonl` rows are absent.

Manual verification:

- `cargo test`, then `cq --reindex` and confirm the 37 Explore subagents of session
  `725b3651` appear (`cq sql "SELECT agent_type, COUNT(*) FROM tool_calls WHERE
  session_id = '725b3651-…' AND is_sidechain GROUP BY agent_type"`).

## Known limitations

- `agent_type` depends on the sidecar `.meta.json`. If a future Claude Code version
  stops writing it, `agent_type` is NULL but `agent_id` / `is_sidechain` still work.
- `workflow_id` depends on the `workflows/wf_<id>/` path layout. A layout change
  would null it without affecting other columns.
