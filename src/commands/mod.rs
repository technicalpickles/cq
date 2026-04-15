pub mod sessions;
pub mod tools;
pub mod messages;
pub mod projects;
pub mod sql;
pub mod schema;

use crate::style;

/// Returns "LIMIT N" or empty string when limit is 0 (unlimited).
pub fn limit_clause(limit: usize) -> String {
    if limit == 0 {
        String::new()
    } else {
        format!("LIMIT {limit}")
    }
}

/// Print "Session <id> not found." when --session is specified but no results match.
pub fn print_session_not_found(session_id: &str) {
    let short = &session_id[..std::cmp::min(8, session_id.len())];
    eprintln!(
        "Session {}... not found.",
        short
    );
}

/// Print "No results." with contextual suggestions based on active filters.
pub fn print_no_results(scope: &crate::scope::QueryScope, extra_filters: &[&str]) {
    eprintln!("No results.");

    let mut active: Vec<String> = Vec::new();
    if scope.project.is_some() {
        active.push("--project".to_string());
    }
    if scope.session.is_some() {
        active.push("--session".to_string());
    }
    if scope.since.is_some() {
        active.push("--since".to_string());
    }
    for f in extra_filters {
        active.push(f.to_string());
    }

    if !active.is_empty() {
        eprintln!(
            "{}",
            style::hint(&format!("Active filters: {}. Try broadening or removing one.", active.join(", ")))
        );
    }
}

/// Run a COUNT(*) with the same WHERE clause and print a truncation hint if results were capped.
/// Call after rendering results. `displayed` is how many rows were actually shown.
/// Skips the hint if limit is 0 (unlimited) or displayed < limit (all rows fit).
pub fn print_truncation_hint(
    conn: &duckdb::Connection,
    from_clause: &str,
    where_clause: &str,
    params: &[&dyn duckdb::types::ToSql],
    displayed: usize,
    limit: usize,
) {
    if limit == 0 || displayed < limit {
        return;
    }

    let count_sql = format!("SELECT COUNT(*) FROM {from_clause} WHERE {where_clause}");
    if let Ok(total) = conn.query_row(&count_sql, params, |row| row.get::<_, i64>(0)) {
        let total = total as usize;
        if total > displayed {
            eprintln!(
                "{}",
                style::hint(&format!(
                    "Showing {} of {} results. Use --limit 0 for all.",
                    displayed, total
                ))
            );
        }
    }
}
