use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone)]
pub struct QueryScope {
    pub project: Option<String>,
    pub session: Option<String>,
    pub since: Option<String>,
}

impl QueryScope {
    pub fn new(project: Option<String>, session: Option<String>, since: Option<String>) -> Self {
        Self { project, session, since }
    }

    /// Parse --since into an absolute timestamp cutoff.
    /// Returns Ok(None) if no --since was provided.
    pub fn since_timestamp(&self) -> Result<Option<DateTime<Utc>>> {
        let since = match &self.since {
            Some(s) => s,
            None => return Ok(None),
        };

        let len = since.len();
        if len < 2 {
            return Err(anyhow!("Invalid duration: {since}"));
        }

        let (num_str, unit) = since.split_at(len - 1);
        let num: i64 = num_str
            .parse()
            .map_err(|_| anyhow!("Invalid duration number: {num_str}"))?;

        let duration = match unit {
            "d" => Duration::days(num),
            "h" => Duration::hours(num),
            "m" => Duration::minutes(num),
            _ => return Err(anyhow!("Unknown duration unit: {unit}. Use d, h, or m.")),
        };

        Ok(Some(Utc::now() - duration))
    }
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
}
