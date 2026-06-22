# opencode Source: Findings & Design Input

This document captures the schema mapping, integration analysis, and open design
questions for wiring opencode session data into cq's existing views.

## Status

Prototype complete. SQL queries validated against a live
`~/.local/share/opencode/opencode.db` (v1.17.5 format). Rust code artifacts
in `examples/opencode_spike.rs` and `tests/opencode_spike.rs` compile but
require ~3GB free disk to build from source.

## Schema mapping

### sessions view

| cq column | opencode source | Notes |
|-----------|----------------|-------|
| `session_id` | `session.id` | Different namespace (`ses_...` prefix) |
| `project` | `session.directory` | Real path; no encoding/decoding needed |
| `source` | constant `'opencode'` | |
| `started_at` | `session.time_created` (epoch ms) | Needs `/1000` and ISO 8601 format conversion |
| `ended_at` | `session.time_updated` (epoch ms) | Same conversion |
| `message_count` | COUNT from `message` table | |
| `tool_call_count` | COUNT of `part` rows with `data.type='tool'` | |
| `user_message_count` | COUNT of `message` rows with `data.role='user'` | |
| `subagent_count` | COUNT of `session` rows with `parent_id = s.id` | Different model: sub-agents are sessions not in-message |
| `first_user_message` | `session.title` | opencode sets title to the initial prompt |

Missing from opencode: nothing critical absent; `session.model` (JSON) is extra data available.

### messages view

| cq column | opencode source | Notes |
|-----------|----------------|-------|
| `session_id` | `message.session_id` | |
| `project` | via `session.directory` JOIN | |
| `source` | constant `'opencode'` | |
| `uuid` | `message.id` | Different namespace (`msg_...` prefix) |
| `parent_uuid` | `json_extract(message.data, '$.parentID')` | |
| `type` | `json_extract(message.data, '$.role')` | Values: `user`, `assistant` |
| `timestamp` | `message.time_created` (epoch ms) | ISO 8601 conversion needed |
| `text` | `part.data.text` where `part.data.type='text'` | Text NOT in message.data; lives in part table |
| `tool_count` | COUNT of `part` rows with `data.type='tool'` for this message | |
| `model` | `message.data.modelID` (assistant) OR `message.data.model.modelID` (user) | Different path by role |
| `agent_id` | NULL | Sub-agents are sessions, not in-message IDs |
| `is_sidechain` | false | Main loop only; sub-sessions handled via parent_id |
| `agent_type` | `json_extract(message.data, '$.agent')` | Values: `build`, `general`, etc. |
| `workflow_id` | NULL | opencode has no workflow concept |

Key gap: `text` for user messages comes from part rows (type='text'), not from the
message's own data column. The initial user prompt text is in `session.title` instead.

### tool_calls view

| cq column | opencode source | Notes |
|-----------|----------------|-------|
| `session_id` | `part.session_id` | |
| `project` | via `session.directory` JOIN | |
| `source` | constant `'opencode'` | |
| `message_uuid` | `part.message_id` | |
| `tool_use_id` | `json_extract(part.data, '$.callID')` | String (opencode) vs string (cq); join key |
| `name` | `json_extract(part.data, '$.tool')` | e.g. `bash`, `read`, `write`, `grep`, `task` |
| `input` | `json_extract(part.data, '$.state.input')` | JSON object, maps cleanly |
| `timestamp` | `part.time_created` (epoch ms) | |
| `agent_id` | NULL | |
| `is_sidechain` | false | |
| `agent_type` | via `message.data.agent` JOIN | |
| `workflow_id` | NULL | |

### tool_results view

| cq column | opencode source | Notes |
|-----------|----------------|-------|
| `session_id` | `part.session_id` | |
| `project` | via `session.directory` JOIN | |
| `source` | constant `'opencode'` | |
| `tool_use_id` | `json_extract(part.data, '$.callID')` | Same part row as tool_calls |
| `is_error` | `part.data.state.status != 'completed'` | 49 completed / 1 error in live data |
| `content` | `json_extract(part.data, '$.state.output')` | The stdout/result text |
| `agent_id` | NULL | |
| `is_sidechain` | false | |
| `agent_type` | via `message.data.agent` JOIN | |
| `workflow_id` | NULL | |

## Key structural difference: tool call/result co-location

In cq/Claude Code, a tool call (the request) and tool result (the response) are
separate JSONL records. They join on `tool_use_id`.

