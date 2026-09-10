# Session trace view

Status: approved (design)
Date: 2026-09-10

## Problem

cq can tell you *what* happened in a session but not *when* or *alongside what*.
`cq tools` gives counts, `cq messages` gives chronology, and neither answers the
questions that actually come up when you stare at a long session:

- where are the bursts of very rapid tool calls, and what are they doing
- which subagents were running concurrently, and which one spawned which
- which tool calls blocked for a long time
- where did the wall-clock time actually go

A flat event log does not answer these. They are trace questions, and they want a
trace viewer: duration bars, parallel lanes, zoom.

An earlier design (`docs/superpowers/specs/2026-04-16-cli-ux-features-design.md`,
Feature 4) specified a `sessions --timeline` flag that printed interleaved
call/result rows. It was never implemented. This design supersedes it: the flat
row list it described cannot express duration, overlap, or lane structure, which
is the entire point.

## On-disk facts

All confirmed against real transcripts on 2026-09-10.

**Durations are recoverable.** Every `tool_use` and every `tool_result` is its own
JSONL record carrying its own `timestamp`. A tool call's duration is
`result.timestamp - call.timestamp`. Verified on session
`a8d21396-8fac-4952-83c5-f1fed9631439`: 1,020 of 1,020 calls paired, zero
unpaired.

**Concurrency is real and modest.** Parallel tool calls appear as adjacent
`tool_use` records ~12ms apart whose results return out of order, so their
intervals genuinely cross. Measured across three sessions (2,918 spans): 163
crossing pairs, 23 properly-nested pairs, and no lane ever needed more than 3
rows to pack without collision.

**Tool execution is a minority of wall clock.** Session `a8d21396` ran 344.9
minutes with 169.3 minutes inside tool spans: 49%. The gaps are half the story,
which is why zoom matters more than bar rendering.

**The spawn tree is exact, not heuristic.** Each plain subagent has a sidecar
`agent-<id>.meta.json`:

```json
{ "agentType": "general-purpose",
  "description": "Implement Task 11: zip, size warning, exit contract",
  "toolUseId": "toolu_01KVBdGrmu7CUcDro8hPy7mT",
  "spawnDepth": 1,
  "requestShape": "background" }
```

`toolUseId` is the `Agent` call that spawned the lane, so the parent edge is a
join against `tool_calls`, not a guess. Verified on session
`73d761d2-290f-456a-a9ab-610fffbac202`: 24 subagent lanes, 3 levels deep, **0
unresolved parent edges**, all 24 spawned by `Agent`.

**Depth is overwhelmingly shallow.** Across all 1,006 subagent lanes on disk:

| spawnDepth | lanes | share |
|---|---|---|
| 1 | 940 | 93.4% |
| 2 | 63 | 6.3% |
| 3 | 2 | 0.2% |

**Workflow subagents are the exception.** The 4 lanes under
`subagents/workflows/wf_<id>/` carry only `{agentType, spawnDepth, model}` — no
`toolUseId`, no `description`. Their parent edge is unresolvable from meta.json;
they group by the existing `workflow_id` column instead. (Consistent with
`docs/specs/2026-06-01-index-subagent-transcripts-design.md`, which noted the
same asymmetry.)

**Human turns are separable from tool plumbing.** Message breakdown for
`a8d21396`:

| type | text | tools | count | meaning |
|---|---|---|---|---|
| user | not null | no | 84 | genuine human turns |
| user | null | no | 1,020 | `tool_result` carriers |
| assistant | not null | no | 370 | prose |
| assistant | null | yes | 1,020 | `tool_use` carriers |
| assistant | null | no | 619 | thinking-only records |

`type = 'user' AND text IS NOT NULL` cleanly isolates human turns, which is what
makes gap classification possible.

## Goals

- One command that renders a session as a trace: lanes, duration bars, gaps,
  markers, and parent→child edges.
- Serve both readers. A human gets a viewer with real zoom; an agent gets span
  rows it can aggregate and summarize.
- Stay a query tool. The trace is a view plus a formatter, not a new subsystem.

## Non-goals

- Live or streaming traces. cq answers questions about sessions that have
  happened (`docs/design-principles.md`).
