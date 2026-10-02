//! The command line: `shelfy-server serve` runs the API, `shelfy-server admin
//! …` runs an operator command against the same data directory.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::admin::AdminArgs;
use crate::config::ServeArgs;

const AFTER_HELP: &str = "Every option can also be set with the environment variable shown next \
                          to it. RUST_LOG sets the log filter (default: info).";

/// Shelfy Web API server and operator commands.
#[derive(Debug, Parser)]
#[command(name = "shelfy-server", version, about, after_help = AFTER_HELP)]
pub struct Cli {
    /// What to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the HTTP API until SIGTERM or Ctrl-C.
    Serve(ServeArgs),
    /// Operator commands. Their output goes to stdout, never to the logs.
    Admin(AdminArgs),
}

/// Parses the command line and runs the command; the process exit code.
#[must_use]
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Serve(args) => crate::serve::run(args),
        Command::Admin(args) => crate::admin::run(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use clap::CommandFactory as _;

    use super::*;
    use crate::admin::AdminCommand;
    use crate::config::{Config, ConfigError, LogFormat};

    fn serve(args: &[&str]) -> Result<Config, String> {
        let argv = ["shelfy-server", "serve", "--data-dir", "/srv/shelfy"]
            .iter()
            .chain(args);
        let cli = Cli::try_parse_from(argv).map_err(|e| e.to_string())?;
        let Command::Serve(args) = cli.command else {
            unreachable!("parsed serve")
        };
        Config::from_args(args).map_err(|e| e.to_string())
    }

    #[test]
    fn the_cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn serve_flags_are_parsed_and_validated() {
        let config = serve(&[
            "--listen",
            "127.0.0.1:18087",
            "--metrics-listen",
            "127.0.0.1:19087",
            "--public-url",
            "http://localhost:18087/",
            "--log-format",
            "text",
        ])
        .unwrap();
        assert_eq!(
            config.listen,
            "127.0.0.1:18087".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            config.metrics_listen,
            "127.0.0.1:19087".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(config.public_url.as_str(), "http://localhost:18087");
        assert_eq!(config.log_format, LogFormat::Text);
        assert_eq!(config.data_dir.root(), std::path::Path::new("/srv/shelfy"));

        let err = serve(&["--public-url", "https://example.com/app"]).unwrap_err();
        assert!(err.contains("origin only"), "{err}");
        let err = serve(&["--listen", "not-an-address"]).unwrap_err();
        assert!(err.contains("--listen"), "{err}");
        let err = serve(&[
            "--listen",
            "0.0.0.0:8080",
            "--metrics-listen",
            "127.0.0.1:8080",
        ])
        .unwrap_err();
        let expected = ConfigError::SameListener(
            "0.0.0.0:8080".parse().unwrap(),
            "127.0.0.1:8080".parse().unwrap(),
        );
        assert_eq!(err, expected.to_string());
    }

    #[test]
    fn sign_in_flags_are_parsed_and_validated() {
        let config = serve(&[]).unwrap();
        assert!(config.trusted_proxies.is_empty(), "no proxy is trusted");
        let config = serve(&["--trusted-proxies", "172.16.0.0/12, ::1"]).unwrap();
        assert_eq!(config.trusted_proxies.to_string(), "172.16.0.0/12,::1/128");
        let err = serve(&["--trusted-proxies", "172.16.0.0/40"]).unwrap_err();
        assert!(err.contains("--trusted-proxies"), "{err}");

        // The dev mailbox writes links to disk: loopback public URLs only.
        assert!(serve(&["--dev-mailbox"]).is_ok());
        let err =
            serve(&["--dev-mailbox", "--public-url", "https://refs.example.test"]).unwrap_err();
        assert!(err.contains("SHELFY_DEV_MAILBOX"), "{err}");
    }

    #[test]
    fn serve_takes_a_web_app_with_an_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let err = serve(&["--web-dir", path]).unwrap_err();
        assert!(
            err.contains("SHELFY_WEB_DIR") && err.contains("index.html"),
            "{err}"
        );
        std::fs::write(dir.path().join("index.html"), "<!doctype html>").unwrap();
        let config = serve(&["--web-dir", path]).unwrap();
        assert_eq!(config.web.unwrap().root(), dir.path());
        assert!(serve(&[]).unwrap().web.is_none(), "API only by default");
    }

    #[test]
    fn admin_commands_take_the_data_dir_anywhere() {
        let cli = Cli::try_parse_from([
            "shelfy-server",
            "admin",
            "snapshot",
            "--data-dir",
            "/srv/shelfy",
            "--user",
            "A1",
            "--user",
            "B2",
        ])
        .unwrap();
        let Command::Admin(admin) = cli.command else {
            unreachable!("parsed admin")
        };
        assert_eq!(admin.data.data_dir, std::path::Path::new("/srv/shelfy"));
        let AdminCommand::Snapshot(snapshot) = admin.command else {
            unreachable!("parsed snapshot")
        };
        assert_eq!(snapshot.users, ["A1", "B2"]);
        assert!(
            Cli::try_parse_from(["shelfy-server", "admin", "invite", "--ttl-days", "31"]).is_err()
        );
    }
}
