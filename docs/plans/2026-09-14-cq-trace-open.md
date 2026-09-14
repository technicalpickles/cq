# `cq trace --open` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `cq trace --open` (both `--format firefox-profiler` and `--format perfetto`) so the trace opens directly in a browser instead of requiring a manual save-and-drag-and-drop. Separately, fix `--format firefox-profiler`/`--format perfetto` so they never dump raw JSON onto an interactive terminal when `--open` isn't given.

**Architecture:** A new `src/trace/open_browser.rs` module runs a tiny local HTTP server (via `tiny_http`) that serves the trace JSON with the right CORS header, and launches the OS browser (via the `open` crate) pointed at the hosted viewer's URL-loading scheme (`profiler.firefox.com/from-url/...` or `ui.perfetto.dev/#!/?url=...`). `firefox_profiler.rs` and `perfetto.rs` each gain a `to_json` function (replacing their existing `emit`) that builds the JSON as a `String` without printing it, used by both the `--open` path and the non-`--open` default. `commands/trace.rs` decides, per invocation, whether to serve-and-open, print to stdout (piped/redirected), or write to a deterministic tmp file (interactive terminal, no `--open`).

**Tech Stack:** Rust, clap, serde_json (all existing). New dependencies: `tiny_http` (local HTTP server) and `open` (cross-platform browser launch).

