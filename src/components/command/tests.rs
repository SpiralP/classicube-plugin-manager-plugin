use std::collections::BTreeMap;

use super::*;
use crate::config::{SELF_OWNER, SELF_REPO};

fn empty_sub() -> Subscription {
    Subscription::default()
}

fn config_with(entries: &[(&str, &str, Subscription)]) -> Config {
    let mut subscriptions: BTreeMap<String, BTreeMap<String, Subscription>> = BTreeMap::new();
    for (owner, repo, sub) in entries {
        subscriptions
            .entry((*owner).into())
            .or_default()
            .insert((*repo).into(), sub.clone());
    }
    Config { subscriptions }
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
    // Both forms are subscribed; the literal candidate (index 0) wins.
    let config = config_with(&[
        ("SpiralP", "classicube-foo-plugin", empty_sub()),
        ("SpiralP", "foo", empty_sub()),
    ]);
    let candidates = vec![
        ("SpiralP".into(), "foo".into()),
        ("SpiralP".into(), "classicube-foo-plugin".into()),
    ];
    let (owner, repo, _) = find_subscription(&config, &candidates).unwrap();
    assert_eq!(owner, "SpiralP");
    assert_eq!(repo, "foo");
}

#[test]
fn find_subscription_falls_back_to_expanded() {
    let config = config_with(&[("SpiralP", "classicube-foo-plugin", empty_sub())]);
    let candidates = vec![
        ("SpiralP".into(), "foo".into()),
        ("SpiralP".into(), "classicube-foo-plugin".into()),
    ];
    let (owner, repo, _) = find_subscription(&config, &candidates).unwrap();
    assert_eq!(owner, "SpiralP");
    assert_eq!(repo, "classicube-foo-plugin");
}

