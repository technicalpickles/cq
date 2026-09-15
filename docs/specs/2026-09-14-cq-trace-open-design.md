# `cq trace --open`

Status: approved (design)
Date: 2026-09-14

## Problem

`cq trace --format firefox-profiler` and `cq trace --format perfetto` both
just `println!` their JSON to stdout (`src/trace/firefox_profiler.rs`,
`src/trace/perfetto.rs`) unconditionally — including when run interactively
with no redirect, which floods the terminal with a giant JSON blob. Loading
either into its viewer today means redirecting stdout to a file, then
manually opening the viewer and drag-and-dropping the file in. `--open`
should do that last step for the user; separately, the raw-JSON-to-a-live-
terminal case should stop happening at all.

## Goals

- `cq trace --session <id> --open` opens the trace directly in a browser,
  with the profile already loaded — no manual save-then-drag-and-drop.
- Works for both `--format firefox-profiler` and `--format perfetto`.
- Without `--open`: these two formats never dump raw JSON onto an
  interactive terminal. Piped/redirected output is unchanged.

## Non-goals

- A locally-hosted Firefox Profiler or Perfetto UI. Researched and ruled
  out — see "Rejected: locally-hosted UI" below.
- Uploading/sharing the trace anywhere. `--open` only ever talks to
  `127.0.0.1`; see "Privacy" under Risks.

## Approach: local httpd + the hosted viewer's URL-loading scheme

Both Firefox Profiler and Perfetto document (and their own official tooling
uses) the same trick: run a tiny local HTTP server, then point the hosted
viewer at it via a URL parameter. Confirmed against two real, maintained
precedents rather than just the docs:

- **Firefox Profiler**: `samply` (Mozilla's own Rust sampling profiler,
  crates.io) does exactly this for its `samply load` command — starts a
  local server, opens `https://profiler.firefox.com/from-url/<encoded local
  url>`, blocks in the foreground serving until Ctrl-C.
- **Perfetto**: `google/perfetto`'s own `tools/open_trace_in_ui` script does
  the same thing — local server on `127.0.0.1`, opens
  `https://ui.perfetto.dev/#!/?url=<encoded local url>&referrer=...`. (Their
  script serves until the first successful fetch then exits; see the
  Ctrl-C-for-both decision below.)

Both confirm something their own docs don't state outright: a plain
`http://127.0.0.1:<port>` URL fetches fine from the `https://`-hosted viewer.
Neither the "must be HTTPS" language in Firefox Profiler's
`docs-developer/loading-in-profiles.md` nor Perfetto's deep-linking docs are
enforced against localhost in practice — browsers exempt `127.0.0.1`/
`localhost` from mixed-content blocking. So no self-signed cert, no
bootstrap HTML page, no `postMessage` handshake — just a server and a CORS
header.

### Rejected: locally-hosted UI

