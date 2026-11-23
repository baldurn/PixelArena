//! Simulation: the display in a window instead of on controllers.
//!
//! The window shows vertical LED chains, spaced and sized from the physical
//! dimensions of the display, with black between the chains. `led_scale` draws
//! the LEDs larger than life so they are visible on a monitor; each LED is a
//! square core plus a round glow. Where LEDs overlap, the brightest one shows,
//! so a lit stretch of strip reads as a bar as bright as a single LED.
//!
//! With `pipe_cm` each chain sits in a diffusing pipe of that diameter instead:
//! an LED lights the full width of the pipe, brightest in the middle like a lit
//! cylinder, and spreads a little along it. Unlit pipes show faintly.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::overlay;
use crate::sim3d::{self, Camera};

pub(crate) const LED_CM: f64 = 0.5; // a WS2812B is 5 x 5 mm
const GLOW: f32 = 0.6; // brightness of the glow at the LED's centre, relative to the LED
pub(crate) const PIPE_BODY: f32 = 14.0; // how an unlit pipe shows, 0-255

/// Latest frame (chains x leds RGB, row-major), written by the server thread
pub type FrameSlot = Arc<Mutex<Option<Arc<Vec<u8>>>>>;

pub struct Simulator {
    pub chains: usize,
    pub leds: usize,
    pub width_cm: f64,
    pub height_cm: f64,
    pub led_scale: f64,
    /// Diameter of the diffusing pipe around each chain, if any
    pub pipe_cm: Option<f64>,
    /// The chains stand in a circle; matters for the 3D view
    pub cylinder: bool,
    /// Start in the 3D view; V switches
    pub view3d: bool,
    /// Start fullscreen; F switches
    pub fullscreen: bool,
    /// The server's ports, for the connection instructions on screen
    pub port: u16,
    pub ddp_port: Option<u16>,
    pub fps: f64,
    pub frame: FrameSlot,
    /// Set by the server thread if it stops; closes the window
    pub failed: Arc<AtomicBool>,
    /// Called with (player 1 or 2, button, pressed) for the players' keys
    pub on_input: Option<InputHandler>,
}

