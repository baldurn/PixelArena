//! HTTP (the self-documentation), the game WebSocket and the frame clock.
//!
//! The server owns the frame clock: every tick it sends the active game's
//! latest frame to the controllers (LEDF to each controller, then one LEDS
//! broadcast, see udp-demo/src/led.rs). Only one game is connected at a time:
//! a new game takes over the display and the previous one is disconnected.
//!
//! A DDP stream (WLED, xLights, see ddp.rs) is the other source. When a stream
//! starts it takes over the display like a new game would; a game that
//! connects later is shown instead, and once it leaves the stream is shown
//! again if it is still running. With neither, the idle animation plays.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};

use crate::audio::{self, Audio, Voice, MAX_CLIP_SECONDS, SAMPLE_RATE, WAVES};
use crate::ddp;
use crate::layout::{Layout, LED_PORT};
use crate::sim::FrameSlot;

pub const PROTOCOL_VERSION: u32 = 1;
/// What each player has, in the order games see them
pub const BUTTONS: [&str; 5] = ["up", "down", "left", "right", "fire"];
const MAX_MESSAGE: usize = 8 * 1024 * 1024;
const DOCS: &str = include_str!("docs.md");

struct Client {
    id: u64,
    peer: SocketAddr,
    name: Option<String>,
    frame: Option<Bytes>,
    frames: u64,
    sounds: HashMap<String, Arc<Vec<i16>>>,
    last_error: Option<Instant>,
    tx: mpsc::Sender<Message>,
    /// Ends the session when a newer game takes over
    kick: Arc<Notify>,
}

impl Client {
    fn label(&self) -> String {
        format!("{} ({})", self.name.as_deref().unwrap_or("unnamed"), self.peer)
    }

    /// Messages to a client that is not reading are dropped rather than buffered forever
    fn send(&self, msg: Value) {
        let _ = self.tx.try_send(Message::Text(msg.to_string().into()));
    }

    /// At most a few errors per second, so a broken game does not flood itself
    fn error(&mut self, code: &str, message: String) {
        let now = Instant::now();
        if self.last_error.map_or(true, |t| now - t >= Duration::from_millis(200)) {
            self.last_error = Some(now);
            self.send(json!({"type": "error", "error": code, "message": message}));
        }
    }
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    Flat,
    /// The columns stand in a circle, so x wraps around
    Cylinder,
}

/// The real size and shape of the display, so games can draw in centimetres
#[derive(Deserialize, Clone)]
pub struct Physical {
    pub shape: Shape,
    /// For a cylinder: the circumference
    pub width_cm: f64,
    pub height_cm: f64,
    /// Width of the lit pipe each column sits in, if any
    pub pipe_cm: Option<f64>,
}

pub struct Server {
    pub layout: Layout,
    pub fps: f64,
    pub brightness: f64,
    pub gamma: f64,
    pub broadcast: Ipv4Addr,
    pub audio: Option<Arc<Audio>>,
    pub simulator: Option<FrameSlot>,
    pub physical: Option<Physical>,
    pub port: u16,
    /// UDP port for DDP input, None if disabled
    pub ddp_port: Option<u16>,
    ddp: Mutex<ddp::Receiver>,
    /// Whether anything delivers player input (the simulator's keyboard)
    input_enabled: bool,
    table: [u8; 256],
    frame_size: usize,
    /// The connected game, if any
    game: Mutex<Option<Client>>,
    next_id: AtomicU64,
}

/// HSV with full saturation -> RGB, h in 0..1
fn hue(h: f64, v: f64) -> [u8; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let x = 1.0 - (h6 % 2.0 - 1.0).abs();
    let (r, g, b) = match h6 as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    [r, g, b].map(|c: f64| (c * v * 255.0) as u8)
}

