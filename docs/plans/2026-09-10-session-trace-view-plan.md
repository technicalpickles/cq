# Session Trace View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `cq trace --session <id>` which renders a session as a trace — lanes, duration bars, gaps, markers, parent→child edges — as a terminal waterfall, as span rows for agents, or as Chrome Trace Event JSON for Perfetto.

**Architecture:** Three data-layer changes make durations and the spawn tree queryable (`tool_results.timestamp`, three new `file_registry` columns, a new `agents` view). A `trace` module then derives spans and gaps from those views, and three thin formatters render the same model. The Perfetto emitter is isolated so a future protobuf emitter is a one-module swap.

**Tech Stack:** Rust, DuckDB (via the `duckdb` crate with `bundled` + `json`), clap for CLI, serde_json for trace emission. No new dependencies.

**Design spec:** `docs/specs/2026-09-10-session-trace-view-design.md`. Read it first — it records why legacy JSON was chosen over protobuf, why the `agents` view exists, and two claims deliberately left unverified.

---

## Test fixture: read this before writing any test

A fixture set already exists for this work, built to match **real** Claude Code
on-disk layout rather than the hand-simplified shape of the older fixtures:

```
tests/fixtures/a1b2c3d4-0000-4000-8000-000000000001.jsonl
tests/fixtures/a1b2c3d4-0000-4000-8000-000000000001/subagents/agent-sub1.jsonl
tests/fixtures/a1b2c3d4-0000-4000-8000-000000000001/subagents/agent-sub1.meta.json
tests/fixtures/a1b2c3d4-0000-4000-8000-000000000001/subagents/agent-sub2.jsonl
tests/fixtures/a1b2c3d4-0000-4000-8000-000000000001/subagents/agent-sub2.meta.json
```

**The filename must be the session UUID.** `session_id_for_file`
(`src/claude_provider.rs:248`) derives a session id from the *first path
component* under the project dir — the filename stem, or the directory name for
a nested subagent file. `--session` then prefix-matches that. This is why the
older fixtures (`simple_session.jsonl`, whose internal `sessionId` is
`sess-002`) cannot be targeted with `--session` at all, and why this one is
named for its UUID.

What the fixture deliberately contains:

| feature | where | why |
|---|---|---|
| one `tool_use` per record, one `tool_result` per record | throughout | matches real output; the old `multi_tool_session.jsonl` puts two results in one record, which cannot express overlap |
| non-nested overlap | `toolu_m1` (12:00:01.000 +3.000s) and `toolu_m2` (12:00:01.012 +5.588s) | the exact shape Task 6 spikes: starts 12ms apart, results out of order |
| 60s human gap | `a4` prose at 12:00:41 → `u2` text at 12:01:41 | exercises `GapKind::Human` |
| error result | `toolu_m3` | `is_error` propagation |
| depth-1 subagent | `agent-sub1`, `toolUseId: toolu_agent1` | lane + parent edge |
| depth-2 subagent | `agent-sub2`, `toolUseId: toolu_agent2` (spawned from inside sub1) | Task 9's ancestor walk needs two real hops |

Session id constant for tests: `a1b2c3d4-0000-4000-8000-000000000001`

### Integration tests must go through the harness

`tests/integration_test.rs` does **not** invoke `cq` bare. It copies fixtures
into a temp projects dir and points cq at it with env vars. Use
`setup_env(&[...])` + `cq_cmd(&env)` — a bare `Command::cargo_bin("cq")` would
index the developer's real `~/.claude/projects`, which is slow and would not
contain the fixture session.

`setup_env` copies fixtures **flat** (`std::fs::copy` with no `create_dir_all`),
so it cannot place the nested subagent files. Add this helper next to it, in
`tests/integration_test.rs`:

```rust
/// Copy a fixture tree (a `<uuid>.jsonl` plus its `<uuid>/subagents/` sidecars)
/// into the temp projects dir, preserving layout. `setup_env` only handles flat
/// files, but subagent discovery depends on the nested directory structure.
fn setup_env_tree(session_id: &str) -> TestEnv {
    let env = setup_env(&[]);
    let project_dir = env.projects.path().join("-Users-test-myproject");
    std::fs::create_dir_all(&project_dir).unwrap();

    std::fs::copy(
        fixture_path(&format!("{session_id}.jsonl")),
        project_dir.join(format!("{session_id}.jsonl")),
    )
    .unwrap();

    let src_subs = fixture_path(session_id).join("subagents");
    let dest_subs = project_dir.join(session_id).join("subagents");
    std::fs::create_dir_all(&dest_subs).unwrap();
    for entry in std::fs::read_dir(&src_subs).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), dest_subs.join(entry.file_name())).unwrap();
    }
    env
}
```

Every integration test in Tasks 4, 5, 7 and 9 uses:

```rust
const TRACE_SESSION: &str = "a1b2c3d4-0000-4000-8000-000000000001";

let env = setup_env_tree(TRACE_SESSION);
let output = cq_cmd(&env)
    .args(["--session", TRACE_SESSION, "trace"])
    .output()
    .unwrap();
```

Every integration test written in the tasks below already uses this pattern.

---

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `src/views.rs` | add `timestamp` to both `tool_results` bodies + the empty variant; add `agents` view body | 1, 3 |
| `src/provider.rs` | register `View::Agents` | 3 |
| `src/cache.rs` | `file_registry` columns, `SCHEMA_VERSION` bump | 2 |
| `src/indexer.rs` | read `toolUseId`/`spawnDepth`/`description` from meta.json | 2 |
| `src/trace/mod.rs` | span + gap model, the one source of truth for both | 4 |
| `src/trace/waterfall.rs` | terminal renderer | 5 |
| `src/trace/perfetto.rs` | Chrome Trace Event emitter | 7 |
| `src/commands/trace.rs` | command wiring, flag validation | 4, 5, 7 |
| `src/main.rs` | `Trace` clap variant + dispatch | 4 |
| `src/commands/schema.rs` | document the new column and view | 8 |

Spans and gaps live together in `src/trace/mod.rs` because they are derived from the same query and are meaningless apart. Formatters are separate files because each is independently replaceable — that isolation is the whole hedge against the JSON-longevity risk in the spec.

---

### Task 1: `tool_results` gains `timestamp`

Everything else depends on this. Without it there are no durations.

**Files:**
- Modify: `src/views.rs` (`claude_tool_results_sql`, `codex_tool_results_sql`, `empty_view_sql`)
- Test: `tests/views_test.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests/views_test.rs`:

```rust
// ---- tool_results.timestamp ----

#[test]
fn tool_results_expose_timestamp() {
    let conn = setup_db("multi_tool_session.jsonl");
    let ts: String = conn
        .query_row(
            "SELECT timestamp FROM tool_results WHERE tool_use_id = 'toolu_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        ts.starts_with("20") && ts.ends_with('Z'),
        "expected ISO8601 timestamp, got {ts:?}"
    );
}

#[test]
fn tool_result_timestamp_is_at_or_after_its_call() {
    let conn = setup_db("multi_tool_session.jsonl");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls tc
             JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
             WHERE tr.timestamp < tc.timestamp",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "no result may predate its own call");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test views_test tool_results_expose_timestamp -- --nocapture`

