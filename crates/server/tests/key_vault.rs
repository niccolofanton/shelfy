//! Synthetic acceptance checks for P3-02: rotation/restart, isolation and snapshots.

mod support;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use secrecy::{ExposeSecret as _, SecretString};
use shelfy_core::db::UserDb;
use shelfy_server::admin::rekey::{RekeyReport, rekey};
use shelfy_server::admin::snapshot::{SnapshotOptions, snapshot};
use shelfy_server::ai::vault::KeyVault;
use shelfy_server::control::provider_keys;
use shelfy_server::state::AppState;
use support::{TestState, auth::owner};

fn vault(current: u8, previous: Option<u8>) -> KeyVault {
    let encode = |b| SecretString::from(STANDARD.encode([b; 32]));
    KeyVault::new(Some(encode(current)), previous.map(encode)).unwrap()
}

#[test]
fn rotation_dry_run_restart_and_snapshot() {
    let t = TestState::with_config(|c| c.vault = vault(1, None));
    let user = owner(&t);
    let key = SecretString::from("synthetic-provider-key-4242");
    for provider in ["alpha", "beta", "gamma"] {
        let sealed = t.state.vault().seal(&user, provider, &key).unwrap();
        t.state
            .control()
            .write(|tx| provider_keys::put(tx, &user, provider, &sealed, 42))
            .unwrap();
    }
    t.state
        .control()
        .write(|tx| provider_keys::touch(tx, &user, "alpha", 100))
        .unwrap();
    let mut config = t.state.config().clone();
    config.vault = vault(2, Some(1));
    let rotating = AppState::open(config.clone()).unwrap();
    let expected = RekeyReport {
        scanned: 3,
        rekeyed: 3,
        ..RekeyReport::default()
    };
    assert_eq!(
        rekey(rotating.control(), rotating.vault(), true).unwrap(),
        expected
    );
    assert_eq!(
        rotating
            .control()
            .read(|c| provider_keys::get(c, &user, "alpha"))
            .unwrap()
            .unwrap()
            .sealed
            .key_version,
        t.state.vault().key_version().unwrap()
    );
    assert_eq!(
        rekey(rotating.control(), rotating.vault(), false).unwrap(),
        expected
    );
    assert_eq!(
        rekey(rotating.control(), rotating.vault(), false).unwrap(),
        RekeyReport {
            scanned: 3,
            unchanged: 3,
            ..RekeyReport::default()
        }
    );
    rotating.close();
    t.state.clone().close();
    config.vault = vault(2, None);
    let restarted = AppState::open(config).unwrap();
    for provider in ["alpha", "beta", "gamma"] {
        let row = restarted
            .control()
            .read(|c| provider_keys::get(c, &user, provider))
            .unwrap()
            .unwrap();
        assert_eq!(row.created_at, 42);
        assert_eq!(row.last_used_at, (provider == "alpha").then_some(100));
        assert_eq!(
            restarted
                .vault()
                .open(&user, provider, &row.sealed)
                .unwrap()
                .expose_secret(),
            key.expose_secret()
        );
    }
    // Build a library so the real snapshot command can finish every DB.
    let _library = UserDb::open(t.data_dir().library_db(&user), &Default::default()).unwrap();
    let out = t.dir.path().join("snapshots");
    assert!(
        snapshot(&t.data_dir(), &out, &SnapshotOptions::default())
            .unwrap()
            .failed
            .is_empty()
    );
    let copy = rusqlite::Connection::open(out.join("control.sqlite")).unwrap();
    let sealed = provider_keys::get(&copy, &user, "alpha")
        .unwrap()
        .unwrap()
        .sealed;
    assert_eq!(
        restarted
            .vault()
            .open(&user, "alpha", &sealed)
            .unwrap()
            .expose_secret(),
        key.expose_secret()
    );
    for path in [out.join("control.sqlite"), out.join("snapshot-state.json")] {
        let bytes = std::fs::read(path).unwrap();
        for secret in [
            key.expose_secret().as_bytes(),
            STANDARD.encode([2; 32]).as_bytes(),
            &[2u8; 32],
        ] {
            assert!(!bytes.windows(secret.len()).any(|w| w == secret));
        }
    }
}

