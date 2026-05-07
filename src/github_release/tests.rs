use super::*;

#[test]
fn parses_minimal_release() {
    let json = br#"{"tag_name":"v1.0.0","published_at":"2024-12-15T12:34:56Z"}"#;
    let r: GitHubRelease = serde_json::from_slice(json).unwrap();
    assert_eq!(r.tag_name, "v1.0.0");
    assert_eq!(r.published_at, 1_734_266_096);
    assert!(r.assets.is_empty());
}

#[test]
fn parses_release_with_no_assets_field() {
    // Releases without an `assets` key (rare, but `#[serde(default)]` should
    // make it lenient) should deserialize to an empty vec, not fail.
    let json = br#"{"tag_name":"v0.0.1","published_at":"2024-01-01T00:00:00Z"}"#;
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
            {"name": "plugin.so", "url": "https://api.example/assets/1", "browser_download_url": "https://example/plugin.so"},
            {"name": "plugin.dll", "url": "https://api.example/assets/2", "browser_download_url": "https://example/plugin.dll"}
        ],
        "body": "Release notes here."
    }"#;
    let r: GitHubRelease = serde_json::from_slice(json).unwrap();
    assert_eq!(r.tag_name, "v2.3.4");
    assert_eq!(r.published_at, 1_767_225_600);
    assert_eq!(r.assets.len(), 2);
    assert_eq!(r.assets[0].name, "plugin.so");
    assert_eq!(r.assets[0].url, "https://api.example/assets/1");
    assert_eq!(r.assets[1].name, "plugin.dll");
}

#[test]
fn parses_asset_with_digest() {
    let json = br#"{
        "name": "plugin.so",
        "url": "https://api.example/assets/1",
        "browser_download_url": "https://example/plugin.so",
        "digest": "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    }"#;
    let a: GitHubReleaseAsset = serde_json::from_slice(json).unwrap();
    assert_eq!(
        a.digest.as_deref(),
        Some("sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"),
    );
}

#[test]
fn parses_asset_without_digest_field() {
    let json = br#"{
        "name": "plugin.so",
        "url": "https://api.example/assets/1",
        "browser_download_url": "https://example/plugin.so"
    }"#;
    let a: GitHubReleaseAsset = serde_json::from_slice(json).unwrap();
    assert!(a.digest.is_none());
}

#[test]
fn parses_releases_list_picks_newest_by_published_at() {
    // Mirrors the `/releases?per_page=N` payload that `fetch_newest_release`
    // hits for the prerelease channel. The selection logic is one line —
    // `max_by_key(|r| r.published_at)` — but exercising it here pins down
    // both the JSON-list parse and the contract that ordering in the payload
    // doesn't matter (real GitHub responses are usually but not strictly
    // sorted).
    let json = br#"[
        {"tag_name":"v1.0.0","published_at":"2024-01-15T00:00:00Z"},
        {"tag_name":"v1.1.0-rc1","published_at":"2024-03-01T00:00:00Z"},
        {"tag_name":"v0.9.0","published_at":"2024-02-10T00:00:00Z"}
    ]"#;
    let releases: Vec<GitHubRelease> = serde_json::from_slice(json).unwrap();
    let newest = releases.into_iter().max_by_key(|r| r.published_at).unwrap();
    assert_eq!(newest.tag_name, "v1.1.0-rc1");
}

#[test]
fn parses_empty_releases_list() {
    // A repo with no releases (yet) returns `[]`. The prerelease channel
    // should treat that as "nothing to install" rather than crashing on the
    // parse — handled at the call site by `max_by_key` returning None.
    let json = b"[]";
    let releases: Vec<GitHubRelease> = serde_json::from_slice(json).unwrap();
    assert!(releases.is_empty());
}

#[test]
fn missing_published_at_fails() {
    // GitHub always sends `published_at` for a published release; absence is
    // a real signal that something's wrong with the payload, not something
    // we want to paper over.
    let json = br#"{"tag_name":"v1.0.0"}"#;
    assert!(serde_json::from_slice::<GitHubRelease>(json).is_err());
}

#[test]
fn malformed_published_at_fails() {
    let json = br#"{"tag_name":"v1.0.0","published_at":"not-a-date"}"#;
    assert!(serde_json::from_slice::<GitHubRelease>(json).is_err());
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
    let json = br#"{"tag_name":"v1.0.0","published_at":"2024-01-01T00:00:00Z"}"#;
    assert!(serde_json::from_slice::<GitHubError>(json).is_err());
}

