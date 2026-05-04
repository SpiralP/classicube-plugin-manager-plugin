use std::collections::BTreeMap;

use tempfile::{NamedTempFile, tempdir};

use super::*;
use crate::config::{Channel, SubscriptionState};

#[test]
fn needs_install_no_prior_state() {
    // Never installed → install on first sight.
    assert!(needs_install(None, None, 200));
}

#[test]
fn needs_install_published_newer() {
    assert!(needs_install(Some(100), Some("p.so"), 200));
}

#[test]
fn needs_install_published_equal() {
    // Same release as what we have → skip. This is the case where a previous
    // pass ran successfully and nothing has changed upstream.
    assert!(!needs_install(Some(200), Some("p.so"), 200));
}

#[test]
fn needs_install_published_older() {
    // Upstream is older than what we have (maintainer retracted the latest
    // release). Don't auto-downgrade.
    assert!(!needs_install(Some(300), Some("p.so"), 200));
}

#[test]
fn needs_install_asset_missing_but_timestamp_matches() {
    // Pre-asset-tracking installs end up here: timestamp matches but
    // installed_asset was never populated, so we don't know what file to
    // dlopen. Re-download to fill in the field.
    assert!(needs_install(Some(200), None, 200));
}

#[test]
fn needs_install_asset_known_but_no_timestamp() {
    // Defensive: a hand-edited config could pair an asset with no
    // installed_at. Treat as install-needed so we re-resolve cleanly.
    assert!(needs_install(None, Some("p.so"), 200));
}

fn empty_sub() -> Subscription {
    Subscription::default()
}

fn config_with(entries: &[(&str, &str, Subscription)]) -> Config {
    let mut subscriptions: BTreeMap<String, BTreeMap<String, Subscription>> = BTreeMap::new();
    for (owner, repo, sub) in entries {
        subscriptions
            .entry((*owner).into())
            .or_default()
            .insert((*repo).into(), sub.clone());
    }
    Config { subscriptions }
}

fn pick<'a>(cfg: &'a Config, owner: &str, repo: &str) -> &'a Subscription {
    cfg.subscriptions.get(owner).unwrap().get(repo).unwrap()
}

#[test]
fn updates_targeted_subscription_only() {
    let cfg = config_with(&[("alice", "one", empty_sub()), ("bob", "two", empty_sub())]);
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();

    persist_cache_updates_to(
        f.path(),
        12_345,
        vec![("alice".into(), "one".into(), "v9.9.9".into(), 9_000)],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    let alice = pick(&loaded, "alice", "one");
    let bob = pick(&loaded, "bob", "two");
    assert_eq!(alice.state.cached_tag.as_deref(), Some("v9.9.9"));
    assert_eq!(alice.state.cached_at, Some(12_345));
    assert_eq!(alice.state.cached_published_at, Some(9_000));
    assert!(bob.state.cached_tag.is_none());
    assert!(bob.state.cached_at.is_none());
    assert!(bob.state.cached_published_at.is_none());
}

#[test]
fn unknown_owner_repo_silently_skipped() {
    let cfg = config_with(&[("alice", "one", empty_sub())]);
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();

    persist_cache_updates_to(
        f.path(),
        42,
        vec![("ghost".into(), "missing".into(), "v0.0.1".into(), 10)],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    let alice = pick(&loaded, "alice", "one");
    assert!(alice.state.cached_tag.is_none());
    assert!(alice.state.cached_at.is_none());
    assert!(alice.state.cached_published_at.is_none());
}

#[test]
fn missing_config_file_writes_empty_default() {
    // load() returns default on NotFound, so the update is applied to an
    // empty subs list — every entry takes the unknown-row branch — and
    // save() writes back the empty default.
    let dir = tempdir().unwrap();
    let path = dir.path().join("nope.toml");
    persist_cache_updates_to(
        &path,
        7,
        vec![("alice".into(), "one".into(), "v1.0.0".into(), 1)],
    )
    .unwrap();
    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded, Config::default());
}

#[test]
fn installed_version_writes_targeted_subscription() {
    let cfg = config_with(&[("alice", "one", empty_sub()), ("bob", "two", empty_sub())]);
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();

    persist_installed_versions_to(
        f.path(),
        999,
        vec![(
            "alice".into(),
            "one".into(),
            "v1.0.0".into(),
            "one.so".into(),
            500,
        )],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    let alice = pick(&loaded, "alice", "one");
    let bob = pick(&loaded, "bob", "two");
    assert_eq!(alice.state.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(alice.state.installed_asset.as_deref(), Some("one.so"));
    assert_eq!(alice.state.installed_at, Some(500));
    assert_eq!(alice.state.cached_tag.as_deref(), Some("v1.0.0"));
    assert_eq!(alice.state.cached_at, Some(999));
    assert_eq!(alice.state.cached_published_at, Some(500));
    assert!(bob.state.installed_version.is_none());
    assert!(bob.state.installed_asset.is_none());
    assert!(bob.state.installed_at.is_none());
    assert!(bob.state.cached_tag.is_none());
}

#[test]
fn installed_version_unknown_owner_repo_skipped() {
    let cfg = config_with(&[("alice", "one", empty_sub())]);
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();

    persist_installed_versions_to(
        f.path(),
        42,
        vec![(
            "ghost".into(),
            "missing".into(),
            "v9.9.9".into(),
            "missing.so".into(),
            10,
        )],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    let alice = pick(&loaded, "alice", "one");
    assert!(alice.state.installed_version.is_none());
    assert!(alice.state.installed_asset.is_none());
    assert!(alice.state.installed_at.is_none());
}

#[test]
fn persist_helpers_ignore_disabled_flag() {
    // Disabling is enforced at the call site (auto-check loop, /update commands).
    // The persist helpers stay dumb: if a row is in the updates list, they
    // write to it regardless of `disabled`. This documents that contract.
    let cfg = config_with(&[(
        "alice",
        "one",
        Subscription {
            disabled: true,
            channel: Channel::default(),
            token: None,
            state: SubscriptionState::default(),
        },
    )]);
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();

    persist_cache_updates_to(
        f.path(),
        77,
        vec![("alice".into(), "one".into(), "v2.0.0".into(), 200)],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    let alice = pick(&loaded, "alice", "one");
    assert!(alice.disabled);
    assert_eq!(alice.state.cached_tag.as_deref(), Some("v2.0.0"));
    assert_eq!(alice.state.cached_at, Some(77));
    assert_eq!(alice.state.cached_published_at, Some(200));
}

#[test]
fn installed_version_missing_config_file_writes_empty_default() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nope.toml");
    persist_installed_versions_to(
        &path,
        7,
        vec![(
            "alice".into(),
            "one".into(),
            "v1.0.0".into(),
            "one.so".into(),
            1,
        )],
    )
    .unwrap();
    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded, Config::default());
}
