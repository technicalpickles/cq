# cq

CLI tool for querying Claude Code session transcripts with SQL. Rust + DuckDB.

Reads Claude Code's JSONL session files from `~/.claude/projects/`, indexes them into a persistent DuckDB cache at `~/.cache/cq/index.duckdb` (or `$CQ_CACHE_DIR` if set), and exposes five SQL views: `sessions`, `messages`, `tool_calls`, `tool_results`, `hook_events`. Sync is incremental: files are re-parsed only when their mtime or size changes.

The input format is not ours and is not a documented contract. Before you write anything that reads or reasons about transcripts, read `docs/session-storage.md`: it covers the on-disk layout, which record types actually show up, and the places the format will surprise you.

## Docs

| Doc | What it's for |
|-----|---------------|
| `docs/session-storage.md` | The transcript format on disk: layout, record types, gotchas |
| `docs/cli-ux-conventions.md` | Flags, help text, error messages, output behavior, plus the docs-sync table |
| `docs/design-principles.md` | Why the defaults are what they are |
| `docs/use-cases.md` | Worked queries |
| `CONTEXT.md` | Harness / Provider / Source glossary |
| `docs/adr/` | Decisions with lasting consequences |

## Architecture

```
main.rs           CLI (clap), arg parsing, dispatches to commands
lib.rs            Library entry point, re-exports modules for integration tests
commands/
  sessions.rs     List/filter sessions
  tools.rs        Tool call queries + summary mode (no filters = grouped counts)
  hooks.rs        Hook event queries + summary mode (mirrors tools.rs)
  messages.rs     Message queries
  search.rs       BM25-ranked full-text search over the persisted message search index
  projects.rs     `cq projects`: per-project session/message/tool/skill counts
  context.rs      ContextSqlBuilder: grep-style context windows (-C/--after/--before)
  mod.rs          Shared arg validators (count-by, fields, context-window conflicts)
  sql.rs          Raw SQL passthrough (intentionally unparameterized)
  schema.rs       View schema docs + example queries (pure text, no DB needed)
output.rs         Shared rendering: table (comfy-table) or JSON, accepts params
style.rs          Terminal styling helpers (colors, dim/bold, TTY detection)
views.rs          Per-provider view SQL (Claude bodies over raw_records) + the composer that UNION ALLs active providers' contributions into the five views; every row carries a `source` column (within-Claude root name) and a `harness` column (`'claude'`)
db.rs             Orchestrates cache open + indexer sync, registers views, returns DbSetup
cache.rs          Persistent DuckDB cache at ~/.cache/cq/index.duckdb; schema versioning + rebuild
full_text.rs      Lazy physical message snapshot + DuckDB FTS index lifecycle
indexer.rs        Incremental sync: file_registry + recursive mtime fast-path, fs2 file lock; recurses into <session>/subagents/** and captures agentType from meta.json
sync_scope.rs     SyncScope: narrows which files the indexer touches (derived from --project etc.)
provider.rs       TranscriptProvider trait
claude_provider.rs  ClaudeProvider: recursively discovers JSONL files (incl. subagents) from ~/.claude/projects/
scope.rs          QueryScope: --project, --session, --since parsing
source.rs         Source: named transcript roots (`main` + discovered cenv envs); backs --source
```

## Design principles

CQ is a query tool, not a monitoring tool. Built-in commands infer current-context scope: Claude outside a Codex runtime, Codex inside one, the current project directory, and the active Claude source. `--all` removes inferred scope; explicit filters still apply. Within Claude, `--source <name>` targets one transcript root, else cq selects the active source (matched via `CLAUDE_CONFIG_DIR`). Stale-but-available beats error. Explicit always wins (`--reindex`/`--no-reindex` override all automatic behavior). See `docs/design-principles.md` for the full reference, and `CONTEXT.md` for the Harness/Provider/Source glossary.

## Key patterns

