//! DDP (Distributed Display Protocol, http://www.3waylabs.com/ddp/) input, so
//! WLED, xLights and other lighting software can drive the display.
//!
//! A sender treats the canvas as one strip of width*height pixels, row by row
//! from the top, each row left to right (the same order as a game's frames).
//! The sender's canvas may be smaller than the display; it is then stretched
//! to fill it, each pixel repeated. Its size is given (--ddp-size) or detected
//! from where each frame ends: the push packet is the last one of a frame. A
//! frame of fewer pixels than the display keeps the display's width and has
//! fewer rows if it can, else keeps the height and has fewer columns.
//! Data packets fill a buffer at their byte offset; the packet with the push
//! flag shows it.
//!
//! Header (10 bytes, 14 with a timecode):
//!   0     flags: VV.T SRQP  version 01, Timecode, Storage, Reply, Query, Push
//!   1     sequence number (low 4 bits, 0 = not used)
//!   2     data type: C R TTT SSS  type 1 = RGB, 3 = RGBW, 4 = grey; size 3 = 8 bits
//!   3     destination id: 1 = display, 255 = all, 250 = config, 251 = status
//!   4..8  data offset in bytes, big endian
//!   8..10 data length, big endian
//!   10..14 timecode, if T is set

use std::net::SocketAddr;
use std::time::{Duration, Instant};

pub const DDP_PORT: u16 = 4048;
/// A stream counts as ended when nothing arrives for this long
pub const STREAM_TIMEOUT: Duration = Duration::from_secs(3);

const VERSION_1: u8 = 0x40;
const FLAG_TIMECODE: u8 = 0x10;
const FLAG_REPLY: u8 = 0x04;
const FLAG_QUERY: u8 = 0x02;
const FLAG_PUSH: u8 = 0x01;

const ID_DISPLAY: u8 = 1;
const ID_CONFIG: u8 = 250;
const ID_STATUS: u8 = 251;
const ID_ALL: u8 = 255;

#[derive(Debug, PartialEq)]
pub struct Packet<'a> {
    pub push: bool,
    pub query: bool,
    pub reply: bool,
    pub data_type: u8,
    pub id: u8,
    pub offset: usize,
    pub data: &'a [u8],
}

