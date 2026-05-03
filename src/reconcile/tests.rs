use std::fs;

use tempfile::tempdir;

use super::*;
use crate::config::{Channel, SELF_OWNER, SELF_REPO, Subscription};

fn sub(owner: &str, repo: &str) -> Subscription {
    Subscription {
        owner: owner.into(),
        repo: repo.into(),
        channel: Channel::default(),
        disabled: false,
        installed_version: None,
        installed_asset: None,
        installed_at: None,
        cached_tag: None,
        cached_at: None,
        cached_published_at: None,
    }
}

fn write_config(path: &Path, cfg: &Config) {
    cfg.save_to(path).unwrap();
}

fn touch(dir: &Path, name: &str) {
    fs::write(dir.join(name), b"").unwrap();
}

#[test]
fn missing_config_and_missing_dir_yield_empty_report() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("plugin-updater.toml");
    let managed = dir.path().join("does-not-exist");

    let report = reconcile(&cfg_path, &managed).unwrap();

    assert_eq!(report, ReconcileReport::default());
    assert!(!cfg_path.exists(), "reconcile must not create the config");
}

#[test]
fn sub_without_installed_asset_is_ignored() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![sub("a", "b")],
        },
    );

    let report = reconcile(&cfg_path, &managed).unwrap();

    assert!(report.missing.is_empty());
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    assert_eq!(after.subscriptions, vec![sub("a", "b")]);
}

#[test]
fn file_present_for_sub_is_claimed_and_no_changes() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "lib.so");

    let original = Subscription {
        installed_version: Some("v1.0.0".into()),
        installed_asset: Some("lib.so".into()),
        ..sub("a", "b")
    };
    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![original.clone()],
        },
    );

    let report = reconcile(&cfg_path, &managed).unwrap();

    assert!(report.missing.is_empty());
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    assert_eq!(after.subscriptions, vec![original]);
}

#[test]
fn file_absent_disables_sub_and_clears_installed_fields() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();

    let other = Subscription {
        installed_version: Some("v9.9.9".into()),
        installed_asset: Some("other.so".into()),
        ..sub("c", "d")
    };
    touch(&managed, "other.so");

    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![
                Subscription {
                    installed_version: Some("v1.0.0".into()),
                    installed_asset: Some("lib.so".into()),
                    ..sub("a", "b")
                },
                other.clone(),
            ],
        },
    );

    let report = reconcile(&cfg_path, &managed).unwrap();

    assert_eq!(
        report.missing,
        vec![MissingFile {
            owner: "a".into(),
            repo: "b".into(),
            asset: "lib.so".into(),
        }],
    );
    assert!(report.orphans.is_empty());

    let after = Config::load_from(&cfg_path).unwrap();
    assert_eq!(after.subscriptions.len(), 2);
    let a_b = &after.subscriptions[0];
    assert!(a_b.disabled);
    assert_eq!(a_b.installed_version, None);
    assert_eq!(a_b.installed_asset, None);
    // Other sub untouched.
    assert_eq!(after.subscriptions[1], other);
}

#[test]
fn orphan_file_is_reported_without_rewriting_config() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "stranger.so");

    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![sub("a", "b")],
        },
    );
    let mtime_before = fs::metadata(&cfg_path).unwrap().modified().unwrap();

    let report = reconcile(&cfg_path, &managed).unwrap();

    assert!(report.missing.is_empty());
    assert_eq!(report.orphans, vec!["stranger.so".to_string()]);
    let mtime_after = fs::metadata(&cfg_path).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after, "config must not be rewritten");
}

#[test]
fn missing_and_orphan_combined() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "stranger.so");
    touch(&managed, "kept.so");

    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![
                Subscription {
                    installed_version: Some("v1.0.0".into()),
                    installed_asset: Some("kept.so".into()),
                    ..sub("a", "b")
                },
                Subscription {
                    installed_version: Some("v2.0.0".into()),
                    installed_asset: Some("missing.so".into()),
                    ..sub("c", "d")
                },
            ],
        },
    );

    let report = reconcile(&cfg_path, &managed).unwrap();

    assert_eq!(
        report.missing,
        vec![MissingFile {
            owner: "c".into(),
            repo: "d".into(),
            asset: "missing.so".into(),
        }],
    );
    assert_eq!(report.orphans, vec!["stranger.so".to_string()]);

    let after = Config::load_from(&cfg_path).unwrap();
    let kept = &after.subscriptions[0];
    assert_eq!(kept.installed_asset.as_deref(), Some("kept.so"));
    assert!(!kept.disabled);
    let dropped = &after.subscriptions[1];
    assert!(dropped.disabled);
    assert_eq!(dropped.installed_asset, None);
    assert_eq!(dropped.installed_version, None);
}

