#[cfg(test)]
mod tests;

use std::{
    cell::RefCell,
    os::raw::c_int,
    slice,
    time::{SystemTime, UNIX_EPOCH},
};

use classicube_helpers::{async_manager, color};
use classicube_sys::{OwnedChatCommand, cc_string};
use tracing::error;

use crate::{
    asset_match,
    chat::{print_async, print_wrapped},
    component::Component,
    components::updater::persist_installed_versions,
    config::{Config, Subscription},
    github_release, installer,
};

thread_local!(
    static COMMAND: RefCell<Option<OwnedChatCommand>> = const { RefCell::new(None) };
);

fn parse_owner_repo(s: &str) -> Option<(String, String)> {
    let (owner, repo) = s.split_once('/')?;
    if owner.is_empty()
        || repo.is_empty()
        || owner.contains(char::is_whitespace)
        || repo.contains(char::is_whitespace)
        || repo.contains('/')
    {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

const USAGE_LINES: &[&str] = &[
    "&a/client Updater subscribe <owner>/<repo>",
    "&a/client Updater unsubscribe <owner>/<repo>",
    "&a/client Updater disable <owner>/<repo>",
    "&a/client Updater enable <owner>/<repo>",
    "&a/client Updater list",
    "&a/client Updater update [<owner>/<repo>]",
];

fn print_usage() {
    print_wrapped(format!("{}Usage:", color::YELLOW));
    for line in USAGE_LINES {
        print_wrapped(*line);
    }
}

async fn print_load_error(e: &anyhow::Error) {
    error!("loading config: {e:#}");
    print_async(format!(
        "{}Refusing to modify config (load failed — fix plugins/plugin-updater.toml first): {}{e}",
        color::RED,
        color::WHITE,
    ))
    .await;
}

async fn print_save_error(e: &anyhow::Error) {
    error!("saving config: {e:#}");
    print_async(format!(
        "{}Failed to save config: {}{e}",
        color::RED,
        color::WHITE,
    ))
    .await;
}

fn handle_subscribe(spec: &str) {
    let Some((owner, repo)) = parse_owner_repo(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };

    async_manager::spawn(async move {
        let mut config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        let already = config
            .subscriptions
            .iter()
            .any(|s| s.owner.eq_ignore_ascii_case(&owner) && s.repo.eq_ignore_ascii_case(&repo));
        if already {
            print_async(format!(
                "{}Already subscribed to {}{}/{}",
                color::YELLOW,
                color::LIME,
                owner,
                repo,
            ))
            .await;
            return;
        }

        config.subscriptions.push(Subscription {
            owner: owner.clone(),
            repo: repo.clone(),
            disabled: false,
            installed_version: None,
            installed_asset: None,
            installed_at: None,
            cached_tag: None,
            cached_at: None,
            cached_published_at: None,
        });
        if let Err(e) = config.save() {
            print_save_error(&e).await;
            return;
        }
        print_async(format!(
            "{}Subscribed to {}{}/{}",
            color::PINK,
            color::LIME,
            owner,
            repo,
        ))
        .await;
    });
}

fn handle_unsubscribe(spec: &str) {
    let Some((owner, repo)) = parse_owner_repo(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };

    async_manager::spawn(async move {
        let mut config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        let before = config.subscriptions.len();
        config.subscriptions.retain(|s| {
            !(s.owner.eq_ignore_ascii_case(&owner) && s.repo.eq_ignore_ascii_case(&repo))
        });
        if config.subscriptions.len() == before {
            print_async(format!(
                "{}Not subscribed to {}{}/{}",
                color::YELLOW,
                color::LIME,
                owner,
                repo,
            ))
            .await;
            return;
        }

        if let Err(e) = config.save() {
            print_save_error(&e).await;
            return;
        }
        print_async(format!(
            "{}Unsubscribed from {}{}/{}",
            color::PINK,
            color::LIME,
            owner,
            repo,
        ))
        .await;
    });
}

fn set_disabled(spec: &str, disabled: bool) {
    let Some((owner, repo)) = parse_owner_repo(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };

    async_manager::spawn(async move {
        let mut config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        let Some(sub) = config
            .subscriptions
            .iter_mut()
            .find(|s| s.owner.eq_ignore_ascii_case(&owner) && s.repo.eq_ignore_ascii_case(&repo))
        else {
            print_async(format!(
                "{}Not subscribed to {}{}/{}",
                color::YELLOW,
                color::LIME,
                owner,
                repo,
            ))
            .await;
            return;
        };

        if sub.disabled == disabled {
            let word = if disabled { "disabled" } else { "enabled" };
            print_async(format!(
                "{}Already {word} {}{}/{}",
                color::YELLOW,
                color::LIME,
                owner,
                repo,
            ))
            .await;
            return;
        }
        sub.disabled = disabled;

        if let Err(e) = config.save() {
            print_save_error(&e).await;
            return;
        }
        let word = if disabled { "Disabled" } else { "Enabled" };
        print_async(format!(
            "{}{word} {}{}/{}",
            color::PINK,
            color::LIME,
            owner,
            repo,
        ))
        .await;
    });
}

fn handle_list() {
    async_manager::spawn(async move {
        let config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                error!("loading config: {e:#}");
                print_async(format!(
                    "{}Failed to load config: {}{e}",
                    color::RED,
                    color::WHITE,
                ))
                .await;
                return;
            }
        };

        if config.subscriptions.is_empty() {
            print_async(format!("{}No subscriptions", color::YELLOW)).await;
            return;
        }
        print_async(format!(
            "{}Subscriptions ({}):",
            color::PINK,
            config.subscriptions.len()
        ))
        .await;
        for sub in &config.subscriptions {
            let suffix = if sub.disabled {
                format!(" {}[disabled]", color::RED)
            } else {
                String::new()
            };
            let line = match &sub.installed_version {
                Some(v) => format!(
                    "  {}{}/{} {}(installed: {}{}{}){suffix}",
                    color::LIME,
                    sub.owner,
                    sub.repo,
                    color::PINK,
                    color::YELLOW,
                    v,
                    color::PINK,
                ),
                None => format!("  {}{}/{}{suffix}", color::LIME, sub.owner, sub.repo,),
            };
            print_async(line).await;
        }
    });
}

