//! Bouncing balls: round in real centimetres, falling under gravity, rolling
//! around the circle and colliding with each other. Each bounce plays a note.

use display_demos::{hue, Game, Rng};
use serde_json::json;

const BALLS: usize = 7;
const RADIUS_CM: f64 = 16.0;
const GRAVITY: f64 = 500.0; // cm/s², a little gentler than real
const NOTES: [f64; 7] = [262.0, 294.0, 330.0, 392.0, 440.0, 523.0, 587.0];

struct Ball {
    s: f64,
    z: f64,
    vs: f64,
    vz: f64,
    colour: [f64; 3],
    note: f64,
}

fn main() {
    let game = Game::connect("Balls");
    let g = &game.geometry;
    let mut rng = Rng::new(0x5eed);
    let mut balls: Vec<Ball> = (0..BALLS)
        .map(|i| Ball {
            s: g.width_cm * i as f64 / BALLS as f64,
            z: rng.range(RADIUS_CM, g.height_cm / 2.0),
            vs: rng.range(60.0, 160.0) * if i % 2 == 0 { 1.0 } else { -1.0 },
            vz: 0.0,
            colour: hue(i as f64 / BALLS as f64, 255.0),
            note: NOTES[i % NOTES.len()],
        })
        .collect();

    game.run(|g, dt, frame, sounds| {
        let floor = g.height_cm - RADIUS_CM;
        for b in balls.iter_mut() {
            b.vz += GRAVITY * dt;
            b.s = g.wrap(b.s + b.vs * dt);
            b.z += b.vz * dt;
            if b.z > floor && b.vz > 0.0 {
                b.z = floor;
                // Keep them lively: a ball that has nearly stopped gets kicked up again
                b.vz = if b.vz < 150.0 { -rng.range(350.0, 450.0) } else { -b.vz * 0.92 };
                sounds.push(json!({"type": "tone", "wave": "sine", "freq": b.note, "ms": 120, "volume": 0.3}));
            }
            if b.z < RADIUS_CM && b.vz < 0.0 {
                b.z = RADIUS_CM;
                b.vz = -b.vz;
            }
            if !g.wraps {
                let (lo, hi) = (RADIUS_CM, g.width_cm - RADIUS_CM);
                if (b.s < lo && b.vs < 0.0) || (b.s > hi && b.vs > 0.0) {
                    b.vs = -b.vs;
                }
            }
        }
        // Elastic collisions between equal balls: swap velocity along the line between centres
        for i in 0..balls.len() {
            for j in i + 1..balls.len() {
                let ds = g.ds(balls[i].s, balls[j].s);
                let dz = balls[j].z - balls[i].z;
                let dist = (ds * ds + dz * dz).sqrt();
                if dist >= 2.0 * RADIUS_CM || dist == 0.0 {
                    continue;
                }
                let (ns, nz) = (ds / dist, dz / dist);
                let closing = (balls[i].vs - balls[j].vs) * ns + (balls[i].vz - balls[j].vz) * nz;
                if closing > 0.0 {
                    balls[i].vs -= closing * ns;
                    balls[i].vz -= closing * nz;
                    balls[j].vs += closing * ns;
                    balls[j].vz += closing * nz;
                    sounds.push(json!({"type": "tone", "wave": "square", "freq": balls[i].note * 2.0,
                                       "ms": 50, "volume": 0.2}));
                }
            }
        }

        frame.clear();
        for b in &balls {
            for x in g.cols_near(b.s, RADIUS_CM) {
                let ds = g.ds(b.s, g.s(x));
                if ds.abs() >= RADIUS_CM {
                    continue;
                }
                // The ball's height at this column, from the circle equation
                let half = (RADIUS_CM * RADIUS_CM - ds * ds).sqrt();
                for y in g.rows(b.z - half, b.z + half) {
                    let dist = (ds * ds + (g.z(y) - b.z).powi(2)).sqrt();
                    let light = ((RADIUS_CM - dist) / 3.0).clamp(0.0, 1.0); // soft rim
                    frame.add(x, y, b.colour.map(|c| c * light));
                }
            }
        }
    });
}
