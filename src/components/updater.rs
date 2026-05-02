#[cfg(test)]
mod tests;

use std::{
    cell::Cell,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use classicube_helpers::{async_manager, color};
use tracing::{debug, error, info, warn};

use crate::{
    asset_match,
    chat::print_async,
    component::Component,
    config::{self, Config, Subscription},
    github_release::{self, GitHubRelease},
    installer, loader, reconcile,
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
            async_manager::spawn_on_main_thread(async move {
                match Config::load() {
                    Ok(cfg) => loader::init_managed(&cfg.subscriptions),
                    Err(e) => {
                        error!("loading config for managed-load: {e:#}");
                    }
                }
            });
        });
    }
}

async fn run_initial_pass() -> Result<()> {
    run_reconcile_and_warn().await;

    let subs = Config::load()?.subscriptions;
    if subs.is_empty() {
        info!("no subscriptions; skipping update check");
        return Ok(());
    }

    let now = unix_now();
    let mut new_tags: Vec<(String, String, String, u64)> = Vec::new();
    let mut installed: Vec<(String, String, String, String, u64)> = Vec::new();

    for sub in &subs {
        if sub.disabled {
            debug!("{}/{} disabled; skipping", sub.owner, sub.repo);
            continue;
        }

        let (tag, published_at, mut release_in_hand) = match resolve_latest_release(sub, now).await
        {
            Ok(t) => t,
            Err(e) => {
                warn!("checking {}/{}: {e:#}", sub.owner, sub.repo);
                print_async(format!(
                    "{}Failed to check {}{}/{}{}: {}{e}",
                    color::RED,
                    color::LIME,
                    sub.owner,
                    sub.repo,
                    color::RED,
                    color::WHITE,
                ))
                .await;
                continue;
            }
        };
        if release_in_hand.is_some() {
            new_tags.push((
                sub.owner.clone(),
                sub.repo.clone(),
                tag.clone(),
                published_at,
            ));
        }

        if !needs_install(
            sub.installed_at,
            sub.installed_asset.as_deref(),
            published_at,
        ) {
            debug!("{}/{} up to date ({tag})", sub.owner, sub.repo);
            continue;
        }

        let release = match release_in_hand.take() {
            Some(r) => r,
            None => match github_release::get_latest_release(&sub.owner, &sub.repo).await {
                Ok(r) => r,
                Err(e) => {
                    warn!("fetching release for {}/{}: {e:#}", sub.owner, sub.repo);
                    print_async(format!(
                        "{}Failed to fetch release for {}{}/{}{}: {}{e}",
                        color::RED,
                        color::LIME,
                        sub.owner,
                        sub.repo,
                        color::RED,
                        color::WHITE,
                    ))
                    .await;
                    continue;
                }
            },
        };

        let asset = match asset_match::pick_asset(
            &release.assets,
            std::env::consts::ARCH,
            std::env::consts::DLL_SUFFIX,
        ) {
            Ok(a) => a,
            Err(e) => {
                warn!("asset match {}/{}: {e:#}", sub.owner, sub.repo);
                print_async(format!(
                    "{}No suitable asset for {}{}/{}{}: {}{e}",
                    color::RED,
                    color::LIME,
                    sub.owner,
                    sub.repo,
                    color::RED,
                    color::WHITE,
                ))
                .await;
                continue;
            }
        };

        print_async(format!(
            "{}Installing {}{} {}for {}{}/{} {}({}{}{})",
            color::PINK,
            color::GREEN,
            release.tag_name,
            color::PINK,
            color::LIME,
            sub.owner,
            sub.repo,
            color::PINK,
            color::LIME,
            asset.name,
            color::PINK,
        ))
        .await;

        match installer::download_to_managed_dir(asset).await {
            Ok(path) => {
                installed.push((
                    sub.owner.clone(),
                    sub.repo.clone(),
                    release.tag_name.clone(),
                    asset.name.clone(),
                    release.published_at,
                ));
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
            Err(e) => {
                error!("installing {}/{}: {e:#}", sub.owner, sub.repo);
                print_async(format!(
                    "{}Install failed for {}{}/{}{}: {}{e}",
                    color::RED,
                    color::LIME,
                    sub.owner,
                    sub.repo,
                    color::RED,
                    color::WHITE,
                ))
                .await;
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

async fn run_reconcile_and_warn() {
    let report =
        match reconcile::reconcile(config::config_path(), Path::new(installer::MANAGED_DIR)) {
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
        warn!("orphan in {}: {name}", installer::MANAGED_DIR);
        print_async(format!(
            "{}Orphan in {}{}{}: {}{}{} (no subscription claims this)",
            color::YELLOW,
            color::LIME,
            installer::MANAGED_DIR,
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

async fn resolve_latest_release(
    sub: &Subscription,
    now: u64,
) -> Result<(String, u64, Option<GitHubRelease>)> {
    if let Some((tag, pub_at)) = sub.fresh_cached_release(now, TTL_SECS) {
        debug!("{}/{} served from cache ({tag})", sub.owner, sub.repo);
        return Ok((tag.to_owned(), pub_at, None));
    }
    let release = github_release::get_latest_release(&sub.owner, &sub.repo).await?;
    Ok((
        release.tag_name.clone(),
        release.published_at,
        Some(release),
    ))
}

fn persist_cache_updates(now: u64, updates: Vec<(String, String, String, u64)>) -> Result<()> {
    persist_cache_updates_to(config::config_path(), now, updates)
}

fn persist_cache_updates_to(
    path: &Path,
    now: u64,
    updates: Vec<(String, String, String, u64)>,
) -> Result<()> {
    // Re-read so we don't clobber concurrent /subscribe edits made on the
    // game thread while HTTP was in flight.
    let mut fresh = Config::load_from(path)?;
    for (owner, repo, tag, published_at) in updates {
        if let Some(sub) = fresh
            .subscriptions
            .iter_mut()
            .find(|s| s.owner == owner && s.repo == repo)
        {
            sub.cached_tag = Some(tag);
            sub.cached_at = Some(now);
            sub.cached_published_at = Some(published_at);
        }
    }
    fresh.save_to(path)
}

// Tuple shape: (owner, repo, version, asset_filename, published_at).
pub fn persist_installed_versions(
    now: u64,
    updates: Vec<(String, String, String, String, u64)>,
) -> Result<()> {
    persist_installed_versions_to(config::config_path(), now, updates)
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
            .iter_mut()
            .find(|s| s.owner == owner && s.repo == repo)
        {
            sub.installed_version = Some(version.clone());
            sub.installed_asset = Some(asset);
            sub.installed_at = Some(published_at);
            // Installing the version means whatever we just stored *is* the
            // up-to-date cached tag from the user's perspective.
            sub.cached_tag = Some(version);
            sub.cached_at = Some(now);
            sub.cached_published_at = Some(published_at);
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
