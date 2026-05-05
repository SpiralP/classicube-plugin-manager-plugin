#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};

use crate::secret::Secret;

const CONFIG_PATH: &str = "plugins/plugin-manager.toml";

/// Pre-rename config path. The v3 -> v4 startup migration renames this file
/// to [`CONFIG_PATH`] (and rewrites the self-subscription key) on first run,
/// then never touches it again.
const LEGACY_CONFIG_PATH: &str = "plugins/plugin-updater.toml";

/// Pre-rename crate name. The startup migration rewrites a self subscription
/// keyed at `(SELF_OWNER, LEGACY_SELF_REPO)` into `(SELF_OWNER, SELF_REPO)`,
/// so existing v3 users don't lose their subscription on upgrade.
const LEGACY_SELF_REPO: &str = "classicube-plugin-updater-plugin";

/// Comment line written between the user-editable region (every `[owner.repo]`
/// table) and the plugin-managed region (every `[owner.repo.state]` table) on
/// `save_to`. Comments are stripped on parse, so this is regenerated from
/// scratch on every write.
const STATE_DIVIDER: &str = "# ---- managed by plugin-manager (do not edit below) ----";

/// Owner of this plugin's own repo. Used to identify the "self" subscription
/// so the auto-update path can install over the loaded binary instead of
/// going through the managed-plugin pipeline.
pub const SELF_OWNER: &str = "SpiralP";

/// Repo of this plugin's own repo, derived from the crate name so the two
/// can't drift. Matches the canonical `classicube-$name-plugin` convention.
pub const SELF_REPO: &str = env!("CARGO_PKG_NAME");

pub fn config_path() -> &'static Path {
    Path::new(CONFIG_PATH)
}

/// Whether `(owner, repo)` refers to this plugin itself.
pub fn is_self(owner: &str, repo: &str) -> bool {
    owner == SELF_OWNER && repo == SELF_REPO
}

/// One-shot v3 -> v4 rename: if the legacy `plugins/plugin-updater.toml`
/// exists and the new path is absent, rename the file and rewrite a
/// `SpiralP/classicube-plugin-updater-plugin` self subscription to the new
/// crate name. After the new file is in place this is a no-op.
///
/// Errors are logged by the caller; a failed migration must not block
/// startup (the user can rename the file by hand).
pub fn migrate_legacy_config() -> Result<()> {
    migrate_legacy_config_at(Path::new(LEGACY_CONFIG_PATH), config_path())
}

pub(crate) fn migrate_legacy_config_at(legacy: &Path, current: &Path) -> Result<()> {
    if current.exists() || !legacy.exists() {
        return Ok(());
    }
    fs::rename(legacy, current)
        .with_context(|| format!("renaming {} -> {}", legacy.display(), current.display()))?;
    let mut cfg = Config::load_from(current)?;
    if rewrite_legacy_self_key(&mut cfg) {
        cfg.save_to(current)?;
    }
    Ok(())
}

/// Move `(SELF_OWNER, LEGACY_SELF_REPO)` -> `(SELF_OWNER, SELF_REPO)` in the
/// in-memory config. Returns `true` if anything changed. The new key wins on
/// collision; the legacy entry is dropped either way.
fn rewrite_legacy_self_key(cfg: &mut Config) -> bool {
    let Some(owner_map) = cfg.subscriptions.get_mut(SELF_OWNER) else {
        return false;
    };
    let Some(legacy_sub) = owner_map.remove(LEGACY_SELF_REPO) else {
        return false;
    };
    owner_map.entry(SELF_REPO.into()).or_insert(legacy_sub);
    true
}

/// Top-level config. The TOML document is the map directly: each subscription
/// renders as a `[owner.repo]` table at the document root, with no wrapper.
/// `BTreeMap` sorts keys alphabetically, so `save()` always rewrites the file
/// in a deterministic order regardless of the order subscriptions were added.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Config {
    pub subscriptions: BTreeMap<String, BTreeMap<String, Subscription>>,
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

