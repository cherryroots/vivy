//! Messing around (stage 6): code plays the game by itself, fast, from where it is now,
//! pressing buttons at random, and keeps the moments where something new showed up: a
//! new room, a text box, a menu. No model; it runs on the Game Boy's thread at many times
//! real speed. The moments go to a model once (`insights.rs`), which says what each one
//! shows about how the game works.
//!
//! It's a small Go-Explore: each screen is boiled down to a coarse picture (a cell), and
//! every cell first seen keeps a save state. Each go loads one of them, the ones tried
//! least and the newest first, and presses at random from there. That reaches far more
//! than pressing at random from the start, which mostly walks back and forth.
//!
//! The game is put back as it was at the end.

use std::collections::HashMap;

use super::console::Machine;
use super::emulator::{Button, HEIGHT, WIDTH};

/// Pixels on each side of a cell's block: 10 by 9 blocks on the screen.
const BLOCK: usize = 16;
/// Shades a block's average is cut to.
const SHADES: u32 = 4;
/// Presses in one go from a cell.
const PRESSES: u32 = 30;
/// Cells kept at most (each holds a save state of about 50 KB).
pub const CELLS_MOST: usize = 600;
/// Presses before a new cell kept with its moment, to say what led there.
const LED_THERE: usize = 6;
/// Frames a screen must stay the same to count: a fade or a scroll isn't a screen.
const STEADY: u32 = 8;

/// A screen boiled down: each block's average shade.
type Blocks = Vec<u8>;

/// How a new screen came: what makes it worth a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A button that isn't a direction brought it: a text box, a menu, something used.
    Button,
    /// Walking went through a dark (or blank) screen to it: a door, a new area.
    Through,
    /// Walking scrolled to it: mostly more of the same place.
    Walk,
}

/// A moment where something new showed up.
#[derive(Debug, Clone)]
pub struct Moment {
    /// The screen before the presses, and after (160×144, RGB).
    pub before: Vec<u8>,
    pub after: Vec<u8>,
    /// The last presses before it, like `up:12`, oldest first.
    pub presses: Vec<String>,
    /// How much of the screen changed, 0 to 1.
    pub changed: f32,
    pub kind: Kind,
    /// The coarse picture of the screen after, and which of its blocks are new, to tell
    /// moments apart.
    blocks: Blocks,
    new: Vec<bool>,
    /// The coarse picture of the screen before.
    was: Blocks,
}

impl Moment {
    /// A moment as a fake game finds it, for tests elsewhere.
    #[cfg(test)]
    pub fn for_test(presses: &[&str]) -> Moment {
        let screen = vec![0u8; WIDTH * HEIGHT * 3];
        Moment {
            before: screen.clone(),
            after: screen,
            presses: presses.iter().map(|p| p.to_string()).collect(),
            changed: 1.0,
            kind: Kind::Button,
            blocks: vec![0; 90],
            new: vec![true; 90],
            was: vec![0; 90],
        }
    }
}

/// What messing around found.
#[derive(Debug, Default)]
pub struct Found {
    /// Different screens seen.
    pub cells: usize,
    pub moments: Vec<Moment>,
    /// Frames played.
    pub frames: u64,
}

/// One screen kept, with a save state to come back to it.
struct Cell {
    state: Vec<u8>,
    screen: Vec<u8>,
    /// Goes started from it.
    chosen: u32,
    /// When it was found, counting cells.
    found: usize,
}

/// A screen's blocks: each 16×16 square's average shade, cut to a few.
fn blocks_of(rgb: &[u8]) -> Blocks {
    let (cols, rows) = (WIDTH / BLOCK, HEIGHT / BLOCK);
    let mut out = Vec::with_capacity(cols * rows);
    for by in 0..rows {
        for bx in 0..cols {
            let mut sum = 0u32;
            for y in by * BLOCK..(by + 1) * BLOCK {
                for x in bx * BLOCK..(bx + 1) * BLOCK {
                    let i = (y * WIDTH + x) * 3;
                    sum += u32::from(rgb[i]) + u32::from(rgb[i + 1]) + u32::from(rgb[i + 2]);
                }
            }
            let average = sum / (BLOCK * BLOCK * 3) as u32;
            out.push((average * SHADES / 256) as u8);
        }
    }
    out
}