**Design spec:** `docs/specs/2026-09-14-cq-trace-open-design.md`. Read it first — it records why this mechanism (not a bootstrap/postMessage page, not a locally-hosted UI) and the precedents (`samply`, `google/perfetto`'s `open_trace_in_ui`) that confirm it.

---

## Before you start

Run the existing test suite once, to have a clean baseline to compare against:

```bash
cargo test
```

Expected: all tests pass. If they don't, stop and figure out why before starting this plan — a pre-existing failure will otherwise get blamed on this work later.

---

## Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add `tiny_http` and `open` to `[dependencies]`**

In `Cargo.toml`, after the `dirs = "5"` line (the last line of `[dependencies]`), add:

```toml
# --open: local httpd serving the trace JSON to whichever hosted viewer
# (profiler.firefox.com or ui.perfetto.dev) the user's browser opens.
tiny_http = "0.12"
# --open: launches the OS default browser. Cross-platform so no hand-rolled
# per-OS `open`/`xdg-open`/`start` shell-out is needed.
open = "5"
```

- [ ] **Step 2: Verify the build picks them up**

Run: `cargo check`
Expected: compiles successfully (no code uses the new crates yet, so this just confirms the versions resolve).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
build(cq): add tiny_http and open for trace --open

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `src/trace/open_browser.rs` — the shared serve-and-open module

**Files:**
- Create: `src/trace/open_browser.rs`
- Modify: `src/trace/mod.rs:17-19`

This is the module described in the design doc's "Data flow / module changes" section: a third concern (network I/O + process spawn) that doesn't belong in either renderer or in `commands/trace.rs`. Built test-first: the request/response logic is unit-testable without opening a real browser by splitting `serve_forever` (the actual serving loop, unit-tested) out from `serve_and_open` (which also calls `open::that`, never exercised by automated tests).

- [ ] **Step 1: Register the module**

In `src/trace/mod.rs`, change:

```rust
pub mod firefox_profiler;
pub mod perfetto;
pub mod waterfall;
```

to:

```rust
pub mod firefox_profiler;
pub mod open_browser;
pub mod perfetto;
pub mod waterfall;
```

- [ ] **Step 2: Write the failing tests**

Create `src/trace/open_browser.rs` with just the test module (no implementation yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    #[test]
    fn percent_encode_escapes_colon_and_slash() {
        assert_eq!(
            percent_encode("http://127.0.0.1:4242/trace.json"),
            "http%3A%2F%2F127.0.0.1%3A4242%2Ftrace.json"
        );
    }

    #[test]
    fn serve_forever_responds_with_the_json_body_and_headers() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let json = r#"{"hello":"world"}"#.to_string();

        std::thread::scope(|scope| {
            scope.spawn(|| serve_forever(&server, &json, "https://example.com"));

            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(
                    b"GET /trace.json HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();

            assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");
            assert!(
                response.contains("Content-Type: application/json"),
                "got: {response}"
            );
            assert!(
                response.contains("Access-Control-Allow-Origin: https://example.com"),
                "got: {response}"
            );
            assert!(
                response.contains("Cache-Control: no-cache"),
                "got: {response}"
            );
            assert!(response.ends_with(r#"{"hello":"world"}"#), "got: {response}");

            server.unblock();
        });
    }
}
```

- [ ] **Step 3: Run to verify it fails (doesn't compile)**

Run: `cargo test --lib trace::open_browser`
Expected: FAIL — `error[E0432]: unresolved import` / `cannot find function `percent_encode`` / `serve_forever` (nothing is defined yet).

- [ ] **Step 4: Implement**

Add this above the test module in `src/trace/open_browser.rs`:

```rust
//! The mechanism behind `cq trace --open`: a tiny local HTTP server serves
//! the trace JSON, and the OS browser is pointed at whichever hosted
//! viewer's URL-loading scheme the caller wants
//! (`profiler.firefox.com/from-url/...` or `ui.perfetto.dev/#!/?url=...`).
//!
//! No bootstrap HTML page, no `postMessage` handshake: confirmed against two
//! real precedents (`samply`'s `samply load`, `google/perfetto`'s own
//! `tools/open_trace_in_ui`) that a plain `http://127.0.0.1:<port>` URL
//! fetches fine from the `https://`-hosted viewer, despite neither viewer's
//! docs stating that outright. See
//! `docs/specs/2026-09-14-cq-trace-open-design.md`.

use anyhow::Result;

/// Starts a local httpd on an OS-assigned port, serves `json_body` to any
/// request with the given CORS origin, opens the browser at
/// `make_browser_url(local_url)`, and blocks serving until Ctrl-C.
pub fn serve_and_open(
    json_body: String,
    cors_origin: &str,
    make_browser_url: impl FnOnce(&str) -> String,
) -> Result<()> {
    let server = tiny_http::Server::http("127.0.0.1:0")?;
    let port = server
        .server_addr()
        .to_ip()
        .expect("Server::http always binds a real IP socket, never a Unix socket")
        .port();
    let local_url = format!("http://127.0.0.1:{port}/trace.json");
    let browser_url = make_browser_url(&local_url);

    eprintln!("Serving trace at {local_url}");
    eprintln!("Opening {browser_url}");
    eprintln!("Press Ctrl-C to stop.");
    if let Err(e) = open::that(&browser_url) {
        eprintln!(
            "Couldn't open a browser automatically ({e}); open this URL yourself:\n{browser_url}"
        );
    }

    serve_forever(&server, &json_body, cors_origin);
    Ok(())
}

/// Responds to every request on `server` with `json_body` and the
/// CORS/content-type/cache headers a browser-hosted viewer needs, until the
/// server is unblocked (Ctrl-C in production; `Server::unblock()` in
/// tests). Split out from `serve_and_open` so tests can exercise the real
/// request/response behavior without ever calling `open::that`.
fn serve_forever(server: &tiny_http::Server, json_body: &str, cors_origin: &str) {
    for request in server.incoming_requests() {
        let response = tiny_http::Response::from_string(json_body.to_string())
            .with_header(content_type_json())
            .with_header(cors_header(cors_origin))
            .with_header(no_cache_header());
        let _ = request.respond(response);
    }
}

fn content_type_json() -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static header is always valid")
}

fn cors_header(origin: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Access-Control-Allow-Origin"[..], origin.as_bytes())
        .expect("cors_origin is always one of our own hardcoded https:// literals")
}

fn no_cache_header() -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-cache"[..])
        .expect("static header is always valid")
}

/// Percent-encode the handful of characters that ever appear in a
/// `http://127.0.0.1:<port>/trace.json`-shaped local URL and aren't already
/// URL-safe: `:` and `/`. Not a general-purpose percent-encoder — scoped to
/// this one fixed input shape, so no crate is needed for it.
pub fn percent_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ':' => "%3A".to_string(),
            '/' => "%2F".to_string(),
            other => other.to_string(),
        })
        .collect()
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib trace::open_browser`
Expected: PASS (2 tests: `percent_encode_escapes_colon_and_slash`,
`serve_forever_responds_with_the_json_body_and_headers`).

- [ ] **Step 6: Commit**

```bash
git add src/trace/mod.rs src/trace/open_browser.rs
git commit -m "$(cat <<'EOF'
feat(cq): add trace::open_browser, the shared --open plumbing

