use std::fs;

use tempfile::tempdir;

use super::{ApiVersionCheck, check_api_version, detect_collision_in};

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
