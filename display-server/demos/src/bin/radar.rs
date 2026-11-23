//! Radar: a green sweep circles the ring with a fading phosphor trail. Range
//! lines run around it every 25 cm. Slowly drifting targets light up and ping
//! when the beam passes them; higher targets ping higher.

use display_demos::{Game, Rng};
use serde_json::json;

const TURNS_PER_SECOND: f64 = 0.25;
const TRAIL: f64 = 10.0; // larger is a shorter trail
const RANGE_LINE_CM: f64 = 25.0;
const TARGETS: usize = 6;

struct Target {
    s: f64,
    z: f64,
    vs: f64,
    vz: f64,
    glow: f64,
}

fn main() {
    let game = Game::connect("Radar");
    let g = &game.geometry;
    let mut rng = Rng::new(0x7ada);
    let mut targets: Vec<Target> = (0..TARGETS)
        .map(|_| Target {
            s: rng.range(0.0, g.width_cm),
            z: rng.range(10.0, g.height_cm - 10.0),
            vs: rng.range(-12.0, 12.0),
            vz: rng.range(-4.0, 4.0),
            glow: 0.0,
        })
        .collect();
    let mut t = 0.0f64;

    game.run(|g, dt, frame, sounds| {
        let before = (t * TURNS_PER_SECOND).fract();
        t += dt;
        let sweep = (t * TURNS_PER_SECOND).fract();

        for tg in targets.iter_mut() {
            tg.s = g.wrap(tg.s + tg.vs * dt);
            tg.z += tg.vz * dt;
            if !(10.0..g.height_cm - 10.0).contains(&tg.z) {
                tg.vz = -tg.vz;
            }
            tg.glow *= (-dt).exp();
            // Did the beam pass the target during this tick?
            let at = tg.s / g.width_cm;
            let passed = if before <= sweep { before < at && at <= sweep } else { at > before || at <= sweep };
            if passed {
                tg.glow = 1.0;
                let pitch = 1600.0 - 8.0 * tg.z;
                sounds.push(json!({"type": "tone", "wave": "sine", "freq": pitch, "ms": 90, "volume": 0.25}));
            }
        }

        frame.clear();
        for x in 0..g.width {
            // How far behind the beam this column is, in turns
            let behind = (sweep - g.s(x) / g.width_cm).rem_euclid(1.0);
            let trail = (-behind * TRAIL).exp();
            for y in 0..g.height {
                let z = g.z(y);
                let on_line = (z % RANGE_LINE_CM) < g.row_cm * 1.5;
                let base = if on_line { 0.18 + 0.5 * trail } else { 0.5 * trail };
                frame.add(x, y, [0.0, 200.0 * base, 50.0 * base]);
            }
        }
        for tg in &targets {
            frame.splat(g, tg.s, tg.z, 6.0, [140.0 * tg.glow, 255.0 * tg.glow, 140.0 * tg.glow]);
        }
    });
}
