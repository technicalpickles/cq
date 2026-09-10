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

use crate::trace::{Gap, GapKind, Span};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;

fn epoch_us(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|d| d.timestamp_micros())
        .unwrap_or(0)
}

pub fn emit(spans: &[Span], gaps: &[Gap], session_id: &str) -> Result<()> {
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

    // Until the parent edge is threaded through (agents view join), every lane
    // shares one process. Task 7 follow-up: group by depth-1 ancestor.
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

    println!("{}", serde_json::to_string(&events)?);
    Ok(())
}
