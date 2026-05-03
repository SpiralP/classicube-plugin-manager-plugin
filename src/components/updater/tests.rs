use tempfile::NamedTempFile;

use super::*;

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

fn sub(owner: &str, repo: &str) -> Subscription {
    Subscription {
        owner: owner.into(),
        repo: repo.into(),
        channel: crate::config::Channel::default(),
        disabled: false,
        installed_version: None,
        installed_asset: None,
        installed_at: None,
        cached_tag: None,
        cached_at: None,
        cached_published_at: None,
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
        vec![("alice".into(), "one".into(), "v9.9.9".into(), 9_000)],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    let alice = &loaded.subscriptions[0];
    let bob = &loaded.subscriptions[1];
    assert_eq!(alice.cached_tag.as_deref(), Some("v9.9.9"));
    assert_eq!(alice.cached_at, Some(12_345));
    assert_eq!(alice.cached_published_at, Some(9_000));
    assert!(bob.cached_tag.is_none());
    assert!(bob.cached_at.is_none());
    assert!(bob.cached_published_at.is_none());
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
        vec![("ghost".into(), "missing".into(), "v0.0.1".into(), 10)],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded.subscriptions.len(), 1);
    assert!(loaded.subscriptions[0].cached_tag.is_none());
    assert!(loaded.subscriptions[0].cached_at.is_none());
    assert!(loaded.subscriptions[0].cached_published_at.is_none());
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
        vec![("alice".into(), "one".into(), "v1.0.0".into(), 1)],
    )
    .unwrap();
    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded, Config::default());
}

#[test]
fn installed_version_writes_targeted_subscription() {
    let cfg = Config {
        subscriptions: vec![sub("alice", "one"), sub("bob", "two")],
    };
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
    let alice = &loaded.subscriptions[0];
    let bob = &loaded.subscriptions[1];
    assert_eq!(alice.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(alice.installed_asset.as_deref(), Some("one.so"));
    assert_eq!(alice.installed_at, Some(500));
    assert_eq!(alice.cached_tag.as_deref(), Some("v1.0.0"));
    assert_eq!(alice.cached_at, Some(999));
    assert_eq!(alice.cached_published_at, Some(500));
    assert!(bob.installed_version.is_none());
    assert!(bob.installed_asset.is_none());
    assert!(bob.installed_at.is_none());
    assert!(bob.cached_tag.is_none());
}

#[test]
fn installed_version_unknown_owner_repo_skipped() {
    let cfg = Config {
        subscriptions: vec![sub("alice", "one")],
    };
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
    assert_eq!(loaded.subscriptions.len(), 1);
    assert!(loaded.subscriptions[0].installed_version.is_none());
    assert!(loaded.subscriptions[0].installed_asset.is_none());
    assert!(loaded.subscriptions[0].installed_at.is_none());
}

#[test]
fn persist_helpers_ignore_disabled_flag() {
    // Disabling is enforced at the call site (auto-check loop, /update commands).
    // The persist helpers stay dumb: if a row is in the updates list, they
    // write to it regardless of `disabled`. This documents that contract.
    let cfg = Config {
        subscriptions: vec![Subscription {
            disabled: true,
            ..sub("alice", "one")
        }],
    };
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();

    persist_cache_updates_to(
        f.path(),
        77,
        vec![("alice".into(), "one".into(), "v2.0.0".into(), 200)],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    assert!(loaded.subscriptions[0].disabled);
    assert_eq!(
        loaded.subscriptions[0].cached_tag.as_deref(),
        Some("v2.0.0")
    );
    assert_eq!(loaded.subscriptions[0].cached_at, Some(77));
    assert_eq!(loaded.subscriptions[0].cached_published_at, Some(200));
}

#[test]
fn installed_version_missing_config_file_writes_empty_default() {
    let dir = tempfile::tempdir().unwrap();
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
