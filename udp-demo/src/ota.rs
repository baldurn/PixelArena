//! Pull-based OTA: fetch a manifest from the update server and install the
//! image it names if it differs from the running firmware.
//!
//! Images are identified by the SHA-256 digest ESP-IDF appends to every app
//! image (the last 32 bytes of the .bin), which is also what
//! `esp_partition_get_sha256` reports for an installed app partition.

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{bail, Context};
use embedded_svc::http::client::Client;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::ota::EspOta;
use esp_idf_svc::sys;
use log::{info, warn};
use serde::Deserialize;

pub const SERVER_PORT: u16 = 8000;
const MANIFEST_PATH: &str = "/firmware.json";

#[derive(Deserialize)]
struct Manifest {
    sha256: String,
    url: String,
    size: usize,
}

pub enum CheckResult {
    UpToDate,
    /// The server offers the image that previously failed and was rolled back.
    Rejected,
}

pub fn running_sha256() -> Option<[u8; 32]> {
    partition_sha256(unsafe { sys::esp_ota_get_running_partition() })
}

pub fn rejected_sha256() -> Option<[u8; 32]> {
    partition_sha256(unsafe { sys::esp_ota_get_last_invalid_partition() })
}

fn partition_sha256(partition: *const sys::esp_partition_t) -> Option<[u8; 32]> {
    if partition.is_null() {
        return None;
    }
    let mut sha = [0u8; 32];
    let err = unsafe { sys::esp_partition_get_sha256(partition, sha.as_mut_ptr()) };
    (err == sys::ESP_OK).then_some(sha)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_sha256(s: &str) -> anyhow::Result<[u8; 32]> {
    if s.len() != 64 {
        bail!("bad sha256 length {}", s.len());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

fn connect(timeout: Duration) -> anyhow::Result<Client<EspHttpConnection>> {
    let conn = EspHttpConnection::new(&Configuration {
        timeout: Some(timeout),
        buffer_size: Some(4096),
        ..Default::default()
    })?;
    Ok(Client::wrap(conn))
}

fn fetch_manifest(server: Ipv4Addr) -> anyhow::Result<Manifest> {
    let url = format!("http://{server}:{SERVER_PORT}{MANIFEST_PATH}");
    let mut client = connect(Duration::from_secs(3))?;
    let mut resp = client.get(&url)?.submit()?;
    if resp.status() != 200 {
        bail!("GET {url}: HTTP {}", resp.status());
    }
    let mut body = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() > 4096 {
            bail!("manifest too large");
        }
    }
    serde_json::from_slice(&body).context("parsing manifest")
}

/// Checks the server for new firmware. On a successful update this does not
/// return: the device reboots into the new image.
pub fn check_and_update(server: Ipv4Addr) -> anyhow::Result<CheckResult> {
    let manifest = fetch_manifest(server)?;
    let wanted = parse_sha256(&manifest.sha256)?;

    if running_sha256() == Some(wanted) {
        return Ok(CheckResult::UpToDate);
    }
    if rejected_sha256() == Some(wanted) {
        return Ok(CheckResult::Rejected);
    }

    let url = if manifest.url.starts_with("http") {
        manifest.url.clone()
    } else {
        format!("http://{server}:{SERVER_PORT}{}", manifest.url)
    };
    info!("OTA: downloading {url} ({} bytes, sha256 {})", manifest.size, manifest.sha256);

    let mut ota = EspOta::new()?;
    let mut update = ota.initiate_update()?;
    let mut client = connect(Duration::from_secs(10))?;
    let mut resp = client.get(&url)?.submit()?;
    if resp.status() != 200 {
        update.abort()?;
        bail!("GET {url}: HTTP {}", resp.status());
    }

    // Stream into the inactive slot, keeping the last 32 bytes (the appended digest)
    let mut buf = vec![0u8; 4096];
    let mut total = 0usize;
    let mut tail: Vec<u8> = Vec::with_capacity(64);
    let mut next_progress = 0usize;
    loop {
        let n = match resp.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                update.abort()?;
                bail!("download failed after {total} bytes: {e:?}");
            }
        };
        if n == 0 {
            break;
        }
        update.write(&buf[..n])?;
        total += n;
        tail.extend_from_slice(&buf[..n]);
        if tail.len() > 32 {
            tail.drain(..tail.len() - 32);
        }
        if total >= next_progress {
            info!("OTA: {total}/{} bytes", manifest.size);
            next_progress += 128 * 1024;
        }
    }

    if total != manifest.size {
        update.abort()?;
        bail!("size mismatch: got {total}, manifest says {}", manifest.size);
    }
    if tail != wanted {
        update.abort()?;
        bail!("digest mismatch: image ends with {}", hex(&tail));
    }

    // esp_ota_end validates the image (including its digest) and sets it as boot slot
    update.complete()?;
    info!("OTA: update installed, rebooting");
    std::thread::sleep(Duration::from_millis(200));
    esp_idf_svc::hal::reset::restart();
}

/// Runs a check and logs the outcome; errors (e.g. server down) are not fatal.
pub fn check_logged(server: Ipv4Addr) {
    match check_and_update(server) {
        Ok(CheckResult::UpToDate) => info!("OTA: firmware is up to date"),
        Ok(CheckResult::Rejected) => {
            warn!("OTA: server offers the image that was rolled back, skipping")
        }
        Err(e) => warn!("OTA: check failed: {e:#}"),
    }
}
