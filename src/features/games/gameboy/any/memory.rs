//! Finding where a game keeps things in its memory without being told, the way cheat
//! finders do: copy the memory, do something, copy it again, and keep the bytes that
//! changed the way the thing would have. Pure: it works on copies of memory.
//!
//! - **Where she stands.** Each step's scroll says how far she moved (`moves.rs`). A byte
//!   that holds her x goes up by one for each step right (or by 16, if the game counts
//!   pixels), down for each step left, and stays put when she goes up or down or bumps into
//!   something. Every byte starts as a guess, and each step drops the ones that didn't
//!   follow it. A byte is let off one miss, and one more every 8 steps it got right: walking
//!   into the next area makes the real one jump.
//! - **Which area she's in.** A byte that changes whenever she goes somewhere new (her
//!   position jumps), and never while she walks around inside an area. A byte that keeps
//!   the area she came from changes just as often, so going back to a place drops the
//!   bytes that don't hold what they held there, and a place that looks new drops the
//!   bytes that hold what they held in a place she's been.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use super::super::emulator::Emulator;

/// Work RAM, where games keep nearly everything, and high RAM: (first address, bytes).
const WORK: (u16, usize) = (0xc000, 0x2000);
const HIGH: (u16, usize) = (0xff80, 0x7f);

/// How much a byte may change per step: 1 if the game counts steps, up to 16 if it counts
/// pixels.
const PER_STEP: [u8; 5] = [1, 2, 4, 8, 16];

/// Steps along an axis a byte must follow before it counts as found.
const FOUND_AFTER: u32 = 4;

/// Area changes a byte must follow before it counts as the area.
const AREA_AFTER: u32 = 3;

/// A copy of the memory a game keeps its own things in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory(Vec<u8>);

impl Memory {
    pub const LEN: usize = WORK.1 + HIGH.1;

    pub fn read(emu: &mut Emulator) -> Memory {
        Memory((0..Self::LEN).map(|i| emu.read(address(i))).collect())
    }

    /// Memory from bytes, in the order `address` gives (for the tests).
    #[cfg(test)]
    pub fn from_bytes(bytes: Vec<u8>) -> Memory {
        assert_eq!(bytes.len(), Self::LEN);
        Memory(bytes)
    }

    pub fn get(&self, address: u16) -> u8 {
        index(address).map_or(0, |i| self.0[i])
    }

    #[cfg(test)]
    pub fn set(&mut self, address: u16, value: u8) {
        let i = index(address).expect("an address in work or high RAM");
        self.0[i] = value;
    }
}

/// The address of the `i`th byte of a copy.
pub fn address(i: usize) -> u16 {
    if i < WORK.1 {
        WORK.0 + i as u16
    } else {
        HIGH.0 + (i - WORK.1) as u16
    }
}

/// Where an address is in a copy, if it's copied.
fn index(address: u16) -> Option<usize> {
    let a = usize::from(address);
    let (work, high) = (usize::from(WORK.0), usize::from(HIGH.0));
    if (work..work + WORK.1).contains(&a) {
        Some(a - work)
    } else if (high..high + HIGH.1).contains(&a) {
        Some(WORK.1 + a - high)
    } else {
        None
    }
}

/// A byte that holds her x or y: where it is, and how much it changes per step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Byte {
    pub address: u16,
    pub per_step: u8,
}

