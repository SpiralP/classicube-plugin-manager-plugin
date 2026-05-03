use super::*;

fn sub(owner: &str, repo: &str) -> Subscription {
    Subscription {
        owner: owner.into(),
        repo: repo.into(),
        channel: crate::config::Channel::default(),
        disabled: false,
        installed_version: None,
        installed_asset: None,
        installed_at: None,
        cached_tag: None,
        cached_at: None,
        cached_published_at: None,
    }
}

#[test]
fn bare_name_expands_with_default_owner() {
    assert_eq!(
        expand_candidates("foo"),
        Some(vec![
            ("SpiralP".into(), "foo".into()),
            ("SpiralP".into(), "classicube-foo-plugin".into()),
        ])
    );
}

#[test]
fn owner_and_short_repo_expands() {
    assert_eq!(
        expand_candidates("octocat/foo"),
        Some(vec![
            ("octocat".into(), "foo".into()),
            ("octocat".into(), "classicube-foo-plugin".into()),
        ])
    );
}

#[test]
fn already_canonical_repo_no_expansion() {
    assert_eq!(
        expand_candidates("octocat/classicube-foo-plugin"),
        Some(vec![("octocat".into(), "classicube-foo-plugin".into())])
    );
}

#[test]
fn bare_canonical_repo_uses_default_owner() {
    assert_eq!(
        expand_candidates("classicube-foo-plugin"),
        Some(vec![("SpiralP".into(), "classicube-foo-plugin".into())])
    );
}

#[test]
fn allows_dots_hyphens_underscores() {
    assert_eq!(
        expand_candidates("hyphen-name/under_score.dot"),
        Some(vec![
            ("hyphen-name".into(), "under_score.dot".into()),
            (
                "hyphen-name".into(),
                "classicube-under_score.dot-plugin".into()
            ),
        ])
    );
}

#[test]
fn empty_string_rejected() {
    assert_eq!(expand_candidates(""), None);
}

#[test]
fn missing_repo_rejected() {
    assert_eq!(expand_candidates("owner/"), None);
}

#[test]
fn missing_owner_rejected() {
    assert_eq!(expand_candidates("/repo"), None);
}

#[test]
fn whitespace_rejected() {
    assert_eq!(expand_candidates("a b/repo"), None);
    assert_eq!(expand_candidates("owner /repo"), None);
    assert_eq!(expand_candidates("owner/ repo"), None);
    assert_eq!(expand_candidates("owner/re po"), None);
    // Bare name with whitespace is also invalid (would map to default owner).
    assert_eq!(expand_candidates("a b"), None);
}

#[test]
fn nested_slash_rejected() {
    // split_once takes the first '/', so "a/b/c" → owner="a", repo="b/c".
    // The repo-side slash check rejects it.
    assert_eq!(expand_candidates("a/b/c"), None);
}

#[test]
fn prefix_only_still_expands() {
    // Prefix without `-plugin` suffix is not canonical.
    assert_eq!(
        expand_candidates("classicube-bar"),
        Some(vec![
            ("SpiralP".into(), "classicube-bar".into()),
            ("SpiralP".into(), "classicube-classicube-bar-plugin".into()),
        ])
    );
}

#[test]
fn suffix_only_still_expands() {
    // Suffix without `classicube-` prefix is not canonical.
    assert_eq!(
        expand_candidates("bar-plugin"),
        Some(vec![
            ("SpiralP".into(), "bar-plugin".into()),
            ("SpiralP".into(), "classicube-bar-plugin-plugin".into()),
        ])
    );
}

#[test]
fn empty_middle_still_expands() {
    // `classicube--plugin` has prefix and suffix but no name; treat as
    // not-yet-expanded so we still try the (likely bogus) further expansion.
    assert_eq!(
        expand_candidates("classicube--plugin"),
        Some(vec![
            ("SpiralP".into(), "classicube--plugin".into()),
            (
                "SpiralP".into(),
                "classicube-classicube--plugin-plugin".into()
            ),
        ])
    );
}

#[test]
fn find_subscription_picks_literal_over_expanded() {
    // Both forms are subscribed; the literal candidate (index 0) wins even
    // when it appears later in the subscription list.
    let config = Config {
        subscriptions: vec![
            sub("SpiralP", "classicube-foo-plugin"),
            sub("SpiralP", "foo"),
        ],
    };
    let candidates = vec![
        ("SpiralP".into(), "foo".into()),
        ("SpiralP".into(), "classicube-foo-plugin".into()),
    ];
    assert_eq!(find_subscription_index(&config, &candidates), Some(1));
}

