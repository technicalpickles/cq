use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};

/// Validate that a session ID looks like a UUID.
/// Format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx (8-4-4-4-12 hex chars)
pub fn validate_session_id(id: &str) -> Result<()> {
    let parts: Vec<&str> = id.split('-').collect();
    let valid = parts.len() == 5
        && parts[0].len() == 8
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() == 4
        && parts[4].len() == 12
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()));

    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "'{}' is not a valid session ID. Expected UUID format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx\nHint: Run 'cq sessions' to find session IDs",
            id
        ))
    }
}

#[derive(Debug, Clone)]
pub struct QueryScope {
    pub project: Option<String>,
    pub session: Option<String>,
    pub since: Option<String>,
    pub source: Option<String>,
    pub harness: Option<String>,
}

impl QueryScope {
    pub fn new(project: Option<String>, session: Option<String>, since: Option<String>) -> Self {
        Self {
            project,
            session,
            since,
            source: None,
            harness: None,
        }
    }

    /// Set the source filter (builder style so existing `new` callers are untouched).
    pub fn with_source(mut self, source: Option<String>) -> Self {
        self.source = source;
        self
    }

    /// Set the transcript harness filter (for example, `claude` or `codex`).
    pub fn with_harness(mut self, harness: Option<String>) -> Self {
        self.harness = harness;
        self
    }

    /// Parse --since into an absolute timestamp cutoff.
    /// Returns Ok(None) if no --since was provided.
    pub fn since_timestamp(&self) -> Result<Option<DateTime<Utc>>> {
        let since = match &self.since {
            Some(s) => s,
            None => return Ok(None),
        };
        Ok(Some(Utc::now() - parse_duration(since)?))
    }
}

/// Parse a `<number><unit>` duration such as `7d`, `24h`, or `30m`. Shared by
/// `--since` and by the search index's staleness window, so both accept exactly
/// the same grammar.
pub fn parse_duration(value: &str) -> Result<Duration> {
    let len = value.len();
    if len < 2 {
        return Err(anyhow!(
            "Invalid duration '{value}'\nExpected format: <number><unit> (e.g. 7d, 24h, 30m)"
        ));
    }

    let (num_str, unit) = value.split_at(len - 1);
    let num: i64 = num_str.parse().map_err(|_| {
        anyhow!("Invalid duration '{value}'\nExpected format: <number><unit> (e.g. 7d, 24h, 30m)")
    })?;

    match unit {
        "d" => Ok(Duration::days(num)),
        "h" => Ok(Duration::hours(num)),
        "m" => Ok(Duration::minutes(num)),
        "s" => Ok(Duration::seconds(num)),
        _ => Err(anyhow!("Unknown duration unit '{unit}' in '{value}'\nValid units: d (days), h (hours), m (minutes), s (seconds)")),
    }
}

/// The Claude session this process is running inside, if any. Mirrors the Codex
/// helpers above: a pure function for testing, plus a thin environment reader.
pub fn active_claude_session() -> Option<String> {
    std::env::var("CLAUDE_SESSION_ID")
        .ok()
        .filter(|value| !value.is_empty())
}

/// SQL predicate for CQ's Claude-only `--source` dimension.
/// `prefix` is an optional relation prefix such as `"tc."`.
pub fn source_filter_sql(prefix: &str) -> String {
    format!("{prefix}source = ?")
}

/// SQL predicate for CQ's top-level transcript harness dimension.
/// `prefix` is an optional relation prefix such as `"tc."`.
pub fn harness_filter_sql(prefix: &str) -> String {
    format!("{prefix}harness = ?")
}

/// True when the process is running inside a Codex session. Either variable is
/// sufficient because the Codex runtime has used both identifiers.
pub fn is_codex_runtime(session_id: Option<&str>, thread_id: Option<&str>) -> bool {
    [session_id, thread_id]
        .into_iter()
        .flatten()
        .any(|value| !value.is_empty())
}

/// Infer the active harness from runtime metadata. Codex runtime identifiers
/// select Codex; every other runtime selects Claude.
pub fn inferred_harness(session_id: Option<&str>, thread_id: Option<&str>) -> &'static str {
    if is_codex_runtime(session_id, thread_id) {
        "codex"
    } else {
        "claude"
    }
}

/// Return the active harness inferred from the current process environment.
pub fn active_harness() -> &'static str {
    inferred_harness(
        std::env::var("CODEX_SESSION_ID").ok().as_deref(),
        std::env::var("CODEX_THREAD_ID").ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_since_days() {
        let scope = QueryScope::new(None, None, Some("7d".to_string()));
        let cutoff = scope.since_timestamp().unwrap().unwrap();
        let now = chrono::Utc::now();
        let diff = now - cutoff;
        assert!(diff.num_days() >= 6 && diff.num_days() <= 7);
    }

    #[test]
    fn parse_since_hours() {
        let scope = QueryScope::new(None, None, Some("24h".to_string()));
        let cutoff = scope.since_timestamp().unwrap().unwrap();
        let now = chrono::Utc::now();
        let diff = now - cutoff;
        assert!(diff.num_hours() >= 23 && diff.num_hours() <= 24);
    }

    #[test]
    fn parse_since_minutes() {
        let scope = QueryScope::new(None, None, Some("30m".to_string()));
        let cutoff = scope.since_timestamp().unwrap().unwrap();
        let now = chrono::Utc::now();
        let diff = now - cutoff;
        assert!(diff.num_minutes() >= 29 && diff.num_minutes() <= 30);
    }

    #[test]
    fn parse_since_invalid() {
        let scope = QueryScope::new(None, None, Some("7x".to_string()));
        assert!(scope.since_timestamp().is_err());
    }

    #[test]
    fn no_since_returns_none() {
        let scope = QueryScope::new(None, None, None);
        assert!(scope.since_timestamp().unwrap().is_none());
    }

    #[test]
    fn validate_session_id_valid() {
        assert!(validate_session_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
    }

    #[test]
    fn validate_session_id_short_prefix() {
        assert!(validate_session_id("550e8400").is_err());
    }

    #[test]
    fn validate_session_id_garbage() {
        assert!(validate_session_id("not-a-uuid").is_err());
    }

    #[test]
    fn validate_session_id_empty() {
        assert!(validate_session_id("").is_err());
    }

    #[test]
    fn detects_codex_runtime_from_either_identifier() {
        assert!(is_codex_runtime(Some("session"), None));
        assert!(is_codex_runtime(None, Some("thread")));
        assert!(!is_codex_runtime(Some(""), Some("")));
        assert!(!is_codex_runtime(None, None));
    }
    #[test]
    fn infers_claude_except_in_a_codex_runtime() {
        assert_eq!(inferred_harness(None, None), "claude");
        assert_eq!(inferred_harness(Some("session"), None), "codex");
        assert_eq!(inferred_harness(None, Some("thread")), "codex");
    }
}
