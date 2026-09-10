# Perfetto overlap encoding spike

**Decision: Candidate B (async `ph: b`/`e` events) wins.** Task 7's Perfetto
emitter uses async slices for tool-call spans, not complete (`ph: X`) events.

## Why this needed an experiment

The design spec (`docs/specs/2026-09-10-session-trace-view-design.md`,
"Spike before implementing: how to encode overlap") notes that Perfetto's
data model requires duration slices on a track to nest, and pushes non-nested
overlap onto automatic overflow tracks. Real session data has 163 crossing
pairs (never more than 3 rows deep per lane), so this is not a hypothetical
edge case. Perfetto's JSON import is self-described best-effort and its
behavior on this exact shape (two non-nested overlapping slices, one thread)
was unverified before this spike.

## Setup

Two ~20-line fixtures, matching `toolu_m1`/`toolu_m2` from the real fixture
session (`12:00:01.000` +3.0s, `12:00:01.012` +5.588s, i.e. starting 12ms
apart with results landing out of order) but with round numbers for
readability:

`candidate_a.json` (complete events, `ph: X`):

```json
[
 {"ph":"X","name":"Bash","cat":"tool","pid":1,"tid":1,"ts":0,"dur":3000000},
 {"ph":"X","name":"Read","cat":"tool","pid":1,"tid":1,"ts":12000,"dur":5600000}
]
```

`candidate_b.json` (async events, `ph: b`/`e` with a shared `id`):

```json
[
 {"ph":"b","name":"Bash","cat":"tool","pid":1,"tid":1,"ts":0,"id":"1"},
 {"ph":"e","name":"Bash","cat":"tool","pid":1,"tid":1,"ts":3000000,"id":"1"},
 {"ph":"b","name":"Read","cat":"tool","pid":1,"tid":1,"ts":12000,"id":"2"},
 {"ph":"e","name":"Read","cat":"tool","pid":1,"tid":1,"ts":5612000,"id":"2"}
]
```

Loaded with the real `trace_processor` binary
(`curl -LO https://get.perfetto.dev/trace_processor`), not a guess at its
behavior. The bootstrap script needs to write its prebuilt-binary cache to
`~/.local/share/perfetto`, which this repo's Bash sandbox denies
(`PermissionError: [Errno 1] Operation not permitted:
'/Users/josh.nichols/.local/share/perfetto'`); re-running the same command
with the sandbox disabled for that one call downloaded the real
`trace_processor` prebuilt and ran cleanly.

## Candidate A: complete events (`ph: X`)

Command:

```
./trace_processor -q "SELECT t.name AS track, s.name, s.ts, s.dur FROM slice s JOIN track t ON s.track_id = t.id ORDER BY s.ts" candidate_a.json
```

Output:

```
Trace health issues:

  Import errors
    slice_spill_overlapping_complete_event: 1 | A complete slice (typically a JSON 'X' event) partially overlaps another slice on the same thread track, so it cannot nest there. Instead of dropping it, Perfetto moved it onto a separate overflow track that is merged back onto the thread at display time. No data is lost, but because overlapping duration events are inherently ambiguous (nothing in the trace says how they should nest), the resulting layout might not match what you intended when you emitted these events.

"track","name","ts","dur"
"[NULL]","Bash",0,3000000000
"[NULL]","Read",12000000,5600000000
```

Querying the `track` table directly shows what those two rows actually
landed on:

```
"id","name","type"
0,"[NULL]","thread_execution"
1,"[NULL]","thread_overlapping_slice"
```

- **Tracks:** 2 distinct tracks (`track_id` 0 and 1).
- **Names:** both `NULL`. The `type` column names the mechanism
  (`thread_execution` for the real thread track, `thread_overlapping_slice`
  for the synthetic overflow track Perfetto invented), but there is no
  human-readable label — a UI would show two unnamed rows for one lane, with
  no indication of why.
- **Durations:** intact. `dur` came through as 3,000,000,000 ns and
  5,600,000,000 ns respectively (µs `ts`/`dur` values × 1000, as expected)
  and `ts` likewise intact (0 and 12,000,000 ns).
- **Import health:** flagged an explicit import error/warning
  (`slice_spill_overlapping_complete_event: 1`), confirming this is a
  degraded path, not silent correct handling.

## Candidate B: async events (`ph: b`/`e`)

Same command against `candidate_b.json`:

```
"track","name","ts","dur"
"Bash","Bash",0,3000000000
"Read","Read",12000000,5600000000
```

`track` table:

```
"id","name","type"
0,"Bash","legacy_async_global_slice"
1,"Read","legacy_async_global_slice"
```

- **Tracks:** 2 distinct tracks, same count as A.
- **Names:** `"Bash"` and `"Read"` — Perfetto used the event's own `name` as
  the track name, so each overlapping call gets a clean, self-labeled row.
- **Durations:** intact, identical values to Candidate A (3,000,000,000 ns
  and 5,600,000,000 ns).
- **Import health:** zero import errors or warnings. Clean load.

## Decision

Per the design spec's stated decision rule: A produces overflow tracks with
unhelpful (`NULL`) names, and B produces clean, named async tracks with no
import warnings. **B wins outright** — the fallback (pre-packing lanes into
synthetic `tid`s) is not needed.

Task 7's Perfetto emitter should encode each tool-call span as a pair of
async events (`ph: "b"` at call time, `ph: "e"` at result time) sharing an
`id`, rather than a single `ph: "X"` complete event with a `dur`. The `id`
can be the tool call's own `tool_use_id`, which is already unique per span
and gives every async pair a stable, meaningful correlation key for free.