serve_and_open (local httpd + OS browser launch) and percent_encode,
unit-tested via serve_forever without ever invoking open::that.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `firefox_profiler.rs` — replace `emit` with `to_json`

**Files:**
- Modify: `src/trace/firefox_profiler.rs:298-307` (the `emit` function)
- Modify: `src/trace/firefox_profiler.rs` test module (add one test)

`emit` currently builds the profile and `println!`s it — the only caller is `commands/trace.rs`, which Task 6 rewrites to decide *how* to deliver the JSON (print, save to a tmp file, or serve-and-open) rather than always printing. So `emit` becomes `to_json`, returning the string instead of printing it.

- [ ] **Step 1: Write the failing test**

In `src/trace/firefox_profiler.rs`'s existing `#[cfg(test)] mod tests` block, add (near the other `build_profile`-based tests):

```rust
#[test]
fn to_json_matches_build_profile() {
    let spans = vec![span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100)];
    let groups = fixture_groups();
    let session_id = "a1b2c3d4-0000-4000-8000-000000000001";

    let json = to_json(&spans, &[], session_id, &groups).unwrap();
    let parsed: Value = serde_json::from_str(&json).unwrap();
    let expected = build_profile(&spans, &[], session_id, &groups);

    assert_eq!(parsed, expected);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib trace::firefox_profiler::tests::to_json_matches_build_profile`
Expected: FAIL — `error[E0425]: cannot find function `to_json` in this scope`.

- [ ] **Step 3: Replace `emit` with `to_json`**

Replace this (lines 298-307):

```rust
pub fn emit(
    spans: &[Span],
    gaps: &[Gap],
    session_id: &str,
    groups: &HashMap<String, String>,
) -> Result<()> {
    let profile = build_profile(spans, gaps, session_id, groups);
    println!("{}", serde_json::to_string(&profile)?);
    Ok(())
}
```

with:

```rust
/// Builds the profile JSON as a string, without printing it — used by every
/// caller (`--open`, the interactive-terminal tmp-file default, and the
/// plain stdout default) so there is exactly one place that decides how the
/// JSON reaches the user.
pub fn to_json(
    spans: &[Span],
    gaps: &[Gap],
    session_id: &str,
    groups: &HashMap<String, String>,
) -> Result<String> {
    let profile = build_profile(spans, gaps, session_id, groups);
    Ok(serde_json::to_string(&profile)?)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trace::firefox_profiler`
Expected: PASS, including the new `to_json_matches_build_profile` test.

- [ ] **Step 5: Commit**

```bash
git add src/trace/firefox_profiler.rs
git commit -m "$(cat <<'EOF'
refactor(cq): firefox_profiler::emit -> to_json

Returns the built JSON instead of printing it, so callers (--open,
the tmp-file default, plain stdout) each decide how to deliver it.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `perfetto.rs` — replace `emit` with `to_json`

**Files:**
- Modify: `src/trace/perfetto.rs:43-52` (the `emit` function)
- Modify: `src/trace/perfetto.rs` test module (add one test)

Same change as Task 3, mirrored for the Perfetto emitter.

- [ ] **Step 1: Write the failing test**

In `src/trace/perfetto.rs`'s existing `#[cfg(test)] mod tests` block, add:

```rust
#[test]
fn to_json_matches_build_events() {
    let spans = vec![span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100)];
    let groups = fixture_groups();
    let session_id = "a1b2c3d4-0000-4000-8000-000000000001";

    let json = to_json(&spans, &[], session_id, &groups).unwrap();
    let parsed: Vec<Value> = serde_json::from_str(&json).unwrap();
    let expected = build_events(&spans, &[], session_id, &groups);

    assert_eq!(parsed, expected);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib trace::perfetto::tests::to_json_matches_build_events`
Expected: FAIL — `error[E0425]: cannot find function `to_json` in this scope`.

- [ ] **Step 3: Replace `emit` with `to_json`**

