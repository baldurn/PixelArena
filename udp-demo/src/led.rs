//! WS2812 strips driven over UDP, one RMT channel per strip.
//!
//! Protocol (UDP port 5002, big-endian):
//!   frame: "LEDF" | frame_id: u16 | strip: u8 | 0: u8 | count: u16 | count * (r, g, b)
//!   sync:  "LEDS" | frame_id: u16
//!
//! A frame is converted to wire order (GRB) and power-limited as soon as it
//! arrives, but only shown when a sync with the same frame_id arrives. On sync,
//! every strip holding that frame_id starts transmitting at once; strips that
//! did not receive it keep their current picture. Sync is normally broadcast so
//! every controller on the segment latches at once.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;

use esp_idf_svc::hal::gpio::{AnyOutputPin, Pin};
use esp_idf_svc::sys::{self, esp, esp_timer_get_time, EspError};
use log::{info, warn};

pub const NUM_STRIPS: usize = 4;
pub const LEDS_PER_STRIP: usize = 144;
pub const LED_PORT: u16 = 5002;
/// Current allowed per strip; frames are scaled down to fit.
/// Each WS2812 colour channel draws ~20 mA at full brightness.
const POWER_BUDGET_MA: u32 = 1000;
const MA_PER_CHANNEL: u32 = 20;
/// RMT memory per channel. The ESP32 has 512 symbols shared by all channels;
/// 128 leaves the refill ISR 80 us of slack per half block.
const RMT_SYMBOLS_PER_CHANNEL: usize = 512 / NUM_STRIPS;

const LED_TASK_PRIORITY: u32 = 15;

const FRAME_MAGIC: &[u8; 4] = b"LEDF";
const SYNC_MAGIC: &[u8; 4] = b"LEDS";
const FRAME_HEADER: usize = 10;

pub struct LedStats {
    pub frames: AtomicU32,
    /// Syncs that started at least one strip
    pub shown: AtomicU32,
    /// Syncs that matched no pending frame
    pub missed: AtomicU32,
    pub bad_packets: AtomicU32,
    /// Microseconds from sync packet received to the first strip started
    pub last_latency_us: AtomicU32,
    pub max_latency_us: AtomicU32,
    /// Microseconds between the first and last strip starting
    pub last_skew_us: AtomicU32,
    pub max_skew_us: AtomicU32,
}

pub static STATS: LedStats = LedStats {
    frames: AtomicU32::new(0),
    shown: AtomicU32::new(0),
    missed: AtomicU32::new(0),
    bad_packets: AtomicU32::new(0),
    last_latency_us: AtomicU32::new(0),
    max_latency_us: AtomicU32::new(0),
    last_skew_us: AtomicU32::new(0),
    max_skew_us: AtomicU32::new(0),
};

/// Single writer, so no fetch_max (which also trips an Xtensa LLVM bug)
fn record(last: &AtomicU32, max: &AtomicU32, value: u32) {
    last.store(value, Ordering::Relaxed);
    if value > max.load(Ordering::Relaxed) {
        max.store(value, Ordering::Relaxed);
    }
}

/// WS2812 output using ESP-IDF's RMT TX channel and bytes encoder directly.
/// (esp-idf-hal 0.47's encoder wrapper crashed with InstrFetchProhibited here.)
struct Strip {
    channel: sys::rmt_channel_handle_t,
    encoder: sys::rmt_encoder_handle_t,
    /// Next picture, in GRB wire order. Also the transmit buffer: the LED
    /// thread waits for transmission to finish before reading more packets.
    pending: [u8; LEDS_PER_STRIP * 3],
    pending_id: Option<u16>,
}

/// One RMT symbol: high for `high` ticks, then low for `low` ticks.
fn symbol(high: u32, low: u32) -> sys::rmt_symbol_word_t {
    sys::rmt_symbol_word_t {
        val: high | (1 << 15) | (low << 16),
    }
}

impl Strip {
    fn new(gpio: i32) -> Result<Self, EspError> {
        let mut channel = core::ptr::null_mut();
        let mut encoder = core::ptr::null_mut();
        let channel_config = sys::rmt_tx_channel_config_t {
            gpio_num: gpio,
            clk_src: sys::soc_periph_rmt_clk_src_t_RMT_CLK_SRC_DEFAULT,
            // 10 MHz -> 0.1 us ticks
            resolution_hz: 10_000_000,
            mem_block_symbols: RMT_SYMBOLS_PER_CHANNEL,
            trans_queue_depth: 4,
            ..Default::default()
        };
        // WS2812B: T0H 0.4 / T0L 0.85, T1H 0.8 / T1L 0.45 us
        let mut encoder_config = sys::rmt_bytes_encoder_config_t {
            bit0: symbol(4, 9),
            bit1: symbol(8, 5),
            ..Default::default()
        };
        encoder_config.flags.set_msb_first(1);
        unsafe {
            esp!(sys::rmt_new_tx_channel(&channel_config, &mut channel))?;
            esp!(sys::rmt_new_bytes_encoder(&encoder_config, &mut encoder))?;
            esp!(sys::rmt_enable(channel))?;
        }
        Ok(Self {
            channel,
            encoder,
            pending: [0; LEDS_PER_STRIP * 3],
            pending_id: None,
        })
    }

