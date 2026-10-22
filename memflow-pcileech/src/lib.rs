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

/// A readable physical range reported by LeechCore.
///
/// `end` is exclusive. `source_offset` is the address LeechCore maps `start`
/// to in the backing device (for example, the corresponding file offset for
/// an ELF core-dump segment).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeechCorePhysicalRange {
    pub start: u64,
    pub end: u64,
    pub source_offset: u64,
}

impl LeechCorePhysicalRange {
    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

struct LeechCoreHandle {
    handle: Mutex<HANDLE>,
    close: unsafe fn(HANDLE),
}

impl LeechCoreHandle {
    fn new(handle: HANDLE) -> Self {
        debug_assert!(!handle.is_null());
        Self {
            handle: Mutex::new(handle),
            close: close_leechcore_handle,
        }
    }

    #[cfg(test)]
    fn with_closer(handle: HANDLE, close: unsafe fn(HANDLE)) -> Self {
        debug_assert!(!handle.is_null());
        Self {
            handle: Mutex::new(handle),
            close,
        }
    }

    fn lock(&self) -> parking_lot::MutexGuard<'_, HANDLE> {
        self.handle.lock()
    }
}

// All access to the native handle is serialized by `handle`. LeechCore handles
// may be used from different threads, but no operation may run concurrently on
// the same handle through this wrapper.
unsafe impl Send for LeechCoreHandle {}
unsafe impl Sync for LeechCoreHandle {}

impl Drop for LeechCoreHandle {
    fn drop(&mut self) {
        let handle = self.handle.get_mut();
        if !handle.is_null() {
            unsafe { (self.close)(*handle) };
            *handle = null_mut();
        }
    }
}

unsafe fn close_leechcore_handle(handle: HANDLE) {
    unsafe { LcClose(handle) };
}

struct LeechCoreAllocation(*mut c_void);

impl LeechCoreAllocation {
    fn new(pointer: *mut c_void) -> Self {
        Self(pointer)
    }
}

impl Drop for LeechCoreAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LcMemFree(self.0) };
            self.0 = null_mut();
        }
    }
}

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
    handle: Arc<LeechCoreHandle>,
    conf: LC_CONFIG,
    mem_map: Option<MemoryMap<(Address, umem)>>,
}

