//! `admin flags`: the server-wide flags in `feature_flags` (P2-03; plan
//! §3.9 "platform parser broken: flip the kill switch"; E4 has no `/admin`
//! page, so this is the way to flip one, P2-G7).
//!
//! | Command | What it does |
//! |---|---|
//! | `admin flags list` | every flag: key, effective value, `default` or `set`, its values and what it does; a stored key no flag has is listed as `unknown` |
//! | `admin flags get KEY` | prints the effective value, as JSON |
//! | `admin flags set KEY VALUE` | checks VALUE against the flag (JSON; a version may be given bare, `0.3.0`), stores it, audits `flag.set` |
//! | `admin flags unset KEY` | removes the stored value, so the default applies again; audits `flag.unset` |
//!
//! Only the keys of the typed registry ([`crate::extension::flags::FLAGS`])
//! can be set, each within its type and bounds. A running server reads the
//! flags again at most 30 seconds after a change
//! ([`crate::extension::FLAGS_TTL`]); the extension applies them on its next
//! config refresh. Flags hold no secrets: values are printed and audited.
//!
//! ```sh
//! shelfy-server admin flags set extension.instagram.replay false
//! shelfy-server admin flags unset extension.instagram.replay
//! ```

use std::io::Write;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use shelfy_core::repo::RepoError;

use super::open_existing_control;
use crate::config::DataDir;
use crate::control::audit::{self, Entry};
use crate::control::flags as rows;
use crate::extension::FLAGS_TTL;
use crate::extension::flags::{FLAGS, Flag, FlagType, FlagValue, Flags};
use crate::ids::now_ms;

/// Arguments of `admin flags`.
#[derive(Debug, Args)]
pub struct FlagsArgs {
    /// What to do.
    #[command(subcommand)]
    pub command: FlagsCommand,
}

/// The `admin flags` commands.
#[derive(Debug, Subcommand)]
pub enum FlagsCommand {
    /// List every flag with its effective value, and whether it is set or
    /// the default.
    List,
    /// Print a flag's effective value as JSON.
    Get {
        /// The flag, such as `extension.instagram.replay`.
        key: String,
    },
    /// Set a flag. The value is JSON (`false`, `700`, `"0.3.0"`); a version
    /// may be given without quotes.
    Set {
        /// The flag, such as `extension.instagram.replay`.
        key: String,
        /// The new value.
        value: String,
    },
    /// Remove a flag's value, so its default applies again.
    Unset {
        /// The flag.
        key: String,
    },
}

/// One line of `admin flags list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The key.
    pub key: String,
    /// The effective value, as JSON; for an unknown key, the stored text.
    pub value: String,
    /// `default`, `set`, or `unknown` (a stored key no flag has).
    pub source: &'static str,
    /// The values it takes: `bool`, `version` or `MIN..=MAX`.
    pub kind: String,
    /// What it does.
    pub help: &'static str,
}

/// Runs `admin flags`.
///
/// # Errors
///
/// No control database, an unknown flag, a value that does not fit it, or
/// a database failure.
pub fn run(data: &DataDir, args: &FlagsArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    match &args.command {
        FlagsCommand::List => {
            let listed = list(data)?;
            let width = listed.iter().map(|line| line.key.len()).max().unwrap_or(0);
            for line in listed {
                writeln!(
                    out,
                    "{:<width$}  {:<9}  {:<7}  {:<16}  {}",
                    line.key, line.value, line.source, line.kind, line.help
                )?;
            }
        }
        FlagsCommand::Get { key } => writeln!(out, "{}", get(data, key)?)?,
        FlagsCommand::Set { key, value } => {
            let value = set(data, key, value)?;
            writeln!(out, "{key} = {}", value.to_json())?;
            eprintln!(
                "a running server applies it within {} seconds",
                FLAGS_TTL.as_secs()
            );
        }
        FlagsCommand::Unset { key } => {
            if unset(data, key)? {
                eprintln!(
                    "{key} is back to its default; a running server applies it within {} seconds",
                    FLAGS_TTL.as_secs()
                );
            } else {
                eprintln!("{key} was not set: its default applies");
            }
        }
    }
    Ok(())
}

fn known(key: &str) -> anyhow::Result<&'static Flag> {
    Flag::find(key).ok_or_else(|| {
        anyhow::anyhow!("unknown flag {key:?}: `admin flags list` shows the known ones")
    })
}

/// Every flag, with its effective value; then the stored keys no flag has.
///
/// # Errors
///
/// No control database, or the query failed.
pub fn list(data: &DataDir) -> anyhow::Result<Vec<Listed>> {
    let control = open_existing_control(data)?;
    let stored = control.read(rows::list)?;
    let flags = Flags::from_rows(&stored);
    let mut listed: Vec<Listed> = FLAGS
        .iter()
        .map(|flag| Listed {
            key: flag.key.to_owned(),
            value: flags
                .get(flag.key)
                .map_or_else(String::new, |value| value.to_json().to_string()),
            source: if flags.is_overridden(flag.key) {
                "set"
            } else {
                "default"
            },
            kind: flag.type_name(),
            help: flag.help,
        })
        .collect();
    listed.extend(
        stored
            .into_iter()
            .filter(|row| Flag::find(&row.key).is_none())
            .map(|row| Listed {
                key: row.key,
                value: row.value_json,
                source: "unknown",
                kind: String::new(),
                help: "not a flag of this build: `admin flags unset` removes it",
            }),
    );
    Ok(listed)
}

