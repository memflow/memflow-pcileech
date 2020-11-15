// https://github.com/ufrisk/pcileech/blob/master/pcileech/device.c

use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::Arc;
use std::sync::Mutex;

use libc::malloc;
use log::{error, info, warn};

use memflow::*;
use memflow_derive::connector;

use leechcore_sys::*;

fn build_lc_config(device: &str) -> LC_CONFIG {
    let cdevice = unsafe { &*(device.as_bytes() as *const [u8] as *const [i8]) };
    let mut adevice: [c_char; 260] = [0; 260];
    //    adevice.clone_from_slice(unsafe { &*(cdevice.as_bytes_with_nul() as *const [u8] as *const [i8]) });
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
}

unsafe impl Send for PciLeech {}

impl Clone for PciLeech {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

// TODO: proper drop + free impl -> LcMemFree(pLcErrorInfo);
impl PciLeech {
    pub fn new(device: &str) -> Result<Self> {
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

        error!("fVolatile: {:?}", conf.fVolatile);
        error!("fWritable: {:?}", conf.fWritable);
        error!("paMax: {:?}", conf.paMax);

        Ok(Self {
            handle: Arc::new(Mutex::new(handle)),
            metadata: PhysicalMemoryMetadata {
                size: conf.paMax as usize,
                readonly: if conf.fVolatile == 0 { true } else { false },
                // TODO: writable
            },
        })
    }
}

impl PhysicalMemory for PciLeech {
    fn phys_read_raw_list(&mut self, data: &mut [PhysicalReadData]) -> Result<()> {
        let handle = self.handle.lock().unwrap();

        // TODO: everything apart from 0x1000 byte buffers crashes...
        for read in data.iter_mut() {
            // TODO: ensure reading just 1 page...
            // TODO: handle page boundaries
            if read.1.len() < 0x1000 {
                let mut page = [0u8; 0x1000];
                let aligned = read.0.address().as_page_aligned(0x1000);
                unsafe {
                    LcRead(
                        *handle,
                        aligned.as_u64(),
                        page.len() as u32,
                        page.as_mut_ptr(),
                    )
                };
                let offs = (read.0.as_u64() - aligned.as_u64()) as usize;
                read.1.copy_from_slice(&page[offs..offs + read.1.len()]);
            } else {
                // TODO: handle multiple pages at once
                // TODO: handle page alignment
                unsafe {
                    LcRead(
                        *handle,
                        read.0.as_u64(),
                        read.1.len() as u32,
                        read.1.as_mut_ptr(),
                    )
                };
            }
        }
        Ok(())
    }

    fn phys_write_raw_list(&mut self, data: &[PhysicalWriteData]) -> Result<()> {
        let handle = self.handle.lock().unwrap();

        // TODO: everything apart from 0x1000 byte buffers crashes...
        unsafe {
            for write in data.iter() {
                LcWrite(
                    *handle,
                    write.0.as_u64(),
                    write.1.len() as u32,
                    write.1.as_ptr() as *mut u8,
                );

                /*
                // TODO: ensure reading just 1 page...
                // TODO: handle page boundaries
                if write.1.len() < 0x1000 {
                    let mut page = [0u8; 0x1000];
                    let aligned = write.0.address().as_page_aligned(0x1000);
                    unsafe {
                        LcRead(
                            self.handle,
                            aligned.as_u64(),
                            page.len() as u32,
                            page.as_mut_ptr(),
                        )
                    };
                    let offs = (read.0.as_u64() - aligned.as_u64()) as usize;
                    read.1.copy_from_slice(
                        &page[offs..offs+read.1.len()],
                    );
                } else {
                    // TODO: handle multiple pages at once
                    // TODO: handle page alignment
                    unsafe {
                        LcRead(
                            self.handle,
                            read.0.as_u64(),
                            read.1.len() as u32,
                            read.1.as_mut_ptr(),
                        )
                    };
                }
                */
            }
        }
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