unsafe impl Send for PciLeech {}

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
        let mut p_lc_config_error_info = std::ptr::null_mut::<LC_CONFIG_ERRORINFO>();
        let handle = unsafe { LcCreateEx(&mut conf, &mut p_lc_config_error_info) };
        // LcCreateEx may return allocated error information. LeechCore requires
        // callers to release every non-null result with LcMemFree.
        let _error_info_allocation =
            LeechCoreAllocation::new(p_lc_config_error_info.cast::<c_void>());
        if handle.is_null() {
            error!("Unable to create leechcore context: {conf:?} pErr: {p_lc_config_error_info:?}");
            // TODO: handle version error
            // TODO: handle special case of fUserInputRequest
            let err = if p_lc_config_error_info.is_null() {
                None
            } else {
                // Keep the allocation owned by the guard above and format it by
                // reference before the guard releases it.
                Some(format!("{:?}", unsafe { &*p_lc_config_error_info }))
            };

            return Err(Error(ErrorOrigin::Connector, ErrorKind::Configuration)
                .log_error(format!("unable to create leechcore context: {err:?}",)));
        }

        // From this point on every early return closes the native handle. Clones
        // share this owner and the final clone closes it exactly once.
        let handle = Arc::new(LeechCoreHandle::new(handle));

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
            {
                let handle = handle.lock();
                unsafe {
                    LcGetOption(*handle, LC_OPT_FPGA_FPGA_ID, &mut id);
                    LcGetOption(*handle, LC_OPT_FPGA_VERSION_MAJOR, &mut version_major);
                    LcGetOption(*handle, LC_OPT_FPGA_VERSION_MINOR, &mut version_minor);
                }
            }
            if version_major >= 4 && (version_major >= 5 || version_minor >= 7) {
                // enable auto-clear of status register [master abort].
                info!("Trying to enable status register auto-clear");
                let mut data = [0x10, 0x00, 0x10, 0x00];
                let result = {
                    let handle = handle.lock();
                    unsafe {
                        LcCommand(
                            *handle,
                            LC_CMD_FPGA_CFGREGPCIE_MARKWR | 0x002,
                            data.len() as u32,
                            data.as_mut_ptr(),
                            null_mut(),
                            null_mut(),
                        )
                    }
                };
                if result != 0 {
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
            handle,
            conf,
            mem_map,
        })
    }

    /// Returns the readable physical ranges configured in LeechCore.
    ///
    /// The returned ranges describe the connector's backing device before an
    /// optional memflow `mem_map` overlay is applied. This is especially useful
    /// for sparse file formats, whose physical address space must not be treated
    /// as one contiguous range.
    pub fn physical_memory_ranges(&self) -> Result<Vec<LeechCorePhysicalRange>> {
        let mut buffer: PBYTE = null_mut();
        let mut buffer_len: u32 = 0;
        let result = {
            let handle = self.handle.lock();
            unsafe {
                LcCommand(
                    *handle,
                    LC_CMD_MEMMAP_GET_STRUCT,
                    0,
                    null_mut(),
                    &mut buffer,
                    &mut buffer_len,
                )
            }
        };
        let _allocation = LeechCoreAllocation::new(buffer.cast::<c_void>());

        if result == 0 {
            return Err(Error(ErrorOrigin::Connector, ErrorKind::Configuration)
                .log_error("unable to retrieve the LeechCore physical memory map"));
        }

        let entry_size = std::mem::size_of::<LC_MEMMAP_ENTRY>();
        let buffer_len = buffer_len as usize;
        if buffer_len % entry_size != 0 {
            return Err(Error(ErrorOrigin::Connector, ErrorKind::InvalidMemorySize).log_error(
                format!(
                    "invalid LeechCore physical memory map size: {buffer_len} is not a multiple of {entry_size}"
                ),
            ));
        }
        if buffer_len == 0 {
            return Ok(Vec::new());
        }
        if buffer.is_null() {
            return Err(Error(ErrorOrigin::Connector, ErrorKind::InvalidMemorySize)
                .log_error("LeechCore returned a null physical memory map buffer"));
        }

        let entry_count = buffer_len / entry_size;
        // LeechCore itself caps its map at 0x0010_0000 entries. Enforce that
        // contract before doing pointer arithmetic over a foreign allocation.
        if entry_count > 0x0010_0000 {
            return Err(
                Error(ErrorOrigin::Connector, ErrorKind::InvalidMemorySize).log_error(format!(
                    "LeechCore returned too many physical memory map entries: {entry_count}"
                )),
            );
        }

        let mut entries = Vec::with_capacity(entry_count);
        for index in 0..entry_count {
            let entry = unsafe { ptr::read_unaligned(buffer.cast::<LC_MEMMAP_ENTRY>().add(index)) };
            entries.push(entry);
        }

        convert_memory_map_entries(&entries).map_err(|message| {
            Error(ErrorOrigin::Connector, ErrorKind::InvalidMemorySize).log_error(message)
        })
    }
}

