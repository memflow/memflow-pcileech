mod fpga;
mod ft60x;

use memflow_core::connector::ConnectorArgs;
use memflow_core::*;
use memflow_derive::connector;

#[allow(unused)]
pub struct PciLeech {
    device: fpga::Device,
}

impl PciLeech {
    pub fn new() -> Result<Self> {
        let mut device = fpga::Device::new()?;
        // TODO: validate version

        //device.restart().ok();

        // device = fpga::Device::new()?;

        // TODO: validate device id
        device.get_devid()?;

        // https://github.com/ufrisk/LeechCore/blob/master/leechcore/device_fpga.c#L2133

        Ok(Self { device })
    }
}

impl PhysicalMemory for PciLeech {
    fn phys_read_raw_list(&mut self, _data: &mut [PhysicalReadData]) -> Result<()> {
        Err(Error::Connector(
            "memflow_pcileech::phys_read_iter not implemented",
        ))
    }

    fn phys_write_raw_list(&mut self, _data: &[PhysicalWriteData]) -> Result<()> {
        Err(Error::Connector(
            "memflow_pcileech::phys_write_iter not implemented",
        ))
    }
}

// TODO: handle args properly
/// Creates a new Pcileech Connector instance.
#[connector(name = "pcileech")]
pub fn create_connector(_args: &ConnectorArgs) -> Result<PciLeech> {
    PciLeech::new()
}
