//! Raindrops: drops land on the cylinder and send out rings. A ring travels
//! both ways around the circle and meets itself on the far side. Each drop
//! plays a plink.

use display_demos::{hue, Game, Rng};
use serde_json::json;

const SPEED: f64 = 110.0; // cm/s
const RING_CM: f64 = 9.0; // half-thickness of a ring
const FADE_SECONDS: f64 = 2.2;
const PENTATONIC: [f64; 5] = [523.0, 587.0, 659.0, 784.0, 880.0];

struct Ripple {
    s: f64,
    z: f64,
    age: f64,
    colour: [f64; 3],
}

fn main() {
    let mut rng = Rng::new(0xd20b);
    let mut ripples: Vec<Ripple> = Vec::new();
    let mut next_drop = 0.0;
    Game::connect("Ripples").run(|g, dt, frame, sounds| {
        next_drop -= dt;
        if next_drop <= 0.0 {
            next_drop = rng.range(0.3, 1.1);
            ripples.push(Ripple {
                s: rng.range(0.0, g.width_cm),
                z: rng.range(10.0, g.height_cm - 10.0),
                age: 0.0,
                colour: hue(rng.range(0.45, 0.75), 255.0), // blues and violets
            });
            let note = PENTATONIC[rng.range(0.0, PENTATONIC.len() as f64) as usize];
            sounds.push(json!({"type": "tone", "wave": "sine", "freq": note * 2.0, "to_freq": note,
                               "ms": 180, "volume": 0.35}));
        }
        for r in ripples.iter_mut() {
            r.age += dt;
        }
        ripples.retain(|r| r.age < FADE_SECONDS * 3.0);

        frame.clear();
        for x in 0..g.width {
            for r in &ripples {
                // Short way around, so a ring passing the far side meets its other half
                let ds = g.ds(r.s, g.s(x));
                let radius = r.age * SPEED;
                let fade = (-r.age / FADE_SECONDS).exp();
                // The splash itself, then the expanding ring and a fainter one behind it
                let splash = (1.0 - r.age * 4.0).max(0.0);
                for y in g.rows(r.z - radius - RING_CM * 2.0, r.z + radius + RING_CM * 2.0) {
                    let dist = (ds * ds + (g.z(y) - r.z).powi(2)).sqrt();
                    let ring = |behind: f64| (-((dist - radius + behind) / RING_CM).powi(2)).exp();
                    let light = fade * (ring(0.0) + 0.4 * ring(35.0)) + splash * (-dist / 8.0).exp();
                    if light > 0.01 {
                        frame.add(x, y, r.colour.map(|c| c * light));
                    }
                }
            }
        }
    });
}
