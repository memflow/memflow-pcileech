#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(improper_ctypes)]
#![allow(clippy::missing_safety_doc)]

//#![allow(clippy::useless_transmute)]
//#![allow(clippy::cognitive_complexity)]

include!(concat!(env!("OUT_DIR"), "/leechcore.rs"));

#[cfg(target_os = "windows")]
extern "C" {
    pub fn DllMain(_: *const u8, _: u32, _: *const u8) -> u32;
}

use ctor::{ctor, dtor};

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
