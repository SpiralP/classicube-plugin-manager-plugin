#[cfg(test)]
mod tests;

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use tracing::debug;

use crate::{
    asset_match,
    config::{self, Config},
};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub missing: Vec<MissingFile>,
    pub orphans: Vec<String>,
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MissingFile {
    pub owner: String,
    pub repo: String,
    pub asset: String,
}

/// A file that looks like a build artifact for a known subscription's repo
/// (per `asset_match::matches_repo`) but isn't claimed by the subscription's
/// `installed_asset`. Most often a rust-cdylib variant filename
/// (`libclassicube_foo_plugin.so`) sitting next to a managed canonical asset
/// (`classicube-foo-plugin.so`); ClassiCube would `dlopen` both as duplicates.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Conflict {
    pub dir: ConflictDir,
    pub filename: String,
    pub owner: String,
    pub repo: String,
    /// What the matching subscription thinks it owns on disk, if anything.
    /// Surfaced in chat so the user can see the canonical-vs-variant pair
    /// at a glance.
    pub installed_asset: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConflictDir {
    Plugins,
    Managed,
}

/// Scan `managed_dir` and `plugins_dir` and reconcile them against the
/// subscriptions in `config_path`.
///
/// - If a subscription's `installed_asset` filename isn't on disk in
///   `managed_dir`, the sub is disabled and its installed fields cleared.
///   Manual deletion is treated as intent to stop running the plugin, not a
///   redownload trigger.
/// - Files in `managed_dir` not claimed by any subscription's
///   `installed_asset` are reported as orphans. Any orphan whose filename
///   looks like a build artifact for a known subscription's repo is demoted
///   from `orphans` to `conflicts`.
/// - Files in `plugins_dir` whose filename looks like a build artifact for a
///   known subscription's repo are reported as conflicts. Other files in
///   `plugins_dir` are ignored; the game's plugins dir is shared with the
///   user's own files and unmanaged plugins.
/// - The running manager binary's basename (`self_running_basename`) is
///   excluded from the `plugins_dir` scan so we don't flag ourselves.
///
/// The config is rewritten only when at least one subscription was disabled.
pub fn reconcile(
    config_path: &Path,
    plugins_dir: &Path,
    managed_dir: &Path,
    dll_suffix: &str,
    self_running_basename: Option<&str>,
) -> Result<ReconcileReport> {
    let mut config = Config::load_from(config_path)?;
    let managed_on_disk = list_dir_files(managed_dir)
        .with_context(|| format!("listing {}", managed_dir.display()))?;
    let plugins_on_disk = list_dir_files(plugins_dir)
        .with_context(|| format!("listing {}", plugins_dir.display()))?;

    // Snapshot (owner, repo, installed_asset) up front: the missing-clearing
    // pass below mutates `installed_asset` to None for missing subs, but the
    // conflict warning reads better with the *original* claim ("sub thought
    // it owned X, but Y looks similar - is one of them the build you meant?").
    let subs: Vec<(String, String, Option<String>)> = config
        .subscriptions
        .iter()
        .flat_map(|(o, repos)| {
            repos
                .iter()
                .map(move |(r, s)| (o.clone(), r.clone(), s.state.installed_asset.clone()))
        })
        .collect();

    let mut report = ReconcileReport::default();
    let mut claimed: HashSet<String> = HashSet::new();

    for (owner, repos) in &mut config.subscriptions {
        for (repo, sub) in repos {
            // The self subscription installs into plugins/ (not plugins/managed/),
            // so it never participates in this reconcile pass - checking here
            // would always flag it missing. Self-vs-variant conflicts are
            // handled in the conflict-classification pass below, which scans
            // plugins/ too.
            if config::is_self(owner, repo) {
                continue;
            }
            let Some(asset) = sub.state.installed_asset.clone() else {
                continue;
            };
            if managed_on_disk.contains(&asset) {
                claimed.insert(asset);
            } else {
                report.missing.push(MissingFile {
                    owner: owner.clone(),
                    repo: repo.clone(),
                    asset,
                });
                sub.disabled = true;
                sub.state.installed_version = None;
                sub.state.installed_asset = None;
                sub.state.installed_at = None;
            }
        }
    }

    let mut managed_orphan_names: Vec<String> = managed_on_disk
        .iter()
        .filter(|n| !claimed.contains(*n))
        .cloned()
        .collect();
    managed_orphan_names.sort();
    for filename in managed_orphan_names {
        if let Some((owner, repo, installed_asset)) = match_repo(&subs, &filename, dll_suffix) {
            report.conflicts.push(Conflict {
                dir: ConflictDir::Managed,
                filename,
                owner,
                repo,
                installed_asset,
            });
        } else {
            report.orphans.push(filename);
        }
    }

    let mut plugins_files: Vec<String> = plugins_on_disk.into_iter().collect();
    plugins_files.sort();
    for filename in plugins_files {
        if Some(filename.as_str()) == self_running_basename {
            continue;
        }
        if let Some((owner, repo, installed_asset)) = match_repo(&subs, &filename, dll_suffix) {
            report.conflicts.push(Conflict {
                dir: ConflictDir::Plugins,
                filename,
                owner,
                repo,
                installed_asset,
            });
        }
    }

    if !report.missing.is_empty() {
        config
            .save_to(config_path)
            .with_context(|| format!("saving {}", config_path.display()))?;
    }

    Ok(report)
}

