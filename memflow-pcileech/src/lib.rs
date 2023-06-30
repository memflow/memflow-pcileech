use std::path::Path;
use std::ptr;
use std::slice;

use log::{error, info};

use memflow::cglue;
use memflow::mem::phys_mem::*;
use memflow::prelude::v1::*;

mod leechcore;
use leechcore::{LeechCore, LeechCoreSys, MemReadScatter, MemWriteScatter, PAGE_SIZE};

// the absolute minimum BUF_ALIGN is 4.
// using 8 bytes as BUF_ALIGN here simplifies things a lot
// and makes our gap detection code work in cases where page boundaries would be crossed.
const BUF_ALIGN: u64 = 8;
const BUF_MIN_LEN: usize = 8;
const BUF_LEN_ALIGN: usize = 8;

cglue_impl_group!(PciLeech, ConnectorInstance<'a>, {});

#[allow(clippy::mutex_atomic)]
#[derive(Clone)]
pub struct PciLeech {
    core: LeechCoreSys,
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
        let core = LeechCoreSys::new(device, remote, mem_map.is_some(), auto_clear)?;
        Ok(Self { core, mem_map })
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

        // allocate scatter buffer
        let mut mems = vec![];

        // prepare mems
        let mut gaps = Vec::new();
        for (addr, _, out) in vec.iter_mut() {
            for (page_addr, out) in CSliceMut::from(out).page_chunks(addr.address(), PAGE_SIZE) {
                let addr_align = page_addr.to_umem() & (BUF_ALIGN - 1);
                let len_align = out.len() & (BUF_LEN_ALIGN - 1);

                if addr_align == 0 && len_align == 0 && out.len() >= BUF_MIN_LEN {
                    // properly aligned read
                    mems.push(MemReadScatter {
                        address: page_addr.to_umem(),
                        buffer: out.as_mut_ptr(),
                        buffer_len: out.len(),
                    });
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

                    mems.push(MemReadScatter {
                        address: page_addr_align,
                        buffer: buffer_ptr,
                        buffer_len,
                    });
                }
            }
        }

        // dispatch read
        self.core.read_scatter(&mems[..]).ok(); // TODO: handle error

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

        // allocate scatter buffer
        let mut mems = vec![];

        // prepare mems
        let mut gaps = Vec::new();
        for write in vec.iter() {
            for (page_addr, out) in write.2.page_chunks(write.0.into(), PAGE_SIZE) {
                let addr_align = page_addr.to_umem() & (BUF_ALIGN - 1);
                let len_align = out.len() & (BUF_LEN_ALIGN - 1);

                if addr_align == 0 && len_align == 0 && out.len() >= BUF_MIN_LEN {
                    // properly aligned write
                    mems.push(MemWriteScatter {
                        address: page_addr.to_umem(),
                        buffer: out.as_ptr(),
                        buffer_len: out.len(),
                    });
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
                    mems.push(MemWriteScatter {
                        address: page_addr_align,
                        buffer: buffer_ptr,
                        buffer_len,
                    });
                }
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
        self.core.write_scatter(&mems[..]).ok(); // TODO: handle error

        if !gaps.is_empty() {
            for gap in gaps.iter() {
                let _ = unsafe { Box::from_raw(gap.gap_buffer) };
                // drop buffer
            }
        }

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
                (self.core.pa_max() as usize - 1_usize).into(),
                self.core.pa_max() as umem,
            )
        };
        PhysicalMemoryMetadata {
            max_address,
            real_size,
            readonly: !self.core.volatile(),
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
