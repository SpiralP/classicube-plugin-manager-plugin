use std::{collections::BTreeMap, fs};

use tempfile::tempdir;

use super::*;
use crate::config::{Channel, SELF_OWNER, SELF_REPO, Subscription, SubscriptionState};

fn empty_sub() -> Subscription {
    Subscription {
        channel: Channel::default(),
        disabled: false,
        token: None,
        state: SubscriptionState::default(),
    }
}

fn config_with(entries: Vec<(&str, &str, Subscription)>) -> Config {
    let mut subscriptions: BTreeMap<String, BTreeMap<String, Subscription>> = BTreeMap::new();
    for (owner, repo, sub) in entries {
        subscriptions
            .entry(owner.into())
            .or_default()
            .insert(repo.into(), sub);
    }
    Config { subscriptions }
}

fn write_config(path: &Path, cfg: &Config) {
    cfg.save_to(path).unwrap();
}

fn touch(dir: &Path, name: &str) {
    fs::write(dir.join(name), b"").unwrap();
}

fn pick<'a>(cfg: &'a Config, owner: &str, repo: &str) -> &'a Subscription {
    cfg.subscriptions.get(owner).unwrap().get(repo).unwrap()
}

/// Wrapper used by tests that don't care about the plugins/ scan or
/// self-running-binary exclusion. Points `plugins_dir` at a path that
/// doesn't exist - `list_dir_files` returns empty for ENOENT, so the
/// behavior matches a blank dir without forcing every test to mkdir.
fn rec(cfg_path: &Path, managed: &Path) -> Result<ReconcileReport> {
    reconcile(
        cfg_path,
        &managed.with_file_name("__no_plugins_dir__"),
        managed,
        ".so",
        None,
    )
}

#[test]
fn missing_config_and_missing_dir_yield_empty_report() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("plugin-manager.toml");
    let managed = dir.path().join("does-not-exist");

    let report = rec(&cfg_path, &managed).unwrap();

    assert_eq!(report, ReconcileReport::default());
    assert!(!cfg_path.exists(), "reconcile must not create the config");
}

#[test]
fn sub_without_installed_asset_is_ignored() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    let cfg = config_with(vec![("a", "b", empty_sub())]);
    write_config(&cfg_path, &cfg);

    let report = rec(&cfg_path, &managed).unwrap();

    assert!(report.missing.is_empty());
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    assert_eq!(after, cfg);
}

#[test]
fn file_present_for_sub_is_claimed_and_no_changes() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "lib.so");

    let original = Subscription {
        state: SubscriptionState {
            installed_version: Some("v1.0.0".into()),
            installed_asset: Some("lib.so".into()),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    let cfg = config_with(vec![("a", "b", original.clone())]);
    write_config(&cfg_path, &cfg);

    let report = rec(&cfg_path, &managed).unwrap();

    assert!(report.missing.is_empty());
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    assert_eq!(pick(&after, "a", "b"), &original);
}

#[test]
fn file_absent_disables_sub_and_clears_installed_fields() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();

    let other = Subscription {
        state: SubscriptionState {
            installed_version: Some("v9.9.9".into()),
            installed_asset: Some("other.so".into()),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    touch(&managed, "other.so");

    let cfg = config_with(vec![
        (
            "a",
            "b",
            Subscription {
                state: SubscriptionState {
                    installed_version: Some("v1.0.0".into()),
                    installed_asset: Some("lib.so".into()),
                    ..SubscriptionState::default()
                },
                ..empty_sub()
            },
        ),
        ("c", "d", other.clone()),
    ]);
    write_config(&cfg_path, &cfg);

    let report = rec(&cfg_path, &managed).unwrap();

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
    let a_b = pick(&after, "a", "b");
    assert!(a_b.disabled);
    assert_eq!(a_b.state.installed_version, None);
    assert_eq!(a_b.state.installed_asset, None);
    // Other sub untouched.
    assert_eq!(pick(&after, "c", "d"), &other);
}

#[test]
fn orphan_file_is_reported_without_rewriting_config() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "stranger.so");

    let cfg = config_with(vec![("a", "b", empty_sub())]);
    write_config(&cfg_path, &cfg);
    let mtime_before = fs::metadata(&cfg_path).unwrap().modified().unwrap();

    let report = rec(&cfg_path, &managed).unwrap();

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

    let cfg = config_with(vec![
        (
            "a",
            "b",
            Subscription {
                state: SubscriptionState {
                    installed_version: Some("v1.0.0".into()),
                    installed_asset: Some("kept.so".into()),
                    ..SubscriptionState::default()
                },
                ..empty_sub()
            },
        ),
        (
            "c",
            "d",
            Subscription {
                state: SubscriptionState {
                    installed_version: Some("v2.0.0".into()),
                    installed_asset: Some("missing.so".into()),
                    ..SubscriptionState::default()
                },
                ..empty_sub()
            },
        ),
    ]);
    write_config(&cfg_path, &cfg);

    let report = rec(&cfg_path, &managed).unwrap();

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
    let kept = pick(&after, "a", "b");
    assert_eq!(kept.state.installed_asset.as_deref(), Some("kept.so"));
    assert!(!kept.disabled);
    let dropped = pick(&after, "c", "d");
    assert!(dropped.disabled);
    assert_eq!(dropped.state.installed_asset, None);
    assert_eq!(dropped.state.installed_version, None);
}

