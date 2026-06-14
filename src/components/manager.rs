#[cfg(test)]
mod tests;

use std::{
    cell::Cell,
    env,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use classicube_helpers::{async_manager, color};
use tracing::{debug, error, info, warn};

use crate::{
    asset_match::{self, pick_asset},
    chat::print_async,
    component::Component,
    config::{self, Config, Subscription, config_path},
    github_release::{GitHubRelease, get_release_for_channel, resolve_expected_digest},
    installer::{
        self, MANAGED_DIR, PLUGINS_DIR, cleanup_self_old, download_self, download_to_managed_dir,
        mark_previous_self_aside,
    },
    loader::{LifecyclePhase, init_managed},
    reconcile::{self, ConflictDir},
    secret::Secret,
    self_path::current_lib_path,
};

const TTL_SECS: u64 = 60 * 60;

thread_local!(
    static CHECKED: Cell<bool> = const { Cell::new(false) };
);

#[derive(Default)]
pub struct Manager;

impl Component for Manager {
    fn name(&self) -> &'static str {
        "Manager"
    }

    fn init(&mut self) {
        if let Err(e) = config::migrate_legacy_config() {
            warn!("legacy config migration failed: {e:#}");
        }
        // v4 -> v5: lift `[owner.repo.state]` subtables out of the user
        // file into `plugins/managed/state.toml`. Runs after the v3->v4
        // rename so a v3 install gets renamed then split in a single boot.
        if let Err(e) = config::migrate_state_into_sidecar() {
            warn!("state sidecar migration failed: {e:#}");
        }
    }

    fn free(&mut self) {
        // The deferred update pass is one-shot per init cycle. Reset so a
        // hot-reload's next on_new_map_loaded re-runs it.
        CHECKED.set(false);
    }

    fn on_new_map_loaded(&mut self) {
        if CHECKED.get() {
            return;
        }
        CHECKED.set(true);

        async_manager::spawn(async move {
            if let Err(e) = run_initial_pass().await {
                error!("initial update pass failed: {e:#}");
                print_async(format!(
                    "{}Plugin manager pass failed: {}{e}",
                    color::RED,
                    color::WHITE,
                ))
                .await;
            }

            // Hand off to the loader on the main thread regardless of update
            // outcome — load whatever's on disk even if a network fetch failed.
            // Load fresh off-thread so we see installed_asset writes from the
            // pass, then hop to main only for the dlopen. Flatten the nested
            // map into triples so the loader keeps a simple slice signature.
            let cfg = match Config::load() {
                Ok(cfg) => cfg,
                Err(e) => {
                    error!("loading config for managed-load: {e:#}");
                    return;
                }
            };
            // Same self-disabled kill-switch as Loader::init: if the user
            // has disabled the manager's own subscription, skip the orphan
            // sweep AND the Catchup managed-load. run_initial_pass already
            // bailed before any installs, so there's nothing fresh on disk
            // to pick up here, and the user expects total dormancy (no
            // surprise file deletions in plugins/managed/).
            if config::is_self_disabled(&cfg) {
                info!(
                    "manager subscription is disabled; skipping orphan sweep + Catchup \
                     managed-load"
                );
                return;
            }
            // Drop any file in plugins/managed/ that no live subscription
            // claims as its installed_asset. Runs AFTER the update pass (so
            // newly written versioned files are already claimed) and BEFORE
            // init_managed (so we don't unlink something we're about to
            // dlopen).
            let swept = reconcile::sweep_managed_orphans(Path::new(MANAGED_DIR), &cfg);
            if !swept.is_empty() {
                // Warn at the point of action: the orphan / managed-conflict
                // observations in `run_reconcile_and_warn` were demoted
                // (they're transient predictions of what's about to be
                // deleted); this consolidated line is the actual record of
                // what got removed.
                warn!(
                    "cleaned up {} stale plugin binar{}: {}",
                    swept.len(),
                    if swept.len() == 1 { "y" } else { "ies" },
                    swept.join(", "),
                );
            }
            let subs: Vec<(String, String, Subscription)> = cfg
                .subscriptions
                .into_iter()
                .flat_map(|(owner, repos)| {
                    repos
                        .into_iter()
                        .map(move |(repo, sub)| (owner.clone(), repo, sub))
                })
                .collect();
            async_manager::run_on_main_thread(async move {
                // Catchup phase: subs already loaded at host-Init time return
                // AlreadyLoaded and are silently skipped. Subs the deferred
                // pass freshly installed get Init + OnNewMap + OnNewMapLoaded
                // since the host already dispatched those against an empty
                // LOADED before this future ran.
                init_managed(&subs, LifecyclePhase::Catchup);
            })
            .await;
        });
    }
}

