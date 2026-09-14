//! Firefox Profiler native processed-profile JSON emitter.
//!
//! Sibling to [`crate::trace::perfetto`], reading the same span/gap model.
//! Chosen over extending the Chrome Trace emitter because Firefox Profiler's
//! own Chrome Trace importer hardcodes every marker category to grey
//! (`firefox-devtools/profiler`'s `src/profile-logic/import/chrome.ts`); the
//! only way to get real per-category colors in the Marker Chart is to emit
//! the format Firefox Profiler loads directly, with a real `meta.categories`
//! list. See `docs/specs/2026-09-11-firefox-profiler-native-emitter-design.md`.

use crate::trace::{epoch_ms, marker_detail, truncate_for_marker, Gap, GapKind, Span};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Firefox Profiler's fixed 10-value color palette (`GraphColor` in
/// `firefox-profiler`'s `src/types/profile.ts`). Category colors below must
/// only use these names.
const CATEGORIES: &[(&str, &str)] = &[
    ("Bash", "orange"),
    ("File ops", "blue"),
    ("Search", "purple"),
    ("MCP tool", "teal"),
    ("Other tool", "magenta"),
    ("Think gap", "grey"),
    ("Human gap", "green"),
];

/// Index into [`CATEGORIES`] for one tool span, by its tool name. Fixed
/// buckets, not literal tool names: a single real 1,020-call session already
/// uses 11 distinct tool names, past the 10-color palette, and MCP tools push
/// that further. See the design doc's "Category taxonomy" section.
fn category_for_tool(name: &str) -> usize {
    match name {
        "Bash" => 0,
        "Read" | "Edit" | "Write" => 1,
        "Grep" | "Glob" | "ToolSearch" => 2,
        _ if name.starts_with("mcp__") => 3,
        _ => 4, // Other tool: Agent, Skill, Monitor, and anything not listed above
    }
}

/// Index into [`CATEGORIES`] for one gap, by its [`GapKind`].
fn category_for_gap(kind: GapKind) -> usize {
    match kind {
        GapKind::Think => 5,
        GapKind::Human => 6,
    }
}

fn build_categories() -> Value {
    Value::Array(
        CATEGORIES
            .iter()
            .map(|(name, color)| json!({"name": name, "color": color, "subcategories": ["Other"]}))
            .collect(),
    )
}

/// Two generic marker types -- `ToolCall` and `Gap` -- rather than one per
/// literal tool name. `MarkerSchema.fields` is declared once per marker
/// *type* in Firefox Profiler, not computed per marker instance, so a
/// labeled `Command:`/`Description:` row per tool would mean hardcoding each
/// tool's input shape in Rust. See the design doc's field-structure
/// non-goal: this stays generic, matching `perfetto.rs`'s existing
/// `marker_detail` philosophy.
fn build_marker_schema() -> Value {
    json!([
        {
            "name": "ToolCall",
            "display": ["marker-chart", "marker-table", "timeline-overview"],
            "fields": [{"key": "input", "label": "Input", "format": "string"}]
        },
        {
            "name": "Gap",
            "display": ["marker-chart", "marker-table", "timeline-overview"],
            "fields": [{"key": "detail", "label": "Detail", "format": "string"}]
        }
    ])
}

fn build_meta() -> Value {
    json!({
        "interval": 1,
        "startTime": 0,
        "abi": "",
        "misc": "",
        "oscpu": "",
        "platform": "",
        "processType": 0,
        "extensions": {"id": [], "name": [], "baseURL": [], "length": 0},
        "categories": build_categories(),
        "product": "cq",
        "stackwalk": 0,
        "toolkit": "",
        "version": 36,
        "preprocessedProfileVersion": 72,
        "appBuildID": "",
        "sourceURL": "",
        "physicalCPUs": 0,
        "logicalCPUs": 0,
        "CPUName": "",
        "symbolicated": true,
        "markerSchema": build_marker_schema()
    })
}

