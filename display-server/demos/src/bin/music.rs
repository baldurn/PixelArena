//! Music: plays an arpeggio over Am - F - C - G and lights it up. Each note
//! lights a bar at its own place around the circle (the twelve notes of the
//! octave are spread evenly around it), higher notes reach higher, and the bass
//! makes the base of the whole ring glow. Bars drift around the ring as they
//! fade, so the music leaves swirling trails.

use display_demos::{hue, Game};
use serde_json::json;

const BPM: f64 = 112.0;
/// Chord tones as MIDI notes: Am, F, C, G
const CHORDS: [[i32; 3]; 4] = [[57, 60, 64], [53, 57, 60], [48, 52, 55], [55, 59, 62]];
/// Eight eighth notes per bar: (chord tone, octaves up)
const PATTERN: [(usize, i32); 8] = [(0, 0), (1, 0), (2, 0), (0, 1), (2, 0), (1, 0), (0, 1), (2, 1)];
const BAR_HALF_WIDTH_CM: f64 = 35.0;
const BASS_GLOW_CM: f64 = 35.0;
const TURNS_PER_SECOND: f64 = 0.02;
/// How fast a bar drifts around the ring after its note, turns per second
const DRIFT: f64 = 0.06;
const FADE_PER_SECOND: f64 = 1.2;

fn freq(midi: i32) -> f64 {
    440.0 * 2f64.powf((midi - 69) as f64 / 12.0)
}

struct Bar {
    midi: i32,
    energy: f64,
    age: f64,
}

fn main() {
    let eighth = 60.0 / BPM / 2.0;
    let mut t = 0.0;
    let mut next = 0usize; // next eighth note to play
    let mut bars: Vec<Bar> = Vec::new();
    let mut bass = (0, 0.0); // (pitch class, energy)

    Game::connect("Music").run(|g, dt, frame, sounds| {
        t += dt;
        while next as f64 * eighth <= t {
            let chord = CHORDS[(next / 8) % CHORDS.len()];
            let (tone, octave) = PATTERN[next % 8];
            let midi = chord[tone] + 12 * octave;
            sounds.push(json!({"type": "tone", "wave": "triangle", "freq": freq(midi),
                               "ms": eighth * 900.0, "volume": 0.25}));
            bars.push(Bar { midi, energy: 1.0, age: 0.0 });
            // Bass on beats one and three
            if next % 4 == 0 {
                let root = chord[0] - 12;
                sounds.push(json!({"type": "tone", "wave": "sine", "freq": freq(root),
                                   "ms": eighth * 3800.0, "volume": 0.35}));
                bass = (root.rem_euclid(12), 1.0);
            }
            next += 1;
        }
        let decay = (-dt * FADE_PER_SECOND).exp();
        for b in bars.iter_mut() {
            b.energy *= decay;
            b.age += dt;
        }
        bars.retain(|b| b.energy > 0.02);
        bass.1 *= (-dt * 1.2).exp();

        frame.clear();
        let turn = t * TURNS_PER_SECOND;
        // Bass: a glow around the whole base, in the colour of the chord's root
        let bass_colour = hue(bass.0 as f64 / 12.0, 255.0 * bass.1 * 0.9);
        for y in g.rows(g.height_cm - BASS_GLOW_CM, g.height_cm) {
            let up = (g.height_cm - g.z(y)) / BASS_GLOW_CM;
            for x in 0..g.width {
                frame.add(x, y, bass_colour.map(|c| c * (1.0 - up)));
            }
        }
        // Notes: a bar per note at its pitch class's place around the circle
        for b in &bars {
            let pitch_class = b.midi.rem_euclid(12) as f64 / 12.0;
            let centre = g.wrap((pitch_class + turn + b.age * DRIFT) * g.width_cm);
            // Higher notes reach higher; bars sink a little as they fade
            let pitch = ((b.midi - 48) as f64 / 30.0).clamp(0.0, 1.0);
            let reach = (0.5 + 0.5 * pitch) * g.height_cm * b.energy.powf(0.3);
            let colour = hue(pitch_class, 255.0);
            for x in g.cols_near(centre, BAR_HALF_WIDTH_CM) {
                let across = 1.0 - (g.ds(centre, g.s(x)).abs() / BAR_HALF_WIDTH_CM);
                if across <= 0.0 {
                    continue;
                }
                for y in g.rows(g.height_cm - reach, g.height_cm) {
                    // Brightest at the top of the bar
                    let up = (g.height_cm - g.z(y)) / reach;
                    let light = b.energy.powf(0.7) * across * (0.5 + 0.5 * up);
                    frame.add(x, y, colour.map(|c| c * light));
                }
            }
        }
    });
}
