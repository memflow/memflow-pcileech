/*!
Scans physical memory via the pcileech connector for the Linux kernel banner
(`"Linux version ..."`) without requiring a defs file, then prints the
discovered version(s) and guidance on where to obtain matching debug symbols.

This mirrors the banner-scan approach used by Volatility3.

# Usage:
```bash
# FPGA device (default):
cargo run --example discover_kernel -- :device=FPGA

# Raw memory file (for testing):
cargo run --example discover_kernel -- :device=rawmem,remote=/path/to/memory.raw
```
*/
use std::env::args;

use memflow_linux::discover_kernel_versions;

fn main() {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let connector_args = if let Some(arg) = args().nth(1) {
        arg.parse()
    } else {
        ":device=FPGA".parse()
    }
    .expect("unable to parse connector arguments");

    let mut connector = memflow_pcileech::create_connector(&connector_args)
        .expect("unable to create pcileech connector");

    println!("Scanning physical memory for Linux kernel banner(s)...");
    let found = discover_kernel_versions(&mut connector);

    if found.is_empty() {
        eprintln!("No Linux kernel banner found in physical memory.");
        eprintln!("Make sure the device is connected and the target is a running Linux system.");
        return;
    }

    println!("\nDiscovered {} unique kernel version(s):\n", found.len());
    for info in &found {
        println!("  Version  : {}", info.version);
        println!("  Banner   : {}", info.banner);
        println!("  Phys addr: {}", info.phys_address);
        println!();
    }

    println!("--- How to obtain debug symbols ---\n");
    for info in &found {
        println!("Kernel: {}", info.version);
        print_symbol_hints(&info.version);
        println!();
    }
}

fn print_symbol_hints(version: &str) {
    let lower = version.to_lowercase();

    if lower.contains("ubuntu")
        || lower.contains("generic")
        || lower.contains("azure")
        || lower.contains("aws")
        || lower.contains("gcp")
    {
        println!("  Ubuntu / Debian:");
        println!("    sudo apt-get install linux-image-{version}-dbgsym");
        println!("    (or download from https://ddebs.ubuntu.com/)");
        println!("  Then generate defs with memflow-linux-defs:");
        println!("    generate_defs /usr/lib/debug/boot/vmlinux-{version}");
    } else if lower.contains("fc")
        || lower.contains("el")
        || lower.contains("rhel")
        || lower.contains("centos")
    {
        println!("  Fedora / RHEL / CentOS:");
        println!("    sudo dnf debuginfo-install kernel-{version}");
        println!("    # vmlinux lands in /usr/lib/debug/lib/modules/{version}/vmlinux");
        println!("  Then generate defs with memflow-linux-defs:");
        println!("    generate_defs /usr/lib/debug/lib/modules/{version}/vmlinux");
    } else if lower.contains("arch") || lower.contains("manjaro") {
        println!("  Arch Linux:");
        println!("    Build vmlinux from the matching kernel source.");
        println!("    See: https://wiki.archlinux.org/title/Debugging/Getting_traces");
    } else {
        println!("  Generic steps:");
        println!("    1. Obtain the unstripped vmlinux for kernel {version}");
        println!("       Ubuntu/Debian : https://ddebs.ubuntu.com/");
        println!("       Fedora/RHEL   : dnf debuginfo-install kernel-{version}");
        println!("       From source   : the vmlinux at the kernel build-tree root");
        println!("    2. Generate a defs file with memflow-linux-defs:");
        println!("       generate_defs /path/to/vmlinux");
        println!("    3. Pass the generated .toml when running memflow-linux:");
        println!("       MEMFLOW_LINUX_PROFILE=/path/to/vmlinux.toml");
    }
}
