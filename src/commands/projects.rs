use anyhow::Result;
use duckdb::Connection;
use duckdb::types::Value;

use crate::output::OutputFormat;
use crate::scope::QueryScope;
use crate::style;

struct ProjectRow {
    project: String,
    sessions: i64,
    messages: i64,
    tools: i64,
    skills: i64,
    last_activity: String,
}

struct SkillRow {
    project: String,
    skill: String,
}

fn val_str(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Null => String::new(),
        other => format!("{:?}", other),
    }
}

fn val_i64(v: &Value) -> i64 {
    match v {
        Value::TinyInt(n) => *n as i64,
        Value::SmallInt(n) => *n as i64,
        Value::Int(n) => *n as i64,
        Value::BigInt(n) => *n,
        Value::HugeInt(n) => *n as i64,
        _ => 0,
    }
}

fn project_leaf(project: &str) -> String {
    project
        .split('/')
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or(project)
        .to_string()
}

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    show_skills: bool,
    format: &OutputFormat,
    limit: usize,
) -> Result<()> {
    let mut conditions = vec!["1=1".to_string()];
    let mut params: Vec<Box<dyn duckdb::types::ToSql>> = Vec::new();

    if let Some(project) = &scope.project {
        conditions.push("s.project ILIKE ?".to_string());
        params.push(Box::new(format!("%{project}%")));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("s.started_at >= '{formatted}'"));
    }

    let where_clause = conditions.join(" AND ");
    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let limit_clause = super::limit_clause(limit);

    if matches!(format, OutputFormat::Json) {
        // JSON mode: include skill list per project
        let sql = format!(
            "SELECT
                s.project,
                count(DISTINCT s.session_id) as sessions,
                cast(sum(s.message_count) as BIGINT) as messages,
                cast(sum(s.tool_call_count) as BIGINT) as tools,
                max(s.ended_at) as last_activity
            FROM sessions s
            WHERE {where_clause}
            GROUP BY s.project
            ORDER BY last_activity DESC
            {limit_clause}"
        );

        // For JSON, build the result manually to include skills array
        let mut stmt = conn.prepare(&sql)?;
        let mut rows_iter = stmt.query(&param_refs[..])?;
        let mut json_rows: Vec<serde_json::Value> = Vec::new();

        while let Some(row) = rows_iter.next()? {
            let project = row.get::<_, String>(0).unwrap_or_default();
            let sessions = val_i64(&row.get::<_, Value>(1).unwrap_or(Value::Null));
            let messages = val_i64(&row.get::<_, Value>(2).unwrap_or(Value::Null));
            let tools = val_i64(&row.get::<_, Value>(3).unwrap_or(Value::Null));
            let last_activity = row.get::<_, String>(4).unwrap_or_default();

            // Fetch skills for this project
            let skill_sql = "SELECT DISTINCT json_extract_string(input, '$.skill') as skill
                FROM tool_calls
                WHERE name = 'Skill' AND project = ?
                ORDER BY skill";
            let mut skill_stmt = conn.prepare(skill_sql)?;
            let mut skill_iter = skill_stmt.query(&[&project as &dyn duckdb::types::ToSql])?;
            let mut skills: Vec<String> = Vec::new();
            while let Some(skill_row) = skill_iter.next()? {
                let s = skill_row.get::<_, String>(0).unwrap_or_default();
                if !s.is_empty() {
                    skills.push(s);
                }
            }

            json_rows.push(serde_json::json!({
                "project": project,
                "sessions": sessions,
                "messages": messages,
                "tools": tools,
                "skills": skills,
                "skill_count": skills.len(),
                "last_activity": last_activity,
            }));
        }

        println!("{}", serde_json::to_string_pretty(&json_rows)?);
        return Ok(());
    }

    // Query project aggregates with skill count via subquery
    let sql = format!(
        "SELECT
            s.project,
            count(DISTINCT s.session_id) as sessions,
            cast(sum(s.message_count) as BIGINT) as messages,
            cast(sum(s.tool_call_count) as BIGINT) as tools,
            (SELECT count(DISTINCT json_extract_string(tc.input, '$.skill'))
             FROM tool_calls tc
             WHERE tc.name = 'Skill' AND tc.project = s.project) as skills,
            max(s.ended_at) as last_activity
        FROM sessions s
        WHERE {where_clause}
        GROUP BY s.project
        ORDER BY last_activity DESC
        {limit_clause}"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(&param_refs[..])?;
    let mut project_rows: Vec<ProjectRow> = Vec::new();

    while let Some(row) = rows_iter.next()? {
        let values: Vec<Value> = (0..6)
            .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
            .collect();
        project_rows.push(ProjectRow {
            project: val_str(&values[0]),
            sessions: val_i64(&values[1]),
            messages: val_i64(&values[2]),
            tools: val_i64(&values[3]),
            skills: val_i64(&values[4]),
            last_activity: val_str(&values[5]),
        });
    }

    // Fetch skill names if --skills flag
    let skill_rows = if show_skills {
        let projects: Vec<&str> = project_rows.iter().map(|r| r.project.as_str()).collect();
        fetch_skills(conn, &projects)?
    } else {
        vec![]
    };

    match format {
        OutputFormat::Table => render_table(&project_rows, &skill_rows, show_skills),
        _ => render_oneline(&project_rows, &skill_rows, show_skills),
    }

    // Truncation hint (inline because projects counts distinct, not rows)
    if limit > 0 && project_rows.len() >= limit {
        let count_sql = format!(
            "SELECT COUNT(DISTINCT s.project) FROM sessions s WHERE {where_clause}"
        );
        if let Ok(total) = conn.query_row(&count_sql, &param_refs[..], |row| row.get::<_, i64>(0)) {
            let total = total as usize;
            if total > project_rows.len() {
                eprintln!("{}", style::hint(&format!(
                    "Showing {} of {} projects. Use --limit 0 for all.",
                    project_rows.len(), total
                )));
            }
        }
    }

    Ok(())
}

