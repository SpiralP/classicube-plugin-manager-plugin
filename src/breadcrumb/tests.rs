use std::{fs, path::Path, process};

use tempfile::tempdir;

use super::{Entry, breadcrumb_path, clear, collect_dead, current_ns_inode, write};

#[test]
fn write_then_clear_round_trip() {
    let dir = tempdir().unwrap();
    write(dir.path(), "octocat", "hello-world", "OnNewMap").unwrap();
    let expected = breadcrumb_path(dir.path(), current_ns_inode(), process::id());
    assert!(expected.exists(), "{} should exist", expected.display());
    let body = fs::read_to_string(&expected).unwrap();
    let parsed: Entry = toml::from_str(&body).unwrap();
    assert_eq!(parsed.owner, "octocat");
    assert_eq!(parsed.repo, "hello-world");
    assert_eq!(parsed.callback, "OnNewMap");

    clear(dir.path()).unwrap();
    assert!(!expected.exists());
}

#[test]
fn write_overwrites_previous_entry() {
    let dir = tempdir().unwrap();
    write(dir.path(), "octocat", "first", "Init").unwrap();
    write(dir.path(), "octocat", "second", "OnNewMap").unwrap();
    let path = breadcrumb_path(dir.path(), current_ns_inode(), process::id());
    let parsed: Entry = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.repo, "second");
    assert_eq!(parsed.callback, "OnNewMap");
    clear(dir.path()).unwrap();
}

#[test]
fn clear_is_idempotent_when_missing() {
    let dir = tempdir().unwrap();
    clear(dir.path()).unwrap();
    clear(dir.path()).unwrap();
}

#[test]
fn write_creates_missing_dir() {
    // Production hits `BREADCRUMB_DIR = "plugins/managed/breadcrumbs"` on
    // fresh installs where the dir doesn't exist yet. `write` must
    // `create_dir_all` rather than expecting the caller to pre-create it.
    let parent = tempdir().unwrap();
    let nested = parent.path().join("a").join("b").join("breadcrumbs");
    assert!(!nested.exists());
    write(&nested, "octocat", "hello-world", "Init").unwrap();
    assert!(nested.is_dir());
    let expected = breadcrumb_path(&nested, current_ns_inode(), process::id());
    assert!(expected.exists());
}

#[test]
fn breadcrumb_filename_includes_ns_and_pid() {
    let dir = tempdir().unwrap();
    let path = breadcrumb_path(dir.path(), 4_026_534_130, 4242);
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        "4026534130-4242.toml"
    );
    assert_eq!(path.parent().unwrap(), dir.path());
}

fn seed_breadcrumb(dir: &Path, ns_inode: u64, pid: u32, entry: &Entry) -> std::path::PathBuf {
    let path = breadcrumb_path(dir, ns_inode, pid);
    fs::write(&path, toml::to_string(entry).unwrap()).unwrap();
    path
}

#[test]
fn collect_dead_consumes_file_and_unlinks() {
    let dir = tempdir().unwrap();
    let entry = Entry {
        owner: "octocat".into(),
        repo: "ghost-plugin".into(),
        callback: "OnNewMapLoaded".into(),
    };
    let path = seed_breadcrumb(dir.path(), current_ns_inode(), 999_999, &entry);

    let out = collect_dead(dir.path()).unwrap();
    assert_eq!(
        out.get(&("octocat".into(), "ghost-plugin".into()))
            .map(String::as_str),
        Some("OnNewMapLoaded")
    );
    assert!(!path.exists(), "{} should be unlinked", path.display());
}

#[test]
fn collect_dead_consumes_our_own_pid_file() {
    // A breadcrumb file at <our_ns>-<our_pid>.toml on startup is
    // necessarily stale (we haven't written one this run), and the
    // consume-everything semantics handle it the same as any other:
    // read + unlink.
    let dir = tempdir().unwrap();
    let entry = Entry {
        owner: "octocat".into(),
        repo: "recycled".into(),
        callback: "Free".into(),
    };
    let path = seed_breadcrumb(dir.path(), current_ns_inode(), process::id(), &entry);

    let out = collect_dead(dir.path()).unwrap();
    assert_eq!(
        out.get(&("octocat".into(), "recycled".into()))
            .map(String::as_str),
        Some("Free")
    );
    assert!(!path.exists());
}

#[test]
fn collect_dead_consumes_cross_namespace_file() {
    // The scan no longer cares which PID namespace produced the file -
    // a sandbox-restart with a fresh ns_inode is the common case where
    // our own previous-launch file looks "cross-namespace," so we
    // consume it instead of leaving it on disk forever.
    let dir = tempdir().unwrap();
    let other_ns = current_ns_inode().wrapping_add(12_345);
    let entry = Entry {
        owner: "octocat".into(),
        repo: "sibling-plugin".into(),
        callback: "OnNewMap".into(),
    };
    let path = seed_breadcrumb(dir.path(), other_ns, 999_999, &entry);

    let out = collect_dead(dir.path()).unwrap();
    assert_eq!(
        out.get(&("octocat".into(), "sibling-plugin".into()))
            .map(String::as_str),
        Some("OnNewMap")
    );
    assert!(!path.exists(), "{} should be unlinked", path.display());
}

#[test]
fn collect_dead_skips_unparseable_files() {
    // Files in the dir that aren't valid breadcrumb TOML get logged at
    // warn and skipped - no panic, no spurious carry-over insertion.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("README.md"), "").unwrap();
    fs::write(dir.path().join("abc-123.toml"), "").unwrap();
    fs::write(dir.path().join("1234.toml"), "not valid toml at all").unwrap();
    fs::write(dir.path().join("1234.tmp"), "").unwrap();
    fs::write(dir.path().join("1-2-3.toml"), "").unwrap();
    let out = collect_dead(dir.path()).unwrap();
    assert!(out.is_empty(), "got {out:?}");
}

#[test]
fn collect_dead_empty_when_dir_missing() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");
    let out = collect_dead(&missing).unwrap();
    assert!(out.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn current_ns_inode_is_nonzero_on_linux() {
    // Sanity check the readlink + parse path actually returns a real
    // namespace inode on Linux. The kernel emits values like
    // 4026531836 (initial PID namespace) or higher for nested ones.
    assert_ne!(current_ns_inode(), 0, "ns_inode parse should succeed");
}
