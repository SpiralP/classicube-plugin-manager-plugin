mod plugin;

#[cfg(test)]
mod tests;

use std::{
    cell::RefCell,
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

struct LoadedPlugin {
    owner: String,
    repo: String,
    library: *mut c_void,
    component: *mut IGameComponent,
}

thread_local!(
    static LOADED: RefCell<Vec<LoadedPlugin>> = const { RefCell::new(Vec::new()) };
);

pub fn init_managed(subs: &[(String, String, Subscription)]) {
    for (owner, repo, sub) in subs {
        if sub.disabled {
            continue;
        }
        // The self subscription lives in plugins/, not plugins/managed/, and
        // is already loaded by the game — dlopen'ing it here would double-load.
        if config::is_self(owner, repo) {
            continue;
        }
        let id = format!("{owner}/{repo}");
        if let Some(prev) = sub.state.in_callback.as_deref() {
            // Last session's breadcrumb survived. Treat it as "this sub
            // crashed inside `prev` last run", warn, clear the breadcrumb,
            // and skip loading this session — the user can /remove,
            // /update, or just retry on next startup.
            warn!("{id} crashed inside {prev} last session; skipping this run");
            print_wrapped(format!(
                "{}Previous session crashed inside {}{id}{} {}{prev}{}. Skipped this run.",
                color::YELLOW,
                color::LIME,
                color::YELLOW,
                color::LIME,
                color::YELLOW,
            ));
            if let Err(e) = config::set_in_callback_to(config_path(), owner, repo, None) {
                warn!("clearing carry-over breadcrumb for {id}: {e:#}");
            }
            continue;
        }
        let Some(asset) = sub.state.installed_asset.as_deref() else {
            continue;
        };
        let already_loaded = LOADED
            .with_borrow(|loaded| loaded.iter().any(|p| p.owner == *owner && p.repo == *repo));
        if already_loaded {
            continue;
        }

        // Reconcile already printed a chat warning for any plugins/ conflict
        // covering this sub, so skip silently here - a second message would
        // just duplicate the first.
        match detect_plugins_dir_conflict(
            Path::new(PLUGINS_DIR),
            repo,
            env::consts::DLL_SUFFIX,
            Some(asset),
        ) {
            Ok(Some(collision)) => {
                warn!(
                    "{id} not loaded: {} would load as a duplicate",
                    collision.display()
                );
                continue;
            }
            Ok(None) => {}
            Err(e) => warn!("collision check for {id}: {e:#}"),
        }

        let path = Path::new(MANAGED_DIR).join(asset);
        let path_str = path.to_string_lossy().into_owned();
        // Use the dlopen function name so a crash in the loaded library's
        // static constructors (before Init even runs) is attributed to the
        // load step rather than blamed on Init.
        let load_result = with_breadcrumb(owner, repo, "DynamicLib_Load2", || {
            plugin::try_load(&path_str)
        });
        match load_result {
            Ok((library, component, api_version)) => {
                match check_api_version(Plugin_ApiVersion, api_version) {
                    ApiVersionCheck::Ok => {
                        LOADED.with_borrow_mut(|loaded| {
                            loaded.push(LoadedPlugin {
                                owner: owner.clone(),
                                repo: repo.clone(),
                                library,
                                component,
                            });
                        });
                        debug!("loaded {id} from {path_str}");
                        run_init_sequence(component, owner, repo);
                    }
                    ApiVersionCheck::PluginOutdated => {
                        warn!(
                            "{id} has Plugin_ApiVersion {api_version}, host expects \
                             {Plugin_ApiVersion}; refusing to load"
                        );
                        print_wrapped(format!(
                            "{}{}{}{} plugin is outdated! Try getting a more recent version",
                            color::RED,
                            color::LIME,
                            id,
                            color::RED,
                        ));
                    }
                    ApiVersionCheck::HostOutdated => {
                        warn!(
                            "{id} has Plugin_ApiVersion {api_version}, host expects \
                             {Plugin_ApiVersion}; refusing to load"
                        );
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
            Err(e) => {
                error!("loading {id} from {path_str}: {e:#}");
                print_wrapped(format!(
                    "{}Failed to load {}{}{}: {}{e}",
                    color::RED,
                    color::LIME,
                    id,
                    color::RED,
                    color::WHITE,
                ));
            }
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

fn run_init_sequence(component: *mut IGameComponent, owner: &str, repo: &str) {
    let id = format!("{owner}/{repo}");
    let component = unsafe { &mut *component };
    if let Some(f) = component.Init {
        debug!("calling Init on {id}");
        with_breadcrumb(owner, repo, "Init", || unsafe { f() });
    }
    if let Some(f) = component.OnNewMap {
        debug!("calling OnNewMap on {id}");
        with_breadcrumb(owner, repo, "OnNewMap", || unsafe { f() });
    }
    if let Some(f) = component.OnNewMapLoaded {
        debug!("calling OnNewMapLoaded on {id}");
        with_breadcrumb(owner, repo, "OnNewMapLoaded", || unsafe { f() });
    }
}

pub fn free() {
    let drained: Vec<LoadedPlugin> =
        LOADED.with_borrow_mut(|loaded| loaded.drain(..).rev().collect());
    for plugin in &drained {
        let component = unsafe { &mut *plugin.component };
        if let Some(f) = component.Free {
            debug!("calling Free on {}/{}", plugin.owner, plugin.repo);
            with_breadcrumb(&plugin.owner, &plugin.repo, "Free", || unsafe { f() });
        }
    }
    for plugin in drained {
        plugin::unload(plugin.library);
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
        if !entry.metadata()?.is_file() {
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
