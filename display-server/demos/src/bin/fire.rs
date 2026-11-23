//! Fire: flames rise all around the ring at a real speed and height. Heat
//! climbs the pipes row by row, losing a random amount on the way, and now and
//! then licks sideways into the next pipe. Crackles.

use display_demos::{Game, Rng};
use serde_json::json;

const FLAME_CM: f64 = 55.0; // typical flame height
const RISE_CM_PER_S: f64 = 120.0;

/// Black, through red and orange, to yellow-white
fn colour(heat: f64) -> [f64; 3] {
    let t = heat.clamp(0.0, 1.0) * 3.0;
    [
        (t * 255.0).min(255.0),
        ((t - 1.0) * 255.0).clamp(0.0, 255.0) * 0.8,
        ((t - 2.0) * 255.0).clamp(0.0, 255.0) * 0.5,
    ]
}

fn main() {
    let game = Game::connect("Fire");
    let (w, h) = (game.geometry.width, game.geometry.height);
    let mut rng = Rng::new(0xf12e);
    let mut heat = vec![0f64; w * h];
    // How strongly each pipe burns at the bottom; wanders over time
    let mut base: Vec<f64> = (0..w).map(|_| rng.range(0.6, 1.0)).collect();
    let mut carry = 0.0;
    let mut next_crackle = 0.0;

    game.run(|g, dt, frame, sounds| {
        for b in base.iter_mut() {
            *b = (*b + rng.range(-0.08, 0.08)).clamp(0.35, 1.0);
        }
        // Heat climbs whole rows at a time; the remainder carries to the next tick
        carry += RISE_CM_PER_S * dt / g.row_cm;
        let steps = carry as usize;
        carry -= steps as f64;
        // Average loss per row climbed, so flames reach about FLAME_CM
        let decay = 2.0 * g.row_cm / FLAME_CM;
        for _ in 0..steps {
            for x in 0..w {
                heat[(h - 1) * w + x] = base[x] * rng.range(0.8, 1.0);
            }
            for y in 0..h - 1 {
                for x in 0..w {
                    let from = if rng.range(0.0, 1.0) < 0.02 {
                        let dx = if rng.range(0.0, 1.0) < 0.5 { 1 } else { -1 };
                        let n = x as i64 + dx;
                        if g.wraps { n.rem_euclid(w as i64) as usize } else { n.clamp(0, w as i64 - 1) as usize }
                    } else {
                        x
                    };
                    heat[y * w + x] = (heat[(y + 1) * w + from] - rng.range(0.0, decay)).max(0.0);
                }
            }
        }
        for y in 0..h {
            for x in 0..w {
                frame.set(x, y, colour(heat[y * w + x]));
            }
        }

        next_crackle -= dt;
        if next_crackle <= 0.0 {
            next_crackle = rng.range(0.05, 0.5);
            sounds.push(json!({"type": "tone", "wave": "noise", "freq": 1000, "ms": rng.range(15.0, 50.0),
                               "volume": rng.range(0.08, 0.2)}));
        }
    });
}
