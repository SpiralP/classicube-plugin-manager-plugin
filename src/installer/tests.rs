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
