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
    asset_match::pick_asset,
    chat::print_async,
    component::Component,
    config::{self, Config, Subscription, config_path},
    github_release::{GitHubRelease, get_release_for_channel, resolve_expected_digest},
    installer::{MANAGED_DIR, cleanup_self_old, download_self, download_to_managed_dir},
    loader::init_managed,
    reconcile,
    secret::Secret,
};

const TTL_SECS: u64 = 60 * 60;

thread_local!(
    static CHECKED: Cell<bool> = const { Cell::new(false) };
);

#[derive(Default)]
pub struct Updater;

impl Component for Updater {
    fn name(&self) -> &'static str {
        "Updater"
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
                    "{}Plugin updater pass failed: {}{e}",
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
            let subs: Vec<(String, String, Subscription)> = match Config::load() {
                Ok(cfg) => cfg
                    .subscriptions
                    .into_iter()
                    .flat_map(|(owner, repos)| {
                        repos
                            .into_iter()
                            .map(move |(repo, sub)| (owner.clone(), repo, sub))
                    })
                    .collect(),
                Err(e) => {
                    error!("loading config for managed-load: {e:#}");
                    return;
                }
            };
            async_manager::spawn_on_main_thread(async move {
                init_managed(&subs);
            });
        });
    }
}

async fn run_initial_pass() -> Result<()> {
    cleanup_self_old();
    ensure_self_subscription().await;
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

            let asset =
                match pick_asset(&release.assets, env::consts::ARCH, env::consts::DLL_SUFFIX) {
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
                "{}Installing {}{} {}for {}{owner}/{repo} {}({}{}{})",
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
            let install_result = if is_self {
                download_self(asset, expected_digest.as_deref(), token).await
            } else {
                download_to_managed_dir(asset, expected_digest.as_deref(), token).await
            };
            match install_result {
                Ok(path) => {
                    installed.push((
                        owner.clone(),
                        repo.clone(),
                        release.tag_name.clone(),
                        asset.name.clone(),
                        release.published_at,
                    ));
                    if is_self {
                        print_async(format!(
                            "{}Plugin updater updated to {}{}{} - restart ClassiCube to use the \
                             new version",
                            color::PINK,
                            color::GREEN,
                            release.tag_name,
                            color::PINK,
                        ))
                        .await;
                    } else {
                        print_async(format!(
                            "{}Installed {}{} {}-> {}{}",
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
                        "{}Install failed for {}{owner}/{repo}{}: {}{e}",
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

async fn ensure_self_subscription() {
    let mut cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            warn!("loading config to ensure self subscription: {e:#}");
            return;
        }
    };
    if !cfg.ensure_self() {
        return;
    }
    if let Err(e) = cfg.save() {
        warn!("saving config after ensure_self: {e:#}");
    } else {
        debug!("auto-added self subscription to config");
    }
}

async fn run_reconcile_and_warn() {
    let report = match reconcile::reconcile(config_path(), Path::new(MANAGED_DIR)) {
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
    for name in &report.orphans {
        warn!("orphan in {}: {name}", MANAGED_DIR);
        print_async(format!(
            "{}Orphan in {}{}{}: {}{}{} (no subscription claims this)",
            color::YELLOW,
            color::LIME,
            MANAGED_DIR,
            color::YELLOW,
            color::LIME,
            name,
            color::YELLOW,
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
    // Re-read so we don't clobber concurrent /add edits made on the
    // game thread while HTTP was in flight.
    let mut fresh = Config::load_from(path)?;
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
    fresh.save_to(path)
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
    let mut fresh = Config::load_from(path)?;
    for (owner, repo, version, asset, published_at) in updates {
        if let Some(sub) = fresh
            .subscriptions
            .get_mut(&owner)
            .and_then(|m| m.get_mut(&repo))
        {
            sub.state.installed_version = Some(version.clone());
            sub.state.installed_asset = Some(asset);
            sub.state.installed_at = Some(published_at);
            // Installing the version means whatever we just stored *is* the
            // up-to-date cached tag from the user's perspective.
            sub.state.cached_tag = Some(version);
            sub.state.cached_at = Some(now);
            sub.state.cached_published_at = Some(published_at);
        }
    }
    fresh.save_to(path)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
