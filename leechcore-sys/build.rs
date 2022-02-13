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

#[cfg(target_os = "macos")]
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
        "device_vmware.c",
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
        .flag(&format!("-D{}", os_define()))
        .flag("-D_GNU_SOURCE");
    // EXPORTED_FUNCTION= to not export any symbols

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
            .split(' ')
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

fn main() {
    let target = env::var("TARGET").unwrap();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // build leechcore
    build_leechcore(&target);

    // generate bindings
    let mut builder = bindgen::builder()
        .clang_arg(format!("-D{} -D_GNU_SOURCE", os_define()))
        .header("./src/leechcore/leechcore/leechcore.h");

    // workaround for windows.h
    // see https://github.com/rust-lang/rust-bindgen/issues/1556
    if target.contains("windows") {
        builder = builder.blacklist_type("_?P?IMAGE_TLS_DIRECTORY.*")
    }

    let bindings = builder
        .generate()
        .unwrap_or_else(|err| panic!("Failed to generate bindings: {:?}", err));

    let bindings_path = out_dir.join("leechcore.rs");
    bindings
        .write_to_file(&bindings_path)
        .unwrap_or_else(|_| panic!("Failed to write {}", bindings_path.display()));
}
