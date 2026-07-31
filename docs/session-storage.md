# Session storage reference

How Claude Code lays out transcripts on disk, what's inside them, and the places where the format will surprise you. This is the reference cq's indexer is written against, so if you're touching `indexer.rs`, `claude_provider.rs`, or `views.rs`, start here.

Claude Code owns this format and doesn't document it as a public contract, so treat everything below as observed behavior that can change under you. Where a claim was verified against a real corpus or against DuckDB directly, it says so.

## Where files live

A transcript root is a `projects/` directory. Inside it, one directory per project, and inside that, one JSONL file per session:

```
~/.claude/projects/
  -Users-alice-myproject/
    8274321f-7030-4ac2-a5be-84ffe2d8dbbb.jsonl        # the main-loop session
    8274321f-7030-4ac2-a5be-84ffe2d8dbbb/
      subagents/
        agent-ac51870c0d1f311f7.jsonl                 # one subagent
        agent-ac51870c0d1f311f7.meta.json             # its metadata sidecar
        workflows/
          wf_372d663d-79d/
            agent-a66c6983798bf06ed.jsonl             # a workflow's agent
            journal.jsonl                             # resume ledger, NOT a transcript
```

The session UUID does double duty: it names the `.jsonl` file and it names the sibling directory holding that session's subagents. Subagent files are `agent-<hash>.jsonl`, and each one may have an `agent-<hash>.meta.json` next to it carrying `agentType`. Workflow agents nest one level deeper under `subagents/workflows/wf_<id>/`, which is where cq gets `workflow_id` from (`WORKFLOW_ID_EXPR` in `views.rs` regexes it out of the path).

`journal.jsonl` is workflow resume bookkeeping, not a transcript. `is_indexable_jsonl` in `indexer.rs` excludes it by filename, so the exclusion is global rather than scoped to workflow dirs.

### More than one root

cq calls a root a *source*. There are two kinds:

| Source | Location |
|--------|----------|
| `main` | `$CQ_PROJECTS_DIR`, else `~/.claude/projects` |
| one per cenv env | `$CENV_BASE/<env>/projects`, else `~/.local/share/cenv/<env>/projects` |

cenv discovery only descends one level, so it finds `<base>/<env>/projects` and never wanders into an env's `plugins/` cache. An env without a `projects/` directory is skipped. Sources are sorted by name so ordering is deterministic.

## The project directory name is lossy

This is the gotcha most likely to bite you, because the decoded value looks plausible while being wrong.

Encoding replaces both `/` and `.` with `-`:

```rust
path.replace(['/', '.'], "-")
```

So `/Users/josh.nichols/pickleton` becomes `-Users-josh-nichols-pickleton`, and there is no way to tell from the name alone which of those hyphens was a slash, which was a dot, and which was a literal hyphen in a directory name. Decoding is a blind `replace('-', '/')`, which turns that example back into `/Users/josh/nichols/pickleton`. Wrong, and wrong in a way that still looks like a real path.

cq works around it by capturing the authoritative `cwd` from inside the transcript at index time, storing it in `file_registry`, and only falling back to decoding the directory name when that's missing:

```sql
COALESCE(
  (SELECT fr.cwd FROM file_registry fr WHERE fr.file_path = source_file),
  '/' || replace(regexp_extract(source_file, '.*/([^/]+)/[^/]+$', 1)[2:], '-', '/')
)
```

If you're writing anything new that needs a project path, use the registry's `cwd`. Reach for the fallback only when you have no records to read it from, and expect it to mangle any path containing a dot or a hyphen.

## What's actually in the file

Each line is one JSON object with a `type`. It's a mixed event log, not a message list, and the message types are a minority of the volume. Counted across a real 479-file corpus:

| `type` | Rows | What it is |
|--------|------|------------|
| `assistant` | 28081 | assistant turn |
| `user` | 15079 | user turn, including tool results |
| `attachment` | 4949 | out-of-band attachments, including hook output |
| `last-prompt` | 3621 | bookkeeping |
| `mode` | 3189 | bookkeeping |
| `permission-mode` | 3094 | bookkeeping |
| `ai-title` | 2872 | generated session title |
| `system` | 2299 | system-injected content |
| `file-history-snapshot` | 1552 | file state tracking |
| `queue-operation` | 735 | bookkeeping |
| `pr-link` | 496 | bookkeeping |
| `file-history-delta` | 257 | file state tracking |
| `agent-name` | 63 | bookkeeping |

Two consequences. First, `SELECT count(*) FROM raw_records` is not a message count and never was. Second, cq's five views model only a slice of this, so a `type` you care about may be sitting in `raw_records` with no view over it. Check before assuming a view covers it.