impl Byte {
    /// Her x or y in steps, by this byte.
    pub fn steps(self, memory: &Memory) -> i32 {
        i32::from(memory.get(self.address) / self.per_step)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tally {
    /// Steps along the axis it followed.
    moved: u32,
    /// Steps it rightly stayed put for.
    still: u32,
    /// Steps it got wrong.
    missed: u32,
}

impl Tally {
    /// One miss is let off, and one more for every 8 steps it followed.
    fn in_the_running(self) -> bool {
        self.missed <= 1 + self.moved / 8
    }
}

/// The guesses for one axis.
#[derive(Debug, Clone)]
struct Axis {
    guesses: Vec<(Byte, Tally)>,
    /// The byte picked, kept while it stays in the running, so the numbers don't change
    /// under her when another byte catches up.
    chosen: Option<Byte>,
}

impl Axis {
    fn new() -> Axis {
        let guesses = (0..Memory::LEN)
            .flat_map(|i| {
                PER_STEP.map(|per_step| {
                    let byte = Byte {
                        address: address(i),
                        per_step,
                    };
                    (byte, Tally::default())
                })
            })
            .collect();
        Axis {
            guesses,
            chosen: None,
        }
    }

    /// Only this byte, already picked: found before, and still checked.
    fn known(byte: Byte) -> Axis {
        let tally = Tally {
            moved: FOUND_AFTER,
            ..Tally::default()
        };
        Axis {
            guesses: vec![(byte, tally)],
            chosen: Some(byte),
        }
    }

    /// A step of `steps` along this axis (0 when she went the other way or didn't move).
    fn step(&mut self, before: &Memory, after: &Memory, steps: i32) {
        for (byte, tally) in &mut self.guesses {
            let change = after
                .get(byte.address)
                .wrapping_sub(before.get(byte.address));
            // Wraps like the byte does: a step left counting pixels is 256 - 16.
            let expected = (steps * i32::from(byte.per_step)) as u8;
            match (change == expected, steps != 0) {
                (true, true) => tally.moved += 1,
                (true, false) => tally.still += 1,
                (false, _) => tally.missed += 1,
            }
        }
        self.guesses.retain(|(_, tally)| tally.in_the_running());
        if self.guesses.is_empty() {
            // Nothing followed: start again with every byte.
            *self = Axis::new();
        }
        if self
            .chosen
            .is_some_and(|c| !self.guesses.iter().any(|(b, _)| *b == c))
        {
            self.chosen = None;
        }
        if self.chosen.is_none() {
            self.chosen = self.best();
        }
    }

    /// The byte that followed the most steps, once one has followed enough. Fewer misses,
    /// then counting steps rather than pixels, then the lower address break ties.
    fn best(&self) -> Option<Byte> {
        self.guesses
            .iter()
            .filter(|(_, t)| t.moved >= FOUND_AFTER)
            .min_by_key(|(b, t)| (u32::MAX - t.moved, t.missed, b.per_step, b.address))
            .map(|(b, _)| *b)
    }

    #[cfg(test)]
    fn holds(&self, address: u16) -> bool {
        self.guesses.iter().any(|(b, _)| b.address == address)
    }
}

/// Finding her x and y.
#[derive(Debug, Clone)]
pub struct Position {
    x: Axis,
    y: Axis,
    /// Steps it has been told about.
    pub steps: u32,
}

impl Default for Position {
    fn default() -> Position {
        Position {
            x: Axis::new(),
            y: Axis::new(),
            steps: 0,
        }
    }
}

impl Position {
    /// Only these bytes, found before (in an earlier session): still checked every step.
    pub fn known(x: Byte, y: Byte) -> Position {
        Position {
            x: Axis::known(x),
            y: Axis::known(y),
            steps: 0,
        }
    }

    /// One step, from memory before it and after: `moved` is how far the screen says she
    /// went, in steps, (0, 0) for a bump or a turn.
    pub fn step(&mut self, before: &Memory, after: &Memory, moved: (i32, i32)) {
        self.x.step(before, after, moved.0);
        self.y.step(before, after, moved.1);
        self.steps += 1;
    }

    /// The bytes for x and y, once both are found.
    pub fn found(&self) -> Option<(Byte, Byte)> {
        Some((self.x.chosen?, self.y.chosen?))
    }

    /// Where she stands, by memory.
    pub fn at(&self, memory: &Memory) -> Option<(i32, i32)> {
        let (x, y) = self.found()?;
        Some((x.steps(memory), y.steps(memory)))
    }

    #[cfg(test)]
    /// Whether a byte is still a guess for x or y (for the measurements).
    pub fn holds(&self, address: u16) -> (bool, bool) {
        (self.x.holds(address), self.y.holds(address))
    }

    #[cfg(test)]
    /// How many guesses are left for x and y.
    pub fn left(&self) -> (usize, usize) {
        (self.x.guesses.len(), self.y.guesses.len())
    }
}

/// A byte that holds her sprite's x or y on the screen, in pixels, give or take a
/// constant: some games keep where she is on the screen, others where she is on a map that
/// scrolls under her (her pixel plus the scroll).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pixel {
    pub address: u16,
    /// The byte counts the scroll too.
    pub scrolls: bool,
    /// What the byte holds more than her pixel (and the scroll).
    pub offset: u8,
}

impl Pixel {
    /// Her sprite's pixel on the screen, by this byte, with the screen scrolled `scroll`.
    /// A little past the screen's top or left edge is below 0.
    pub fn on_screen(self, memory: &Memory, scroll: u8) -> i32 {
        let mut pixel = memory.get(self.address).wrapping_sub(self.offset);
        if self.scrolls {
            pixel = pixel.wrapping_sub(scroll);
        }
        if pixel >= PAST_EDGE {
            i32::from(pixel) - 256
        } else {
            i32::from(pixel)
        }
    }

    /// What the byte holds less her pixel (and the scroll): the same every time, for the
    /// byte that's hers.
    fn offset_now(address: u16, scrolls: bool, memory: &Memory, pixel: i32, scroll: u8) -> u8 {
        let mut offset = memory.get(address).wrapping_sub(pixel as u8);
        if scrolls {
            offset = offset.wrapping_sub(scroll);
        }
        offset
    }
}

/// Pixels a sprite can be past the screen's top or left edge: a byte's pixel this far
/// round from 0 is below it.
const PAST_EDGE: u8 = 224;

/// Pixels the middle of her sprites may be off where a byte says: something walking beside
/// her, or a sword or shield she holds, counts as one of her sprites now and then (in
/// Link's Awakening's village, by 4).
const PIXEL_NEAR: u8 = 4;

/// Pixels the middle of her sprites must have moved since the last look counted for a
/// look to say anything: twice what it can be off by, so a byte that stays put never
/// agrees.
const MOVED_PIXELS: i32 = 2 * PIXEL_NEAR as i32;

/// Moves along an axis a byte must follow before it counts as her pixel.
const PIXEL_AFTER: u32 = 8;

/// One guess at her pixel along an axis: the offset most looks agreed on (a running
/// majority vote, so a look that took another sprite for her doesn't move it), and how
/// often looks agreed and didn't.
#[derive(Debug, Clone, Copy)]
struct PixelGuess {
    address: u16,
    scrolls: bool,
    offset: u8,
    lead: u32,
    agreed: u32,
    missed: u32,
    /// How often each offset near `offset` was seen (from `PIXEL_NEAR` below to as far
    /// above), so the byte puts her where her sprites' middle most often is.
    near: [u16; 2 * PIXEL_NEAR as usize + 1],
}

impl PixelGuess {
    fn new(address: u16, scrolls: bool) -> PixelGuess {
        PixelGuess {
            address,
            scrolls,
            offset: 0,
            lead: 0,
            agreed: 0,
            missed: 0,
            near: [0; 2 * PIXEL_NEAR as usize + 1],
        }
    }

    /// Starts again from this offset.
    fn from(&mut self, offset: u8) {
        self.offset = offset;
        self.lead = 1;
        self.near = [0; 2 * PIXEL_NEAR as usize + 1];
        self.near[usize::from(PIXEL_NEAR)] = 1;
    }

    fn pixel(self) -> Pixel {
        let (most, _) = self
            .near
            .iter()
            .enumerate()
            .rev()
            .max_by_key(|&(i, &n)| (n, usize::from(PIXEL_NEAR).abs_diff(i) == 0))
            .unwrap_or((usize::from(PIXEL_NEAR), &0));
        Pixel {
            address: self.address,
            scrolls: self.scrolls,
            offset: self
                .offset
                .wrapping_add(most as u8)
                .wrapping_sub(PIXEL_NEAR),
        }
    }

