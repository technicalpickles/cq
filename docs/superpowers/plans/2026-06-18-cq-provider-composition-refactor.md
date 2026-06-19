# cq Provider-Composition Refactor (PR 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor cq's view registration so the four SQL views (`messages`, `tool_calls`, `tool_results`, `sessions`) are composed from per-provider SQL contributions via `UNION ALL`, and add a `harness` column to the view contract — all with zero behavior change for the only current provider (Claude).

**Architecture:** Today `db::setup_connection` hardcodes the indexer sync then calls `views::register_derived_views`, which builds the four views directly over the `raw_records` cache table. This PR introduces a `View` enum and two new `TranscriptProvider` trait methods — `prepare(&Connection) -> Result<bool>` (is this provider active?) and `contribute_view_sql(View) -> Option<String>` (a SELECT *body* for one view) — plus a `views::compose_views` composer that wraps each active provider's body in parens and `UNION ALL`s them into the final view. With Claude as the sole provider, every view is a single-contribution pass-through, so existing tests stay green. The new `harness` column is `'claude'` for all Claude rows. PR 2 (`OpenCodeProvider`, separate plan) plugs into this seam.

**Tech Stack:** Rust, DuckDB (`duckdb` crate `=1.10501.0`, bundled), `anyhow`, `cargo test`.

---

## Context the implementer needs

- **Run tests:** `cargo test` from `repos/cq/worktrees/opencode-source-prototype`. The first build compiles DuckDB from source (~3GB, several minutes). Subsequent builds are fast.
- **The three test files:** `tests/views_test.rs` (view SQL correctness against JSONL fixtures), `tests/integration_test.rs` (end-to-end CLI), `src/*.rs` unit tests. The view tests build views via `cq::views::register_views(&conn, &paths)` after manually creating a `file_registry` table — see `setup_db` / `setup_db_multi` helpers at the top of `tests/views_test.rs`.
- **Why `harness` must be in `sessions` AND filtered:** `register_sessions_view` aggregates `FROM messages GROUP BY session_id`. Once `messages` is a `UNION` across providers (PR 2), an unfiltered Claude `sessions` body would aggregate opencode rows too. Adding `WHERE harness = 'claude'` to the Claude `sessions` body is a no-op today (all rows are Claude) and prevents that latent bug. Bake it in now.
- **`prepare` is deliberately trivial for Claude in this PR.** The JSONL→`raw_records` sync stays in `db.rs`/`indexer.rs` (it's Claude-specific machinery PR 2 doesn't touch). `ClaudeProvider::prepare` just returns `Ok(true)`. The method exists to establish the seam; `OpenCodeProvider::prepare` (PR 2) is where it does real work (`INSTALL`/`LOAD`/`ATTACH`).
- **Docs-in-sync rule:** `CLAUDE.md` requires that behavior changes update matching docs. The new `harness` column touches `src/commands/schema.rs` (view column docs) and the README/CLAUDE.md view tables. Task 6 covers this.
- **Column placement:** put `harness` immediately after the existing `source` column in every view, for a stable, readable column order.

---

## File Structure

- `src/provider.rs` — **Modify.** Add `View` enum; add `prepare` + `contribute_view_sql` to the `TranscriptProvider` trait (with default impls so non-Claude code compiles).
- `src/views.rs` — **Modify.** Extract the four Claude view bodies into `pub` body-builder functions returning `String` (each gaining `harness`); add `compose_views` + `empty_view_sql`; rewire `register_views`/`register_derived_views` through the composer.
- `src/claude_provider.rs` — **Modify.** Implement `prepare` (returns `Ok(true)`) and `contribute_view_sql` (delegates to the `views::claude_*_sql` body functions).
- `src/db.rs` — **Modify.** `setup_connection` takes the provider list, runs `prepare` on each to get the active set, and calls `compose_views` instead of `register_derived_views`.
- `src/main.rs` — **Modify.** Build `Vec<Box<dyn TranscriptProvider>>` (just Claude for now) and pass it to `setup_connection`.
- `src/commands/schema.rs` — **Modify.** Add `harness` to the documented column lists.
- `tests/views_test.rs` — **Modify.** Add `harness`-column regression tests; fix any column-set assertions broken by the new column.
- `README.md`, `CLAUDE.md` — **Modify.** Add `harness` to the view column tables.

