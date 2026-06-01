# Index Subagent Transcripts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make subagent transcripts under `<session>/subagents/**/agent-*.jsonl` queryable in cq, tagged so callers can include/exclude/focus subagents, while keeping the `sessions` overview clean.

**Architecture:** The indexer recurses into the `subagents/` subtree (excluding `journal.jsonl`) and captures `agentType` from each `agent-<id>.meta.json` sidecar into `file_registry`. The row-level views (`messages`, `tool_calls`, `tool_results`) gain four tag columns (`agent_id`, `is_sidechain`, `agent_type`, `workflow_id`); `session_id` stays the parent. The `sessions` view filters its counts to main-loop rows and adds a `subagent_count` signal. The Auto-sync mtime fast-path becomes a recursive max so new subagent files are detected without `--reindex`.

**Tech Stack:** Rust, DuckDB (bundled, `json` feature), `serde_json`, `assert_cmd` + `predicates` for integration tests, `tempfile` for fixtures.

**Reference spec:** `docs/specs/2026-06-01-index-subagent-transcripts-design.md`

---

## File structure

| File | Responsibility | Change |
|---|---|---|
| `tests/fixtures/mixed_sidechain_session.jsonl` | main-loop + sidechain rows, one session | create |
| `tests/fixtures/subagents/workflows/wf_testrun/agent-wf1.jsonl` | workflow subagent at a workflow path | create |
| `tests/views_test.rs` | view SQL correctness | add tag-column + sessions tests; extend `setup_db` helpers |
| `src/views.rs` | view definitions | add tag columns + `subagent_count`; filter sessions counts; update empty views |
| `src/cache.rs` | cache schema | add `file_registry.agent_type`; bump `SCHEMA_VERSION` |
| `src/indexer.rs` | sync + scan | recursive scan (exclude `journal.jsonl`); capture `agent_type`; recursive `max_dir_mtime` |
| `src/claude_provider.rs` | file discovery (trait) | recursive `discover_files`; session-dir-aware filter |
| `tests/integration_test.rs` | end-to-end CLI | subagent indexing, `agent_type`, journal exclusion |
| `src/commands/schema.rs` | `cq schema` docs | document new columns + example |
| `README.md` | Views section | note tag columns |
| `CLAUDE.md` | architecture / patterns | note recursion |
| `claude-plugin/skills/cq/SKILL.md` | skill schema | mirror schema additions |

Run all tests with `cargo test`. Run a single test with `cargo test <name> -- --exact` (or `cargo test --test views_test <name>`).

---

## Task 1: Tag row-level views with `agent_id` and `is_sidechain`

These two columns come straight from the JSON, so this task needs no registry or cache change. It is the tracer bullet: prove subagent rows are distinguishable.

**Files:**
- Create: `tests/fixtures/mixed_sidechain_session.jsonl`
- Modify: `src/views.rs` (add expr consts; add columns to `messages`, `tool_calls`, `tool_results` and their empty variants)
- Test: `tests/views_test.rs`

- [ ] **Step 1: Create the fixture**

Create `tests/fixtures/mixed_sidechain_session.jsonl` with exactly these 5 lines (one JSON object per line, no trailing blank line):

```json
{"type":"user","message":{"role":"user","content":"run the analysis"},"uuid":"mu1","parentUuid":null,"isSidechain":false,"timestamp":"2026-05-01T10:00:00.000Z","sessionId":"sess-mix","cwd":"/Users/test/myproject"}
{"type":"assistant","message":{"id":"m1","role":"assistant","model":"claude-opus-4-8","content":[{"type":"text","text":"Dispatching a subagent."},{"type":"tool_use","id":"toolu_task","name":"Task","input":{"subagent_type":"Explore","prompt":"look around"}}]},"uuid":"ma1","parentUuid":"mu1","isSidechain":false,"timestamp":"2026-05-01T10:00:05.000Z","sessionId":"sess-mix","cwd":"/Users/test/myproject"}
{"type":"user","message":{"role":"user","content":"look around the repo"},"uuid":"su1","parentUuid":null,"isSidechain":true,"agentId":"agentAAA","timestamp":"2026-05-01T10:00:06.000Z","sessionId":"sess-mix","cwd":"/Users/test/myproject"}
{"type":"assistant","message":{"id":"a2","role":"assistant","model":"claude-haiku-4-5","content":[{"type":"text","text":"Reading."},{"type":"tool_use","id":"toolu_s1","name":"Read","input":{"file_path":"/Users/test/myproject/README.md"}},{"type":"tool_use","id":"toolu_s2","name":"Grep","input":{"pattern":"TODO"}}]},"uuid":"sa1","parentUuid":"su1","isSidechain":true,"agentId":"agentAAA","timestamp":"2026-05-01T10:00:08.000Z","sessionId":"sess-mix","cwd":"/Users/test/myproject"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_s1","content":"# Readme"},{"type":"tool_result","tool_use_id":"toolu_s2","content":"no todos"}]},"uuid":"su2","parentUuid":"sa1","isSidechain":true,"agentId":"agentAAA","timestamp":"2026-05-01T10:00:09.000Z","sessionId":"sess-mix","cwd":"/Users/test/myproject"}
```

