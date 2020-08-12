use log::Level;

use memflow_core::connector::ConnectorArgs;
use memflow_pcileech::{create_connector, PcieGen};

fn main() {
    simple_logger::init_with_level(Level::Trace).unwrap();
    let mut conn = create_connector(&ConnectorArgs::new()).unwrap();
    conn.set_pcie_gen(PcieGen::Gen2).unwrap();
}