pub type InputHandler = Box<dyn Fn(u8, &'static str, bool)>;

/// Player 1: arrows and Enter. Player 2: W A S D and Space.
fn player_key(code: KeyCode) -> Option<(u8, &'static str)> {
    Some(match code {
        KeyCode::ArrowUp => (1, "up"),
        KeyCode::ArrowDown => (1, "down"),
        KeyCode::ArrowLeft => (1, "left"),
        KeyCode::ArrowRight => (1, "right"),
        KeyCode::Enter | KeyCode::NumpadEnter => (1, "fire"),
        KeyCode::KeyW => (2, "up"),
        KeyCode::KeyS => (2, "down"),
        KeyCode::KeyA => (2, "left"),
        KeyCode::KeyD => (2, "right"),
        KeyCode::Space => (2, "fire"),
        _ => return None,
    })
}

/// Where everything goes in a window of a given size
struct Fit {
    win_w: usize,
    img_w: usize,
    img_h: usize,
    offset: (usize, usize),
    pipes: bool,
    /// Per image row: the LEDs reaching it, with their core and glow weight
    rows: Vec<Vec<(usize, f32, f32)>>,
    /// Per chain: image columns x0..x1 with the core and glow weight of each
    columns: Vec<(usize, Vec<f32>, Vec<f32>)>,
}

impl Simulator {
    fn fit(&self, w: usize, h: usize) -> Fit {
        let ppc = (w as f64 / self.width_cm).min(h as f64 / self.height_cm);
        let pitch_x = self.width_cm / self.chains as f64 * ppc;
        let pitch_y = self.height_cm / self.leds as f64 * ppc;
        let img_w = ((self.width_cm * ppc).round() as usize).clamp(1, w);
        let img_h = ((self.height_cm * ppc).round() as usize).clamp(1, h);

        // Light is a horizontal times a vertical profile, each giving a core
        // and a glow weight for a distance d in pixels from the LED's centre.
        type Profile = Box<dyn Fn(f64) -> (f32, f32)>;
        let (across, along, reach_x, reach_y): (Profile, Profile, f64, f64) = match self.pipe_cm {
            Some(pipe_cm) => {
                let r = (pipe_cm / 2.0 * ppc).max(0.5);
                let sigma = (pipe_cm / 4.0 * ppc).max(0.5);
                let across = move |d: f64| {
                    let edge = (r + 0.5 - d.abs()).clamp(0.0, 1.0);
                    let cylinder = (1.0 - (d / r).powi(2)).max(0.0).sqrt();
                    ((edge * (0.6 + 0.4 * cylinder)) as f32, 0.0)
                };
                let along = move |d: f64| ((-0.5 * (d / sigma).powi(2)).exp() as f32, 0.0);
                // Every row must reach an LED, however small the window
                (Box::new(across), Box::new(along), r + 1.0, (2.5 * sigma).max(pitch_y / 2.0 + 0.5))
            }
            None => {
                // A square core plus a Gaussian glow, which is round
                let core = (LED_CM / 2.0 * self.led_scale * ppc).max(0.5);
                let sigma = (core * 1.5).max(1.0);
                let profile = move |d: f64| {
                    let core_w = (core + 0.5 - d.abs()).clamp(0.0, 1.0) as f32; // anti-aliased edge
                    let glow_w = (-0.5 * (d / sigma).powi(2)).exp() as f32;
                    (core_w, glow_w)
                };
                let reach = core.max(3.0 * sigma);
                (Box::new(profile), Box::new(profile), reach, reach)
            }
        };

        let rows = (0..img_h)
            .map(|r| {
                let y = r as f64 + 0.5;
                let first = ((y - reach_y) / pitch_y - 0.5).floor().max(0.0) as usize;
                let last = (((y + reach_y) / pitch_y - 0.5).ceil().max(0.0) as usize).min(self.leds - 1);
                (first..=last)
                    .filter_map(|j| {
                        let d = y - (j as f64 + 0.5) * pitch_y;
                        (d.abs() <= reach_y).then(|| {
                            // The glow strength goes in once, on this axis
                            let (c, g) = along(d);
                            (j, c, GLOW * g)
                        })
                    })
                    .collect()
            })
            .collect();
        let columns = (0..self.chains)
            .map(|i| {
                let cx = (i as f64 + 0.5) * pitch_x;
                let x0 = (cx - reach_x).max(0.0) as usize;
                let x1 = ((cx + reach_x).ceil() as usize).min(img_w);
                let (core_x, glow_x) = (x0..x1).map(|x| across(x as f64 + 0.5 - cx)).unzip();
                (x0, core_x, glow_x)
            })
            .collect();
        let offset = ((w - img_w) / 2, (h - img_h) / 2);
        Fit { win_w: w, img_w, img_h, offset, pipes: self.pipe_cm.is_some(), rows, columns }
    }

    fn render(&self, fit: &Fit, frame: &[u8], light: &mut Vec<[f32; 3]>, out: &mut [u32]) {
        light.clear();
        light.resize(fit.img_w * fit.img_h, [0.0; 3]);
        let px = |j: usize, i: usize| &frame[(j * self.chains + i) * 3..(j * self.chains + i) * 3 + 3];
        if fit.pipes {
            for (x0, core_x, _) in &fit.columns {
                for r in 0..fit.img_h {
                    for (p, &cw) in light[r * fit.img_w + x0..].iter_mut().zip(core_x) {
                        *p = [PIPE_BODY * cw; 3];
                    }
                }
            }
        }
        for (i, (x0, core_x, glow_x)) in fit.columns.iter().enumerate() {
            if (0..self.leds).all(|j| px(j, i) == [0, 0, 0]) {
                continue;
            }
            for (r, reaching) in fit.rows.iter().enumerate() {
                // This chain's core and glow colour on this row, from the brightest LED
                let (mut core, mut glow) = ([0f32; 3], [0f32; 3]);
                for &(j, cw, gw) in reaching {
                    for (c, &v) in px(j, i).iter().enumerate() {
                        core[c] = core[c].max(cw * v as f32);
                        glow[c] = glow[c].max(gw * v as f32);
                    }
                }
                if core == [0.0; 3] && glow == [0.0; 3] {
                    continue;
                }
                let row = &mut light[r * fit.img_w + x0..r * fit.img_w + x0 + core_x.len()];
                for ((p, &cw), &gw) in row.iter_mut().zip(core_x).zip(glow_x) {
                    for c in 0..3 {
                        // max also where a wide glow reaches the next chain
                        p[c] = p[c].max(core[c] * cw).max(glow[c] * gw);
                    }
                }
            }
        }
        out.fill(0);
        let (ox, oy) = fit.offset;
        for (r, row) in light.chunks(fit.img_w).enumerate() {
            let dst = &mut out[(oy + r) * fit.win_w + ox..][..fit.img_w];
            for (d, p) in dst.iter_mut().zip(row) {
                let [r, g, b] = p.map(|v| v.min(255.0) as u32);
                *d = r << 16 | g << 8 | b;
            }
        }
    }

    /// Window loop; must run on the main thread. Returns when the window is
    /// closed or the server fails.
    pub fn run(&self) -> Result<(), String> {
        let event_loop = EventLoop::new().map_err(|e| format!("cannot open window: {e}"))?;
        let mut app = App {
            sim: self,
            window: None,
            surface: None,
            error: None,
            size: (0, 0),
            fit: None,
            light: Vec::new(),
            shown: None,
            view3d: self.view3d,
            camera: Camera::new(self),
            keys_down: HashSet::new(),
            mouse: None,
            mouse_moved: Instant::now(),
            cursor_visible: true,
            drag: None,
            redraw: true,
            tick: Duration::from_secs_f64(1.0 / (self.fps * 2.0)),
            next_tick: Instant::now(),
            profile: std::env::var_os("SIM_PROFILE").map(|_| Profile::default()),
            info: Vec::new(),
            show_info: true,
            info_checked: None,
        };
        event_loop.run_app(&mut app).map_err(|e| format!("window: {e}"))?;
        app.error.map_or(Ok(()), Err)
    }
}

/// Time spent drawing, for SIM_PROFILE
struct Profile {
    render: Duration,
    present: Duration,
    frames: u32,
    since: Instant,
}

impl Default for Profile {
    fn default() -> Self {
        Self { render: Duration::ZERO, present: Duration::ZERO, frames: 0, since: Instant::now() }
    }
}

/// The simulator window: winit for the window and input, softbuffer to show
/// the pixels (shared memory, so a full-screen frame costs little to show)
struct App<'a> {
    sim: &'a Simulator,
    window: Option<Rc<Window>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    error: Option<String>,
    size: (usize, usize),
    fit: Option<Fit>,
    light: Vec<[f32; 3]>,
    shown: Option<Arc<Vec<u8>>>,
    view3d: bool,
    camera: Camera,
    keys_down: HashSet<KeyCode>,
    mouse: Option<(f64, f64)>,
    mouse_moved: Instant,
    cursor_visible: bool,
    /// Mouse position while the left button is held
    drag: Option<(f64, f64)>,
    redraw: bool,
    tick: Duration,
    next_tick: Instant,
    profile: Option<Profile>,
    /// Connection instructions shown in the corner; I switches
    info: Vec<String>,
    show_info: bool,
    info_checked: Option<Instant>,
}

