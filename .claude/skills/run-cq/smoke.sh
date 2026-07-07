#!/usr/bin/env bash
# Smoke driver for cq — builds the release binary if needed, then drives every
# subcommand against an ISOLATED fresh cache so it never touches the user's real
# ~/.cache/cq/index.duckdb. Asserts exit codes (including the error path).
#
# Reads real transcripts from ~/.claude/projects (or $CQ_PROJECTS_DIR). Works
# even with zero session data — commands print "No results." and exit 0.
#
# Usage:  .claude/skills/run-cq/smoke.sh
# Run from the repo root. Exit 0 = all checks passed.
set -uo pipefail

cd "$(git rev-parse --show-toplevel)" || exit 1
BIN="$PWD/target/release/cq"

# Isolated cache so the smoke run is hermetic and doesn't disturb the real index.
CACHE="$(mktemp -d "${TMPDIR:-/tmp}/cq-smoke.XXXXXX")"
export CQ_CACHE_DIR="$CACHE"
trap 'rm -rf "$CACHE"' EXIT

fail=0
pass() { printf '  ok   %s\n' "$1"; }
bad()  { printf '  FAIL %s\n' "$1"; fail=1; }

# check <label> <expected-exit> -- <command...>
check() {
  local label=$1 want=$2; shift 3
  "$@" >/tmp/cq-smoke.out 2>&1
  local got=$?
  if [ "$got" -eq "$want" ]; then pass "$label (exit $got)"
  else bad "$label (want exit $want, got $got)"; sed 's/^/       | /' /tmp/cq-smoke.out; fi
}

echo "== build =="
if [ ! -x "$BIN" ]; then
  echo "release binary missing — building (DuckDB compiles from source, ~4 min)"
  cargo build --release || { echo "build failed"; exit 1; }
fi
"$BIN" --version || exit 1

echo "== drive subcommands (isolated cache: $CACHE) =="
check "version"          0 -- "$BIN" --version
check "help"             0 -- "$BIN" --help
check "tools summary"    0 -- "$BIN" tools --all --limit 5
check "sessions"         0 -- "$BIN" sessions --all --limit 3
check "messages"         0 -- "$BIN" messages --all --limit 3
check "projects"         0 -- "$BIN" projects --all
check "raw sql"          0 -- "$BIN" sql "SELECT COUNT(*) AS n FROM messages"
check "schema --examples" 0 -- "$BIN" schema --examples
check "json output"      0 -- "$BIN" sessions --all --limit 1 --json
# Forgiveness path: an invalid enum value must exit non-zero with a helpful error.
check "error: bad --count-by" 1 -- "$BIN" tools --count-by bogus

echo "== result =="
if [ "$fail" -eq 0 ]; then echo "ALL CHECKS PASSED"; else echo "SOME CHECKS FAILED"; fi
exit "$fail"
