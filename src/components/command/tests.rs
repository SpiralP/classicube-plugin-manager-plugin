use super::*;

fn sub(owner: &str, repo: &str) -> Subscription {
    Subscription {
        owner: owner.into(),
        repo: repo.into(),
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
fn find_subscription_no_match() {
    let config = Config::default();
    let candidates = vec![("SpiralP".into(), "foo".into())];
    assert_eq!(find_subscription_index(&config, &candidates), None);
}
