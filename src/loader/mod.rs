mod plugin;

#[cfg(test)]
mod tests;

use std::{
    cell::RefCell,
    collections::HashSet,
    env, fs, io,
    os::raw::{c_int, c_void},
    path::{Path, PathBuf},
};

use classicube_helpers::color;
use classicube_sys::IGameComponent;
use tracing::{debug, error, warn};

use crate::{
    asset_match,
    chat::print_wrapped,
    component::Plugin_ApiVersion,
    config::{self, Subscription, config_path},
    installer::{MANAGED_DIR, PLUGINS_DIR},
};

// Why we don't `dlclose` / `FreeLibrary` managed plugins on unload:
// managed plugins typically use Rust's `thread_local!`, which registers
// per-thread destructors via `__cxa_thread_atexit_impl` (glibc) or pthread
// TSD. Those destructor function pointers live inside the plugin's mapped
// pages. The ClassiCube game thread doesn't exit until the process exits,
// so unmapping the library while a `thread_local!` cell is still
// initialized leaves a dangling destructor that fires at process shutdown
// against unmapped memory.
//
// glibc since ~2.18 quietly neutralizes this by refcounting the DSO when
// `__cxa_thread_atexit_impl` is used, so `dlclose` becomes a no-op there
// anyway - we lose nothing on glibc. musl has no such protection (hard
// crash); Windows `FreeLibrary` similarly drops TLS state aggressively.
//
// So `/unload` calls the plugin's `Free` (to deregister event handlers,
// chat commands, scheduled tasks) and removes the entry from `LOADED`,
// but the library stays mapped for the rest of the process lifetime.
// Real reload of a freshly-updated binary requires a game restart,
// matching the rest of the codebase (self-update, deferred-load).
struct LoadedPlugin {
    owner: String,
    repo: String,
    #[expect(
        dead_code,
        reason = "library handle is intentionally leaked; see module comment about TLS destructors"
    )]
    library: *mut c_void,
    component: *mut IGameComponent,
}

thread_local!(
    static LOADED: RefCell<Vec<LoadedPlugin>> = const { RefCell::new(Vec::new()) };
);

// Subs that the Startup carry-over check decided to skip this session.
// The disk `in_callback` breadcrumb is the source of truth ONLY at Startup,
// before any callback has fired and started flickering it on/off; after
// that, a worker-thread `Config::load()` from the deferred update pass can
// land mid-`with_breadcrumb_at` and see a transient `Some` that has nothing
// to do with last session's crash. So we read the disk breadcrumb only at
// Startup, copy "skip this sub" into this set, and have all later phases
// (Catchup, the deferred pass's auto-load) consult the set instead. Tests
// rely on the loader running on the test thread, hence `thread_local!`.
thread_local!(
    static SKIPPED_CARRYOVER: RefCell<HashSet<(String, String)>> = RefCell::default();
);

/// Which lifecycle callbacks to invoke after a successful `dlopen` + `Init`.
///
/// `Startup` is the host-`Init` path (`Loader::init`): the host hasn't yet
/// dispatched `OnNewMap` / `OnNewMapLoaded`, so we ONLY call the managed
/// plugin's `Init`. The Loader component's existing `on_new_map` /
/// `on_new_map_loaded` forwarders deliver those events when the host fires
/// them for real.
///
/// `Catchup` is for mid-session loads (deferred update pass, `/load`,
/// post-`/update` reload): the host has already fired `Init`, `OnNewMap`,
/// and at least one `OnNewMapLoaded` against an empty `LOADED`, so we
/// fire all three on the new entry to bring it in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecyclePhase {
    Startup,
    Catchup,
}

pub fn init_managed(subs: &[(String, String, Subscription)], phase: LifecyclePhase) {
    for (owner, repo, sub) in subs {
        let outcome = load_one(owner, repo, sub, phase);
        report_init_outcome(owner, repo, &outcome);
    }
}

