//! Styx: a self-playing version of the 1983 PC game (a Qix clone). The player
//! draws lines out from claimed ground; closing a line claims the area the
//! Styx is not in. The Styx, a swirling tangle of lines, bounces around the
//! free area, and if it touches a line being drawn the player loses a life.
//! Claiming 75 % clears the level and the next Styx is faster.
//!
//! On a cylinder the field wraps around and only the top and bottom start
//! claimed. A single line from top to bottom does not cut a cylinder in two,
//! so the player claims rectangles drawn out from a border.

use std::collections::VecDeque;

use display_demos::{hue, Game, Rng};
use serde_json::json;

const ROWS_PER_CELL: usize = 4;
const STEP_SECONDS: f64 = 0.04;
const GOAL: f64 = 0.75;
const LIVES: u32 = 3;
const STYX_TRAIL: usize = 8; // earlier positions of the Styx drawn behind it
const STYX_MAX_CM: f64 = 70.0; // longest the Styx stretches

#[derive(Clone, Copy, PartialEq)]
enum Cell {
    Free,
    Claimed,
    Trail,
}

const DIRS: [(i64, i64); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

struct Board {
    w: usize,
    h: usize,
    wraps: bool,
    cells: Vec<Cell>,
    start_claimed: usize,
}

impl Board {
    fn new(w: usize, h: usize, wraps: bool) -> Self {
        let mut cells = vec![Cell::Free; w * h];
        for y in 0..h {
            for x in 0..w {
                // Top and bottom are claimed; a flat display also has sides
                if y == 0 || y == h - 1 || (!wraps && (x == 0 || x == w - 1)) {
                    cells[y * w + x] = Cell::Claimed;
                }
            }
        }
        let start_claimed = cells.iter().filter(|&&c| c == Cell::Claimed).count();
        Self { w, h, wraps, cells, start_claimed }
    }

    /// Cell index, wrapping around on a cylinder; None off the field
    fn index(&self, x: i64, y: i64) -> Option<usize> {
        let x = if self.wraps { x.rem_euclid(self.w as i64) } else { x };
        ((0..self.w as i64).contains(&x) && (0..self.h as i64).contains(&y)).then(|| y as usize * self.w + x as usize)
    }

    /// x brought back onto the field on a cylinder
    fn wrap_x(&self, x: i64) -> i64 {
        if self.wraps { x.rem_euclid(self.w as i64) } else { x }
    }

    fn get(&self, x: i64, y: i64) -> Option<Cell> {
        self.index(x, y).map(|i| self.cells[i])
    }

    fn set(&mut self, x: i64, y: i64, cell: Cell) {
        if let Some(i) = self.index(x, y) {
            self.cells[i] = cell;
        }
    }

    /// Claimed ground next to free space: where the player can start a line
    fn is_edge(&self, x: i64, y: i64) -> bool {
        self.get(x, y) == Some(Cell::Claimed) && DIRS.iter().any(|(dx, dy)| self.get(x + dx, y + dy) == Some(Cell::Free))
    }

    fn claimed_fraction(&self) -> f64 {
        let claimed = self.cells.iter().filter(|&&c| c == Cell::Claimed).count();
        (claimed - self.start_claimed) as f64 / (self.cells.len() - self.start_claimed) as f64
    }

    /// Closes the trail: free space the Styx cannot reach becomes claimed,
    /// and so does the trail. Returns how many cells were claimed.
    fn close(&mut self, styx: &[(i64, i64)]) -> usize {
        let mut reached = vec![false; self.cells.len()];
        let mut queue: VecDeque<(i64, i64)> = VecDeque::new();
        for &(x, y) in styx {
            if let Some(i) = self.index(x, y) {
                if self.cells[i] == Cell::Free && !reached[i] {
                    reached[i] = true;
                    queue.push_back((x, y));
                }
            }
        }
        while let Some((x, y)) = queue.pop_front() {
            for (dx, dy) in DIRS {
                if let Some(i) = self.index(x + dx, y + dy) {
                    if self.cells[i] == Cell::Free && !reached[i] {
                        reached[i] = true;
                        queue.push_back((x + dx, y + dy));
                    }
                }
            }
        }
        let mut claimed = 0;
        for (i, cell) in self.cells.iter_mut().enumerate() {
            if *cell == Cell::Trail || (*cell == Cell::Free && !reached[i]) {
                *cell = Cell::Claimed;
                claimed += 1;
            }
        }
        claimed
    }

    /// Shortest walk over claimed ground to a random edge cell, if any
    fn walk_to_edge(&self, from: (i64, i64), rng: &mut Rng, avoid: (f64, f64)) -> Option<VecDeque<(i64, i64)>> {
        let mut prev = vec![usize::MAX; self.cells.len()];
        let start = self.index(from.0, from.1)?;
        prev[start] = start;
        let mut queue = VecDeque::from([from]);
        let mut edges = Vec::new();
        while let Some((x, y)) = queue.pop_front() {
            if self.is_edge(x, y) && (x, y) != from {
                edges.push((x, y));
            }
            for (dx, dy) in DIRS {
                if let Some(i) = self.index(x + dx, y + dy) {
                    if self.cells[i] == Cell::Claimed && prev[i] == usize::MAX {
                        prev[i] = self.index(x, y).unwrap();
                        queue.push_back((x + dx, y + dy));
                    }
                }
            }
        }
        // A few random candidates; keep the one furthest from the Styx
        let w = self.w as f64;
        let far = |&(x, y): &(i64, i64)| {
            let dx = (x as f64 - avoid.0).abs();
            let dx = if self.wraps { dx.min(w - dx) } else { dx };
            dx * 4.0 + (y as f64 - avoid.1).abs()
        };
        let target = (0..4)
            .filter_map(|_| edges.get(rng.range(0.0, edges.len() as f64) as usize).copied())
            .max_by(|a, b| far(a).total_cmp(&far(b)))?;
        let mut path = VecDeque::new();
        let mut i = self.index(target.0, target.1)?;
        while i != start {
            path.push_front(((i % self.w) as i64, (i / self.w) as i64));
            i = prev[i];
        }
        Some(path)
    }
}

/// The Styx: a line whose two ends bounce around the free area
struct Styx {
    ends: [(f64, f64); 2],
    speeds: [(f64, f64); 2],
    /// Fastest the ends may move, cells per second across and down
    max_speed: (f64, f64),
    trail: VecDeque<[(f64, f64); 2]>,
}

impl Styx {
    fn new(board: &Board, rng: &mut Rng, speed: f64) -> Self {
        let centre = (rng.range(0.0, board.w as f64), board.h as f64 / 2.0);
        let mut v = || (rng.range(-2.5, 2.5) * speed, rng.range(-9.0, 9.0) * speed);
        let speeds = [v(), v()];
        Self { ends: [centre, (centre.0 + 1.0, centre.1 + 2.0)], speeds, max_speed: (3.0 * speed, 11.0 * speed), trail: VecDeque::new() }
    }

    /// Cells along the line, the short way around
    fn cells(&self, board: &Board) -> Vec<(i64, i64)> {
        let [(ax, ay), (bx, by)] = self.ends;
        let w = board.w as f64;
        let dx = if board.wraps { (bx - ax) - w * ((bx - ax) / w).round() } else { bx - ax };
        (0..=24)
            .map(|k| {
                let f = k as f64 / 24.0;
                ((ax + dx * f).floor() as i64, (ay + (by - ay) * f).floor() as i64)
            })
            .collect()
    }

    fn step(&mut self, board: &Board, dt: f64, rng: &mut Rng, cm_per_cell: (f64, f64)) {
        self.trail.push_front(self.ends);
        self.trail.truncate(STYX_TRAIL);
        let open = |x: f64, y: f64| matches!(board.get(x.floor() as i64, y.floor() as i64), Some(Cell::Free | Cell::Trail));
        for e in 0..2 {
            let (x, y) = self.ends[e];
            let (mut vx, mut vy) = self.speeds[e];
            // A little randomness keeps the swirl unpredictable
            vx = (vx + rng.range(-1.0, 1.0)).clamp(-self.max_speed.0, self.max_speed.0);
            vy = (vy + rng.range(-4.0, 4.0)).clamp(-self.max_speed.1, self.max_speed.1);
            if !open(x + vx * dt, y) {
                vx = -vx;
            }
            if !open(x, y + vy * dt) {
                vy = -vy;
            }
            if open(x + vx * dt, y + vy * dt) {
                let nx = x + vx * dt;
                self.ends[e] = (if board.wraps { nx.rem_euclid(board.w as f64) } else { nx }, y + vy * dt);
            }
            self.speeds[e] = (vx, vy);
        }
        // Keep the line from stretching too far: the second end is pulled toward the first
        let [(ax, ay), (bx, by)] = self.ends;
        let w = board.w as f64;
        let dx = if board.wraps { (bx - ax) - w * ((bx - ax) / w).round() } else { bx - ax };
        let length = ((dx * cm_per_cell.0).powi(2) + ((by - ay) * cm_per_cell.1).powi(2)).sqrt();
        if length > STYX_MAX_CM {
            self.speeds[1].0 -= dx.signum() * 2.0;
            self.speeds[1].1 -= (by - ay).signum() * 8.0;
        }
        // An end shut out of the free area (after a capture) rejoins the other
        if !open(bx, by) {
            self.ends[1] = self.ends[0];
        }
    }
}

struct Player {
    x: i64,
    y: i64,
    /// Legs of the line being drawn: (direction, cells); the last runs until claimed ground
    plan: VecDeque<((i64, i64), usize)>,
    walk: VecDeque<(i64, i64)>,
    drawing: bool,
    start: (i64, i64),
    /// Direction the line set out in, to turn back along
    inward: (i64, i64),
    /// x without wrapping, and the box the line covers so far in those terms
    ux: i64,
    area: Area,
}

impl Player {
    fn new(h: usize) -> Self {
        Self {
            x: 0,
            y: h as i64 - 1,
            plan: VecDeque::new(),
            walk: VecDeque::new(),
            drawing: false,
            start: (0, 0),
            inward: (0, 0),
            ux: 0,
            area: Area { x0: 0, x1: 0, y0: 0, y1: 0 },
        }
    }
}

/// A box of cells; x is not wrapped, so it can run past the end of the field
#[derive(Clone, Copy)]
struct Area {
    x0: i64,
    x1: i64,
    y0: i64,
    y1: i64,
}

impl Area {
    fn around(points: &[(i64, i64)]) -> Self {
        let xs = points.iter().map(|p| p.0);
        let ys = points.iter().map(|p| p.1);
        Self { x0: xs.clone().min().unwrap(), x1: xs.max().unwrap(), y0: ys.clone().min().unwrap(), y1: ys.max().unwrap() }
    }

    /// Is the Styx within `margin` cells of this box?
    fn threatened(&self, styx: &Styx, margin: (f64, f64), w: usize, wraps: bool) -> bool {
        let [a, b] = styx.ends;
        let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        [a, b, mid].iter().any(|&(px, py)| {
            let cx = (self.x0 + self.x1) as f64 / 2.0 + 0.5;
            let cy = (self.y0 + self.y1) as f64 / 2.0 + 0.5;
            let mut dx = px - cx;
            if wraps {
                dx -= w as f64 * (dx / w as f64).round();
            }
            dx.abs() <= (self.x1 - self.x0) as f64 / 2.0 + 0.5 + margin.0
                && (py - cy).abs() <= (self.y1 - self.y0) as f64 / 2.0 + 0.5 + margin.1
        })
    }
}

fn main() {
    let game = Game::connect("Styx");
    let g = &game.geometry;
    let (w, h) = (g.width, g.height / ROWS_PER_CELL);
    let cm_per_cell = (g.col_cm, g.row_cm * ROWS_PER_CELL as f64);
    let mut rng = Rng::new(0x5717);
    let mut level = 1u32;
    let mut lives = LIVES;
    let mut board = Board::new(w, h, g.wraps);
    let mut styx = Styx::new(&board, &mut rng, 1.0);
    let mut player = Player::new(h);
    let mut step_timer = 0.0;
    let mut t = 0.0;
    // Flashes: (seconds left, kind) for a lost life or a cleared level
    let (mut death_flash, mut level_flash) = (0.0f64, 0.0f64);

    game.run(|g, dt, frame, sounds| {
        t += dt;
        if level_flash > 0.0 {
            level_flash -= dt;
            if level_flash <= 0.0 {
                level += 1;
                board = Board::new(w, h, g.wraps);
                styx = Styx::new(&board, &mut rng, 1.0 + 0.25 * (level - 1) as f64);
                player = Player::new(h);
            }
        } else {
            styx.step(&board, dt, &mut rng, cm_per_cell);
            step_timer += dt;
        }
        death_flash = (death_flash - dt).max(0.0);

        while step_timer >= STEP_SECONDS && level_flash <= 0.0 {
            step_timer -= STEP_SECONDS;
            let p = &mut player;
            if p.drawing {
                let last_leg = p.plan.len() == 1;
                let (dir, left) = p.plan.front_mut().map(|(d, n)| (*d, n)).unwrap();
                let (nx, ny) = (board.wrap_x(p.x + dir.0), p.y + dir.1);
                if !last_leg {
                    *left -= 1;
                    if *left == 0 {
                        p.plan.pop_front();
                    }
                }
                match board.get(nx, ny) {
                    Some(Cell::Free) => {
                        board.set(nx, ny, Cell::Trail);
                        (p.x, p.y) = (nx, ny);
                        p.ux += dir.0;
                        p.area = Area::around(&[(p.area.x0, p.area.y0), (p.area.x1, p.area.y1), (p.ux, ny)]);
                        sounds.push(json!({"type": "tone", "wave": "square", "freq": 200 + (t * 40.0) as i64 % 300,
                                           "ms": 25, "volume": 0.06}));
                    }
                    Some(Cell::Claimed) => {
                        (p.x, p.y) = (nx, ny);
                        p.drawing = false;
                        p.plan.clear();
                        let claimed = board.close(&styx.cells(&board));
                        sounds.push(json!({"type": "tone", "wave": "sine", "freq": 300,
                                           "to_freq": (300 + claimed * 6).min(1600), "ms": 300, "volume": 0.3}));
                        if board.claimed_fraction() >= GOAL {
                            println!("level {level} cleared");
                            level_flash = 2.0;
                            for (k, f) in [523, 659, 784, 1047].iter().enumerate() {
                                sounds.push(json!({"type": "tone", "wave": "triangle", "freq": f, "ms": 150 + 150 * k,
                                                   "volume": 0.3}));
                            }
                        }
                    }
                    // Off the field or across its own line: give up the line
                    _ => p.plan.clear(),
                }
                if p.drawing && p.plan.is_empty() {
                    p.drawing = false;
                }
            } else if let Some((x, y)) = p.walk.pop_front() {
                (p.x, p.y) = (x, y);
            } else if board.is_edge(p.x, p.y) {
                // Plan a rectangle out into free space and back
                let inward: Vec<_> = DIRS.iter().filter(|(dx, dy)| board.get(p.x + dx, p.y + dy) == Some(Cell::Free)).collect();
                let &&(ix, iy) = &inward[rng.range(0.0, inward.len() as f64) as usize];
                let side = if ix == 0 {
                    (if rng.range(0.0, 1.0) < 0.5 { 1 } else { -1 }, 0)
                } else {
                    (0, if rng.range(0.0, 1.0) < 0.5 { 1 } else { -1 })
                };
                // Cells are much taller than wide in cm, so vertical legs take more cells
                let depth = if ix == 0 { rng.range(6.0, 18.0) } else { rng.range(3.0, 9.0) } as usize;
                let width = if side.0 == 0 { rng.range(6.0, 18.0) } else { rng.range(3.0, 9.0) } as usize;
                // Only go if the Styx is well clear of where the rectangle would be
                let (d, wd) = (depth as i64, width as i64);
                let far = (p.x + ix * d, p.y + iy * d);
                let planned = Area::around(&[(p.x, p.y), far, (far.0 + side.0 * wd, far.1 + side.1 * wd),
                                             (p.x + side.0 * wd, p.y + side.1 * wd)]);
                if !planned.threatened(&styx, (3.0, 8.0), w, board.wraps) {
                    p.plan = VecDeque::from([((ix, iy), depth), (side, width), ((-ix, -iy), usize::MAX)]);
                    p.drawing = true;
                    p.start = (p.x, p.y);
                    p.inward = (ix, iy);
                    p.ux = p.x;
                    p.area = Area::around(&[(p.x, p.y)]);
                } else if let Some(path) = board.walk_to_edge((p.x, p.y), &mut rng, styx.ends[0]) {
                    p.walk = path;
                }
            } else if let Some(path) = board.walk_to_edge((p.x, p.y), &mut rng, styx.ends[0]) {
                p.walk = path;
            } else if let Some(i) = (0..board.cells.len()).find(|&i| board.is_edge((i % w) as i64, (i / w) as i64)) {
                // Cut off from every edge: hop to one
                (p.x, p.y) = ((i % w) as i64, (i / w) as i64);
            }
        }

        // The Styx coming close: turn straight back to the border
        if player.drawing && player.plan.len() > 1 && player.area.threatened(&styx, (2.0, 10.0), w, board.wraps) {
            player.plan = VecDeque::from([((-player.inward.0, -player.inward.1), usize::MAX)]);
        }

        // The Styx touching the line (or the player while drawing) costs a life
        if player.drawing {
            let hit = styx.cells(&board).iter().any(|&(x, y)| {
                board.get(x, y) == Some(Cell::Trail) || (board.index(x, y) == board.index(player.x, player.y))
            });
            if hit {
                for c in board.cells.iter_mut() {
                    if *c == Cell::Trail {
                        *c = Cell::Free;
                    }
                }
                (player.x, player.y) = player.start;
                player.drawing = false;
                player.plan.clear();
                // Get away from the Styx before trying again
                player.walk = board.walk_to_edge(player.start, &mut rng, styx.ends[0]).unwrap_or_default();
                death_flash = 0.6;
                sounds.push(json!({"type": "tone", "wave": "saw", "freq": 400, "to_freq": 60, "ms": 700, "volume": 0.35}));
                lives -= 1;
                println!("life lost on level {level}, {lives} left, {:.0} % claimed", board.claimed_fraction() * 100.0);
                if lives == 0 {
                    // Game over: start again from level 1
                    lives = LIVES;
                    level = 0;
                    level_flash = 1.5;
                }
            }
        }

        // Draw: claimed ground, its edge, the line, the player, the Styx
        frame.clear();
        let ground = hue(0.6 + 0.17 * level as f64, 1.0);
        for cy in 0..h {
            for cx in 0..w {
                let (x, y) = (cx as i64, cy as i64);
                let rgb = match board.get(x, y).unwrap() {
                    Cell::Free => continue,
                    Cell::Trail => [255.0, 210.0, 0.0],
                    Cell::Claimed if board.is_edge(x, y) => [90.0, 140.0, 255.0],
                    Cell::Claimed => ground.map(|c| c * 110.0),
                };
                for row in cy * ROWS_PER_CELL..(cy + 1) * ROWS_PER_CELL {
                    frame.add(cx, row, rgb);
                }
            }
        }
        let blink = if (t * 8.0) as i64 % 2 == 0 { 255.0 } else { 160.0 };
        for row in player.y as usize * ROWS_PER_CELL..(player.y as usize + 1) * ROWS_PER_CELL {
            frame.set(player.x as usize, row, [blink; 3]);
        }
        for (k, ends) in styx.trail.iter().enumerate() {
            let fade = 1.0 - k as f64 / STYX_TRAIL as f64;
            let colour = hue(t * 0.4 + k as f64 * 0.06, 255.0 * fade);
            let [(ax, ay), (bx, by)] = *ends;
            let dx = if g.wraps { (bx - ax) - w as f64 * ((bx - ax) / w as f64).round() } else { bx - ax };
            for s in 0..=30 {
                let f = s as f64 / 30.0;
                let (x, y) = (ax + dx * f, ay + (by - ay) * f);
                frame.splat(g, g.wrap(x * g.col_cm), y * cm_per_cell.1, 2.5, colour);
            }
        }
        // Flashes over everything
        if death_flash > 0.0 {
            let red = [255.0 * death_flash, 0.0, 0.0];
            for x in 0..g.width {
                for y in 0..g.height {
                    frame.add(x, y, red);
                }
            }
        }
        if level_flash > 0.0 {
            for x in 0..g.width {
                for y in 0..g.height {
                    frame.add(x, y, hue(x as f64 / g.width as f64 + t, 60.0));
                }
            }
        }
    });
}
