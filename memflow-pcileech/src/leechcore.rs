use ::std::ptr::null_mut;
use std::ffi::c_void;
use std::os::raw::c_char;
use std::sync::{Arc, Mutex};

use log::info;

use memflow::prelude::v1::*;

use leechcore_sys::*;

pub const PAGE_SIZE: usize = 0x1000usize;

pub struct MemReadScatter {
    pub address: umem,
    //pub buffer: CSliceMut<'a, u8>,
    pub buffer: *mut u8,
    pub buffer_len: usize,
}

pub struct MemWriteScatter {
    pub address: umem,
    //pub buffer: CSliceRef<'a, u8>,
    pub buffer: *const u8,
    pub buffer_len: usize,
}

pub trait LeechCore {
    fn volatile(&self) -> bool;
    fn pa_max(&self) -> u64;

    fn read_scatter(&mut self, mems: &[MemReadScatter]) -> Result<()>;
    fn write_scatter(&mut self, mems: &[MemWriteScatter]) -> Result<()>;
}

#[derive(Clone)]
pub struct LeechCoreSys {
    handle: Arc<Mutex<HANDLE>>,
    conf: LC_CONFIG,
}

impl LeechCoreSys {
    pub fn new(
        device: &str,
        remote: Option<&str>,
        with_mem_map: bool,
        auto_clear: bool,
    ) -> Result<Self> {
        // open device
        let mut conf = Self::build_lc_config(device, remote, with_mem_map);
        let err = std::ptr::null_mut::<PLC_CONFIG_ERRORINFO>();
        let handle = unsafe { LcCreateEx(&mut conf, err) };
        if handle.is_null() {
            // TODO: handle version error
            // TODO: handle special case of fUserInputRequest
            return Err(Error(ErrorOrigin::Connector, ErrorKind::Configuration)
                .log_error(format!("unable to create leechcore context: {err:?}")));
        }

        // TODO: allow handling these errors properly
        /*
            typedef struct tdLC_CONFIG_ERRORINFO {
            DWORD dwVersion;                        // must equal LC_CONFIG_ERRORINFO_VERSION
            DWORD cbStruct;
            DWORD _FutureUse[16];
            BOOL fUserInputRequest;
            DWORD cwszUserText;
            WCHAR wszUserText[];
        } LC_CONFIG_ERRORINFO, *PLC_CONFIG_ERRORINFO, **PPLC_CONFIG_ERRORINFO;
        */

        if auto_clear {
            let (mut id, mut version_major, mut version_minor) = (0, 0, 0);
            unsafe {
                LcGetOption(handle, LC_OPT_FPGA_FPGA_ID, &mut id);
                LcGetOption(handle, LC_OPT_FPGA_VERSION_MAJOR, &mut version_major);
                LcGetOption(handle, LC_OPT_FPGA_VERSION_MINOR, &mut version_minor);
            }
            if version_major >= 4 && (version_major >= 5 || version_minor >= 7) {
                // enable auto-clear of status register [master abort].
                info!("Trying to enable status register auto-clear");
                let mut data = [0x10, 0x00, 0x10, 0x00];
                if unsafe {
                    LcCommand(
                        handle,
                        LC_CMD_FPGA_CFGREGPCIE_MARKWR | 0x002,
                        data.len() as u32,
                        data.as_mut_ptr(),
                        null_mut(),
                        null_mut(),
                    )
                } != 0
                {
                    info!("Successfully enabled status register auto-clear");
                } else {
                    return Err(Error(ErrorOrigin::Connector, ErrorKind::Configuration)
                        .log_error("Could not enable status register auto-clear due to outdated bitstream."));
                }
            } else {
                return Err(Error(ErrorOrigin::Connector, ErrorKind::Configuration)
                    .log_error("Could not enable status register auto-clear due to outdated bitstream. Auto-clear is only available for bitstreams 4.7 and newer."));
            }
        }

        Ok(Self {
            handle: Arc::new(Mutex::new(handle)),
            conf,
        })
    }

