use std::io::IsTerminal;

use anyhow::Result;
use clap::{Parser, Subcommand};
use cq::claude_provider::ClaudeProvider;
use cq::commands::{messages, projects, schema, sessions, sql, tools};
use cq::db;
use cq::output::OutputFormat;
use cq::scope::QueryScope;

#[derive(Parser)]
#[command(name = "cq", version, about = "Query AI agent session transcripts with SQL")]
struct Cli {
    /// Scope to a project (substring match, e.g. 'myproject')
    #[arg(short = 'p', long, global = true)]
    project: Option<String>,

    /// Scope to a session by UUID (prefix match supported)
    #[arg(short = 's', long, global = true)]
    session: Option<String>,

    /// Time filter (e.g. 7d, 24h, 30m)
    #[arg(long, global = true)]
    since: Option<String>,

    /// Force full reindex of session files (waits for lock if index is busy)
    #[arg(long, global = true, conflicts_with = "no_reindex")]
    reindex: bool,

    /// Skip sync entirely, use cached data (fastest, no lock contention)
    #[arg(long, global = true, conflicts_with = "reindex")]
    no_reindex: bool,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Output as aligned table with header
    #[arg(long, global = true)]
    table: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    /// Show full output without truncation (auto-enabled when piped)
    #[arg(long, global = true)]
    wide: bool,

    /// Show all projects (disable auto-scoping to current directory)
    #[arg(long, global = true)]
    all: bool,

    /// Scope to a named source (e.g. 'main', or a cenv env name). Spans all sources with --all.
    #[arg(long, global = true)]
    source: Option<String>,

    /// Maximum number of results (0 for unlimited)
    #[arg(long, global = true, default_value_t = 50)]
    limit: usize,