    /// One miss is let off, and one more for every 4 looks it agreed with.
    fn in_the_running(self) -> bool {
        self.missed <= 1 + self.agreed / 4
    }
}

/// The guesses at her pixel along one axis.
#[derive(Debug, Clone)]
struct PixelAxis {
    guesses: Vec<PixelGuess>,
    /// Her pixel (and the scroll) at the last look counted, for guesses that don't count
    /// the scroll and for ones that do: only a look where that moved further than her
    /// sprite is found off by says anything (a byte that stays put agrees with a sprite
    /// that only wobbled).
    last: [Option<i32>; 2],
    chosen: Option<Pixel>,
}

impl PixelAxis {
    fn new() -> PixelAxis {
        let guesses = (0..Memory::LEN)
            .flat_map(|i| [false, true].map(|scrolls| PixelGuess::new(address(i), scrolls)))
            .collect();
        PixelAxis {
            guesses,
            last: [None; 2],
            chosen: None,
        }
    }

    /// Only this byte, found before: still checked.
    fn known(pixel: Pixel) -> PixelAxis {
        let mut guess = PixelGuess::new(pixel.address, pixel.scrolls);
        guess.from(pixel.offset);
        guess.lead = PIXEL_AFTER;
        guess.agreed = PIXEL_AFTER;
        PixelAxis {
            guesses: vec![guess],
            last: [None; 2],
            chosen: Some(pixel),
        }
    }

    /// Her sprite was seen at `at` along this axis (the middle of her sprites), with the
    /// screen scrolled `scroll`.
    fn saw(&mut self, memory: &Memory, at: i32, scroll: u8) {
        let moved = [false, true].map(|scrolls| {
            let now = at + if scrolls { i32::from(scroll) } else { 0 };
            let last = &mut self.last[usize::from(scrolls)];
            let moved = last.is_some_and(|last| (last - now).abs() >= MOVED_PIXELS);
            if moved || last.is_none() {
                *last = Some(now);
            }
            moved
        });
        for g in &mut self.guesses {
            if !moved[usize::from(g.scrolls)] {
                continue;
            }
            let now = Pixel::offset_now(g.address, g.scrolls, memory, at, scroll);
            let off = now.wrapping_sub(g.offset).wrapping_add(PIXEL_NEAR);
            if g.lead == 0 && g.agreed == 0 {
                // The first look counted: the offset to check from.
                g.from(now);
            } else if off <= 2 * PIXEL_NEAR {
                g.agreed += 1;
                g.lead += 1;
                g.near[usize::from(off)] += 1;
            } else {
                g.missed += 1;
                if g.lead == 0 {
                    g.from(now);
                } else {
                    g.lead -= 1;
                }
            }
        }
        self.guesses.retain(|g| g.in_the_running());
        if self.guesses.is_empty() {
            *self = PixelAxis::new();
        }
        let chosen = self.chosen.and_then(|c| {
            self.guesses
                .iter()
                .find(|g| g.address == c.address && g.scrolls == c.scrolls)
        });
        self.chosen = match chosen {
            Some(g) => Some(g.pixel()),
            None => self.best(),
        };
    }

    /// Drops the guesses at these bytes, the chosen one too.
    fn leave_out(&mut self, bytes: &Range<u16>) {
        self.guesses.retain(|g| !bytes.contains(&g.address));
        if self.chosen.is_some_and(|c| bytes.contains(&c.address)) {
            self.chosen = self.best();
        }
        if self.guesses.is_empty() {
            *self = PixelAxis::new();
            self.guesses.retain(|g| !bytes.contains(&g.address));
        }
    }

    /// The byte that agreed the most, once one agreed enough. Fewer misses, then one that
    /// doesn't count the scroll, then the lower address break ties (the stack, at the top
    /// of high RAM, holds copies of her pixel only while the code using it runs).
    fn best(&self) -> Option<Pixel> {
        self.guesses
            .iter()
            .filter(|g| g.agreed >= PIXEL_AFTER)
            .min_by_key(|g| (u32::MAX - g.agreed, g.missed, g.scrolls, g.address))
            .map(|g| g.pixel())
    }
}

/// Bytes of the sprite table (4 to a sprite: y, x, picture, flags).
const SPRITE_TABLE: u16 = 160;

/// Where work RAM holds a copy of the sprite table, if it does: games build the table in
/// work RAM and have it copied in every frame, from the start of a page. A sprite's bytes
/// there follow her sprite exactly, but only while she keeps her place in the table, and
/// games hand the places out again as sprites come and go (in Link's Awakening, once
/// others came on screen, the copy's x and y were someone else's, and her map drifted
/// from the picture). So they say nothing a sprite doesn't. A page is the copy when most
/// of the table's bytes that aren't 0 are the same there (a frame's moves aside).
pub fn sprite_table_copy(memory: &Memory, table: &[u8]) -> Option<Range<u16>> {
    let table = table.get(..usize::from(SPRITE_TABLE))?;
    let used = table.iter().filter(|&&b| b != 0).count();
    if used < 8 {
        return None;
    }
    (WORK.0 >> 8..=(WORK.0 + WORK.1 as u16 - 1) >> 8)
        .map(|page| page << 8)
        .find(|&start| {
            let same = (0..SPRITE_TABLE)
                .filter(|&i| {
                    let b = table[usize::from(i)];
                    b != 0 && memory.get(start + i) == b
                })
                .count();
            same * 4 >= used * 3
        })
        .map(|start| start..start + SPRITE_TABLE)
}

/// Finding where her sprite is on the screen, in a game where she walks across it: bytes
/// that move with her sprite, pixel for pixel, while she walks. Once found, memory says
/// where she is on the screen exactly, whatever else moves with her (a companion, a crab
/// the step followed instead).
#[derive(Debug, Clone)]
pub struct Pixels {
    x: PixelAxis,
    y: PixelAxis,
}

impl Default for Pixels {
    fn default() -> Pixels {
        Pixels {
            x: PixelAxis::new(),
            y: PixelAxis::new(),
        }
    }
}

impl Pixels {
    /// Only these bytes, found before: still checked.
    pub fn known(x: Pixel, y: Pixel) -> Pixels {
        Pixels {
            x: PixelAxis::known(x),
            y: PixelAxis::known(y),
        }
    }

