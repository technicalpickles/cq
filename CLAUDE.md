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