### Where the views get their rows

- `messages` reads `user` and `assistant` records.
- `sessions` aggregates `messages`, and most of its columns are computed `FILTER (WHERE NOT is_sidechain)` so subagent turns don't inflate a session's counts. `subagent_count` is the deliberate exception, counting `DISTINCT agent_id` across everything.
- `tool_calls` reads `tool_use` blocks out of `assistant` records.
- `tool_results` reads `tool_result` blocks out of `user` records.
- `hook_events` reads `attachment` records where `attachment.type = 'hook_success'`, pulling `attachment.hookEvent` and `attachment.hookName`.

The `advisor()` case breaks the usual call/result symmetry: it uses `server_tool_use` and `advisor_tool_result` blocks, and both live inside `assistant` records, so the result is *not* in the following `user` record the way a normal tool result is. `views.rs` special-cases both sides.

## Identity and ordering

Records carry `uuid`, `parentUuid`, `sessionId`, `isSidechain`, and `timestamp`. Subagent rows carry the parent's `sessionId`, so a session is a set of files rather than a single file. Any query that assumes one file per session will undercount: in a real 479-file corpus, 132 distinct `agent_id`s roll up into 44 parent sessions.

`parent_uuid` is projected into the views but nothing joins on it, and `--timeline` orders by SQL rather than walking the chain. So a missing record leaves a dangling `parentUuid` without breaking any traversal cq actually performs. Worth knowing before you write the first thing that does walk it.

## Gotchas

### Unpaired UTF-16 surrogate escapes

A transcript line can contain `\uD83D` with no matching low surrogate. That's invalid JSON per spec, because a supplementary-plane escape has to be a complete high/low pair, but lenient parsers (Python's `json`, `jq`) accept it and hand back a string containing an isolated surrogate code point. Stricter parsers reject the line outright.

It shows up when a large captured tool output gets truncated at a fixed byte length that lands mid-character, leaving half a surrogate pair behind when the truncated string is later JSON-encoded. Emoji are the usual culprit.

Rare but real: 0 of 479 files in one local corpus, and enough to break indexing in another.

If you write a parser against these files, decide deliberately whether you're strict or lenient, because the two disagree on real data.

### `ignore_errors=true` nulls the value, it doesn't drop the row

DuckDB-specific, and the behavior is the opposite of what the flag name suggests:

```sql
SELECT json FROM read_json(path, format='newline_delimited', records=false, ignore_errors=true)
```

A line the parser rejects still produces a row. Only its value comes back as SQL `NULL`, so the row count is unaffected. Verified directly against the three-line fixture at `tests/fixtures/unparseable_record.jsonl`: three rows out, one with a `NULL` `json`.

Since `raw_records.json` is `NOT NULL`, inserting that result crashes the whole file's index with a constraint violation. `index_files` stages each file's rows in a temp table, drops the `NULL` ones, warns to stderr with a count and path, and inserts the rest. A file with one bad line indexes minus that line.

Dropping the `NOT NULL` constraint is not the smaller fix it appears to be. Every view reads `json` without expecting `NULL`, so it moves the crash from index time to query time.

### Large tool output lives outside the transcript

A record can carry `toolUseResult.persistedOutputPath` and `persistedOutputSize` instead of the full output, pointing at a sidecar file (74 records in a real 479-file corpus, so it's uncommon but not exotic). The transcript alone is not the complete record of what a tool returned, and those sidecars have their own lifetime. Anything reconstructing full tool output has to follow the pointer and handle it being gone.

This is also the mechanism behind the surrogate gotcha above: truncating a large output to a fixed byte length is what leaves half a surrogate pair in the record that stays behind.

### mtime and size are the change signal

`file_registry` stores `mtime_ns` and `file_size`, and the indexer re-parses a file only when one of them moves. Sessions are appended to as they run, so this works, but it means any in-place rewrite that preserves both is invisible to sync. `--reindex` is the escape hatch.

The Auto fast-path takes a *recursive* max mtime so a new file deep under `subagents/` still registers as a change.

### A fully unparseable file indexes as empty

If every line is rejected, the file registers in `file_registry` with zero records and a `NULL` `cwd`. No crash, and the warning fires. But mtime and size are now recorded, so subsequent syncs skip it and it stays empty until `--reindex`. Verified by building that fixture and running it.

## Related

- `CLAUDE.md`, "Key patterns", for how the indexer and views fit together.
- `CONTEXT.md` for the Harness / Provider / Source glossary.
- `docs/opencode-source-findings.md` for how a second harness's storage compares.
