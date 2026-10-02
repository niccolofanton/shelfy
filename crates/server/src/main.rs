//! `shelfy-server` entry point. The server and its commands arrive in later
//! tasks (plan §10); for now it only reports its version.

use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args_os().nth(1) {
        Some(arg) if arg == "--version" || arg == "-V" => {
            println!("shelfy-server {}", shelfy_server::VERSION);
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: shelfy-server --version");
            ExitCode::from(2)
        }
    }
}