Expected: FAIL. DuckDB reports a binder error along the lines of `Referenced column "timestamp" not found in FROM clause` for the `tool_results` view.

If instead it fails with `no rows returned`, the fixture's tool_use_id differs — run `cargo test --test views_test -- --list` and inspect `tests/fixtures/multi_tool_session.jsonl` for the actual id, then use that id. Do not change the assertion to match a NULL.

- [ ] **Step 3: Add the column to the Claude view body**

In `src/views.rs`, in `claude_tool_results_sql()`, add the timestamp line immediately after the `harness` line so column order matches the other views:

```rust
            'claude' AS harness,
            json_extract_string(json, '$.timestamp') AS timestamp,
            json_extract_string(item, '$.tool_use_id') AS tool_use_id,
```

- [ ] **Step 4: Add the column to the Codex view body**

In `codex_tool_results_sql()` there are **two** `SELECT` branches (one for `function_call_output`, one for `custom_tool_call_output`). Add the same line to **both**, after the `harness` line. Codex records carry a record-level timestamp, so this is the correct expression for both:

```rust
        'codex' AS harness,
        json_extract_string(record.json, '$.timestamp') AS timestamp,
        json_extract_string(record.json, '$.payload.call_id') AS tool_use_id,
```

Missing either branch produces a `UNION ALL` arity mismatch, which DuckDB reports as a column-count error rather than pointing at the branch — so add both before compiling.

- [ ] **Step 5: Add the column to the empty variant**

In `empty_view_sql`, the `View::ToolResults` arm, after `harness`:

```rust
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS timestamp,
            NULL::VARCHAR AS tool_use_id,
```

The empty variant must keep identical column order to the real bodies; `compose_views` unions them.

- [ ] **Step 6: Run the tests**

Run: `cargo test --test views_test`

Expected: PASS, including the two new tests and every pre-existing one. If a pre-existing test fails on column count, one of the three bodies above was missed.

- [ ] **Step 7: Commit**

```bash
git add src/views.rs tests/views_test.rs
git commit -m "feat(views): expose timestamp on tool_results

Every tool_result is its own JSONL record with its own timestamp; the
view just wasn't projecting it. This is what makes a tool call's duration
computable as result.timestamp - call.timestamp."
```

---

### Task 2: `file_registry` picks up the meta.json spawn fields

**Files:**
- Modify: `src/cache.rs` (schema + `SCHEMA_VERSION`)
- Modify: `src/indexer.rs` (`read_agent_type` → richer sidecar read, plus the INSERT)
- Test: `tests/views_test.rs`, `tests/cache_test.rs`

- [ ] **Step 1: Write the failing test for sidecar parsing**

The fixtures already exist — see "Test fixture" above. Do not create new ones.

Append to `tests/views_test.rs`:

```rust
// ---- meta.json sidecar fields ----

const TRACE_SESSION: &str = "a1b2c3d4-0000-4000-8000-000000000001";

fn sub_fixture(name: &str) -> PathBuf {
    fixture_path(TRACE_SESSION).join("subagents").join(name)
}

#[test]
fn sidecar_fields_are_parsed_from_meta_json() {
    let meta = cq::indexer::read_agent_meta(&sub_fixture("agent-sub1.jsonl"))
        .expect("sidecar should parse");
    assert_eq!(meta.agent_type.as_deref(), Some("general-purpose"));
    assert_eq!(meta.parent_tool_use_id.as_deref(), Some("toolu_agent1"));
    assert_eq!(meta.spawn_depth, Some(1));
    assert_eq!(meta.description.as_deref(), Some("Sub work"));
}

#[test]
fn sidecar_reads_depth_two_subagent() {
    let meta = cq::indexer::read_agent_meta(&sub_fixture("agent-sub2.jsonl"))
        .expect("sidecar should parse");
    assert_eq!(meta.agent_type.as_deref(), Some("Explore"));
    assert_eq!(meta.parent_tool_use_id.as_deref(), Some("toolu_agent2"));
    assert_eq!(meta.spawn_depth, Some(2));
}

#[test]
fn sidecar_absent_yields_none() {
    let meta = cq::indexer::read_agent_meta(&fixture_path("simple_session.jsonl"));
    assert!(meta.is_none(), "non-subagent files have no sidecar");
}

#[test]
fn workflow_sidecar_has_no_parent_edge() {
    // Workflow subagents carry only {agentType, spawnDepth, model} -- no toolUseId.
    let path = fixture_path("subagents/workflows/wf_testrun/agent-wf1.jsonl");
    if let Some(meta) = cq::indexer::read_agent_meta(&path) {
        assert_eq!(
            meta.parent_tool_use_id, None,
            "workflow subagents have no resolvable parent"
        );
    }
}
```

`tests/views_test.rs` may not already import `PathBuf` in a way that makes
`sub_fixture` compile — it does (`use std::path::PathBuf;` at line 2), but check
rather than assume.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test views_test sidecar -- --nocapture`

Expected: FAIL to compile — `cannot find function 'read_agent_meta' in module 'cq::indexer'`.

- [ ] **Step 3: Replace `read_agent_type` with a struct-returning read**

In `src/indexer.rs`, replace the existing `read_agent_type` function (currently at line 356) with:

```rust
/// Fields read from a subagent's sibling `agent-<id>.meta.json`.
///
/// Plain subagents carry all four. Workflow subagents (under
/// `subagents/workflows/wf_<id>/`) carry only `agentType` and `spawnDepth`,
/// so `parent_tool_use_id` and `description` are None for them and their
/// parentage has to come from the path-derived `workflow_id` instead.
#[derive(Debug, Default, Clone)]
pub struct AgentMeta {
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub spawn_depth: Option<i64>,
}

/// Read the sidecar `agent-<id>.meta.json` next to a subagent transcript.
/// Returns None when this isn't a subagent transcript or the sidecar is
/// missing or unparseable.
pub fn read_agent_meta(file: &Path) -> Option<AgentMeta> {
    let name = file.file_name()?.to_str()?;
    if !name.starts_with("agent-") {
        return None;
    }
    let stem = file.file_stem()?.to_str()?;
    let meta = file.with_file_name(format!("{stem}.meta.json"));
    let data = std::fs::read_to_string(&meta).ok()?;
    let value: serde_json::Value = serde_json::from_str(&data).ok()?;
    Some(AgentMeta {
        agent_type: value
            .get("agentType")
            .and_then(|v| v.as_str())
            .map(String::from),
        description: value
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
        parent_tool_use_id: value
            .get("toolUseId")
            .and_then(|v| v.as_str())
            .map(String::from),
        spawn_depth: value.get("spawnDepth").and_then(|v| v.as_i64()),
    })
}
```

- [ ] **Step 4: Update the INSERT to store the new fields**

In `src/indexer.rs` around line 484, replace the `read_agent_type` call and the INSERT with:

```rust
        let meta = read_agent_meta(file).unwrap_or_default();

        conn.execute(
            "INSERT INTO file_registry
                (file_path, mtime_ns, file_size, cwd, agent_type, source,
                 agent_description, parent_tool_use_id, spawn_depth)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            duckdb::params![
                path_str,
                mtime_ns,
                file_size,
                cwd,
                meta.agent_type,
                source_name,
                meta.description,
                meta.parent_tool_use_id,
                meta.spawn_depth
            ],
        )?;
