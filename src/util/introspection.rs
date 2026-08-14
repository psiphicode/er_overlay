#![allow(unsafe_op_in_unsafe_fn)]

use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
use winapi::shared::minwindef::HMODULE;
use winapi::um::libloaderapi::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GetModuleFileNameW, GetModuleHandleExW,
};

fn get_own_module_handle() -> Option<HMODULE> {
    let mut handle: HMODULE = std::ptr::null_mut();
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            get_own_module_handle as *const _,
            &mut handle,
        )
    };
    if ok == 0 { None } else { Some(handle) }
}

pub fn get_dll_path() -> Option<PathBuf> {
    let handle = get_own_module_handle()?;
    let mut buf = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(handle, buf.as_mut_ptr(), buf.len() as u32) };
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
}

pub fn get_dll_directory() -> Option<PathBuf> {
    get_dll_path()?.parent().map(|p| p.to_path_buf())
}
