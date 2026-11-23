//! A small client for the display server, shared by the demos.
//!
//! Demos work in centimetres on the display's surface: `s` across (around the
//! circle on a cylinder) and `z` down from the top. `Geometry` converts to
//! pixels, so things are round and move at the same speed in every direction
//! even though the pixels are far from square.
//!
//! Every demo takes the same arguments:
//!
//!   demo [ws://host:port/ws] [--timeout SECONDS] [--exit-on-input]
//!
//! The server address defaults to ws://localhost:8080/ws. With --timeout the
//! demo stops after that many seconds, so a script can show demos in turn
//! (see cycle.sh); a game that takes input (`run_with_input`) is not stopped
//! while someone is playing it. With --exit-on-input, a demo that takes no
//! input exits as soon as a player presses a button, so the script can start
//! a game instead. Exit status: 0 when the timeout ends it, 2 when another
//! game or a DDP stream took over the display, 3 when a player pressed a
//! button (--exit-on-input), 1 on errors.

use std::net::TcpStream;
use std::process::exit;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub struct Geometry {
    pub width: usize,
    pub height: usize,
    /// Distance between column centres, cm
    pub col_cm: f64,
    /// Distance between row centres, cm
    pub row_cm: f64,
    /// Across the whole display; the circumference on a cylinder
    pub width_cm: f64,
    pub height_cm: f64,
    pub wraps: bool,
}

impl Geometry {
    fn from_welcome(display: &Value) -> Self {
        let width = display["width"].as_u64().unwrap() as usize;
        let height = display["height"].as_u64().unwrap() as usize;
        let physical = &display["physical"];
        // Without a physical description, pixels are 1 cm squares on a flat display
        let width_cm = physical["width_cm"].as_f64().unwrap_or(width as f64);
        let height_cm = physical["height_cm"].as_f64().unwrap_or(height as f64);
        Self {
            width,
            height,
            col_cm: width_cm / width as f64,
            row_cm: height_cm / height as f64,
            width_cm,
            height_cm,
            wraps: physical["wraps_x"].as_bool().unwrap_or(false),
        }
    }

    /// Centre of column x, cm across
    pub fn s(&self, x: usize) -> f64 {
        (x as f64 + 0.5) * self.col_cm
    }

    /// Centre of row y, cm down
    pub fn z(&self, y: usize) -> f64 {
        (y as f64 + 0.5) * self.row_cm
    }

    /// Signed distance across from `from` to `to`, the short way around on a cylinder
    pub fn ds(&self, from: f64, to: f64) -> f64 {
        let d = to - from;
        if self.wraps {
            d - self.width_cm * (d / self.width_cm).round()
        } else {
            d
        }
    }

    /// Wraps a position across back onto the display
    pub fn wrap(&self, s: f64) -> f64 {
        if self.wraps { s.rem_euclid(self.width_cm) } else { s }
    }

    /// Rows whose centres are within z0..z1 cm
    pub fn rows(&self, z0: f64, z1: f64) -> std::ops::Range<usize> {
        let first = (z0 / self.row_cm - 0.5).ceil().max(0.0) as usize;
        let last = ((z1 / self.row_cm - 0.5).floor() + 1.0).clamp(0.0, self.height as f64) as usize;
        first.min(last)..last
    }

    /// Columns whose centres are within `r` cm of `s`, the short way around
    pub fn cols_near(&self, s: f64, r: f64) -> impl Iterator<Item = usize> + '_ {
        let centre = (s / self.col_cm - 0.5).round() as i64;
        let reach = (r / self.col_cm).ceil() as i64 + 1;
        let w = self.width as i64;
        (centre - reach..=centre + reach).filter_map(move |x| {
            if self.wraps {
                Some(x.rem_euclid(w) as usize)
            } else {
                (0..w).contains(&x).then_some(x as usize)
            }
        })
    }
}

pub struct Frame {
    width: usize,
    pub pixels: Vec<u8>,
}

impl Frame {
    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    /// Adds light to a pixel, saturating at full brightness
    pub fn add(&mut self, x: usize, y: usize, rgb: [f64; 3]) {
        let i = (y * self.width + x) * 3;
        for (p, c) in self.pixels[i..i + 3].iter_mut().zip(rgb) {
            *p = (*p as f64 + c.max(0.0)).min(255.0) as u8;
        }
    }

    /// Dims everything, for trails: 0.8 keeps 80 % per tick
    pub fn fade(&mut self, keep: f64) {
        for p in self.pixels.iter_mut() {
            *p = (*p as f64 * keep) as u8;
        }
    }