    fn build_lc_config(device: &str, remote: Option<&str>, with_mem_map: bool) -> LC_CONFIG {
        // TODO: refactor how the static strings are handled
        let cdevice = unsafe { &*(device.as_bytes() as *const [u8] as *const [c_char]) };
        let mut adevice: [c_char; 260] = [0; 260];
        adevice[..device.len().min(260)].copy_from_slice(&cdevice[..device.len().min(260)]);

        // set remote in case user specified the remote flag
        let mut aremote: [c_char; 260] = [0; 260];
        if let Some(remote) = remote {
            let cremote = unsafe { &*(remote.as_bytes() as *const [u8] as *const [c_char]) };
            aremote[..remote.len().min(260)].copy_from_slice(&cremote[..remote.len().min(260)]);
        }

        // set paMax to -1 if mem map is set to disable automatic scanning
        let pa_max = if with_mem_map { u64::MAX } else { 0 };

        LC_CONFIG {
            dwVersion: LC_CONFIG_VERSION,
            dwPrintfVerbosity: LC_CONFIG_PRINTF_ENABLED | LC_CONFIG_PRINTF_V | LC_CONFIG_PRINTF_VV,
            szDevice: adevice,
            szRemote: aremote,
            pfn_printf_opt: None, // TODO: custom info() wrapper
            paMax: pa_max,

            // these are set by leechcore so we dont touch them
            fVolatile: 0,
            fWritable: 0,
            fRemote: 0,
            fRemoteDisableCompress: 0,
            szDeviceName: [0; 260],
        }
    }

    unsafe fn alloc_scatter2(num_pages: u64) -> Result<PPMEM_SCATTER> {
        let mut mems = std::ptr::null_mut::<PMEM_SCATTER>();
        let result = LcAllocScatter2(
            (num_pages * PAGE_SIZE as u64) as u32,
            std::ptr::null_mut(),
            num_pages as u32,
            &mut mems as *mut PPMEM_SCATTER,
        );
        if result != 1 {
            return Err(Error(ErrorOrigin::Connector, ErrorKind::InvalidMemorySize)
                .log_error("unable to allocate scatter buffer"));
        }

        Ok(mems)
    }

    unsafe fn mem_free(mems: PPMEM_SCATTER) {
        LcMemFree(mems as *mut c_void);
    }
}

impl LeechCore for LeechCoreSys {
    fn volatile(&self) -> bool {
        self.conf.fVolatile != 0
    }

    fn pa_max(&self) -> u64 {
        self.conf.paMax
    }

    fn read_scatter(&mut self, mems: &[MemReadScatter]) -> Result<()> {
        let num_pages = mems.len() as u64;

        // allocate read buffers
        let lc_mems = unsafe { Self::alloc_scatter2(num_pages)? };

        // copy over memory definitions
        for (i, mem) in mems.iter().enumerate() {
            let lc_mem = unsafe { *lc_mems.add(i) };
            unsafe { (*lc_mem).qwA = mem.address };
            unsafe { (*lc_mem).__bindgen_anon_1.pb = mem.buffer };
            unsafe { (*lc_mem).cb = mem.buffer_len as u32 };
        }

        // dispatch read
        {
            let handle = self.handle.lock().unwrap();
            unsafe {
                LcReadScatter(*handle, num_pages as u32, lc_mems);
            }
        }

        // free buffers
        unsafe {
            Self::mem_free(lc_mems);
        }

        Ok(())
    }

    fn write_scatter(&mut self, mems: &[MemWriteScatter]) -> Result<()> {
        let num_pages = mems.len() as u64;

        // allocate write buffers
        let lc_mems = unsafe { Self::alloc_scatter2(num_pages)? };

        // copy over memory definitions
        for (i, mem) in mems.iter().enumerate() {
            let lc_mem = unsafe { *lc_mems.add(i) };
            unsafe { (*lc_mem).qwA = mem.address };
            unsafe { (*lc_mem).__bindgen_anon_1.pb = mem.buffer as *mut u8 };
            unsafe { (*lc_mem).cb = mem.buffer_len as u32 };
        }

        // dispatch write
        {
            let handle = self.handle.lock().unwrap();
            unsafe {
                LcWriteScatter(*handle, num_pages as u32, lc_mems);
            }
        }

        // free buffers
        unsafe {
            Self::mem_free(lc_mems);
        }

        Ok(())
    }
}

pub struct LeechCoreMock {}