fn convert_memory_map_entries(
    entries: &[LC_MEMMAP_ENTRY],
) -> std::result::Result<Vec<LeechCorePhysicalRange>, String> {
    let mut ranges = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.cb == 0 {
            return Err(format!(
                "LeechCore physical memory map contains an empty range at {:#x}",
                entry.pa
            ));
        }
        let end = entry.pa.checked_add(entry.cb).ok_or_else(|| {
            format!(
                "LeechCore physical memory range overflows: base={:#x}, size={:#x}",
                entry.pa, entry.cb
            )
        })?;
        entry.paRemap.checked_add(entry.cb).ok_or_else(|| {
            format!(
                "LeechCore source range overflows: offset={:#x}, size={:#x}",
                entry.paRemap, entry.cb
            )
        })?;
        ranges.push(LeechCorePhysicalRange {
            start: entry.pa,
            end,
            source_offset: entry.paRemap,
        });
    }

    ranges.sort_unstable_by_key(|range| range.start);
    for adjacent in ranges.windows(2) {
        if adjacent[0].end > adjacent[1].start {
            return Err(format!(
                "LeechCore physical memory map contains overlapping ranges {:#x}..{:#x} and {:#x}..{:#x}",
                adjacent[0].start, adjacent[0].end, adjacent[1].start, adjacent[1].end
            ));
        }
    }

    Ok(ranges)
}

struct ReadGap {
    scatter_index: usize,
    gap_buffer: *mut u8,
    gap_buffer_len: usize,
    out_buffer: *mut u8,
    out_start: usize,
    out_end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReadPageSpan {
    request_index: usize,
    offset: usize,
    len: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReadResultSpan {
    request_index: usize,
    offset: usize,
    len: usize,
    success: bool,
}

fn coalesce_read_results(
    pages: &[ReadPageSpan],
    page_success: &[bool],
) -> std::result::Result<Vec<ReadResultSpan>, String> {
    if pages.len() != page_success.len() {
        return Err(format!(
            "read page/status count mismatch: {} pages, {} statuses",
            pages.len(),
            page_success.len()
        ));
    }

    let mut results: Vec<ReadResultSpan> = Vec::with_capacity(pages.len());
    for (page, success) in pages.iter().zip(page_success.iter().copied()) {
        if page.len == 0 {
            return Err(format!(
                "empty read page span for request {} at offset {:#x}",
                page.request_index, page.offset
            ));
        }
        let page_end = page.offset.checked_add(page.len).ok_or_else(|| {
            format!(
                "read page span overflows for request {}: offset={:#x}, size={:#x}",
                page.request_index, page.offset, page.len
            )
        })?;

        if let Some(previous) = results.last_mut() {
            let previous_end = previous.offset.checked_add(previous.len).ok_or_else(|| {
                format!(
                    "coalesced read span overflows for request {}",
                    previous.request_index
                )
            })?;
            if previous.request_index == page.request_index {
                if previous_end != page.offset {
                    return Err(format!(
                        "non-contiguous read page spans for request {}: previous end={:#x}, next offset={:#x}",
                        page.request_index, previous_end, page.offset
                    ));
                }
                if previous.success == success {
                    previous.len = page_end - previous.offset;
                    continue;
                }
            }
        }

        results.push(ReadResultSpan {
            request_index: page.request_index,
            offset: page.offset,
            len: page.len,
            success,
        });
    }

    Ok(results)
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
                .map_iter(data.inp, data.out_fail.as_deref_mut())
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
        let mut page_spans = Vec::with_capacity(num_pages as usize);
        let mut i = 0usize;
        for (request_index, (addr, _, request_out)) in vec.iter_mut().enumerate() {
            let request_len = request_out.len();
            let mut request_offset = 0usize;
            for (page_addr, page_out) in
                CSliceMut::from(request_out).page_chunks(addr.address(), PAGE_SIZE)
            {
                let mem = unsafe { *mems.add(i) };
                let page_len = page_out.len();
                page_spans.push(ReadPageSpan {
                    request_index,
                    offset: request_offset,
                    len: page_len,
                });

                let addr_align = page_addr.to_umem() & (BUF_ALIGN - 1);
                let len_align = page_len & (BUF_LEN_ALIGN - 1);

                if addr_align == 0 && len_align == 0 && page_len >= BUF_MIN_LEN {
                    // properly aligned read
                    unsafe { (*mem).qwA = page_addr.to_umem() };
                    unsafe { (*mem).__bindgen_anon_1.pb = page_out.as_mut_ptr() };
                    unsafe { (*mem).cb = page_len as u32 };
                } else {
                    // non-aligned or small read
                    let page_addr_align = page_addr.to_umem() - addr_align;
                    let mut buffer_len = page_len + addr_align as usize;
                    let buf_align = buffer_len & (BUF_LEN_ALIGN - 1);
                    if buf_align > 0 {
                        buffer_len += BUF_LEN_ALIGN - buf_align;
                    }
                    buffer_len = buffer_len.max(BUF_MIN_LEN);

                    // note that this always holds true because addr alignment is equal to buf length alignment
                    assert!(buffer_len >= page_len);

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
                        scatter_index: i,
                        gap_buffer: buffer_ptr,
                        gap_buffer_len: buffer_len,
                        out_buffer: page_out.as_mut_ptr(),
                        out_start: addr_align as usize,
                        out_end: page_len + addr_align as usize,
                    });

                    unsafe { (*mem).qwA = page_addr_align };
                    unsafe { (*mem).__bindgen_anon_1.pb = buffer_ptr };
                    unsafe { (*mem).cb = buffer_len as u32 };
                }

                i += 1;
                request_offset += page_len;
            }
            debug_assert_eq!(request_offset, request_len);
        }
        debug_assert_eq!(i, num_pages as usize);

