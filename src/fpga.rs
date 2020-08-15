// TODO: unpub?
pub mod tlps;
use tlps::*;

use crate::ft60x::*;

use core::mem::MaybeUninit;
use core::time::Duration;

use log::{info, trace, warn};

use memflow_core::{
    error::{Error, Result},
    size,
};

use bitfield::bitfield;
use dataview::Pod;
use pretty_hex::*;

pub const FPGA_CONFIG_CORE: u16 = 0x0003;
pub const FPGA_CONFIG_PCIE: u16 = 0x0001;
pub const FPGA_CONFIG_SPACE_READONLY: u16 = 0x0000;
pub const FPGA_CONFIG_SPACE_READWRITE: u16 = 0x8000;

// TODO: remove unused
#[allow(unused)]
pub struct PhyConfig {
    magic: u8,           // 8 bit
    tp_cfg: u8,          // 4 bit
    tp: u8,              // 4 bit
    pub wr: PhyConfigWr, // 16 bits
    pub rd: PhyConfigRd, // 32 bits
}

bitfield! {
    pub struct PhyConfigWr(u16);
    impl Debug;
    pub pl_directed_link_auton, set_pl_directed_link_auton: 0;
    pub pl_directed_link_change, set_pl_directed_link_change: 2, 1;
    pub pl_directed_link_speed, set_pl_directed_link_speed: 3;
    pub pl_directed_link_width, set_pl_directed_link_width: 5, 4;
    pub pl_upstream_prefer_deemph, _: 6;
    pub pl_transmit_hot_rst, set_pl_transmit_hot_rst: 7;
    pub pl_downstream_deemph_source, _: 8;
    //_, _: 16, 9;
}
const _: [(); core::mem::size_of::<PhyConfigWr>()] = [(); 2];

bitfield! {
    pub struct PhyConfigRd(u32);
    impl Debug;
    pub pl_ltssm_state, _: 5, 0;
    pub pl_rx_pm_state, _: 7, 6;
    pub pl_tx_pm_state, _: 10, 8;
    pub pl_initial_link_width, _: 13, 11;
    pub pl_lane_reversal_mode, _: 15, 14;
    pub pl_sel_lnk_width, _: 17, 16;
    pub pl_phy_lnk_up, _: 18;
    pub pl_link_gen2_cap, _: 19;
    pub pl_link_partner_gen2_supported, _: 20;
    pub pl_link_upcfg_cap, _: 21;
    pub pl_sel_lnk_rate, _: 22;
    pub pl_directed_change_done, _: 23;
    pub pl_received_hot_rst, _: 24;
    //_, _: 31, 25;
}
const _: [(); core::mem::size_of::<PhyConfigRd>()] = [(); 4];

pub struct Device {
    ft60: FT60x,
}

impl Device {
    pub fn new() -> Result<Self> {
        let mut ft60 = FT60x::new()?;
        ft60.abort_pipe(0x02)?;
        ft60.abort_pipe(0x82)?;

        ft60.set_suspend_timeout(Duration::new(0, 0))?;

        // check chip configuration
        let mut conf = ft60.config()?;
        info!(
            "ft60x config: fifo_mode={} channel_config={} optional_feature={}",
            conf.fifo_mode, conf.channel_config, conf.optional_feature_support
        );

        if conf.fifo_mode != FifoMode::Mode245 as i8
            || conf.channel_config != ChannelConfig::Config1 as i8
            || conf.optional_feature_support != OptionalFeatureSupport::DisableAll as i16
        {
            warn!("bad ft60x config, reconfiguring chip");

            conf.fifo_mode = FifoMode::Mode245 as i8;
            conf.channel_config = ChannelConfig::Config1 as i8;
            conf.optional_feature_support = OptionalFeatureSupport::DisableAll as i16;

            ft60.set_config(&conf)?;
        } else {
            info!("ft60x config is valid");
        }

        Ok(Self { ft60 })
    }

    /// Restarts the PCIE device
    fn reset_core(&mut self) -> Result<()> {
        let reset_code: [u8; 2] = [0x00, 0x80];
        self.write_config_ex_raw(
            0x0002,
            reset_code,
            reset_code,
            FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READWRITE,
        )?;
        std::thread::sleep(Duration::from_millis(1000));
        self.ft60 = FT60x::new()?;
        Ok(())
    }

