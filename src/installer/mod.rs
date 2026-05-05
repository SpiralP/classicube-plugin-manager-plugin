#[cfg(test)]
mod tests;

use std::{
    env,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::{
    config::{SELF_OWNER, SELF_REPO},
    github_release::{GitHubReleaseAsset, make_client, resolve_auth_token},
    self_path::current_lib_path,
};

pub const PLUGINS_DIR: &str = "plugins";
pub const MANAGED_DIR: &str = "plugins/managed";

/// Build the on-disk filename for a managed plugin binary. Including the
/// version tag in the filename gives every release a distinct path, which is
/// what makes in-session `/update` actually swap code: glibc's `dlopen`
/// dedupes by realpath (and Windows by module name), so a fresh dlopen of
/// the *same* path returns the cached handle - i.e. the old code keeps
/// running. A fresh path forces a fresh mapping.
///
/// Schema: `<owner>-<repo>-<sanitized_tag><ext>`. The tag is sanitized so it
/// can't escape the directory or produce surprising filenames - any char
/// outside `[A-Za-z0-9._-]` becomes `_`. Sanitized tags are capped at 64
/// chars to keep paths reasonable; tags rarely approach this.
pub fn versioned_managed_filename(owner: &str, repo: &str, tag: &str, ext: &str) -> String {
    let safe_tag = sanitize_tag(tag);
    format!("{owner}-{repo}-{safe_tag}{ext}")
}

fn sanitize_tag(tag: &str) -> String {
    tag.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// Filename prefixes for self binaries the manager itself produces or has
/// historically shipped. `cleanup_self_old` uses these to scope its
/// `*.old` sweep so we only reap files that belong to this plugin.
///
/// - `classicube_plugin_manager` is the v4 release-asset shape
///   (`classicube_plugin_manager_<os>_<arch>.<ext>`) - what users have
///   on disk before their first post-versioning self-update, and what
///   the previous-loaded basename gets renamed aside as.
/// - `classicube_plugin_updater` is the v3 release-asset shape, kept
///   so a v3 file renamed aside (or any `.old` left over from a v3->v4
///   transition) still gets reaped.
///
/// The current versioned scheme uses the runtime-computed prefix
/// `<SELF_OWNER>-<SELF_REPO>-` (built where the sweep runs - constants
/// can't be concatenated at compile time without macro tricks).
const LEGACY_SELF_PREFIXES: &[&str] = &["classicube_plugin_manager", "classicube_plugin_updater"];

pub async fn download_to_managed_dir(
    owner: &str,
    repo: &str,
    tag: &str,
    asset: &GitHubReleaseAsset,
    expected_digest: Option<&str>,
    token: Option<&str>,
) -> Result<PathBuf> {
    let filename = versioned_managed_filename(owner, repo, tag, env::consts::DLL_SUFFIX);
    debug!("downloading {} -> {}/{}", asset.url, MANAGED_DIR, filename);
    let bytes = download_bytes(asset, token).await?;
    install_bytes_to(Path::new(MANAGED_DIR), &filename, &bytes, expected_digest)
}

/// Best-effort delete of the previous versioned managed file after a
/// successful update. No-op when `previous == new` or `previous` is None.
/// On Linux/macOS the unlink succeeds even while the library is mapped
/// (the dirent is decoupled from the inode); on Windows the delete may
/// fail with a sharing violation, in which case the startup orphan sweep
/// catches the leftover next session.
pub fn cleanup_previous_managed(managed_dir: &Path, previous: Option<&str>, new: &str) {
    let Some(prev) = previous else { return };
    if prev == new {
        return;
    }
    let path = managed_dir.join(prev);
    match fs::remove_file(&path) {
        Ok(()) => debug!("removed prior managed binary {}", path.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => debug!("could not remove {}: {e}", path.display()),
    }
}

/// Self-update install: download the new manager binary into `plugins/`
/// under the deterministic versioned filename
/// `<SELF_OWNER>-<SELF_REPO>-<tag><ext>` (same scheme as managed plugins),
/// distinct from whatever path the loaded binary was opened from. The
/// user still has to restart for the new code to run; mapping vs dirent
/// decoupling means the in-process old code keeps running until then.
///
/// Writing to a fresh path avoids overwriting the currently-mmap'd file
/// and lets us pick a stable name regardless of what the user originally
/// installed (release-asset shape, rust-cdylib variant, hand-renamed,
/// etc.). Marking the previous on-disk file aside (so ClassiCube doesn't
/// load both copies on next launch) is the caller's job - see
/// `mark_previous_self_aside`.
pub async fn download_self(
    asset: &GitHubReleaseAsset,
    expected_digest: Option<&str>,
    token: Option<&str>,
    tag: &str,
) -> Result<PathBuf> {
    let loaded = current_lib_path().context("resolving self path")?;
    let (dir, new_basename) = resolve_self_update_target(&loaded, tag, env::consts::DLL_SUFFIX)?;

    debug!(
        "self-updating: downloading {} -> {}",
        asset.url,
        dir.join(&new_basename).display(),
    );
    let bytes = download_bytes(asset, token).await?;
    install_bytes_to(&dir, &new_basename, &bytes, expected_digest)
}

/// Pure: derive the directory and target basename for a self-update from
/// the currently-loaded binary's path. Refuses cases that would corrupt
/// the install:
///
/// - loaded path has no parent (shouldn't happen in practice).
/// - parent isn't `plugins/` (we don't know where to put files safely).
/// - target basename equals loaded basename (we'd write over the
///   currently-mmap'd file - belt-and-suspenders against stale-config
///   short-circuits in callers).
pub(crate) fn resolve_self_update_target(
    loaded: &Path,
    tag: &str,
    ext: &str,
) -> Result<(PathBuf, String)> {
    let dir = loaded
        .parent()
        .ok_or_else(|| anyhow!("loaded path has no parent: {}", loaded.display()))?;
    if dir.file_name() != Some(OsStr::new("plugins")) {
        bail!(
            "loaded manager binary at {} is not directly under plugins/; refusing to self-update",
            loaded.display()
        );
    }
    let basename = loaded
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("loaded path has no UTF-8 filename: {}", loaded.display()))?;
    let new_basename = versioned_managed_filename(SELF_OWNER, SELF_REPO, tag, ext);
    if new_basename == basename {
        bail!(
            "self already loaded at {} for tag {tag}; nothing to do",
            loaded.display()
        );
    }
    Ok((dir.to_path_buf(), new_basename))
}

/// Best-effort: mark a previously-loaded self binary aside as `<prev>.old`
/// after a self-update has written a new versioned file at `<dir>/<new>`.
/// ClassiCube auto-loads every plugin file at launch, so leaving the prior
/// `<prev>` in `plugins/` would cause two managers to load on the next
/// session. We rename rather than delete because:
///
/// - Linux/macOS: rename works while the file is mapped; the in-memory
///   mapping is decoupled from the dirent.
/// - Windows: `MoveFileExW` allows renaming a locked DLL (already
///   exercised by the legacy in-place self-update path).
///
/// After a successful rename, try to delete the `.old` immediately - same
/// pattern as `cleanup_previous_managed` and the trailing
/// `fs::remove_file(&old_path)` in `install_bytes_to`. Linux/macOS unlink
/// succeeds even while the library is still mapped; Windows fails with a
/// sharing violation and the startup `cleanup_self_old` sweep mops it up
/// next session. No-op when `prev == new` (same-tag re-install, where
/// install_bytes_to already handled it) or when the file is missing.
pub fn mark_previous_self_aside(dir: &Path, prev: &str, new: &str) {
    if prev == new {
        return;
    }
    let prev_path = dir.join(prev);
    if !prev_path.exists() {
        return;
    }
    let old_path = dir.join(format!("{prev}.old"));
    match fs::rename(&prev_path, &old_path) {
        Ok(()) => debug!(
            "renamed previous self {} -> {}",
            prev_path.display(),
            old_path.display()
        ),
        Err(e) => debug!(
            "could not rename previous self {} -> {}: {e}",
            prev_path.display(),
            old_path.display()
        ),
    }
    let _ = fs::remove_file(&old_path);
}

/// Best-effort cleanup of leftover self-binary `*.old` files in `plugins/`
/// at startup. The previous-session DLL mapping is gone by now, so the
/// lock that produced the `.old` (Windows) is gone too. On Linux/macOS
/// most `.old` files are deleted in-session by `install_bytes_to`'s
/// rename dance; this sweep is the safety net.
///
/// Scope: files in the running self binary's parent directory whose name
/// ends in `.old` and starts with one of the known self-binary prefixes
/// (current versioned scheme `<SELF_OWNER>-<SELF_REPO>-` plus legacy
/// release-asset shapes). That keeps unrelated `.old` files alone while
/// catching every plausible previous on-disk shape.
pub fn cleanup_self_old() {
    let loaded = match current_lib_path() {
        Ok(p) => p,
        Err(e) => {
            debug!("cleanup_self_old: skipping ({e:#})");
            return;
        }
    };
    let Some(dir) = loaded.parent() else {
        return;
    };
    cleanup_self_old_in(dir);
}

pub(crate) fn cleanup_self_old_in(dir: &Path) {
    let versioned_prefix = format!("{SELF_OWNER}-{SELF_REPO}-");
    let prefixes: Vec<&str> = std::iter::once(versioned_prefix.as_str())
        .chain(LEGACY_SELF_PREFIXES.iter().copied())
        .collect();

    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            debug!("cleanup_self_old: read_dir {} failed: {e}", dir.display());
            return;
        }
    };
    for entry in read_dir.flatten() {
        let entry_name = entry.file_name();
        let Some(name) = entry_name.to_str() else {
            continue;
        };
        if !name.ends_with(".old") {
            continue;
        }
        if !prefixes.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => debug!("removed leftover {}", path.display()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => debug!("could not remove {}: {e}", path.display()),
        }
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