async fn run_initial_pass() -> Result<()> {
    cleanup_self_old();
    ensure_self_subscription().await;

    // Honor /disable on the manager's own subscription: skip reconcile, the
    // per-sub update loop, and (via the early-return in on_new_map_loaded)
    // the orphan sweep + Catchup managed-load. Explicit user commands like
    // /update or /load still work; this only short-circuits the auto-pass.
    if config::is_self_disabled(&Config::load()?) {
        info!("manager subscription is disabled; skipping initial update pass");
        return Ok(());
    }

    run_reconcile_and_warn().await;

    let subs = Config::load()?.subscriptions;
    if subs.is_empty() {
        info!("no subscriptions; skipping update check");
        return Ok(());
    }

    let now = unix_now();
    let mut new_tags: Vec<(String, String, String, u64)> = Vec::new();
    let mut installed: Vec<(String, String, String, String, u64)> = Vec::new();

    for (owner, repos) in &subs {
        for (repo, sub) in repos {
            if sub.disabled {
                debug!("{owner}/{repo} disabled; skipping");
                continue;
            }

            // For self, skip auto-update entirely if the loaded binary
            // looks like a dev/manual build (cargo-build output or
            // hand-placed canonical name). Done before the release fetch
            // and the user-facing "Downloading..." message so a dev iterating
            // on the manager doesn't see noise about an update that would
            // overwrite their own build. Released assets carry
            // `_<os>_<arch>` tokens and don't normalize to SELF_REPO.
            if config::is_self(owner, repo)
                && let Some(loaded_basename) = current_lib_path()
                    .ok()
                    .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
                && asset_match::is_canonical_or_cdylib_name(
                    &loaded_basename,
                    config::SELF_REPO,
                    env::consts::DLL_SUFFIX,
                )
            {
                debug!("self looks like a dev build ({loaded_basename}); skipping auto-update");
                continue;
            }

            let (tag, published_at, mut release_in_hand) =
                match resolve_latest_release(owner, repo, sub, now, false).await {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("checking {owner}/{repo}: {e:#}");
                        print_async(format!(
                            "{}Failed to check {}{owner}/{repo}{}: {}{e}",
                            color::RED,
                            color::LIME,
                            color::RED,
                            color::WHITE,
                        ))
                        .await;
                        continue;
                    }
                };
            if release_in_hand.is_some() {
                new_tags.push((owner.clone(), repo.clone(), tag.clone(), published_at));
            }

            // Short-circuit when we already have this exact tag installed
            // and the file is on disk. Same-tag re-installs don't actually
            // swap code in-session anyway (versioned filename collides with
            // the currently-mapped one, so dlopen returns the cached
            // handle), and this dodges a wasted asset fetch. Self lives in
            // plugins/, managed in plugins/managed/.
            if sub.state.installed_version.as_deref() == Some(&tag)
                && let Some(asset_name) = sub.state.installed_asset.as_deref()
            {
                let dir = if config::is_self(owner, repo) {
                    PLUGINS_DIR
                } else {
                    MANAGED_DIR
                };
                if Path::new(dir).join(asset_name).exists() {
                    debug!("{owner}/{repo} already on {tag} with asset on disk; skipping");
                    continue;
                }
            }
            // Self-only: even if config didn't track installed_version (or
            // doesn't match the loaded file), refuse to "update" to a tag
            // whose versioned file already exists in plugins/. The on-disk
            // file is the source of truth for what'll be loaded next
            // restart.
            if config::is_self(owner, repo) {
                let target = installer::versioned_managed_filename(
                    config::SELF_OWNER,
                    config::SELF_REPO,
                    &tag,
                    env::consts::DLL_SUFFIX,
                );
                if Path::new(PLUGINS_DIR).join(&target).exists() {
                    debug!("self target {target} already on disk; skipping");
                    continue;
                }
            }

            if !needs_install(
                sub.state.installed_at,
                sub.state.installed_asset.as_deref(),
                published_at,
            ) {
                debug!("{owner}/{repo} up to date ({tag})");
                continue;
            }

            let release = match release_in_hand.take() {
                Some(r) => r,
                None => match get_release_for_channel(
                    owner,
                    repo,
                    &sub.channel,
                    sub.token.as_ref().map(Secret::expose),
                )
                .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        warn!("fetching release for {owner}/{repo}: {e:#}");
                        print_async(format!(
                            "{}Failed to fetch release for {}{owner}/{repo}{}: {}{e}",
                            color::RED,
                            color::LIME,
                            color::RED,
                            color::WHITE,
                        ))
                        .await;
                        continue;
                    }
                },
            };

            let asset = match pick_asset(
                &release.tag_name,
                &release.assets,
                env::consts::ARCH,
                env::consts::DLL_SUFFIX,
            ) {
                Ok(a) => a,
                Err(e) => {
                    warn!("asset match {owner}/{repo}: {e:#}");
                    print_async(format!(
                        "{}No suitable asset for {}{owner}/{repo}{}: {}{e}",
                        color::RED,
                        color::LIME,
                        color::RED,
                        color::WHITE,
                    ))
                    .await;
                    continue;
                }
            };

            print_async(format!(
                "{}Downloading {}{} {}for {}{owner}/{repo} {}({}{}{})",
                color::PINK,
                color::GREEN,
                release.tag_name,
                color::PINK,
                color::LIME,
                color::PINK,
                color::LIME,
                asset.name,
                color::PINK,
            ))
            .await;

            let expected_digest = match resolve_expected_digest(asset) {
                Ok(d) => d,
                Err(e) => {
                    warn!("digest resolve failed for {owner}/{repo}: {e:#}");
                    print_async(format!(
                        "{}Digest check failed for {}{owner}/{repo}{}: {}{e}",
                        color::RED,
                        color::LIME,
                        color::RED,
                        color::WHITE,
                    ))
                    .await;
                    continue;
                }
            };

            let is_self = config::is_self(owner, repo);
            let token = sub.token.as_ref().map(Secret::expose);
            // For self, capture the loaded basename BEFORE downloading - we
            // need to know what file to rename aside as `.old` after a new
            // versioned binary lands. current_lib_path() is unaffected by
            // the install; it tracks the in-process mapping, not the
            // dirent.
            let prev_self_basename = if is_self {
                current_lib_path()
                    .ok()
                    .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            } else {
                None
            };
            let install_result = if is_self {
                download_self(asset, expected_digest.as_deref(), token, &release.tag_name).await
            } else {
                download_to_managed_dir(
                    owner,
                    repo,
                    &release.tag_name,
                    asset,
                    expected_digest.as_deref(),
                    token,
                )
                .await
            };
            match install_result {
                Ok(path) => {
                    let installed_basename = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map_or_else(|| asset.name.clone(), str::to_owned);
                    if is_self
                        && let Some(prev) = prev_self_basename.as_deref()
                        && let Some(dir) = path.parent()
                    {
                        // Rename the previously-loaded self file aside so
                        // ClassiCube doesn't auto-load both copies on next
                        // launch. Best-effort: a Windows lock that blocks
                        // the rename leaves a `.old` for cleanup_self_old
                        // to reap on the *next* startup (current session
                        // still has the old code mapped).
                        mark_previous_self_aside(dir, prev, &installed_basename);
                    }
                    installed.push((
                        owner.clone(),
                        repo.clone(),
                        release.tag_name.clone(),
                        installed_basename,
                        release.published_at,
                    ));
                    if is_self {
                        print_async(format!(
                            "{}Plugin manager updated to {}{}{} - restart ClassiCube to use the \
                             new version",
                            color::PINK,
                            color::GREEN,
                            release.tag_name,
                            color::PINK,
                        ))
                        .await;
                    } else {
                        print_async(format!(
                            "{}Downloaded {}{} {}-> {}{}",
                            color::PINK,
                            color::GREEN,
                            release.tag_name,
                            color::PINK,
                            color::YELLOW,
                            path.display(),
                        ))
                        .await;
                    }
                }
                Err(e) => {
                    error!("installing {owner}/{repo}: {e:#}");
                    print_async(format!(
                        "{}Download failed for {}{owner}/{repo}{}: {}{e}",
                        color::RED,
                        color::LIME,
                        color::RED,
                        color::WHITE,
                    ))
                    .await;
                }
            }
        }
    }

    if !new_tags.is_empty()
        && let Err(e) = persist_cache_updates(now, new_tags)
    {
        warn!("saving config (cache update): {e:#}");
    }
    if !installed.is_empty()
        && let Err(e) = persist_installed_versions(now, installed)
    {
        warn!("saving config (installed versions): {e:#}");
    }

    Ok(())
}

