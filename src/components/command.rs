#[cfg(test)]
mod tests;

use std::{
    cell::RefCell,
    collections::BTreeMap,
    env, fs, io,
    os::raw::c_int,
    path::{Path, PathBuf},
    slice,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Error, Result, bail};
use classicube_helpers::{async_manager, color};
use classicube_sys::{OwnedChatCommand, cc_string};
use tracing::{debug, error, warn};

use crate::{
    asset_match::{self, pick_asset},
    chat::{print_async, print_wrapped},
    component::Component,
    components::manager::{
        persist_cache_updates, persist_installed_versions, resolve_latest_release,
    },
    config::{self, Channel, Config, Subscription, SubscriptionState, config_path},
    discover,
    github_release::{GitHubRelease, get_release_for_channel, resolve_expected_digest},
    installer::{
        self, MANAGED_DIR, PLUGINS_DIR, cleanup_previous_managed, download_self,
        download_to_managed_dir, mark_previous_self_aside,
    },
    loader::{self, LifecyclePhase, LoadOutcome, UnloadOutcome},
    reconcile,
    secret::Secret,
    self_path::current_lib_path,
};

thread_local!(
    static COMMAND: RefCell<Option<OwnedChatCommand>> = const { RefCell::new(None) };
);

/// Default owner used when a shorthand has no `owner/` prefix. SpiralP owns
/// most ClassiCube plugins and follows the `classicube-$name-plugin` naming
/// convention, so `/add foo` resolves to `SpiralP/classicube-foo-plugin`.
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
    // Curated shorthand wins over the generic `classicube-$name-plugin`
    // expansion: bare input only (no slash) — owner-prefixed input always
    // means "I know what I want" and skips the curated lookup. A hit returns
    // a single canonical candidate, so callers like `handle_add` skip
    // `resolve_canonical`'s speculative 404 probe.
    if !input.contains('/')
        && let Some(entry) = discover::lookup_shorthand(input)
    {
        return Some(vec![(entry.owner.clone(), entry.repo.clone())]);
    }
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

/// Find the first subscription that matches any candidate, preferring
/// earlier candidates (literal before expanded). Comparison is
/// case-insensitive, mirroring how `handle_add` checks for duplicates.
/// Returns the *stored* keys (preserving the user's original case) plus a
/// reference to the subscription, so callers can use them as map-removal /
/// chat-display keys without re-walking.
fn find_subscription<'a>(
    config: &'a Config,
    candidates: &[(String, String)],
) -> Option<(String, String, &'a Subscription)> {
    for (owner, repo) in candidates {
        if let Some((stored_owner, repos)) = config
            .subscriptions
            .iter()
            .find(|(o, _)| o.eq_ignore_ascii_case(owner))
            && let Some((stored_repo, sub)) =
                repos.iter().find(|(r, _)| r.eq_ignore_ascii_case(repo))
        {
            return Some((stored_owner.clone(), stored_repo.clone(), sub));
        }
    }
    None
}

fn find_subscription_mut<'a>(
    config: &'a mut Config,
    candidates: &[(String, String)],
) -> Option<(String, String, &'a mut Subscription)> {
    let (stored_owner, stored_repo) = find_stored_keys(config, candidates)?;
    let sub = config
        .subscriptions
        .get_mut(&stored_owner)?
        .get_mut(&stored_repo)?;
    Some((stored_owner, stored_repo, sub))
}

/// Resolve `candidates` to the actual stored keys (preserving the user's
/// original case) via case-insensitive comparison. Returns the first hit in
/// candidate order. Used by `find_subscription_mut` to decouple the
/// immutable lookup phase from the final mutable borrow, so the borrow
/// checker accepts a `&'a mut Subscription` return.
fn find_stored_keys(config: &Config, candidates: &[(String, String)]) -> Option<(String, String)> {
    for (owner, repo) in candidates {
        if let Some((stored_owner, repos)) = config
            .subscriptions
            .iter()
            .find(|(o, _)| o.eq_ignore_ascii_case(owner))
            && let Some((stored_repo, _)) = repos.iter().find(|(r, _)| r.eq_ignore_ascii_case(repo))
        {
            return Some((stored_owner.clone(), stored_repo.clone()));
        }
    }
    None
}

const USAGE_LINES: &[&str] = &[
    "&a/client Manager add <owner>/<repo> [stable|prerelease|tag <ref>] [token <token>]",
    "&a/client Manager remove <owner>/<repo>",
    "&a/client Manager channel <owner>/<repo> stable|prerelease|tag <ref>",
    "&a/client Manager disable <owner>/<repo>",
    "&a/client Manager enable <owner>/<repo>",
    "&a/client Manager pause <owner>/<repo>",
    "&a/client Manager unpause <owner>/<repo>",
    "&a/client Manager list",
    "&a/client Manager update [<owner>/<repo>]",
    "&a/client Manager load <owner>/<repo>",
    "&a/client Manager unload <owner>/<repo>",
    "&a/client Manager discover [<search>]",
];

/// Parse the trailing channel arguments after `<owner>/<repo>`.
///
/// Accepted forms (CLI):
/// - `[]`            → `Channel::Stable` (default for `/add`)
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

/// Parse the trailing args after `<owner>/<repo>` for `/add`. Strips a
/// trailing `["token", t]` pair if present, then defers to
/// `parse_channel_args` for the rest. The token slot must be the last
/// two args; embedding it between channel args is rejected by
/// `parse_channel_args`'s strict whitelist.
fn parse_add_args(args: &[&str]) -> Result<(Channel, Option<String>), String> {
    let (channel_args, token) = match args {
        [rest @ .., "token", t] => {
            if t.is_empty() {
                return Err("token value cannot be empty".into());
            }
            (rest, Some((*t).to_owned()))
        }
        [.., "token"] => return Err("expected token <value>, got bare token".into()),
        _ => (args, None),
    };
    let channel = parse_channel_args(channel_args)?;
    Ok((channel, token))
}

