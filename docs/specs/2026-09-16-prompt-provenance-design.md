# Message provenance columns (`prompt_origin`, `prompt_source`, `is_meta`)

Status: implemented
Date: 2026-09-16

## Problem

The `messages` view has no way to tell a genuinely-typed human turn apart
from harness-injected content that also lands in a `type='user'` row:
skill-file dumps, task/subagent notifications, slash-command XML wrappers,
tool results. Verified against the real corpus: of ~3.5M words under
`type='user'` in one project's history, ~64% came from three injected
patterns alone (skill dumps, task notifications, subagent notifications).
Any "how much did I type vs. Claude write" query today needs a hand-rolled
grep+`json.loads` pass over raw JSONL outside of cq entirely.

Claude Code's own JSONL already tags this on the record itself — it's
just never `json_extract`'d into a view:

- `origin.kind`: `"human"` | `"task-notification"` | `"coordinator"` |
  `"peer"` | `"auto-continuation"`
- `promptSource`: `"typed"` | `"queued"` | `"system"` | `"sdk"`
- `isMeta` (+ `turnCompanion`): marks ancillary content bolted onto a turn
  (skill loads) rather than the turn's own submission

`raw_records.json` already stores the full original line per
`docs/session-storage.md`'s ingest model, so nothing needs re-parsing to
get at these — this is a view-layer change only.

## Use cases

- Human-vs-assistant word/message ratio queries (the motivating case).
- Filtering search/analysis to only your own typed turns, excluding
  replayed skill content and subagent chatter that otherwise pollutes
  `type='user'` full-text search hits.
- Distinguishing a `queued` message (typed while Claude was busy) from a
  live `typed` one, if that distinction ever matters to a query.

## Column design

Add three columns to `messages`. Claude rows populate all three from the
record's own JSON; Codex/other providers emit `NULL` for `prompt_origin`
and `prompt_source` (no equivalent field — see Non-goals) but `false` for
`is_meta`, matching `is_meta`'s always-non-null convention on Claude rows
rather than introducing a three-valued boolean:

| Column | Type | Source | Values |
|---|---|---|---|
| `prompt_origin` | VARCHAR | `json_extract_string(json, '$.origin.kind')` | `human`, `task-notification`, `coordinator`, `peer`, `auto-continuation`, `NULL` |
| `prompt_source` | VARCHAR | `json_extract_string(json, '$.promptSource')` | `typed`, `queued`, `system`, `sdk`, `NULL` |
| `is_meta` | BOOLEAN | `COALESCE(CAST(json_extract(json, '$.isMeta') AS BOOLEAN), false)` (same shape as the existing `IS_SIDECHAIN_EXPR`) | `true`/`false`, never `NULL` |

No new column is needed to flag tool-result rows — those already surface
as `text IS NULL` (no `text` block in a `tool_result` content array), so
that boundary is already queryable.

**Naming:** `prompt_*` prefix (not `origin`/`source` bare) to avoid
colliding with likely future columns on other views, and because these
are properties of the *prompt*, not of the message row in general —
consistent with existing snake_case columns like `is_sidechain`.

## Where this lands

`src/views.rs::claude_messages_sql()` — add the three `json_extract_string`
expressions as module-level consts (mirroring `PROJECT_EXPR`,
`AGENT_ID_EXPR`, etc.), wire into both the `string_msgs` and `array_msgs`
branches. `src/views.rs::codex_messages_sql()` and `empty_view_sql` get
matching literals (`NULL`/`NULL`/`false`) so the `UNION ALL` column set
stays aligned — the two providers combine positionally, so the column
counts must match at all times, not just once every provider is done.

No `cache.rs` schema change, no `SCHEMA_VERSION` bump — `raw_records`
already has the full JSON per record.

## Testing

- `tests/views_test.rs`: extend Claude fixtures with rows carrying each
  `origin.kind` value, one `isMeta: true` row, and one row missing these
  fields entirely (older client versions — confirmed these fields aren't
  universally present; a real corpus has records with `origin.kind` set
  and no `promptSource` at all). Assert the view surfaces `NULL` rather
  than erroring on missing fields.
- Assert Codex fixture rows produce `NULL` for `prompt_origin`/`prompt_source`
  and `false` (never `NULL`) for `is_meta`.
- `docs/session-storage.md` needs its own note on these fields per the
  repo's docs-sync table (transcript-format quirks live there, not just in
  this design doc): `origin` and `isMeta` each show up in three states in
  the wild (present, absent, and present-but-JSON-`null`), and
  `promptSource` isn't co-present with `origin.kind` on all client
  versions. The `COALESCE`/`json_extract` shapes above are written to
  survive all three states, not just the two you'd guess from the field
  name.

## Non-goals (this design)

- **Codex support.** Codex's rollout format has no per-record provenance
  field — `response_item`/role=user rows (injected content and genuine
  typed turns alike) are structurally identical. The actual signal lives
  in a *sibling* record type, `event_msg`/`payload.type=="user_message"`,
  correlated to its `response_item` counterpart by exact
  `(source_file, timestamp, text)` match — verified 22/22 on real session
  data, including one timestamp collision resolved by also requiring the
  text match. This needs a join (a `WITH codex_human_turns AS (...)` CTE
  hash-joined into `codex_messages_sql()`), not a single `json_extract`,
  and only yields `prompt_origin` — Codex's `user_message` payload has no
  `promptSource`-equivalent (typed/queued/dictated), so `prompt_source`
  stays `NULL` for Codex even after that follow-up lands. Mechanically
  compatible with this design (same `raw_records`-only, no-schema-bump
  shape) — left out of v1 because nothing's blocking on it yet.
- **opencode support.** No provider exists yet; out of scope regardless.
- **A convenience "human word count" command/flag.** The three raw
  columns are enough to write the query by hand
  (`WHERE prompt_origin='human' AND NOT is_meta`); a packaged command is
  a separate, later decision if this gets used often enough to want one.