---

## Task 1: Add the `View` enum and extend the `TranscriptProvider` trait

**Files:**
- Modify: `src/provider.rs`

- [ ] **Step 1: Add the `View` enum and trait methods**

In `src/provider.rs`, add the enum below the imports and extend the trait. The two new methods get default impls so the trait stays object-safe and existing impls compile until updated:

```rust
/// The four queryable views cq composes from provider contributions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Messages,
    ToolCalls,
    ToolResults,
    Sessions,
}

impl View {
    /// All views in dependency order. `Sessions` aggregates over `messages`,
    /// so `Messages` must be composed before `Sessions`.
    pub const ALL: [View; 4] = [View::Messages, View::ToolCalls, View::ToolResults, View::Sessions];

    /// The SQL view name.
    pub fn name(self) -> &'static str {
        match self {
            View::Messages => "messages",
            View::ToolCalls => "tool_calls",
            View::ToolResults => "tool_results",
            View::Sessions => "sessions",
        }
    }
}

pub trait TranscriptProvider {
    fn name(&self) -> &str;
    fn discover_files(&self, scope: &QueryScope) -> Result<Vec<PathBuf>>;
    fn register_views(&self, conn: &Connection, files: &[PathBuf]) -> Result<()>;
    fn list_projects(&self) -> Result<Vec<ProjectInfo>>;

    /// Prepare the connection for this provider and report whether it is active
    /// (should contribute views). Claude: returns Ok(true). Future providers may
    /// ATTACH external storage here and return Ok(false) to sit out gracefully.
    fn prepare(&self, _conn: &Connection) -> Result<bool> {
        Ok(true)
    }

    /// The SQL SELECT *body* (no `CREATE VIEW` wrapper) this provider contributes
    /// for `view`, or None if it contributes nothing. The composer wraps each body
    /// in parens and `UNION ALL`s contributions from all active providers.
    fn contribute_view_sql(&self, _view: View) -> Option<String> {
        None
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build`
Expected: PASS (warnings about unused `View` are fine at this point).

- [ ] **Step 3: Commit**

```bash
git add src/provider.rs
git commit -m "refactor: add View enum and prepare/contribute_view_sql trait methods"
```

---

## Task 2: Extract Claude view bodies into functions and add the `harness` column

**Files:**
- Modify: `src/views.rs`

This task extracts the inner SQL of each `register_*_view` function into a `pub fn claude_*_sql() -> String` that returns just the SELECT body (no `CREATE OR REPLACE VIEW ... AS`), with `'claude' AS harness` added. The existing `register_*_view` functions will be removed in Task 3 (replaced by the composer), so here we only *add* the body functions.

- [ ] **Step 1: Add `claude_messages_sql`**

Add to `src/views.rs`. This is the body from `register_messages_view`, with `'claude' AS harness` added after `source` in both CTEs:

```rust
/// The Claude `messages` view body (SELECT only, no CREATE VIEW wrapper).
pub fn claude_messages_sql() -> String {
    format!("WITH string_msgs AS (
            SELECT
                json_extract_string(json, '$.sessionId') AS session_id,
                {PROJECT_EXPR} AS project,
                {SOURCE_EXPR} AS source,
                'claude' AS harness,
                json_extract_string(json, '$.uuid') AS uuid,
                json_extract_string(json, '$.parentUuid') AS parent_uuid,
                json_extract_string(json, '$.type') AS type,
                json_extract_string(json, '$.timestamp') AS timestamp,
                json_extract_string(json, '$.message.content') AS text,
                CAST(0 AS BIGINT) AS tool_count,
                json_extract_string(json, '$.message.model') AS model,
                {AGENT_ID_EXPR} AS agent_id,
                {IS_SIDECHAIN_EXPR} AS is_sidechain,
                {AGENT_TYPE_EXPR} AS agent_type,
                {WORKFLOW_ID_EXPR} AS workflow_id
            FROM raw_records
            WHERE json_extract_string(json, '$.type') IN ('user', 'assistant')
            AND json_type(json_extract(json, '$.message.content')) = 'VARCHAR'
        ),
        array_msgs AS (
            SELECT
                json_extract_string(json, '$.sessionId') AS session_id,
                {PROJECT_EXPR} AS project,
                {SOURCE_EXPR} AS source,
                'claude' AS harness,
                json_extract_string(json, '$.uuid') AS uuid,
                json_extract_string(json, '$.parentUuid') AS parent_uuid,
                json_extract_string(json, '$.type') AS type,
                json_extract_string(json, '$.timestamp') AS timestamp,
                (SELECT json_extract_string(item, '$.text')
                 FROM (SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item)
                 WHERE json_extract_string(item, '$.type') = 'text'
                 LIMIT 1) AS text,
                CASE WHEN json_extract_string(json, '$.type') = 'assistant' THEN
                    (SELECT COUNT(*)
                     FROM (SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item)
                     WHERE json_extract_string(item, '$.type') = 'tool_use')
                ELSE CAST(0 AS BIGINT)
                END AS tool_count,
                json_extract_string(json, '$.message.model') AS model,
                {AGENT_ID_EXPR} AS agent_id,
                {IS_SIDECHAIN_EXPR} AS is_sidechain,
                {AGENT_TYPE_EXPR} AS agent_type,
                {WORKFLOW_ID_EXPR} AS workflow_id
            FROM raw_records
            WHERE json_extract_string(json, '$.type') IN ('user', 'assistant')
            AND json_type(json_extract(json, '$.message.content')) = 'ARRAY'
        )
        SELECT * FROM string_msgs
        UNION ALL
        SELECT * FROM array_msgs")
}
```

- [ ] **Step 2: Add `claude_tool_calls_sql`**

```rust
/// The Claude `tool_calls` view body.
pub fn claude_tool_calls_sql() -> String {
    format!("SELECT
            json_extract_string(json, '$.sessionId') AS session_id,
            {PROJECT_EXPR} AS project,
            {SOURCE_EXPR} AS source,
            'claude' AS harness,
            json_extract_string(json, '$.uuid') AS message_uuid,
            json_extract_string(item, '$.id') AS tool_use_id,
            json_extract_string(item, '$.name') AS name,
            json_extract(item, '$.input') AS input,
            json_extract_string(json, '$.timestamp') AS timestamp,
            {AGENT_ID_EXPR} AS agent_id,
            {IS_SIDECHAIN_EXPR} AS is_sidechain,
            {AGENT_TYPE_EXPR} AS agent_type,
            {WORKFLOW_ID_EXPR} AS workflow_id
        FROM raw_records,
        LATERAL (
            SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item
        )
        WHERE json_extract_string(json, '$.type') = 'assistant'
        AND json_type(json_extract(json, '$.message.content')) = 'ARRAY'
        AND json_extract_string(item, '$.type') = 'tool_use'")
}
```

- [ ] **Step 3: Add `claude_tool_results_sql`**

```rust
/// The Claude `tool_results` view body.
pub fn claude_tool_results_sql() -> String {
    format!("SELECT
            json_extract_string(json, '$.sessionId') AS session_id,
            {PROJECT_EXPR} AS project,
            {SOURCE_EXPR} AS source,
            'claude' AS harness,
            json_extract_string(item, '$.tool_use_id') AS tool_use_id,
            COALESCE(CAST(json_extract(item, '$.is_error') AS BOOLEAN), false) AS is_error,
            json_extract_string(item, '$.content') AS content,
            {AGENT_ID_EXPR} AS agent_id,
            {IS_SIDECHAIN_EXPR} AS is_sidechain,
            {AGENT_TYPE_EXPR} AS agent_type,
            {WORKFLOW_ID_EXPR} AS workflow_id
        FROM raw_records,
        LATERAL (
            SELECT UNNEST(CAST(json_extract(json, '$.message.content') AS JSON[])) AS item
        )
        WHERE json_extract_string(json, '$.type') = 'user'
        AND json_type(json_extract(json, '$.message.content')) = 'ARRAY'
        AND json_extract_string(item, '$.type') = 'tool_result'")
}
```

- [ ] **Step 4: Add `claude_sessions_sql`**

This is the `register_sessions_view` body with two changes: a `'claude' AS harness` output column, and `WHERE harness = 'claude'` filters on both the outer `messages` scan and the `first_user_message` subquery (no-op today, required for PR 2 correctness):