/// Switch a subscription to a new channel and invalidate its cached release
/// fields. The cache lives per-subscription, so without clearing it a stale
/// stable lookup could mask a prerelease (or vice-versa) until the TTL
/// expires. Installed-state fields (`installed_*`) are deliberately untouched
/// — those describe what's on disk, not what's on GitHub.
fn apply_channel_switch(sub: &mut Subscription, new: Channel) {
    sub.channel = new;
    sub.state.cached_tag = None;
    sub.state.cached_at = None;
    sub.state.cached_published_at = None;
}

/// True when the new channel is a tag pin matching the already-installed
/// version. Used by `/channel` to skip the post-switch auto-update for the
/// pause-to-current-tag case: switching to the tag we already have should
/// not "Check for latest release..." then chat "is already on ...".
/// Non-tag channels (`Stable`, `Prerelease`) always return false; their
/// resolved release might happen to equal `installed_version`, but
/// `run_update_with_release` already short-circuits that downstream.
fn channel_matches_installed(channel: &Channel, installed: Option<&str>) -> bool {
    matches!(channel, Channel::Tag(v) if installed == Some(v.as_str()))
}

/// Decide which `Channel::Tag` value to switch to when `/pause` is invoked.
/// Returns the pinned channel on success, or a chat-ready reason for refusing
/// (no installed version yet, or the subscription is already pinned).
fn pause_target(sub: &Subscription) -> Result<Channel, String> {
    if let Channel::Tag(v) = &sub.channel {
        return Err(format!("already paused on tag: {v}"));
    }
    let Some(v) = sub.state.installed_version.clone() else {
        return Err("no installed version; run /client Manager update <spec> first".into());
    };
    Ok(Channel::Tag(v))
}

/// Returns a chat-ready refusal message when `(owner, repo)` is the manager's
/// own subscription. Used by mutating handlers that would otherwise leave the
/// user in a half-state (entry removed but binary still loaded, or
/// self-updates silently disabled). `action` is the verb shown in the message
/// (e.g. `"remove"`, `"disable"`).
fn refuse_self_mutation(owner: &str, repo: &str, action: &str) -> Option<String> {
    if !config::is_self(owner, repo) {
        return None;
    }
    Some(format!(
        "{}Refusing to {action} {}{owner}/{repo}{}: this is the manager plugin itself. Use \
         {}/client Manager update{} to upgrade it; edit plugins/plugin-manager.toml by hand if \
         you really need to change this entry.",
        color::YELLOW,
        color::LIME,
        color::YELLOW,
        color::LIME,
        color::YELLOW,
    ))
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
        "{}Refusing to modify config (load failed - fix plugins/plugin-manager.toml first): {}{e}",
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

/// Look in `plugins/` and `plugins/managed/` for files that look like a build
/// artifact for `repo` but aren't basenames we'd write to ourselves. Returns
/// `Ok(Vec::new())` when there's no conflict.
///
/// `skip_basenames` should include any files we're allowed to overwrite or
/// already manage: the canonical asset name we're about to install, the sub's
/// existing `installed_asset`, or (for self-update) the running binary's
/// basename. Anything else matching the repo's name shape is a duplicate-load
/// hazard.
fn find_install_conflicts(repo: &str, skip_basenames: &[&str]) -> Vec<PathBuf> {
    match reconcile::find_variant_conflicts(
        Path::new(PLUGINS_DIR),
        Path::new(MANAGED_DIR),
        repo,
        env::consts::DLL_SUFFIX,
        skip_basenames,
    ) {
        Ok(v) => v,
        Err(e) => {
            // Scan failure is non-fatal: we proceed without the safety net
            // rather than blocking the user on transient I/O.
            warn!("scanning for variant conflicts of {repo}: {e:#}");
            Vec::new()
        }
    }
}

/// Chat-format a conflict refusal. The caller has already decided to abort
/// the install/add; this prints the user-facing reason.
async fn print_install_conflict(spec: &str, action: &str, conflicts: &[PathBuf]) {
    let listed = conflicts
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    print_async(format!(
        "{}Refusing to {action} {}{spec}{}: existing file(s) would load as a duplicate of this \
         plugin: {}{}{} - delete one to avoid loading both",
        color::YELLOW,
        color::LIME,
        color::YELLOW,
        color::LIME,
        listed,
        color::YELLOW,
    ))
    .await;
}

async fn print_not_added(spec: &str) {
    print_async(format!(
        "{}Not added: {}{}{}; use {}add{} first",
        color::YELLOW,
        color::LIME,
        spec,
        color::YELLOW,
        color::LIME,
        color::YELLOW,
    ))
    .await;
}

fn handle_add(spec: &str, channel: Channel, token: Option<String>) {
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

        if let Some((existing_owner, existing_repo, _)) = find_subscription(&config, &candidates) {
            let token_note = if token.is_some() {
                format!(
                    " {}(token ignored; edit plugins/plugin-manager.toml to change it)",
                    color::YELLOW,
                )
            } else {
                String::new()
            };
            print_async(format!(
                "{}Already added: {}{existing_owner}/{existing_repo} {}(use {}/client Manager \
                 channel{} to switch channels){token_note}",
                color::YELLOW,
                color::LIME,
                color::YELLOW,
                color::LIME,
                color::YELLOW,
            ))
            .await;
            return;
        }
        drop(config);

        let install_token = token.clone();
        let Some((owner, repo)) = add_subscription(&spec, candidates, &channel, token).await else {
            return;
        };
        print_async(format!(
            "{}Added {}{owner}/{repo}{}{}, installing...",
            color::PINK,
            color::LIME,
            channel_suffix(&channel),
            color::PINK,
        ))
        .await;
        run_update_task(owner, repo, channel, install_token).await;
    });
}