/// Outcome of attempting to load one subscription's managed binary. Variants
/// are pure values — the caller decides how (and whether) to surface each one
/// to the user. `init_managed` keeps its existing mostly-silent behavior;
/// `/load` chats every variant.
pub enum LoadOutcome {
    /// dlopen + Init/OnNewMap/OnNewMapLoaded all succeeded; the entry is in `LOADED`.
    Loaded,
    /// `sub.disabled = true`. Auto-load skips silently; `/load` refuses.
    Disabled,
    /// `(owner, repo)` is the manager itself - the game already owns its handle.
    IsSelf,
    /// Previous session left an `in_callback` breadcrumb on this sub, just
    /// detected by the Startup pass. The disk breadcrumb is cleared so the
    /// next session can auto-retry, and the sub is recorded in
    /// `SKIPPED_CARRYOVER` so any subsequent auto-load attempt this session
    /// (e.g. the deferred pass's Catchup) returns `SkippedFromCarryover`
    /// without re-chatting. The carry-over name is returned for chat.
    CrashCarryover { previous: String },
    /// Startup already detected and chatted a carry-over for this sub; a
    /// later auto-load attempt is honoring that decision silently. The
    /// caller does NOT chat. Explicit user-driven retry paths (`/load`,
    /// post-`/update` reload) clear the entry first via
    /// `clear_carryover_skip` and so never see this variant.
    SkippedFromCarryover,
    /// `sub.state.installed_asset` is None - nothing to dlopen yet.
    NotInstalled,
    /// An entry for `(owner, repo)` was already in `LOADED`.
    AlreadyLoaded,
    /// A file in `plugins/` would auto-load as a duplicate of this sub.
    PluginsDirConflict { path: PathBuf },
    /// `dlopen` (or one of the symbol lookups) failed.
    LoadError(anyhow::Error),
    /// Plugin's `Plugin_ApiVersion` is older than the host expects.
    PluginOutdated { plugin: c_int, host: c_int },
    /// Plugin's `Plugin_ApiVersion` is newer than the host can run.
    HostOutdated { plugin: c_int, host: c_int },
}

/// Outcome of `unload_one`. `/unload` is the only caller; values map directly
/// to chat replies.
pub enum UnloadOutcome {
    /// Component's `Free` ran (best-effort). The library stays mapped -
    /// see module comment about thread-local destructors.
    Unloaded,
    /// No `LOADED` entry for `(owner, repo)`.
    NotLoaded,
    /// `(owner, repo)` is the manager itself - refuse, the game owns the handle.
    IsSelf,
}

/// Pure pre-flight checks for `load_one`. Returns `Some(_)` for outcomes
/// decidable from `sub` alone (no FFI, no `LOADED` access, no filesystem,
/// no config write). Extracted so tests can exercise these branches without
/// pulling `plugin::try_load` (and its `DynamicLib_*` symbols) into the
/// test-binary link graph - the host provides those symbols at runtime, not
/// at `cargo test` link time.
fn classify_early(owner: &str, repo: &str, sub: &Subscription) -> Option<LoadOutcome> {
    if sub.disabled {
        return Some(LoadOutcome::Disabled);
    }
    if config::is_self(owner, repo) {
        return Some(LoadOutcome::IsSelf);
    }
    None
}