Replace this (lines 43-52):

```rust
pub fn emit(
    spans: &[Span],
    gaps: &[Gap],
    session_id: &str,
    groups: &HashMap<String, String>,
) -> Result<()> {
    let events = build_events(spans, gaps, session_id, groups);
    println!("{}", serde_json::to_string(&events)?);
    Ok(())
}
```

with:

```rust
/// Builds the Chrome Trace Event JSON as a string, without printing it —
/// see `firefox_profiler::to_json`'s doc comment for why.
pub fn to_json(
    spans: &[Span],
    gaps: &[Gap],
    session_id: &str,
    groups: &HashMap<String, String>,
) -> Result<String> {
    let events = build_events(spans, gaps, session_id, groups);
    Ok(serde_json::to_string(&events)?)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trace::perfetto`
Expected: PASS, including the new `to_json_matches_build_events` test.

- [ ] **Step 5: Commit**

```bash
git add src/trace/perfetto.rs
git commit -m "$(cat <<'EOF'
refactor(cq): perfetto::emit -> to_json

Mirrors the firefox_profiler::to_json change: returns the built JSON
instead of printing it.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `commands/trace.rs` — wire up `--open` and the tmp-file default

**Files:**
- Modify: `src/commands/trace.rs:1-6` (imports)
- Modify: `src/commands/trace.rs:21-103` (`run`, the `TraceOutput` match)
- Modify: `src/commands/trace.rs` test module (add unit tests)

This is where the design doc's CLI-surface validation, the tmp-file default, and the `--open` dispatch all live.

- [ ] **Step 1: Write the failing unit tests**

In `src/commands/trace.rs`'s existing `#[cfg(test)] mod tests` block (after the existing `try_parse_bound` tests), add:

```rust
#[test]
fn tmp_trace_path_is_deterministic_per_session_and_format() {
    let dir = std::path::Path::new("/tmp/example");
    let a = tmp_trace_path(dir, "a1b2c3d4-0000-4000-8000-000000000001", "firefox-profiler");
    let b = tmp_trace_path(dir, "a1b2c3d4-0000-4000-8000-000000000001", "firefox-profiler");
    assert_eq!(a, b, "same session+format must produce the same path");
    assert_eq!(
        a,
        dir.join("cq-trace-a1b2c3d4-firefox-profiler.json"),
        "got: {a:?}"
    );
}

#[test]
fn tmp_trace_path_differs_by_format() {
    let dir = std::path::Path::new("/tmp/example");
    let session_id = "a1b2c3d4-0000-4000-8000-000000000001";
    assert_ne!(
        tmp_trace_path(dir, session_id, "firefox-profiler"),
        tmp_trace_path(dir, session_id, "perfetto"),
    );
}

#[test]
fn write_or_print_interactive_writes_the_json_to_the_tmp_path() {
    let dir = tempfile::tempdir().unwrap();
    let session_id = "a1b2c3d4-0000-4000-8000-000000000001";
    let json = r#"{"hello":"world"}"#;

    write_or_print(json, session_id, "firefox-profiler", true, dir.path()).unwrap();

    let written =
        std::fs::read_to_string(tmp_trace_path(dir.path(), session_id, "firefox-profiler"))
            .unwrap();
    assert_eq!(written, json);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib commands::trace::tests::tmp_trace_path`
Expected: FAIL — `error[E0425]: cannot find function `tmp_trace_path` in this scope` (and similarly for `write_or_print`).

- [ ] **Step 3: Update imports**

Replace lines 1-6:

```rust
use anyhow::Result;
use duckdb::Connection;

use crate::output::OutputFormat;
use crate::scope::QueryScope;
use crate::trace;
```

with:

```rust
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::Result;
use duckdb::Connection;

use crate::output::OutputFormat;
use crate::scope::QueryScope;
use crate::trace;
```

- [ ] **Step 4: Add the `--open` parameter and its up-front validation**

In the `run` function signature (currently lines 21-28), replace:

```rust
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    format: &OutputFormat,
    output: TraceOutput,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<()> {
```

with:

