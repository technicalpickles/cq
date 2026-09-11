//! The span and gap model for a session trace.
//!
//! One session, decomposed into the two things a trace is made of:
//!
//! - a [`Span`] is a paired `tool_use` / `tool_result` -- a bar with a real
//!   duration, on a lane
//! - a [`Gap`] is dead air on a lane between two records, classified by what
//!   bounds it
//!
//! This module is the single source of truth both renderers consume, so it
//! holds SQL and no formatting. The renderers ([`waterfall`], [`perfetto`])
//! hold formatting and no SQL. That split is deliberate: the emitted trace
//! format is the part of this feature most likely to need replacing, so it
//! stays isolated behind this model. See
//! `docs/specs/2026-09-10-session-trace-view-design.md`.

pub mod perfetto;
pub mod waterfall;

use anyhow::{Context, Result};
use duckdb::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Parse a fixed-width UTC ISO8601 timestamp (as stored on [`Span`]/[`Gap`])
/// into epoch milliseconds. Unparseable input maps to `0` rather than erroring
/// -- these timestamps always come from our own SQL layer, so a parse failure
/// here means something upstream is already broken, and windowing/rendering
/// degrading gracefully is preferable to a panic over a display detail.
/// Shared by the waterfall renderer and the `--from`/`--to` window parser so
/// both agree on what a timestamp means.
pub fn epoch_ms(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// A tool call with a measured duration: one `tool_use` joined to its
/// `tool_result`.
///
/// `lane` is the subagent that made the call, or `"main"` for the main loop.
/// Unpaired calls (a `tool_use` whose result never landed) are absent rather
/// than zero-duration, since the join is what produces the interval.
#[derive(Debug, Clone, Serialize)]
pub struct Span {
    /// Subagent id that made the call, or `"main"` for the main loop.
    pub lane: String,
    /// The lane's agent type from its sidecar (NULL for the main loop).
    pub agent_type: Option<String>,
    /// Tool name (`Bash`, `Read`, `Agent`, ...).
    pub name: String,
    /// `tool_use` timestamp, a fixed-width UTC ISO string.
    pub start: String,
    /// `tool_result` timestamp, a fixed-width UTC ISO string.
    pub end: String,
    /// `end - start` in milliseconds.
    pub duration_ms: i64,
    /// Whether the result came back as an error.
    pub is_error: bool,
    /// The `tool_use` block's id, unique within the session. Also the join key
    /// for a subagent lane's parent edge (`agents.parent_tool_use_id`).
    pub tool_use_id: String,
    /// The call's input, verbatim. Parsed so consumers get structure rather
    /// than an escaped blob; input that doesn't parse is preserved as a JSON
    /// string rather than dropped.
    pub input: Option<serde_json::Value>,
}

/// What bounds a gap, and therefore who the session was waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// No human turn between the two records: the model was generating.
    Think,
    /// The next record is a genuine user turn: blocked on the human.
    Human,
}

/// Dead air on one lane, between two consecutive records on that lane.
#[derive(Debug, Clone, Serialize)]
pub struct Gap {
    /// Subagent id, or `"main"` for the main loop.
    pub lane: String,
    pub kind: GapKind,
    /// Timestamp of the record the gap starts after.
    pub start: String,
    /// Timestamp of the record the gap ends at.
    pub end: String,
    /// `end - start` in milliseconds; always positive (zero-length and
    /// out-of-order intervals are filtered out).
    pub duration_ms: i64,
    /// The text of the record that closes the gap, if it carried any --
    /// the human's message for a [`GapKind::Human`] gap, or the assistant's
    /// own text for a [`GapKind::Think`] gap that happened to produce one.
    /// `None` when the closing record had no text (e.g. a `tool_result`-only
    /// user turn, or an assistant turn that was pure tool calls).
    pub closing_text: Option<String>,
}