/// Ensure the self subscription exists and stamp its stored install state to
/// the running binary, in a single `modify_at` so only one write is issued
/// per startup. `current_lib_path` failure is non-fatal: we still record the
/// baked-in version, just not the asset basename.
async fn ensure_self_subscription() {
    let want_version = config::self_installed_version();
    let want_asset = current_lib_path()
        .ok()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));

    let added = match Config::modify_at(config_path(), |cfg| {
        cfg.ensure_self(&want_version, want_asset.as_deref())
    }) {
        Ok(added) => added,
        Err(e) => {
            warn!("ensuring self subscription: {e:#}");
            return;
        }
    };
    if added {
        debug!("auto-added self subscription to config");
    }
    debug!(
        "reconciled self installed_version to {} (asset: {:?})",
        want_version, want_asset
    );
}

async fn run_reconcile_and_warn() {
    // Skip the running self binary in the plugins/ scan so we don't flag
    // ourselves as a conflict for the self subscription. Failure to resolve
    // the self path is non-fatal; in the rare case we can't, the worst
    // outcome is one extra warning the user can ignore.
    let self_basename = current_lib_path()
        .ok()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()));
    let report = match reconcile::reconcile(
        config_path(),
        Path::new(PLUGINS_DIR),
        Path::new(MANAGED_DIR),
        env::consts::DLL_SUFFIX,
        self_basename.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => {
            warn!("reconcile failed: {e:#}");
            print_async(format!(
                "{}Reconcile failed: {}{e}",
                color::RED,
                color::WHITE,
            ))
            .await;
            return;
        }
    };
    for missing in &report.missing {
        warn!(
            "missing managed file for {}/{}: {} (sub disabled)",
            missing.owner, missing.repo, missing.asset,
        );
        print_async(format!(
            "{}Missing {}{}{} for {}{}/{}{}: subscription disabled (edit toml to re-enable)",
            color::YELLOW,
            color::LIME,
            missing.asset,
            color::YELLOW,
            color::LIME,
            missing.owner,
            missing.repo,
            color::YELLOW,
        ))
        .await;
    }
    // Orphans and managed-dir conflicts both end up in
    // sweep_managed_orphans' delete list a few steps later; the sweep
    // emits a single consolidated `warn!` for whatever it actually
    // removed, which is the right place to surface the cleanup. No
    // chat for either - sweep is routine.
    for conflict in &report.conflicts {
        if matches!(conflict.dir, ConflictDir::Managed) {
            // Stray file in managed/ - loader only opens `installed_asset`,
            // so it's pure clutter; the sweep will remove it.
            continue;
        }
        // Plugins-dir conflict: ClassiCube auto-loads the user's file in
        // plugins/, so the loader skips the managed copy to keep only one
        // instance live. The user has to intervene; we don't touch
        // plugins/. This one stays a warn + chat because the sweep won't
        // clean it up.
        let claim = match &conflict.installed_asset {
            Some(a) => format!(" (managed file: {a})"),
            None => String::new(),
        };
        warn!(
            "conflict in {}: {} duplicates {}/{}{}",
            PLUGINS_DIR, conflict.filename, conflict.owner, conflict.repo, claim,
        );
        print_async(format!(
            "{}Conflict in {}{}{}: {}{}{} duplicates {}{}/{}{}{} - skipping the managed copy; \
             delete one to consolidate",
            color::YELLOW,
            color::LIME,
            PLUGINS_DIR,
            color::YELLOW,
            color::LIME,
            conflict.filename,
            color::YELLOW,
            color::LIME,
            conflict.owner,
            conflict.repo,
            color::YELLOW,
            claim,
        ))
        .await;
    }
}

