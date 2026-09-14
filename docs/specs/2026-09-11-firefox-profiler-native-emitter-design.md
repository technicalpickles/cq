# Firefox Profiler native emitter

Status: approved (design)
Date: 2026-09-11

## Problem

`cq trace --perfetto` emits Chrome Trace Event JSON, tagging every event with a
category (`src/trace/perfetto.rs`: `"cat": "tool"` for tool spans, `"cat": "gap"`
for gaps). Firefox Profiler's own Chrome Trace importer already loads this file
(it's one of the two viewers the existing design doc,
`docs/specs/2026-09-10-session-trace-view-design.md`, targets), but every marker
renders grey in the Marker Chart regardless of category.

Confirmed as an importer limitation, not a cq bug (issue #49): the importer's
`getOrCreateCategoryIndex` (`firefox-devtools/profiler`'s
`src/profile-logic/import/chrome.ts`) hardcodes `color: 'grey'` for every category
it synthesizes from a Chrome Trace Event's `cat` field, no matter the name. There
is no field cq can add to the Chrome Trace JSON that reaches a real color, because
category-to-color mapping for imported traces is owned entirely by the importer.

### What's *not* still a problem

Issue #49 also floated per-marker coloring via `MarkerSchema.colorField`, and a
related open concern (issue #46) about `args` being invisible in Firefox Profiler
at all. Both turned out to be already handled by tooling that exists independently
of this work:

- **`args.detail` already ships** (issue #46's fix, merged). A tool span's `input`
  renders as a string under `Fields: Details:` in Firefox Profiler's UI and in
  `profiler-cli`.
- **`profiler-cli`** (`@firefox-devtools/profiler-cli`, aka `pq`) is a real,
  Mozilla-maintained terminal daemon+query tool shipped from the same repo as the
  web UI. It goes through the exact same import pipeline
  (`unserializeProfileOfArbitraryFormat`) as the browser, so it already loads a cq
  Chrome Trace export today with zero cq-side changes. Verified against a real cq
  session export on 2026-09-11:

  ```
  $ profiler-cli thread markers --list --search Bash
  m-1  Bash  t=6.310s  5.027s  {"command":"npx ...","description":"Get vault info..."}

  $ profiler-cli marker info m-1
  Type: EventWithDetail
  Fields:
    Details: {"command":"...","description":"..."}
  ```

  This directly undercuts the "no SQL, no CLI, no batch mode" reasoning the
  2026-09-10 design doc used to reject Firefox Profiler as a *primary* viewer —
  that reasoning no longer holds for querying and detail visibility, though the
  data-model mismatch (samples/stacks vs. this data's all-markers shape) that
  doc also raised is unaffected either way and still stands.

So the only concrete gap left, after profiler-cli is accounted for, is the
original visual one: **tool spans and gaps look identical in the browser's Marker
Chart / timeline**, because color never reaches the page.

## Goals

- Tool spans and gaps render in visually distinct colors in Firefox Profiler's
  Marker Chart and timeline, when loading a cq-exported trace in the browser.
- Stay additive: no change to the existing `--perfetto` (Chrome Trace) output or
  the span/gap model it's built from.

## Non-goals

- Replacing the Chrome Trace / Perfetto emitter. Perfetto remains the primary
  target per the 2026-09-10 design doc (SQL via `trace_processor`, real zoom, a
  data model that actually fits slices-on-tracks). This work is additive.
- Structured, per-tool-name field display (e.g. `Command:` / `Description:` as
  separate labeled rows instead of one JSON blob). Firefox Profiler's
  `MarkerSchema.fields` is declared once per marker *type*, at schema-registration
  time — not computed per marker instance — so getting a labeled `Command:` row
  for `Bash` means hardcoding, in Rust, that a `Bash` marker's data has a
  `command` key, and repeating that per tool. `perfetto.rs`'s own doc comment
  already explains why the existing emitter stays generic instead: `input`'s
  shape "isn't a documented contract any more than the transcript format itself
  is," and cq supports more than one harness (Claude, Codex) with different tool
  shapes, so a fixed per-tool field map is never complete, just progressively
  less incomplete. Scoped out; the blob field via `profiler-cli`/the sidebar
  already carries the same information.
- Per-literal-tool-name colors. A real 1,020-call reference session
  (`a8d21396`, cited in the 2026-09-10 doc) already has 11 distinct tool names,
  past Firefox Profiler's fixed 10-value `GraphColor` palette
  (`src/types/profile.ts`: blue, green, grey, ink, magenta, orange, purple, red,
  teal, yellow). MCP tools push that further. A fixed, small taxonomy (below)
  stays under the palette by construction and needs no maintenance as new tool
  names show up.
- `colorField`-based per-marker dynamic coloring. Category-index coloring
  (`meta.categories` + a `category` index per marker) covers the stated goal;
  `colorField` would only matter for finer distinctions like highlighting error
  spans independently of category, which isn't asked for here.

## Approach: a new sibling emitter, not a Chrome Trace change

`src/trace/perfetto.rs`'s own doc comment already anticipated this: "the
emitted trace format is the part of this feature most likely to need
replacing... isolated on purpose." A new module, `src/trace/firefox_profiler.rs`,
sits next to it, reading the same `Span`/`Gap` model from `trace/mod.rs`. No SQL
changes. No changes to the Perfetto/Chrome-Trace path.

Rather than importing via Firefox Profiler's Chrome Trace path (where the color
bug lives), this emits Firefox Profiler's **native processed-profile format**
directly (`docs-developer/processed-profile-format.md`, typed at
`src/types/profile.ts` in the profiler repo). That format is not a documented
importer target in the usual sense — the browser loads it directly, recognizing
it via `meta.preprocessedProfileVersion` — but it's the one place cq can actually
set marker colors, because coloring for a native profile comes from
`meta.categories` (an array of `{name, color, subcategories}`) plus each marker's
`category` field, an index into that list — not from anything the Chrome Trace
importer's fixed schema construction can be influenced to do.

## Category taxonomy

Fixed buckets, not literal tool names, to stay under the 10-color palette
regardless of how many distinct tools or MCP servers a session used:

| Category   | Color   | Covers                                              |
|------------|---------|------------------------------------------------------|
| Bash       | orange  | `Bash`                                                |
| File ops   | blue    | `Read`, `Edit`, `Write`                               |
| Search     | purple  | `Grep`, `Glob`, `ToolSearch`                          |
| MCP tool   | teal    | any name prefixed `mcp__`                             |
| Other tool | magenta | everything else (`Agent`, `Skill`, `Monitor`, future/Codex-native tools — the catch-all, so a new tool never needs a code change to render *some* color) |
| Think gap  | grey    | `GapKind::Think`                                      |
| Human gap  | green   | `GapKind::Human`                                      |

7 buckets, 3 colors (red, ink, yellow) unused and available if a future
distinction is worth adding.

`is_error` keeps its existing behavior — a `" (error)"` suffix on the marker
name (same as `perfetto.rs` today) — rather than a separate color, avoiding two
overlapping coloring mechanisms for one span.

## Marker fields

One schema type for tool spans, one for gaps (not one per literal tool name,
per the field-structure non-goal above):

- **Tool span**: `name` = tool name (with the `(error)` suffix as needed),
  `category` from the table above, one generic `fields` entry (`key: "input"`,
  `format: "string"`) holding the same JSON-string blob `perfetto.rs` already
  puts at `args.detail` today.
- **Gap**: `name` = `"think"` or `"blocked on you"`, `category` from the table
  above, one generic `fields` entry for the closing text when present (same
  content `perfetto.rs` puts at `args.detail` for gaps today).

This carries over the *content* of the existing `args.detail` fix (issue #46)
unchanged — the improvement here is entirely that the marker now has a real
`MarkerSchema` (so Firefox Profiler renders it as `Type: <ours>` with a proper
`Fields:` section) and a real category (so it colors), not that the field content
changes.

## CLI surface

Supersedes the CLI-surface reasoning in the 2026-09-10 design doc, which chose a
boolean (`--perfetto`) over a `--format` enum specifically because there were
only two renderer choices at the time and an enum seemed unnecessary next to the
global `--json`/`--table` flags. With a third renderer now in play, the enum is
worth it:

```
cq trace --session <id>                     # terminal waterfall (default)
cq trace --session <id> --format perfetto          # Chrome Trace JSON on stdout
cq trace --session <id> --format firefox-profiler  # Firefox Profiler native JSON on stdout
cq trace --session <id> --perfetto                 # deprecated alias for --format perfetto
```

`--perfetto` is already shipped, so it stays rather than being removed: parsed
as a legacy boolean that, when present, behaves as if `--format perfetto` had
been passed. `--format waterfall` is accepted but redundant with the no-flag
default, for symmetry.

The global `--json` flag is unaffected and still wins over any renderer choice,
per existing behavior in `commands/trace.rs`.

## Risks

**Version coupling.** The processed-profile format is versioned against
`PROCESSED_PROFILE_VERSION` (60+, actively incremented as the profiler evolves —
72 as of 2026-09-11) and is internal to that project's own JS, not a published
external spec the way Chrome Trace JSON is. This is the same shape of longevity
risk the 2026-09-10 design doc used to justify rejecting Perfetto's TrackEvent
protobuf in favor of Chrome Trace JSON — here it's unavoidable, since coloring
is only reachable through the native format. Mitigated the same way that doc
mitigates the Chrome-Trace-JSON-deprecation risk: this emitter is one isolated
module behind the shared span/gap model, so a future version bump is a
same-module fix, not a design change.

**No SQL, no batch mode, still true for the *browser* UI.** `profiler-cli`
closes this gap for command-line use, but the browser viewer itself still has
none of Perfetto's `trace_processor`/`batch_trace_processor` querying. This
emitter is explicitly not trying to make Firefox Profiler a competitor to
Perfetto as primary viewer — see Non-goals.

## Testing

- Unit tests on `firefox_profiler::build_profile` (or equivalent), mirroring
  `perfetto.rs`'s existing test style — assert on the structured profile object,
  not stdout.
- A golden-file test on the emitted JSON, same pattern as planned for the
  Perfetto emitter.
- One manual load into a locally-built Firefox Profiler UI (per the 2026-09-10
  doc's local-only guidance — self-hosted build or `profiler-cli`, not the
  hosted page, since a real trace holds verbatim tool inputs). Confirm the
  Marker Chart actually shows distinct colors per category — a file that's
  schema-shaped but silently misses a required field (e.g. no samples on a
  thread) can still pass every automated test and fail to render.
- Re-run the same `profiler-cli thread markers`/`marker info` check used to
  validate this design against the new format's output, to confirm nothing
  regressed relative to the Chrome Trace path.

## Implementation order

1. `src/trace/firefox_profiler.rs`: build the processed-profile JSON structure
   from `Span`/`Gap`, with the fixed category taxonomy and generic fields.
2. Golden-file + unit tests.
3. `--format` enum on `cq trace`, `--perfetto` as a deprecated alias mapping to
   `--format perfetto`.
4. Manual verification: load into a local Firefox Profiler build, confirm colors;
   cross-check with `profiler-cli`.
5. Update `docs/cli-ux-conventions.md`'s docs-sync table entries and the README
   flag reference for the new `--format` flag, per `CLAUDE.md`'s "Keeping docs in
   sync" checklist.
