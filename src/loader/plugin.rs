use std::{
    ffi::CString,
    os::raw::{c_int, c_void},
};

use anyhow::{Result, bail};
use classicube_helpers::time;
use classicube_sys::{
    DynamicLib_DescribeError, DynamicLib_Get2, DynamicLib_Load2, IGameComponent, OwnedString,
    cc_string,
};
use tracing::{debug, warn};

fn get_error() -> String {
    let mut buf = [0u8; 256];
    let mut s = cc_string {
        buffer: buf.as_mut_ptr() as *mut _,
        length: 0,
        capacity: buf.len() as u16,
    };
    unsafe {
        DynamicLib_DescribeError(&mut s);
    }
    format!("{s}")
}

fn dll_load(path: &str) -> Result<*mut c_void> {
    let owned = OwnedString::new(path);
    let ptr = unsafe { DynamicLib_Load2(owned.as_cc_string()) };
    if ptr.is_null() {
        bail!(get_error());
    }
    Ok(ptr)
}

fn dll_get(library: *mut c_void, symbol_name: &str) -> Result<*mut c_void> {
    let symbol = CString::new(symbol_name)?;
    let ptr = unsafe { DynamicLib_Get2(library, symbol.as_ptr()) };
    if ptr.is_null() {
        bail!(get_error());
    }
    Ok(ptr)
}

pub fn try_load(path: &str) -> Result<(*mut c_void, *mut IGameComponent, c_int)> {
    let library = time!("dll_load", 5000, {
        debug!("dll_load {path}");
        dll_load(path)?
    });
    let api_version_ptr = dll_get(library, "Plugin_ApiVersion")? as *const c_int;
    let api_version = unsafe { *api_version_ptr };
    let plugin_component = dll_get(library, "Plugin_Component")? as *mut IGameComponent;
    Ok((library, plugin_component, api_version))
}

// classicube-sys exposes no `DynamicLib_Unload`; drop down to the platform
// primitive directly. dlclose only actually unmaps when the refcount hits zero
// AND nothing still holds pointers into the library (event lists, scheduled
// tasks, chat commands, etc.); a managed plugin that doesn't deregister
// cleanly in `Free` will crash the host on the next callback. That risk is
// the whole reason `/unload` exists - to verify the answer with real plugins.
#[cfg(unix)]
pub fn unload(library: *mut c_void) {
    let rc = unsafe { libc::dlclose(library) };
    if rc != 0 {
        warn!("dlclose returned {rc}: {}", get_error());
    } else {
        debug!("dlclose ok");
    }
}

#[cfg(windows)]
pub fn unload(library: *mut c_void) {
    use windows::Win32::Foundation::{FreeLibrary, HMODULE};
    match unsafe { FreeLibrary(HMODULE(library.cast())) } {
        Ok(()) => debug!("FreeLibrary ok"),
        Err(e) => warn!("FreeLibrary failed: {e}"),
    }
}
