# Message provenance columns (`prompt_origin`, `prompt_source`, `is_meta`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `prompt_origin`, `prompt_source`, `is_meta` columns to the `messages` view, sourced from fields Claude Code's JSONL already carries per-record (`origin.kind`, `promptSource`, `isMeta`) so `WHERE prompt_origin = 'human' AND NOT is_meta` becomes a plain SQL filter for genuinely-typed human turns.

**Architecture:** Pure view-layer change, no cache/schema-version bump — `raw_records.json` already stores the full JSON per line. Three new `json_extract_string`/`json_extract` expressions get added, as module-level consts (mirroring `PROJECT_EXPR`, `AGENT_ID_EXPR`, etc.), to every SELECT that produces the `messages` view's column set. That's three sites in `src/views.rs`, all of which must stay column-aligned since they're combined with positional `UNION ALL`: `claude_messages_sql()` (both `string_msgs` and `array_msgs` branches), `codex_messages_sql()` (emits `NULL`/`false` literals — Codex has no equivalent fields, see design doc's Non-goals), and `empty_view_sql(View::Messages)` (emits the same literals for the no-files/no-providers case).

**Tech Stack:** Rust, DuckDB (`json_extract_string`/`json_extract` over the `json` column), existing `cargo test` suite (`tests/views_test.rs` fixture pattern).

**Reference:** `docs/specs/2026-09-16-prompt-provenance-design.md`

---

### Task 1: Add the three column expressions to `claude_messages_sql()`

**Files:**
- Modify: `src/views.rs`

- [ ] **Step 1: Add the module-level consts**

In `src/views.rs`, after `SOURCE_EXPR` (line 32) and before `register_views` (line 34), add:

```rust
/// SQL expression for prompt provenance: who/what actually submitted this
/// turn. `"human"` means a real keystroke; other values
/// (`task-notification`, `coordinator`, `peer`, `auto-continuation`) are
/// harness-injected. Absent on older client versions -- NULL, not an error.
const PROMPT_ORIGIN_EXPR: &str = "json_extract_string(json, '$.origin.kind')";

/// SQL expression for how the prompt was submitted: `typed`/`queued` are
/// both real human input; `system`/`sdk` are not. Absent on older client
/// versions -- NULL, not an error.
const PROMPT_SOURCE_EXPR: &str = "json_extract_string(json, '$.promptSource')";

/// SQL expression flagging ancillary content bolted onto a turn (e.g. a
/// skill-file dump) rather than the turn's own submission. Defaults false
/// when absent, matching IS_SIDECHAIN_EXPR's always-non-null convention.
const IS_META_EXPR: &str =
    "COALESCE(CAST(json_extract(json, '$.isMeta') AS BOOLEAN), false)";
```

- [ ] **Step 2: Wire into `string_msgs`**

In `claude_messages_sql()` (line 91), in the `string_msgs` CTE's SELECT list, add after `{WORKFLOW_ID_EXPR} AS workflow_id` (line 108):

```rust
                {WORKFLOW_ID_EXPR} AS workflow_id,
                {PROMPT_ORIGIN_EXPR} AS prompt_origin,
                {PROMPT_SOURCE_EXPR} AS prompt_source,
                {IS_META_EXPR} AS is_meta
```

- [ ] **Step 3: Wire into `array_msgs`**

Same addition after the `array_msgs` CTE's `{WORKFLOW_ID_EXPR} AS workflow_id` (line 137).

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: succeeds. (`SELECT * FROM string_msgs UNION ALL SELECT * FROM array_msgs` at the bottom of the function needs no change — both branches now have matching column sets.)

- [ ] **Step 5: Commit**

```bash
git add src/views.rs
git commit -m "feat: add prompt_origin/prompt_source/is_meta to claude_messages_sql"
```

---

### Task 2: Keep `codex_messages_sql()` and `empty_view_sql` column-aligned

**Files:**
- Modify: `src/views.rs`

- [ ] **Step 1: Add NULL/false literals to `codex_messages_sql()`**

In `codex_messages_sql()` (line 401), add after `NULL::VARCHAR AS workflow_id` (line 441):

```rust
        NULL::VARCHAR AS workflow_id,
        NULL::VARCHAR AS prompt_origin,
        NULL::VARCHAR AS prompt_source,
        false AS is_meta
```

Codex genuinely has no per-record equivalent — see the design doc's Non-goals for why (`event_msg`/`user_message` correlation is a follow-up, not this task).

- [ ] **Step 2: Add matching literals to `empty_view_sql(View::Messages)`**

At line 610, add after `NULL::VARCHAR AS workflow_id` (line 628):

```rust
            NULL::VARCHAR AS workflow_id,
            NULL::VARCHAR AS prompt_origin,
            NULL::VARCHAR AS prompt_source,
            false AS is_meta
        WHERE 1=0"
```

(Adjust so the trailing `WHERE 1=0` stays on the body, not duplicated — check the existing formatting before editing.)

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: succeeds.