    /// Clears the pipe before starting any new read attempts
    pub fn clear_pipe(&mut self) -> Result<()> {
        let dummy = [
            // dword->qword resynch v4.5+
            0x66, 0x66, 0x55, 0x55, 0x66, 0x66, 0x55, 0x55, 0x66, 0x66, 0x55, 0x55, 0x66, 0x66,
            0x55, 0x55, // cmd msg: FPGA bitstream version (major.minor)    v4
            0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x13, 0x77,
            // cmd msg: FPGA bitstream version (major)          v3
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x77,
        ];

        //self.reset_core()?;

        self.ft60.write_pipe(&dummy)?;

        let mut buf = vec![0u8; size::mb(1)];
        if self.ft60.read_pipe(&mut buf[..0x1000])? >= 0x1000 {
            if self.ft60.read_pipe(&mut buf)? == buf.len() {
                self.reset_core()?;
            }
        }

        Ok(())
    }

    pub fn read_version(&mut self) -> Result<(u8, u8)> {
        let version_major =
            self.read_config::<u8>(0x0008, FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY)?;
        info!("version_major = {}", version_major);

        let version_minor =
            self.read_config::<u8>(0x0009, FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY)?;
        info!("version_minor={}", version_minor);

        Ok((version_major, version_minor))
    }

    pub fn read_devid(&mut self) -> Result<(u8, u16)> {
        let fpga_id =
            self.read_config::<u8>(0x000a, FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY)?;
        info!("fpga_id={}", fpga_id);

        let mut device_id = self
            .read_config::<u16>(0x0008, FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READONLY)
            .unwrap_or_default();
        if device_id == 0 {
            info!("pci device_id is unset. checking pcie magic.");

            let magic_pcie = self
                .read_config::<u16>(0x0000, FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READWRITE)
                .unwrap_or_default();
            info!("magic_pcie={:?}", magic_pcie);

            if magic_pcie == 0x6745 {
                warn!("failed to get device_id. trying to recover via hot reset");
                self.hot_reset().ok();
                device_id = self
                    .read_config::<u16>(0x0008, FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READONLY)
                    .unwrap_or_default();
            }
        }
        let device_id_le = device_id.to_be();
        info!("device_id={:?}", device_id_le);

        // swap device_id bytes only on LE systems
        Ok((fpga_id, device_id_le))
    }

    pub fn write_inactivity_timer(&mut self) -> Result<()> {
        let inactivity_timer = 0x000186a0u32; // set inactivity timer to 1ms (0x0186a0 * 100MHz) [only later activated on UDP bitstreams]
        self.write_config(
            0x0008,
            inactivity_timer,
            FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READWRITE,
        )
    }

    pub fn hot_reset(&mut self) -> Result<()> {
        warn!("hot resetting the fpga");

        let mut wr = self.get_phy_wr()?;
        wr.set_pl_transmit_hot_rst(true);
        self.set_phy_wr(&wr)?;

        std::thread::sleep(Duration::from_millis(250)); // TODO: poll pl_ltssm_state + timeout with failure

        wr.set_pl_transmit_hot_rst(false);
        self.set_phy_wr(&wr)?;
        Ok(())
    }

    pub fn get_phy(&mut self) -> Result<(PhyConfigWr, PhyConfigRd)> {
        Ok((self.get_phy_wr()?, self.get_phy_rd()?))
    }

    pub fn get_phy_wr(&mut self) -> Result<PhyConfigWr> {
        let wr_raw =
            self.read_config::<u16>(0x0016, FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READWRITE)?;
        Ok(PhyConfigWr { 0: wr_raw })
    }

    pub fn set_phy_wr(&mut self, wr: &PhyConfigWr) -> Result<()> {
        self.write_config(0x0016, wr.0, FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READWRITE)
    }

    pub fn get_phy_rd(&mut self) -> Result<PhyConfigRd> {
        let rd_raw =
            self.read_config::<u32>(0x000a, FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READONLY)?;
        Ok(PhyConfigRd { 0: rd_raw })
    }

