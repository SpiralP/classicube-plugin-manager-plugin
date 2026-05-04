use std::fmt;

use serde::{Deserialize, Serialize};

/// String wrapper that redacts itself in `Debug` output. Use for any field
/// (currently only the per-subscription GitHub token) whose value would be
/// dangerous to leak into `tracing` logs or panic messages.
///
/// `Debug` is the only redaction — there is intentionally no `Display` impl
/// so a stray `{}` format won't print the secret. Call `expose()` explicitly
/// when you need the raw string (i.e. constructing an Authorization header).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    #[cfg(test)]
    pub fn new(s: String) -> Self {
        Self(s)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_leak() {
        let s = Secret::new("github_pat_supersecret".into());
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("supersecret"), "debug output leaked: {dbg}");
        assert_eq!(dbg, "Secret(<redacted>)");
    }

    #[test]
    fn expose_returns_inner() {
        let s = Secret::new("abc".into());
        assert_eq!(s.expose(), "abc");
    }

    #[test]
    fn serde_round_trip() {
        let s = Secret::new("ghp_xyz".into());
        let toml_value = toml::Value::try_from(&s).unwrap();
        assert_eq!(toml_value.as_str(), Some("ghp_xyz"));
        let back: Secret = toml_value.try_into().unwrap();
        assert_eq!(back.expose(), "ghp_xyz");
    }
}
