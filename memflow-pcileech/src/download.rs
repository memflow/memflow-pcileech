use log::{error, info, warn};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Cursor, Read, Write};
use std::path::PathBuf;
use zip::ZipArchive;

use memflow::error::{Error, ErrorKind, ErrorOrigin, Result};

#[cfg(feature = "download_progress")]
use {
    indicatif::{ProgressBar, ProgressStyle},
    progress_streams::ProgressReader,
    std::sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    std::sync::Arc,
};

// Windows
#[cfg(all(target_os = "windows", target_arch = "x86"))]
pub fn download_url() -> (&'static str, &'static str, &'static str) {
    (
        "https://ftdichip.com/wp-content/uploads/2025/03/Winusb_D3XX_Release_1.4.0.0.zip",
        "WU_FTD3XXLib/Lib/Dynamic/x86/FTD3XXWU.dll",
        "xxx",
    )
}
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub fn download_url() -> (&'static str, &'static str, &'static str) {
    (
        "https://ftdichip.com/wp-content/uploads/2025/03/Winusb_D3XX_Release_1.4.0.0.zip",
        "WU_FTD3XXLib/Lib/Dynamic/x64/FTD3XXWU.dll",
        "f0315b7f20ebdf1303082b63d6dd598ff7d98d3b738fc7444d000a4b64913666",
    )
}
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
pub fn download_url() -> (&'static str, &'static str, &'static str) {
    (
        "https://ftdichip.com/wp-content/uploads/2025/03/Winusb_D3XX_Release_1.4.0.0.zip",
        "WU_FTD3XXLib/Lib/Dynamic/ARM64/FTD3XXWU.dll",
        "xxx",
    )
}

// TODO: linux
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn download_url() -> (&'static str, &'static str, &'static str) {
    (
        "https://ftdichip.com/wp-content/uploads/2023/11/FTD3XXLibrary_v1.3.0.8.zip",
        "FTD3XXLibrary_v1.3.0.8/x64/DLL/FTD3XX.dll",
        "1234",
    )
}

// TODO: mac

fn download_file(url: &str) -> Result<Vec<u8>> {
    info!("downloading file from {}", url);
    let resp = ureq::get(url).call().map_err(|_| {
        Error(ErrorOrigin::Connector, ErrorKind::Http).log_error("unable to download file")
    })?;

    assert!(resp.has("Content-Length"));
    let len = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap();

    let mut reader = resp.into_reader();
    let buffer = read_to_end(&mut reader, len)?;

    assert_eq!(buffer.len(), len);
    Ok(buffer)
}

#[cfg(feature = "download_progress")]
fn read_to_end<T: Read>(reader: &mut T, len: usize) -> Result<Vec<u8>> {
    let mut buffer = vec![];

    let total = Arc::new(AtomicUsize::new(0));
    let mut reader = ProgressReader::new(reader, |progress: usize| {
        total.fetch_add(progress, Ordering::SeqCst);
    });
    let pb = ProgressBar::new(len as u64);
    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("#>-"));

    let finished = Arc::new(AtomicBool::new(false));
    let thread = {
        let finished_thread = finished.clone();
        let total_thread = total.clone();

        std::thread::spawn(move || {
            while !finished_thread.load(Ordering::Relaxed) {
                pb.set_position(total_thread.load(Ordering::SeqCst) as u64);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            pb.finish_with_message("downloaded");
        })
    };

    reader.read_to_end(&mut buffer).map_err(|_| {
        Error(ErrorOrigin::Connector, ErrorKind::Http).log_error("unable to read from http request")
    })?;
    finished.store(true, Ordering::Relaxed);
    thread.join().unwrap();

    Ok(buffer)
}

#[cfg(not(feature = "download_progress"))]
fn read_to_end<T: Read>(reader: &mut T, _len: usize) -> Result<Vec<u8>> {
    let mut buffer = vec![];
    reader.read_to_end(&mut buffer).map_err(|_| {
        Error(ErrorOrigin::Connector, ErrorKind::Http).log_error("unable to read from http request")
    })?;
    Ok(buffer)
}

pub fn download_driver() -> Result<()> {
    let (url, file_to_extract, file_checksum) = download_url();

    let file_to_extract_path: PathBuf = file_to_extract.parse().unwrap();
    let file_to_extract_name = file_to_extract_path.file_name().unwrap().to_str().unwrap();

    // Get the current executable directory
    let exe_path = std::env::current_exe().expect("Failed to get current executable path");
    let exe_dir = exe_path
        .parent()
        .expect("Failed to get parent directory of executable");

    // Create the output path
    let output_path = exe_dir.join(file_to_extract_name);

    // TODO: check hashsum
    // Check if file exists in current path already and return Ok(())
    if output_path.exists() {
        info!("ftdi driver found");
        return Ok(());
    }

    let contents = download_file(url)?;

    // Read the zip file in memory
    let cursor = Cursor::new(contents);
    let mut archive = ZipArchive::new(cursor).map_err(|_| {
        Error(ErrorOrigin::Connector, ErrorKind::Http).log_error("Failed to parse zip archive")
    })?;

    // Find and extract the specific file
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|_| Error(ErrorOrigin::Connector, ErrorKind::UnableToReadFile))?;
        if file.name() == file_to_extract {
            info!("Found file to extract: {}", file_to_extract);

            info!("Extracting to: {}", output_path.display());

            // Create the output file
            let mut output_file = File::create(&output_path).expect(&format!(
                "Failed to create output file: {}",
                output_path.display()
            ));

            let mut file_contents = Vec::new();
            file.read_to_end(&mut file_contents).unwrap();
            let hash = format!("{:x}", Sha256::digest(&file_contents));
            if hash != file_checksum {
                error!(
                    "invalid checksum of extracted {} (found {})",
                    file_to_extract_name, hash
                );
                return Ok(());
            }

            // Copy the file content
            output_file
                .write_all(&file_contents)
                .expect("Failed to write extracted file");
            output_file.flush().unwrap();

            info!(
                "Successfully extracted {} to {}",
                file_to_extract,
                output_path.display()
            );
            return Ok(());
        }
    }

    Err(
        Error(ErrorOrigin::Connector, ErrorKind::NotFound).log_error(format!(
            "file '{}' not found in zip archive",
            file_to_extract
        )),
    )
}
