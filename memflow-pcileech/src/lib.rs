// https://github.com/ufrisk/pcileech/blob/master/pcileech/device.c

use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;
use std::slice;
use std::sync::{Arc, Mutex};

use log::{error, info, Level};

use memflow::derive::connector;
use memflow::prelude::v1::*;

use leechcore_sys::*;

const PAGE_SIZE: usize = 0x1000usize;

const BUF_ALIGN: u64 = 4;
const BUF_MIN_LEN: usize = 8;
const BUF_LEN_ALIGN: usize = 8;

fn build_lc_config(device: &str) -> LC_CONFIG {
    let cdevice = unsafe { &*(device.as_bytes() as *const [u8] as *const [c_char]) };
    let mut adevice: [c_char; 260] = [0; 260];
    adevice[..device.len().min(260)].copy_from_slice(&cdevice[..device.len().min(260)]);

    // TODO: copy device + remote

    LC_CONFIG {
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
    }
}

const fn calc_num_pages(start: u64, size: u64) -> u64 {
    ((start & (PAGE_SIZE as u64 - 1)) + size + (PAGE_SIZE as u64 - 1)) >> 12
}

#[allow(clippy::mutex_atomic)]
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
            metadata: self.metadata,
            mem_map: self.mem_map.clone(),
        }
    }
}

// TODO: proper drop + free impl -> LcMemFree(pLcErrorInfo);
#[allow(clippy::mutex_atomic)]
impl PciLeech {
    pub fn new(device: &str) -> Result<Self> {
        Self::with_mapping(device, MemoryMap::new())
    }

    pub fn with_memmap<P: AsRef<Path>>(device: &str, path: P) -> Result<Self> {
        info!(
            "loading memory mappings from file: {}",
            path.as_ref().to_string_lossy()
        );
        let memmap = MemoryMap::open(path)?;
        info!("{:?}", memmap);
        Self::with_mapping(device, memmap)
    }

    #[allow(clippy::mutex_atomic)]
    fn with_mapping(device: &str, mem_map: MemoryMap<(Address, usize)>) -> Result<Self> {
        // open device
        let mut conf = build_lc_config(device);
        let err = std::ptr::null_mut::<PLC_CONFIG_ERRORINFO>();
        let handle = unsafe { LcCreateEx(&mut conf, err) };
        if handle.is_null() {
            // TODO: handle version error
            // TODO: handle special case of fUserInputRequest
            error!("leechcore error: {:?}", err);
            return Err(Error(ErrorOrigin::Connector, ErrorKind::Configuration)
                .log_error("unable to create leechcore context"));
        }

        Ok(Self {
            handle: Arc::new(Mutex::new(handle)),
            metadata: PhysicalMemoryMetadata {
                size: conf.paMax as usize,
                readonly: conf.fVolatile == 0,
                // TODO: writable flag
            },
            mem_map,
        })
    }
}

struct ReadGap {
    gap_buffer: *mut u8,
    gap_buffer_len: usize,
    out_buffer: *mut u8,
    out_start: usize,
    out_end: usize,
}

