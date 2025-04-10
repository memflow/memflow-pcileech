use parking_lot::Mutex;
use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;
use std::ptr::null_mut;
use std::slice;
use std::sync::Arc;

use log::LevelFilter;
use log::{error, info};

use memflow::cglue;
use memflow::mem::phys_mem::*;
use memflow::prelude::v1::*;

use leechcore_sys::*;

const PAGE_SIZE: usize = 0x1000usize;

// the absolute minimum BUF_ALIGN is 4.
// using 8 bytes as BUF_ALIGN here simplifies things a lot
// and makes our gap detection code work in cases where page boundaries would be crossed.
const BUF_ALIGN: u64 = 8;
const BUF_MIN_LEN: usize = 8;
const BUF_LEN_ALIGN: usize = 8;

cglue_impl_group!(PciLeech, ConnectorInstance<'a>, {});

fn build_lc_config(device: &str, remote: Option<&str>, with_mem_map: bool) -> LC_CONFIG {
    // configure verbosity based on current level
    let printf_verbosity = match log::max_level() {
        LevelFilter::Off => 0,
        LevelFilter::Error | LevelFilter::Warn => LC_CONFIG_PRINTF_ENABLED,
        LevelFilter::Info => LC_CONFIG_PRINTF_ENABLED | LC_CONFIG_PRINTF_V,
        LevelFilter::Debug => {
            LC_CONFIG_PRINTF_ENABLED
                | LC_CONFIG_PRINTF_V
                | LC_CONFIG_PRINTF_ENABLED
                | LC_CONFIG_PRINTF_VV
        }
        LevelFilter::Trace => {
            LC_CONFIG_PRINTF_ENABLED
                | LC_CONFIG_PRINTF_V
                | LC_CONFIG_PRINTF_ENABLED
                | LC_CONFIG_PRINTF_VVV
        }
    };

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
        dwPrintfVerbosity: printf_verbosity,
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
    pub fn new(device: &str, remote: Option<&str>, auto_clear: bool) -> Result<Self> {
        Self::new_internal(device, remote, None, auto_clear)
    }

    pub fn with_mem_map_file<P: AsRef<Path>>(
        device: &str,
        remote: Option<&str>,
        path: P,
        auto_clear: bool,
    ) -> Result<Self> {
        info!(
            "loading memory mappings from file: {}",
            path.as_ref().to_string_lossy()
        );
        let mem_map = MemoryMap::open(path)?;
        info!("{:?}", mem_map);
        Self::new_internal(device, remote, Some(mem_map), auto_clear)
    }

    #[allow(clippy::mutex_atomic)]
    fn new_internal(
        device: &str,
        remote: Option<&str>,
        mem_map: Option<MemoryMap<(Address, umem)>>,
        auto_clear: bool,
    ) -> Result<Self> {
        // open device
        let mut conf = build_lc_config(device, remote, mem_map.is_some());
        let p_lc_config_error_info = std::ptr::null_mut::<LC_CONFIG_ERRORINFO>();
        let pp_lc_config_error_info = &raw const p_lc_config_error_info as *mut PLC_CONFIG_ERRORINFO;
        let handle = unsafe { LcCreateEx(&mut conf, pp_lc_config_error_info) };
        if handle.is_null() {
            // TODO: handle version error
            // TODO: handle special case of fUserInputRequest
            let err = if p_lc_config_error_info.is_null() {
                None
            } else {
                // read the data at the error
                Some(unsafe { p_lc_config_error_info.read() })
            };

            return Err(Error(ErrorOrigin::Connector, ErrorKind::Configuration)
                .log_error(format!("unable to create leechcore context: {err:?}", )));
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

        #[allow(clippy::arc_with_non_send_sync)]
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

impl PhysicalMemory for PciLeech {
    fn phys_read_raw_iter<'a>(&mut self, mut data: PhysicalReadMemOps) -> Result<()> {
        let mut vec = if let Some(mem_map) = &self.mem_map {
            mem_map
                .map_iter(data.inp, data.out_fail)
                .map(|d| (d.0 .0.into(), d.1, d.2))
                .collect::<Vec<_>>()
        } else {
            data.inp.map(|d| (d.0, d.1, d.2)).collect::<Vec<_>>()
        };

        // get total number of pages
        let num_pages = vec.iter().fold(0u64, |acc, read| {
            acc + calc_num_pages(read.0.to_umem(), read.2.len() as u64)
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
        for (addr, _, out) in vec.iter_mut() {
            for (page_addr, out) in CSliceMut::from(out).page_chunks(addr.address(), PAGE_SIZE) {
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
                    let page_addr_align = page_addr.to_umem() - addr_align;
                    let mut buffer_len = out.len() + addr_align as usize;
                    let buf_align = buffer_len & (BUF_LEN_ALIGN - 1);
                    if buf_align > 0 {
                        buffer_len += BUF_LEN_ALIGN - buf_align;
                    }
                    buffer_len = buffer_len.max(BUF_MIN_LEN);

                    // note that this always holds true because addr alignment is equal to buf length alignment
                    assert!(buffer_len >= out.len());

                    // we never want to cross page boundaries, otherwise the read will just not work
                    assert_eq!(
                        page_addr.to_umem() - (page_addr.to_umem() & (PAGE_SIZE as umem - 1)),
                        (page_addr_align + buffer_len as umem - 1)
                            - ((page_addr_align + buffer_len as umem - 1)
                                & (PAGE_SIZE as umem - 1))
                    );

                    let buffer = vec![0u8; buffer_len].into_boxed_slice();
                    let buffer_ptr = Box::into_raw(buffer) as *mut u8;

                    gaps.push(ReadGap {
                        gap_buffer: buffer_ptr,
                        gap_buffer_len: buffer_len,
                        out_buffer: out.as_mut_ptr(),
                        out_start: addr_align as usize,
                        out_end: out.len() + addr_align as usize,
                    });

                    unsafe { (*mem).qwA = page_addr_align };
                    unsafe { (*mem).__bindgen_anon_1.pb = buffer_ptr };
                    unsafe { (*mem).cb = buffer_len as u32 };
                }

                i += 1;
            }
        }

        // dispatch read
        {
            let handle = self.handle.lock();
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

        // call out sucess for everything
        // TODO: implement proper callback based on `f` in scatter
        for (_, meta_addr, out) in vec.into_iter() {
            opt_call(data.out.as_deref_mut(), CTup2(meta_addr, out));
        }

        Ok(())
    }

    fn phys_write_raw_iter<'a>(&mut self, mut data: PhysicalWriteMemOps) -> Result<()> {
        let vec = if let Some(mem_map) = &self.mem_map {
            mem_map
                .map_iter(data.inp, data.out_fail)
                .map(|d| (d.0 .0.into(), d.1, d.2))
                .collect::<Vec<_>>()
        } else {
            data.inp.map(|d| (d.0, d.1, d.2)).collect::<Vec<_>>()
        };

        // get total number of pages
        let num_pages = vec.iter().fold(0u64, |acc, read| {
            acc + calc_num_pages(read.0.to_umem(), read.2.len() as u64)
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
            for (page_addr, out) in write.2.page_chunks(write.0.into(), PAGE_SIZE) {
                let mem = unsafe { *mems.add(i) };

                let addr_align = page_addr.to_umem() & (BUF_ALIGN - 1);
                let len_align = out.len() & (BUF_LEN_ALIGN - 1);

                if addr_align == 0 && len_align == 0 && out.len() >= BUF_MIN_LEN {
                    // properly aligned write
                    unsafe { (*mem).qwA = page_addr.to_umem() };
                    unsafe { (*mem).__bindgen_anon_1.pb = out.as_ptr() as *mut u8 };
                    unsafe { (*mem).cb = out.len() as u32 };
                } else {
                    // non-aligned or small write
                    let page_addr_align = page_addr.to_umem() - addr_align;
                    let mut buffer_len = out.len() + addr_align as usize;
                    let buf_align = buffer_len & (BUF_LEN_ALIGN - 1);
                    if buf_align > 0 {
                        buffer_len += BUF_LEN_ALIGN - buf_align;
                    }
                    buffer_len = buffer_len.max(BUF_MIN_LEN);

                    // note that this always holds true because addr alignment is equal to buf length alignment
                    assert!(buffer_len >= out.len());

                    // we never want to cross page boundaries, otherwise the write will just not work
                    assert_eq!(
                        page_addr.to_umem() - (page_addr.to_umem() & (PAGE_SIZE as umem - 1)),
                        (page_addr_align + buffer_len as umem - 1)
                            - ((page_addr_align + buffer_len as umem - 1)
                                & (PAGE_SIZE as umem - 1))
                    );

                    // prepare gap buffer for writing
                    let buffer = vec![0u8; buffer_len].into_boxed_slice();
                    let buffer_ptr = Box::into_raw(buffer) as *mut u8;

                    // send over to our gaps list
                    gaps.push(WriteGap {
                        gap_addr: page_addr_align.into(),
                        gap_buffer: buffer_ptr,
                        gap_buffer_len: buffer_len,
                        in_buffer: out.as_ptr(),
                        in_start: addr_align as usize,
                        in_end: out.len() + addr_align as usize,
                    });

                    // store pointers into pcileech struct for writing (after we dispatched a read)
                    unsafe { (*mem).qwA = page_addr_align };
                    unsafe { (*mem).__bindgen_anon_1.pb = buffer_ptr };
                    unsafe { (*mem).cb = buffer_len as u32 };
                }

                i += 1;
            }
        }

        // dispatch necessary reads to fill the gaps
        if !gaps.is_empty() {
            let mut vec: Vec<CTup2<PhysicalAddress, &mut [u8]>> = gaps
                .iter()
                .map(|g| {
                    CTup2(g.gap_addr, unsafe {
                        slice::from_raw_parts_mut(g.gap_buffer, g.gap_buffer_len)
                    })
                })
                .collect::<Vec<_>>();

            let mut iter = vec
                .iter_mut()
                .map(|CTup2(a, d)| (*a, CSliceRef::from(d.as_bytes())));

            MemOps::with(&mut iter, None, None, |data| self.phys_write_raw_iter(data))?;

            for (gap, read) in gaps.iter().zip(vec) {
                let in_buffer =
                    unsafe { slice::from_raw_parts(gap.in_buffer, gap.in_end - gap.in_start) };
                read.1[gap.in_start..gap.in_end].copy_from_slice(in_buffer);
            }
        }

        // TODO:
        // opt_call(out.as_deref_mut(), CTup2(meta_addr, data));

        // dispatch write
        {
            let handle = self.handle.lock();
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

        // call out sucess for everything
        // TODO: implement proper callback based on `f` in scatter
        for (_, meta_addr, out) in vec.into_iter() {
            opt_call(data.out.as_deref_mut(), CTup2(meta_addr, out));
        }

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
        .arg(ArgDescriptor::new("remote").description("the remote target to be used by LeechCore"))
        .arg(ArgDescriptor::new("memmap").description("the memory map file of the target machine"))
        .arg(ArgDescriptor::new("auto-clear").description("tries to enable the status register auto-clear function (only available for bitstreams 4.7 and upwards)"))
}

/// Creates a new PciLeech Connector instance.
#[connector(name = "pcileech", help_fn = "help", target_list_fn = "target_list")]
pub fn create_connector(args: &ConnectorArgs) -> Result<PciLeech> {
    let validator = validator();

    let args = &args.extra_args;

    match validator.validate(args) {
        Ok(_) => {
            let device = args
                .get("device")
                .or_else(|| args.get_default())
                .ok_or_else(|| {
                    Error(ErrorOrigin::Connector, ErrorKind::ArgValidation)
                        .log_error("'device' argument is missing")
                })?;
            let remote = args.get("remote");
            let auto_clear = args.get("auto-clear").is_some();
            if let Some(memmap) = args.get("memmap") {
                PciLeech::with_mem_map_file(device, remote, memmap, auto_clear)
            } else {
                PciLeech::new(device, remote, auto_clear)
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
{validator}"
    )
}

/// Retrieve a list of all currently available PciLeech targets.
pub fn target_list() -> Result<Vec<TargetInfo>> {
    // TODO: check if usb is connected, then list 1 target
    Ok(vec![])
}