/// Resolve `candidates` to a canonical `(owner, repo)`, insert a fresh
/// subscription on `channel`, and persist. Shared by explicit
/// `/add` and the implicit-add paths in `/update`, `/enable`,
/// `/channel`. Single-candidate inputs skip the probe - callers
/// that go on to install (`run_update`) hit GitHub anyway. Returns
/// `None` on probe-resolve failure or save failure, after printing
/// a chat-ready error. Caller must already have verified that no
/// subscription matches `candidates`.
async fn add_subscription(
    spec: &str,
    candidates: Vec<(String, String)>,
    channel: &Channel,
    token: Option<String>,
) -> Option<(String, String)> {
    let (owner, repo) = if candidates.len() == 1 {
        candidates.into_iter().next().unwrap()
    } else {
        match resolve_canonical(&candidates, channel, token.as_deref()).await {
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
                return None;
            }
        }
    };

    // For the self repo the running binary's filename is a legitimate match
    // and must be skipped; everything else flagged here would create a
    // double-load.
    let self_basename = if config::is_self(&owner, &repo) {
        current_lib_path()
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
    } else {
        None
    };
    let skip: Vec<&str> = self_basename.as_deref().into_iter().collect();
    let conflicts = find_install_conflicts(&repo, &skip);
    if !conflicts.is_empty() {
        print_install_conflict(spec, "add", &conflicts).await;
        return None;
    }

    let owner_for_save = owner.clone();
    let repo_for_save = repo.clone();
    let channel_for_save = channel.clone();
    let save_result = Config::modify_at(config_path(), move |config| {
        config
            .subscriptions
            .entry(owner_for_save)
            .or_default()
            .insert(
                repo_for_save,
                Subscription {
                    channel: channel_for_save,
                    disabled: false,
                    token: token.map(Secret::new),
                    state: SubscriptionState::default(),
                },
            );
    });
    if let Err(e) = save_result {
        print_save_error(&e).await;
        return None;
    }
    Some((owner, repo))
}

/// Probe each candidate against the GitHub API and OS asset filter; return
/// the first `(owner, repo)` whose release for `channel` has a matching
/// asset for our platform. Errors aggregate the per-candidate failure
/// messages.
async fn resolve_canonical(
    candidates: &[(String, String)],
    channel: &Channel,
    token: Option<&str>,
) -> Result<(String, String)> {
    let mut errors: Vec<String> = Vec::new();
    for (owner, repo) in candidates {
        match probe_release(owner, repo, channel, token).await {
            Ok(()) => return Ok((owner.clone(), repo.clone())),
            Err(e) => errors.push(format!("{owner}/{repo}: {e}")),
        }
    }
    bail!("{}", errors.join("; "));
}

async fn probe_release(
    owner: &str,
    repo: &str,
    channel: &Channel,
    token: Option<&str>,
) -> Result<()> {
    // The subscription doesn't exist yet, but `/add` may have supplied a
    // token inline; pass it through so private-repo probes succeed instead
    // of failing with the "may be private - add a token" hint.
    let release = get_release_for_channel(owner, repo, channel, token).await?;
    pick_asset(&release.assets, env::consts::ARCH, env::consts::DLL_SUFFIX)?;
    Ok(())
}

/// Implicit-add path for commands whose user intent reads as "I want
/// this plugin on" (`/update`, `/enable`, `/channel`). Wraps
/// `add_subscription` with an "(auto), installing..." chat message and
/// hands off to the existing install path. Caller has already checked
/// that no subscription exists for `candidates`.
async fn auto_add_and_install(spec: &str, candidates: Vec<(String, String)>, channel: Channel) {
    let Some((owner, repo)) = add_subscription(spec, candidates, &channel, None).await else {
        return;
    };
    print_async(format!(
        "{}Added {}{owner}/{repo}{} {}(auto), installing...",
        color::PINK,
        color::LIME,
        channel_suffix(&channel),
        color::PINK,
    ))
    .await;
    run_update_task(owner, repo, channel, None).await;
}

fn handle_remove(spec: &str) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        enum RemoveOutcome {
            NotAdded,
            RefuseSelf(String),
            Removed {
                stored_owner: String,
                stored_repo: String,
                installed_asset: Option<String>,
            },
        }
        let outcome = Config::modify_at(config_path(), |config| {
            let Some((stored_owner, stored_repo, sub)) = find_subscription(config, &candidates)
            else {
                return RemoveOutcome::NotAdded;
            };
            if let Some(msg) = refuse_self_mutation(&stored_owner, &stored_repo, "remove") {
                return RemoveOutcome::RefuseSelf(msg);
            }
            let installed_asset = sub.state.installed_asset.clone();
            if let Some(repos) = config.subscriptions.get_mut(&stored_owner) {
                repos.remove(&stored_repo);
                if repos.is_empty() {
                    config.subscriptions.remove(&stored_owner);
                }
            }
            RemoveOutcome::Removed {
                stored_owner,
                stored_repo,
                installed_asset,
            }
        });
        let (stored_owner, stored_repo, installed_asset) = match outcome {
            Err(e) => {
                print_save_error(&e).await;
                return;
            }
            Ok(RemoveOutcome::NotAdded) => {
                print_async(format!(
                    "{}Not added: {}{}",
                    color::YELLOW,
                    color::LIME,
                    spec,
                ))
                .await;
                return;
            }
            Ok(RemoveOutcome::RefuseSelf(msg)) => {
                print_async(msg).await;
                return;
            }
            Ok(RemoveOutcome::Removed {
                stored_owner,
                stored_repo,
                installed_asset,
            }) => (stored_owner, stored_repo, installed_asset),
        };
        print_async(format!(
            "{}Removed {}{stored_owner}/{stored_repo}",
            color::PINK,
            color::LIME,
        ))
        .await;

        run_unload_followup(stored_owner.clone(), stored_repo.clone()).await;

        if let Some(name) = installed_asset {
            let path = Path::new(MANAGED_DIR).join(&name);
            match fs::remove_file(&path) {
                Ok(()) => debug!("removed managed binary {}", path.display()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    warn!("could not remove {}: {e}", path.display());
                    // Windows holds a sharing lock on the mapped DLL: telling
                    // the user to delete it by hand is wrong (Explorer hits
                    // the same violation). `fs::rename` (MoveFileExW) succeeds
                    // against a locked DLL even though `DeleteFile` doesn't,
                    // so move it aside to `<name>.old` (matching the
                    // `install_bytes_to` convention) and let the startup
                    // sweep reap it next session.
                    let aside = path.with_file_name(format!("{name}.old"));
                    match fs::rename(&path, &aside) {
                        Ok(()) => debug!("renamed locked {} -> .old", path.display()),
                        Err(e2) => {
                            warn!("could not rename {}: {e2}", path.display());
                            print_async(format!(
                                "{}Could not delete {}{}{}: {}{}{}; still in use, will be cleaned \
                                 up on next restart.",
                                color::YELLOW,
                                color::LIME,
                                path.display(),
                                color::YELLOW,
                                color::LIME,
                                e,
                                color::YELLOW,
                            ))
                            .await;
                        }
                    }
                }
            }
        }
    });
}

