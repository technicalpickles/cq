use anyhow::Result;
use duckdb::types::Value;
use duckdb::Connection;

use crate::output::OutputFormat;
use crate::scope::QueryScope;
use crate::style;

struct ProjectRow {
    project: String,
    source: String,
    sessions: i64,
    messages: i64,
    tools: i64,
    skills: i64,
    last_activity: String,
}

struct SkillRow {
    project: String,
    source: String,
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
        .rfind(|s| !s.is_empty())
        .unwrap_or(project)
        .to_string()
}

pub fn run(
    conn: &Connection,
    scope: &QueryScope,
    show_skills: bool,
    format: &OutputFormat,
    limit: usize,
    offset: usize,
    _wide: bool,
) -> Result<()> {
    // When the user did not scope to one source, the same repo path can exist
    // under multiple sources. Group by (source, project) so they show as distinct
    // rows and surface a SOURCE column. When scoped to one source, group by
    // project alone (source is constant, so the column is redundant).
    let group_by_source = scope.source.is_none();

    let mut conditions = vec!["1=1".to_string()];
    let mut params: Vec<Box<dyn duckdb::types::ToSql>> = Vec::new();

    if let Some(project) = &scope.project {
        conditions.push("s.project ILIKE ?".to_string());
        params.push(Box::new(format!("%{project}%")));
    }

    if let Some(source) = &scope.source {
        conditions.push(crate::scope::source_filter_sql("s."));
        params.push(Box::new(source.clone()));
    }

    if let Some(ts) = scope.since_timestamp()? {
        let formatted = ts.format("%Y-%m-%d %H:%M:%S").to_string();
        conditions.push(format!("s.started_at >= '{formatted}'"));
    }

    let where_clause = conditions.join(" AND ");
    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let limit_clause = super::limit_clause(limit);
    let offset_clause = super::offset_clause(offset);

    if matches!(format, OutputFormat::Json) {
        // JSON mode: include skill list per project. Always carry source so the
        // field is present regardless of scope. Group by (source, project).
        let sql = format!(
            "SELECT
                s.project,
                s.source,
                count(DISTINCT s.session_id) as sessions,
                cast(sum(s.message_count) as BIGINT) as messages,
                cast(sum(s.tool_call_count) as BIGINT) as tools,
                max(s.ended_at) as last_activity
            FROM sessions s
            WHERE {where_clause}
            GROUP BY s.project, s.source
            ORDER BY last_activity DESC
            {limit_clause}
            {offset_clause}"
        );

        // For JSON, build the result manually to include skills array
        let mut stmt = conn.prepare(&sql)?;
        let mut rows_iter = stmt.query(&param_refs[..])?;
        let mut json_rows: Vec<serde_json::Value> = Vec::new();

        while let Some(row) = rows_iter.next()? {
            let project = row.get::<_, String>(0).unwrap_or_default();
            let source = row.get::<_, String>(1).unwrap_or_default();
            let sessions = val_i64(&row.get::<_, Value>(2).unwrap_or(Value::Null));
            let messages = val_i64(&row.get::<_, Value>(3).unwrap_or(Value::Null));
            let tools = val_i64(&row.get::<_, Value>(4).unwrap_or(Value::Null));
            let last_activity = row.get::<_, String>(5).unwrap_or_default();

            // Fetch skills for this (source, project): a Skill tool_call in one
            // source must not inflate another source's skill list.
            let skill_sql = "SELECT DISTINCT json_extract_string(input, '$.skill') as skill
                FROM tool_calls
                WHERE name = 'Skill' AND project = ? AND source = ?
                ORDER BY skill";
            let mut skill_stmt = conn.prepare(skill_sql)?;
            let mut skill_iter = skill_stmt.query([
                &project as &dyn duckdb::types::ToSql,
                &source as &dyn duckdb::types::ToSql,
            ])?;
            let mut skills: Vec<String> = Vec::new();
            while let Some(skill_row) = skill_iter.next()? {
                let s = skill_row.get::<_, String>(0).unwrap_or_default();
                if !s.is_empty() {
                    skills.push(s);
                }
            }

            json_rows.push(serde_json::json!({
                "project": project,
                "source": source,
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

    // Query project aggregates with a per-(source, project) skill-count CTE.
    //
    // A correlated subquery in the SELECT list would put its `?` BEFORE the outer
    // WHERE's placeholders, making param ordering fragile. A CTE keeps the skill
    // pre-aggregation source-correct AND keeps params readable: the CTE's optional
    // `source = ?` is the FIRST placeholder in the SQL text, so we push it first.
    //
    // The CTE always groups skills by (source, project). The join key includes
    // source so a Skill call in one source can't inflate another source's count.
    let skill_cte_filter = if scope.source.is_some() {
        "AND (harness != 'claude' OR source = ?)"
    } else {
        ""
    };

    // Params in SQL-text order: 1) CTE source filter (if scoped), 2) outer WHERE
    // params (project ILIKE, then outer source) as already collected in `params`.
    let mut agg_params: Vec<Box<dyn duckdb::types::ToSql>> = Vec::new();
    if let Some(source) = &scope.source {
        agg_params.push(Box::new(source.clone()));
    }
    if let Some(project) = &scope.project {
        agg_params.push(Box::new(format!("%{project}%")));
    }
    if let Some(source) = &scope.source {
        agg_params.push(Box::new(source.clone()));
    }

    let sql = format!(
        "WITH skill_counts AS (
            SELECT source, project,
                   count(DISTINCT json_extract_string(input, '$.skill')) as skills
            FROM tool_calls
            WHERE name = 'Skill' {skill_cte_filter}
            GROUP BY source, project
        )
        SELECT
            s.project,
            s.source,
            count(DISTINCT s.session_id) as sessions,
            cast(sum(s.message_count) as BIGINT) as messages,
            cast(sum(s.tool_call_count) as BIGINT) as tools,
            COALESCE(max(sc.skills), 0) as skills,
            max(s.ended_at) as last_activity
        FROM sessions s
        LEFT JOIN skill_counts sc
            ON sc.project = s.project AND sc.source = s.source
        WHERE {where_clause}
        GROUP BY s.project, s.source
        ORDER BY last_activity DESC
        {limit_clause}
        {offset_clause}"
    );

    let agg_param_refs: Vec<&dyn duckdb::types::ToSql> =
        agg_params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(&agg_param_refs[..])?;
    let mut project_rows: Vec<ProjectRow> = Vec::new();

    while let Some(row) = rows_iter.next()? {
        let values: Vec<Value> = (0..7)
            .map(|i| row.get::<_, Value>(i).unwrap_or(Value::Null))
            .collect();
        project_rows.push(ProjectRow {
            project: val_str(&values[0]),
            source: val_str(&values[1]),
            sessions: val_i64(&values[2]),
            messages: val_i64(&values[3]),
            tools: val_i64(&values[4]),
            skills: val_i64(&values[5]),
            last_activity: val_str(&values[6]),
        });
    }

    if project_rows.is_empty() {
        super::print_no_results(scope, &[]);
        return Ok(());
    }

    // Fetch skill names if --skills flag. Keyed on (source, project) so a skill
    // call in one source doesn't show up under another source's row.
    let skill_rows = if show_skills {
        let pairs: Vec<(&str, &str)> = project_rows
            .iter()
            .map(|r| (r.source.as_str(), r.project.as_str()))
            .collect();
        fetch_skills(conn, scope.source.as_deref(), &pairs)?
    } else {
        vec![]
    };

    match format {
        OutputFormat::Table => {
            render_table(&project_rows, &skill_rows, show_skills, group_by_source)
        }
        _ => render_oneline(&project_rows, &skill_rows, show_skills, group_by_source),
    }

    // Truncation hint (inline because projects counts distinct, not rows).
    // We group by (source, project), so count distinct (source, project) pairs.
    if limit > 0 && project_rows.len() >= limit {
        let count_sql = format!(
            "SELECT COUNT(DISTINCT (s.source, s.project)) FROM sessions s WHERE {where_clause}"
        );
        if let Ok(total) = conn.query_row(&count_sql, &param_refs[..], |row| row.get::<_, i64>(0)) {
            let total = total as usize;
            if total > project_rows.len() {
                eprintln!(
                    "{}",
                    style::hint(&format!(
                        "Showing {} of {} projects. Use --limit 0 for all.",
                        project_rows.len(),
                        total
                    ))
                );
            }
        }
    }

    Ok(())
}

/// Fetch skill names per (source, project). `source_filter` restricts the scan
/// to one source when the user scoped with --source; `pairs` is the set of
/// (source, project) rows to keep. Keying on (source, project) keeps a Skill
/// call in one source from appearing under another source's row.
fn fetch_skills(
    conn: &Connection,
    source_filter: Option<&str>,
    pairs: &[(&str, &str)],
) -> Result<Vec<SkillRow>> {
    if pairs.is_empty() {
        return Ok(vec![]);
    }

    let (filter_sql, params): (&str, Vec<Box<dyn duckdb::types::ToSql>>) = match source_filter {
        Some(src) => (
            "AND (harness != 'claude' OR source = ?)",
            vec![Box::new(src.to_string())],
        ),
        None => ("", vec![]),
    };

    let sql = format!(
        "SELECT source, project, json_extract_string(input, '$.skill') as skill
        FROM tool_calls
        WHERE name = 'Skill' {filter_sql}
        GROUP BY source, project, skill
        ORDER BY source, project, skill"
    );

    let param_refs: Vec<&dyn duckdb::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows_iter = stmt.query(&param_refs[..])?;
    let mut skills: Vec<SkillRow> = Vec::new();

    while let Some(row) = rows_iter.next()? {
        let source = row.get::<_, String>(0).unwrap_or_default();
        let project = row.get::<_, String>(1).unwrap_or_default();
        let skill = row.get::<_, String>(2).unwrap_or_default();
        if !skill.is_empty() && pairs.contains(&(source.as_str(), project.as_str())) {
            skills.push(SkillRow {
                project,
                source,
                skill,
            });
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

fn render_oneline(
    rows: &[ProjectRow],
    skill_rows: &[SkillRow],
    show_skills: bool,
    show_source: bool,
) {
    // Column order: last_activity, project, [source], sessions, messages, tools, skills.
    let plain_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
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

            let mut cols = vec![time_ago, project];
            if show_source {
                cols.push(if r.source.is_empty() {
                    style::null_display().to_string()
                } else {
                    r.source.clone()
                });
            }
            cols.extend([sessions, messages, tools, skills]);
            cols
        })
        .collect();

    let ncols = if show_source { 7 } else { 6 };
    let mut widths = vec![0usize; ncols];
    for row in &plain_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    // project is at index 1 (Primary); the source column (when shown) at index 2
    // uses Secondary; everything else is Dim.
    for (idx, row) in plain_rows.iter().enumerate() {
        let cols: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let padded = if i == ncols - 1 {
                    cell.clone()
                } else {
                    style::pad_right(cell, widths[i])
                };
                let color = match i {
                    1 => style::Color::Primary,
                    2 if show_source => style::Color::Secondary,
                    _ => style::Color::Dim,
                };
                style::color(&padded, color)
            })
            .collect();
        println!("{}", cols.join("  "));

        if show_skills {
            let r = &rows[idx];
            let skills: Vec<&str> = skill_rows
                .iter()
                .filter(|s| s.project == r.project && s.source == r.source)
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

fn render_table(
    rows: &[ProjectRow],
    skill_rows: &[SkillRow],
    show_skills: bool,
    show_source: bool,
) {
    let mut headers: Vec<&str> = vec!["last_activity", "project"];
    if show_source {
        headers.push("source");
    }
    headers.extend(["sessions", "messages", "tools", "skills"]);
    if show_skills {
        headers.push("skill_names");
    }

    let string_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
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

            let mut row = vec![time_ago, project];
            if show_source {
                row.push(if r.source.is_empty() {
                    style::null_display().to_string()
                } else {
                    r.source.clone()
                });
            }
            row.extend([
                r.sessions.to_string(),
                format_count(r.messages),
                format_count(r.tools),
                r.skills.to_string(),
            ]);

            if show_skills {
                let skills: Vec<&str> = skill_rows
                    .iter()
                    .filter(|s| s.project == r.project && s.source == r.source)
                    .map(|s| s.skill.as_str())
                    .collect();
                row.push(skills.join(", "));
            }

            row
        })
        .collect();

    style::print_light_table(&headers, &string_rows);
}
