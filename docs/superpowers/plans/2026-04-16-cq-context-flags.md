# cq Context Flags (-A/-B/-C) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add grep-style `-A N` / `-B N` / `-C N` context flags to `cq tools` and `cq messages` so users can see the messages surrounding a match without writing SQL window functions.

**Architecture:** When a context flag is set, each command takes a "context path" instead of its normal query path. The context path builds two CTEs — an ordered in-session row stream and a matches-only set — then expands each match into a window bounded by the session, deduplicates overlapping windows, and assigns a `match_group` integer via an islands-style gap detection. Rows carry `match_kind` (match/before/after) and `match_group` as metadata columns. Table output drops these columns and uses color + `--` separators for visual distinction (grep-faithful); JSON output keeps them. For `cq tools --json`, match rows retain the tool shape while context rows are message-shaped (heterogeneous, jq-native). Context is bounded to the session (never crosses), `--limit N` limits matches (not output rows, grep-style), and `--count-by` + context is rejected as a hard error.

**Tech Stack:** Rust, clap 4 (derive), DuckDB (window functions, CTEs, `ANY_VALUE`), assert_cmd for integration tests.

---

## Design Decisions (locked)

| Question | Decision | Why |
|---|---|---|
| Which commands? | `cq tools`, `cq messages` only | `sessions` is the container, not a line — context makes no sense on it |
| Unit of context | Messages always | Grep's atom is the line; cq's message is the equivalent. Applies even on `cq tools` matches |
| Output shape (table) | Normalized to messages; `--` separators + dimmed context rows | Tables need schema consistency; grep-faithful visuals |
| Output shape (`--json`) | Heterogeneous: match rows keep native shape, context rows are messages | JSON handles heterogeneity natively; preserves tool-specific fields on matches |
| `--limit N` semantics | Limits matches, not output rows | Grep's `-m` convention |
| Group separators | Non-contiguous match windows get `--` in table; `match_group` integer in JSON | Grep parity |
| `--count-by` + context | Error | Aggregation + per-row context don't mix |
| Cross-session context | Never | Session = grep's file boundary |
| `-C N` vs `-A M -B K` | `-C` is shorthand for `-A N -B N`; explicit `-A`/`-B` override | Grep convention |

---

## File Structure

| File | Responsibility | Tasks |
|------|---------------|-------|
| `src/main.rs` | Clap flag definitions on `Tools` and `Messages` subcommands, dispatch | 1 |
| `src/commands/mod.rs` | Shared `ContextWindow` struct, validation helpers, conflict check | 2 |
| `src/commands/context.rs` *(new)* | Core context-window SQL builder for the `messages` view | 3 |
| `src/commands/messages.rs` | New `run_with_context` path using `context::build_messages_context_sql` | 4 |
| `src/commands/tools.rs` | New `run_with_context` path: match on tool_calls, expand to messages, emit heterogeneous rows in JSON | 5 |
| `src/output.rs` | Context-aware table renderer (drops `match_kind`/`match_group`, adds `--` separators, dims context rows) | 6 |
| `src/style.rs` | `dim_context_row` helper | 6 |
| `tests/integration_test.rs` | End-to-end CLI tests | 1, 4, 5, 6, 7 |
| `tests/fixtures/context_session.jsonl` *(new)* | Deterministic fixture with 8+ messages in one session, known tool call positions | 4 |
| `docs/cli-ux-conventions.md` | Brief addition documenting context flag behavior | 7 |
| `docs/use-cases.md` | Add "Trace what happened around a tool call" use case | 7 |

---

## Task 1: Add clap flags and dispatch

**Files:**
- Modify: `src/main.rs:87-124` (Tools + Messages subcommand definitions and dispatch arms at 255-261)
- Test: `tests/integration_test.rs`

- [ ] **Step 1: Write failing help-text tests**

Add to `tests/integration_test.rs`:

```rust
#[test]
fn tools_help_shows_context_flags() {
    Command::cargo_bin("cq").unwrap()
        .args(["tools", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("-A"))
        .stdout(predicate::str::contains("-B"))
        .stdout(predicate::str::contains("-C"))
        .stdout(predicate::str::contains("messages after each match"))
        .stdout(predicate::str::contains("messages before each match"));
}

#[test]
fn messages_help_shows_context_flags() {
    Command::cargo_bin("cq").unwrap()
        .args(["messages", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("-A"))
        .stdout(predicate::str::contains("-B"))
        .stdout(predicate::str::contains("-C"));
}

#[test]
fn tools_context_conflicts_with_count_by() {
    let env = setup_env(&["simple_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["tools", "-C", "2", "--count-by", "name"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--count-by") && stderr.contains("context"),
        "Should explain conflict between --count-by and context flags, got: {stderr}"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test integration_test tools_help_shows_context_flags -- --nocapture`
Expected: FAIL — flags don't exist yet.

- [ ] **Step 3: Add flags to the Tools subcommand**

Modify `src/main.rs` Tools variant (around lines 87-106). Add three fields:

```rust
/// Show N messages after each match (grep -A)
#[arg(short = 'A', long = "after-context", value_name = "N")]
after: Option<usize>,

/// Show N messages before each match (grep -B)
#[arg(short = 'B', long = "before-context", value_name = "N")]
before: Option<usize>,

/// Show N messages before and after each match (grep -C, shorthand for -A N -B N)
#[arg(short = 'C', long = "context", value_name = "N", conflicts_with_all = ["after", "before"])]
context: Option<usize>,
```