/// The effective value of the flag `key`, as JSON.
///
/// # Errors
///
/// An unknown flag; no control database; the query failed.
pub fn get(data: &DataDir, key: &str) -> anyhow::Result<Value> {
    let flag = known(key)?;
    let control = open_existing_control(data)?;
    let stored = control.read(rows::list)?;
    let value = Flags::from_rows(&stored)
        .get(flag.key)
        .unwrap_or_else(|| flag.default_value());
    Ok(value.to_json())
}

/// Reads `text` as a value of `flag`: JSON, or for a version flag the bare
/// version.
///
/// # Errors
///
/// Why the value does not fit the flag.
pub fn parse_value(flag: &Flag, text: &str) -> anyhow::Result<FlagValue> {
    let text = text.trim();
    let value = match serde_json::from_str::<Value>(text) {
        Ok(value) => value,
        Err(_) if matches!(flag.kind, FlagType::Version { .. }) => Value::String(text.to_owned()),
        Err(_) => anyhow::bail!("{}: {text:?} is not JSON", flag.key),
    };
    flag.parse(&value)
        .map_err(|reason| anyhow::anyhow!("{}: {reason}", flag.key))
}

/// Sets the flag `key` to `text` (see [`parse_value`]) and writes
/// `flag.set` to the audit log; returns the value stored.
///
/// # Errors
///
/// An unknown flag, a value that does not fit, no control database, or the
/// write failed.
pub fn set(data: &DataDir, key: &str, text: &str) -> anyhow::Result<FlagValue> {
    let flag = known(key)?;
    let value = parse_value(flag, text)?;
    let json = value.to_json();
    let control = open_existing_control(data)?;
    let now = now_ms();
    control
        .write(|tx| {
            rows::set(tx, flag.key, &json.to_string(), now)?;
            let meta = json!({ "key": flag.key, "value": json });
            let entry = Entry {
                action: audit::FLAG_SET,
                actor_user_id: None,
                target: None,
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok::<_, RepoError>(())
        })
        .context("cannot store the flag")?;
    Ok(value)
}

/// Removes the stored value of `key` and writes `flag.unset` to the audit
/// log; returns whether a value was stored. A stored key that no flag has
/// (left by another build) can be removed too.
///
/// # Errors
///
/// A key that is neither a flag nor stored; no control database; the write
/// failed.
pub fn unset(data: &DataDir, key: &str) -> anyhow::Result<bool> {
    let control = open_existing_control(data)?;
    let now = now_ms();
    let removed = control
        .write(|tx| {
            let stored = rows::get(tx, key)?.is_some();
            if !stored && Flag::find(key).is_none() {
                return Ok(None);
            }
            let removed = rows::unset(tx, key)?;
            if removed {
                let meta = json!({ "key": key });
                let entry = Entry {
                    action: audit::FLAG_UNSET,
                    actor_user_id: None,
                    target: None,
                    meta: Some(&meta),
                };
                audit::record(tx, &entry, now)?;
            }
            Ok::<_, RepoError>(Some(removed))
        })
        .context("cannot remove the flag")?;
    removed.ok_or_else(|| {
        anyhow::anyhow!("unknown flag {key:?}: `admin flags list` shows the known ones")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::flags::keys;

    #[test]
    fn values_are_read_as_json_or_a_bare_version() {
        let min = Flag::find(keys::MIN_VERSION).unwrap();
        for text in ["0.3.0", "\"0.3.0\"", " 0.3.0 "] {
            assert_eq!(
                parse_value(min, text).unwrap().to_json(),
                json!("0.3.0"),
                "{text}"
            );
        }
        assert_eq!(
            parse_value(min, "0.3.0-beta").unwrap().to_json(),
            json!("0.3.0-beta")
        );
        assert!(parse_value(min, "0.3.0-").is_err());
        let switch = Flag::find(keys::INSTAGRAM_REPLAY).unwrap();
        assert_eq!(
            parse_value(switch, "false").unwrap(),
            FlagValue::Bool(false)
        );
        let err = parse_value(switch, "no").unwrap_err().to_string();
        assert!(err.contains("not JSON"), "{err}");
        let err = parse_value(switch, "0").unwrap_err().to_string();
        assert_eq!(err, "extension.instagram.replay: must be true or false");
        let steps = Flag::find(keys::MAX_STEPS).unwrap();
        assert_eq!(parse_value(steps, "20000").unwrap(), FlagValue::Int(20_000));
        let err = parse_value(steps, "0").unwrap_err().to_string();
        assert_eq!(
            err,
            "extension.maxSteps: must be an integer from 1 to 100000"
        );
    }
}
