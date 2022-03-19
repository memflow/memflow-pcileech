#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(improper_ctypes)]
#![allow(deref_nullptr)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::redundant_static_lifetimes)]
#![allow(clippy::redundant_static_lifetimes)]

include!(concat!(env!("OUT_DIR"), "/leechcore.rs"));

#[cfg(target_os = "windows")]
use ctor::{ctor, dtor};

#[cfg(target_os = "windows")]
extern "C" {
    pub fn DllMain(_: *const u8, _: u32, _: *const u8) -> u32;
}

#[ctor]
#[cfg(target_os = "windows")]
fn leechcore_attach() {
    // DLL_PROCESS_ATTACH
    unsafe {
        DllMain(std::ptr::null_mut(), 1, std::ptr::null_mut());
    }
}

#[dtor]
#[cfg(target_os = "windows")]
fn leechcore_detach() {
    // DLL_PROCESS_DETACH
    unsafe {
        DllMain(std::ptr::null_mut(), 0, std::ptr::null_mut());
    }
}