impl App<'_> {
    fn fail(&mut self, event_loop: &ActiveEventLoop, message: String) {
        self.error = Some(message);
        event_loop.exit();
    }

    fn toggle_fullscreen(&self, on: Option<bool>) {
        let Some(window) = &self.window else { return };
        let on = on.unwrap_or(window.fullscreen().is_none());
        window.set_fullscreen(on.then_some(Fullscreen::Borderless(None)));
    }

    /// Runs at twice the display's frame rate: held keys, spinning, new frames
    fn on_tick(&mut self) {
        // The address can change (DHCP), so look again now and then
        if self.info_checked.map_or(true, |t| t.elapsed() >= Duration::from_secs(10)) {
            self.info_checked = Some(Instant::now());
            let info = overlay::lines(overlay::local_ip(), self.sim.port, self.sim.ddp_port, self.sim.chains, self.sim.leds);
            if info != self.info {
                self.info = info;
                self.redraw = true;
            }
        }
        if self.view3d {
            if self.camera.spinning {
                self.camera.orbit(0.15 / self.sim.fps, 0.0);
                self.redraw = true;
            }
        }
        let frame = self.sim.frame.lock().unwrap().clone();
        if let Some(frame) = frame {
            if self.shown.as_ref().map_or(true, |s| s[..] != frame[..]) {
                self.redraw = true;
            }
        }
        // The mouse pointer hides when it has not moved for a while, as on a kiosk
        let visible = self.mouse_moved.elapsed() < Duration::from_secs(2);
        if visible != self.cursor_visible {
            self.cursor_visible = visible;
            if let Some(window) = &self.window {
                window.set_cursor_visible(visible);
            }
        }
    }

    fn draw(&mut self) -> Result<(), String> {
        let (Some(surface), (w, h)) = (self.surface.as_mut(), self.size) else { return Ok(()) };
        if w == 0 || h == 0 {
            return Ok(());
        }
        let frame = self.sim.frame.lock().unwrap().clone();
        let t = Instant::now();
        let mut buffer = surface.buffer_mut().map_err(|e| format!("window buffer: {e}"))?;
        match &frame {
            Some(frame) if self.view3d => sim3d::render(self.sim, &self.camera, frame, w, h, &mut buffer),
            Some(frame) => {
                if let Some(fit) = &self.fit {
                    self.sim.render(fit, frame, &mut self.light, &mut buffer);
                }
            }
            None => buffer.fill(0),
        }
        if self.show_info {
            overlay::draw(&mut buffer, w, h, &self.info);
        }
        let rendered = Instant::now();
        buffer.present().map_err(|e| format!("showing the window: {e}"))?;
        self.shown = frame;
        if let Some(p) = &mut self.profile {
            p.render += rendered - t;
            p.present += rendered.elapsed();
            p.frames += 1;
            if p.since.elapsed() >= Duration::from_secs(5) {
                let n = p.frames.max(1) as f64;
                eprintln!(
                    "sim: {w}x{h} {}, {:.1} fps drawn, render {:.1} ms, show {:.1} ms per frame",
                    if self.view3d { "3d" } else { "flat" },
                    p.frames as f64 / p.since.elapsed().as_secs_f64(),
                    p.render.as_secs_f64() * 1000.0 / n,
                    p.present.as_secs_f64() * 1000.0 / n
                );
                *p = Profile::default();
            }
        }
        Ok(())
    }
}