/// Carry-over classification, split out of `load_one` so tests can drive it
/// against a temp config without pulling `plugin::try_load` symbols into the
/// test link graph. Returns `Some(_)` when load should short-circuit.
///
/// Two ways to short-circuit:
/// 1. `SKIPPED_CARRYOVER` already contains `(owner, repo)` (any phase) -
///    Startup detected a carry-over earlier this session. Returns
///    `SkippedFromCarryover` (silent).
/// 2. `phase == Startup` and `sub.state.in_callback` is `Some(_)` on disk -
///    last session crashed. Records the sub in `SKIPPED_CARRYOVER`, clears
///    the disk breadcrumb (so next session auto-retries), returns
///    `CrashCarryover { previous }`.
///
/// Catchup intentionally does NOT read the disk breadcrumb: at that point
/// the host is firing managed-plugin callbacks on the main thread and
/// `with_breadcrumb_at` is flickering `in_callback` on/off; a worker-thread
/// `Config::load()` can land between the set and clear and produce a
/// false-positive carry-over.
fn classify_carryover_at(
    path: &Path,
    owner: &str,
    repo: &str,
    sub: &Subscription,
    phase: LifecyclePhase,
) -> Option<LoadOutcome> {
    let key = (owner.to_owned(), repo.to_owned());
    if SKIPPED_CARRYOVER.with_borrow(|s| s.contains(&key)) {
        return Some(LoadOutcome::SkippedFromCarryover);
    }
    if phase == LifecyclePhase::Startup
        && let Some(prev) = sub.state.in_callback.as_deref()
    {
        let previous = prev.to_owned();
        SKIPPED_CARRYOVER.with_borrow_mut(|s| {
            s.insert(key);
        });
        if let Err(e) = config::set_in_callback_to(path, owner, repo, None) {
            warn!("clearing carry-over breadcrumb for {owner}/{repo}: {e:#}");
        }
        return Some(LoadOutcome::CrashCarryover { previous });
    }
    None
}

/// Drop `(owner, repo)` from the in-process carry-over skip set so a
/// subsequent `load_one` can attempt the dlopen even if the Startup pass
/// previously declared this sub crashed-this-session. Used by explicit
/// user-driven retry paths (`/load`, post-`/update` reload). The deferred
/// update pass's auto-load does NOT call this - Startup-skipped subs stay
/// skipped until the user opts in via one of those commands.
pub fn clear_carryover_skip(owner: &str, repo: &str) {
    SKIPPED_CARRYOVER.with_borrow_mut(|s| {
        s.remove(&(owner.to_owned(), repo.to_owned()));
    });
}

/// Load one subscription's managed binary into the running process, mirroring
/// what `init_managed` does at startup. Mutates `LOADED` and (for the
/// carry-over case) the on-disk config. Chats "Loading {id}" right before
/// the dlopen so a crash in the loaded library leaves a visible trail; the
/// returned `LoadOutcome` is the caller's hook for outcome chat
/// (`report_init_outcome` for auto-load, custom mapping for `/load`).
pub fn load_one(owner: &str, repo: &str, sub: &Subscription, phase: LifecyclePhase) -> LoadOutcome {
    if let Some(o) = classify_early(owner, repo, sub) {
        return o;
    }
    if let Some(o) = classify_carryover_at(config_path(), owner, repo, sub, phase) {
        return o;
    }
    let id = format!("{owner}/{repo}");
    let Some(asset) = sub.state.installed_asset.as_deref() else {
        return LoadOutcome::NotInstalled;
    };
    if is_loaded(owner, repo) {
        return LoadOutcome::AlreadyLoaded;
    }

    match detect_plugins_dir_conflict(
        Path::new(PLUGINS_DIR),
        repo,
        env::consts::DLL_SUFFIX,
        Some(asset),
    ) {
        Ok(Some(path)) => return LoadOutcome::PluginsDirConflict { path },
        Ok(None) => {}
        Err(e) => warn!("collision check for {id}: {e:#}"),
    }

    let path = Path::new(MANAGED_DIR).join(asset);
    let path_str = path.to_string_lossy().into_owned();
    print_wrapped(format!("{}Loading {}{id}", color::PINK, color::LIME));
    // Use the dlopen function name so a crash in the loaded library's static
    // constructors (before Init even runs) is attributed to the load step
    // rather than blamed on Init.
    let load_result = with_breadcrumb(owner, repo, "DynamicLib_Load2", || {
        plugin::try_load(&path_str)
    });
    let (library, component, api_version) = match load_result {
        Ok(t) => t,
        Err(e) => return LoadOutcome::LoadError(e),
    };
    match check_api_version(Plugin_ApiVersion, api_version) {
        ApiVersionCheck::Ok => {
            LOADED.with_borrow_mut(|loaded| {
                loaded.push(LoadedPlugin {
                    owner: owner.to_owned(),
                    repo: repo.to_owned(),
                    library,
                    component,
                });
            });
            debug!("loaded {id} from {path_str}");
            run_init_sequence(component, owner, repo, phase);
            LoadOutcome::Loaded
        }
        ApiVersionCheck::PluginOutdated => LoadOutcome::PluginOutdated {
            plugin: api_version,
            host: Plugin_ApiVersion,
        },
        ApiVersionCheck::HostOutdated => LoadOutcome::HostOutdated {
            plugin: api_version,
            host: Plugin_ApiVersion,
        },
    }
}

