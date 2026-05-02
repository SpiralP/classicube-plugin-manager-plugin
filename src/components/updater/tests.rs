use tempfile::NamedTempFile;

use super::*;

fn sub(owner: &str, repo: &str) -> Subscription {
    Subscription {
        owner: owner.into(),
        repo: repo.into(),
        installed_version: None,
        installed_asset: None,
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
        )],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    let alice = &loaded.subscriptions[0];
    let bob = &loaded.subscriptions[1];
    assert_eq!(alice.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(alice.installed_asset.as_deref(), Some("one.so"));
    assert_eq!(alice.cached_tag.as_deref(), Some("v1.0.0"));
    assert_eq!(alice.cached_at, Some(999));
    assert!(bob.installed_version.is_none());
    assert!(bob.installed_asset.is_none());
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
        )],
    )
    .unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded.subscriptions.len(), 1);
    assert!(loaded.subscriptions[0].installed_version.is_none());
    assert!(loaded.subscriptions[0].installed_asset.is_none());
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
        )],
    )
    .unwrap();
    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded, Config::default());
}
