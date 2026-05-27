//! Atomic file write via tempfile + rename.
//!
//! Three production call sites used this pattern with slight variations
//! (`Config::save_to`, `StateFile::save_to`, `breadcrumb::write`); this
//! module factors it out behind two named functions so the durability
//! choice is explicit at the call site rather than buried in a bool flag.

use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result};
use tempfile::NamedTempFile;

/// Atomically write `bytes` to `path` via tmpfile + rename. Creates the
/// parent directory if missing. fsyncs the file and (best-effort) the
/// parent directory, so the rename survives power loss on filesystems
/// that need it (ext4, xfs). Use this for any state that must survive an
/// unclean shutdown.
pub fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    write_inner(path, bytes, true)
}

/// Like [`write_synced`] but skips fsync on both the file and the parent
/// directory. Use this only on hot paths where crash-only durability is
/// sufficient - e.g. crash breadcrumbs, where power loss between the
/// rename and the crash is an acceptable failure mode.
pub fn write_unsynced(path: &Path, bytes: &[u8]) -> Result<()> {
    write_inner(path, bytes, false)
}

fn write_inner(path: &Path, bytes: &[u8], sync: bool) -> Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("creating directory {}", parent.display()))?;
    let mut tmp = NamedTempFile::new_in(parent)
        .with_context(|| format!("creating tmp file in {}", parent.display()))?;
    tmp.write_all(bytes)
        .with_context(|| format!("writing {}", tmp.path().display()))?;
    if sync {
        tmp.as_file()
            .sync_all()
            .with_context(|| format!("fsync {}", tmp.path().display()))?;
    }
    tmp.persist(path)
        .with_context(|| format!("renaming tmp -> {}", path.display()))?;
    if sync && let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn synced_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.txt");
        write_synced(&path, b"hello synced").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello synced");
    }

    #[test]
    fn unsynced_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("b.txt");
        write_unsynced(&path, b"hello unsynced").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello unsynced");
    }

    #[test]
    fn creates_missing_parent_dir() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/deeper/c.txt");
        write_synced(&path, b"in a nested dir").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"in a nested dir");
        assert!(path.parent().unwrap().is_dir());
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("d.txt");
        write_synced(&path, b"first").unwrap();
        write_synced(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
    }

    #[test]
    fn parent_is_cwd_when_path_has_no_parent_component() {
        // `Path::new("foo.txt").parent()` returns Some("") - the empty-component
        // edge case. The helper should fall back to "." rather than panic or
        // try to create a directory with an empty name.
        let dir = tempdir().unwrap();
        let prev_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = write_synced(Path::new("bare.txt"), b"bare");
        // Restore cwd before asserting so a failure doesn't poison other tests.
        std::env::set_current_dir(prev_cwd).unwrap();
        result.unwrap();
        assert_eq!(fs::read(dir.path().join("bare.txt")).unwrap(), b"bare");
    }
}