Add the same three fields to the `Messages` variant (around lines 107-124).

- [ ] **Step 4: Update dispatch to pass context tuple**

In `src/main.rs` dispatch (around 255-261), destructure the new fields and build a `ContextWindow`:

```rust
Command::Tools { name, grep, errors, fields, count_by, after, before, context } => {
    let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
    let ctx = cq::commands::ContextWindow::from_flags(after, before, context);
    tools::run(&conn, &scope, name.as_deref(), grep.as_deref(), errors, field_refs.as_deref(), count_by.as_deref(), ctx, &format, cli.limit, cli.offset, wide)?;
}
Command::Messages { msg_type, grep, fields, count_by, after, before, context } => {
    let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
    let ctx = cq::commands::ContextWindow::from_flags(after, before, context);
    messages::run(&conn, &scope, msg_type.as_deref(), grep.as_deref(), field_refs.as_deref(), count_by.as_deref(), ctx, &format, cli.limit, cli.offset, wide)?;
}
```

(Signatures of `tools::run` and `messages::run` will be extended in Tasks 4 and 5 to accept `ctx`. For this step, just pass it; those tasks add the handling.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test integration_test tools_help_shows_context_flags messages_help_shows_context_flags -- --nocapture`
Expected: PASS (help output now mentions the flags).

The `tools_context_conflicts_with_count_by` test still fails — that's wired in Task 2.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs tests/integration_test.rs
git commit -m "feat(cli): add -A/-B/-C flags to tools and messages commands"
```

---

## Task 2: ContextWindow struct and conflict validation

**Files:**
- Modify: `src/commands/mod.rs`
- Test: `tests/integration_test.rs` (test from Task 1 Step 1)

- [ ] **Step 1: Write unit test for ContextWindow::from_flags**

Add to `src/commands/mod.rs` at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_window_none_when_no_flags() {
        let ctx = ContextWindow::from_flags(None, None, None);
        assert!(ctx.is_none());
    }

    #[test]
    fn context_window_c_sets_both() {
        let ctx = ContextWindow::from_flags(None, None, Some(3)).unwrap();
        assert_eq!(ctx.before, 3);
        assert_eq!(ctx.after, 3);
    }

    #[test]
    fn context_window_explicit_a_b() {
        let ctx = ContextWindow::from_flags(Some(5), Some(2), None).unwrap();
        assert_eq!(ctx.before, 2);
        assert_eq!(ctx.after, 5);
    }

    #[test]
    fn context_window_a_only_b_defaults_to_zero() {
        let ctx = ContextWindow::from_flags(Some(4), None, None).unwrap();
        assert_eq!(ctx.before, 0);
        assert_eq!(ctx.after, 4);
    }

    #[test]
    fn context_window_b_only_a_defaults_to_zero() {
        let ctx = ContextWindow::from_flags(None, Some(4), None).unwrap();
        assert_eq!(ctx.before, 4);
        assert_eq!(ctx.after, 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test context_window -- --nocapture`
Expected: FAIL — `ContextWindow` type does not exist.

- [ ] **Step 3: Define ContextWindow and conflict checker**

Add to `src/commands/mod.rs`:

```rust
/// Describes a grep-style context window around matches.
/// `before` and `after` are message counts in the same session.
#[derive(Clone, Copy, Debug)]
pub struct ContextWindow {
    pub before: usize,
    pub after: usize,
}

impl ContextWindow {
    /// Resolve clap's --after/--before/--context trio into an Option<ContextWindow>.
    /// Returns None when no context flag is set.
    /// `--context` (if set) wins over `--after` and `--before` (clap's conflicts_with_all
    /// should already prevent mixing, but we defend anyway).
    pub fn from_flags(after: Option<usize>, before: Option<usize>, context: Option<usize>) -> Option<Self> {
        if let Some(c) = context {
            return Some(ContextWindow { before: c, after: c });
        }
        if after.is_none() && before.is_none() {
            return None;
        }
        Some(ContextWindow {
            before: before.unwrap_or(0),
            after: after.unwrap_or(0),
        })
    }
}

/// Error out when --count-by is combined with context flags.
/// Aggregation produces summary rows; context surrounds individual rows. Incompatible.
pub fn check_count_by_context_conflict(count_by: Option<&str>, ctx: Option<ContextWindow>) {
    if count_by.is_some() && ctx.is_some() {
        eprintln!(
            "Error: --count-by cannot be used with -A, -B, or -C\n\
             --count-by aggregates rows into counts; context flags surround individual matches with nearby messages"
        );
        std::process::exit(1);
    }
}
```

- [ ] **Step 4: Run unit tests to verify they pass**

Run: `cargo test context_window -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Wire the conflict check into tools and messages**

In `src/commands/tools.rs::run`, immediately after `super::check_count_by_fields_conflict(count_by, fields);`, add:

```rust
super::check_count_by_context_conflict(count_by, ctx);
```

Same in `src/commands/messages.rs::run`. The `ctx: Option<ContextWindow>` parameter will be added to both signatures in Tasks 4 and 5; for this step just add the parameter and call the conflict check so the tests from Task 1 can compile.

- [ ] **Step 6: Run integration test for the conflict**

Run: `cargo test --test integration_test tools_context_conflicts_with_count_by -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/commands/mod.rs src/commands/tools.rs src/commands/messages.rs
git commit -m "feat(cli): add ContextWindow and count-by conflict check"
```

---

## Task 3: SQL builder for messages context windows

**Files:**
- Create: `src/commands/context.rs`
- Modify: `src/commands/mod.rs` (add `pub mod context;`)
- Test: unit tests in `src/commands/context.rs`

**Approach note:** The SQL uses four CTEs:

1. `ordered` — all messages in session with a per-session ordinal (`ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY timestamp, uuid)`).
2. `matches` — joins the caller's matches subquery (which returns `session_id`, `message_uuid`) against `ordered` on `(session_id, uuid = message_uuid)` to recover the ordinal, with a global `match_idx`. Applies the match `LIMIT` here.
3. `expanded` — for each match, join back to `ordered` on `session_id` AND `ord BETWEEN match_ord - before AND match_ord + after`. Tags each row's `match_kind`.
4. `deduped` + `grouped` — dedupe rows that appear in multiple overlapping windows (prefer `match` kind, lowest `match_idx`). Then assign `match_group` via classic islands-and-gaps: `SUM(CASE WHEN ord = LAG(ord) + 1 THEN 0 ELSE 1 END) OVER (PARTITION BY session_id ORDER BY ord)`.

**Param-positioning contract:** `ordered_scope_where`'s params (if any) go first; then `matches_subquery`'s params. Callers must assemble their param vector in this order. This is because `ordered` is emitted before `matches` in the generated SQL.

- [ ] **Step 1: Write unit tests for the SQL builder**

Add to a new file `src/commands/context.rs`:

```rust
use crate::commands::ContextWindow;

/// Build a SQL query that returns messages around matches, with `match_kind` and `match_group` columns.
///
/// `match_from_clause`: the FROM + WHERE that yields rows with a `session_id` and `message_uuid`
///   identifying anchor messages. For `cq messages`, this is just `messages` with the user's filters.
///   For `cq tools`, this is `tool_calls` joined with message-level filters, projecting `session_id` and
///   `message_uuid` (which identifies the parent assistant message in the `messages` view via `uuid`).
/// `match_where`: already-composed WHERE conditions for matches.
/// `match_limit`: limits the number of matches (grep's -m semantics); 0 means unlimited.
pub struct ContextSqlBuilder<'a> {
    pub window: ContextWindow,
    /// SQL fragment selecting matches. Must return columns `session_id` and `message_uuid`.
    pub matches_subquery: &'a str,
    /// Fully-qualified session scope conditions for the `ordered` CTE (no tool/message-specific filters).
    pub ordered_scope_where: &'a str,
    pub match_limit: usize,
}

impl<'a> ContextSqlBuilder<'a> {
    pub fn build(&self) -> String {
        let before = self.window.before;
        let after = self.window.after;
        let limit_clause = if self.match_limit == 0 {
            String::new()
        } else {
            format!("LIMIT {}", self.match_limit)
        };
        format!(
            r#"
WITH ordered AS (
    SELECT session_id, uuid, type, timestamp, text, model, tool_count, project,
           ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY timestamp, uuid) AS ord
    FROM messages
    WHERE {ordered_scope_where}
),
matches AS (
    SELECT m.session_id, m.message_uuid, o.ord AS match_ord,
           ROW_NUMBER() OVER (ORDER BY m.session_id, o.ord) AS match_idx
    FROM ({matches_subquery}) m
    JOIN ordered o ON m.session_id = o.session_id AND m.message_uuid = o.uuid
    ORDER BY m.session_id, o.ord
    {limit_clause}
),
expanded AS (
    SELECT o.session_id, o.uuid, o.type, o.timestamp, o.text, o.model, o.tool_count, o.project, o.ord,
           m.match_ord, m.match_idx,
           CASE
               WHEN o.ord = m.match_ord THEN 'match'
               WHEN o.ord < m.match_ord THEN 'before'
               ELSE 'after'
           END AS match_kind
    FROM ordered o
    JOIN matches m
      ON o.session_id = m.session_id
     AND o.ord BETWEEN m.match_ord - {before} AND m.match_ord + {after}
),
deduped AS (
    SELECT session_id, ord,
           ANY_VALUE(uuid) AS uuid,
           ANY_VALUE(type) AS type,
           ANY_VALUE(timestamp) AS timestamp,
           ANY_VALUE(text) AS text,
           ANY_VALUE(model) AS model,
           ANY_VALUE(tool_count) AS tool_count,
           ANY_VALUE(project) AS project,
           MIN(match_idx) AS match_idx,
           MAX(CASE WHEN match_kind = 'match' THEN 1 ELSE 0 END) AS is_match_any,
           ANY_VALUE(match_kind) AS any_kind
    FROM expanded
    GROUP BY session_id, ord
),
grouped AS (
    SELECT *,
           SUM(CASE WHEN ord = LAG(ord) OVER (PARTITION BY session_id ORDER BY ord) + 1 THEN 0 ELSE 1 END)
             OVER (PARTITION BY session_id ORDER BY ord) AS match_group
    FROM deduped
)
SELECT session_id, uuid, type, timestamp, text, model, tool_count, project,
       CASE WHEN is_match_any = 1 THEN 'match' ELSE any_kind END AS match_kind,
       match_group
FROM grouped
ORDER BY session_id, ord
"#,
            ordered_scope_where = self.ordered_scope_where,
            matches_subquery = self.matches_subquery,
            before = before,
            after = after,
            limit_clause = limit_clause,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_includes_before_and_after_bounds() {
        let b = ContextSqlBuilder {
            window: ContextWindow { before: 2, after: 3 },
            matches_subquery: "SELECT session_id, uuid AS message_uuid FROM messages WHERE type = 'user'",
            ordered_scope_where: "1=1",
            match_limit: 0,
        };
        let sql = b.build();
        assert!(sql.contains("match_ord - 2"));
        assert!(sql.contains("match_ord + 3"));
        assert!(sql.contains("SUM(CASE WHEN ord = LAG(ord)"));
        assert!(!sql.contains("LIMIT"));
    }

    #[test]
    fn builder_includes_match_limit_when_nonzero() {
        let b = ContextSqlBuilder {
            window: ContextWindow { before: 1, after: 1 },
            matches_subquery: "SELECT session_id, uuid AS message_uuid FROM messages",
            ordered_scope_where: "1=1",
            match_limit: 5,
        };
        let sql = b.build();
        assert!(sql.contains("LIMIT 5"));
    }
}
```

- [ ] **Step 2: Register the module**

Add to `src/commands/mod.rs`:

```rust
pub mod context;
```

And re-export the helper:

```rust
pub use context::ContextSqlBuilder;
```

- [ ] **Step 3: Run unit tests**

Run: `cargo test --lib context:: -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/commands/context.rs src/commands/mod.rs
git commit -m "feat(context): add ContextSqlBuilder for message-level windows"
```

---

## Task 4: Wire context into `cq messages`

**Files:**
- Modify: `src/commands/messages.rs` (add `run_with_context` branch, update `run` signature)
- Create: `tests/fixtures/context_session.jsonl`
- Modify: `tests/integration_test.rs`

- [ ] **Step 1: Create the deterministic fixture**

Create `tests/fixtures/context_session.jsonl` with 9 messages in one session, all with known timestamps and content. Include at least one `Read` tool call at message 4 so we can anchor on it later:

```jsonl
{"type":"user","message":{"role":"user","content":"one"},"uuid":"ctx-u1","parentUuid":null,"isSidechain":false,"timestamp":"2026-04-14T10:00:01.000Z","sessionId":"cccc0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"assistant","message":{"id":"msg_c1","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"two"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5}},"uuid":"ctx-a1","parentUuid":"ctx-u1","isSidechain":false,"timestamp":"2026-04-14T10:00:02.000Z","sessionId":"cccc0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"user","message":{"role":"user","content":"three"},"uuid":"ctx-u2","parentUuid":"ctx-a1","isSidechain":false,"timestamp":"2026-04-14T10:00:03.000Z","sessionId":"cccc0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"assistant","message":{"id":"msg_c2","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"tool_use","id":"toolu_c100","name":"Read","input":{"file_path":"/anchor.txt"}}],"stop_reason":"tool_use","usage":{"input_tokens":20,"output_tokens":10}},"uuid":"ctx-a2","parentUuid":"ctx-u2","isSidechain":false,"timestamp":"2026-04-14T10:00:04.000Z","sessionId":"cccc0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_c100","content":"five"}]},"uuid":"ctx-u3","parentUuid":"ctx-a2","isSidechain":false,"timestamp":"2026-04-14T10:00:05.000Z","sessionId":"cccc0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"assistant","message":{"id":"msg_c3","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"six NEEDLE"}],"stop_reason":"end_turn","usage":{"input_tokens":30,"output_tokens":15}},"uuid":"ctx-a3","parentUuid":"ctx-u3","isSidechain":false,"timestamp":"2026-04-14T10:00:06.000Z","sessionId":"cccc0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"user","message":{"role":"user","content":"seven"},"uuid":"ctx-u4","parentUuid":"ctx-a3","isSidechain":false,"timestamp":"2026-04-14T10:00:07.000Z","sessionId":"cccc0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"assistant","message":{"id":"msg_c4","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"eight"}],"stop_reason":"end_turn","usage":{"input_tokens":40,"output_tokens":20}},"uuid":"ctx-a4","parentUuid":"ctx-u4","isSidechain":false,"timestamp":"2026-04-14T10:00:08.000Z","sessionId":"cccc0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
{"type":"user","message":{"role":"user","content":"nine"},"uuid":"ctx-u5","parentUuid":"ctx-a4","isSidechain":false,"timestamp":"2026-04-14T10:00:09.000Z","sessionId":"cccc0000-0000-0000-0000-000000000001","cwd":"/Users/test/myproject","version":"2.1.104","gitBranch":"main"}
```

Ordinals within this session: 1=one, 2=two, 3=three, 4=Read tool_use (text null), 5=tool_result "five", 6="six NEEDLE", 7=seven, 8=eight, 9=nine.

- [ ] **Step 2: Write failing integration tests for `cq messages` context**

Add to `tests/integration_test.rs`:

```rust
#[test]
fn messages_grep_with_context_a_shows_following_messages() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "messages", "--grep", "NEEDLE", "-A", "2"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let rows = parsed.as_array().unwrap();
    assert_eq!(rows.len(), 3, "expected match + 2 after, got {}: {}", rows.len(), stdout);
    assert_eq!(rows[0]["match_kind"], "match");
    assert_eq!(rows[1]["match_kind"], "after");
    assert_eq!(rows[2]["match_kind"], "after");
    assert!(rows[0]["text"].as_str().unwrap().contains("NEEDLE"));
}

#[test]
fn messages_grep_with_context_c_shows_surrounding() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "messages", "--grep", "NEEDLE", "-C", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["match_kind"], "before");
    assert_eq!(rows[1]["match_kind"], "match");
    assert_eq!(rows[2]["match_kind"], "after");
}

#[test]
fn messages_context_does_not_cross_session_boundary() {
    // Two sessions; match is in the second. -B 5 shouldn't pull anything from the first.
    let env = setup_env(&["simple_session.jsonl", "context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "messages", "--grep", "NEEDLE", "-B", "10"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    // All returned rows must be from the same session as the match.
    let match_session = rows.iter()
        .find(|r| r["match_kind"] == "match")
        .and_then(|r| r["session_id"].as_str())
        .unwrap()
        .to_string();
    for row in &rows {
        assert_eq!(row["session_id"].as_str().unwrap(), match_session, "cross-session leak: {row}");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test integration_test messages_grep_with_context -- --nocapture`
Expected: FAIL — context path not implemented.

- [ ] **Step 4: Update `messages::run` signature and add context branch**

In `src/commands/messages.rs`, change the `pub fn run` signature to accept `ctx: Option<super::ContextWindow>` (place it after `count_by`):

```rust
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    msg_type: Option<&str>,
    grep: Option<&str>,
    fields: Option<&[&str]>,
    count_by: Option<&str>,
    ctx: Option<super::ContextWindow>,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    wide: bool,
) -> Result<()> {
    super::check_count_by_fields_conflict(count_by, fields);
    super::check_count_by_context_conflict(count_by, ctx);

    if let Some(col) = count_by {
        let resolved = super::validate_count_by(col, VALID_COUNT_BY_COLUMNS, "messages");
        return run_count_by(conn, scope, msg_type, grep, &resolved, format, wide);
    }

    if let Some(window) = ctx {
        return run_with_context(conn, scope, msg_type, grep, window, format, limit, wide);
    }

    // ... existing code unchanged
```

Add the new function:

```rust
fn run_with_context(
    conn: &Connection,
    scope: &QueryScope,
    msg_type: Option<&str>,
    grep: Option<&str>,
    window: super::ContextWindow,
    format: &OutputFormat,
    match_limit: usize,
    wide: bool,
) -> Result<()> {
    // Build scope conditions (used in both `ordered` and `matches` CTEs).
    let mut scope_conditions = vec!["1=1".to_string()];
    let mut params: Vec<Box<dyn duckdb::types::ToSql>> = Vec::new();

    if let Some(project) = &scope.project {
        scope_conditions.push("project ILIKE ?".to_string());
        params.push(Box::new(format!("%{project}%")));
    }
    if let Some(session) = &scope.session {
        scope_conditions.push("session_id = ?".to_string());
        params.push(Box::new(session.clone()));
    }
    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        scope_conditions.push(format!("timestamp >= '{formatted}'"));
    }
    let scope_where = scope_conditions.join(" AND ");

    // Additional match-level conditions (type, grep) applied on top of the scoped ordered CTE.
    let mut match_conditions = vec!["1=1".to_string()];
    if let Some(t) = msg_type {
        match_conditions.push("type = ?".to_string());
        params.push(Box::new(t.to_string()));
    }
    if let Some(pattern) = grep {
        match_conditions.push("text ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }
    let match_where = match_conditions.join(" AND ");

    // Matches subquery: pull session_id + uuid (as message_uuid) from the scoped `messages`
    // filtered by match criteria.
    let matches_subquery = format!(
        "SELECT session_id, uuid AS message_uuid FROM messages WHERE {scope_where} AND {match_where}"
    );

    let builder = super::ContextSqlBuilder {
        window,
        matches_subquery: &matches_subquery,
        ordered_scope_where: &scope_where,
        match_limit,
    };
    let sql = builder.build();

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    // For JSON and Table modes, use generic `output::print_results` so every column in the
    // SELECT list becomes a column/field in the output (including match_kind, match_group).
    // For Default (TTY) mode, use the context-aware renderer added in Task 6.
    match format {
        OutputFormat::Json | OutputFormat::Table => {
            output::print_results(&mut stmt, &param_refs, format, wide)
        }
        OutputFormat::Default => {
            crate::output::print_context_rows(&mut stmt, &param_refs, wide)
        }
    }
}
```

**Param ordering:** Per the builder's contract, params appear in SQL in this order: `ordered_scope_where`'s params first, then `matches_subquery`'s params. Append them to `params` in that order. For `cq messages`, scope params (project, session, since) go first, then match-level params (type, grep). The builder embeds each exactly once.

- [ ] **Step 5: Update `main.rs` dispatch to pass `ctx`**

Already handled in Task 1 Step 4 — just confirm compilation now works after Task 4's signature change.

- [ ] **Step 6: Run integration tests**

Run: `cargo test --test integration_test messages_grep_with_context messages_context_does_not_cross_session -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Run the full test suite to catch regressions**

Run: `cargo test`
Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add src/commands/messages.rs src/commands/context.rs tests/fixtures/context_session.jsonl tests/integration_test.rs
git commit -m "feat(messages): support -A/-B/-C context flags"
```

---

## Task 5: Wire context into `cq tools`

**Files:**
- Modify: `src/commands/tools.rs`
- Modify: `tests/integration_test.rs`

- [ ] **Step 1: Write failing integration tests**

```rust
#[test]
fn tools_with_context_c_shows_surrounding_messages() {
    let env = setup_env(&["context_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "tools", "Read", "-C", "1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    // Read tool is at ord 4; -C 1 gives ords 3, 4, 5 → 3 rows
    assert_eq!(rows.len(), 3);
    // Match row should carry the tool name (tool-shaped) or the anchor message (message-shaped);
    // in our design, match rows in --json retain tool-specific fields.
    let match_row = rows.iter().find(|r| r["match_kind"] == "match").unwrap();
    assert_eq!(match_row["name"], "Read", "match row should retain tool name, got: {match_row}");
    // Context rows are message-shaped; they should have a `type` field (user/assistant).
    let context_rows: Vec<_> = rows.iter().filter(|r| r["match_kind"] != "match").collect();
    assert_eq!(context_rows.len(), 2);
    for ctx_row in &context_rows {
        assert!(ctx_row["type"].is_string(), "context row should be message-shaped, got: {ctx_row}");
    }
}

#[test]
fn tools_with_context_respects_match_limit() {
    // Put 2 Read tool calls in one session and use --limit 1 -C 0:
    // should return exactly 1 match row (grep -m semantics).
    // Use existing multi_tool_session.jsonl which has multiple tool calls.
    let env = setup_env(&["multi_tool_session.jsonl"]);
    let output = cq_cmd(&env)
        .args(["--json", "tools", "--limit", "1", "-C", "0"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let match_count = rows.iter().filter(|r| r["match_kind"] == "match").count();
    assert_eq!(match_count, 1, "expected 1 match with --limit 1");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test integration_test tools_with_context -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Update `tools::run` signature and add context branch**

In `src/commands/tools.rs`, add `ctx: Option<super::ContextWindow>` to the `run` signature. After existing conflict checks, insert:

```rust
super::check_count_by_context_conflict(count_by, ctx);

if let Some(window) = ctx {
    return run_with_context(conn, scope, tool_name, grep, errors_only, window, format, limit, wide);
}
```

Add `run_with_context`:

```rust
fn run_with_context(
    conn: &Connection,
    scope: &QueryScope,
    tool_name: Option<&str>,
    grep: Option<&str>,
    errors_only: bool,
    window: super::ContextWindow,
    format: &OutputFormat,
    match_limit: usize,
    wide: bool,
) -> Result<()> {
    // Scope conditions for the `ordered` CTE (over messages).
    let mut scope_conditions = vec!["1=1".to_string()];
    let mut params: Vec<Box<dyn duckdb::types::ToSql>> = Vec::new();

    if let Some(project) = &scope.project {
        scope_conditions.push("project ILIKE ?".to_string());
        params.push(Box::new(format!("%{project}%")));
    }
    if let Some(session) = &scope.session {
        scope_conditions.push("session_id = ?".to_string());
        params.push(Box::new(session.clone()));
    }
    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        scope_conditions.push(format!("timestamp >= '{formatted}'"));
    }
    let scope_where = scope_conditions.join(" AND ");

    // Matches subquery: tool_calls filtered by name/grep/errors, projected as session_id + message_uuid.
    let mut tool_conditions = vec!["1=1".to_string()];
    if let Some(name) = tool_name {
        tool_conditions.push("tc.name = ?".to_string());
        params.push(Box::new(name.to_string()));
    }
    if let Some(pattern) = grep {
        tool_conditions.push("CAST(tc.input AS VARCHAR) ILIKE ?".to_string());
        params.push(Box::new(format!("%{pattern}%")));
    }
    // Scope conditions also apply on tool_calls (by project/session/since).
    // We deliberately duplicate scope here because tool_calls view has these columns.
    if let Some(project) = &scope.project {
        tool_conditions.push("tc.project ILIKE ?".to_string());
        params.push(Box::new(format!("%{project}%")));
    }
    if let Some(session) = &scope.session {
        tool_conditions.push("tc.session_id = ?".to_string());
        params.push(Box::new(session.clone()));
    }
    let tool_where = tool_conditions.join(" AND ");

    let errors_join = if errors_only {
        "JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id AND tr.is_error = true"
    } else {
        ""
    };

    // Materialize matches to a temp table BEFORE building the context SQL. This eliminates
    // param duplication and lets both the context CTE and the JSON enrichment query reference
    // the same rows by table name.
    let matches_sql = format!(
        "CREATE OR REPLACE TEMP TABLE cq_ctx_matches AS \
         SELECT tc.session_id, tc.message_uuid, tc.name, CAST(tc.input AS VARCHAR) AS input, tc.tool_use_id, tc.timestamp \
         FROM tool_calls tc {errors_join} WHERE {tool_where}"
    );
    {
        let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        // Scope params were pushed before tool params, so skip scope params for this statement.
        let tool_param_start = scope_conditions.len() - 1; // -1 for the "1=1" entry which has no param
        conn.execute(&matches_sql, &param_refs[tool_param_start..])?;
    }

    // Now the context builder's matches_subquery is just SELECT from the temp table — no params.
    let matches_subquery = "SELECT session_id, message_uuid FROM cq_ctx_matches".to_string();
    let builder = super::ContextSqlBuilder {
        window,
        matches_subquery: &matches_subquery,
        ordered_scope_where: &scope_where,
        match_limit,
    };
    let sql = builder.build();

    // Only scope params remain for the context SQL.
    let scope_param_count = scope_conditions.iter().filter(|c| c.contains('?')).count();
    let scope_param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().take(scope_param_count).map(|p| p.as_ref()).collect();

    match format {
        OutputFormat::Json => {
            // Heterogeneous JSON: wrap the context SQL and LEFT JOIN the temp matches table
            // to enrich match rows with tool-specific columns.
            let wrapped = format!(
                "WITH ctx AS ({sql})
                 SELECT ctx.session_id, ctx.uuid, ctx.type, ctx.timestamp, ctx.text,
                        ctx.model, ctx.tool_count, ctx.project, ctx.match_kind, ctx.match_group,
                        m.name AS tool_name, m.input AS tool_input, m.tool_use_id
                 FROM ctx
                 LEFT JOIN cq_ctx_matches m
                   ON ctx.match_kind = 'match' AND ctx.uuid = m.message_uuid
                 ORDER BY ctx.session_id, ctx.timestamp"
            );
            let mut stmt = conn.prepare(&wrapped)?;
            output::print_results(&mut stmt, &scope_param_refs, format, wide)
        }
        OutputFormat::Table => {
            let mut stmt = conn.prepare(&sql)?;
            output::print_results(&mut stmt, &scope_param_refs, format, wide)
        }
        OutputFormat::Default => {
            let mut stmt = conn.prepare(&sql)?;
            crate::output::print_context_rows(&mut stmt, &scope_param_refs, wide)
        }
    }
}
```

**Note on param partitioning:** `params` is built with scope params first, then tool-match params. We pass the full vector when executing the `CREATE TEMP TABLE` (skipping the "1=1" base entry which has no `?`), and only the scope-param prefix when executing the context SQL (since the matches_subquery is now parameter-free).

- [ ] **Step 4: Run integration tests**

Run: `cargo test --test integration_test tools_with_context -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run full suite**

Run: `cargo test`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add src/commands/tools.rs src/output.rs tests/integration_test.rs
git commit -m "feat(tools): support -A/-B/-C context flags"
```

---

## Task 6: Context-aware table rendering (TTY)

**Files:**
- Modify: `src/output.rs` (add `print_context_rows`)
- Modify: `src/style.rs` (add dim helper)
- Modify: `tests/integration_test.rs`

- [ ] **Step 1: Write failing tests for TTY rendering**

```rust
#[test]
fn tty_context_hides_match_kind_and_group_columns() {
    let env = setup_env(&["context_session.jsonl"]);
    // Default mode (not --json, not --table) = TTY-style.
    // Even though assert_cmd captures stdout (not a TTY), NO_COLOR is set so we can grep plain text.
    let output = cq_cmd(&env)
        .args(["messages", "--grep", "NEEDLE", "-C", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("match_kind") && !stdout.contains("match_group"),
        "TTY output should not include metadata column headers, got:\n{stdout}"
    );
}

#[test]
fn tty_context_separates_non_contiguous_groups_with_dashes() {
    // Two matches in the same session far enough apart that -C 0 creates two groups.
    let env = setup_env(&["context_session.jsonl"]);
    // NEEDLE at ord 6 and (for this test, imagine a second) - if the fixture only has one,
    // modify to use a grep that matches two distinct ordinals. Use grep "n" which matches
    // "one", "nine", "seven", etc.
    let output = cq_cmd(&env)
        .args(["messages", "--grep", "seven", "-A", "0", "-B", "0"])
        .output()
        .unwrap();
    assert!(output.status.success());
    // With one match and zero context, no separator should appear (only one group).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("--"), "single group should not have separators, got:\n{stdout}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test integration_test tty_context -- --nocapture`
Expected: FAIL — `print_context_rows` doesn't exist / still shows metadata.

- [ ] **Step 3: Add `print_context_rows`**

Add to `src/output.rs`:

```rust
/// Render context-bearing rows for TTY output.
/// Drops `match_kind` and `match_group` columns from visual output.
/// Dims rows where `match_kind != 'match'`.
/// Prints `--` separator line when `match_group` changes.
pub fn print_context_rows(
    stmt: &mut duckdb::Statement,
    params: &[&dyn duckdb::types::ToSql],
    wide: bool,
) -> anyhow::Result<()> {
    let mut rows_iter = stmt.query(params)?;
    let column_names: Vec<String> = rows_iter.as_ref().unwrap().column_names()
        .iter().map(|s| s.to_string()).collect();

    let kind_idx = column_names.iter().position(|c| c == "match_kind");
    let group_idx = column_names.iter().position(|c| c == "match_group");

    let display_indices: Vec<usize> = (0..column_names.len())
        .filter(|i| Some(*i) != kind_idx && Some(*i) != group_idx)
        .collect();

    let max_width = if wide { 0 } else { 120 };
    let mut prev_group: Option<i64> = None;
    let mut out_rows: Vec<(Vec<String>, bool)> = Vec::new(); // (cells, is_match)

    while let Some(row) = rows_iter.next()? {
        let values: Vec<duckdb::types::Value> = (0..column_names.len())
            .map(|i| row.get::<_, duckdb::types::Value>(i).unwrap_or(duckdb::types::Value::Null))
            .collect();

        let this_group = group_idx.and_then(|i| match &values[i] {
            duckdb::types::Value::BigInt(n) => Some(*n),
            duckdb::types::Value::Int(n) => Some(*n as i64),
            _ => None,
        });
        let is_match = kind_idx
            .map(|i| matches!(&values[i], duckdb::types::Value::Text(s) if s == "match"))
            .unwrap_or(true);

        // Flush separator between groups.
        if let (Some(prev), Some(this)) = (prev_group, this_group) {
            if this != prev {
                out_rows.push((vec!["--".to_string()], false));
            }
        }
        prev_group = this_group.or(prev_group);

        let cells: Vec<String> = display_indices.iter()
            .map(|&i| value_to_string(&values[i], max_width))
            .collect();
        out_rows.push((cells, is_match));
    }

    // Render: dim context rows, normal for matches.
    for (cells, is_match) in &out_rows {
        if cells.len() == 1 && cells[0] == "--" {
            println!("--");
            continue;
        }
        let line = cells.join("  ");
        if *is_match {
            println!("{line}");
        } else {
            println!("{}", crate::style::dim(&line));
        }
    }
    Ok(())
}
```

Add `dim` to `src/style.rs`:

```rust
pub fn dim(s: &str) -> String {
    color(s, Color::Dim)
}
```

(If `Color::Dim` already exists per other renderers, reuse it; otherwise add a dim variant.)

- [ ] **Step 4: Run tests**

Run: `cargo test --test integration_test tty_context -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run full suite**

Run: `cargo test`
Expected: all green.

- [ ] **Step 6: Manual smoke test**

Run: `cargo run -- messages --grep NEEDLE -C 1 --session cccc0000` in a scratch test environment, or rely on the fixture via the existing test flow.
Expected: three rows, the middle one in default color, the outer two dimmed.

- [ ] **Step 7: Commit**

```bash
git add src/output.rs src/style.rs tests/integration_test.rs
git commit -m "feat(output): dim context rows and separate groups with -- in TTY mode"
```

---

## Task 7: Docs

**Files:**
- Modify: `docs/cli-ux-conventions.md`
- Modify: `docs/use-cases.md`
- Modify: `CLAUDE.md` (one-line pointer if needed)

- [ ] **Step 1: Add a context-flags section to `docs/cli-ux-conventions.md`**

Insert under "Checklist: adding a new flag" or a new top-level section:

````markdown
## Context flags (`-A`/`-B`/`-C`)

cq mirrors grep's context flags on `tools` and `messages`:

- `-A N` — N messages after each match
- `-B N` — N messages before each match
- `-C N` — N messages before AND after (shorthand for `-A N -B N`)

Context is always counted in messages (not tool calls). When `cq tools` has a context flag, its output becomes message-shaped for the terminal; `--json` keeps heterogeneous rows with tool-specific fields on matches. `--limit N` limits matches, not total output rows. `--count-by` is incompatible with context flags. Context never crosses session boundaries.

```bash
# What did the agent do right after this Skill invocation?
cq tools Skill --grep 'agent-meta:park' -A 3

# Three messages on either side of a grep hit
cq messages --grep 'compaction' -C 3
```
````

- [ ] **Step 2: Add a use case entry to `docs/use-cases.md`**

Add:

````markdown
## Trace what happened around a tool call

```bash
cq tools Read --grep '/etc/passwd' -C 2
```

Show the Read call plus two messages before and after. Useful for debugging why a tool was called, what context the agent had, and what it did with the result.
````

- [ ] **Step 3: Run all tests one final time**

Run: `cargo test`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add docs/cli-ux-conventions.md docs/use-cases.md
git commit -m "docs: document -A/-B/-C context flags"
```

---

## Task 8: Final verification and PR

- [ ] **Step 1: Verify all tests pass**

Run: `cargo test`
Expected: all green, no skipped tests.

- [ ] **Step 2: Push branch and open PR**

```bash
git push -u origin <branch>
gh pr create
```

---

## Self-Review Notes

- **Spec coverage:** The bean requested `-A`/`-B`/`-C` on tools, messages, and `sessions --grep`. Plan covers tools + messages; sessions is deliberately excluded per the design rationale (sessions is the container, not a line). Documented in the use-cases and cli-ux-conventions additions.
- **Param positioning:** The builder contract (scope params first, match params second) is written once in Task 3's approach note and applied consistently in Tasks 4 and 5.
- **Temp table for tools:** Task 5 sidesteps param duplication by materializing matches to `cq_ctx_matches` via `CREATE OR REPLACE TEMP TABLE`, letting both the context SQL and the heterogeneous JSON wrapper reference the table by name.
- **Clap version:** Existing Cargo.toml uses clap 4 derive. `conflicts_with_all` on `context` prevents `-C` with `-A`/`-B`. Clap auto-generates short help from doc comments.
- **Type consistency:** `ContextWindow`, `ContextSqlBuilder`, `run_with_context`, and `print_context_rows` names are used identically across Tasks 2-6. Fields `before`/`after` (not `pre`/`post` or similar) throughout.
