# Design Principles

These are the foundational assumptions that guide how cq is built. CLI conventions live in `docs/cli-ux-conventions.md`. This document covers the deeper "why" behind cq's architecture and behavior.

## CQ is a query tool, not a monitoring tool

CQ answers questions about sessions that have happened. It's not watching a live stream. Freshness means "has this been indexed," not "is this millisecond-current."

This shapes everything from how indexing works (lazy, not eager) to what we optimize for (query speed over data recency).

## Default is global, narrow explicitly

Most CQ questions span projects: "what sessions did I run today," "which skills get invoked most," "find that session where I was debugging X." Defaulting to one project would make these queries silently incomplete.

Scoping is always opt-in via flags like `--project` or `--session`. The unscoped default shows everything.

## Three query scopes

| Scope | When | Example |
|-------|------|---------|
| **Global** | Questions across all work | `cq sessions --since today` |
| **Project** | Focused on a specific codebase | `cq sessions --project ~/pickleton` |
| **Session** | Looking at one session's details | `cq messages --session <id>` |

These scopes affect both what data is queried and how much work the indexer does to keep data fresh. Sync scope follows query scope.

## Stale-but-available beats error

If something goes wrong during indexing (lock contention, filesystem issues), serve cached data and tell the user on stderr. A slightly stale result is infinitely more useful than a crash.

This applies to concurrent access especially: multiple processes running CQ simultaneously should never cause failures. The worst case is one process uses slightly older data.

## Explicit always wins

Automatic behavior (smart sync, mtime checks, lock fallbacks) should do the right thing by default. But when the user says `--reindex` or `--no-reindex`, that overrides all smartness. The escape hatch is never hidden, never conditional.

## Stderr for process, stdout for data

Progress messages, sync status, cache warnings, and hints go to stderr. Query results go to stdout. This lets piped output stay clean and lets humans see what's happening without corrupting data flow.
