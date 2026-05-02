use std::fs;

use tempfile::tempdir;

use super::*;

#[test]
fn fresh_install_writes_file_with_no_artifacts() {
    let dir = tempdir().unwrap();
    let path = install_bytes_to(dir.path(), "plugin.so", b"hello").unwrap();

    assert_eq!(path, dir.path().join("plugin.so"));
    assert_eq!(fs::read(&path).unwrap(), b"hello");

    // No .new / .old leftovers from a successful install.
    assert!(!dir.path().join("plugin.so.new").exists());
    assert!(!dir.path().join("plugin.so.old").exists());
}

#[test]
fn replaces_existing_file_and_cleans_up() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("plugin.so"), b"old").unwrap();

    install_bytes_to(dir.path(), "plugin.so", b"new").unwrap();

    assert_eq!(fs::read(dir.path().join("plugin.so")).unwrap(), b"new");
    assert!(!dir.path().join("plugin.so.new").exists());
    assert!(!dir.path().join("plugin.so.old").exists());
}

#[test]
fn creates_missing_parent_directory() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("a").join("b");
    assert!(!nested.exists());

    let path = install_bytes_to(&nested, "plugin.so", b"data").unwrap();

    assert!(nested.is_dir());
    assert_eq!(fs::read(&path).unwrap(), b"data");
}

#[test]
fn overwrites_stale_new_artifact() {
    // A previous failed run could have left a .new behind; the next install
    // should overwrite it cleanly.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("plugin.so.new"), b"stale-new").unwrap();

    install_bytes_to(dir.path(), "plugin.so", b"fresh").unwrap();

    assert_eq!(fs::read(dir.path().join("plugin.so")).unwrap(), b"fresh");
    assert!(!dir.path().join("plugin.so.new").exists());
}

#[test]
fn cleans_up_leftover_old_artifact() {
    // A previous failed run could have left a .old behind. The current run
    // moves the existing final file aside (overwriting the stale .old via
    // rename), then removes .old at the end.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("plugin.so"), b"existing").unwrap();
    fs::write(dir.path().join("plugin.so.old"), b"stale-old").unwrap();

    install_bytes_to(dir.path(), "plugin.so", b"new").unwrap();

    assert_eq!(fs::read(dir.path().join("plugin.so")).unwrap(), b"new");
    assert!(!dir.path().join("plugin.so.old").exists());
}
