#[cfg(test)]
mod tests;

use std::{
    cell::RefCell,
    env,
    os::raw::c_int,
    slice,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Error, Result, bail};
use classicube_helpers::{async_manager, color};
use classicube_sys::{OwnedChatCommand, cc_string};
use tracing::error;

use crate::{
    asset_match::pick_asset,
    chat::{print_async, print_wrapped},
    component::Component,
    components::updater::persist_installed_versions,
    config::{Channel, Config, Subscription},
    github_release::{get_release_for_channel, resolve_expected_digest},
    installer::download_to_managed_dir,
};

thread_local!(
    static COMMAND: RefCell<Option<OwnedChatCommand>> = const { RefCell::new(None) };
);

/// Default owner used when a shorthand has no `owner/` prefix. SpiralP owns
/// most ClassiCube plugins and follows the `classicube-$name-plugin` naming
/// convention, so `/subscribe foo` resolves to `SpiralP/classicube-foo-plugin`.
const DEFAULT_OWNER: &str = "SpiralP";

/// Expand user-typed shorthand into ordered `(owner, repo)` candidates to try.
/// The literal interpretation comes first; the `classicube-$name-plugin`
/// expansion comes second when the input doesn't already look canonical.
///
/// - `foo`                            → [(SpiralP, foo), (SpiralP, classicube-foo-plugin)]
/// - `owner/foo`                      → [(owner, foo), (owner, classicube-foo-plugin)]
/// - `owner/classicube-foo-plugin`    → [(owner, classicube-foo-plugin)]
/// - `classicube-foo-plugin`          → [(SpiralP, classicube-foo-plugin)]
fn expand_candidates(input: &str) -> Option<Vec<(String, String)>> {
    let (owner, repo) = match input.split_once('/') {
        Some((o, r)) => (o, r),
        None => (DEFAULT_OWNER, input),
    };
    if owner.is_empty()
        || repo.is_empty()
        || owner.contains(char::is_whitespace)
        || repo.contains(char::is_whitespace)
        || repo.contains('/')
    {
        return None;
    }
    let owner = owner.to_string();
    let repo = repo.to_string();
    if is_canonical_repo_name(&repo) {
        Some(vec![(owner, repo)])
    } else {
        let expanded = format!("classicube-{repo}-plugin");
        Some(vec![(owner.clone(), repo), (owner, expanded)])
    }
}

fn is_canonical_repo_name(repo: &str) -> bool {
    repo.strip_prefix("classicube-")
        .and_then(|r| r.strip_suffix("-plugin"))
        .is_some_and(|middle| !middle.is_empty())
}

/// Find the index of the first subscription that matches any candidate,
/// preferring earlier candidates (literal before expanded). Comparison is
/// case-insensitive, mirroring how `handle_subscribe` checks for duplicates.
fn find_subscription_index(config: &Config, candidates: &[(String, String)]) -> Option<usize> {
    for (owner, repo) in candidates {
        if let Some(idx) = config
            .subscriptions
            .iter()
            .position(|s| s.owner.eq_ignore_ascii_case(owner) && s.repo.eq_ignore_ascii_case(repo))
        {
            return Some(idx);
        }
    }
    None
}

const USAGE_LINES: &[&str] = &[
    "&a/client Updater subscribe <owner>/<repo> [stable|prerelease|tag <ref>]",
    "&a/client Updater unsubscribe <owner>/<repo>",
    "&a/client Updater channel <owner>/<repo> stable|prerelease|tag <ref>",
    "&a/client Updater disable <owner>/<repo>",
    "&a/client Updater enable <owner>/<repo>",
    "&a/client Updater list",
    "&a/client Updater update [<owner>/<repo>]",
];

/// Parse the trailing channel arguments after `<owner>/<repo>`.
///
/// Accepted forms (CLI):
/// - `[]`            → `Channel::Stable` (default for `/subscribe`)
/// - `["stable"]`    → `Channel::Stable`
/// - `["prerelease"]`→ `Channel::Prerelease`
/// - `["tag", ref]`  → `Channel::Tag(ref)` (preferred CLI form)
/// - `["tag:ref"]`   → `Channel::Tag(ref)` (TOML form, also accepted)
fn parse_channel_args(args: &[&str]) -> Result<Channel, String> {
    match args {
        [] => Ok(Channel::Stable),
        ["tag", t] => Channel::from_tag(t),
        [single] => single.parse(),
        _ => Err(format!(
            "expected stable, prerelease, or tag <ref>; got: {}",
            args.join(" ")
        )),
    }
}

/// Switch a subscription to a new channel and invalidate its cached release
/// fields. The cache lives per-subscription, so without clearing it a stale
/// stable lookup could mask a prerelease (or vice-versa) until the TTL
/// expires. Installed-state fields (`installed_*`) are deliberately untouched
/// — those describe what's on disk, not what's on GitHub.
fn apply_channel_switch(sub: &mut Subscription, new: Channel) {
    sub.channel = new;
    sub.cached_tag = None;
    sub.cached_at = None;
    sub.cached_published_at = None;
}