In opencode, both live in the SAME `part` row: `state.input` is the call,
`state.output` is the result. The `callID` field is the equivalent of `tool_use_id`.

This means:
- `tool_calls` and `tool_results` views over opencode produce the SAME row count.
- The join key (`callID` = `tool_use_id`) works correctly.
- No "pending tool call without result" rows exist in opencode (call and result are
  atomic from the DB perspective).

## Does the data map cleanly?

Yes, with three predictable wrinkles:

1. **Timestamp units.** opencode stores all times as epoch milliseconds (BIGINT).
   cq stores timestamps as ISO 8601 VARCHAR strings. A native provider needs to emit
   ISO 8601 from the SQL (DuckDB: `strftime(epoch_ms(t), format)`; SQLite:
   `datetime(t/1000, 'unixepoch')`).

2. **Text content is in parts, not messages.** The user message text field requires
   a subquery against the `part` table. The initial user prompt is in `session.title`
   rather than appearing as a separate text part (it's the session's title).

3. **Sub-agents are sessions, not in-message IDs.** cq uses `agent_id` (an in-message
   field) to identify sub-agent work; opencode creates a full child session with
   `parent_id` pointing to the parent. Mapping: `agent_id = NULL` everywhere,
   `subagent_count = COUNT(child sessions)`. The cq concepts `is_sidechain` and
   `workflow_id` have no equivalent in opencode.

No data is lost; the gaps (`agent_id`, `is_sidechain`, `workflow_id`) all NULL-out
cleanly.

## Timestamp conversion

opencode: `time_created INTEGER` (epoch milliseconds, e.g. `1781732636455`)

DuckDB conversion: `strftime(epoch_ms(time_created), '%Y-%m-%dT%H:%M:%SZ')`

SQLite conversion (for testing): `datetime(time_created/1000, 'unixepoch')`

cq currently stores timestamps as VARCHAR ISO 8601 strings. A native OpenCode
provider would need to emit the same format for string comparisons in the sessions
view (`MIN(timestamp)`, `MAX(timestamp)`) to work correctly.

## Integration path analysis

### Option (a): Converter materialization

Convert opencode sessions to Claude Code-compatible JSONL in a temp directory, then
let the existing `ClaudeProvider` index them.

**What this costs:**
- Write a one-shot converter (Rust or shell) that reads opencode.db and writes JSONL
  files in the format `<projects_dir>/<encoded-project>/<session-id>.jsonl`.
- The converter must translate each `(session, message, part)` triple into cq's
  existing JSON envelope: `{ "type": "user"|"assistant", "uuid": "...", "sessionId":
  "...", "timestamp": "...", "message": { "content": [...] } }`.
- The directory layout must match what `ClaudeProvider` expects.
- The converter output is ephemeral (temp dir) or persistent (synced dir).
- No changes to cq's core codebase.

**What it doesn't cost:**
- No changes to `main.rs` provider dispatch.
- No changes to `indexer.rs` or `views.rs`.
- Can be implemented entirely outside the cq codebase.

**Drawbacks:**
- Data fidelity: some opencode fields (cost, token breakdown, diffs) are not in
  cq's JSONL schema and would be dropped.
- Staleness: the converter must be re-run when opencode adds sessions.
- The produced JSONL is a translation artifact, not the real data.
- Tool call/result co-location needs careful splitting into separate JSONL records.

### Option (b): Native OpenCodeProvider

A new `OpenCodeProvider` that implements `TranscriptProvider` and reads opencode.db
directly, generating cq views that query the SQLite tables.

