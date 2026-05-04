use std::{collections::BTreeMap, io::Write};

use tempfile::{NamedTempFile, tempdir};

use super::*;

fn sub() -> Subscription {
    Subscription::default()
}

fn one_sub_config(owner: &str, repo: &str, sub: Subscription) -> Config {
    let mut subscriptions = BTreeMap::new();
    let mut repos = BTreeMap::new();
    repos.insert(repo.into(), sub);
    subscriptions.insert(owner.into(), repos);
    Config { subscriptions }
}

fn first_sub(cfg: &Config) -> (&str, &str, &Subscription) {
    let (owner, repos) = cfg.subscriptions.iter().next().unwrap();
    let (repo, sub) = repos.iter().next().unwrap();
    (owner, repo, sub)
}

#[test]
fn fresh_within_ttl() {
    let s = Subscription {
        state: SubscriptionState {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(100),
            cached_published_at: Some(50),
            ..SubscriptionState::default()
        },
        ..sub()
    };
    assert_eq!(s.fresh_cached_release(150, 100), Some(("v1.0.0", 50)));
}

#[test]
fn fresh_at_ttl_boundary_is_stale() {
    // The check is strict `<`, so equal-to-TTL counts as expired.
    let s = Subscription {
        state: SubscriptionState {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(100),
            cached_published_at: Some(50),
            ..SubscriptionState::default()
        },
        ..sub()
    };
    assert_eq!(s.fresh_cached_release(200, 100), None);
}

#[test]
fn fresh_with_clock_skew() {
    // saturating_sub avoids panicking when `now < cached_at`; treat as fresh.
    let s = Subscription {
        state: SubscriptionState {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(500),
            cached_published_at: Some(50),
            ..SubscriptionState::default()
        },
        ..sub()
    };
    assert_eq!(s.fresh_cached_release(100, 60), Some(("v1.0.0", 50)));
}

#[test]
fn missing_cached_tag_is_stale() {
    let s = Subscription {
        state: SubscriptionState {
            cached_at: Some(100),
            cached_published_at: Some(50),
            ..SubscriptionState::default()
        },
        ..sub()
    };
    assert_eq!(s.fresh_cached_release(150, 100), None);
}

#[test]
fn missing_cached_at_is_stale() {
    let s = Subscription {
        state: SubscriptionState {
            cached_tag: Some("v1.0.0".into()),
            cached_published_at: Some(50),
            ..SubscriptionState::default()
        },
        ..sub()
    };
    assert_eq!(s.fresh_cached_release(150, 100), None);
}

#[test]
fn missing_cached_published_at_is_stale() {
    // Without the timestamp the cached tag is useless for the install
    // decision, so treat it as stale and force a refetch.
    let s = Subscription {
        state: SubscriptionState {
            cached_tag: Some("v1.0.0".into()),
            cached_at: Some(100),
            ..SubscriptionState::default()
        },
        ..sub()
    };
    assert_eq!(s.fresh_cached_release(150, 100), None);
}

#[test]
fn load_missing_file_yields_default() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("does-not-exist.toml");
    let cfg = Config::load_from(&path).unwrap();
    assert_eq!(cfg, Config::default());
}

#[test]
fn load_malformed_file_errors() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"this is not = valid ::: toml [[[").unwrap();
    let err = Config::load_from(f.path()).unwrap_err();
    let chain = format!("{err:#}");
    assert!(chain.contains("parsing"), "expected 'parsing' in: {chain}");
}

#[test]
fn round_trip_default_config() {
    let f = NamedTempFile::new().unwrap();
    Config::default().save_to(f.path()).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, Config::default());
}

