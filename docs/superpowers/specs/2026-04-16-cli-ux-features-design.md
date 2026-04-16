# cq CLI UX Features Design

Four features that improve cq's output control and aggregation capabilities.

Related beans: gt-u9ff, gt-ilzd, gt-r4mh, gt-m6bl

## Design Principles

### `--fields` is column selection everywhere

`--fields` appears on `tools`, `messages`, and `sessions`. It means "show only these columns." The implementation varies by command:

- **tools**: extracts from the JSON `input` blob (`json_extract_string(input, '$.field')`)
- **messages** and **sessions**: selects flat columns by name

Users don't need to think about this difference. They write `--fields text` or `--fields command` and get the column they asked for.

This matters for piping. `--fields` turns cq into a data source for shell pipelines:

```bash
cq sessions --fields session_id | xargs -I{} cq tools --session {}
cq messages --type user --fields text
cq tools Bash --fields command
```

### TTY-aware output

Truncation serves terminal readability. When stdout isn't a terminal, the consumer is another program, and truncation destroys data. cq detects TTY and adjusts:

- **Terminal**: truncated columns (current behavior)
- **Pipe**: full output (auto-wide)
- **`--wide`**: forces full output in terminal

This follows the convention of `ls`, `git log`, and other tools that adapt to their output context.

There is no `--no-wide` flag for forcing truncation when piped. The escape hatch already exists: `--json` gives structured output for programmatic consumers, and auto-wide handles the common case of piping to grep/awk/cut.

### Error messages follow a consistent template

Every validation error across these features uses the same structure:

```
Error: <what went wrong specifically>
Valid <thing>: <comma-separated list>
Hint: <how to learn more, if applicable>
```

Examples:
```
Error: Unknown field 'mesage' for messages
Valid fields: session_id, project, type, timestamp, text, model, tool_count

Error: Unknown count-by column 'tool' for tools
Valid columns: name, session, project

Error: --timeline requires --session
Usage: cq sessions --session <id> --timeline
```

This applies to `--fields` validation, `--count-by` validation, and flag combination errors (`--timeline` without `--session`, `--count-by` with `--fields`).

### Valid values are discoverable in `--help`

`--fields` and `--count-by` list their valid values directly in the flag's help text, so users can discover them without trial and error:

```
$ cq messages --help
...
--fields <FIELDS>   Extract specific columns (comma-separated)
                    [valid: session_id, project, type, timestamp, text, model, tool_count]
--count-by <COLUMN> Aggregate rows into counts by column
                    [valid: type, session, project]
```

Each command lists its own valid values since they differ per command. This is the primary discovery path. The error messages (with `cq schema` hints) are the fallback for typos.

### Friendly aliases for common columns

`--count-by session` maps to `session_id` in the SQL. `--fields` should accept the same alias: `--fields session` works like `--fields session_id`. This keeps the two flags consistent with each other.

Aliases: `session` -> `session_id`.

## Feature 0: Help text and error message improvements (existing flags)

While adding new flags, bring existing constrained-value flags up to the same discoverability standard.

### Changes

**`messages --type`**: Change help from "Filter by message type (user or assistant)" to include the `[valid: ...]` format:
```
--type <TYPE>  Filter by message type [valid: user, assistant]
```

**`schema [NAME]`**: Add valid values to the positional arg help:
```
[NAME]  Show documentation for a specific view [valid: messages, tool_calls, tool_results, sessions]
```

**`tools [NAME]`**: Add a hint about discovery:
```
[NAME]  Filter to a specific tool name (run 'cq tools' to see available names)
```

**`tools --fields`**: Clarify that fields come from tool input JSON:
```
--fields <FIELDS>  Extract specific input fields as columns (comma-separated; fields depend on the tool, see 'cq schema tool_calls')
```

### Existing error message alignment

Bring existing validation errors in line with the error template.

**`--since` with bad input** (scope.rs): Currently splits the input and gives a confusing partial message ("Invalid duration number: bogu" for input "bogus"). Should validate the full input first:

