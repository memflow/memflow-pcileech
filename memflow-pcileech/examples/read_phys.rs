/*!
This example shows how to use the pcileech connector to read physical_memory
from a target machine. It also evaluates the number of read cycles per second
and prints them to stdout.
*/
use std::env::args;
use std::time::Instant;

use log::info;

use memflow::prelude::v1::*;

fn main() {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let connector_args = if let Some(arg) = args().nth(1) {
        arg.parse()
    } else {
        ":device=FPGA".parse()
    }
    .expect("unable to parse command line arguments");

    let mut connector = memflow_pcileech::create_connector(&connector_args)
        .expect("unable to create pcileech connector");

    let metadata = connector.metadata();
    info!("Received metadata: {:?}", metadata);

    let mut mem = vec![0; 8];
    connector
        .phys_view()
        .read_raw_into(Address::from(0x1000), &mut mem)
        .expect("unable to read physical memory");
    info!("Received memory: {:?}", mem);

    let start = Instant::now();
    let mut counter = 0;
    loop {
        let mut buf = vec![0; 0x1000];
        connector
            .phys_view()
            .read_raw_into(Address::from(0x1000), &mut buf)
            .expect("unable to read physical memory");

        counter += 1;
        if (counter % 10000000) == 0 {
            let elapsed = start.elapsed().as_millis() as f64;
            if elapsed > 0.0 {
                info!("{} reads/sec", (f64::from(counter)) / elapsed * 1000.0);
                info!("{} ms/read", elapsed / (f64::from(counter)));
            }
        }
    }
}
