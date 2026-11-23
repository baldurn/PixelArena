//! Display server: games draw on the LED display over a WebSocket. The server
//! documents its own protocol at http://<host>:<port>/, so a game can be
//! written by handing that URL to an AI.
//!
//!   display-server [--config layout.json] [--port 8080] [--audio auto|native|pipewire|aplay|none]
//!                  [--ddp-port 4048] [--ddp-size WxH]
//!                  DDP input for WLED and xLights; port 0 turns it off
//!   display-server --simulate CHAINS --width-cm W --height-cm H [--leds N]
//!                  [--led-scale S | --pipe-cm D] [--cylinder] [--view flat|3d] [--fullscreen]
//!                  show the display in a window instead of on controllers

mod audio;
mod ddp;
mod layout;
mod overlay;
mod server;
mod sim;
mod sim3d;

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, ValueEnum};
use serde::Deserialize;

use layout::{Layout, Segment, LEDS_PER_STRIP};
use server::Server;
use sim::Simulator;

#[derive(Clone, Copy, ValueEnum)]
enum AudioBackend {
    /// macOS and Windows: native. Linux: pipewire if installed, else aplay, else none
    Auto,
    /// the system's own sound output (macOS and Windows)
    Native,
    /// play with pw-cat
    Pipewire,
    /// play directly through ALSA; fails while PipeWire holds the device
    Aplay,
    None,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum View {
    /// The display unrolled flat, to scale
    Flat,
    /// The chains standing where they really are, in perspective
    #[value(name = "3d")]
    ThreeD,
}

#[derive(Parser)]
#[command(about = "LED display server for games; the protocol is documented at http://<host>:<port>/")]
struct Args {
    /// Layout JSON (see layout.example.json); default: discover controllers, one row per strip
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, default_value_t = 8080)]
    port: u16,
    #[arg(long, value_enum, default_value_t = AudioBackend::Auto)]
    audio: AudioBackend,
    /// ALSA device for aplay (e.g. plughw:1,0), or PipeWire target for pw-cat
    #[arg(long)]
    audio_device: Option<String>,
    /// UDP port for DDP input from WLED, xLights etc.; 0 disables it
    #[arg(long, default_value_t = ddp::DDP_PORT)]
    ddp_port: u16,
    /// Size of the canvas DDP senders draw, e.g. 60x72 for half the rows;
    /// it is stretched to the display. Default: detected from each frame's length
    #[arg(long, value_name = "WxH", value_parser = parse_size)]
    ddp_size: Option<(usize, usize)>,

    /// Show the display in a window with this many vertical LED chains,
    /// spread evenly across the width, instead of driving controllers
    #[arg(long, value_name = "CHAINS", help_heading = "Simulation")]
    simulate: Option<usize>,
    /// LEDs per chain
    #[arg(long, default_value_t = LEDS_PER_STRIP, help_heading = "Simulation")]
    leds: usize,
    /// Width of the display in cm
    #[arg(long, help_heading = "Simulation")]
    width_cm: Option<f64>,
    /// Height of the display (length of a chain) in cm
    #[arg(long, help_heading = "Simulation")]
    height_cm: Option<f64>,
    /// Draw LEDs this many times their real 5 mm size, with a glow
    #[arg(long, default_value_t = 1.0, help_heading = "Simulation")]
    led_scale: f64,
    /// Each chain is inside a diffusing pipe of this diameter in cm; replaces --led-scale
    #[arg(long, help_heading = "Simulation")]
    pipe_cm: Option<f64>,
    /// The chains stand in a circle --width-cm around, so x wraps around
    #[arg(long, help_heading = "Simulation")]
    cylinder: bool,
    /// How the simulator shows the display; V in the window switches
    #[arg(long, value_enum, default_value_t = View::Flat, help_heading = "Simulation")]
    view: View,
    /// Start the window fullscreen; F in the window switches, Esc leaves
    #[arg(long, help_heading = "Simulation")]
    fullscreen: bool,
}

#[derive(Deserialize, Default)]
struct Config {
    width: Option<usize>,
    height: Option<usize>,
    segments: Option<Vec<Segment>>,
    controllers: Option<Vec<Ipv4Addr>>,
    fps: Option<f64>,
    brightness: Option<f64>,
    gamma: Option<f64>,
    broadcast: Option<Ipv4Addr>,
    physical: Option<server::Physical>,
}

fn parse_size(s: &str) -> Result<(usize, usize), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or("expected WIDTHxHEIGHT, e.g. 60x72")?;
    match (w.trim().parse(), h.trim().parse()) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err("width and height must be positive whole numbers".into()),
    }
}

#[cfg(not(target_os = "linux"))]
fn native_audio() -> Result<Arc<audio::Audio>, String> {
    audio::Audio::start_native()
}

#[cfg(target_os = "linux")]
fn native_audio() -> Result<Arc<audio::Audio>, String> {
    Err("--audio native is for macOS and Windows; on Linux use pipewire or aplay".into())
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| Path::new(&dir).join(program).is_file()))
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("display-server: {message}");
    std::process::exit(1)
}

/// Started from ssh or a script, only DISPLAY may be set, and the window would
/// go through XWayland, which is much slower. Use the desktop's own Wayland
/// socket when there is one. Must run before any other thread starts.
fn prefer_wayland() {
    if !cfg!(target_os = "linux") || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return;
    }
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else { return };
    if Path::new(&runtime).join("wayland-0").exists() {
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
    }
}