fn handle_channel(spec: &str, channel: Channel) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        enum ChannelOutcome {
            NoSub,
            AlreadyOnChannel {
                owner: String,
                repo: String,
            },
            Switched {
                owner: String,
                repo: String,
                token: Option<String>,
                installed_version: Option<String>,
            },
        }
        let outcome = Config::modify_at(config_path(), |config| {
            let Some((owner, repo, sub)) = find_subscription_mut(config, &candidates) else {
                return ChannelOutcome::NoSub;
            };
            if sub.channel == channel {
                return ChannelOutcome::AlreadyOnChannel { owner, repo };
            }
            let token = sub.token.as_ref().map(|s| s.expose().to_owned());
            let installed_version = sub.state.installed_version.clone();
            apply_channel_switch(sub, channel.clone());
            ChannelOutcome::Switched {
                owner,
                repo,
                token,
                installed_version,
            }
        });
        match outcome {
            Err(e) => print_save_error(&e).await,
            Ok(ChannelOutcome::AlreadyOnChannel { owner, repo }) => {
                print_async(format!(
                    "{}{owner}/{repo} {}already on channel {}{}",
                    color::LIME,
                    color::YELLOW,
                    color::PINK,
                    channel.pretty(),
                ))
                .await;
            }
            Ok(ChannelOutcome::Switched {
                owner,
                repo,
                token,
                installed_version,
            }) => {
                print_async(format!(
                    "{}Channel for {}{owner}/{repo} {}set to {}{}",
                    color::PINK,
                    color::LIME,
                    color::PINK,
                    color::YELLOW,
                    channel.pretty(),
                ))
                .await;
                // Pulling the new channel's binary is the whole point of
                // the switch; skip only the pause-to-current-tag case to
                // avoid the "Checking..." / "already on ..." chat noise on
                // top of the "Channel set to ..." line we just printed.
                if !channel_matches_installed(&channel, installed_version.as_deref()) {
                    run_update_task(owner, repo, channel, token).await;
                }
            }
            Ok(ChannelOutcome::NoSub) => {
                auto_add_and_install(&spec, candidates, channel).await;
            }
        }
    });
}

fn set_disabled(spec: &str, disabled: bool) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        enum SetDisabledOutcome {
            NoSub,
            RefuseSelf(String),
            AlreadyMatched { owner: String, repo: String },
            Toggled { owner: String, repo: String },
        }
        let outcome = Config::modify_at(config_path(), |config| {
            let Some((owner, repo, sub)) = find_subscription_mut(config, &candidates) else {
                return SetDisabledOutcome::NoSub;
            };
            if disabled && let Some(msg) = refuse_self_mutation(&owner, &repo, "disable") {
                return SetDisabledOutcome::RefuseSelf(msg);
            }
            if sub.disabled == disabled {
                return SetDisabledOutcome::AlreadyMatched { owner, repo };
            }
            sub.disabled = disabled;
            SetDisabledOutcome::Toggled { owner, repo }
        });
        match outcome {
            Err(e) => print_save_error(&e).await,
            Ok(SetDisabledOutcome::RefuseSelf(msg)) => print_async(msg).await,
            Ok(SetDisabledOutcome::AlreadyMatched { owner, repo }) => {
                let word = if disabled { "disabled" } else { "enabled" };
                print_async(format!(
                    "{}Already {word} {}{owner}/{repo}",
                    color::YELLOW,
                    color::LIME,
                ))
                .await;
            }
            Ok(SetDisabledOutcome::Toggled { owner, repo }) => {
                let word = if disabled { "Disabled" } else { "Enabled" };
                print_async(format!(
                    "{}{word} {}{owner}/{repo}",
                    color::PINK,
                    color::LIME,
                ))
                .await;
                if disabled {
                    run_unload_followup(owner, repo).await;
                }
            }
            Ok(SetDisabledOutcome::NoSub) => {
                // /disable on an unsubscribed repo would create a sub only to
                // immediately turn it off, which is pointless. /enable, on the
                // other hand, reads as "I want this plugin on" - same intent as
                // /update, so auto-subscribe + install with the default channel.
                if disabled {
                    print_not_added(&spec).await;
                } else {
                    auto_add_and_install(&spec, candidates, Channel::Stable).await;
                }
            }
        }
    });
}

