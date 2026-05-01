use std::{fs, io, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const CONFIG_PATH: &str = "plugins/plugin-updater.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub owner: String,
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        match fs::read_to_string(Path::new(CONFIG_PATH)) {
            Ok(contents) => {
                toml::from_str(&contents).with_context(|| format!("parsing {CONFIG_PATH}"))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {CONFIG_PATH}")),
        }
    }

    pub fn save(&self) -> Result<()> {
        let toml_str = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(Path::new(CONFIG_PATH), toml_str)
            .with_context(|| format!("writing {CONFIG_PATH}"))?;
        Ok(())
    }
}