    /// A soft round blob of light at (s, z) cm with radius `r` cm. Across, it is
    /// shared between the two nearest columns by distance, so something moving
    /// around the ring slides smoothly from pipe to pipe instead of jumping.
    pub fn splat(&mut self, g: &Geometry, s: f64, z: f64, r: f64, rgb: [f64; 3]) {
        for x in g.cols_near(s, g.col_cm) {
            let across = 1.0 - g.ds(s, g.s(x)).abs() / g.col_cm.max(r);
            if across <= 0.0 {
                continue;
            }
            for y in g.rows(z - r, z + r) {
                let down = 1.0 - (g.z(y) - z).abs() / r;
                if down > 0.0 {
                    self.add(x, y, rgb.map(|c| c * across * down));
                }
            }
        }
    }

    pub fn set(&mut self, x: usize, y: usize, rgb: [f64; 3]) {
        let i = (y * self.width + x) * 3;
        for (p, c) in self.pixels[i..i + 3].iter_mut().zip(rgb) {
            *p = c.clamp(0.0, 255.0) as u8;
        }
    }
}

/// Full-saturation colour for a hue in 0..1, scaled to `v` (0-255)
pub fn hue(h: f64, v: f64) -> [f64; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let x = 1.0 - (h6 % 2.0 - 1.0).abs();
    let rgb = match h6 as u32 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    };
    rgb.map(|c| c * v)
}

/// Small deterministic random numbers; demos do not need a real generator
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Uniform in lo..hi
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        lo + (hi - lo) * (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    Fire,
}

impl Button {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "up" => Button::Up,
            "down" => Button::Down,
            "left" => Button::Left,
            "right" => Button::Right,
            "fire" => Button::Fire,
            _ => return None,
        })
    }
}

/// The players' buttons, from the server's input messages. Players are 1 and 2.
#[derive(Default)]
pub struct Input {
    held: [[bool; 5]; 2],
    /// Went down since the previous tick
    pressed: [[bool; 5]; 2],
    last_used: [Option<Instant>; 2],
}

impl Input {
    pub fn held(&self, player: usize, button: Button) -> bool {
        self.held[player - 1][button as usize]
    }

    /// The button went down since the last tick: for things done once per press
    pub fn pressed(&self, player: usize, button: Button) -> bool {
        self.pressed[player - 1][button as usize]
    }

    /// -1, 0 or 1 from the left and right buttons
    pub fn x(&self, player: usize) -> f64 {
        self.held(player, Button::Right) as i32 as f64 - self.held(player, Button::Left) as i32 as f64
    }

    /// -1, 0 or 1 from the up and down buttons; down is positive, like z
    pub fn y(&self, player: usize) -> f64 {
        self.held(player, Button::Down) as i32 as f64 - self.held(player, Button::Up) as i32 as f64
    }

    /// Whether this player has touched a button within `within`
    pub fn active(&self, player: usize, within: Duration) -> bool {
        self.last_used[player - 1].is_some_and(|t| t.elapsed() < within)
    }

    fn event(&mut self, msg: &Value) {
        let (Some(player), Some(button), Some(down)) =
            (msg["player"].as_u64(), msg["button"].as_str().and_then(Button::parse), msg["pressed"].as_bool())
        else {
            return;
        };
        if !(1..=2).contains(&player) {
            return;
        }
        let p = player as usize - 1;
        self.held[p][button as usize] = down;
        if down {
            self.pressed[p][button as usize] = true;
        }
        self.last_used[p] = Some(Instant::now());
    }
}

/// A game is not stopped by --timeout until nobody has played for this long
const PLAYING_GRACE: Duration = Duration::from_secs(30);

pub struct Game {
    pub geometry: Geometry,
    pub fps: f64,
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    name: String,
    /// When to stop, from --timeout
    deadline: Option<Instant>,
    exit_on_input: bool,
}

struct Args {
    url: String,
    timeout: Option<Duration>,
    exit_on_input: bool,
}

fn args(name: &str) -> Args {
    let usage = || -> ! {
        eprintln!("usage: {} [ws://host:port/ws] [--timeout SECONDS] [--exit-on-input]", name.to_lowercase());
        exit(1)
    };
    let mut parsed = Args { url: "ws://localhost:8080/ws".into(), timeout: None, exit_on_input: false };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let seconds = match arg.as_str() {
            "--timeout" | "-t" => Some(args.next().unwrap_or_else(|| usage())),
            _ => arg.strip_prefix("--timeout=").map(str::to_string),
        };
        match seconds {
            Some(v) => match v.parse::<f64>() {
                Ok(t) if t > 0.0 && t.is_finite() => parsed.timeout = Some(Duration::from_secs_f64(t)),
                _ => {
                    eprintln!("{name}: --timeout needs a positive number of seconds, not '{v}'");
                    exit(1)
                }
            },
            None if arg == "--help" || arg == "-h" => usage(),
            None if arg == "--exit-on-input" => parsed.exit_on_input = true,
            None if arg.starts_with('-') => {
                eprintln!("{name}: unknown option {arg}");
                usage()
            }
            None => parsed.url = arg,
        }
    }
    parsed
}

