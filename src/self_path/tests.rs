use std::fs;

use tempfile::tempdir;

use super::*;

#[test]
fn current_lib_path_resolves_to_existing_file() {
    // In a `cargo test` run, dladdr / GetModuleHandleExW resolve to the test
    // binary itself rather than the cdylib — but it should still return some
    // valid, existing path on disk. This is a smoke test that the platform
    // FFI is wired up; the real cdylib resolution is exercised by the run.
    let path = current_lib_path().expect("current_lib_path returned an error");
    assert!(
        path.exists(),
        "current_lib_path returned non-existent path: {}",
        path.display()
    );
}

#[test]
fn rename_legacy_binary_swaps_release_asset_name() {
    // Mirrors the v3 release asset shape: classicube_plugin_updater_<os>_<arch>.<ext>.
    let dir = tempdir().unwrap();
    let legacy = dir.path().join("classicube_plugin_updater_linux_x86_64.so");
    let new = dir.path().join("classicube_plugin_manager_linux_x86_64.so");
    fs::write(&legacy, b"contents").unwrap();

    rename_legacy_binary_at(
        &legacy,
        "classicube_plugin_updater",
        "classicube_plugin_manager",
    )
    .unwrap();

    assert!(!legacy.exists());
    assert!(new.exists());
    assert_eq!(fs::read(&new).unwrap(), b"contents");
}

#[test]
fn rename_legacy_binary_handles_rustc_default_name_too() {
    // A user who built from source has libclassicube_plugin_updater_plugin.so;
    // the substring swap still produces a sensible new name.
    let dir = tempdir().unwrap();
    let legacy = dir.path().join("libclassicube_plugin_updater_plugin.so");
    let new = dir.path().join("libclassicube_plugin_manager_plugin.so");
    fs::write(&legacy, b"contents").unwrap();

    rename_legacy_binary_at(
        &legacy,
        "classicube_plugin_updater",
        "classicube_plugin_manager",
    )
    .unwrap();

    assert!(!legacy.exists());
    assert!(new.exists());
}

#[test]
fn rename_legacy_binary_no_op_when_new_path_exists() {
    let dir = tempdir().unwrap();
    let legacy = dir.path().join("classicube_plugin_updater_linux_x86_64.so");
    let new = dir.path().join("classicube_plugin_manager_linux_x86_64.so");
    fs::write(&legacy, b"old").unwrap();
    fs::write(&new, b"new").unwrap();

    rename_legacy_binary_at(
        &legacy,
        "classicube_plugin_updater",
        "classicube_plugin_manager",
    )
    .unwrap();

    assert!(legacy.exists(), "legacy left in place when new exists");
    assert_eq!(fs::read(&new).unwrap(), b"new");
}

#[test]
fn rename_legacy_binary_no_op_when_basename_does_not_match() {
    let dir = tempdir().unwrap();
    let unrelated = dir.path().join("libsomething_else.so");
    fs::write(&unrelated, b"contents").unwrap();

    rename_legacy_binary_at(
        &unrelated,
        "classicube_plugin_updater",
        "classicube_plugin_manager",
    )
    .unwrap();

    assert!(unrelated.exists());
}
