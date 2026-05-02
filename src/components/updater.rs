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
    chat::print_async,
    component::Component,
    config::{self, Config, Subscription},
    github_release,
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
            if let Err(e) = check_subscriptions().await {
                error!("update check failed: {e:#}");
                print_async(format!(
                    "{}Plugin update check failed: {}{e}",
                    color::RED,
                    color::WHITE,
                ))
                .await;
            }
        });
    }
}

async fn check_subscriptions() -> Result<()> {
    let subs = Config::load()?.subscriptions;
    if subs.is_empty() {
        info!("no subscriptions; skipping update check");
        return Ok(());
    }

    let now = unix_now();
    let mut new_tags: Vec<(String, String, String)> = Vec::new();

    for sub in &subs {
        if sub.disabled {
            debug!("{}/{} disabled; skipping check", sub.owner, sub.repo);
            continue;
        }

        let cached = sub.fresh_cached_tag(now, TTL_SECS).map(str::to_owned);

        let tag = match cached {
            Some(t) => {
                debug!("{}/{} served from cache ({t})", sub.owner, sub.repo);
                t
            }
            None => match github_release::get_latest_release(&sub.owner, &sub.repo).await {
                Ok(release) => {
                    new_tags.push((
                        sub.owner.clone(),
                        sub.repo.clone(),
                        release.tag_name.clone(),
                    ));
                    release.tag_name
                }
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
            },
        };

        notify(sub, &tag).await;
    }

    if !new_tags.is_empty()
        && let Err(e) = persist_cache_updates(now, new_tags)
    {
        warn!("saving config (cache update): {e:#}");
    }

    Ok(())
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

async fn notify(sub: &Subscription, latest: &str) {
    match &sub.installed_version {
        Some(installed) if installed == latest => {
            info!("{}/{} up to date ({latest})", sub.owner, sub.repo);
        }
        Some(installed) => {
            print_async(format!(
                "{}New release {}{} {}for {}{}/{} {}(installed: {}{}{})",
                color::PINK,
                color::GREEN,
                latest,
                color::PINK,
                color::LIME,
                sub.owner,
                sub.repo,
                color::PINK,
                color::YELLOW,
                installed,
                color::PINK,
            ))
            .await;
        }
        None => {
            print_async(format!(
                "{}Latest release {}{} {}for {}{}/{} {}(not installed via updater)",
                color::PINK,
                color::GREEN,
                latest,
                color::PINK,
                color::LIME,
                sub.owner,
                sub.repo,
                color::PINK,
            ))
            .await;
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