#[test]
fn idempotent_after_reconcile_clears_missing_sub() {
    // Already-disabled sub whose file is also missing is reported once; a
    // second pass against the now-cleared state changes nothing.
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();

    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![Subscription {
                disabled: true,
                installed_version: Some("v1.0.0".into()),
                installed_asset: Some("gone.so".into()),
                ..sub("a", "b")
            }],
        },
    );

    let first = reconcile(&cfg_path, &managed).unwrap();
    assert_eq!(first.missing.len(), 1);

    let after_first = Config::load_from(&cfg_path).unwrap();
    let mtime_after_first = fs::metadata(&cfg_path).unwrap().modified().unwrap();

    let second = reconcile(&cfg_path, &managed).unwrap();
    assert!(second.missing.is_empty());
    assert!(second.orphans.is_empty());

    let after_second = Config::load_from(&cfg_path).unwrap();
    assert_eq!(after_first, after_second);
    let mtime_after_second = fs::metadata(&cfg_path).unwrap().modified().unwrap();
    assert_eq!(
        mtime_after_first, mtime_after_second,
        "second pass must not rewrite the config",
    );
}

#[test]
fn disabled_sub_claims_its_file_so_it_is_not_an_orphan() {
    // A user can disable a sub but keep the file around (planning to
    // re-enable). The file must still be considered "claimed" — otherwise
    // we'd nag them about it as an orphan every launch.
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "kept.so");

    let original = Subscription {
        disabled: true,
        installed_version: Some("v1.0.0".into()),
        installed_asset: Some("kept.so".into()),
        ..sub("a", "b")
    };
    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![original.clone()],
        },
    );
    let mtime_before = fs::metadata(&cfg_path).unwrap().modified().unwrap();

    let report = reconcile(&cfg_path, &managed).unwrap();

    assert!(report.missing.is_empty());
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    assert_eq!(after.subscriptions, vec![original]);
    let mtime_after = fs::metadata(&cfg_path).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after);
}

#[test]
fn orphans_are_sorted_for_deterministic_output() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    // Touch in a deliberately non-alphabetical order — readdir order is
    // filesystem-dependent so we shouldn't rely on it either way.
    for name in ["zeta.so", "alpha.so", "mid.so"] {
        touch(&managed, name);
    }

    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![],
        },
    );

    let report = reconcile(&cfg_path, &managed).unwrap();
    assert_eq!(
        report.orphans,
        vec![
            "alpha.so".to_string(),
            "mid.so".to_string(),
            "zeta.so".to_string(),
        ],
    );
}

#[test]
fn every_sub_missing_disables_all_and_reports_all() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();

    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![
                Subscription {
                    installed_version: Some("v1".into()),
                    installed_asset: Some("a.so".into()),
                    ..sub("o1", "r1")
                },
                Subscription {
                    installed_version: Some("v2".into()),
                    installed_asset: Some("b.so".into()),
                    ..sub("o2", "r2")
                },
            ],
        },
    );

    let report = reconcile(&cfg_path, &managed).unwrap();

    assert_eq!(report.missing.len(), 2);
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    assert!(after.subscriptions.iter().all(|s| s.disabled));
    assert!(
        after
            .subscriptions
            .iter()
            .all(|s| s.installed_asset.is_none() && s.installed_version.is_none())
    );
}

#[test]
fn self_subscription_is_skipped() {
    // The self subscription installs into plugins/, not plugins/managed/, so
    // reconcile must not flag it as missing or clear its installed fields.
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();

    let original = Subscription {
        installed_version: Some("v1.0.0".into()),
        installed_asset: Some("plugin_updater.so".into()),
        installed_at: Some(1_700_000_000),
        ..sub(SELF_OWNER, SELF_REPO)
    };
    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![original.clone()],
        },
    );
    let mtime_before = fs::metadata(&cfg_path).unwrap().modified().unwrap();

    let report = reconcile(&cfg_path, &managed).unwrap();

    assert!(
        report.missing.is_empty(),
        "self should not be flagged missing: {:?}",
        report.missing
    );
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    assert_eq!(after.subscriptions, vec![original]);
    let mtime_after = fs::metadata(&cfg_path).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "config must not be rewritten when only the self sub would have triggered it"
    );
}

#[test]
fn directories_in_managed_are_not_treated_as_files() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    fs::create_dir(managed.join("subdir")).unwrap();

    write_config(
        &cfg_path,
        &Config {
            subscriptions: vec![],
        },
    );

    let report = reconcile(&cfg_path, &managed).unwrap();
    assert!(report.orphans.is_empty(), "subdir should not be an orphan");
}
