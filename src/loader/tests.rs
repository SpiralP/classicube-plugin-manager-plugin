use std::{
    collections::HashSet,
    fs, panic,
    path::{Path, PathBuf},
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use classicube_sys::IGameComponent;
use tempfile::tempdir;

use super::{
    ApiVersionCheck, CARRYOVERS, LifecyclePhase, LoadOutcome, SKIPPED_CARRYOVER, UnloadOutcome,
    check_api_version, classify_carryover_at, classify_early, classify_early_unload,
    clear_carryover_skip, detect_plugins_dir_conflict, prime_carryover_scan_at,
    run_init_sequence_at, with_breadcrumb_at,
};
use crate::config::{self, Subscription};

#[test]
fn missing_dir_returns_none() {
    let dir = tempdir().unwrap();
    let nonexistent = dir.path().join("does-not-exist");
    let result =
        detect_plugins_dir_conflict(&nonexistent, "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[test]
fn empty_dir_returns_none() {
    let dir = tempdir().unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[test]
fn canonical_named_file_is_a_conflict() {
    // ClassiCube would load this file directly out of plugins/; if we then
    // also load the managed copy, the plugin runs twice.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("classicube-foo-plugin.so"), b"x").unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert_eq!(
        result.as_deref(),
        Some(dir.path().join("classicube-foo-plugin.so").as_path())
    );
}

#[test]
fn variant_named_file_is_a_conflict() {
    // rust-cdylib output: lib prefix + underscores. ClassiCube loads it the
    // same way as the canonical filename, so it's also a duplicate-load
    // hazard.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("libclassicube_foo_plugin.so"), b"x").unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert_eq!(
        result.as_deref(),
        Some(dir.path().join("libclassicube_foo_plugin.so").as_path())
    );
}

#[test]
fn matches_installed_asset_filename_exactly() {
    // Release-asset names like `classicube_foo_linux_x86_64.so` don't match
    // the repo via shape normalization. If the user puts a copy of that
    // exact filename in plugins/ alongside our managed copy, ClassiCube
    // would load both. The installed_asset hint catches it.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("classicube_foo_linux_x86_64.so"), b"x").unwrap();
    let result = detect_plugins_dir_conflict(
        dir.path(),
        "classicube-foo-plugin",
        ".so",
        Some("classicube_foo_linux_x86_64.so"),
    )
    .unwrap();
    assert_eq!(
        result.as_deref(),
        Some(dir.path().join("classicube_foo_linux_x86_64.so").as_path())
    );
}

#[test]
fn installed_asset_hint_does_not_match_unrelated_files() {
    // Without a name-shape match and without an installed_asset equality, a
    // file like the asset hint that's *not* on disk shouldn't surface as a
    // conflict.
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("something-else.so"), b"x").unwrap();
    let result = detect_plugins_dir_conflict(
        dir.path(),
        "classicube-foo-plugin",
        ".so",
        Some("classicube_foo_linux_x86_64.so"),
    )
    .unwrap();
    assert!(result.is_none());
}

#[test]
fn unrelated_files_are_not_conflicts() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("classicube-bar-plugin.so"), b"x").unwrap();
    fs::write(dir.path().join("README.md"), b"x").unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[test]
fn directory_with_matching_name_does_not_collide() {
    // ClassiCube loads files, not directories, so a directory of the same
    // name shouldn't trigger a double-load warning.
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("classicube-foo-plugin.so")).unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[cfg(unix)]
#[test]
fn symlink_to_plugin_file_is_a_conflict() {
    // Common dev-loop pattern: `ln -s target/release/lib...so plugins/`.
    // `dlopen` follows symlinks, so we have to flag them as conflicts;
    // `DirEntry::metadata` is `lstat` and would silently drop them.
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let target = dir.path().join("real-libclassicube_foo_plugin.so");
    fs::write(&target, b"x").unwrap();
    let link = dir.path().join("libclassicube_foo_plugin.so");
    symlink(&target, &link).unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert_eq!(result.as_deref(), Some(link.as_path()));
}