#[test]
fn round_trip_populated_subscription() {
    let cfg = one_sub_config(
        "octocat",
        "hello-world",
        Subscription {
            state: SubscriptionState {
                installed_version: Some("v1.2.3".into()),
                installed_asset: Some("hello-world.so".into()),
                installed_at: Some(1_700_000_000),
                cached_tag: Some("v1.2.4".into()),
                cached_at: Some(1_700_000_500),
                cached_published_at: Some(1_700_000_400),
                in_callback: None,
            },
            ..sub()
        },
    );
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn dotted_key_round_trip() {
    // Two repos under the same owner share an outer table; the file uses
    // `[owner.repo]` headers (no `[[subscriptions]]` wrapper).
    let mut repos = BTreeMap::new();
    repos.insert("classicube-foo-plugin".into(), sub());
    repos.insert(
        "classicube-bar-plugin".into(),
        Subscription {
            channel: Channel::Prerelease,
            ..sub()
        },
    );
    let mut subscriptions = BTreeMap::new();
    subscriptions.insert("SpiralP".into(), repos);
    let cfg = Config { subscriptions };

    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        on_disk.contains("[SpiralP.classicube-foo-plugin]"),
        "expected dotted-key header in: {on_disk}",
    );
    assert!(
        on_disk.contains("[SpiralP.classicube-bar-plugin]"),
        "expected dotted-key header in: {on_disk}",
    );
    assert!(
        !on_disk.contains("[[subscriptions]]"),
        "should not emit array-of-tables: {on_disk}",
    );
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn bare_subscription_skips_optional_fields_in_toml() {
    let cfg = one_sub_config("octocat", "hello-world", sub());
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(!on_disk.contains("disabled"));
    assert!(!on_disk.contains("installed_version"));
    assert!(!on_disk.contains("installed_asset"));
    assert!(!on_disk.contains("installed_at"));
    assert!(!on_disk.contains("cached_tag"));
    assert!(!on_disk.contains("cached_at"));
    assert!(!on_disk.contains("cached_published_at"));
    // The empty-state subtable is omitted entirely so a freshly-subscribed
    // entry doesn't carry an empty `[octocat.hello-world.state]` header.
    assert!(
        !on_disk.contains(".state]"),
        "empty state subtable should be skipped: {on_disk}",
    );
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn state_subtable_round_trip() {
    let cfg = one_sub_config(
        "octocat",
        "hello-world",
        Subscription {
            state: SubscriptionState {
                installed_version: Some("v1.0.0".into()),
                ..SubscriptionState::default()
            },
            ..sub()
        },
    );
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        on_disk.contains("[octocat.hello-world.state]"),
        "expected state subtable header in: {on_disk}",
    );
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn disabled_round_trip() {
    let cfg = one_sub_config(
        "octocat",
        "hello-world",
        Subscription {
            disabled: true,
            ..sub()
        },
    );
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        on_disk.contains("disabled = true"),
        "expected `disabled = true` in: {on_disk}",
    );
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn channel_default_is_stable() {
    assert_eq!(Channel::default(), Channel::Stable);
    assert!(Channel::Stable.is_default());
    assert!(!Channel::Prerelease.is_default());
    assert!(!Channel::Tag("v1".into()).is_default());
}

#[test]
fn channel_parses_known_strings() {
    assert_eq!("stable".parse::<Channel>(), Ok(Channel::Stable));
    assert_eq!("prerelease".parse::<Channel>(), Ok(Channel::Prerelease));
    assert_eq!(
        "tag:v1.2.3".parse::<Channel>(),
        Ok(Channel::Tag("v1.2.3".into()))
    );
}

#[test]
fn channel_rejects_empty_or_whitespace_tag() {
    assert!("tag:".parse::<Channel>().is_err());
    assert!("tag:   ".parse::<Channel>().is_err());
    assert!("tag:foo bar".parse::<Channel>().is_err());
}

#[test]
fn channel_from_tag_trims_and_validates() {
    assert_eq!(
        Channel::from_tag("v1.2.3"),
        Ok(Channel::Tag("v1.2.3".into())),
    );
    // Surrounding whitespace is trimmed; this matters for hand-edited tomls.
    assert_eq!(
        Channel::from_tag("  v1.2.3  "),
        Ok(Channel::Tag("v1.2.3".into())),
    );
    assert!(Channel::from_tag("").is_err());
    assert!(Channel::from_tag("   ").is_err());
    assert!(Channel::from_tag("v 1").is_err());
}

#[test]
fn channel_pretty_per_variant() {
    assert_eq!(Channel::Stable.pretty(), "stable");
    assert_eq!(Channel::Prerelease.pretty(), "prerelease");
    assert_eq!(Channel::Tag("v1.2.3".into()).pretty(), "tag: v1.2.3");
}

#[test]
fn channel_rejects_unknown() {
    assert!("nightly".parse::<Channel>().is_err());
    assert!("".parse::<Channel>().is_err());
}

#[test]
fn channel_round_trip_in_subscription() {
    let mut repos = BTreeMap::new();
    repos.insert(
        "stable".into(),
        Subscription {
            channel: Channel::Stable,
            ..sub()
        },
    );
    repos.insert(
        "pre".into(),
        Subscription {
            channel: Channel::Prerelease,
            ..sub()
        },
    );
    repos.insert(
        "pinned".into(),
        Subscription {
            channel: Channel::Tag("v1.2.3".into()),
            ..sub()
        },
    );
    let mut subscriptions = BTreeMap::new();
    subscriptions.insert("a".into(), repos);
    let cfg = Config { subscriptions };

    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    // Default (stable) is skipped, others render as the agreed string form.
    assert!(
        !on_disk.contains("channel = \"stable\""),
        "stable should be skipped: {on_disk}",
    );
    assert!(on_disk.contains("channel = \"prerelease\""));
    assert!(on_disk.contains("channel = \"tag:v1.2.3\""));
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn channel_round_trips_complex_tag() {
    // Real-world tags include rc / build-metadata characters; make sure the
    // `tag:<ref>` form round-trips them as-is rather than mangling them.
    let cfg = one_sub_config(
        "a",
        "b",
        Subscription {
            channel: Channel::Tag("v1.2.3-rc1+build.5".into()),
            ..sub()
        },
    );
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        on_disk.contains("channel = \"tag:v1.2.3-rc1+build.5\""),
        "expected exact tag form in: {on_disk}",
    );
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn is_self_matches_pkg_identity() {
    assert!(super::is_self(SELF_OWNER, SELF_REPO));
}

#[test]
fn is_self_rejects_other_owners_and_repos() {
    assert!(!super::is_self("octocat", SELF_REPO));
    assert!(!super::is_self(SELF_OWNER, "some-other-plugin"));
}

#[test]
fn is_self_is_case_sensitive() {
    // The chat-command path normalizes case before storage; once stored,
    // is_self compares verbatim against the pkg-name constant.
    assert!(!super::is_self(&SELF_OWNER.to_lowercase(), SELF_REPO));
    assert!(!super::is_self(SELF_OWNER, &SELF_REPO.to_uppercase()));
}

#[test]
fn ensure_self_adds_when_missing() {
    let mut cfg = Config::default();
    assert!(cfg.ensure_self());
    let added = cfg
        .subscriptions
        .get(SELF_OWNER)
        .and_then(|m| m.get(SELF_REPO))
        .expect("self subscription should exist after ensure_self");
    assert_eq!(added.channel, Channel::Stable);
    assert!(!added.disabled);
    assert!(added.state.is_empty());
}

#[test]
fn ensure_self_is_noop_when_present() {
    let prepared = Subscription {
        channel: Channel::Prerelease,
        disabled: true,
        token: None,
        state: SubscriptionState {
            installed_version: Some("v9.9.9".into()),
            ..SubscriptionState::default()
        },
    };
    let mut cfg = one_sub_config(SELF_OWNER, SELF_REPO, prepared.clone());
    assert!(!cfg.ensure_self());
    let kept = cfg
        .subscriptions
        .get(SELF_OWNER)
        .and_then(|m| m.get(SELF_REPO))
        .unwrap();
    assert_eq!(kept, &prepared);
}

#[test]
fn ensure_self_does_not_disturb_other_subscriptions() {
    let other = Subscription {
        state: SubscriptionState {
            installed_version: Some("v1.0.0".into()),
            installed_asset: Some("other.so".into()),
            ..SubscriptionState::default()
        },
        ..sub()
    };
    let mut cfg = one_sub_config("octocat", "hello-world", other.clone());
    assert!(cfg.ensure_self());
    // Both entries are present; BTreeMap order is alphabetical.
    assert_eq!(
        cfg.subscriptions
            .get("octocat")
            .and_then(|m| m.get("hello-world")),
        Some(&other),
    );
    assert!(
        cfg.subscriptions
            .get(SELF_OWNER)
            .and_then(|m| m.get(SELF_REPO))
            .is_some()
    );
}

#[test]
fn keys_round_trip_in_alphabetical_order() {
    let mut subscriptions = BTreeMap::new();
    for owner in ["c", "a", "b"] {
        let mut repos = BTreeMap::new();
        repos.insert("only".into(), sub());
        subscriptions.insert(owner.into(), repos);
    }
    let cfg = Config { subscriptions };
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    let owners: Vec<&String> = loaded.subscriptions.keys().collect();
    assert_eq!(
        owners,
        vec![&"a".to_string(), &"b".to_string(), &"c".to_string()]
    );
}

#[test]
fn rejects_repo_with_dot_on_load() {
    // `[a."b.c"]` parses fine in TOML (quoted key keeps the dot intact in
    // the inner key name), so we have to reject it explicitly during
    // validation — without that, the loader would accept a repo name TOML
    // would later silently re-nest if the user removed the quotes.
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"[a.\"b.c\"]\n").unwrap();
    let err = Config::load_from(f.path()).unwrap_err();
    let chain = format!("{err:#}");
    assert!(
        chain.contains("'.'") || chain.contains("nested table"),
        "expected dot-rejection message in: {chain}",
    );
}

#[test]
fn rejects_empty_owner_segment() {
    // Quoted empty TOML key — invalid for our schema even if TOML allows it.
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"[\"\".\"some-repo\"]\n").unwrap();
    let err = Config::load_from(f.path()).unwrap_err();
    let chain = format!("{err:#}");
    assert!(
        chain.contains("empty"),
        "expected empty-segment error in: {chain}",
    );
}

#[test]
fn rejects_whitespace_in_keys() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"[\"some owner\".\"some-repo\"]\n").unwrap();
    let err = Config::load_from(f.path()).unwrap_err();
    let chain = format!("{err:#}");
    assert!(
        chain.contains("whitespace"),
        "expected whitespace error in: {chain}",
    );
}