    /// Prints out all internal registers of the FPGA to `info!()`
    /// In detail this will request the core/pcie readonly and read/write registers
    /// and print them out via `info!()`. This is usually useful when debugging any
    /// issues with the hardware.
    pub fn print_registers(&mut self) -> Result<()> {
        info!(
            "core read-only registers: {:?}",
            self.get_register(FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY)?
                .hex_dump()
        );
        info!(
            "core read-write registers: {:?}",
            self.get_register(FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READWRITE)?
                .hex_dump()
        );
        info!(
            "pcie read-only registers: {:?}",
            self.get_register(FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READONLY)?
                .hex_dump()
        );
        info!(
            "core read-write registers: {:?}",
            self.get_register(FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READWRITE)?
                .hex_dump()
        );
        Ok(())
    }

    fn get_register(&mut self, flags: u16) -> Result<Vec<u8>> {
        let size = self.read_config::<u16>(0x0004, flags)?;
        info!(
            "reading fpga device config register {:x} with a length of {:x} bytes.",
            flags, size
        );
        let mut buf = vec![0u8; size as usize];
        self.read_config_into_raw(0x0000, &mut buf[..], flags)?;
        Ok(buf)
    }

    // TODO: implement more config dump options
    fn get_pcie_drp(&mut self) {
        let read_enable = [0x10, 0x00, 0x10, 0x00, 0x80, 0x02, 0x23, 0x77];
        let read_address = [0x00, 0x00, 0xff, 0xff, 0x80, 0x1c, 0x23, 0x77];
        let result_meta = [0x00, 0x00, 0x00, 0x00, 0x80, 0x1c, 0x13, 0x77];
        let result_data = [0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x13, 0x77];

        // create read request
        for addr in (0..0x100).step_by(32) {
            for dw in (0..32).step_by(2) {}
        }

        // interpret result
    }

    // TODO: send_tlp_ ...
    /*pub fn send_tlp32(&mut self, tlp: u32, keep_alive: bool, flush: bool) -> Result<()> {
        self.send_tlp_raw(tlp.as_bytes(), keep_alive, flush)
    }

    pub fn send_tlp64(&mut self, tlp: u64, keep_alive: bool, flush: bool) -> Result<()> {
        self.send_tlp_raw(tlp.as_bytes(), keep_alive, flush)
    }*/

    // TODO: this is duplicated code (see config_parse_response)
    pub fn recv_tlps_64(&mut self, bytes: u32 /* maybe u16? */) -> Result<()> {
        let mut respbuf = vec![0u8; 0x1000]; // TEST
        self.ft60.read_pipe(&mut respbuf)?;

        let view = respbuf.as_data_view();
        let mut skip = 0;
        for i in (0..respbuf.len()).step_by(32) {
            if i + skip >= respbuf.len() {
                break;
            }

            while view.copy::<u32>(i + skip) == 0x55556666 {
                trace!("ftdi workaround detected, skipping 4 bytes");
                skip += 4;
                if i + skip + 32 > respbuf.len() {
                    return Err(Error::Connector("out of range config read"));
                }
            }

            let mut status = view.copy::<u32>(i + skip);
            if status & 0xf0000000 != 0xe0000000 {
                trace!("invalid status reply, skipping");
                continue;
            }

            trace!("parsing data buffer");
            for j in 0..7 {
                if (status & 0x03) == 0 {
                    println!("pcie tlp received :)");
                }
                if (status & 0x07) == 4 {
                    println!("pcie tlp LAST received :)");
                }
            }
        }

        Ok(())
    }

