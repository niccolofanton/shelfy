//! Whether a user's browser extension is connected (P2-03; contract C8,
//! plan §2.10 `extension.status`).
//!
//! Every request with an extension token marks it seen, with the version
//! its `X-Shelfy-Extension` header names ([`super::seen`]). In memory only:
//! a restart forgets it, and the next request reconnects.
//!
//! A user's status:
//!
//! - `connected`: one of their extension tokens made a request in the last
//!   [`super::ExtensionSettings::presence_timeout`] (10 minutes);
//! - `version`: the highest version those tokens sent, so two browsers on
//!   different versions do not make it flap; once disconnected, the version
//!   it had when last connected;
//! - `lastSeenAt`: the newest request of any of their tokens since the
//!   server started.
//!
//! `extension.status` goes out when `connected` or `version` changes, not on
//! every request: the first request after a silence, a new version, the
//! maintenance sweep that finds every token silent ([`Presence::sweep`],
//! every 30 s), or the revocation of the last connected token
//! ([`Presence::forget`]). Memory: one entry per user whose extension made a
//! request since the start, holding the tokens seen in the last timeout.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::ExtensionVersion;
use crate::auth::millis;
use crate::events::model::ExtensionStatusEvent;

/// The extensions of every user.
pub struct Presence {
    timeout_ms: i64,
    users: Mutex<HashMap<Box<str>, UserPresence>>,
}

#[derive(Default)]
struct UserPresence {
    /// Last request of each token seen within the timeout, by token id.
    tokens: HashMap<Box<str>, Seen>,
    /// The status last announced: `(connected, version)`.
    announced: (bool, Option<ExtensionVersion>),
    /// The newest request of any token, unix ms.
    last_seen_at: Option<i64>,
}

#[derive(Clone, Copy)]
struct Seen {
    at: i64,
    version: Option<ExtensionVersion>,
}

impl UserPresence {
    /// `(connected, version)` at `now`.
    fn state(&self, now: i64, timeout_ms: i64) -> (bool, Option<ExtensionVersion>) {
        let live = self
            .tokens
            .values()
            .filter(|seen| now.saturating_sub(seen.at) < timeout_ms);
        let mut connected = false;
        let mut version = None;
        for seen in live {
            connected = true;
            version = version.max(seen.version);
        }
        if connected {
            (true, version)
        } else {
            (false, self.announced.1)
        }
    }

    fn status(&self, state: (bool, Option<ExtensionVersion>)) -> ExtensionStatusEvent {
        ExtensionStatusEvent {
            connected: state.0,
            version: state.1.map(|version| version.to_string()),
            last_seen_at: self.last_seen_at,
        }
    }

    /// The status to announce, if it changed since the last one.
    fn settle(&mut self, now: i64, timeout_ms: i64) -> Option<ExtensionStatusEvent> {
        let state = self.state(now, timeout_ms);
        if state == self.announced {
            return None;
        }
        self.announced = state;
        Some(self.status(state))
    }
}

