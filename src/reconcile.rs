#[cfg(test)]
mod tests;

use std::{collections::HashSet, fs, io, path::Path};

use anyhow::{Context, Result};

use crate::config::{self, Config};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub missing: Vec<MissingFile>,
    pub orphans: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MissingFile {
    pub owner: String,
    pub repo: String,
    pub asset: String,
}

/// Scan `managed_dir` and reconcile it against the subscriptions in
/// `config_path`.
///
/// - If a subscription's `installed_asset` filename isn't on disk, the sub is
///   disabled and its installed fields cleared. Manual deletion is treated as
///   intent to stop running the plugin, not a redownload trigger.
/// - Files in `managed_dir` not claimed by any subscription's
///   `installed_asset` are reported as orphans but never touched.
///
/// The config is rewritten only when at least one subscription was disabled.
pub fn reconcile(config_path: &Path, managed_dir: &Path) -> Result<ReconcileReport> {
    let mut config = Config::load_from(config_path)?;
    let on_disk = list_managed_files(managed_dir)
        .with_context(|| format!("listing {}", managed_dir.display()))?;

    let mut report = ReconcileReport::default();
    let mut claimed: HashSet<String> = HashSet::new();

    for (owner, repos) in &mut config.subscriptions {
        for (repo, sub) in repos {
            // The self subscription installs into plugins/ (not plugins/managed/),
            // so it never participates in this reconcile pass — checking here
            // would always flag it missing.
            if config::is_self(owner, repo) {
                continue;
            }
            let Some(asset) = sub.state.installed_asset.clone() else {
                continue;
            };
            if on_disk.contains(&asset) {
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

    for filename in on_disk {
        if !claimed.contains(&filename) {
            report.orphans.push(filename);
        }
    }
    report.orphans.sort();

    if !report.missing.is_empty() {
        config
            .save_to(config_path)
            .with_context(|| format!("saving {}", config_path.display()))?;
    }

    Ok(report)
}

fn list_managed_files(dir: &Path) -> io::Result<HashSet<String>> {
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(e),
    };
    let mut files = HashSet::new();
    for entry in read_dir {
        let entry = entry?;
        if entry.metadata()?.is_file()
            && let Some(name) = entry.file_name().to_str()
        {
            files.insert(name.to_owned());
        }
    }
    Ok(files)
}
