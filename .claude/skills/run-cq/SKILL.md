---
name: run-cq
description: Build, run, test, and smoke-drive cq — the SQL-over-Claude-Code-transcripts CLI (Rust + DuckDB). Use when asked to build cq, run it, try a subcommand, verify a change works end-to-end, or run its tests.
---

`cq` is a Rust CLI that indexes Claude Code JSONL session transcripts into a
DuckDB cache and queries them with SQL. No server, no UI — you drive it by
running subcommands and checking output + exit codes.

**Agent path:** run `.claude/skills/run-cq/smoke.sh` — it builds the release
binary if missing, then exercises every subcommand against an isolated cache and
asserts exit codes (including the error path). All paths below are relative to
the repo root.

Verified on macOS (Darwin, arm64). Linux is a first-class target too (CI ships
Linux x86_64 + arm64 builds), but the commands below were run on macOS.

## Prerequisites

- **Rust toolchain** (`cargo`, `rustc`). Verified with 1.96.
- **A C++ compiler.** The `duckdb` crate is pinned with the `bundled` feature, so
  it compiles DuckDB from C++ source on first build. macOS: Xcode Command Line
  Tools (`xcode-select --install`) provide `clang`. Ubuntu: `sudo apt-get install -y build-essential`.

No system DuckDB, no other services. The tool reads `~/.claude/projects/` (or
`$CQ_PROJECTS_DIR`) and writes a cache at `~/.cache/cq/index.duckdb` (or
`$CQ_CACHE_DIR`).

## Build

```bash
cargo build --release
# → target/release/cq  (first build ~4 min: DuckDB compiles from source; later builds are seconds)
./target/release/cq --version
# → cq 0.2.0
```

## Run (agent path)

The smoke driver is the fastest way to confirm a working binary. It uses an
isolated `CQ_CACHE_DIR` under `$TMPDIR`, so it never touches your real cache:

```bash
.claude/skills/run-cq/smoke.sh
# builds if needed, then:
#   ok   version (exit 0)
#   ok   tools summary (exit 0)
#   ...
#   ok   error: bad --count-by (exit 1)
#   ALL CHECKS PASSED
```

Exit 0 = every check passed. It drives: `--version`, `--help`, `tools`,
`sessions`, `messages`, `projects`, `sql`, `schema --examples`, `--json`
output, and one deliberate error case that must exit 1.

To poke it by hand (these all ran clean on real session data):

```bash
BIN=./target/release/cq
$BIN tools --all --limit 10                          # bar-chart summary of tool usage
$BIN sessions --all --limit 3                        # recent sessions
$BIN sql "SELECT COUNT(*) AS n FROM messages"        # raw SQL over the four views
$BIN schema --examples                               # view schema + example queries (no DB needed)
$BIN sessions --all --limit 1 --json                 # machine-readable output
```

`--all` disables auto-scoping to the current directory. Without it, queries scope
to the project matching your cwd (from the repo root you'll get cq's own
sessions, which may be sparse).

## Run (human path)

Same binary, no wrapper: `cargo run -- <subcommand>`, e.g.
`cargo run -- tools --limit 5`. See `README.md` and `docs/` for the tour.

## Test

```bash
cargo test
# → 5 suites: lib (57), cache (11), integration (95), views (32), + 0 doc-tests. All pass in ~20s.
```

Fixtures are handcrafted JSONL under `tests/fixtures/`; tests don't need your
real `~/.claude/projects/` data.

## Gotchas

- **`--all` matters for a visible result.** cq defaults to auto-scoping to the
  current directory's project. Run from the repo root without `--all` and you
  only see cq's own sessions. Use `--all` (or `--project <name>`) to see
  everything. An over-scoped query prints `No results.` plus an `Active filters:`
  hint — that's exit 0, not a failure.
- **First build is slow, and it's the C++, not your code.** `bundled` DuckDB
  compiles from source (~4 min). If a build hangs on `libduckdb-sys`, it's
  compiling, not stuck.
- **`--reindex` waits for the file lock; `--no-reindex` skips sync entirely.**
  Default (`Auto`) does an mtime fast-path and skips if the lock is busy, so a
  concurrent cq won't block you — you just read slightly stale data.
- **Writing under `.claude/skills/` may hit a sandbox deny rule.** If you edit
  this skill or `smoke.sh` and the write is refused, that's the tool sandbox, not
  a permissions bug — `.claude/skills` is on the deny list. Manage with `/sandbox`.

## Troubleshooting

- **`Error: Unknown count-by column '<x>' for tools` (exit 1)** — expected for an
  invalid `--count-by`. Valid columns: `name`, `session`, `project`. The smoke
  driver relies on this exiting non-zero.
- **Fresh/empty `CQ_CACHE_DIR`** — first run prints `Synced N new files` and
  builds `index.duckdb` + `index.lock` from scratch (a couple seconds for ~500
  files). Not an error.
