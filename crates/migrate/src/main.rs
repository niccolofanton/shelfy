//! `shelfy-migrate`: moves a desktop Shelfy library to Shelfy Web.
//!
//! It reads the desktop library read-only, builds a bundle (a new-schema
//! database plus content-addressed media), uploads only what the server lacks
//! and asks the server to install it. The commands (§4.1) arrive with the
//! migration tasks; for now it only reports its version.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §4.

use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args_os().nth(1) {
        Some(arg) if arg == "--version" || arg == "-V" => {
            println!("shelfy-migrate {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: shelfy-migrate --version");
            ExitCode::from(2)
        }
    }
}
