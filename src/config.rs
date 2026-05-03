#[cfg(test)]
mod tests;

use std::{fs, io, path::Path, str::FromStr};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};

const CONFIG_PATH: &str = "plugins/plugin-updater.toml";

pub fn config_path() -> &'static Path {
    Path::new(CONFIG_PATH)
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
}

/// Which release line a subscription tracks. Stable is the default — same as
/// the historical "always /releases/latest" behavior. Prerelease picks the
/// newest entry from `/releases` (regardless of the prerelease bit), so it
/// captures both regular and pre-release tags. Tag pins to a specific
/// release; auto-update is effectively a no-op once that tag is installed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Channel {
    #[default]
    Stable,
    Prerelease,
    Tag(String),
}

impl Channel {
    pub fn is_default(&self) -> bool {
        matches!(self, Self::Stable)
    }

    /// Validate a tag string for `Channel::Tag`. Empty or whitespace-bearing
    /// tags are rejected so we never construct a tag we can't put in a URL.
    pub fn from_tag(tag: &str) -> Result<Self, String> {
        let trimmed = tag.trim();
        if trimmed.is_empty() {
            Err("tag channel requires a non-empty tag".into())
        } else if trimmed.chars().any(char::is_whitespace) {
            Err(format!("tag must not contain whitespace: {tag:?}"))
        } else {
            Ok(Self::Tag(trimmed.to_owned()))
        }
    }

    /// Human-readable label for chat output. Stable returns `"stable"` even
    /// though we usually skip rendering it; `/list` and `/channel` decide
    /// whether to show it.
    pub fn pretty(&self) -> String {
        match self {
            Self::Stable => "stable".into(),
            Self::Prerelease => "prerelease".into(),
            Self::Tag(v) => format!("tag: {v}"),
        }
    }
}

impl FromStr for Channel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stable" => Ok(Self::Stable),
            "prerelease" => Ok(Self::Prerelease),
            other => match other.strip_prefix("tag:") {
                Some(t) => Self::from_tag(t),
                None => Err(format!(
                    "unknown channel {other:?}; expected stable, prerelease, or tag:<ref>"
                )),
            },
        }
    }
}

impl Serialize for Channel {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Stable => s.serialize_str("stable"),
            Self::Prerelease => s.serialize_str("prerelease"),
            Self::Tag(t) => s.serialize_str(&format!("tag:{t}")),
        }
    }
}

impl<'de> Deserialize<'de> for Channel {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Channel::from_str(&s).map_err(DeError::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub owner: String,
    pub repo: String,
    #[serde(default, skip_serializing_if = "Channel::is_default")]
    pub channel: Channel,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_asset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_published_at: Option<u64>,
}

impl Subscription {
    /// Returns `(cached_tag, cached_published_at)` when the cache is within
    /// `ttl_secs` and both fields are populated. Both are required because
    /// downstream needs the tag for display/logging *and* the timestamp for
    /// the install decision.
    pub fn fresh_cached_release(&self, now: u64, ttl_secs: u64) -> Option<(&str, u64)> {
        match (&self.cached_tag, self.cached_at, self.cached_published_at) {
            (Some(tag), Some(at), Some(pub_at)) if now.saturating_sub(at) < ttl_secs => {
                Some((tag, pub_at))
            }
            _ => None,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(config_path())
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                toml::from_str(&contents).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        let toml_str = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(path, toml_str).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