#[test]
fn partial_rotation_keeps_failures_and_can_resume() {
    let t = TestState::with_config(|c| c.vault = vault(1, None));
    let user = owner(&t);
    let key = SecretString::from("synthetic-provider-credential");
    for (provider, master) in [
        ("a", vault(1, None)),
        ("b", vault(3, None)),
        ("c", vault(1, None)),
    ] {
        let sealed = master.seal(&user, provider, &key).unwrap();
        t.state
            .control()
            .write(|tx| provider_keys::put(tx, &user, provider, &sealed, 42))
            .unwrap();
    }
    let rotating = vault(2, Some(1));
    assert_eq!(
        rekey(t.state.control(), &rotating, false).unwrap(),
        RekeyReport {
            scanned: 3,
            rekeyed: 2,
            failed: 1,
            ..RekeyReport::default()
        }
    );
    assert_eq!(
        rekey(t.state.control(), &vault(2, Some(3)), false).unwrap(),
        RekeyReport {
            scanned: 3,
            rekeyed: 1,
            unchanged: 2,
            ..RekeyReport::default()
        }
    );
    assert_eq!(
        rekey(t.state.control(), &vault(2, None), false)
            .unwrap()
            .unchanged,
        3
    );
}

#[test]
fn rotation_scans_more_than_one_batch_and_rejects_corrupt_current_rows() {
    let t = TestState::with_config(|c| c.vault = vault(1, None));
    let user = owner(&t);
    for number in 0..105 {
        let provider = format!("provider-{number:03}");
        let sealed = t
            .state
            .vault()
            .seal(
                &user,
                &provider,
                &SecretString::from("synthetic-provider-key"),
            )
            .unwrap();
        t.state
            .control()
            .write(|tx| provider_keys::put(tx, &user, &provider, &sealed, 42))
            .unwrap();
    }
    let rotating = vault(2, Some(1));
    assert_eq!(
        rekey(t.state.control(), &rotating, false).unwrap().rekeyed,
        105
    );
    t.state
        .control()
        .write(|tx| {
            tx.execute(
                "UPDATE provider_keys SET ciphertext = X'00' WHERE provider_id = 'provider-100'",
                [],
            )?;
            Ok::<_, shelfy_core::repo::RepoError>(())
        })
        .unwrap();
    let report = rekey(t.state.control(), &vault(2, None), false).unwrap();
    assert_eq!(report.unchanged, 104);
    assert_eq!(report.failed, 1);
}

#[test]
fn rekey_cli_prints_counts_and_rejects_incomplete_rotation() {
    use std::process::Command;
    let t = TestState::with_config(|c| c.vault = vault(1, None));
    let user = owner(&t);
    let secret = SecretString::from("synthetic-cli-provider-key");
    let sealed = t.state.vault().seal(&user, "test", &secret).unwrap();
    t.state
        .control()
        .write(|tx| provider_keys::put(tx, &user, "test", &sealed, 42))
        .unwrap();
    let run = |dry: bool, previous: Option<u8>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shelfy-server"));
        command
            .env_clear()
            .env("SHELFY_DATA_DIR", t.data_dir().root())
            .env("SHELFY_MASTER_KEY", STANDARD.encode([2; 32]))
            .args(["admin", "rekey"]);
        if let Some(key) = previous {
            command.env("SHELFY_MASTER_KEY_PREVIOUS", STANDARD.encode([key; 32]));
        }
        if dry {
            command.arg("--dry-run");
        }
        command.output().unwrap()
    };
    let failed = run(false, None);
    assert!(!failed.status.success());
    assert_eq!(
        String::from_utf8(failed.stdout).unwrap(),
        "scanned=1 rekeyed=0 unchanged=0 skipped=0 failed=1\n"
    );
    for dry in [true, false] {
        let output = run(dry, Some(1));
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "scanned=1 rekeyed=1 unchanged=0 skipped=0 failed=0\n"
        );
    }
    let output = run(false, None);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "scanned=1 rekeyed=0 unchanged=1 skipped=0 failed=0\n"
    );
}

#[test]
fn process_validation_and_help_never_echo_master_keys() {
    use std::process::Command;
    for (variable, value) in [
        ("SHELFY_MASTER_KEY", "planted-invalid-master-key".to_owned()),
        ("SHELFY_MASTER_KEY_PREVIOUS", STANDARD.encode([1; 31])),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_shelfy-server"))
            .env_clear()
            .env(variable, &value)
            .arg("serve")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let rendered = format!(
            "{}{}",
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap()
        );
        assert!(rendered.contains(variable));
        assert!(!rendered.contains(&value));
    }
    let master = STANDARD.encode([5; 32]);
    let output = Command::new(env!("CARGO_BIN_EXE_shelfy-server"))
        .env_clear()
        .env("SHELFY_MASTER_KEY", &master)
        .args(["serve", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8(output.stdout).unwrap().contains(&master));
}
