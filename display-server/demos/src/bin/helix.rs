//! Barber pole: coloured stripes spiral up the cylinder at a true 30 degree
//! angle and turn around it. The angle slowly breathes between steep and flat.

use display_demos::{hue, Game};

const STRIPES: f64 = 6.0;
const TURNS_PER_SECOND: f64 = 0.12;

fn main() {
    let mut t = 0.0f64;
    Game::connect("Helix").run(|g, dt, frame, _| {
        t += dt;
        // Stripe slope in cm up per cm across: tan(30 degrees), breathing +-15 degrees
        let angle = (30.0 + 15.0 * (t * 0.2).sin()).to_radians();
        let slope = angle.tan();
        for x in 0..g.width {
            for y in 0..g.height {
                // Fraction of the way around, shifted by height so stripes slant
                let phase = (g.s(x) + (g.height_cm - g.z(y)) / slope) / g.width_cm + t * TURNS_PER_SECOND;
                let stripe = (phase * STRIPES).rem_euclid(STRIPES);
                // Soft edges between stripes
                let edge = (stripe.fract().min(1.0 - stripe.fract()) * 6.0).min(1.0);
                let colour = hue(stripe.floor() / STRIPES + t * 0.02, 255.0 * edge);
                frame.set(x, y, colour);
            }
        }
    });
}
