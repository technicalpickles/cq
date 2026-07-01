# cq

cq queries AI coding-session transcripts with SQL. It indexes transcripts into a DuckDB cache and exposes four views (`sessions`, `messages`, `tool_calls`, `tool_results`). This context defines the terms cq uses to talk about *where transcripts come from*, since more than one tool can produce them.

## Language

**Harness**:
The tool that produced the transcripts — Claude Code, opencode. Distinct harnesses store transcripts in fundamentally different shapes (Claude Code: a tree of JSONL files; opencode: one append-only SQLite DB).
_Avoid_: "source" for this meaning (see below), "client", "agent".

**Provider**:
The cq component that knows how to read one **Harness**'s storage. `ClaudeProvider` exists today; `OpenCodeProvider` is planned. Modeled by the `TranscriptProvider` trait, though the runtime path does not yet dispatch through it polymorphically.
_Avoid_: "adapter", "backend".

**Source**:
A named JSONL transcript root *within the Claude provider*: `"main"` (`~/.claude/projects`) plus one per discovered cenv env. This is what `--source` selects. It is **not** the cross-harness concept — opencode is a new **Provider**, not a new **Source**.
_Avoid_: using "source" to mean a different harness.

## Relationships

- A **Harness** is read by exactly one **Provider**.
- The Claude **Provider** spans one or more **Sources** (main + cenv envs); `--source` scopes within it.
- The opencode **Provider** has no **Sources** in the current sense — its storage is a single SQLite DB, not a directory of JSONL roots.
- The cq views carry two distinct columns: `harness` is the harness/provider tag (`'claude'` / `'opencode'`), and `source` is the within-Claude root name that the `--source` flag selects (`main` + cenv envs).

## Flagged ambiguities

- "source" was used in the opencode findings doc to mean "a new harness" (`'opencode'` source). Resolved: that is a **Provider**/**Harness**, not a **Source**. cq's existing `Source` keeps its narrow within-Claude meaning. The earlier overload on the views' `source` column was resolved by giving the views a separate `harness` column for the provider tag, leaving `source` to mean the within-Claude root name (matching the `--source` flag).