- [ ] **Step 2: Write the failing tests**

Append to `tests/views_test.rs`:

```rust
// ---- subagent tagging ----

#[test]
fn messages_tag_sidechain_rows() {
    let conn = setup_db("mixed_sidechain_session.jsonl");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 5, "all main-loop and sidechain messages are queryable");

    let (is_side, agent): (bool, Option<String>) = conn
        .query_row(
            "SELECT is_sidechain, agent_id FROM messages WHERE uuid = 'su1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(is_side);
    assert_eq!(agent.as_deref(), Some("agentAAA"));

    let (is_side, agent): (bool, Option<String>) = conn
        .query_row(
            "SELECT is_sidechain, agent_id FROM messages WHERE uuid = 'mu1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(!is_side);
    assert_eq!(agent, None);
}

#[test]
fn tool_calls_tag_sidechain_rows() {
    let conn = setup_db("mixed_sidechain_session.jsonl");
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 3, "Task + Read + Grep");

    let main_only: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE NOT is_sidechain",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(main_only, 1, "only the Task call is main-loop");

    let agent: Option<String> = conn
        .query_row(
            "SELECT agent_id FROM tool_calls WHERE name = 'Read'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(agent.as_deref(), Some("agentAAA"));
}

#[test]
fn tool_results_tag_sidechain_rows() {
    let conn = setup_db("mixed_sidechain_session.jsonl");
    let is_side: bool = conn
        .query_row(
            "SELECT is_sidechain FROM tool_results WHERE tool_use_id = 'toolu_s1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(is_side);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test views_test tag_sidechain -- --nocapture` and `cargo test --test views_test tool_results_tag`
Expected: FAIL: compile error / `Binder Error: column "is_sidechain" not found`.

- [ ] **Step 4: Add the expression constants**

In `src/views.rs`, just below the existing `PROJECT_EXPR` const, add:

```rust
/// SQL expression for the subagent identity (NULL for main-loop rows).
const AGENT_ID_EXPR: &str = "json_extract_string(json, '$.agentId')";

/// SQL expression for the sidechain flag (always non-null; false for main loop).
const IS_SIDECHAIN_EXPR: &str =
    "COALESCE(CAST(json_extract(json, '$.isSidechain') AS BOOLEAN), false)";

/// SQL expression for the workflow run id, parsed from the file path.
/// NULL for plain subagents and main-loop rows.
const WORKFLOW_ID_EXPR: &str =
    "NULLIF(regexp_extract(source_file, 'workflows/(wf_[^/]+)/', 1), '')";

/// SQL expression for the subagent type, read from the sidecar meta.json at
/// index time and stored in file_registry. NULL for main-loop rows.
const AGENT_TYPE_EXPR: &str =
    "(SELECT fr.agent_type FROM file_registry fr WHERE fr.file_path = source_file)";
```

