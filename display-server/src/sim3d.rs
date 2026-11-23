//! 3D view for the simulator: the chains stand where they really are (in a
//! circle for a cylinder, in a row otherwise), seen in perspective from an
//! orbiting camera. Software rendered: each pipe is drawn as a vertical strip,
//! far pipes first so near ones cover them.

use std::f64::consts::{PI, TAU};

use crate::sim::{Simulator, LED_CM, PIPE_BODY};

const FOV: f64 = 40.0; // vertical field of view, degrees
const SUB: usize = 4; // light samples per LED along a chain
const FAR_DIM: f64 = 0.35; // how much darker the far side is than the near side
const FLOOR: f32 = 40.0; // brightness of the floor circle

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}

pub struct Camera {
    /// Around the display, radians; 0 looks at column 0 from outside
    pub yaw: f64,
    /// Above the horizon, radians
    pub pitch: f64,
    pub distance_cm: f64,
    pub spinning: bool,
}

impl Camera {
    pub fn new(sim: &Simulator) -> Self {
        let across = if sim.cylinder { sim.width_cm / PI } else { sim.width_cm };
        Self {
            yaw: 0.0,
            pitch: 25f64.to_radians(),
            distance_cm: 1.4 * across.max(sim.height_cm) / (FOV.to_radians() / 2.0).tan() / 2.0,
            spinning: false,
        }
    }

    pub fn orbit(&mut self, dyaw: f64, dpitch: f64) {
        self.yaw = (self.yaw + dyaw).rem_euclid(TAU);
        self.pitch = (self.pitch + dpitch).clamp(-30f64.to_radians(), 85f64.to_radians());
    }

    pub fn zoom(&mut self, factor: f64) {
        self.distance_cm = (self.distance_cm * factor).clamp(50.0, 100_000.0);
    }
}

/// Where each chain stands: (x, z) on the floor in cm, centred on the origin.
/// On a cylinder x increases left to right for someone standing outside.
fn positions(sim: &Simulator) -> Vec<(f64, f64)> {
    (0..sim.chains)
        .map(|i| {
            let f = (i as f64 + 0.5) / sim.chains as f64;
            if sim.cylinder {
                let r = sim.width_cm / TAU;
                (r * (TAU * f).sin(), r * (TAU * f).cos())
            } else {
                ((f - 0.5) * sim.width_cm, 0.0)
            }
        })
        .collect()
}

