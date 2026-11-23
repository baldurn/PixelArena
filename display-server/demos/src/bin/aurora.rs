//! Aurora: curtains of northern lights ripple around the ring, green at their
//! lower edge fading into violet rays above, over a soft drone. Every wave fits
//! a whole number of times around the circle, so there is no seam on a
//! cylinder.

use display_demos::{Game, Rng};
use serde_json::json;
use std::f64::consts::TAU;

const GREEN: [f64; 3] = [40.0, 255.0, 120.0];
const VIOLET: [f64; 3] = [170.0, 50.0, 255.0];
/// Low notes for the drone, Hz (A minor pentatonic)
const DRONE: [f64; 5] = [110.0, 130.8, 146.8, 164.8, 196.0];

struct Curtain {
    /// Lower edge, cm down from the top
    edge_cm: f64,
    /// (waves around the circle, amplitude cm, speed rad/s, phase)
    waves: [(f64, f64, f64, f64); 3],
    /// Height of the rays above the edge, cm
    rays_cm: f64,
    brightness: f64,
}

impl Curtain {
    fn edge(&self, angle: f64, t: f64) -> f64 {
        self.edge_cm + self.waves.iter().map(|&(n, amp, speed, phase)| amp * (n * angle + speed * t + phase).sin()).sum::<f64>()
    }
}

fn main() {
    let mut rng = Rng::new(0xa0a0);
    let curtains = [
        Curtain {
            edge_cm: 70.0,
            waves: [(2.0, 10.0, 0.35, 0.0), (5.0, 5.0, -0.8, 1.0), (9.0, 2.5, 1.3, 2.0)],
            rays_cm: 45.0,
            brightness: 1.0,
        },
        Curtain {
            edge_cm: 50.0,
            waves: [(3.0, 8.0, -0.25, 2.5), (7.0, 4.0, 0.6, 0.3), (11.0, 2.0, -1.1, 1.7)],
            rays_cm: 30.0,
            brightness: 0.6,
        },
    ];
    let mut t = 0.0;
    let mut next_drone = 0.0;

    Game::connect("Aurora").run(|g, dt, frame, sounds| {
        t += dt;
        next_drone -= dt;
        if next_drone <= 0.0 {
            next_drone = 4.0;
            let note = DRONE[rng.range(0.0, DRONE.len() as f64) as usize];
            sounds.push(json!({"type": "tone", "wave": "sine", "freq": note, "ms": 5000, "volume": 0.15}));
            sounds.push(json!({"type": "tone", "wave": "sine", "freq": note * 1.5, "ms": 5000, "volume": 0.07}));
        }

        frame.clear();
        for x in 0..g.width {
            // Fraction of the way around, as an angle; waves use whole multiples of it
            let angle = TAU * g.s(x) / g.width_cm;
            for c in &curtains {
                let edge = c.edge(angle, t);
                // Rays shimmer per pipe: brightness and height vary along the curtain
                let shimmer = 0.55
                    + 0.25 * (13.0 * angle + 2.3 * t).sin()
                    + 0.2 * (29.0 * angle - 3.7 * t + x as f64).sin();
                let rays = c.rays_cm * (0.7 + 0.3 * (7.0 * angle - 0.9 * t).sin());
                for y in g.rows(edge - rays * 2.5, edge + 4.0) {
                    let above = edge - g.z(y); // cm above the lower edge
                    let light = if above < 0.0 { (above / 2.0).exp() } else { (-above / rays).exp() };
                    let mix = (above / rays).clamp(0.0, 1.0);
                    let colour: [f64; 3] = std::array::from_fn(|i| GREEN[i] + (VIOLET[i] - GREEN[i]) * mix);
                    frame.add(x, y, colour.map(|v| v * light * shimmer * c.brightness));
                }
            }
        }
    });
}