#[cfg(unix)]
#[test]
fn dangling_symlink_is_skipped() {
    // A dangling symlink can't be `dlopen`'d, so it isn't a real
    // duplicate-load hazard. Skip rather than error.
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let link = dir.path().join("libclassicube_foo_plugin.so");
    symlink(dir.path().join("does-not-exist.so"), &link).unwrap();
    let result =
        detect_plugins_dir_conflict(dir.path(), "classicube-foo-plugin", ".so", None).unwrap();
    assert!(result.is_none());
}

#[test]
fn api_version_equal_is_ok() {
    assert_eq!(check_api_version(1, 1), ApiVersionCheck::Ok);
}

#[test]
fn api_version_plugin_lower_is_outdated() {
    assert_eq!(check_api_version(2, 1), ApiVersionCheck::PluginOutdated);
}

#[test]
fn api_version_plugin_higher_means_host_outdated() {
    assert_eq!(check_api_version(1, 2), ApiVersionCheck::HostOutdated);
}

/// Path math for the per-process breadcrumb file, duplicated here so the
/// loader tests don't have to leak the `breadcrumb` module's internals.
/// Kept in sync with `breadcrumb::breadcrumb_path`.
fn breadcrumb_file(dir: &Path, pid: u32) -> PathBuf {
    let ns = crate::breadcrumb::current_ns_inode();
    dir.join(format!("{ns}-{pid}.toml"))
}

/// Fixed PID used to seed a "previous-session crashed" breadcrumb file
/// in tests. The scan is consume-everything now, so the PID doesn't
/// need to be one the OS reports as dead - any number will do.
const SEED_PID: u32 = 12_345;

#[test]
fn breadcrumb_set_during_call_and_cleared_after() {
    let dir = tempdir().unwrap();
    let our_file = breadcrumb_file(dir.path(), std::process::id());

    let mid_call = std::cell::Cell::new(false);
    with_breadcrumb_at(dir.path(), "octocat", "hello-world", "OnNewMap", || {
        mid_call.set(our_file.exists());
    });

    assert!(
        mid_call.into_inner(),
        "breadcrumb file should exist mid-call"
    );
    assert!(
        !our_file.exists(),
        "breadcrumb file should be cleared after"
    );
}

#[test]
fn breadcrumb_survives_panic_in_closure() {
    // The whole point of the breadcrumb is to survive a crash inside the
    // managed callback. A panic is the closest in-process analog: if `f`
    // panics, the post-call clear must not run, and the on-disk breadcrumb
    // file must remain so the next-startup carry-over check can fire.
    let dir = tempdir().unwrap();
    let our_file = breadcrumb_file(dir.path(), std::process::id());

    let dir_for_closure = dir.path().to_owned();
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        with_breadcrumb_at(&dir_for_closure, "octocat", "hello-world", "Init", || {
            panic!("simulated crash");
        })
    }));
    assert!(result.is_err(), "expected panic to propagate");
    assert!(
        our_file.exists(),
        "breadcrumb file should remain after panic: {}",
        our_file.display()
    );
}

#[test]
fn breadcrumb_returns_closure_value() {
    let dir = tempdir().unwrap();
    let n = with_breadcrumb_at(dir.path(), "octocat", "hello-world", "Reset", || 42);
    assert_eq!(n, 42);
}

// `classify_early` exercises the FFI-free, side-effect-free branches of
// `load_one`. The success and dlopen-error paths need a real plugin binary
// and are out of scope for unit tests. The `unload_one` branches exposed to
// tests are similarly the ones that don't reach the platform unload call.

#[test]
fn classify_early_disabled_returns_disabled() {
    let sub = Subscription {
        disabled: true,
        ..Subscription::default()
    };
    assert!(matches!(
        classify_early("octocat", "hello-world", &sub),
        Some(LoadOutcome::Disabled)
    ));
}

#[test]
fn classify_early_self_returns_is_self() {
    // Even if the user's config has the self sub enabled and "installed", we
    // never dlopen it - the game already owns its handle.
    let sub = Subscription::default();
    assert!(matches!(
        classify_early(config::SELF_OWNER, config::SELF_REPO, &sub),
        Some(LoadOutcome::IsSelf)
    ));
}

#[test]
fn classify_early_disabled_takes_precedence_over_self() {
    // If the user disables the self sub by hand and then we also short-circuit
    // on is_self, both reasons apply; report Disabled because the disabled
    // flag is the user's explicit intent and the more actionable hint.
    let sub = Subscription {
        disabled: true,
        ..Subscription::default()
    };
    assert!(matches!(
        classify_early(config::SELF_OWNER, config::SELF_REPO, &sub),
        Some(LoadOutcome::Disabled)
    ));
}

