//! Per-process crash-recovery breadcrumb.
//!
//! On entering each managed-plugin IGameComponent callback the loader
//! writes a file `<dir>/<ns_inode>-<pid>.toml` (production: `BREADCRUMB_DIR`
//! is `plugins/managed/breadcrumbs/`); on exit it deletes the file. If
//! the process dies inside the callback the file is left behind, and
//! the next startup picks it up via [`collect_dead`], which reads every
//! parseable breadcrumb file in the dir, returns each as a carry-over,
//! and unlinks it.
//!
//! The breadcrumbs live in their own subdir under `plugins/managed/`
//! (which is itself internal state, not meant for user inspection).
//! `reconcile::sweep_managed_orphans` only scans regular files at the
//! top of `plugins/managed/`, so a subfolder is invisible to it.
//!
//! Filenames include a `<ns_inode>` segment - the PID namespace inode
//! parsed from `/proc/self/ns/pid` (Linux) or a constant `0` (other
//! platforms). This handles sandboxed setups (nsjail, bubblewrap,
//! docker, ...) that share `plugins/` between games but give each
//! sandbox its own PID namespace: two siblings would otherwise both
//! write to `<pid>.toml` (often the same low PID like `1`) and
//! overwrite each other's in-flight contents. The namespace inode is
//! only for write-time isolation between concurrent instances - the
//! scan no longer matches `ns_inode` or probes PID liveness. We don't
//! try to distinguish "previous self crashed" from "concurrent sibling
//! mid-callback": doing so via `kill(pid, 0)` proved fragile (PID
//! reuse, EPERM, Windows access-denied) and didn't handle the common
//! sandbox-restart case where `ns_inode` flips every launch (so our
//! own previous-launch file would look cross-namespace and never get
//! reaped). Trade-off: in the rare "two ClassiCube instances running
//! simultaneously and sharing plugins/" case, instance B's startup
//! will consume A's in-flight breadcrumb and falsely warn "previous
//! session crashed inside foo/bar Init"; A keeps running fine, A's
//! later `clear` is a silent no-op, and B's user retries via `/load
//! owner/repo`.
//!
//! Why a separate file instead of a field on the shared config: the
//! breadcrumb flips twice per callback per managed plugin (set + clear).
//! Two instances of the game pounding on `plugin-manager.toml` from the
//! hot path is wasteful, and the live-instance breadcrumb being visible
//! to the other instance produces false-positive "crashed inside ..."
//! warnings on startup. The per-process file shape sidesteps both.

#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process,
    sync::OnceLock,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tracing::{debug, warn};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    owner: String,
    repo: String,
    callback: String,
}

/// Persist a breadcrumb for `(owner, repo, callback)` for the current
/// process. Atomic via tmp + rename so a concurrent reader from another
/// instance's startup scan never sees a torn file. No fsync: a managed
/// plugin segfault leaves the page dirty in the kernel cache where the
/// next process can still read it, and we'd rather not pay an fsync per
/// `OnNewMap`. Power loss between the rename and the crash defeats the
/// breadcrumb, which is acceptable - we only claim to catch crashes, not
/// hardware failures.
pub fn write(dir: &Path, owner: &str, repo: &str, callback: &str) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let target = breadcrumb_path(dir, current_ns_inode(), process::id());
    let entry = Entry {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        callback: callback.to_owned(),
    };
    let body = toml::to_string(&entry).context("serializing breadcrumb")?;
    let mut tmp = NamedTempFile::new_in(dir)
        .with_context(|| format!("creating tmp file in {}", dir.display()))?;
    tmp.write_all(body.as_bytes())
        .with_context(|| format!("writing {}", tmp.path().display()))?;
    tmp.persist(&target)
        .with_context(|| format!("renaming tmp -> {}", target.display()))?;
    Ok(())
}

/// Best-effort delete of this process's breadcrumb. `NotFound` is silent
/// (it's normal for `clear` to run after a successful `write` + delete
/// pair from a previous callback already removed it, or for a sub that
/// never wrote one at all).
pub fn clear(dir: &Path) -> Result<()> {
    let target = breadcrumb_path(dir, current_ns_inode(), process::id());
    match fs::remove_file(&target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", target.display())),
    }
}

/// Scan `dir` for every entry that parses as a breadcrumb TOML, return
/// `(owner, repo) -> callback` for each, and unlink the consumed files.
/// There is no filename filter, no PID-liveness check, and no
/// `ns_inode`-matching gate - any file in the dir whose contents
/// deserialize into `Entry` becomes a carry-over. See the module doc
/// for the trade-off discussion.
///
/// On read or parse error for an individual file the entry is logged
/// at `warn` and skipped (the file is left in place so a future startup
/// with the fix can retry). The scan itself only returns `Err` if the
/// directory exists but cannot be read.
pub fn collect_dead(dir: &Path) -> Result<HashMap<(String, String), String>> {
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = HashMap::new();
    for dirent in read_dir {
        let dirent = match dirent {
            Ok(d) => d,
            Err(e) => {
                warn!("breadcrumb scan: {e:#}");
                continue;
            }
        };
        let path = dirent.path();
        match fs::read_to_string(&path) {
            Ok(body) => match toml::from_str::<Entry>(&body) {
                Ok(entry) => {
                    debug!(
                        "consuming carry-over breadcrumb {} for {}/{} in {}",
                        path.display(),
                        entry.owner,
                        entry.repo,
                        entry.callback
                    );
                    out.insert((entry.owner, entry.repo), entry.callback);
                    if let Err(e) = fs::remove_file(&path) {
                        warn!("removing {}: {e:#}", path.display());
                    }
                }
                Err(e) => warn!("parsing {}: {e}", path.display()),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => warn!("reading {}: {e:#}", path.display()),
        }
    }
    Ok(out)
}

fn breadcrumb_path(dir: &Path, ns_inode: u64, pid: u32) -> PathBuf {
    dir.join(format!("{ns_inode}-{pid}.toml"))
}

/// PID namespace inode for the current process, cached for the
/// process lifetime. On Linux this comes from `readlink
/// /proc/self/ns/pid` (format `pid:[<inode>]`); on other platforms
/// (and on Linux when `/proc` isn't available) it falls back to `0`.
/// The fallback means all processes look like they're in the same
/// namespace, which is correct on non-Linux and harmless on Linux
/// systems without `/proc` (no sandbox isolation possible there
/// either).
pub(crate) fn current_ns_inode() -> u64 {
    static CACHE: OnceLock<u64> = OnceLock::new();
    *CACHE.get_or_init(read_ns_inode)
}

#[cfg(target_os = "linux")]
fn read_ns_inode() -> u64 {
    match fs::read_link("/proc/self/ns/pid") {
        Ok(target) => {
            let s = target.to_string_lossy();
            if let Some(inner) = s.strip_prefix("pid:[").and_then(|s| s.strip_suffix("]"))
                && let Ok(n) = inner.parse::<u64>()
            {
                return n;
            }
            warn!("unexpected /proc/self/ns/pid format: {s:?}; falling back to ns_inode = 0");
            0
        }
        Err(e) => {
            warn!("reading /proc/self/ns/pid: {e:#}; falling back to ns_inode = 0");
            0
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn read_ns_inode() -> u64 {
    0
}