```rust
/// The Claude `sessions` view body. Aggregates over the (possibly multi-provider)
/// `messages` view, filtered to Claude rows so it never scoops up another harness's
/// messages once `messages` becomes a UNION.
pub fn claude_sessions_sql() -> String {
    "SELECT
            session_id,
            COALESCE(
                MAX(project) FILTER (WHERE NOT is_sidechain),
                MAX(project)
            ) AS project,
            COALESCE(
                MAX(source) FILTER (WHERE NOT is_sidechain),
                MAX(source)
            ) AS source,
            'claude' AS harness,
            MIN(timestamp) FILTER (WHERE NOT is_sidechain) AS started_at,
            MAX(timestamp) FILTER (WHERE NOT is_sidechain) AS ended_at,
            COUNT(*) FILTER (WHERE NOT is_sidechain) AS message_count,
            CAST(COALESCE(SUM(tool_count) FILTER (WHERE NOT is_sidechain), 0) AS BIGINT) AS tool_call_count,
            COUNT(*) FILTER (WHERE type = 'user' AND NOT is_sidechain) AS user_message_count,
            COUNT(DISTINCT agent_id) AS subagent_count,
            (SELECT text FROM messages m2
             WHERE m2.session_id = m1.session_id
             AND m2.harness = 'claude'
             AND m2.type = 'user'
             AND NOT m2.is_sidechain
             AND m2.text IS NOT NULL
             AND m2.text != ''
             AND m2.text NOT LIKE '<%'
             AND m2.text NOT LIKE 'Base directory for this skill%'
             AND m2.text NOT LIKE '#%'
             ORDER BY m2.timestamp LIMIT 1) AS first_user_message
        FROM messages m1
        WHERE harness = 'claude'
        GROUP BY session_id".to_string()
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo build`
Expected: PASS. Warnings about the new functions being unused are expected — Task 3 wires them in.

- [ ] **Step 6: Commit**

```bash
git add src/views.rs
git commit -m "refactor: extract Claude view bodies into functions, add harness column"
```

---

## Task 3: Add the composer and rewire `register_views` through it

**Files:**
- Modify: `src/views.rs`
- Modify: `src/claude_provider.rs`

- [ ] **Step 1: Implement `ClaudeProvider::contribute_view_sql` and `prepare`**

In `src/claude_provider.rs`, inside `impl TranscriptProvider for ClaudeProvider`, add (keep the existing `name`/`discover_files`/`register_views`/`list_projects`):

```rust
    fn prepare(&self, _conn: &Connection) -> Result<bool> {
        // Claude's JSONL->raw_records sync happens in db::setup_connection /
        // indexer; nothing extra to prepare here. Always active.
        Ok(true)
    }

    fn contribute_view_sql(&self, view: crate::provider::View) -> Option<String> {
        use crate::provider::View;
        Some(match view {
            View::Messages => crate::views::claude_messages_sql(),
            View::ToolCalls => crate::views::claude_tool_calls_sql(),
            View::ToolResults => crate::views::claude_tool_results_sql(),
            View::Sessions => crate::views::claude_sessions_sql(),
        })
    }
```

Ensure `use crate::provider::View;` or the fully-qualified path resolves (the snippet uses the full path, so no new top-level import is strictly required).

- [ ] **Step 2: Add `empty_view_sql` and `compose_views` to `views.rs`**

Add to `src/views.rs`. `empty_view_sql` returns the empty body for one view (the existing `register_empty_views` bodies, each with `NULL::VARCHAR AS harness` added after `source`):