**What this costs:**
- A new provider struct in `src/opencode_provider.rs`.
- `register_views()` must create SQL views reading from the attached SQLite DB
  (DuckDB's `ATTACH ... (TYPE sqlite)` mechanism).
- The `main.rs` dispatch refactor: today `ClaudeProvider` is constructed directly
  (`~line 209`); the code does not use the `TranscriptProvider` trait polymorphically.
  A second provider forces this refactor (likely `Box<dyn TranscriptProvider>`).
- `indexer.rs` assumes `read_json` over JSONL files; the SQLite source breaks this
  assumption. The indexer would need a bypass or a separate "sqlite source" code
  path. The `file_registry` mtime/size model does not apply to a single DB file.
- `cache.rs` schema (`file_registry`) stores per-file mtime/size. A SQLite source
  would need a different freshness mechanism (e.g., check opencode.db mtime).
- Views that reference `source_file` (currently all four cq views do) would need
  alternatives for the non-JSONL case.

**What it doesn't cost:**
- No data fidelity loss; native SQL over the real tables.
- No converter maintenance.
- Automatically reflects new sessions.

### Recommendation

For the immediate spike/integration: **start with (a)**. The converter approach
lets you validate the schema mapping with zero changes to the cq codebase. The
JSONL output can be pointed at with `CQ_PROJECTS_DIR` in tests.

For the production integration: **(b) is the right long-term answer**, but only
after the `main.rs` polymorphic dispatch refactor is done as a separate PR. The
refactor is ~50 lines of straightforward Rust but it touches the cq startup path
and deserves its own review.

The two paths are not mutually exclusive: ship (a) as the cq v1.x opencode bridge,
then replace it with (b) in v2.

## SQLite source and the mtime cache model

cq's incremental cache uses `file_registry(file_path, mtime_ns, file_size)` to
decide what to re-parse. A SQLite source breaks this in two ways:

1. There's ONE file (`opencode.db`) not many JSONL files. The "file" is the whole DB.
2. opencode.db is append-only; a change in mtime means "there's new data," not "this
   whole file changed." The indexer would over-index (re-reading everything on any
   DB change) unless it tracks some other cursor (e.g., `MAX(time_updated)` from
   the session table, or the sqlite `wal` checkpoint sequence).

The cleanest approach for (b): store the DB path in `file_registry` with the opencode.db
mtime. When mtime changes, run a "delta scan" that queries `WHERE time_created > last_seen`.
This needs a new column in `file_registry` (e.g., `cursor BIGINT`) to store the last
`MAX(session.time_updated)` seen.

Option (a) sidesteps this entirely: the converter controls the JSONL files, which
have normal mtimes.

## Open questions for the design step

> **Resolved 2026-06-18** in a grill-with-docs design session. See the
> "Design resolutions" section below and `docs/adr/0001-opencode-via-live-attach.md`.
> The questions are kept here for the reasoning trail.

1. **Provider dispatch refactor scope.** How disruptive is the `main.rs` refactor
   to go from `ClaudeProvider` directly to `Box<dyn TranscriptProvider>`? Are there
   other callers that would also need updating?

2. **Sub-agent representation.** opencode's parent_id sub-sessions vs cq's in-message
   `agent_id`. For the production integration: should opencode sub-sessions be
   surfaced as first-class sessions (with `is_sidechain = true` on the parent's
   messages)? Or should they be flattened? The current prototype nulls both out.

3. **Source column in the cache.** `file_registry.source` was added for the
   multi-source PR. A SQLite source with no JSONL files needs a different key.
   What's the canonical "file path" for an opencode source row?

4. **Scope filtering.** `cq sessions --project .` scans the current directory
   against `project` in the sessions view. For opencode, `project = session.directory`
   (real path). The ILIKE match should work as-is. Verify this.

5. **Token / cost data.** opencode tracks `cost`, `tokens_input/output/reasoning/
   cache_read/cache_write` per session. cq currently has no cost/tokens in any view.
   Worth adding to the sessions view in a future PR?

6. **DuckDB sqlite extension bundling.** The prototype uses `INSTALL sqlite` which
   downloads the extension from DuckDB's servers on first use. For a production
   integration, we'd want this bundled or pre-installed. The `libduckdb-sys` crate
   has a `duckdb-loadable-extension` feature but not a bundled sqlite scanner.
   How does the extension get delivered?

## Design resolutions (2026-06-18)

Vocabulary settled in `CONTEXT.md`: **Harness** (Claude Code, opencode) is read by
a **Provider** (`ClaudeProvider`, `OpenCodeProvider`); **Source** keeps its narrow
within-Claude meaning (main + cenv JSONL roots, selected by `--source`). opencode
is a new Provider, not a new Source. Architecture recorded in
`docs/adr/0001-opencode-via-live-attach.md`.

