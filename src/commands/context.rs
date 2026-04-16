use crate::commands::ContextWindow;

/// Emits a parameterized DuckDB SQL query that returns messages in grep-style context windows
/// around matching rows, with `match_kind` and `match_group` columns added.
///
/// The generated SQL produces two CTEs in order: `ordered` (all messages in session scope),
/// then `matches` (filtered rows). Both CTEs use `?` placeholders for dynamic parameters.
///
/// Param-positioning contract: callers must bind params in order:
/// 1. All `ordered_scope_where` params first
/// 2. All `matches_subquery` params second
/// This matches the SQL generation order (ordered CTE before matches CTE).
///
/// Trust boundary: both `matches_subquery` and `ordered_scope_where` are trusted SQL fragments.
/// Callers must use `?` placeholders for user input; never interpolate strings directly.
pub struct ContextSqlBuilder<'a> {
    pub window: ContextWindow,
    /// SQL fragment selecting matches. Must return columns `session_id` and `message_uuid`.
    pub matches_subquery: &'a str,
    /// Fully-qualified session scope conditions for the `ordered` CTE (no tool/message-specific filters).
    pub ordered_scope_where: &'a str,
    pub match_limit: usize,
}

impl<'a> ContextSqlBuilder<'a> {
    pub fn build(&self) -> String {
        let before = self.window.before;
        let after = self.window.after;
        let limit_clause = if self.match_limit == 0 {
            String::new()
        } else {
            format!("LIMIT {}", self.match_limit)
        };
        format!(
            r#"
WITH ordered AS (
    SELECT session_id, uuid, type, timestamp, text, model, tool_count, project,
           ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY timestamp, uuid) AS ord
    FROM messages
    WHERE {ordered_scope_where}
),
matches AS (
    SELECT m.session_id, m.message_uuid, o.ord AS match_ord,
           ROW_NUMBER() OVER (ORDER BY m.session_id, o.ord) AS match_idx
    FROM ({matches_subquery}) m
    JOIN ordered o ON m.session_id = o.session_id AND m.message_uuid = o.uuid
    ORDER BY m.session_id, o.ord
    {limit_clause}
),
expanded AS (
    SELECT o.session_id, o.uuid, o.type, o.timestamp, o.text, o.model, o.tool_count, o.project, o.ord,
           m.match_ord, m.match_idx,
           CASE
               WHEN o.ord = m.match_ord THEN 'match'
               WHEN o.ord < m.match_ord THEN 'before'
               ELSE 'after'
           END AS match_kind
    FROM ordered o
    JOIN matches m
      ON o.session_id = m.session_id
     AND o.ord BETWEEN m.match_ord - {before} AND m.match_ord + {after}
),
deduped AS (
    SELECT session_id, ord,
           ANY_VALUE(uuid) AS uuid,
           ANY_VALUE(type) AS type,
           ANY_VALUE(timestamp) AS timestamp,
           ANY_VALUE(text) AS text,
           ANY_VALUE(model) AS model,
           ANY_VALUE(tool_count) AS tool_count,
           ANY_VALUE(project) AS project,
           MIN(match_idx) AS match_idx,
           MAX(CASE WHEN match_kind = 'match' THEN 1 ELSE 0 END) AS is_match_any,
           ANY_VALUE(match_kind) AS any_kind
    FROM expanded
    GROUP BY session_id, ord
),
grouped AS (
    SELECT *,
           SUM(CASE WHEN ord = LAG(ord) OVER (PARTITION BY session_id ORDER BY ord) + 1 THEN 0 ELSE 1 END)
             OVER (PARTITION BY session_id ORDER BY ord) AS match_group
    FROM deduped
)
SELECT session_id, uuid, type, timestamp, text, model, tool_count, project,
       CASE WHEN is_match_any = 1 THEN 'match' ELSE any_kind END AS match_kind,
       match_group
FROM grouped
ORDER BY session_id, ord
"#,
            ordered_scope_where = self.ordered_scope_where,
            matches_subquery = self.matches_subquery,
            before = before,
            after = after,
            limit_clause = limit_clause,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_includes_before_and_after_bounds() {
        let b = ContextSqlBuilder {
            window: ContextWindow { before: 2, after: 3 },
            matches_subquery: "SELECT session_id, uuid AS message_uuid FROM messages WHERE type = 'user'",
            ordered_scope_where: "1=1",
            match_limit: 0,
        };
        let sql = b.build();
        assert!(sql.contains("match_ord - 2"));
        assert!(sql.contains("match_ord + 3"));
        assert!(sql.contains("SUM(CASE WHEN ord = LAG(ord)"));
        assert!(!sql.contains("LIMIT"));
    }

    #[test]
    fn builder_includes_match_limit_when_nonzero() {
        let b = ContextSqlBuilder {
            window: ContextWindow { before: 1, after: 1 },
            matches_subquery: "SELECT session_id, uuid AS message_uuid FROM messages",
            ordered_scope_where: "1=1",
            match_limit: 5,
        };
        let sql = b.build();
        assert!(sql.contains("LIMIT 5"));
    }
}
