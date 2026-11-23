//! Duel: two players shoot at each other across the display. Player 1
//! (arrows + Enter, cyan) holds the bottom band, player 2 (W A S D + Space,
//! orange) the top band. Left and right move around the ring, up and down a
//! little within your band, fire sends a shot at the other side. Each hit
//! fills a fifth of your ring at your edge; the first to five wins.
//!
//! A side nobody has touched for a while is played by the computer, so with
//! nobody there it plays itself, and anyone can take over a side by pressing
//! a button.

use std::time::Duration;

use display_demos::{Button, Game, Geometry, Input, Rng};
use serde_json::{json, Value};

const WIN_SCORE: u32 = 5;
const ACROSS_CM_PER_S: f64 = 170.0;
const UPDOWN_CM_PER_S: f64 = 45.0;
const SHOT_CM_PER_S: f64 = 150.0;
const RELOAD_S: f64 = 0.25;
const MAX_SHOTS: usize = 4;
const SHIP_CM: f64 = 5.0;
/// How close a shot must pass to hit: across, and along the pipe
const HIT_ACROSS_CM: f64 = 8.0;
const HIT_ALONG_CM: f64 = 6.0;
/// Untouchable after being hit, seconds
const SHIELD_S: f64 = 1.2;
/// A side untouched for this long is played by the computer
const HUMAN_TIMEOUT: Duration = Duration::from_secs(15);
const CELEBRATE_S: f64 = 3.0;

struct Player {
    colour: [f64; 3],
    /// Band the ship moves in, cm down from the top
    zone: (f64, f64),
    /// Shots move this way along z: -1 up, 1 down
    aim: f64,
    s: f64,
    z: f64,
    score: u32,
    reload: f64,
    shield: f64,
    shots: Vec<(f64, f64)>,
    /// Computer play: where it wants to be along z, and its reaction delay
    ai_z: f64,
    ai_think: f64,
    ai_dir: f64,
}

impl Player {
    fn new(g: &Geometry, number: usize) -> Self {
        let h = g.height_cm;
        let (colour, zone, aim, s) = if number == 1 {
            ([0.0, 200.0, 255.0], (0.62 * h, 0.9 * h), -1.0, 0.0)
        } else {
            ([255.0, 110.0, 0.0], (0.1 * h, 0.38 * h), 1.0, g.width_cm / 2.0)
        };
        let z = (zone.0 + zone.1) / 2.0;
        Self { colour, zone, aim, s, z, score: 0, reload: 0.0, shield: 0.0, shots: Vec::new(),
               ai_z: z, ai_think: 0.0, ai_dir: 0.0 }
    }

    fn fire(&mut self, sounds: &mut Vec<Value>, pitch: f64) {
        if self.reload > 0.0 || self.shots.len() >= MAX_SHOTS || self.shield > 0.0 {
            return;
        }
        self.reload = RELOAD_S;
        self.shots.push((self.s, self.z + self.aim * SHIP_CM));
        sounds.push(json!({"type": "tone", "wave": "square", "freq": pitch, "to_freq": pitch / 2.0,
                           "ms": 70, "volume": 0.15}));
    }
}

/// Moves around the ring (or along a flat display, stopping at its ends)
fn move_across(g: &Geometry, s: f64, ds: f64) -> f64 {
    if g.wraps { g.wrap(s + ds) } else { (s + ds).clamp(0.0, g.width_cm) }
}

/// The computer's buttons for player `me`: left/right (-1..1), up/down, and fire
fn computer(g: &Geometry, me: &mut Player, enemy: &Player, rng: &mut Rng, dt: f64) -> (f64, f64, bool) {
    // A shot of the enemy's coming at us: step aside
    let threat = enemy.shots.iter().find(|&&(s, z)| {
        let ahead = (me.z - z) * enemy.aim; // positive while still approaching
        (0.0..35.0).contains(&ahead) && g.ds(me.s, s).abs() < HIT_ACROSS_CM * 1.5
    });
    // Rethink now and then, not every tick, so it is beatable
    me.ai_think -= dt;
    if me.ai_think <= 0.0 {
        me.ai_think = rng.range(0.15, 0.4);
        let lead = rng.range(-12.0, 12.0);
        let gap = g.ds(me.s, enemy.s + lead);
        me.ai_dir = if gap.abs() < 4.0 { 0.0 } else { gap.signum() };
        if rng.range(0.0, 1.0) < 0.2 {
            me.ai_z = rng.range(me.zone.0, me.zone.1);
        }
    }
    let across = match threat {
        // Away from the side the shot is on
        Some(&(s, _)) => if g.ds(me.s, s) > 0.0 { -1.0 } else { 1.0 },
        None => me.ai_dir,
    };
    let updown = if (me.ai_z - me.z).abs() < 2.0 { 0.0 } else { (me.ai_z - me.z).signum() };
    let fire = g.ds(me.s, enemy.s).abs() < 12.0 && rng.range(0.0, 1.0) < 0.3;
    (across, updown, fire)
}

