use std::{ffi::CString, os::raw::c_void};

use anyhow::{Result, bail};
use classicube_helpers::time;
use classicube_sys::{
    DynamicLib_DescribeError, DynamicLib_Get2, DynamicLib_Load2, IGameComponent, OwnedString,
    cc_string,
};
use tracing::debug;

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

pub fn try_load(path: &str) -> Result<(*mut c_void, *mut IGameComponent)> {
    let library = time!("dll_load", 5000, {
        debug!("dll_load {path}");
        dll_load(path)?
    });
    let plugin_component = dll_get(library, "Plugin_Component")? as *mut IGameComponent;
    Ok((library, plugin_component))
}

// classicube-sys exposes no `DynamicLib_Unload`; the library stays mapped
// until process exit. Match cef-loader's behavior — keep this around as a
// hook so future symmetry (e.g. `dlclose` via `libc`) lands in one place.
pub fn unload(_library: *mut c_void) {}