/// Find all files in `plugins_dir` and `managed_dir` that look like build
/// artifacts for `repo` (per `asset_match::matches_repo`), excluding any whose
/// basename is in `skip_basenames`. Used by `/add` and `/update` to refuse
/// installs that would create a second-loaded copy of a plugin already on
/// disk under a different naming convention.
///
/// Returns paths in deterministic order (plugins/ entries before managed/,
/// each sorted by filename).
pub fn find_variant_conflicts(
    plugins_dir: &Path,
    managed_dir: &Path,
    repo: &str,
    dll_suffix: &str,
    skip_basenames: &[&str],
) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for dir in [plugins_dir, managed_dir] {
        let mut names: Vec<String> = list_dir_files(dir)?.into_iter().collect();
        names.sort();
        for name in names {
            if skip_basenames.contains(&name.as_str()) {
                continue;
            }
            if asset_match::matches_repo(&name, repo, dll_suffix) {
                out.push(dir.join(&name));
            }
        }
    }
    Ok(out)
}

/// Best-effort delete of stale files in `managed_dir`. Two kinds get reaped:
///
/// 1. Regular files whose basename isn't claimed by any subscription's
///    `state.installed_asset`. Safety net for orphans the in-session
///    `cleanup_previous_managed` or `handle_remove` couldn't unlink (e.g.
///    Windows sharing violation, panic mid-update, manual user copies).
/// 2. Any `*.old` file, unconditionally. `.old` is a "marked for deletion"
///    suffix written by `install_bytes_to` and `handle_remove`'s
///    rename-aside fallback; the previous session's DLL mapping is gone by
///    the time we run, so the lock that produced the `.old` is gone too.
///
/// `.new` files are left alone: an in-flight `install_bytes_to` may still
/// be racing them (we don't currently run installs concurrently with the
/// sweep, but the sweep is cheap insurance against future concurrency).
///
/// Errors are swallowed individually so one failure doesn't abort the rest
/// of the sweep. Returns the basenames that were actually deleted, in
/// sorted order, for logging and tests. Call this AFTER any per-session
/// updates have written their new versioned files and persisted the new
/// `installed_asset`, and BEFORE the loader dlopens anything - so we don't
/// delete a file we're about to map.
pub fn sweep_managed_orphans(managed_dir: &Path, config: &Config) -> Vec<String> {
    let on_disk = match list_dir_files(managed_dir) {
        Ok(set) => set,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            debug!(
                "sweep_managed_orphans: listing {} failed: {e:#}",
                managed_dir.display()
            );
            return Vec::new();
        }
    };

    let claimed: HashSet<&str> = config
        .subscriptions
        .values()
        .flat_map(|repos| repos.values())
        .filter_map(|s| s.state.installed_asset.as_deref())
        .collect();

    let mut victims: Vec<String> = on_disk
        .into_iter()
        .filter(|name| {
            if name.ends_with(".new") {
                return false;
            }
            if name.ends_with(".old") {
                return true;
            }
            !claimed.contains(name.as_str())
        })
        .collect();
    victims.sort();

    let mut deleted = Vec::with_capacity(victims.len());
    for name in victims {
        let path = managed_dir.join(&name);
        match fs::remove_file(&path) {
            Ok(()) => {
                debug!("swept orphan {}", path.display());
                deleted.push(name);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => debug!("could not sweep {}: {e}", path.display()),
        }
    }
    deleted
}

fn match_repo(
    subs: &[(String, String, Option<String>)],
    filename: &str,
    dll_suffix: &str,
) -> Option<(String, String, Option<String>)> {
    // Match by the repo's name shape (canonical or rust-cdylib variant) OR
    // by exact filename equality with the sub's `installed_asset`. The
    // exact-name path catches release assets named after the build target
    // (e.g. `classicube_foo_linux_x86_64.so`) where the filename shape
    // doesn't match the repo name on its own.
    subs.iter()
        .find(|(_, repo, installed_asset)| {
            asset_match::matches_repo(filename, repo, dll_suffix)
                || installed_asset.as_deref() == Some(filename)
        })
        .cloned()
}

fn list_dir_files(dir: &Path) -> io::Result<HashSet<String>> {
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(e),
    };
    let mut files = HashSet::new();
    for entry in read_dir {
        let entry = entry?;
        // Follow symlinks: ClassiCube's `dlopen` follows them, so a symlink
        // to a regular `.so` is a real plugin file for our purposes.
        // `DirEntry::metadata` is `lstat`, which would mark symlinks as
        // non-files and silently drop them from duplicate-load detection.
        // Dangling symlinks are useless to the dynamic linker, so swallow
        // NotFound and skip them rather than aborting the whole scan.
        let path = entry.path();
        let md = match fs::metadata(&path) {
            Ok(md) => md,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if md.is_file()
            && let Some(name) = entry.file_name().to_str()
        {
            files.insert(name.to_owned());
        }
    }
    Ok(files)
}
