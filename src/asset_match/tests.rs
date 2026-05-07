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
    let got = pick_asset("v1.0.0", &assets, "x86_64", ".so").unwrap();
    assert_eq!(got.name, "plugin_linux_x86_64.so");
}

#[test]
fn matches_amd64_alias_for_x86_64() {
    let assets = [asset("plugin_amd64.so")];
    let got = pick_asset("v1.0.0", &assets, "x86_64", ".so").unwrap();
    assert_eq!(got.name, "plugin_amd64.so");
}

#[test]
fn matches_arm64_alias_for_aarch64() {
    let assets = [asset("plugin_arm64.dylib")];
    let got = pick_asset("v1.0.0", &assets, "aarch64", ".dylib").unwrap();
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
    let got = pick_asset("v1.0.0", &assets, "aarch64", ".dylib").unwrap();
    assert_eq!(got.name, "plugin_macos_aarch64.dylib");
}

#[test]
fn picks_self_update_naming() {
    // Locks in the self-update path against this repo's own release naming
    // (see `.github/workflows/build.yml` mac job).
    let assets = [asset("classicube_plugin_manager_macos_aarch64.dylib")];
    let got = pick_asset("v1.0.0", &assets, "aarch64", ".dylib").unwrap();
    assert_eq!(got.name, "classicube_plugin_manager_macos_aarch64.dylib");
}

#[test]
fn matches_darwin_arm64_naming() {
    // Confirms the `darwin`+`arm64` convention some plugins use works without
    // any explicit `darwin` alias — the `arm64` arch token already aliases
    // `aarch64`, and `.dylib` discriminates macOS.
    let assets = [asset("plugin_darwin_arm64.dylib")];
    let got = pick_asset("v1.0.0", &assets, "aarch64", ".dylib").unwrap();
    assert_eq!(got.name, "plugin_darwin_arm64.dylib");
}

#[test]
fn case_insensitive() {
    let assets = [asset("Plugin_X86_64.SO")];
    let got = pick_asset("v1.0.0", &assets, "x86_64", ".so").unwrap();
    assert_eq!(got.name, "Plugin_X86_64.SO");
}

#[test]
fn empty_assets_errors() {
    let err = pick_asset("v1.2.3", &[], "x86_64", ".so").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("no assets"), "{msg}");
    // The tag should appear so the user knows which release is empty -
    // a release with zero uploaded assets is usually a draft / mid-CI
    // window, not a misconfigured arch.
    assert!(msg.contains("v1.2.3"), "{msg}");
}

#[test]
fn no_suffix_match_errors() {
    let assets = [asset("plugin.dll"), asset("plugin.dylib")];
    let err = pick_asset("v1.0.0", &assets, "x86_64", ".so").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains(".so"), "{msg}");
    assert!(msg.contains("plugin.dll"), "{msg}");
}

#[test]
fn suffix_matches_but_arch_does_not() {
    let assets = [asset("plugin_aarch64.so")];
    let err = pick_asset("v1.0.0", &assets, "x86_64", ".so").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("arch"), "{msg}");
    assert!(msg.contains("plugin_aarch64.so"), "{msg}");
}

#[test]
fn ambiguous_match_errors_with_candidates() {
    let assets = [asset("plugin_x86_64.so"), asset("plugin_x86_64-debug.so")];
    let err = pick_asset("v1.0.0", &assets, "x86_64", ".so").unwrap_err();
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
    let err = pick_asset("v1.0.0", &assets, "x86", ".so").unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("arch"), "{msg}");
}

#[test]
fn x86_matches_i686_asset() {
    let assets = [asset("plugin_i686.so")];
    let got = pick_asset("v1.0.0", &assets, "x86", ".so").unwrap();
    assert_eq!(got.name, "plugin_i686.so");
}

#[test]
fn arm_matches_armv7_asset() {
    let assets = [asset("plugin_armv7.so")];
    let got = pick_asset("v1.0.0", &assets, "arm", ".so").unwrap();
    assert_eq!(got.name, "plugin_armv7.so");
}

#[test]
fn unsupported_arch_errors() {
    let assets = [asset("plugin.so")];
    let err = pick_asset("v1.0.0", &assets, "riscv64", ".so").unwrap_err();
    assert!(format!("{err}").contains("unsupported arch"));
}