#[test]
fn find_subscription_falls_back_to_expanded() {
    let config = Config {
        subscriptions: vec![sub("SpiralP", "classicube-foo-plugin")],
    };
    let candidates = vec![
        ("SpiralP".into(), "foo".into()),
        ("SpiralP".into(), "classicube-foo-plugin".into()),
    ];
    assert_eq!(find_subscription_index(&config, &candidates), Some(0));
}

#[test]
fn find_subscription_case_insensitive() {
    let config = Config {
        subscriptions: vec![sub("SpiralP", "classicube-foo-plugin")],
    };
    let candidates = vec![("spiralp".into(), "CLASSICUBE-FOO-PLUGIN".into())];
    assert_eq!(find_subscription_index(&config, &candidates), Some(0));
}

#[test]
fn parse_channel_args_empty_is_stable() {
    assert_eq!(parse_channel_args(&[]), Ok(Channel::Stable));
}

#[test]
fn parse_channel_args_named_variants() {
    assert_eq!(parse_channel_args(&["stable"]), Ok(Channel::Stable));
    assert_eq!(parse_channel_args(&["prerelease"]), Ok(Channel::Prerelease));
}

#[test]
fn parse_channel_args_two_arg_tag_form() {
    assert_eq!(
        parse_channel_args(&["tag", "v1.2.3"]),
        Ok(Channel::Tag("v1.2.3".into())),
    );
}

#[test]
fn parse_channel_args_colon_tag_form() {
    assert_eq!(
        parse_channel_args(&["tag:v1.2.3"]),
        Ok(Channel::Tag("v1.2.3".into())),
    );
}

#[test]
fn parse_channel_args_rejects_empty_tag() {
    assert!(parse_channel_args(&["tag", ""]).is_err());
    assert!(parse_channel_args(&["tag:"]).is_err());
}

#[test]
fn parse_channel_args_rejects_unknown_keyword() {
    assert!(parse_channel_args(&["nightly"]).is_err());
}

#[test]
fn parse_channel_args_rejects_bare_tag() {
    // A bare `tag` with no ref falls into the single-arg branch and fails to
    // parse — it's neither "stable", "prerelease", nor a "tag:" prefix.
    assert!(parse_channel_args(&["tag"]).is_err());
}

#[test]
fn apply_channel_switch_clears_cache_fields() {
    let mut s = Subscription {
        channel: Channel::Stable,
        cached_tag: Some("v1.0.0".into()),
        cached_at: Some(100),
        cached_published_at: Some(50),
        ..sub("a", "b")
    };
    apply_channel_switch(&mut s, Channel::Prerelease);
    assert_eq!(s.channel, Channel::Prerelease);
    assert!(s.cached_tag.is_none());
    assert!(s.cached_at.is_none());
    assert!(s.cached_published_at.is_none());
}

#[test]
fn apply_channel_switch_preserves_installed_state() {
    // installed_* describes what's on disk, which doesn't change just because
    // the user pointed the subscription at a different channel.
    let mut s = Subscription {
        channel: Channel::Stable,
        installed_version: Some("v1.0.0".into()),
        installed_asset: Some("a.so".into()),
        installed_at: Some(500),
        ..sub("a", "b")
    };
    apply_channel_switch(&mut s, Channel::Tag("v2.0.0".into()));
    assert_eq!(s.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(s.installed_asset.as_deref(), Some("a.so"));
    assert_eq!(s.installed_at, Some(500));
}

#[test]
fn parse_channel_args_rejects_extra_args() {
    assert!(parse_channel_args(&["stable", "extra"]).is_err());
    assert!(parse_channel_args(&["tag", "v1", "extra"]).is_err());
}

#[test]
fn channel_suffix_skips_default() {
    assert_eq!(channel_suffix(&Channel::Stable), "");
    assert!(channel_suffix(&Channel::Prerelease).contains("prerelease"));
    assert!(channel_suffix(&Channel::Tag("v1".into())).contains("tag: v1"));
}

#[test]
fn find_subscription_no_match() {
    let config = Config::default();
    let candidates = vec![("SpiralP".into(), "foo".into())];
    assert_eq!(find_subscription_index(&config, &candidates), None);
}
