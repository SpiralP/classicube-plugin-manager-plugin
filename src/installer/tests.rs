use std::fs;

use tempfile::tempdir;

use super::*;

// SHA-256 of b"hello", computed once and verified against `printf hello | sha256sum`.
const HELLO_DIGEST: &str =
    "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

#[test]
fn fresh_install_writes_file_with_no_artifacts() {
    let dir = tempdir().unwrap();
    let path = install_bytes_to(dir.path(), "plugin.so", b"hello", None).unwrap();

    assert_eq!(path, dir.path().join("plugin.so"));
    assert_eq!(fs::read(&path).unwrap(), b"hello");

    // No .new / .old leftovers from a successful install.
    assert!(!dir.path().join("plugin.so.new").exists());
    assert!(!dir.path().join("plugin.so.old").exists());
}

#[test]
fn replaces_existing_file_and_cleans_up() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("plugin.so"), b"old").unwrap();

    install_bytes_to(dir.path(), "plugin.so", b"new", None).unwrap();

    assert_eq!(fs::read(dir.path().join("plugin.so")).unwrap(), b"new");
    assert!(!dir.path().join("plugin.so.new").exists());
    assert!(!dir.path().join("plugin.so.old").exists());
}

#[test]
fn creates_missing_parent_directory() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("a").join("b");
    assert!(!nested.exists());

    let path = install_bytes_to(&nested, "plugin.so", b"data", None).unwrap();

    assert!(nested.is_dir());
    assert_eq!(fs::read(&path).unwrap(), b"data");
}

#[test]
fn overwrites_stale_new_artifact() {
    // A previous failed run could have left a .new behind; the next install
    // should overwrite it cleanly.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("plugin.so.new"), b"stale-new").unwrap();

    install_bytes_to(dir.path(), "plugin.so", b"fresh", None).unwrap();

    assert_eq!(fs::read(dir.path().join("plugin.so")).unwrap(), b"fresh");
    assert!(!dir.path().join("plugin.so.new").exists());
}

#[test]
fn cleans_up_leftover_old_artifact() {
    // A previous failed run could have left a .old behind. The current run
    // moves the existing final file aside (overwriting the stale .old via
    // rename), then removes .old at the end.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("plugin.so"), b"existing").unwrap();
    fs::write(dir.path().join("plugin.so.old"), b"stale-old").unwrap();

    install_bytes_to(dir.path(), "plugin.so", b"new", None).unwrap();

    assert_eq!(fs::read(dir.path().join("plugin.so")).unwrap(), b"new");
    assert!(!dir.path().join("plugin.so.old").exists());
}

#[test]
fn matching_digest_installs() {
    let dir = tempdir().unwrap();
    let path = install_bytes_to(dir.path(), "plugin.so", b"hello", Some(HELLO_DIGEST)).unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"hello");
    assert!(!dir.path().join("plugin.so.new").exists());
    assert!(!dir.path().join("plugin.so.old").exists());
}

#[test]
fn mismatched_digest_writes_nothing() {
    let dir = tempdir().unwrap();
    // Wrong digest — last hex flipped from `4` to `5`.
    let wrong = "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9825";

    let err = install_bytes_to(dir.path(), "plugin.so", b"hello", Some(wrong)).unwrap_err();
    assert!(format!("{err:#}").contains("sha256 mismatch"));

    // No file, no `.new`, no `.old` — verification ran before any disk write.
    assert!(!dir.path().join("plugin.so").exists());
    assert!(!dir.path().join("plugin.so.new").exists());
    assert!(!dir.path().join("plugin.so.old").exists());
}

#[test]
fn mismatched_digest_preserves_existing_good_file() {
    // The user-visible safety property: a failed update must not destroy the
    // currently-installed plugin.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("plugin.so"), b"good-old-version").unwrap();
    let wrong = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    let err = install_bytes_to(dir.path(), "plugin.so", b"hello", Some(wrong)).unwrap_err();
    assert!(format!("{err:#}").contains("sha256 mismatch"));

    assert_eq!(
        fs::read(dir.path().join("plugin.so")).unwrap(),
        b"good-old-version",
    );
    assert!(!dir.path().join("plugin.so.new").exists());
    assert!(!dir.path().join("plugin.so.old").exists());
}

#[test]
fn malformed_expected_digest_errors_before_write() {
    let dir = tempdir().unwrap();
    let err =
        install_bytes_to(dir.path(), "plugin.so", b"hello", Some("not-a-digest")).unwrap_err();
    assert!(format!("{err:#}").contains("sha256:"));

    assert!(!dir.path().join("plugin.so").exists());
    assert!(!dir.path().join("plugin.so.new").exists());
}