/// Whether a subscription's on-disk install needs to be (re)written.
///
/// We compare the release's `published_at` against the saved `installed_at`
/// so that cosmetic tag differences (`v1.2.3` vs `1.2.3`) and ad-hoc tag
/// schemes (`nightly`, dated tags) just work, and so that a maintainer
/// retracting a release to an older version doesn't auto-downgrade us.
///
/// `installed_asset.is_none()` covers two cases: never-installed, and
/// installed-via-an-older-version-of-this-plugin (before the field existed).
/// Either way, we want to lay down the asset so the loader has a path to
/// `dlopen`.
fn needs_install(
    installed_at: Option<u64>,
    installed_asset: Option<&str>,
    latest_published_at: u64,
) -> bool {
    installed_asset.is_none() || installed_at.is_none_or(|t| latest_published_at > t)
}

pub(crate) async fn resolve_latest_release(
    owner: &str,
    repo: &str,
    sub: &Subscription,
    now: u64,
    force_refresh: bool,
) -> Result<(String, u64, Option<GitHubRelease>)> {
    if !force_refresh && let Some((tag, pub_at)) = sub.fresh_cached_release(now, TTL_SECS) {
        debug!("{owner}/{repo} served from cache ({tag})");
        return Ok((tag.to_owned(), pub_at, None));
    }
    let release = get_release_for_channel(
        owner,
        repo,
        &sub.channel,
        sub.token.as_ref().map(Secret::expose),
    )
    .await?;
    Ok((
        release.tag_name.clone(),
        release.published_at,
        Some(release),
    ))
}

