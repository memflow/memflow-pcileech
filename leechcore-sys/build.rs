extern crate cc;
extern crate pkg_config;

#[cfg(feature = "bindgen")]
extern crate bindgen;

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

fn build() {
    let mut files = vec![
        "device_file.c",
        "device_fpga.c",
        "device_hibr.c",
        "device_pmem.c",
        "device_tmd.c",
        "device_usb3380.c",
        "device_vmm.c",
        "device_vmware.c",
        "leechcore.c",
        "leechrpcclient.c",
        "memmap.c",
        "oscompatibility.c",
        "util.c",
        "ob/ob_bytequeue.c",
        "ob/ob_core.c",
        "ob/ob_map.c",
        "ob/ob_set.c",
    ];
    if target().contains("windows") {
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
        .include("src/leechcore/includes/")
        .flag(&format!("-D{}", os_define()))
        .flag("-D_GNU_SOURCE");
    // EXPORTED_FUNCTION= to not export any symbols

    if !target().contains("windows") {
        // setup additional flags
        cfg.flag("-fPIC");
        cfg.flag("-pthread");
        cfg.flag("-fvisibility=hidden");
        cfg.flag("-fstack-protector-strong");
        cfg.flag("-D_FORTIFY_SOURCE=2");
        cfg.flag("-O1"); // this is necessary, otherwise inline funcs in leechcore will result in undefined external symbols
        cfg.flag("-z,noexecstack");
        cfg.flag("-Wall");
        cfg.flag("-Wno-multichar");
        cfg.flag("-Wno-unused-result");
        cfg.flag("-Wno-unused-variable");
        cfg.flag("-Wno-unused-value");
        cfg.flag("-Wno-pointer-to-int-cast");
        cfg.flag("-Wno-int-to-pointer-cast");
        cfg.flag("-g");
        cfg.flag("-ldl");

        // add libusb-1.0 on *nix
        pkg_config::probe_library("libusb-1.0")
            .unwrap_or_else(|err| panic!("Failed to find libusb-1.0 via pkg-config: {:?}", err));

        let libusb_flags = Command::new("pkg-config")
            .args(["libusb-1.0", "--libs", "--cflags"])
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
        std::fs::copy("gen/leechrpc_c.c", "src/leechcore/leechcore/leechrpc_c.c")
            .expect("Failed to copy leechrpc_c.c");
        std::fs::copy("gen/leechrpc_h.h", "src/leechcore/leechcore/leechrpc_h.h")
            .expect("Failed to copy leechrpc_h.h");

        // link against required libraries
        println!("cargo:rustc-link-lib=rpcrt4");
        println!("cargo:rustc-link-lib=setupapi");
        println!("cargo:rustc-link-lib=winusb");
        println!("cargo:rustc-link-lib=ws2_32");
        println!("cargo:rustc-link-lib=secur32");
        println!("cargo:rustc-link-lib=credui");
        println!("cargo:rustc-link-lib=ole32");
    }

    cfg.compile("libleechcore.a");

    if target().contains("windows") {
        // remove temporary generated files
        std::fs::remove_file("src/leechcore/leechcore/leechrpc_c.c")
            .expect("Failed to remove leechrpc_c.c");
        std::fs::remove_file("src/leechcore/leechcore/leechrpc_h.h")
            .expect("Failed to remove leechrpc_h.h");
    }

    println!("cargo:rustc-link-lib=static=leechcore");
}

#[cfg(feature = "bindgen")]
fn generate_bindings() {
    let mut builder = bindgen::builder()
        .clang_arg(format!("-D{} -D_GNU_SOURCE", os_define()))
        .header("./src/leechcore/leechcore/leechcore.h");

    // workaround for windows.h
    // see https://github.com/rust-lang/rust-bindgen/issues/1556
    if target().contains("windows") {
        builder = builder.blocklist_type("_?P?IMAGE_TLS_DIRECTORY.*")
    }

    let bindings = builder
        .generate()
        .unwrap_or_else(|err| panic!("Failed to generate bindings: {:?}", err));

    bindings
        .write_to_file(&bindings_src_path())
        .unwrap_or_else(|_| panic!("Failed to write {}", bindings_src_path().display()));
}

fn copy_bindings() {
    let bindings_src_path = bindings_src_path();
    let bindings_dst_path = bindings_dst_path();
    std::fs::copy(bindings_src_path, bindings_dst_path)
        .expect("Failed to copy leechcore.rs bindings to OUT_DIR");
}

// path helper functions
fn target() -> String {
    env::var("TARGET").unwrap()
}
fn out_dir() -> PathBuf {
    PathBuf::from(env::var("OUT_DIR").unwrap())
}
fn bindings_src_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    #[cfg(target_os = "windows")]
    let bindings_src_path = manifest_dir.join("src").join("leechcore_windows.rs");
    #[cfg(target_os = "linux")]
    let bindings_src_path = manifest_dir.join("src").join("leechcore_linux.rs");
    #[cfg(target_os = "macos")]
    let bindings_src_path = manifest_dir.join("src").join("leechcore_mac.rs");

    bindings_src_path
}
fn bindings_dst_path() -> PathBuf {
    out_dir().to_path_buf().join("leechcore.rs")
}

fn main() {
    // build leechcore
    build();

    // generate bindings from headers (optional)
    #[cfg(feature = "bindgen")]
    generate_bindings();

    // copy bindings to build directory
    copy_bindings();
}
