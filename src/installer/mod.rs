#[cfg(test)]
mod tests;

use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::{
    github_release::{GitHubReleaseAsset, make_client, resolve_auth_token},
    self_path::current_lib_path,
};

pub const PLUGINS_DIR: &str = "plugins";
pub const MANAGED_DIR: &str = "plugins/managed";

pub async fn download_to_managed_dir(
    asset: &GitHubReleaseAsset,
    expected_digest: Option<&str>,
    token: Option<&str>,
) -> Result<PathBuf> {
    debug!(
        "downloading {} -> {}/{}",
        asset.url, MANAGED_DIR, asset.name
    );
    let bytes = download_bytes(asset, token).await?;
    install_bytes_to(Path::new(MANAGED_DIR), &asset.name, &bytes, expected_digest)
}

/// Self-update install: write the new bytes over the loaded updater binary
/// in `plugins/`. The existing `install_bytes_to` rename dance handles the
/// loaded-and-locked file correctly:
///
/// - Linux: `rename` of an mmap'd file is allowed; the in-memory mapping is
///   decoupled from the dirent.
/// - Windows: `MoveFileExW` (under `fs::rename`) allows renaming a locked
///   DLL even though it forbids overwriting one. The leftover `.old` can't
///   be deleted in-session and is cleaned up on next startup.
///
/// The current process keeps running the old code; the user must restart to
/// pick up the new version.
pub async fn download_self(
    asset: &GitHubReleaseAsset,
    expected_digest: Option<&str>,
    token: Option<&str>,
) -> Result<PathBuf> {
    let loaded = current_lib_path().context("resolving self path")?;
    let dir = loaded
        .parent()
        .ok_or_else(|| anyhow!("loaded path has no parent: {}", loaded.display()))?;
    if dir.file_name() != Some(OsStr::new("plugins")) {
        bail!(
            "loaded updater binary at {} is not directly under plugins/; refusing to self-update",
            loaded.display()
        );
    }
    let basename = loaded
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("loaded path has no UTF-8 filename: {}", loaded.display()))?;

    debug!(
        "self-updating: downloading {} -> {}",
        asset.url,
        loaded.display(),
    );
    let bytes = download_bytes(asset, token).await?;
    install_bytes_to(dir, basename, &bytes, expected_digest)
}

/// Best-effort cleanup of a `<self>.old` left behind by a previous
/// self-update. On Linux the `.old` is removed in-session by
/// `install_bytes_to`; on Windows the loader's lock prevents that, so we
/// retry here at startup (when the previous loaded copy is no longer mapped
/// by anything).
pub fn cleanup_self_old() {
    let loaded = match current_lib_path() {
        Ok(p) => p,
        Err(e) => {
            debug!("cleanup_self_old: skipping ({e:#})");
            return;
        }
    };
    let Some(file_name) = loaded.file_name().and_then(OsStr::to_str) else {
        return;
    };
    let Some(dir) = loaded.parent() else {
        return;
    };
    let old_path = dir.join(format!("{file_name}.old"));
    match fs::remove_file(&old_path) {
        Ok(()) => debug!("removed leftover {}", old_path.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => debug!("could not remove {}: {e}", old_path.display()),
    }
}

async fn download_bytes(asset: &GitHubReleaseAsset, token: Option<&str>) -> Result<Vec<u8>> {
    // Hit the API URL with `Accept: application/octet-stream` — that's the
    // only path that honors Bearer tokens for private-repo assets. GitHub
    // 302s to a signed `objects.githubusercontent.com` URL; reqwest strips
    // Authorization on the cross-host hop (see
    // `reqwest::redirect::remove_sensitive_headers`), which is fine because
    // the signed URL needs no auth.
    let mut request = make_client()
        .get(&asset.url)
        .header(ACCEPT, HeaderValue::from_static("application/octet-stream"));
    if let Some(t) = resolve_auth_token(token) {
        let mut header_value = HeaderValue::from_str(&format!("Bearer {t}"))
            .map_err(|e| anyhow!("invalid token characters: {e}"))?;
        header_value.set_sensitive(true);
        request = request.header(AUTHORIZATION, header_value);
    }
    let bytes = request
        .send()
        .await
        .with_context(|| format!("requesting {}", asset.url))?
        .error_for_status()
        .with_context(|| format!("HTTP error for {}", asset.url))?
        .bytes()
        .await
        .with_context(|| format!("reading body of {}", asset.url))?;
    Ok(bytes.to_vec())
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