/// Pure pre-flight check for `unload_one`. Mirrors `classify_early` on the
/// load side: returns `Some(_)` for outcomes decidable without FFI / `LOADED`
/// access, extracted so tests can exercise the IsSelf branch without pulling
/// `print_wrapped` / `Chat_Add` into the test-binary link graph.
fn classify_early_unload(owner: &str, repo: &str) -> Option<UnloadOutcome> {
    if config::is_self(owner, repo) {
        return Some(UnloadOutcome::IsSelf);
    }
    None
}

/// Unload the running copy of `(owner, repo)`: drop it from `LOADED` and
/// call the component's `Free` wrapped in a breadcrumb so it deregisters
/// host-side state. The library stays mapped - see module comment about
/// thread-local destructors. The LOADED borrow is released before `Free`
/// runs so a managed callback can re-enter the host (chat, etc.) without
/// deadlocking. Chats "Unloading {id}" right before the `Free` call (and
/// only when there's actually a `Free` to invoke).
pub fn unload_one(owner: &str, repo: &str) -> UnloadOutcome {
    if let Some(o) = classify_early_unload(owner, repo) {
        return o;
    }
    let plugin = LOADED.with_borrow_mut(|loaded| {
        loaded
            .iter()
            .position(|p| p.owner == owner && p.repo == repo)
            .map(|i| loaded.remove(i))
    });
    let Some(plugin) = plugin else {
        return UnloadOutcome::NotLoaded;
    };
    let id = format!("{}/{}", plugin.owner, plugin.repo);
    let component = unsafe { &mut *plugin.component };
    if let Some(f) = component.Free {
        debug!("calling Free on {id}");
        print_wrapped(format!("{}Unloading {}{id}", color::PINK, color::LIME));
        with_breadcrumb(&plugin.owner, &plugin.repo, "Free", || unsafe { f() });
    }
    UnloadOutcome::Unloaded
}

/// Whether `(owner, repo)` currently has an entry in `LOADED`.
pub fn is_loaded(owner: &str, repo: &str) -> bool {
    LOADED.with_borrow(|loaded| loaded.iter().any(|p| p.owner == owner && p.repo == repo))
}

