use log::Level;

use memflow::connector::ConnectorArgs;
use memflow_pcileech::{create_connector, PcieGen};

fn main() {
    simple_logger::init_with_level(Level::Trace).unwrap();
    let mut conn = create_connector(&ConnectorArgs::new()).unwrap();
    conn.set_pcie_gen(PcieGen::Gen2).unwrap();

    // TODO: put this + more in a conn print trait ->
    println!(
        "pcie device opened with link width {} and pcie gen {:?}",
        conn.pcie_link_width(),
        conn.pcie_gen()
    );

    conn.test_read().unwrap();
}