/// Every paired tool call in the session, ordered by call time.
///
/// The join is on `tool_use_id` alone, matching
/// `sessions::run_timeline`: ids are unique within a session, and a
/// subagent's records carry the parent session's `sessionId`, so filtering
/// the call side by session is enough to scope both.
pub fn spans(conn: &Connection, session_id: &str) -> Result<Vec<Span>> {
    // Timestamps are fixed-width UTC ISO strings (VARCHAR), so lexical order is
    // chronological and ORDER BY needs no cast. Only the subtraction does.
    let sql = "SELECT
            COALESCE(tc.agent_id, 'main') AS lane,
            tc.agent_type,
            tc.name,
            tc.timestamp AS span_start,
            tr.timestamp AS span_end,
            CAST((epoch_ms(CAST(tr.timestamp AS TIMESTAMP))
                - epoch_ms(CAST(tc.timestamp AS TIMESTAMP))) AS BIGINT) AS duration_ms,
            tr.is_error,
            tc.tool_use_id,
            CAST(tc.input AS VARCHAR) AS input
        FROM tool_calls tc
        JOIN tool_results tr ON tc.tool_use_id = tr.tool_use_id
        WHERE tc.session_id = ?
        ORDER BY tc.timestamp";

    let mut stmt = conn.prepare(sql).context("preparing trace span query")?;
    let rows = stmt
        .query_map([session_id], |row| {
            let raw_input: Option<String> = row.get(8)?;
            Ok(Span {
                lane: row.get(0)?,
                agent_type: row.get(1)?,
                name: row.get(2)?,
                start: row.get(3)?,
                end: row.get(4)?,
                duration_ms: row.get(5)?,
                is_error: row.get(6)?,
                tool_use_id: row.get(7)?,
                input: raw_input.map(|raw| {
                    serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw))
                }),
            })
        })
        .context("running trace span query")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("reading trace span rows")?;
    Ok(rows)
}

/// Every gap in the session, ordered by start time.
///
/// **Gaps are per-lane, and lanes run concurrently, so gap durations do not sum
/// to wall clock.** On one real 345-minute session, human gaps summed to 316
/// minutes and think gaps to 370 minutes: 686 minutes of "gap" inside a
/// 345-minute session. That is not a bug, it is subagent lanes idling in
/// parallel with the main loop and with each other, each counted on its own
/// lane. Any consumer that sums gap durations across lanes and compares the
/// total to elapsed wall time will be wrong. Compare within a single lane, or
/// aggregate by lane and report per lane.
///
/// Classification hinges on `text IS NOT NULL`: a `user` record carrying only
/// `tool_result` blocks has NULL text, while a genuine human turn has text.
/// (Measured on one session: 84 human turns against 1,020 `tool_result`
/// carriers.) So a gap whose closing record is a user turn *with* text is
/// [`GapKind::Human`]; everything else is [`GapKind::Think`].
pub fn gaps(conn: &Connection, session_id: &str) -> Result<Vec<Gap>> {
    let sql = "WITH lane_events AS (
            SELECT COALESCE(agent_id, 'main') AS lane, timestamp, type,
                   text IS NOT NULL AS has_text, text
            FROM messages WHERE session_id = ?
        ),
        ordered AS (
            SELECT lane, timestamp,
                   LEAD(timestamp) OVER (PARTITION BY lane ORDER BY timestamp) AS next_ts,
                   LEAD(type)      OVER (PARTITION BY lane ORDER BY timestamp) AS next_type,
                   LEAD(has_text)  OVER (PARTITION BY lane ORDER BY timestamp) AS next_has_text,
                   LEAD(text)      OVER (PARTITION BY lane ORDER BY timestamp) AS next_text
            FROM lane_events
        )
        SELECT lane,
               CASE WHEN next_type = 'user' AND next_has_text THEN 'human' ELSE 'think' END AS kind,
               timestamp AS gap_start, next_ts AS gap_end,
               CAST((epoch_ms(CAST(next_ts AS TIMESTAMP))
                   - epoch_ms(CAST(timestamp AS TIMESTAMP))) AS BIGINT) AS duration_ms,
               next_text AS closing_text
        FROM ordered
        WHERE next_ts IS NOT NULL
          AND epoch_ms(CAST(next_ts AS TIMESTAMP)) - epoch_ms(CAST(timestamp AS TIMESTAMP)) > 0
        ORDER BY gap_start";

    let mut stmt = conn.prepare(sql).context("preparing trace gap query")?;
    let rows = stmt
        .query_map([session_id], |row| {
            let kind: String = row.get(1)?;
            Ok(Gap {
                lane: row.get(0)?,
                kind: if kind == "human" {
                    GapKind::Human
                } else {
                    GapKind::Think
                },
                start: row.get(2)?,
                end: row.get(3)?,
                duration_ms: row.get(4)?,
                closing_text: row.get(5)?,
            })
        })
        .context("running trace gap query")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("reading trace gap rows")?;
    Ok(rows)
}