/// User-editable subscription fields. Machine-managed install + cache fields
/// live under the nested `state` table (`[owner.repo.state]` in TOML), so
/// hand-edits to channel/disabled don't accidentally touch fields the plugin
/// owns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    #[serde(default, skip_serializing_if = "Channel::is_default")]
    pub channel: Channel,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Optional GitHub PAT used for this repo only. When set, attached as
    /// `Authorization: Bearer …` to release-list and asset-download calls.
    /// Wrapped in `Secret` so a stray `{:?}` doesn't leak it into logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Secret>,
    #[serde(default, skip_serializing_if = "SubscriptionState::is_empty")]
    pub state: SubscriptionState,
}

/// Plugin-managed state for a subscription. Renders as a `[owner.repo.state]`
/// subtable in TOML, omitted entirely when every field is `None`. Fields are
/// declared in A-Z order so `toml::to_string_pretty` writes them in
/// alphabetical order on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_published_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tag: Option<String>,
    /// Crash-recovery breadcrumb: the name of the managed-plugin
    /// `IGameComponent` callback currently in flight. Set right before we
    /// invoke a managed callback (and persisted to disk), cleared right after
    /// it returns. If the game crashes during the callback, this field
    /// survives the restart and tells us who to blame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_callback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_asset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
}

impl SubscriptionState {
    pub fn is_empty(&self) -> bool {
        self.cached_at.is_none()
            && self.cached_published_at.is_none()
            && self.cached_tag.is_none()
            && self.in_callback.is_none()
            && self.installed_asset.is_none()
            && self.installed_at.is_none()
            && self.installed_version.is_none()
    }
}

impl Subscription {
    /// Returns `(cached_tag, cached_published_at)` when the cache is within
    /// `ttl_secs` and both fields are populated. Both are required because
    /// downstream needs the tag for display/logging *and* the timestamp for
    /// the install decision.
    pub fn fresh_cached_release(&self, now: u64, ttl_secs: u64) -> Option<(&str, u64)> {
        let s = &self.state;
        match (&s.cached_tag, s.cached_at, s.cached_published_at) {
            (Some(tag), Some(at), Some(pub_at)) if now.saturating_sub(at) < ttl_secs => {
                Some((tag, pub_at))
            }
            _ => None,
        }
    }
}

/// Serialize-only mirror of the user-editable half of `Subscription`. Renders
/// the same `[owner.repo]` body as a full `Subscription` would, but with no
/// nested `state` field, so the user region of the file contains nothing
/// machine-managed. Fields are A-Z so `to_string_pretty` writes them in
/// alphabetical order. Values are owned (cloned from source) because serde's
/// `skip_serializing_if` predicates expect `fn(&FieldType) -> bool`, which
/// the stdlib helpers like `Channel::is_default` and `std::ops::Not::not`
/// only satisfy when the field is held by value.
#[derive(Serialize)]
struct UserView {
    #[serde(skip_serializing_if = "Channel::is_default")]
    channel: Channel,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<Secret>,
}

/// Single-key wrapper that adds a literal `state` segment to the table path
/// during the state-region serialize, so a `BTreeMap<owner, BTreeMap<repo,
/// StateLeaf>>` renders as `[owner.repo.state]` headers.
type StateLeaf<'a> = BTreeMap<&'static str, &'a SubscriptionState>;

impl Config {
    /// Ensure a subscription for this plugin's own repo exists so the
    /// self-update path picks it up automatically. Returns `true` if a
    /// fresh entry was added; the caller is responsible for persisting.
    /// An existing entry — even one the user has disabled or pinned — is
    /// left alone.
    pub fn ensure_self(&mut self) -> bool {
        let owner_map = self.subscriptions.entry(SELF_OWNER.into()).or_default();
        if owner_map.contains_key(SELF_REPO) {
            return false;
        }
        owner_map.insert(SELF_REPO.into(), Subscription::default());
        true
    }