/// The profile-level tables cq never populates (no stacks, no samples --
/// this data is 100% markers). Verified empty and accepted by Firefox
/// Profiler's importer against a hand-built minimal profile on 2026-09-11
/// (see the design doc's Testing section); every field is a plain empty
/// array rather than the `{}` shape the app's own `JSON.stringify` produces
/// for a zero-length typed array, which the importer accepts identically.
fn build_shared(string_array: Vec<String>) -> Value {
    json!({
        "stackTable": {"frame": [], "prefixOffset": [], "length": 0},
        "frameTable": {
            "flags": [], "address": [], "category": [], "subcategory": [],
            "func": [], "lib": [], "nativeSymbol": [], "innerWindowID": [],
            "line": [], "column": [], "originalLocation": [], "length": 0
        },
        "funcTable": {
            "isJS": [], "relevantForJS": [], "name": [], "resource": [],
            "source": [], "lineNumber": [], "columnNumber": [],
            "originalLocation": [], "length": 0
        },
        "resourceTable": {"name": [], "host": [], "type": [], "length": 0},
        "nativeSymbols": {
            "libIndex": [], "address": [], "name": [], "functionSize": [], "length": 0
        },
        "sources": {
            "id": [], "filename": [], "startLine": [], "startColumn": [],
            "sourceMapURL": [], "content": [], "length": 0
        },
        "stringArray": string_array,
        "sourceLocationTable": {"source": [], "line": [], "column": [], "length": 0}
    })
}

fn empty_samples() -> Value {
    json!({
        "weightType": "samples",
        "weight": null,
        "eventDelay": [],
        "stack": [],
        "time": [],
        "length": 0
    })
}

/// Interns strings into `shared.stringArray`, returning the index a marker's
/// `name` field should reference. Shared across every thread in the profile
/// -- `RawProfileSharedData.stringArray` lives at the profile level in
/// Firefox Profiler's format, not per-thread.
struct StringTable {
    strings: Vec<String>,
    index: HashMap<String, u32>,
}

impl StringTable {
    fn new() -> Self {
        Self {
            strings: Vec::new(),
            index: HashMap::new(),
        }
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.index.insert(s.to_string(), i);
        i
    }

    fn into_vec(self) -> Vec<String> {
        self.strings
    }
}

/// One row of a `RawMarkerTable`, before being flattened into the
/// format's parallel-array shape by [`markers_to_json`].
struct RawMarker {
    name_index: u32,
    start_ms: f64,
    end_ms: f64,
    category: usize,
    data: Value,
}

/// `MarkerPhase::Interval` (`INTERVAL` in `firefox-profiler`'s
/// `src/app-logic/constants.ts`) -- every span and gap cq traces has both a
/// start and an end, so nothing here ever needs Instant/IntervalStart/End.
const PHASE_INTERVAL: u8 = 1;

/// Every marker on one lane, spans and gaps merged and sorted by start time.
fn build_lane_markers(
    spans: &[Span],
    gaps: &[Gap],
    lane: &str,
    session_start_ms: i64,
    strings: &mut StringTable,
) -> Vec<RawMarker> {
    let mut markers = Vec::new();

    for s in spans.iter().filter(|s| s.lane == lane) {
        let name = if s.is_error {
            format!("{} (error)", s.name)
        } else {
            s.name.clone()
        };
        let mut data = json!({"type": "ToolCall"});
        if let Some(detail) = marker_detail(&s.input) {
            data["input"] = json!(detail);
        }
        markers.push(RawMarker {
            name_index: strings.intern(&name),
            start_ms: (epoch_ms(&s.start) - session_start_ms) as f64,
            end_ms: (epoch_ms(&s.end) - session_start_ms) as f64,
            category: category_for_tool(&s.name),
            data,
        });
    }

    for g in gaps.iter().filter(|g| g.lane == lane) {
        let name = match g.kind {
            GapKind::Human => "blocked on you",
            GapKind::Think => "think",
        };
        let mut data = json!({"type": "Gap"});
        if let Some(text) = g.closing_text.as_deref().map(str::trim) {
            if !text.is_empty() {
                data["detail"] = json!(truncate_for_marker(text));
            }
        }
        markers.push(RawMarker {
            name_index: strings.intern(name),
            start_ms: (epoch_ms(&g.start) - session_start_ms) as f64,
            end_ms: (epoch_ms(&g.end) - session_start_ms) as f64,
            category: category_for_gap(g.kind),
            data,
        });
    }

    markers.sort_by(|a, b| a.start_ms.partial_cmp(&b.start_ms).unwrap());
    markers
}