#[test]
fn idempotent_after_reconcile_clears_missing_sub() {
    // Already-disabled sub whose file is also missing is reported once; a
    // second pass against the now-cleared state changes nothing.
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();

    let cfg = config_with(vec![(
        "a",
        "b",
        Subscription {
            disabled: true,
            state: SubscriptionState {
                installed_version: Some("v1.0.0".into()),
                installed_asset: Some("gone.so".into()),
                ..SubscriptionState::default()
            },
            ..empty_sub()
        },
    )]);
    write_config(&cfg_path, &cfg);

    let first = rec(&cfg_path, &managed).unwrap();
    assert_eq!(first.missing.len(), 1);

    let after_first = Config::load_from(&cfg_path).unwrap();
    let mtime_after_first = fs::metadata(&cfg_path).unwrap().modified().unwrap();

    let second = rec(&cfg_path, &managed).unwrap();
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
        state: SubscriptionState {
            installed_version: Some("v1.0.0".into()),
            installed_asset: Some("kept.so".into()),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    let cfg = config_with(vec![("a", "b", original.clone())]);
    write_config(&cfg_path, &cfg);
    let mtime_before = fs::metadata(&cfg_path).unwrap().modified().unwrap();

    let report = rec(&cfg_path, &managed).unwrap();

    assert!(report.missing.is_empty());
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    assert_eq!(pick(&after, "a", "b"), &original);
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

    write_config(&cfg_path, &Config::default());

    let report = rec(&cfg_path, &managed).unwrap();
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

    let cfg = config_with(vec![
        (
            "o1",
            "r1",
            Subscription {
                state: SubscriptionState {
                    installed_version: Some("v1".into()),
                    installed_asset: Some("a.so".into()),
                    ..SubscriptionState::default()
                },
                ..empty_sub()
            },
        ),
        (
            "o2",
            "r2",
            Subscription {
                state: SubscriptionState {
                    installed_version: Some("v2".into()),
                    installed_asset: Some("b.so".into()),
                    ..SubscriptionState::default()
                },
                ..empty_sub()
            },
        ),
    ]);
    write_config(&cfg_path, &cfg);

    let report = rec(&cfg_path, &managed).unwrap();

    assert_eq!(report.missing.len(), 2);
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    let all_subs: Vec<&Subscription> = after
        .subscriptions
        .values()
        .flat_map(BTreeMap::values)
        .collect();
    assert_eq!(all_subs.len(), 2);
    assert!(all_subs.iter().all(|s| s.disabled));
    assert!(
        all_subs
            .iter()
            .all(|s| s.state.installed_asset.is_none() && s.state.installed_version.is_none())
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
        state: SubscriptionState {
            installed_version: Some("v1.0.0".into()),
            installed_asset: Some("plugin_manager.so".into()),
            installed_at: Some(1_700_000_000),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    let cfg = config_with(vec![(SELF_OWNER, SELF_REPO, original.clone())]);
    write_config(&cfg_path, &cfg);
    let mtime_before = fs::metadata(&cfg_path).unwrap().modified().unwrap();

    let report = rec(&cfg_path, &managed).unwrap();

    assert!(
        report.missing.is_empty(),
        "self should not be flagged missing: {:?}",
        report.missing
    );
    assert!(report.orphans.is_empty());
    let after = Config::load_from(&cfg_path).unwrap();
    assert_eq!(pick(&after, SELF_OWNER, SELF_REPO), &original);
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

    write_config(&cfg_path, &Config::default());

    let report = rec(&cfg_path, &managed).unwrap();
    assert!(report.orphans.is_empty(), "subdir should not be an orphan");
}

#[test]
fn variant_in_managed_demotes_orphan_to_conflict() {
    // User manually built the plugin (cargo cdylib output) and dropped it
    // into plugins/managed/. That's a duplicate-load hazard with whatever the
    // canonical asset name is, so we surface it as a conflict, not an orphan.
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let plugins = dir.path().join("plugins");
    let managed = dir.path().join("managed");
    fs::create_dir(&plugins).unwrap();
    fs::create_dir(&managed).unwrap();
    touch(&managed, "libclassicube_foo_plugin.so");

    let cfg = config_with(vec![(
        "owner",
        "classicube-foo-plugin",
        Subscription {
            state: SubscriptionState {
                installed_version: Some("v1.0.0".into()),
                installed_asset: Some("classicube-foo-plugin.so".into()),
                ..SubscriptionState::default()
            },
            ..empty_sub()
        },
    )]);
    write_config(&cfg_path, &cfg);

    let report = reconcile(&cfg_path, &plugins, &managed, ".so", None).unwrap();

    assert!(report.orphans.is_empty(), "{:?}", report.orphans);
    assert_eq!(
        report.conflicts,
        vec![Conflict {
            dir: ConflictDir::Managed,
            filename: "libclassicube_foo_plugin.so".into(),
            owner: "owner".into(),
            repo: "classicube-foo-plugin".into(),
            installed_asset: Some("classicube-foo-plugin.so".into()),
        }],
    );
    // Sub itself shows missing because installed_asset (canonical name) wasn't
    // on disk; only the variant was. Reconcile disables it as usual.
    assert_eq!(report.missing.len(), 1);
}

#[test]
fn installed_asset_filename_in_plugins_dir_is_a_conflict() {
    // A copy of the actual release asset filename in plugins/ alongside the
    // managed copy is a duplicate-load. matches_repo's shape rules don't
    // catch e.g. `classicube_foo_linux_x86_64.so`, so we fall back to
    // exact-filename equality with installed_asset.
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let plugins = dir.path().join("plugins");
    let managed = dir.path().join("managed");
    fs::create_dir(&plugins).unwrap();
    fs::create_dir(&managed).unwrap();
    touch(&plugins, "classicube_foo_linux_x86_64.so");
    touch(&managed, "classicube_foo_linux_x86_64.so");

    let cfg = config_with(vec![(
        "owner",
        "classicube-foo-plugin",
        Subscription {
            state: SubscriptionState {
                installed_version: Some("v1.0.0".into()),
                installed_asset: Some("classicube_foo_linux_x86_64.so".into()),
                ..SubscriptionState::default()
            },
            ..empty_sub()
        },
    )]);
    write_config(&cfg_path, &cfg);

    let report = reconcile(&cfg_path, &plugins, &managed, ".so", None).unwrap();

    assert!(report.missing.is_empty());
    assert!(report.orphans.is_empty());
    assert_eq!(
        report.conflicts,
        vec![Conflict {
            dir: ConflictDir::Plugins,
            filename: "classicube_foo_linux_x86_64.so".into(),
            owner: "owner".into(),
            repo: "classicube-foo-plugin".into(),
            installed_asset: Some("classicube_foo_linux_x86_64.so".into()),
        }],
    );
}

#[test]
fn variant_in_plugins_dir_is_a_conflict() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let plugins = dir.path().join("plugins");
    let managed = dir.path().join("managed");
    fs::create_dir(&plugins).unwrap();
    fs::create_dir(&managed).unwrap();
    touch(&plugins, "libclassicube_foo_plugin.so");
    touch(&managed, "classicube-foo-plugin.so");

    let cfg = config_with(vec![(
        "owner",
        "classicube-foo-plugin",
        Subscription {
            state: SubscriptionState {
                installed_version: Some("v1.0.0".into()),
                installed_asset: Some("classicube-foo-plugin.so".into()),
                ..SubscriptionState::default()
            },
            ..empty_sub()
        },
    )]);
    write_config(&cfg_path, &cfg);

    let report = reconcile(&cfg_path, &plugins, &managed, ".so", None).unwrap();

    assert!(report.missing.is_empty());
    assert!(report.orphans.is_empty());
    assert_eq!(
        report.conflicts,
        vec![Conflict {
            dir: ConflictDir::Plugins,
            filename: "libclassicube_foo_plugin.so".into(),
            owner: "owner".into(),
            repo: "classicube-foo-plugin".into(),
            installed_asset: Some("classicube-foo-plugin.so".into()),
        }],
    );
}

#[test]
fn unrelated_files_in_plugins_dir_are_ignored() {
    // plugins/ is shared with the user's own files and unmanaged plugins.
    // Files that don't match any subscription's repo by name must not appear
    // anywhere in the report.
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let plugins = dir.path().join("plugins");
    let managed = dir.path().join("managed");
    fs::create_dir(&plugins).unwrap();
    fs::create_dir(&managed).unwrap();
    touch(&plugins, "some-other-plugin.so");
    touch(&plugins, "user-private-build.so");

    let cfg = config_with(vec![(
        "owner",
        "classicube-foo-plugin",
        Subscription {
            state: SubscriptionState {
                installed_version: Some("v1.0.0".into()),
                installed_asset: Some("classicube-foo-plugin.so".into()),
                ..SubscriptionState::default()
            },
            ..empty_sub()
        },
    )]);
    write_config(&cfg_path, &cfg);
    touch(&managed, "classicube-foo-plugin.so");

    let report = reconcile(&cfg_path, &plugins, &managed, ".so", None).unwrap();

    assert_eq!(report, ReconcileReport::default());
}

#[test]
fn self_running_basename_excluded_from_plugins_scan() {
    // The running self binary lives in plugins/ and matches the self
    // subscription's repo by name. It must be skipped or we'd spam a
    // conflict warning every startup.
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let plugins = dir.path().join("plugins");
    let managed = dir.path().join("managed");
    fs::create_dir(&plugins).unwrap();
    fs::create_dir(&managed).unwrap();
    touch(&plugins, "libclassicube_plugin_manager_plugin.so");

    let cfg = config_with(vec![(SELF_OWNER, SELF_REPO, empty_sub())]);
    write_config(&cfg_path, &cfg);

    let report = reconcile(
        &cfg_path,
        &plugins,
        &managed,
        ".so",
        Some("libclassicube_plugin_manager_plugin.so"),
    )
    .unwrap();

    assert_eq!(report, ReconcileReport::default());
}

#[test]
fn second_self_named_file_in_plugins_is_a_conflict() {
    // Running binary is libclassicube_plugin_manager_plugin.so; a stray
    // canonical-named copy alongside it would cause the game to load both
    // (one through our self path, one as an unrelated plugin).
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("c.toml");
    let plugins = dir.path().join("plugins");
    let managed = dir.path().join("managed");
    fs::create_dir(&plugins).unwrap();
    fs::create_dir(&managed).unwrap();
    touch(&plugins, "libclassicube_plugin_manager_plugin.so");
    touch(&plugins, "classicube-plugin-manager-plugin.so");

    let cfg = config_with(vec![(SELF_OWNER, SELF_REPO, empty_sub())]);
    write_config(&cfg_path, &cfg);

    let report = reconcile(
        &cfg_path,
        &plugins,
        &managed,
        ".so",
        Some("libclassicube_plugin_manager_plugin.so"),
    )
    .unwrap();

    assert_eq!(report.missing, vec![]);
    assert_eq!(report.orphans, Vec::<String>::new());
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].dir, ConflictDir::Plugins);
    assert_eq!(
        report.conflicts[0].filename,
        "classicube-plugin-manager-plugin.so"
    );
    assert_eq!(report.conflicts[0].owner, SELF_OWNER);
    assert_eq!(report.conflicts[0].repo, SELF_REPO);
}

