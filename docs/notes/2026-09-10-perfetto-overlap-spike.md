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

## Task 7 implementation: verification against the real emitter

The emitter (`src/trace/perfetto.rs`) was built per the decision above:
tool-call spans as `ph: "b"`/`"e"` pairs keyed by `tool_use_id`, gaps as
`ph: "X"` complete events. Verified against real output from the emitter
code path, not a hand-written fixture -- ran `cargo run -- --session
a1b2c3d4-0000-4000-8000-000000000001 trace --perfetto` against a temp
project tree seeded with the `TRACE_SESSION` fixture (same layout
`setup_env_tree` builds in `tests/integration_test.rs`), redirected to
`cq-trace.json`.

### `trace_processor` check: pass

Used the `trace_processor` prebuilt already cached from the spike above
(`~/.local/share/perfetto/prebuilts/`); the wrapper script
(`curl -LO https://get.perfetto.dev/trace_processor`) needed no sandbox
override this time since the cache already existed. Note the CLI syntax
changed since the spike ran (`v58.2` uses `trace_processor query <trace>
<sql>`, not `-q <sql> <trace>`).

Track/type breakdown:

```
"type","count(*)"
"legacy_async_global_slice",6
"thread_execution",3
```

Zero rows for `SELECT * FROM stats WHERE value > 0 AND (name LIKE
'%spill%' OR name LIKE '%overlap%' OR severity='error')` -- no
`slice_spill_overlapping_complete_event`, no import errors of any kind.

The two known-overlapping fixture spans, confirmed directly from the
emitted JSON: `toolu_m1` (`Bash`, ts 1789041601000000, ends
1789041604000000) and `toolu_m2` (`Read`, ts 1789041601012000, ends
1789041606600000) -- 12ms apart, 3s vs 5.588s duration, same `pid`/`tid`
(1/1), same shape as the spike's synthetic candidates. They land on two
distinct, cleanly-named `legacy_async_global_slice` tracks (`"Bash"` and
`"Read"`), not merged onto any `[NULL]`/`thread_overlapping_slice`
overflow track. Matches Candidate B's behavior exactly.

Track count matches lane count: 3 `thread_execution` tracks, one per lane
(`main`, `agent-sub1`, `agent-sub2` -- confirmed via `thread.name`, which
is where the human-readable name actually lives for a `thread_execution`
track; `track.name` itself reads `[NULL]` for these, which is normal
Perfetto behavior, not a defect -- the *async* tracks are the ones whose
`track.name` is the tool name). A 4th synthetic thread (`tid: 0`, `name:
[NULL]`) is Perfetto's own bookkeeping for the `pid`-level `process_name`
metadata event and isn't one of the fixture's lanes. Slice count (23) is
sane: 7 async span slices + 16 gap complete-event slices. Duration sums
recovered from the async tracks (`5788 + 50000 + 1500 + 1000 + 3000 =
61288`, in the query's mislabeled "secs" column -- `dur` is nanoseconds,
so `/1e6` is actually milliseconds, a unit mistake in the ad hoc query
itself, not in the emitter) match the fixture's known total tool-call
duration of 61288ms exactly.

Args placement: confirmed by inspecting the emitted events directly
(`grep`/`python3 -m json.tool` on the output) rather than via a
`trace_processor` args query -- the `args` object is present only on the
`ph: "b"` event of each pair, matching what the skeleton already did.
`trace_processor`'s own display (Marker Chart, below) surfaced the tool
name and category correctly from that placement, which is the only
consumer-visible confirmation this spike needed; a dedicated
`slice.arg_set_id` query on the `e` event wasn't run separately since the
JSON simply has no `args` key there to query.

### Firefox Profiler check: verified

