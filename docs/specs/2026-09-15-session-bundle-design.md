# Session bundle export (`cq bundle`)

Status: approved (design)
Date: 2026-09-15

## Problem

There's no way to get one session's transcript out of `~/.claude/projects/` as a
single portable artifact. Today that means hand-rolling a shell script that
finds the right `.jsonl`, remembers subagents live in a sibling `subagents/`
dir, and generally reinvents file discovery cq already has. That's exactly
what happened outside cq (a shell script in gusto/claude-code) and it grew out
of hand quickly, because the on-disk shape has more edges than it looks like
at first: nested subagents, workflow agents nested deeper still, and tool
results that point *outside* the transcript entirely via
`persistedOutputPath`.

`cq bundle --session <id>` packages one session into a zip using cq's
existing file discovery, so none of that has to be re-derived by hand.

## Use cases

Not designed around one narrow use case — three come up:

- **Share** a session with someone (Slack, a GitHub issue, Anthropic support)
  for debugging.
- **Archive** a session locally before it rotates out of
  `~/.claude/projects/` or a cenv env gets torn down.
- **Feed another tool** — re-import elsewhere, or hand off the raw JSONL to a
  different analysis pipeline.

All three want the same artifact: the session's own files, faithfully
reproduced, not a lossy derived view. That's why the default bundle is raw
JSONL, not a rendered HTML transcript or a CSV dump of cq's parsed rows —
those are convenience copies that can drift from the source; the JSONL is the
source.

**Security note, not a design requirement:** a bundle is a local zip, no
riskier than the JSONL already sitting on disk. But session transcripts can
contain full tool outputs, including file contents, command output, and MCP
results, so if a bundle is heading somewhere external (Anthropic support, a
public issue tracker), treat that as a deliberate decision at send time, same
as sharing the raw file would be. This design does not add redaction; that's
a separate feature if it's ever needed.

Multi-session bundling (`--since`, `--project`, a query producing N sessions
in one zip) is an explicit non-goal for v1. The shape is compatible with
adding it later (one subfolder per session instead of one session's files at
the zip root), but v1 covers exactly one session per invocation, matching how
`cq trace` already works.

## On-disk facts (from `docs/session-storage.md`)

- A session is `<project>/<uuid>.jsonl` plus a sibling `<uuid>/subagents/`
  directory holding that session's subagents.
- Subagent files are `agent-<hash>.jsonl`, each optionally paired with an
  `agent-<hash>.meta.json` sidecar carrying `agentType`.
- Workflow agents nest one level deeper:
  `subagents/workflows/wf_<id>/agent-<hash>.jsonl`.
- `journal.jsonl` can appear alongside these — it's a workflow ledger, not
  part of the Claude session, and `discover_files`/`indexer.rs` already
  exclude it. `cq bundle` inherits that exclusion by reusing the same
  discovery code.
- Some `tool_result` records carry `toolUseResult.persistedOutputPath` (and
  `persistedOutputSize`) instead of inlining output — about 74 of 479 files
  in a real corpus, per `docs/session-storage.md`. The transcript alone is
  not the complete record of what a tool returned in those cases, and the
  sidecar file has its own lifetime — it may already be gone by the time
  someone bundles the session.

## Command surface

```
cq bundle --session <id> [-o|--output <path>]
```

- `--session` is required. Same rationale as `cq trace`: a bundle is one
  session's shape, there's nothing to bundle across sessions, so this is
  required rather than defaulted. Unknown/non-matching id uses the existing
  `print_session_not_found` path.
- `-o`/`--output` is optional. Default: `./session-<id>.zip` in the caller's
  cwd (`<id>` is the full session id cq resolved, not the possibly-partial
  prefix the caller typed). `cq bundle` is the first cq command whose whole
  point is writing a file rather than printing to stdout, so unlike every
  other command it needs a real default destination rather than "print
  unless redirected."
- Session id resolution and file discovery reuse
  `ClaudeProvider::discover_files` + `QueryScope`, the same machinery
  `cq trace`/`cq sessions --session` already use. No new discovery logic.

## Zip layout

```
session-<id>.zip
  main.jsonl                                  # the top-level <uuid>.jsonl
  subagents/
    agent-<hash>.jsonl
    agent-<hash>.meta.json
    workflows/wf_<id>/agent-<hash2>.jsonl      # same relative shape as on disk
  sidecars/
    <persistedOutputPath basename>             # best-effort, flat
  manifest.json
```

`main.jsonl` is a rename for clarity in the archive; everything else keeps
its on-disk relative shape so nesting (which subagent belongs to which
workflow) survives unzip.

### manifest.json

```json
{
  "session_id": "abc123de-...",
  "project": "pickleton",
  "cwd": "/Users/josh.nichols/pickleton",
  "harness": "claude",
  "source": "main",
  "created_at": "2026-09-10T14:02:11Z",
  "last_message_at": "2026-09-10T15:40:03Z",
  "files": ["main.jsonl", "subagents/agent-xxx.jsonl"],
  "sidecars_included": ["sidecars/abc.output"],
  "sidecars_missing": ["def-hash"],
  "cq_version": "0.x.y"
}
```

Session-level fields (`project`, `cwd`, `harness`, `source`,
`created_at`/`last_message_at`) come straight from the `sessions` view — no
new parsing needed to produce them.

## Sidecar handling

`persistedOutputPath` isn't a DuckDB view column today, and this design
doesn't add one. Adding a schema column would couple the indexer to an
export-only feature; instead, `cq bundle` scans the raw JSONL lines it's
already reading off disk (parse each line, check for
`toolUseResult.persistedOutputPath`) and follows any pointer it finds.

- **Found and readable:** copy into `sidecars/`, list under
  `sidecars_included`.
- **Pointer present but file missing:** skip it, print a warning to stderr,
  list under `sidecars_missing`. Never a hard failure — this matches cq's
  existing "stale-but-available beats error" principle (`docs/design-principles.md`).

## Output / progress reporting

Following cq's stderr-for-progress / stdout-for-data convention, though here
there's no "data" in the piped-output sense — the zip *is* the output. So:

- Progress and the final summary go to stderr (consistent with every other
  cq command), e.g.:
  ```
  Wrote ./session-abc123de.zip (3 files, 1 sidecar, 412 KB)
  ```
- A missing sidecar prints its own warning line to stderr as it's skipped.
- `--json` is not planned for v1 — there's no meaningful row-shaped
  alternative to "wrote a zip file," unlike `cq trace --json` which has real
  span rows to fall back to.

## Testing

- Fixture-based integration test extending the existing `tests/fixtures/`
  pattern: a session with a subagent, a nested workflow agent, a
  `journal.jsonl` (must be excluded), and one `persistedOutputPath` pointer
  (one resolving, one dangling) to exercise both sidecar branches.
- Assert on the zip's contents (file list, manifest fields) rather than
  fixed byte output, since zip encoding details aren't the thing under test.
- Unit test for the persistedOutputPath scan against a handcrafted JSONL
  line, independent of the zip-writing path.

## Non-goals (v1)

- Multi-session bundling (`--since`/`--project`/query producing N sessions).
- Any rendered/derived output (HTML transcript, CSV of parsed rows) bundled
  alongside the raw files. If this comes up later, it's an additive flag,
  not a change to the default.
- Redaction/secret-scrubbing of bundle contents.