```
Error: Invalid duration 'bogus'
Expected format: <number><unit> (e.g. 7d, 24h, 30m)
```

**`--since` with bad unit** (scope.rs): Currently "Unknown duration unit: x. Use d, h, or m." Close but should align:

```
Error: Unknown duration unit 'x' in '7x'
Valid units: d (days), h (hours), m (minutes)
```

**`--session` with bad ID** (scope.rs): Currently good structure but could add a discovery hint:

```
Error: 'bad' is not a valid session ID
Expected UUID format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
Hint: Run 'cq sessions' to find session IDs
```

**`schema` with unknown view** (schema.rs): Currently "Unknown view: bogus. Known views: ...". Align wording:

```
Error: Unknown view 'bogus'
Valid views: messages, tool_calls, tool_results, sessions
```

### Help text for global flags

**`--session`**: Change from "Scope to a session (prefix match)" to:
```
--session <ID>  Scope to a session by UUID (prefix match supported)
```

**`--project`**: Change from "Scope to a project (substring match)" to:
```
--project <NAME>  Scope to a project (substring match, e.g. 'myproject')
```

### Files changed

- `src/main.rs`: update help strings on existing args
- `src/scope.rs`: improve `--since` and `--session` error messages
- `src/commands/schema.rs`: align error format

## Feature 1: `--wide` flag and TTY detection (gt-u9ff)

### CLI

```
--wide    Show full column values without truncation (global flag)
```

### Behavior

| Context | Truncation |
|---------|-----------|
| Terminal, no flag | Truncated (current behavior) |
| Terminal, `--wide` | Full output |
| Piped (not TTY) | Full output |
| `--json` | Full output (unchanged) |

### Implementation

Add `--wide` as a global flag in `main.rs`. Compute effective wide as:

```rust
let wide = cli.wide || !std::io::stdout().is_terminal();
```

Thread `wide` into each command's `run()` function, which passes it to render functions. When wide is true, render functions skip `style::truncate()` calls.

`output::value_to_string()` currently hardcodes `truncate(&s, 120)`. Change it to accept a max width (0 for unlimited) so the generic `print_results` path also respects wide mode.

### Files changed

- `src/main.rs`: add `--wide` flag, compute effective wide, pass to commands
- `src/output.rs`: `value_to_string` accepts width param
- `src/commands/tools.rs`: thread wide to render functions
- `src/commands/messages.rs`: thread wide to render functions
- `src/commands/sessions.rs`: thread wide to render functions

## Feature 2: `--fields` on messages and sessions (gt-ilzd)

### CLI

```
cq messages --fields text              # just message text
cq messages --type user --fields text  # filtered + field selection
cq sessions --fields session_id        # just IDs for piping
cq sessions --fields session_id,first_user_message
```

### Valid fields

**messages**: session_id, project, type, timestamp, text, model, tool_count

**sessions**: session_id, project, started_at, ended_at, message_count, tool_call_count, user_message_count, first_user_message

Invalid field names produce an error listing valid options and pointing to `cq schema <view>` for field descriptions:

```
Error: Unknown field 'mesage' for messages
Valid fields: session_id, project, type, timestamp, text, model, tool_count
Hint: Run 'cq schema messages' for field descriptions
```

### Implementation

For flat-column commands, `--fields` modifies the SELECT clause to include only the requested columns. The render path uses the generic `output::print_results` since column names come from the query itself.

This differs from `tools --fields` which builds `json_extract_string` expressions. The user-facing behavior is the same: name a field, get that column.

### Files changed

- `src/main.rs`: add `--fields` to Messages and Sessions commands
- `src/commands/messages.rs`: add field validation, modify SELECT when fields specified
- `src/commands/sessions.rs`: add field validation, modify SELECT when fields specified

## Feature 3: `--count-by` aggregation (gt-r4mh)

### CLI

```
cq tools --count-by name                    # same as current summary
cq tools --errors --count-by session        # error counts per session
cq tools Bash --count-by session            # Bash usage per session
cq messages --type user --count-by session  # user turns per session
cq messages --count-by type                 # message type breakdown
cq sessions --count-by project              # sessions per project
```