pub fn render(sim: &Simulator, cam: &Camera, frame: &[u8], w: usize, h: usize, out: &mut [u32]) {
    out.fill(0);
    let pipes = sim.pipe_cm.is_some();
    let pipe_cm = sim.pipe_cm.unwrap_or(LED_CM * sim.led_scale);
    let row_cm = sim.height_cm / sim.leds as f64;

    let target = [0.0, sim.height_cm / 2.0, 0.0];
    let eye = [
        cam.distance_cm * cam.pitch.cos() * cam.yaw.sin(),
        target[1] + cam.distance_cm * cam.pitch.sin(),
        cam.distance_cm * cam.pitch.cos() * cam.yaw.cos(),
    ];
    let forward = norm(sub(target, eye));
    let right = norm(cross(forward, [0.0, 1.0, 0.0]));
    let up = cross(right, forward);
    let focal = h as f64 / 2.0 / (FOV.to_radians() / 2.0).tan();
    // Screen x, screen y, depth
    let project = |p: V3| {
        let d = sub(p, eye);
        let z = dot(d, forward);
        (w as f64 / 2.0 + focal * dot(d, right) / z, h as f64 / 2.0 - focal * dot(d, up) / z, z)
    };
    let mut put = |x: f64, y: f64, rgb: [f32; 3], alpha: f32| {
        if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 {
            return;
        }
        let p = &mut out[y as usize * w + x as usize];
        let old = [(*p >> 16) & 255, (*p >> 8) & 255, *p & 255].map(|c| c as f32);
        let [r, g, b] = [0, 1, 2].map(|c| (old[c] + (rgb[c].min(255.0) - old[c]) * alpha) as u32);
        *p = r << 16 | g << 8 | b;
    };

    // Floor outline, so the shape reads even when everything is dark
    if sim.cylinder {
        let r = sim.width_cm / TAU + pipe_cm;
        for k in 0..720 {
            let a = TAU * k as f64 / 720.0;
            let (x, y, z) = project([r * a.sin(), 0.0, r * a.cos()]);
            if z > 1.0 {
                put(x, y, [FLOOR; 3], 1.0);
            }
        }
    }

    let positions = positions(sim);
    let depth = |&(x, z): &(f64, f64)| dot(sub([x, sim.height_cm / 2.0, z], eye), forward);
    let (near, far) = positions.iter().map(depth).fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(d), hi.max(d)));
    let mut order: Vec<usize> = (0..sim.chains).collect();
    order.sort_by(|&a, &b| depth(&positions[b]).total_cmp(&depth(&positions[a])));

    let samples = sim.leds * SUB;
    let mut light = vec![[0f32; 3]; samples];
    for i in order {
        let (px, pz) = positions[i];
        let top = project([px, sim.height_cm, pz]);
        let bottom = project([px, 0.0, pz]);
        if top.2 < 1.0 || bottom.2 < 1.0 {
            continue; // behind the camera
        }
        let dim = if far > near { 1.0 - FAR_DIM * (depth(&positions[i]) - near) / (far - near) } else { 1.0 } as f32;

        // Light along the chain, from the top, the same way the flat view lights a pipe
        let lit = (0..sim.leds).any(|j| frame[(j * sim.chains + i) * 3..][..3] != [0, 0, 0]);
        if lit {
            for (k, sample) in light.iter_mut().enumerate() {
                let z = (k as f64 + 0.5) * row_cm / SUB as f64;
                let reach = if pipes { pipe_cm * 0.625 } else { pipe_cm / 2.0 };
                let first = ((z - reach) / row_cm - 0.5).floor().max(0.0) as usize;
                let last = (((z + reach) / row_cm - 0.5).ceil().max(0.0) as usize).min(sim.leds - 1);
                *sample = [0.0; 3];
                for j in first..=last {
                    let d = z - (j as f64 + 0.5) * row_cm;
                    let weight = if pipes {
                        (-0.5 * (d / (pipe_cm / 4.0)).powi(2)).exp() as f32
                    } else if d.abs() <= pipe_cm / 2.0 {
                        1.0
                    } else {
                        0.0
                    };
                    let rgb = &frame[(j * sim.chains + i) * 3..][..3];
                    for c in 0..3 {
                        sample[c] = sample[c].max(weight * rgb[c] as f32);
                    }
                }
            }
        }

        let (y0, y1) = (top.1, bottom.1);
        if y1 <= y0 {
            continue;
        }
        for sy in (y0.floor().max(0.0) as usize)..(y1.ceil().min(h as f64) as usize) {
            let s = ((sy as f64 + 0.5 - y0) / (y1 - y0)).clamp(0.0, 1.0);
            // Perspective-correct position along the pipe, 0 at the top
            let inv_z = (1.0 - s) / top.2 + s / bottom.2;
            let t = s / bottom.2 / inv_z;
            let cx = top.0 + (bottom.0 - top.0) * s;
            let half = (focal * pipe_cm / 2.0 * inv_z).max(0.5);
            let colour = if lit { light[((t * samples as f64) as usize).min(samples - 1)] } else { [0.0; 3] };
            for sx in (cx - half - 1.0).floor().max(0.0) as usize..((cx + half + 1.0).ceil().min(w as f64) as usize) {
                let d = sx as f64 + 0.5 - cx;
                let edge = (half + 0.5 - d.abs()).clamp(0.0, 1.0) as f32;
                if edge <= 0.0 {
                    continue;
                }
                // Lit cylinder: brightest down the middle
                let shade = (0.6 + 0.4 * (1.0 - (d / half).powi(2)).max(0.0).sqrt()) as f32 * dim;
                let body = if pipes { PIPE_BODY } else { 0.0 };
                let rgb = colour.map(|c| c.max(body) * shade);
                put(sx as f64, sy as f64, rgb, edge);
            }
        }
    }
}
