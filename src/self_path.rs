#[cfg(test)]
mod tests;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use tracing::{debug, info, warn};

/// Resolve the on-disk path of the shared library (or test binary) that
/// contains this function. Used by the self-update path to locate the loaded
/// manager binary so we can rename it through `install_bytes_to`.
///
/// Linux/macOS: `dladdr` resolves any code address to its containing object.
/// Windows: `GetModuleHandleExW(FROM_ADDRESS, ...)` then `GetModuleFileNameW`.
#[cfg(unix)]
pub fn current_lib_path() -> Result<PathBuf> {
    use std::{ffi::CStr, mem, os::raw::c_void};

    let mut info: libc::Dl_info = unsafe { mem::zeroed() };
    let addr = current_lib_path as *const c_void;
    let rc = unsafe { libc::dladdr(addr, &mut info) };
    if rc == 0 {
        return Err(anyhow!("dladdr failed for current cdylib"));
    }
    if info.dli_fname.is_null() {
        return Err(anyhow!("dladdr returned null dli_fname"));
    }
    let cstr = unsafe { CStr::from_ptr(info.dli_fname) };
    let s = cstr
        .to_str()
        .map_err(|e| anyhow!("non-UTF8 dli_fname: {e}"))?;
    Ok(PathBuf::from(s))
}

#[cfg(windows)]
pub fn current_lib_path() -> Result<PathBuf> {
    use std::{
        ffi::OsString,
        os::{raw::c_void, windows::ffi::OsStringExt},
    };

    use windows::{
        Win32::{
            Foundation::HMODULE,
            System::LibraryLoader::{
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
                GetModuleHandleExW,
            },
        },
        core::PCWSTR,
    };

    let mut module = HMODULE::default();
    let addr = current_lib_path as *const c_void;
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(addr.cast::<u16>()),
            &mut module,
        )
        .map_err(|e| anyhow!("GetModuleHandleExW failed: {e}"))?;
    }

    let mut buf: Vec<u16> = vec![0; 1024];
    loop {
        let n = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
        if n == 0 {
            return Err(anyhow!("GetModuleFileNameW failed"));
        }
        if n < buf.len() {
            buf.truncate(n);
            break;
        }
        buf.resize(buf.len() * 2, 0);
    }
    Ok(PathBuf::from(OsString::from_wide(&buf)))
}

/// Substring in the published v3 GitHub-release asset names
/// (`classicube_plugin_updater_<os>_<arch>.<ext>`). Released artifacts ship
/// under this name regardless of platform, and that's the name that ends up
/// in `plugins/` on a user's machine - not rustc's default
/// `libclassicube_plugin_updater_plugin.<ext>`.
const LEGACY_NAME_FRAGMENT: &str = "classicube_plugin_updater";

/// Replacement substring for the v4 release asset names
/// (`classicube_plugin_manager_<os>_<arch>.<ext>`).
const NEW_NAME_FRAGMENT: &str = "classicube_plugin_manager";

/// Best-effort v3 -> v4 migration: if the running binary's filename contains
/// the legacy `classicube_plugin_updater` fragment, rename it to swap that
/// fragment for `classicube_plugin_manager`, preserving the OS/arch/ext
/// suffix. The mapping in this process keeps pointing at the old inode
/// (Linux/macOS) or the old name (Windows), so the rename only takes effect
/// on next startup. If the rename fails, log and move on - the user can
/// rename by hand.
pub fn rename_legacy_self_binary() {
    let loaded = match current_lib_path() {
        Ok(p) => p,
        Err(e) => {
            debug!("skip legacy binary rename: {e:#}");
            return;
        }
    };
    if let Err(e) = rename_legacy_binary_at(&loaded, LEGACY_NAME_FRAGMENT, NEW_NAME_FRAGMENT) {
        warn!("legacy binary rename failed: {e:#}");
    }
}

pub(crate) fn rename_legacy_binary_at(
    loaded: &Path,
    legacy_fragment: &str,
    new_fragment: &str,
) -> Result<()> {
    let Some(basename) = loaded.file_name().and_then(|n| n.to_str()) else {
        return Ok(());
    };
    if !basename.contains(legacy_fragment) {
        return Ok(());
    }
    let new_basename = basename.replace(legacy_fragment, new_fragment);
    let Some(dir) = loaded.parent() else {
        return Ok(());
    };
    let new_path = dir.join(&new_basename);
    if new_path.exists() {
        return Ok(());
    }
    match fs::rename(loaded, &new_path) {
        Ok(()) => {
            info!(
                "renamed legacy binary {} -> {}",
                loaded.display(),
                new_path.display()
            );
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e)
            .with_context(|| format!("renaming {} -> {}", loaded.display(), new_path.display())),
    }
}
