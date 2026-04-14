# cq UX Round 2: Output Format Redesign

**Goal:** Replace cq's table-only output with command-specific formats that are scannable, colored, and terminal-friendly. Drop comfy-table. Add color via owo-colors. Keep --json unchanged.

**Scope:** Output rendering only. No changes to SQL views, data model, or query logic.

## Design Decisions

- **One-line is the new default.** Sessions, tool details, and messages render one row per line with column-aligned fields. Tools summary renders a horizontal bar chart.
- **Tables still exist but look better.** `--table` forces a light column-aligned format with a `──────` header separator. No box borders.
- **Per-command renderers, not a generic abstraction.** The bar chart for tools summary doesn't fit a generic column model. Each command owns its rendering. Shared helpers handle color, padding, truncation, and time formatting.
- **comfy-table removed.** Replaced by hand-rolled column alignment in a new `style` module.

## Output Formats by Command

### `cq sessions` (default: oneline)

**Oneline:**
```
16m ago  pickleton  c82e9d4c  16m  29msg  9t  Unpark cq UX improvements session
34m ago  pickleton  bfc27bd2   7m  61msg  23t  pull up my blog playground for the...
46m ago  pickleton  ede19c66  46m  101msg 31t  Unpark boot cache implementation session
```

Color: time ago (dim), project (blue), session ID (yellow), duration (dim), counts (dim), message (default).

**Table (`--table`):**
```
started     project     session_id  dur  msgs  tools  first_user_message
──────────  ──────────  ──────────  ───  ────  ─────  ──────────────────
16m ago     pickleton   c82e9d4c    16m  29    9      Unpark cq UX improvements...
34m ago     pickleton   bfc27bd2     7m  61    23     pull up my blog playground...
```

### `cq tools` summary (default: bar chart)

**Bar chart:**
```
Bash          ████████████████████████████████  10574
Read          ██████████                         3428
Edit          ██████                             1993
TaskUpdate    ████                               1293
Write         ███                                1049
```

Color: tool name (blue), bar (green), count (dim). Shows top N tools (controlled by `--limit`, default 10).

**Table (`--table`):**
```
name        count
──────────  ─────
Bash        10574
Read         3428
```

### `cq tools <name>` detail (default: oneline)

**Oneline:**
```
c82e9d4c  Bash    cargo run --release -- sessions --limit 5  2.3s
c82e9d4c  Read    src/output.rs                              0.1s
```

Color: session ID (yellow), tool name (blue), input (default), duration (dim).

### `cq messages` (default: oneline)

**Oneline:**
```
c82e9d4c  user       16m ago  Unpark cq UX improvements session
c82e9d4c  assistant  16m ago  -
```

**Table (`--table`):**
```
session_id  type       timestamp  text
──────────  ─────────  ─────────  ──────────────────────────────────────
c82e9d4c    user       16m ago    Unpark cq UX improvements session
c82e9d4c    assistant  16m ago    -
```

### `cq sql` (default: light table)

Raw SQL passthrough always uses the light table format (header + separator + aligned columns). `--table` has no effect since it's already a table. `--json` switches to JSON array output as usual. Color applies to the header row only (dim).

### `cq schema`

Pure text output. Unaffected by `--table`, `--no-color`, or any formatting changes. Does not receive `OutputFormat`.

### All commands: `--json`

Unchanged. Pretty-printed JSON array to stdout.

## New CLI Flags

| Flag | Effect |
|------|--------|
| `--table` | Forces light table format (header + separator + aligned columns) |
| `--no-color` | Disables color output |
| `--json` | Existing, unchanged |

`--table` and `--json` are mutually exclusive. If both are passed, `--json` wins. This precedence is enforced in `main.rs` when constructing `OutputFormat`, not by clap's conflict system.

## Color System

**Crate:** `owo-colors` v4 with `supports-colors` feature.

**Semantic palette:**

| Role | Color | Used For |
|------|-------|----------|
| Primary | Blue | Project names, tool names |
| Secondary | Yellow | Session IDs |
| Dim | Gray/dimmed | Timestamps, durations, counts, metadata |
| Bar | Green | Bar chart fill |
| Default | Terminal default | Message text, input text |

**Color disable chain:** The `supports-colors` feature handles TTY auto-detection automatically. On top of that, call `owo_colors::set_override(false)` at startup if `--no-color` flag is set or `NO_COLOR` env var is present. These are two separate mechanisms: the feature detects TTY, the override handles explicit opt-out.

## Text Handling

- **NULL renders as `-`** (dash), not "NULL"
- **Truncation at 60 chars** with `...` suffix for text fields (first_user_message, tool input)
- **Short session IDs:** first 8 characters by default in all non-table formats
- **Relative timestamps:** "16m ago", "2h ago", "3d ago" instead of ISO

## File Changes

### New: `src/style.rs`

Shared formatting helpers:

- `colorize(text, role)` wraps owo-colors with semantic roles
- `relative_time(iso_timestamp)` converts to "16m ago"
- `format_duration(start, end)` converts to "16m", "2h30m"  
- `truncate(text, max)` truncates with "..."
- `null_display()` returns "-"
- `short_id(uuid, len)` first N chars
- `align_columns(rows)` pads to column widths, joins with double-space
- `print_table_header(headers, widths)` header row + `──────` separator
- `print_bar(value, max_value, max_width)` proportional `████` bar

### Modified: `src/output.rs`

Remove comfy-table rendering. Simplify to:
- `print_results()` for raw SQL output (`cq sql`), using the light table style
- `print_json()` unchanged

### Modified: `src/main.rs`

- Add `--no-color` and `--table` flags to clap args
- Call `owo_colors::set_override(false)` at startup when color is disabled
- Pass `OutputFormat` to command handlers

### Modified: `src/commands/sessions.rs`

Add `render_oneline()` and `render_table()`. Command fetches data as rows, dispatches to renderer based on format.

### Modified: `src/commands/tools.rs`

Add `render_bar_chart()`, `render_oneline()`, `render_table()`. Summary mode defaults to bar chart. Detail mode defaults to oneline.

### Modified: `src/commands/messages.rs`

Add `render_oneline()` and `render_table()`.

### Modified: `Cargo.toml`

- Add: `owo-colors = { version = "4", features = ["supports-colors"] }`
- Remove: `comfy-table = "7"`

## Format Dispatch

```rust
pub enum OutputFormat {
    Default,  // each command picks its natural format
    Table,
    Json,
}
```

Each command interprets `Default` as its preferred format: oneline for sessions/messages/tool-detail, bar chart for tool summary. `--table` forces `Table`. `--json` forces `Json`.

## Tests

**Unaffected:** `tests/views_test.rs` (SQL-level, format-independent).

**Updated:** `tests/integration_test.rs` assertions change since output format changes.

**New:**
- `src/style.rs` unit tests for `relative_time`, `truncate`, `short_id`, `align_columns`
- Integration tests verifying `--table` and `--json` flags produce expected formats
- Integration test verifying `--no-color` strips ANSI codes