### Valid count-by columns

**tools**: `name`, `session` (maps to session_id), `project`

**messages**: `type`, `session` (maps to session_id), `project`

**sessions**: `project`

`session` is a friendly alias for `session_id` in the GROUP BY and display. Sessions only supports `project` since the other columns are already unique per session.

### Behavior

`--count-by` switches to aggregation mode: `SELECT <column>, COUNT(*) AS count ... GROUP BY <column> ORDER BY count DESC`. All existing filters (--errors, --grep, --since, name positional) still apply as WHERE conditions before aggregation.

Default rendering uses the bar chart format (same as `cq tools` summary mode). `--table` and `--json` work as usual.

`--count-by` and `--fields` are mutually exclusive:

```
Error: --count-by and --fields cannot be used together
--count-by aggregates rows into counts; --fields selects columns from detail rows
```

Invalid column names produce a targeted error:

```
Error: Unknown count-by column 'tool' for tools
Valid columns: name, session, project
```

### Implementation

When `--count-by` is present, build an aggregation query instead of a detail query. Reuse the existing `render_bar_chart` from tools.rs (extract to a shared location or duplicate, depending on how clean it stays).

### Files changed

- `src/main.rs`: add `--count-by` to Tools, Messages, and Sessions commands
- `src/commands/tools.rs`: add aggregation path, validate column name
- `src/commands/messages.rs`: add aggregation path, validate column name
- `src/commands/sessions.rs`: add aggregation path, validate column name

## Feature 4: Session timeline (gt-m6bl)

### CLI

```
cq sessions --session <id> --timeline
```

Requires `--session`. Without it:

```
Error: --timeline requires --session
Usage: cq sessions --session <id> --timeline
Hint: Run 'cq sessions' to find session IDs
```

### Output

Chronological interleaved view of tool calls and their results:

```
14:02:01  call    Read    /src/main.rs
14:02:01  result  Read    1,234 bytes
14:02:03  call    Edit    /src/main.rs
14:02:04  result  Edit    ok
14:02:05  call    Bash    cargo test
14:02:15  result  Bash    error (2,456 bytes)
```

Columns:
- **time**: HH:MM:SS extracted from timestamp
- **event**: `call` or `result`
- **tool**: tool name
- **summary**: for calls, a snippet of the input (first meaningful field). For results, status + content length. Errors highlighted.

### SQL

```sql
SELECT 'call' AS event, tc.timestamp, tc.name,
       CAST(tc.input AS VARCHAR) AS detail
FROM tool_calls tc
WHERE tc.session_id = ?
UNION ALL
SELECT 'result' AS event, tc.timestamp, tc.name,
       CASE WHEN tr.is_error THEN 'error' ELSE 'ok' END
       || ' (' || LENGTH(tr.content) || ' bytes)' AS detail
FROM tool_calls tc
JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
WHERE tc.session_id = ?
ORDER BY timestamp, event
```

Results are ordered by timestamp, with `call` sorted before `result` at the same timestamp so pairs stay together.

### Rendering

Default format uses the aligned-columns style with color:
- Time in dim
- `call`/`result` in primary/secondary
- Tool name in primary
- Error results highlighted

`--json` and `--table` work as usual. `--wide` applies to the detail/summary column.

### Files changed

- `src/main.rs`: add `--timeline` flag to Sessions command
- `src/commands/sessions.rs`: add `run_timeline()`, validate --session required

## Implementation order

1. **--wide** first, since it changes the output plumbing that other features use
2. **--fields on messages/sessions**, since it's a straightforward port
3. **--count-by**, since it adds a new query mode
4. **timeline**, since it's the most self-contained new feature

## Testing

Each feature gets:
- Integration tests in `tests/integration_test.rs` (CLI invocation with assert_cmd)
- The timeline feature gets a view test in `tests/views_test.rs` for the SQL query

Fixture data in `tests/fixtures/` already has tool_calls and tool_results, which covers all four features.