    /// Starts sending the pending picture; call `wait` before modifying it.
    fn start(&mut self) -> Result<(), EspError> {
        let config = sys::rmt_transmit_config_t::default();
        unsafe {
            esp!(sys::rmt_transmit(
                self.channel,
                self.encoder,
                self.pending.as_ptr().cast(),
                self.pending.len(),
                &config,
            ))
        }
    }

    fn wait(&mut self) -> Result<(), EspError> {
        unsafe { esp!(sys::rmt_tx_wait_all_done(self.channel, 100)) }
    }
}

/// Converts RGB triplets to GRB wire order, scaling to stay within the power budget.
fn prepare(rgb: &[u8], grb: &mut [u8; LEDS_PER_STRIP * 3]) {
    let total: u32 = rgb.iter().map(|&c| c as u32).sum();
    let budget = POWER_BUDGET_MA * 255 / MA_PER_CHANNEL;
    let scale = |c: u8| {
        if total > budget {
            (c as u32 * budget / total) as u8
        } else {
            c
        }
    };
    grb.fill(0);
    for (out, px) in grb.chunks_exact_mut(3).zip(rgb.chunks_exact(3)) {
        out[0] = scale(px[1]);
        out[1] = scale(px[0]);
        out[2] = scale(px[2]);
    }
}

fn now_us() -> i64 {
    unsafe { esp_timer_get_time() }
}

/// Runs the LED receiver. `ready` is signalled once every strip has been
/// driven successfully.
pub fn run(
    pins: [AnyOutputPin<'static>; NUM_STRIPS],
    socket: UdpSocket,
    ready: Sender<()>,
) -> anyhow::Result<()> {
    // Run above the main loop so a sync is never queued behind echo/OTA work
    unsafe { sys::vTaskPrioritySet(core::ptr::null_mut(), LED_TASK_PRIORITY) };

    // `pins` is held for the lifetime of the thread so nothing else can claim them
    let mut strips = Vec::with_capacity(NUM_STRIPS);
    for pin in &pins {
        strips.push(Strip::new(pin.pin() as i32)?);
    }

    // Start dark
    for strip in strips.iter_mut() {
        strip.start()?;
    }
    for strip in strips.iter_mut() {
        strip.wait()?;
    }

    let _ = ready.send(());
    let gpios: Vec<_> = pins.iter().map(|p| p.pin()).collect();
    info!(
        "LED: {NUM_STRIPS} strips x {LEDS_PER_STRIP} LEDs on GPIO {gpios:?}, UDP port {LED_PORT}, \
         budget {POWER_BUDGET_MA} mA per strip"
    );

    let mut buf = [0u8; 1500];
    loop {
        let (len, from) = socket.recv_from(&mut buf)?;
        let received = now_us();
        let pkt = &buf[..len];

        if len >= 6 && &pkt[..4] == SYNC_MAGIC {
            let id = u16::from_be_bytes([pkt[4], pkt[5]]);
            let mut first_start = None;
            let mut last_start = 0;
            for strip in strips.iter_mut().filter(|s| s.pending_id == Some(id)) {
                let t = now_us();
                strip.start()?;
                first_start.get_or_insert(t);
                last_start = t;
            }
            let Some(first_start) = first_start else {
                STATS.missed.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            for strip in strips.iter_mut().filter(|s| s.pending_id == Some(id)) {
                strip.wait()?;
            }
            STATS.shown.fetch_add(1, Ordering::Relaxed);
            record(
                &STATS.last_latency_us,
                &STATS.max_latency_us,
                (first_start - received) as u32,
            );
            record(
                &STATS.last_skew_us,
                &STATS.max_skew_us,
                (last_start - first_start) as u32,
            );
        } else if len >= FRAME_HEADER && &pkt[..4] == FRAME_MAGIC {
            let id = u16::from_be_bytes([pkt[4], pkt[5]]);
            let index = pkt[6] as usize;
            let count = u16::from_be_bytes([pkt[8], pkt[9]]) as usize;
            if index >= NUM_STRIPS || count > LEDS_PER_STRIP || len < FRAME_HEADER + count * 3 {
                STATS.bad_packets.fetch_add(1, Ordering::Relaxed);
                warn!("LED: bad frame from {from}: strip {index}, count {count}, {len} bytes");
                continue;
            }
            let strip = &mut strips[index];
            prepare(&pkt[FRAME_HEADER..FRAME_HEADER + count * 3], &mut strip.pending);
            strip.pending_id = Some(id);
            STATS.frames.fetch_add(1, Ordering::Relaxed);
        } else {
            STATS.bad_packets.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub fn stats_summary() -> String {
    format!(
        "led frames {} shown {} missed {} bad {} latency {}us max {}us skew {}us max {}us",
        STATS.frames.load(Ordering::Relaxed),
        STATS.shown.load(Ordering::Relaxed),
        STATS.missed.load(Ordering::Relaxed),
        STATS.bad_packets.load(Ordering::Relaxed),
        STATS.last_latency_us.load(Ordering::Relaxed),
        STATS.max_latency_us.load(Ordering::Relaxed),
        STATS.last_skew_us.load(Ordering::Relaxed),
        STATS.max_skew_us.load(Ordering::Relaxed),
    )
}
