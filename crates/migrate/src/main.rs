//! `shelfy-migrate`: moves a desktop Shelfy library to Shelfy Web.
//!
//! It reads the desktop library read-only, builds a bundle (a new-schema
//! database plus content-addressed media), uploads only what the server lacks
//! and asks the server to install it. Commands: the device sign-in
//! (`login`), the dry run (`plan`), the column mapping (`mapping`) and the
//! migration itself (`run`).
//!
//! Exit status: 0 done, 1 the dry run's criteria or the reconciliation
//! failed, 2 usage, 3 the library or the server failed, 4 the desktop app
//! holds the library open.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §4.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use clap::{Args, Parser, Subcommand};
use shelfy_core::legacy::{LegacyDb, catalog};
use shelfy_migrate::client::{Client, Header};
use shelfy_migrate::login::{self, LoginEvent, LoginOptions};
use shelfy_migrate::plan::{PlanOptions, plan};
use shelfy_migrate::render;
use shelfy_migrate::report::ServerCheck;
use shelfy_migrate::run::{self, DesktopOpen, RunOptions};

/// Exit status of a plan whose SPIKE-1 criteria fail, or of a run whose
/// reconciliation does not match.
const EXIT_PLAN_FAILED: u8 = 1;
/// Exit status when the library cannot be read, or a run fails.
const EXIT_UNREADABLE: u8 = 3;
/// Exit status when the desktop app holds the library open.
const EXIT_DESKTOP_OPEN: u8 = 4;

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
    /// Sign in: approve the code it shows on the web app's /device page,
    /// and the `migrate` token (valid 7 days) is saved to a private file.
    Login(LoginArgs),
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

/// How the CLI reaches the server.
#[derive(Debug, Args)]
struct ServerArgs {
    /// An extra request header, `Name: value`, sent with every request: the
    /// Cloudflare Access service token while Access guards the host
    /// (`CF-Access-Client-Id: …`, `CF-Access-Client-Secret: …`). `@FILE`
    /// reads one header per line from a file, which keeps secrets out of the
    /// shell history. Repeatable. Values are never printed.
    #[arg(long = "header", value_name = "HEADER")]
    headers: Vec<String>,
}

/// Where the `migrate` token comes from.
#[derive(Debug, Args)]
struct TokenArgs {
    /// A `migrate` token. Prefer the token file of `login`. Never printed.
    #[arg(
        long,
        env = "SHELFY_MIGRATE_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN",
        conflicts_with = "token_file"
    )]
    token: Option<String>,
    /// The file holding the `migrate` token (mode 0600). Default: the file
    /// `login` writes (`~/.config/shelfy-migrate/token`).
    #[arg(long, env = "SHELFY_MIGRATE_TOKEN_FILE", value_name = "PATH")]
    token_file: Option<PathBuf>,
}

impl TokenArgs {
    fn resolve(&self) -> anyhow::Result<String> {
        if let Some(token) = &self.token {
            return Ok(token.trim().to_owned());
        }
        let path = self
            .token_file
            .clone()
            .or_else(login::default_token_file)
            .context("no token: run `shelfy-migrate login <server>` first")?;
        anyhow::ensure!(
            path.exists(),
            "no token at {}: run `shelfy-migrate login <server>` first",
            path.display()
        );
        login::read_token(&path)
    }
}

