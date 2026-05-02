#[cfg(test)]
mod tests;

use std::{fs, io, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const CONFIG_PATH: &str = "plugins/plugin-updater.toml";

pub fn config_path() -> &'static Path {
    Path::new(CONFIG_PATH)
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub owner: String,
    pub repo: String,
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
