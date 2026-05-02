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
        bail!("unsupported arch `{arch}` — no known asset name tokens to match");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> GitHubReleaseAsset {
        GitHubReleaseAsset {
            name: name.into(),
            browser_download_url: format!("https://example/{name}"),
        }
    }

    #[test]
    fn picks_unique_match() {
        let assets = [
            asset("plugin_linux_x86_64.so"),
            asset("plugin_windows_x86_64.dll"),
        ];
        let got = pick_asset(&assets, "x86_64", ".so").unwrap();
        assert_eq!(got.name, "plugin_linux_x86_64.so");
    }

    #[test]
    fn matches_amd64_alias_for_x86_64() {
        let assets = [asset("plugin_amd64.so")];
        let got = pick_asset(&assets, "x86_64", ".so").unwrap();
        assert_eq!(got.name, "plugin_amd64.so");
    }

    #[test]
    fn matches_arm64_alias_for_aarch64() {
        let assets = [asset("plugin_arm64.dylib")];
        let got = pick_asset(&assets, "aarch64", ".dylib").unwrap();
        assert_eq!(got.name, "plugin_arm64.dylib");
    }

    #[test]
    fn case_insensitive() {
        let assets = [asset("Plugin_X86_64.SO")];
        let got = pick_asset(&assets, "x86_64", ".so").unwrap();
        assert_eq!(got.name, "Plugin_X86_64.SO");
    }

    #[test]
    fn empty_assets_errors() {
        let err = pick_asset(&[], "x86_64", ".so").unwrap_err();
        assert!(format!("{err}").contains("no assets"));
    }

    #[test]
    fn no_suffix_match_errors() {
        let assets = [asset("plugin.dll"), asset("plugin.dylib")];
        let err = pick_asset(&assets, "x86_64", ".so").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains(".so"), "{msg}");
        assert!(msg.contains("plugin.dll"), "{msg}");
    }

    #[test]
    fn suffix_matches_but_arch_does_not() {
        let assets = [asset("plugin_aarch64.so")];
        let err = pick_asset(&assets, "x86_64", ".so").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("arch"), "{msg}");
        assert!(msg.contains("plugin_aarch64.so"), "{msg}");
    }

    #[test]
    fn ambiguous_match_errors_with_candidates() {
        let assets = [asset("plugin_x86_64.so"), asset("plugin_x86_64-debug.so")];
        let err = pick_asset(&assets, "x86_64", ".so").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains("plugin_x86_64.so"), "{msg}");
        assert!(msg.contains("plugin_x86_64-debug.so"), "{msg}");
    }

    #[test]
    fn x86_does_not_match_x86_64_asset() {
        // Word-bounded match: "x86" should NOT match "x86_64" (the `_` after
        // `x86` makes it a different token), so a 32-bit user shouldn't get
        // a 64-bit binary.
        let assets = [asset("plugin_x86_64.so")];
        let err = pick_asset(&assets, "x86", ".so").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("arch"), "{msg}");
    }

    #[test]
    fn x86_matches_i686_asset() {
        let assets = [asset("plugin_i686.so")];
        let got = pick_asset(&assets, "x86", ".so").unwrap();
        assert_eq!(got.name, "plugin_i686.so");
    }

    #[test]
    fn arm_matches_armv7_asset() {
        let assets = [asset("plugin_armv7.so")];
        let got = pick_asset(&assets, "arm", ".so").unwrap();
        assert_eq!(got.name, "plugin_armv7.so");
    }

    #[test]
    fn unsupported_arch_errors() {
        let assets = [asset("plugin.so")];
        let err = pick_asset(&assets, "riscv64", ".so").unwrap_err();
        assert!(format!("{err}").contains("unsupported arch"));
    }

    #[test]
    fn arm_token_does_not_partial_match_armv8() {
        // Word-bounded: arch="arm" should not match an "armv8" asset because
        // 'v' after "arm" is alphanumeric (no word boundary).
        let assets = [asset("plugin_armv8.so")];
        let err = pick_asset(&assets, "arm", ".so").unwrap_err();
        assert!(format!("{err}").contains("arch"));
    }
}
