# cq

CLI tool for querying Claude Code session transcripts with SQL. Rust + DuckDB.

Reads Claude Code's JSONL session files from `~/.claude/projects/`, loads them into an in-memory DuckDB instance, and exposes four SQL views: `sessions`, `messages`, `tool_calls`, `tool_results`.

## Architecture

```
main.rs           CLI (clap), arg parsing, dispatches to commands
commands/
  sessions.rs     List/filter sessions
  tools.rs        Tool call queries + summary mode (no filters = grouped counts)
  messages.rs     Message queries
  sql.rs          Raw SQL passthrough (intentionally unparameterized)
  schema.rs       View schema docs + example queries (pure text, no DB needed)
output.rs         Shared rendering: table (comfy-table) or JSON, accepts params
views.rs          SQL view definitions (raw_records, messages, tool_calls, tool_results, sessions)
db.rs             Connection setup: discover files, register views, return DbSetup
provider.rs       TranscriptProvider trait
claude_provider.rs  ClaudeProvider: discovers JSONL files from ~/.claude/projects/
scope.rs          QueryScope: --project, --session, --since parsing
```

## Key patterns

- **Commands build SQL + params, output renders.** Each command constructs a WHERE clause with `?` placeholders, collects params in a `Vec<Box<dyn ToSql>>`, and passes both to `output::print_results`.
- **Provider trait** abstracts file discovery. Only `ClaudeProvider` exists today but the trait allows other transcript sources.
- **stderr for progress, stdout for data.** "Scanned N files" and "No results." go to stderr so piped output stays clean.
- **Project paths are decoded in SQL.** `PROJECT_EXPR` in `views.rs` converts encoded directory names (e.g. `-Users-alice-myproject`) back to paths (`/Users/alice/myproject`).
- **ILIKE for project filtering.** `--project` does substring match, not exact.

## Tests

```bash
cargo test              # all tests (unit + integration + view tests)
```

- `tests/views_test.rs` (20 tests): View SQL correctness against fixture JSONL files
- `tests/integration_test.rs` (12 tests): End-to-end CLI tests with `assert_cmd`
- `src/` unit tests (14 tests): Scope parsing, path encoding, file discovery

Fixtures live in `tests/fixtures/` as handcrafted JSONL files.

## Developing

```bash
cargo run -- sessions --limit 5                        # basic usage
cargo run -- tools --limit 5                           # tool summary
cargo run -- sessions --project myproject --limit 5    # project filter
cargo run -- sql "SELECT COUNT(*) FROM messages"       # raw SQL
cargo run -- schema --examples                         # see available views + queries
```