pub fn parse(buf: &[u8]) -> Result<Packet<'_>, String> {
    if buf.len() < 10 {
        return Err(format!("{} bytes is shorter than the header", buf.len()));
    }
    let flags = buf[0];
    if flags & 0xc0 != VERSION_1 {
        return Err(format!("unsupported version {}", flags >> 6));
    }
    let header = if flags & FLAG_TIMECODE != 0 { 14 } else { 10 };
    let offset = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    let len = u16::from_be_bytes([buf[8], buf[9]]) as usize;
    let data = buf
        .get(header..header + len)
        .ok_or_else(|| format!("header says {len} data bytes, packet has {}", buf.len().saturating_sub(header)))?;
    Ok(Packet {
        push: flags & FLAG_PUSH != 0,
        query: flags & FLAG_QUERY != 0,
        reply: flags & FLAG_REPLY != 0,
        data_type: buf[2],
        id: buf[3],
        offset,
        data,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Format {
    Rgb,
    Rgbw,
    Grey,
}

impl Format {
    fn of(data_type: u8) -> Result<Self, String> {
        // 0 is "undefined" and 0x01 is what older senders mean by RGB
        if data_type <= 0x01 {
            return Ok(Format::Rgb);
        }
        let size = data_type & 0x07;
        if data_type & 0x80 != 0 || !(size == 0 || size == 3) {
            return Err(format!("data type {data_type:#04x}: only 8-bit RGB, RGBW and grey are supported"));
        }
        match (data_type >> 3) & 0x07 {
            0 | 1 => Ok(Format::Rgb),
            3 => Ok(Format::Rgbw),
            4 => Ok(Format::Grey),
            t => Err(format!("data type {data_type:#04x}: pixel type {t} is not supported")),
        }
    }

    fn bytes(self) -> usize {
        match self {
            Format::Rgb => 3,
            Format::Rgbw => 4,
            Format::Grey => 1,
        }
    }
}

/// What a packet asks of the server
pub enum Action {
    Nothing,
    /// The first packet of a new stream: it takes over the display
    StreamStarted,
    /// Send this packet back to the sender
    Reply(Vec<u8>),
}

pub struct Receiver {
    display_w: usize,
    display_h: usize,
    /// The sender's canvas size is given rather than detected
    fixed: bool,
    /// The sender's canvas
    pub width: usize,
    pub height: usize,
    /// Where the data of the frame being received ends, in bytes
    frame_end: usize,
    /// For each display pixel, the sender pixel it shows
    map: Vec<usize>,
    format: Format,
    /// Raw pixels in `format`, as received so far for the next frame
    staging: Vec<u8>,
    /// The last pushed frame, rgb24
    frame: Option<Vec<u8>>,
    pub source: Option<SocketAddr>,
    last_packet: Option<Instant>,
    pub frames: u64,
    last_error: Option<Instant>,
}

impl Receiver {
    /// For a display_w x display_h display. `size` is the sender's canvas, None to detect it.
    pub fn new(size: Option<(usize, usize)>, display_w: usize, display_h: usize) -> Self {
        let (width, height) = size.unwrap_or((display_w, display_h));
        let mut r = Self {
            display_w,
            display_h,
            fixed: size.is_some(),
            width: 0,
            height: 0,
            frame_end: 0,
            map: Vec::new(),
            format: Format::Rgb,
            staging: Vec::new(),
            frame: None,
            source: None,
            last_packet: None,
            frames: 0,
            last_error: None,
        };
        r.resize(width, height);
        r.staging = vec![0; r.capacity() * 3];
        r
    }

    pub fn detects_size(&self) -> bool {
        !self.fixed
    }

    /// Pixels the staging buffer holds: the given canvas, or as many as the display has
    pub fn capacity(&self) -> usize {
        if self.fixed { self.width * self.height } else { self.display_w * self.display_h }
    }

    fn resize(&mut self, width: usize, height: usize) {
        let (dw, dh) = (self.display_w, self.display_h);
        self.width = width;
        self.height = height;
        self.map = (0..dh)
            .flat_map(|y| (0..dw).map(move |x| (y * height / dh) * width + x * width / dw))
            .collect();
    }

    /// The sender's canvas, from how many pixels a frame has
    fn detect(&self, pixels: usize) -> (usize, usize) {
        let (w, h) = (self.display_w, self.display_h);
        if pixels >= w * h || pixels == 0 {
            (w, h)
        } else if pixels % w == 0 {
            (w, pixels / w)
        } else if pixels % h == 0 {
            (pixels / h, h)
        } else {
            // No whole number of rows or columns: show it unstretched
            (w, h)
        }
    }

    pub fn live(&self, now: Instant) -> bool {
        self.last_packet.is_some_and(|t| now - t < STREAM_TIMEOUT)
    }

    /// The frame to show, if a stream is live. Ends a stream that timed out.
    pub fn current(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.last_packet.is_some() && !self.live(now) {
            if let Some(source) = self.source {
                println!("ddp: stream from {source} ended, {} frames", self.frames);
            }
            self.last_packet = None;
            self.frame = None;
            self.frames = 0;
        }
        self.frame.clone()
    }

    pub fn handle(&mut self, buf: &[u8], from: SocketAddr, status: &dyn Fn(u8) -> Option<String>) -> Action {
        let packet = match parse(buf) {
            Ok(p) => p,
            Err(e) => return self.error(from, e),
        };
        if packet.reply {
            return Action::Nothing;
        }
        if packet.query {
            return match status(packet.id) {
                Some(json) => Action::Reply(reply(packet.id, json.as_bytes())),
                None => Action::Nothing,
            };
        }
        if !matches!(packet.id, ID_DISPLAY | ID_ALL) {
            return Action::Nothing;
        }
        let format = match Format::of(packet.data_type) {
            Ok(f) => f,
            Err(e) => return self.error(from, e),
        };
        // A push with no data (type 0) shows what is buffered in the current format
        if format != self.format && !packet.data.is_empty() {
            self.format = format;
            self.staging = vec![0; self.capacity() * format.bytes()];
        }

        let now = Instant::now();
        let started = !self.live(now);
        if started {
            self.frames = 0;
            self.frame = None;
            self.frame_end = 0;
            println!("ddp: stream from {from} started");
        }
        self.last_packet = Some(now);
        self.source = Some(from);

        let end = self.staging.len();
        if packet.offset < end {
            let n = packet.data.len().min(end - packet.offset);
            self.staging[packet.offset..packet.offset + n].copy_from_slice(&packet.data[..n]);
        }
        self.frame_end = self.frame_end.max(packet.offset + packet.data.len());
        if packet.push {
            let pixels = self.frame_end.div_ceil(self.format.bytes());
            self.frame_end = 0;
            if !self.fixed && pixels > 0 {
                let (w, h) = self.detect(pixels);
                if (w, h) != (self.width, self.height) {
                    println!(
                        "ddp: {pixels} pixels per frame from {from}: a {w} x {h} canvas, shown on {} x {}",
                        self.display_w, self.display_h
                    );
                    self.resize(w, h);
                }
            }
            self.frame = Some(self.to_rgb());
            self.frames += 1;
        }
        if started { Action::StreamStarted } else { Action::Nothing }
    }

    /// The display frame, rgb24
    fn to_rgb(&self) -> Vec<u8> {
        let n = self.format.bytes();
        self.map
            .iter()
            .flat_map(|&i| {
                let p = &self.staging[i * n..i * n + n];
                match self.format {
                    Format::Rgb => [p[0], p[1], p[2]],
                    Format::Grey => [p[0]; 3],
                    Format::Rgbw => [p[0].saturating_add(p[3]), p[1].saturating_add(p[3]), p[2].saturating_add(p[3])],
                }
            })
            .collect()
    }

    /// Logged at most every few seconds, so a misconfigured sender does not flood the log
    fn error(&mut self, from: SocketAddr, message: String) -> Action {
        let now = Instant::now();
        if self.last_error.map_or(true, |t| now - t >= Duration::from_secs(5)) {
            self.last_error = Some(now);
            eprintln!("ddp: ignoring packet from {from}: {message}");
        }
        Action::Nothing
    }
}

/// A reply to a query, carrying JSON
fn reply(id: u8, json: &[u8]) -> Vec<u8> {
    let mut packet = vec![VERSION_1 | FLAG_REPLY | FLAG_PUSH, 0, 0, id, 0, 0, 0, 0];
    packet.extend_from_slice(&(json.len() as u16).to_be_bytes());
    packet.extend_from_slice(json);
    packet
}

/// JSON answers to status and config queries, as the DDP spec describes them
pub fn query_answer(id: u8, pixels: usize) -> Option<String> {
    match id {
        ID_STATUS => Some(
            serde_json::json!({"status": {"man": "OrangeMakers", "mod": "display-server",
                                          "ver": env!("CARGO_PKG_VERSION")}})
            .to_string(),
        ),
        ID_CONFIG => Some(
            serde_json::json!({"config": {"ports": [{"port": 0, "ts": 0, "l": pixels, "ss": 0}]}}).to_string(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(flags: u8, data_type: u8, id: u8, offset: u32, data: &[u8]) -> Vec<u8> {
        let mut p = vec![flags, 1, data_type, id];
        p.extend_from_slice(&offset.to_be_bytes());
        p.extend_from_slice(&(data.len() as u16).to_be_bytes());
        p.extend_from_slice(data);
        p
    }

    fn from() -> SocketAddr {
        "192.168.50.20:4048".parse().unwrap()
    }

    fn no_status(_: u8) -> Option<String> {
        None
    }

    #[test]
    fn parses_header_and_timecode() {
        let p = packet(0x41, 0x0b, 1, 300, &[1, 2, 3]);
        let parsed = parse(&p).unwrap();
        assert!(parsed.push && !parsed.query);
        assert_eq!((parsed.data_type, parsed.id, parsed.offset, parsed.data), (0x0b, 1, 300, &[1u8, 2, 3][..]));

        let mut t = packet(0x51, 0x0b, 1, 0, &[]);
        t[9] = 2;
        t.extend_from_slice(&[0, 0, 0, 0, 7, 8]);
        assert_eq!(parse(&t).unwrap().data, &[7, 8]);

        assert!(parse(&packet(0x81, 0x0b, 1, 0, &[1])).is_err());
        let mut short = packet(0x41, 0x0b, 1, 0, &[1, 2, 3]);
        short.pop();
        assert!(parse(&short).is_err());
    }

    #[test]
    fn assembles_frame_from_packets() {
        let mut r = Receiver::new(None, 4, 1);
        assert!(matches!(r.handle(&packet(0x40, 0x0b, 1, 0, &[1, 2, 3, 4, 5, 6]), from(), &no_status),
                         Action::StreamStarted));
        assert_eq!(r.current(Instant::now()), None);
        // Data past the end of the canvas is dropped
        r.handle(&packet(0x41, 0x0b, 1, 6, &[7, 8, 9, 10, 11, 12, 13, 14]), from(), &no_status);
        assert_eq!(r.current(Instant::now()), Some(vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]));
        assert_eq!(r.frames, 1);
    }

    #[test]
    fn converts_rgbw_and_grey() {
        let mut r = Receiver::new(None, 2, 1);
        r.handle(&packet(0x41, 0x1b, 1, 0, &[10, 20, 30, 5, 250, 0, 0, 10]), from(), &no_status);
        assert_eq!(r.current(Instant::now()), Some(vec![15, 25, 35, 255, 10, 10]));
        r.handle(&packet(0x41, 0x23, 255, 0, &[9, 200]), from(), &no_status);
        assert_eq!(r.current(Instant::now()), Some(vec![9, 9, 9, 200, 200, 200]));
    }

    #[test]
    fn stretches_smaller_canvas() {
        // 2x1 sender on a 4x2 display: every pixel twice across and twice down
        let mut r = Receiver::new(Some((2, 1)), 4, 2);
        r.handle(&packet(0x41, 0x0b, 1, 0, &[1, 2, 3, 4, 5, 6]), from(), &no_status);
        let row = [1, 2, 3, 1, 2, 3, 4, 5, 6, 4, 5, 6];
        assert_eq!(r.current(Instant::now()), Some([row, row].concat()));
    }

    #[test]
    fn detects_smaller_canvas() {
        // A 4x4 display; the sender draws 4x2, then 2x4, then the full size
        let mut r = Receiver::new(None, 4, 4);
        let half: Vec<u8> = (1..=24).collect();
        r.handle(&packet(0x40, 0x0b, 1, 0, &half[..12]), from(), &no_status);
        r.handle(&packet(0x41, 0x0b, 1, 12, &half[12..]), from(), &no_status);
        assert_eq!((r.width, r.height), (4, 2));
        let frame = r.current(Instant::now()).unwrap();
        assert_eq!(&frame[..12], &half[..12]);
        assert_eq!(&frame[12..24], &half[..12]);
        assert_eq!(&frame[24..36], &half[12..]);

        r.handle(&packet(0x41, 0x0b, 1, 0, &half), from(), &no_status);
        assert_eq!((r.width, r.height), (4, 2));
        r.handle(&packet(0x41, 0x0b, 1, 0, &[0; 6 * 3]), from(), &no_status);
        assert_eq!((r.width, r.height), (4, 4), "6 pixels is not whole rows or columns");
        r.handle(&packet(0x41, 0x0b, 1, 0, &[0; 48]), from(), &no_status);
        assert_eq!((r.width, r.height), (4, 4));
    }

    #[test]
    fn answers_status_query() {
        let mut r = Receiver::new(None, 2, 1);
        let Action::Reply(p) = r.handle(&packet(0x42, 0, 251, 0, &[]), from(), &|id| query_answer(id, 2)) else {
            panic!("no reply")
        };
        let parsed = parse(&p).unwrap();
        assert!(parsed.reply && parsed.push);
        let json: serde_json::Value = serde_json::from_slice(parsed.data).unwrap();
        assert_eq!(json["status"]["mod"], "display-server");
        assert!(!r.live(Instant::now()));
    }
}