#[test]
fn classify_early_normal_sub_returns_none() {
    // Falls through to the FFI/LOADED/filesystem checks in the full load_one.
    let sub = Subscription::default();
    assert!(classify_early("octocat", "hello-world", &sub).is_none());
}

#[test]
fn classify_early_unload_self_returns_is_self() {
    // Mirror of classify_early_self_returns_is_self on the unload side: even
    // if the user's config has the self sub enabled and "installed", /unload
    // self must refuse - the game owns the manager's handle, not us.
    assert!(matches!(
        classify_early_unload(config::SELF_OWNER, config::SELF_REPO),
        Some(UnloadOutcome::IsSelf)
    ));
}

#[test]
fn classify_early_unload_normal_sub_returns_none() {
    // Falls through to the LOADED lookup in the full unload_one.
    assert!(classify_early_unload("octocat", "hello-world").is_none());
}

// run_init_sequence dispatch tests. The "real Init must only call Init"
// invariant is the entire point of LifecyclePhase::Startup - if a future
// refactor reintroduces the OnNewMap / OnNewMapLoaded calls in the Startup
// branch, managed plugins would see those events fired twice (once early,
// once when the Loader component forwards the host's real dispatch). These
// tests are the regression guard.

// Single mutex serializes the two callback-counter tests below so they
// can coexist under `cargo test` (nextest already process-isolates, but
// this keeps the tests safe under either runner).
static CALLBACK_TEST_LOCK: Mutex<()> = Mutex::new(());
static INIT_CALLS: AtomicU32 = AtomicU32::new(0);
static ON_NEW_MAP_CALLS: AtomicU32 = AtomicU32::new(0);
static ON_NEW_MAP_LOADED_CALLS: AtomicU32 = AtomicU32::new(0);

extern "C" fn fake_init() {
    INIT_CALLS.fetch_add(1, Ordering::SeqCst);
}
extern "C" fn fake_on_new_map() {
    ON_NEW_MAP_CALLS.fetch_add(1, Ordering::SeqCst);
}
extern "C" fn fake_on_new_map_loaded() {
    ON_NEW_MAP_LOADED_CALLS.fetch_add(1, Ordering::SeqCst);
}

fn make_fake_component() -> IGameComponent {
    IGameComponent {
        Init: Some(fake_init),
        Free: None,
        Reset: None,
        OnNewMap: Some(fake_on_new_map),
        OnNewMapLoaded: Some(fake_on_new_map_loaded),
        next: ptr::null_mut(),
    }
}

fn reset_counters() {
    INIT_CALLS.store(0, Ordering::SeqCst);
    ON_NEW_MAP_CALLS.store(0, Ordering::SeqCst);
    ON_NEW_MAP_LOADED_CALLS.store(0, Ordering::SeqCst);
}

