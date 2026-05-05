use std::{collections::BTreeMap, fs, panic};

use tempfile::{NamedTempFile, tempdir};

use super::{
    ApiVersionCheck, LoadOutcome, check_api_version, classify_early, detect_plugins_dir_conflict,
    with_breadcrumb_at,
};
use crate::config::{self, Config, Subscription};

#[test]
fn missing_dir_returns_none() {
    let dir = tempdir().unwrap();
    let nonexistent = dir.path().join("does-not-exist");
    let result =
        detect_plugins_dir_conflict(&nonexistent, "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[test]
fn empty_dir_returns_none() {
    let dir = tempdir().unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[test]
fn canonical_named_file_is_a_conflict() {
    // ClassiCube would load this file directly out of plugins/; if we then
    // also load the managed copy, the plugin runs twice.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("classicube-foo-plugin.so"), b"x").unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert_eq!(
        result.as_deref(),
        Some(dir.path().join("classicube-foo-plugin.so").as_path())
    );
}

#[test]
fn variant_named_file_is_a_conflict() {
    // rust-cdylib output: lib prefix + underscores. ClassiCube loads it the
    // same way as the canonical filename, so it's also a duplicate-load
    // hazard.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("libclassicube_foo_plugin.so"), b"x").unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert_eq!(
        result.as_deref(),
        Some(dir.path().join("libclassicube_foo_plugin.so").as_path())
    );
}

#[test]
fn matches_installed_asset_filename_exactly() {
    // Release-asset names like `classicube_foo_linux_x86_64.so` don't match
    // the repo via shape normalization. If the user puts a copy of that
    // exact filename in plugins/ alongside our managed copy, ClassiCube
    // would load both. The installed_asset hint catches it.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("classicube_foo_linux_x86_64.so"), b"x").unwrap();
    let result = detect_plugins_dir_conflict(
        dir.path(),
        "classicube-foo-plugin",
        ".so",
        Some("classicube_foo_linux_x86_64.so"),
    )
    .unwrap();
    assert_eq!(
        result.as_deref(),
        Some(dir.path().join("classicube_foo_linux_x86_64.so").as_path())
    );
}

#[test]
fn installed_asset_hint_does_not_match_unrelated_files() {
    // Without a name-shape match and without an installed_asset equality, a
    // file like the asset hint that's *not* on disk shouldn't surface as a
    // conflict.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("something-else.so"), b"x").unwrap();
    let result = detect_plugins_dir_conflict(
        dir.path(),
        "classicube-foo-plugin",
        ".so",
        Some("classicube_foo_linux_x86_64.so"),
    )
    .unwrap();
    assert!(result.is_none());
}

#[test]
fn unrelated_files_are_not_conflicts() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("classicube-bar-plugin.so"), b"x").unwrap();
    fs::write(dir.path().join("README.md"), b"x").unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[test]
fn directory_with_matching_name_does_not_collide() {
    // ClassiCube loads files, not directories, so a directory of the same
    // name shouldn't trigger a double-load warning.
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("classicube-foo-plugin.so")).unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[cfg(unix)]
#[test]
fn symlink_to_plugin_file_is_a_conflict() {
    // Common dev-loop pattern: `ln -s target/release/lib...so plugins/`.
    // `dlopen` follows symlinks, so we have to flag them as conflicts;
    // `DirEntry::metadata` is `lstat` and would silently drop them.
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let target = dir.path().join("real-libclassicube_foo_plugin.so");
    fs::write(&target, b"x").unwrap();
    let link = dir.path().join("libclassicube_foo_plugin.so");
    symlink(&target, &link).unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert_eq!(result.as_deref(), Some(link.as_path()));
}