- **Commands build SQL + params, output renders.** Each command constructs a WHERE clause with `?` placeholders, collects params in a `Vec<Box<dyn ToSql>>`, and passes both to `output::print_results`.
- **Persistent cache + incremental sync.** `cache.rs` opens the cache DB and handles schema versioning. `indexer.rs` walks files, checks mtime + size against `file_registry`, and only re-parses what changed. `fs2` file locking serializes concurrent writers; readers fall back to cached data when the lock is busy. How the scan handles nested subagent files, and why `index_files` stages rows in a temp table before inserting, both follow from the on-disk format: `docs/session-storage.md`.
- **Full-text search trades freshness for latency, on purpose.** DuckDB's FTS index targets a physical `cq_fts_messages` snapshot because `messages` is a composed view and FTS indexes do not update automatically. `cache_meta.fts_sync_at` records the transcript sync that snapshot covers, and `fts_built_at` records when it was built; the first answers "did the data move," the second "how old is this." `cq search` rebuilds only when the data moved *and* the index has aged past `CQ_FTS_MAX_AGE` (default 5m), because a rebuild costs several times a normal query while the median gap between cq invocations is ~16s. Serving stale always prints why on stderr, with a sharper warning when the caller's own session is ahead of the index (checked via `CLAUDE_SESSION_ID`, not by inspecting results, so false negatives get caught). `--reindex` forces a rebuild explicitly, `--no-reindex` skips it. Other commands never pay the FTS cost. Full reasoning and the usage data behind the 5m default: `docs/notes/2026-08-27-full-text-search-progress.md`.
- **SyncMode is explicit over smart.** `Auto` (default) does mtime fast-path + try-lock + skip-if-busy. `Force` (`--reindex`) waits for the lock and re-parses everything. `Skip` (`--no-reindex`) bypasses sync entirely. User flags always beat smart behavior.
- **SyncScope narrows sync work.** A `--project` filter also restricts which files the indexer touches, not just which rows the query returns. Derived in `main.rs` from the CLI flags, passed through `db::setup_connection` into `indexer::sync`.
- **Provider trait** abstracts a transcript *harness*. Beyond file discovery, a provider has `prepare(conn) -> bool` (is it active?) and `contribute_view_sql(view)` (a SELECT body); `views::compose_views` UNION ALLs the active providers' contributions, tagging rows with a `harness` column. Only `ClaudeProvider` exists today (always active; its bodies read `raw_records`); the seam is built for a second harness to plug in.
- **stderr for progress, stdout for data.** "Scanned N files" and "No results." go to stderr so piped output stays clean.
- **Project paths come from the registry, not the directory name.** `PROJECT_EXPR` in `views.rs` prefers the `cwd` captured at index time and only falls back to decoding the encoded directory name. The encoding is lossy (it eats dots as well as slashes), so the fallback is wrong for any path containing a dot or a hyphen. See `docs/session-storage.md`.
- **ILIKE for project filtering.** `--project` does substring match, not exact.
- **`advisor()` calls use server-side content blocks, not `tool_use`/`tool_result`.** `claude_tool_calls_sql`/`claude_tool_results_sql` in `views.rs` special-case them so they surface as `name = 'advisor'`. The block shapes and why the result isn't where you'd expect are in `docs/session-storage.md`.

## Keeping docs in sync

When you change behavior, the matching docs need to move with it. `docs/cli-ux-conventions.md` has a "Keeping docs in sync" table mapping change types to the docs they affect (flags → README table, modules → CLAUDE.md tree, sync/cache → Key patterns, etc.). Run through the matching row before considering a change done.

## CLI UX conventions

`docs/cli-ux-conventions.md` is the reference, with the examples, rationale, and a checklist to run before calling a flag done. Read it when you touch CLI behavior. The rules it comes down to:

- Fixed-value flags list their values in `--help` as `[valid: a, b, c]`. Dynamic ones name the command that reveals them.
- Validation errors follow `Error:` / `Valid <thing>:` / `Hint:`, and always quote the input the user actually passed.
- Column aliases (`session` for `session_id`) work everywhere that column is accepted, not just where you added them.
- Truncate for terminals, never for pipes: `let wide = cli.wide || !std::io::stdout().is_terminal();`

## Tests

```bash
cargo test              # all tests (unit + integration + view tests)
```

- `tests/views_test.rs`: View SQL correctness against fixture JSONL files
- `tests/integration_test.rs`: End-to-end CLI tests with `assert_cmd`
- `tests/cache_test.rs`: Cache open / schema-version / rebuild behavior
- `src/` unit tests: Scope parsing, path encoding, file discovery, source discovery

Fixtures live in `tests/fixtures/` as handcrafted JSONL files.

## Developing

```bash
cargo run -- sessions --limit 5                        # basic usage
cargo run -- tools --limit 5                           # tool summary
cargo run -- sessions --project myproject --limit 5    # project filter
cargo run -- sql "SELECT COUNT(*) FROM messages"       # raw SQL
cargo run -- schema --examples                         # see available views + queries
```

## Releasing

Releases are driven by [release-please](https://github.com/googleapis/release-please)
off conventional commits. The flow:

1. Land conventional commits on `main` (`feat:` → minor bump, `fix:` → patch, etc.).
2. `release-please.yml` keeps an open "release PR" that bumps `Cargo.toml` + `Cargo.lock`
   and updates `CHANGELOG.md`. Merge it when you want to ship.
3. Merging that PR cuts the git tag + GitHub release. The `release: published` event
   then fires `release.yml`, which builds `cq` for macOS (arm64 + x86_64) and Linux
   (x86_64 + arm64) and attaches the archives to the release.

You don't bump versions or push tags by hand. Each target builds on its own native
runner because the bundled DuckDB compiles C++ from source, which makes cross-compiling
more trouble than it's worth. There is no crates.io publish step today.

The bump version lives in `.release-please-manifest.json` (kept in sync with
`Cargo.toml`). `release.yml` only fires when the release is created with a token that
triggers downstream workflows — the `RELEASE_PLEASE_TOKEN` secret (a PAT or GitHub App
token), not the default `GITHUB_TOKEN`.
