#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Mutex, PoisonError},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};

use crate::{atomic_write, secret::Secret};

/// Process-wide lock around the load + mutate + save chain. Held for the
/// entire body of [`Config::modify_at`] so the main thread and worker
/// threads (the deferred update pass, chat-command handlers) can't
/// interleave a read-modify-write cycle. Without this, a worker that
/// loaded stale state could save later and silently overwrite an edit
/// landed in between.
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

const CONFIG_PATH: &str = "plugins/plugin-manager.toml";

/// Pre-rename config path. The v3 -> v4 startup migration renames this file
/// to [`CONFIG_PATH`] (and rewrites the self-subscription key) on first run,
/// then never touches it again.
const LEGACY_CONFIG_PATH: &str = "plugins/plugin-updater.toml";

/// Pre-rename crate name. The startup migration rewrites a self subscription
/// keyed at `(SELF_OWNER, LEGACY_SELF_REPO)` into `(SELF_OWNER, SELF_REPO)`,
/// so existing v3 users don't lose their subscription on upgrade.
const LEGACY_SELF_REPO: &str = "classicube-plugin-updater-plugin";

/// Basename of the machine-managed state sidecar. Lives next to the breadcrumbs
/// dir inside `plugins/managed/` so the directory holds everything the manager
/// owns and a `rm -rf plugins/managed/` is a clean nuke-and-resubscribe. The
/// orphan sweep and the reconcile pass both filter this basename out so
/// neither classifies it as a stray plugin binary.
pub const MANAGED_STATE_BASENAME: &str = "state.toml";

/// Subdirectory under `plugins/` that holds managed plugin binaries, the
/// per-process breadcrumb files, and the sidecar state file.
const MANAGED_DIR_NAME: &str = "managed";

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

/// Derive the state-file path that pairs with `user_path`. Production code
/// passes `config_path()` which yields `plugins/managed/state.toml`. Tests
/// pass a tempfile path and get `<tempdir>/managed/state.toml`, so the
/// sidecar moves with the user file in every test harness.
///
/// Path layout: take the user file's parent (or `.` for a bare filename),
/// then descend into `managed/state.toml`. The user file's basename
/// (`plugin-manager.toml` in production, anything in tests) is not part of
/// the state path - the sidecar is named after its purpose, not after the
/// file it pairs with.
pub fn state_path_for(user_path: &Path) -> PathBuf {
    let parent = match user_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    parent.join(MANAGED_DIR_NAME).join(MANAGED_STATE_BASENAME)
}

/// Whether `(owner, repo)` refers to this plugin itself.
pub fn is_self(owner: &str, repo: &str) -> bool {
    owner == SELF_OWNER && repo == SELF_REPO
}

