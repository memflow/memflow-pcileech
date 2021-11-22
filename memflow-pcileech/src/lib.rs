use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;
use std::slice;
use std::sync::{Arc, Mutex};

use log::{error, info, Level};

use memflow::cglue;
use memflow::mem::phys_mem::*;
use memflow::prelude::v1::*;

use leechcore_sys::*;

const PAGE_SIZE: usize = 0x1000usize;

const BUF_ALIGN: u64 = 4;
const BUF_MIN_LEN: usize = 8;
const BUF_LEN_ALIGN: usize = 8;

cglue_impl_group!(PciLeech, ConnectorInstance<'a>, {});

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
#[derive(Clone)]
pub struct PciLeech {
    handle: Arc<Mutex<HANDLE>>,
    conf: LC_CONFIG,
    mem_map: Option<MemoryMap<(Address, umem)>>,
}

unsafe impl Send for PciLeech {}

// TODO: proper drop + free impl -> LcMemFree(pLcErrorInfo);
#[allow(clippy::mutex_atomic)]
impl PciLeech {
    pub fn new(device: &str) -> Result<Self> {
        Self::new_internal(device, None)
    }

    pub fn with_mem_map_file<P: AsRef<Path>>(device: &str, path: P) -> Result<Self> {
        info!(
            "loading memory mappings from file: {}",
            path.as_ref().to_string_lossy()
        );
        let mem_map = MemoryMap::open(path)?;
        info!("{:?}", mem_map);
        Self::new_internal(device, Some(mem_map))
    }

    #[allow(clippy::mutex_atomic)]
    fn new_internal(device: &str, mem_map: Option<MemoryMap<(Address, umem)>>) -> Result<Self> {
        // open device
        let mut conf = build_lc_config(device);
        let err = std::ptr::null_mut::<PLC_CONFIG_ERRORINFO>();
        let handle = unsafe { LcCreateEx(&mut conf, err) };
        if handle.is_null() {
            // TODO: handle version error
            // TODO: handle special case of fUserInputRequest
            return Err(Error(ErrorOrigin::Connector, ErrorKind::Configuration)
                .log_error(&format!("unable to create leechcore context: {:?}", err)));
        }

        Ok(Self {
            handle: Arc::new(Mutex::new(handle)),
            conf,
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
    fn phys_read_raw_iter<'a>(
        &mut self,
        data: CIterator<PhysicalReadData<'a>>,
        out_fail: &mut PhysicalReadFailCallback<'_, 'a>,
    ) -> Result<()> {
        let vec = if let Some(mem_map) = &self.mem_map {
            let mut callback = &mut |(a, b): (Address, _)| out_fail.call(MemData(a.into(), b));
            mem_map
                .map_iter(data.map(|MemData(addr, buf)| (addr, buf)), &mut callback)
                .map(|d| (d.0 .0.into(), d.1))
                .collect::<Vec<_>>()
        } else {
            data.map(|MemData(addr, buf)| (addr, buf))
                .collect::<Vec<_>>()
        };

        // get total number of pages
        let num_pages = vec.iter().fold(0u64, |acc, read| {
            acc + calc_num_pages(read.0.to_umem(), read.1.len() as u64)
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
        for read in vec.into_iter() {
            for (page_addr, out) in read.1.page_chunks(read.0.into(), PAGE_SIZE) {
                let mem = unsafe { *mems.add(i) };

                let addr_align = page_addr.to_umem() & (BUF_ALIGN - 1);
                let len_align = out.len() & (BUF_LEN_ALIGN - 1);

                if addr_align == 0 && len_align == 0 && out.len() >= BUF_MIN_LEN {
                    // properly aligned read
                    unsafe { (*mem).qwA = page_addr.to_umem() };
                    unsafe { (*mem).__bindgen_anon_1.pb = out.as_mut_ptr() };
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

                    unsafe { (*mem).qwA = page_addr.to_umem() - addr_align };
                    unsafe { (*mem).__bindgen_anon_1.pb = buffer_ptr };
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

    fn phys_write_raw_iter<'a>(
        &mut self,
        data: CIterator<PhysicalWriteData<'a>>,
        out_fail: &mut PhysicalWriteFailCallback<'_, 'a>,
    ) -> Result<()> {
        let vec = if let Some(mem_map) = &self.mem_map {
            let mut callback = &mut |(a, b): (Address, _)| out_fail.call(MemData(a.into(), b));
            mem_map
                .map_iter(data.map(|MemData(addr, buf)| (addr, buf)), &mut callback)
                .map(|d| (d.0 .0.into(), d.1))
                .collect::<Vec<_>>()
        } else {
            data.map(|MemData(addr, buf)| (addr, buf))
                .collect::<Vec<_>>()
        };

        // get total number of pages
        let num_pages = vec.iter().fold(0u64, |acc, read| {
            acc + calc_num_pages(read.0.to_umem(), read.1.len() as u64)
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
        for write in vec.iter() {
            for (page_addr, out) in write.1.page_chunks(write.0.into(), PAGE_SIZE) {
                let mem = unsafe { *mems.add(i) };

                let addr_align = page_addr.to_umem() & (BUF_ALIGN - 1);
                let len_align = out.len() & (BUF_LEN_ALIGN - 1);

                if addr_align == 0 && len_align == 0 && out.len() >= BUF_MIN_LEN {
                    // properly aligned read
                    unsafe { (*mem).qwA = page_addr.to_umem() };
                    unsafe { (*mem).__bindgen_anon_1.pb = out.as_ptr() as *mut u8 };
                    unsafe { (*mem).cb = out.len() as u32 };
                } else {
                    // non-aligned or small read
                    let mut buffer_len = (out.len() + addr_align as usize).max(BUF_MIN_LEN);
                    buffer_len += BUF_LEN_ALIGN - (buffer_len & (BUF_LEN_ALIGN - 1));

                    // prepare gap buffer for reading
                    let write_addr = (page_addr.to_umem() - addr_align).into();
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
                    unsafe { (*mem).qwA = write_addr.to_umem() };
                    unsafe { (*mem).__bindgen_anon_1.pb = buffer_ptr };
                    unsafe { (*mem).cb = buffer_len as u32 };
                }

                i += 1;
            }
        }

        // dispatch necessary reads to fill the gaps
        if !gaps.is_empty() {
            let iter = gaps.iter().map(|g| {
                MemData(
                    g.gap_addr,
                    unsafe { slice::from_raw_parts_mut(g.gap_buffer, g.gap_buffer_len) }.into(),
                )
            });

            let out_fail = &mut |_| true;
            self.phys_read_raw_iter((&mut iter.clone()).into(), &mut out_fail.into())?;

            for (gap, mut read) in gaps.iter().zip(iter) {
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
        let (max_address, real_size) = if let Some(mem_map) = &self.mem_map {
            (mem_map.max_address(), mem_map.real_size())
        } else {
            (
                (self.conf.paMax as usize - 1_usize).into(),
                self.conf.paMax as umem,
            )
        };
        PhysicalMemoryMetadata {
            max_address,
            real_size,
            readonly: self.conf.fVolatile == 0,
            ideal_batch_size: 128,
        }
    }

    // Sets the memory map only in cases where no previous memory map was being set by the end-user.
    fn set_mem_map(&mut self, mem_map: &[PhysicalMemoryMapping]) {
        if self.mem_map.is_none() {
            self.mem_map = Some(MemoryMap::<(Address, umem)>::from_vec(mem_map.to_vec()));
        }
    }
}

fn validator() -> ArgsValidator {
    ArgsValidator::new()
        .arg(ArgDescriptor::new("default").description("the target device to be used by LeechCore"))
        .arg(ArgDescriptor::new("device").description("the target device to be used by LeechCore"))
        .arg(ArgDescriptor::new("memmap").description("the memory map file of the target machine"))
}

/// Creates a new PciLeech Connector instance.
#[connector(name = "pcileech", help_fn = "help", target_list_fn = "target_list")]
pub fn create_connector(args: &Args, log_level: Level) -> Result<PciLeech> {
    simple_logger::SimpleLogger::new()
        .with_level(log_level.to_level_filter())
        .init()
        .ok();

    let validator = validator();
    match validator.validate(&args) {
        Ok(_) => {
            let device = args
                .get("device")
                .or_else(|| args.get_default())
                .ok_or_else(|| {
                    Error(ErrorOrigin::Connector, ErrorKind::ArgValidation)
                        .log_error("'device' argument is missing")
                })?;

            if let Some(memmap) = args.get("memmap") {
                PciLeech::with_mem_map_file(device, memmap)
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

/// Retrieve the help text for the Qemu Procfs Connector.
pub fn help() -> String {
    let validator = validator();
    format!(
        "\
The `pcileech` connector implements the LeechCore interface of pcileech for memflow.

More information about pcileech can be found under https://github.com/ufrisk/pcileech.

This connector requires access to the usb ports to access the pcileech hardware.

Available arguments are:
{}",
        validator.to_string()
    )
}

/// Retrieve a list of all currently available PciLeech targets.
pub fn target_list() -> Result<Vec<TargetInfo>> {
    Ok(vec![])
}