#[test]
fn iso8601_epoch() {
    assert_eq!(parse_iso8601_z("1970-01-01T00:00:00Z"), Some(0));
}

#[test]
fn iso8601_known_date() {
    // 2024-12-15T12:34:56Z verified via `date -u -d ... +%s`.
    assert_eq!(parse_iso8601_z("2024-12-15T12:34:56Z"), Some(1_734_266_096),);
}

#[test]
fn iso8601_leap_day() {
    // 2024-02-29 is valid; 2023-02-29 is not.
    assert!(parse_iso8601_z("2024-02-29T00:00:00Z").is_some());
    assert_eq!(parse_iso8601_z("2023-02-29T00:00:00Z"), None);
}

#[test]
fn iso8601_century_leap_rule() {
    // 2000 is a leap year (divisible by 400); 2100 is not (divisible by 100
    // but not 400). The parser only sees years up to 9999 so 2100 is the
    // smallest century non-leap we can hit.
    assert!(parse_iso8601_z("2000-02-29T00:00:00Z").is_some());
    assert_eq!(parse_iso8601_z("2100-02-29T00:00:00Z"), None);
}

#[test]
fn classify_anonymous_404_hints_private_repo() {
    // The whole point of the hint: an anonymous 404 may mean "private repo",
    // not "wrong owner/repo", and the user can't tell from the generic
    // `message: "Not Found"` body. The hint nudges them toward the token
    // workflow without forcing them to read the README first.
    let body = br#"{"message":"Not Found","documentation_url":"https://docs"}"#;
    let err = classify_error(StatusCode::NOT_FOUND, false, body);
    let msg = format!("{err:#}");
    assert!(
        msg.contains("private") && msg.contains("token"),
        "expected private-repo hint in: {msg}",
    );
}

#[test]
fn classify_authed_404_mentions_token_access() {
    // 404 with a token attached has two real causes: the repo is gone, or
    // the token can't see it (fine-grained PATs return 404 for repos
    // they aren't scoped to). Don't repeat the anonymous "add a token"
    // hint - they have one - but do mention the access possibility so a
    // misconfigured fine-grained PAT isn't invisible.
    let body = br#"{"message":"Not Found"}"#;
    let err = classify_error(StatusCode::NOT_FOUND, true, body);
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("private"),
        "should not repeat the anonymous private-repo hint: {msg}",
    );
    assert!(
        msg.contains("lacks") && msg.contains("access"),
        "expected token-access wording in: {msg}",
    );
    assert!(msg.contains("Not Found"), "expected api message in: {msg}");
}

#[test]
fn classify_401_with_token_hints_expiry() {
    let body = br#"{"message":"Bad credentials"}"#;
    let err = classify_error(StatusCode::UNAUTHORIZED, true, body);
    let msg = format!("{err:#}");
    assert!(
        msg.contains("auth failed") && msg.contains("expired"),
        "expected expiry hint in: {msg}",
    );
}

#[test]
fn classify_403_with_token_hints_scope() {
    // 403 with a token usually means the PAT is valid but lacks
    // `Contents: Read` on this specific repo. Same hint as 401 — the user's
    // fix is to re-mint with the right scope.
    let body = br#"{"message":"Forbidden"}"#;
    let err = classify_error(StatusCode::FORBIDDEN, true, body);
    let msg = format!("{err:#}");
    assert!(
        msg.contains("auth failed"),
        "expected auth-failed hint in: {msg}",
    );
}

#[test]
fn classify_falls_back_to_api_message() {
    // For any other non-success status, prefer GitHub's `message` field over
    // a generic "HTTP 422" — the API message is usually more actionable.
    let body = br#"{"message":"Validation Failed"}"#;
    let err = classify_error(StatusCode::UNPROCESSABLE_ENTITY, false, body);
    assert_eq!(format!("{err}"), "Validation Failed");
}

#[test]
fn classify_falls_back_to_status_for_non_json_body() {
    let err = classify_error(StatusCode::BAD_GATEWAY, false, b"<html>nope</html>");
    let msg = format!("{err}");
    assert!(msg.contains("502"), "expected status in fallback: {msg}");
}