impl ApplicationHandler for App<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let sim = self.sim;
        let (w0, h0) = if sim.view3d {
            (1200.0, 800.0)
        } else {
            let ppc = (1800.0 / sim.width_cm).min(900.0 / sim.height_cm);
            ((sim.width_cm * ppc).round(), (sim.height_cm * ppc).round())
        };
        let title = format!(
            "LED display: {} chains x {} LEDs, {} x {} cm  |  V: flat/3D view, F: fullscreen, I: address, \
             players: arrows+Enter and WASD+Space, 3D: drag to orbit, scroll to zoom, R: spin",
            sim.chains, sim.leds, sim.width_cm, sim.height_cm
        );
        let attributes = Window::default_attributes()
            .with_title(title)
            .with_inner_size(LogicalSize::new(w0, h0))
            .with_fullscreen(sim.fullscreen.then_some(Fullscreen::Borderless(None)));
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Rc::new(w),
            Err(e) => return self.fail(event_loop, format!("cannot open window: {e}")),
        };
        let surface = Context::new(window.clone()).and_then(|context| Surface::new(&context, window.clone()));
        match surface {
            Ok(s) => self.surface = Some(s),
            Err(e) => return self.fail(event_loop, format!("cannot draw in the window: {e}")),
        }
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                let (w, h) = (size.width as usize, size.height as usize);
                if let (Some(surface), Some(nw), Some(nh)) =
                    (self.surface.as_mut(), NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    if let Err(e) = surface.resize(nw, nh) {
                        return self.fail(event_loop, format!("resizing the window: {e}"));
                    }
                    self.size = (w, h);
                    self.fit = Some(self.sim.fit(w, h));
                    self.redraw = true;
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.draw() {
                    self.fail(event_loop, e);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let pressed = event.state == ElementState::Pressed;
                if let Some((player, button)) = player_key(code) {
                    // Only changes go to the game: no key repeat
                    let changed = if pressed { self.keys_down.insert(code) } else { self.keys_down.remove(&code) };
                    if changed {
                        if let Some(on_input) = &self.sim.on_input {
                            on_input(player, button, pressed);
                        }
                    }
                    return;
                }
                if !pressed || event.repeat {
                    return;
                }
                match code {
                    KeyCode::KeyV => {
                        self.view3d = !self.view3d;
                        self.redraw = true;
                    }
                    KeyCode::KeyR if self.view3d => self.camera.spinning = !self.camera.spinning,
                    KeyCode::KeyF => self.toggle_fullscreen(None),
                    KeyCode::KeyI => {
                        self.show_info = !self.show_info;
                        self.redraw = true;
                    }
                    KeyCode::Escape => self.toggle_fullscreen(Some(false)),
                    _ => {}
                }
            }
            // Keys let go while the window was not looking would otherwise stay held
            WindowEvent::Focused(false) => {
                for code in std::mem::take(&mut self.keys_down) {
                    if let (Some((player, button)), Some(on_input)) = (player_key(code), &self.sim.on_input) {
                        on_input(player, button, false);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = (position.x, position.y);
                self.mouse = Some(p);
                self.mouse_moved = Instant::now();
                if let (Some((x0, y0)), true) = (self.drag, self.view3d) {
                    self.camera.orbit(-(p.0 - x0) * 0.008, (p.1 - y0) * 0.008);
                    self.redraw = true;
                }
                if self.drag.is_some() {
                    self.drag = Some(p);
                }
            }
            WindowEvent::CursorLeft { .. } => self.mouse = None,
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                self.drag = (state == ElementState::Pressed).then_some(self.mouse).flatten();
            }
            WindowEvent::MouseWheel { delta, .. } if self.view3d => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
                };
                self.camera.zoom(0.9f64.powf(lines));
                self.redraw = true;
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.sim.failed.load(Ordering::Relaxed) {
            return event_loop.exit();
        }
        let now = Instant::now();
        if now >= self.next_tick {
            self.on_tick();
            // Skip ticks that were missed rather than catching up
            self.next_tick = (self.next_tick + self.tick).max(now);
        }
        if self.redraw {
            self.redraw = false;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glow() {
        let sim = Simulator {
            chains: 12, leds: 144, width_cm: 240.0, height_cm: 100.0, led_scale: 4.0, pipe_cm: None,
            cylinder: false, view3d: false, fullscreen: false, port: 8080, ddp_port: None, fps: 40.0, on_input: None,
            frame: Arc::new(Mutex::new(None)), failed: Arc::new(AtomicBool::new(false)),
        };
        let fit = sim.fit(1200, 500);
        let mut frame = vec![0u8; 12 * 144 * 3];
        frame[(50 * 12 + 5) * 3] = 255; // one red LED: chain 5, LED 50
        for j in 0..144 {
            frame[j * 12 * 3] = 38; // chain 0 lit at 15 %
        }
        let (mut light, mut out) = (Vec::new(), vec![0u32; 1200 * 500]);
        sim.render(&fit, &frame, &mut light, &mut out);
        let red = |x: usize, y: usize| out[y * 1200 + x] >> 16;

        // 5 px per cm: chain 5 is at x = 550, LED 50 at y = 175; core is x 545..555
        assert_eq!(red(550, 175), 255);
        let halo = red(555, 175);
        assert!((60..200).contains(&halo), "halo next to the core is {halo}");
        assert!(red(570, 175) < halo / 4, "halo fades out");
        assert_eq!(red(600, 175), 0);
        // Overlapping LEDs do not add up: a dim chain stays dim
        assert_eq!(red(50, 250), 38);
    }
}