#[test]
fn parse_sha256_digest_accepts_canonical() {
    let bytes = parse_sha256_digest(HELLO_DIGEST).unwrap();
    assert_eq!(bytes[0], 0x2c);
    assert_eq!(bytes[31], 0x24);
}

#[test]
fn parse_sha256_digest_rejects_missing_prefix() {
    let bare = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    assert!(parse_sha256_digest(bare).is_err());
}

#[test]
fn parse_sha256_digest_rejects_other_algos() {
    assert!(
        parse_sha256_digest(
            "sha512:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        )
        .is_err()
    );
    assert!(parse_sha256_digest("md5:2cf24dba5fb0a30e26e83b2ac5b9e29e").is_err());
}

#[test]
fn parse_sha256_digest_rejects_wrong_length() {
    // 63 chars
    assert!(
        parse_sha256_digest(
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b982"
        )
        .is_err()
    );
    // 65 chars
    assert!(
        parse_sha256_digest(
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b98244"
        )
        .is_err()
    );
    // empty
    assert!(parse_sha256_digest("sha256:").is_err());
}

#[test]
fn parse_sha256_digest_rejects_uppercase() {
    // Be strict — GitHub returns lowercase. Uppercase indicates the digest came
    // from somewhere else and we'd rather flag it than silently canonicalize.
    let upper = "sha256:2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824";
    assert!(parse_sha256_digest(upper).is_err());
}

#[test]
fn parse_sha256_digest_rejects_non_hex() {
    // 'g' isn't hex
    let bad = "sha256:gcf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    assert!(parse_sha256_digest(bad).is_err());
}

#[test]
fn versioned_filename_is_simple_when_inputs_are_clean() {
    let got = versioned_managed_filename("SpiralP", "classicube-leash-plugin", "v0.3.1", ".so");
    assert_eq!(got, "SpiralP-classicube-leash-plugin-v0.3.1.so");
}

#[test]
fn versioned_filename_preserves_safe_chars() {
    let got = versioned_managed_filename("o", "r", "1.2.3-rc.4_alpha", ".dll");
    assert_eq!(got, "o-r-1.2.3-rc.4_alpha.dll");
}

#[test]
fn versioned_filename_sanitizes_unsafe_chars() {
    // Anything outside [A-Za-z0-9._-] maps to `_`.
    let got = versioned_managed_filename("o", "r", "a/b c+d", ".so");
    assert_eq!(got, "o-r-a_b_c_d.so");
}

#[test]
fn versioned_filename_replaces_non_ascii() {
    // Non-ASCII isn't `is_ascii_alphanumeric`, so each scalar becomes `_`.
    let got = versioned_managed_filename("o", "r", "café", ".so");
    assert_eq!(got, "o-r-caf_.so");
}

#[test]
fn versioned_filename_blocks_path_traversal() {
    // The sanitized tag is one filename component - no slashes can
    // survive, so it can never escape MANAGED_DIR even with a malicious
    // tag.
    let got = versioned_managed_filename("o", "r", "../../etc/passwd", ".so");
    assert!(!got.contains('/'));
}

#[test]
fn versioned_filename_caps_long_tags_at_64() {
    let long_tag = "a".repeat(200);
    let got = versioned_managed_filename("o", "r", &long_tag, ".so");
    // Prefix `o-r-` (4 bytes) + 64 `a`s + `.so` (3 bytes).
    assert_eq!(got.len(), 4 + 64 + 3);
    assert!(got.starts_with("o-r-"));
    assert!(got.ends_with(".so"));
}

#[test]
fn cleanup_previous_managed_noop_when_previous_is_none() {
    // A bogus dir is fine - the helper short-circuits before touching it.
    cleanup_previous_managed(Path::new("/__nope__"), None, "anything.so");
}

#[test]
fn cleanup_previous_managed_noop_when_previous_equals_new() {
    // Caller passing the same name means "no rename happened" - we'd
    // otherwise unlink the file we just persisted as the claim.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("same.so"), b"data").unwrap();

    cleanup_previous_managed(dir.path(), Some("same.so"), "same.so");

    assert!(dir.path().join("same.so").exists());
}

#[test]
fn cleanup_previous_managed_deletes_when_different() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("old.so"), b"old").unwrap();
    fs::write(dir.path().join("new.so"), b"new").unwrap();

    cleanup_previous_managed(dir.path(), Some("old.so"), "new.so");

    assert!(!dir.path().join("old.so").exists());
    assert!(dir.path().join("new.so").exists());
}

#[test]
fn cleanup_previous_managed_swallows_missing_file() {
    let dir = tempdir().unwrap();
    // Should not panic, should not error - ENOENT is the silent path.
    cleanup_previous_managed(dir.path(), Some("never-existed.so"), "fresh.so");
}