/// Map every lane in a session to the depth-1 lane that contains it. `main`
/// maps to itself; a depth-1 lane maps to itself; a deeper lane maps to the
/// depth-1 ancestor at the top of its dispatch chain.
///
/// Resolved in Rust, not SQL. The chain is `agents.parent_tool_use_id ->
/// tool_calls.tool_use_id -> tool_calls.agent_id`, walked upward one hop per
/// spawn. A recursive CTE can express that walk, but expressing "stop the
/// moment an ancestor repeats one already on this path" -- the cycle guard a
/// malformed sidecar demands -- inside a recursive CTE means threading a path
/// array through every recursive step and checking `list_contains` on it each
/// time; a `HashSet` in a loop says the same thing directly. Correctness
/// matters more than doing it in SQL, so this takes the loop.
///
/// A lane whose sidecar carries no `parent_tool_use_id` (a workflow subagent,
/// per `docs/session-storage.md`) has no chain to walk: it groups under its
/// own `workflow_id`, or its own lane id if even that's absent. `main` itself
/// never appears in the `agents` view (main-loop rows are excluded there by
/// construction -- see `claude_agents_sql`), so it's seeded directly rather
/// than resolved.
///
/// Cycle guard: a per-lane visited set stops the walk the instant an ancestor
/// repeats one already seen while resolving *that* lane, falling back to
/// grouping the lane under itself rather than looping. A hard cap on loop
/// iterations (bounded by the number of lanes in the session) backs that up
/// in case the visited-set logic itself has a bug -- belt and suspenders, so
/// a corrupted sidecar can never hang `cq trace --perfetto`.
pub fn lane_groups(conn: &Connection, session_id: &str) -> Result<HashMap<String, String>> {
    let mut stmt = conn
        .prepare(
            "SELECT agent_id, parent_tool_use_id, workflow_id FROM agents WHERE session_id = ?",
        )
        .context("preparing lane_groups agents query")?;
    let agent_rows: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .context("running lane_groups agents query")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("reading lane_groups agent rows")?;

    let mut stmt = conn
        .prepare(
            "SELECT tool_use_id, COALESCE(agent_id, 'main') FROM tool_calls WHERE session_id = ?",
        )
        .context("preparing lane_groups tool_calls query")?;
    let tool_use_to_lane: HashMap<String, String> = stmt
        .query_map([session_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .context("running lane_groups tool_calls query")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("reading lane_groups tool_call rows")?
        .into_iter()
        .collect();

    // agent_id -> (parent_tool_use_id, workflow_id), owned rather than
    // borrowed: the chain-walk below reassigns `current` on every hop, and
    // keeping everything owned sidesteps the double-reference lifetime
    // tangle a `HashMap<&str, (&Option<String>, ..)>` would create for no
    // real benefit -- these are small strings, not a hot loop.
    let agents_by_id: HashMap<String, (Option<String>, Option<String>)> = agent_rows
        .iter()
        .map(|(id, parent, workflow)| (id.clone(), (parent.clone(), workflow.clone())))
        .collect();

    let mut out: HashMap<String, String> = HashMap::new();
    out.insert("main".to_string(), "main".to_string());

    for (agent_id, _, _) in &agent_rows {
        let mut current = agent_id.clone();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(current.clone());

        // Bounded by the number of lanes: the real chain can never be longer
        // than that, cycle or not. In practice this cap never fires -- the
        // `visited` check below always catches a cycle within
        // agent_rows.len() steps first, since every step that keeps the walk
        // alive must land on one of that many distinct agent ids
        // (pigeonhole). It's kept anyway as a second guard against a bug in
        // the visited-set logic itself.
        let max_steps = agent_rows.len() + 1;
        let mut group = current.clone();
        for _ in 0..max_steps {
            let Some((parent_tool_use_id, workflow_id)) = agents_by_id.get(&current) else {
                // current always comes from the agents table, so this
                // shouldn't happen; group under wherever we got to.
                group = current.clone();
                break;
            };
            let Some(ptid) = parent_tool_use_id else {
                // No parent edge: a workflow subagent. Group by workflow_id,
                // or its own lane id if even that's missing.
                group = workflow_id.clone().unwrap_or_else(|| current.clone());
                break;
            };
            let ancestor = tool_use_to_lane
                .get(ptid)
                .cloned()
                .unwrap_or_else(|| "main".to_string());

            if ancestor == "main" {
                // current is a depth-1 lane; it groups under itself.
                group = current.clone();
                break;
            }
            if !visited.insert(ancestor.clone()) {
                // Cycle: this ancestor is already on the path we've walked
                // resolving this lane. Group the lane under itself rather
                // than loop forever chasing a malformed sidecar.
                group = agent_id.clone();
                break;
            }
            current = ancestor;
            group = current.clone();
        }
        out.insert(agent_id.clone(), group);
    }

    Ok(out)
}
