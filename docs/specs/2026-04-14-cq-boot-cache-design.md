# cq Boot Cache

**Date:** 2026-04-14
**Status:** Approved

## Problem

cq parses every JSONL session file on every invocation. With 4,441 files across 128 projects, even a release build takes 3.6s before the actual query runs. Most of that time is DuckDB reading and parsing unchanged files.

## Solution

Persist parsed session data in a DuckDB file. On startup, stat the filesystem, diff against a registry of known files, and re-parse only what changed. Drop rows for deleted files. A `--reindex` flag forces a full rebuild.

## Cache Location

`~/.cache/cq/index.duckdb`, discovered via the `dirs` crate's `cache_dir()`. Users can blow it away with `rm -rf ~/.cache/cq` to force a clean start.

## Schema

```sql
CREATE TABLE cache_meta (
    version INTEGER NOT NULL
);

CREATE TABLE file_registry (
    file_path TEXT PRIMARY KEY,
    mtime_ns BIGINT,
    file_size BIGINT,
    cwd TEXT,
    indexed_at TIMESTAMP DEFAULT current_timestamp
);

CREATE TABLE raw_records (
    source_file TEXT,
    json JSON
);
```

`cache_meta` stores a single row with the schema version. On startup, if the version doesn't match the expected constant in the code, cq drops everything and rebuilds. This handles DuckDB upgrades and schema changes without mysterious errors.

`file_registry` tracks every indexed JSONL file with its mtime and size (for change detection) and `cwd` (extracted from the first user message in the session, for accurate project path display).

`raw_records` replaces the current `read_json`-backed view with a persisted table. The `source_file` column links each row back to `file_registry.file_path` so we can surgically delete rows when a file changes or disappears.

### JSON storage and view SQL

The current in-memory approach uses `read_json(..., records=false)` which produces a STRUCT-typed `json` column. Views access fields via struct dot notation: `json.sessionId`, `json.type`, `json.message.content`.

A persistent table can't use STRUCT typing because the schema varies across Claude Code versions and session types. Instead, `raw_records.json` stores opaque `JSON`. All view SQL switches from struct dot notation to `json_extract_string` / `json_extract` calls:

| Before (struct) | After (JSON extract) |
|---|---|
| `json.sessionId` | `json_extract_string(json, '$.sessionId')` |
| `json.type` | `json_extract_string(json, '$.type')` |
| `json.message.content` | `json_extract(json, '$.message.content')` |
| `json.message.model` | `json_extract_string(json, '$.message.model')` |

This is more verbose but schema-agnostic: new fields in future Claude Code versions won't break the cache.

## Boot Sequence

1. Open (or create) the persistent DB at `~/.cache/cq/index.duckdb`.
2. Check `cache_meta.version`. If missing or mismatched, drop all tables and treat as cold start.
3. Stat all JSONL files under the projects directory (always all files, regardless of `--project`/`--session` flags).
4. Diff against `file_registry`:
   - **New files:** not in registry.
   - **Changed files:** mtime or size differs.
   - **Deleted files:** in registry but not on disk.
5. Delete rows from `raw_records` and `file_registry` for changed and deleted files.
6. Parse new and changed files with DuckDB's `read_json`. Insert results into `raw_records`. Extract `cwd` from the first record with a non-null `cwd` field (typically the first user message) and insert into `file_registry`.
7. Register the four derived views (`messages`, `tool_calls`, `tool_results`, `sessions`) on top of `raw_records`.
8. Run the query (with `--project`/`--session` applied as WHERE clauses).

Step 3 always indexes everything. The ~50ms cost to stat 4,441 files is negligible, and a single global cache is simpler than per-project caches. Scoping via `--project` and `--session` applies at query time, not at indexing time.

## Project Path Improvement

The current `PROJECT_EXPR` decodes directory names by replacing `-` with `/`. This is lossy: `/Users/josh.nichols/gt/audience_broadcast/polecats/furiosa` becomes `/Users/josh/nichols/gt/audience/broadcast/polecats/furiosa` because underscores, dots, and other characters are also encoded as dashes.

Session files contain the real working directory in the `cwd` field on message envelopes (the top-level JSON object, not inside `message.content`). The JSON path is `$.cwd`. The cache reads the first record in each file that has a non-null `cwd` and stores it in `file_registry.cwd`.

Views join `raw_records.source_file` against `file_registry` to get the project path via `cwd`. Files with no `cwd` (empty sessions, sessions with only system records) fall back to the old directory-name decode.

## Reindexing

`--reindex` flag, available on any command. Drops both tables and rebuilds from scratch. Composes naturally: `cq tools Skill --reindex`.

## Expected Performance

| Scenario | Time |
|----------|------|
| First run (cold cache) | ~3.6s (same as today, plus write overhead) |
| Warm boot, no changes | ~200ms |
| Warm boot, few new files | ~300ms |
| `--reindex` | ~3.6s |

## Change Detection

mtime (nanosecond precision) plus file size. Both come from a single `stat()` call per file, costing ~50ms total for 4,441 files. This catches normal edits and appends. Edge cases like `cp -p` (preserved mtime with different content) are unlikely for session files that Claude Code writes.

## View Definitions

The four derived views (`messages`, `tool_calls`, `tool_results`, `sessions`) stay as SQL views, not materialized tables. This means:

- View definition changes (filtering logic, new columns) take effect immediately without reindexing.
- Query cost is proportional to the data, not the number of views.
- The only persistent state is `raw_records` and `file_registry`.

## What This Doesn't Change

- CLI flags and command structure stay the same.
- `--project` and `--session` scoping still work (as WHERE clauses on the views).
- `cq sql` still runs arbitrary SQL against the same views.
- JSON output mode is unaffected.
- The provider trait stays, though `discover_files` shifts from "find files to parse" to "find files to diff."

## Known Tradeoffs

**Active sessions get re-parsed every invocation.** Session JSONL files grow as messages are appended. The mtime+size diff correctly detects changes, but that means querying data from an active session always re-parses the whole file. This is fine for typical session sizes. A future optimization could track byte offsets for append-only detection, but that's not worth the complexity now.

**Concurrent access.** DuckDB's file-based storage handles concurrent readers. Concurrent writers (e.g., two `--reindex` invocations) will block on DuckDB's write lock. A `--reindex` while another query is running will briefly leave the DB empty. This is acceptable for a single-user CLI tool.

**Parse errors.** Files that fail to parse (truncated writes, corrupt JSON) get registered in `file_registry` anyway (to avoid retrying every invocation) but log a warning to stderr. This matches the current `ignore_errors=true` behavior in `read_json`.