```rust
use crate::provider::{TranscriptProvider, View};

/// The empty-view body (correct schema, zero rows) for one view. Used when no
/// active provider contributes to that view.
fn empty_view_sql(view: View) -> &'static str {
    match view {
        View::Messages => "SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS source,
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS uuid,
            NULL::VARCHAR AS parent_uuid,
            NULL::VARCHAR AS type,
            NULL::VARCHAR AS timestamp,
            NULL::VARCHAR AS text,
            CAST(0 AS BIGINT) AS tool_count,
            NULL::VARCHAR AS model,
            NULL::VARCHAR AS agent_id,
            false AS is_sidechain,
            NULL::VARCHAR AS agent_type,
            NULL::VARCHAR AS workflow_id
        WHERE 1=0",
        View::ToolCalls => "SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS source,
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS message_uuid,
            NULL::VARCHAR AS tool_use_id,
            NULL::VARCHAR AS name,
            NULL::JSON AS input,
            NULL::VARCHAR AS timestamp,
            NULL::VARCHAR AS agent_id,
            false AS is_sidechain,
            NULL::VARCHAR AS agent_type,
            NULL::VARCHAR AS workflow_id
        WHERE 1=0",
        View::ToolResults => "SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS source,
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS tool_use_id,
            false AS is_error,
            NULL::VARCHAR AS content,
            NULL::VARCHAR AS agent_id,
            false AS is_sidechain,
            NULL::VARCHAR AS agent_type,
            NULL::VARCHAR AS workflow_id
        WHERE 1=0",
        View::Sessions => "SELECT
            NULL::VARCHAR AS session_id,
            NULL::VARCHAR AS project,
            NULL::VARCHAR AS source,
            NULL::VARCHAR AS harness,
            NULL::VARCHAR AS started_at,
            NULL::VARCHAR AS ended_at,
            CAST(0 AS BIGINT) AS message_count,
            CAST(0 AS BIGINT) AS tool_call_count,
            CAST(0 AS BIGINT) AS user_message_count,
            CAST(0 AS BIGINT) AS subagent_count,
            NULL::VARCHAR AS first_user_message
        WHERE 1=0",
    }
}

/// Compose the four views from the active providers' contributions. Each
/// contribution is wrapped in parens and `UNION ALL`ed. A view with no
/// contributors falls back to the empty-view schema. Views are created in
/// `View::ALL` order so `sessions` (which reads `messages`) comes last.
pub fn compose_views(conn: &Connection, providers: &[&dyn TranscriptProvider]) -> Result<()> {
    for view in View::ALL {
        let parts: Vec<String> = providers
            .iter()
            .filter_map(|p| p.contribute_view_sql(view))
            .map(|body| format!("({body})"))
            .collect();
        let body = if parts.is_empty() {
            empty_view_sql(view).to_string()
        } else {
            parts.join("\nUNION ALL\n")
        };
        let sql = format!("CREATE OR REPLACE VIEW {} AS {body}", view.name());
        conn.execute_batch(&sql)
            .with_context(|| format!("Failed to create {} view", view.name()))?;
    }
    Ok(())
}
```

- [ ] **Step 3: Rewire `register_views` and `register_derived_views`, delete the old per-view + empty functions**

Replace the bodies of `register_views` and `register_derived_views`, and delete `register_messages_view`, `register_tool_calls_view`, `register_tool_results_view`, `register_sessions_view`, and `register_empty_views` (their SQL now lives in the `claude_*_sql` / `empty_view_sql` functions):

```rust
/// Register all queryable views against the given JSONL transcript files.
/// When `files` is empty, creates empty views with the correct schema.
pub fn register_views(conn: &Connection, files: &[PathBuf]) -> Result<()> {
    if files.is_empty() {
        // No raw_records source; compose with no active providers -> empty views.
        return compose_views(conn, &[]);
    }
    let file_list = build_file_list(files);
    register_raw_view(conn, &file_list)?;
    register_derived_views(conn)
}

/// Register the derived views over an existing `raw_records`. Composes from the
/// Claude provider (the only provider that reads `raw_records`).
pub fn register_derived_views(conn: &Connection) -> Result<()> {
    let claude = crate::claude_provider::ClaudeProvider::new_with_base(std::path::PathBuf::new());
    compose_views(conn, &[&claude])
}
```

> Note: `contribute_view_sql` ignores the provider's source list (the bodies read `raw_records`), so the throwaway `new_with_base(PathBuf::new())` is safe and cheap here.

- [ ] **Step 4: Run the full view test suite**

Run: `cargo test --test views_test`
Expected: PASS for all existing tests. The `harness` column is additive; tests selecting specific columns or `COUNT(*)` are unaffected. If any test asserts an exact column set (e.g. via `SELECT *` shape or a hardcoded column count), fix it in Task 5's test step — note the failure name and continue.

- [ ] **Step 5: Commit**

```bash
git add src/views.rs src/claude_provider.rs
git commit -m "refactor: compose views from provider contributions via UNION ALL"
```

---

## Task 4: Thread the provider collection through `db` and `main`