/// Map `LoadOutcome` to startup-style logging + chat. Mirrors the behavior
/// the inlined `init_managed` loop used to have: silent on the routine
/// skips (Disabled / IsSelf / NotInstalled / AlreadyLoaded), warn-only for
/// plugins-dir conflicts (reconcile already chatted), full chat for actual
/// errors and api-version mismatches. `/load` does NOT use this mapping -
/// it chats every variant.
fn report_init_outcome(owner: &str, repo: &str, outcome: &LoadOutcome) {
    let id = format!("{owner}/{repo}");
    match outcome {
        LoadOutcome::Loaded
        | LoadOutcome::Disabled
        | LoadOutcome::IsSelf
        | LoadOutcome::NotInstalled
        | LoadOutcome::AlreadyLoaded
        | LoadOutcome::SkippedFromCarryover => {}
        LoadOutcome::CrashCarryover { previous } => {
            warn!("{id} crashed inside {previous} last session; skipping this run");
            print_wrapped(format!(
                "{}Previous session crashed inside {}{id}{} {}{previous}{}. Skipped this run.",
                color::YELLOW,
                color::LIME,
                color::YELLOW,
                color::LIME,
                color::YELLOW,
            ));
        }
        LoadOutcome::PluginsDirConflict { path } => {
            warn!(
                "{id} not loaded: {} would load as a duplicate",
                path.display()
            );
        }
        LoadOutcome::LoadError(e) => {
            error!("loading {id}: {e:#}");
            print_wrapped(format!(
                "{}Failed to load {}{}{}: {}{e}",
                color::RED,
                color::LIME,
                id,
                color::RED,
                color::WHITE,
            ));
        }
        LoadOutcome::PluginOutdated { plugin, host } => {
            warn!("{id} has Plugin_ApiVersion {plugin}, host expects {host}; refusing to load");
            print_wrapped(format!(
                "{}{}{}{} plugin is outdated! Try getting a more recent version",
                color::RED,
                color::LIME,
                id,
                color::RED,
            ));
        }
        LoadOutcome::HostOutdated { plugin, host } => {
            warn!("{id} has Plugin_ApiVersion {plugin}, host expects {host}; refusing to load");
            print_wrapped(format!(
                "{}Your game is too outdated to use {}{}{} plugin! Try updating it",
                color::RED,
                color::LIME,
                id,
                color::RED,
            ));
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ApiVersionCheck {
    Ok,
    PluginOutdated,
    HostOutdated,
}

fn check_api_version(host: c_int, plugin: c_int) -> ApiVersionCheck {
    if plugin < host {
        ApiVersionCheck::PluginOutdated
    } else if plugin > host {
        ApiVersionCheck::HostOutdated
    } else {
        ApiVersionCheck::Ok
    }
}

fn run_init_sequence(
    component: *mut IGameComponent,
    owner: &str,
    repo: &str,
    phase: LifecyclePhase,
) {
    run_init_sequence_at(config_path(), component, owner, repo, phase);
}

fn run_init_sequence_at(
    path: &Path,
    component: *mut IGameComponent,
    owner: &str,
    repo: &str,
    phase: LifecyclePhase,
) {
    let id = format!("{owner}/{repo}");
    let component = unsafe { &mut *component };
    if let Some(f) = component.Init {
        debug!("calling Init on {id}");
        with_breadcrumb_at(path, owner, repo, "Init", || unsafe { f() });
    }
    // Startup runs from the host's own Init callback - the host has NOT yet
    // dispatched OnNewMap or OnNewMapLoaded, so we must not pre-fire them.
    // The Loader component's on_new_map / on_new_map_loaded forwarders will
    // deliver those when the host fires them for real.
    if phase == LifecyclePhase::Startup {
        return;
    }
    if let Some(f) = component.OnNewMap {
        debug!("calling OnNewMap on {id}");
        with_breadcrumb_at(path, owner, repo, "OnNewMap", || unsafe { f() });
    }
    if let Some(f) = component.OnNewMapLoaded {
        debug!("calling OnNewMapLoaded on {id}");
        with_breadcrumb_at(path, owner, repo, "OnNewMapLoaded", || unsafe { f() });
    }
}

pub fn free() {
    // Don't carry skip decisions across a hot-reload boundary. The next
    // Startup will repopulate from the on-disk breadcrumb if the previous
    // session actually crashed; stale entries from this cycle would otherwise
    // suppress a legitimate retry after the user reloads us.
    SKIPPED_CARRYOVER.with_borrow_mut(HashSet::clear);

    let drained: Vec<LoadedPlugin> =
        LOADED.with_borrow_mut(|loaded| loaded.drain(..).rev().collect());
    for plugin in &drained {
        let component = unsafe { &mut *plugin.component };
        if let Some(f) = component.Free {
            debug!("calling Free on {}/{}", plugin.owner, plugin.repo);
            with_breadcrumb(&plugin.owner, &plugin.repo, "Free", || unsafe { f() });
        }
    }
}

pub fn reset() {
    forward_callback("Reset", |c| c.Reset);
}

pub fn on_new_map() {
    forward_callback("OnNewMap", |c| c.OnNewMap);
}

pub fn on_new_map_loaded() {
    forward_callback("OnNewMapLoaded", |c| c.OnNewMapLoaded);
}

fn forward_callback(name: &str, pick: impl Fn(&IGameComponent) -> Option<unsafe extern "C" fn()>) {
    // Snapshot pointers under the borrow so a managed plugin's callback can
    // re-enter the host (chat, etc.) without deadlocking on LOADED.
    let snapshot: Vec<(String, String, *mut IGameComponent)> = LOADED.with_borrow(|loaded| {
        loaded
            .iter()
            .map(|p| (p.owner.clone(), p.repo.clone(), p.component))
            .collect()
    });
    for (owner, repo, component) in snapshot {
        let component = unsafe { &*component };
        if let Some(f) = pick(component) {
            debug!("calling {name} on {owner}/{repo}");
            with_breadcrumb(&owner, &repo, name, || unsafe { f() });
        }
    }
}

fn with_breadcrumb<R>(owner: &str, repo: &str, callback: &str, f: impl FnOnce() -> R) -> R {
    with_breadcrumb_at(config_path(), owner, repo, callback, f)
}

/// Persist `in_callback = Some(name)` for `(owner, repo)`, run `f`, then
/// clear `in_callback` and persist again. If `f` panics or the process dies
/// mid-call, the breadcrumb survives — that's the entire point. The
/// `let r = f(); clear; r` shape (rather than `Drop`) is deliberate so an
/// unwind skips the clear and leaves the breadcrumb on disk.
fn with_breadcrumb_at<R>(
    path: &Path,
    owner: &str,
    repo: &str,
    callback: &str,
    f: impl FnOnce() -> R,
) -> R {
    if let Err(e) = config::set_in_callback_to(path, owner, repo, Some(callback.into())) {
        warn!("breadcrumb set for {owner}/{repo} {callback}: {e:#}");
    }
    let r = f();
    if let Err(e) = config::set_in_callback_to(path, owner, repo, None) {
        warn!("breadcrumb clear for {owner}/{repo} {callback}: {e:#}");
    }
    r
}

/// Returns the path of a regular file under `plugins_dir` (the game-loaded
/// `plugins/`) that ClassiCube would already `dlopen` as part of `repo`'s
/// plugin. A file is considered a conflict if it either matches the repo's
/// canonical or rust-cdylib variant naming (per `asset_match::matches_repo`),
/// or has the same filename as `installed_asset` - the latter catches custom
/// release-asset naming like `classicube_foo_linux_x86_64.so` where the
/// shape doesn't match the repo name. When present, the loader skips loading
/// the managed copy to avoid running the plugin twice. Directories are
/// ignored - ClassiCube only loads files.
fn detect_plugins_dir_conflict(
    plugins_dir: &Path,
    repo: &str,
    dll_suffix: &str,
    installed_asset: Option<&str>,
) -> io::Result<Option<PathBuf>> {
    let read_dir = match fs::read_dir(plugins_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut hits: Vec<String> = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        // Follow symlinks: `dlopen` does, so a symlink-to-`.so` is a real
        // plugin file. `DirEntry::metadata` is `lstat` and would skip them.
        let path = entry.path();
        let md = match fs::metadata(&path) {
            Ok(md) => md,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !md.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if asset_match::matches_repo(&name, repo, dll_suffix)
            || Some(name.as_str()) == installed_asset
        {
            hits.push(name);
        }
    }
    // Sort for deterministic output regardless of readdir order.
    hits.sort();
    Ok(hits.into_iter().next().map(|n| plugins_dir.join(n)))
}