#[test]
fn find_variant_conflicts_finds_both_dirs_and_skips() {
    let dir = tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    let managed = dir.path().join("managed");
    fs::create_dir(&plugins).unwrap();
    fs::create_dir(&managed).unwrap();
    touch(&plugins, "libclassicube_foo_plugin.so");
    touch(&managed, "classicube-foo-plugin.so");
    touch(&managed, "classicube-foo-plugin.so.tmp"); // wrong suffix, ignored
    touch(&managed, "unrelated.so");

    // No skips: both matching files come back.
    let got =
        find_variant_conflicts(&plugins, &managed, "classicube-foo-plugin", ".so", &[]).unwrap();
    assert_eq!(
        got,
        vec![
            plugins.join("libclassicube_foo_plugin.so"),
            managed.join("classicube-foo-plugin.so"),
        ]
    );

    // Skip the canonical (e.g. our installed_asset): only the variant remains.
    let got = find_variant_conflicts(
        &plugins,
        &managed,
        "classicube-foo-plugin",
        ".so",
        &["classicube-foo-plugin.so"],
    )
    .unwrap();
    assert_eq!(got, vec![plugins.join("libclassicube_foo_plugin.so")]);
}

#[test]
fn find_variant_conflicts_handles_missing_dirs() {
    // Production may run before plugins/managed/ exists; ENOENT must be
    // treated as "no files," not an error.
    let got = find_variant_conflicts(
        Path::new("/__no_such_plugins__"),
        Path::new("/__no_such_managed__"),
        "classicube-foo-plugin",
        ".so",
        &[],
    )
    .unwrap();
    assert!(got.is_empty());
}

