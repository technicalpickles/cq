//! Chrome Trace Event JSON emitter.
//!
//! Isolated on purpose: the spec's JSON-longevity risk is hedged by making a
//! protobuf emitter a swap of this file, with the span model untouched.
//!
//! Hierarchy mapping: pid = a top-level dispatch (main loop, or a depth-1
//! subagent and everything it spawned); tid = the individual lane. Perfetto
//! ignores thread_sort_index, so pid grouping is the only real nesting
//! available in this format. The pid a lane belongs to comes from
//! [`crate::trace::lane_groups`], computed once by the caller and passed in
//! -- this module stays formatting-only, no SQL, per the split described in
//! `trace/mod.rs`'s module doc.
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

/// Cap placed on every `args.detail` string this module emits (see
/// [`marker_detail`] and the gap-closing-text handling in `build_events`),
/// so one oversized command or pasted message can't blow up a marker's
/// on-chart label.
const MAX_DETAIL_LEN: usize = 200;

/// Truncate `s` to [`MAX_DETAIL_LEN`] characters, marking truncation with a
/// trailing ellipsis. Character-counted, not byte-counted, so this never
/// splits a multi-byte UTF-8 sequence.
fn truncate_for_marker(s: &str) -> String {
    if s.chars().count() <= MAX_DETAIL_LEN {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(MAX_DETAIL_LEN).collect();
        truncated.push('…');
        truncated
    }
}

/// A short, single-line summary of a tool call's `input`, placed at
/// `args.detail` on the emitted event.
///
/// This is the one shape Firefox Profiler's Chrome Trace importer turns into
/// a visible label -- on the Marker Chart, the Marker Table's Details
/// column, and the tooltip, with no click or hover needed. Every other
/// shape `args` takes here (`input`, `tool_use_id`, `duration_ms`,
/// `is_error`) gets silently dropped by that importer, since it only reads
/// `args.data` (an object) or `args.detail` (a string); see
/// `firefox-devtools/profiler`'s `src/profile-logic/import/chrome.ts` and
/// this project's issue #46. Perfetto's own UI shows `args` regardless of
/// shape, so adding `detail` doesn't cost that viewer anything.
///
/// Deliberately generic rather than keyed on known fields like `command` or
/// `file_path`: `input`'s shape isn't a documented contract any more than
/// the transcript format itself is (see `docs/session-storage.md`), and a
/// fixed set of known tool names would silently stop covering new tools
/// (MCP tools, new skills, ...) as they show up. Rendering the whole value
/// as compact JSON costs nothing to keep current.
fn marker_detail(input: &Option<Value>) -> Option<String> {
    let input = input.as_ref()?;
    Some(truncate_for_marker(&input.to_string()))
}

pub fn emit(
    spans: &[Span],
    gaps: &[Gap],
    session_id: &str,
    groups: &HashMap<String, String>,
) -> Result<()> {
    let events = build_events(spans, gaps, session_id, groups);
    println!("{}", serde_json::to_string(&events)?);
    Ok(())
}