```rust
pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    format: &OutputFormat,
    output: TraceOutput,
    from: Option<&str>,
    to: Option<&str>,
    open: bool,
) -> Result<()> {
    // Both checks up front, before touching the database: --open only makes
    // sense for the two JSON-producing renderers, and --json already has its
    // own escape-hatch meaning (raw span rows) that --open can't fulfill.
    if open && matches!(output, TraceOutput::Waterfall) {
        eprintln!("Error: --open is not supported with --format waterfall");
        eprintln!("Valid formats for --open: firefox-profiler, perfetto");
        std::process::exit(1);
    }
    if open && matches!(format, OutputFormat::Json) {
        eprintln!("Error: --open is not supported with --json");
        eprintln!("Valid formats for --open: firefox-profiler, perfetto");
        std::process::exit(1);
    }
```

(The existing `let session_id = match scope.session.as_deref() { ... };` line that immediately follows stays as-is — these two checks are new lines inserted just above it, inside the same function body.)

- [ ] **Step 5: Replace the `TraceOutput` match**

Replace the existing match (originally lines 90-103):

```rust
    match output {
        TraceOutput::Waterfall => trace::waterfall::render(&spans, &gaps),
        TraceOutput::Perfetto => {
            // Only the process-grouped renderers need lane groups;
            // waterfall has no notion of pid, so this query is skipped for it.
            let groups = trace::lane_groups(conn, session_id)?;
            trace::perfetto::emit(&spans, &gaps, session_id, &groups)
        }
        TraceOutput::FirefoxProfiler => {
            let groups = trace::lane_groups(conn, session_id)?;
            trace::firefox_profiler::emit(&spans, &gaps, session_id, &groups)
        }
    }
}
```

with:

```rust
    match output {
        TraceOutput::Waterfall => trace::waterfall::render(&spans, &gaps),
        TraceOutput::Perfetto => {
            // Only the process-grouped renderers need lane groups;
            // waterfall has no notion of pid, so this query is skipped for it.
            let groups = trace::lane_groups(conn, session_id)?;
            let json = trace::perfetto::to_json(&spans, &gaps, session_id, &groups)?;
            if open {
                trace::open_browser::serve_and_open(
                    json,
                    "https://ui.perfetto.dev",
                    perfetto_browser_url,
                )
            } else {
                write_or_print(
                    &json,
                    session_id,
                    "perfetto",
                    std::io::stdout().is_terminal(),
                    &std::env::temp_dir(),
                )
            }
        }
        TraceOutput::FirefoxProfiler => {
            let groups = trace::lane_groups(conn, session_id)?;
            let json = trace::firefox_profiler::to_json(&spans, &gaps, session_id, &groups)?;
            if open {
                trace::open_browser::serve_and_open(
                    json,
                    "https://profiler.firefox.com",
                    firefox_profiler_browser_url,
                )
            } else {
                write_or_print(
                    &json,
                    session_id,
                    "firefox-profiler",
                    std::io::stdout().is_terminal(),
                    &std::env::temp_dir(),
                )
            }
        }
    }
}

/// Builds `https://profiler.firefox.com/from-url/<encoded local url>` — see
/// `docs-developer/loading-in-profiles.md` in the `firefox-devtools/profiler`
/// repo and `docs/specs/2026-09-14-cq-trace-open-design.md`.
fn firefox_profiler_browser_url(local_url: &str) -> String {
    format!(
        "https://profiler.firefox.com/from-url/{}",
        trace::open_browser::percent_encode(local_url)
    )
}

/// Builds `https://ui.perfetto.dev/#!/?url=<encoded local url>&referrer=cq`
/// — matches the URL shape `google/perfetto`'s own `tools/open_trace_in_ui`
/// script builds (params after `#!/`, a fragment, not a real query string).
fn perfetto_browser_url(local_url: &str) -> String {
    format!(
        "https://ui.perfetto.dev/#!/?url={}&referrer=cq",
        trace::open_browser::percent_encode(local_url)
    )
}

