# Design Principles

These are the foundational assumptions that guide how cq is built. CLI conventions live in `docs/cli-ux-conventions.md`. This document covers the deeper "why" behind cq's architecture and behavior.

## CQ is a query tool, not a monitoring tool

CQ answers questions about sessions that have happened. It's not watching a live stream. Freshness means "has this been indexed," not "is this millisecond-current."

This shapes everything from how indexing works (lazy, not eager) to what we optimize for (query speed over data recency).

## Auto-scope to the current project, escape to global

Most CQ questions happen inside a project: "what did I do in this repo today," "which sessions touched this codebase." When you run cq from a directory that matches a known project, cq auto-scopes to that project and prints the scope on stderr. `--all` escapes to global when you want everything.

This makes the common case fast and obvious, without hiding the rest. The scope hint on stderr means you always see what's being matched, and piping still gets clean stdout.

## Query scopes

| Scope | When | How |
|-------|------|-----|
| **Auto-project** (default in a project dir) | Focused on current repo | `cq sessions --since today` |
| **Global** | Questions across all work | `cq sessions --all` |
| **Named project** | A different codebase | `cq sessions --project myproject` |
| **Session** | One session's details | `cq messages --session <id>` |

Scope affects both what data is queried and how much work the indexer does. Sync scope follows query scope, so `--project foo` narrows the indexer to that project's files too.

## Select the active harness by default

Built-in commands select `harness = 'codex'` inside a Codex runtime and
`harness = 'claude'` everywhere else. This keeps a normal query focused on the
tool the user is currently using, without relying on Codex rows having no
Claude `source`. `--harness` explicitly chooses one harness, while `--all`
spans them. `cq sql` is deliberately raw and never receives automatic scope.

## Stale-but-available beats error

If something goes wrong during indexing (lock contention, filesystem issues), serve cached data and tell the user on stderr. A slightly stale result is infinitely more useful than a crash.

This applies to concurrent access especially: multiple processes running CQ simultaneously should never cause failures. The worst case is one process uses slightly older data.

## Explicit always wins

Automatic behavior (smart sync, mtime checks, lock fallbacks) should do the right thing by default. But when the user says `--reindex` or `--no-reindex`, that overrides all smartness. The escape hatch is never hidden, never conditional.

## Stderr for process, stdout for data

Progress messages, sync status, cache warnings, and hints go to stderr. Query results go to stdout. This lets piped output stay clean and lets humans see what's happening without corrupting data flow.