fn handle_update_one(spec: &str) {
    let Some((owner, repo)) = parse_owner_repo(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };

    async_manager::spawn(async move {
        let config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        let Some(sub) = config
            .subscriptions
            .iter()
            .find(|s| s.owner.eq_ignore_ascii_case(&owner) && s.repo.eq_ignore_ascii_case(&repo))
        else {
            print_async(format!(
                "{}Not subscribed to {}{}/{}{}; use {}subscribe{} first",
                color::YELLOW,
                color::LIME,
                owner,
                repo,
                color::YELLOW,
                color::LIME,
                color::YELLOW,
            ))
            .await;
            return;
        };

        if sub.disabled {
            print_async(format!(
                "{}Subscription {}{}/{} {}is disabled; use {}enable {}/{}{} first",
                color::YELLOW,
                color::LIME,
                owner,
                repo,
                color::YELLOW,
                color::LIME,
                owner,
                repo,
                color::YELLOW,
            ))
            .await;
            return;
        }

        spawn_update_task(owner, repo);
    });
}

fn handle_update_all() {
    async_manager::spawn(async move {
        let config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        let stale: Vec<(String, String)> = config
            .subscriptions
            .iter()
            .filter(|s| !s.disabled)
            .filter(|s| match (s.installed_at, s.cached_published_at) {
                (Some(installed_at), Some(latest_pub_at)) => latest_pub_at > installed_at,
                _ => true,
            })
            .map(|s| (s.owner.clone(), s.repo.clone()))
            .collect();

        if stale.is_empty() {
            print_async(format!("{}Nothing to update", color::YELLOW)).await;
            return;
        }

        print_async(format!(
            "{}Updating {}{}{} subscription(s)...",
            color::PINK,
            color::YELLOW,
            stale.len(),
            color::PINK,
        ))
        .await;
        for (owner, repo) in stale {
            spawn_update_task(owner, repo);
        }
    });
}

fn spawn_update_task(owner: String, repo: String) {
    async_manager::spawn(async move {
        if let Err(e) = run_update(&owner, &repo).await {
            error!("update {}/{}: {e:#}", owner, repo);
            print_async(format!(
                "{}Update {}{}/{}{} failed: {}{e}",
                color::RED,
                color::LIME,
                owner,
                repo,
                color::RED,
                color::WHITE,
            ))
            .await;
        }
    });
}

async fn run_update(owner: &str, repo: &str) -> anyhow::Result<()> {
    print_async(format!(
        "{}Checking {}{}/{}{} for latest release...",
        color::PINK,
        color::LIME,
        owner,
        repo,
        color::PINK,
    ))
    .await;

    let release = github_release::get_latest_release(owner, repo).await?;
    let asset = asset_match::pick_asset(
        &release.assets,
        std::env::consts::ARCH,
        std::env::consts::DLL_SUFFIX,
    )?;

    print_async(format!(
        "{}Downloading {}{} {}({}{}{}) ...",
        color::PINK,
        color::YELLOW,
        release.tag_name,
        color::PINK,
        color::LIME,
        asset.name,
        color::PINK,
    ))
    .await;

    let path = installer::download_to_managed_dir(asset).await?;

    let now = unix_now();
    persist_installed_versions(
        now,
        vec![(
            owner.to_owned(),
            repo.to_owned(),
            release.tag_name.clone(),
            asset.name.clone(),
            release.published_at,
        )],
    )?;

    print_async(format!(
        "{}Installed {}{} {}for {}{}/{} {}-> {}{}{} (restart to load)",
        color::PINK,
        color::GREEN,
        release.tag_name,
        color::PINK,
        color::LIME,
        owner,
        repo,
        color::PINK,
        color::YELLOW,
        path.display(),
        color::PINK,
    ))
    .await;

    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

extern "C" fn c_callback(args: *const cc_string, args_count: c_int) {
    let args = unsafe { slice::from_raw_parts(args, args_count as usize) };
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();

    match args.as_slice() {
        ["subscribe", spec] => handle_subscribe(spec),
        ["unsubscribe", spec] => handle_unsubscribe(spec),
        ["disable", spec] => set_disabled(spec, true),
        ["enable", spec] => set_disabled(spec, false),
        ["list"] => handle_list(),
        ["update"] => handle_update_all(),
        ["update", spec] => handle_update_one(spec),
        _ => print_usage(),
    }
}

#[derive(Default)]
pub struct Command;

impl Component for Command {
    fn name(&self) -> &'static str {
        "Command"
    }

    fn init(&mut self) {
        COMMAND.with(|cell| {
            let mut cmd = OwnedChatCommand::new("Updater", c_callback, false, USAGE_LINES.to_vec());
            cmd.register();
            *cell.borrow_mut() = Some(cmd);
        });
    }

    fn free(&mut self) {
        COMMAND.with(|cell| {
            cell.borrow_mut().take();
        });
    }
}