#[derive(Debug, Args)]
struct LoginArgs {
    /// The Shelfy Web server: its public origin (`https://refs.example.com`).
    #[arg(value_name = "URL")]
    server: String,
    #[command(flatten)]
    connection: ServerArgs,
    /// Where to save the token. Default: `~/.config/shelfy-migrate/token`
    /// (`$XDG_CONFIG_HOME` when set).
    #[arg(long, env = "SHELFY_MIGRATE_TOKEN_FILE", value_name = "PATH")]
    token_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct PlanArgs {
    /// The desktop library (`<userData>/shelfy.sqlite`), opened read-only.
    #[arg(long, value_name = "PATH")]
    db: PathBuf,
    /// The desktop userData directory that holds `assets/` (and the
    /// settings). Files are looked up there, read-only. Defaults to the
    /// library's directory when it has an `assets/` directory.
    #[arg(long, value_name = "DIR")]
    media_root: Option<PathBuf>,
    /// Read the library even while the desktop app holds it open.
    #[arg(long)]
    allow_open: bool,
    /// Also check the web library and the quota on this server (needs
    /// `login`).
    #[arg(long, env = "SHELFY_SERVER_URL", value_name = "URL")]
    server: Option<String>,
    #[command(flatten)]
    token: TokenArgs,
    #[command(flatten)]
    connection: ServerArgs,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Leave keys, legacy ids and paths out of the report.
    #[arg(long)]
    redact: bool,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// The desktop library (`<userData>/shelfy.sqlite`), read-only.
    #[arg(long, value_name = "PATH")]
    db: PathBuf,
    /// The desktop userData directory that holds `assets/`. Defaults to the
    /// library's directory when it has an `assets/` directory.
    #[arg(long, value_name = "DIR")]
    media_root: Option<PathBuf>,
    /// The Shelfy Web server: its public origin.
    #[arg(long, env = "SHELFY_SERVER_URL", value_name = "URL")]
    server: String,
    #[command(flatten)]
    token: TokenArgs,
    #[command(flatten)]
    connection: ServerArgs,
    /// Where the snapshot, the bundle and the resume state go. A re-run with
    /// the same directory continues interrupted uploads.
    #[arg(long, value_name = "DIR")]
    work_dir: Option<PathBuf>,
    /// Also upload the kept videos (excluded by default, plan §4.2).
    #[arg(long)]
    with_videos: bool,
    /// Merge into a web library that is not empty (an empty one is replaced).
    #[arg(long)]
    merge: bool,
    /// Read the library even while the desktop app holds it open, from a
    /// snapshot taken now: changes made after it are not migrated.
    #[arg(long)]
    allow_open: bool,
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
    /// Bytes per upload request (tests, slow links).
    #[arg(long, hide = true, default_value_t = run::CHUNK_BYTES)]
    chunk_bytes: usize,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Login(args) => run_login(args),
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

/// The exit status of a failed command.
fn failure(err: &anyhow::Error) -> ExitCode {
    eprintln!("error: {err:#}");
    if err.downcast_ref::<DesktopOpen>().is_some() {
        ExitCode::from(EXIT_DESKTOP_OPEN)
    } else {
        ExitCode::from(EXIT_UNREADABLE)
    }
}

fn run_login(args: LoginArgs) -> ExitCode {
    let result = (|| -> anyhow::Result<login::Saved> {
        let headers = Header::parse_all(&args.connection.headers)?;
        let token_file = args
            .token_file
            .clone()
            .or_else(login::default_token_file)
            .context("no home directory: pass --token-file")?;
        let options = LoginOptions {
            server: args.server.clone(),
            headers,
            token_file,
            min_interval: Duration::from_secs(1),
            max_restarts: 2,
        };
        login::login(&options, &mut |event| {
            let mut err = std::io::stderr();
            let _ = match event {
                LoginEvent::Code(code) => writeln!(
                    err,
                    "To sign shelfy-migrate in, open\n\n    {}\n\nsigned in to Shelfy Web, and enter the code\n\n    {}\n\n\
                     (or open {}). Approve it only if it is the code above. It expires in {} minutes.\n\
                     Waiting for the approval…",
                    code.verification_uri,
                    code.user_code,
                    code.verification_uri_complete,
                    code.expires_in.div_ceil(60)
                ),
                LoginEvent::SlowDown(seconds) => {
                    writeln!(err, "the server asked to poll every {seconds} s")
                }
                LoginEvent::Waiting(wait) => {
                    writeln!(err, "too many polls: waiting {} s", wait.as_secs())
                }
                LoginEvent::Restart => writeln!(err, "\nthe code expired: here is a new one"),
            };
        })
    })();
    match result {
        Ok(saved) => {
            eprintln!(
                "Signed in. The token is saved in {} (mode 0600), valid until {} UTC.",
                saved.path.display(),
                utc(saved.expires_at)
            );
            ExitCode::SUCCESS
        }
        Err(err) => failure(&err),
    }
}

/// `YYYY-MM-DD HH:MM` of a unix-ms time.
fn utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
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
        Err(e) => failure(&e),
    }
}

fn plan_report(args: &PlanArgs) -> anyhow::Result<(String, bool)> {
    // Before this process opens the library (desktop.rs).
    let open = run::check_desktop(&args.db, args.allow_open)?;
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
    let mut report = plan(&db, &options).context("cannot read the library")?;
    report.desktop_open = Some(open);
    if let Some(server) = &args.server {
        let headers = Header::parse_all(&args.connection.headers)?;
        let token = args.token.resolve()?;
        let client = Client::with_headers(server, Some(&token), &headers)?;
        let preflight = client.preflight().context("cannot ask the server")?;
        let upload = &report.files.upload;
        report.server = Some(ServerCheck::new(
            &preflight,
            upload.bytes_default,
            upload.bytes_videos,
        ));
    }
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
    let prepared = (|| -> anyhow::Result<RunOptions> {
        let token = args.token.resolve()?;
        let headers = Header::parse_all(&args.connection.headers)?;
        let mut options = RunOptions::new(
            args.db.clone(),
            &args.server,
            &token,
            args.work_dir
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("shelfy-migrate")),
        );
        options.media_root = args.media_root.clone();
        options.headers = headers;
        options.with_videos = args.with_videos;
        options.merge = args.merge;
        options.allow_open = args.allow_open;
        options.keep_work = args.keep_work;
        options.list_orphans = args.list_orphans;
        options.chunk_bytes = args.chunk_bytes;
        Ok(options)
    })();
    let options = match prepared {
        Ok(options) => options,
        Err(err) => return failure(&err),
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
            let code = failure(&e);
            if e.downcast_ref::<DesktopOpen>().is_none() {
                eprintln!(
                    "the work directory keeps the upload state: run the same command again to continue"
                );
            }
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_print_in_utc() {
        assert_eq!(utc(0), "1970-01-01 00:00");
        assert_eq!(utc(1_790_899_200_000), "2026-10-02 00:00");
        assert_eq!(utc(951_782_400_000 + 3_723_000), "2000-02-29 01:02");
    }

    #[test]
    fn the_cli_parses() {
        use clap::CommandFactory as _;
        Cli::command().debug_assert();
        let cli = Cli::try_parse_from([
            "shelfy-migrate",
            "run",
            "--db",
            "/x/shelfy.sqlite",
            "--server",
            "https://refs.example.test",
            "--header",
            "CF-Access-Client-Id: a",
            "--header",
            "@/tmp/secret.headers",
            "--merge",
        ])
        .unwrap();
        let Command::Run(run) = cli.command else {
            panic!("run");
        };
        assert_eq!(run.connection.headers.len(), 2);
        assert!(run.merge && !run.with_videos);
        assert_eq!(run.chunk_bytes, run::CHUNK_BYTES);
        assert!(Cli::try_parse_from(["shelfy-migrate", "login"]).is_err());
    }
}