        // dispatch read
        {
            let handle = self.handle.lock();
            unsafe {
                LcReadScatter(*handle, num_pages as u32, mems);
            }
        }

        let page_success = (0..num_pages as usize)
            .map(|index| {
                let mem = unsafe { *mems.add(index) };
                unsafe { (*mem).f != 0 }
            })
            .collect::<Vec<_>>();

        // Gather only successful reads that required an aligned temporary
        // buffer. A failed scatter buffer is deliberately not copied into the
        // caller's output, because LeechCore does not define its contents.
        for gap in gaps {
            let buffer: Box<[u8]> = unsafe {
                Box::from_raw(ptr::slice_from_raw_parts_mut(
                    gap.gap_buffer,
                    gap.gap_buffer_len,
                ))
            };

            if page_success[gap.scatter_index] {
                let out_buffer = unsafe {
                    slice::from_raw_parts_mut(gap.out_buffer, gap.out_end - gap.out_start)
                };
                out_buffer.copy_from_slice(&buffer[gap.out_start..gap.out_end]);
            }
        }

        // free temporary buffers
        unsafe {
            LcMemFree(mems as *mut c_void);
        };

        let results = coalesce_read_results(&page_spans, &page_success).map_err(|message| {
            Error(ErrorOrigin::Connector, ErrorKind::InvalidMemorySize).log_error(message)
        })?;

