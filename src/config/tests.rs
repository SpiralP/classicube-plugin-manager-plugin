use tempfile::NamedTempFile;

use super::*;

fn sub(owner: &str, repo: &str) -> Subscription {
    Subscription {
        owner: owner.into(),
        repo: repo.into(),
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
fn fresh_within_ttl() {
    let s = Subscription {
        cached_tag: Some("v1.0.0".into()),
        cached_at: Some(100),
        cached_published_at: Some(50),
        ..sub("a", "b")
    };
    assert_eq!(s.fresh_cached_release(150, 100), Some(("v1.0.0", 50)));
}

#[test]
fn fresh_at_ttl_boundary_is_stale() {
    // The check is strict `<`, so equal-to-TTL counts as expired.
    let s = Subscription {
        cached_tag: Some("v1.0.0".into()),
        cached_at: Some(100),
        cached_published_at: Some(50),
        ..sub("a", "b")
    };
    assert_eq!(s.fresh_cached_release(200, 100), None);
}

#[test]
fn fresh_with_clock_skew() {
    // saturating_sub avoids panicking when `now < cached_at`; treat as fresh.
    let s = Subscription {
        cached_tag: Some("v1.0.0".into()),
        cached_at: Some(500),
        cached_published_at: Some(50),
        ..sub("a", "b")
    };
    assert_eq!(s.fresh_cached_release(100, 60), Some(("v1.0.0", 50)));
}

#[test]
fn missing_cached_tag_is_stale() {
    let s = Subscription {
        cached_tag: None,
        cached_at: Some(100),
        cached_published_at: Some(50),
        ..sub("a", "b")
    };
    assert_eq!(s.fresh_cached_release(150, 100), None);
}

#[test]
fn missing_cached_at_is_stale() {
    let s = Subscription {
        cached_tag: Some("v1.0.0".into()),
        cached_at: None,
        cached_published_at: Some(50),
        ..sub("a", "b")
    };
    assert_eq!(s.fresh_cached_release(150, 100), None);
}

#[test]
fn missing_cached_published_at_is_stale() {
    // Without the timestamp the cached tag is useless for the install
    // decision, so treat it as stale and force a refetch.
    let s = Subscription {
        cached_tag: Some("v1.0.0".into()),
        cached_at: Some(100),
        cached_published_at: None,
        ..sub("a", "b")
    };
    assert_eq!(s.fresh_cached_release(150, 100), None);
}

#[test]
fn load_missing_file_yields_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does-not-exist.toml");
    let cfg = Config::load_from(&path).unwrap();
    assert_eq!(cfg, Config::default());
}

#[test]
fn load_malformed_file_errors() {
    let mut f = NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut f, b"this is not = valid ::: toml [[[").unwrap();
    let err = Config::load_from(f.path()).unwrap_err();
    let chain = format!("{err:#}");
    assert!(chain.contains("parsing"), "expected 'parsing' in: {chain}");
}

#[test]
fn round_trip_default_config() {
    let f = NamedTempFile::new().unwrap();
    Config::default().save_to(f.path()).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, Config::default());
}

#[test]
fn round_trip_populated_subscription() {
    let cfg = Config {
        subscriptions: vec![Subscription {
            owner: "octocat".into(),
            repo: "hello-world".into(),
            disabled: false,
            installed_version: Some("v1.2.3".into()),
            installed_asset: Some("hello-world.so".into()),
            installed_at: Some(1_700_000_000),
            cached_tag: Some("v1.2.4".into()),
            cached_at: Some(1_700_000_500),
            cached_published_at: Some(1_700_000_400),
        }],
    };
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn bare_subscription_skips_optional_fields_in_toml() {
    let cfg = Config {
        subscriptions: vec![sub("octocat", "hello-world")],
    };
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(!on_disk.contains("disabled"));
    assert!(!on_disk.contains("installed_version"));
    assert!(!on_disk.contains("installed_asset"));
    assert!(!on_disk.contains("installed_at"));
    assert!(!on_disk.contains("cached_tag"));
    assert!(!on_disk.contains("cached_at"));
    assert!(!on_disk.contains("cached_published_at"));
    // Round-trip still works.
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn disabled_round_trip() {
    let cfg = Config {
        subscriptions: vec![Subscription {
            disabled: true,
            ..sub("octocat", "hello-world")
        }],
    };
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        on_disk.contains("disabled = true"),
        "expected `disabled = true` in: {on_disk}",
    );
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn disabled_default_when_missing_from_toml() {
    // Older configs (written before this field existed) must continue to load.
    let mut f = NamedTempFile::new().unwrap();
    std::io::Write::write_all(
        &mut f,
        b"[[subscriptions]]\nowner = \"octocat\"\nrepo = \"hello-world\"\n",
    )
    .unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded.subscriptions.len(), 1);
    assert!(!loaded.subscriptions[0].disabled);
}

#[test]
fn legacy_config_without_timestamps_loads() {
    // A config written by an older version of the plugin lacks
    // installed_at / cached_published_at. Loading must still succeed; the
    // missing fields default to None and trigger a reinstall on next check.
    let mut f = NamedTempFile::new().unwrap();
    std::io::Write::write_all(
        &mut f,
        b"[[subscriptions]]
owner = \"octocat\"
repo = \"hello-world\"
installed_version = \"v1.2.3\"
installed_asset = \"hello-world.so\"
cached_tag = \"v1.2.3\"
cached_at = 1700000000
",
    )
    .unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    let sub = &loaded.subscriptions[0];
    assert_eq!(sub.installed_version.as_deref(), Some("v1.2.3"));
    assert!(sub.installed_at.is_none());
    assert!(sub.cached_published_at.is_none());
}