/// The version of the running manager binary, baked in at compile time.
/// Ground truth for "what self code is loaded right now" - the stored
/// `installed_version` can drift (half-applied self-update, hand-edited
/// TOML), the compiled-in version cannot. Release tags carry a leading `v`,
/// so we prefix one to match the tag-name string comparisons elsewhere.
pub fn self_installed_version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Whether the manager's own subscription is present and marked `disabled`.
/// `/disable` on self acts as a kill-switch: when true, both `Loader::init`
/// (host-Init managed-load) and the manager's deferred initial pass
/// (auto-update + Catchup managed-load) bail out, leaving the manager binary
/// loaded but otherwise dormant. Returns false when the entry is absent
/// (pre-`ensure_self` startup window).
pub fn is_self_disabled(cfg: &Config) -> bool {
    cfg.subscriptions
        .get(SELF_OWNER)
        .and_then(|repos| repos.get(SELF_REPO))
        .is_some_and(|s| s.disabled)
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
    Config::modify_at(current, rewrite_legacy_self_key)?;
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

/// One-shot v4 -> v5 split: if a v4 user file at `path` still carries
/// `[owner.repo.state]` subtables and the new sidecar at
/// `state_path_for(path)` doesn't exist yet, lift those state subtables
/// into the sidecar and rewrite the user file without them. After the
/// sidecar is in place this is a no-op.
///
/// We deliberately don't write an empty sidecar - if every legacy sub has
/// an empty state, the file looks fresh-install enough that leaving it
/// alone is correct (next save creates the sidecar naturally).
///
/// Errors are logged by the caller; a failed migration must not block
/// startup.
pub fn migrate_state_into_sidecar() -> Result<()> {
    migrate_state_into_sidecar_at(config_path())
}

pub(crate) fn migrate_state_into_sidecar_at(user_path: &Path) -> Result<()> {
    let state_path = state_path_for(user_path);
    if state_path.exists() {
        return Ok(());
    }
    let contents = match fs::read_to_string(user_path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", user_path.display())),
    };
    // Parse with the legacy schema (state subtable alive). A file already
    // in the new shape (no [owner.repo.state] tables) parses too - every
    // `state` field comes back default, which the "any state?" check
    // below skips over.
    let legacy: LegacyConfig = toml::from_str(&contents)
        .with_context(|| format!("parsing legacy {}", user_path.display()))?;

    let any_state = legacy
        .subscriptions
        .values()
        .flat_map(|repos| repos.values())
        .any(|s| !s.state.is_empty());
    if !any_state {
        return Ok(());
    }

    // Hand the lifted in-memory config off to `save_to`, which writes both
    // the sidecar and the rewritten user file via the regular atomic-rename
    // path. Sidecar first, user file second - the same crash-safety
    // ordering as a normal save.
    let modern = legacy.into_modern();
    modern.save_to(user_path)?;
    Ok(())
}

/// Parallel layout used only by [`migrate_state_into_sidecar_at`] to parse a
/// v4 user file with its `[owner.repo.state]` subtables intact. Once parsed
/// it converts straight into a modern [`Config`]; nothing else in the codebase
/// touches this type.
#[derive(Deserialize)]
#[serde(transparent)]
struct LegacyConfig {
    subscriptions: BTreeMap<String, BTreeMap<String, LegacySubscription>>,
}

#[derive(Deserialize)]
struct LegacySubscription {
    #[serde(default)]
    channel: Channel,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    token: Option<Secret>,
    #[serde(default)]
    state: SubscriptionState,
}

impl LegacyConfig {
    fn into_modern(self) -> Config {
        let mut subscriptions = BTreeMap::new();
        for (owner, repos) in self.subscriptions {
            let mut out = BTreeMap::new();
            for (repo, sub) in repos {
                out.insert(
                    repo,
                    Subscription {
                        channel: sub.channel,
                        disabled: sub.disabled,
                        token: sub.token,
                        state: sub.state,
                    },
                );
            }
            subscriptions.insert(owner, out);
        }
        Config { subscriptions }
    }
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

/// In-memory subscription record. Two regions live side by side on the same
/// struct so existing call sites can read `sub.channel` and `sub.state.X`
/// the same way; on disk those two regions are persisted to **separate
/// files** (`plugins/plugin-manager.toml` and `plugins/managed/state.toml`)
/// so a hand-edit of the user file can never clobber machine-managed state.
///
/// The `state` field is marked `#[serde(skip)]` so the user file's
/// `[owner.repo]` table only carries `channel` / `disabled` / `token`.
/// `Config::load_from` re-populates `state` from the sidecar after parsing
/// the user file; `Config::save_to` extracts it back into the sidecar.
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
    /// Machine-managed state for this subscription. Persisted to the sidecar
    /// (`plugins/managed/state.toml`), not the user file - serde skips this
    /// field entirely on the `Subscription` round-trip, and the load/save
    /// path zips it in/out around the user-file I/O.
    #[serde(skip)]
    pub state: SubscriptionState,
}

/// Plugin-managed state for a subscription. Renders into the sidecar
/// `plugins/managed/state.toml` under a hoisted `[owner.repo]` header (no
/// redundant `.state` nesting - the whole file is state). Fields are declared
/// in A-Z order so `toml::to_string_pretty` writes them in alphabetical
/// order on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_published_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tag: Option<String>,
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

/// On-disk shape of `plugins/managed/state.toml`. Hoisted layout: each
/// `[owner.repo]` header in the sidecar holds the [`SubscriptionState`]
/// fields directly, with no redundant `.state` nesting (the whole file is
/// state, so the extra segment would carry no information). Empty entries
/// are skipped on save - the file only contains rows for subs that actually
/// have cached/installed data.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct StateFile {
    entries: BTreeMap<String, BTreeMap<String, SubscriptionState>>,
}

impl StateFile {
    fn load_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                toml::from_str(&contents).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Write the sidecar via tmpfile + atomic rename, creating the
    /// `plugins/managed/` directory if it doesn't exist yet. The state file
    /// is always rewritten (even when empty); an empty file on disk means
    /// "no managed state yet" and is the natural fresh-install marker.
    fn save_to(&self, path: &Path) -> Result<()> {
        let serialized = toml::to_string_pretty(self).context("serializing state sidecar")?;
        atomic_write::write_synced(path, serialized.as_bytes())
    }
}

