#[cfg(test)]
mod tests;

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
