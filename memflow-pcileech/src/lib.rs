// https://github.com/ufrisk/pcileech/blob/master/pcileech/device.c

use std::ffi::c_void;
use std::os::raw::c_char;
use std::ptr;
use std::slice;
use std::sync::{Arc, Mutex};

use log::{error, info, warn};

use memflow::*;
use memflow_derive::connector;

use leechcore_sys::*;

const PAGE_SIZE: usize = 0x1000usize;
const BUF_ALIGN: u64 = 4;
const BUF_MIN_LEN: usize = 8;
const BUF_LEN_ALIGN: usize = 8;

const fn calc_num_pages(start: u64, size: u64) -> u64 {
    ((start & (PAGE_SIZE as u64 - 1)) + size + (PAGE_SIZE as u64 - 1)) >> 12
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
        //let mem_map = &self.mem_map;

        // get total number of pages
        let num_pages = data.iter().fold(0u64, |acc, read| {
            acc + calc_num_pages(read.0.as_u64(), read.1.len() as u64)
        });

        // allocate scatter buffer
        let mut mems = std::ptr::null_mut::<PMEM_SCATTER>();
        let result = unsafe {
            LcAllocScatter2(
                (num_pages * PAGE_SIZE as u64) as u32,
                std::ptr::null_mut(),
                num_pages as u32,
                &mut mems as *mut PPMEM_SCATTER,
            )
        };
        if result != 1 {
            return Err(Error::Connector("unable to allocate scatter buffer"));
        }

        // prepare mems
        let mut i = 0usize;
        for read in data.iter_mut() {
            for (page_addr, out) in read.1.page_chunks(read.0.into(), PAGE_SIZE) {
                let mem = unsafe { *mems.offset(i as isize) };

                let addr_align = page_addr.as_u64() & (BUF_ALIGN - 1);
                let len_align = out.len() & (BUF_LEN_ALIGN - 1);

                if addr_align == 0 && len_align == 0 && out.len() >= BUF_MIN_LEN {
                    // properly aligned read
                    unsafe { (*mem).qwA = page_addr.as_u64() };
                    unsafe { (*mem).pb = out.as_mut_ptr() };
                    unsafe { (*mem).cb = out.len() as u32 };
                } else {
                    // non-aligned or small read
                    let mut buffer_len = (out.len() + addr_align as usize).max(BUF_MIN_LEN);
                    buffer_len += BUF_LEN_ALIGN - (buffer_len & (BUF_LEN_ALIGN - 1));

                    let buffer = vec![0u8; buffer_len].into_boxed_slice();

                    unsafe { (*mem).qwA = page_addr.as_u64() - addr_align };
                    unsafe { (*mem).pb = Box::into_raw(buffer) as *mut u8 };
                    unsafe { (*mem).cb = buffer_len as u32 };
                }

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

        // gather all 'bogus' reads we had to custom-allocate
        i = 0usize;
        for read in data.iter_mut() {
            for (page_addr, out) in read.1.page_chunks(read.0.into(), PAGE_SIZE) {
                let mem = unsafe { *mems.offset(i as isize) };

                let addr_align = page_addr.as_u64() & (BUF_ALIGN - 1);
                let len_align = out.len() & (BUF_LEN_ALIGN - 1);

                if addr_align != 0 || len_align != 0 || out.len() < BUF_MIN_LEN {
                    // take ownership of the buffer again
                    // and copy buffer into original again
                    let buffer: Box<[u8]> = unsafe {
                        Box::from_raw(ptr::slice_from_raw_parts_mut((*mem).pb, (*mem).cb as usize))
                    };
                    out.copy_from_slice(
                        &buffer[addr_align as usize..out.len() + addr_align as usize],
                    );
                }

                i += 1;
            }
        }

        // free temporary buffers
        unsafe {
            LcMemFree(mems as *mut c_void);
        };

        Ok(())
    }

    fn phys_write_raw_list(&mut self, data: &[PhysicalWriteData]) -> Result<()> {
        //let mem_map = &self.mem_map;

        // get total number of pages
        let num_pages = data.iter().fold(0u64, |acc, read| {
            acc + calc_num_pages(read.0.as_u64(), read.1.len() as u64)
        });

        // allocate scatter buffer
        let mut mems = std::ptr::null_mut::<PMEM_SCATTER>();
        let result = unsafe {
            LcAllocScatter2(
                (num_pages * PAGE_SIZE as u64) as u32,
                std::ptr::null_mut(),
                num_pages as u32,
                &mut mems as *mut PPMEM_SCATTER,
            )
        };
        if result != 1 {
            return Err(Error::Connector("unable to allocate scatter buffer"));
        }

        // prepare mems
        let mut i = 0usize;
        for write in data.iter() {
            for (page_addr, out) in write.1.page_chunks(write.0.into(), PAGE_SIZE) {
                let mem = unsafe { *mems.offset(i as isize) };

                let addr_align = page_addr.as_u64() & (BUF_ALIGN - 1);
                let len_align = out.len() & (BUF_LEN_ALIGN - 1);

                if addr_align == 0 && len_align == 0 && out.len() >= BUF_MIN_LEN {
                    // properly aligned read
                    unsafe { (*mem).qwA = page_addr.as_u64() };
                    unsafe { (*mem).pb = out.as_ptr() as *mut u8 };
                    unsafe { (*mem).cb = out.len() as u32 };
                } else {
                    // non-aligned or small read
                    let mut buffer_len = (out.len() + addr_align as usize).max(BUF_MIN_LEN);
                    buffer_len += BUF_LEN_ALIGN - (buffer_len & (BUF_LEN_ALIGN - 1));

                    let mut buffer = vec![0u8; buffer_len].into_boxed_slice();

                    let write_addr = (page_addr.as_u64() - addr_align).into();
                    self.phys_read_into(write_addr, &mut buffer[..])?;

                    // copy data over
                    buffer[addr_align as usize..out.len() + addr_align as usize]
                        .copy_from_slice(out);

                    unsafe { (*mem).qwA = write_addr.as_u64() };
                    unsafe { (*mem).pb = Box::into_raw(buffer) as *mut u8 };
                    unsafe { (*mem).cb = buffer_len as u32 };
                }

                i += 1;
            }
        }

        // dispatch write
        {
            let handle = self.handle.lock().unwrap();
            unsafe {
                LcWriteScatter(*handle, num_pages as u32, mems);
            }
        }

        // free temporary buffers
        unsafe {
            LcMemFree(mems as *mut c_void);
        };

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
