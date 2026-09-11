# Firefox Profiler Native Emitter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `cq trace` a second output format, targeting Firefox Profiler's native processed-profile JSON directly, so tool spans and gaps render in distinct colors in the browser's Marker Chart (issue #49) — something the existing Chrome Trace emitter can never do, because Firefox Profiler's own Chrome Trace importer hardcodes every category to grey.

**Architecture:** A new sibling module, `src/trace/firefox_profiler.rs`, reads the same `Span`/`Gap` model `perfetto.rs` already reads from `trace/mod.rs`. Two shared helpers (`marker_detail`, `truncate_for_marker`) move from `perfetto.rs` into `trace/mod.rs` so both emitters use identical detail-string truncation. `cq trace` gains a `--format <waterfall|perfetto|firefox-profiler>` enum; the existing `--perfetto` boolean stays as a deprecated alias for `--format perfetto`.

**Tech Stack:** Rust, `serde_json` (ad hoc `json!()` construction, matching `perfetto.rs`'s existing style — no new typed structs), `clap` (`ValueEnum` derive).

**Design doc:** `docs/specs/2026-09-11-firefox-profiler-native-emitter-design.md`. Read it first — this plan implements that design and doesn't re-derive its reasoning (category taxonomy, why generic fields, why this format over extending Chrome Trace).

---

## Status: Tasks 1–5 already implemented and verified

Tasks 1–5 below were implemented directly rather than dispatched to a fresh subagent, because getting the exact JSON shape right required empirical verification against Firefox Profiler's actual importer — a fresh subagent with no access to the `profiler` repo's source would have had to re-derive the same schema facts (which fields are required, what `{}` vs `[]` means for empty typed arrays, whether zero samples is accepted, how `data.type` maps to `MarkerSchema.name`) through trial and error against a tool it can't easily round-trip against. That verification is done; the code below is the actual, tested result, not a plan to be re-executed.

**Verified 2026-09-11:**
- All 31 `src/trace/` unit tests pass (`cargo test --lib trace::`).
- All 98 lib unit tests pass; the only integration-test failures (8, in `search_*`) are pre-existing and network-gated (DuckDB's `fts` extension download), unrelated to this change.
- A real cq session (`85de473d-650c-441d-9157-5fa48d1c9bae`) exported via `cq trace --format firefox-profiler` and loaded into `profiler-cli` (Mozilla's own CLI, built from the `profiler` repo's source) end to end: `Bash` markers show `Type: ToolCall`, `Category: Bash`; `mcp__qmd__query` shows `Category: MCP tool`; gaps show `Type: Gap`, `Category: Think gap` — exactly matching the design's taxonomy.

**Files actually changed (Tasks 1–5, done):**
- Modified: `src/trace/mod.rs` — hoisted `MAX_DETAIL_LEN`, `truncate_for_marker`, `marker_detail` from `perfetto.rs`; added `firefox_profiler` module declaration; added `detail_tests` module for the hoisted functions.
- Modified: `src/trace/perfetto.rs` — imports the hoisted helpers from `super`/`crate::trace` instead of defining them locally; removed the now-duplicate tests.
- Created: `src/trace/firefox_profiler.rs` — the emitter itself (category taxonomy, `StringTable`, marker building, thread/pid/tid grouping mirroring `perfetto.rs`, `build_profile`, `emit`), plus its own test module (12 tests).
- Modified: `src/commands/trace.rs` — `TraceOutput` gained a `FirefoxProfiler` variant; `run()`'s match arm dispatches to `trace::firefox_profiler::emit`.
- Modified: `src/main.rs` — new `TraceFormat` `ValueEnum` (`Waterfall`/`Perfetto`/`FirefoxProfiler`, clap kebab-cases the last to `firefox-profiler`); `Trace` subcommand gained `--format` (`conflicts_with = "perfetto"`) alongside the now-`hide = true` legacy `--perfetto`; dispatch match arm resolves both into one `TraceOutput`.

If you're re-verifying rather than trusting this record: `cargo test --lib trace::` from `src/trace/firefox_profiler.rs`'s directory, and the manual profiler-cli round-trip recipe is in this plan's Task 6 verification step below (same recipe, already run once).

---

## Remaining tasks (dispatch these to fresh subagents)

### Task 6: Integration tests for the new CLI surface

**Files:**
- Modify: `tests/integration_test.rs`

The exact pattern to mirror is `perfetto_output_is_valid_trace_json` (search for that name in `tests/integration_test.rs`, around line 3221): `setup_env_tree(TRACE_SESSION)` builds the fixture env, `cq_cmd(&env)` returns the `assert_cmd::Command` builder, `TRACE_SESSION` is the fixture session's id constant. Use those exact helpers, not a bespoke one.

- [ ] **Step 1: Write the failing tests**

Add these to `tests/integration_test.rs`, right after `perfetto_gaps_are_categorized` (or wherever the other `trace --perfetto` tests live):

```rust
#[test]
fn firefox_profiler_output_has_categories_and_marker_schema() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env)
        .args(["--session", TRACE_SESSION, "trace", "--format", "firefox-profiler"])
        .output()
        .unwrap();
    let profile: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("--format firefox-profiler must emit valid JSON");
    assert!(profile["meta"]["categories"].is_array());
    assert!(
        profile["meta"]["categories"].as_array().unwrap().len() <= 10,
        "must stay within Firefox Profiler's 10-color GraphColor palette"
    );
    assert!(profile["meta"]["markerSchema"].is_array());
    assert!(profile["threads"].is_array());
    assert!(
        !profile["threads"].as_array().unwrap().is_empty(),
        "expected at least one thread"
    );
}

#[test]
fn firefox_profiler_tool_markers_carry_the_toolcall_type() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env)
        .args(["--session", TRACE_SESSION, "trace", "--format", "firefox-profiler"])
        .output()
        .unwrap();
    let profile: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let threads = profile["threads"].as_array().unwrap();
    let has_toolcall_marker = threads.iter().any(|t| {
        t["markers"]["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["type"] == "ToolCall")
    });
    assert!(has_toolcall_marker, "expected at least one ToolCall marker across threads");
}

#[test]
fn trace_perfetto_flag_is_a_deprecated_alias_for_format_perfetto() {
    let env = setup_env_tree(TRACE_SESSION);
    let via_flag = cq_cmd(&env)
        .args(["--session", TRACE_SESSION, "trace", "--perfetto"])
        .output()
        .unwrap();
    let via_format = cq_cmd(&env)
        .args(["--session", TRACE_SESSION, "trace", "--format", "perfetto"])
        .output()
        .unwrap();
    assert!(via_flag.status.success());
    assert!(via_format.status.success());
    assert_eq!(via_flag.stdout, via_format.stdout);
}

#[test]
fn trace_format_and_perfetto_flag_together_is_a_clap_error() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env)
        .args(["--session", TRACE_SESSION, "trace", "--format", "perfetto", "--perfetto"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "expected clap's own conflicts_with message, got: {stderr}"
    );
}
```

- [ ] **Step 2: Run the new tests**

```bash
cargo test --test integration_test firefox_profiler
cargo test --test integration_test trace_perfetto_flag_is_a_deprecated_alias
cargo test --test integration_test trace_format_and_perfetto_flag_together
```

Expected: all pass immediately. The `--format`/`--perfetto` CLI wiring already exists (Tasks 1–5 are done) — this task is about coverage, not a red-green cycle on already-shipped code. If any fail, that's a real bug in the existing implementation to fix, not a sign the test is wrong.

- [ ] **Step 3: Run the full integration suite to confirm no regressions**

```bash
cargo test --test integration_test
```

Expected: same 125 passing / 8 pre-existing network-gated `search_*` failures as before this task (see this plan's Status section) — no new failures.

- [ ] **Step 4: Commit**

```bash
git add tests/integration_test.rs
git commit -m "test(trace): cover --format firefox-profiler and the --perfetto alias"
```

### Task 7: Docs sync

**Files:**
- Modify: `docs/cli-ux-conventions.md`
- Modify: `README.md`

Per `CLAUDE.md`'s "Keeping docs in sync" pointer: read `docs/cli-ux-conventions.md`'s own "Keeping docs in sync" table first, find the row for flag changes, and follow exactly what it says to update. Do not guess the format — that table is the authority on what changes where.

- [ ] **Step 1: Update `docs/cli-ux-conventions.md`**

Find wherever `cq trace`'s `--perfetto` flag is currently documented (search the file for `perfetto`) and add `--format` alongside it, following this file's own stated convention for fixed-value flags: `[valid: waterfall, perfetto, firefox-profiler]` in the flag's one-line description, matching the pattern already used for `--harness` (`[valid: claude, codex]`) elsewhere in this same file.

- [ ] **Step 2: Update the README flag table**

Find the `cq trace` row(s) in README.md's flag reference (Part 2, per `.claude/rules/readme.md`'s structure) and add `--format` there too, noting `--perfetto` as deprecated.

- [ ] **Step 3: Verify no other doc references the old `--perfetto`-only behavior**

```bash
grep -rn "\-\-perfetto" docs/ README.md src/commands/schema.rs
```

Check `src/commands/schema.rs` specifically — `cq schema --examples` may include a `cq trace --perfetto` example that should show `--format firefox-profiler` too, or at minimum isn't now misleading.

- [ ] **Step 4: Commit**

```bash
git add docs/cli-ux-conventions.md README.md
git commit -m "docs(trace): document --format and the firefox-profiler emitter"
```

## Verification (Task 6's manual round-trip, for reference — already run once during Task 1–5)

```bash
cq trace --session <id> --all --format firefox-profiler > /tmp/trace.json
# In a local checkout of firefox-devtools/profiler:
yarn build-cli
PROFILER_CLI_SESSION_DIR=$TMPDIR/pcli node profiler-cli/dist/profiler-cli.js load /tmp/trace.json
PROFILER_CLI_SESSION_DIR=$TMPDIR/pcli node profiler-cli/dist/profiler-cli.js thread markers --list
# Expect: Bash markers show Category: Bash, mcp__* tools show Category: MCP tool,
# think/blocked-on-you gaps show Category: Think gap / Human gap.
```
