use anyhow::Result;
use clap::{Parser, Subcommand};
use cq::claude_provider::ClaudeProvider;
use cq::commands::{messages, projects, schema, sessions, sql, tools};
use cq::db;
use cq::output::OutputFormat;
use cq::scope::QueryScope;

#[derive(Parser)]
#[command(name = "cq", about = "Query AI agent session transcripts with SQL")]
struct Cli {
    /// Scope to a project (substring match)
    #[arg(short = 'p', long, global = true)]
    project: Option<String>,

    /// Scope to a session (prefix match)
    #[arg(short = 's', long, global = true)]
    session: Option<String>,

    /// Time filter (e.g. 7d, 24h, 30m)
    #[arg(long, global = true)]
    since: Option<String>,

    /// Force full reindex of session files
    #[arg(long, global = true)]
    reindex: bool,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Output as aligned table with header
    #[arg(long, global = true)]
    table: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    no_color: bool,

    /// Show all projects (disable auto-scoping to current directory)
    #[arg(long, global = true)]
    all: bool,

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
    },
    /// Query tool calls
    Tools {
        /// Filter to a specific tool name
        name: Option<String>,

        /// Filter tool inputs by content
        #[arg(long)]
        grep: Option<String>,

        /// Show only tool calls that returned errors
        #[arg(long)]
        errors: bool,

        /// Extract specific input fields as columns (comma-separated)
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
    },
    /// Query messages
    Messages {
        /// Filter by message type (user or assistant)
        #[arg(long = "type", name = "type")]
        msg_type: Option<String>,

        /// Filter messages by content
        #[arg(long)]
        grep: Option<String>,
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
        /// Show documentation for a specific view
        name: Option<String>,

        /// Show example queries
        #[arg(long)]
        examples: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

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

    let scope = QueryScope::new(project, cli.session, cli.since);

    let options = db::DbOptions {
        reindex: cli.reindex,
        ..Default::default()
    };

    let start = std::time::Instant::now();
    let db_setup = db::setup_connection(provider.base_dir(), &options)?;
    let elapsed = start.elapsed();
    if db_setup.file_count > 0 {
        eprintln!("Indexed {} files in {:.1}s", db_setup.file_count, elapsed.as_secs_f64());
    } else {
        eprintln!("Cache up to date ({:.1}s)", elapsed.as_secs_f64());
    }

    let conn = db_setup.conn;

    match cli.command {
        Command::Sessions { grep } => {
            sessions::run(&conn, &scope, grep.as_deref(), &format, cli.limit, cli.offset)?;
        }
        Command::Tools { name, grep, errors, fields } => {
            let field_refs: Option<Vec<&str>> = fields.as_ref().map(|f| f.iter().map(|s| s.as_str()).collect());
            tools::run(&conn, &scope, name.as_deref(), grep.as_deref(), errors, field_refs.as_deref(), &format, cli.limit, cli.offset)?;
        }
        Command::Messages { msg_type, grep } => {
            messages::run(&conn, &scope, msg_type.as_deref(), grep.as_deref(), &format, cli.limit, cli.offset)?;
        }
        Command::Projects { skills } => {
            projects::run(&conn, &scope, skills, &format, cli.limit, cli.offset)?;
        }
        Command::Sql { query } => {
            sql::run(&conn, &query, &format)?;
        }
        Command::Schema { .. } => unreachable!(),
    }

    Ok(())
}
