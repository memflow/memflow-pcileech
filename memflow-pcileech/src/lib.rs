// https://github.com/ufrisk/pcileech/blob/master/pcileech/device.c

use std::ffi::c_void;
use std::os::raw::c_char;
use std::slice;
use std::sync::{Arc, Mutex};

use log::{error, info, warn};

use memflow::*;
use memflow_derive::connector;

use leechcore_sys::*;

const PAGE_SIZE: u64 = 0x1000u64;

const fn calc_num_pages(start: u64, size: u64) -> u64 {
    ((start & (PAGE_SIZE - 1)) + size + (PAGE_SIZE - 1)) >> 12
}

fn build_lc_config(device: &str) -> LC_CONFIG {
    let cdevice = unsafe { &*(device.as_bytes() as *const [u8] as *const [c_char]) };
    let mut adevice: [c_char; 260] = [0; 260];
    adevice[..device.len().min(260)].copy_from_slice(&cdevice[..device.len().min(260)]);

    let cfg = LC_CONFIG {
        dwVersion: LC_CONFIG_VERSION,
        dwPrintfVerbosity: LC_CONFIG_PRINTF_ENABLED | LC_CONFIG_PRINTF_V | LC_CONFIG_PRINTF_VV,
        szDevice: adevice,
        szRemote: [0; 260],
        pfn_printf_opt: None, // TODO: custom info() wrapper
        paMax: 0,
        fVolatile: 0,
        fWritable: 0,
        fRemote: 0,
        fRemoteDisableCompress: 0,
        szDeviceName: [0; 260],
    };

    // TODO: copy device + remote

    cfg
}

#[derive(Debug)]
pub struct PciLeech {
    handle: Arc<Mutex<HANDLE>>,
    metadata: PhysicalMemoryMetadata,
    mem_map: MemoryMap<(Address, usize)>,
}

unsafe impl Send for PciLeech {}

impl Clone for PciLeech {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            metadata: self.metadata.clone(),
            mem_map: self.mem_map.clone(),
        }
    }
}

// TODO: proper drop + free impl -> LcMemFree(pLcErrorInfo);
impl PciLeech {
    pub fn new(device: &str) -> Result<Self> {
        Self::with_map(device, MemoryMap::new())
    }

    // TODO: load a memory map via the arguments
    pub fn with_map(device: &str, mem_map: MemoryMap<(Address, usize)>) -> Result<Self> {
        // open device
        let mut conf = build_lc_config(device);
        let err = std::ptr::null_mut::<PLC_CONFIG_ERRORINFO>();
        let handle = unsafe { LcCreateEx(&mut conf, err) };
        if handle.is_null() {
            // TODO: handle version error
            // TODO: handle special case of fUserInputRequest
            error!("leechcore error: {:?}", err);
            return Err(Error::Connector("unable to create leechcore context"));
        }

        Ok(Self {
            handle: Arc::new(Mutex::new(handle)),
            metadata: PhysicalMemoryMetadata {
                size: conf.paMax as usize,
                readonly: if conf.fVolatile == 0 { true } else { false },
                // TODO: writable flag
            },
            mem_map,
        })
    }
}

// TODO: handle mem_map
impl PhysicalMemory for PciLeech {
    fn phys_read_raw_list(&mut self, data: &mut [PhysicalReadData]) -> Result<()> {
        let mem_map = &self.mem_map;

        // get total number of pages
        let num_pages = data.iter().fold(0u64, |acc, read| {
            acc + calc_num_pages(read.0.as_u64(), read.1.len() as u64)
        });

        // allocate scatter buffer
        let mut mems = std::ptr::null_mut::<PMEM_SCATTER>();
        let result = unsafe { LcAllocScatter1(num_pages as u32, &mut mems as *mut PPMEM_SCATTER) };
        if result != 1 {
            return Err(Error::Connector("unable to allocate scatter buffer"));
        }

        // prepare mems
        let mut i = 0usize;
        for read in data.iter() {
            let base = read.0.address().as_page_aligned(0x1000).as_u64();
            let num_pages = calc_num_pages(read.0.as_u64(), read.1.len() as u64);
            for p in 0..num_pages {
                let mem = unsafe { *mems.offset(i as isize) };
                unsafe { (*mem).qwA = base + p * 0x1000 };
                i += 1;
            }
        }

        // dispatch read
        {
            let handle = self.handle.lock().unwrap();
            unsafe {
                LcReadScatter(*handle, num_pages as u32, mems);
            }
        }

        // load reads back into data
        i = 0;
        for read in data.iter_mut() {
            let num_pages = calc_num_pages(read.0.as_u64(), read.1.len() as u64);

            // internally lc will allocate a continuous buffer
            let mem = unsafe { *mems.offset(i as isize) };

            let offset = (read.0.as_u64() - unsafe { (*mem).qwA }) as usize;
            //println!("offset={}", offset);

            let page = unsafe { slice::from_raw_parts((*mem).pb, (num_pages * 0x1000) as usize) };
            read.1
                .copy_from_slice(&page[offset..(offset + read.1.len())]);

            i += num_pages as usize;
        }

        // free temporary buffers
        unsafe {
            LcMemFree(mems as *mut c_void);
        };

        Ok(())
    }

    fn phys_write_raw_list(&mut self, data: &[PhysicalWriteData]) -> Result<()> {
        /*
        let mem_map = &self.mem_map;

        let mut void = FnExtend::void();
        let mut iter = mem_map.map_iter(data.iter().copied().map(<_>::from), &mut void);

        let handle = self.handle.lock().unwrap();

        let mut elem = iter.next();
        while let Some(((addr, _), out)) = elem {
            let result = unsafe {
                LcWrite(
                    *handle,
                    addr.as_u64(),
                    out.len() as u32,
                    out.as_ptr() as *mut u8,
                )
            };
            if result != 1 {
                return Err(Error::Connector("unable to write memory"));
            }
            //println!("write({}, {}) = {}", addr.as_u64(), out.len(), result);

            elem = iter.next();
        }
        */

        Ok(())
    }

    fn metadata(&self) -> PhysicalMemoryMetadata {
        self.metadata.clone()
    }
}

/// Creates a new PciLeech Connector instance.
#[connector(name = "pcileech")]
pub fn create_connector(args: &ConnectorArgs) -> Result<PciLeech> {
    let device = args
        .get("device")
        .or_else(|| args.get_default())
        .ok_or(Error::Connector("argument 'device' missing"))?;
    PciLeech::new(device)
}