#[test]
fn arm_token_does_not_partial_match_armv8() {
    // Word-bounded: arch="arm" should not match an "armv8" asset because
    // 'v' after "arm" is alphanumeric (no word boundary).
    let assets = [asset("plugin_armv8.so")];
    let err = pick_asset("v1.0.0", &assets, "arm", ".so").unwrap_err();
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

#[test]
fn matches_repo_target_tuple_linux() {
    // Release-asset shape used by cef-loader and friends:
    // `<repo without -plugin>_<os>_<arch>.<ext>`.
    assert!(matches_repo(
        "classicube_cef_loader_linux_x86_64.so",
        "classicube-cef-loader-plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_windows() {
    assert!(matches_repo(
        "classicube_cef_loader_windows_x86_64.dll",
        "classicube-cef-loader-plugin",
        ".dll",
    ));
}

#[test]
fn matches_repo_target_tuple_macos() {
    // Mirrors `picks_self_update_naming` - this is the exact filename our
    // own CI publishes for the mac job.
    assert!(matches_repo(
        "classicube_plugin_manager_macos_aarch64.dylib",
        "classicube-plugin-manager-plugin",
        ".dylib",
    ));
}

#[test]
fn matches_repo_target_tuple_arch_only() {
    // Some release schemes drop the OS token (the suffix already implies it).
    assert!(matches_repo(
        "classicube_foo_x86_64.so",
        "classicube-foo-plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_no_false_match_shorter_sibling() {
    // `classicube-cef-plugin` (short = `classicube-cef`) must NOT match a
    // cef-loader asset. The part after `classicube-cef-` is `loader-...`,
    // which is not a known OS/arch token, so the boundary check rejects it.
    assert!(!matches_repo(
        "classicube_cef_loader_linux_x86_64.so",
        "classicube-cef-plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_requires_plugin_suffix() {
    // Repos without `-plugin` skip the target-tuple branch and only use the
    // existing equality check.
    assert!(!matches_repo(
        "classicube_cef_linux_x86_64.so",
        "classicube-cef",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_is_case_insensitive() {
    assert!(matches_repo(
        "Classicube_Cef_Loader_Linux_X86_64.SO",
        "classicube-cef-loader-plugin",
        ".so",
    ));
    assert!(matches_repo(
        "classicube_cef_loader_linux_x86_64.so",
        "Classicube-Cef-Loader-Plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_rejects_wrong_suffix() {
    // Suffix mismatch shortcuts before the target-tuple branch even runs.
    assert!(!matches_repo(
        "classicube_cef_loader_linux_x86_64.dll",
        "classicube-cef-loader-plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_rejects_no_token_boundary() {
    // After the repo's short prefix, the next segment must be exactly an
    // OS/arch token followed by `-` or end-of-string. `linuxfoo` has no
    // boundary so it isn't a recognized platform token.
    assert!(!matches_repo(
        "classicube_foo_linuxfoo.so",
        "classicube-foo-plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_rejects_extra_segment_before_token() {
    // `classicube-foo-extra-linux-x86-64` doesn't belong to
    // `classicube-foo-plugin` - the segment after the short prefix is
    // `extra`, not an OS/arch token.
    assert!(!matches_repo(
        "classicube_foo_extra_linux_x86_64.so",
        "classicube-foo-plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_os_only() {
    // Some schemes drop the arch token and let the suffix carry the OS.
    // `_<os>` alone after the short prefix should still be enough.
    assert!(matches_repo(
        "classicube_foo_linux.so",
        "classicube-foo-plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_amd64_alias() {
    // `amd64` is a recognized alias of `x86_64` in arch-token form.
    assert!(matches_repo(
        "classicube_foo_linux_amd64.so",
        "classicube-foo-plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_unknown_arch_does_not_match() {
    // We only know about a fixed set of arches; an unfamiliar one (here
    // `armv5`) shouldn't false-match. If it ever shows up in real releases,
    // extend `starts_with_platform_token`.
    assert!(!matches_repo(
        "classicube_foo_armv5.so",
        "classicube-foo-plugin",
        ".so",
    ));
}

#[test]
fn matches_repo_target_tuple_repo_dash_plugin_only() {
    // Pathological-but-legal: a repo literally named `-plugin` strips to
    // empty short. We don't need to support it, but the code should not
    // panic and should not match arbitrary `<token>.so` files.
    assert!(!matches_repo("linux_x86_64.so", "-plugin", ".so"));
}

#[test]
fn starts_with_platform_token_empty_is_false() {
    assert!(!starts_with_platform_token(""));
}

#[test]
fn starts_with_platform_token_bare_os_tokens() {
    for os in ["linux", "windows", "macos", "darwin"] {
        assert!(starts_with_platform_token(os), "bare {os} should match");
    }
}

#[test]
fn starts_with_platform_token_bare_arch_tokens() {
    // Tokens are post-normalization (`_`->`-`), so the x86_64 alias is `x86-64`.
    for arch in [
        "x86-64", "amd64", "aarch64", "arm64", "armv8", "armv7", "i686", "i386", "x86", "arm",
    ] {
        assert!(starts_with_platform_token(arch), "bare {arch} should match");
    }
}

#[test]
fn starts_with_platform_token_with_separator_and_more() {
    assert!(starts_with_platform_token("linux-x86-64"));
    assert!(starts_with_platform_token("darwin-arm64"));
    assert!(starts_with_platform_token("x86-64-debug"));
    assert!(starts_with_platform_token("arm-anything"));
}

#[test]
fn starts_with_platform_token_requires_word_boundary() {
    // Token must be followed by `-` or end of string. Alphanumeric
    // continuation means no match.
    assert!(!starts_with_platform_token("linuxfoo"));
    assert!(!starts_with_platform_token("armfoo"));
    assert!(!starts_with_platform_token("x86debug"));
    // x86-64 starts with `x86`, but the `-` after `x86` is the boundary, so
    // `x86` matches and `-64` is the remainder. Confirm explicitly.
    assert!(starts_with_platform_token("x86-debug"));
}

#[test]
fn starts_with_platform_token_arm_word_boundary_does_not_partial_match() {
    // `arm` should not partial-match `armv5` (no `-` after `arm`, and
    // `armv5` itself isn't in our list).
    assert!(!starts_with_platform_token("armv5"));
    // But `armv7` and `armv8` are explicitly listed.
    assert!(starts_with_platform_token("armv7"));
    assert!(starts_with_platform_token("armv8"));
}

#[test]
fn starts_with_platform_token_rejects_non_token_prefix() {
    assert!(!starts_with_platform_token("loader-linux-x86-64"));
    assert!(!starts_with_platform_token("plugin-darwin-arm64"));
    assert!(!starts_with_platform_token("foo"));
    assert!(!starts_with_platform_token("-linux"));
}

#[test]
fn is_canonical_or_cdylib_name_matches_canonical() {
    assert!(is_canonical_or_cdylib_name(
        "classicube-foo-plugin.so",
        "classicube-foo-plugin",
        ".so",
    ));
}

#[test]
fn is_canonical_or_cdylib_name_matches_unix_cdylib_variant() {
    // Linux/macOS rust-cdylib output: `lib` prefix + underscores.
    assert!(is_canonical_or_cdylib_name(
        "libclassicube_foo_plugin.so",
        "classicube-foo-plugin",
        ".so",
    ));
    assert!(is_canonical_or_cdylib_name(
        "libclassicube_foo_plugin.dylib",
        "classicube-foo-plugin",
        ".dylib",
    ));
}

#[test]
fn is_canonical_or_cdylib_name_matches_windows_cdylib_variant() {
    // Windows rust-cdylib output: no `lib` prefix, underscores.
    assert!(is_canonical_or_cdylib_name(
        "classicube_foo_plugin.dll",
        "classicube-foo-plugin",
        ".dll",
    ));
}

#[test]
fn is_canonical_or_cdylib_name_rejects_target_tuple_release_asset() {
    // The whole point: released assets carry OS/arch tokens after the
    // repo prefix and must NOT register as a dev-build name.
    assert!(!is_canonical_or_cdylib_name(
        "classicube_foo_linux_x86_64.so",
        "classicube-foo-plugin",
        ".so",
    ));
    assert!(!is_canonical_or_cdylib_name(
        "classicube_plugin_manager_windows_x86_64.dll",
        "classicube-plugin-manager-plugin",
        ".dll",
    ));
}

#[test]
fn is_canonical_or_cdylib_name_rejects_versioned_name() {
    // Our own write target `<owner>-<repo>-<tag>.<ext>` must not match
    // either - it carries an extra prefix and a tag suffix.
    assert!(!is_canonical_or_cdylib_name(
        "SpiralP-classicube-plugin-manager-plugin-v0.3.1.so",
        "classicube-plugin-manager-plugin",
        ".so",
    ));
}

#[test]
fn is_canonical_or_cdylib_name_is_case_insensitive() {
    assert!(is_canonical_or_cdylib_name(
        "LibClassicube_Foo_Plugin.SO",
        "classicube-foo-plugin",
        ".so",
    ));
}

#[test]
fn is_canonical_or_cdylib_name_rejects_wrong_suffix() {
    assert!(!is_canonical_or_cdylib_name(
        "libclassicube_foo_plugin.dll",
        "classicube-foo-plugin",
        ".so",
    ));
}