Investigated whether cq could open a fully local copy of the Firefox
Profiler web app instead of the hosted `profiler.firefox.com` (avoiding any
dependency on Mozilla's hosting). Ruled out:

- `@firefox-devtools/profiler-cli` (npm, bins `profiler-cli`/`pq`) is a
  terminal query tool only (confirmed via its own bin surface: `load`,
  `thread samples`, `marker info`, etc.) — no browser UI.
- The `firefox-devtools/profiler` repo has no build artifact anywhere:
  no release assets (`gh api repos/firefox-devtools/profiler/releases` shows
  only source-only `profiler-cli-*` tags), no built-static branch (the
  `production` branch is source, deployed via Netlify at build time, not a
  committed `dist/`). Running the UI locally means cloning the repo and
  running `yarn build-prod && yarn serve-static` — a real Node/yarn/build
  dependency, disproportionate for a CLI flag meant to work for anyone who
  installs cq.
- No equivalent search turned up an npx-runnable UI server either; same
  conclusion.

Perfetto's UI has no npx-runnable local-serving distribution either, but
this was moot once the local-httpd-plus-hosted-viewer approach was settled
as the shared mechanism for both formats.

## CLI surface

- New `--open` boolean flag on `cq trace`.
- `--open` implies `--format firefox-profiler` when `--format` is omitted.
- `--open` with `--format waterfall`, or with `--json`, is a validation
  error (matching the `Error:` / `Valid ...` / `Hint:` convention already
  used elsewhere in `commands/trace.rs`, e.g. `try_parse_bound`):

  ```
  Error: --open is not supported with --format waterfall
  Valid formats for --open: firefox-profiler, perfetto
  ```

- `--open --format firefox-profiler` and `--open --format perfetto` both
  work, sharing the serving/browser-launch plumbing below.
- `--from`/`--to` windowing is unaffected — `--open` only changes how the
  already-windowed trace gets to the browser.

## Data flow / module changes

New `src/trace/open_browser.rs`, sibling to `perfetto.rs`/
`firefox_profiler.rs`. It's a third concern (network I/O + process spawn),
not formatting and not SQL, so it doesn't belong in either renderer or in
`commands/trace.rs`:

```rust
pub fn serve_and_open(
    json_body: String,
    cors_origin: &str,
    make_browser_url: impl FnOnce(&str) -> String,
) -> Result<()> {
    let server = tiny_http::Server::http("127.0.0.1:0")?;
    let port = server.server_addr().to_ip().unwrap().port();
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
    for request in server.incoming_requests() {
        let response = tiny_http::Response::from_string(json_body.clone())
            .with_header(content_type_json())
            .with_header(cors_header(cors_origin))
            .with_header(no_cache_header());
        let _ = request.respond(response);
    }
    Ok(())
}
```

`firefox_profiler.rs` and `perfetto.rs` each already factor their JSON
construction out of `emit()` for testing (`build_profile`, `build_events`);
add a sibling that serializes that value to a `String` without printing, for
`--open` to serve. `commands/trace.rs` branches on `--open`:

- **firefox-profiler**: `open_browser::serve_and_open(profile_json,
  "https://profiler.firefox.com", |local_url|
  format!("https://profiler.firefox.com/from-url/{}",
  percent_encode(local_url)))`
- **perfetto**: `open_browser::serve_and_open(events_json,
  "https://ui.perfetto.dev", |local_url|
  format!("https://ui.perfetto.dev/#!/?url={}&referrer=cq",
  percent_encode(local_url)))`

`percent_encode` is a small hand-rolled helper, not a new dependency — the
only input it ever sees is `http://127.0.0.1:<port>/trace.json`, a fixed,
known character set (only `:` and `/` need encoding).

### Server lifetime: Ctrl-C for both formats

`samply` blocks until Ctrl-C; Perfetto's own `open_trace_in_ui` serves until
the first successful fetch, then exits. Explicitly decided to diverge from
`open_trace_in_ui`'s lifetime and use Ctrl-C for both formats, so cq's
`--open` behaves identically regardless of which viewer it's pointed at —
one lifetime rule for users to remember, not two.

### Port

OS-assigned ephemeral (`127.0.0.1:0`), read back after bind, for both
formats. `open_trace_in_ui` hardcodes port 9001 (reusing Perfetto's
trace_processor RPC port by convention), but nothing in Perfetto's docs or
that script suggests the port is enforced by a browser-side CSP — it reads
as tidiness on the script author's part, not a requirement. Ephemeral avoids
port collisions and needs no `--port` flag.

## Default behavior: don't dump JSON onto an interactive terminal

Independent of `--open`, but found while designing it: today `--format
firefox-profiler`/`--format perfetto` always `println!`, even when stdout is
a live terminal with no redirect. cq already has a precedent for
terminal-aware output (`docs/cli-ux-conventions.md`'s `let wide = cli.wide
|| !std::io::stdout().is_terminal();`), so this follows the same shape:

- `!io::stdout().is_terminal()` (piped or redirected): unchanged — print the
  JSON to stdout exactly like today. Scripts and `> file.json` redirects
  keep working.
- `io::stdout().is_terminal()` (interactive, no redirect, and `--open` not
  given): write the JSON to a deterministic path under
  `std::env::temp_dir()` — `cq-trace-<session_id prefix>-<format>.json` —
  instead of printing it, and tell the user where it landed on stderr, e.g.:

  ```
  Wrote firefox-profiler trace to /tmp/cq-trace-a1b2c3d4-firefox-profiler.json
  Open it at https://profiler.firefox.com, or re-run with --open.
  ```

  Deterministic per session+format, overwritten on each run — no new
  dependency (`std::env::temp_dir()` respects `$TMPDIR`), and re-running the
  same trace refreshes the same file instead of littering the tmp dir with a
  new copy every time.
- `--open` skips this decision entirely — it never touches stdout, terminal
  or not; see above.

## New dependencies

- `tiny_http` — minimal HTTP server. Serving one static JSON blob on one
  route with a couple of headers doesn't need more than this.
- `open` — cross-platform "launch the OS default browser." No hand-rolled
  per-OS `Command::new("open"/"xdg-open"/"start")` needed; this is exactly
  what the crate is for, and it's what `samply` itself uses.

Neither existing dependency (`duckdb`, `clap`, `serde_json`, etc.) covers
either concern.

## Risks

**Privacy.** The trace JSON never leaves the machine over the network —
`serve_and_open` only ever binds `127.0.0.1`, and the hosted viewer's own JS
is what fetches from it, client-side, in the user's own browser. Only the
viewer's static assets (JS/CSS) come from the hosted origin; the trace bytes
stay local unless the user explicitly clicks "Share"/"Upload" inside the
viewer's own UI. This is the same trust model `samply` and Perfetto's own
`open_trace_in_ui` already ship. Worth stating explicitly because the prior
`2026-09-11-firefox-profiler-native-emitter-design.md` doc's Testing section
flagged hosted-page verification as undesirable "since a real trace holds
verbatim tool inputs" — that caution was about *sending* a real trace to
verify against the hosted page during that design's testing, not about this
mechanism's actual data flow, which never transmits the trace anywhere.

**Two upstream services, indirectly depended on.** `--open` breaks if
`profiler.firefox.com` or `ui.perfetto.dev` change their URL-loading scheme
or go down. Same exposure `samply` and `open_trace_in_ui` already accept;
not something cq can mitigate beyond noticing if it breaks.

## Testing

- Unit tests on `open_browser::serve_and_open`'s actual serving behavior:
  spawn it in a thread, hit it with a raw `TcpStream` GET, assert the body,
  `Content-Type`, and `Access-Control-Allow-Origin` headers, then
  `server.unblock()` to end the loop and join the thread. This covers the
  real request/response logic without opening a browser or blocking a test
  process forever.
- Unit tests on the two `make_browser_url` closures (or their extracted
  equivalents) and on `percent_encode`, asserting the exact URL strings
  built for a given local URL.
- The CLI-level `--open` flag itself (real infinite Ctrl-C loop + real
  browser launch) is not covered by automated tests — called out here
  explicitly rather than silently skipped. Verified manually: run `cq trace
  --session <id> --open --format firefox-profiler` and `--format perfetto`
  against a real session, confirm each opens with the trace already loaded.
- Integration test (`tests/integration_test.rs`, via `assert_cmd`) for the
  terminal-detection default: piping `cq trace --format perfetto`'s stdout
  still yields the JSON on stdout unchanged (this is already how the
  existing tests capture output, so it's the regression to guard). A direct
  test of the "interactive terminal writes a file" branch needs a real PTY,
  which `assert_cmd` doesn't give you — cover that branch with a unit test
  that injects the `is_terminal()` decision (a trait or a plain bool
  parameter) rather than reading the real stdout, and verify the temp-file
  path/contents manually once.

## Implementation order

1. Terminal-detection default: route `firefox_profiler`/`perfetto` output
   through an `is_terminal()` check in `commands/trace.rs`, writing to
   `std::env::temp_dir()` instead of `println!` when interactive. Unit +
   integration tests per above. This lands independently of `--open` and is
   worth shipping even before it.
2. Add `tiny_http` and `open` to `Cargo.toml`.
3. `src/trace/open_browser.rs`: `serve_and_open`, `percent_encode`, header
   helpers, unit tests per above.
4. Non-printing JSON-string variants of `firefox_profiler::emit` and
   `perfetto::emit` (shared by both the tmp-file path and `--open`).
5. `--open` flag on `cq trace` (`main.rs`), validation in
   `commands/trace.rs`, wiring to `open_browser::serve_and_open` for both
   formats.
6. Manual verification against a real session, both formats, both the
   tmp-file default and `--open`.
7. Update `docs/cli-ux-conventions.md`'s docs-sync table and the README flag
   reference for `--open`, per `CLAUDE.md`'s "Keeping docs in sync"
   checklist.