```

If any other call site of `read_agent_type` remains, `cargo build` will name it; convert each to `read_agent_meta(...).unwrap_or_default().agent_type`.

- [ ] **Step 5: Add the columns to the cache schema and bump the version**

In `src/cache.rs`, extend the `file_registry` DDL:

```sql
        CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT NOT NULL,
            file_size BIGINT NOT NULL,
            cwd TEXT,
            agent_type TEXT,
            source TEXT,
            agent_description TEXT,
            parent_tool_use_id TEXT,
            spawn_depth BIGINT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        );
```

Then bump the version at `src/cache.rs:6`:

```rust
pub const SCHEMA_VERSION: i32 = 7;
```

The bump is required, not optional: an existing cache created at version 6 has no `agent_description` column, and the INSERT above would fail against it. Version 7 forces a rebuild. Verify the mismatch path already handles this by reading the `Some(v) if v == SCHEMA_VERSION` arm at `src/cache.rs:65` — it should return `Ok(true)` (needs rebuild) for any other version.

- [ ] **Step 6: Update the test helpers' `file_registry` DDL**

`tests/views_test.rs` creates `file_registry` by hand in `setup_db` and `setup_db_multi` (and again inside `agent_type_flows_from_registry` around line 517). Add the three columns to **every** hand-written copy:

```rust
            agent_type TEXT,
            source TEXT,
            agent_description TEXT,
            parent_tool_use_id TEXT,
            spawn_depth BIGINT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