**Core architecture: live ATTACH, never cached.** `OpenCodeProvider` queries
opencode.db live via DuckDB `ATTACH ... (TYPE sqlite, READ_ONLY)`; rows are never
written to `raw_records`/`file_registry`. This deletes the mtime-cursor problem
(Q3's append-only-DB headache) entirely — a no-index provider is always fresh.

**Ship in two PRs:**

- **PR 1 — Claude-only no-op refactor.** Build a provider collection in `main.rs`
  instead of the hardcoded `ClaudeProvider::new()`. Generalize the trait to two
  phases: `prepare(conn)` (Claude: sync JSONL → `raw_records`; the seam where
  opencode will `ATTACH`) and `contribute_view_sql(view) -> Option<String>`. Add a
  view **composer** that `UNION ALL`s contributions (trivial pass-through with one
  provider, so all tests stay green). Establish the new **`harness`** column
  (`'claude'`/`'opencode'`) in the view contract; Claude contributes `'claude'`.
  Claude-specific scope/`--source` plumbing (`project_for_cwd`,
  `source_for_config_dir`, `sources`, `project_dirs_for_query`) stays inherent to
  `ClaudeProvider`, off the trait.
- **PR 2 — `OpenCodeProvider`.** `prepare` = `INSTALL sqlite; LOAD sqlite; ATTACH`;
  `contribute_view_sql` = the SELECTs below. Added only when opencode.db exists.

**Resolution by question:**

1. *Refactor scope* → bigger than "swap a constructor": the runtime path
   (`setup_connection` → `sync_sources`) bypasses the trait and is hardwired to the
   `(name, projects_dir)` JSONL shape. Real job = collect view-SQL from active
   providers and compose with `UNION ALL`. Done as PR 1 above.
2. *Sub-agents* → **first-class sessions, real IDs preserved.** Derive
   `is_sidechain = (session.parent_id IS NOT NULL)` (cheap JOIN; do NOT rewrite child
   `session_id` into the parent). `agent_id = NULL`. `agent_type` from
   `message.data.agent`. `subagent_count` = count of child sessions. (Live data: 3 of
   11 sessions are sub-agents, so this is real, not hypothetical.)
3. *Source column / canonical path* → new **`harness`** column, distinct from the
   within-Claude `--source`. opencode rows have no `file_registry`/`source_file`
   entry; opencode SELECTs supply `NULL` for those columns.
4. *Scope filtering* → opencode `project = session.directory` (real path), existing
   `ILIKE` works. Verify in PR 2.
5. *Token / cost* → **deferred** to a later, harness-aware PR (asymmetric: Claude has
   no cost, tokens need per-message aggregation).
6. *sqlite extension delivery* → **runtime `INSTALL`+`LOAD` with graceful degrade.**
   If the extension can't load, `OpenCodeProvider.prepare` returns "inactive":
   contributes no views, and warns on stderr *only* when opencode.db exists (or
   `--harness opencode` was explicitly requested). Claude queries are unaffected
   ("stale-but-available beats error"). The one-time `INSTALL` needs network (fails
   under Claude Code's sandbox until cached in `~/.duckdb/`). Static bundling deferred.

### Schema drift caught against live opencode.db (v as of 2026-06-17)

The prototype's *session* SQL is partially stale: the `session` table has
**flattened** — `cost`, `tokens_input/output/reasoning/cache_read/cache_write`,
`agent`, `model`, `directory`, `title`, `parent_id` are now real columns, not JSON in
a `data` blob. The `message` and `part` tables still carry a JSON `data` blob, so the
prototype's `json_extract` SQL holds *there* (`message.data.role`/`.agent`;
`part.data.type` ∈ `text|tool|patch|reasoning|step-start|step-finish`; tool parts have
`.tool`/`.callID`/`.state.status`/`.state.input`/`.state.output`). PR 2 must refresh
the session-level SELECT to read flat columns.

### Reference SELECT (sessions view, opencode contribution)

```sql
SELECT
  s.id                                                     AS session_id,
  s.directory                                              AS project,
  'opencode'                                               AS harness,
  strftime(epoch_ms(s.time_created), '%Y-%m-%dT%H:%M:%SZ') AS started_at,
  strftime(epoch_ms(s.time_updated), '%Y-%m-%dT%H:%M:%SZ') AS ended_at,
  (SELECT COUNT(*) FROM oc.message m WHERE m.session_id = s.id) AS message_count,
  (SELECT COUNT(*) FROM oc.session c WHERE c.parent_id = s.id)  AS subagent_count,
  (s.parent_id IS NOT NULL)                                AS is_sidechain,
  s.title                                                  AS first_user_message
FROM oc.session s
```
