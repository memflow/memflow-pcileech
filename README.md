# This repository is currently work-in-progress and might not fully work.

# memflow-pcileech

This connector implements a rust-native implementation of the pcileech interface.

More information about pcileech can be found under https://github.com/ufrisk/pcileech.


## Compilation

First make sure that the `leechcore` submodule is checked out:
```
git submodule init
git submodule sync
git submodule update
```

Install the following build tools:
- gcc
- clang
- libusb-1.0 (only required on linux)

On Windows you additionally need to supply the proprietary FTD3XX.dll.

On Linux you need to check-out and compile the `leechcore_ft601_driver_linux` project from the [LeechCore-Plugins](https://github.com/ufrisk/LeechCore-plugins) repository.

More information about these requirements can be found in the [LeechCore-Plugins](https://github.com/ufrisk/LeechCore-plugins) repository.

### Running the example

To run the example simply execute:

```cargo run --example read_phys --release -- FPGA```

On Linux the example binary will be ran with `sudo -E` to elevate privileges.

Since the invoked binary is placed in the `target/release/examples` or `/target/debug/examples` folder the `leechcore_ft601_driver_linux.so` has to be placed in the corresponding folder.
On Windows the `FTD3XX.dll` has to be placed in the corresponding examples folder as well.

### Installing the library

The `./install.sh` script will just compile and install the plugin.
The connector will be installed to `~/.local/lib/memflow` by default.
Additionally the `--system` flag can be specified which will install the connector in `/usr/lib/memflow` as well.

Remarks: The `install.sh` script does currently not place the `leechcore_ft601_driver_linux.so` / `FTD3XX.dll` in the corresponding folders. Please make sure to provide it manually.

### Building the stand-alone connector for dynamic loading

The stand-alone connector of this library is feature-gated behind the `inventory` feature.
To compile a dynamic library for use with the connector inventory use the following command:

```cargo build --release --all-features```

As mentioned above the `leechcore_ft601_driver_linux.so` or `FTD3XX.dll` have to be placed in the same folder the connector library is placed in.

### Using the library in a rust project

To use the plugin in a rust project just include it in your Cargo.toml

```
memflow-pcileech = { git = "https://github.com/memflow/memflow-pcileech", branch = "master" }
```

Make sure to _NOT_ enable the `plugin` feature when importing multiple
connectors in a rust project without using the memflow plugin inventory.
This might cause duplicated exports being generated in your project.

After adding the dependency to your Cargo.toml you can easily create a new Connector instance and pass it some args:

```rust
let args: Vec<String> = env::args().collect();
let conn_args = if args.len() > 1 {
    ConnectorArgs::parse(&args[1]).expect("unable to parse arguments")
} else {
    ConnectorArgs::new()
};

let mut conn = memflow_pcileech::create_connector(&conn_args)
    .expect("unable to initialize memflow_pcileech");
```

## Arguments

The following arguments can be used when loading the connector:

- `device` - the name of the pcileech device to open (e.g. FPGA) (default argument, required)
- `memmap` - a file that contains a custom memory map in TOML format (optional)

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

## License

Licensed under GPL-3.0 License, see [LICENSE](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, shall be licensed as above, without any additional terms or conditions.