impl Server {
    pub fn new(
        layout: Layout,
        fps: f64,
        brightness: f64,
        gamma: f64,
        broadcast: Ipv4Addr,
        audio: Option<Arc<Audio>>,
        simulator: Option<FrameSlot>,
        physical: Option<Physical>,
        port: u16,
        ddp_port: Option<u16>,
        ddp_size: Option<(usize, usize)>,
        input_enabled: bool,
    ) -> Self {
        let table = std::array::from_fn(|i| ((i as f64 / 255.0).powf(gamma) * brightness * 255.0).round() as u8);
        let frame_size = layout.width * layout.height * 3;
        let ddp = Mutex::new(ddp::Receiver::new(ddp_size, layout.width, layout.height));
        Self {
            layout,
            fps,
            brightness,
            gamma,
            broadcast,
            audio,
            simulator,
            physical,
            port,
            ddp_port,
            ddp,
            input_enabled,
            table,
            frame_size,
            game: Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }

    fn audio_enabled(&self) -> bool {
        self.audio.as_ref().is_some_and(|a| a.alive())
    }

    fn display_info(&self) -> Value {
        json!({
            "width": self.layout.width, "height": self.layout.height, "fps": self.fps,
            "pixel_format": "rgb24", "frame_bytes": self.frame_size,
            "brightness": self.brightness, "gamma": self.gamma,
            "physical": self.physical_info(),
        })
    }

    fn physical_info(&self) -> Value {
        let Some(p) = &self.physical else { return Value::Null };
        let cylinder = p.shape == Shape::Cylinder;
        let round = |v: f64| (v * 1000.0).round() / 1000.0;
        json!({
            "shape": if cylinder { "cylinder" } else { "flat" },
            "wraps_x": cylinder,
            "width_cm": p.width_cm, "height_cm": p.height_cm,
            "diameter_cm": cylinder.then(|| round(p.width_cm / std::f64::consts::PI)),
            "column_pitch_cm": round(p.width_cm / self.layout.width as f64),
            "row_pitch_cm": round(p.height_cm / self.layout.height as f64),
            "pipe_cm": p.pipe_cm,
        })
    }

    /// The physical part of the documentation
    fn physical_text(&self) -> String {
        let (w, h) = (self.layout.width, self.layout.height);
        let Some(p) = &self.physical else {
            return "The physical size of this display is not configured: treat the pixels as \
                    square and the display as flat."
                .into();
        };
        let (col, row) = (p.width_cm / w as f64, p.height_cm / h as f64);
        let mut text = match p.shape {
            Shape::Cylinder => format!(
                "- The display is the outside of a **cylinder**: the {w} columns stand in a \
                 circle {:.0} cm around ({:.0} cm across), {:.0} cm tall.\n\
                 - **x wraps around**: column {} is next to column 0. Anything moving off one \
                 side continues on the other, and distances in x are measured the short way \
                 around. There is no left or right edge.\n\
                 - x increases from left to right for someone standing outside the circle. \
                 Column x is at angle 360 * (x + 0.5) / {w} degrees around the circle. Nobody \
                 sees the whole display at once: from outside, less than half of it faces you.\n",
                p.width_cm,
                p.width_cm / std::f64::consts::PI,
                p.height_cm,
                w - 1
            ),
            Shape::Flat => format!(
                "- The display is **flat**, {:.0} cm wide and {:.0} cm tall.\n",
                p.width_cm, p.height_cm
            ),
        };
        text += &format!(
            "- **Pixels are not square.** Columns are {col:.1} cm apart, rows {row:.2} cm apart: \
             a pixel is {:.0} times as far from its left and right neighbours as from the ones \
             above and below. To draw something round, or move at the same speed in every \
             direction, work in centimetres and convert: x = cm_across / {col:.3}, \
             y = cm_down / {row:.4}.\n",
            col / row
        );
        if let Some(pipe) = p.pipe_cm {
            text += &format!(
                "- Each column is a vertical pipe {pipe:.0} cm wide, lit from inside; between \
                 the pipes it is dark. A single lit pixel shows as a short glowing band across \
                 its pipe.\n"
            );
        }
        text += "- These values are also in the `physical` field of the `welcome` message.";
        text
    }

    fn input_info(&self) -> Value {
        json!({"enabled": self.input_enabled, "players": 2, "buttons": BUTTONS})
    }

    /// A player pressed or released a button; the connected game hears about it
    pub fn input(&self, player: u8, button: &str, pressed: bool) {
        if let Some(c) = self.game.lock().unwrap().as_ref() {
            c.send(json!({"type": "input", "player": player, "button": button, "pressed": pressed}));
        }
    }

    fn audio_info(&self) -> Value {
        json!({"enabled": self.audio_enabled(), "muted": self.audio.as_ref().is_some_and(|a| a.muted()),
               "sample_rate": SAMPLE_RATE,
               "max_clip_seconds": MAX_CLIP_SECONDS, "waves": WAVES})
    }

    fn status(&self) -> Value {
        json!({
            "protocol": PROTOCOL_VERSION, "display": self.display_info(), "audio": self.audio_info(),
            "controllers": self.layout.controllers,
            "game": self.game.lock().unwrap().as_ref().map(Client::label),
            "ddp": self.ddp_status(),
        })
    }

    fn ddp_status(&self) -> Value {
        let ddp = self.ddp.lock().unwrap();
        let live = ddp.live(Instant::now());
        json!({"port": self.ddp_port, "width": ddp.width, "height": ddp.height,
               "source": live.then(|| ddp.source.map(|s| s.to_string())).flatten(),
               "frames": if live { ddp.frames } else { 0 }})
    }

    fn ddp_text(&self) -> String {
        let Some(port) = self.ddp_port else {
            return "DDP input is disabled on this server.".into();
        };
        let (w, h, detect) = {
            let ddp = self.ddp.lock().unwrap();
            if ddp.detects_size() {
                (self.layout.width, self.layout.height, true)
            } else {
                (ddp.width, ddp.height, false)
            }
        };
        let stretch = if detect {
            format!(
                " A sender may also draw a smaller canvas: send fewer pixels per frame and the \
                 server stretches it to fill the display, repeating pixels. A frame of a whole \
                 number of rows, e.g. {w} x {} = {} pixels, is shown with each row twice.",
                h / 2,
                w * (h / 2)
            )
        } else if (w, h) == (self.layout.width, self.layout.height) {
            String::new()
        } else {
            format!(
                " The DDP canvas is {w} x {h}, smaller than the display: the server stretches it \
                 to {} x {}, repeating pixels.",
                self.layout.width, self.layout.height
            )
        };
        format!(
            "The server also accepts [DDP](http://www.3waylabs.com/ddp/) on **UDP port {port}**, so \
             WLED, xLights and other lighting software can drive the display. The display is one \
             strip of **{} pixels**: row 0 left to right, then row 1, and so on, the same order as \
             a game's frames.{stretch} 8-bit RGB, RGBW (white is added to R, G and B) and grey are \
             accepted; a frame is shown when the packet with the push flag arrives.\n\n\
             - **WLED**: in LED Preferences add a *Network* output of type DDP RGB, IP address \
             of this server, start 0, length {}. To use 2D effects, set up a {w} x {h} matrix \
             (2D Configuration), starting top left, horizontal, not serpentine.\n\
             - A DDP stream that starts takes over the display from a connected game (the game \
             gets `replaced`). A game that connects while a stream runs is shown instead of the \
             stream. A stream ends after {} seconds without packets.",
            w * h,
            w * h,
            ddp::STREAM_TIMEOUT.as_secs()
        )
    }

    fn docs(&self, host: &str) -> String {
        let audio_state = if self.audio_enabled() {
            "enabled"
        } else {
            "disabled on this server (sound messages are accepted but nothing plays)"
        };
        // Longest names first, so no name is replaced inside a longer one
        let mut vars = [
            ("audio_state", audio_state.to_string()),
            ("input_state", if self.input_enabled {
                "available: players use the keyboard of the display's screen".to_string()
            } else {
                "not available on this server (no keyboard), so no input messages arrive".to_string()
            }),
            ("frame_bytes", self.frame_size.to_string()),
            ("sample_rate", SAMPLE_RATE.to_string()),
            ("max_clip", MAX_CLIP_SECONDS.to_string()),
            ("protocol", PROTOCOL_VERSION.to_string()),
            ("height", self.layout.height.to_string()),
            ("last_x", (self.layout.width - 1).to_string()),
            ("last_y", (self.layout.height - 1).to_string()),
            ("width", self.layout.width.to_string()),
            ("host", host.to_string()),
            ("physical_text", self.physical_text()),
            ("ddp_text", self.ddp_text()),
            ("physical_json", self.physical_info().to_string()),
            ("fps", self.fps.to_string()),
        ];
        vars.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        vars.iter().fold(DOCS.to_string(), |s, (k, v)| s.replace(&format!("${k}"), v))
    }

    // --- frame clock ---

    fn idle_frame(&self, tick: u64) -> Vec<u8> {
        let (w, h) = (self.layout.width, self.layout.height);
        let mut out = Vec::with_capacity(self.frame_size);
        for y in 0..h {
            for x in 0..w {
                let h = x as f64 / w as f64 + y as f64 / (2 * h) as f64 + tick as f64 / 600.0;
                out.extend_from_slice(&hue(h, 0.15));
            }
        }
        out
    }

    pub async fn run_display(self: Arc<Self>) {
        let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.expect("UDP socket");
        socket.set_broadcast(true).expect("broadcast");
        let mut interval = tokio::time::interval(Duration::from_secs_f64(1.0 / self.fps));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let (mut tick, mut frame_id, mut send_errors) = (0u64, 0u16, 0u64);
        loop {
            interval.tick().await;
            let (frame, active) = match self.game.lock().unwrap().as_ref() {
                Some(c) => (c.frame.clone().map(|f| f.to_vec()), Some(c.id)),
                None => (None, None),
            };
            let frame = match (frame, active) {
                (Some(f), _) => f,
                (None, Some(_)) => vec![0; self.frame_size],
                (None, None) => {
                    self.ddp.lock().unwrap().current(Instant::now()).unwrap_or_else(|| self.idle_frame(tick))
                }
            };
            let frame: Vec<u8> = frame.iter().map(|&b| self.table[b as usize]).collect();
            frame_id = frame_id.wrapping_add(1);
            if !self.layout.controllers.is_empty() {
                let mut result = Ok(());
                for (controller, packet) in self.layout.packets(&frame, frame_id) {
                    result = result.and(socket.send_to(&packet, (controller, LED_PORT)).await.map(|_| ()));
                }
                // Give the controllers time to convert the frame before the sync
                tokio::time::sleep(Duration::from_millis(2)).await;
                let mut sync = b"LEDS".to_vec();
                sync.extend_from_slice(&frame_id.to_be_bytes());
                result = result.and(socket.send_to(&sync, (self.broadcast, LED_PORT)).await.map(|_| ()));
                if let Err(e) = result {
                    send_errors += 1;
                    if [1, 10, 100].contains(&send_errors) || send_errors % 1000 == 0 {
                        eprintln!("LED send failed ({send_errors} times): {e}");
                    }
                }
            }
            if let Some(slot) = &self.simulator {
                *slot.lock().unwrap() = Some(Arc::new(frame));
            }
            if let Some(id) = active {
                self.with_game(id, |c| c.send(json!({"type": "tick", "frame": tick})));
            }
            tick += 1;
        }
    }

    // --- WebSocket sessions ---

    async fn session(self: Arc<Self>, socket: WebSocket, peer: SocketAddr) {
        let (mut sink, mut stream) = socket.split();
        let (tx, mut rx) = mpsc::channel::<Message>(256);
        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
        });
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let kick = Arc::new(Notify::new());
        {
            let mut game = self.game.lock().unwrap();
            let client = Client {
                id,
                peer,
                name: None,
                frame: None,
                frames: 0,
                sounds: HashMap::new(),
                last_error: None,
                tx,
                kick: kick.clone(),
            };
            client.send(json!({"type": "welcome", "protocol": PROTOCOL_VERSION,
                               "display": self.display_info(), "audio": self.audio_info(),
                               "input": self.input_info()}));
            if let Some(old) = game.replace(client) {
                self.retire(old, "another game connected and took over the display", "replaced by another game");
            }
            println!("connected: {peer}");
        }