Loaded the actual `profiler.firefox.com` web app (via Chrome, using
browser automation -- not the Firefox browser itself, but the same tool
this check is about, which is browser-agnostic) and drag-and-drop/file-
uploaded the real `cq-trace.json` output. It auto-detected the format as
"Chrome Trace" (page title became "Chrome Trace – Firefox Profiler") and
rendered a 1m43s full-range timeline with all three lanes (`main` PID 1,
`agent-sub1`, `agent-sub2`) as separate tracks. Switching to the Marker
Chart tab populated real rows: a `gap` category with `blocked on you` and
`think` sub-rows showing bars, and a `tool` category with `Agent`,
`Bash`, `Bash (error)`, `Read` sub-rows each showing bars at the expected
positions. Screenshot captured during the session at
`/Users/josh.nichols/Library/Caches/superpowers/browser/2026-09-10/session-1789067396890/004-click.png`
(a local, ephemeral browser-automation cache path, not part of this
repo).

## Task 9 implementation: `trace_processor` re-verification of pid grouping

Task 9 replaced the fixed `pid = 1` with `trace::lane_groups`, so `pid` now
means a top-level dispatch (main, or a depth-1 subagent and everything it
spawned) instead of the whole session sharing one process. Re-ran the same
kind of check Task 7 did, against real emitter output, not a hand-written
fixture.

Seeded a temp project tree (`CQ_PROJECTS_DIR` pointed at it, `CENV_BASE`
pointed at a nonexistent path to keep the real host's transcripts out of
the sync -- omitting that the first time round pulled in 279 unrelated
real session files) with the `TRACE_SESSION` fixture's main + `agent-sub1`
+ `agent-sub2` files, then ran:

```
cargo run -- --session a1b2c3d4-0000-4000-8000-000000000001 trace --perfetto
```

and fed the output to the same `trace_processor_shell` prebuilt cached from
Tasks 6/7 (`~/.local/share/perfetto/prebuilts/`).

### Process count: 2, not 1

```sql
SELECT upid, pid, name FROM process ORDER BY pid;
```

```
"upid","pid","name"
0,0,"[NULL]"
2,1,"session a1b2c3d4"
1,2,"agent-sub1 (general-purpose)"
```

`upid`/`pid` 0 is `trace_processor`'s own synthetic bookkeeping process (the
same kind of artifact Task 7 noted for `tid: 0`), not one of the fixture's
lanes. The two real processes are pid 1 (`main`'s group, named from the
session id) and pid 2 (`agent-sub1`'s group, named from its own
`agent_type`) -- exactly the fixture's two depth-1 dispatches: `main` itself
never dispatches anything besides `agent-sub1`, and `agent-sub2` is a
depth-2 lane spawned from inside `agent-sub1`, so it does not get a third
process of its own.

No import errors or spill/overlap warnings: `SELECT name, value FROM stats
WHERE value > 0 AND (name LIKE '%spill%' OR name LIKE '%overlap%' OR
severity='error')` returns zero rows, same clean result as Task 7.

### Collapsing pid 2 folds `agent-sub2`'s slices under it

```sql
SELECT p.pid, t.tid, t.name AS thread_name, COUNT(*) AS slice_count
FROM slice s
JOIN thread_track tt ON s.track_id = tt.id
JOIN thread t ON tt.utid = t.utid
JOIN process p ON t.upid = p.upid
GROUP BY p.pid, t.tid, t.name
ORDER BY p.pid, t.tid;
```

```
"pid","tid","thread_name","slice_count"
0,0,"[NULL]",0
1,1,"main",10
2,2,"agent-sub1",4
2,3,"agent-sub2",2
```

Both `agent-sub1` (tid 2, 4 slices) and `agent-sub2` (tid 3, 2 slices) sit
under pid 2. Since a UI's "collapse process" acts on every thread sharing
that `pid`, collapsing pid 2 folds `agent-sub2`'s slices in with
`agent-sub1`'s rather than leaving them as a separate top-level row --
confirmed directly from this grouped count, not by opening a UI. That is
the actual point of Task 9: `agent-sub2`'s tool calls are counted under the
same process as the lane that spawned it, not under their own.
