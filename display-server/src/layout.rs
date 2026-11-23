//! Which canvas pixel each LED shows: (controller, strip, LED) <-> (x, y).

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

use serde::Deserialize;

// Firmware limits, see udp-demo/src/led.rs
pub const LEDS_PER_STRIP: usize = 144;
pub const STRIPS_PER_CONTROLLER: u8 = 4;
pub const LED_PORT: u16 = 5002;
const HEARTBEAT_PORT: u16 = 5001;

#[derive(Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Right,
    Left,
    Down,
    Up,
}

/// A run of consecutive LEDs on one strip, lying in a straight line on the canvas.
#[derive(Deserialize)]
pub struct Segment {
    pub controller: Ipv4Addr,
    pub strip: u8,
    pub x: i64,
    pub y: i64,
    #[serde(default)]
    pub direction: Direction,
    pub length: usize,
    /// First LED on the strip
    #[serde(default)]
    pub start: usize,
}

pub struct Layout {
    pub width: usize,
    pub height: usize,
    /// (controller, strip) -> canvas pixel number for each LED, None if unused
    strips: BTreeMap<(Ipv4Addr, u8), Vec<Option<usize>>>,
    pub controllers: Vec<Ipv4Addr>,
}

impl Layout {
    pub fn new(width: usize, height: usize, segments: &[Segment]) -> Result<Self, String> {
        let mut strips: BTreeMap<_, Vec<Option<usize>>> = BTreeMap::new();
        for seg in segments {
            if seg.strip >= STRIPS_PER_CONTROLLER {
                return Err(format!(
                    "segment {}/{}: strip must be 0..{}",
                    seg.controller,
                    seg.strip,
                    STRIPS_PER_CONTROLLER - 1
                ));
            }
            let (dx, dy) = match seg.direction {
                Direction::Right => (1, 0),
                Direction::Left => (-1, 0),
                Direction::Down => (0, 1),
                Direction::Up => (0, -1),
            };
            let leds = strips.entry((seg.controller, seg.strip)).or_default();
            for k in 0..seg.length {
                let (x, y, i) = (seg.x + k as i64 * dx, seg.y + k as i64 * dy, seg.start + k);
                if !(0..width as i64).contains(&x) || !(0..height as i64).contains(&y) {
                    return Err(format!(
                        "segment {}/{}: LED {k} at ({x}, {y}) is off the canvas",
                        seg.controller, seg.strip
                    ));
                }
                if i >= LEDS_PER_STRIP {
                    return Err(format!(
                        "segment {}/{}: runs past LED {}",
                        seg.controller,
                        seg.strip,
                        LEDS_PER_STRIP - 1
                    ));
                }
                if leds.len() <= i {
                    leds.resize(i + 1, None);
                }
                leds[i] = Some(y as usize * width + x as usize);
            }
        }
        let mut controllers: Vec<_> = strips.keys().map(|&(c, _)| c).collect();
        controllers.dedup();
        Ok(Self { width, height, strips, controllers })
    }

    /// Each strip is a row, left to right; controllers stack top to bottom.
    pub fn default_for(controllers: &[Ipv4Addr]) -> Result<Self, String> {
        let segments: Vec<_> = controllers
            .iter()
            .enumerate()
            .flat_map(|(n, &controller)| {
                (0..STRIPS_PER_CONTROLLER).map(move |strip| Segment {
                    controller,
                    strip,
                    x: 0,
                    y: (n * STRIPS_PER_CONTROLLER as usize + strip as usize) as i64,
                    direction: Direction::Right,
                    length: LEDS_PER_STRIP,
                    start: 0,
                })
            })
            .collect();
        Self::new(
            LEDS_PER_STRIP,
            STRIPS_PER_CONTROLLER as usize * controllers.len(),
            &segments,
        )
    }

    /// frame: width*height*3 RGB bytes -> LEDF packet for each strip
    pub fn packets(&self, frame: &[u8], frame_id: u16) -> Vec<(Ipv4Addr, Vec<u8>)> {
        self.strips
            .iter()
            .map(|(&(controller, strip), leds)| {
                let mut packet = Vec::with_capacity(10 + leds.len() * 3);
                packet.extend_from_slice(b"LEDF");
                packet.extend_from_slice(&frame_id.to_be_bytes());
                packet.extend_from_slice(&[strip, 0]);
                packet.extend_from_slice(&(leds.len() as u16).to_be_bytes());
                for led in leds {
                    match led {
                        Some(p) => packet.extend_from_slice(&frame[p * 3..p * 3 + 3]),
                        None => packet.extend_from_slice(&[0, 0, 0]),
                    }
                }
                (controller, packet)
            })
            .collect()
    }
}

/// Controllers that send heartbeats within `wait`.
pub fn discover(wait: Duration) -> std::io::Result<Vec<Ipv4Addr>> {
    let socket = UdpSocket::bind(("0.0.0.0", HEARTBEAT_PORT))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut found = Vec::new();
    let mut buf = [0u8; 1500];
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if let Ok((_, std::net::SocketAddr::V4(addr))) = socket.recv_from(&mut buf) {
            found.push(*addr.ip());
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}
