use owo_colors::{OwoColorize, Stream::Stdout};
use chrono::{DateTime, Utc};

pub enum Color {
    Primary,
    Secondary,
    Dim,
    Bar,
}

pub fn color(text: &str, role: Color) -> String {
    match role {
        Color::Primary => format!("{}", text.if_supports_color(Stdout, |t| t.blue())),
        Color::Secondary => format!("{}", text.if_supports_color(Stdout, |t| t.yellow())),
        Color::Dim => format!("{}", text.if_supports_color(Stdout, |t| t.dimmed())),
        Color::Bar => format!("{}", text.if_supports_color(Stdout, |t| t.green())),
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max <= 3 {
        s.chars().take(max).collect()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

pub fn null_display() -> &'static str {
    "-"
}

pub fn short_id(id: &str, len: usize) -> String {
    id.chars().take(len).collect()
}

pub fn relative_time(iso_ts: &str) -> String {
    let parsed = DateTime::parse_from_rfc3339(iso_ts)
        .or_else(|_| DateTime::parse_from_str(iso_ts, "%Y-%m-%dT%H:%M:%S%.f%z"));

    let dt = match parsed {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return iso_ts.to_string(),
    };

    let now = Utc::now();
    let diff = now.signed_duration_since(dt);
    let secs = diff.num_seconds();

    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", diff.num_minutes())
    } else if secs < 86400 {
        format!("{}h ago", diff.num_hours())
    } else {
        format!("{}d ago", diff.num_days())
    }
}

pub fn format_duration_mins(mins: i64) -> String {
    if mins < 1 {
        "<1m".to_string()
    } else if mins < 60 {
        format!("{}m", mins)
    } else {
        let h = mins / 60;
        let m = mins % 60;
        if m == 0 {
            format!("{}h", h)
        } else {
            format!("{}h{}m", h, m)
        }
    }
}

pub fn pad_right(s: &str, width: usize) -> String {
    let len = s.len();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}

pub fn pad_left(s: &str, width: usize) -> String {
    let len = s.len();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat(width - len), s)
    }
}

pub fn align_columns(rows: &[Vec<String>]) -> Vec<String> {
    if rows.is_empty() {
        return vec![];
    }

    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; ncols];

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    rows.iter()
        .map(|row| {
            let padded: Vec<String> = (0..ncols)
                .map(|i| {
                    let cell = row.get(i).map(|s| s.as_str()).unwrap_or("");
                    // Last column: no trailing padding
                    if i == ncols - 1 {
                        cell.to_string()
                    } else {
                        pad_right(cell, widths[i])
                    }
                })
                .collect();
            padded.join("  ")
        })
        .collect()
}

pub fn print_light_table(headers: &[&str], rows: &[Vec<String>]) {
    if rows.is_empty() {
        eprintln!("No results.");
        return;
    }
    if headers.is_empty() {
        return;
    }

    let ncols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < ncols && cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    // Print header row
    let header_cells: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            if i == ncols - 1 {
                h.to_string()
            } else {
                pad_right(h, widths[i])
            }
        })
        .collect();
    println!("{}", color(&header_cells.join("  "), Color::Dim));

    // Print separator
    let sep_cells: Vec<String> = widths
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            if i == ncols - 1 {
                "\u{2500}".repeat(w)
            } else {
                "\u{2500}".repeat(w)
            }
        })
        .collect();
    println!("{}", color(&sep_cells.join("  "), Color::Dim));

    // Print data rows
    for row in rows {
        let cells: Vec<String> = (0..ncols)
            .map(|i| {
                let cell = row.get(i).map(|s| s.as_str()).unwrap_or("");
                if i == ncols - 1 {
                    cell.to_string()
                } else {
                    pad_right(cell, widths[i])
                }
            })
            .collect();
        println!("{}", cells.join("  "));
    }
}

pub fn bar(value: i64, max_value: i64, max_width: usize) -> String {
    if max_value <= 0 || max_width == 0 {
        return String::new();
    }
    let ratio = value as f64 / max_value as f64;
    let filled = ((ratio * max_width as f64).round() as usize).max(1).min(max_width);
    "\u{2588}".repeat(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    // truncate tests
    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    // null_display test
    #[test]
    fn null_display_returns_dash() {
        assert_eq!(null_display(), "-");
    }

    // short_id tests
    #[test]
    fn short_id_8_chars() {
        let id = "abcdef1234567890";
        assert_eq!(short_id(id, 8), "abcdef12");
    }

    #[test]
    fn short_id_zero_chars() {
        assert_eq!(short_id("abcdef", 0), "");
    }

    #[test]
    fn short_id_full_36_chars() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(short_id(id, 36), id);
    }

    // relative_time tests
    #[test]
    fn relative_time_minutes() {
        let ts = (Utc::now() - chrono::Duration::minutes(16)).to_rfc3339();
        let result = relative_time(&ts);
        assert_eq!(result, "16m ago");
    }

    #[test]
    fn relative_time_hours() {
        let ts = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let result = relative_time(&ts);
        assert_eq!(result, "2h ago");
    }

    #[test]
    fn relative_time_days() {
        let ts = (Utc::now() - chrono::Duration::days(3)).to_rfc3339();
        let result = relative_time(&ts);
        assert_eq!(result, "3d ago");
    }

    // format_duration_mins tests
    #[test]
    fn format_duration_minutes_only() {
        assert_eq!(format_duration_mins(16), "16m");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration_mins(150), "2h30m");
    }

    #[test]
    fn format_duration_zero() {
        assert_eq!(format_duration_mins(0), "<1m");
    }

    // align_columns test
    #[test]
    fn align_columns_mixed_widths() {
        let rows = vec![
            vec!["foo".to_string(), "bar".to_string()],
            vec!["longer".to_string(), "x".to_string()],
        ];
        let result = align_columns(&rows);
        // First col width = 6 ("longer"), second col is last so no trailing pad
        assert_eq!(result[0], "foo     bar");
        assert_eq!(result[1], "longer  x");
    }

    // bar tests
    #[test]
    fn bar_proportional() {
        let result = bar(50, 100, 10);
        assert_eq!(result, "\u{2588}".repeat(5));
    }

    #[test]
    fn bar_minimum_one_char() {
        let result = bar(1, 1000, 10);
        assert_eq!(result, "\u{2588}");
    }
}