    /// Her sprite was seen at `at` on the screen (the middle of her sprites), scrolled
    /// `scroll`, while she could walk.
    pub fn saw(&mut self, memory: &Memory, at: (i32, i32), scroll: (u8, u8)) {
        self.x.saw(memory, at.0, scroll.0);
        self.y.saw(memory, at.1, scroll.1);
    }

    /// The bytes for her x and y, once both are found.
    pub fn found(&self) -> Option<(Pixel, Pixel)> {
        Some((self.x.chosen?, self.y.chosen?))
    }

    /// These bytes aren't hers whatever they did (a copy of the sprite table): dropped,
    /// and found again from the rest if they were the ones found.
    pub fn leave_out(&mut self, bytes: &Range<u16>) {
        self.x.leave_out(bytes);
        self.y.leave_out(bytes);
    }

    /// The bytes found weren't hers after all: found again from every byte.
    pub fn forget(&mut self) {
        *self = Pixels::default();
    }

    /// Where her sprite is on the screen, by memory.
    pub fn at(&self, memory: &Memory, scroll: (u8, u8)) -> Option<(i32, i32)> {
        let (x, y) = self.found()?;
        Some((x.on_screen(memory, scroll.0), y.on_screen(memory, scroll.1)))
    }

    #[cfg(test)]
    /// How many guesses are left for x and y.
    pub fn left(&self) -> (usize, usize) {
        (self.x.guesses.len(), self.y.guesses.len())
    }
}

/// A byte that says which screen she's on, where the game slides from one screen to the
/// next (Link's Awakening's rooms): a room number, which changes by the same amount every
/// time she slides on right (and back by as much sliding left), by another amount sliding
/// down, and never while she stays on one screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenByte {
    pub address: u16,
    /// What it changes by sliding on to the next screen right, and down.
    pub right: u8,
    pub down: u8,
}

impl ScreenByte {
    /// Where a screen is on the grid of screens the byte counts, across and down: with
    /// `right` 1 and `down` 16, screen 0x35 is 5 across and 3 down. `None` where the two
    /// don't make a grid (the larger isn't a whole number of the smaller).
    pub fn grid(self, value: u8) -> Option<(i32, i32)> {
        let (r, d, v) = (
            i32::from(self.right),
            i32::from(self.down),
            i32::from(value),
        );
        if r == 0 || d == 0 {
            None
        } else if d > r && d % r == 0 {
            Some(((v % d) / r, v / d))
        } else if r > d && r % d == 0 {
            Some((v / r, (v % r) / d))
        } else {
            None
        }
    }
}

/// Slides each way (across and up or down) a byte must follow before it counts as the
/// screen.
const SCREEN_AFTER: u32 = 3;

/// What a guess learned along one axis: what the byte changes by sliding on (right or
/// down), and how many slides it followed.
#[derive(Debug, Clone, Copy, Default)]
struct Along {
    by: Option<u8>,
    agreed: u32,
}

impl Along {
    /// Whether the byte changed as a slide `way` (1 right or down, -1 left or up) would.
    fn slid(&mut self, changed: u8, way: i32) -> bool {
        let by = if way > 0 {
            changed
        } else {
            changed.wrapping_neg()
        };
        match self.by {
            _ if changed == 0 => return false,
            None => self.by = Some(by),
            Some(b) if b != by => return false,
            Some(_) => {}
        }
        self.agreed += 1;
        true
    }
}

#[derive(Debug, Clone, Copy)]
struct ScreenGuess {
    address: u16,
    across: Along,
    up_down: Along,
    missed: u32,
}

impl ScreenGuess {
    /// One miss is let off, and one more for every 4 slides it followed.
    fn in_the_running(self) -> bool {
        self.missed <= 1 + (self.across.agreed + self.up_down.agreed) / 4
    }

    fn found(self) -> Option<ScreenByte> {
        (self.across.agreed >= SCREEN_AFTER && self.up_down.agreed >= SCREEN_AFTER).then_some(
            ScreenByte {
                address: self.address,
                right: self.across.by?,
                down: self.up_down.by?,
            },
        )
    }
}

/// Finding which screen she's on, in a game that slides from screen to screen: with it,
/// where she is is the screen and her step on it, read rather than counted. A copy of the
/// camera follows every slide one way too, but not the other, and comes round again
/// (Link's Awakening's is back where it was 8 screens on), so it's never the screen.
#[derive(Debug, Clone)]
pub struct Screens {
    guesses: Vec<ScreenGuess>,
}

impl Default for Screens {
    fn default() -> Screens {
        Screens {
            guesses: (0..Memory::LEN)
                .map(|i| ScreenGuess {
                    address: address(i),
                    across: Along::default(),
                    up_down: Along::default(),
                    missed: 0,
                })
                .collect(),
        }
    }
}

impl Screens {
    /// This byte, found before, and every other byte again.
    pub fn known(byte: ScreenByte) -> Screens {
        let mut screens = Screens::default();
        if let Some(g) = screens
            .guesses
            .iter_mut()
            .find(|g| g.address == byte.address)
        {
            g.across = Along {
                by: Some(byte.right),
                agreed: SCREEN_AFTER,
            };
            g.up_down = Along {
                by: Some(byte.down),
                agreed: SCREEN_AFTER,
            };
        }
        screens
    }