- Multi-session traces. One session per invocation.
- Token and context-size counter tracks. Deferred until counters prove useful.
- A cq-authored HTML trace viewer. Rejected below.

## Approach: export to Perfetto, don't build a viewer

### What Perfetto is, and why target it

[Perfetto](https://perfetto.dev/) is an open-source tracing and trace-analysis
stack from Google. It is the default tracing system for Android and Chromium, so
it is heavily exercised and unlikely to be abandoned. Three parts matter here:

- **[The UI](https://perfetto.dev/docs/visualization/perfetto-ui)** at
  `ui.perfetto.dev`: a browser-based trace viewer with zoom, pan, span search,
  collapsible track groups, flow-arrow following, and an args inspector. No
  install.
- **[`trace_processor`](https://perfetto.dev/docs/analysis/trace-processor)**: a
  library and CLI that loads a trace and exposes it as **SQL tables**.
- **Ingest breadth**: it reads its own protobuf format plus Chrome JSON, Firefox
  Profiler JSON, Linux `perf`, ftrace, macOS Instruments, and Fuchsia traces.

It is the right target for three reasons.

**Zoom is the hard part and it is already solved.** A session spans hours and
contains sub-second bursts; that is four or five orders of magnitude. Every
renderer cq could plausibly ship (terminal or hand-written HTML) would spend most
of its effort re-implementing pan and zoom badly.

**The mental model already fits.** A session *is* a trace: concurrent lanes of
timed work with causal edges between them. Subagents map to tracks, tool calls to
slices, `Agent` dispatches to flow arrows. Nothing has to be contorted.

**It keeps cq a query tool.** `trace_processor` means the exported trace stays
queryable with SQL, so the export is not a dead-end picture. That is continuous
with `cq sql`, not a departure from it.

The cost is a two-step ritual (emit a file, open it somewhere) and a vocabulary
built for CPU profiling rather than agents: tracks are called "threads," lane
groups are "processes."

### Data handling

Session transcripts contain real work content, so where the trace goes matters.

Perfetto ships
[`open_trace_in_ui`](https://github.com/google/perfetto/blob/main/tools/open_trace_in_ui),
which serves the trace from a local HTTP server and points the UI at
`127.0.0.1`. Since perfetto.dev's servers cannot reach a loopback address, the
browser must be doing the parsing locally in that mode. `trace_processor` is also
a plain local CLI with no network involvement at all.

What is **not** documented anywhere I could find is whether dragging a file onto
`ui.perfetto.dev` is equally local. The docs make no privacy, data-residency, or
client-side-processing statement. Treat that path as unverified and prefer
`open_trace_in_ui` or `trace_processor` until someone confirms it.

### Renderers considered

Three renderers were considered.

**Terminal waterfall alone.** Prototyped against real data. The lane cascade
reads well, but 76 columns across 345 minutes is 4.5 minutes per column, at which
a 12-call burst is a single block. It cannot answer the rapid-calls question
without windowing, and windowing means you are hand-driving zoom from the shell.

**cq generates its own HTML viewer.** Full control of vocabulary, at the cost of
maintaining a trace UI that will lose to Perfetto on zoom, pan, and search for a
long time. Squarely against "cq is a query tool."

**Export a standard trace format.** Chosen. Zoom, pan, span search, flow arrows,
and duration aggregation are the hard parts, and they already exist. cq emits;
Perfetto renders.

The terminal waterfall survives as the default output, because "show me the shape
of this session" is a real and frequent question that should not require a
browser. It is a convenience, not the answer to the zoom problem.

### Format: legacy Chrome JSON, hierarchy mapped onto pid/tid

Perfetto ingests two things: the legacy Chrome Trace Event JSON format, and its
native TrackEvent protobuf.

Verified against
[Visualizing external trace formats](https://perfetto.dev/docs/getting-started/other-formats)
and
[Building synthetic traces with TrackEvent](https://perfetto.dev/docs/reference/synthetic-track-event)
rather than assumed:

| capability | legacy JSON | TrackEvent protobuf |
|---|---|---|
| duration slices | `X`, `B`/`E` | `TYPE_SLICE_BEGIN/END` |
| instant markers | `I` | `TYPE_INSTANT` |
| flow arrows | `s`/`t`/`f` | `flow_id` |
| counters | `C` | yes, with shared Y axis |
| nesting depth | one level, via `pid` | arbitrary, via `parent_uuid` |
| row ordering | **none** | `child_ordering` + `sibling_order_rank` |
| same-named lanes | distinct `tid` keeps them apart | merge by default |
| support posture | legacy, best-effort | native |
| other viewers | `chrome://tracing`, Speedscope | Perfetto only |

Two findings decided this.

`thread_sort_index` and `process_sort_index` **are not supported by Perfetto and
are not planned** ([perfetto-dev
thread](https://groups.google.com/g/perfetto-dev/c/zOe_Y2FxGGk),
[issue #764](https://github.com/google/perfetto/issues/764)). They work in
`chrome://tracing` and do nothing here. So legacy
JSON offers no control over row order at all, which removes the obvious way to
place child lanes under their parents.

But `pid` grouping does render as a collapsible process group. Mapping the tree
onto `pid`/`tid` — `pid` per top-level dispatch, `tid` per lane — recovers exactly
one level of real grouping, and 93.4% of subagents live exactly one level down.
Depth-3 lanes flatten into their grandparent's group; that is 2 lanes in the
entire recorded history.

Protobuf's headline feature is arbitrary-depth nesting, which this data barely
uses, and its default sibling-merging would fuse the 643 lanes named
`general-purpose` into one track unless `SIBLING_MERGE_BEHAVIOR_NONE` is set. It
also costs `prost`, `prost-build`, a `build.rs`, and a vendored subset of
Perfetto's `.proto` files to keep current; the `perfetto` crate on crates.io is a
`0.0.0` placeholder. cq's runtime dependencies today are `duckdb`, `clap`,
`serde`, `serde_json`, `chrono`, `anyhow`, `dirs`, `fs2`, `owo-colors`, with no
protobuf toolchain anywhere.

Legacy JSON adds zero dependencies and zero build steps. The emitter sits behind
the span model, so switching to protobuf later changes one module.

## Data layer

Three changes, each an extension of code that already exists.

### 1. `tool_results` gains `timestamp`

```sql
json_extract_string(json, '$.timestamp') AS timestamp
```

Additive, same shape as every other field in `claude_tool_results_sql()`. This is
the change that makes durations exist. `cq schema` output and the `cq` skill's
inline schema both need updating.

### 2. `file_registry` picks up three fields from meta.json

It already reads `agent_type` from the sidecar at index time. Add:

| field | from | null when |
|---|---|---|
| `parent_tool_use_id` | `toolUseId` | workflow subagents, main loop |
| `spawn_depth` | `spawnDepth` | main loop |
| `agent_description` | `description` | workflow subagents, main loop |

The registry field is `agent_description` to match the existing `agent_type`
prefix convention; the `agents` view exposes it as plain `description`, since the
view's rows are already scoped to one agent each.

### 3. New `agents` view

One row per subagent lane:

```
session_id, project, source, harness, agent_id, agent_type, description,
parent_tool_use_id, spawn_depth, workflow_id, started_at, ended_at,
tool_call_count
```

This makes the spawn tree queryable on its own rather than as a trace-only side
effect: `cq agents --session <id>` answers "which subagent started which" with no
Perfetto involved, and it is the natural home for lane metadata that would
otherwise repeat across all 1,020 span rows.

The alternative — hanging the three columns off `tool_calls`/`tool_results`
alongside `agent_type` — was rejected because it makes the tree reachable only by
`SELECT DISTINCT` over span rows.

## Span and gap model

A **span** is a paired `tool_use` / `tool_result`: lane, tool name, start, end,
duration, error flag, input JSON. Derived by joining `tool_calls` to
`tool_results` on `tool_use_id`, which is what the new `timestamp` column
enables.

A **gap** is dead air on a lane, classified by what bounds it:

- **think gap** — tool result at T, next tool call at T′ on the same lane, no
  human turn between. The model generating.
- **human gap** — assistant emits prose and stops; next record is a genuine user
  turn (`type = 'user' AND text IS NOT NULL`). Blocked on the human.

Both computed with `LAG`/`LEAD` partitioned by lane and ordered by timestamp.
Timestamps are fixed-width UTC strings, so lexical order is chronological and no
cast is needed except for the subtraction itself.

### Known ambiguity: slow tool vs unanswered permission prompt

The `tool_use` record is written when the assistant message arrives, before any
permission prompt is answered. So a long span in the main loop may be a slow tool
or a prompt sitting unanswered, and the two are indistinguishable in the
transcript.

Two spans over 900s were investigated on `a8d21396` (a `Read` at 941s, a `Bash`
at 928s). In both cases call and result are adjacent records, so no data is
missing and the durations are real. Both are in subagent lanes, where no prompt
can occur, so those two are genuine.

The design does not attempt to label this. Durations are emitted as measured.

## CLI surface

```
cq trace --session <id>              # terminal waterfall (default)
cq trace --session <id> --perfetto   # Chrome Trace JSON on stdout
cq trace --session <id> --json       # span rows (existing global flag)
cq trace --session <id> --from +12m --to +17m      # window the waterfall
```

`--from` and `--to` take an offset from session start (`+12m`, `+90s`) or an
absolute ISO timestamp. Offsets are the common case, since the interesting window
is usually found by looking at the full-session waterfall first.

`--session` is required, following the error template in
`docs/cli-ux-conventions.md`:

```
Error: cq trace requires --session
Usage: cq trace --session <id>
Hint: Run 'cq sessions' to find session IDs
```

`--perfetto` is a boolean rather than a `--format <FMT>` enum, because an enum
would collide with the existing global `--json` and `--table` flags. One
mechanism per concern.

## Perfetto mapping

| trace concept | cq |
|---|---|
| `ts` | epoch microseconds (the format requires µs; transcript timestamps are ms-precision) |
| `pid` 1 | main loop |
| `pid` 2..N | one per depth-1 subagent, containing it and its descendants |
| `tid` | one per lane |
| `M` `process_name` | `agent_type` plus `description` |
| `M` `thread_name` | lane label with depth suffix |
| duration slice | `name` = tool name, `args` = input JSON verbatim, `dur` = µs |
| gap slice | `cat: "gap"`, named `think` or `blocked on you` |
| skill / hook | `ph: i` instant, thread-scoped |
| parent → child | flow `s` on the parent's `Agent` span, `f` on the child's first span, shared `id` |

Workflow subagents have no resolvable parent, so they get no flow arrow and group
under a `pid` derived from `workflow_id`.

### Spike before implementing: how to encode overlap

Perfetto's data model requires duration slices on a track to nest, and pushes
non-nested overlap onto automatic overflow tracks. The measured data does overlap
(163 crossing pairs) but stays shallow (never more than 3 rows per lane).

Two encodings are viable:

1. **Async slices** (`ph: b`/`e` with a shared `id`). The format-sanctioned answer
   for overlapping work.
2. **Pre-packed sub-rows.** Greedily pack each lane into synthetic `tid`s so no
   two slices on a row collide. Deterministic, and we control the labels.

Perfetto's JSON support is self-described best-effort, and how it renders legacy
async events for this shape is unverified. Resolve with a ~20-line fixture and one
file load before committing to either. Default to async; fall back to pre-packing
if the rendering is poor.

## Testing

- View tests in `tests/views_test.rs` for `tool_results.timestamp` and for the
  `agents` view, including null `parent_tool_use_id` on a workflow subagent.
- A fixture session engineered to contain: two overlapping parallel calls, a
  depth-3 subagent, an error result, and a human gap.
- A golden-file test on the emitted trace JSON.
- One manual load of the golden file into Perfetto. A file that is
  schema-shaped but silently mangled on import passes every automated test, so
  this check cannot be skipped on the first implementation.

## Implementation order

1. `tool_results.timestamp`, with view tests. Everything else depends on it.
2. `file_registry` fields and the `agents` view.
3. The span and gap model, exposed via `cq trace --json`.
4. Terminal waterfall.
5. The overlap-encoding spike, then the Perfetto emitter.