#[test]
fn find_subscription_case_insensitive_returns_stored_keys() {
    // Lookup is case-insensitive but the returned keys preserve the user's
    // original casing — handlers use them for chat messages and as
    // map-removal keys.
    let config = config_with(&[("SpiralP", "classicube-foo-plugin", empty_sub())]);
    let candidates = vec![("spiralp".into(), "CLASSICUBE-FOO-PLUGIN".into())];
    let (owner, repo, _) = find_subscription(&config, &candidates).unwrap();
    assert_eq!(owner, "SpiralP");
    assert_eq!(repo, "classicube-foo-plugin");
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
        state: SubscriptionState {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(100),
            cached_published_at: Some(50),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    apply_channel_switch(&mut s, Channel::Prerelease);
    assert_eq!(s.channel, Channel::Prerelease);
    assert!(s.state.cached_tag.is_none());
    assert!(s.state.cached_at.is_none());
    assert!(s.state.cached_published_at.is_none());
}

#[test]
fn apply_channel_switch_preserves_installed_state() {
    // installed_* describes what's on disk, which doesn't change just because
    // the user pointed the subscription at a different channel.
    let mut s = Subscription {
        channel: Channel::Stable,
        state: SubscriptionState {
            installed_version: Some("v1.0.0".into()),
            installed_asset: Some("a.so".into()),
            installed_at: Some(500),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    apply_channel_switch(&mut s, Channel::Tag("v2.0.0".into()));
    assert_eq!(s.state.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(s.state.installed_asset.as_deref(), Some("a.so"));
    assert_eq!(s.state.installed_at, Some(500));
}

#[test]
fn channel_matches_installed_tag_equal_skips() {
    assert!(channel_matches_installed(
        &Channel::Tag("v1.0.0".into()),
        Some("v1.0.0"),
    ));
}

#[test]
fn channel_matches_installed_tag_different_runs() {
    assert!(!channel_matches_installed(
        &Channel::Tag("v2.0.0".into()),
        Some("v1.0.0"),
    ));
}

#[test]
fn channel_matches_installed_tag_no_installed_runs() {
    assert!(!channel_matches_installed(
        &Channel::Tag("v1.0.0".into()),
        None,
    ));
}

#[test]
fn channel_matches_installed_stable_never_matches() {
    // Non-tag channels never short-circuit at the call site; the same-tag
    // skip in run_update_with_release handles the case where the resolved
    // release happens to equal installed_version.
    assert!(!channel_matches_installed(&Channel::Stable, Some("v1.0.0")));
    assert!(!channel_matches_installed(&Channel::Stable, None));
}

#[test]
fn channel_matches_installed_prerelease_never_matches() {
    assert!(!channel_matches_installed(
        &Channel::Prerelease,
        Some("v1.0.0"),
    ));
    assert!(!channel_matches_installed(&Channel::Prerelease, None));
}

#[test]
fn apply_add_update_no_changes_when_same_channel_and_no_token() {
    let mut s = Subscription {
        channel: Channel::Stable,
        ..empty_sub()
    };
    let before = s.clone();
    assert_eq!(
        apply_add_update(&mut s, Channel::Stable, None),
        AddUpdateDecision::NoChanges,
    );
    assert_eq!(s, before);
}

#[test]
fn apply_add_update_token_added_on_tokenless_sub() {
    let mut s = Subscription {
        channel: Channel::Stable,
        state: SubscriptionState {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(100),
            cached_published_at: Some(50),
            installed_version: Some("v1.0.0".into()),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    assert_eq!(
        apply_add_update(&mut s, Channel::Stable, Some("ghp_abc".into())),
        AddUpdateDecision::Modified {
            channel_changed: false,
            token_changed: true,
        },
    );
    assert_eq!(s.token.as_ref().map(|t| t.expose()), Some("ghp_abc"));
    // Channel unchanged, so cache stays put.
    assert_eq!(s.state.cached_tag.as_deref(), Some("v1.0.0"));
    assert_eq!(s.state.cached_at, Some(100));
    assert_eq!(s.state.installed_version.as_deref(), Some("v1.0.0"));
}

#[test]
fn apply_add_update_token_replaced_when_different() {
    let mut s = Subscription {
        channel: Channel::Stable,
        token: Some(Secret::new("ghp_old".into())),
        ..empty_sub()
    };
    assert_eq!(
        apply_add_update(&mut s, Channel::Stable, Some("ghp_new".into())),
        AddUpdateDecision::Modified {
            channel_changed: false,
            token_changed: true,
        },
    );
    assert_eq!(s.token.as_ref().map(|t| t.expose()), Some("ghp_new"));
}

#[test]
fn apply_add_update_matching_token_is_no_op() {
    let mut s = Subscription {
        channel: Channel::Stable,
        token: Some(Secret::new("ghp_abc".into())),
        ..empty_sub()
    };
    assert_eq!(
        apply_add_update(&mut s, Channel::Stable, Some("ghp_abc".into())),
        AddUpdateDecision::NoChanges,
    );
    assert_eq!(s.token.as_ref().map(|t| t.expose()), Some("ghp_abc"));
}

#[test]
fn apply_add_update_no_token_arg_preserves_existing_token() {
    // Re-running `/add foo/bar prerelease` on a tokened sub must not strip
    // the token. Removing a token still requires hand-editing the TOML.
    let mut s = Subscription {
        channel: Channel::Stable,
        token: Some(Secret::new("ghp_keep".into())),
        ..empty_sub()
    };
    assert_eq!(
        apply_add_update(&mut s, Channel::Prerelease, None),
        AddUpdateDecision::Modified {
            channel_changed: true,
            token_changed: false,
        },
    );
    assert_eq!(s.token.as_ref().map(|t| t.expose()), Some("ghp_keep"));
}

#[test]
fn apply_add_update_channel_change_clears_cache_preserves_install() {
    let mut s = Subscription {
        channel: Channel::Stable,
        state: SubscriptionState {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(100),
            cached_published_at: Some(50),
            installed_version: Some("v1.0.0".into()),
            installed_asset: Some("a.so".into()),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    assert_eq!(
        apply_add_update(&mut s, Channel::Prerelease, None),
        AddUpdateDecision::Modified {
            channel_changed: true,
            token_changed: false,
        },
    );
    assert_eq!(s.channel, Channel::Prerelease);
    assert!(s.state.cached_tag.is_none());
    assert!(s.state.cached_at.is_none());
    assert!(s.state.cached_published_at.is_none());
    // installed_* describes what's on disk; channel switch must not touch it.
    assert_eq!(s.state.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(s.state.installed_asset.as_deref(), Some("a.so"));
}

#[test]
fn apply_add_update_channel_and_token_change_together() {
    let mut s = Subscription {
        channel: Channel::Stable,
        token: Some(Secret::new("ghp_old".into())),
        state: SubscriptionState {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(100),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    assert_eq!(
        apply_add_update(&mut s, Channel::Prerelease, Some("ghp_new".into())),
        AddUpdateDecision::Modified {
            channel_changed: true,
            token_changed: true,
        },
    );
    assert_eq!(s.channel, Channel::Prerelease);
    assert_eq!(s.token.as_ref().map(|t| t.expose()), Some("ghp_new"));
    assert!(s.state.cached_tag.is_none());
    assert!(s.state.cached_at.is_none());
}

#[test]
fn parse_channel_args_rejects_extra_args() {
    assert!(parse_channel_args(&["stable", "extra"]).is_err());
    assert!(parse_channel_args(&["tag", "v1", "extra"]).is_err());
}

#[test]
fn parse_add_args_empty_is_stable_no_token() {
    assert_eq!(parse_add_args(&[]), Ok((Channel::Stable, None)));
}

#[test]
fn parse_add_args_channel_only() {
    assert_eq!(
        parse_add_args(&["prerelease"]),
        Ok((Channel::Prerelease, None))
    );
    assert_eq!(
        parse_add_args(&["tag", "v1.2.3"]),
        Ok((Channel::Tag("v1.2.3".into()), None))
    );
}

#[test]
fn parse_add_args_token_only() {
    assert_eq!(
        parse_add_args(&["token", "ghp_abc123"]),
        Ok((Channel::Stable, Some("ghp_abc123".into())))
    );
}

#[test]
fn parse_add_args_channel_and_token() {
    assert_eq!(
        parse_add_args(&["prerelease", "token", "ghp_abc123"]),
        Ok((Channel::Prerelease, Some("ghp_abc123".into())))
    );
    assert_eq!(
        parse_add_args(&["tag", "v1.2.3", "token", "ghp_abc123"]),
        Ok((Channel::Tag("v1.2.3".into()), Some("ghp_abc123".into())))
    );
}

#[test]
fn parse_add_args_rejects_bare_token() {
    assert!(parse_add_args(&["token"]).is_err());
    assert!(parse_add_args(&["prerelease", "token"]).is_err());
}

#[test]
fn parse_add_args_rejects_empty_token_value() {
    assert!(parse_add_args(&["token", ""]).is_err());
}

#[test]
fn parse_add_args_rejects_token_before_channel() {
    // Token must be the trailing clause; embedding it earlier leaves
    // `parse_channel_args` with a bogus arg list and it bails.
    assert!(parse_add_args(&["token", "ghp_abc", "prerelease"]).is_err());
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
    assert!(find_subscription(&config, &candidates).is_none());
}

#[test]
fn expand_candidates_curated_shorthand_wins() {
    // A bare curated shorthand returns the single canonical pair; the
    // generic two-candidate `classicube-$name-plugin` expansion is skipped.
    assert_eq!(
        expand_candidates("cef"),
        Some(vec![(
            "SpiralP".into(),
            "classicube-cef-loader-plugin".into(),
        )])
    );
}

#[test]
fn expand_candidates_curated_shorthand_case_insensitive() {
    assert_eq!(
        expand_candidates("CEF"),
        Some(vec![(
            "SpiralP".into(),
            "classicube-cef-loader-plugin".into(),
        )])
    );
}

#[test]
fn expand_candidates_owner_prefixed_skips_curated() {
    // Owner-prefixed input always means "I know what I want" — the curated
    // lookup is skipped even when the bare name would have matched.
    assert_eq!(
        expand_candidates("octocat/cef"),
        Some(vec![
            ("octocat".into(), "cef".into()),
            ("octocat".into(), "classicube-cef-plugin".into()),
        ])
    );
}

#[test]
fn expand_candidates_uncurated_bare_still_expands() {
    // A bare name with no curated entry still falls through to the generic
    // two-candidate expansion.
    assert_eq!(
        expand_candidates("zzz-not-curated"),
        Some(vec![
            ("SpiralP".into(), "zzz-not-curated".into()),
            ("SpiralP".into(), "classicube-zzz-not-curated-plugin".into()),
        ])
    );
}

#[test]
fn pause_target_pins_to_installed_version_from_stable() {
    let sub = Subscription {
        channel: Channel::Stable,
        state: SubscriptionState {
            installed_version: Some("v1.2.3".into()),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    assert_eq!(pause_target(&sub), Ok(Channel::Tag("v1.2.3".into())));
}

#[test]
fn pause_target_pins_to_installed_version_from_prerelease() {
    let sub = Subscription {
        channel: Channel::Prerelease,
        state: SubscriptionState {
            installed_version: Some("v1.2.3-rc1".into()),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    assert_eq!(pause_target(&sub), Ok(Channel::Tag("v1.2.3-rc1".into())));
}

#[test]
fn pause_target_refuses_when_nothing_installed() {
    let sub = Subscription {
        channel: Channel::Stable,
        state: SubscriptionState::default(),
        ..empty_sub()
    };
    let err = pause_target(&sub).unwrap_err();
    assert!(err.contains("no installed version"), "got: {err}");
}

#[test]
fn refuse_self_mutation_blocks_self_owner_repo() {
    let msg = refuse_self_mutation(SELF_OWNER, SELF_REPO, "remove")
        .expect("self target should be refused");
    assert!(msg.contains("manager plugin"), "got: {msg}");
    assert!(msg.contains("remove"), "got: {msg}");
    assert!(msg.contains(SELF_OWNER), "got: {msg}");
    assert!(msg.contains(SELF_REPO), "got: {msg}");
}

#[test]
fn refuse_self_mutation_allows_other_repo() {
    assert!(refuse_self_mutation("octocat", "classicube-foo-plugin", "remove").is_none());
}

#[test]
fn refuse_self_mutation_is_case_sensitive() {
    // Handlers feed in the *stored* keys returned by find_subscription_mut,
    // so user-typed casing is normalized away before this check sees it.
    // Only TOML-on-disk casing reaches us, and config::is_self is exact-match.
    assert!(refuse_self_mutation("spiralp", SELF_REPO, "remove").is_none());
}

#[test]
fn refuse_self_mutation_message_is_ascii() {
    let msg = refuse_self_mutation(SELF_OWNER, SELF_REPO, "remove").unwrap();
    assert!(msg.is_ascii(), "chat output must be ASCII: {msg:?}");
}

#[test]
fn pause_target_refuses_when_already_pinned() {
    let sub = Subscription {
        channel: Channel::Tag("v1.0.0".into()),
        state: SubscriptionState {
            installed_version: Some("v1.0.0".into()),
            ..SubscriptionState::default()
        },
        ..empty_sub()
    };
    let err = pause_target(&sub).unwrap_err();
    assert!(err.contains("already paused"), "got: {err}");
    assert!(err.contains("v1.0.0"), "got: {err}");
}