#[test]
fn sweep_managed_orphans_deletes_unclaimed_files_only() {
    let dir = tempdir().unwrap();
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "claimed.so");
    touch(&managed, "stranger.so");
    touch(&managed, "old-version.so");

    let mut sub = empty_sub();
    sub.state.installed_asset = Some("claimed.so".into());
    let cfg = config_with(vec![("a", "b", sub)]);

    let deleted = sweep_managed_orphans(&managed, &cfg);

    assert_eq!(deleted, vec!["old-version.so", "stranger.so"]);
    assert!(managed.join("claimed.so").exists());
    assert!(!managed.join("stranger.so").exists());
    assert!(!managed.join("old-version.so").exists());
}

#[test]
fn sweep_managed_orphans_skips_new_keeps_reaping_old() {
    // `.new` files belong to an in-flight `install_bytes_to` and must not be
    // pulled out from under it. `.old` is "marked for deletion" - the
    // previous session's mapping is gone, so we always reap it, even if the
    // base name is still claimed (the live binary lives under the base
    // path, not the `.old`).
    let dir = tempdir().unwrap();
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "claimed.so");
    touch(&managed, "claimed.so.new");
    touch(&managed, "claimed.so.old");
    touch(&managed, "garbage");

    let mut sub = empty_sub();
    sub.state.installed_asset = Some("claimed.so".into());
    let cfg = config_with(vec![("a", "b", sub)]);

    let deleted = sweep_managed_orphans(&managed, &cfg);

    assert_eq!(deleted, vec!["claimed.so.old", "garbage"]);
    assert!(managed.join("claimed.so").exists());
    assert!(managed.join("claimed.so.new").exists());
    assert!(!managed.join("claimed.so.old").exists());
}

