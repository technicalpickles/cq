# cq

CLI tool for querying Claude Code session transcripts with SQL. Rust + DuckDB.

Reads Claude Code's JSONL session files from `~/.claude/projects/`, indexes them into a persistent DuckDB cache at `~/.cache/cq/index.duckdb` (or `$CQ_CACHE_DIR` if set), and exposes five SQL views: `sessions`, `messages`, `tool_calls`, `tool_results`, `hook_events`. Sync is incremental: files are re-parsed only when their mtime or size changes.

## Architecture

```
main.rs           CLI (clap), arg parsing, dispatches to commands
lib.rs            Library entry point, re-exports modules for integration tests
commands/
  sessions.rs     List/filter sessions
  tools.rs        Tool call queries + summary mode (no filters = grouped counts)
  hooks.rs        Hook event queries + summary mode (mirrors tools.rs)
  messages.rs     Message queries
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
indexer.rs        Incremental sync: file_registry + recursive mtime fast-path, fs2 file lock; recurses into <session>/subagents/** and captures agentType from meta.json
sync_scope.rs     SyncScope: narrows which files the indexer touches (derived from --project etc.)
provider.rs       TranscriptProvider trait
claude_provider.rs  ClaudeProvider: recursively discovers JSONL files (incl. subagents) from ~/.claude/projects/
scope.rs          QueryScope: --project, --session, --since parsing
source.rs         Source: named transcript roots (`main` + discovered cenv envs); backs --source
```

## Design principles

CQ is a query tool, not a monitoring tool. Default is auto-scope to the current project directory; `--all` escapes to global. Sources scope the same way: `--source <name>` targets one transcript root, else cq auto-scopes to the active source (matched via `CLAUDE_CONFIG_DIR`), and `--all` spans all sources. Stale-but-available beats error. Explicit always wins (`--reindex`/`--no-reindex` override all automatic behavior). See `docs/design-principles.md` for the full reference, and `CONTEXT.md` for the Harness/Provider/Source glossary.

## Key patterns

- **Commands build SQL + params, output renders.** Each command constructs a WHERE clause with `?` placeholders, collects params in a `Vec<Box<dyn ToSql>>`, and passes both to `output::print_results`.
- **Persistent cache + incremental sync.** `cache.rs` opens the cache DB and handles schema versioning. `indexer.rs` walks files, checks mtime + size against `file_registry`, and only re-parses what changed. `fs2` file locking serializes concurrent writers; readers fall back to cached data when the lock is busy. The scan recurses into `<session>/subagents/**` (excluding `journal.jsonl`); subagent rows carry the parent `session_id` plus `is_sidechain`/`agent_id`/`agent_type`/`workflow_id` tags. The Auto mtime fast-path is a recursive max so new deep files are detected.
- **SyncMode is explicit over smart.** `Auto` (default) does mtime fast-path + try-lock + skip-if-busy. `Force` (`--reindex`) waits for the lock and re-parses everything. `Skip` (`--no-reindex`) bypasses sync entirely. User flags always beat smart behavior.
- **SyncScope narrows sync work.** A `--project` filter also restricts which files the indexer touches, not just which rows the query returns. Derived in `main.rs` from the CLI flags, passed through `db::setup_connection` into `indexer::sync`.
- **Provider trait** abstracts a transcript *harness*. Beyond file discovery, a provider has `prepare(conn) -> bool` (is it active?) and `contribute_view_sql(view)` (a SELECT body); `views::compose_views` UNION ALLs the active providers' contributions, tagging rows with a `harness` column. Only `ClaudeProvider` exists today (always active; its bodies read `raw_records`); the seam is built for a second harness to plug in.
- **stderr for progress, stdout for data.** "Scanned N files" and "No results." go to stderr so piped output stays clean.
- **Project paths are decoded in SQL.** `PROJECT_EXPR` in `views.rs` converts encoded directory names (e.g. `-Users-alice-myproject`) back to paths (`/Users/alice/myproject`).
- **ILIKE for project filtering.** `--project` does substring match, not exact.
- **`advisor()` calls use server-side content blocks, not `tool_use`/`tool_result`.** They show up as `server_tool_use` (call) and `advisor_tool_result` (result) blocks, both inside `assistant`-type records -- the result is *not* in the following `user`-type record like a normal tool result. `claude_tool_calls_sql`/`claude_tool_results_sql` in `views.rs` special-case both: `tool_calls` matches `server_tool_use` alongside `tool_use` (same `id`/`name`/`input` shape), and `tool_results` matches `advisor_tool_result` in `assistant` records alongside `tool_result` in `user` records, unwrapping `content.text` since `content` is a nested object (`{type, text}`) instead of a plain string. Rows surface with `name = 'advisor'`.

## Keeping docs in sync

When you change behavior, the matching docs need to move with it. `docs/cli-ux-conventions.md` has a "Keeping docs in sync" table mapping change types to the docs they affect (flags → README table, modules → CLAUDE.md tree, sync/cache → Key patterns, etc.). Run through the matching row before considering a change done.

## CLI UX conventions

When adding or modifying CLI behavior, follow these conventions. See `docs/cli-ux-conventions.md` for the full reference with examples, rationale, and checklists.

### Discoverability: valid values in `--help`

Any flag or arg that accepts a fixed set of values lists them in the help text using `[valid: ...]` format:

```
--type <TYPE>       Filter by message type [valid: user, assistant]
--count-by <COL>    Aggregate rows into counts [valid: name, session, project]
--fields <FIELDS>   Extract specific columns [valid: session_id, project, type, ...]
```

For dynamic or tool-dependent values, point to where the user can discover them:

```
[NAME]  Filter to a specific tool name (run 'cq tools' to see available names)
```

When adding a new flag, ask: "Can the user discover valid values without trial and error?" If not, add them to `--help`.

### Forgiveness: error message template

Every validation error uses this structure:

```
Error: <what went wrong, including the invalid input in quotes>
Valid <thing>: <comma-separated list>
Hint: <how to learn more or fix it, if applicable>
```

The three lines serve different needs: the first tells you what's wrong, the second tells you what's right, the third tells you where to go next. The hint line is optional for cases where the valid values already make the fix obvious.

When adding validation, always include the user's invalid input in the error so they can see their typo.

### Consistency: friendly aliases

Use short aliases for common column references: `session` maps to `session_id`. Keep aliases consistent across flags (`--fields session` and `--count-by session` both work). If you add a new alias, it should work everywhere that column appears.

### TTY-aware output

Truncation serves terminal readability. When piped, output full values (the consumer is another program). `--wide` forces full output in terminal. `--json` always gives full output.

The pattern: `let wide = cli.wide || !std::io::stdout().is_terminal();`

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
