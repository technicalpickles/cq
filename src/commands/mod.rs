pub mod sessions;
pub mod tools;
pub mod messages;
pub mod projects;
pub mod sql;
pub mod schema;

/// Returns "LIMIT N" or empty string when limit is 0 (unlimited).
pub fn limit_clause(limit: usize) -> String {
    if limit == 0 {
        String::new()
    } else {
        format!("LIMIT {limit}")
    }
}
