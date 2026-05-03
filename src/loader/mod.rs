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
    config::Subscription,
    installer::{MANAGED_DIR, PLUGINS_DIR},
};

struct LoadedPlugin {
    id: String,
    library: *mut c_void,
    component: *mut IGameComponent,
}

thread_local!(
    static LOADED: RefCell<Vec<LoadedPlugin>> = const { RefCell::new(Vec::new()) };
);

pub fn init_managed(subs: &[Subscription]) {
    for sub in subs {
        if sub.disabled {
            continue;
        }
        // The self subscription lives in plugins/, not plugins/managed/, and
        // is already loaded by the game — dlopen'ing it here would double-load.
        if sub.is_self() {
            continue;
        }
        let Some(asset) = sub.installed_asset.as_deref() else {
            continue;
        };
        let id = format!("{}/{}", sub.owner, sub.repo);
        let already_loaded = LOADED.with_borrow(|loaded| loaded.iter().any(|p| p.id == id));
        if already_loaded {
            continue;
        }

        if let Err(e) = warn_on_collision(asset) {
            warn!("collision check for {id}: {e:#}");
        }

        let path = Path::new(MANAGED_DIR).join(asset);
        let path_str = path.to_string_lossy().into_owned();
        match plugin::try_load(&path_str) {
            Ok((library, component, api_version)) => {
                match check_api_version(Plugin_ApiVersion, api_version) {
                    ApiVersionCheck::Ok => {
                        LOADED.with_borrow_mut(|loaded| {
                            loaded.push(LoadedPlugin {
                                id: id.clone(),
                                library,
                                component,
                            });
                        });
                        debug!("loaded {id} from {path_str}");
                        run_init_sequence(component, &id);
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

fn run_init_sequence(component: *mut IGameComponent, id: &str) {
    let component = unsafe { &mut *component };
    if let Some(f) = component.Init {
        debug!("calling Init on {id}");
        unsafe { f() };
    }
    if let Some(f) = component.OnNewMap {
        debug!("calling OnNewMap on {id}");
        unsafe { f() };
    }
    if let Some(f) = component.OnNewMapLoaded {
        debug!("calling OnNewMapLoaded on {id}");
        unsafe { f() };
    }
}

pub fn free() {
    let drained: Vec<LoadedPlugin> =
        LOADED.with_borrow_mut(|loaded| loaded.drain(..).rev().collect());
    for plugin in &drained {
        let component = unsafe { &mut *plugin.component };
        if let Some(f) = component.Free {
            debug!("calling Free on {}", plugin.id);
            unsafe { f() };
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
    let snapshot: Vec<(String, *mut IGameComponent)> =
        LOADED.with_borrow(|loaded| loaded.iter().map(|p| (p.id.clone(), p.component)).collect());
    for (id, component) in snapshot {
        let component = unsafe { &*component };
        if let Some(f) = pick(component) {
            debug!("calling {name} on {id}");
            unsafe { f() };
        }
    }
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