/// Without `--open`: keep piped/redirected stdout exactly as it's always
/// been (so `> file.json` and script consumers see no change), but stop
/// dumping the raw JSON onto an interactive terminal — write it to a
/// deterministic tmp path instead. `interactive` and `tmp_dir` are passed in
/// rather than read here so tests can drive both branches without a real
/// terminal or relying on `$TMPDIR`.
fn write_or_print(
    json: &str,
    session_id: &str,
    format_name: &str,
    interactive: bool,
    tmp_dir: &Path,
) -> Result<()> {
    if interactive {
        let path = tmp_trace_path(tmp_dir, session_id, format_name);
        std::fs::write(&path, json)?;
        eprintln!("Wrote {format_name} trace to {}", path.display());
        eprintln!(
            "Open it at {}, or re-run with --open.",
            viewer_url(format_name)
        );
    } else {
        println!("{json}");
    }
    Ok(())
}

/// Deterministic per session+format, so re-running the same trace refreshes
/// the same file instead of littering the tmp dir with a new copy every run.
fn tmp_trace_path(tmp_dir: &Path, session_id: &str, format_name: &str) -> PathBuf {
    let short_id = &session_id[..8.min(session_id.len())];
    tmp_dir.join(format!("cq-trace-{short_id}-{format_name}.json"))
}

fn viewer_url(format_name: &str) -> &'static str {
    match format_name {
        "firefox-profiler" => "https://profiler.firefox.com",
        "perfetto" => "https://ui.perfetto.dev",
        other => unreachable!("write_or_print is only ever called with \"firefox-profiler\" or \"perfetto\", got {other:?}"),
    }
}
```

- [ ] **Step 6: Run the new unit tests to verify they pass**

Run: `cargo test --lib commands::trace::tests`
Expected: PASS, including the three new tests from Step 1 plus all pre-existing ones in this module (`offset_seconds`, `bad_unit_quotes_input`, etc. — unaffected by this change).

- [ ] **Step 7: Run the full crate build to catch the now-broken `main.rs` call site**

Run: `cargo check`
Expected: FAIL — `error[E0061]: this function takes 7 arguments but 6 arguments were supplied` at the `trace::run(...)` call in `src/main.rs`. This is expected; Task 6 fixes it. Confirming the error here (rather than skipping straight to Task 6) is the point — it proves the compiler, not just intuition, is tracking this dependency.

- [ ] **Step 8: Commit**

```bash
git add src/commands/trace.rs
git commit -m "$(cat <<'EOF'
feat(cq): wire --open and the interactive-terminal tmp-file default

trace::run gains an `open` param: validates --open against --format/
--json, dispatches to open_browser::serve_and_open for firefox-profiler
and perfetto, and otherwise writes to a deterministic tmp path when
stdout is an interactive terminal (piped/redirected output unchanged).

Known-broken: src/main.rs's call site needs updating (next commit).

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `main.rs` — the `--open` CLI flag

**Files:**
- Modify: `src/main.rs:223-238` (`Command::Trace` variant definition)
- Modify: `src/main.rs:553-575` (`Command::Trace` dispatch arm)

- [ ] **Step 1: Add the `--open` field to the `Trace` subcommand**

Replace (lines 223-238):

```rust
    /// Render a session as a trace: one lane per subagent, duration bars per tool call, gaps between
    Trace {
        /// Which renderer to use [valid: waterfall, perfetto, firefox-profiler]
        #[arg(long = "format", value_enum, conflicts_with = "perfetto")]
        trace_format: Option<TraceFormat>,

        /// Deprecated: use `--format perfetto` instead. Emits Chrome Trace
        /// Event JSON on stdout (loads in Perfetto, Firefox Profiler, Speedscope)
        #[arg(long, hide = true)]
        perfetto: bool,

        /// Window start: offset from session start (e.g. +12m, +90s) or an absolute ISO timestamp
        #[arg(long)]
        from: Option<String>,

        /// Window end: offset from session start (e.g. +17m, +90s) or an absolute ISO timestamp
        #[arg(long)]
        to: Option<String>,
    },
```

with:

```rust
    /// Render a session as a trace: one lane per subagent, duration bars per tool call, gaps between
    Trace {
        /// Which renderer to use [valid: waterfall, perfetto, firefox-profiler]
        #[arg(long = "format", value_enum, conflicts_with = "perfetto")]
        trace_format: Option<TraceFormat>,

        /// Deprecated: use `--format perfetto` instead. Emits Chrome Trace
        /// Event JSON on stdout (loads in Perfetto, Firefox Profiler, Speedscope)
        #[arg(long, hide = true)]
        perfetto: bool,

        /// Open the trace directly in a browser (firefox-profiler or perfetto)
        /// instead of printing/saving its JSON. Implies --format
        /// firefox-profiler when --format is omitted.
        #[arg(long)]
        open: bool,

        /// Window start: offset from session start (e.g. +12m, +90s) or an absolute ISO timestamp
        #[arg(long)]
        from: Option<String>,

        /// Window end: offset from session start (e.g. +17m, +90s) or an absolute ISO timestamp
        #[arg(long)]
        to: Option<String>,
    },
```

- [ ] **Step 2: Thread `open` through the dispatch arm**

Replace (lines 553-575):

```rust
        Command::Trace {
            trace_format,
            perfetto,
            from,
            to,
        } => {
            // clap's `conflicts_with` on `trace_format` already rules out
            // both being set, so only one of these two branches can apply.
            let output = match trace_format {
                Some(TraceFormat::Waterfall) => trace::TraceOutput::Waterfall,
                Some(TraceFormat::Perfetto) => trace::TraceOutput::Perfetto,
                Some(TraceFormat::FirefoxProfiler) => trace::TraceOutput::FirefoxProfiler,
                None if perfetto => trace::TraceOutput::Perfetto,
                None => trace::TraceOutput::Waterfall,
            };
            trace::run(
                &conn,
                &scope,
                &format,
                output,
                from.as_deref(),
                to.as_deref(),
            )?;
```

with:

```rust
        Command::Trace {
            trace_format,
            perfetto,
            open,
            from,
            to,
        } => {
            // clap's `conflicts_with` on `trace_format` already rules out
            // both being set, so only one of these two branches can apply.
            let output = match trace_format {
                Some(TraceFormat::Waterfall) => trace::TraceOutput::Waterfall,
                Some(TraceFormat::Perfetto) => trace::TraceOutput::Perfetto,
                Some(TraceFormat::FirefoxProfiler) => trace::TraceOutput::FirefoxProfiler,
                None if perfetto => trace::TraceOutput::Perfetto,
                // --open with no explicit --format needs a real target; the
                // richer/newer renderer is the implied default.
                None if open => trace::TraceOutput::FirefoxProfiler,
                None => trace::TraceOutput::Waterfall,
            };
            trace::run(
                &conn,
                &scope,
                &format,
                output,
                from.as_deref(),
                to.as_deref(),
                open,
            )?;
```

- [ ] **Step 3: Verify the whole crate builds**

Run: `cargo build`
Expected: compiles with no errors or warnings.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: PASS — every pre-existing test (including `perfetto_output_is_valid_trace_json`, `firefox_profiler_output_has_categories_and_marker_schema`, `trace_perfetto_flag_is_a_deprecated_alias_for_format_perfetto`, etc. in `tests/integration_test.rs`) plus every new unit test from Tasks 2–5.

These existing tests are the regression guard for "piped/redirected output is unchanged": `assert_cmd`'s `.output()` captures stdout through a pipe, so `std::io::stdout().is_terminal()` evaluates to `false` inside the subprocess exactly as it always has, and `write_or_print` takes the `println!` branch unchanged. If any of them fail here, something in Task 5's `write_or_print` wiring regressed the non-interactive path — stop and fix it before continuing.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "$(cat <<'EOF'
feat(cq): add --open flag to cq trace

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Integration tests for the `--open` validation errors

**Files:**
- Modify: `tests/integration_test.rs` (add two tests near the existing `trace_format_and_perfetto_flag_together_is_a_clap_error` test, around line 3392)

These two error paths (`--open` + `--format waterfall`, `--open` + `--json`) exit before ever reaching `open_browser::serve_and_open` — safe to run in a normal test process. **Do not** write a test that lets `--open` reach `serve_and_open` for real: that call blocks forever waiting for Ctrl-C and would hang the test suite.

- [ ] **Step 1: Write the failing tests**

Add, right after `trace_format_and_perfetto_flag_together_is_a_clap_error`:

