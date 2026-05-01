use std::cell::Cell;

use anyhow::Result;
use classicube_helpers::{async_manager, color};
use tracing::{error, info, warn};

use crate::{
    chat::print_async,
    component::Component,
    config::{Config, Subscription},
    github_release,
};

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

    for sub in &subs {
        if let Err(e) = check_one(sub).await {
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
        }
    }

    Ok(())
}

async fn check_one(sub: &Subscription) -> Result<()> {
    let release = github_release::get_latest_release(&sub.owner, &sub.repo).await?;
    let latest = &release.tag_name;

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
    Ok(())
}
