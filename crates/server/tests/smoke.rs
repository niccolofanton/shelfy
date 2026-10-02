//! Smoke test: the `shelfy-server` binary builds, links the library and runs.

use std::process::Command;

#[test]
fn prints_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_shelfy-server"))
        .arg("--version")
        .output()
        .expect("failed to run shelfy-server");
    assert!(output.status.success(), "exit status: {}", output.status);
    let stdout = String::from_utf8(output.stdout).expect("stdout is not UTF-8");
    assert_eq!(
        stdout.trim_end(),
        format!("shelfy-server {}", shelfy_server::VERSION)
    );
}
