#[cfg(test)]
mod tests;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::github_release::{GitHubReleaseAsset, make_client};

pub const PLUGINS_DIR: &str = "plugins";
pub const MANAGED_DIR: &str = "plugins/managed";

pub async fn download_to_managed_dir(
    asset: &GitHubReleaseAsset,
    expected_digest: Option<&str>,
) -> Result<PathBuf> {
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

    install_bytes_to(Path::new(MANAGED_DIR), &asset.name, &bytes, expected_digest)
}

/// Atomically write `bytes` to `dir/asset_name`. The existing file (if any)
/// is moved to `dir/asset_name.old` first; the new file lands via `.new` →
/// rename. Leftover `.old` is best-effort cleaned at the end.
///
/// If `expected_digest` is `Some("sha256:<hex>")`, the bytes are verified
/// before any disk write — a mismatch (or malformed digest) returns `Err`
/// with `dir` untouched, so any pre-existing installed file survives.
pub fn install_bytes_to(
    dir: &Path,
    asset_name: &str,
    bytes: &[u8],
    expected_digest: Option<&str>,
) -> Result<PathBuf> {
    if let Some(expected) = expected_digest {
        verify_sha256(bytes, expected)
            .with_context(|| format!("verifying digest for {asset_name}"))?;
        debug!("digest ok for {asset_name}");
    }

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
            return Err(anyhow!(
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

/// Parse a `"sha256:<64 lowercase hex>"` digest string into its 32 raw bytes.
/// Strict — uppercase, wrong length, non-hex, or any other prefix is rejected.
pub fn parse_sha256_digest(s: &str) -> Result<[u8; 32]> {
    let hex = s
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("digest missing 'sha256:' prefix: {s}"))?;
    if hex.len() != 64 {
        bail!("sha256 digest must be 64 hex chars, got {}: {s}", hex.len());
    }
    let mut out = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let hi = hex_nibble(chunk[0]).ok_or_else(|| anyhow!("non-hex char in digest: {s}"))?;
        let lo = hex_nibble(chunk[1]).ok_or_else(|| anyhow!("non-hex char in digest: {s}"))?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

fn to_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    let want = parse_sha256_digest(expected)?;
    let got = Sha256::digest(bytes);
    if got.as_slice() != want.as_slice() {
        bail!(
            "sha256 mismatch: expected sha256:{} got sha256:{}",
            to_hex_lower(&want),
            to_hex_lower(got.as_slice()),
        );
    }
    Ok(())
}