        let mut reason = "closed".to_string();
        loop {
            let msg = tokio::select! {
                msg = stream.next() => msg,
                _ = kick.notified() => break,
            };
            match msg {
                Some(Ok(Message::Binary(data))) => self.on_frame(id, data),
                Some(Ok(Message::Text(text))) => self.on_command(id, text.as_str()),
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    reason = format!("connection lost: {e}");
                    break;
                }
            }
        }

        {
            let mut game = self.game.lock().unwrap();
            if game.as_ref().is_some_and(|c| c.id == id) {
                let client = game.take().unwrap();
                if let Some(audio) = &self.audio {
                    audio.stop(id, None);
                }
                println!("disconnected: {}, {reason}, {} frames", client.label(), client.frames);
            }
        }
        // The client and its sender are gone, so the writer stops once it has
        // sent what is queued (such as the close after being replaced)
        let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
    }

    /// Disconnects a game that something else took the display from
    fn retire(&self, old: Client, message: &str, reason: &str) {
        if let Some(audio) = &self.audio {
            audio.stop(old.id, None);
        }
        old.send(json!({"type": "replaced", "message": message}));
        let _ = old.tx.try_send(Message::Close(Some(CloseFrame {
            // Normal closure, so a game's receive loop simply ends
            code: 1000,
            reason: reason.to_string().into(),
        })));
        old.kick.notify_one();
        println!("replaced: {}, {} frames", old.label(), old.frames);
    }

    // --- DDP input ---

    async fn run_ddp(self: Arc<Self>, socket: tokio::net::UdpSocket) {
        let pixels = self.ddp.lock().unwrap().capacity();
        let answer = move |id| ddp::query_answer(id, pixels);
        let mut buf = vec![0u8; 65536];
        loop {
            let (n, from) = match socket.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("ddp: receive failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let action = self.ddp.lock().unwrap().handle(&buf[..n], from, &answer);
            match action {
                ddp::Action::Nothing => {}
                ddp::Action::Reply(packet) => {
                    let _ = socket.send_to(&packet, from).await;
                }
                ddp::Action::StreamStarted => {
                    // Not holding the DDP lock here, so the two locks are never held together
                    let old = self.game.lock().unwrap().take();
                    if let Some(old) = old {
                        self.retire(
                            old,
                            &format!("a DDP stream from {} took over the display", from.ip()),
                            "replaced by a DDP stream",
                        );
                    }
                }
            }
        }
    }

    /// Runs `f` on the connected game if it is still the one with this id
    fn with_game<R>(&self, id: u64, f: impl FnOnce(&mut Client) -> R) -> Option<R> {
        self.game.lock().unwrap().as_mut().filter(|c| c.id == id).map(f)
    }

    fn on_frame(&self, id: u64, data: Bytes) {
        let mut game = self.game.lock().unwrap();
        let Some(client) = game.as_mut().filter(|c| c.id == id) else { return };
        if data.len() != self.frame_size {
            client.error(
                "bad_frame_size",
                format!(
                    "frame is {} bytes, expected {} (width {} x height {} x 3 bytes RGB)",
                    data.len(),
                    self.frame_size,
                    self.layout.width,
                    self.layout.height
                ),
            );
            return;
        }
        client.frame = Some(data);
        client.frames += 1;
    }

    fn on_command(&self, id: u64, text: &str) {
        let msg: Result<serde_json::Map<String, Value>, _> = serde_json::from_str(text);
        let result = match &msg {
            Err(e) => Err(("bad_json", format!("could not parse text message as a JSON object: {e}"))),
            Ok(msg) => {
                let kind = msg.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "hello" | "sound_load" | "sound_play" | "sound_stop" | "tone" => {
                        self.command(id, kind, msg).map_err(|e| ("bad_message", format!("{kind}: {e}")))
                    }
                    _ => Err((
                        "unknown_type",
                        format!(
                            "unknown message type {}; valid types: hello, sound_load, sound_play, sound_stop, tone",
                            msg.get("type").map_or("null".to_string(), Value::to_string)
                        ),
                    )),
                }
            }
        };
        if let Err((code, message)) = result {
            self.with_game(id, |c| c.error(code, message));
        }
    }

    fn command(&self, id: u64, kind: &str, msg: &serde_json::Map<String, Value>) -> Result<(), String> {
        let field = |name: &str| msg.get(name).filter(|v| !v.is_null());
        let required = |name: &str| field(name).ok_or_else(|| format!("missing field '{name}'"));
        let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_string);
        let number = |name: &str| -> Result<Option<f64>, String> {
            field(name).map(|v| v.as_f64().ok_or_else(|| format!("{name} must be a number"))).transpose()
        };
        let volume = || -> Result<f32, String> {
            let v = number("volume")?.unwrap_or(1.0);
            if (0.0..=1.0).contains(&v) { Ok(v as f32) } else { Err("volume must be between 0 and 1".into()) }
        };

        // Decoding happens before taking the lock
        let decoded = if kind == "sound_load" {
            let data = required("data")?.as_str().ok_or("data must be a base64 string")?;
            let wav = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| format!("data is not valid base64: {e}"))?;
            Some(Arc::new(audio::decode_wav(&wav)?))
        } else {
            None
        };

        let mut game = self.game.lock().unwrap();
        let Some(client) = game.as_mut().filter(|c| c.id == id) else { return Ok(()) };
        match kind {
            "hello" => {
                client.name = Some(text(required("name")?).chars().take(40).collect());
                println!("hello: {}", client.label());
            }
            "sound_load" => {
                let name = text(required("id")?);
                let samples = decoded.unwrap();
                let seconds = (samples.len() as f64 / SAMPLE_RATE as f64 * 1000.0).round() / 1000.0;
                client.sounds.insert(name.clone(), samples);
                client.send(json!({"type": "sound_loaded", "id": name, "seconds": seconds}));
            }
            "sound_play" => {
                let name = text(required("id")?);
                let samples = client
                    .sounds
                    .get(&name)
                    .ok_or_else(|| format!("no sound with id '{name}'; send sound_load first"))?
                    .clone();
                let volume = volume()?;
                let looped = field("loop").and_then(Value::as_bool).unwrap_or(false);
                if let Some(audio) = &self.audio {
                    audio.play(Voice::new(id, Some(name), samples, volume, looped));
                }
            }
            "sound_stop" => {
                if let Some(audio) = &self.audio {
                    audio.stop(id, field("id").map(text).as_deref());
                }
            }
            "tone" => {
                let wave = field("wave").map(text).unwrap_or_else(|| "square".into());
                if !WAVES.contains(&wave.as_str()) {
                    return Err(format!("wave must be one of {}", WAVES.join(", ")));
                }
                let freq = number("freq")?.ok_or("missing field 'freq'")?;
                let to_freq = number("to_freq")?;
                let ms = number("ms")?.unwrap_or(100.0);
                if !(20.0..=10000.0).contains(&freq) || to_freq.is_some_and(|f| !(20.0..=10000.0).contains(&f)) {
                    return Err("freq and to_freq must be between 20 and 10000 Hz".into());
                }
                if !(1.0..=5000.0).contains(&ms) {
                    return Err("ms must be between 1 and 5000".into());
                }
                let volume = volume()?;
                if let Some(audio) = &self.audio {
                    let samples = audio::make_tone(&wave, freq, ms, to_freq, id ^ client.frames);
                    audio.play(Voice::new(id, field("id").map(text), Arc::new(samples), volume, false));
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    // --- HTTP ---

    pub async fn serve(self: Arc<Self>) -> std::io::Result<()> {
        let app = Router::new()
            .route("/", get(docs))
            .route("/docs", get(docs))
            .route("/protocol.md", get(docs))
            .route("/info", get(info))
            .route("/ws", get(ws))
            .fallback(|| async { (StatusCode::NOT_FOUND, "Not found. The protocol documentation is at /\n") })
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", self.port)).await?;
        let target = if self.simulator.is_some() {
            "the simulator window".to_string()
        } else if self.layout.controllers.is_empty() {
            "controllers none".to_string()
        } else {
            let list: Vec<_> = self.layout.controllers.iter().map(Ipv4Addr::to_string).collect();
            format!("controllers {}", list.join(", "))
        };
        println!(
            "display {}x{} @ {} fps on {target}; docs at http://<this host>:{}/",
            self.layout.width, self.layout.height, self.fps, self.port
        );
        if let Some(port) = self.ddp_port {
            let socket = tokio::net::UdpSocket::bind(("0.0.0.0", port)).await?;
            let ddp = self.ddp.lock().unwrap();
            let size = if ddp.detects_size() { "size detected".to_string() } else {
                format!("{} x {} canvas", ddp.width, ddp.height)
            };
            println!("DDP input on UDP port {port} (up to {} pixels, {size})", ddp.capacity());
            drop(ddp);
            tokio::spawn(self.clone().run_ddp(socket));
        }
        tokio::spawn(self.clone().run_display());
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await
    }
}

async fn docs(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map_or_else(|| format!("<this host>:{}", server.port), str::to_string);
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        server.docs(&host),
    )
        .into_response()
}

async fn info(State(server): State<Arc<Server>>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json"), (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        serde_json::to_string_pretty(&server.status()).unwrap() + "\n",
    )
        .into_response()
}

async fn ws(
    State(server): State<Arc<Server>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| server.session(socket, peer))
}