impl Game {
    pub fn connect(name: &str) -> Self {
        let Args { url, timeout, exit_on_input } = args(name);
        let (mut ws, _) = tungstenite::connect(&url).unwrap_or_else(|e| {
            eprintln!("{name}: cannot connect to {url}: {e}");
            exit(1)
        });
        let welcome: Value = loop {
            if let Message::Text(t) = ws.read().expect("welcome") {
                break serde_json::from_str(&t).expect("welcome JSON");
            }
        };
        ws.send(Message::text(json!({"type": "hello", "name": name}).to_string())).expect("hello");
        let geometry = Geometry::from_welcome(&welcome["display"]);
        println!(
            "{name}: {}x{} pixels, {:.0} x {:.0} cm, {}",
            geometry.width,
            geometry.height,
            geometry.width_cm,
            geometry.height_cm,
            if geometry.wraps { "cylinder" } else { "flat" }
        );
        let fps = welcome["display"]["fps"].as_f64().unwrap_or(40.0);
        let deadline = timeout.map(|t| Instant::now() + t);
        Self { geometry, fps, ws, name: name.to_string(), deadline, exit_on_input }
    }

    /// Calls `step` once per display tick with the time step in seconds, the
    /// frame to draw and a list to push sound messages onto. Returns when the
    /// --timeout runs out. Exits the program with status 2 when another game
    /// takes over the display, and 1 when the connection is lost.
    pub fn run(self, mut step: impl FnMut(&Geometry, f64, &mut Frame, &mut Vec<Value>)) {
        self.run_loop(false, |g, dt, frame, sounds, _| step(g, dt, frame, sounds));
    }

    /// Like `run`, for games: `step` also gets the players' buttons. With
    /// --timeout, the game keeps going while someone is playing.
    pub fn run_with_input(self, step: impl FnMut(&Geometry, f64, &mut Frame, &mut Vec<Value>, &Input)) {
        self.run_loop(true, step);
    }

    fn run_loop(mut self, playable: bool, mut step: impl FnMut(&Geometry, f64, &mut Frame, &mut Vec<Value>, &Input)) {
        let g = &self.geometry;
        let mut input = Input::default();
        let mut frame = Frame { width: g.width, pixels: vec![0; g.width * g.height * 3] };
        let mut sounds = Vec::new();
        let dt = 1.0 / self.fps;
        let mut replaced = false;
        loop {
            let playing = playable && (1..=2).any(|p| input.active(p, PLAYING_GRACE));
            if self.deadline.is_some_and(|d| Instant::now() >= d) && !playing {
                println!("{}: timeout, stopping", self.name);
                // Closing makes the server stop this demo's sounds
                let _ = self.ws.close(None);
                while self.ws.read().is_ok() {}
                return;
            }
            let msg = match self.ws.read() {
                Ok(Message::Text(t)) => serde_json::from_str::<Value>(&t).unwrap_or_default(),
                Ok(Message::Close(_)) | Err(_) if replaced => exit(2),
                Ok(Message::Close(_)) | Err(_) => {
                    eprintln!("{}: the server closed the connection", self.name);
                    exit(1)
                }
                Ok(_) => continue,
            };
            match msg["type"].as_str() {
                Some("tick") => {}
                Some("input") => {
                    if !playable && self.exit_on_input && msg["pressed"].as_bool() == Some(true) {
                        println!("{}: a player pressed a button, making way for a game", self.name);
                        let _ = self.ws.close(None);
                        while self.ws.read().is_ok() {}
                        exit(3);
                    }
                    input.event(&msg);
                    continue;
                }
                Some("replaced") => {
                    println!("{}: {}", self.name, msg["message"].as_str().unwrap_or("another game took over the display"));
                    replaced = true;
                    continue;
                }
                Some("error") => {
                    eprintln!("server: {}", msg["message"]);
                    continue;
                }
                _ => continue,
            }
            step(&self.geometry, dt, &mut frame, &mut sounds, &input);
            input.pressed = Default::default();
            for sound in sounds.drain(..) {
                let _ = self.ws.send(Message::text(sound.to_string()));
            }
            if self.ws.send(Message::binary(frame.pixels.clone())).is_err() && !replaced {
                eprintln!("{}: lost the connection to the server", self.name);
                exit(1)
            }
        }
    }
}