fn handle_pause(spec: &str) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        enum PauseOutcome {
            NotAdded,
            CannotPause {
                owner: String,
                repo: String,
                error: String,
            },
            Paused {
                owner: String,
                repo: String,
                pinned_tag: String,
            },
        }
        let outcome = Config::modify_at(config_path(), |config| {
            let Some((owner, repo, sub)) = find_subscription_mut(config, &candidates) else {
                return PauseOutcome::NotAdded;
            };
            let target = match pause_target(sub) {
                Ok(c) => c,
                Err(error) => return PauseOutcome::CannotPause { owner, repo, error },
            };
            let pinned_tag = match &target {
                Channel::Tag(t) => t.clone(),
                _ => unreachable!("pause_target only returns Channel::Tag"),
            };
            apply_channel_switch(sub, target);
            PauseOutcome::Paused {
                owner,
                repo,
                pinned_tag,
            }
        });
        match outcome {
            Err(e) => print_save_error(&e).await,
            Ok(PauseOutcome::NotAdded) => print_not_added(&spec).await,
            Ok(PauseOutcome::CannotPause { owner, repo, error }) => {
                print_async(format!(
                    "{}Cannot pause {}{owner}/{repo}{}: {}{error}",
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                    color::WHITE,
                ))
                .await;
            }
            Ok(PauseOutcome::Paused {
                owner,
                repo,
                pinned_tag,
            }) => {
                print_async(format!(
                    "{}Paused {}{owner}/{repo} {}on tag {}{}",
                    color::PINK,
                    color::LIME,
                    color::PINK,
                    color::YELLOW,
                    pinned_tag,
                ))
                .await;
            }
        }
    });
}