/// The share of blocks that differ between two screens.
fn changed(a: &Blocks, b: &Blocks) -> f32 {
    let differ = a.iter().zip(b).filter(|(x, y)| x != y).count();
    differ as f32 / a.len().max(1) as f32
}

/// Farthest a screen is looked for scrolled, in pixels each way.
const SCROLL_MOST: i32 = 48;

/// The share of sampled pixels that differ with `after` lined up `shift` from `before`,
/// sampling every `every` pixels; `None` if under half the screen overlaps.
fn mismatch(before: &[u8], after: &[u8], shift: (i32, i32), every: usize) -> Option<f32> {
    let (dx, dy) = shift;
    let (mut seen, mut differ) = (0u32, 0u32);
    for y in (0..HEIGHT).step_by(every) {
        let oy = y as i32 + dy;
        if oy < 0 || oy >= HEIGHT as i32 {
            continue;
        }
        for x in (0..WIDTH).step_by(every) {
            let ox = x as i32 + dx;
            if ox < 0 || ox >= WIDTH as i32 {
                continue;
            }
            seen += 1;
            if after[(y * WIDTH + x) * 3] != before[(oy as usize * WIDTH + ox as usize) * 3] {
                differ += 1;
            }
        }
    }
    let samples = (HEIGHT.div_ceil(every) * WIDTH.div_ceil(every)) as u32;
    (seen * 2 >= samples).then(|| differ as f32 / seen as f32)
}