#[test]
fn stable_404_probe_empty_list_says_no_releases() {
    // Stable channel hit /releases/latest -> 404, then probe of
    // /releases?per_page=1 returned 200 []. Repo exists, just has nothing
    // published yet. Message must not nag about a token (which wouldn't
    // help here).
    let err = classify_stable_404_probe(
        "owner",
        "repo",
        StatusCode::OK,
        b"[]",
        false, // anonymous - if we still showed the private-repo hint, it'd be wrong
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("no published releases"),
        "expected no-releases message: {msg}"
    );
    assert!(msg.contains("owner/repo"), "expected repo in: {msg}");
    assert!(
        !msg.contains("private"),
        "should not show private-repo hint when probe shows repo exists: {msg}"
    );
    assert!(
        !msg.contains("token"),
        "should not show token hint when probe shows repo exists: {msg}"
    );
}

#[test]
fn stable_404_probe_nonempty_list_suggests_prerelease_channel() {
    // Stable hit 404, but probe found at least one release - meaning the
    // repo has only prereleases / drafts. Suggest the prerelease channel
    // instead of the token hint.
    let body = br#"[{"tag_name":"v1.0.0-rc1","published_at":"2024-12-15T12:34:56Z"}]"#;
    let err = classify_stable_404_probe("owner", "repo", StatusCode::OK, body, false);
    let msg = format!("{err:#}");
    assert!(
        msg.contains("prerelease"),
        "expected prerelease hint: {msg}"
    );
    assert!(msg.contains("owner/repo"), "expected repo in: {msg}");
    assert!(!msg.contains("private"), "{msg}");
}

#[test]
fn stable_404_probe_falls_back_to_classify_when_probe_also_404s() {
    // Probe also 404'd - the repo really is missing or private. Defer to
    // the standard classifier so the anonymous-404 token hint still shows.
    let body = br#"{"message":"Not Found"}"#;
    let err = classify_stable_404_probe("owner", "repo", StatusCode::NOT_FOUND, body, false);
    let msg = format!("{err:#}");
    assert!(msg.contains("private"), "expected token hint: {msg}");
    assert!(msg.contains("token"), "expected token hint: {msg}");
}

#[test]
fn stable_404_probe_authed_probe_404_keeps_authed_message() {
    // Authed and probe still 404s -> defer to classify_error, which spells
    // out both possibilities (repo missing OR token lacks access) because
    // fine-grained PATs return 404 for repos they can't see. Still
    // suppress the anonymous "add a token" hint.
    let body = br#"{"message":"Not Found"}"#;
    let err = classify_stable_404_probe("owner", "repo", StatusCode::NOT_FOUND, body, true);
    let msg = format!("{err:#}");
    assert!(!msg.contains("private"), "{msg}");
    assert!(
        msg.contains("lacks") && msg.contains("access"),
        "expected token-access wording in: {msg}",
    );
    assert!(msg.contains("Not Found"), "expected api message: {msg}");
}

#[test]
fn stable_404_probe_unexpected_body_does_not_panic() {
    // Probe returned 200 but the body wasn't a JSON array - unexpected,
    // but we want a chat-shaped error rather than a parse panic.
    let err =
        classify_stable_404_probe("owner", "repo", StatusCode::OK, b"<html>...</html>", false);
    let msg = format!("{err:#}");
    assert!(
        msg.contains("probe"),
        "expected probe-shaped message: {msg}"
    );
}

#[test]
fn iso8601_rejects_malformed() {
    // Wrong length / missing separators / wrong tz indicator — all None.
    assert_eq!(parse_iso8601_z(""), None);
    assert_eq!(parse_iso8601_z("not-a-date"), None);
    assert_eq!(parse_iso8601_z("2024-12-15T12:34:56"), None); // missing Z
    assert_eq!(parse_iso8601_z("2024-12-15T12:34:56+0000"), None); // not Z
    assert_eq!(parse_iso8601_z("2024/12/15T12:34:56Z"), None); // wrong sep
    assert_eq!(parse_iso8601_z("2024-13-01T00:00:00Z"), None); // bad month
    assert_eq!(parse_iso8601_z("2024-12-32T00:00:00Z"), None); // bad day
    assert_eq!(parse_iso8601_z("2024-12-15T24:00:00Z"), None); // bad hour
    assert_eq!(parse_iso8601_z("2024-12-15T12:60:00Z"), None); // bad minute
    assert_eq!(parse_iso8601_z("2024-12-15T12:00:60Z"), None); // bad second
    assert_eq!(parse_iso8601_z("1969-12-31T23:59:59Z"), None); // pre-epoch
}