#[test]
fn sweep_managed_orphans_reaps_old_when_base_unclaimed() {
    // `.old` left behind by a prior `/remove` rename-aside, with no
    // matching subscription. Should still be reaped.
    let dir = tempdir().unwrap();
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "gone.so.old");

    let cfg = config_with(vec![]);
    let deleted = sweep_managed_orphans(&managed, &cfg);

    assert_eq!(deleted, vec!["gone.so.old"]);
    assert!(!managed.join("gone.so.old").exists());
}

#[test]
fn sweep_managed_orphans_handles_missing_dir() {
    // Production may run before plugins/managed/ exists.
    let cfg = config_with(vec![]);
    let deleted = sweep_managed_orphans(Path::new("/__no_such_managed_dir__"), &cfg);
    assert!(deleted.is_empty());
}

#[test]
fn sweep_managed_orphans_empty_config_clears_dir() {
    let dir = tempdir().unwrap();
    let managed = dir.path().join("managed");
    fs::create_dir(&managed).unwrap();
    touch(&managed, "a.so");
    touch(&managed, "b.so");

    let cfg = config_with(vec![]);
    let deleted = sweep_managed_orphans(&managed, &cfg);

    assert_eq!(deleted, vec!["a.so", "b.so"]);
    assert!(!managed.join("a.so").exists());
    assert!(!managed.join("b.so").exists());
}
