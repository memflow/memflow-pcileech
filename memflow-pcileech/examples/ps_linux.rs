/*!
This example shows how to use the pcileech connector in conjunction
with the Linux OS layer. This example does not use the `Inventory` feature of
memflow but hard-wires the connector instance with the OS layer directly.

The example is an adaption of the memflow core process list example:
https://github.com/memflow/memflow/blob/next/memflow/examples/process_list.rs

# Usage:
Set `MEMFLOW_LINUX_PROFILE` to a generated Linux defs `.toml` file that matches
the target kernel. You may optionally set `MEMFLOW_LINUX_KERNEL_HINT` to the
physical kernel base as a hexadecimal value. To fail fast when defs-backed
discovery does not match, set `MEMFLOW_LINUX_GENERIC_FALLBACK=off`.

```bash
MEMFLOW_LINUX_PROFILE=/path/to/vmlinux.toml \
cargo run --example ps_linux -- :device=FPGA
```

*/
use std::env::{args, var};

use log::info;

use memflow::prelude::v1::*;
use memflow_linux::LinuxKernel;

fn main() {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let connector_args = if let Some(arg) = args().nth(1) {
        arg.parse()
    } else {
        ":device=FPGA".parse()
    }
    .expect("unable to parse command line arguments");

    let profile = var("MEMFLOW_LINUX_PROFILE")
        .expect("MEMFLOW_LINUX_PROFILE must point to a generated Linux defs TOML");

    let kernel_hint = var("MEMFLOW_LINUX_KERNEL_HINT")
        .ok()
        .map(|value| {
            value
                .trim_start_matches("0x")
                .trim_start_matches("0X")
                .to_owned()
        })
        .map(|value| {
            u64::from_str_radix(&value, 16)
                .map(Address::from)
                .expect("MEMFLOW_LINUX_KERNEL_HINT must be a hexadecimal physical address")
        });

    let connector = memflow_pcileech::create_connector(&connector_args)
        .expect("unable to create pcileech connector");

    let mut builder = LinuxKernel::builder(connector).profile(profile);
    if let Some(kernel_hint) = kernel_hint {
        builder = builder.kernel_hint(kernel_hint);
    }

    let mut os = builder
        .build_default_caches()
        .build()
        .expect("unable to create linux instance with pcileech connector");

    let process_list = os.process_info_list().expect("unable to read process list");

    info!(
        "{:>5} {:>10} {:>10} {:<}",
        "PID", "SYS ARCH", "PROC ARCH", "NAME"
    );

    for p in process_list {
        info!(
            "{:>5} {:^10} {:^10} {}",
            p.pid, p.sys_arch, p.proc_arch, p.name
        );
    }
}
