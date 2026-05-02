#[cfg(test)]
mod tests;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use tracing::{debug, warn};

use crate::github_release::{GitHubReleaseAsset, make_client};

pub const PLUGINS_DIR: &str = "plugins";
pub const MANAGED_DIR: &str = "plugins-managed";

pub async fn download_to_managed_dir(asset: &GitHubReleaseAsset) -> Result<PathBuf> {
    debug!(
        "downloading {} -> {}/{}",
        asset.browser_download_url, MANAGED_DIR, asset.name
    );
    let bytes = make_client()
        .get(&asset.browser_download_url)
        .send()
        .await
        .with_context(|| format!("requesting {}", asset.browser_download_url))?
        .error_for_status()
        .with_context(|| format!("HTTP error for {}", asset.browser_download_url))?
        .bytes()
        .await
        .with_context(|| format!("reading body of {}", asset.browser_download_url))?;

    install_bytes_to(Path::new(MANAGED_DIR), &asset.name, &bytes)
}

/// Atomically write `bytes` to `dir/asset_name`. The existing file (if any)
/// is moved to `dir/asset_name.old` first; the new file lands via `.new` →
/// rename. Leftover `.old` is best-effort cleaned at the end.
pub fn install_bytes_to(dir: &Path, asset_name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let final_path = dir.join(asset_name);
    let new_path = dir.join(format!("{asset_name}.new"));
    let old_path = dir.join(format!("{asset_name}.old"));

    if let Some(parent) = final_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }

    fs::write(&new_path, bytes).with_context(|| format!("writing {}", new_path.display()))?;

    if final_path.exists()
        && let Err(e) = fs::rename(&final_path, &old_path)
    {
        warn!(
            "couldn't move existing {} aside ({e}); trying delete",
            final_path.display()
        );
        if let Err(e2) = fs::remove_file(&final_path)
            && e2.kind() != io::ErrorKind::NotFound
        {
            let _ = fs::remove_file(&new_path);
            return Err(anyhow::anyhow!(
                "couldn't replace {}: rename failed ({e}) and delete failed ({e2})",
                final_path.display()
            ));
        }
    }

    fs::rename(&new_path, &final_path).with_context(|| {
        format!(
            "renaming {} -> {}",
            new_path.display(),
            final_path.display()
        )
    })?;

    let _ = fs::remove_file(&old_path);

    Ok(final_path)
}
