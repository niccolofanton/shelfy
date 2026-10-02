//! Typed redaction (plan §3.7): values that must never reach the logs —
//! captions, post URLs, query strings, tokens, provider keys, emails — travel
//! in [`Redacted`], whose `Debug` and `Display` print `[redacted]`.
//!
//! A struct holding such a value derives `Debug` safely when the field is
//! `Redacted<_>`, and `tracing::info!(email = %Redacted(&email))` records the
//! field's presence without its content. Reading the value takes an explicit
//! [`Redacted::expose`].

use std::fmt;

/// What a redacted value prints as.
pub const REDACTED: &str = "[redacted]";

/// A value that is never printed.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Redacted<T>(pub T);

impl<T> Redacted<T> {
    /// The value, for the one place that needs it.
    #[must_use]
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Unwraps the value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> From<T> for Redacted<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    #[allow(dead_code)] // fields are only read through `Debug`
    struct Login {
        user: &'static str,
        email: Redacted<String>,
    }

    #[test]
    fn redacted_values_never_print() {
        let email = Redacted("owner@example.test".to_owned());
        assert_eq!(format!("{email}"), REDACTED);
        assert_eq!(format!("{email:?}"), REDACTED);
        let login = Login {
            user: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            email,
        };
        let debug = format!("{login:?}");
        assert!(!debug.contains("owner@example.test"), "{debug}");
        assert!(debug.contains(REDACTED));
        assert_eq!(login.email.expose(), "owner@example.test");
    }
}