/// How far the screen scrolled from `before` to `after`: the shift that lines them up
/// best, found coarsely, then to the pixel. A game's scroll registers can't say it, since
/// some change them partway down the screen.
fn scrolled(before: &[u8], after: &[u8]) -> (i32, i32) {
    let best = |shifts: Vec<(i32, i32)>, every: usize| {
        shifts
            .into_iter()
            .filter_map(|s| mismatch(before, after, s, every).map(|m| (m, s)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or((0, 0), |(_, s)| s)
    };
    let around = |(x, y): (i32, i32), far: i32, step: usize| {
        let mut out = vec![(x, y)];
        for dy in (-far..=far).step_by(step) {
            for dx in (-far..=far).step_by(step) {
                out.push((x + dx, y + dy));
            }
        }
        out
    };
    // Every third pixel: an even step would only ever sample the same spot of each 8-pixel
    // tile, and every shift inside a tile would look the same.
    let coarse = best(around((0, 0), SCROLL_MOST, 4), 3);
    best(around(coarse, 4, 1), 1)
}

/// The blocks that really changed from `before` to `after` (RGB screens), with the
/// background lined up by how far it scrolled: walking that only scrolls a little changes
/// almost nothing. A block counts when an eighth of its pixels differ, or when none of it
/// was on the screen before.
fn moved(before: &[u8], after: &[u8], (dx, dy): (i32, i32)) -> Vec<bool> {
    let (cols, rows) = (WIDTH / BLOCK, HEIGHT / BLOCK);
    let mut out = Vec::with_capacity(cols * rows);
    for by in 0..rows {
        for bx in 0..cols {
            let (mut seen, mut differ) = (0, 0);
            for y in by * BLOCK..(by + 1) * BLOCK {
                for x in bx * BLOCK..(bx + 1) * BLOCK {
                    let (ox, oy) = (x as i32 + dx, y as i32 + dy);
                    if ox < 0 || oy < 0 || ox >= WIDTH as i32 || oy >= HEIGHT as i32 {
                        continue;
                    }
                    let a = (y * WIDTH + x) * 3;
                    let b = (oy as usize * WIDTH + ox as usize) * 3;
                    seen += 1;
                    if after[a..a + 3] != before[b..b + 3] {
                        differ += 1;
                    }
                }
            }
            out.push(seen == 0 || differ * 8 > seen);
        }
    }
    out
}

/// The share of blocks set.
fn share(mask: &[bool]) -> f32 {
    mask.iter().filter(|&&m| m).count() as f32 / mask.len().max(1) as f32
}

/// Whether two moments show the same thing: the same screen, or the same part changing
/// (the same menu opened, or closed, over somewhere else).
fn alike(a: &Moment, b: &Moment) -> bool {
    if changed(&a.blocks, &b.blocks) < 0.1 {
        return true;
    }
    let same_part = |a_blocks: &Blocks, b_blocks: &Blocks| {
        let (mut same, mut either) = (0, 0);
        for i in 0..a.new.len() {
            if a.new[i] || b.new[i] {
                either += 1;
                if a.new[i] && b.new[i] && a_blocks[i] == b_blocks[i] {
                    same += 1;
                }
            }
        }
        either > 0 && same * 10 >= either * 6
    };
    same_part(&a.blocks, &b.blocks) || same_part(&a.was, &b.was)
}

/// Whether a screen is all one shade (dark, or switched off): nothing to keep.
fn blank(blocks: &Blocks) -> bool {
    blocks.iter().all(|&b| b == blocks[0])
}

/// A random press: directions most, A often, the rest now and then. Held long enough to
/// walk a step or two.
fn press(rng: &mut fastrand::Rng) -> (Button, u32) {
    let button = match rng.u32(0..100) {
        0..50 => [Button::Up, Button::Down, Button::Left, Button::Right][rng.usize(0..4)],
        50..75 => Button::A,
        75..90 => Button::B,
        90..95 => Button::Start,
        _ => Button::Select,
    };
    let hold = if button.is_direction() {
        rng.u32(8..=40)
    } else {
        rng.u32(4..=12)
    };
    (button, hold)
}

/// Which cell the next go starts from: the ones tried least, and the newest, most often.
fn choose(cells: &[Cell], rng: &mut fastrand::Rng) -> usize {
    let newest = cells.len().saturating_sub(cells.len() / 10 + 1);
    let weight = |c: &Cell| {
        let fresh = if c.found >= newest { 1.0 } else { 0.0 };
        1.0 / f64::from(c.chosen + 1).sqrt() + fresh
    };
    let total: f64 = cells.iter().map(weight).sum();
    let mut pick = rng.f64() * total;
    for (i, cell) in cells.iter().enumerate() {
        pick -= weight(cell);
        if pick <= 0.0 {
            return i;
        }
    }
    cells.len() - 1
}

/// Plays at random for `frames` frames from where the game is now, then puts it back.
pub fn mess_around(machine: &mut Machine, frames: u64, seed: u64) -> anyhow::Result<Found> {
    let start = machine.emulator.save_state()?;
    let found = explore(machine, &start, frames, seed);
    machine.load_state(&start)?;
    found
}

/// Messes around for `frames` frames and gives the screens seen and the `most` moments most
/// worth a look ([`pick`]), then puts the game back.
pub fn moments(
    machine: &mut Machine,
    frames: u64,
    seed: u64,
    most: usize,
) -> anyhow::Result<(usize, Vec<Moment>)> {
    let found = mess_around(machine, frames, seed)?;
    let picked = pick(&found.moments, most).into_iter().cloned().collect();
    Ok((found.cells, picked))
}

fn explore(machine: &mut Machine, start: &[u8], budget: u64, seed: u64) -> anyhow::Result<Found> {
    let emulator = &mut machine.emulator;
    let mut rng = fastrand::Rng::with_seed(seed);
    let mut found = Found::default();
    let first = emulator.screen();
    let mut cells = vec![Cell {
        state: start.to_vec(),
        screen: first.clone(),
        chosen: 0,
        found: 0,
    }];
    let mut seen: HashMap<Blocks, usize> = HashMap::from([(blocks_of(&first), 0)]);
    let begun = emulator.frames();
    let spent = |e: &super::emulator::Emulator| e.frames() - begun;
    while spent(emulator) < budget {
        let from = choose(&cells, &mut rng);
        cells[from].chosen += 1;
        emulator.load_state(&cells[from].state)?;
        let mut before = cells[from].screen.clone();
        // Since `before`: whether she walked, and whether the screen went blank on the way
        // (a door or a new area).
        let (mut walked, mut went_blank) = (false, false);
        let mut pressed: Vec<String> = Vec::new();
        for _ in 0..PRESSES {
            let (button, hold) = press(&mut rng);
            walked |= button.is_direction();
            let mut look = |emulator: &mut super::emulator::Emulator, frame: u32| {
                emulator.frame();
                if frame.is_multiple_of(2) && !went_blank {
                    went_blank = blank(&blocks_of(&emulator.screen()));
                }
            };
            emulator.hold(&[button]);
            for frame in 0..hold {
                look(emulator, frame);
            }
            emulator.release();
            for frame in 0..rng.u32(6..=20) {
                look(emulator, frame);
            }
            pressed.push(format!("{}:{hold}", button.name()));
            // Only a screen that holds still counts: not a fade or a scroll on its way.
            let screen = emulator.screen();
            let blocks = blocks_of(&screen);
            for _ in 0..STEADY {
                emulator.frame();
            }
            if blocks_of(&emulator.screen()) != blocks || blank(&blocks) {
                continue;
            }
            if !seen.contains_key(&blocks) && cells.len() < CELLS_MOST {
                seen.insert(blocks.clone(), cells.len());
                let new = moved(&before, &screen, scrolled(&before, &screen));
                found.moments.push(Moment {
                    changed: share(&new),
                    new,
                    was: blocks_of(&before),
                    kind: if !walked {
                        Kind::Button
                    } else if went_blank {
                        Kind::Through
                    } else {
                        Kind::Walk
                    },
                    before: before.clone(),
                    after: screen.clone(),
                    presses: pressed[pressed.len().saturating_sub(LED_THERE)..].to_vec(),
                    blocks: blocks.clone(),
                });
                cells.push(Cell {
                    state: emulator.save_state()?,
                    screen: screen.clone(),
                    chosen: 0,
                    found: cells.len(),
                });
                pressed.clear();
            }
            before = screen;
            (walked, went_blank) = (false, false);
        }
    }
    found.cells = cells.len();
    found.frames = spent(emulator);
    Ok(found)
}

/// The `most` moments most worth a look, each unlike the ones picked before it: first
/// what buttons did and where doors led, the biggest changes first; walking that only
/// scrolled fills at most a quarter.
pub fn pick(moments: &[Moment], most: usize) -> Vec<&Moment> {
    let mut order: Vec<&Moment> = moments.iter().collect();
    order.sort_by(|a, b| b.changed.total_cmp(&a.changed));
    let (walks, others): (Vec<&Moment>, Vec<&Moment>) =
        order.into_iter().partition(|m| m.kind == Kind::Walk);
    let mut picked: Vec<&Moment> = Vec::new();
    let walks_most = most / 4;
    for (moment, walk) in others
        .into_iter()
        .map(|m| (m, false))
        .chain(walks.into_iter().map(|m| (m, true)))
    {
        if picked.len() == most {
            break;
        }
        if walk && picked.iter().filter(|p| p.kind == Kind::Walk).count() >= walks_most {
            break;
        }
        if !picked.iter().any(|p| alike(p, moment)) {
            picked.push(moment);
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::test_rom;

    fn moment(changed: f32, shade: u8, first: u8) -> Moment {
        walked(changed, shade, first, Kind::Button)
    }

    fn walked(changed: f32, shade: u8, first: u8, kind: Kind) -> Moment {
        let mut blocks = vec![shade; 90];
        blocks[0] = first;
        Moment {
            before: Vec::new(),
            after: Vec::new(),
            presses: Vec::new(),
            changed,
            kind,
            new: vec![true; blocks.len()],
            was: vec![shade + 10; blocks.len()],
            blocks,
        }
    }

    #[test]
    fn blocks_cut_the_screen_into_shades() {
        let mut rgb = vec![255u8; WIDTH * HEIGHT * 3];
        // The top left block black.
        for y in 0..BLOCK {
            for x in 0..BLOCK {
                let i = (y * WIDTH + x) * 3;
                rgb[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        let blocks = blocks_of(&rgb);
        assert_eq!(blocks.len(), 90);
        assert_eq!(blocks[0], 0);
        assert_eq!(blocks[1], 3);
        assert!(!blank(&blocks));
        assert!(blank(&blocks_of(&vec![0u8; WIDTH * HEIGHT * 3])));
        assert!(
            (changed(&blocks, &blocks_of(&vec![255u8; WIDTH * HEIGHT * 3])) - 1.0 / 90.0).abs()
                < 1e-6
        );
    }

    #[test]
    fn a_screen_that_only_scrolled_barely_changed() {
        // Tiles of random shades, then the same moved 3 pixels left as a scroll does.
        let mut rng = fastrand::Rng::with_seed(3);
        let tiles: Vec<u8> = (0..32 * 32).map(|_| rng.u8(0..4) * 85).collect();
        let shade = |x: usize, y: usize| tiles[(y / 8) * 32 + x / 8];
        let screen = |shift: usize| {
            let mut rgb = vec![0u8; WIDTH * HEIGHT * 3];
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let i = (y * WIDTH + x) * 3;
                    rgb[i..i + 3].fill(shade(x + shift, y));
                }
            }
            rgb
        };
        let (before, after) = (screen(0), screen(3));
        assert_eq!(scrolled(&before, &after), (3, 0));
        assert_eq!(scrolled(&before, &before), (0, 0));
        assert!(share(&moved(&before, &after, (3, 0))) < 0.01);
        assert!(share(&moved(&before, &after, (0, 0))) > 0.9);
        assert_eq!(share(&moved(&before, &before, (0, 0))), 0.0);
    }

    #[test]
    fn the_same_menu_over_somewhere_else_is_alike() {
        let mut menu = walked(0.3, 1, 1, Kind::Button);
        let mut other = walked(0.3, 2, 2, Kind::Button);
        // The bottom row is the same menu on both; everything above differs.
        for m in [&mut menu, &mut other] {
            m.new = (0..90).map(|i| i >= 80).collect();
            m.blocks[80..].fill(3);
        }
        assert!(alike(&menu, &other));
        other.blocks[80..85].fill(0);
        assert!(!alike(&menu, &other));
        // The same menu closing: alike by the screens before.
        menu.was[80..].fill(2);
        other.was[80..].fill(2);
        assert!(alike(&menu, &other));
    }

    #[test]
    fn the_biggest_changes_are_picked_once_each() {
        let moments = [
            moment(0.1, 1, 1),
            moment(0.9, 2, 2),
            // Almost the same screen as the one above: left out.
            moment(0.8, 2, 3),
            moment(0.5, 3, 3),
        ];
        let picked: Vec<f32> = pick(&moments, 5).iter().map(|m| m.changed).collect();
        assert_eq!(picked, vec![0.9, 0.5, 0.1]);
        assert_eq!(pick(&moments, 1).len(), 1);
    }

    #[test]
    fn walks_that_only_scroll_fill_a_quarter_at_most() {
        let moments = [
            walked(0.9, 1, 1, Kind::Walk),
            walked(0.8, 2, 2, Kind::Walk),
            walked(0.3, 3, 3, Kind::Through),
            walked(0.2, 0, 0, Kind::Button),
        ];
        let picked: Vec<f32> = pick(&moments, 4).iter().map(|m| m.changed).collect();
        assert_eq!(picked, vec![0.3, 0.2, 0.9]);
    }

    #[test]
    fn messing_around_finds_screens_and_puts_the_game_back() {
        let mut m = Machine::new("test.gb", &test_rom::build()).unwrap();
        m.run(60);
        let before = m.emulator.save_state().unwrap();
        let found = mess_around(&mut m, 3_000, 7).unwrap();
        assert!(found.frames >= 3_000);
        assert_eq!(found.cells, found.moments.len() + 1);
        // Put back as `Machine::load_state` does: the save, then one frame.
        let mut back = Machine::new("test.gb", &test_rom::build()).unwrap();
        back.load_state(&before).unwrap();
        m.run(30);
        back.run(30);
        assert!(m.emulator.screen() == back.emulator.screen());
    }
}
