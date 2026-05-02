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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_asset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<u64>,
}

impl Subscription {
    pub fn fresh_cached_tag(&self, now: u64, ttl_secs: u64) -> Option<&str> {
        match (&self.cached_tag, self.cached_at) {
            (Some(tag), Some(at)) if now.saturating_sub(at) < ttl_secs => Some(tag),
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

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;

    fn sub(owner: &str, repo: &str) -> Subscription {
        Subscription {
            owner: owner.into(),
            repo: repo.into(),
            installed_version: None,
            installed_asset: None,
            cached_tag: None,
            cached_at: None,
        }
    }

    #[test]
    fn fresh_within_ttl() {
        let s = Subscription {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(100),
            ..sub("a", "b")
        };
        assert_eq!(s.fresh_cached_tag(150, 100), Some("v1.0.0"));
    }

    #[test]
    fn fresh_at_ttl_boundary_is_stale() {
        // The check is strict `<`, so equal-to-TTL counts as expired.
        let s = Subscription {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(100),
            ..sub("a", "b")
        };
        assert_eq!(s.fresh_cached_tag(200, 100), None);
    }

    #[test]
    fn fresh_with_clock_skew() {
        // saturating_sub avoids panicking when `now < cached_at`; treat as fresh.
        let s = Subscription {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(500),
            ..sub("a", "b")
        };
        assert_eq!(s.fresh_cached_tag(100, 60), Some("v1.0.0"));
    }

    #[test]
    fn missing_cached_tag_is_stale() {
        let s = Subscription {
            cached_tag: None,
            cached_at: Some(100),
            ..sub("a", "b")
        };
        assert_eq!(s.fresh_cached_tag(150, 100), None);
    }

    #[test]
    fn missing_cached_at_is_stale() {
        let s = Subscription {
            cached_tag: Some("v1.0.0".into()),
            cached_at: None,
            ..sub("a", "b")
        };
        assert_eq!(s.fresh_cached_tag(150, 100), None);
    }

    #[test]
    fn load_missing_file_yields_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn load_malformed_file_errors() {
        let mut f = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"this is not = valid ::: toml [[[").unwrap();
        let err = Config::load_from(f.path()).unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("parsing"), "expected 'parsing' in: {chain}");
    }

    #[test]
    fn round_trip_default_config() {
        let f = NamedTempFile::new().unwrap();
        Config::default().save_to(f.path()).unwrap();
        let loaded = Config::load_from(f.path()).unwrap();
        assert_eq!(loaded, Config::default());
    }

    #[test]
    fn round_trip_populated_subscription() {
        let cfg = Config {
            subscriptions: vec![Subscription {
                owner: "octocat".into(),
                repo: "hello-world".into(),
                installed_version: Some("v1.2.3".into()),
                installed_asset: Some("hello-world.so".into()),
                cached_tag: Some("v1.2.4".into()),
                cached_at: Some(1_700_000_000),
            }],
        };
        let f = NamedTempFile::new().unwrap();
        cfg.save_to(f.path()).unwrap();
        let loaded = Config::load_from(f.path()).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn bare_subscription_skips_optional_fields_in_toml() {
        let cfg = Config {
            subscriptions: vec![sub("octocat", "hello-world")],
        };
        let f = NamedTempFile::new().unwrap();
        cfg.save_to(f.path()).unwrap();
        let on_disk = fs::read_to_string(f.path()).unwrap();
        assert!(!on_disk.contains("installed_version"));
        assert!(!on_disk.contains("installed_asset"));
        assert!(!on_disk.contains("cached_tag"));
        assert!(!on_disk.contains("cached_at"));
        // Round-trip still works.
        let loaded = Config::load_from(f.path()).unwrap();
        assert_eq!(loaded, cfg);
    }
}
