//! Digital rain: green streaks fall down the pipes at different speeds, a
//! bright head followed by a fading trail of flickering "glyphs".

use display_demos::{Game, Rng};

const DROPS_PER_PIPE_PER_SECOND: f64 = 0.5;

struct Drop {
    x: usize,
    /// Head position, cm down
    z: f64,
    speed: f64,
    trail: f64,
    seed: u64,
}

/// A stable pseudo-random value in 0..1 for a pixel, a drop and a moment
fn glyph(x: usize, y: usize, seed: u64, moment: u64) -> f64 {
    let mut v = (x as u64) << 40 ^ (y as u64) << 20 ^ seed ^ moment.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    v ^= v >> 33;
    v = v.wrapping_mul(0xff51_afd7_ed55_8ccd);
    v ^= v >> 33;
    (v >> 11) as f64 / (1u64 << 53) as f64
}

fn main() {
    let mut rng = Rng::new(0x4a1);
    let mut drops: Vec<Drop> = Vec::new();
    let mut t = 0.0;
    Game::connect("Rain").run(|g, dt, frame, _| {
        t += dt;
        for x in 0..g.width {
            if rng.range(0.0, 1.0) < DROPS_PER_PIPE_PER_SECOND * dt {
                drops.push(Drop {
                    x,
                    z: 0.0,
                    speed: rng.range(40.0, 160.0),
                    trail: rng.range(20.0, 70.0),
                    seed: rng.range(0.0, 1e15) as u64,
                });
            }
        }
        for d in drops.iter_mut() {
            d.z += d.speed * dt;
        }
        drops.retain(|d| d.z - d.trail < g.height_cm);

        frame.clear();
        // Glyphs change about 8 times a second
        let moment = (t * 8.0) as u64;
        for d in &drops {
            for y in g.rows(d.z - d.trail, d.z) {
                let behind = d.z - g.z(y);
                let fade = 1.0 - behind / d.trail;
                if behind < 2.0 {
                    frame.add(d.x, y, [180.0, 255.0, 180.0]);
                } else {
                    let flicker = 0.4 + 0.6 * glyph(d.x, y, d.seed, moment);
                    frame.add(d.x, y, [0.0, 255.0 * fade * flicker, 50.0 * fade * flicker]);
                }
            }
        }
    });
}
