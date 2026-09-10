//! Terminal waterfall renderer for a session trace.
//!
//! One row per lane, bars scaled to terminal width. The header always states
//! the time-per-column, because at full-session zoom a burst of rapid calls
//! collapses into a single block and the scale is the only thing that tells
//! you so.
//!
//! Any wall-clock figure printed here is either main-lane-only (`blocked on
//! you`) or a plain sum of tool-work across lanes labeled as exactly that
//! (`tool work`) -- never a naive sum of gap durations across lanes, which
//! run concurrently and would double- or triple-count wall clock. See
//! "Known ambiguity" and the headline measurement in
//! `docs/specs/2026-09-10-session-trace-view-design.md`.

use crate::trace::{epoch_ms, Gap, GapKind, Span};
use anyhow::Result;

const BAR: char = '\u{2588}';

/// Lane order: main first, then by first activity, so subagent fan-out reads
/// top-to-bottom as a cascade.
fn lane_order(spans: &[Span]) -> Vec<String> {
    let mut seen: Vec<(String, i64)> = Vec::new();
    for s in spans {
        if !seen.iter().any(|(l, _)| *l == s.lane) {
            seen.push((s.lane.clone(), epoch_ms(&s.start)));
        }
    }
    seen.sort_by_key(|(lane, first)| (lane != "main", *first));
    seen.into_iter().map(|(l, _)| l).collect()
}

/// "Blocked on you": human gaps on the main lane only.
///
/// A human only ever talks to the main loop -- subagents have no human
/// turns -- so this is already the correct wall-clock figure, not an
/// approximation of one. Summing `GapKind::Human` across every lane instead
/// would double-count if a subagent's own opening turn were ever
/// misclassified as human, and would be actively misleading framing even
/// when it isn't: a "blocked on you" total that isn't scoped to the one lane
/// a human can actually block invites exactly the kind of impossible-looking
/// total the design doc warns about for gaps in general.
fn blocked_on_you_ms(gaps: &[Gap]) -> i64 {
    gaps.iter()
        .filter(|g| g.lane == "main" && g.kind == GapKind::Human)
        .map(|g| g.duration_ms)
        .sum()
}

/// Shorten a lane id for the row label: `main` as-is, `agent-<id>` down to a
/// short prefix of the id so rows stay aligned.
fn short_lane(lane: &str) -> String {
    if lane == "main" {
        return lane.to_string();
    }
    lane.strip_prefix("agent-")
        .map(|s| s.chars().take(9).collect())
        .unwrap_or_else(|| lane.to_string())
}

/// Terminal width in columns. `COLUMNS` if set (as `assert_cmd`-driven tests
/// and many shells export it), else a sane fallback for a piped/non-tty
/// invocation. cq has no existing terminal-width helper to reuse here --
/// `--wide`'s truncation elsewhere is a boolean toggle, not a real column
/// query -- so this is deliberately the only place that reads `COLUMNS`.
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(100)
}

/// Render the waterfall to stdout. `spans` and `gaps` are assumed
/// pre-windowed by the caller (`commands::trace::run` applies `--from`/`--to`
/// before calling this).
pub fn render(spans: &[Span], gaps: &[Gap]) -> Result<()> {
    if spans.is_empty() {
        println!("No spans in this session (or the window is empty).");
        return Ok(());
    }

    let width: usize = terminal_width().saturating_sub(28).max(20);
    let t0 = spans.iter().map(|s| epoch_ms(&s.start)).min().unwrap_or(0);
    let t1 = spans
        .iter()
        .map(|s| epoch_ms(&s.end))
        .max()
        .unwrap_or(t0 + 1);
    let total = (t1 - t0).max(1);

    let per_col = total as f64 / width as f64 / 1000.0;
    let scale = if per_col >= 60.0 {
        format!("{:.1} min/col", per_col / 60.0)
    } else {
        format!("{per_col:.1} s/col")
    };

    let lanes = lane_order(spans);

    // Summed across every lane on purpose: this is total tool-work done, a
    // number worth knowing in its own right, but it is only a fraction of
    // wall clock when lanes never overlap -- which they do. Printed as work
    // done, never as a percentage of `total`.
    let tool_ms: i64 = spans.iter().map(|s| s.duration_ms).sum();
    let blocked_ms = blocked_on_you_ms(gaps);

    println!(
        "{} spans  {} lanes  {:.1} min wall  [{width} cols = {scale}]",
        spans.len(),
        lanes.len(),
        total as f64 / 60_000.0,
    );
    println!(
        "{:.1} min of tool work across {} lane{}   blocked on you {:.1} min",
        tool_ms as f64 / 60_000.0,
        lanes.len(),
        if lanes.len() == 1 { "" } else { "s" },
        blocked_ms as f64 / 60_000.0,
    );

    for lane in &lanes {
        let lane_spans: Vec<&Span> = spans.iter().filter(|s| s.lane == *lane).collect();
        let mut buf = vec![' '; width];
        for s in &lane_spans {
            let a =
                (((epoch_ms(&s.start) - t0) as f64 / total as f64) * (width - 1) as f64) as usize;
            let b = (((epoch_ms(&s.end) - t0) as f64 / total as f64) * (width - 1) as f64) as usize;
            for cell in buf.iter_mut().take(b.max(a) + 1).skip(a) {
                *cell = BAR;
            }
        }
        let label = short_lane(lane);
        println!(
            "{label:<18.18} {:>4} {}",
            lane_spans.len(),
            buf.iter().collect::<String>()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{Gap, GapKind};

    fn gap(lane: &str, kind: GapKind, duration_ms: i64) -> Gap {
        Gap {
            lane: lane.to_string(),
            kind,
            start: String::new(),
            end: String::new(),
            duration_ms,
        }
    }

    /// Mutation target for the "sum human gaps across every lane" bug the
    /// plan draft shipped: naively summing would give 60_000 + 999_000. Only
    /// the main lane's 60_000ms may count.
    #[test]
    fn blocked_on_you_counts_main_lane_human_gaps_only() {
        let gaps = vec![
            gap("main", GapKind::Human, 60_000),
            gap("agent-sub1", GapKind::Human, 999_000),
            gap("main", GapKind::Think, 5_000),
        ];
        assert_eq!(blocked_on_you_ms(&gaps), 60_000);
    }

    #[test]
    fn lane_order_puts_main_first_then_by_first_activity() {
        let span = |lane: &str, start: &str| Span {
            lane: lane.to_string(),
            agent_type: None,
            name: "Bash".to_string(),
            start: start.to_string(),
            end: start.to_string(),
            duration_ms: 1,
            is_error: false,
            tool_use_id: format!("toolu_{lane}"),
            input: None,
        };
        let spans = vec![
            span("agent-sub2", "2026-09-10T12:00:13.000Z"),
            span("main", "2026-09-10T12:00:01.000Z"),
            span("agent-sub1", "2026-09-10T12:00:10.000Z"),
        ];
        assert_eq!(
            lane_order(&spans),
            vec![
                "main".to_string(),
                "agent-sub1".to_string(),
                "agent-sub2".to_string()
            ]
        );
    }
}
