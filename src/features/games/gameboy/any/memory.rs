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
            if std::env::var("VIVY_GB_TRACE").is_ok() {
                println!("TRACE chosen {chosen:04X} changed while walking");
            }
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
            if std::env::var("VIVY_GB_TRACE").is_ok() {
                println!("TRACE every guess dropped: starting again");
            }
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
            if std::env::var("VIVY_GB_TRACE").is_ok() && self.chosen != best.map(|g| g.address) {
                println!(
                    "TRACE choose {:?} -> {:?} ({} guesses)",
                    self.chosen,
                    best.map(|g| g.address),
                    self.guesses.len()
                );
            }
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
}
