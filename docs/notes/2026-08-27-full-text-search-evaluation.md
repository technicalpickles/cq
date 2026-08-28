# Full-text search evaluation (2026-08-27)

This dated note preserves the evidence used to choose cq's initial full-text
search defaults. It is not command reference or an implementation status log.

- Current CLI behavior: `docs/cli-ux-conventions.md`
- User-facing rationale: `docs/design-principles.md`
- Implementation invariants: `CLAUDE.md`

## Search quality observed

The evaluation used the historical session that motivated
[issue #39](https://github.com/technicalpickles/cq/issues/39). The target
discussion used phrases such as “cold read,” while the remembered description
included related word forms and broader vocabulary.

| Query | Observed target rank |
|---|---:|
| Exact grep for `cold read` | 12 |
| `cq search "cold read"` | 1 |
| Longer natural-language search, user messages only | 3 overall; 1 after excluding the diagnostic session |
| Five manually selected grep variants | 3 |

DuckDB full-text search uses disjunctive (OR) term matching by default. This
produced many candidates for a short query, but BM25 ranking placed the target
session first. The result supports broad lexical retrieval plus ranking; it
does not demonstrate semantic similarity. Different vocabulary that expresses
the same idea still requires embeddings or another semantic layer.

## Performance observed

These measurements came from one machine with a warm disk cache. They describe
the relative cost of operations, not stable performance targets.

The isolated benchmark covered roughly 1,400 transcript files and 132,000
messages. About 22,500 non-empty messages entered the FTS snapshot.

| Operation | Approximate time |
|---|---:|
| Initial transcript sync with an empty cache | 61 seconds |
| Initial FTS snapshot and index build | 33 seconds |
| Cached search with a fresh index | 1 second |
| FTS rebuild after several changed files | 10 seconds |

A separate 1,289-file cache showed where normal invocations spent time:

| Operation | Approximate time |
|---|---:|
| Binary startup (`cq schema`) | 0.01 seconds |
| DB open, view composition, and sync fast path (`cq sql "SELECT 1"`) | 0.09 seconds |
| `cq tools --limit 1 --all` | 0.62 seconds |
| `cq sql "SELECT count(*) FROM messages"` | 1.36 seconds |
| `cq messages --limit 1 --all` | 3.80 seconds |
| `cq sessions --limit 1 --all` | 4.75 seconds |

Transcript sync accounted for only 2–5 percent of a typical command. Most time
went to extracting JSON from `raw_records` through the composed views. The
physical `cq_fts_messages_*` snapshots avoided that repeated extraction, so a
warm search was faster than `cq messages` despite the extra FTS machinery.

## Freshness evidence

The freshness decision used 479 cq invocations across 59 sessions in the main
source and all projects:

| Question | Observed value |
|---|---:|
| Invocations that targeted their own live session | 0 of 479 |
| Explicit `--session` filters | 32, all targeting other sessions |
| Shortest requested `--since` window | 24 hours |
| Median gap between consecutive cq calls in one session | 16 seconds |
| Follow-up calls within 30 seconds | 289 of 420 (69%) |
| Follow-up calls within 60 seconds | 327 of 420 (78%) |

The corpus showed archival queries and bursty command usage. A five-minute
staleness window avoided repeated rebuilds inside a burst while remaining small
relative to the observed query windows.

These calls predated `cq search`, so the conclusion was an inference rather
than direct search telemetry. Search may encourage people to query their
current conversation more often. Re-run the queries below as the corpus grows;
the usage shape matters more than the exact totals.

## Re-run the freshness analysis

The following query counts cq calls, explicit session targeting, and forced
reindexes. Match the `--session` flag specifically: a bare UUID can also appear
in a scratchpad path and produce false positives.

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
SELECT count(*) AS total,
       count(*) FILTER (WHERE target = session_id) AS targets_own_session,
       count(*) FILTER (WHERE cmd ILIKE '%--reindex%') AS forced
FROM calls;
```

The following query measures burst spacing:

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
       median(gap_s) AS median_gap_s
FROM (
  SELECT date_diff(
           'second',
           lag(ts) OVER (PARTITION BY session_id ORDER BY ts),
           ts
         ) AS gap_s
  FROM calls
);
```

Signals that the default needs adjustment include frequent stale-index notices,
searches immediately repeated with `--reindex`, and searches targeting the
caller's live session.

## Follow-ups

### Targeted refresh

`--reindex` currently rebuilds the complete transcript cache and FTS index.
Combining it with `--session` narrows the search results, not the rebuild work.
A future targeted refresh could sync the selected or current session and update
only its searchable messages. That requires an index-update strategy because
DuckDB's FTS index does not track table mutations automatically.

The current-session case still needs a full `--reindex` today: detecting that
the session is ahead can produce a warning, but detection does not make the
missing messages searchable.

### Scoped sync watermark

`max_dir_mtime` respects `SyncScope::Projects`, but `sync_sources` writes one
global `last_sync_at` value. A project-scoped sync can therefore advance the
watermark past changes in an unscanned project. A durable fix needs per-scope
watermarks rather than moving the existing global write.

### Materialized query views

Query-time JSON extraction dominated the measured cost of `sessions` and
`messages`. The materialized FTS snapshot demonstrates a broader optimization
opportunity, but materializing other views needs separate invalidation and
migration design.
