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
    installer, loader,
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
    let subs = Config::load()?.subscriptions;
    if subs.is_empty() {
        info!("no subscriptions; skipping update check");
        return Ok(());
    }

    let now = unix_now();
    let mut new_tags: Vec<(String, String, String)> = Vec::new();
    let mut installed: Vec<(String, String, String, String)> = Vec::new();

    for sub in &subs {
        if sub.disabled {
            debug!("{}/{} disabled; skipping", sub.owner, sub.repo);
            continue;
        }

        let (tag, mut release_in_hand) = match resolve_latest_tag(sub, now).await {
            Ok(pair) => pair,
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
        if let Some(ref r) = release_in_hand {
            new_tags.push((sub.owner.clone(), sub.repo.clone(), r.tag_name.clone()));
        }

        if !needs_install(
            sub.installed_version.as_deref(),
            sub.installed_asset.as_deref(),
            &tag,
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

/// Whether a subscription's on-disk install needs to be (re)written.
///
/// `installed_asset.is_none()` covers two cases: never-installed, and
/// installed-via-an-older-version-of-this-plugin (before the field existed).
/// Either way, we want to lay down the asset so the loader has a path to
/// `dlopen`.
fn needs_install(
    installed_version: Option<&str>,
    installed_asset: Option<&str>,
    latest_tag: &str,
) -> bool {
    installed_version != Some(latest_tag) || installed_asset.is_none()
}

async fn resolve_latest_tag(
    sub: &Subscription,
    now: u64,
) -> Result<(String, Option<GitHubRelease>)> {
    if let Some(t) = sub.fresh_cached_tag(now, TTL_SECS) {
        debug!("{}/{} served from cache ({t})", sub.owner, sub.repo);
        return Ok((t.to_owned(), None));
    }
    let release = github_release::get_latest_release(&sub.owner, &sub.repo).await?;
    Ok((release.tag_name.clone(), Some(release)))
}

fn persist_cache_updates(now: u64, updates: Vec<(String, String, String)>) -> Result<()> {
    persist_cache_updates_to(config::config_path(), now, updates)
}

fn persist_cache_updates_to(
    path: &Path,
    now: u64,
    updates: Vec<(String, String, String)>,
) -> Result<()> {
    // Re-read so we don't clobber concurrent /subscribe edits made on the
    // game thread while HTTP was in flight.
    let mut fresh = Config::load_from(path)?;
    for (owner, repo, tag) in updates {
        if let Some(sub) = fresh
            .subscriptions
            .iter_mut()
            .find(|s| s.owner == owner && s.repo == repo)
        {
            sub.cached_tag = Some(tag);
            sub.cached_at = Some(now);
        }
    }
    fresh.save_to(path)
}

// Tuple shape: (owner, repo, version, asset_filename).
pub fn persist_installed_versions(
    now: u64,
    updates: Vec<(String, String, String, String)>,
) -> Result<()> {
    persist_installed_versions_to(config::config_path(), now, updates)
}

pub fn persist_installed_versions_to(
    path: &Path,
    now: u64,
    updates: Vec<(String, String, String, String)>,
) -> Result<()> {
    let mut fresh = Config::load_from(path)?;
    for (owner, repo, version, asset) in updates {
        if let Some(sub) = fresh
            .subscriptions
            .iter_mut()
            .find(|s| s.owner == owner && s.repo == repo)
        {
            sub.installed_version = Some(version.clone());
            sub.installed_asset = Some(asset);
            // Installing the version means whatever we just stored *is* the
            // up-to-date cached tag from the user's perspective.
            sub.cached_tag = Some(version);
            sub.cached_at = Some(now);
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
