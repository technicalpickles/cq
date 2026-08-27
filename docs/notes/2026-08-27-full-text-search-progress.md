# Full-text search progress

Status: design decisions resolved and implemented; draft PR nearly ready
Date: 2026-08-27

This note records the first lexical full-text-search slice for
[issue #39](https://github.com/technicalpickles/cq/issues/39), the results of
testing it against the issue's motivating example, and the design decisions
reached while reviewing it. It is a progress record, not a final design.

## What we implemented

Commit `1e93ec7` adds `cq search <QUERY>` using DuckDB's FTS extension and BM25
ranking. The command supports the existing global scope controls, JSON and table
output, limit and offset, and `--type user|assistant` filtering.

DuckDB cannot build an FTS index over the composed `messages` view, so the
implementation copies searchable message text into a physical
`cq_fts_messages` snapshot. `PRAGMA create_fts_index` builds an index over that
snapshot with Porter stemming and English stopwords. Search results include a
BM25 score and retain enough message and session metadata to locate the source
conversation.

The snapshot and index are created lazily. `cache_meta.fts_sync_at` records the
transcript sync generation covered by the current FTS snapshot. `cq search`
rebuilds the snapshot and index when that marker differs from `last_sync_at`;
other subcommands do not load or refresh the FTS extension.

The change also includes CLI and schema documentation plus an integration test
covering stemming (`listing` matching `list`), scores, type filtering, and a
refresh after a transcript changes.

Draft PR: [#41](https://github.com/technicalpickles/cq/pull/41)

## Verification completed

The implementation passed:

- `cargo test` (236 tests)
- `cargo clippy --all-targets -- -D warnings`
- `cargo fmt -- --check`

We also tested against the real transcript example that motivated issue #39,
using an isolated temporary cache so the normal cq cache was not changed. The
temporary cache was removed after the test.

On the current local corpus, the known historical session behaved as follows:

- Exact grep for `cold read` returned 30 messages. The target session appeared
  at rank 12 because grep is ordered by time rather than relevance.
- `cq search "cold read"` returned many matches because DuckDB FTS uses
  disjunctive (OR) term matching by default, but BM25 ranked the target session
  first.
- The issue's longer natural-language query with `--type user` ranked the target
  session third overall and first after excluding the current diagnostic
  conversation, whose messages repeated the query terms.
- The original five manually chosen grep variants found the target at rank 3,
  but only after guessing phrases to OR together.

This demonstrates a useful lexical-search improvement: a single natural-language
query can rank the motivating result highly without manually assembling several
grep expressions. It does not demonstrate semantic similarity. Queries that use
different vocabulary from the source still require embeddings or another
semantic retrieval layer.

## Performance observed

All numbers below come from one local corpus on one machine with a warm disk
cache. They describe shape, not stable performance targets.

The isolated FTS benchmark covered roughly 1,400 transcript files and 132,000
messages. About 22,500 non-empty messages entered the FTS snapshot.

| Operation | Approximate time |
|---|---:|
| Initial transcript sync (empty cache) | 61 seconds |
| Initial FTS snapshot and index build | 33 seconds |
| Cached search with a fresh index | 1 second |
| FTS rebuild after a handful of changed files | 10 seconds |

### Where a normal invocation actually spends its time

Measured separately, on a 1,289-file cache:

| Step | Time |
|---|---:|
| Binary startup (`cq schema`, no DB) | 0.01s |
| DB open + view composition + sync fast path (`cq sql "SELECT 1"`) | 0.09s |
| `cq tools --limit 1 --all` | 0.62s |
| `cq sql "SELECT count(*) FROM messages"` | 1.36s |
| `cq messages --limit 1 --all` | 3.80s |
| `cq sessions --limit 1 --all` | 4.75s |

Transcript sync is 2 to 5 percent of a typical command. The remainder is the
composed views extracting JSON out of `raw_records` at query time, on every
query, from scratch. `--no-reindex` skips sync entirely and is not measurably
faster than the default, which confirms sync is not the bottleneck.

Two consequences:

- Search against a warm index (1s) beats `cq messages` (3.8s) because
  `cq_fts_messages` is a physical table with the text already extracted. The
  snapshot exists because DuckDB cannot index a view, but it is also a
  materialization win.
- A 10-second FTS rebuild lands on top of a sub-second baseline, so it costs two
  to three times an entire normal cq command. That makes avoiding unnecessary
  rebuilds more valuable than the raw number suggests.

### What `--reindex` actually does

It is a full cache rebuild, and the mechanism is not where you would look for
it. `do_sync` (`src/indexer.rs:166`) takes no `SyncMode` parameter, so reading
the indexer alone suggests Force cannot re-parse unchanged files. The work
happens one layer up: `db.rs:50` derives `force_rebuild` from `SyncMode::Force`,
and `cache::open` then calls `rebuild()`, which drops `raw_records`,
`file_registry`, `cache_meta`, and the FTS schema and table. Against an emptied
registry every file looks new, so everything is re-parsed. Measured cost is
about 18.5 seconds against about 5 seconds for the default path.

Worth knowing before testing anything in this area: `--reindex` destroys the FTS
index rather than aging it, so it cannot be used to simulate a stale index. A
test that reaches for it will always observe a cold rebuild instead.

## Which commands pay the refresh cost

The recurring FTS rebuild cost is currently isolated to `cq search`.

| Invocation | Transcript sync | FTS refresh |
|---|---|---|
| `cq search ...` | Automatic sync | Only if the data moved *and* the index is older than `CQ_FTS_MAX_AGE`; otherwise serves stale and says so |
| `cq --no-reindex search ...` | Skipped | Skipped. Errors if no index exists at all |
| `cq --reindex search ...` | Force rebuild (18.5s) | Always, since the cache rebuild drops the index |
| Other database-backed subcommands | Their existing sync behavior | Never |
| `cq schema` | Bypasses transcript sync | Never |

Adding `fts_sync_at` and `fts_built_at` bumped the main cache schema from version
4 to 5, so the first database-backed command run against a version 4 cache
rebuilds that cache even if the command is not `search`. This is a one-time cost,
separate from the recurring FTS refresh cost, and it is accepted (see decision 1).

## Usage evidence

Everything in the freshness decision rests on measured usage, gathered by
querying cq's own transcripts. 479 cq invocations across 59 sessions, main
source, all projects:

| Question | Answer |
|---|---|
| Invocations that targeted their own live session | 0 of 479 |
| Explicit `--session` filters | 32, all pointing at other sessions |
| Shortest `--since` window ever requested | `24h` (once); `7d` is most common |
| Median gap between consecutive cq calls in a session | 16 seconds |
| Follow-up calls within 30s of the previous | 289 of 420 (69%) |
| Within 60s | 327 of 420 (78%) |

Two readings. There is no demonstrated demand for real-time freshness: nobody
has ever pointed cq at the conversation they were sitting in, and the tightest
window anyone asked for is a full day. And usage is extremely bursty, which is
the best case for a staleness window: a burst of eight searches 16 seconds apart
is eight rebuilds under always-refresh and one rebuild under a 60-second window.

Two caveats. All 479 calls predate `search`, so this is inferred from `sql`,
`sessions`, and `tools` behavior. Search could invite a different habit. Issue
39's motivating case (find a session from weeks ago) is more archival than the
average query here, not less, so the pattern most likely holds.

And these are a snapshot, not fixed values. Re-running the queries below will
give different totals, partly because the corpus grows and partly because the
act of measuring adds cq invocations to it. The session that gathered these
numbers pushed the total from 479 to 496 and the median gap from 16s to 17s
while it ran. Treat the shape as the finding, not the digits. For reference at
the same moment, 10 of those invocations used `--reindex`, which is the baseline
for monitoring item 2 below.

## Decisions

### 1. Keep the FTS marker on `cache_meta`, accept the version bump

Considered and rejected: moving `fts_sync_at` to its own lazily-created table to
avoid the one-time full cache rebuild for users who never run `cq search`.

Separate-table is worse on two counts once the one-time cost is set aside.
Adjacency gives free invalidation, because a version bump recreates `cache_meta`
with the marker at its `-1` default, so the marker dies with the data it
describes. A separate table survives a rebuild unless `rebuild()` explicitly
drops it. Separate-table also needs `CREATE TABLE IF NOT EXISTS` ceremony on
every `prepare()` call.

The decoupling argument does not hold either: `src/cache.rs` already drops
`fts_main_cq_fts_messages` and `cq_fts_messages` in `rebuild()`, so it knows
about FTS today. A separate table adds a third line to that same drop list.

The remaining argument is the one-time rebuild, and it is weak on its own. The
cache is derived data, so nothing is lost but time; cq is at 0.5.0 and has
already shipped four such bumps; and anyone who runs `search` pays the 33-second
index build regardless.

Since version 5 has not shipped, `cache_meta` can be shaped freely on this
branch and the whole feature still lands as a single bump. The TTL needs a
wall-clock `fts_built_at` alongside the data revision, since those answer
different questions.

### 2. Track data changes, not completed scans

`sync_sources` writes `last_sync_at = now_ns` unconditionally once it takes the
lock, so a scan that changes nothing still invalidates the FTS snapshot. Replace
the comparison with a data revision that advances only when rows are added,
changed, or removed:

```rust
if agg.added + agg.changed + agg.removed > 0 {
    cache::bump_data_revision(conn)?;
}
```

Two things to keep in perspective.

This buys less than it appears. The `indexer.rs:61` early return already bails
before the lock when no directory mtime advanced, and it does not write the
watermark. A false bump needs a directory mtime to move without any indexed
`.jsonl` changing, which is uncommon. Meanwhile the dominant rebuild trigger is
a true positive: running `cq search` from a live session means that session's
own transcript just changed. The rebuild fires either way, correctly. The
staleness window in decision 3 is what addresses that, not this.

The forced-rebuild path survives this change, but only by accident, so it should
not be left resting on one. `--reindex` drops the FTS schema and table outright
as part of the cache rebuild described above, so `search_objects_exist()` returns
false and `prepare()` rebuilds no matter how freshness is compared. That holds
whether the comparison uses a timestamp or a data revision.

`SyncMode` is threaded into `prepare()` anyway, with an explicit
`SyncMode::Force => rebuild` arm. It is redundant with the drop today, and it is
there so that a future change to what `--reindex` clears cannot quietly leave a
stale index unfixable by any flag.

### 3. Bounded staleness, 5 minute default

Chosen policy: auto-refresh when the index is missing or older than a wall-clock
TTL, serve stale otherwise, and always report the staleness.

Wall-clock age is the right axis, and change count is the wrong one. The
dominant source of change is the caller's own live session appending, which is
precisely the data with zero demonstrated demand. Counting changes would make
the index churn hardest for the content nobody reads.

The flag surface already exists and does not need extending:

| Flag | Transcript sync | FTS |
|---|---|---|
| `--reindex` | Force (18.5s) | Force rebuild, explicitly (see decision 2) |
| *(default)* | Auto, mtime fast path | TTL, 5m |
| `--no-reindex` | Skip | Skip |

Making `--no-reindex` skip the FTS refresh fixes an existing wart: today it
skips the transcript sync and then rebuilds the index anyway if the generations
differ, which is a 10-second surprise from the flag whose entire pitch is
"fastest."

Because `--reindex` and `--no-reindex` already express always-refresh and
never-refresh, `CQ_FTS_MAX_AGE` only needs to tune the middle case. It does not
need `0` or `never` sentinel values, which also means `since_timestamp`'s
existing `d`/`h`/`m` grammar works unmodified.

A `search --refresh` flag was considered, for "rebuild the index but skip the
full stat walk." It was dropped: with `--reindex` measured at 18.5s rather than
the assumed 61s, the flag saves roughly 13 seconds on an operation the usage
data says is rare. Easy to add later if the monitoring below shows demand.

### 4. Report staleness, do not infer it from results

cq reports on stderr whenever the index is behind, with the age and what is
missing, and uses sharper wording when the caller's own session is part of what
is missing. The cq skill documents the tradeoff so an agent knows a stale hit is
expected and knows which lever to pull.

Rejected: triggering the warning when results happen to include the caller's own
session. That fires on the safe case and stays silent on the dangerous one. If
the index is five minutes stale, the caller's recent messages are absent, so the
real failure is the false negative, where a matching message is simply not there
and no warning appears.

The correct check asks whether the caller's session has content the index lacks,
regardless of what came back:

```sql
SELECT (SELECT max(timestamp) FROM messages        WHERE session_id = ?) AS live,
       (SELECT max(timestamp) FROM cq_fts_messages WHERE session_id = ?) AS indexed
```

`live > indexed`, or `indexed IS NULL`, means the live session is ahead of the
index. This works because transcript sync stays on Auto and is cheap, so
`messages` is current even when the snapshot is not. `CLAUDE_SESSION_ID` is
available in the environment, and `src/scope.rs:107-129` already establishes the
pattern for this: a pure function over `Option<&str>` plus a thin `active_*()`
wrapper that reads the environment, which keeps it unit-testable.

### 5. Prefer one result per session by default (still open)

The index is correctly message-level, but several high-scoring messages from one
session can crowd out other useful sessions. Keep the message-level index and
collapse the default result set to the best-scoring message per session, for
example with `ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY score DESC)`.
An option such as `--messages` could expose every matching passage when desired.

This is the last undecided item.

### 6. Keep broad matching, but make its behavior clear

DuckDB BM25's default OR semantics produced hundreds of candidates for
`cold read`, while still ranking the desired session first. Broad retrieval is
useful when combined with ranking and session deduplication. A future
`--all-terms` mode could serve callers that need conjunctive matching, but it is
not required for the first useful slice.

## Monitoring the 5 minute default

The default was chosen from usage data, so it should be revisited with usage
data. cq can measure itself; these are re-runnable against cq's own transcripts.

Four things worth watching:

1. How often `search` serves a stale index, and how stale it was.
2. How often anyone passes `--reindex` alongside `search`.
3. Searches immediately re-run with a force flag. This is the strongest signal
   the window is too loose, and it would stand out clearly given the 16-second
   burst pattern.
4. Whether anyone ever searches their own live session. Today that is 0 of 479.

The query shape used to gather the usage evidence above, kept here so it does
not have to be re-derived. Note that DuckDB needs `regexp_matches`, not the `~`
operator, and that matching a bare session UUID anywhere in the command line
gives false positives because the scratchpad path contains it. Match the
`--session` flag explicitly instead.

```sql
WITH calls AS (
  SELECT session_id,
         timestamp::TIMESTAMP AS ts,
         json_extract_string(input, '$.command') AS cmd,
         regexp_extract(json_extract_string(input, '$.command'),
                        '--session[= ]+([0-9a-fA-F-]{36})', 1) AS target
  FROM tool_calls
  WHERE name = 'Bash'
    AND regexp_matches(json_extract_string(input, '$.command'),
                       '(^|[^a-zA-Z0-9_/.-])cq ')
)
SELECT count(*)                                    AS total,
       count(*) FILTER (WHERE target = session_id) AS targets_own_session,
       count(*) FILTER (WHERE cmd ILIKE '%--reindex%') AS forced
FROM calls;
```

Burst spacing, which is what justifies the window size:

```sql
WITH calls AS (
  SELECT session_id, timestamp::TIMESTAMP AS ts
  FROM tool_calls
  WHERE name = 'Bash'
    AND regexp_matches(json_extract_string(input, '$.command'),
                       '(^|[^a-zA-Z0-9_/.-])cq ')
)
SELECT count(*) FILTER (WHERE gap_s <= 30) AS within_30s,
       count(*) FILTER (WHERE gap_s <= 60) AS within_60s,
       median(gap_s)                       AS median_gap_s
FROM (SELECT date_diff('second',
                       lag(ts) OVER (PARTITION BY session_id ORDER BY ts),
                       ts) AS gap_s
      FROM calls);
```

## Separate issues found

Neither belongs in this PR.

**Scoped sync advances the global watermark.** `max_dir_mtime` respects
`SyncScope::Projects`, but `sync_sources` writes a single global `last_sync_at`
as if the whole tree were covered. After `cq --project foo sessions`, a project
that changed earlier can be skipped by the Auto-mode mtime check indefinitely,
so its changes stay unindexed with no signal. Only Auto is affected; `--reindex`
ignores the watermark. The single write is deliberate for the multi-source case,
so the fix is a per-scope watermark rather than moving the write into the loop.
Documented in a comment at the write site in `src/indexer.rs` and in
[a PR comment](https://github.com/technicalpickles/cq/pull/41#issuecomment-5442507166).

**Materialization is the real performance story.** Query-time JSON extraction
from `raw_records` dominates every command, not just search. The same trick that
makes `cq_fts_messages` fast would speed up `sessions` and `messages` too, but it
needs its own design work on what invalidates what. Recorded in
[a PR comment](https://github.com/technicalpickles/cq/pull/41#issuecomment-5442962442).

## Remaining before this leaves draft

Built and covered by tests: the 5-minute window with `CQ_FTS_MAX_AGE`, the
explicit `--reindex` force, `--no-reindex` skipping the FTS refresh, staleness
reporting with the own-session escalation, and best-message-per-session with
`--all-matches`. Verified against the real corpus: a warm search returns in about
0.5 seconds, and collapsing turns 3,963 matching messages into 648 sessions, with
the top session alone contributing 214 of them.

Still outstanding:

1. **Update the cq skill** to document the freshness tradeoff, so an agent knows
   a stale hit is expected and knows `--reindex` is the lever. The skill lives in
   the `pickled-claude-plugins` repo, not here, so it is a separate change.
2. **Decide whether the data revision from decision 2 is worth doing at all.**
   The window already caps rebuild frequency, which was most of its value. What
   remains is avoiding a rebuild when the window expires and nothing changed, on
   an otherwise idle machine.
3. **Re-check the numbers after the corpus grows**, using the monitoring queries
   above rather than the figures recorded here.

The public summary of the experiment and tradeoffs is also recorded in
[this issue comment](https://github.com/technicalpickles/cq/issues/39#issuecomment-5440451213).
