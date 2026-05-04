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
