mod plugin;

#[cfg(test)]
mod tests;

use std::{
    cell::RefCell,
    io,
    os::raw::{c_int, c_void},
    path::{Path, PathBuf},
};

use classicube_helpers::color;
use classicube_sys::IGameComponent;
use tracing::{debug, error, warn};

use crate::{
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
            // and skip loading this session — the user can /unsubscribe,
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

        if let Err(e) = warn_on_collision(asset) {
            warn!("collision check for {id}: {e:#}");
        }

        let path = Path::new(MANAGED_DIR).join(asset);
        let path_str = path.to_string_lossy().into_owned();
        // Use the dlopen function name so a crash in the loaded library's
        // static constructors (before Init even runs) is attributed to the
        // load step rather than blamed on Init.
        let load_result =
            with_breadcrumb(owner, repo, "DynamicLib_Load2", || plugin::try_load(&path_str));
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

fn warn_on_collision(asset: &str) -> io::Result<()> {
    let Some(collision) = detect_collision_in(Path::new(PLUGINS_DIR), asset)? else {
        return Ok(());
    };
    warn!(
        "{} also exists; remove it to avoid loading two copies",
        collision.display()
    );
    print_wrapped(format!(
        "{}Found {}{}{}: remove it to avoid loading two copies of the plugin",
        color::YELLOW,
        color::LIME,
        collision.display(),
        color::YELLOW,
    ));
    Ok(())
}

/// Returns the path of a same-named regular file under `plugins_dir` (the
/// game-loaded `plugins/`), if any. A directory of the same name is ignored
/// — only a file would actually be loaded by ClassiCube.
fn detect_collision_in(plugins_dir: &Path, asset: &str) -> io::Result<Option<PathBuf>> {
    let path = plugins_dir.join(asset);
    match path.metadata() {
        Ok(m) if m.is_file() => Ok(Some(path)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