fn markers_to_json(markers: Vec<RawMarker>) -> Value {
    let length = markers.len();
    let mut data = Vec::with_capacity(length);
    let mut name = Vec::with_capacity(length);
    let mut start_time = Vec::with_capacity(length);
    let mut end_time = Vec::with_capacity(length);
    let mut phase = Vec::with_capacity(length);
    let mut category = Vec::with_capacity(length);

    for m in markers {
        data.push(m.data);
        name.push(m.name_index);
        start_time.push(m.start_ms);
        end_time.push(m.end_ms);
        phase.push(PHASE_INTERVAL);
        category.push(m.category);
    }

    json!({
        "data": data,
        "name": name,
        "startTime": start_time,
        "endTime": end_time,
        "phase": phase,
        "category": category,
        "length": length
    })
}

fn build_thread(lane: &str, tid: i64, pid: i64, process_name: &str, markers: Value) -> Value {
    json!({
        "processType": "default",
        "processStartupTime": 0,
        "processShutdownTime": null,
        "registerTime": 0,
        "unregisterTime": null,
        "pausedRanges": [],
        "name": lane,
        "isMainThread": lane == "main",
        "pid": pid.to_string(),
        "tid": tid,
        "processName": process_name,
        "samples": empty_samples(),
        "markers": markers
    })
}

pub fn emit(
    spans: &[Span],
    gaps: &[Gap],
    session_id: &str,
    groups: &HashMap<String, String>,
) -> Result<()> {
    let profile = build_profile(spans, gaps, session_id, groups);
    println!("{}", serde_json::to_string(&profile)?);
    Ok(())
}

