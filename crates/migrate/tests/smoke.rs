//! Smoke test: the `shelfy-migrate` binary builds and runs.

use std::process::Command;

#[test]
fn prints_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_shelfy-migrate"))
        .arg("--version")
        .output()
        .expect("failed to run shelfy-migrate");
    assert!(output.status.success(), "exit status: {}", output.status);
    let stdout = String::from_utf8(output.stdout).expect("stdout is not UTF-8");
    assert_eq!(
        stdout.trim_end(),
        concat!("shelfy-migrate ", env!("CARGO_PKG_VERSION"))
    );
}
