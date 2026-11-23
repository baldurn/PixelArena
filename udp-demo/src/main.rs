use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use esp_idf_svc::eth::{BlockingEth, EspEth, EthDriver, RmiiClockConfig, RmiiEthChipset};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::ota::EspOta;
use log::{info, warn};

mod led;
mod ota;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const ECHO_PORT: u16 = 5000;
const HEARTBEAT_PORT: u16 = 5001;
const OTA_CHECK_INTERVAL: Duration = Duration::from_secs(300);

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let fw = ota::running_sha256().map(|s| ota::hex(&s)).unwrap_or_default();
    info!("udp-demo {VERSION}, firmware sha256 {fw}");

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;
    let sysloop = EspSystemEventLoop::take()?;

    // WT32-ETH01: LAN8720 PHY at address 1, 50 MHz clock fed into GPIO0,
    // oscillator enabled via GPIO16, MDC on GPIO23, MDIO on GPIO18.
    let driver = EthDriver::new_rmii(
        peripherals.mac,
        pins.gpio25,
        pins.gpio26,
        pins.gpio27,
        pins.gpio23,
        pins.gpio22,
        pins.gpio21,
        pins.gpio19,
        pins.gpio18,
        RmiiClockConfig::Input(pins.gpio0),
        Some(pins.gpio16),
        RmiiEthChipset::LAN87XX,
        Some(1),
        sysloop.clone(),
    )?;

    // Default netif configuration is a DHCP client
    let mut eth = BlockingEth::wrap(EspEth::wrap(driver)?, sysloop)?;
    info!("Starting Ethernet, waiting for DHCP...");
    eth.start()?;
    eth.wait_netif_up()?;
    let ip_info = eth.eth().netif().get_ip_info()?;
    let server = ip_info.subnet.gateway;
    info!("Ethernet up: ip {} gateway/update server {server}", ip_info.ip);

    // May reboot into new firmware
    ota::check_logged(server);

    let led_socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, led::LED_PORT))?;
    let led_pins = [
        pins.gpio2.degrade_output(),
        pins.gpio4.degrade_output(),
        pins.gpio14.degrade_output(),
        pins.gpio32.degrade_output(),
    ];
    let (led_ready, led_ready_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("led".into())
        .stack_size(12 * 1024)
        .spawn(move || {
            if let Err(e) = led::run(led_pins, led_socket, led_ready) {
                warn!("LED thread exited: {e:#}");
            }
        })?;

    // Keep this image only once networking and the LED output both work;
    // otherwise the bootloader rolls back to the previous slot on reset.
    led_ready_rx.recv_timeout(Duration::from_secs(5))?;
    EspOta::new()?.mark_running_slot_valid()?;
    info!("firmware marked valid");
    if let Some(sha) = ota::rejected_sha256() {
        warn!("previously rolled back image: {}", ota::hex(&sha));
    }

    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, ECHO_PORT))?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    info!("UDP echo on port {ECHO_PORT}, heartbeat to {server}:{HEARTBEAT_PORT}");

    let heartbeat_to = SocketAddrV4::new(server, HEARTBEAT_PORT);
    let start = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut last_ota_check = Instant::now();
    let mut heartbeat_count: u32 = 0;
    let mut buf = [0u8; 1500];

    loop {
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                let msg = String::from_utf8_lossy(&buf[..len]).into_owned();
                if msg.trim() == "check" {
                    socket.send_to(b"checking for firmware update", from)?;
                    info!("OTA check requested by {from}");
                    ota::check_logged(server);
                    last_ota_check = Instant::now();
                } else {
                    // Reply first: logging over the 115200 baud UART takes milliseconds
                    let reply = format!("ESP32 echo: {msg}");
                    socket.send_to(reply.as_bytes(), from)?;
                    info!("rx {len} bytes from {from}: {msg}");
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => warn!("recv error: {e}"),
        }

        if last_heartbeat.elapsed() >= Duration::from_secs(2) {
            last_heartbeat = Instant::now();
            heartbeat_count += 1;
            let msg = format!(
                "heartbeat {heartbeat_count} uptime {}s version {VERSION} fw {} {}",
                start.elapsed().as_secs(),
                &fw[..fw.len().min(12)],
                led::stats_summary()
            );
            if let Err(e) = socket.send_to(msg.as_bytes(), heartbeat_to) {
                warn!("heartbeat send failed: {e}");
            }
        }

        if last_ota_check.elapsed() >= OTA_CHECK_INTERVAL {
            last_ota_check = Instant::now();
            ota::check_logged(server);
        }
    }
}
