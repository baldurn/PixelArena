//! Fireworks: rockets whistle up from the bottom and burst into sparks that
//! fall under gravity, leaving fading trails. Each burst booms.

use display_demos::{hue, Game, Rng};
use serde_json::json;

const GRAVITY: f64 = 250.0; // cm/s²
const SPARK_GRAVITY: f64 = 90.0; // sparks float down slower
const TRAIL_KEEP: f64 = 0.78; // trail brightness kept per tick

struct Spark {
    s: f64,
    z: f64,
    vs: f64,
    vz: f64,
    life: f64,
    lifetime: f64,
    colour: [f64; 3],
}

struct Rocket {
    s: f64,
    z: f64,
    vs: f64,
    vz: f64,
}

fn main() {
    let mut rng = Rng::new(0xb00f);
    let mut rockets: Vec<Rocket> = Vec::new();
    let mut sparks: Vec<Spark> = Vec::new();
    let mut next_launch = 0.0;

    Game::connect("Fireworks").run(|g, dt, frame, sounds| {
        next_launch -= dt;
        if next_launch <= 0.0 {
            next_launch = rng.range(0.4, 1.4);
            rockets.push(Rocket {
                s: rng.range(0.0, g.width_cm),
                z: g.height_cm,
                vs: rng.range(-20.0, 20.0),
                // Bursts between about 55 and 90 cm up
                vz: -rng.range(165.0, 210.0),
            });
            sounds.push(json!({"type": "tone", "wave": "sine", "freq": 700, "to_freq": 1600,
                               "ms": 650, "volume": 0.1}));
        }

        frame.fade(TRAIL_KEEP);
        rockets.retain_mut(|r| {
            r.vz += GRAVITY * dt;
            r.s = g.wrap(r.s + r.vs * dt);
            r.z += r.vz * dt;
            frame.splat(g, r.s, r.z, 3.0, [255.0, 200.0, 120.0]);
            if r.vz < -25.0 {
                return true;
            }
            // Burst at the top of the climb
            let base = rng.range(0.0, 1.0);
            let count = rng.range(40.0, 70.0) as usize;
            for k in 0..count {
                let angle = rng.range(0.0, std::f64::consts::TAU);
                let speed = rng.range(20.0, 95.0);
                let lifetime = rng.range(1.0, 1.9);
                // Some bursts are two-coloured
                let h = if k % 2 == 0 { base } else { base + 0.12 };
                sparks.push(Spark {
                    s: r.s,
                    z: r.z,
                    vs: speed * angle.cos(),
                    vz: speed * angle.sin(),
                    life: lifetime,
                    lifetime,
                    colour: hue(h, 255.0),
                });
            }
            frame.splat(g, r.s, r.z, 18.0, [255.0, 255.0, 220.0]);
            sounds.push(json!({"type": "tone", "wave": "noise", "freq": 1000, "ms": 350, "volume": 0.3}));
            sounds.push(json!({"type": "tone", "wave": "sine", "freq": 90, "to_freq": 40, "ms": 450, "volume": 0.45}));
            false
        });

        let drag = (-dt * 1.5).exp();
        sparks.retain_mut(|p| {
            p.vs *= drag;
            p.vz = p.vz * drag + SPARK_GRAVITY * dt;
            p.s = g.wrap(p.s + p.vs * dt);
            p.z += p.vz * dt;
            p.life -= dt;
            let left = (p.life / p.lifetime).max(0.0);
            // Sparks flicker a little as they burn out
            let flicker = if left < 0.4 { rng.range(0.4, 1.0) } else { 1.0 };
            frame.splat(g, p.s, p.z, 2.5, p.colour.map(|c| c * left.powf(1.5) * flicker));
            p.life > 0.0 && p.z < g.height_cm
        });
    });
}
