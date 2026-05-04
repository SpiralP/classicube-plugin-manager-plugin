use std::{collections::BTreeMap, fs, panic};

use tempfile::{NamedTempFile, tempdir};

use super::{ApiVersionCheck, check_api_version, detect_collision_in, with_breadcrumb_at};
use crate::config::{Config, Subscription};

#[test]
fn missing_dir_returns_none() {
    let dir = tempdir().unwrap();
    let nonexistent = dir.path().join("does-not-exist");
    let result = detect_collision_in(&nonexistent, "plugin.so").unwrap();
    assert!(result.is_none());
}

#[test]
fn missing_file_returns_none() {
    let dir = tempdir().unwrap();
    let result = detect_collision_in(dir.path(), "plugin.so").unwrap();
    assert!(result.is_none());
}

#[test]
fn existing_file_returns_path() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("plugin.so"), b"x").unwrap();
    let result = detect_collision_in(dir.path(), "plugin.so").unwrap();
    assert_eq!(
        result.as_deref(),
        Some(dir.path().join("plugin.so").as_path())
    );
}

#[test]
fn directory_with_same_name_does_not_collide() {
    // ClassiCube loads files, not directories, so a directory of the same
    // name shouldn't trigger a double-load warning.
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("plugin.so")).unwrap();
    let result = detect_collision_in(dir.path(), "plugin.so").unwrap();
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