- [ ] **Step 4: Commit**

```bash
git add src/views.rs
git commit -m "fix: align codex_messages_sql and empty messages view with new prompt provenance columns"
```

---

### Task 3: Fixture + view tests

**Files:**
- Create: `tests/fixtures/prompt_provenance.jsonl`
- Modify: `tests/views_test.rs`

- [ ] **Step 1: Write the fixture**

`tests/fixtures/prompt_provenance.jsonl` — one row per case: a real human turn, a task-notification, a coordinator-origin turn, an `isMeta` skill-dump turn, and a row with none of these fields at all (simulating an older client). Base the JSON shape on `tests/fixtures/simple_session.jsonl`'s user-message records, adding the new fields:

```
{"type":"user","message":{"role":"user","content":"commit and push"},"uuid":"u1","parentUuid":null,"isSidechain":false,"timestamp":"2026-09-16T10:00:00.000Z","sessionId":"sess-prov","cwd":"/Users/test/myproject","version":"2.1.273","gitBranch":"main","origin":{"kind":"human"},"promptSource":"typed"}
{"type":"user","message":{"role":"user","content":"<task-notification>...</task-notification>"},"uuid":"u2","parentUuid":"u1","isSidechain":false,"timestamp":"2026-09-16T10:00:05.000Z","sessionId":"sess-prov","cwd":"/Users/test/myproject","version":"2.1.273","gitBranch":"main","origin":{"kind":"task-notification"},"promptSource":"system"}
{"type":"user","message":{"role":"user","content":"resume from handoff"},"uuid":"u3","parentUuid":"u2","isSidechain":false,"timestamp":"2026-09-16T10:00:10.000Z","sessionId":"sess-prov","cwd":"/Users/test/myproject","version":"2.1.273","gitBranch":"main","origin":{"kind":"coordinator"},"promptSource":"sdk"}
{"type":"user","message":{"role":"user","content":"Base directory for this skill: ..."},"uuid":"u4","parentUuid":"u3","isSidechain":false,"timestamp":"2026-09-16T10:00:15.000Z","sessionId":"sess-prov","cwd":"/Users/test/myproject","version":"2.1.273","gitBranch":"main","isMeta":true,"turnCompanion":true}
{"type":"user","message":{"role":"user","content":"old client message"},"uuid":"u5","parentUuid":"u4","isSidechain":false,"timestamp":"2026-09-16T10:00:20.000Z","sessionId":"sess-prov","cwd":"/Users/test/myproject","version":"2.0.0","gitBranch":"main"}
```

- [ ] **Step 2: Write the tests**

Add to `tests/views_test.rs`, near the other messages-view tests (after `messages_tag_sidechain_rows`):

```rust
#[test]
fn messages_view_prompt_origin_human() {
    let conn = setup_db("prompt_provenance.jsonl");
    let origin: String = conn
        .query_row(
            "SELECT prompt_origin FROM messages WHERE uuid = 'u1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(origin, "human");
    let source: String = conn
        .query_row(
            "SELECT prompt_source FROM messages WHERE uuid = 'u1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(source, "typed");
}

#[test]
fn messages_view_prompt_origin_non_human_values() {
    let conn = setup_db("prompt_provenance.jsonl");
    let mut stmt = conn
        .prepare("SELECT uuid, prompt_origin, prompt_source FROM messages WHERE uuid IN ('u2', 'u3') ORDER BY uuid")
        .unwrap();
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![
            ("u2".to_string(), "task-notification".to_string(), "system".to_string()),
            ("u3".to_string(), "coordinator".to_string(), "sdk".to_string()),
        ]
    );
}

#[test]
fn messages_view_is_meta_flag() {
    let conn = setup_db("prompt_provenance.jsonl");
    let is_meta: bool = conn
        .query_row("SELECT is_meta FROM messages WHERE uuid = 'u4'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(is_meta);

    // Every other row in the fixture defaults to false, not NULL.
    let non_meta_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE uuid != 'u4' AND is_meta = false",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(non_meta_count, 4);
}

#[test]
fn messages_view_prompt_fields_null_when_absent() {
    let conn = setup_db("prompt_provenance.jsonl");
    let (origin, source): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT prompt_origin, prompt_source FROM messages WHERE uuid = 'u5'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, None);
    assert_eq!(source, None);
}
```

- [ ] **Step 3: Codex/empty-view alignment test**

Add a test confirming the Codex branch still produces the (`NULL`, `NULL`, `false`) triple — extend whatever existing Codex fixture/test already exercises `codex_messages_sql()` (check for one before adding a new fixture; if none exists, skip this sub-step and note it in the commit message rather than inventing new Codex fixture infrastructure out of scope for this task).