fn handle_unpause(spec: &str) {
    let Some(candidates) = expand_candidates(spec) else {
        print_wrapped(format!("{}Expected owner/repo, got: {spec}", color::RED));
        return;
    };
    let spec = spec.to_string();

    async_manager::spawn(async move {
        enum UnpauseOutcome {
            NotAdded,
            NotPaused {
                owner: String,
                repo: String,
                channel: Channel,
            },
            Resumed {
                owner: String,
                repo: String,
            },
        }
        let outcome = Config::modify_at(config_path(), |config| {
            let Some((owner, repo, sub)) = find_subscription_mut(config, &candidates) else {
                return UnpauseOutcome::NotAdded;
            };
            if !matches!(sub.channel, Channel::Tag(_)) {
                return UnpauseOutcome::NotPaused {
                    owner,
                    repo,
                    channel: sub.channel.clone(),
                };
            }
            apply_channel_switch(sub, Channel::Stable);
            UnpauseOutcome::Resumed { owner, repo }
        });
        match outcome {
            Err(e) => print_save_error(&e).await,
            Ok(UnpauseOutcome::NotAdded) => print_not_added(&spec).await,
            Ok(UnpauseOutcome::NotPaused {
                owner,
                repo,
                channel,
            }) => {
                print_async(format!(
                    "{}{owner}/{repo} {}is not paused (channel: {}{}{})",
                    color::LIME,
                    color::YELLOW,
                    color::PINK,
                    channel.pretty(),
                    color::YELLOW,
                ))
                .await;
            }
            Ok(UnpauseOutcome::Resumed { owner, repo }) => {
                print_async(format!(
                    "{}Resumed {}{owner}/{repo} {}on stable {}(use {}/client Manager channel{} to \
                     switch to prerelease)",
                    color::PINK,
                    color::LIME,
                    color::PINK,
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                ))
                .await;
            }
        }
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

        let total: usize = config.subscriptions.values().map(BTreeMap::len).sum();
        if total == 0 {
            print_async(format!("{}No subscriptions", color::YELLOW)).await;
            return;
        }
        print_async(format!("{}Subscriptions ({total}):", color::PINK,)).await;
        for (owner, repos) in &config.subscriptions {
            for (repo, sub) in repos {
                let mut line = format!("  {}{owner}/{repo}", color::LIME);
                if let Some(v) = &sub.state.installed_version {
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

        if let Some((owner, repo, sub)) = find_subscription(&config, &candidates) {
            if sub.disabled {
                print_async(format!(
                    "{}Subscription {}{owner}/{repo} {}is disabled; use {}enable {owner}/{repo}{} \
                     first",
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                ))
                .await;
                return;
            }

            let token = sub.token.as_ref().map(|s| s.expose().to_owned());
            run_update_task(owner, repo, sub.channel.clone(), token).await;
            return;
        }
        drop(config);

        auto_add_and_install(&spec, candidates, Channel::Stable).await;
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

        let candidates: Vec<(String, String, Subscription)> = config
            .subscriptions
            .into_iter()
            .flat_map(|(owner, repos)| {
                repos
                    .into_iter()
                    .map(move |(repo, sub)| (owner.clone(), repo, sub))
            })
            .filter(|(_, _, s)| !s.disabled)
            .collect();

        if candidates.is_empty() {
            print_async(format!("{}Nothing to update", color::YELLOW)).await;
            return;
        }

        print_async(format!(
            "{}Checking {}{}{} subscription(s) for updates...",
            color::PINK,
            color::YELLOW,
            candidates.len(),
            color::PINK,
        ))
        .await;

        let now = unix_now();
        let mut cache_updates: Vec<(String, String, String, u64)> = Vec::new();
        let mut stale: Vec<(String, String, Option<String>, GitHubRelease)> = Vec::new();

        for (owner, repo, sub) in candidates {
            match resolve_latest_release(&owner, &repo, &sub, now, true).await {
                Ok((tag, pub_at, Some(release))) => {
                    cache_updates.push((owner.clone(), repo.clone(), tag, pub_at));
                    if sub.state.installed_at.is_none_or(|i| pub_at > i) {
                        let token = sub.token.as_ref().map(|t| t.expose().to_owned());
                        stale.push((owner, repo, token, release));
                    }
                }
                Ok((_, _, None)) => {
                    // resolve_latest_release with force_refresh=true always
                    // returns Some(release); no-op fallback.
                }
                Err(e) => {
                    error!("checking {}/{}: {e:#}", owner, repo);
                    print_async(format!(
                        "{}Failed to check {}{}/{}{}: {}{e}",
                        color::RED,
                        color::LIME,
                        owner,
                        repo,
                        color::RED,
                        color::WHITE,
                    ))
                    .await;
                }
            }
        }

        if !cache_updates.is_empty()
            && let Err(e) = persist_cache_updates(now, cache_updates)
        {
            error!("saving config (cache update): {e:#}");
        }

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
        for (owner, repo, token, release) in stale {
            run_update_task_with_release(owner, repo, token, release).await;
        }
    });
}

async fn run_update_task(owner: String, repo: String, channel: Channel, token: Option<String>) {
    if let Err(e) = run_update(&owner, &repo, &channel, token.as_deref()).await {
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
}

async fn run_update_task_with_release(
    owner: String,
    repo: String,
    token: Option<String>,
    release: GitHubRelease,
) {
    if let Err(e) = run_update_with_release(&owner, &repo, token.as_deref(), release).await {
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
}

async fn run_update(owner: &str, repo: &str, channel: &Channel, token: Option<&str>) -> Result<()> {
    print_async(format!(
        "{}Checking {}{}/{}{} for latest release...",
        color::PINK,
        color::LIME,
        owner,
        repo,
        color::PINK,
    ))
    .await;

    let release = get_release_for_channel(owner, repo, channel, token).await?;
    run_update_with_release(owner, repo, token, release).await
}

async fn run_update_with_release(
    owner: &str,
    repo: &str,
    token: Option<&str>,
    release: GitHubRelease,
) -> Result<()> {
    let asset = pick_asset(&release.assets, env::consts::ARCH, env::consts::DLL_SUFFIX)?;
    let is_self = config::is_self(owner, repo);

    // Snapshot what the sub thinks it owns on disk *before* we touch
    // anything: lets us short-circuit a same-tag re-install (and lets the
    // post-install A-cleanup unlink the prior versioned binary). Loading
    // off-thread is fine - the runtime is single-threaded but Config::load
    // is blocking I/O and we want it out of the main thread regardless.
    let (prev_version, prev_asset) = match Config::load() {
        Ok(cfg) => cfg
            .subscriptions
            .get(owner)
            .and_then(|repos| repos.get(repo))
            .map(|s| {
                (
                    s.state.installed_version.clone(),
                    s.state.installed_asset.clone(),
                )
            })
            .unwrap_or((None, None)),
        Err(_) => (None, None),
    };

    // Skip the download entirely when we're already on this exact tag and
    // the file is on disk. Same-tag re-installs don't actually swap code in
    // the running process anyway (versioned filename collides with the
    // currently-mapped one, so dlopen returns the cached handle), so
    // there's no behavior to deliver. Self lives in plugins/, managed in
    // plugins/managed/.
    let already_installed = (prev_version.as_deref() == Some(&release.tag_name)
        && prev_asset.as_deref().is_some_and(|name| {
            let dir = if is_self { PLUGINS_DIR } else { MANAGED_DIR };
            Path::new(dir).join(name).exists()
        }))
        || (is_self
            && Path::new(PLUGINS_DIR)
                .join(installer::versioned_managed_filename(
                    config::SELF_OWNER,
                    config::SELF_REPO,
                    &release.tag_name,
                    env::consts::DLL_SUFFIX,
                ))
                .exists());
    if already_installed {
        print_async(format!(
            "{}{}/{} {}is already on {}{}{}; nothing to do",
            color::LIME,
            owner,
            repo,
            color::PINK,
            color::GREEN,
            release.tag_name,
            color::PINK,
        ))
        .await;
        return Ok(());
    }

    // Skip the file we'd legitimately overwrite: for non-self, the
    // versioned managed filename we're about to write (matches the prior
    // install of the same tag, if any); for self, the running binary's
    // basename. Anything else flagged would load as a duplicate.
    let new_managed_name = if is_self {
        None
    } else {
        Some(installer::versioned_managed_filename(
            owner,
            repo,
            &release.tag_name,
            env::consts::DLL_SUFFIX,
        ))
    };
    let self_basename = if is_self {
        current_lib_path()
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
    } else {
        None
    };
    // Refuse to overwrite a `cargo build` self - the loaded file is what
    // the dev wants to keep iterating on. Released self assets carry the
    // `_<os>_<arch>` tokens and don't normalize to SELF_REPO.
    if is_self
        && let Some(name) = self_basename.as_deref()
        && asset_match::is_canonical_or_cdylib_name(
            name,
            config::SELF_REPO,
            env::consts::DLL_SUFFIX,
        )
    {
        print_async(format!(
            "{}Skipping self-update: loaded {}{}{} looks like a dev build (replace it with a \
             released binary if you want self-updates)",
            color::YELLOW,
            color::LIME,
            name,
            color::YELLOW,
        ))
        .await;
        return Ok(());
    }
    let skip: Vec<&str> = if is_self {
        self_basename.as_deref().into_iter().collect()
    } else {
        new_managed_name.as_deref().into_iter().collect()
    };
    let conflicts = find_install_conflicts(repo, &skip);
    if !conflicts.is_empty() {
        let spec = format!("{owner}/{repo}");
        print_install_conflict(&spec, "update", &conflicts).await;
        bail!(
            "refusing to install: existing file(s) would load as a duplicate; delete one of: {}",
            conflicts
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

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
    let path = if is_self {
        download_self(asset, expected_digest.as_deref(), token, &release.tag_name).await?
    } else {
        download_to_managed_dir(
            owner,
            repo,
            &release.tag_name,
            asset,
            expected_digest.as_deref(),
            token,
        )
        .await?
    };

    let installed_basename = path
        .file_name()
        .and_then(|n| n.to_str())
        .map_or_else(|| asset.name.clone(), str::to_owned);

    // For self, mark the previously-loaded file aside *before* persisting
    // the new claim. If we crashed between install and rename, the next
    // launch would load both the old and new versioned files (two
    // managers); doing the rename first shrinks that window. Best-effort -
    // a Windows lock leaves a `.old` for the next-session sweep.
    if is_self
        && let Some(prev) = self_basename.as_deref()
        && let Some(dir) = path.parent()
    {
        mark_previous_self_aside(dir, prev, &installed_basename);
    }

    let now = unix_now();
    persist_installed_versions(
        now,
        vec![(
            owner.to_owned(),
            repo.to_owned(),
            release.tag_name.clone(),
            installed_basename.clone(),
            release.published_at,
        )],
    )?;

    if is_self {
        print_async(format!(
            "{}Plugin manager updated to {}{}{} - restart ClassiCube to use the new version",
            color::PINK,
            color::GREEN,
            release.tag_name,
            color::PINK,
        ))
        .await;
        return Ok(());
    }

    // A-cleanup: best-effort unlink of the prior versioned binary now that
    // the new one is on disk and persisted. Linux/macOS unlinks even while
    // the old library is still mapped; Windows may fail with sharing
    // violation, in which case the startup orphan sweep mops it up next
    // session. cleanup_previous_managed is a no-op when prev == new (e.g.
    // ad-hoc tag like `nightly` re-uploaded under the same name, though we
    // already short-circuited that above when installed_version matches).
    cleanup_previous_managed(
        Path::new(MANAGED_DIR),
        prev_asset.as_deref(),
        &installed_basename,
    );

    print_async(format!(
        "{}Installed {}{} {}for {}{}/{} {}-> {}{}",
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
    ))
    .await;

    // In-session swap. If the plugin is already loaded, drop the LOADED
    // entry and dlopen the new versioned path - fresh path, fresh mapping,
    // new code runs immediately. The old library stays mapped (we don't
    // dlclose; see src/loader/mod.rs module comment about TLS destructors).
    // If the plugin isn't loaded yet (fresh install via /add, or user had
    // /unloaded it), dlopen the new path so /update doesn't require a
    // separate /load.
    let owner_s = owner.to_owned();
    let repo_s = repo.to_owned();
    let sub_for_load = Config::load().ok().and_then(|c| {
        c.subscriptions
            .get(&owner_s)
            .and_then(|r| r.get(&repo_s))
            .cloned()
    });
    if let Some(sub) = sub_for_load {
        async_manager::run_on_main_thread(async move {
            let id = format!("{owner_s}/{repo_s}");
            if loader::is_loaded(&owner_s, &repo_s) {
                loader::unload_one(&owner_s, &repo_s);
            }
            // The user just got a fresh binary; honor "load it now" intent
            // and drop any session-skip flag the Startup pass set for this
            // sub so the dlopen actually happens.
            loader::clear_carryover_skip(&owner_s, &repo_s);
            let outcome = loader::load_one(&owner_s, &repo_s, &sub, LifecyclePhase::Catchup);
            chat_post_update_load_outcome(&id, &outcome);
        })
        .await;
    }

    Ok(())
}

/// Chat for the in-session reload that follows a successful `/update`.
/// `loader::load_one` already chats "Loading X" before the dlopen, so the
/// success arm is silent here; this only surfaces failure modes and odd
/// edge cases distinct from `handle_load`'s messages.
fn chat_post_update_load_outcome(id: &str, outcome: &LoadOutcome) {
    match outcome {
        LoadOutcome::Loaded => {}
        LoadOutcome::Disabled
        | LoadOutcome::IsSelf
        | LoadOutcome::NotInstalled
        | LoadOutcome::AlreadyLoaded
        | LoadOutcome::SkippedFromCarryover => {
            // Disabled: user opted out; don't auto-load. IsSelf: never reached
            // (caller skips the swap for self). NotInstalled / AlreadyLoaded:
            // shouldn't happen post-install; stay silent.
            // SkippedFromCarryover: caller cleared the skip set before the
            // load_one call, so this is unreachable; stay silent.
        }
        LoadOutcome::CrashCarryover { previous } => print_wrapped(format!(
            "{}{id} crashed inside {}{previous}{} last session; cleared the breadcrumb. Try again.",
            color::YELLOW,
            color::LIME,
            color::YELLOW,
        )),
        LoadOutcome::PluginsDirConflict { path } => print_wrapped(format!(
            "{}Installed but not loaded: {}{}{} would load as a duplicate; delete one",
            color::YELLOW,
            color::LIME,
            path.display(),
            color::YELLOW,
        )),
        LoadOutcome::LoadError(e) => print_wrapped(format!(
            "{}Installed but failed to load {}{id}{}: {}{e}",
            color::RED,
            color::LIME,
            color::RED,
            color::WHITE,
        )),
        LoadOutcome::PluginOutdated { plugin, host } => print_wrapped(format!(
            "{}{id}{} plugin is outdated (api {plugin}, host expects {host})",
            color::LIME,
            color::RED,
        )),
        LoadOutcome::HostOutdated { plugin, host } => print_wrapped(format!(
            "{}Game is too outdated for {}{id}{} (api {plugin}, host expects {host})",
            color::RED,
            color::LIME,
            color::RED,
        )),
    }
}

/// Drop a plugin's LOADED entry on the main thread, used as a follow-up to
/// `/remove` and `/disable` so the in-process state matches the
/// just-persisted config. Silent when nothing was loaded; the caller has
/// already chatted about the primary action ("Removed", "Disabled").
async fn run_unload_followup(owner: String, repo: String) {
    async_manager::run_on_main_thread(async move {
        loader::unload_one(&owner, &repo);
    })
    .await;
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
        ["add", spec, rest @ ..] => match parse_add_args(rest) {
            Ok((c, t)) => handle_add(spec, c, t),
            Err(e) => print_wrapped(format!("{}{e}", color::RED)),
        },
        ["remove", spec] => handle_remove(spec),
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
        ["pause", spec] => handle_pause(spec),
        ["unpause", spec] => handle_unpause(spec),
        ["list"] => handle_list(),
        ["update"] => handle_update_all(),
        ["update", spec] => handle_update_one(spec),
        ["load", spec] => handle_load(spec),
        ["unload", spec] => handle_unload(spec),
        ["discover"] => handle_discover(None),
        ["discover", term] => handle_discover(Some(term)),
        _ => print_usage(),
    }
}

fn handle_load(spec: &str) {
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

        let Some((owner, repo, sub)) =
            find_subscription(&config, &candidates).map(|(o, r, s)| (o, r, s.clone()))
        else {
            print_not_added(&spec).await;
            return;
        };

        async_manager::run_on_main_thread(async move {
            let id = format!("{}/{}", owner, repo);
            // /load is the explicit-retry path: drop any session-skip flag
            // Startup set so the dlopen actually happens.
            loader::clear_carryover_skip(&owner, &repo);
            let outcome = loader::load_one(&owner, &repo, &sub, LifecyclePhase::Catchup);
            match outcome {
                LoadOutcome::Loaded => {}
                LoadOutcome::Disabled => print_wrapped(format!(
                    "{}{id} {}is disabled; use {}/client Manager enable {id}{} first",
                    color::LIME,
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                )),
                LoadOutcome::IsSelf => print_wrapped(format!(
                    "{}Refusing to load {}{id}{}: this is the manager plugin itself.",
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                )),
                // Reachable only if the disk breadcrumb was set AFTER we
                // cleared the skip set above (e.g. another callback wrote
                // it mid-flight); in practice load_one's classify_carryover
                // only reads disk under Startup, so this arm is effectively
                // dead from /load. Keep it for completeness.
                LoadOutcome::CrashCarryover { previous } => print_wrapped(format!(
                    "{}{id} crashed inside {}{previous}{} last session; cleared the breadcrumb. \
                     Try again.",
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                )),
                // /load cleared the skip set above, so this should not fire.
                LoadOutcome::SkippedFromCarryover => {}
                LoadOutcome::NotInstalled => print_wrapped(format!(
                    "{}{id} {}has no installed binary; use {}/client Manager update {id}{} first",
                    color::LIME,
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                )),
                LoadOutcome::AlreadyLoaded => print_wrapped(format!(
                    "{}{id} {}is already loaded",
                    color::LIME,
                    color::YELLOW,
                )),
                LoadOutcome::PluginsDirConflict { path } => print_wrapped(format!(
                    "{}Refusing to load {}{id}{}: {}{}{} would load as a duplicate; delete one",
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                    color::LIME,
                    path.display(),
                    color::YELLOW,
                )),
                LoadOutcome::LoadError(e) => print_wrapped(format!(
                    "{}Failed to load {}{id}{}: {}{e}",
                    color::RED,
                    color::LIME,
                    color::RED,
                    color::WHITE,
                )),
                LoadOutcome::PluginOutdated { plugin, host } => print_wrapped(format!(
                    "{}{id}{} plugin is outdated (api {plugin}, host expects {host}); update the \
                     plugin",
                    color::LIME,
                    color::RED,
                )),
                LoadOutcome::HostOutdated { plugin, host } => print_wrapped(format!(
                    "{}Game is too outdated for {}{id}{} (api {plugin}, host expects {host}); \
                     update the game",
                    color::RED,
                    color::LIME,
                    color::RED,
                )),
            }
        })
        .await;
    });
}

fn handle_unload(spec: &str) {
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

        let Some((owner, repo, _)) = find_subscription(&config, &candidates) else {
            print_not_added(&spec).await;
            return;
        };

        async_manager::run_on_main_thread(async move {
            let id = format!("{}/{}", owner, repo);
            match loader::unload_one(&owner, &repo) {
                UnloadOutcome::Unloaded => {}
                UnloadOutcome::NotLoaded => print_wrapped(format!(
                    "{}{id} {}is not loaded",
                    color::LIME,
                    color::YELLOW,
                )),
                UnloadOutcome::IsSelf => print_wrapped(format!(
                    "{}Refusing to unload {}{id}{}: this is the manager plugin itself.",
                    color::YELLOW,
                    color::LIME,
                    color::YELLOW,
                )),
            }
        })
        .await;
    });
}

fn handle_discover(term: Option<&str>) {
    let header = match term {
        None => format!("{}Curated plugins:", color::YELLOW),
        Some(t) => format!(
            "{}Plugins matching {}{t}{}:",
            color::YELLOW,
            color::LIME,
            color::YELLOW
        ),
    };
    print_wrapped(header);

    let mut any = false;
    for entry in discover::iter_filtered(term) {
        any = true;
        let shorthand = match &entry.shorthand {
            Some(s) => format!(" {}[{s}]", color::YELLOW),
            None => String::new(),
        };
        print_wrapped(format!(
            "{}{}/{}{shorthand} {}- {}{}",
            color::LIME,
            entry.owner,
            entry.repo,
            color::WHITE,
            color::WHITE,
            entry.description,
        ));
    }
    if !any {
        print_wrapped(format!("{}No matches.", color::YELLOW));
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
            let mut cmd = OwnedChatCommand::new("Manager", c_callback, false, USAGE_LINES.to_vec());
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