pub(crate) fn persist_cache_updates(
    now: u64,
    updates: Vec<(String, String, String, u64)>,
) -> Result<()> {
    persist_cache_updates_to(config_path(), now, updates)
}

fn persist_cache_updates_to(
    path: &Path,
    now: u64,
    updates: Vec<(String, String, String, u64)>,
) -> Result<()> {
    // modify_at takes the process-wide CONFIG_LOCK and re-reads inside it,
    // so concurrent breadcrumb writes on the main thread can't be lost.
    Config::modify_at(path, |fresh| {
        for (owner, repo, tag, published_at) in updates {
            if let Some(sub) = fresh
                .subscriptions
                .get_mut(&owner)
                .and_then(|m| m.get_mut(&repo))
            {
                sub.state.cached_tag = Some(tag);
                sub.state.cached_at = Some(now);
                sub.state.cached_published_at = Some(published_at);
            }
        }
    })
}

// Tuple shape: (owner, repo, version, asset_filename, published_at).
pub fn persist_installed_versions(
    now: u64,
    updates: Vec<(String, String, String, String, u64)>,
) -> Result<()> {
    persist_installed_versions_to(config_path(), now, updates)
}

pub fn persist_installed_versions_to(
    path: &Path,
    now: u64,
    updates: Vec<(String, String, String, String, u64)>,
) -> Result<()> {
    Config::modify_at(path, |fresh| {
        for (owner, repo, version, asset, published_at) in updates {
            if let Some(sub) = fresh
                .subscriptions
                .get_mut(&owner)
                .and_then(|m| m.get_mut(&repo))
            {
                sub.state.installed_version = Some(version.clone());
                sub.state.installed_asset = Some(asset);
                sub.state.installed_at = Some(published_at);
                // Installing the version means whatever we just stored *is*
                // the up-to-date cached tag from the user's perspective.
                sub.state.cached_tag = Some(version);
                sub.state.cached_at = Some(now);
                sub.state.cached_published_at = Some(published_at);
            }
        }
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