    pub fn load() -> Result<Self> {
        Self::load_from(config_path())
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(config_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                let cfg: Self = toml::from_str(&contents)
                    .with_context(|| format!("parsing {}", path.display()))?;
                cfg.validate()
                    .with_context(|| format!("validating {}", path.display()))?;
                Ok(cfg)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Serialize and write the config, then `fsync` so the bytes are durable
    /// before we return. The crash-recovery breadcrumb relies on writes
    /// surviving an immediately-following process death; `fs::write` alone
    /// only hands the data to the kernel.
    ///
    /// The output is split into two regions: every `[owner.repo]` table comes
    /// first, then a single `STATE_DIVIDER` comment, then every
    /// `[owner.repo.state]` table. Each region is produced by its own
    /// `toml::to_string_pretty` call over a flat `BTreeMap` view of `self`,
    /// so concatenation is the only string work this function does.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let mut user_map: BTreeMap<&str, BTreeMap<&str, UserView>> = BTreeMap::new();
        let mut state_map: BTreeMap<&str, BTreeMap<&str, StateLeaf<'_>>> = BTreeMap::new();
        for (owner, repos) in &self.subscriptions {
            for (repo, sub) in repos {
                user_map.entry(owner).or_default().insert(
                    repo,
                    UserView {
                        channel: sub.channel.clone(),
                        disabled: sub.disabled,
                        token: sub.token.clone(),
                    },
                );
                if !sub.state.is_empty() {
                    let mut leaf = StateLeaf::new();
                    leaf.insert("state", &sub.state);
                    state_map.entry(owner).or_default().insert(repo, leaf);
                }
            }
        }

        let mut out = toml::to_string_pretty(&user_map).context("serializing user view")?;
        if !state_map.is_empty() {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(STATE_DIVIDER);
            out.push_str("\n\n");
            out.push_str(&toml::to_string_pretty(&state_map).context("serializing state view")?);
        }

        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        f.write_all(out.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync {}", path.display()))?;
        Ok(())
    }

    /// Reject configs whose owner/repo keys would be ambiguous or unsafe.
    /// `.` in a repo segment is a TOML nesting marker, so `[a.b.c]` parses
    /// as three nested tables, not as `repo = "b.c"`. We reject it on load
    /// so a hand-edit that uses an unquoted dotted name fails fast with a
    /// clear message instead of silently producing a deeper map.
    fn validate(&self) -> Result<()> {
        for (owner, repos) in &self.subscriptions {
            validate_segment("owner", owner)?;
            for repo in repos.keys() {
                validate_segment("repo", repo)?;
                if repo.contains('.') {
                    bail!(
                        "repo {repo:?} contains '.', which TOML parses as a nested table; rename \
                         the entry or use a quoted key"
                    );
                }
            }
        }
        Ok(())
    }
}

/// Set or clear the crash-recovery breadcrumb for one subscription, then
/// persist. Re-reads the on-disk config first so a concurrent cache write
/// from the background manager task doesn't clobber the breadcrumb (and
/// vice-versa) — same pattern as `persist_cache_updates_to`.
///
/// No-op if the subscription is no longer present.
pub fn set_in_callback_to(
    path: &Path,
    owner: &str,
    repo: &str,
    value: Option<String>,
) -> Result<()> {
    let mut cfg = Config::load_from(path)?;
    if let Some(sub) = cfg
        .subscriptions
        .get_mut(owner)
        .and_then(|m| m.get_mut(repo))
    {
        sub.state.in_callback = value;
        cfg.save_to(path)?;
    }
    Ok(())
}

fn validate_segment(kind: &str, s: &str) -> Result<()> {
    if s.is_empty() {
        bail!("{kind} segment is empty");
    }
    if s.chars().any(char::is_whitespace) {
        bail!("{kind} {s:?} contains whitespace");
    }
    Ok(())
}