(`AGENT_TYPE_EXPR` and `WORKFLOW_ID_EXPR` are used in later tasks; defining them together keeps the block coherent. `WORKFLOW_ID_EXPR` is also used in this task's column lists.)

- [ ] **Step 5: Add columns to the `messages` view**

In `register_messages_view`, add the four columns to BOTH the `string_msgs` and `array_msgs` CTE SELECT lists, after the existing `model` column. The UNION ALL requires identical column order in both branches.

In `string_msgs`, change the tail of the SELECT from:

```rust
                json_extract_string(json, '$.message.model') AS model
            FROM raw_records
```

to:

```rust
                json_extract_string(json, '$.message.model') AS model,
                {AGENT_ID_EXPR} AS agent_id,
                {IS_SIDECHAIN_EXPR} AS is_sidechain,
                {AGENT_TYPE_EXPR} AS agent_type,
                {WORKFLOW_ID_EXPR} AS workflow_id
            FROM raw_records
```

Apply the identical change to the `array_msgs` branch (same four lines after its `model` column).

- [ ] **Step 6: Add columns to the `tool_calls` view**

In `register_tool_calls_view`, change:

```rust
            json_extract_string(json, '$.timestamp') AS timestamp
        FROM raw_records,
```

to:

```rust
            json_extract_string(json, '$.timestamp') AS timestamp,
            {AGENT_ID_EXPR} AS agent_id,
            {IS_SIDECHAIN_EXPR} AS is_sidechain,
            {AGENT_TYPE_EXPR} AS agent_type,
            {WORKFLOW_ID_EXPR} AS workflow_id
        FROM raw_records,
```

- [ ] **Step 7: Add columns to the `tool_results` view**

In `register_tool_results_view`, change:

```rust
            json_extract_string(item, '$.content') AS content
        FROM raw_records,
```

to:

```rust
            json_extract_string(item, '$.content') AS content,
            {AGENT_ID_EXPR} AS agent_id,
            {IS_SIDECHAIN_EXPR} AS is_sidechain,
            {AGENT_TYPE_EXPR} AS agent_type,
            {WORKFLOW_ID_EXPR} AS workflow_id
        FROM raw_records,
```

- [ ] **Step 8: Add columns to the empty views**

In `register_empty_views`, add the four columns to the empty `messages`, `tool_calls`, and `tool_results` definitions (after each one's last existing column), using these typed NULLs:

```sql
            NULL::VARCHAR AS agent_id,
            false AS is_sidechain,
            NULL::VARCHAR AS agent_type,
            NULL::VARCHAR AS workflow_id
```

For `messages` it goes after `model`; for `tool_calls` after `timestamp`; for `tool_results` after `content`. Keep `WHERE 1=0` last.

- [ ] **Step 9: Run tests to verify they pass**

Run: `cargo test --test views_test`
Expected: PASS, including the three new tests and all pre-existing view tests.

- [ ] **Step 10: Commit**

```bash
git add tests/fixtures/mixed_sidechain_session.jsonl src/views.rs tests/views_test.rs
git commit -m "feat: tag row-level views with agent_id and is_sidechain"
```

---

## Task 2: Clean `sessions` counts + `subagent_count`

**Files:**
- Modify: `src/views.rs` (`register_sessions_view`, empty `sessions`)
- Test: `tests/views_test.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests/views_test.rs`:

```rust
#[test]
fn sessions_counts_exclude_sidechains() {
    let conn = setup_db("mixed_sidechain_session.jsonl");
    let (msgs, tools, users, subs): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT message_count, tool_call_count, user_message_count, subagent_count
             FROM sessions WHERE session_id = 'sess-mix'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(msgs, 2, "only main-loop messages counted");
    assert_eq!(tools, 1, "only the main-loop Task call counted");
    assert_eq!(users, 1, "only the main-loop user turn counted");
    assert_eq!(subs, 1, "one distinct subagent (agentAAA)");

    let first: String = conn
        .query_row(
            "SELECT first_user_message FROM sessions WHERE session_id = 'sess-mix'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(first, "run the analysis");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test views_test sessions_counts_exclude_sidechains`
Expected: FAIL: `subagent_count` column not found, or `message_count` is 5 not 2.

- [ ] **Step 3: Rewrite `register_sessions_view`**

Replace the SQL string in `register_sessions_view` with:

```rust
    let sql = "CREATE OR REPLACE VIEW sessions AS
        SELECT
            session_id,
            project,
            MIN(timestamp) FILTER (WHERE NOT is_sidechain) AS started_at,
            MAX(timestamp) FILTER (WHERE NOT is_sidechain) AS ended_at,
            COUNT(*) FILTER (WHERE NOT is_sidechain) AS message_count,
            CAST(COALESCE(SUM(tool_count) FILTER (WHERE NOT is_sidechain), 0) AS BIGINT) AS tool_call_count,
            COUNT(*) FILTER (WHERE type = 'user' AND NOT is_sidechain) AS user_message_count,
            COUNT(DISTINCT agent_id) AS subagent_count,
            (SELECT text FROM messages m2
             WHERE m2.session_id = m1.session_id
             AND m2.type = 'user'
             AND NOT m2.is_sidechain
             AND m2.text IS NOT NULL
             AND m2.text != ''
             AND m2.text NOT LIKE '<%'
             AND m2.text NOT LIKE 'Base directory for this skill%'
             AND m2.text NOT LIKE '#%'
             ORDER BY m2.timestamp LIMIT 1) AS first_user_message
        FROM messages m1
        GROUP BY session_id, project";
```

(`subagent_count` counts distinct `agent_id`; main-loop rows have `agent_id = NULL`, which `COUNT(DISTINCT)` ignores.)

- [ ] **Step 4: Add `subagent_count` to the empty `sessions` view**

In `register_empty_views`, in the empty `sessions` definition, add after `user_message_count`:

```sql
            CAST(0 AS BIGINT) AS subagent_count,
```

(Keep `first_user_message` as the last column, then `WHERE 1=0`.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test views_test`
Expected: PASS (new test + all existing session tests).

- [ ] **Step 6: Commit**

```bash
git add src/views.rs tests/views_test.rs
git commit -m "feat: keep sessions counts main-loop-only, add subagent_count"
```

---

## Task 3: `workflow_id` from the file path

**Files:**
- Create: `tests/fixtures/subagents/workflows/wf_testrun/agent-wf1.jsonl`
- Test: `tests/views_test.rs`

(The `WORKFLOW_ID_EXPR` column was already added in Task 1; this task proves the path extraction with a fixture placed at a workflow path.)

- [ ] **Step 1: Create the nested fixture**

Create `tests/fixtures/subagents/workflows/wf_testrun/agent-wf1.jsonl` with one line:

```json
{"type":"assistant","message":{"id":"w1","role":"assistant","model":"claude-haiku-4-5","content":[{"type":"tool_use","id":"toolu_w1","name":"Glob","input":{"pattern":"**/*.rs"}}]},"uuid":"wa1","parentUuid":null,"isSidechain":true,"agentId":"agentWF","timestamp":"2026-05-01T11:00:00.000Z","sessionId":"sess-wf","cwd":"/Users/test/myproject"}
```

- [ ] **Step 2: Write the failing test**

Append to `tests/views_test.rs`:

```rust
#[test]
fn workflow_id_extracted_from_path() {
    let conn = setup_db("subagents/workflows/wf_testrun/agent-wf1.jsonl");
    let wf: Option<String> = conn
        .query_row(
            "SELECT workflow_id FROM tool_calls WHERE name = 'Glob'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(wf.as_deref(), Some("wf_testrun"));
}

#[test]
fn workflow_id_null_for_non_workflow() {
    let conn = setup_db("multi_tool_session.jsonl");
    let nulls: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE workflow_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nulls, 0);
}
```

(`setup_db` joins the fixture name onto `tests/fixtures/`, so a relative path with slashes resolves correctly.)

- [ ] **Step 3: Run tests**

Run: `cargo test --test views_test workflow_id`
Expected: PASS (the column and expr already exist from Task 1; this confirms extraction).

If it FAILS because `regexp_extract`'s 3-arg form differs in the bundled DuckDB version, verify with `cargo run -- sql "SELECT regexp_extract('a/workflows/wf_x/b', 'workflows/(wf_[^/]+)/', 1)"`; it must return `wf_x`. Adjust `WORKFLOW_ID_EXPR` only if this prints empty.

- [ ] **Step 4: Commit**

```bash
git add tests/fixtures/subagents/workflows/wf_testrun/agent-wf1.jsonl tests/views_test.rs
git commit -m "test: confirm workflow_id extraction from path"
```

---

## Task 4: `agent_type` plumbing (cache schema + view)

The `AGENT_TYPE_EXPR` column already exists in the views (Task 1). It reads `file_registry.agent_type`, which does not exist yet. This task adds the column to the cache schema and the test helpers, and proves the view surfaces it.

**Files:**
- Modify: `src/cache.rs` (`SCHEMA_VERSION`, `file_registry` DDL)
- Modify: `tests/views_test.rs` (`setup_db`, `setup_db_multi` helpers; new test)

- [ ] **Step 1: Add `agent_type` to the cache schema**

In `src/cache.rs`, bump the version:

```rust
pub const SCHEMA_VERSION: i32 = 3;
```

In `rebuild`, change the `file_registry` table definition to include the column:

```rust
        CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT NOT NULL,
            file_size BIGINT NOT NULL,
            cwd TEXT,
            agent_type TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        );
```

- [ ] **Step 2: Add `agent_type` to the test helpers**

In `tests/views_test.rs`, in BOTH `setup_db` and `setup_db_multi`, change the `file_registry` creation to include `agent_type`:

```rust
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT,
            file_size BIGINT,
            cwd TEXT,
            agent_type TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )"
    ).unwrap();