/// Suffix to append after `owner/repo` in chat output when the channel is
/// non-default. Returns an empty string for `Channel::Stable`.
fn channel_suffix(channel: &Channel) -> String {
    if channel.is_default() {
        String::new()
    } else {
        format!(" {}({})", color::PINK, channel.pretty())
    }
}

fn print_usage() {
    print_wrapped(format!("{}Usage:", color::YELLOW));
    for line in USAGE_LINES {
        print_wrapped(*line);
    }
}

async fn print_load_error(e: &Error) {
    error!("loading config: {e:#}");
    print_async(format!(
        "{}Refusing to modify config (load failed — fix plugins/plugin-updater.toml first): {}{e}",
        color::RED,
        color::WHITE,
    ))
    .await;
}

async fn print_save_error(e: &Error) {
    error!("saving config: {e:#}");
    print_async(format!(
        "{}Failed to save config: {}{e}",
        color::RED,
        color::WHITE,
    ))
    .await;
}

fn handle_subscribe(spec: &str, channel: Channel) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        let mut config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        if let Some(idx) = find_subscription_index(&config, &candidates) {
            let existing = &config.subscriptions[idx];
            print_async(format!(
                "{}Already subscribed to {}{}/{} {}(use {}/client Updater channel{} to switch \
                 channels)",
                color::YELLOW,
                color::LIME,
                existing.owner,
                existing.repo,
                color::YELLOW,
                color::LIME,
                color::YELLOW,
            ))
            .await;
            return;
        }

        // Single canonical candidate: skip the network probe to preserve the
        // fast subscribe path. Multiple candidates: probe each against the
        // GitHub API + OS asset filter and persist the first that succeeds.
        let (owner, repo) = if candidates.len() == 1 {
            candidates.into_iter().next().unwrap()
        } else {
            match resolve_canonical(&candidates, &channel).await {
                Ok(pair) => pair,
                Err(e) => {
                    print_async(format!(
                        "{}Failed to resolve {}{}{}: {}{e}",
                        color::RED,
                        color::LIME,
                        spec,
                        color::RED,
                        color::WHITE,
                    ))
                    .await;
                    return;
                }
            }
        };

        config.subscriptions.push(Subscription {
            owner: owner.clone(),
            repo: repo.clone(),
            channel: channel.clone(),
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
            "{}Subscribed to {}{}/{}{}",
            color::PINK,
            color::LIME,
            owner,
            repo,
            channel_suffix(&channel),
        ))
        .await;
    });
}

/// Probe each candidate against the GitHub API and OS asset filter; return
/// the first `(owner, repo)` whose release for `channel` has a matching
/// asset for our platform. Errors aggregate the per-candidate failure
/// messages.
async fn resolve_canonical(
    candidates: &[(String, String)],
    channel: &Channel,
) -> Result<(String, String)> {
    let mut errors: Vec<String> = Vec::new();
    for (owner, repo) in candidates {
        match probe_release(owner, repo, channel).await {
            Ok(()) => return Ok((owner.clone(), repo.clone())),
            Err(e) => errors.push(format!("{owner}/{repo}: {e}")),
        }
    }
    bail!("{}", errors.join("; "));
}

async fn probe_release(owner: &str, repo: &str, channel: &Channel) -> Result<()> {
    let release = get_release_for_channel(owner, repo, channel).await?;
    pick_asset(&release.assets, env::consts::ARCH, env::consts::DLL_SUFFIX)?;
    Ok(())
}

fn handle_unsubscribe(spec: &str) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        let mut config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        let Some(idx) = find_subscription_index(&config, &candidates) else {
            print_async(format!(
                "{}Not subscribed to {}{}",
                color::YELLOW,
                color::LIME,
                spec,
            ))
            .await;
            return;
        };

        let removed = config.subscriptions.remove(idx);
        if let Err(e) = config.save() {
            print_save_error(&e).await;
            return;
        }
        print_async(format!(
            "{}Unsubscribed from {}{}/{}",
            color::PINK,
            color::LIME,
            removed.owner,
            removed.repo,
        ))
        .await;
    });
}

