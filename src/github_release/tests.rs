use super::*;

#[test]
fn parses_minimal_release() {
    let json = br#"{"tag_name":"v1.0.0"}"#;
    let r: GitHubRelease = serde_json::from_slice(json).unwrap();
    assert_eq!(r.tag_name, "v1.0.0");
    assert!(r.assets.is_empty());
}

#[test]
fn parses_release_with_no_assets_field() {
    // Releases without an `assets` key (rare, but `#[serde(default)]` should
    // make it lenient) should deserialize to an empty vec, not fail.
    let json = br#"{"tag_name":"v0.0.1"}"#;
    let r: GitHubRelease = serde_json::from_slice(json).unwrap();
    assert!(r.assets.is_empty());
}

#[test]
fn ignores_extra_release_fields() {
    // Trimmed shape of an actual GitHub /releases/latest payload —
    // confirms that the many fields we don't model don't break parsing.
    let json = br#"{
        "url": "https://api.github.com/repos/o/r/releases/1",
        "id": 1,
        "tag_name": "v2.3.4",
        "name": "v2.3.4",
        "draft": false,
        "prerelease": false,
        "created_at": "2026-01-01T00:00:00Z",
        "published_at": "2026-01-01T00:00:00Z",
        "assets": [
            {"name": "plugin.so", "browser_download_url": "https://example/plugin.so"},
            {"name": "plugin.dll", "browser_download_url": "https://example/plugin.dll"}
        ],
        "body": "Release notes here."
    }"#;
    let r: GitHubRelease = serde_json::from_slice(json).unwrap();
    assert_eq!(r.tag_name, "v2.3.4");
    assert_eq!(r.assets.len(), 2);
    assert_eq!(r.assets[0].name, "plugin.so");
    assert_eq!(
        r.assets[0].browser_download_url,
        "https://example/plugin.so"
    );
    assert_eq!(r.assets[1].name, "plugin.dll");
}

#[test]
fn parses_error_payload() {
    let json = br#"{"message":"Not Found","documentation_url":"https://docs"}"#;
    let e: GitHubError = serde_json::from_slice(json).unwrap();
    assert_eq!(e.message, "Not Found");
}

#[test]
fn release_payload_does_not_match_error_shape() {
    // Sanity check: the success-path body does not accidentally deserialize
    // into GitHubError (the get_latest_release flow tries error first).
    let json = br#"{"tag_name":"v1.0.0"}"#;
    assert!(serde_json::from_slice::<GitHubError>(json).is_err());
}