struct WriteGap {
    gap_addr: PhysicalAddress,
    gap_buffer: *mut u8,
    gap_buffer_len: usize,
    in_buffer: *const u8,
    in_start: usize,
    in_end: usize,
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
            return Err(Error(ErrorOrigin::Connector, ErrorKind::InvalidMemorySize)
                .log_error("unable to allocate scatter buffer"));
        }

        // prepare mems
        let mut gaps = Vec::new();
        let mut i = 0usize;
        for read in data.iter_mut() {
            for (page_addr, out) in read.1.page_chunks(read.0.into(), PAGE_SIZE) {
                let mem = unsafe { *mems.add(i) };

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
                    let buffer_ptr = Box::into_raw(buffer) as *mut u8;

                    gaps.push(ReadGap {
                        gap_buffer: buffer_ptr,
                        gap_buffer_len: buffer_len,
                        out_buffer: out.as_mut_ptr(),
                        out_start: addr_align as usize,
                        out_end: out.len() + addr_align as usize,
                    });

                    unsafe { (*mem).qwA = page_addr.as_u64() - addr_align };
                    unsafe { (*mem).pb = buffer_ptr };
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
        if !gaps.is_empty() {
            for gap in gaps.iter() {
                let buffer: Box<[u8]> = unsafe {
                    Box::from_raw(ptr::slice_from_raw_parts_mut(
                        gap.gap_buffer,
                        gap.gap_buffer_len,
                    ))
                };

                let out_buffer = unsafe {
                    slice::from_raw_parts_mut(gap.out_buffer, gap.out_end - gap.out_start)
                };
                out_buffer.copy_from_slice(&buffer[gap.out_start..gap.out_end]);

                // drop buffer
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
            return Err(Error(ErrorOrigin::Connector, ErrorKind::InvalidMemorySize)
                .log_error("unable to allocate scatter buffer"));
        }

        // prepare mems
        let mut gaps = Vec::new();
        let mut i = 0usize;
        for write in data.iter() {
            for (page_addr, out) in write.1.page_chunks(write.0.into(), PAGE_SIZE) {
                let mem = unsafe { *mems.add(i) };

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

                    // prepare gap buffer for reading
                    let write_addr = (page_addr.as_u64() - addr_align).into();
                    let buffer = vec![0u8; buffer_len].into_boxed_slice();
                    let buffer_ptr = Box::into_raw(buffer) as *mut u8;

                    // send over to our gaps list
                    gaps.push(WriteGap {
                        gap_addr: write_addr,
                        gap_buffer: buffer_ptr,
                        gap_buffer_len: buffer_len,
                        in_buffer: out.as_ptr(),
                        in_start: addr_align as usize,
                        in_end: out.len() + addr_align as usize,
                    });

                    // store pointers into pcileech struct for writing (after we dispatched a read)
                    unsafe { (*mem).qwA = write_addr.as_u64() };
                    unsafe { (*mem).pb = buffer_ptr };
                    unsafe { (*mem).cb = buffer_len as u32 };
                }

                i += 1;
            }
        }

        // dispatch necessary reads to fill the gaps
        if !gaps.is_empty() {
            let mut datas = gaps
                .iter()
                .map(|g| {
                    PhysicalReadData(g.gap_addr, unsafe {
                        slice::from_raw_parts_mut(g.gap_buffer, g.gap_buffer_len)
                    })
                })
                .collect::<Vec<_>>();

            self.phys_read_raw_list(datas.as_mut_slice())?;

            for (gap, read) in gaps.iter().zip(datas) {
                let in_buffer =
                    unsafe { slice::from_raw_parts(gap.in_buffer, gap.in_end - gap.in_start) };
                read.1[gap.in_start..gap.in_end].copy_from_slice(in_buffer);
            }
        }

        // dispatch write
        {
            let handle = self.handle.lock().unwrap();
            unsafe {
                LcWriteScatter(*handle, num_pages as u32, mems);
            }
        }

        if !gaps.is_empty() {
            for gap in gaps.iter() {
                let _ = unsafe { Box::from_raw(gap.gap_buffer) };
                // drop buffer
            }
        }

        // free temporary buffers
        unsafe {
            LcMemFree(mems as *mut c_void);
        };

        Ok(())
    }

    fn metadata(&self) -> PhysicalMemoryMetadata {
        self.metadata
    }

    fn set_mem_map(&mut self, mem_map: MemoryMap<(Address, usize)>) {
        // TODO: check if current mem_map is empty
        // TODO: update metadata.size
        self.mem_map = mem_map;
    }
}

/// Creates a new PciLeech Connector instance.
pub fn create_connector(args: &Args, log_level: Level) -> Result<PciLeech> {
    simple_logger::SimpleLogger::new()
        .with_level(log_level.to_level_filter())
        .init()
        .ok();

    let validator = ArgsValidator::new()
        .arg(ArgDescriptor::new("default").description("the target device to be used by LeechCore"))
        .arg(ArgDescriptor::new("device").description("the target device to be used by LeechCore"))
        .arg(ArgDescriptor::new("memmap").description("the memory map file of the target machine"));

    match validator.validate(&args) {
        Ok(_) => {
            let device = args.get("device").or_else(|| args.get_default()).ok_or(
                Error(ErrorOrigin::Connector, ErrorKind::ArgValidation)
                    .log_error("'device' argument is missing"),
            )?;

            if let Some(memmap) = args.get("memmap") {
                PciLeech::with_memmap(device, memmap)
            } else {
                PciLeech::new(device)
            }
        }
        Err(err) => {
            error!(
                "unable to validate provided arguments, valid arguments are:\n{}",
                validator
            );
            Err(err)
        }
    }
}

/// Creates a new PciLeech Connector instance.
#[connector(name = "pcileech")]
pub fn create_connector_instance(args: &Args, log_level: Level) -> Result<ConnectorInstance> {
    let connector = create_connector(args, log_level)?;
    let instance = ConnectorInstance::builder(connector).build();
    Ok(instance)
}