**Files:**
- Modify: `src/db.rs:40-61`
- Modify: `src/main.rs:209` and `src/main.rs:295`

- [ ] **Step 1: Update `setup_connection` to accept providers and compose from the active set**

In `src/db.rs`, change `setup_connection` to take the provider list, run `prepare` on each, and compose from the active providers. Keep the indexer sync exactly as-is (Claude machinery):

```rust
use crate::provider::TranscriptProvider;

pub fn setup_connection(
    providers: &[Box<dyn TranscriptProvider>],
    sources: &[(String, std::path::PathBuf)],
    options: &DbOptions,
    scope: SyncScope,
) -> Result<DbSetup> {
    let cache_dir = cache::cache_dir()?;
    let force_rebuild = options.sync_mode == SyncMode::Force;
    let conn = cache::open(&cache_dir, force_rebuild)?;

    let result = indexer::sync_sources(&conn, sources, options.sync_mode, scope, &cache_dir)?;
    let file_count = result.stats.added + result.stats.changed;

    // Ask each provider to prepare and report whether it is active, then compose.
    let mut active: Vec<&dyn TranscriptProvider> = Vec::new();
    for p in providers {
        if p.prepare(&conn)? {
            active.push(p.as_ref());
        }
    }
    views::compose_views(&conn, &active)?;

    Ok(DbSetup {
        conn,
        file_count,
        total_files: result.stats.total,
        skipped: result.skipped,
        lock_busy: result.lock_busy,
    })
}
```

- [ ] **Step 2: Build the provider list in `main.rs` and pass it**

In `src/main.rs`, the provider is already constructed at line 209 (`let provider = ClaudeProvider::new()?;`) and used for scope logic. Keep that. Add a boxed provider collection just before the `setup_connection` call (line 295) and pass it. Change:

```rust
    let db_setup = db::setup_connection(&sources, &options, sync_scope)?;
```

to:

```rust
    let providers: Vec<Box<dyn cq::provider::TranscriptProvider>> =
        vec![Box::new(cq::claude_provider::ClaudeProvider::new()?)];
    let db_setup = db::setup_connection(&providers, &sources, &options, sync_scope)?;
```

> The `sources` vec (line 275) is still derived from the scope provider for the indexer; leave it. This PR keeps two references to Claude (the scope `provider` and the boxed `providers[0]`); consolidating them is out of scope and a candidate for a later cleanup.

- [ ] **Step 3: Build and run the full suite**

Run: `cargo build && cargo test`
Expected: PASS (build clean, all tests green).

- [ ] **Step 4: Smoke-test against real data (no behavior change)**

Run: `cargo run -- sessions --limit 3` and `cargo run -- sql "SELECT DISTINCT harness FROM sessions"`
Expected: the first prints sessions as before; the second prints a single row `claude`.

- [ ] **Step 5: Commit**

```bash
git add src/db.rs src/main.rs
git commit -m "refactor: thread provider collection through setup_connection"
```

---

## Task 5: Add `harness` regression tests and fix any broken column-set assertions

**Files:**
- Modify: `tests/views_test.rs`

- [ ] **Step 1: Add a test that every view exposes `harness = 'claude'`**

Append to `tests/views_test.rs` (uses the existing `setup_db` helper and the `simple_session.jsonl` fixture already used by the first test):

```rust
#[test]
fn views_expose_claude_harness() {
    let conn = setup_db("simple_session.jsonl");
    for view in ["messages", "tool_calls", "tool_results", "sessions"] {
        // Every non-empty view's rows are tagged harness='claude'.
        let distinct: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM (SELECT DISTINCT harness FROM {view} WHERE harness IS NOT NULL)"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(distinct <= 1, "{view} should have at most one harness value");
        let claude: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {view} WHERE harness = 'claude'"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        let total: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {view}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(claude, total, "all {view} rows should be harness='claude'");
    }
}

#[test]
fn empty_views_have_harness_column() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE file_registry (
            file_path TEXT PRIMARY KEY, mtime_ns BIGINT, file_size BIGINT,
            cwd TEXT, agent_type TEXT, source TEXT,
            indexed_at TIMESTAMP DEFAULT current_timestamp
        )",
    )
    .unwrap();
    cq::views::register_views(&conn, &[]).unwrap();
    // Selecting harness from each empty view must not error (column exists).
    for view in ["messages", "tool_calls", "tool_results", "sessions"] {
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {view} WHERE harness IS NULL"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{view} should be empty");
    }
}
```