fn fetch_skills(conn: &Connection, projects: &[&str]) -> Result<Vec<SkillRow>> {
    if projects.is_empty() {
        return Ok(vec![]);
    }

    let sql = "SELECT project, json_extract_string(input, '$.skill') as skill
        FROM tool_calls
        WHERE name = 'Skill'
        GROUP BY project, skill
        ORDER BY project, skill";

    let mut stmt = conn.prepare(sql)?;
    let mut rows_iter = stmt.query([])?;
    let mut skills: Vec<SkillRow> = Vec::new();

    while let Some(row) = rows_iter.next()? {
        let project = row.get::<_, String>(0).unwrap_or_default();
        let skill = row.get::<_, String>(1).unwrap_or_default();
        if !skill.is_empty() && projects.contains(&project.as_str()) {
            skills.push(SkillRow { project, skill });
        }
    }

    Ok(skills)
}

fn format_count(n: i64) -> String {
    if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn render_oneline(rows: &[ProjectRow], skill_rows: &[SkillRow], show_skills: bool) {
    if rows.is_empty() {
        eprintln!("No results.");
        return;
    }

    let plain_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        let time_ago = if r.last_activity.is_empty() {
            style::null_display().to_string()
        } else {
            style::relative_time(&r.last_activity)
        };

        let project = if r.project.is_empty() {
            style::null_display().to_string()
        } else {
            project_leaf(&r.project)
        };

        let sessions = format!("{}s", r.sessions);
        let messages = format!("{} msgs", format_count(r.messages));
        let tools = format!("{} tools", format_count(r.tools));
        let skills = format!("{} skills", r.skills);

        vec![time_ago, project, sessions, messages, tools, skills]
    }).collect();

    let ncols = 6;
    let mut widths = vec![0usize; ncols];
    for row in &plain_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    for (idx, row) in plain_rows.iter().enumerate() {
        let cols: Vec<String> = row.iter().enumerate().map(|(i, cell)| {
            let padded = if i == ncols - 1 {
                cell.clone()
            } else {
                style::pad_right(cell, widths[i])
            };
            match i {
                0 => style::color(&padded, style::Color::Dim),
                1 => style::color(&padded, style::Color::Primary),
                _ => style::color(&padded, style::Color::Dim),
            }
        }).collect();
        println!("{}", cols.join("  "));

        if show_skills {
            let project = &rows[idx].project;
            let skills: Vec<&str> = skill_rows
                .iter()
                .filter(|s| s.project == *project)
                .map(|s| s.skill.as_str())
                .collect();
            if !skills.is_empty() {
                let display: Vec<&str> = skills.iter().take(4).copied().collect();
                let mut line = format!("  \u{2514} {}", display.join(", "));
                if skills.len() > 4 {
                    line.push_str(&format!(" +{}", skills.len() - 4));
                }
                println!("{}", style::color(&line, style::Color::Dim));
            }
        }
    }
}

fn render_table(rows: &[ProjectRow], skill_rows: &[SkillRow], show_skills: bool) {
    if rows.is_empty() {
        eprintln!("No results.");
        return;
    }

    let headers = if show_skills {
        vec!["last_activity", "project", "sessions", "messages", "tools", "skills", "skill_names"]
    } else {
        vec!["last_activity", "project", "sessions", "messages", "tools", "skills"]
    };

    let string_rows: Vec<Vec<String>> = rows.iter().map(|r| {
        let time_ago = if r.last_activity.is_empty() {
            style::null_display().to_string()
        } else {
            style::relative_time(&r.last_activity)
        };

        let project = if r.project.is_empty() {
            style::null_display().to_string()
        } else {
            project_leaf(&r.project)
        };

        let mut row = vec![
            time_ago,
            project,
            r.sessions.to_string(),
            format_count(r.messages),
            format_count(r.tools),
            r.skills.to_string(),
        ];

        if show_skills {
            let skills: Vec<&str> = skill_rows
                .iter()
                .filter(|s| s.project == r.project)
                .map(|s| s.skill.as_str())
                .collect();
            row.push(skills.join(", "));
        }

        row
    }).collect();

    let header_refs: Vec<&str> = headers.iter().map(|s| *s).collect();
    style::print_light_table(&header_refs, &string_rows);
}