    pub fn send_tlps_64(&mut self, tlps: &[TlpReadWrite64], keep_alive: bool) -> Result<()> {
        let bytes = tlps
            .iter()
            .map(|t| t.as_bytes())
            .collect::<Vec<_>>()
            .concat();

        let bytes32 =
            unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u32, bytes.len() / 4) };

        self.send_tlps_raw(&bytes32, keep_alive)
    }

    /// Send a TLP to the fpga
    fn send_tlps_raw(&mut self, tlps: &[u32], keep_alive: bool) -> Result<()> {
        if tlps.len() > 4 + 32 {
            return Err(Error::Connector("tlp buffer is too large"));
        }
        /*
        if(cbTlp && (ctx->txbuf.cb + (cbTlp << 1) + (fFlush ? 8 : 0) >= ctx->perf.MAX_SIZE_TX)) {
            if(!DeviceFPGA_TxTlp(ctxLC, ctx, NULL, 0, FALSE, TRUE)) { return FALSE; }
        }
        */

        // TLP_Print

        // create transmit buffer
        let mut buf = Vec::new();
        for tlp in tlps.iter() {
            buf.push(tlp.to_be());
            buf.push(0x77000000); // TX TLP
        }

        // TODO: remove this pop in the algorithm
        if !tlps.is_empty() {
            buf.pop();
            buf.push(0x77040000); // TX TLP VALID LAST
        }

        if keep_alive {
            buf.push(0xffeeddcc);
            buf.push(0x77020000);
        }

        // currently we just flush out every tlp transmission immediately
        // and not buffer them internally.
        let byte_buf: &[u8] = unsafe { std::mem::transmute(buf.as_slice()) };

        // TODO: handle error codes?
        self.ft60.write_pipe(byte_buf)?;
        //        if status == 0x20 {
        // failure
        //      }

        Ok(())
    }

    #[allow(clippy::uninit_assumed_init)]
    fn read_config<T: Pod>(&mut self, addr: u16, flags: u16) -> Result<T> {
        let mut obj: T = unsafe { MaybeUninit::uninit().assume_init() };
        self.read_config_into_raw(addr, obj.as_bytes_mut(), flags)?;
        Ok(obj)
    }

    fn read_config_build_request(addr: u16, bytes: u16, flags: u16) -> Vec<u8> {
        let mut res = Vec::new();
        for a in (addr..addr + bytes).step_by(2) {
            let mut req = [0u8; 8];
            req[4] = ((a | (flags & 0x8000)) >> 8) as u8;
            req[5] = (a & 0xff) as u8;
            req[6] = (0x10 | (flags & 0x03)) as u8;
            req[7] = 0x77;
            res.extend_from_slice(&req);
        }
        res
    }

    fn read_config_parse_response(
        addr: u16,
        respbuf: &[u8],
        outbuf: &mut [u8],
        flags: u16,
    ) -> Result<()> {
        let view = respbuf.as_data_view();
        let mut skip = 0;
        for i in (0..respbuf.len()).step_by(32) {
            if i + skip >= respbuf.len() {
                break;
            }

            while view.copy::<u32>(i + skip) == 0x55556666 {
                //trace!("ftdi workaround detected, skipping 4 bytes");
                skip += 4;
                if i + skip + 32 > respbuf.len() {
                    return Err(Error::Connector("out of range config read"));
                }
            }

            let mut status = view.copy::<u32>(i + skip);
            if status & 0xf0000000 != 0xe0000000 {
                trace!("invalid status reply, skipping");
            }

            trace!("parsing data buffer");
            for j in 0..7 {
                let status_flag = (status & 0x0f) == (flags & 0x03) as u32;
                status >>= 4; // move to next status
                if !status_flag {
                    //trace!("status source flag does not match source");
                    continue;
                }

                let data = view.copy::<u32>(i + skip + 4 + j * 4);
                let mut a = (data as u16).to_be(); // only enforce a byteswap if we are on le
                a -= (flags & 0x8000) + addr;
                if a >= outbuf.len() as u16 {
                    trace!("address data out of range, skipping");
                    continue;
                }

                if a == outbuf.len() as u16 - 1 {
                    outbuf[a as usize] = ((data >> 16) & 0xff) as u8;
                } else {
                    let b = (((data >> 16) & 0xffff) as u16).to_le_bytes();
                    outbuf[a as usize] = b[0];
                    outbuf[a as usize + 1] = b[1];
                }
            }
        }
        Ok(())
    }

    fn read_config_into_raw(&mut self, addr: u16, buf: &mut [u8], flags: u16) -> Result<()> {
        if buf.is_empty() || buf.len() > size::kb(4) || addr > size::kb(4) as u16 {
            return Err(Error::Connector("invalid config address requested"));
        }

        let req = Self::read_config_build_request(addr, buf.len() as u16, flags);

        self.ft60.write_pipe(&req)?;

        let mut readbuf = [0u8; size::kb(128)];
        let bytes = self.ft60.read_pipe(&mut readbuf)?;

        Self::read_config_parse_response(addr, &readbuf[..bytes], buf, flags)
    }

    fn write_config<T: Pod>(&mut self, addr: u16, obj: T, flags: u16) -> Result<()> {
        self.write_config_raw(addr, obj.as_bytes(), flags)
    }

    fn write_config_build_request(addr: u16, buf: &[u8], flags: u16) -> Result<Vec<u8>> {
        if buf.is_empty() || buf.len() > 0x200 || addr > size::kb(4) as u16 {
            return Err(Error::Connector("invalid config address to write"));
        }

        let mut outbuf = [0u8; 0x800];
        let mut ptr = 0;
        for i in (0..buf.len()).step_by(2) {
            let a = (addr + i as u16) | (flags & 0x8000);
            outbuf[ptr] = buf[i as usize]; // byte_value_addr
            outbuf[ptr + 1] = if buf.len() == i + 1 {
                0
            } else {
                buf[i as usize + 1]
            }; // byte_value_addr + 1
            outbuf[ptr + 2] = 0xFF; // byte_mask_addr
            outbuf[ptr + 3] = if buf.len() == i + 1 { 0 } else { 0xFF }; // byte_mask_addr + 1
            outbuf[ptr + 4] = (a >> 8) as u8; // addr_high = bit[6:0], write_regbank = bit[7]
            outbuf[ptr + 5] = (a & 0xFF) as u8; // addr_low
            outbuf[ptr + 6] = (0x20 | (flags & 0x03)) as u8; // target = bit[0:1], read = bit[4], write = bit[5]
            outbuf[ptr + 7] = 0x77; // MAGIC 0x77
            ptr += 8;
        }

        Ok(outbuf[..ptr].to_vec())
    }

    fn write_config_raw(&mut self, addr: u16, buf: &[u8], flags: u16) -> Result<()> {
        let outbuf = Self::write_config_build_request(addr, buf, flags)?;
        self.ft60.write_pipe(&outbuf)
    }

    fn write_config_ex_build_request(
        addr: u16,
        buf: [u8; 2],
        mask: [u8; 2],
        flags: u16,
    ) -> Result<[u8; 8]> {
        let a = (addr as u16) | (flags & 0x8000);
        Ok([
            buf[0],
            buf[1],
            mask[0],
            mask[1],
            (a >> 8) as u8,                // addr_high
            (a & 0xFF) as u8,              // addr_low
            (0x20 | (flags & 0x03)) as u8, // target = bit[0:1], read = bit[4], write = bit[5]
            0x77,                          // MAGIC 0x77
        ])
    }

    fn write_config_ex_raw(
        &mut self,
        addr: u16,
        buf: [u8; 2],
        mask: [u8; 2],
        flags: u16,
    ) -> Result<()> {
        let outbuf = Self::write_config_ex_build_request(addr, buf, mask, flags)?;
        self.ft60.write_pipe(&outbuf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_struct_sizes() {
        assert_eq!(size_of::<PhyConfigWr>(), 2);
        assert_eq!(size_of::<PhyConfigRd>(), 4);
    }

    #[test]
    fn test_config_read_build_request() {
        assert_eq!(
            Device::read_config_build_request(
                0x0008,
                1,
                FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY
            ),
            [0x0, 0x0, 0x0, 0x0, 0x0, 0x8, 0x13, 0x77]
        );
        assert_eq!(
            Device::read_config_build_request(
                0x0009,
                1,
                FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY
            ),
            [0x0, 0x0, 0x0, 0x0, 0x0, 0x9, 0x13, 0x77]
        );

        assert_eq!(
            Device::read_config_build_request(
                0x0008,
                2,
                FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READONLY
            ),
            [0x0, 0x0, 0x0, 0x0, 0x0, 0x8, 0x11, 0x77]
        );
        assert_eq!(
            Device::read_config_build_request(
                0x0000,
                2,
                FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READWRITE
            ),
            [0x0, 0x0, 0x0, 0x0, 0x80, 0x0, 0x11, 0x77]
        );

        assert_eq!(
            Device::read_config_build_request(
                0x0016,
                2,
                FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READWRITE
            ),
            [0x0, 0x0, 0x0, 0x0, 0x80, 0x16, 0x11, 0x77]
        );
        assert_eq!(
            Device::read_config_build_request(
                0x000a,
                4,
                FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READONLY
            ),
            [0x0, 0x0, 0x0, 0x0, 0x0, 0xA, 0x11, 0x77, 0x0, 0x0, 0x0, 0x0, 0x0, 0xC, 0x11, 0x77]
        );
    }

    #[test]
    fn test_config_parse_version_major() {
        let mut version_major = 0u8;
        Device::read_config_parse_response(
            0x0008,
            &[
                102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102,
                85, 85, 243, 255, 255, 239, 0, 8, 4, 2, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ],
            version_major.as_bytes_mut(),
            FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY,
        )
        .unwrap();
        assert_eq!(version_major, 4);
    }

    #[test]
    fn test_config_parse_version_minor() {
        let mut version_minor = 0u8;
        Device::read_config_parse_response(
            0x0009,
            &[
                102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102,
                85, 85, 243, 255, 255, 239, 0, 9, 2, 1, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ],
            version_minor.as_bytes_mut(),
            FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY,
        )
        .unwrap();
        assert_eq!(version_minor, 2);
    }

    #[test]
    fn test_config_parse_fpga_id() {
        let mut fpga_id = 0u8;
        Device::read_config_parse_response(
            0x000a,
            &[
                102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102,
                85, 85, 243, 255, 255, 239, 0, 10, 1, 0, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ],
            fpga_id.as_bytes_mut(),
            FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY,
        )
        .unwrap();
        assert_eq!(fpga_id, 1);
    }

    #[test]
    fn test_config_parse_device_id() {
        let mut fpga_id = 0u8;
        Device::read_config_parse_response(
            0x000a,
            &[
                102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102,
                85, 85, 243, 255, 255, 239, 0, 10, 1, 0, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ],
            fpga_id.as_bytes_mut(),
            FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READONLY,
        )
        .unwrap();
        assert_eq!(fpga_id, 1);
    }

    #[test]
    fn test_config_parse_phy_wr() {
        let mut wr = 0u16;
        Device::read_config_parse_response(
            0x0016,
            &[
                102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102,
                85, 85, 241, 255, 255, 239, 128, 22, 72, 0, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ],
            wr.as_bytes_mut(),
            FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READWRITE,
        )
        .unwrap();
        assert_eq!(wr, 0x48);
    }

    #[test]
    fn test_config_parse_phy_rd() {
        let mut rd = 0u32;
        Device::read_config_parse_response(
            0x000a,
            &[
                102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102, 85, 85, 102, 102,
                85, 85, 17, 255, 255, 239, 0, 10, 25, 8, 0, 12, 28, 0, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ],
            rd.as_bytes_mut(),
            FPGA_CONFIG_PCIE | FPGA_CONFIG_SPACE_READONLY,
        )
        .unwrap();
        assert_eq!(rd, 0x1C0819);
    }

    #[test]
    fn write_config_inactivity_timer() {
        let inactivity_timer = 0x000186a0u32; // set inactivity timer to 1ms (0x0186a0 * 100MHz) [only later activated on UDP bitstreams]
        let outbuf = Device::write_config_build_request(
            0x0008,
            inactivity_timer.as_bytes(),
            FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READWRITE,
        )
        .unwrap();
        assert_eq!(
            outbuf,
            [160, 134, 255, 255, 128, 8, 35, 119, 1, 0, 255, 255, 128, 10, 35, 119]
        );
    }

    #[test]
    fn write_config_core_reset() {
        let code: [u8; 2] = [0x00, 0x80];
        let outbuf = Device::write_config_ex_build_request(
            0x0002,
            code,
            code,
            FPGA_CONFIG_CORE | FPGA_CONFIG_SPACE_READWRITE,
        )
        .unwrap();
        assert_eq!(outbuf, [0, 128, 0, 128, 128, 2, 35, 119,]);
    }
}