fn handle_channel(spec: &str, channel: Channel) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        let mut config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        let Some(idx) = find_subscription_index(&config, &candidates) else {
            print_async(format!(
                "{}Not subscribed to {}{}",
                color::YELLOW,
                color::LIME,
                spec,
            ))
            .await;
            return;
        };

        let sub = &mut config.subscriptions[idx];
        if sub.channel == channel {
            print_async(format!(
                "{}{}/{} {}already on channel {}{}",
                color::LIME,
                sub.owner,
                sub.repo,
                color::YELLOW,
                color::PINK,
                channel.pretty(),
            ))
            .await;
            return;
        }
        apply_channel_switch(sub, channel.clone());
        let owner = sub.owner.clone();
        let repo = sub.repo.clone();

        if let Err(e) = config.save() {
            print_save_error(&e).await;
            return;
        }
        print_async(format!(
            "{}Channel for {}{}/{} {}set to {}{}",
            color::PINK,
            color::LIME,
            owner,
            repo,
            color::PINK,
            color::YELLOW,
            channel.pretty(),
        ))
        .await;
    });
}

fn set_disabled(spec: &str, disabled: bool) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        let mut config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        let Some(idx) = find_subscription_index(&config, &candidates) else {
            print_async(format!(
                "{}Not subscribed to {}{}",
                color::YELLOW,
                color::LIME,
                spec,
            ))
            .await;
            return;
        };

        let sub = &mut config.subscriptions[idx];
        if sub.disabled == disabled {
            let word = if disabled { "disabled" } else { "enabled" };
            print_async(format!(
                "{}Already {word} {}{}/{}",
                color::YELLOW,
                color::LIME,
                sub.owner,
                sub.repo,
            ))
            .await;
            return;
        }
        sub.disabled = disabled;
        let owner = sub.owner.clone();
        let repo = sub.repo.clone();

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
            let mut line = format!("  {}{}/{}", color::LIME, sub.owner, sub.repo);
            if let Some(v) = &sub.installed_version {
                line.push_str(&format!(
                    " {}(installed: {}{}{})",
                    color::PINK,
                    color::YELLOW,
                    v,
                    color::PINK,
                ));
            }
            line.push_str(&channel_suffix(&sub.channel));
            if sub.disabled {
                line.push_str(&format!(" {}[disabled]", color::RED));
            }
            print_async(line).await;
        }
    });
}

fn handle_update_one(spec: &str) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        let config = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                print_load_error(&e).await;
                return;
            }
        };

        let Some(idx) = find_subscription_index(&config, &candidates) else {
            print_async(format!(
                "{}Not subscribed to {}{}{}; use {}subscribe{} first",
                color::YELLOW,
                color::LIME,
                spec,
                color::YELLOW,
                color::LIME,
                color::YELLOW,
            ))
            .await;
            return;
        };

        let sub = &config.subscriptions[idx];
        if sub.disabled {
            print_async(format!(
                "{}Subscription {}{}/{} {}is disabled; use {}enable {}/{}{} first",
                color::YELLOW,
                color::LIME,
                sub.owner,
                sub.repo,
                color::YELLOW,
                color::LIME,
                sub.owner,
                sub.repo,
                color::YELLOW,
            ))
            .await;
            return;
        }

        spawn_update_task(sub.owner.clone(), sub.repo.clone(), sub.channel.clone());
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

        let stale: Vec<(String, String, Channel)> = config
            .subscriptions
            .iter()
            .filter(|s| !s.disabled)
            .filter(|s| match (s.installed_at, s.cached_published_at) {
                (Some(installed_at), Some(latest_pub_at)) => latest_pub_at > installed_at,
                _ => true,
            })
            .map(|s| (s.owner.clone(), s.repo.clone(), s.channel.clone()))
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
        for (owner, repo, channel) in stale {
            spawn_update_task(owner, repo, channel);
        }
    });
}

fn spawn_update_task(owner: String, repo: String, channel: Channel) {
    async_manager::spawn(async move {
        if let Err(e) = run_update(&owner, &repo, &channel).await {
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

async fn run_update(owner: &str, repo: &str, channel: &Channel) -> Result<()> {
    print_async(format!(
        "{}Checking {}{}/{}{} for latest release...",
        color::PINK,
        color::LIME,
        owner,
        repo,
        color::PINK,
    ))
    .await;

    let release = get_release_for_channel(owner, repo, channel).await?;
    let asset = pick_asset(&release.assets, env::consts::ARCH, env::consts::DLL_SUFFIX)?;

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

    let expected_digest = resolve_expected_digest(asset)?;
    let path = download_to_managed_dir(asset, expected_digest.as_deref()).await?;

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
        ["subscribe", spec, channel_args @ ..] => match parse_channel_args(channel_args) {
            Ok(c) => handle_subscribe(spec, c),
            Err(e) => print_wrapped(format!("{}{e}", color::RED)),
        },
        ["unsubscribe", spec] => handle_unsubscribe(spec),
        ["channel", spec, channel_args @ ..] => {
            if channel_args.is_empty() {
                print_usage();
            } else {
                match parse_channel_args(channel_args) {
                    Ok(c) => handle_channel(spec, c),
                    Err(e) => print_wrapped(format!("{}{e}", color::RED)),
                }
            }
        }
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