#[cfg(unix)]
#[test]
fn dangling_symlink_is_skipped() {
    // A dangling symlink can't be `dlopen`'d, so it isn't a real
    // duplicate-load hazard. Skip rather than error.
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let link = dir.path().join("libclassicube_foo_plugin.so");
    symlink(dir.path().join("does-not-exist.so"), &link).unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[test]
fn api_version_equal_is_ok() {
    assert_eq!(check_api_version(1, 1), ApiVersionCheck::Ok);
}

#[test]
fn api_version_plugin_lower_is_outdated() {
    assert_eq!(check_api_version(2, 1), ApiVersionCheck::PluginOutdated);
}

#[test]
fn api_version_plugin_higher_means_host_outdated() {
    assert_eq!(check_api_version(1, 2), ApiVersionCheck::HostOutdated);
}

fn config_with_one_sub(path: &std::path::Path, owner: &str, repo: &str) {
    let mut repos = BTreeMap::new();
    repos.insert(repo.into(), Subscription::default());
    let mut subscriptions = BTreeMap::new();
    subscriptions.insert(owner.into(), repos);
    Config { subscriptions }.save_to(path).unwrap();
}

fn read_in_callback(path: &std::path::Path, owner: &str, repo: &str) -> Option<String> {
    Config::load_from(path)
        .unwrap()
        .subscriptions
        .get(owner)
        .and_then(|m| m.get(repo))
        .and_then(|s| s.state.in_callback.clone())
}

#[test]
fn breadcrumb_set_during_call_and_cleared_after() {
    let f = NamedTempFile::new().unwrap();
    config_with_one_sub(f.path(), "octocat", "hello-world");

    let mid_call = std::cell::Cell::new(None::<String>);
    with_breadcrumb_at(f.path(), "octocat", "hello-world", "OnNewMap", || {
        mid_call.set(read_in_callback(f.path(), "octocat", "hello-world"));
    });

    assert_eq!(mid_call.into_inner().as_deref(), Some("OnNewMap"));
    assert!(read_in_callback(f.path(), "octocat", "hello-world").is_none());
}

#[test]
fn breadcrumb_survives_panic_in_closure() {
    // The whole point of the breadcrumb is to survive a crash inside the
    // managed callback. A panic is the closest in-process analog: if `f`
    // panics, the post-call clear must not run, and the on-disk breadcrumb
    // must remain set so the next-startup carry-over check can fire.
    let f = NamedTempFile::new().unwrap();
    config_with_one_sub(f.path(), "octocat", "hello-world");

    let path = f.path().to_owned();
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        with_breadcrumb_at(&path, "octocat", "hello-world", "Init", || {
            panic!("simulated crash");
        })
    }));
    assert!(result.is_err(), "expected panic to propagate");
    assert_eq!(
        read_in_callback(f.path(), "octocat", "hello-world").as_deref(),
        Some("Init"),
    );
}

#[test]
fn breadcrumb_returns_closure_value() {
    let f = NamedTempFile::new().unwrap();
    config_with_one_sub(f.path(), "octocat", "hello-world");
    let n = with_breadcrumb_at(f.path(), "octocat", "hello-world", "Reset", || 42);
    assert_eq!(n, 42);
}

// `classify_early` exercises the FFI-free, side-effect-free branches of
// `load_one`. The success and dlopen-error paths need a real plugin binary
// and are out of scope for unit tests. The `unload_one` branches exposed to
// tests are similarly the ones that don't reach the platform unload call.

#[test]
fn classify_early_disabled_returns_disabled() {
    let sub = Subscription {
        disabled: true,
        ..Subscription::default()
    };
    assert!(matches!(
        classify_early("octocat", "hello-world", &sub),
        Some(LoadOutcome::Disabled)
    ));
}

#[test]
fn classify_early_self_returns_is_self() {
    // Even if the user's config has the self sub enabled and "installed", we
    // never dlopen it - the game already owns its handle.
    let sub = Subscription::default();
    assert!(matches!(
        classify_early(config::SELF_OWNER, config::SELF_REPO, &sub),
        Some(LoadOutcome::IsSelf)
    ));
}

#[test]
fn classify_early_disabled_takes_precedence_over_self() {
    // If the user disables the self sub by hand and then we also short-circuit
    // on is_self, both reasons apply; report Disabled because the disabled
    // flag is the user's explicit intent and the more actionable hint.
    let sub = Subscription {
        disabled: true,
        ..Subscription::default()
    };
    assert!(matches!(
        classify_early(config::SELF_OWNER, config::SELF_REPO, &sub),
        Some(LoadOutcome::Disabled)
    ));
}

#[test]
fn classify_early_normal_sub_returns_none() {
    // Falls through to the FFI/LOADED/filesystem checks in the full load_one.
    let sub = Subscription::default();
    assert!(classify_early("octocat", "hello-world", &sub).is_none());
}