/// Build the processed-profile JSON object for one session, without
/// printing it. Pulled out of [`emit`] so tests can assert on the
/// structured value directly, mirroring `perfetto::build_events`.
///
/// `groups` maps each lane to the depth-1 ancestor that is its pid group
/// (see [`crate::trace::lane_groups`]) -- same contract as
/// `perfetto::build_events`, including the same tid/pid assignment
/// convention (main first, so it sorts to tid/pid 1).
fn build_profile(
    spans: &[Span],
    gaps: &[Gap],
    session_id: &str,
    groups: &HashMap<String, String>,
) -> Value {
    let mut strings = StringTable::new();

    // Earliest timestamp across every span/gap becomes t=0; the processed
    // profile format wants small, thread-relative millisecond times, not
    // absolute epoch values.
    let session_start_ms = spans
        .iter()
        .map(|s| epoch_ms(&s.start))
        .chain(gaps.iter().map(|g| epoch_ms(&g.start)))
        .min()
        .unwrap_or(0);

    let mut tids: HashMap<&str, i64> = HashMap::new();
    tids.insert("main", 1);
    let mut next_tid = 2;
    for lane in spans
        .iter()
        .map(|s| s.lane.as_str())
        .chain(gaps.iter().map(|g| g.lane.as_str()))
    {
        if !tids.contains_key(lane) {
            tids.insert(lane, next_tid);
            next_tid += 1;
        }
    }

    let group_of = |lane: &str| -> String {
        groups
            .get(lane)
            .cloned()
            .unwrap_or_else(|| lane.to_string())
    };

    let mut pids: HashMap<String, i64> = HashMap::new();
    pids.insert("main".to_string(), 1);
    let mut next_pid = 2;
    for lane in spans
        .iter()
        .map(|s| s.lane.as_str())
        .chain(gaps.iter().map(|g| g.lane.as_str()))
    {
        let g = group_of(lane);
        if let std::collections::hash_map::Entry::Vacant(e) = pids.entry(g) {
            e.insert(next_pid);
            next_pid += 1;
        }
    }

    // A group's own agent_type, read off a span whose lane *is* the group --
    // used to label that group's processName. Mirrors perfetto.rs exactly.
    let mut group_agent_type: HashMap<&str, &str> = HashMap::new();
    for s in spans {
        if group_of(&s.lane) == s.lane {
            if let Some(t) = s.agent_type.as_deref() {
                group_agent_type.entry(s.lane.as_str()).or_insert(t);
            }
        }
    }

    let process_name = |group: &str| -> String {
        if group == "main" {
            format!("session {}", &session_id[..8.min(session_id.len())])
        } else {
            match group_agent_type.get(group) {
                Some(agent_type) => format!("{group} ({agent_type})"),
                None => group.to_string(),
            }
        }
    };

    let mut lanes: Vec<&str> = tids.keys().copied().collect();
    lanes.sort_by_key(|lane| tids[lane]);

    let threads: Vec<Value> = lanes
        .into_iter()
        .map(|lane| {
            let tid = tids[lane];
            let group = group_of(lane);
            let pid = pids[&group];
            let markers = build_lane_markers(spans, gaps, lane, session_start_ms, &mut strings);
            build_thread(
                lane,
                tid,
                pid,
                &process_name(&group),
                markers_to_json(markers),
            )
        })
        .collect();

    json!({
        "meta": build_meta(),
        "libs": [],
        "pages": [],
        "shared": build_shared(strings.into_vec()),
        "threads": threads
    })
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

    fn gap(lane: &str, kind: GapKind, start: &str, closing_text: Option<&str>) -> Gap {
        Gap {
            lane: lane.to_string(),
            kind,
            start: start.to_string(),
            end: start.to_string(),
            duration_ms: 1,
            closing_text: closing_text.map(str::to_string),
        }
    }

    fn fixture_groups() -> HashMap<String, String> {
        [
            ("main".to_string(), "main".to_string()),
            ("agent-sub1".to_string(), "agent-sub1".to_string()),
            ("agent-sub2".to_string(), "agent-sub1".to_string()),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn category_taxonomy_covers_known_tool_names() {
        assert_eq!(category_for_tool("Bash"), 0);
        assert_eq!(category_for_tool("Read"), 1);
        assert_eq!(category_for_tool("Edit"), 1);
        assert_eq!(category_for_tool("Write"), 1);
        assert_eq!(category_for_tool("Grep"), 2);
        assert_eq!(category_for_tool("Glob"), 2);
        assert_eq!(category_for_tool("ToolSearch"), 2);
        assert_eq!(category_for_tool("mcp__qmd__query"), 3);
        assert_eq!(category_for_tool("Agent"), 4);
        assert_eq!(category_for_tool("Skill"), 4);
        assert_eq!(category_for_tool("SomeBrandNewTool"), 4);
    }

    #[test]
    fn category_taxonomy_covers_both_gap_kinds() {
        assert_eq!(category_for_gap(GapKind::Think), 5);
        assert_eq!(category_for_gap(GapKind::Human), 6);
    }

    #[test]
    fn categories_stay_within_the_ten_color_palette() {
        assert!(
            CATEGORIES.len() <= 10,
            "Firefox Profiler's GraphColor palette has only 10 values"
        );
    }

    #[test]
    fn string_table_interns_each_distinct_string_once() {
        let mut strings = StringTable::new();
        let a = strings.intern("Bash");
        let b = strings.intern("think");
        let a_again = strings.intern("Bash");
        assert_eq!(a, a_again);
        assert_ne!(a, b);
        assert_eq!(
            strings.into_vec(),
            vec!["Bash".to_string(), "think".to_string()]
        );
    }

    #[test]
    fn tool_span_becomes_a_toolcall_marker_with_its_category() {
        let spans = vec![span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100)];
        let profile = build_profile(
            &spans,
            &[],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let markers = &profile["threads"][0]["markers"];
        assert_eq!(markers["length"], 1);
        assert_eq!(markers["category"][0], 0, "Bash is category 0");
        assert_eq!(markers["data"][0]["type"], "ToolCall");

        let name_index = markers["name"][0].as_u64().unwrap() as usize;
        assert_eq!(profile["shared"]["stringArray"][name_index], "Bash");
    }

    #[test]
    fn error_span_gets_a_suffixed_marker_name() {
        let mut spans = vec![span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100)];
        spans[0].is_error = true;
        let profile = build_profile(
            &spans,
            &[],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let name_index = profile["threads"][0]["markers"]["name"][0]
            .as_u64()
            .unwrap() as usize;
        assert_eq!(profile["shared"]["stringArray"][name_index], "Bash (error)");
    }

    #[test]
    fn gap_becomes_a_gap_marker_with_its_kind_category() {
        let gaps = vec![gap(
            "main",
            GapKind::Human,
            "2026-09-10T12:00:00.000Z",
            Some("hi"),
        )];
        let profile = build_profile(
            &[],
            &gaps,
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let markers = &profile["threads"][0]["markers"];
        assert_eq!(markers["length"], 1);
        assert_eq!(markers["category"][0], 6, "Human gap is category 6");
        assert_eq!(markers["data"][0]["type"], "Gap");
        assert_eq!(markers["data"][0]["detail"], "hi");

        let name_index = markers["name"][0].as_u64().unwrap() as usize;
        assert_eq!(
            profile["shared"]["stringArray"][name_index],
            "blocked on you"
        );
    }

    #[test]
    fn gap_omits_detail_when_closing_text_is_absent_or_blank() {
        let gaps = vec![
            gap("main", GapKind::Think, "2026-09-10T12:00:00.000Z", None),
            gap(
                "main",
                GapKind::Think,
                "2026-09-10T12:00:01.000Z",
                Some("   "),
            ),
        ];
        let profile = build_profile(
            &[],
            &gaps,
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let data = &profile["threads"][0]["markers"]["data"];
        assert!(data[0].get("detail").is_none());
        assert!(data[1].get("detail").is_none());
    }

    /// Same parity check as perfetto.rs's
    /// `pid_groups_a_depth_two_lane_under_its_depth_one_ancestor`: agent-sub2
    /// (depth 2, dispatched from inside agent-sub1) must land on
    /// agent-sub1's pid/thread, not get its own.
    #[test]
    fn pid_groups_a_depth_two_lane_under_its_depth_one_ancestor() {
        let spans = vec![
            span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100),
            span("agent-sub1", "toolu_2", "2026-09-10T12:00:01.000Z", 200),
            span("agent-sub2", "toolu_3", "2026-09-10T12:00:02.000Z", 300),
        ];
        let profile = build_profile(
            &spans,
            &[],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let threads = profile["threads"].as_array().unwrap();
        assert_eq!(threads.len(), 3, "one thread per lane");

        let pid_of = |lane: &str| -> Value {
            threads
                .iter()
                .find(|t| t["name"] == lane)
                .expect("lane must have a thread")["pid"]
                .clone()
        };

        let main_pid = pid_of("main");
        let sub1_pid = pid_of("agent-sub1");
        let sub2_pid = pid_of("agent-sub2");

        assert_ne!(main_pid, sub1_pid);
        assert_eq!(
            sub1_pid, sub2_pid,
            "agent-sub2 must share agent-sub1's pid, not get its own"
        );
    }

    #[test]
    fn markers_on_one_lane_are_sorted_by_start_time() {
        let spans = vec![
            span("main", "toolu_2", "2026-09-10T12:00:05.000Z", 100),
            span("main", "toolu_1", "2026-09-10T12:00:00.000Z", 100),
        ];
        let profile = build_profile(
            &spans,
            &[],
            "a1b2c3d4-0000-4000-8000-000000000001",
            &fixture_groups(),
        );

        let start_time = &profile["threads"][0]["markers"]["startTime"];
        let a = start_time[0].as_f64().unwrap();
        let b = start_time[1].as_f64().unwrap();
        assert!(a < b, "markers must be emitted in start-time order");
    }
}