fn main() {
    let a = Args::parse();
    if a.simulate.is_some() {
        prefer_wayland();
    }
    let cfg: Config = match &a.config {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
            .unwrap_or_else(|e| fail(format!("{}: {e}", path.display()))),
        None => Config::default(),
    };
    let fps = cfg.fps.unwrap_or(40.0);

    let simulator = a.simulate.map(|chains| {
        let (Some(width_cm), Some(height_cm)) = (a.width_cm, a.height_cm) else {
            fail("--simulate needs --width-cm and --height-cm")
        };
        if chains < 1 || a.leds < 1 || width_cm <= 0.0 || height_cm <= 0.0 || a.led_scale <= 0.0
            || a.pipe_cm.is_some_and(|p| p <= 0.0)
        {
            fail("chains, LEDs, dimensions, LED scale and pipe diameter must be positive");
        }
        if a.pipe_cm.is_some_and(|p| p * chains as f64 > width_cm) {
            fail("the pipes are wider than the display");
        }
        Simulator {
            chains,
            leds: a.leds,
            width_cm,
            height_cm,
            led_scale: a.led_scale,
            pipe_cm: a.pipe_cm,
            cylinder: a.cylinder,
            view3d: a.view == View::ThreeD,
            fullscreen: a.fullscreen,
            port: a.port,
            ddp_port: (a.ddp_port != 0).then_some(a.ddp_port),
            fps,
            frame: Arc::new(Mutex::new(None)),
            failed: Arc::new(AtomicBool::new(false)),
            on_input: None,
        }
    });

    let layout = if let Some(sim) = &simulator {
        // Canvas column x is chain x, row y is LED y counted from the top
        Layout::new(sim.chains, sim.leds, &[])
    } else if let Some(segments) = &cfg.segments {
        let (Some(w), Some(h)) = (cfg.width, cfg.height) else { fail("a layout with segments needs width and height") };
        Layout::new(w, h, segments)
    } else {
        let controllers = match &cfg.controllers {
            Some(c) => c.clone(),
            None => layout::discover(Duration::from_secs(3))
                .unwrap_or_else(|e| fail(format!("listening for controller heartbeats: {e}"))),
        };
        if controllers.is_empty() {
            fail("no controllers found (no heartbeats on UDP 5001) and no layout given");
        }
        Layout::default_for(&controllers)
    }
    .unwrap_or_else(|e| fail(e));

    let auto = matches!(a.audio, AudioBackend::Auto);
    let backend = match a.audio {
        AudioBackend::Auto if !cfg!(target_os = "linux") => AudioBackend::Native,
        AudioBackend::Auto if which("pw-cat") => AudioBackend::Pipewire,
        AudioBackend::Auto if which("aplay") => AudioBackend::Aplay,
        AudioBackend::Auto => AudioBackend::None,
        b => b,
    };
    let rate = audio::SAMPLE_RATE.to_string();
    let command: Option<Vec<String>> = match backend {
        AudioBackend::Pipewire => {
            let mut cmd = vec!["pw-cat", "--playback", "--raw", "--format", "s16", "--rate", &rate,
                               "--channels", "1", "--latency", "50ms", "--media-role", "Game"];
            if let Some(d) = &a.audio_device {
                cmd.extend(["--target", d]);
            }
            cmd.push("-");
            Some(cmd.into_iter().map(String::from).collect())
        }
        AudioBackend::Aplay => {
            let mut cmd = vec!["aplay", "-q", "-t", "raw", "-f", "S16_LE", "-r", &rate, "-c", "1",
                               "--buffer-time=60000"];
            if let Some(d) = &a.audio_device {
                cmd.extend(["-D", d]);
            }
            Some(cmd.into_iter().map(String::from).collect())
        }
        _ => None,
    };
    let audio = match (command, backend) {
        (Some(cmd), _) => {
            Some(audio::Audio::start(&cmd).unwrap_or_else(|e| fail(format!("starting {}: {e}", cmd[0]))))
        }
        (None, AudioBackend::Native) => match native_audio() {
            Ok(audio) => Some(audio),
            // No sound device is no reason not to show the display
            Err(e) if auto => {
                eprintln!("display-server: no sound: {e}");
                None
            }
            Err(e) => fail(format!("sound output: {e}")),
        },
        _ => None,
    };

    let physical = match &simulator {
        Some(sim) => Some(server::Physical {
            shape: if a.cylinder { server::Shape::Cylinder } else { server::Shape::Flat },
            width_cm: sim.width_cm,
            height_cm: sim.height_cm,
            pipe_cm: sim.pipe_cm,
        }),
        None => cfg.physical.clone(),
    };
    let server = Arc::new(Server::new(
        layout,
        fps,
        cfg.brightness.unwrap_or(1.0),
        cfg.gamma.unwrap_or(1.0),
        cfg.broadcast.unwrap_or(Ipv4Addr::new(192, 168, 50, 255)),
        audio,
        simulator.as_ref().map(|s| s.frame.clone()),
        physical,
        a.port,
        (a.ddp_port != 0).then_some(a.ddp_port),
        a.ddp_size,
        simulator.is_some(),
    ));
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| fail(e));
    let Some(mut sim) = simulator else {
        runtime.block_on(server.serve()).unwrap_or_else(|e| fail(e));
        return;
    };

    // Players' keys in the window go to the connected game
    let input_server = server.clone();
    sim.on_input = Some(Box::new(move |player, button, pressed| input_server.input(player, button, pressed)));

    // The window must own the main thread; closing it ends the program
    let failed = sim.failed.clone();
    std::thread::spawn(move || {
        if let Err(e) = runtime.block_on(server.serve()) {
            eprintln!("display-server: {e}");
        }
        failed.store(true, Ordering::Relaxed);
    });
    sim.run().unwrap_or_else(|e| fail(e));
    // The window closed because the server stopped (e.g. its port was taken)
    if sim.failed.load(Ordering::Relaxed) {
        std::process::exit(1);
    }
}