fn main() {
    let mut rng = Rng::new(0xd0e1);
    let mut players: Option<[Player; 2]> = None;
    let mut sparks: Vec<(f64, f64, f64, f64, f64, [f64; 3])> = Vec::new(); // s, z, vs, vz, life, colour
    // Winner (0 or 1) and time left of the celebration
    let mut celebrate: Option<(usize, f64)> = None;
    let mut t = 0.0;

    Game::connect("Duel").run_with_input(|g, dt, frame, sounds, input: &Input| {
        t += dt;
        let ps = players.get_or_insert_with(|| [Player::new(g, 1), Player::new(g, 2)]);

        if let Some((winner, left)) = &mut celebrate {
            *left -= dt;
            if *left <= 0.0 {
                *ps = [Player::new(g, 1), Player::new(g, 2)];
                celebrate = None;
            } else {
                // The winner's colour pulses over everything
                let pulse = 0.5 + 0.5 * (t * 8.0).sin();
                let c = ps[*winner].colour.map(|v| v * (0.15 + 0.5 * pulse));
                for y in 0..g.height {
                    for x in 0..g.width {
                        frame.set(x, y, c);
                    }
                }
                return;
            }
        }

        frame.fade(0.55);
        for i in 0..2 {
            let (me, enemy) = if i == 0 { let [a, b] = ps; (a, &*b) } else { let [a, b] = ps; (b, &*a) };
            let human = input.active(i + 1, HUMAN_TIMEOUT);
            let (across, updown, fire) = if human {
                (input.x(i + 1), input.y(i + 1), input.pressed(i + 1, Button::Fire) || input.held(i + 1, Button::Fire))
            } else {
                computer(g, me, enemy, &mut rng, dt)
            };
            me.s = move_across(g, me.s, across * ACROSS_CM_PER_S * dt);
            me.z = (me.z + updown * UPDOWN_CM_PER_S * dt).clamp(me.zone.0, me.zone.1);
            me.reload -= dt;
            me.shield -= dt;
            if fire {
                me.fire(sounds, if i == 0 { 1000.0 } else { 700.0 });
            }
        }

        // Shots fly, and hit or leave the display
        for i in 0..2 {
            let [a, b] = ps;
            let (me, enemy) = if i == 0 { (a, b) } else { (b, a) };
            let mut hit = false;
            me.shots.retain_mut(|(s, z)| {
                *z += me.aim * SHOT_CM_PER_S * dt;
                if enemy.shield <= 0.0
                    && g.ds(*s, enemy.s).abs() < HIT_ACROSS_CM
                    && (*z - enemy.z).abs() < HIT_ALONG_CM
                {
                    hit = true;
                    return false;
                }
                (0.0..g.height_cm).contains(z)
            });
            if hit {
                me.score += 1;
                enemy.shield = SHIELD_S;
                for _ in 0..24 {
                    let (a, v) = (rng.range(0.0, std::f64::consts::TAU), rng.range(30.0, 120.0));
                    sparks.push((enemy.s, enemy.z, a.cos() * v, a.sin() * v * 0.6, rng.range(0.4, 0.9), enemy.colour));
                }
                sounds.push(json!({"type": "tone", "wave": "noise", "freq": 200, "ms": 300, "volume": 0.4}));
                sounds.push(json!({"type": "tone", "wave": "saw", "freq": 500, "to_freq": 60, "ms": 400, "volume": 0.3}));
                if me.score >= WIN_SCORE {
                    celebrate = Some((i, CELEBRATE_S));
                    sounds.push(json!({"type": "tone", "wave": "square", "freq": 300, "to_freq": 1500, "ms": 900, "volume": 0.3}));
                }
            }
        }

        // Explosions
        sparks.retain_mut(|(s, z, vs, vz, life, c)| {
            *life -= dt;
            *s = move_across(g, *s, *vs * dt);
            *z += *vz * dt;
            *vz += 60.0 * dt; // they fall
            if *life > 0.0 && (0.0..g.height_cm).contains(z) {
                frame.splat(g, *s, *z, 2.5, c.map(|v| v * *life));
                true
            } else {
                false
            }
        });

        for p in ps.iter() {
            for &(s, z) in &p.shots {
                frame.splat(g, s, z, 3.0, p.colour.map(|v| v * 0.5 + 120.0));
            }
            // Blinks while shielded after a hit
            if p.shield <= 0.0 || (t * 12.0).sin() > 0.0 {
                frame.splat(g, p.s, p.z, SHIP_CM, p.colour);
                // A nose pointing at the enemy
                frame.splat(g, p.s, p.z + p.aim * SHIP_CM * 1.2, SHIP_CM * 0.5, p.colour.map(|v| v * 0.8 + 50.0));
            }
        }

        // Scores: a ring at each player's edge, a fifth per point
        for (p, rows) in [(&ps[0], g.rows(g.height_cm - 1.6, g.height_cm)), (&ps[1], g.rows(0.0, 1.6))] {
            let lit = g.width * p.score as usize / WIN_SCORE as usize;
            for x in 0..lit {
                for y in rows.clone() {
                    frame.set(x, y, p.colour.map(|v| v * 0.6));
                }
            }
        }
    });
}