```rust
#[test]
fn open_with_format_waterfall_is_an_error() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env)
        .args([
            "--session",
            TRACE_SESSION,
            "trace",
            "--format",
            "waterfall",
            "--open",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--open is not supported with --format waterfall"),
        "got: {stderr}"
    );
}

#[test]
fn open_with_json_is_an_error() {
    let env = setup_env_tree(TRACE_SESSION);
    let output = cq_cmd(&env)
        .args(["--session", TRACE_SESSION, "--json", "trace", "--open"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--open is not supported with --json"),
        "got: {stderr}"
    );
}
```

- [ ] **Step 2: Run to verify they pass**

Run: `cargo test --test integration_test open_with`
Expected: PASS (2 tests).

(These should already pass immediately, since Task 5/6 already implemented the validation — this step confirms the CLI wiring behaves exactly as the unit-level checks intended, at the actual binary boundary.)

- [ ] **Step 3: Commit**

```bash
git add tests/integration_test.rs
git commit -m "$(cat <<'EOF'
test(cq): pin --open's validation errors at the CLI boundary

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Manual verification (not automated — do this yourself)

Automated tests intentionally never exercise the real `serve_and_open` path (it blocks on Ctrl-C and launches a real browser). Verify it by hand against a real session:

- [ ] **Step 1: Find a real session id**

Run: `cargo run -- sessions --limit 5`
Note one of the session ids printed.

- [ ] **Step 2: Verify `--open` for firefox-profiler**

Run: `cargo run -- trace --session <id> --open --format firefox-profiler`
Expected:
- stderr prints `Serving trace at http://127.0.0.1:<port>/trace.json`, `Opening https://profiler.firefox.com/from-url/...`, `Press Ctrl-C to stop.`
- your default browser opens to `profiler.firefox.com` with the trace already loaded (Marker Chart shows tool spans/gaps, not a blank/error state).
- `Ctrl-C` in the terminal stops the command.

- [ ] **Step 3: Verify `--open` for perfetto**

Run: `cargo run -- trace --session <id> --open --format perfetto`
Expected: same shape, browser opens `ui.perfetto.dev` with the trace loaded on its timeline. `Ctrl-C` stops it.

- [ ] **Step 4: Verify the tmp-file default (no `--open`, interactive terminal)**

Run directly in your terminal (not piped): `cargo run -- trace --session <id> --format firefox-profiler`
Expected: no JSON dumped to the screen. stderr prints `Wrote firefox-profiler trace to /tmp/cq-trace-<short-id>-firefox-profiler.json` and `Open it at https://profiler.firefox.com, or re-run with --open.` Confirm the file exists and contains the trace JSON: `cat /tmp/cq-trace-<short-id>-firefox-profiler.json | head -c 200`.

- [ ] **Step 5: Verify piped output is unchanged**

Run: `cargo run -- trace --session <id> --format firefox-profiler | head -c 200`
Expected: raw JSON on stdout, exactly like before this change — no "Wrote ... to" message, no tmp file involved.

---

## Task 9: Docs

**Files:**
- Modify: `docs/cli-ux-conventions.md` (docs-sync table)
- Modify: `README.md` (flag reference)

Per `CLAUDE.md`'s "Keeping docs in sync" table: a new flag needs the README's flag table updated. Read `docs/cli-ux-conventions.md`'s "Keeping docs in sync" section first to confirm exactly which row(s) apply and which other doc(s) it points at for a new flag — don't guess the row from memory, the table is short enough to just check.

- [ ] **Step 1: Update the README's flag reference**

Add `--open` to the trace command's flag documentation in `README.md`, alongside the existing `--format`/`--from`/`--to` entries. Match the existing table/list style exactly (check how `--format firefox-profiler` is currently documented there and mirror it).

- [ ] **Step 2: Confirm nothing else in the docs-sync table applies**

This change didn't touch `views.rs`, the module tree structure (beyond adding one new file, already reflected in `CLAUDE.md`'s Architecture tree — update that too if the table's row for "modules" says so), or sync/cache behavior. Update `CLAUDE.md`'s Architecture tree entry for `trace/` to list `open_browser.rs` alongside `perfetto.rs`/`waterfall.rs`.

- [ ] **Step 3: Commit**

```bash
git add README.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs(cq): document --open

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```
