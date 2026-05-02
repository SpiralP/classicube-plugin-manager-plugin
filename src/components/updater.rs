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

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;

    fn sub(owner: &str, repo: &str) -> Subscription {
        Subscription {
            owner: owner.into(),
            repo: repo.into(),
            installed_version: None,
            cached_tag: None,
            cached_at: None,
        }
    }

    #[test]
    fn updates_targeted_subscription_only() {
        let cfg = Config {
            subscriptions: vec![sub("alice", "one"), sub("bob", "two")],
        };
        let f = NamedTempFile::new().unwrap();
        cfg.save_to(f.path()).unwrap();

        persist_cache_updates_to(
            f.path(),
            12_345,
            vec![("alice".into(), "one".into(), "v9.9.9".into())],
        )
        .unwrap();

        let loaded = Config::load_from(f.path()).unwrap();
        let alice = &loaded.subscriptions[0];
        let bob = &loaded.subscriptions[1];
        assert_eq!(alice.cached_tag.as_deref(), Some("v9.9.9"));
        assert_eq!(alice.cached_at, Some(12_345));
        assert!(bob.cached_tag.is_none());
        assert!(bob.cached_at.is_none());
    }

    #[test]
    fn unknown_owner_repo_silently_skipped() {
        let cfg = Config {
            subscriptions: vec![sub("alice", "one")],
        };
        let f = NamedTempFile::new().unwrap();
        cfg.save_to(f.path()).unwrap();

        persist_cache_updates_to(
            f.path(),
            42,
            vec![("ghost".into(), "missing".into(), "v0.0.1".into())],
        )
        .unwrap();

        let loaded = Config::load_from(f.path()).unwrap();
        assert_eq!(loaded.subscriptions.len(), 1);
        assert!(loaded.subscriptions[0].cached_tag.is_none());
        assert!(loaded.subscriptions[0].cached_at.is_none());
    }

    #[test]
    fn missing_config_file_writes_empty_default() {
        // load() returns default on NotFound, so the update is applied to an
        // empty subs list — every entry takes the unknown-row branch — and
        // save() writes back the empty default.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        persist_cache_updates_to(
            &path,
            7,
            vec![("alice".into(), "one".into(), "v1.0.0".into())],
        )
        .unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded, Config::default());
    }
}
