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

fn main() -> () {
    //let target = env::var("TARGET").unwrap();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let objs = vec![
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

    pkg_config::probe_library("libusb-1.0")
        .unwrap_or_else(|err| panic!("Failed to find libusb-1.0 via pkg-config: {:?}", err));

    // TODO: windows
    // TODO: pkg_config ?
    let libusb_flags = Command::new("pkg-config")
        .args(&["libusb-1.0", "--libs", "--cflags"])
        .output()
        .unwrap_or_else(|err| panic!("Failed to find libusb-1.0 via pkg-config: {:?}", err));

    let mut cfg = cc::Build::new();
    cfg.cpp(false)
        .files(
            objs.iter()
                .map(|o| "src/leechcore/leechcore/".to_string() + o)
                .collect::<Vec<_>>(),
        )
        .flag(&format!("-D{}", os_define()))
        .flag("-fPIC")
        .flag("-fvisibility=hidden")
        .flag("-pthread")
        .flag("-g")
        .flag("-ldl");

    for flag in String::from_utf8_lossy(&libusb_flags.stdout)
        .trim()
        .split(" ")
    {
        cfg.flag(flag);
    }

    cfg.compile("libleechcore.a");

    // generate bindings
    let bindings = bindgen::builder()
        .clang_arg(format!("-D{}", os_define()))
        .header("./src/leechcore/leechcore/leechcore.h")
        .generate()
        .unwrap_or_else(|err| panic!("Failed to generate bindings: {:?}", err));

    let bindings_path = out_dir.join("leechcore.rs");
    bindings
        .write_to_file(&bindings_path)
        .unwrap_or_else(|_| panic!("Failed to write {}", bindings_path.display()));

    println!("cargo:rustc-link-lib=static=leechcore");
}
