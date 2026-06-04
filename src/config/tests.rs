use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use tempfile::{TempDir, tempdir};

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

/// Per-test scratch directory so the sidecar at `state_path_for(path)` is
/// unique per test. Without this, parallel tests race on the same
/// `/tmp/managed/state.toml` because `NamedTempFile::new()` places the file
/// in the shared `/tmp` and the derived sidecar parent collapses to a single
/// shared location.
struct TempConfig {
    _dir: TempDir,
    path: PathBuf,
}

impl TempConfig {
    fn new() -> Self {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugin-manager.toml");
        Self { _dir: dir, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn state_path(&self) -> PathBuf {
        state_path_for(&self.path)
    }
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
    let f = TempConfig::new();
    fs::write(f.path(), b"this is not = valid ::: toml [[[").unwrap();
    let err = Config::load_from(f.path()).unwrap_err();
    let chain = format!("{err:#}");
    assert!(chain.contains("parsing"), "expected 'parsing' in: {chain}");
}

#[test]
fn round_trip_default_config() {
    let f = TempConfig::new();
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
            },
            ..sub()
        },
    );
    let f = TempConfig::new();
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

    let f = TempConfig::new();
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
    let f = TempConfig::new();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(!on_disk.contains("disabled"));
    // State fields belong in the sidecar, never in the user file.
    assert!(!on_disk.contains("installed_version"));
    assert!(!on_disk.contains("installed_asset"));
    assert!(!on_disk.contains("installed_at"));
    assert!(!on_disk.contains("cached_tag"));
    assert!(!on_disk.contains("cached_at"));
    assert!(!on_disk.contains("cached_published_at"));
    // No `[owner.repo.state]` subtables in the user file under the new
    // layout - state moved to the sidecar.
    assert!(
        !on_disk.contains(".state]"),
        "user file must not carry state subtables: {on_disk}",
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
    let f = TempConfig::new();
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

    let f = TempConfig::new();
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
    let f = TempConfig::new();
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
fn is_self_disabled_false_when_entry_missing() {
    // Pre-`ensure_self` startup window: no self entry yet -> not disabled.
    let cfg = Config::default();
    assert!(!super::is_self_disabled(&cfg));
}

#[test]
fn is_self_disabled_false_when_self_enabled() {
    let cfg = one_sub_config(
        SELF_OWNER,
        SELF_REPO,
        Subscription {
            disabled: false,
            ..sub()
        },
    );
    assert!(!super::is_self_disabled(&cfg));
}

#[test]
fn is_self_disabled_true_when_self_disabled() {
    let cfg = one_sub_config(
        SELF_OWNER,
        SELF_REPO,
        Subscription {
            disabled: true,
            ..sub()
        },
    );
    assert!(super::is_self_disabled(&cfg));
}

#[test]
fn is_self_disabled_ignores_other_disabled_subs() {
    // A disabled OTHER subscription must not look like self being disabled.
    let cfg = one_sub_config(
        "octocat",
        "classicube-foo-plugin",
        Subscription {
            disabled: true,
            ..sub()
        },
    );
    assert!(!super::is_self_disabled(&cfg));
}

#[test]
fn ensure_self_adds_when_missing() {
    let mut cfg = Config::default();
    assert!(cfg.ensure_self("v1.0.0", Some("manager.so")));
    let added = cfg
        .subscriptions
        .get(SELF_OWNER)
        .and_then(|m| m.get(SELF_REPO))
        .expect("self subscription should exist after ensure_self");
    assert_eq!(added.channel, Channel::Stable);
    assert!(!added.disabled);
    // Fresh entry: install state stamped to the running binary, installed_at
    // left unset so needs_install still treats it as install-needed.
    assert_eq!(added.state.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(added.state.installed_asset.as_deref(), Some("manager.so"));
    assert!(added.state.installed_at.is_none());
}

#[test]
fn ensure_self_keeps_user_fields_but_restamps_state_when_present() {
    let prepared = Subscription {
        channel: Channel::Prerelease,
        disabled: true,
        token: None,
        state: SubscriptionState {
            installed_version: Some("v9.9.9".into()),
            installed_asset: Some("old.so".into()),
            installed_at: Some(42),
            ..SubscriptionState::default()
        },
    };
    let mut cfg = one_sub_config(SELF_OWNER, SELF_REPO, prepared);
    // Record already exists, so nothing was "added".
    assert!(!cfg.ensure_self("v1.0.0", Some("new.so")));
    let kept = &cfg.subscriptions[SELF_OWNER][SELF_REPO];
    // User-file fields preserved (a disabled / pinned self is left alone)...
    assert_eq!(kept.channel, Channel::Prerelease);
    assert!(kept.disabled);
    // ...but install state is re-stamped to the running binary.
    assert_eq!(kept.state.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(kept.state.installed_asset.as_deref(), Some("new.so"));
    // installed_at must not be touched - needs_install depends on it.
    assert_eq!(kept.state.installed_at, Some(42));
}

#[test]
fn ensure_self_none_asset_leaves_installed_asset_alone() {
    let prepared = Subscription {
        state: SubscriptionState {
            installed_asset: Some("kept.so".into()),
            ..SubscriptionState::default()
        },
        ..sub()
    };
    let mut cfg = one_sub_config(SELF_OWNER, SELF_REPO, prepared);
    assert!(!cfg.ensure_self("v1.0.0", None));
    let kept = &cfg.subscriptions[SELF_OWNER][SELF_REPO].state;
    assert_eq!(kept.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(kept.installed_asset.as_deref(), Some("kept.so"));
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
    assert!(cfg.ensure_self("v1.0.0", Some("manager.so")));
    // Both entries are present; BTreeMap order is alphabetical.
    assert_eq!(
        cfg.subscriptions
            .get("octocat")
            .and_then(|m| m.get("hello-world")),
        Some(&other),
    );
    // Self was added and stamped to the running binary in the same call.
    let self_state = &cfg.subscriptions[SELF_OWNER][SELF_REPO].state;
    assert_eq!(self_state.installed_version.as_deref(), Some("v1.0.0"));
    assert_eq!(self_state.installed_asset.as_deref(), Some("manager.so"));
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
    let f = TempConfig::new();
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
    let f = TempConfig::new();
    fs::write(f.path(), b"[a.\"b.c\"]\n").unwrap();
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
    let f = TempConfig::new();
    fs::write(f.path(), b"[\"\".\"some-repo\"]\n").unwrap();
    let err = Config::load_from(f.path()).unwrap_err();
    let chain = format!("{err:#}");
    assert!(
        chain.contains("empty"),
        "expected empty-segment error in: {chain}",
    );
}

#[test]
fn rejects_whitespace_in_keys() {
    let f = TempConfig::new();
    fs::write(f.path(), b"[\"some owner\".\"some-repo\"]\n").unwrap();
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
    let f = TempConfig::new();
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
    let f = TempConfig::new();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    assert!(
        !on_disk.contains("token"),
        "absent token should not render: {on_disk}",
    );
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

#[test]
fn user_fields_render_alphabetically() {
    // channel < disabled < token. Same ordering check as before, but for the
    // user file only now that state lives in the sidecar.
    let cfg = one_sub_config(
        "owner",
        "repo",
        Subscription {
            channel: Channel::Prerelease,
            disabled: true,
            token: Some(Secret::new("tok".into())),
            state: SubscriptionState::default(),
        },
    );
    let f = TempConfig::new();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    let p_channel = on_disk.find("channel = ").unwrap();
    let p_disabled = on_disk.find("disabled = ").unwrap();
    let p_token = on_disk.find("token = ").unwrap();
    assert!(p_channel < p_disabled, "channel before disabled");
    assert!(p_disabled < p_token, "disabled before token");
}

#[test]
fn user_headers_render_alphabetically() {
    let mut subscriptions = BTreeMap::new();
    for owner in ["b-owner", "a-owner"] {
        let mut repos = BTreeMap::new();
        repos.insert("z-repo".into(), sub());
        repos.insert("m-repo".into(), sub());
        subscriptions.insert(owner.into(), repos);
    }
    let cfg = Config { subscriptions };
    let f = TempConfig::new();
    cfg.save_to(f.path()).unwrap();
    let on_disk = fs::read_to_string(f.path()).unwrap();
    let order = [
        on_disk.find("[a-owner.m-repo]").unwrap(),
        on_disk.find("[a-owner.z-repo]").unwrap(),
        on_disk.find("[b-owner.m-repo]").unwrap(),
        on_disk.find("[b-owner.z-repo]").unwrap(),
    ];
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "user region not A-Z: {on_disk}"
    );
}

// ---- state sidecar ----

#[test]
fn save_emits_state_sidecar_with_hoisted_keys() {
    // Two subs, both with state. The sidecar must contain
    // `[owner.repo]` headers (hoisted, no `.state` segment) carrying the
    // state fields directly. The user file must contain only `[owner.repo]`
    // headers with user-editable fields.
    let mut repos = BTreeMap::new();
    repos.insert(
        "alpha".into(),
        Subscription {
            channel: Channel::Prerelease,
            state: SubscriptionState {
                installed_version: Some("v1".into()),
                ..SubscriptionState::default()
            },
            ..sub()
        },
    );
    repos.insert(
        "beta".into(),
        Subscription {
            disabled: true,
            state: SubscriptionState {
                installed_version: Some("v2".into()),
                ..SubscriptionState::default()
            },
            ..sub()
        },
    );
    let mut subscriptions = BTreeMap::new();
    subscriptions.insert("owner".into(), repos);
    let cfg = Config { subscriptions };

    let f = TempConfig::new();
    cfg.save_to(f.path()).unwrap();

    let user_disk = fs::read_to_string(f.path()).unwrap();
    assert!(user_disk.contains("[owner.alpha]"));
    assert!(user_disk.contains("[owner.beta]"));
    // Hoisted layout: no `.state]` headers in either file.
    assert!(
        !user_disk.contains("state]"),
        "user file must not carry state subtables: {user_disk}",
    );
    assert!(!user_disk.contains("installed_version"));

    let state_disk = fs::read_to_string(f.state_path()).unwrap();
    assert!(
        state_disk.contains("[owner.alpha]"),
        "state file should use hoisted owner.repo headers: {state_disk}",
    );
    assert!(state_disk.contains("[owner.beta]"));
    assert!(
        !state_disk.contains(".state]"),
        "state file must not carry redundant .state nesting: {state_disk}",
    );
    assert!(state_disk.contains("installed_version = \"v1\""));
    assert!(state_disk.contains("installed_version = \"v2\""));

    let loaded = Config::load_from(f.path()).unwrap();
    assert_eq!(loaded, cfg);
}

#[test]
fn save_creates_managed_dir_when_missing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("plugin-manager.toml");
    let cfg = one_sub_config(
        "owner",
        "repo",
        Subscription {
            state: SubscriptionState {
                installed_version: Some("v1".into()),
                ..SubscriptionState::default()
            },
            ..sub()
        },
    );
    cfg.save_to(&path).unwrap();
    let state_path = state_path_for(&path);
    assert!(
        state_path.parent().unwrap().is_dir(),
        "managed/ should have been created"
    );
    assert!(state_path.is_file(), "state.toml should exist");
}

#[test]
fn load_drops_state_without_matching_user_entry() {
    // Hand-craft a sidecar containing a row for an owner/repo the user
    // file doesn't list. `load_from` should silently ignore the orphan
    // state entry rather than fabricating a Subscription for it.
    let f = TempConfig::new();
    one_sub_config("real-owner", "real-repo", sub())
        .save_to(f.path())
        .unwrap();
    // Overwrite the sidecar with both a real and an orphan entry.
    fs::create_dir_all(f.state_path().parent().unwrap()).unwrap();
    fs::write(
        f.state_path(),
        b"[real-owner.real-repo]\ninstalled_version = \"v1\"\n\n\
          [ghost-owner.ghost-repo]\ninstalled_version = \"v9\"\n",
    )
    .unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    assert!(!loaded.subscriptions.contains_key("ghost-owner"));
    assert_eq!(
        loaded
            .subscriptions
            .get("real-owner")
            .and_then(|m| m.get("real-repo"))
            .unwrap()
            .state
            .installed_version
            .as_deref(),
        Some("v1"),
    );
}

#[test]
fn load_with_missing_sidecar_yields_empty_state() {
    // Fresh-install case: user file present, sidecar missing.
    let cfg = one_sub_config(
        "owner",
        "repo",
        Subscription {
            channel: Channel::Prerelease,
            ..sub()
        },
    );
    let f = TempConfig::new();
    cfg.save_to(f.path()).unwrap();
    // Delete the sidecar that save_to wrote; mimic a hand-removed cache.
    fs::remove_file(f.state_path()).unwrap();
    let loaded = Config::load_from(f.path()).unwrap();
    let (_, _, s) = first_sub(&loaded);
    assert!(s.state.is_empty(), "sidecar gone -> empty state");
    assert_eq!(s.channel, Channel::Prerelease);
}

#[test]
fn sidecar_rewritten_when_state_clears() {
    // Save with state, then save again with state cleared. The sidecar must
    // shrink so a re-load returns a state-free Subscription.
    let f = TempConfig::new();
    let mut cfg = one_sub_config(
        "owner",
        "repo",
        Subscription {
            state: SubscriptionState {
                installed_version: Some("v1".into()),
                ..SubscriptionState::default()
            },
            ..sub()
        },
    );
    cfg.save_to(f.path()).unwrap();
    for sub in cfg.subscriptions.values_mut().flat_map(|r| r.values_mut()) {
        sub.state = SubscriptionState::default();
    }
    cfg.save_to(f.path()).unwrap();

    let loaded = Config::load_from(f.path()).unwrap();
    let (_, _, s) = first_sub(&loaded);
    assert!(s.state.is_empty(), "state should be cleared in sidecar");

    let state_disk = fs::read_to_string(f.state_path()).unwrap();
    assert!(
        !state_disk.contains("installed_version"),
        "stale state should be gone from sidecar: {state_disk}",
    );
}

#[test]
fn state_fields_render_alphabetically_in_sidecar() {
    let cfg = one_sub_config(
        "owner",
        "repo",
        Subscription {
            state: SubscriptionState {
                cached_at: Some(2),
                cached_published_at: Some(3),
                cached_tag: Some("t".into()),
                installed_asset: Some("a.so".into()),
                installed_at: Some(1),
                installed_version: Some("v".into()),
            },
            ..sub()
        },
    );
    let f = TempConfig::new();
    cfg.save_to(f.path()).unwrap();
    let state_disk = fs::read_to_string(f.state_path()).unwrap();
    let p_cached_at = state_disk.find("cached_at = ").unwrap();
    let p_cached_published_at = state_disk.find("cached_published_at = ").unwrap();
    let p_cached_tag = state_disk.find("cached_tag = ").unwrap();
    let p_installed_asset = state_disk.find("installed_asset = ").unwrap();
    let p_installed_at = state_disk.find("installed_at = ").unwrap();
    let p_installed_version = state_disk.find("installed_version = ").unwrap();
    assert!(p_cached_at < p_cached_published_at);
    assert!(p_cached_published_at < p_cached_tag);
    assert!(p_cached_tag < p_installed_asset);
    assert!(p_installed_asset < p_installed_at);
    assert!(p_installed_at < p_installed_version);
}

#[test]
fn state_path_lives_under_managed_dir() {
    let p = state_path_for(Path::new("plugins/plugin-manager.toml"));
    assert_eq!(p, Path::new("plugins/managed/state.toml"));
}

// ---- v4 -> v5 split migration ----

#[test]
fn migrate_state_split_lifts_state_into_sidecar() {
    let dir = tempdir().unwrap();
    let user_path = dir.path().join("plugin-manager.toml");
    let state_path = state_path_for(&user_path);

    // Hand-write a v4-shape file: user fields + state subtable mixed in
    // the same document with the legacy STATE_DIVIDER comment.
    fs::write(
        &user_path,
        b"[octocat.hello-world]\n\
          channel = \"prerelease\"\n\
          disabled = true\n\n\
          # ---- managed by plugin-manager (do not edit below) ----\n\n\
          [octocat.hello-world.state]\n\
          installed_version = \"v1.2.3\"\n\
          installed_asset = \"hello-world.so\"\n",
    )
    .unwrap();

    migrate_state_into_sidecar_at(&user_path).unwrap();

    // User file must no longer carry the state subtable.
    let user_disk = fs::read_to_string(&user_path).unwrap();
    assert!(
        !user_disk.contains(".state]"),
        "user file should be free of state subtables after migrate: {user_disk}",
    );
    assert!(user_disk.contains("[octocat.hello-world]"));
    assert!(user_disk.contains("channel = \"prerelease\""));
    assert!(user_disk.contains("disabled = true"));

    // Sidecar must now hold the lifted state with hoisted [owner.repo].
    let state_disk = fs::read_to_string(&state_path).unwrap();
    assert!(state_disk.contains("[octocat.hello-world]"));
    assert!(state_disk.contains("installed_version = \"v1.2.3\""));
    assert!(state_disk.contains("installed_asset = \"hello-world.so\""));
    // Round-trip preserves the merged shape.
    let loaded = Config::load_from(&user_path).unwrap();
    let s = loaded
        .subscriptions
        .get("octocat")
        .and_then(|m| m.get("hello-world"))
        .unwrap();
    assert_eq!(s.channel, Channel::Prerelease);
    assert!(s.disabled);
    assert_eq!(s.state.installed_version.as_deref(), Some("v1.2.3"));
    assert_eq!(s.state.installed_asset.as_deref(), Some("hello-world.so"));
}

#[test]
fn migrate_state_split_is_noop_when_sidecar_exists() {
    let dir = tempdir().unwrap();
    let user_path = dir.path().join("plugin-manager.toml");
    let state_path = state_path_for(&user_path);

    fs::write(
        &user_path,
        b"[octocat.hello-world]\n\n\
          [octocat.hello-world.state]\n\
          installed_version = \"legacy\"\n",
    )
    .unwrap();
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    fs::write(
        &state_path,
        b"[octocat.hello-world]\ninstalled_version = \"already-migrated\"\n",
    )
    .unwrap();

    migrate_state_into_sidecar_at(&user_path).unwrap();

    // User file untouched; sidecar untouched.
    assert!(fs::read_to_string(&user_path).unwrap().contains(".state]"));
    assert!(
        fs::read_to_string(&state_path)
            .unwrap()
            .contains("already-migrated"),
    );
}

#[test]
fn migrate_state_split_is_noop_when_user_file_missing() {
    let dir = tempdir().unwrap();
    let user_path = dir.path().join("plugin-manager.toml");
    let state_path = state_path_for(&user_path);

    migrate_state_into_sidecar_at(&user_path).unwrap();

    assert!(!user_path.exists());
    assert!(!state_path.exists());
}

#[test]
fn migrate_state_split_skips_writing_when_no_legacy_state() {
    let dir = tempdir().unwrap();
    let user_path = dir.path().join("plugin-manager.toml");
    let state_path = state_path_for(&user_path);
    // A file in the new shape (no state subtables) - migration should be
    // idempotent and not create an empty sidecar.
    fs::write(&user_path, b"[octocat.hello-world]\ndisabled = true\n").unwrap();

    migrate_state_into_sidecar_at(&user_path).unwrap();

    assert!(!state_path.exists(), "no state -> no sidecar written");
    assert!(
        fs::read_to_string(&user_path)
            .unwrap()
            .contains("disabled = true")
    );
}

// ---- v3 -> v4 file-rename migration ----

#[test]
fn migrate_legacy_config_renames_file_and_self_key() {
    let dir = tempdir().unwrap();
    let legacy = dir.path().join("plugin-updater.toml");
    let current = dir.path().join("plugin-manager.toml");

    let original = one_sub_config(
        SELF_OWNER,
        LEGACY_SELF_REPO,
        Subscription {
            state: SubscriptionState {
                installed_version: Some("v1.2.3".into()),
                ..SubscriptionState::default()
            },
            ..sub()
        },
    );
    original.save_to(&legacy).unwrap();

    migrate_legacy_config_at(&legacy, &current).unwrap();

    assert!(!legacy.exists(), "legacy file should be removed");
    assert!(current.exists(), "new file should exist");

    let migrated = Config::load_from(&current).unwrap();
    let owner_map = migrated.subscriptions.get(SELF_OWNER).unwrap();
    assert!(!owner_map.contains_key(LEGACY_SELF_REPO));
    let new_sub = owner_map
        .get(SELF_REPO)
        .expect("self key should be rewritten");
    assert_eq!(new_sub.state.installed_version.as_deref(), Some("v1.2.3"));
}

#[test]
fn migrate_legacy_config_no_op_when_new_path_exists() {
    let dir = tempdir().unwrap();
    let legacy = dir.path().join("plugin-updater.toml");
    let current = dir.path().join("plugin-manager.toml");
    one_sub_config("a", "b", sub()).save_to(&legacy).unwrap();
    one_sub_config("c", "d", sub()).save_to(&current).unwrap();

    migrate_legacy_config_at(&legacy, &current).unwrap();

    assert!(
        legacy.exists(),
        "legacy untouched when new file already exists"
    );
    let cfg = Config::load_from(&current).unwrap();
    assert!(cfg.subscriptions.contains_key("c"));
    assert!(!cfg.subscriptions.contains_key("a"));
}

#[test]
fn migrate_legacy_config_no_op_when_legacy_missing() {
    let dir = tempdir().unwrap();
    let legacy = dir.path().join("plugin-updater.toml");
    let current = dir.path().join("plugin-manager.toml");

    migrate_legacy_config_at(&legacy, &current).unwrap();

    assert!(!current.exists());
}

#[test]
fn migrate_legacy_config_keeps_new_self_when_both_keys_present() {
    // Pathological: a hand-edited legacy file already contains an entry under
    // the new repo name. The new entry wins; the legacy entry is dropped.
    let dir = tempdir().unwrap();
    let legacy = dir.path().join("plugin-updater.toml");
    let current = dir.path().join("plugin-manager.toml");

    let mut subs = BTreeMap::new();
    let mut repos = BTreeMap::new();
    repos.insert(
        LEGACY_SELF_REPO.into(),
        Subscription {
            state: SubscriptionState {
                installed_version: Some("legacy".into()),
                ..SubscriptionState::default()
            },
            ..sub()
        },
    );
    repos.insert(
        SELF_REPO.into(),
        Subscription {
            state: SubscriptionState {
                installed_version: Some("new".into()),
                ..SubscriptionState::default()
            },
            ..sub()
        },
    );
    subs.insert(SELF_OWNER.into(), repos);
    Config {
        subscriptions: subs,
    }
    .save_to(&legacy)
    .unwrap();

    migrate_legacy_config_at(&legacy, &current).unwrap();

    let migrated = Config::load_from(&current).unwrap();
    let owner_map = migrated.subscriptions.get(SELF_OWNER).unwrap();
    assert!(!owner_map.contains_key(LEGACY_SELF_REPO));
    assert_eq!(
        owner_map
            .get(SELF_REPO)
            .unwrap()
            .state
            .installed_version
            .as_deref(),
        Some("new")
    );
}

/// Two threads modify different fields of the same subscription
/// concurrently. Without `Config::modify_at`'s lock, the later writer's
/// `load -> mutate -> save` chain could clobber the earlier writer's
/// change because both load+save are non-atomic together. With the lock,
/// both fields land on disk regardless of order.
#[test]
fn modify_at_serializes_concurrent_writers() {
    let f = TempConfig::new();
    one_sub_config("octocat", "hello-world", sub())
        .save_to(f.path())
        .unwrap();

    let path: PathBuf = f.path().into();
    let iters = 64;
    let barrier = Arc::new(Barrier::new(2));

    let writer_install = {
        let path = path.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            for i in 0..iters {
                let asset = format!("plugin-{i}.so");
                Config::modify_at(&path, |cfg| {
                    let sub = cfg
                        .subscriptions
                        .get_mut("octocat")
                        .and_then(|m| m.get_mut("hello-world"))
                        .unwrap();
                    sub.state.installed_asset = Some(asset.clone());
                })
                .unwrap();
            }
            Config::modify_at(&path, |cfg| {
                let sub = cfg
                    .subscriptions
                    .get_mut("octocat")
                    .and_then(|m| m.get_mut("hello-world"))
                    .unwrap();
                sub.state.installed_asset = None;
            })
            .unwrap();
        })
    };

    let writer_cache = {
        let path = path.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            for i in 0..iters {
                Config::modify_at(&path, |cfg| {
                    let sub = cfg
                        .subscriptions
                        .get_mut("octocat")
                        .and_then(|m| m.get_mut("hello-world"))
                        .unwrap();
                    sub.state.cached_at = Some(i);
                    sub.state.cached_tag = Some(format!("v0.0.{i}"));
                })
                .unwrap();
            }
        })
    };

    writer_install.join().unwrap();
    writer_cache.join().unwrap();

    let final_cfg = Config::load_from(&path).unwrap();
    let (_, _, s) = first_sub(&final_cfg);
    // install writer ends with None.
    assert!(
        s.state.installed_asset.is_none(),
        "stale installed_asset after concurrent writers: {:?}",
        s.state.installed_asset,
    );
    // cache writer ends with the last iter's value.
    assert_eq!(s.state.cached_at, Some(iters - 1));
    assert_eq!(
        s.state.cached_tag.as_deref(),
        Some(format!("v0.0.{}", iters - 1)).as_deref()
    );
}

#[test]
fn self_installed_version_matches_cargo_pkg_version() {
    let v = self_installed_version();
    assert!(v.starts_with('v'), "expected leading v, got {v}");
    assert_eq!(v, format!("v{}", env!("CARGO_PKG_VERSION")));
}