    /// Between two looks on still screens: `slid` says the way she slid along each axis
    /// (0 for none). With no slide at all, the screen byte didn't change.
    pub fn between(&mut self, before: &Memory, after: &Memory, slid: (i32, i32)) {
        for g in &mut self.guesses {
            let changed = after.get(g.address).wrapping_sub(before.get(g.address));
            let agreed = match slid {
                (0, 0) => changed == 0,
                // A slide both ways at once: the change is both together.
                (x, y) if x != 0 && y != 0 => {
                    let by = |a: Along, way: i32| {
                        a.by.map(|b| if way > 0 { b } else { b.wrapping_neg() })
                    };
                    by(g.across, x)
                        .zip(by(g.up_down, y))
                        .is_some_and(|(a, b)| a.wrapping_add(b) == changed)
                }
                (x, 0) => g.across.slid(changed, x),
                (_, y) => g.up_down.slid(changed, y),
            };
            if !agreed {
                g.missed += 1;
            }
        }
        self.guesses.retain(|g| g.in_the_running());
        if self.guesses.is_empty() {
            *self = Screens::default();
        }
    }

    /// The byte that followed the most slides, once one followed enough both ways. Then
    /// the fewest misses and the lower address.
    pub fn found(&self) -> Option<ScreenByte> {
        self.guesses
            .iter()
            .filter_map(|g| Some((g, g.found()?)))
            .min_by_key(|(g, b)| {
                (
                    u32::MAX - g.across.agreed - g.up_down.agreed,
                    g.missed,
                    b.address,
                )
            })
            .map(|(_, b)| b)
    }

    /// Which screen she's on, by the byte found.
    pub fn at(&self, memory: &Memory) -> Option<u8> {
        Some(memory.get(self.found()?.address))
    }