Also add a test for `empty_view_sql(View::Messages)`: unlike the Codex branch (checked by a real runtime `UNION ALL` every time both providers are active), the empty-view body is only reached when zero providers contribute, so a column-count mismatch there raises no error at all — a query naming `prompt_origin` would just get a Binder Error on an empty corpus instead of zero rows, silently. Copy the existing pattern at `tests/views_test.rs` (the `hook_events` empty-view test, "All 10 columns must exist and be selectable without error" — search for `register_views(&conn, &[])`): call `register_views(&conn, &[])`, then assert all 18 `messages` columns (including the three new ones) are selectable and NULL/false as expected.

- [ ] **Step 4: Run the tests**

Run: `cargo test --test views_test`
Expected: PASS, including the 4 (or 5) new tests, no regressions in existing `messages_view_*` tests.

- [ ] **Step 5: Commit**

```bash
git add tests/fixtures/prompt_provenance.jsonl tests/views_test.rs
git commit -m "test: add view coverage for prompt provenance columns"
```

---

### Task 4: Docs

**Files:**
- Modify: `src/commands/schema.rs`
- Modify: `docs/specs/2026-09-16-prompt-provenance-design.md`
- Modify: `docs/session-storage.md`
- Modify: `claude-plugin/skills/cq/SKILL.md`

- [ ] **Step 1: Update `MESSAGES_SCHEMA` (around line 30-46)**

Add after `workflow_id         VARCHAR   Workflow run id (wf_...) if spawned by a workflow, else NULL"#;` — note the trailing `"#;` moves to the new last line:

```
  prompt_origin       VARCHAR   Who/what submitted this turn: 'human', 'task-notification', 'coordinator', 'peer', 'auto-continuation', or NULL if absent (older clients, Codex)
  prompt_source       VARCHAR   How it was submitted: 'typed', 'queued', 'system', 'sdk', or NULL if absent
  is_meta             BOOLEAN   true for ancillary content bolted onto a turn (e.g. a skill-file dump), false otherwise (never NULL)"#;
```

- [ ] **Step 2: Update the duplicate copy inside `SCHEMA_DOCS` (around line 202-219)**

Same three lines, same insertion point, in the inline copy (note: no trailing `"#;` here since it's mid-string — match the existing formatting of that block exactly).

- [ ] **Step 3: Build and spot-check**

Run: `cargo run -- schema messages`
Expected: output includes the three new rows.

- [ ] **Step 4: Update `claude-plugin/skills/cq/SKILL.md`**

Line 49 enumerates the `messages` column set verbatim for agents using cq via this skill (`**messages**: session_id, project, source, harness, uuid, parent_uuid, type, timestamp, text, tool_count, model, agent_id, is_sidechain, agent_type, workflow_id`) and will ship stale otherwise. Append `, prompt_origin, prompt_source, is_meta` to that line.

- [ ] **Step 5: Add a note to `docs/session-storage.md`**

Per this repo's own docs-sync table (`docs/cli-ux-conventions.md`), any transcript-format quirk belongs in `docs/session-storage.md`, not just the design doc. Add a short section (near the existing `toolUseResult.persistedOutputPath` note) covering:

- `origin` (an OBJECT carrying `kind`, sometimes also `from`/`senderTaskId`/`body`/`handback`/`name`) and `isMeta` each appear in three states on real records: present, absent, and present-but-JSON-`null` — not just present/absent. Any extraction needs a `COALESCE` guard (see `IS_META_EXPR`/`IS_SIDECHAIN_EXPR` in `views.rs` for the pattern), not a bare `json_extract_string`.
- `promptSource` is not always co-present with `origin.kind` — older client versions have `origin.kind` without `promptSource`.

- [ ] **Step 6: Mark the design doc implemented**

In `docs/specs/2026-09-16-prompt-provenance-design.md`, change:

```markdown
Status: implemented
Date: 2026-09-16
```

- [ ] **Step 7: Commit**

```bash
git add src/commands/schema.rs docs/specs/2026-09-16-prompt-provenance-design.md docs/session-storage.md claude-plugin/skills/cq/SKILL.md
git commit -m "docs: document prompt provenance columns"
```

---

### Task 5: Full suite + manual smoke test

**Files:** none (verification only)

- [ ] **Step 1: Full test suite**

Run: `cargo test`
Expected: PASS, no regressions anywhere (views, integration, cache tests).

- [ ] **Step 2: Manual smoke test against real data**

Run: `cargo run -- sql "SELECT prompt_origin, prompt_source, is_meta, COUNT(*) FROM messages WHERE type='user' GROUP BY 1,2,3 ORDER BY 4 DESC LIMIT 10"`
Expected: real `origin.kind`/`promptSource` values appear (not all-NULL), matching the taxonomy from the design doc (`human`/`typed` should be one of the top rows if run against a real `~/.claude/projects/` corpus rather than the test fixtures — this needs `CQ_CACHE_DIR` unset or pointed at the real cache, not just `cargo test`'s in-memory fixtures).

No commit for this task — it's confirmation the prior four tasks are correct end to end.
