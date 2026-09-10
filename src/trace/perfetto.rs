//! Chrome Trace Event JSON emitter.
//!
//! Isolated on purpose: the spec's JSON-longevity risk is hedged by making a
//! protobuf emitter a swap of this file, with the span model untouched.
//!
//! Hierarchy mapping: pid = a top-level dispatch (main loop, or a depth-1
//! subagent and everything it spawned); tid = the individual lane. Perfetto
//! ignores thread_sort_index, so pid grouping is the only real nesting
//! available in this format.
//!
//! Tool-call spans are encoded as async `ph: "b"`/`"e"` pairs (a begin and an
//! end event sharing an `id`), not `ph: "X"` complete events. This was
//! settled by experiment, not by reading the spec: two non-nested, overlapping
//! spans on one thread track (a real shape in session data -- see this
//! module's tests and `docs/notes/2026-09-10-perfetto-overlap-spike.md`) push
//! `ph: "X"` events onto an automatic overflow track named `[NULL]`, with an
//! explicit `slice_spill_overlapping_complete_event` import warning from
//! `trace_processor`. The same two spans encoded as `ph: "b"`/`"e"` pairs land
//! on clean, correctly-named tracks with zero warnings. `id` is the span's own
//! `tool_use_id`, which is already unique per span and gives every pair a
//! stable, meaningful correlation key for free.
//!
//! Gaps don't have this problem -- by construction two gaps on the same lane
//! never overlap -- so gaps stay as `ph: "X"` complete events with `dur`.

use crate::trace::{epoch_ms, Gap, GapKind, Span};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Microsecond epoch for a span/gap timestamp. Perfetto's `ts`/`dur` fields
/// are in microseconds while the shared `epoch_ms` helper (also used by
/// `waterfall.rs` and the `--from`/`--to` window parser) is millisecond
/// precision; the underlying data has no sub-millisecond resolution, so this
/// just widens `epoch_ms`'s result rather than re-parsing the RFC3339 string.
fn epoch_us(ts: &str) -> i64 {
    epoch_ms(ts) * 1000
}

pub fn emit(spans: &[Span], gaps: &[Gap], session_id: &str) -> Result<()> {
    let events = build_events(spans, gaps, session_id);
    println!("{}", serde_json::to_string(&events)?);
    Ok(())
}

/// Build the Chrome Trace Event array for one session, without printing it.
/// Pulled out of [`emit`] so tests can assert on the structured events
/// directly instead of capturing stdout.
fn build_events(spans: &[Span], gaps: &[Gap], session_id: &str) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();

    // Assign a tid per lane, main first so it sorts to tid 1.
    let mut tids: HashMap<&str, i64> = HashMap::new();
    tids.insert("main", 1);
    let mut next = 2;
    for s in spans {
        if !tids.contains_key(s.lane.as_str()) {
            tids.insert(s.lane.as_str(), next);
            next += 1;
        }
    }

    // Task 9 groups lanes by their depth-1 ancestor; until then every lane
    // shares one process.
    let pid = 1;

    events.push(json!({
        "ph": "M", "name": "process_name", "pid": pid, "tid": 0,
        "args": {"name": format!("session {}", &session_id[..8.min(session_id.len())])}
    }));
    for (lane, tid) in &tids {
        events.push(json!({
            "ph": "M", "name": "thread_name", "pid": pid, "tid": tid,
            "args": {"name": *lane}
        }));
    }

    // Tool-call spans: async ph:"b"/"e" pairs keyed by tool_use_id -- see this
    // module's doc comment for why complete events are wrong here. Args live
    // on the begin event only; trace_processor associates args from either
    // event of a pair with the resulting slice (verified in Step 5), and the
    // begin event is where the plan's skeleton put them, so there's no reason
    // to duplicate them onto the end event too.
    for s in spans {
        let tid = tids[s.lane.as_str()];
        let name = if s.is_error {
            format!("{} (error)", s.name)
        } else {
            s.name.clone()
        };
        let start_us = epoch_us(&s.start);
        let dur_us = (s.duration_ms * 1000).max(1);
        let args = json!({
            "input": s.input,
            "tool_use_id": s.tool_use_id,
            "duration_ms": s.duration_ms,
            "is_error": s.is_error,
        });
        events.push(json!({
            "ph": "b", "name": name.clone(), "cat": "tool",
            "pid": pid, "tid": tid, "ts": start_us,
            "id": s.tool_use_id, "args": args
        }));
        events.push(json!({
            "ph": "e", "name": name, "cat": "tool",
            "pid": pid, "tid": tid, "ts": start_us + dur_us,
            "id": s.tool_use_id
        }));
    }

    for g in gaps {
        let Some(tid) = tids.get(g.lane.as_str()) else {
            continue;
        };
        events.push(json!({
            "ph": "X",
            "name": match g.kind { GapKind::Human => "blocked on you", GapKind::Think => "think" },
            "cat": "gap",
            "pid": pid,
            "tid": tid,
            "ts": epoch_us(&g.start),
            "dur": (g.duration_ms * 1000).max(1),
            "args": {"duration_ms": g.duration_ms}
        }));
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(lane: &str, tool_use_id: &str, start: &str, duration_ms: i64) -> Span {
        Span {
            lane: lane.to_string(),
            agent_type: None,
            name: "Bash".to_string(),
            start: start.to_string(),
            end: start.to_string(),
            duration_ms,
            is_error: false,
            tool_use_id: tool_use_id.to_string(),
            input: None,
        }
    }

    /// Mutation target for the `ph:"X"` complete-event regression the plan's
    /// original skeleton shipped (see this module's doc comment): every tool
    /// span must land as a `ph:"b"`/`ph:"e"` pair, never a single `ph:"X"`
    /// event, and every `id` among tool-cat events must appear in exactly one
    /// begin and one end.
    #[test]
    fn tool_spans_emit_as_matched_begin_end_pairs() {
        let spans = vec![
            span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100),
            span("agent-sub1", "toolu_2", "2026-09-10T12:00:01.000Z", 200),
            span("agent-sub2", "toolu_3", "2026-09-10T12:00:02.000Z", 300),
        ];
        let events = build_events(&spans, &[], "a1b2c3d4-0000-4000-8000-000000000001");

        let tool_events: Vec<&Value> = events.iter().filter(|e| e["cat"] == "tool").collect();
        assert_eq!(
            tool_events.len(),
            2 * spans.len(),
            "expected a begin and an end event per span"
        );

        for s in &spans {
            let matching: Vec<&&Value> = tool_events
                .iter()
                .filter(|e| e["id"] == json!(s.tool_use_id))
                .collect();
            assert_eq!(
                matching.len(),
                2,
                "expected exactly 2 events for id {}",
                s.tool_use_id
            );

            let phs: Vec<&str> = matching.iter().map(|e| e["ph"].as_str().unwrap()).collect();
            assert!(
                phs.contains(&"b") && phs.contains(&"e"),
                "expected one \"b\" and one \"e\" for id {}, got {phs:?}",
                s.tool_use_id
            );
            assert!(
                phs.iter().all(|ph| *ph != "X"),
                "tool spans must never be ph:\"X\" complete events, got {phs:?}"
            );

            assert_eq!(matching[0]["pid"], 1);
            assert_eq!(matching[1]["pid"], 1);
            assert_eq!(
                matching[0]["tid"], matching[1]["tid"],
                "begin and end for id {} must share a tid",
                s.tool_use_id
            );
        }
    }
}
