pub mod sessions;
pub mod tools;
pub mod messages;
pub mod projects;
pub mod sql;
pub mod schema;

use crate::style;

/// Validate a --count-by column name. Resolves aliases (e.g. "session" -> "session_id").
/// On invalid column, prints error to stderr and exits.
/// Returns the resolved SQL column name.
pub fn validate_count_by(column: &str, valid_columns: &[&str], command_name: &str) -> String {
    let canonical = match column {
        "session" => "session_id",
        other => other,
    };
    if valid_columns.contains(&canonical) {
        return canonical.to_string();
    }
    // Build display names (friendly aliases)
    let display_names: Vec<&str> = valid_columns
        .iter()
        .map(|c| match *c {
            "session_id" => "session",
            other => other,
        })
        .collect();
    eprintln!(
        "Error: Unknown count-by column '{}' for {}\nValid columns: {}",
        column,
        command_name,
        display_names.join(", "),
    );
    std::process::exit(1);
}

/// Describes a grep-style context window around matches.
/// `before` and `after` are message counts in the same session.
#[derive(Clone, Copy, Debug)]
pub struct ContextWindow {
    pub before: usize,
    pub after: usize,
}

impl ContextWindow {
    /// Resolve clap's --after/--before/--context trio into an Option<ContextWindow>.
    /// Returns None when no context flag is set.
    /// `--context` (if set) wins over `--after` and `--before` (clap's conflicts_with_all
    /// should already prevent mixing, but we defend anyway).
    pub fn from_flags(after: Option<usize>, before: Option<usize>, context: Option<usize>) -> Option<Self> {
        if let Some(c) = context {
            return Some(ContextWindow { before: c, after: c });
        }
        if after.is_none() && before.is_none() {
            return None;
        }
        Some(ContextWindow {
            before: before.unwrap_or(0),
            after: after.unwrap_or(0),
        })
    }
}

/// Error out when --count-by is combined with context flags.
/// Aggregation produces summary rows; context surrounds individual rows. Incompatible.
pub fn check_count_by_context_conflict(count_by: Option<&str>, ctx: Option<ContextWindow>) {
    if count_by.is_some() && ctx.is_some() {
        eprintln!(
            "Error: --count-by cannot be used with -A, -B, or -C\n\
             --count-by aggregates rows into counts; context flags surround individual matches with nearby messages"
        );
        std::process::exit(1);
    }
}

/// Check that --count-by and --fields are not both specified.
/// If both are set, prints error to stderr and exits.
pub fn check_count_by_fields_conflict(count_by: Option<&str>, fields: Option<&[&str]>) {
    if count_by.is_some() && fields.is_some() {
        eprintln!(
            "Error: --count-by and --fields cannot be used together\n\
             --count-by aggregates rows into counts; --fields selects columns from detail rows"
        );
        std::process::exit(1);
    }
}

/// Render a bar chart from (label, count) pairs. Used by --count-by and tools summary mode.
pub fn render_bar_chart(rows: &[(String, i64)]) {
    let max_count = rows.iter().map(|r| r.1).max().unwrap_or(1);
    let name_width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let count_width = rows.iter().map(|r| r.1.to_string().len()).max().unwrap_or(0);

    for row in rows {
        let name_padded = style::pad_right(&row.0, name_width);
        let bar_str = style::bar(row.1, max_count, 30);
        let count_str = row.1.to_string();
        let count_padded = style::pad_left(&count_str, count_width);

        println!(
            "{}  {}  {}",
            style::color(&name_padded, style::Color::Primary),
            style::color(&bar_str, style::Color::Bar),
            style::color(&count_padded, style::Color::Dim),
        );
    }
}

/// Validate field names for --fields flag. Resolves aliases (e.g. "session" -> "session_id").
/// On invalid field, prints error to stderr and exits.
pub fn validate_fields(fields: &[&str], valid_fields: &[&str], command_name: &str) -> Vec<String> {
    let mut resolved = Vec::new();
    for field in fields {
        let f = field.trim();
        // Resolve aliases
        let canonical = match f {
            "session" => "session_id",
            other => other,
        };
        if valid_fields.contains(&canonical) {
            resolved.push(canonical.to_string());
        } else {
            eprintln!(
                "Error: Unknown field '{}' for {}\nValid fields: {}\nHint: Run 'cq schema {}' for field descriptions",
                f,
                command_name,
                valid_fields.join(", "),
                command_name,
            );
            std::process::exit(1);
        }
    }
    resolved
}

/// Returns "LIMIT N" or empty string when limit is 0 (unlimited).
pub fn limit_clause(limit: usize) -> String {
    if limit == 0 {
        String::new()
    } else {
        format!("LIMIT {limit}")
    }
}

/// Returns "OFFSET N" or empty string when offset is 0.
pub fn offset_clause(offset: usize) -> String {
    if offset == 0 {
        String::new()
    } else {
        format!("OFFSET {offset}")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_window_none_when_no_flags() {
        let ctx = ContextWindow::from_flags(None, None, None);
        assert!(ctx.is_none());
    }

    #[test]
    fn context_window_c_sets_both() {
        let ctx = ContextWindow::from_flags(None, None, Some(3)).unwrap();
        assert_eq!(ctx.before, 3);
        assert_eq!(ctx.after, 3);
    }

    #[test]
    fn context_window_explicit_a_b() {
        let ctx = ContextWindow::from_flags(Some(5), Some(2), None).unwrap();
        assert_eq!(ctx.before, 2);
        assert_eq!(ctx.after, 5);
    }

    #[test]
    fn context_window_a_only_b_defaults_to_zero() {
        let ctx = ContextWindow::from_flags(Some(4), None, None).unwrap();
        assert_eq!(ctx.before, 0);
        assert_eq!(ctx.after, 4);
    }

    #[test]
    fn context_window_b_only_a_defaults_to_zero() {
        let ctx = ContextWindow::from_flags(None, Some(4), None).unwrap();
        assert_eq!(ctx.before, 4);
        assert_eq!(ctx.after, 0);
    }
}