    #[cfg(test)]
    /// Whether a byte is still in the running.
    pub fn guessed(&self, address: u16) -> bool {
        self.guesses.iter().any(|g| g.address == address)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AreaGuess {
    address: u16,
    /// Area changes it changed for, and stayed put for.
    changed: u32,
    stayed: u32,
    /// The values it took at changes: a byte that names areas takes many.
    values: Vec<u8>,
}

impl AreaGuess {
    fn new(address: u16, changed: u32) -> AreaGuess {
        AreaGuess {
            address,
            changed,
            stayed: 0,
            values: Vec::new(),
        }
    }

    fn took(&mut self, value: u8) {
        if !self.values.contains(&value) {
            self.values.push(value);
        }
    }
}

/// Finding which byte says the area.
#[derive(Debug, Clone)]
pub struct Areas {
    /// How often each byte changed while she walked around inside an area.
    restless: Vec<u32>,
    /// Steps inside an area it has been told about.
    pub walked: u32,
    /// Area changes it has been told about.
    pub changes: u32,
    /// The bytes that still might be it. Every byte starts as one, and starts again if
    /// all are dropped.
    guesses: Vec<AreaGuess>,
    chosen: Option<u16>,
}

impl Default for Areas {
    fn default() -> Areas {
        Areas {
            restless: vec![0; Memory::LEN],
            walked: 0,
            changes: 0,
            guesses: every_byte(),
            chosen: None,
        }
    }
}

fn every_byte() -> Vec<AreaGuess> {
    (0..Memory::LEN)
        .map(|i| AreaGuess::new(address(i), 0))
        .collect()
}

impl Areas {
    /// Only this byte, found before: still checked.
    pub fn known(address: u16) -> Areas {
        Areas {
            guesses: vec![AreaGuess::new(address, AREA_AFTER)],
            chosen: Some(address),
            ..Areas::default()
        }
    }

    /// Changes while walking that a byte is let off: a few, in case code took the next
    /// area for the same one.
    fn calm(&self, i: usize) -> bool {
        self.restless[i] <= 1 + self.walked / 64
    }

    /// A step inside the area.
    pub fn walked(&mut self, before: &Memory, after: &Memory) {
        self.walked += 1;
        // The area's byte never changes while she walks around in one area.
        if let Some(chosen) = self.chosen
            && before.get(chosen) != after.get(chosen)
        {
            self.chosen = None;
        }
        for i in 0..Memory::LEN {
            if before.0[i] != after.0[i] {
                self.restless[i] += 1;
            }
        }
        self.drop_restless();
        self.choose();
    }

    fn drop_restless(&mut self) {
        let calm: Vec<bool> = (0..Memory::LEN).map(|i| self.calm(i)).collect();
        self.guesses
            .retain(|g| index(g.address).is_some_and(|i| calm[i]));
    }

    /// She went somewhere new: through a door, or into the next area.
    pub fn changed(&mut self, before: &Memory, after: &Memory) {
        self.changes += 1;
        for g in &mut self.guesses {
            let (then, now) = (before.get(g.address), after.get(g.address));
            if then != now {
                g.changed += 1;
            } else {
                g.stayed += 1;
            }
            g.took(then);
            g.took(now);
        }
        // A byte that stays put for more than a quarter of the changes says something
        // else (the tileset, the music); one is let off, in case code took a step for a
        // change.
        self.guesses.retain(|g| g.stayed <= 1 + g.changed / 4);
        self.choose();
    }

    /// Better first: more changes followed and fewer missed, then fewer changes while she
    /// walked, then more values (a byte that flips between two says something else),
    /// then the lower address.
    fn rank(&self, g: &AreaGuess) -> (u32, u32, u32, usize, u16) {
        (
            u32::MAX - g.changed,
            g.stayed,
            self.restless(g.address),
            usize::MAX - g.values.len(),
            g.address,
        )
    }

    /// Picks the best guess. The one picked is kept while none followed more changes, so
    /// names don't change under her when another byte catches up; one that stayed put
    /// as she went somewhere new gave two places one name, so it's dropped.
    fn choose(&mut self) {
        if self.guesses.is_empty() {
            self.guesses = every_byte();
            self.drop_restless();
        }
        let best = self
            .guesses
            .iter()
            .filter(|g| g.changed >= AREA_AFTER)
            .min_by_key(|g| self.rank(g));
        let kept = self
            .chosen
            .and_then(|c| self.guesses.iter().find(|g| g.address == c))
            .is_some_and(|g| best.is_some_and(|b| g.stayed == b.stayed));
        if !kept {
            self.chosen = best.map(|g| g.address);
        }
    }

    /// She's back in a place she was in before (through the same door), so the area's
    /// byte holds what it held then: bytes that don't are something else.
    pub fn same_place(&mut self, then: &Memory, now: &Memory) {
        self.guesses
            .retain(|g| then.get(g.address) == now.get(g.address));
        self.choose();
    }

    /// She's somewhere that isn't a place she was in before (it looks different), so the
    /// area's byte holds something else than it did there: bytes that hold the same are
    /// something else.
    pub fn different_place(&mut self, then: &Memory, now: &Memory) {
        self.guesses
            .retain(|g| then.get(g.address) != now.get(g.address));
        self.choose();
    }

    /// The byte that says the area, once found.
    pub fn found(&self) -> Option<u16> {
        self.chosen
    }

    #[cfg(test)]
    /// Whether a byte is still a guess (for the measurements).
    pub fn holds(&self, address: u16) -> bool {
        self.guesses.iter().any(|g| g.address == address)
    }

    #[cfg(test)]
    /// How many guesses are left.
    pub fn left(&self) -> usize {
        self.guesses.len()
    }

    #[cfg(test)]
    /// The best guesses, as lines (for the measurements).
    pub fn best(&self, n: usize) -> Vec<String> {
        let mut guesses = self.guesses.clone();
        guesses.sort_by_key(|g| self.rank(g));
        guesses.truncate(n);
        guesses
            .iter()
            .take(n)
            .map(|g| {
                format!(
                    "{:04X}: changed {}, stayed {}, {} values, changed while walking {}",
                    g.address,
                    g.changed,
                    g.stayed,
                    g.values.len(),
                    index(g.address).map_or(0, |i| self.restless[i])
                )
            })
            .collect()
    }

    /// How often a byte changed while she walked (for the measurements).
    pub fn restless(&self, address: u16) -> u32 {
        index(address).map_or(0, |i| self.restless[i])
    }
}

/// Finding her position, then the area, from the steps she takes.
#[derive(Debug, Clone, Default)]
pub struct Finder {
    pub position: Position,
    pub pixels: Pixels,
    pub screens: Screens,
    pub areas: Areas,
}

impl Finder {
    /// One step, from memory before it and after: which way she pressed, how far the
    /// screen scrolled (in steps), and whether the screen went dark. Says, once her
    /// position is found, whether memory put her somewhere other than the step took her:
    /// through a door, or into the next area with no fade (towns and routes in Pokémon).
    pub fn stepped(
        &mut self,
        before: &Memory,
        after: &Memory,
        pressed: (i32, i32),
        scrolled: (i32, i32),
        dark: bool,
    ) -> Option<bool> {
        if !dark {
            self.position.step(before, after, scrolled);
        }
        // With the bytes picked now, which may have just been picked.
        let (x, y) = self.position.found()?;
        let was = (x.steps(before), y.steps(before));
        let now = (x.steps(after), y.steps(after));
        let moved = if dark { pressed } else { scrolled };
        let landed = (was.0 + moved.0, was.1 + moved.1);
        Some(now != landed && now != was)
    }

    /// What a step said about the area, once code has decided whether she went somewhere
    /// new. Only counted once her position is found, since until then a walk into the
    /// next area with no fade looks like any other step.
    pub fn place(&mut self, before: &Memory, after: &Memory, new_place: bool) {
        if self.position.found().is_none() {
            return;
        }
        if new_place {
            self.areas.changed(before, after);
        } else {
            self.areas.walked(before, after);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const X: u16 = 0xd362;
    const Y: u16 = 0xd361;
    /// A pixel x, as some games keep it.
    const PIXEL_X: u16 = 0xc104;
    const AREA: u16 = 0xd35e;
    const LAST_AREA: u16 = 0xd365;
    const TIMER: u16 = 0xff9a;

    /// A small pretend game: x and y in steps, x in pixels too, the area and the last
    /// one, a timer that ticks, and noise that changes now and then.
    struct Pretend {
        memory: Memory,
        tick: u8,
    }

    impl Pretend {
        fn new() -> Pretend {
            let mut bytes = vec![0; Memory::LEN];
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = (i * 37 % 251) as u8;
            }
            let mut p = Pretend {
                memory: Memory::from_bytes(bytes),
                tick: 0,
            };
            p.place(1, 5, 5);
            p
        }

        fn place(&mut self, area: u8, x: u8, y: u8) {
            let last = self.memory.get(AREA);
            if last != area {
                self.memory.set(LAST_AREA, last);
            }
            self.memory.set(AREA, area);
            self.memory.set(X, x);
            self.memory.set(Y, y);
            self.memory.set(PIXEL_X, x.wrapping_mul(16));
        }

        fn walk(&mut self, dx: i32, dy: i32) -> (Memory, Memory) {
            let before = self.memory.clone();
            let x = (i32::from(self.memory.get(X)) + dx) as u8;
            let y = (i32::from(self.memory.get(Y)) + dy) as u8;
            self.place(self.memory.get(AREA), x, y);
            self.tick = self.tick.wrapping_add(7);
            self.memory.set(TIMER, self.tick);
            // Something unrelated that changes every third step.
            if self.tick.is_multiple_of(3) {
                self.memory.set(0xc500, self.tick);
            }
            (before, self.memory.clone())
        }
    }

    #[test]
    fn addresses_cover_work_and_high_ram() {
        assert_eq!(address(0), 0xc000);
        assert_eq!(address(0x1fff), 0xdfff);
        assert_eq!(address(0x2000), 0xff80);
        assert_eq!(index(0xd362), Some(0x1362));
        assert_eq!(index(0xff80), Some(0x2000));
        assert_eq!(index(0xe000), None);
        assert_eq!(index(0xffff), None);
        for i in [0, 5, 0x1fff, 0x2000, Memory::LEN - 1] {
            assert_eq!(index(address(i)), Some(i));
        }
    }

    #[test]
    fn steps_find_x_and_y() {
        let mut game = Pretend::new();
        let mut position = Position::default();
        let walks = [
            (1, 0),
            (1, 0),
            (0, 1),
            (0, 0),
            (-1, 0),
            (0, -1),
            (0, -1),
            (1, 0),
            (0, 1),
            (0, 1),
            (1, 0),
        ];
        for (i, &(dx, dy)) in walks.iter().enumerate() {
            let (before, after) = game.walk(dx, dy);
            position.step(&before, &after, (dx, dy));
            if i < 6 {
                assert_eq!(position.found(), None, "after {} steps", i + 1);
            }
        }
        let (x, y) = position.found().expect("found");
        assert_eq!(
            (x, y),
            (
                Byte {
                    address: X,
                    per_step: 1
                },
                Byte {
                    address: Y,
                    per_step: 1
                }
            )
        );
        assert_eq!(position.at(&game.memory), Some((8, 6)));
        // The pixel copy followed too; the timer and the noise didn't.
        assert_eq!(position.holds(PIXEL_X), (true, false));
        assert_eq!(position.holds(TIMER), (false, false));
        assert_eq!(position.holds(0xc500), (false, false));
    }

    #[test]
    fn walking_into_the_next_area_is_let_off() {
        let mut game = Pretend::new();
        let mut position = Position::default();
        for _ in 0..8 {
            for (dx, dy) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
                let (before, after) = game.walk(dx, dy);
                position.step(&before, &after, (dx, dy));
            }
        }
        let found = position.found();
        assert!(found.is_some());
        // Up into the next area: y jumps from the top to the bottom of the new one.
        let before = game.memory.clone();
        game.place(2, 5, 35);
        position.step(&before, &game.memory.clone(), (0, -1));
        assert_eq!(position.found(), found);
        assert_eq!(position.at(&game.memory), Some((5, 35)));
    }

    #[test]
    fn a_byte_that_misses_too_often_is_dropped_even_once_chosen() {
        let mut game = Pretend::new();
        let mut position = Position::default();
        for _ in 0..2 {
            for (dx, dy) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
                let (before, after) = game.walk(dx, dy);
                position.step(&before, &after, (dx, dy));
            }
        }
        assert_eq!(position.found().map(|f| f.0.address), Some(X));
        // From now on the game keeps x somewhere else, and the old byte stands still.
        let moved_x: u16 = 0xd400;
        for i in 0..6 {
            let before = game.memory.clone();
            game.memory.set(moved_x, 10 + i);
            game.memory.set(PIXEL_X, (10 + i).wrapping_mul(16));
            position.step(&before, &game.memory.clone(), (1, 0));
        }
        let (x, _) = position.found().expect("still found");
        assert_ne!(x.address, X);
        assert!(x.address == moved_x || x.address == PIXEL_X, "{x:?}");
    }

    #[test]
    fn area_changes_find_the_area_and_not_the_last_one() {
        let mut game = Pretend::new();
        let mut areas = Areas::default();
        let walk_about = |game: &mut Pretend, areas: &mut Areas| {
            for (dx, dy) in [(1, 0), (0, 1), (-1, 0), (0, -1), (0, 0)] {
                let (before, after) = game.walk(dx, dy);
                areas.walked(&before, &after);
            }
        };
        let mut arrived = std::collections::HashMap::new();
        let mut go = |game: &mut Pretend, areas: &mut Areas, area: u8| {
            let before = game.memory.clone();
            game.place(area, 3, 3);
            // Loading an area changes plenty of other bytes too.
            game.memory.set(0xd370, area.wrapping_mul(3));
            areas.changed(&before, &game.memory.clone());
            // Back in a place she's been: the last area byte holds something else now.
            if let Some(then) = arrived.get(&area) {
                areas.same_place(then, &game.memory);
            }
            arrived.entry(area).or_insert_with(|| game.memory.clone());
        };
        walk_about(&mut game, &mut areas);
        go(&mut game, &mut areas, 2);
        assert_eq!(areas.found(), None, "one change isn't enough");
        walk_about(&mut game, &mut areas);
        go(&mut game, &mut areas, 3);
        walk_about(&mut game, &mut areas);
        go(&mut game, &mut areas, 4);
        walk_about(&mut game, &mut areas);
        go(&mut game, &mut areas, 3);
        assert_eq!(areas.found(), Some(AREA));
        assert!(!areas.holds(LAST_AREA), "it keeps the last area");
        assert!(!areas.holds(TIMER) && !areas.holds(X));
    }

    #[test]
    fn going_back_through_the_same_door_drops_bytes_that_differ() {
        let mut game = Pretend::new();
        let mut areas = Areas::default();
        // A byte that counts area loads changes every time, like the area does.
        let loads: u16 = 0xd380;
        let mut arrived = std::collections::HashMap::new();
        for (n, area) in [2, 3, 2, 3].into_iter().enumerate() {
            let before = game.memory.clone();
            game.place(area, 3, 3);
            game.memory.set(loads, n as u8);
            areas.changed(&before, &game.memory.clone());
            if let Some(then) = arrived.get(&area) {
                areas.same_place(then, &game.memory);
            }
            arrived.entry(area).or_insert_with(|| game.memory.clone());
        }
        assert!(!areas.holds(loads));
        assert_eq!(areas.found(), Some(AREA));
    }

    #[test]
    fn a_byte_that_stays_put_for_area_changes_is_dropped() {
        let mut game = Pretend::new();
        let mut areas = Areas::default();
        // The tileset: the same for areas 2 and 3.
        let tileset: u16 = 0xd367;
        for (area, set) in [(2, 1), (3, 1), (4, 2), (5, 2)] {
            let before = game.memory.clone();
            game.place(area, 3, 3);
            game.memory.set(tileset, set);
            areas.changed(&before, &game.memory.clone());
        }
        assert!(!areas.holds(tileset));
        assert_eq!(areas.found(), Some(AREA));
    }

    /// Walks her round a screen that scrolls now and then: `screen_x` holds her pixel plus
    /// 8 (the screen sliding across says nothing about it), `map_y` her pixel plus the
    /// scroll. Now and then something beside her moves the middle of her sprites, and once
    /// the step took someone else for her.
    #[test]
    fn bytes_that_move_with_her_sprite_are_found() {
        let screen_x: u16 = 0xff98;
        let map_y: u16 = 0xd100;
        let mut memory = Pretend::new().memory;
        let mut pixels = Pixels::default();
        let walk = [
            (16, 0),
            (0, 16),
            (-16, 0),
            (0, -16),
            (16, 0),
            (0, 16),
            (16, 0),
            (0, 16),
        ];
        let (mut x, mut y, mut scroll, mut across) = (40i32, 60i32, 0u8, 0u8);
        for (i, (dx, dy)) in walk.iter().cycle().take(32).enumerate() {
            x += dx;
            if i % 4 == 3 {
                across = across.wrapping_add(16);
            }
            // Half the time the screen scrolls down instead of her going down.
            if i % 2 == 0 {
                y += dy;
            } else {
                scroll = (i32::from(scroll) + dy) as u8;
            }
            memory.set(screen_x, (x + 8) as u8);
            memory.set(map_y, (y + i32::from(scroll)) as u8);
            memory.set(0xc500, i as u8);
            let seen = match i {
                9 => (x + 30, y),
                _ if i % 5 == 0 => (x + 4, y - 4),
                _ => (x, y),
            };
            pixels.saw(&memory, seen, (across, scroll));
        }
        let (px, py) = pixels.found().expect("both found");
        assert_eq!((px.address, px.scrolls, px.offset), (screen_x, false, 8));
        assert_eq!((py.address, py.scrolls, py.offset), (map_y, true, 0));
        assert_eq!(pixels.at(&memory, (across, scroll)), Some((x, y)));
    }

    #[test]
    fn a_copy_of_the_sprite_table_is_never_her() {
        let mut memory = Pretend::new().memory;
        let mut table = vec![0u8; 160];
        for (i, b) in table.iter_mut().enumerate().take(40) {
            *b = (i * 7 + 3) as u8;
        }
        assert_eq!(sprite_table_copy(&memory, &table), None);
        for (i, &b) in table.iter().enumerate() {
            memory.set(0xc000 + i as u16, b);
        }
        // A frame's moves differ a little.
        memory.set(0xc001, 99);
        let copy = sprite_table_copy(&memory, &table).expect("found");
        assert_eq!(copy, 0xc000..0xc0a0);
        // An empty table matches any page of zeros.
        assert_eq!(sprite_table_copy(&memory, &[0; 160]), None);

        // Found from the copy before: left out, and found again from the rest.
        let x = Pixel {
            address: 0xc009,
            scrolls: false,
            offset: 0,
        };
        let mut pixels = Pixels::known(
            x,
            Pixel {
                address: 0xc008,
                ..x
            },
        );
        assert!(pixels.found().is_some());
        pixels.leave_out(&copy);
        assert_eq!(pixels.found(), None);
        assert!(pixels.left().0 > 1000);
    }

    #[test]
    fn the_screen_is_the_byte_that_counts_slides() {
        // A world of screens 16 across: the room byte counts 1 a screen right and 16 one
        // down; copies of the camera count 160 pixels a screen across and 128 down.
        const ROOM: u16 = 0xfff6;
        const CAMERA_X: u16 = 0xc120;
        const CAMERA_Y: u16 = 0xc121;
        let mut p = Pretend::new();
        let mut screens = Screens::default();
        let (mut col, mut row) = (2u8, 3u8);
        let set = |p: &mut Pretend, col: u8, row: u8| {
            p.memory.set(ROOM, row * 16 + col);
            p.memory.set(CAMERA_X, col.wrapping_mul(160));
            p.memory.set(CAMERA_Y, row.wrapping_mul(128));
        };
        set(&mut p, col, row);
        // Around the world: right, right, down, down, left, left, up, up, with walks on
        // each screen.
        for way in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
            for _ in 0..2 {
                for _ in 0..3 {
                    let (before, after) = p.walk(1, 0);
                    screens.between(&before, &after, (0, 0));
                }
                // Not before it followed slides both ways.
                if way.1 == 0 && way.0 > 0 {
                    assert_eq!(screens.found(), None);
                }
                let before = p.memory.clone();
                col = (i32::from(col) + way.0) as u8;
                row = (i32::from(row) + way.1) as u8;
                set(&mut p, col, row);
                screens.between(&before, &p.memory, way);
            }
        }
        // The room byte changed once while she stayed, and is still the screen.
        let before = p.memory.clone();
        p.memory.set(ROOM, 0);
        screens.between(&before, &p.memory, (0, 0));
        set(&mut p, col, row);
        let room = ScreenByte {
            address: ROOM,
            right: 1,
            down: 16,
        };
        assert_eq!(screens.found(), Some(room));
        assert_eq!(screens.at(&p.memory), Some(2 + 3 * 16));
        // The camera's copies each missed every slide the other way.
        assert!(!screens.guessed(CAMERA_X));
        assert!(!screens.guessed(CAMERA_Y));
        // Known again from a save, and still the room after a slide.
        let mut again = Screens::known(room);
        let before = p.memory.clone();
        set(&mut p, col + 1, row);
        again.between(&before, &p.memory, (1, 0));
        assert_eq!(again.found(), Some(room));
    }

    #[test]
    fn a_pixel_past_the_top_of_the_screen_is_below_0() {
        let mut memory = Pretend::new().memory;
        memory.set(0xff98, 4);
        let pixel = Pixel {
            address: 0xff98,
            scrolls: false,
            offset: 8,
        };
        assert_eq!(pixel.on_screen(&memory, 0), -4);
    }
}