/// Build the Chrome Trace Event array for one session, without printing it.
/// Pulled out of [`emit`] so tests can assert on the structured events
/// directly instead of capturing stdout.
///
/// `groups` maps each lane to the depth-1 ancestor that is its pid group
/// (see [`crate::trace::lane_groups`]); a lane absent from the map (should
/// not happen in practice, since the caller derives it from the same
/// session) falls back to grouping under itself rather than panicking.
fn build_events(
    spans: &[Span],
    gaps: &[Gap],
    session_id: &str,
    groups: &HashMap<String, String>,
) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();

    let group_of = |lane: &str| -> String {
        groups
            .get(lane)
            .cloned()
            .unwrap_or_else(|| lane.to_string())
    };

    // Assign a tid per lane, main first so it sorts to tid 1.
    let mut tids: HashMap<&str, i64> = HashMap::new();
    tids.insert("main", 1);
    let mut next_tid = 2;
    for s in spans {
        if !tids.contains_key(s.lane.as_str()) {
            tids.insert(s.lane.as_str(), next_tid);
            next_tid += 1;
        }
    }

    // Assign a pid per group, main first so it sorts to pid 1. Every lane a
    // span names has a group (falling back to itself via `group_of` above),
    // so this covers every pid the spans/gaps below will reference.
    let mut pids: HashMap<String, i64> = HashMap::new();
    pids.insert("main".to_string(), 1);
    let mut next_pid = 2;
    for s in spans {
        let g = group_of(&s.lane);
        if let std::collections::hash_map::Entry::Vacant(e) = pids.entry(g) {
            e.insert(next_pid);
            next_pid += 1;
        }
    }

    // A group's own agent_type, read off a span whose lane *is* the group
    // (its own dispatch, not a descendant's) -- used to label that group's
    // process_name. A group with no direct spans of its own (shouldn't
    // happen: a group is always the lane that did the dispatching) is left
    // unlabeled and falls back to the bare lane id.
    //
    // Only agent_type is used, not description, even though the latter would
    // read friendlier ("Sub work" vs "agent-sub1 (general-purpose)"): the
    // description lives on `agents`/`file_registry`, not on `Span`, and
    // pulling it in would widen a struct shared with waterfall.rs for a
    // label-only benefit on this one caller. The lane id in the name is
    // already unique, so this is a lost cosmetic upgrade, not a correctness
    // gap -- two same-agent_type groups never collide on an indistinguishable
    // label.
    let mut group_agent_type: HashMap<&str, &str> = HashMap::new();
    for s in spans {
        if group_of(&s.lane) == s.lane {
            if let Some(t) = s.agent_type.as_deref() {
                group_agent_type.entry(s.lane.as_str()).or_insert(t);
            }
        }
    }

    for (group, pid) in &pids {
        let name = if group == "main" {
            format!("session {}", &session_id[..8.min(session_id.len())])
        } else {
            match group_agent_type.get(group.as_str()) {
                Some(agent_type) => format!("{group} ({agent_type})"),
                None => group.clone(),
            }
        };
        events.push(json!({
            "ph": "M", "name": "process_name", "pid": pid, "tid": 0,
            "args": {"name": name}
        }));
    }
    for (lane, tid) in &tids {
        let pid = pids[&group_of(lane)];
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
        let pid = pids[&group_of(&s.lane)];
        let name = if s.is_error {
            format!("{} (error)", s.name)
        } else {
            s.name.clone()
        };
        let start_us = epoch_us(&s.start);
        let dur_us = (s.duration_ms * 1000).max(1);
        let mut args = json!({
            "input": s.input,
            "tool_use_id": s.tool_use_id,
            "duration_ms": s.duration_ms,
            "is_error": s.is_error,
        });
        if let Some(detail) = marker_detail(&s.input) {
            args["detail"] = json!(detail);
        }
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
        let Some(&tid) = tids.get(g.lane.as_str()) else {
            continue;
        };
        let pid = pids[&group_of(&g.lane)];
        let mut args = json!({"duration_ms": g.duration_ms});
        // The closing record's text, if any: the human's message for a
        // `blocked on you` gap, or the assistant's own text for a `think`
        // gap that happened to produce one. Blank/whitespace-only text
        // (seen on some records) is worth no more than the missing case.
        if let Some(text) = g.closing_text.as_deref().map(str::trim) {
            if !text.is_empty() {
                args["detail"] = json!(truncate_for_marker(text));
            }
        }
        events.push(json!({
            "ph": "X",
            "name": match g.kind { GapKind::Human => "blocked on you", GapKind::Think => "think" },
            "cat": "gap",
            "pid": pid,
            "tid": tid,
            "ts": epoch_us(&g.start),
            "dur": (g.duration_ms * 1000).max(1),
            "args": args
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

    /// The fixture's real shape: agent-sub2 is a depth-2 lane dispatched from
    /// inside agent-sub1, so its group is agent-sub1, not itself.
    fn fixture_groups() -> HashMap<String, String> {
        [
            ("main".to_string(), "main".to_string()),
            ("agent-sub1".to_string(), "agent-sub1".to_string()),
            ("agent-sub2".to_string(), "agent-sub1".to_string()),
        ]
        .into_iter()
        .collect()
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
        let events = build_events(
            &spans,
            &[],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

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

            assert_eq!(
                matching[0]["pid"], matching[1]["pid"],
                "begin and end for id {} must share a pid",
                s.tool_use_id
            );
            assert_eq!(
                matching[0]["tid"], matching[1]["tid"],
                "begin and end for id {} must share a tid",
                s.tool_use_id
            );
        }
    }

    /// Task 9's actual point: pid reflects the depth-1 dispatch tree, not one
    /// shared process. agent-sub2 (depth 2, dispatched from inside
    /// agent-sub1) must land on agent-sub1's pid, and main must land on its
    /// own, distinct pid.
    #[test]
    fn pid_groups_a_depth_two_lane_under_its_depth_one_ancestor() {
        let spans = vec![
            span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100),
            span("agent-sub1", "toolu_2", "2026-09-10T12:00:01.000Z", 200),
            span("agent-sub2", "toolu_3", "2026-09-10T12:00:02.000Z", 300),
        ];
        let events = build_events(
            &spans,
            &[],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let pid_of = |tool_use_id: &str| -> Value {
            events
                .iter()
                .find(|e| e["cat"] == "tool" && e["id"] == json!(tool_use_id))
                .expect("span must have emitted at least one event")["pid"]
                .clone()
        };

        let main_pid = pid_of("toolu_1");
        let sub1_pid = pid_of("toolu_2");
        let sub2_pid = pid_of("toolu_3");

        assert_ne!(
            main_pid, sub1_pid,
            "main and the depth-1 dispatch must be different processes"
        );
        assert_eq!(
            sub1_pid, sub2_pid,
            "agent-sub2 must share agent-sub1's pid, not get its own"
        );

        let process_names: Vec<&Value> = events
            .iter()
            .filter(|e| e["name"] == "process_name")
            .collect();
        assert_eq!(
            process_names.len(),
            2,
            "exactly one process per group -- main's, and agent-sub1's (which also covers agent-sub2)"
        );
    }

    #[test]
    fn marker_detail_is_none_without_input() {
        assert_eq!(marker_detail(&None), None);
    }

    #[test]
    fn marker_detail_renders_short_input_verbatim_as_compact_json() {
        let input = Some(json!({"command": "git status", "description": "Check status"}));
        assert_eq!(
            marker_detail(&input),
            Some(r#"{"command":"git status","description":"Check status"}"#.to_string())
        );
    }

    #[test]
    fn marker_detail_truncates_long_input_with_an_ellipsis() {
        let input = Some(json!({"command": "x".repeat(500)}));
        let detail = marker_detail(&input).expect("input is Some, so detail must be Some");
        assert!(
            detail.ends_with('…'),
            "truncated detail must end with an ellipsis, got {detail:?}"
        );
        // 200 chars of content plus the ellipsis marker itself.
        assert_eq!(detail.chars().count(), 201);
    }

    /// This is the regression this whole change exists to prevent: a tool
    /// span's begin event must carry a `detail` string in `args`, because
    /// that's the one shape Firefox Profiler's Chrome Trace importer turns
    /// into a visible label (issue #46). Every other field in `args` here
    /// is invisible in that viewer.
    #[test]
    fn tool_span_begin_event_carries_detail_when_input_is_present() {
        let mut s = span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100);
        s.input = Some(json!({"command": "echo hi"}));
        let events = build_events(
            &[s],
            &[],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let begin = events
            .iter()
            .find(|e| e["cat"] == "tool" && e["ph"] == "b")
            .expect("must have a begin event");
        assert_eq!(
            begin["args"]["detail"],
            json!(r#"{"command":"echo hi"}"#),
            "begin event args: {:?}",
            begin["args"]
        );
    }

    #[test]
    fn tool_span_begin_event_omits_detail_when_input_is_absent() {
        let s = span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100);
        let events = build_events(
            &[s],
            &[],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let begin = events
            .iter()
            .find(|e| e["cat"] == "tool" && e["ph"] == "b")
            .expect("must have a begin event");
        assert!(
            begin["args"].get("detail").is_none(),
            "begin event args should have no detail key when input is None, got {:?}",
            begin["args"]
        );
    }

    fn gap(kind: GapKind, closing_text: Option<&str>) -> Gap {
        Gap {
            lane: "main".to_string(),
            kind,
            start: "2026-09-10T12:00:00.000Z".to_string(),
            end: "2026-09-10T12:00:01.000Z".to_string(),
            duration_ms: 1000,
            closing_text: closing_text.map(str::to_string),
        }
    }

    /// This is the point of the gap-detail change: a `blocked on you` gap
    /// should carry what the human actually said, not just its duration.
    #[test]
    fn human_gap_event_carries_the_closing_message_as_detail() {
        let events = build_events(
            &[],
            &[gap(GapKind::Human, Some("go ahead"))],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let gap_event = events
            .iter()
            .find(|e| e["cat"] == "gap")
            .expect("must have a gap event");
        assert_eq!(gap_event["args"]["detail"], json!("go ahead"));
    }

    #[test]
    fn gap_event_omits_detail_when_closing_text_is_absent_or_blank() {
        for closing_text in [None, Some("   ")] {
            let events = build_events(
                &[],
                &[gap(GapKind::Think, closing_text)],
                "a1b2c3d4-0000-4000-8000-000000000001",
                &fixture_groups(),
            );

            let gap_event = events
                .iter()
                .find(|e| e["cat"] == "gap")
                .expect("must have a gap event");
            assert!(
                gap_event["args"].get("detail").is_none(),
                "closing_text {closing_text:?} should not produce a detail key, got {:?}",
                gap_event["args"]
            );
        }
    }

    #[test]
    fn gap_event_detail_is_truncated_like_tool_span_detail() {
        let long_text = "x".repeat(500);
        let events = build_events(
            &[],
            &[gap(GapKind::Think, Some(&long_text))],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let gap_event = events
            .iter()
            .find(|e| e["cat"] == "gap")
            .expect("must have a gap event");
        let detail = gap_event["args"]["detail"]
            .as_str()
            .expect("detail must be a string");
        assert!(detail.ends_with('…'));
        assert_eq!(detail.chars().count(), MAX_DETAIL_LEN + 1);
    }
}
