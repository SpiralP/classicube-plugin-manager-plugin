#[cfg(test)]
mod tests;

use anyhow::{Result, anyhow, bail};

use crate::github_release::GitHubReleaseAsset;

pub fn pick_asset<'a>(
    assets: &'a [GitHubReleaseAsset],
    arch: &str,
    dll_suffix: &str,
) -> Result<&'a GitHubReleaseAsset> {
    if assets.is_empty() {
        bail!("release has no assets");
    }

    let suffix_lc = dll_suffix.to_ascii_lowercase();
    let by_suffix: Vec<&GitHubReleaseAsset> = assets
        .iter()
        .filter(|a| a.name.to_ascii_lowercase().ends_with(&suffix_lc))
        .collect();

    if by_suffix.is_empty() {
        bail!(
            "no asset ending in `{dll_suffix}` (available: {})",
            list_names(assets)
        );
    }

    let (tokens, reject) = arch_tokens(arch);
    if tokens.is_empty() {
        bail!("unsupported arch `{arch}` - no known asset name tokens to match");
    }

    let by_arch: Vec<&GitHubReleaseAsset> = by_suffix
        .into_iter()
        .filter(|a| {
            let name_lc = a.name.to_ascii_lowercase();
            // Reject names that look like a sibling-but-incompatible arch
            // (e.g. an x86_64 build when running 32-bit x86). Rejection runs
            // first because rejected tokens are more specific than the
            // preferred tokens that would otherwise false-match.
            if reject
                .iter()
                .any(|t| contains_word(&name_lc, &t.to_ascii_lowercase()))
            {
                return false;
            }
            tokens
                .iter()
                .any(|t| contains_word(&name_lc, &t.to_ascii_lowercase()))
        })
        .collect();

    match by_arch.as_slice() {
        [a] => Ok(*a),
        [] => Err(anyhow!(
            "no asset matched arch `{arch}` (looked for tokens {:?}); available: {}",
            tokens,
            list_names(assets),
        )),
        many => Err(anyhow!(
            "{} assets matched arch `{arch}`, ambiguous: {}",
            many.len(),
            list_names_iter(many.iter().copied()),
        )),
    }
}

/// Returns `(preferred_tokens, reject_tokens)` for the running arch.
/// `reject_tokens` are sibling-but-incompatible arches whose tokens overlap
/// with this arch's preferred tokens (e.g. `x86_64` shares the `x86`
/// substring); an asset name matching any reject token is excluded before
/// preferred-token matching runs.
fn arch_tokens(arch: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match arch {
        "x86_64" => (&["x86_64", "amd64"], &[]),
        "aarch64" => (&["aarch64", "arm64"], &[]),
        "x86" => (&["i686", "i386", "x86"], &["x86_64", "amd64"]),
        "arm" => (&["armv7", "arm"], &["aarch64", "arm64", "armv8"]),
        _ => (&[], &[]),
    }
}

/// Whether `filename` looks like a build artifact for `repo` on this platform.
///
/// Recognizes three shapes:
///
/// - Canonical: `classicube-foo-plugin.so`
/// - Rust-cdylib output for crate `classicube-foo-plugin`:
///   - Linux:   `libclassicube_foo_plugin.so`
///   - macOS:   `libclassicube_foo_plugin.dylib`
///   - Windows: `classicube_foo_plugin.dll` (no `lib` prefix)
/// - Target-tuple: `classicube_foo_<os>_<arch>.so` and similar - the trailing
///   `-plugin` is dropped from the repo and OS+arch tokens take its place
///   (e.g. `classicube_cef_loader_linux_x86_64.so` for
///   `classicube-cef-loader-plugin`). Requires `repo` to end in `-plugin`
///   and the part after the repo's short prefix to begin with a known OS or
///   arch token, so an asset belonging to `classicube-cef-loader-plugin`
///   doesn't false-match `classicube-cef-plugin`.
///
/// Used for duplicate-load detection: a variant-named file in `plugins/` next
/// to a managed canonical asset would cause ClassiCube to `dlopen` both.
///
/// Normalization on the filename is: lowercase, strip `dll_suffix`, strip any
/// leading `lib` prefix, replace `_` with `-`. The result is compared to the
/// repo name lowercased.
pub fn matches_repo(filename: &str, repo: &str, dll_suffix: &str) -> bool {
    let suffix_lc = dll_suffix.to_ascii_lowercase();
    let name_lc = filename.to_ascii_lowercase();
    let Some(stem) = name_lc.strip_suffix(&suffix_lc) else {
        return false;
    };
    let stem = stem.strip_prefix("lib").unwrap_or(stem);
    let normalized = stem.replace('_', "-");
    let repo_lc = repo.to_ascii_lowercase();
    if normalized == repo_lc {
        return true;
    }
    let Some(repo_short) = repo_lc.strip_suffix("-plugin") else {
        return false;
    };
    let Some(rest) = normalized
        .strip_prefix(repo_short)
        .and_then(|r| r.strip_prefix('-'))
    else {
        return false;
    };
    starts_with_platform_token(rest)
}

/// True if `s` starts with a known OS or arch token followed by `-` or end of
/// string. Tokens are post-normalization (`_`->`-`), so `x86_64` is matched
/// as `x86-64`. All architectures are accepted, not just the running one:
/// duplicate-load detection should flag a wrong-arch leftover too.
fn starts_with_platform_token(s: &str) -> bool {
    const OS_TOKENS: &[&str] = &["linux", "windows", "macos", "darwin"];
    const ARCH_TOKENS: &[&str] = &[
        "x86-64", "amd64", "aarch64", "arm64", "armv8", "armv7", "i686", "i386", "x86", "arm",
    ];
    OS_TOKENS.iter().chain(ARCH_TOKENS).any(|tok| {
        s.strip_prefix(tok)
            .is_some_and(|r| r.is_empty() || r.starts_with('-'))
    })
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let n = needle.as_bytes();
    let mut i = 0;
    while i + n.len() <= bytes.len() {
        if &bytes[i..i + n.len()] == n {
            let left_ok = i == 0 || !is_word_byte(bytes[i - 1]);
            let right_idx = i + n.len();
            let right_ok = right_idx == bytes.len() || !is_word_byte(bytes[right_idx]);
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn list_names(assets: &[GitHubReleaseAsset]) -> String {
    list_names_iter(assets.iter())
}

fn list_names_iter<'a>(it: impl Iterator<Item = &'a GitHubReleaseAsset>) -> String {
    it.map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
}
