mod fpga;
mod ft60x;

use log::{info, warn};

use fpga::{PhyConfigRd, PhyConfigWr};

use memflow_core::connector::ConnectorArgs;
use memflow_core::*;
use memflow_derive::connector;

pub enum PcieGen {
    Gen1 = 0,
    Gen2 = 1,
}

#[allow(unused)]
pub struct PciLeech {
    device: fpga::Device,

    version_major: u8,
    version_minor: u8,
    fpga_id: u8,
    device_id: u16,

    phy_wr: PhyConfigWr,
    phy_rd: PhyConfigRd,
}

impl PciLeech {
    pub fn new() -> Result<Self> {
        let mut device = fpga::Device::new()?;
        device.clear_pipe()?;

        let version = device.read_version()?;
        if version.0 != 4 {
            return Err(Error::Connector("only pcileech 4.x devices are supported"));
        }

        device.write_inactivity_timer()?;

        let device_id = device.read_devid()?;
        if device_id.1 == 0 {
            return Err(Error::Connector("fpga did not find a valid pcie device id"));
        }

        let (wr, rd) = device.get_phy()?;

        device.print_registers().ok();

        Ok(Self {
            device,

            version_major: version.0,
            version_minor: version.1,
            fpga_id: device_id.0,
            device_id: device_id.1,

            phy_wr: wr,
            phy_rd: rd,
        })
    }

    pub fn set_pcie_gen(&mut self, gen: PcieGen) -> Result<()> {
        let gen2 = match gen {
            PcieGen::Gen1 => false,
            PcieGen::Gen2 => true,
        };

        if gen2 == self.phy_rd.pl_sel_lnk_rate() {
            info!("requested pcie gen already set.");
            return Ok(());
        }

        if gen2 && !self.phy_rd.pl_link_gen2_cap() {
            warn!("pcie gen2 is not supported by the fpga configuration");
            return Err(Error::Connector(
                "pcie gen2 is not supported by the fpga configuration",
            ));
        }

        // update config
        self.phy_wr.set_pl_directed_link_auton(true);
        self.phy_wr.set_pl_directed_link_speed(gen2);
        self.phy_wr.set_pl_directed_link_change(2);
        self.device.set_phy_wr(&self.phy_wr)?;

        // poll config update
        for _ in 0..32 {
            if let Ok(rd) = self.device.get_phy_rd() {
                if rd.pl_directed_change_done() {
                    info!("fpga changes successfully applied");
                    self.phy_rd = rd;
                    break;
                }
            }
        }

        // reset config
        self.phy_wr.set_pl_directed_link_auton(false);
        self.phy_wr.set_pl_directed_link_speed(false);
        self.phy_wr.set_pl_directed_link_change(0);
        self.device.set_phy_wr(&self.phy_wr)?;

        // update internal state
        self.phy_wr = self.device.get_phy_wr()?;
        self.phy_rd = self.device.get_phy_rd()?;

        Ok(())
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