#[test]
fn first_sub_helper_smoke() {
    // Sanity check that the test helper still surfaces the single entry.
    let cfg = one_sub_config("a", "b", sub());
    let (owner, repo, _) = first_sub(&cfg);
    assert_eq!(owner, "a");
    assert_eq!(repo, "b");
}

#[test]
fn token_round_trip() {
    let cfg = one_sub_config(
        "someorg",
        "secret-plugin",
        Subscription {
            token: Some(Secret::new("github_pat_xyz123".into())),
            ..sub()
        },
    );
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        on_disk.contains("token = \"github_pat_xyz123\""),
        "expected token line in: {on_disk}",
    );
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn token_skipped_when_absent() {
    // A subscription with no token must not emit a `token = ...` line, so
    // freshly-subscribed entries don't carry a stub field that looks like an
    // intentional empty-string token.
    let cfg = one_sub_config("octocat", "hello-world", sub());
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        !on_disk.contains("token"),
        "absent token should not render: {on_disk}",
    );
}

#[test]
fn in_callback_round_trip() {
    let cfg = one_sub_config(
        "octocat",
        "hello-world",
        Subscription {
            state: SubscriptionState {
                in_callback: Some("OnNewMap".into()),
                ..SubscriptionState::default()
            },
            ..sub()
        },
    );
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        on_disk.contains("in_callback = \"OnNewMap\""),
        "expected breadcrumb line in: {on_disk}",
    );
    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn in_callback_alone_keeps_state_subtable() {
    // The breadcrumb has to render even when nothing else is in `state` —
    // otherwise the field is lost across save/reload and the carry-over
    // check on next startup never fires.
    let s = SubscriptionState {
        in_callback: Some("Init".into()),
        ..SubscriptionState::default()
    };
    assert!(!s.is_empty());

    let cfg = one_sub_config("octocat", "hello-world", Subscription { state: s, ..sub() });
    let f = NamedTempFile::new().unwrap();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        on_disk.contains("[octocat.hello-world.state]"),
        "expected state subtable in: {on_disk}",
    );
}