#[test]
fn startup_phase_only_fires_init() {
    // Hard rule from the "init managed on manager Init" change: the host's
    // own Init callback must NOT pre-fire OnNewMap or OnNewMapLoaded. The
    // Loader component's forwarders deliver those when the host dispatches
    // them for real.
    let _guard = CALLBACK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempdir().unwrap();
    reset_counters();

    let mut component = make_fake_component();
    run_init_sequence_at(
        dir.path(),
        &mut component,
        "octocat",
        "hello-world",
        LifecyclePhase::Startup,
    );

    assert_eq!(INIT_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(ON_NEW_MAP_CALLS.load(Ordering::SeqCst), 0);
    assert_eq!(ON_NEW_MAP_LOADED_CALLS.load(Ordering::SeqCst), 0);
}

#[test]
fn catchup_phase_fires_all_three_callbacks() {
    // Mid-session loads (deferred update pass, /load) need full catchup
    // because the host has already dispatched OnNewMap / first
    // OnNewMapLoaded against an empty LOADED before we got here.
    let _guard = CALLBACK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempdir().unwrap();
    reset_counters();

    let mut component = make_fake_component();
    run_init_sequence_at(
        dir.path(),
        &mut component,
        "octocat",
        "hello-world",
        LifecyclePhase::Catchup,
    );

    assert_eq!(INIT_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(ON_NEW_MAP_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(ON_NEW_MAP_LOADED_CALLS.load(Ordering::SeqCst), 1);
}

// classify_carryover_at tests. SKIPPED_CARRYOVER is a thread_local set of
// "we already decided this run, stay quiet" entries; CARRYOVERS is the
// per-process cache of the on-disk-derived dead-PID scan. Both are
// populated by the production code; tests run on the same test thread, so
// we serialize on a Mutex AND drain both up front for a clean baseline.

static CARRYOVER_TEST_LOCK: Mutex<()> = Mutex::new(());

fn reset_carryover_state() {
    SKIPPED_CARRYOVER.with_borrow_mut(HashSet::clear);
    CARRYOVERS.with_borrow_mut(|slot| *slot = None);
}

fn carryover_contains(owner: &str, repo: &str) -> bool {
    SKIPPED_CARRYOVER.with_borrow(|s| s.contains(&(owner.to_owned(), repo.to_owned())))
}

/// Drop a `<ns>-<pid>.toml` file in `dir` so the production
/// `breadcrumb::collect_dead` scan picks it up. Returns the path for
/// follow-up assertions.
fn seed_breadcrumb_file(dir: &Path, pid: u32, owner: &str, repo: &str, callback: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let body = format!("owner = \"{owner}\"\nrepo = \"{repo}\"\ncallback = \"{callback}\"\n");
    let path = breadcrumb_file(dir, pid);
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn startup_phase_records_carryover_and_consumes_disk_file() {
    // Real carry-over from a previous-session crash: Startup must mark the
    // sub in SKIPPED_CARRYOVER (so Catchup respects the skip) AND unlink
    // the on-disk breadcrumb file (so the *next* session can auto-retry
    // rather than staying stuck forever).
    let _guard = CARRYOVER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    reset_carryover_state();
    let dir = tempdir().unwrap();
    let bc = seed_breadcrumb_file(
        dir.path(),
        SEED_PID,
        "octocat",
        "hello-world",
        "OnNewMapLoaded",
    );

    let outcome = classify_carryover_at(
        dir.path(),
        "octocat",
        "hello-world",
        LifecyclePhase::Startup,
    );

    assert!(matches!(
        outcome,
        Some(LoadOutcome::CrashCarryover { ref previous }) if previous == "OnNewMapLoaded"
    ));
    assert!(carryover_contains("octocat", "hello-world"));
    assert!(!bc.exists(), "{} should be unlinked", bc.display());
}

#[test]
fn catchup_phase_does_not_scan_disk() {
    // A breadcrumb file on disk must NOT count as a carry-over on
    // Catchup: Startup is the only phase that consults the scan. The
    // file must be left alone for a *future* Startup to pick up.
    let _guard = CARRYOVER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    reset_carryover_state();
    let dir = tempdir().unwrap();
    let bc = seed_breadcrumb_file(
        dir.path(),
        SEED_PID,
        "octocat",
        "hello-world",
        "OnNewMapLoaded",
    );

    let outcome = classify_carryover_at(
        dir.path(),
        "octocat",
        "hello-world",
        LifecyclePhase::Catchup,
    );

    assert!(outcome.is_none(), "Catchup must fall through");
    assert!(!carryover_contains("octocat", "hello-world"));
    assert!(bc.exists(), "Catchup must leave the disk file alone");
}

#[test]
fn catchup_phase_respects_skipped_carryover_set() {
    // Startup-decision-preserved-into-Catchup: once Startup recorded the
    // skip (via real carry-over OR a hand-populated set in this test),
    // later phases short-circuit to SkippedFromCarryover.
    let _guard = CARRYOVER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    reset_carryover_state();
    SKIPPED_CARRYOVER.with_borrow_mut(|s| {
        s.insert(("octocat".into(), "hello-world".into()));
    });
    let dir = tempdir().unwrap();

    let outcome = classify_carryover_at(
        dir.path(),
        "octocat",
        "hello-world",
        LifecyclePhase::Catchup,
    );

    assert!(matches!(outcome, Some(LoadOutcome::SkippedFromCarryover)));
}

#[test]
fn clear_carryover_skip_allows_subsequent_attempt() {
    // Explicit-retry path (`/load`, post-`/update` reload): clearing the
    // skip must let the next classify_carryover_at fall through, both the
    // in-process SKIPPED_CARRYOVER set AND any cached CARRYOVERS entry
    // (otherwise re-classify would re-fire the carry-over outcome).
    let _guard = CARRYOVER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    reset_carryover_state();
    SKIPPED_CARRYOVER.with_borrow_mut(|s| {
        s.insert(("octocat".into(), "hello-world".into()));
    });
    CARRYOVERS.with_borrow_mut(|slot| {
        let mut m = std::collections::HashMap::new();
        m.insert(("octocat".into(), "hello-world".into()), "Init".into());
        *slot = Some(m);
    });

    clear_carryover_skip("octocat", "hello-world");
    assert!(!carryover_contains("octocat", "hello-world"));
    CARRYOVERS.with_borrow(|slot| {
        let m = slot.as_ref().unwrap();
        assert!(!m.contains_key(&("octocat".to_owned(), "hello-world".to_owned())));
    });

    let dir = tempdir().unwrap();
    let outcome = classify_carryover_at(
        dir.path(),
        "octocat",
        "hello-world",
        LifecyclePhase::Catchup,
    );
    assert!(outcome.is_none());
}

#[test]
fn startup_phase_falls_through_when_no_breadcrumb() {
    // Common path on a clean previous session: no on-disk breadcrumb
    // file, no entry in SKIPPED_CARRYOVER -> classify returns None and
    // load_one proceeds to dlopen.
    let _guard = CARRYOVER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    reset_carryover_state();
    let dir = tempdir().unwrap();

    let outcome = classify_carryover_at(
        dir.path(),
        "octocat",
        "hello-world",
        LifecyclePhase::Startup,
    );

    assert!(outcome.is_none());
    assert!(!carryover_contains("octocat", "hello-world"));
}

#[test]
fn prime_carryover_scan_unlinks_files_even_with_no_subs() {
    // The "user has zero subscriptions" / "all subs disabled" / "only-self"
    // cases never reach classify_carryover_at, so the lazy in-classify scan
    // would never fire. The eager prime call from Loader::init must clean
    // up leftover breadcrumb files anyway.
    let _guard = CARRYOVER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    reset_carryover_state();
    let dir = tempdir().unwrap();
    let bc = seed_breadcrumb_file(dir.path(), SEED_PID, "octocat", "ghost-plugin", "OnNewMap");

    prime_carryover_scan_at(dir.path());

    assert!(!bc.exists(), "{} should be unlinked", bc.display());
    // The entry remains in CARRYOVERS for any later classify_carryover_at
    // call to pick up, even though no current sub references it.
    CARRYOVERS.with_borrow(|slot| {
        let m = slot.as_ref().expect("scan must populate slot");
        assert_eq!(
            m.get(&("octocat".to_owned(), "ghost-plugin".to_owned()))
                .map(String::as_str),
            Some("OnNewMap")
        );
    });
}

#[test]
fn prime_carryover_scan_is_idempotent() {
    // Second call must not re-scan / replace the slot - that would resurrect
    // entries already consumed by classify_carryover_at's map.remove.
    let _guard = CARRYOVER_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    reset_carryover_state();
    let dir = tempdir().unwrap();
    seed_breadcrumb_file(dir.path(), SEED_PID, "octocat", "ghost", "Init");

    prime_carryover_scan_at(dir.path());
    // Simulate classify having consumed the entry.
    CARRYOVERS.with_borrow_mut(|slot| {
        slot.as_mut()
            .unwrap()
            .remove(&("octocat".into(), "ghost".into()));
    });
    // Re-seed (the first prime unlinked the original file, so we're
    // writing a fresh file with new contents). If the second prime
    // erroneously re-scanned, it would find this and insert
    // (other, sub) into the map.
    seed_breadcrumb_file(dir.path(), SEED_PID, "other", "sub", "Free");

    prime_carryover_scan_at(dir.path());

    CARRYOVERS.with_borrow(|slot| {
        let m = slot.as_ref().unwrap();
        assert!(
            !m.contains_key(&("octocat".to_owned(), "ghost".to_owned())),
            "consumed entry must stay consumed"
        );
        assert!(
            !m.contains_key(&("other".to_owned(), "sub".to_owned())),
            "second prime must not re-scan"
        );
    });
}