    /// Number of results to skip before returning
    #[arg(long, global = true, default_value_t = 0)]
    offset: usize,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List sessions
    Sessions {
        /// Filter sessions by content
        #[arg(long)]
        grep: Option<String>,

        /// Extract specific columns (comma-separated) [valid: session_id, project, started_at, ended_at, message_count, tool_call_count, user_message_count, first_user_message]
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,

        /// Aggregate rows into counts by column [valid: project]
        #[arg(long = "count-by")]
        count_by: Option<String>,

        /// Show chronological tool call timeline (requires --session)
        #[arg(long)]
        timeline: bool,
    },
    /// Query tool calls
    Tools {
        /// Filter to a specific tool name (run 'cq tools' to see available names)
        name: Option<String>,

        /// Filter tool inputs by content
        #[arg(long)]
        grep: Option<String>,

        /// Show only tool calls that returned errors
        #[arg(long)]
        errors: bool,

        /// Extract specific input fields as columns (comma-separated; fields depend on the tool, see 'cq schema tool_calls')
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,

        /// Aggregate rows into counts by column [valid: name, session, project]
        #[arg(long = "count-by")]
        count_by: Option<String>,

        /// Show N messages after each match (grep -A)
        #[arg(short = 'A', long = "after-context", value_name = "N")]
        after: Option<usize>,

        /// Show N messages before each match (grep -B)
        #[arg(short = 'B', long = "before-context", value_name = "N")]
        before: Option<usize>,

        /// Show N messages before and after each match (grep -C, shorthand for -A N -B N)
        #[arg(short = 'C', long = "context", value_name = "N", conflicts_with_all = ["after", "before"])]
        context: Option<usize>,
    },
    /// Query messages
    Messages {
        /// Filter by message type [valid: user, assistant]
        #[arg(long = "type", name = "type")]
        msg_type: Option<String>,

        /// Filter messages by content
        #[arg(long)]
        grep: Option<String>,

        /// Extract specific columns (comma-separated) [valid: session_id, project, type, timestamp, text, model, tool_count]
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,

        /// Aggregate rows into counts by column [valid: type, session, project]
        #[arg(long = "count-by")]
        count_by: Option<String>,

        /// Show N messages after each match (grep -A)
        #[arg(short = 'A', long = "after-context", value_name = "N")]
        after: Option<usize>,

        /// Show N messages before each match (grep -B)
        #[arg(short = 'B', long = "before-context", value_name = "N")]
        before: Option<usize>,

        /// Show N messages before and after each match (grep -C, shorthand for -A N -B N)
        #[arg(short = 'C', long = "context", value_name = "N", conflicts_with_all = ["after", "before"])]
        context: Option<usize>,
    },
    /// Summarize projects by session, message, and tool counts
    Projects {
        /// Show skill names used per project
        #[arg(long)]
        skills: bool,
    },
    /// Run a raw SQL query
    Sql {
        /// SQL query to execute
        query: String,
    },
    /// Show view schema documentation
    Schema {
        /// Show documentation for a specific view [valid: messages, tool_calls, tool_results, sessions]
        name: Option<String>,

        /// Show example queries
        #[arg(long)]
        examples: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Wide mode: explicit flag or auto-detect when stdout is not a terminal (piped)
    let wide = cli.wide || !std::io::stdout().is_terminal();

    // Disable color if --no-color or NO_COLOR env var
    if cli.no_color || std::env::var("NO_COLOR").is_ok() {
        owo_colors::set_override(false);
    }

    // --json wins over --table
    let format = if cli.json {
        OutputFormat::Json
    } else if cli.table {
        OutputFormat::Table
    } else {
        OutputFormat::Default
    };

    // Validate --session format before doing any work
    if let Some(ref session) = cli.session {
        if let Err(e) = cq::scope::validate_session_id(session) {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }

    // Schema command doesn't need a DB connection
    if let Command::Schema { name, examples } = &cli.command {
        schema::run(name.as_deref(), *examples);
        return Ok(());
    }

    let provider = ClaudeProvider::new()?;

    // Auto-scope to current project if no explicit --project and not --all
    let is_projects_cmd = matches!(cli.command, Command::Projects { .. });

    let (project, auto_scoped) = if cli.project.is_some() {
        (cli.project, false)
    } else if cli.all || cli.json || is_projects_cmd {
        (None, false)
    } else {
        match std::env::var("PWD").ok() {
            Some(cwd) => match provider.project_for_cwd(&cwd) {
                Some(project_path) => (Some(project_path), true),
                None => (None, false),
            },
            None => (None, false),
        }
    };

    if auto_scoped && !cli.json {
        if let Some(ref p) = project {
            let display = cq::style::abbreviate_home(p);
            eprintln!("{}", cq::style::hint(&format!("Scoped to {display} (use --all for everything)")));
        }
    }

    // Source scope: explicit --source wins; else auto-scope to the active source
    // (the one matching CLAUDE_CONFIG_DIR), unless --all/--json/projects.
    let (source, source_auto) = if cli.source.is_some() {
        (cli.source.clone(), false)
    } else if cli.all || cli.json || is_projects_cmd {
        (None, false)
    } else {
        let active = std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .and_then(|d| provider.source_for_config_dir(std::path::Path::new(&d)))
            .unwrap_or_else(|| "main".to_string());
        (Some(active), true)
    };
    if source_auto && !cli.json {
        if let Some(ref s) = source {
            let others = provider.sources().len().saturating_sub(1);
            eprintln!(
                "{}",
                cq::style::hint(&format!(
                    "Scoped to source '{s}' ({others} other sources; --all to span, --source <name> to target)"
                ))
            );
        }
    }

    let scope = QueryScope::new(project, cli.session, cli.since).with_source(source);

    let sync_mode = if cli.reindex {
        db::SyncMode::Force
    } else if cli.no_reindex {
        db::SyncMode::Skip
    } else {
        db::SyncMode::Auto
    };

    let options = db::DbOptions {
        sync_mode,
        ..Default::default()
    };

    let sources: Vec<(String, std::path::PathBuf)> = provider
        .sources()
        .iter()
        .map(|s| (s.name.clone(), s.projects_dir.clone()))
        .collect();

    let sync_scope = if cli.reindex {
        cq::sync_scope::SyncScope::All
    } else if let Some(ref p) = scope.project {
        let dirs = provider.project_dirs_for_query(p);
        if dirs.is_empty() {
            cq::sync_scope::SyncScope::All
        } else {
            cq::sync_scope::SyncScope::Projects(dirs)
        }
    } else {
        cq::sync_scope::SyncScope::All
    };

    let start = std::time::Instant::now();
    let providers: Vec<Box<dyn cq::provider::TranscriptProvider>> =
        vec![Box::new(cq::claude_provider::ClaudeProvider::new()?)];
    let db_setup = db::setup_connection(&providers, &sources, &options, sync_scope)?;
    let elapsed = start.elapsed();
    if db_setup.lock_busy {
        eprintln!("index busy, using cached data (re-run with --reindex to force)");
    } else if db_setup.skipped {
        // --no-reindex: silence
    } else if db_setup.file_count > 0 {
        eprintln!("Synced {} new files ({} total, {:.1}s)", db_setup.file_count, db_setup.total_files, elapsed.as_secs_f64());
    } else {
        eprintln!("Loaded {} files ({:.1}s)", db_setup.total_files, elapsed.as_secs_f64());
    }

    let conn = db_setup.conn;

    match cli.command {
        Command::Sessions { grep, fields, count_by, timeline } => {
            let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
            sessions::run(&conn, &scope, grep.as_deref(), field_refs.as_deref(), count_by.as_deref(), &format, cli.limit, cli.offset, wide, timeline)?;
        }
        Command::Tools { name, grep, errors, fields, count_by, after, before, context } => {
            let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
            let ctx = cq::commands::ContextWindow::from_flags(after, before, context);
            tools::run(&conn, &scope, name.as_deref(), grep.as_deref(), errors, field_refs.as_deref(), count_by.as_deref(), ctx, &format, cli.limit, cli.offset, wide)?;
        }
        Command::Messages { msg_type, grep, fields, count_by, after, before, context } => {
            let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
            let ctx = cq::commands::ContextWindow::from_flags(after, before, context);
            messages::run(&conn, &scope, msg_type.as_deref(), grep.as_deref(), field_refs.as_deref(), count_by.as_deref(), ctx, &format, cli.limit, cli.offset, wide)?;
        }
        Command::Projects { skills } => {
            projects::run(&conn, &scope, skills, &format, cli.limit, cli.offset, wide)?;
        }
        Command::Sql { query } => {
            if let Err(e) = sql::run(&conn, &query, &format, wide) {
                // Display ({e}), not Debug ({e:?}): the top-level message is the
                // useful DuckDB error (with the `LINE N: ... ^` pointer). The
                // cause chain only adds "Error code 1: Unknown error code" noise,
                // and nothing in sql::run attaches .context worth surfacing. We
                // catch here only to append the timestamp hint after the error.
                eprintln!("Error: {e}");
                if let Some(hint) = sql::timestamp_error_hint(&e.to_string()) {
                    eprintln!("{}", cq::style::hint(hint));
                }
                std::process::exit(1);
            }
        }
        Command::Schema { .. } => unreachable!(),
    }

    Ok(())
}
