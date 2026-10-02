//! `shelfy-server` entry point: everything lives in the library (`cli::main`).

use std::process::ExitCode;

fn main() -> ExitCode {
    shelfy_server::cli::main()
}
