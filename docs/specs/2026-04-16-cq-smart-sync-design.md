# CQ Smart Sync Design

## Problem

CQ re-indexes the DuckDB database on every command. This is fast enough in isolation, but creates race conditions when multiple processes run CQ concurrently (parallel Claude sessions, citizens, manual + background usage). DuckDB's write lock means one process wins and the other crashes.

## Design Principles

**CQ is a query tool, not a monitoring tool.** It answers questions about sessions that have happened. Freshness means "has this been indexed," not "is this millisecond-current."

**Default is global, narrow explicitly.** Most CQ questions span projects: "what sessions did I run today", "which skills get invoked most", "find that session where I was debugging X". Defaulting to one project would make these queries silently incomplete. Scoping is available when you want it.

**Three query scopes:**

| Scope | When | Example |
|-------|------|---------|
| **Global** | Questions across all work | `cq sessions --since today`, `cq sql "SELECT ..."` |
| **Project** | Focused on a specific codebase | `cq sessions --project ~/pickleton` |
| **Session** | Looking at one session's details | `cq messages --session <id>` |

**Sync scope follows query scope.** If you're querying globally, sync checks all projects. Querying a specific project? Only that project needs to be fresh. Querying a specific session? Only that file matters.

**Stale-but-available beats error.** If another process holds the write lock, serve cached data and tell the user. A slightly stale result is infinitely more useful than a crash.

**Explicit always wins.** `--reindex` and `--no-reindex` override all automatic behavior.

## Three Sync Modes

| Flag | Behavior |
|------|----------|
| (default) | Auto sync: mtime check, try-lock, skip if busy |
| `--reindex` | Force sync: full scan, wait for lock |
| `--no-reindex` | No sync: use cached data, skip mtime check entirely |

### Default (auto sync)

1. Read `last_sync_at` from `cache_meta`
2. Stat project directories (scoped to query scope), find max mtime
3. If max mtime <= `last_sync_at`, skip sync. Done.
4. If max mtime > `last_sync_at`, try-lock `~/.cache/cq/index.lock`
   - Lock acquired: run full sync, update `last_sync_at`, release lock
   - Lock busy: skip sync, log to stderr, use cached data

### `--reindex` (force sync)

1. Skip mtime check entirely
2. Lock `~/.cache/cq/index.lock` with 5s timeout
   - Lock acquired: full scan and sync of everything, update `last_sync_at`
   - Lock timeout: error with actionable message

### `--no-reindex` (skip sync)

1. Skip everything. No mtime check, no lock, no sync.
2. Pure read against whatever's cached.
3. Great for scripts that warmed the cache with `--reindex` up front, or citizens doing repeated queries in a loop.

## Fast-Path Mtime Check

**What to check:** Stat the subdirectories under `~/.claude/projects/`. Each subdirectory corresponds to a project (encoded path). Collect the max mtime across all of them (or a scoped subset if `--project` is specified).

**Where to store last sync time:** New `last_sync_at` column in the existing `cache_meta` table. Nanosecond timestamp, updated as the final step of a successful sync.

**Why directory mtime works:** On macOS and Linux, a directory's mtime updates when files are added, removed, or renamed within it. JSONL files live at `~/.claude/projects/<encoded-path>/*.jsonl`, so new sessions or new session files will bump the parent directory mtime.

**Why nanoseconds:** JSONL files can be written rapidly during active sessions. Second granularity could miss updates within the same second as the last sync.

**Edge cases:**

| Case | Behavior |
|------|----------|
| First run (no database) | `last_sync_at` doesn't exist, always syncs |
| Clock skew / mtime weirdness | Worst case: unnecessary sync (same as current behavior) |
| File appended to (active session) | Directory mtime updates, sync triggers |
| Schema version bump (database rebuilt) | `last_sync_at` resets, forces full sync |
| Sync crashes partway through | Timestamp stays old, next run re-syncs. Self-healing. |

## File Lock for Write Contention

**Mechanism:** Exclusive file lock on `~/.cache/cq/index.lock` using OS-level `flock` (Rust's `fs2` crate).

**Granularity:** One lock for the whole database. DuckDB is a single file, so per-project locks would create false confidence. Keep it simple.

**Two lock behaviors:**

| Situation | Lock behavior |
|-----------|---------------|
| Auto sync (default) | `try_lock()`: if busy, skip sync and use cached |
| Force sync (`--reindex`) | `lock()` with 5s timeout: wait, then error if still locked |

**Lock lifecycle:**

```
sync() called
  -> fast mtime check (no lock needed, read-only)
  -> changes detected?
    -> no: return, no lock ever acquired
    -> yes: try_lock(index.lock)
      -> acquired: do the sync, release lock
      -> not acquired + auto: skip sync, log to stderr, return
      -> not acquired + reindex: wait up to 5s, then error
```

## Stderr Feedback

| Scenario | Message |
|----------|---------|
| Auto sync, got lock, synced | `synced 3 new sessions` |
| Auto sync, lock busy | `index busy, using cached data (re-run with --reindex to force)` |
| Auto sync, nothing changed | (silence) |
| `--reindex`, got lock | `reindexing... synced 47 sessions` |
| `--reindex`, lock timeout | `error: index locked by another process after 5s, try again shortly` |
| `--no-reindex` | (silence) |

Quiet by default. Only speak up when something noteworthy happened.

## Sync Scope

`indexer::sync()` currently takes `projects_dir` (the root). It would instead accept a `SyncScope`:

- `SyncScope::All`: scan all project directories (current behavior, default for unscoped queries)
- `SyncScope::Project(path)`: scan one project subdirectory (used with `--project`)
- `SyncScope::File(path)`: check one specific JSONL file (used when session ID maps to a known file)

The fast-path mtime check applies within whichever scope is active.

**How flags map to scope:**

| Query | Sync scope |
|-------|------------|
| `cq sessions` (no filters) | `SyncScope::All` |
| `cq sessions --project ~/pickleton` | `SyncScope::Project(~/pickleton)` |
| `cq messages --session <id>` | `SyncScope::File(<resolved path>)` if file registry has it, otherwise `All` |
| `cq --reindex ...` | `SyncScope::All` (always) |

## Changes to Existing Code

### `cache.rs`
- Add `last_sync_at` column to `cache_meta` table
- Add function to read/write `last_sync_at`

### `indexer.rs`
- Add `SyncScope` enum
- Add mtime check function (stat directories, compare against `last_sync_at`)
- Add file locking around write operations
- `sync()` signature changes to accept `SyncScope` and sync mode

### `db.rs`
- `setup_connection()` passes sync mode and scope through to indexer

### `main.rs`
- Add `--no-reindex` flag (opposite of existing `--reindex`)
- Derive `SyncScope` from command + flags
- Pass sync mode to `setup_connection()`

### New dependency
- `fs2` crate for cross-platform file locking
