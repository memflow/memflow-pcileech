extern crate bindgen;
extern crate cc;
extern crate pkg_config;

use std::env;
use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "windows")]
fn os_define() -> &'static str {
    "_WIN32"
}

#[cfg(target_os = "linux")]
fn os_define() -> &'static str {
    "LINUX"
}

fn build_leechcore(target: &str) {
    let mut files = vec![
        "oscompatibility.c",
        "leechcore.c",
        "util.c",
        "memmap.c",
        "device_file.c",
        "device_fpga.c",
        "device_pmem.c",
        "device_tmd.c",
        "device_usb3380.c",
        "leechrpcclient.c",
    ];
    if target.contains("windows") {
        files.push("leechrpc_c.c");
        files.push("leechrpcshared.c");
    }

    let mut cfg = cc::Build::new();
    cfg.cpp(false)
        .files(
            files
                .iter()
                .map(|o| "src/leechcore/leechcore/".to_string() + o)
                .collect::<Vec<_>>(),
        )
        .flag(&format!("-D{}", os_define()));

    if !target.contains("windows") {
        // setup additional flags
        cfg.flag("-fvisibility=hidden");
        cfg.flag("-fPIC");
        cfg.flag("-pthread");
        cfg.flag("-g");
        cfg.flag("-ldl");

        // add libusb-1.0 on *nix
        pkg_config::probe_library("libusb-1.0")
            .unwrap_or_else(|err| panic!("Failed to find libusb-1.0 via pkg-config: {:?}", err));

        let libusb_flags = Command::new("pkg-config")
            .args(&["libusb-1.0", "--libs", "--cflags"])
            .output()
            .unwrap_or_else(|err| panic!("Failed to find libusb-1.0 via pkg-config: {:?}", err));

        for flag in String::from_utf8_lossy(&libusb_flags.stdout)
            .trim()
            .split(" ")
        {
            cfg.flag(flag);
        }
    } else {
        // copy pre-compiled idl file into the leechcore folder
        std::fs::copy("gen/leechrpc_c.c", "src/leechcore/leechcore/leechrpc_c.c").unwrap();
        std::fs::copy("gen/leechrpc_h.h", "src/leechcore/leechcore/leechrpc_h.h").unwrap();

        // link against required libraries
        println!("cargo:rustc-link-lib=rpcrt4");
        println!("cargo:rustc-link-lib=setupapi");
        println!("cargo:rustc-link-lib=winusb");
        println!("cargo:rustc-link-lib=ws2_32");
    }

    cfg.compile("libleechcore.a");

    if target.contains("windows") {
        // remove temporary generated files
        std::fs::remove_file("src/leechcore/leechcore/leechrpc_c.c").unwrap();
        std::fs::remove_file("src/leechcore/leechcore/leechrpc_h.h").unwrap();
    }

    println!("cargo:rustc-link-lib=static=leechcore");
}

fn main() -> () {
    let target = env::var("TARGET").unwrap();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // build leechcore
    build_leechcore(&target);

    // generate bindings
    let mut builder = bindgen::builder()
        .clang_arg(format!("-D{}", os_define()))
        .header("./src/leechcore/leechcore/leechcore.h");

    // workaround for windows.h
    // see https://github.com/rust-lang/rust-bindgen/issues/1556
    if target.contains("windows") {
        builder = builder
            .blacklist_type("LPMONITORINFOEXA?W?")
            .blacklist_type("LPTOP_LEVEL_EXCEPTION_FILTER")
            .blacklist_type("MONITORINFOEXA?W?")
            .blacklist_type("PEXCEPTION_FILTER")
            .blacklist_type("PEXCEPTION_ROUTINE")
            .blacklist_type("PSLIST_HEADER")
            .blacklist_type("PTOP_LEVEL_EXCEPTION_FILTER")
            .blacklist_type("PVECTORED_EXCEPTION_HANDLER")
            .blacklist_type("_?L?P?CONTEXT")
            .blacklist_type("_?L?P?EXCEPTION_POINTERS")
            .blacklist_type("_?P?DISPATCHER_CONTEXT")
            .blacklist_type("_?P?EXCEPTION_REGISTRATION_RECORD")
            .blacklist_type("_?P?IMAGE_TLS_DIRECTORY.*")
            .blacklist_type("_?P?NT_TIB")
            .blacklist_type("tagMONITORINFOEXA")
            .blacklist_type("tagMONITORINFOEXW")
            .blacklist_function("AddVectoredContinueHandler")
            .blacklist_function("AddVectoredExceptionHandler")
            .blacklist_function("CopyContext")
            .blacklist_function("GetThreadContext")
            .blacklist_function("GetXStateFeaturesMask")
            .blacklist_function("InitializeContext")
            .blacklist_function("InitializeContext2")
            .blacklist_function("InitializeSListHead")
            .blacklist_function("InterlockedFlushSList")
            .blacklist_function("InterlockedPopEntrySList")
            .blacklist_function("InterlockedPushEntrySList")
            .blacklist_function("InterlockedPushListSListEx")
            .blacklist_function("LocateXStateFeature")
            .blacklist_function("QueryDepthSList")
            .blacklist_function("RaiseFailFastException")
            .blacklist_function("RtlCaptureContext")
            .blacklist_function("RtlCaptureContext2")
            .blacklist_function("RtlFirstEntrySList")
            .blacklist_function("RtlInitializeSListHead")
            .blacklist_function("RtlInterlockedFlushSList")
            .blacklist_function("RtlInterlockedPopEntrySList")
            .blacklist_function("RtlInterlockedPushEntrySList")
            .blacklist_function("RtlInterlockedPushListSListEx")
            .blacklist_function("RtlQueryDepthSList")
            .blacklist_function("RtlRestoreContext")
            .blacklist_function("RtlUnwindEx")
            .blacklist_function("RtlVirtualUnwind")
            .blacklist_function("SetThreadContext")
            .blacklist_function("SetUnhandledExceptionFilter")
            .blacklist_function("SetXStateFeaturesMask")
            .blacklist_function("UnhandledExceptionFilter")
            .blacklist_function("__C_specific_handler");
    }

    let bindings = builder
        .generate()
        .unwrap_or_else(|err| panic!("Failed to generate bindings: {:?}", err));

    let bindings_path = out_dir.join("leechcore.rs");
    bindings
        .write_to_file(&bindings_path)
        .unwrap_or_else(|_| panic!("Failed to write {}", bindings_path.display()));
}