impl Config {
    /// Ensure a subscription for this plugin's own repo exists so the
    /// self-update path picks it up automatically, and stamp its stored
    /// install state to the running binary. Returns `true` only when a fresh
    /// entry was added; an existing entry's user-file fields - channel,
    /// token, disabled - are left alone (even if disabled or pinned). The
    /// caller is responsible for persisting.
    ///
    /// `installed_version` (and `installed_asset`, when `asset` is `Some`)
    /// are (re)written every call: the running binary is ground truth for
    /// what self code is loaded, and the stored values can drift
    /// (half-applied self-update, hand-edited TOML). `installed_at` and the
    /// `cached_*` fields are left untouched - `needs_install` compares
    /// `installed_at` against the latest release's `published_at`, so
    /// overwriting it here could suppress a genuinely-newer release.
    ///
    /// `version` / `asset` are passed in (the caller resolves them from
    /// [`self_installed_version`] and the loaded library path) so this stays
    /// a pure in-memory config op with no filesystem dependency.
    pub fn ensure_self(&mut self, version: &str, asset: Option<&str>) -> bool {
        let owner_map = self.subscriptions.entry(SELF_OWNER.into()).or_default();
        let added = !owner_map.contains_key(SELF_REPO);
        let sub = owner_map.entry(SELF_REPO.into()).or_default();
        sub.state.installed_version = Some(version.to_owned());
        if let Some(asset) = asset {
            sub.state.installed_asset = Some(asset.to_owned());
        }
        added
    }

    pub fn load() -> Result<Self> {
        Self::load_from(config_path())
    }

    /// Load both the user file at `path` and the state sidecar at
    /// `state_path_for(path)`, zipping `[owner.repo]` state entries from the
    /// sidecar into the matching in-memory [`Subscription`]. A missing user
    /// file yields `Config::default()` (matches the "fresh install" case the
    /// game can hit on first run); a missing sidecar yields no state but is
    /// not an error. State rows referencing an `(owner, repo)` that the user
    /// file doesn't list are silently dropped - they'd never serialize back
    /// out anyway, so carrying them would just be dead weight.
    pub fn load_from(path: &Path) -> Result<Self> {
        let mut cfg: Self = match fs::read_to_string(path) {
            Ok(contents) => {
                toml::from_str(&contents).with_context(|| format!("parsing {}", path.display()))?
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        cfg.validate()
            .with_context(|| format!("validating {}", path.display()))?;

        let state_path = state_path_for(path);
        let state = StateFile::load_from(&state_path)?;
        for (owner, repos) in state.entries {
            let Some(user_repos) = cfg.subscriptions.get_mut(&owner) else {
                continue;
            };
            for (repo, st) in repos {
                if let Some(sub) = user_repos.get_mut(&repo) {
                    sub.state = st;
                }
            }
        }
        Ok(cfg)
    }

    /// Atomically load + mutate + save the config at `path`. Acquires
    /// [`CONFIG_LOCK`] for the entire chain so concurrent modifications
    /// can't lose updates to each other. The closure runs synchronously
    /// on the calling thread; it must NOT block on async I/O or wait on
    /// other [`Config::modify_at`] callers (would deadlock).
    ///
    /// Always saves, even if `f` made no changes. The cost is one fsync;
    /// the win is that callers don't need a "did anything change?" return
    /// from the closure, and the lock is held for a uniform duration.
    pub fn modify_at<F, R>(path: &Path, f: F) -> Result<R>
    where
        F: FnOnce(&mut Self) -> R,
    {
        let _guard = CONFIG_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let mut cfg = Self::load_from(path)?;
        let r = f(&mut cfg);
        cfg.save_to(path)?;
        Ok(r)
    }

    /// Persist the config in two files: the user-editable TOML at `path`,
    /// and the machine-managed state sidecar at `state_path_for(path)`. The
    /// `state` field on each [`Subscription`] is `#[serde(skip)]`, so the
    /// user file naturally drops everything machine-owned; the sidecar
    /// rebuilds a parallel `BTreeMap<owner, BTreeMap<repo, SubscriptionState>>`
    /// from those skipped fields.
    ///
    /// Each file is written via [`atomic_write::write_synced`] (tmpfile +
    /// atomic rename + fsync). The random suffix on the tmp file prevents
    /// two ClassiCube instances sharing the same config from truncating
    /// each other's tmp, and the atomic rename means concurrent readers
    /// always see either the old or the new file - never a truncated
    /// mid-state.
    ///
    /// Write order: sidecar first, then user file. If we crash between the
    /// two, the sidecar may reference an `(owner, repo)` the user file no
    /// longer carries - which is harmless because `load_from` silently
    /// drops sidecar rows without a matching user entry.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let mut state = StateFile::default();
        for (owner, repos) in &self.subscriptions {
            for (repo, sub) in repos {
                if !sub.state.is_empty() {
                    state
                        .entries
                        .entry(owner.clone())
                        .or_default()
                        .insert(repo.clone(), sub.state.clone());
                }
            }
        }
        state.save_to(&state_path_for(path))?;

        let serialized = toml::to_string_pretty(self).context("serializing user view")?;
        atomic_write::write_synced(path, serialized.as_bytes())
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

fn validate_segment(kind: &str, s: &str) -> Result<()> {
    if s.is_empty() {
        bail!("{kind} segment is empty");
    }
    if s.chars().any(char::is_whitespace) {
        bail!("{kind} {s:?} contains whitespace");
    }
    Ok(())
}