```

Run `grep -n "agent_type TEXT" tests/*.rs` to find them all. Missing one shows up as a confusing binder error in Task 3, not here.

- [ ] **Step 7: Run the tests**

Run: `cargo test`

Expected: PASS, all suites.

- [ ] **Step 8: Commit**

```bash
git add src/cache.rs src/indexer.rs tests/
git commit -m "feat(indexer): read spawn metadata from subagent sidecars

meta.json already gave us agentType; it also carries toolUseId (the Agent
call that spawned the lane), spawnDepth, and a human-readable description.
Reading all four turns the spawn tree into an exact join rather than a
guess. Workflow subagents carry only agentType and spawnDepth, so their
parent edge stays null by design.

Schema version 7: an existing v6 cache has no columns for these."
```

---

### Task 3: the `agents` view

**Files:**
- Modify: `src/views.rs` (new `claude_agents_sql`, `View::Agents` arm in `empty_view_sql`)
- Modify: `src/provider.rs` (add `Agents` to the `View` enum and provider trait)
- Test: `tests/views_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
// ---- agents view ----

#[test]
fn agents_view_lists_subagent_lanes() {
    let conn = setup_db(&format!("{TRACE_SESSION}/subagents/agent-sub1.jsonl"));
    conn.execute(
        "INSERT INTO file_registry
            (file_path, mtime_ns, file_size, cwd, agent_type, source,
             agent_description, parent_tool_use_id, spawn_depth)
         VALUES (?, 0, 0, '/tmp/proj', 'general-purpose', 'main',
                 'Sub work', 'toolu_agent1', 1)",
        [fixture_path("subagents/agent-sub1.jsonl")
            .to_string_lossy()
            .to_string()],
    )
    .unwrap();

    let (agent_id, agent_type, parent, depth, calls): (String, String, String, i64, i64) = conn
        .query_row(
            "SELECT agent_id, agent_type, parent_tool_use_id, spawn_depth, tool_call_count
             FROM agents WHERE agent_id = 'agent-sub1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();

    assert_eq!(agent_id, "agent-sub1");
    assert_eq!(agent_type, "general-purpose");
    assert_eq!(parent, "toolu_agent1");
    assert_eq!(depth, 1);
    assert_eq!(calls, 1, "the fixture has exactly one tool call");
}

#[test]
fn agents_view_excludes_main_loop() {
    let conn = setup_db("simple_session.jsonl");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "main-loop-only session has no agent lanes");
}

#[test]
fn agents_view_spans_cover_the_lane() {
    let conn = setup_db(&format!("{TRACE_SESSION}/subagents/agent-sub1.jsonl"));
    let (start, end): (String, String) = conn
        .query_row(
            "SELECT started_at, ended_at FROM agents WHERE agent_id = 'agent-sub1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(start <= end, "started_at must not exceed ended_at");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test views_test agents_view -- --nocapture`

Expected: FAIL with a DuckDB catalog error, `Table with name agents does not exist`.

- [ ] **Step 3: Add `Agents` to the `View` enum**

In `src/provider.rs`, add `Agents` to the `View` enum and give the trait a body method alongside the existing ones. Follow the exact shape the existing views use — read how `View::ToolResults` is threaded through `compose_views` in `src/views.rs:620` and mirror it. `hook_events` is the best model to copy, because `codex_hook_events_sql` returns `Option<String>` and `agents` is likewise Claude-only.

Codex has no subagents, so `codex_agents_sql` returns `None`.

- [ ] **Step 4: Write the view body**

In `src/views.rs`:

```rust
/// The Claude `agents` view body. One row per subagent lane.
///
/// Lane identity is `agentId`; lane metadata comes from the sidecar fields in
/// `file_registry`. `parent_tool_use_id` joins to `tool_calls.tool_use_id` to
/// recover which `Agent` dispatch spawned this lane -- null for workflow
/// subagents, whose sidecars omit it.
pub fn claude_agents_sql() -> String {
    format!(
        "SELECT
            json_extract_string(json, '$.sessionId') AS session_id,
            {PROJECT_EXPR} AS project,
            {SOURCE_EXPR} AS source,
            'claude' AS harness,
            {AGENT_ID_EXPR} AS agent_id,
            {AGENT_TYPE_EXPR} AS agent_type,
            (SELECT fr.agent_description FROM file_registry fr
              WHERE fr.file_path = source_file) AS description,
            (SELECT fr.parent_tool_use_id FROM file_registry fr
              WHERE fr.file_path = source_file) AS parent_tool_use_id,
            (SELECT fr.spawn_depth FROM file_registry fr
              WHERE fr.file_path = source_file) AS spawn_depth,
            {WORKFLOW_ID_EXPR} AS workflow_id,
            MIN(json_extract_string(json, '$.timestamp')) AS started_at,
            MAX(json_extract_string(json, '$.timestamp')) AS ended_at,
            COUNT(*) FILTER (
                WHERE json_type(json_extract(json, '$.message.content')) = 'ARRAY'
                  AND EXISTS (
                    SELECT 1 FROM UNNEST(
                        CAST(json_extract(json, '$.message.content') AS JSON[])
                    ) AS t(item)
                    WHERE json_extract_string(t.item, '$.type') = 'tool_use'
                  )
            ) AS tool_call_count
        FROM raw_records
        WHERE {IS_SIDECHAIN_EXPR}
          AND {AGENT_ID_EXPR} IS NOT NULL
        GROUP BY session_id, project, source, agent_id, agent_type,
                 description, parent_tool_use_id, spawn_depth, workflow_id,
                 source_file"
    )
}
```

`tool_call_count` counts assistant records containing at least one `tool_use` block. A record with two parallel calls counts once here. If a test asserts a per-call number instead, prefer counting from the `tool_calls` view in the caller rather than complicating this view.

- [ ] **Step 5: Add the empty variant**

In `empty_view_sql`, a new `View::Agents` arm, matching column order exactly:

```rust
        View::Agents => {
            "SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS source,
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS agent_id,
            NULL::VARCHAR AS agent_type,
            NULL::VARCHAR AS description,
            NULL::VARCHAR AS parent_tool_use_id,
            NULL::BIGINT AS spawn_depth,
            NULL::VARCHAR AS workflow_id,
            NULL::VARCHAR AS started_at,
            NULL::VARCHAR AS ended_at,
            CAST(0 AS BIGINT) AS tool_call_count
        WHERE 1=0"
        }
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --test views_test`

Expected: PASS. If `tool_call_count` comes back 0, the `UNNEST` correlation is wrong — verify by running the inner `EXISTS` against `raw_records` directly with `cq sql`.

- [ ] **Step 7: Commit**

```bash
git add src/views.rs src/provider.rs tests/views_test.rs
git commit -m "feat(views): add an agents view for subagent lanes

One row per lane, carrying the sidecar metadata and the parent_tool_use_id
edge. Makes the spawn tree queryable on its own rather than only as a
byproduct of a trace, and keeps lane metadata off all N span rows."
```

---

### Task 4: span and gap model, exposed as `cq trace --json`

**Files:**
- Create: `src/trace/mod.rs`
- Create: `src/commands/trace.rs`
- Modify: `src/lib.rs` (add `pub mod trace;`)
- Modify: `src/commands/mod.rs` (add `pub mod trace;`)
- Modify: `src/main.rs` (clap variant + dispatch)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests/integration_test.rs`. Note it uses `setup_env_tree` + `cq_cmd`, both described in the fixture section above:

```rust
#[test]
fn trace_requires_session() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env).args(["trace"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cq trace requires --session"),
        "got: {stderr}"
    );
    assert!(!output.status.success());
}

#[test]
fn trace_json_emits_spans_with_durations() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env)
        .args([
            "--json",
            "--session",
            TRACE_SESSION,
            "trace",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let first = &rows[0];
    assert!(first["duration_ms"].is_number(), "got: {stdout}");
    assert!(first["lane"].is_string());
    assert!(first["name"].is_string());
}
```

The session id must be a valid UUID shape — `--session` validates format. Add a fixture session whose id matches, or reuse an existing UUID-shaped fixture id; `grep -n "sess-00" tests/fixtures/*.jsonl` shows the convention already in use for `--session` tests.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test integration_test trace_ -- --nocapture`

Expected: FAIL — clap reports `unrecognized subcommand 'trace'`.

- [ ] **Step 3: Define the span and gap model**

Create `src/trace/mod.rs`:

```rust
//! Span and gap model for session traces.
//!
//! A span is a paired tool_use/tool_result: one unit of timed work on one lane.
//! A gap is dead air between spans on a lane, classified by what bounds it.
//! Formatters in this module's children render this model; they never re-query.

pub mod perfetto;
pub mod waterfall;

use anyhow::Result;
use duckdb::Connection;
use serde::Serialize;

/// One paired tool call on one lane.
#[derive(Debug, Clone, Serialize)]
pub struct Span {
    pub lane: String,
    pub agent_type: Option<String>,
    pub name: String,
    pub start: String,
    pub end: String,
    pub duration_ms: i64,
    pub is_error: bool,
    pub tool_use_id: String,
    pub input: String,
}

/// What bounds a stretch of dead air on a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// Tool result, then another tool call on the same lane: the model generating.
    Think,
    /// Assistant stopped and a genuine user turn followed: blocked on the human.
    Human,
}

#[derive(Debug, Clone, Serialize)]
pub struct Gap {
    pub lane: String,
    pub kind: GapKind,
    pub start: String,
    pub end: String,
    pub duration_ms: i64,
}

const SPANS_SQL: &str = "
SELECT
    COALESCE(tc.agent_id, 'main') AS lane,
    tc.agent_type,
    tc.name,
    tc.timestamp AS start,
    tr.timestamp AS end,
    CAST(
        (epoch_ms(CAST(tr.timestamp AS TIMESTAMP))
       - epoch_ms(CAST(tc.timestamp AS TIMESTAMP))) AS BIGINT
    ) AS duration_ms,
    tr.is_error,
    tc.tool_use_id,
    CAST(tc.input AS VARCHAR) AS input
FROM tool_calls tc
JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
WHERE tc.session_id = ?
ORDER BY tc.timestamp";

pub fn spans(conn: &Connection, session_id: &str) -> Result<Vec<Span>> {
    let mut stmt = conn.prepare(SPANS_SQL)?;
    let rows = stmt.query_map([session_id], |r| {
        Ok(Span {
            lane: r.get(0)?,
            agent_type: r.get(1)?,
            name: r.get(2)?,
            start: r.get(3)?,
            end: r.get(4)?,
            duration_ms: r.get(5)?,
            is_error: r.get(6)?,
            tool_use_id: r.get(7)?,
            input: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

const GAPS_SQL: &str = "
WITH lane_events AS (
    SELECT
        COALESCE(agent_id, 'main') AS lane,
        timestamp,
        type,
        text IS NOT NULL AS has_text,
        tool_count > 0 AS has_tools
    FROM messages
    WHERE session_id = ?
),
ordered AS (
    SELECT
        lane, timestamp, type, has_text, has_tools,
        LEAD(timestamp) OVER (PARTITION BY lane ORDER BY timestamp) AS next_ts,
        LEAD(type)      OVER (PARTITION BY lane ORDER BY timestamp) AS next_type,
        LEAD(has_text)  OVER (PARTITION BY lane ORDER BY timestamp) AS next_has_text
    FROM lane_events
)
SELECT
    lane,
    CASE
        WHEN next_type = 'user' AND next_has_text THEN 'human'
        ELSE 'think'
    END AS kind,
    timestamp AS start,
    next_ts AS end,
    CAST((epoch_ms(CAST(next_ts AS TIMESTAMP))
        - epoch_ms(CAST(timestamp AS TIMESTAMP))) AS BIGINT) AS duration_ms
FROM ordered
WHERE next_ts IS NOT NULL
  AND epoch_ms(CAST(next_ts AS TIMESTAMP))
    - epoch_ms(CAST(timestamp AS TIMESTAMP)) > 0
ORDER BY start";

pub fn gaps(conn: &Connection, session_id: &str) -> Result<Vec<Gap>> {
    let mut stmt = conn.prepare(GAPS_SQL)?;
    let rows = stmt.query_map([session_id], |r| {
        let kind: String = r.get(1)?;
        Ok(Gap {
            lane: r.get(0)?,
            kind: if kind == "human" {
                GapKind::Human
            } else {
                GapKind::Think
            },
            start: r.get(2)?,
            end: r.get(3)?,
            duration_ms: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
```

- [ ] **Step 4: Add a unit test for gap classification**

Append to `tests/views_test.rs`:

```rust
#[test]
fn human_gap_is_distinguished_from_think_gap() {
    let conn = setup_db(&format!("{TRACE_SESSION}.jsonl"));
    let gaps = cq::trace::gaps(&conn, TRACE_SESSION).unwrap();
    // Every gap must classify as exactly one kind, and none may be negative.
    for g in &gaps {
        assert!(g.duration_ms > 0, "gap must have positive duration: {g:?}");
    }
    assert!(
        gaps.iter().any(|g| g.kind == cq::trace::GapKind::Think),
        "a tool-heavy session must contain think gaps"
    );
}
```

The fixture contains a deliberate 60s human gap (assistant prose at 12:00:41 -> user text at 12:01:41), so this must find a `GapKind::Human` too. Add that assertion.

- [ ] **Step 5: Wire the command**

Create `src/commands/trace.rs`:

```rust
use anyhow::Result;
use duckdb::Connection;

use crate::output::OutputFormat;
use crate::scope::QueryScope;
use crate::trace;

/// How to render the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceOutput {
    Waterfall,
    Perfetto,
}

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    format: &OutputFormat,
    output: TraceOutput,
) -> Result<()> {
    let Some(session_id) = scope.session.as_ref() else {
        eprintln!("Error: cq trace requires --session");
        eprintln!("Usage: cq trace --session <id>");
        eprintln!("Hint: Run 'cq sessions' to find session IDs");
        std::process::exit(1);
    };

    let spans = trace::spans(conn, session_id)?;
    let gaps = trace::gaps(conn, session_id)?;

    // --json wins over the renderer: it's the machine-readable escape hatch.
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&spans)?);
        return Ok(());
    }

    match output {
        TraceOutput::Perfetto => trace::perfetto::emit(&spans, &gaps, session_id),
        TraceOutput::Waterfall => trace::waterfall::render(&spans, &gaps),
    }
}
```

Add `pub mod trace;` to `src/commands/mod.rs` and `pub mod trace;` to `src/lib.rs`.

- [ ] **Step 6: Add the clap variant**

In `src/main.rs`, in `enum Command`, after the `Sessions` variant:

```rust
    /// Render a session as a trace (lanes, durations, gaps)
    Trace {
        /// Emit Chrome Trace Event JSON for Perfetto instead of the waterfall
        #[arg(long)]
        perfetto: bool,

        /// Window start: offset from session start (e.g. +12m, +90s) or ISO timestamp
        #[arg(long)]
        from: Option<String>,

        /// Window end: offset from session start (e.g. +17m) or ISO timestamp
        #[arg(long)]
        to: Option<String>,
    },
```

And in the dispatch `match`, mirroring how `Command::Sessions` is dispatched around line 429:

```rust
        Command::Trace { perfetto, from, to } => {
            let output = if perfetto {
                commands::trace::TraceOutput::Perfetto
            } else {
                commands::trace::TraceOutput::Waterfall
            };
            let _ = (from, to); // window applied in Task 5
            commands::trace::run(&conn, &scope, &format, output)
        }
```

Stub `waterfall::render` and `perfetto::emit` for now so this compiles:

```rust
// src/trace/waterfall.rs
use crate::trace::{Gap, Span};
use anyhow::Result;

pub fn render(_spans: &[Span], _gaps: &[Gap]) -> Result<()> {
    Ok(())
}
```

```rust
// src/trace/perfetto.rs
use crate::trace::{Gap, Span};
use anyhow::Result;

pub fn emit(_spans: &[Span], _gaps: &[Gap], _session_id: &str) -> Result<()> {
    Ok(())
}
```

- [ ] **Step 7: Run the tests**

Run: `cargo test`

Expected: PASS, including `trace_requires_session` and `trace_json_emits_spans_with_durations`.

- [ ] **Step 8: Commit**

```bash
git add src/trace src/commands/trace.rs src/commands/mod.rs src/lib.rs src/main.rs tests/
git commit -m "feat(trace): add span and gap model behind cq trace --json

Spans join tool_calls to tool_results on tool_use_id, which the new
timestamp column makes possible. Gaps use LAG/LEAD over messages
partitioned by lane, classifying on whether a genuine user turn (type
user with non-null text) bounds the dead air -- that separates being
blocked on the human from the model thinking.

Formatters are stubs; --json is the real output for now."
```

---

### Task 5: terminal waterfall

**Files:**
- Modify: `src/trace/waterfall.rs`
- Modify: `src/commands/trace.rs` (apply `--from`/`--to`)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn trace_waterfall_shows_lanes_and_scale() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env)
        .args(["--session", TRACE_SESSION, "trace"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("main"), "expected a main lane: {stdout}");
    assert!(
        stdout.contains("min/col") || stdout.contains("s/col"),
        "expected an explicit time scale in the header: {stdout}"
    );
    assert!(stdout.contains('\u{2588}'), "expected bar glyphs: {stdout}");
}

#[test]
fn trace_window_narrows_the_span_set() {
    let env = setup_env_tree(TRACE_SESSION);
    let full = cq_cmd(&env)
        .args(["--session", TRACE_SESSION, "trace"])
        .output()
        .unwrap();
    let windowed = cq_cmd(&env)
        .args([
            "--session",
            TRACE_SESSION,
            "trace",
            "--from",
            "+0s",
            "--to",
            "+1s",
        ])
        .output()
        .unwrap();
    assert_ne!(
        String::from_utf8_lossy(&full.stdout),
        String::from_utf8_lossy(&windowed.stdout),
        "a 1-second window must not render identically to the whole session"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test integration_test trace_waterfall -- --nocapture`

Expected: FAIL — stdout is empty, since `render` is a stub.

- [ ] **Step 3: Implement the renderer**

Replace `src/trace/waterfall.rs`:

```rust
//! Terminal waterfall. One row per lane, bars scaled to terminal width.
//!
//! The header always states the time-per-column, because at full-session
//! zoom a burst of rapid calls collapses into a single block and the scale
//! is the only thing that tells you so.

use crate::trace::{Gap, GapKind, Span};
use anyhow::Result;

const BAR: char = '\u{2588}';

fn epoch_ms(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// Lane order: main first, then by first activity, so subagent fan-out
/// reads top-to-bottom as a cascade.
fn lane_order(spans: &[Span]) -> Vec<String> {
    let mut seen: Vec<(String, i64)> = Vec::new();
    for s in spans {
        if !seen.iter().any(|(l, _)| *l == s.lane) {
            seen.push((s.lane.clone(), epoch_ms(&s.start)));
        }
    }
    seen.sort_by_key(|(lane, first)| (lane != "main", *first));
    seen.into_iter().map(|(l, _)| l).collect()
}

pub fn render(spans: &[Span], gaps: &[Gap]) -> Result<()> {
    if spans.is_empty() {
        println!("No spans in this session (or the window is empty).");
        return Ok(());
    }

    let width: usize = terminal_width().saturating_sub(28).max(20);
    let t0 = spans.iter().map(|s| epoch_ms(&s.start)).min().unwrap_or(0);
    let t1 = spans
        .iter()
        .map(|s| epoch_ms(&s.end))
        .max()
        .unwrap_or(t0 + 1);
    let total = (t1 - t0).max(1);

    let per_col = total as f64 / width as f64 / 1000.0;
    let scale = if per_col >= 60.0 {
        format!("{:.1} min/col", per_col / 60.0)
    } else {
        format!("{per_col:.1} s/col")
    };

    let lanes = lane_order(spans);
    let human_ms: i64 = gaps
        .iter()
        .filter(|g| g.kind == GapKind::Human)
        .map(|g| g.duration_ms)
        .sum();
    let tool_ms: i64 = spans.iter().map(|s| s.duration_ms).sum();

    println!(
        "{} spans  {} lanes  {:.1} min wall  [{width} cols = {scale}]",
        spans.len(),
        lanes.len(),
        total as f64 / 60000.0,
    );
    println!(
        "tool {:.1} min ({:.0}%)   blocked on you {:.1} min",
        tool_ms as f64 / 60000.0,
        100.0 * tool_ms as f64 / total as f64,
        human_ms as f64 / 60000.0,
    );

    for lane in &lanes {
        let lane_spans: Vec<&Span> = spans.iter().filter(|s| s.lane == *lane).collect();
        let mut buf = vec![' '; width];
        for s in &lane_spans {
            let a = (((epoch_ms(&s.start) - t0) as f64 / total as f64) * (width - 1) as f64) as usize;
            let b = (((epoch_ms(&s.end) - t0) as f64 / total as f64) * (width - 1) as f64) as usize;
            for cell in buf.iter_mut().take(b.max(a) + 1).skip(a) {
                *cell = BAR;
            }
        }
        let label = short_lane(lane);
        println!(
            "{label:<18.18} {:>4} {}",
            lane_spans.len(),
            buf.iter().collect::<String>()
        );
    }
    Ok(())
}

fn short_lane(lane: &str) -> String {
    if lane == "main" {
        return lane.to_string();
    }
    lane.strip_prefix("agent-")
        .map(|s| s.chars().take(9).collect())
        .unwrap_or_else(|| lane.to_string())
}

fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(100)
}
```

Check whether the codebase already has a terminal-width helper before adding `terminal_width` — `grep -rn "COLUMNS\|terminal_size\|fn width" src/` and reuse it if so, since `--wide` already does TTY-aware truncation somewhere.

- [ ] **Step 4: Apply `--from`/`--to` in the command**

In `src/commands/trace.rs`, add window parsing and filter the span/gap vectors before rendering:

```rust
/// Parse a window bound: `+12m` / `+90s` offset from session start, or an
/// absolute ISO timestamp. Returns epoch milliseconds.
fn parse_bound(bound: &str, session_start_ms: i64) -> Result<i64> {
    if let Some(rest) = bound.strip_prefix('+') {
        let (num, unit) = rest.split_at(rest.len() - 1);
        let n: i64 = num.parse().map_err(|_| {
            anyhow::anyhow!(
                "Error: Invalid window offset '{bound}'\n\
                 Expected format: +<number><unit> (e.g. +12m, +90s)"
            )
        })?;
        let ms = match unit {
            "s" => n * 1_000,
            "m" => n * 60_000,
            "h" => n * 3_600_000,
            _ => anyhow::bail!(
                "Error: Unknown window unit '{unit}' in '{bound}'\n\
                 Valid units: s, m, h"
            ),
        };
        return Ok(session_start_ms + ms);
    }
    Ok(chrono::DateTime::parse_from_rfc3339(bound)
        .map_err(|_| {
            anyhow::anyhow!(
                "Error: Invalid window bound '{bound}'\n\
                 Expected an offset (+12m) or an ISO timestamp"
            )
        })?
        .timestamp_millis())
}
```

Then in `run`, after fetching spans, take `from: Option<&str>` and `to: Option<&str>` parameters and retain spans overlapping the window and gaps within it. Update the `main.rs` call site to pass them instead of `let _ = (from, to);`.

- [ ] **Step 5: Run the tests**

Run: `cargo test`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/trace/waterfall.rs src/commands/trace.rs src/main.rs tests/
git commit -m "feat(trace): render a terminal waterfall with an explicit scale

Lane order is main-then-first-activity so subagent fan-out reads as a
cascade. The header always prints time-per-column, because at full-session
zoom a rapid burst collapses to one block and the scale is the only clue.
Also surfaces the tool-vs-wall-clock ratio, which is the number that
motivated the whole feature."
```

---

### Task 6: spike the overlap encoding

This is a decision-making task, not a feature. It exists because Perfetto's JSON support is best-effort and its behavior on this shape is unverified. **Do not skip to Task 7 without doing this.**

**Files:**
- Create: `docs/notes/2026-09-10-perfetto-overlap-spike.md`

- [ ] **Step 1: Hand-write two candidate fixtures**

Two overlapping, non-nested slices on one track. Write both to `$TMPDIR`.

Candidate A, complete events (`ph: X`):

```json
[
 {"ph":"X","name":"Bash","cat":"tool","pid":1,"tid":1,"ts":0,"dur":3000000},
 {"ph":"X","name":"Read","cat":"tool","pid":1,"tid":1,"ts":12000,"dur":5600000}
]
```

Candidate B, async events (`ph: b`/`e` with a shared `id`):

```json
[
 {"ph":"b","name":"Bash","cat":"tool","pid":1,"tid":1,"ts":0,"id":"1"},
 {"ph":"e","name":"Bash","cat":"tool","pid":1,"tid":1,"ts":3000000,"id":"1"},
 {"ph":"b","name":"Read","cat":"tool","pid":1,"tid":1,"ts":12000,"id":"2"},
 {"ph":"e","name":"Read","cat":"tool","pid":1,"tid":1,"ts":5612000,"id":"2"}
]
```

- [ ] **Step 2: Load each and record what happens**

```bash
curl -LO https://get.perfetto.dev/trace_processor
chmod +x ./trace_processor
./trace_processor -q "SELECT t.name AS track, s.name, s.ts, s.dur FROM slice s JOIN track t ON s.track_id = t.id ORDER BY s.ts" candidate_a.json
```

Repeat for `candidate_b.json`.

Record for each: how many distinct tracks the two slices landed on, what those tracks are named, and whether both durations survived intact.

- [ ] **Step 3: Write the finding down**

Create `docs/notes/2026-09-10-perfetto-overlap-spike.md` stating which candidate was chosen, the actual `trace_processor` output for both, and the track names produced. If A produces overflow tracks with unhelpful names and B produces clean async tracks, B wins. If both are poor, the fallback is pre-packing lanes into synthetic `tid`s (greedy: at most 3 rows per lane, per the spec's measurement).

- [ ] **Step 4: Commit**

```bash
git add docs/notes/2026-09-10-perfetto-overlap-spike.md
git commit -m "docs: settle the Perfetto overlap encoding by experiment

Records actual trace_processor output for complete-event vs async-event
encodings of two non-nested overlapping slices, and which one Task 7 uses."
```

---

### Task 7: Perfetto emitter

**Files:**
- Modify: `src/trace/perfetto.rs`
- Test: `tests/integration_test.rs`, `tests/fixtures/expected_trace.json`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn perfetto_output_is_valid_trace_json() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env)
        .args([
            "--session",
            TRACE_SESSION,
            "trace",
            "--perfetto",
        ])
        .output()
        .unwrap();
    let events: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("must be valid JSON");
    let arr = events.as_array().expect("trace is a JSON array");

    // Metadata naming every process and thread.
    assert!(
        arr.iter().any(|e| e["ph"] == "M" && e["name"] == "process_name"),
        "expected process_name metadata"
    );
    assert!(
        arr.iter().any(|e| e["ph"] == "M" && e["name"] == "thread_name"),
        "expected thread_name metadata"
    );
    // At least one real span, in microseconds.
    let span = arr
        .iter()
        .find(|e| e["cat"] == "tool")
        .expect("expected a tool span");
    assert!(span["ts"].is_number());
    assert!(span["pid"].is_number());
    assert!(span["tid"].is_number());
    // Args carry the tool input verbatim.
    assert!(span["args"].is_object(), "expected an args object");
}

#[test]
fn perfetto_gaps_are_categorized() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env)
        .args([
            "--session",
            TRACE_SESSION,
            "trace",
            "--perfetto",
        ])
        .output()
        .unwrap();
    let events: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["cat"] == "gap"),
        "expected gap slices"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test integration_test perfetto_ -- --nocapture`

Expected: FAIL — stdout is empty (stub), so `serde_json::from_slice` errors on EOF.

- [ ] **Step 3: Implement the emitter**

Replace `src/trace/perfetto.rs`. Use the encoding chosen in Task 6 for spans; the skeleton below shows complete events, so switch the span arm to `b`/`e` pairs if the spike chose async.

```rust
//! Chrome Trace Event JSON emitter.
//!
//! Isolated on purpose: the spec's JSON-longevity risk is hedged by making a
//! protobuf emitter a swap of this file, with the span model untouched.
//!
//! Hierarchy mapping: pid = a top-level dispatch (main loop, or a depth-1
//! subagent and everything it spawned); tid = the individual lane. Perfetto
//! ignores thread_sort_index, so pid grouping is the only real nesting
//! available in this format.

use crate::trace::{Gap, GapKind, Span};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;

fn epoch_us(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|d| d.timestamp_micros())
        .unwrap_or(0)
}

pub fn emit(spans: &[Span], gaps: &[Gap], session_id: &str) -> Result<()> {
    let mut events: Vec<Value> = Vec::new();

    // Assign a tid per lane, main first so it sorts to tid 1.
    let mut tids: HashMap<&str, i64> = HashMap::new();
    tids.insert("main", 1);
    let mut next = 2;
    for s in spans {
        if !tids.contains_key(s.lane.as_str()) {
            tids.insert(s.lane.as_str(), next);
            next += 1;
        }
    }

    // Until the parent edge is threaded through (agents view join), every lane
    // shares one process. Task 7 follow-up: group by depth-1 ancestor.
    let pid = 1;

    events.push(json!({
        "ph": "M", "name": "process_name", "pid": pid, "tid": 0,
        "args": {"name": format!("session {}", &session_id[..8.min(session_id.len())])}
    }));
    for (lane, tid) in &tids {
        events.push(json!({
            "ph": "M", "name": "thread_name", "pid": pid, "tid": tid,
            "args": {"name": *lane}
        }));
    }

    for s in spans {
        let tid = tids[s.lane.as_str()];
        events.push(json!({
            "ph": "X",
            "name": if s.is_error { format!("{} (error)", s.name) } else { s.name.clone() },
            "cat": "tool",
            "pid": pid,
            "tid": tid,
            "ts": epoch_us(&s.start),
            "dur": (s.duration_ms * 1000).max(1),
            "args": {
                "input": s.input,
                "tool_use_id": s.tool_use_id,
                "duration_ms": s.duration_ms,
                "is_error": s.is_error,
            }
        }));
    }

    for g in gaps {
        let Some(tid) = tids.get(g.lane.as_str()) else {
            continue;
        };
        events.push(json!({
            "ph": "X",
            "name": match g.kind { GapKind::Human => "blocked on you", GapKind::Think => "think" },
            "cat": "gap",
            "pid": pid,
            "tid": tid,
            "ts": epoch_us(&g.start),
            "dur": (g.duration_ms * 1000).max(1),
            "args": {"duration_ms": g.duration_ms}
        }));
    }

    println!("{}", serde_json::to_string(&events)?);
    Ok(())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test`

Expected: PASS.

- [ ] **Step 5: Verify against real trace tooling**

This step cannot be automated away — a schema-shaped file that a viewer silently mangles passes every test above.

```bash
cargo run -- --session <a real session id from 'cq sessions'> trace --perfetto > "$TMPDIR/cq-trace.json"
./trace_processor -q "SELECT t.name AS track, count(*) n, sum(s.dur)/1e6 secs FROM slice s JOIN track t ON s.track_id = t.id GROUP BY 1 ORDER BY n DESC LIMIT 15" "$TMPDIR/cq-trace.json"
```

Confirm: track count matches the lane count the waterfall reported, slice count matches the span count, and no track is named something like `overflow`. Then load the same file in Firefox Profiler and confirm the Marker Chart populates — the spec flags that as an inference, and this is where it gets checked.

Record both outcomes in `docs/notes/2026-09-10-perfetto-overlap-spike.md`.

- [ ] **Step 6: Commit**

```bash
git add src/trace/perfetto.rs tests/ docs/notes/
git commit -m "feat(trace): emit Chrome Trace Event JSON for Perfetto

Spans and gaps become slices with the tool input verbatim in args.
Timestamps in microseconds, as the format requires. Kept in one isolated
module so a protobuf emitter is a file swap, not a refactor."
```

---

### Task 8: documentation

**Files:**
- Modify: `src/commands/schema.rs`
- Modify: `README.md`
- Modify: `claude-plugin/` skill (the inline schema listing)

- [ ] **Step 1: Update `cq schema`**

`cq schema` is the documented source of truth (the cq skill says to trust it over its own inline copy). Add `timestamp` to the `tool_results` listing and add the whole `agents` view with per-column descriptions. Read `src/commands/schema.rs` and follow the existing per-view format exactly.

- [ ] **Step 2: Verify**

Run: `cargo run -- schema tool_results` and `cargo run -- schema agents`

Expected: `timestamp` appears for `tool_results`; `agents` prints all 13 columns.

- [ ] **Step 3: Add a README section**

Add a short `cq trace` section in the README's existing voice (it's a screenplay-style narrative — match it, don't bolt on a reference table). The tool-vs-wall-clock ratio is the hook: a session that took 345 minutes spent 169 in tools.

- [ ] **Step 4: Update the skill's inline schema**

The cq skill under `claude-plugin/` carries an inline schema copy. Add `timestamp` to `tool_results` and the `agents` view, plus a line noting `cq trace` exists and that `--perfetto` output pipes to `trace_processor`.

- [ ] **Step 5: Commit**

```bash
git add src/commands/schema.rs README.md claude-plugin/
git commit -m "docs: document tool_results.timestamp, the agents view, and cq trace"
```

---

## Self-Review

**Spec coverage:**

| Spec section | Task |
|---|---|
| `tool_results` gains `timestamp` | 1 |
| `file_registry` picks up three fields | 2 |
| New `agents` view | 3 |
| Span and gap model | 4 |
| CLI surface, `--from`/`--to` | 4, 5 |
| Terminal waterfall | 5 |
| Spike before implementing | 6 |
| Perfetto mapping | 7 |
| Testing (incl. manual Perfetto + Firefox Profiler loads) | 1-7, verification in 7 |
| Docs / `cq schema` | 8 |

**Known gap, deliberately deferred:** the spec maps `pid` to a depth-1 dispatch and its descendants, but Task 7 emits a single `pid` for all lanes and leaves a comment saying so. Grouping needs the `agents.parent_tool_use_id` edge walked to find each lane's depth-1 ancestor. Doing it in Task 7 would mix "emit valid trace JSON" with "resolve the tree," and the first is worth landing on its own. Task 9 below closes it.

### Task 9: group lanes by their depth-1 ancestor

**Files:**
- Modify: `src/trace/mod.rs` (a lane→ancestor resolver over `agents`)
- Modify: `src/trace/perfetto.rs` (use it for `pid`)
- Test: `tests/views_test.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn lane_groups_resolve_to_depth_one_ancestor() {
    let conn = setup_db_multi(&["subagents/agent-sub1.jsonl"]);
    // A depth-2 lane must report the depth-1 lane above it as its group.
    let groups = cq::trace::lane_groups(&conn, TRACE_SESSION).unwrap();
    assert_eq!(groups.get("main").map(String::as_str), Some("main"));
}
```

Extend the fixture set with a depth-2 subagent whose `toolUseId` points at a `tool_use` inside `agent-sub1.jsonl`, so the edge is genuinely two hops.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test views_test lane_groups -- --nocapture`

Expected: FAIL to compile — `cannot find function 'lane_groups'`.

- [ ] **Step 3: Implement the resolver**

In `src/trace/mod.rs`:

```rust
use std::collections::HashMap;

/// Map every lane to the depth-1 lane that contains it (main maps to itself).
/// Walks `agents.parent_tool_use_id` up to `tool_calls.agent_id` until it
/// reaches a lane whose parent is the main loop. Lanes with no resolvable
/// parent (workflow subagents) group under their own `workflow_id`, or
/// themselves if that is also null.
pub fn lane_groups(conn: &Connection, session_id: &str) -> Result<HashMap<String, String>> {
    let sql = "
    WITH RECURSIVE up(agent_id, ancestor, depth) AS (
        SELECT a.agent_id, a.agent_id, a.spawn_depth
        FROM agents a WHERE a.session_id = ?
        UNION ALL
        SELECT u.agent_id,
               COALESCE(tc.agent_id, 'main') AS ancestor,
               p.spawn_depth
        FROM up u
        JOIN agents p ON p.agent_id = u.ancestor AND p.session_id = ?
        JOIN tool_calls tc ON tc.tool_use_id = p.parent_tool_use_id
        WHERE p.spawn_depth > 1
    )
    SELECT agent_id, ancestor FROM up
    WHERE depth = 1 OR ancestor = 'main'";

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([session_id, session_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out: HashMap<String, String> = HashMap::new();
    out.insert("main".to_string(), "main".to_string());
    for row in rows {
        let (lane, ancestor) = row?;
        out.insert(lane, ancestor);
    }
    Ok(out)
}
```

If the recursive CTE proves awkward in DuckDB, resolve it in Rust instead: read all `(agent_id, parent_tool_use_id, spawn_depth)` rows plus a `tool_use_id -> agent_id` map from `tool_calls`, then walk parents in a loop with a visited-set guard. Correctness matters more than doing it in SQL, and a cycle guard is required either way — a malformed sidecar must not hang the command.

- [ ] **Step 4: Use it for `pid` in the emitter**

In `src/trace/perfetto.rs`, replace the fixed `let pid = 1;` with a lookup: assign each distinct group a `pid` (main = 1), then each span's `pid` is `pids[groups[&s.lane]]`. Emit one `process_name` metadata event per group, named from the group lane's `agent_type` and `description`.

- [ ] **Step 5: Run the tests**

Run: `cargo test`

Expected: PASS.

- [ ] **Step 6: Re-verify with trace_processor**

Re-run the Task 7 Step 5 command. Confirm the number of distinct processes now equals the number of depth-1 dispatches rather than 1, and that collapsing a process group in the UI folds its descendants.

- [ ] **Step 7: Commit**

```bash
git add src/trace tests/
git commit -m "feat(trace): group lanes under their depth-1 ancestor

pid now means a top-level dispatch and everything it spawned, which is
the one real nesting level legacy JSON offers and where 93% of subagent
lanes live. Cycle-guarded, since a malformed sidecar must not hang."
```

---

## Final: draft PR

- [ ] Run the full suite: `cargo test` and `cargo clippy --all-targets -- -D warnings`
- [ ] Confirm the diff still does what the title claims: `git diff origin/main...HEAD --stat`
- [ ] Push the branch and open a **draft** PR, using the `git:pull-request` skill for the body
