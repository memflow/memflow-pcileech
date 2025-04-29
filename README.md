# memflow-pcileech

This connector implements the [LeechCore](https://github.com/ufrisk/LeechCore) interface of pcileech for memflow.

More information about pcileech can be found under https://github.com/ufrisk/pcileech.


## Compilation

First make sure that the `leechcore` submodule is checked out:
```
git submodule update --init
```

Install the following build tools:
- clang (only required when selecting feature `bindgen`)
- gcc (only required on linux)
- libusb-1.0 (only required on linux)

If you want to use `bindgen` make sure that libclang can be found by either adding it to your `PATH` or via the `LIBCLANG_PATH` environment variable.

The simplest way to install clang on Windows is by using choco:
```
choco install llvm
```

On Windows you additionally need to supply the proprietary `FTD3XX.dll`. It can be downloaded from the [FTDI Website](https://www.ftdichip.com/Drivers/D3XX.htm) in the `Application Library (DLL)` column.

On Linux you need to check-out and compile the `leechcore_ft601_driver_linux` project from the [LeechCore-Plugins](https://github.com/ufrisk/LeechCore-plugins) repository. On Linux the `leechcore_ft601_driver_linux.so` file currently has to be placed in `/usr/` or `/usr/lib`. Alternatively `LD_LIBRARY_PATH` can be set to the containing path. Check the [dlopen](https://man7.org/linux/man-pages/man3/dlopen.3.html) documentation for all possible import paths.

More information about these requirements can be found in the [LeechCore-Plugins](https://github.com/ufrisk/LeechCore-plugins) repository.

### Running the example

To run the example simply execute:

```
cargo run --example read_phys --release -- FPGA
```

On Linux the example binary will be ran with `sudo -E` to elevate privileges.

Since the invoked binary is placed in the `target/release/examples` or `/target/debug/examples` folder the `leechcore_ft601_driver_linux.so` has to be placed in the corresponding folder.
On Windows the `FTD3XX.dll` has to be placed in the corresponding examples folder.

Alternatively you can also run memflow examples by running them directly from the [memflow](https://github.com/memflow/memflow) repository directory:
```
cargo run --example process_list --release -- --connector pcileech::device=FPGA --os win32
```

### Installing the library

The `./install.sh` script will just compile and install the plugin.
The connector will be installed to `~/.local/lib/memflow` by default.
Additionally the `--system` flag can be specified which will install the connector in `/usr/lib/memflow` as well.

Remarks: The `install.sh` script does currently not place the `leechcore_ft601_driver_linux.so` / `FTD3XX.dll` in the corresponding folders. Please make sure to provide it manually.

### Building the stand-alone connector for dynamic loading

To compile a dynamic library for use with the connector inventory use the following command:
```
cargo build --release
```

If you want to manually execute bindgen at buildtime (e.g. when changing/updating the underlying pcileech repository) then use the following command to build:
```
cargo build --release --features bindgen
```

Note: This requires `clang` (libclang) to be installed on your system.

As mentioned above the `leechcore_ft601_driver_linux.so` or `FTD3XX.dll` have to be placed in the same folder the connector library is placed in.

### Using the library in a rust project

To use the plugin in a rust project just include it in your Cargo.toml

```toml
memflow-pcileech = { git = "https://github.com/memflow/memflow-pcileech", branch = "main" }
```

After adding the dependency to your Cargo.toml you can easily create a new Connector instance and pass it some arguments from the command line:

```rust
let connector_args = if let Some(arg) = args().nth(1) {
    arg.parse()
} else {
    ":device=FPGA".parse()
}
.expect("unable to parse command line arguments");

let mut conn = memflow_pcileech::create_connector(&connector_args)
    .expect("unable to initialize memflow_pcileech");
```

## Arguments

The following arguments can be used when loading the connector:

- `device` - The name of the pcileech device to open (e.g. `FPGA`) (default argument, required)
- `remote` - The remote connection string of the pcileech (e.g. `rpc://insecure:computername.local`) (optional)
- `memmap` - A file that contains a custom memory map in TOML format (optional)
- `auto-clear` - Enables auto-clear of status registers in LeechCore (Auto-clear is only available for bitstreams 4.7 and newer.)

Passing arguments which use the `:` character to pcileech itself requires quotes to escape them. here is an example of using the "driver" mode on pcileech as well as using a memory map file: `:device="fpga://driver=1":memmap="memmap.toml"`. Pcileech takes device arguments by appending `://` to the device name, followed by comma-separated device arguments.

The memory map file must contain a mapping table in the following format:

```toml
[[range]]
base=0x1000
length=0x1000

[[range]]
base=0x2000
length=0x1000
real_base=0x3000
```

The `real_base` parameter is optional. If it is not set there will be no re-mapping.

On Windows systems the memory map can be obtained from the Registry under the following Key:
```
HKEY_LOCAL_MACHINE\\HARDWARE\\RESOURCEMAP\\System Resources\\Physical Memory\\.Translated
```

In case no memory mappings are provided by the user the connector will use the memory mappings found by the os integration (e.g. win32).

## Troubleshooting

Q: The plugin is not detected/found by memflow

A: Make sure to compile the plugin with the correct flags. See the [usage section](#using-the-library-in-a-rust-project) for more information.

## License

Licensed under GPL-3.0 License, see [LICENSE](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, shall be licensed as above, without any additional terms or conditions.