- [ ] **Step 2: Run the new tests**

Run: `cargo test --test views_test views_expose_claude_harness empty_views_have_harness_column`
Expected: PASS.

- [ ] **Step 3: Run the entire view suite and fix any column-set regressions**

Run: `cargo test --test views_test`
Expected: PASS. If a pre-existing test fails because it asserted an exact column count or `SELECT *` row shape, update that assertion to include the new `harness` column (placed after `source`). Do not change any non-harness expected values.

- [ ] **Step 4: Commit**

```bash
git add tests/views_test.rs
git commit -m "test: assert harness column across all four views"
```

---

## Task 6: Update docs for the new `harness` column

**Files:**
- Modify: `src/commands/schema.rs`
- Modify: `README.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add `harness` to the schema command's column docs**

Open `src/commands/schema.rs`. For each of the four views' documented column lists, add a `harness` entry immediately after `source`, described as: `harness — the tool that produced the transcript (claude, opencode)`. Match the file's existing formatting for column entries exactly (find the `source` line in each view's block and mirror its style).

- [ ] **Step 2: Verify the schema command renders**

Run: `cargo run -- schema` and `cargo run -- schema --examples`
Expected: each view lists a `harness` column after `source`; no formatting breakage.

- [ ] **Step 3: Update README and CLAUDE.md view tables**

In `README.md` and `CLAUDE.md`, find the view/column reference tables and add `harness` after `source` with the same description as Step 1. (In `CLAUDE.md`, the relevant spot is the views/architecture description; keep edits minimal and consistent with surrounding style.)

- [ ] **Step 4: Commit**

```bash
git add src/commands/schema.rs README.md CLAUDE.md
git commit -m "docs: document the harness column on all views"
```

---

## Final Verification

- [ ] **Step 1: Full clean build + test**

Run: `cargo build && cargo test`
Expected: clean build, **all** tests green (unit + `views_test` + `integration_test`).

- [ ] **Step 2: Confirm no behavior change for Claude**

Run: `cargo run -- sessions --limit 5`, `cargo run -- tools --limit 5`, `cargo run -- messages --limit 5`
Expected: identical output shape to `main` (plus the views now carry a `harness` column visible via `--fields harness` or `sql`).

- [ ] **Step 3: Confirm the seam is real**

Run: `cargo run -- sql "SELECT harness, COUNT(*) FROM messages GROUP BY harness"`
Expected: a single `claude` row. (PR 2 will add an `opencode` row through the same composer with zero changes to Task 1–6 code.)

---

## Self-Review

**Spec coverage** (against ADR 0001 + findings "Design resolutions"):
- Provider collection in `main.rs` instead of hardcoded `ClaudeProvider::new()` → Task 4. ✓
- Two-phase trait (`prepare` / `contribute_view_sql`) → Task 1. ✓
- `UNION ALL` view composer → Task 3 (`compose_views`). ✓
- New `harness` column on all four Claude views → Tasks 2, 3, 5, 6. ✓
- Claude-specific scope/`--source` plumbing stays off the trait → not moved (Task 4 note). ✓
- Empty-view fallback preserved → Task 3 (`empty_view_sql`). ✓
- Tests stay green / additive change → Tasks 3–5. ✓
- Docs-in-sync → Task 6. ✓
- Out of scope (correctly deferred to PR 2): `OpenCodeProvider`, `ATTACH`, `INSTALL sqlite`, cost/token columns, schema-drift SQL refresh.

**Placeholder scan:** No TBD/TODO/"handle edge cases"/"similar to Task N" — all SQL bodies and Rust are spelled out. ✓

**Type consistency:** `View` enum + `View::ALL`/`View::name()` (Task 1) used identically in `contribute_view_sql` (Tasks 1, 3), `compose_views` and `empty_view_sql` (Task 3). `compose_views(conn, &[&dyn TranscriptProvider])` signature matches its callers in `register_derived_views` (Task 3) and `setup_connection` (Task 4). `claude_*_sql()` names match between definition (Task 2) and use (Task 3). `setup_connection`'s new `providers` first-arg matches the `main.rs` call site (Task 4). ✓