```

- [ ] **Step 3: Write the failing test**

Append to `tests/views_test.rs`:

```rust
#[test]
fn agent_type_flows_from_registry() {
    use std::path::PathBuf;
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY,
            mtime_ns BIGINT,
            file_size BIGINT,
            cwd TEXT,
            agent_type TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )",
    )
    .unwrap();

    let wf_path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("subagents/workflows/wf_testrun/agent-wf1.jsonl");
    let wf_str = wf_path.display().to_string();

    // Register the subagent's agent_type as the indexer would.
    conn.execute(
        "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type)
         VALUES (?, 0, 0, NULL, 'Explore')",
        [&wf_str],
    )
    .unwrap();

    cq::views::register_views(&conn, &[wf_path]).unwrap();

    let at: Option<String> = conn
        .query_row(
            "SELECT agent_type FROM tool_calls WHERE name = 'Glob'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(at.as_deref(), Some("Explore"));
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --test views_test agent_type_flows_from_registry`
Expected: PASS. Also run the full `cargo test --test views_test` to confirm the helper change did not break existing tests.

- [ ] **Step 5: Commit**

```bash
git add src/cache.rs tests/views_test.rs
git commit -m "feat: add agent_type to cache schema and surface it in views"
```

---

## Task 5: Recursive indexer scan + exclude `journal.jsonl`

**Files:**
- Modify: `src/indexer.rs` (`scan_directory` + new helpers)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write the failing integration test**

Append to `tests/integration_test.rs`. This builds a realistic tree by hand (not via `setup_env`, which is flat):

```rust
fn write(path: &std::path::Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn setup_subagent_env() -> TestEnv {
    let projects = TempDir::new().unwrap();
    let proj = projects.path().join("-Users-test-myproject");
    let sess = "11111111-1111-1111-1111-111111111111";

    // Top-level main-loop session.
    write(&proj.join(format!("{sess}.jsonl")),
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}]},\"uuid\":\"a1\",\"parentUuid\":null,\"isSidechain\":false,\"timestamp\":\"2026-05-01T10:00:00.000Z\",\"sessionId\":\"11111111-1111-1111-1111-111111111111\",\"cwd\":\"/Users/test/myproject\"}\n");

    // Plain subagent.
    let sub = proj.join(sess).join("subagents");
    write(&sub.join("agent-aaa.jsonl"),
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m2\",\"role\":\"assistant\",\"model\":\"claude-haiku-4-5\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t2\",\"name\":\"Read\",\"input\":{\"file_path\":\"/x\"}}]},\"uuid\":\"a2\",\"parentUuid\":null,\"isSidechain\":true,\"agentId\":\"aaa\",\"timestamp\":\"2026-05-01T10:00:01.000Z\",\"sessionId\":\"11111111-1111-1111-1111-111111111111\",\"cwd\":\"/Users/test/myproject\"}\n");
    std::fs::write(sub.join("agent-aaa.meta.json"), "{\"agentType\":\"general-purpose\"}").unwrap();

    // Workflow subagent + journal ledger (must be excluded).
    let wf = sub.join("workflows").join("wf_xyz");
    write(&wf.join("agent-bbb.jsonl"),
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m3\",\"role\":\"assistant\",\"model\":\"claude-haiku-4-5\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t3\",\"name\":\"Glob\",\"input\":{\"pattern\":\"*\"}}]},\"uuid\":\"a3\",\"parentUuid\":null,\"isSidechain\":true,\"agentId\":\"bbb\",\"timestamp\":\"2026-05-01T10:00:02.000Z\",\"sessionId\":\"11111111-1111-1111-1111-111111111111\",\"cwd\":\"/Users/test/myproject\"}\n");
    std::fs::write(wf.join("agent-bbb.meta.json"), "{\"agentType\":\"Explore\"}").unwrap();
    std::fs::write(wf.join("journal.jsonl"), "{\"type\":\"started\",\"key\":\"v2:abc\",\"agentId\":\"bbb\"}\n").unwrap();

    TestEnv { projects, cache: TempDir::new().unwrap() }
}