impl Presence {
    /// Presence where `timeout` of silence disconnects.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout_ms: millis(timeout).max(1),
            users: Mutex::new(HashMap::new()),
        }
    }

    fn users(&self) -> MutexGuard<'_, HashMap<Box<str>, UserPresence>> {
        self.users.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a request of token `token_id` of `user_id` at `now` (unix
    /// ms) that named `version`; returns the user's status when it changed.
    pub fn seen(
        &self,
        user_id: &str,
        token_id: &str,
        version: Option<ExtensionVersion>,
        now: i64,
    ) -> Option<ExtensionStatusEvent> {
        let mut users = self.users();
        let user = users.entry(user_id.into()).or_default();
        let seen = user
            .tokens
            .entry(token_id.into())
            .or_insert(Seen { at: now, version });
        // A late request never moves the time back.
        if now >= seen.at {
            *seen = Seen { at: now, version };
        }
        user.last_seen_at = user.last_seen_at.max(Some(now));
        user.settle(now, self.timeout_ms)
    }

    /// Token `token_id` of `user_id` was revoked: it no longer keeps the
    /// extension connected. Returns the user's status when it changed.
    pub fn forget(&self, user_id: &str, token_id: &str, now: i64) -> Option<ExtensionStatusEvent> {
        let mut users = self.users();
        let user = users.get_mut(user_id)?;
        user.tokens.remove(token_id)?;
        user.settle(now, self.timeout_ms)
    }

    /// The status of `user_id`'s extension at `now`.
    #[must_use]
    pub fn status(&self, user_id: &str, now: i64) -> ExtensionStatusEvent {
        match self.users().get(user_id) {
            Some(user) => user.status(user.state(now, self.timeout_ms)),
            None => ExtensionStatusEvent {
                connected: false,
                version: None,
                last_seen_at: None,
            },
        }
    }

    /// Drops the tokens silent for the timeout at `now`; returns the users
    /// whose status changed, with it.
    pub fn sweep(&self, now: i64) -> Vec<(String, ExtensionStatusEvent)> {
        let timeout_ms = self.timeout_ms;
        let mut changed = Vec::new();
        for (user_id, user) in self.users().iter_mut() {
            user.tokens
                .retain(|_, seen| now.saturating_sub(seen.at) < timeout_ms);
            if let Some(status) = user.settle(now, timeout_ms) {
                changed.push((user_id.to_string(), status));
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_790_899_200_000;
    const MINUTE: i64 = 60_000;
    const ALICE: &str = "01ALICE0000000000000000000";
    const BOB: &str = "01BOB000000000000000000000";

    fn presence() -> Presence {
        Presence::new(Duration::from_secs(600))
    }

    fn v(text: &str) -> Option<ExtensionVersion> {
        ExtensionVersion::parse(text)
    }

    fn status(connected: bool, version: Option<&str>, at: i64) -> ExtensionStatusEvent {
        ExtensionStatusEvent {
            connected,
            version: version.map(str::to_owned),
            last_seen_at: Some(at),
        }
    }

    #[test]
    fn a_request_connects_and_ten_silent_minutes_disconnect() {
        let p = presence();
        assert_eq!(
            p.status(ALICE, T0),
            ExtensionStatusEvent {
                connected: false,
                version: None,
                last_seen_at: None
            }
        );
        assert_eq!(
            p.seen(ALICE, "t1", v("0.2.0"), T0),
            Some(status(true, Some("0.2.0"), T0))
        );
        // More requests with the same version announce nothing.
        assert_eq!(p.seen(ALICE, "t1", v("0.2.0"), T0 + MINUTE), None);
        assert_eq!(
            p.status(ALICE, T0 + MINUTE),
            status(true, Some("0.2.0"), T0 + MINUTE)
        );
        // The sweep before the timeout keeps it; after it, disconnects once.
        assert!(p.sweep(T0 + 10 * MINUTE).is_empty());
        let swept = p.sweep(T0 + 11 * MINUTE);
        assert_eq!(
            swept,
            [(ALICE.to_owned(), status(false, Some("0.2.0"), T0 + MINUTE))]
        );
        assert!(p.sweep(T0 + 12 * MINUTE).is_empty());
        assert_eq!(
            p.status(ALICE, T0 + 12 * MINUTE),
            status(false, Some("0.2.0"), T0 + MINUTE),
            "the last version and time are kept"
        );
        // The next request reconnects.
        assert_eq!(
            p.seen(ALICE, "t1", v("0.2.0"), T0 + 20 * MINUTE),
            Some(status(true, Some("0.2.0"), T0 + 20 * MINUTE))
        );
    }

    #[test]
    fn the_status_reads_the_clock_even_before_the_sweep() {
        let p = presence();
        let _ = p.seen(ALICE, "t1", v("0.2.0"), T0);
        assert!(p.status(ALICE, T0 + 10 * MINUTE - 1).connected);
        assert!(!p.status(ALICE, T0 + 10 * MINUTE).connected);
    }

    #[test]
    fn a_new_version_is_announced_and_two_browsers_do_not_flap() {
        let p = presence();
        let _ = p.seen(ALICE, "chrome", v("0.2.0"), T0);
        assert_eq!(
            p.seen(ALICE, "chrome", v("0.3.0"), T0 + 1),
            Some(status(true, Some("0.3.0"), T0 + 1))
        );
        // A second browser on the older version: the highest one stands.
        assert_eq!(p.seen(ALICE, "laptop", v("0.2.0"), T0 + 2), None);
        for i in 3..10 {
            let token = if i % 2 == 0 { "chrome" } else { "laptop" };
            let version = if i % 2 == 0 { "0.3.0" } else { "0.2.0" };
            assert_eq!(p.seen(ALICE, token, v(version), T0 + i), None);
        }
        // Once the newer browser is silent, the older version shows.
        let swept = p.sweep(T0 + 8 + 10 * MINUTE);
        assert_eq!(
            swept,
            [(ALICE.to_owned(), status(true, Some("0.2.0"), T0 + 9))]
        );
    }

    #[test]
    fn a_request_without_a_version_still_connects() {
        let p = presence();
        assert_eq!(
            p.seen(ALICE, "t1", None, T0),
            Some(ExtensionStatusEvent {
                connected: true,
                version: None,
                last_seen_at: Some(T0)
            })
        );
        assert_eq!(
            p.seen(ALICE, "t1", v("0.2.0"), T0 + 1),
            Some(status(true, Some("0.2.0"), T0 + 1))
        );
    }

    #[test]
    fn revoking_the_last_connected_token_disconnects_at_once() {
        let p = presence();
        let _ = p.seen(ALICE, "t1", v("0.2.0"), T0);
        let _ = p.seen(ALICE, "t2", v("0.2.0"), T0);
        assert_eq!(p.forget(ALICE, "t1", T0 + 1), None, "t2 is still there");
        assert_eq!(p.forget(ALICE, "unknown", T0 + 1), None);
        assert_eq!(p.forget(BOB, "t2", T0 + 1), None, "another user's token");
        assert_eq!(
            p.forget(ALICE, "t2", T0 + 2),
            Some(status(false, Some("0.2.0"), T0))
        );
    }

    #[test]
    fn users_are_kept_apart() {
        let p = presence();
        let _ = p.seen(ALICE, "t1", v("0.2.0"), T0);
        assert!(!p.status(BOB, T0).connected);
        assert_eq!(
            p.seen(BOB, "t9", v("0.4.0"), T0 + 1),
            Some(status(true, Some("0.4.0"), T0 + 1))
        );
        assert_eq!(p.status(ALICE, T0 + 1).version.as_deref(), Some("0.2.0"));
        let mut swept = p.sweep(T0 + 20 * MINUTE);
        swept.sort_by(|a, b| a.0.cmp(&b.0));
        let users: Vec<&str> = swept.iter().map(|(user, _)| user.as_str()).collect();
        assert_eq!(users, [ALICE, BOB]);
    }

    #[test]
    fn a_late_request_never_moves_the_time_back() {
        let p = presence();
        let _ = p.seen(ALICE, "t1", v("0.3.0"), T0 + 5);
        assert_eq!(p.seen(ALICE, "t1", v("0.2.0"), T0), None);
        assert_eq!(p.status(ALICE, T0 + 6), status(true, Some("0.3.0"), T0 + 5));
    }
}