#[test]
fn set_in_callback_to_persists() {
    let f = NamedTempFile::new().unwrap();
    one_sub_config("octocat", "hello-world", sub())
        .save_to(f.path())
        .unwrap();

    super::set_in_callback_to(f.path(), "octocat", "hello-world", Some("Init".into())).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    let (_, _, s) = first_sub(&loaded);
    assert_eq!(s.state.in_callback.as_deref(), Some("Init"));

    super::set_in_callback_to(f.path(), "octocat", "hello-world", None).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    let (_, _, s) = first_sub(&loaded);
    assert!(s.state.in_callback.is_none());
}

#[test]
fn set_in_callback_to_preserves_other_fields() {
    // The breadcrumb writer must not stomp on existing install/cache state —
    // a crash during a callback shouldn't lose the installed_version or
    // cached_at that other code paths wrote earlier.
    let prepared = Subscription {
        channel: Channel::Prerelease,
        disabled: false,
        token: Some(Secret::new("github_pat_xyz".into())),
        state: SubscriptionState {
            installed_version: Some("v1.2.3".into()),
            installed_asset: Some("plugin.so".into()),
            installed_at: Some(1_700_000_000),
            cached_tag: Some("v1.2.4".into()),
            cached_at: Some(1_700_000_500),
            cached_published_at: Some(1_700_000_400),
            in_callback: None,
        },
    };
    let f = NamedTempFile::new().unwrap();
    one_sub_config("octocat", "hello-world", prepared.clone())
        .save_to(f.path())
        .unwrap();

    super::set_in_callback_to(f.path(), "octocat", "hello-world", Some("OnNewMap".into())).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    let (_, _, s) = first_sub(&loaded);
    assert_eq!(s.channel, prepared.channel);
    assert_eq!(s.token, prepared.token);
    assert_eq!(s.state.installed_version, prepared.state.installed_version);
    assert_eq!(s.state.installed_asset, prepared.state.installed_asset);
    assert_eq!(s.state.installed_at, prepared.state.installed_at);
    assert_eq!(s.state.cached_tag, prepared.state.cached_tag);
    assert_eq!(s.state.cached_at, prepared.state.cached_at);
    assert_eq!(
        s.state.cached_published_at,
        prepared.state.cached_published_at
    );
    assert_eq!(s.state.in_callback.as_deref(), Some("OnNewMap"));
}

#[test]
fn set_in_callback_to_unknown_sub_is_noop() {
    // The (owner, repo) might not exist if the user `/unsubscribe`d between
    // breadcrumb-set and breadcrumb-clear; the writer must not invent a
    // subscription out of thin air just to record a breadcrumb.
    let f = NamedTempFile::new().unwrap();
    one_sub_config("octocat", "hello-world", sub())
        .save_to(f.path())
        .unwrap();

    super::set_in_callback_to(f.path(), "ghost", "missing", Some("Init".into())).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    assert!(!loaded.subscriptions.contains_key("ghost"));
    assert!(loaded.subscriptions.contains_key("octocat"));
}

#[test]
fn debug_subscription_redacts_token() {
    // Subscription derives Debug; the token field's `Secret` newtype must
    // redact rather than leak the literal PAT into a `tracing` emit or a
    // panic message somewhere in the call graph.
    let s = Subscription {
        token: Some(Secret::new("github_pat_supersecret".into())),
        ..sub()
    };
    let dbg = format!("{s:?}");
    assert!(
        !dbg.contains("supersecret"),
        "token leaked into Debug output: {dbg}",
    );
    assert!(
        dbg.contains("<redacted>"),
        "expected redaction marker in: {dbg}",
    );
}