#[test]
fn indexes_subagent_tool_calls() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args(["sql", "SELECT COUNT(*) FROM tool_calls WHERE is_sidechain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2")); // Read + Glob
}

#[test]
fn excludes_journal_jsonl() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args(["sql", "SELECT COUNT(*) FROM raw_records WHERE source_file LIKE '%journal.jsonl%'"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0"));
}

#[test]
fn workflow_id_visible_end_to_end() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args(["sql", "SELECT workflow_id FROM tool_calls WHERE name = 'Glob'"])
        .assert()
        .success()
        .stdout(predicate::str::contains("wf_xyz"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test integration_test indexes_subagent_tool_calls excludes_journal_jsonl workflow_id_visible_end_to_end`
Expected: FAIL: subagent count is 0 (not indexed) and `wf_xyz` absent.

- [ ] **Step 3: Make the scan recursive**

In `src/indexer.rs`, replace `scan_directory` with a recursive walk plus two helpers:

```rust
fn scan_directory(dir: &Path) -> Result<HashMap<PathBuf, FileInfo>> {
    let mut files = HashMap::new();
    collect_jsonl(dir, &mut files)?;
    Ok(files)
}

/// Recursively collect indexable JSONL transcripts under `dir`.
fn collect_jsonl(dir: &Path, files: &mut HashMap<PathBuf, FileInfo>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_jsonl(&path, files)?;
        } else if is_indexable_jsonl(&path) {
            if let Ok(metadata) = std::fs::metadata(&path) {
                let mtime_ns = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                files.insert(
                    path,
                    FileInfo {
                        mtime_ns,
                        file_size: metadata.len() as i64,
                    },
                );
            }
        }
    }
    Ok(())
}

/// A `.jsonl` transcript we should index. Excludes workflow `journal.jsonl`
/// ledgers, which are resume bookkeeping, not transcripts.
fn is_indexable_jsonl(path: &Path) -> bool {
    path.extension().map(|e| e == "jsonl").unwrap_or(false)
        && path.file_name().map(|n| n != "journal.jsonl").unwrap_or(false)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test integration_test indexes_subagent_tool_calls excludes_journal_jsonl workflow_id_visible_end_to_end`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/indexer.rs tests/integration_test.rs
git commit -m "feat: recurse into subagents/ when scanning, exclude journal.jsonl"
```

---

## Task 6: Capture `agent_type` from `meta.json` during indexing

**Files:**
- Modify: `src/indexer.rs` (`index_files` + helper)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests/integration_test.rs`:

```rust
#[test]
fn populates_agent_type_from_meta() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args(["sql", "SELECT agent_type FROM tool_calls WHERE name = 'Read'"])
        .assert()
        .success()
        .stdout(predicate::str::contains("general-purpose"));
}

#[test]
fn agent_type_null_for_main_loop() {
    let env = setup_subagent_env();
    cq_cmd(&env)
        .args(["sql", "SELECT COUNT(*) FROM tool_calls WHERE name = 'Bash' AND agent_type IS NULL"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test integration_test populates_agent_type_from_meta`
Expected: FAIL: `agent_type` is NULL (meta.json not read yet).

- [ ] **Step 3: Add the meta.json reader**

In `src/indexer.rs`, add (near the other free functions):

```rust
/// Read `agentType` from the sibling `agent-<id>.meta.json`, if this file is a
/// subagent transcript and the sidecar exists.
fn read_agent_type(file: &Path) -> Option<String> {
    let name = file.file_name()?.to_str()?;
    if !name.starts_with("agent-") {
        return None;
    }
    let stem = file.file_stem()?.to_str()?;
    let meta = file.with_file_name(format!("{stem}.meta.json"));
    let data = std::fs::read_to_string(&meta).ok()?;
    let value: serde_json::Value = serde_json::from_str(&data).ok()?;
    value.get("agentType")?.as_str().map(|s| s.to_string())
}
```

- [ ] **Step 4: Write `agent_type` into the registry**

In `index_files`, after the `cwd` lookup and before the `metadata`/`INSERT`, add:

```rust
        let agent_type = read_agent_type(file);
```

Then change the INSERT from:

```rust
        conn.execute(
            "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd) VALUES (?, ?, ?, ?)",
            duckdb::params![path_str, mtime_ns, file_size, cwd],
        )?;
```

to:

```rust
        conn.execute(
            "INSERT INTO file_registry (file_path, mtime_ns, file_size, cwd, agent_type) VALUES (?, ?, ?, ?, ?)",
            duckdb::params![path_str, mtime_ns, file_size, cwd, agent_type],
        )?;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test integration_test`
Expected: PASS (new tests + all prior integration tests).

- [ ] **Step 6: Commit**

```bash
git add src/indexer.rs tests/integration_test.rs
git commit -m "feat: capture subagent agentType from meta.json into file_registry"
```

---

## Task 7: Recursive `max_dir_mtime` for Auto-sync freshness

**Files:**
- Modify: `src/indexer.rs` (`max_dir_mtime` + helpers; add unit test)

- [ ] **Step 1: Write the failing unit test**

In `src/indexer.rs`, add (or extend) a `#[cfg(test)] mod tests` block at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_scope::SyncScope;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn max_dir_mtime_detects_deep_new_file() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("-Users-test-myproject");
        let sub = proj.join("sess-1").join("subagents");
        std::fs::create_dir_all(&sub).unwrap();

        let scope = SyncScope::All;
        let before = max_dir_mtime(tmp.path(), &scope).unwrap();

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(sub.join("agent-new.jsonl"), "{}\n").unwrap();

        let after = max_dir_mtime(tmp.path(), &scope).unwrap();
        assert!(
            after > before,
            "a new deep subagent file must advance max_dir_mtime (before={before}, after={after})"
        );
    }
}
```

(If `src/indexer.rs` already has a `#[cfg(test)] mod tests`, add just the test fn and any missing `use` lines inside it.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib max_dir_mtime_detects_deep_new_file`
Expected: FAIL: `after == before` (shallow stat misses the deep file).

- [ ] **Step 3: Make `max_dir_mtime` recursive**

In `src/indexer.rs`, add two helpers above `max_dir_mtime`:

```rust
fn dir_mtime(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Max mtime of `dir` and all of its subdirectories (recursively). Creating any
/// new file bumps its immediate parent dir's mtime, so this catches new sessions,
/// subagents, and workflow agents. It does not catch pure appends (no directory
/// mtime changes on append); `--reindex` covers that case.
fn max_mtime_recursive(dir: &Path) -> i64 {
    let mut max = dir_mtime(dir);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let m = max_mtime_recursive(&entry.path());
                if m > max {
                    max = m;
                }
            }
        }
    }
    max
}
```

Then rewrite the `SyncScope::All` and `SyncScope::Projects` arms of `max_dir_mtime` to fold `max_mtime_recursive` over the directories instead of a single `std::fs::metadata` stat. Replace the body from the `let dirs_to_check ...` block through the final loop with:

```rust
    let dirs_to_check: Vec<PathBuf> = match scope {
        SyncScope::All => {
            if !projects_dir.exists() {
                return Ok(0);
            }
            std::fs::read_dir(projects_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect()
        }
        SyncScope::Projects(dirs) => dirs.clone(),
        SyncScope::File(f) => {
            return Ok(dir_mtime(f));
        }
    };

    let mut max_mtime: i64 = 0;
    for dir in &dirs_to_check {
        let m = max_mtime_recursive(dir);
        if m > max_mtime {
            max_mtime = m;
        }
    }

    Ok(max_mtime)
```

(The `SyncScope::File` arm previously returned the file's own mtime via `metadata`; `dir_mtime(f)` preserves that. `dir_mtime` works on files too.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib max_dir_mtime_detects_deep_new_file`
Expected: PASS.

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: PASS across unit, view, and integration tests.

- [ ] **Step 6: Commit**

```bash
git add src/indexer.rs
git commit -m "feat: recursive max_dir_mtime so Auto-sync detects deep subagent files"
```

---

## Task 8: Recursive `discover_files` + session-dir-aware filter (trait consistency)

`discover_files` is no longer on the production path (views read the cached `raw_records` table), but it is part of the `TranscriptProvider` trait and exercised by its own unit tests. Keep it correct so the trait does not silently miss subagents.

**Files:**
- Modify: `src/claude_provider.rs` (`discover_files` + helper; add unit test)

- [ ] **Step 1: Write the failing test**

In `src/claude_provider.rs`, inside the existing `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn discover_files_includes_subagents() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("-Users-alice-myproject");
        let sess = "abc12345-0000-0000-0000-000000000000";
        let sub = proj.join(sess).join("subagents");
        fs::create_dir_all(&sub).unwrap();
        fs::write(proj.join(format!("{sess}.jsonl")), "{}").unwrap();
        fs::write(sub.join("agent-x.jsonl"), "{}").unwrap();
        fs::write(sub.join("journal.jsonl"), "{}").unwrap();

        let provider = ClaudeProvider::new_with_base(tmp.path().to_path_buf());

        // No filter: top-level + subagent, but not the journal ledger.
        let all = provider.discover_files(&QueryScope::new(None, None, None)).unwrap();
        assert_eq!(all.len(), 2, "session file + subagent file, journal excluded");

        // Session filter matches the parent session dir for the nested file.
        let scoped = provider
            .discover_files(&QueryScope::new(None, Some("abc12345".to_string()), None))
            .unwrap();
        assert_eq!(scoped.len(), 2);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib discover_files_includes_subagents`
Expected: FAIL: only the top-level file is found (count 1), or the journal is wrongly included.

- [ ] **Step 3: Rewrite `discover_files`**

Replace the `discover_files` method body in `src/claude_provider.rs` with a recursive walk that derives each file's session id from its path:

```rust
    fn discover_files(&self, scope: &QueryScope) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        if !self.base_dir.exists() {
            return Ok(files);
        }

        let project_dirs: Vec<_> = std::fs::read_dir(&self.base_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();

        for entry in project_dirs {
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if let Some(ref project) = scope.project {
                if !self.matches_project(&dir_name, project) {
                    continue;
                }
            }
            let project_path = entry.path();
            let mut found = Vec::new();
            collect_jsonl_paths(&project_path, &mut found);
            for path in found {
                if let Some(ref session) = scope.session {
                    let sid = session_id_for_file(&project_path, &path);
                    if !sid.starts_with(session.as_str()) {
                        continue;
                    }
                }
                files.push(path);
            }
        }
        Ok(files)
    }
```

Add these free functions to `src/claude_provider.rs` (module level, outside the `impl`):

```rust
/// Recursively collect indexable `.jsonl` files under `dir`, excluding
/// workflow `journal.jsonl` ledgers.
fn collect_jsonl_paths(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_jsonl_paths(&path, out);
        } else if path.extension().map(|e| e == "jsonl").unwrap_or(false)
            && path.file_name().map(|n| n != "journal.jsonl").unwrap_or(false)
        {
            out.push(path);
        }
    }
}

/// The session a transcript belongs to: the first path component under the
/// project dir, with any `.jsonl` extension stripped. For a top-level file
/// `<project>/<uuid>.jsonl` this is `<uuid>`; for a nested subagent file
/// `<project>/<uuid>/subagents/agent-x.jsonl` it is also `<uuid>`.
fn session_id_for_file(project_dir: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(project_dir).unwrap_or(file);
    match rel.components().next() {
        Some(first) => {
            let s = first.as_os_str().to_string_lossy();
            s.strip_suffix(".jsonl").unwrap_or(&s).to_string()
        }
        None => String::new(),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS, including the existing `session_prefix_filtering`, `discover_files_no_filter_returns_all`, and `project_filter_excludes_nonmatching` tests (they use flat layouts that the recursive walk still handles).

- [ ] **Step 5: Commit**

```bash
git add src/claude_provider.rs
git commit -m "feat: recurse in discover_files, match session by parent dir"
```

---

## Task 9: Documentation sync

Run through the "Keeping docs in sync" row for "A SQL view's columns" plus the module/sync rows.

**Files:**
- Modify: `src/commands/schema.rs`, `README.md`, `CLAUDE.md`, `claude-plugin/skills/cq/SKILL.md`

- [ ] **Step 1: Update `cq schema` view docs**

In `src/commands/schema.rs`, append these four lines to the column list of `MESSAGES_SCHEMA`, `TOOL_CALLS_SCHEMA`, and `TOOL_RESULTS_SCHEMA` (after each block's last column, before any closing `"#`/`Note:`):

```
  agent_id            VARCHAR   Subagent id; NULL for main-loop rows
  is_sidechain        BOOLEAN   true if this row is from a subagent
  agent_type          VARCHAR   Subagent type from meta.json (e.g. 'Explore'); NULL for main loop
  workflow_id         VARCHAR   Workflow run id (wf_...) if spawned by a workflow, else NULL
```

For `TOOL_CALLS_SCHEMA`, place them before the trailing `Note:` paragraph.

In `SESSIONS_SCHEMA`, add after `user_message_count`:

```
  subagent_count      BIGINT    Distinct subagents spawned in this session
```

Make the identical edits to the duplicated blocks inside the `SCHEMA_DOCS` const (it repeats all four view schemas).

- [ ] **Step 2: Add an example query**

In `EXAMPLE_QUERIES` and the example section of `SCHEMA_DOCS`, add before "Recent sessions:":

```
Subagent tool calls in a session:
  SELECT agent_type, name, COUNT(*) AS count
  FROM tool_calls
  WHERE session_id = 'SESSION_ID' AND is_sidechain
  GROUP BY agent_type, name
  ORDER BY count DESC;
```

- [ ] **Step 3: Update `README.md` Views section**

In `README.md`, replace the Views bullet list with:

```markdown
- **sessions** - one row per session with timestamps, message counts, tool counts (main-loop only), plus a `subagent_count`
- **messages** - one row per conversation turn (user or assistant)
- **tool_calls** - one row per tool invocation, with input as queryable JSON
- **tool_results** - one row per tool response, with an error flag

Subagent activity is indexed too: `messages`, `tool_calls`, and `tool_results` carry `is_sidechain`, `agent_id`, `agent_type`, and `workflow_id` so you can include, exclude, or focus subagents. `cq sessions` stays main-loop-only.
```

- [ ] **Step 4: Update `CLAUDE.md`**

In `CLAUDE.md`, in the architecture tree, change the `indexer.rs` and `claude_provider.rs` lines to:

```
indexer.rs        Incremental sync: file_registry + recursive mtime fast-path, fs2 file lock; recurses into <session>/subagents/** and captures agentType from meta.json
claude_provider.rs  ClaudeProvider: recursively discovers JSONL files (incl. subagents) from ~/.claude/projects/
```

In the "Key patterns" `Persistent cache + incremental sync` bullet, append:

```
The scan recurses into `<session>/subagents/**` (excluding `journal.jsonl`); subagent rows carry the parent `session_id` plus `is_sidechain`/`agent_id`/`agent_type`/`workflow_id` tags. The Auto mtime fast-path is a recursive max so new deep files are detected.
```

- [ ] **Step 5: Update the cq skill**

In `claude-plugin/skills/cq/SKILL.md`, under "## View Schemas", replace the four schema lines with:

```markdown
**sessions**: session_id, project, started_at, ended_at, message_count, tool_call_count, user_message_count, subagent_count, first_user_message (counts are main-loop only)

**messages**: session_id, project, uuid, parent_uuid, type, timestamp, text, tool_count, model, agent_id, is_sidechain, agent_type, workflow_id

**tool_calls**: session_id, project, message_uuid, tool_use_id, name, input (JSON), timestamp, agent_id, is_sidechain, agent_type, workflow_id

**tool_results**: session_id, project, tool_use_id, is_error, content, agent_id, is_sidechain, agent_type, workflow_id

Subagent rows carry the parent `session_id`. Filter with `WHERE NOT is_sidechain` (main loop), `WHERE is_sidechain` (subagents), `WHERE agent_type = 'Explore'`, or `WHERE workflow_id IS NOT NULL`.
```

- [ ] **Step 6: Verify schema output and commit**

Run: `cargo run -- schema | grep -E "agent_id|is_sidechain|subagent_count|workflow_id"`
Expected: the new columns print.

```bash
git add src/commands/schema.rs README.md CLAUDE.md claude-plugin/skills/cq/SKILL.md
git commit -m "docs: document subagent tag columns and subagent_count"
```

---

## Task 10: Full-suite verification + real-data check

**Files:** none (verification only)

- [ ] **Step 1: Run the whole suite**

Run: `cargo test`
Expected: PASS. Confirm the counts in `CLAUDE.md`'s "Tests" section still make sense (new tests added to each group).

- [ ] **Step 2: Build release-quality and lint**

Run: `cargo clippy --all-targets -- -D warnings` then `cargo fmt --check`
Expected: clean. Fix any warnings (unused imports from removed code, etc.) and re-commit if needed.

- [ ] **Step 3: Real-data check against a known workflow session**

Run a `--reindex` against the live corpus and confirm subagents surface:

```bash
cargo run -- --reindex sql "SELECT agent_type, COUNT(*) AS n FROM tool_calls WHERE session_id = '725b3651-1211-4a33-a51d-d02d307d8791' AND is_sidechain GROUP BY agent_type ORDER BY n DESC"
```

Expected: rows for the workflow's subagents (e.g. `Explore`) with non-zero counts, matching the ~37 subagents / ~370 tool calls described in the bean. Also spot-check the overview stays clean:

```bash
cargo run -- sql "SELECT subagent_count FROM sessions WHERE session_id = '725b3651-1211-4a33-a51d-d02d307d8791'"
```

Expected: a non-zero `subagent_count`, while `message_count`/`tool_call_count` remain main-loop sized.

- [ ] **Step 4: Final commit if anything changed**

```bash
git add -A
git commit -m "chore: verification pass for subagent indexing" || true
```

---

## Self-review notes

- **Spec coverage:** recursion (Task 5), journal exclusion (Task 5), agent_type capture (Tasks 4+6), tag columns + NULL convention (Tasks 1,3,4), sessions clean + subagent_count (Task 2), recursive mtime (Task 7), cache migration (Task 4), discover_files trait consistency (Task 8), all doc surfaces from the sync table (Task 9), real-data verification (Task 10). All spec sections map to a task.
- **Ordering dependency:** Task 4 (cache schema + `agent_type` column) must land before Task 5/6 integration tests, because the views reference `file_registry.agent_type` and the real binary opens the cache. The plan orders them this way.
- **Type consistency:** column names (`agent_id`, `is_sidechain`, `agent_type`, `workflow_id`, `subagent_count`) and the expr const names (`AGENT_ID_EXPR`, `IS_SIDECHAIN_EXPR`, `AGENT_TYPE_EXPR`, `WORKFLOW_ID_EXPR`) are used identically across tasks.
