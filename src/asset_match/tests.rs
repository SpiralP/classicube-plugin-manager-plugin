use super::*;

fn asset(name: &str) -> GitHubReleaseAsset {
    GitHubReleaseAsset {
        name: name.into(),
        url: format!("https://api.example/assets/{name}"),
        digest: None,
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
fn picks_macos_dylib_over_other_suffixes() {
    // OS discrimination is implicit in the suffix — `.dylib` is macOS-only,
    // so no `macos`/`darwin` token filter is needed.
    let assets = [
        asset("plugin_linux_aarch64.so"),
        asset("plugin_windows_aarch64.dll"),
        asset("plugin_macos_aarch64.dylib"),
    ];
    let got = pick_asset(&assets, "aarch64", ".dylib").unwrap();
    assert_eq!(got.name, "plugin_macos_aarch64.dylib");
}

#[test]
fn picks_self_update_naming() {
    // Locks in the self-update path against this repo's own release naming
    // (see `.github/workflows/build.yml` mac job).
    let assets = [asset("classicube_plugin_updater_macos_aarch64.dylib")];
    let got = pick_asset(&assets, "aarch64", ".dylib").unwrap();
    assert_eq!(got.name, "classicube_plugin_updater_macos_aarch64.dylib");
}

#[test]
fn matches_darwin_arm64_naming() {
    // Confirms the `darwin`+`arm64` convention some plugins use works without
    // any explicit `darwin` alias — the `arm64` arch token already aliases
    // `aarch64`, and `.dylib` discriminates macOS.
    let assets = [asset("plugin_darwin_arm64.dylib")];
    let got = pick_asset(&assets, "aarch64", ".dylib").unwrap();
    assert_eq!(got.name, "plugin_darwin_arm64.dylib");
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

#[test]
fn matches_repo_canonical_so() {
    assert!(matches_repo(
        "classicube-foo-plugin.so",
        "classicube-foo-plugin",
        ".so"
    ));
}

#[test]
fn matches_repo_unix_cdylib_variant() {
    // `cargo build` default output for a crate named `classicube-foo-plugin`
    // on Linux: `lib` prefix + underscores instead of hyphens.
    assert!(matches_repo(
        "libclassicube_foo_plugin.so",
        "classicube-foo-plugin",
        ".so"
    ));
}

#[test]
fn matches_repo_macos_cdylib_variant() {
    assert!(matches_repo(
        "libclassicube_foo_plugin.dylib",
        "classicube-foo-plugin",
        ".dylib"
    ));
}

#[test]
fn matches_repo_windows_cdylib_variant() {
    // No `lib` prefix on Windows, but underscores instead of hyphens.
    assert!(matches_repo(
        "classicube_foo_plugin.dll",
        "classicube-foo-plugin",
        ".dll"
    ));
}

#[test]
fn matches_repo_canonical_dll() {
    assert!(matches_repo(
        "classicube-foo-plugin.dll",
        "classicube-foo-plugin",
        ".dll"
    ));
}

#[test]
fn matches_repo_is_case_insensitive() {
    assert!(matches_repo(
        "LibClassicube_Foo_Plugin.SO",
        "classicube-foo-plugin",
        ".so"
    ));
    assert!(matches_repo(
        "classicube-foo-plugin.so",
        "Classicube-Foo-Plugin",
        ".so"
    ));
}

#[test]
fn matches_repo_rejects_wrong_suffix() {
    assert!(!matches_repo(
        "classicube-foo-plugin.dll",
        "classicube-foo-plugin",
        ".so"
    ));
    assert!(!matches_repo(
        "classicube-foo-plugin",
        "classicube-foo-plugin",
        ".so"
    ));
}

#[test]
fn matches_repo_rejects_non_library_files() {
    assert!(!matches_repo(
        "classicube-foo-plugin.txt",
        "classicube-foo-plugin",
        ".so"
    ));
    assert!(!matches_repo("README.md", "classicube-foo-plugin", ".so"));
}

#[test]
fn matches_repo_rejects_other_repos() {
    assert!(!matches_repo(
        "classicube-bar-plugin.so",
        "classicube-foo-plugin",
        ".so"
    ));
    assert!(!matches_repo(
        "libclassicube_bar_plugin.so",
        "classicube-foo-plugin",
        ".so"
    ));
}

#[test]
fn matches_repo_handles_non_canonical_repo_names() {
    // A repo name that isn't `classicube-*-plugin` shape should still match
    // its own variants — the predicate is purely string normalization, no
    // canonical-name expansion baked in.
    assert!(matches_repo("cef.so", "cef", ".so"));
    assert!(matches_repo("libcef.so", "cef", ".so"));
    assert!(!matches_repo("cef.so", "classicube-cef-plugin", ".so"));
}
