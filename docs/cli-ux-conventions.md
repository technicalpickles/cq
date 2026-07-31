# CLI UX Conventions

These are the patterns cq follows for flags, help text, error messages, and output behavior. If you're adding a command or flag, read this first.

## The core idea

Every interaction should answer three questions: what happened, what can I do, and what's next. That applies to help text (what can I do?), error messages (what happened, what's next?), and output formatting (what happened?).

## Help text

### Valid values belong in `--help`

If a flag accepts a fixed set of values, list them right in the help string using `[valid: ...]`:

```
--type <TYPE>       Filter by message type [valid: user, assistant]
--count-by <COL>    Aggregate rows into counts [valid: name, session, project]
--fields <FIELDS>   Extract specific columns [valid: session_id, project, type, ...]
```

The test: can a user figure out what to pass without trying and failing first? If not, the help text needs work.

For values that are dynamic or depend on context, point to where the user can discover them:

```
[NAME]  Filter to a specific tool name (run 'cq tools' to see available names)
--fields <FIELDS>  Extract input fields as columns (fields depend on the tool, see 'cq schema tool_calls')
```

### Help strings should include format hints for non-obvious inputs

```
--since <DURATION>  Time filter (e.g. 7d, 24h, 30m)
--session <ID>      Scope to a session by UUID (prefix match supported)
--project <NAME>    Scope to a project (substring match, e.g. 'myproject')
```

The examples in parentheses do a lot of heavy lifting. They're worth the few extra characters.

## Error messages

### The template

Every validation error follows this structure:

```
Error: <what went wrong, with the invalid input in quotes>
Valid <thing>: <comma-separated list>
Hint: <how to learn more or fix it>
```

The three lines serve different needs. The first tells you what's wrong, the second tells you what's right, the third tells you where to go next. The hint line is optional when the valid values already make the fix obvious.

### Examples

```
Error: Unknown field 'mesage' for messages
Valid fields: session_id, project, type, timestamp, text, model, tool_count
Hint: Run 'cq schema messages' for field descriptions
```

```
Error: Invalid duration 'bogus'
Expected format: <number><unit> (e.g. 7d, 24h, 30m)
```

```
Error: --timeline requires --session
Usage: cq sessions --session <id> --timeline
Hint: Run 'cq sessions' to find session IDs
```

```
Error: --count-by and --fields cannot be used together
--count-by aggregates rows into counts; --fields selects columns from detail rows
```

### What makes a good error

1. **Include the user's input in quotes.** They need to see their typo. "Unknown field 'mesage'" is actionable, "Unknown field" is not.
2. **List valid options.** Don't make them guess or go find docs.
3. **Point somewhere when the fix isn't obvious.** "Run 'cq sessions' to find session IDs" saves a trip to `--help`.
4. **Explain conflicts, don't just reject them.** "Cannot be used together" plus a one-liner about why helps the user pick the right flag.

## Output behavior

### TTY-aware formatting

Truncation keeps terminal output readable, but it destroys data when the output goes to another program. cq detects TTY and adjusts:

| Context | Behavior |
|---------|----------|
| Terminal, no flags | Truncated columns (the default experience) |
| Terminal + `--wide` | Full column values |
| Piped (not a TTY) | Full column values automatically |
| `--json` | Full values, always |

The implementation pattern:

```rust
let wide = cli.wide || !std::io::stdout().is_terminal();
```

There's no `--no-wide` for forcing truncation when piped. If you're piping and want structured output, `--json` is the right tool.

### stderr vs stdout

Progress messages, "no results" notices, and hints go to stderr. Data goes to stdout. This lets piped output stay clean:

```bash
# "Scoped to ~/pickleton" goes to stderr, only data hits grep
cq tools Bash | grep "docker"
```

## Column references

### Friendly aliases

Common columns have short aliases that work everywhere: `session` maps to `session_id`. If a user can write `--count-by session`, they should also be able to write `--fields session`. Keep aliases consistent across all flags that accept column names.

If you add a new alias, make sure it works in every flag that accepts columns. Inconsistency here is worse than not having the alias at all.

### `--fields` across commands

`--fields` means "show only these columns." The implementation varies by command, but the user experience is the same everywhere:

- On **tools**: extracts from the JSON `input` blob (`json_extract_string`)
- On **messages** and **sessions**: selects flat columns by name

Users don't need to think about this difference. They write `--fields text` or `--fields command` and get the data.

## Context flags (`-A`/`-B`/`-C`)

cq mirrors grep's context flags on `tools` and `messages`:

- `-A N`: N messages after each match
- `-B N`: N messages before each match
- `-C N`: N messages before AND after (shorthand for `-A N -B N`)

Context is always counted in messages, not tool calls. When `cq tools` has a context flag, its output becomes message-shaped for the terminal: both Default and `--table` curate down to the same 4 columns `cq messages`'s normal output shows (`session_id`, `type`, `timestamp`, `text`), with `--` separators marking group boundaries in both formats. `--json` is the only format that keeps the full row — all columns, `match_kind`/`match_group`, and (on `cq tools` matches) the tool-specific enrichment fields. `--limit N` limits matches, not total output rows. `--count-by` is incompatible with context flags. Context never crosses session boundaries.

```bash
# What did the agent do right after this Skill invocation?
cq tools Skill --grep 'agent-meta:park' -A 3

# Three messages on either side of a grep hit
cq messages --grep 'compaction' -C 3
```

## Checklist: adding a new flag

When you add a flag to cq, run through this:

- [ ] Does it accept a fixed set of values? List them in `--help` with `[valid: ...]`
- [ ] Does it accept dynamic values? Point to where to discover them
- [ ] What happens with invalid input? Write the error message using the template
- [ ] Does it conflict with other flags? Write a clear conflict error explaining why
- [ ] Does it affect output? Make sure it respects `--wide`, `--json`, and `--table`
- [ ] Is the flag name consistent with existing patterns? (e.g. don't add `--group-by` when `--count-by` exists)
- [ ] If it's user-facing, update the README "Common flags" table
- [ ] If it sets a new UX pattern, add an example to this file

## Checklist: adding a new command

Same as above, plus:

- [ ] Does the command show up clearly in `cq --help`?
- [ ] Does it support `--json` and `--table` output modes?
- [ ] Does "no results" give useful hints about active filters?
- [ ] Does it respect `--limit` and `--offset`?
- [ ] Update the README "Quick start" list
- [ ] If it exposes new data patterns, add an example to `docs/use-cases.md` and to `cq schema --examples` output

## Keeping docs in sync

Docs drift when behavior changes but nobody grep'd the docs. Before considering a change done, find the row that matches and run through the updates.

| If you change... | Update... |
|------------------|-----------|
| A user-facing flag | README "Common flags" table, the flag's `--help` string |
| A subcommand's behavior | README "Quick start", `cq schema --examples` output if query patterns shift |
| A module in `src/` | CLAUDE.md architecture tree |
| Sync / cache / scope behavior | CLAUDE.md "Key patterns"; `docs/design-principles.md` if a default changes |
| Anything about the transcript format on disk (a new record `type`, a layout change, a parsing quirk) | `docs/session-storage.md` |
| A SQL view's columns | `cq schema` output (in `src/commands/schema.rs`); README "Views" bullets if a view is added or removed |
| A default behavior (scope, sync mode, output) | `docs/design-principles.md` and any example that relied on the old default |

Rule of thumb: if you catch yourself writing "the default is X" or "cq does Y" in code, grep the docs for the old wording before committing. A stale doc is a bug report you haven't opened yet.
