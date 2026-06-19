# opencode is a live-ATTACHed Provider, not a cached Source

**Status:** accepted

cq reads opencode sessions as a new **Provider** (peer to `ClaudeProvider`), querying opencode's single SQLite DB live via DuckDB `ATTACH ... (TYPE sqlite, READ_ONLY)` on every invocation. opencode rows are **never written to the persistent cache** (`raw_records`/`file_registry`). The four views become `claude_<view> UNION ALL opencode_<view>`, composed at runtime from whichever providers prepared successfully.

## Why

- opencode is a different *harness* with a fundamentally different storage shape (one append-only SQLite DB vs. Claude's tree of JSONL files). Modeling it as another `Source` (cq's within-Claude "a JSONL projects dir" concept) would be a category error — see `CONTEXT.md`.
- Not caching opencode **deletes the hardest open question**: an append-only single-file DB has no clean per-file mtime/size cursor for incremental indexing. Live ATTACH over a local SQLite file is milliseconds and always fresh, so the cache buys nothing here. The "freshness = has this been indexed" principle bends gracefully: a no-index provider is always current.
- The decision is reversible at the view-contract level: if opencode.db ever grows enough to matter, a caching/materialization step can be added behind the same `contribute_view_sql` seam without changing what queries see.

## Considered and rejected

- **(a) Converter to Claude-style JSONL.** Makes opencode masquerade as a Claude `Source` (the collision above), loses cost/token data, and goes stale. Rejected.
- **(b-materialize) `INSERT INTO raw_records` during sync.** Same lossy re-encoding as (a) and revives the mtime-cursor problem. Rejected.
- **Static-bundle the DuckDB `sqlite_scanner`.** Real build-system investment (`duckdb-rs`'s `bundled` feature doesn't expose it). Deferred unless cq ships as a binary where a one-time first-run network fetch is unacceptable.

## Consequences

- Reading opencode requires DuckDB's `sqlite` extension. We `INSTALL`+`LOAD` at runtime (cached in `~/.duckdb/` after first online fetch). If it can't load, `OpenCodeProvider` **degrades gracefully** — contributes no views, warns on stderr, and Claude queries are unaffected ("stale-but-available beats error"). The one-time `INSTALL` needs network (will fail under Claude Code's sandbox until cached).
- A new **`harness`** column (`'claude'`/`'opencode'`) is added to all four views, distinct from the within-Claude `--source` dimension.
