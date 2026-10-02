//! `shelfy-migrate`: moves a desktop Shelfy library to Shelfy Web.
//!
//! It reads the desktop library read-only, builds a bundle (a new-schema
//! database plus content-addressed media), uploads only what the server lacks
//! and asks the server to install it. Commands: the dry run (`plan`), the
//! column mapping (`mapping`) and the migration itself (`run`); `login`, the
//! device-code sign-in, arrives with P1-17 and P1-19.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §4.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use clap::{Args, Parser, Subcommand};
use shelfy_core::legacy::{LegacyDb, catalog};
use shelfy_migrate::plan::{PlanOptions, plan};
use shelfy_migrate::render;
use shelfy_migrate::run::{self, RunOptions};

/// Exit status of a plan whose SPIKE-1 criteria fail, or of a run whose
/// reconciliation does not match.
const EXIT_PLAN_FAILED: u8 = 1;
/// Exit status when the library cannot be read, or a run fails.
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
    /// Migrate: bundle the library, upload what the server lacks, install it.
    /// Re-running continues an interrupted upload.
    Run(RunArgs),
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

#[derive(Debug, Args)]
struct RunArgs {
    /// The desktop library (`<userData>/shelfy.sqlite`), read-only. With a
    /// `-wal` file next to it (Shelfy open), a snapshot is read instead.
    #[arg(long, value_name = "PATH")]
    db: PathBuf,
    /// The desktop userData directory that holds `assets/`. Defaults to the
    /// library's directory when it has an `assets/` directory.
    #[arg(long, value_name = "DIR")]
    media_root: Option<PathBuf>,
    /// The Shelfy Web server: its public origin.
    #[arg(long, env = "SHELFY_SERVER_URL", value_name = "URL")]
    server: String,
    /// A `migrate` token (`shelfy-server admin migrate-token`). Never printed.
    #[arg(
        long,
        env = "SHELFY_MIGRATE_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN",
        required_unless_present = "token_file",
        conflicts_with = "token_file"
    )]
    token: Option<String>,
    /// A file holding the `migrate` token, instead of `--token` (keep it
    /// private: mode 0600).
    #[arg(long, value_name = "PATH")]
    token_file: Option<PathBuf>,
    /// Where the snapshot, the bundle and the resume state go. A re-run with
    /// the same directory continues interrupted uploads.
    #[arg(long, value_name = "DIR")]
    work_dir: Option<PathBuf>,
    /// Also upload the kept videos (excluded by default, plan §4.2).
    #[arg(long)]
    with_videos: bool,
    /// Merge into a web library that is not empty (not supported by the
    /// server yet: it refuses with a conflict).
    #[arg(long)]
    merge: bool,
    /// Keep the work files after a successful install.
    #[arg(long)]
    keep_work: bool,
    /// Also list the files under `assets/` that no row references (OI-11),
    /// so they can be deleted on the desktop.
    #[arg(long)]
    list_orphans: bool,
    /// Print the outcome as JSON.
    #[arg(long)]
    json: bool,
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
        Command::Run(args) => run_migration(args),
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

fn run_migration(args: RunArgs) -> ExitCode {
    let token = match (args.token, &args.token_file) {
        (Some(token), _) => token,
        (None, Some(path)) => match std::fs::read_to_string(path) {
            Ok(text) => text.trim().to_owned(),
            Err(e) => {
                eprintln!("error: cannot read the token file {}: {e}", path.display());
                return ExitCode::from(EXIT_UNREADABLE);
            }
        },
        (None, None) => unreachable!("clap requires --token or --token-file"),
    };
    let options = RunOptions {
        token,
        work_dir: args
            .work_dir
            .unwrap_or_else(|| std::env::temp_dir().join("shelfy-migrate")),
        db: args.db,
        media_root: args.media_root,
        server: args.server,
        with_videos: args.with_videos,
        merge: args.merge,
        keep_work: args.keep_work,
        list_orphans: args.list_orphans,
        poll_interval: Duration::from_secs(1),
    };
    let mut log = std::io::stderr();
    match run::run(&options, &mut log) {
        Ok(outcome) => {
            if args.json {
                match serde_json::to_string_pretty(&outcome) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print!("{}", run::render(&outcome));
            }
            if outcome.matches {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_PLAN_FAILED)
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            eprintln!(
                "the work directory keeps the upload state: run the same command again to continue"
            );
            ExitCode::from(EXIT_UNREADABLE)
        }
    }
}
