use anyhow::Result;
use clap::{Parser, Subcommand};
use cq::claude_provider::ClaudeProvider;
use cq::commands::{messages, schema, sessions, sql, tools};
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

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Maximum number of results
    #[arg(long, global = true, default_value_t = 50)]
    limit: usize,

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

    let format = if cli.json {
        OutputFormat::Json
    } else {
        OutputFormat::Table
    };

    let scope = QueryScope::new(cli.project, cli.session, cli.since);

    // Schema command doesn't need a DB connection
    if let Command::Schema { name, examples } = &cli.command {
        schema::run(name.as_deref(), *examples);
        return Ok(());
    }

    let provider = ClaudeProvider::new()?;
    let conn = db::setup_connection(&provider, &scope)?;

    match cli.command {
        Command::Sessions { grep } => {
            sessions::run(&conn, &scope, grep.as_deref(), &format, cli.limit)?;
        }
        Command::Tools { name, grep, errors } => {
            tools::run(&conn, &scope, name.as_deref(), grep.as_deref(), errors, &format, cli.limit)?;
        }
        Command::Messages { msg_type, grep } => {
            messages::run(&conn, &scope, msg_type.as_deref(), grep.as_deref(), &format, cli.limit)?;
        }
        Command::Sql { query } => {
            sql::run(&conn, &query, &format)?;
        }
        Command::Schema { .. } => unreachable!(),
    }

    Ok(())
}
