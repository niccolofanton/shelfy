//! `shelfy-migrate`: moves a desktop Shelfy library to Shelfy Web.
//!
//! It reads the desktop library read-only, builds a bundle (a new-schema
//! database plus content-addressed media), uploads only what the server lacks
//! and asks the server to install it. Today it has the dry run (`plan`) and
//! the column mapping (`mapping`); `login` and `run` arrive with the
//! migration tasks.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §4.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use clap::{Args, Parser, Subcommand};
use shelfy_core::legacy::{LegacyDb, catalog};
use shelfy_migrate::plan::{PlanOptions, plan};
use shelfy_migrate::render;

/// Exit status of a plan whose SPIKE-1 criteria fail.
const EXIT_PLAN_FAILED: u8 = 1;
/// Exit status when the library cannot be read.
const EXIT_UNREADABLE: u8 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "shelfy-migrate",
    version,
    about = "Move a desktop Shelfy library to Shelfy Web"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Dry run: report how a desktop library maps to the web schema. Writes nothing.
    Plan(PlanArgs),
    /// Print how every desktop column maps to the web schema.
    Mapping {
        /// Print the mapping as JSON instead of a Markdown table.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct PlanArgs {
    /// The desktop library (`<userData>/shelfy.sqlite`), opened read-only.
    #[arg(long, value_name = "PATH")]
    db: PathBuf,
    /// The desktop userData directory that holds `assets/`. Files are looked
    /// up there, read-only. Defaults to the library's directory when it has an
    /// `assets/` directory.
    #[arg(long, value_name = "DIR")]
    media_root: Option<PathBuf>,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Leave keys, legacy ids and paths out of the report.
    #[arg(long)]
    redact: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Plan(args) => run_plan(args),
        Command::Mapping { json } => {
            if json {
                match serde_json::to_string_pretty(catalog::TABLES) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print!("{}", render::mapping_markdown());
            }
            ExitCode::SUCCESS
        }
    }
}

fn run_plan(args: PlanArgs) -> ExitCode {
    match plan_report(&args) {
        Ok((text, pass)) => {
            print!("{text}");
            if pass {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_PLAN_FAILED)
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(EXIT_UNREADABLE)
        }
    }
}

fn plan_report(args: &PlanArgs) -> anyhow::Result<(String, bool)> {
    let db = LegacyDb::open(&args.db)
        .with_context(|| format!("cannot read the library at {}", args.db.display()))?;
    let media_root = args.media_root.clone().or_else(|| {
        let dir = args.db.parent()?;
        dir.join("assets").is_dir().then(|| dir.to_path_buf())
    });
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let options = PlanOptions {
        media_root,
        redact: args.redact,
        now_ms,
    };
    let report = plan(&db, &options).context("cannot read the library")?;
    let text = if args.json {
        let mut json = serde_json::to_string_pretty(&report)?;
        json.push('\n');
        json
    } else {
        render::plan_text(&report)
    };
    Ok((text, report.verdict.pass))
}