        // Emit maximal contiguous success/failure spans for each original
        // request. In particular, a failed page inside a larger request no
        // longer causes its untouched bytes to be reported as successfully
        // read memory.
        for result in results {
            let (_, meta_addr, out) = &mut vec[result.request_index];
            let result_buffer = unsafe {
                slice::from_raw_parts_mut(out.as_mut_ptr().add(result.offset), result.len)
            };
            let callback_data = CTup2(*meta_addr + result.offset, result_buffer.into());
            let keep_going = if result.success {
                opt_call(data.out.as_deref_mut(), callback_data)
            } else {
                opt_call(data.out_fail.as_deref_mut(), callback_data)
            };
            if !keep_going {
                break;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn entry(pa: u64, cb: u64, pa_remap: u64) -> LC_MEMMAP_ENTRY {
        LC_MEMMAP_ENTRY {
            pa,
            cb,
            paRemap: pa_remap,
        }
    }

    #[test]
    fn converts_sorts_and_preserves_memory_map_ranges() {
        let ranges = convert_memory_map_entries(&[
            entry(0x1_0000, 0x2_0000, 0x3000),
            entry(0, 0xa000, 0x1000),
            entry(0xa000, 0x1000, 0xb000),
        ])
        .expect("valid memory map");

        assert_eq!(
            ranges,
            vec![
                LeechCorePhysicalRange {
                    start: 0,
                    end: 0xa000,
                    source_offset: 0x1000,
                },
                LeechCorePhysicalRange {
                    start: 0xa000,
                    end: 0xb000,
                    source_offset: 0xb000,
                },
                LeechCorePhysicalRange {
                    start: 0x1_0000,
                    end: 0x3_0000,
                    source_offset: 0x3000,
                },
            ]
        );
        assert_eq!(ranges[0].len(), 0xa000);
        assert!(!ranges[0].is_empty());
    }

    #[test]
    fn rejects_empty_overflowing_and_overlapping_memory_map_ranges() {
        assert!(convert_memory_map_entries(&[entry(0x1000, 0, 0)]).is_err());
        assert!(convert_memory_map_entries(&[entry(u64::MAX, 2, 0)]).is_err());
        assert!(convert_memory_map_entries(&[entry(0, 2, u64::MAX)]).is_err());
        assert!(convert_memory_map_entries(&[
            entry(0x1000, 0x2000, 0),
            entry(0x2000, 0x1000, 0x3000),
        ])
        .is_err());
    }

    #[test]
    fn splits_and_coalesces_page_results_per_original_request() {
        let pages = [
            ReadPageSpan {
                request_index: 0,
                offset: 0,
                len: 0x800,
            },
            ReadPageSpan {
                request_index: 0,
                offset: 0x800,
                len: 0x1000,
            },
            ReadPageSpan {
                request_index: 0,
                offset: 0x1800,
                len: 0x1000,
            },
            ReadPageSpan {
                request_index: 0,
                offset: 0x2800,
                len: 0x400,
            },
            ReadPageSpan {
                request_index: 1,
                offset: 0,
                len: 0x1000,
            },
        ];

        let results = coalesce_read_results(&pages, &[true, true, false, true, true])
            .expect("valid page result layout");

        assert_eq!(
            results,
            vec![
                ReadResultSpan {
                    request_index: 0,
                    offset: 0,
                    len: 0x1800,
                    success: true,
                },
                ReadResultSpan {
                    request_index: 0,
                    offset: 0x1800,
                    len: 0x1000,
                    success: false,
                },
                ReadResultSpan {
                    request_index: 0,
                    offset: 0x2800,
                    len: 0x400,
                    success: true,
                },
                ReadResultSpan {
                    request_index: 1,
                    offset: 0,
                    len: 0x1000,
                    success: true,
                },
            ]
        );
    }

    #[test]
    fn rejects_inconsistent_page_result_bookkeeping() {
        let page = ReadPageSpan {
            request_index: 0,
            offset: 0,
            len: 0x1000,
        };
        assert!(coalesce_read_results(&[page], &[]).is_err());
        assert!(coalesce_read_results(
            &[
                page,
                ReadPageSpan {
                    request_index: 0,
                    offset: 0x2000,
                    len: 0x1000,
                },
            ],
            &[true, true],
        )
        .is_err());
        assert!(coalesce_read_results(
            &[ReadPageSpan {
                request_index: 0,
                offset: 0,
                len: 0,
            }],
            &[false],
        )
        .is_err());
    }

    unsafe fn count_close(handle: HANDLE) {
        let counter = unsafe { &*(handle.cast::<AtomicUsize>()) };
        counter.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn closes_native_handle_only_after_last_owner_drops() {
        let counter = Box::into_raw(Box::new(AtomicUsize::new(0)));
        let first = Arc::new(LeechCoreHandle::with_closer(
            counter.cast::<c_void>(),
            count_close,
        ));
        let second = Arc::clone(&first);

        drop(first);
        assert_eq!(unsafe { &*counter }.load(Ordering::SeqCst), 0);
        drop(second);
        assert_eq!(unsafe { &*counter }.load(Ordering::SeqCst), 1);

        unsafe { drop(Box::from_raw(counter)) };
    }
}
