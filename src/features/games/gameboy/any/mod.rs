//! Looking at a game code has no memory map for. Nothing here knows a game: everything
//! comes from the screen, the game's memory, and what her own steps taught code.
//!
//! - **Where she is** comes from memory, once the cheat finder (`memory.rs`) has found
//!   the bytes for x and y, which takes a few steps each way. Until then code counts her
//!   steps from where she started, and moves what it drew to the game's own numbers once
//!   they're found.
//! - **Which area** comes from memory too, once a byte is found that changes whenever she
//!   goes somewhere new. Until then places are numbered as she finds them ("Place 2"), and
//!   one she comes back to is known again by its look.
//! - **What a step is** is learned by trying: a step looks like its four tiles (a
//!   fingerprint of their pictures), and a picture she walked onto is floor, one that
//!   blocked her is a wall, one that took her somewhere new is a door, wherever it is. A
//!   picture code hasn't learned yet is left unseen on the map, so exploring goes there.
//! - **Whether she can walk**: her sprite is in the middle of the screen, and the screen
//!   is as it was when she last walked, apart from scrolling. A text box, a menu or a
//!   battle changes many tiles without scrolling.
//!
//! Words on screen need the font's letters named (a later stage), so `text` stays empty;
//! the models see the screen itself.

pub mod memory;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::change::{Look as FrameLook, look as frame_look};
use super::console::Machine;
use super::map::{AreaMap, Dir, Kind, Pos, as_list};
use super::moves::{Outcome, offset};
use super::screen::{COLS, Layer, ROWS, Screen};
use super::walk::settle;
use super::world::{Grid, Stepped, View, World};
use memory::{Areas, Byte, Finder, Memory, Position};

/// Pixels in a step.
const STEP: i32 = 16;

/// The screen pixel her sprite's middle is nearest, in games that scroll under her.
use super::moves::{MIDDLE, middle_of, touching};

/// Pixels from her spot a sprite can be and still be hers.
const HER_NEAR: i32 = 12;

/// Tiles that may change with no scroll while she can still walk (a clock, a counter).
/// Tile numbers are compared, so tiles whose pictures move (water, flowers) don't count.
const CALM_CHANGES: usize = 8;

/// Steps seen to change picture at once, at most, for them to be pictures moving.
const MOVING_MOST: usize = 12;

/// How long a screen must hold still to count as calm when there's no calm screen to
/// compare it with.
const STILL_FRAMES: u32 = 20;

/// To know a place again by its look: steps of the screen that must have been seen there
/// before, and how many of them must look the same.
const KNOWN_AFTER: usize = 12;
const KNOWN_SHARE: f32 = 0.9;
/// A picture on a fifth of the steps or more is too common to tell places apart.
const COMMON_SHARE: usize = 5;
/// The fewest of the other pictures that must also look the same, when that many were seen.
const TELLING_AFTER: usize = 4;

/// A place is another one when, of at least this many different pictures on screen, less
/// than this share were ever seen there.
const OTHER_KINDS: usize = 6;
const OTHER_SHARE: f32 = 0.25;

/// Shifts tried when knowing a place again from steps counted from somewhere else.
const SHIFTS_TRIED: usize = 5;

/// Steps seen before in the same places that, three in four the same, say it's the same
/// place whatever else is on screen.
const VOUCHED_BY: usize = 4;

/// What code has learned about a game it has no memory map for. Saved with the maps.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sight {
    /// What each picture of a step turned out to be, by its fingerprint.
    pub steps: HashMap<u64, Learned>,
    /// The bytes found for her x and y and the area, so the next session starts with them.
    pub found: Found,
    /// What each step of each area looked like, to know a place again.
    pictures: HashMap<String, Pictures>,
    /// Places numbered so far, while no area byte is known.
    places: u32,
    /// Pictures seen to change on a step while she stood still (flowers, water): one
    /// picture moving, which says nothing about where she is.
    #[serde(default)]
    lively: HashSet<u64>,
    /// Where she was at the last look, so a session, a replay or a checkpoint carried on
    /// from a save state starts where she stood then, not at a new place's (0, 0).
    #[serde(default)]
    whereabouts: Option<Whereabouts>,
    /// Where she walks across the screen: her step just in from each door she went
    /// through (the place she left and its door step), so coming in by it again starts
    /// there when the place isn't known by its look.
    #[serde(default, with = "as_list")]
    came_in: HashMap<(String, Pos), Pos>,
    #[serde(skip)]
    live: Live,
}

/// Where she was at a look: the place, the step she stood on, where her sprite was on the
/// screen and its pictures.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Whereabouts {
    pub area: String,
    pub at: Pos,
    pub spot: Option<(i32, i32)>,
    her: Vec<u64>,
    /// Her sprite was known on a screen she walks across, even if it isn't now.
    #[serde(default)]
    walks_across: bool,
}

/// The bytes found in memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub x: Option<Byte>,
    pub y: Option<Byte>,
    pub area: Option<u16>,
}

/// What steps with one picture did when she tried them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Learned {
    pub walked: u32,
    pub blocked: u32,
    pub doors: u32,
    /// Hopped over this way, landing on the step beyond (a ledge).
    #[serde(default)]
    pub over: Option<Dir>,
    /// Stepped onto and carried on somewhere else (floor arrows, a spinner).
    #[serde(default)]
    pub carried: u32,
}

impl Learned {
    /// What a step with this picture is, once she has tried one. A picture walked onto
    /// more often than it went somewhere new is floor, whatever happened once.
    pub fn kind(self) -> Option<Kind> {
        if let Some(dir) = self.over {
            Some(Kind::OneWay(dir))
        } else if self.doors > 0 && self.doors >= self.walked {
            Some(Kind::Door)
        } else if self.carried > 0 && self.carried >= self.walked {
            Some(Kind::Mover)
        } else if self.walked > 0 {
            Some(Kind::Floor)
        } else if self.blocked > 0 {
            Some(Kind::Wall)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Pictures(#[serde(with = "as_list")] HashMap<Pos, u64>);

/// What only matters while playing.
#[derive(Debug, Clone, Default)]
struct Live {
    /// Nothing stands in the middle and she hasn't been seen to move yet, so code can't
    /// tell which sprite is her until she takes a step.
    finding: bool,
    finder: Option<Finder>,
    /// Memory at the last look, and the last look she could walk at.
    memory: Option<Memory>,
    free: Option<Memory>,
    /// The area she's in, by name, and where she stands.
    area: String,
    at: Pos,
    /// The screen the last time she could walk, to tell when something covers it.
    calm: Option<Calm>,
    /// Directions pressed by the last look (`Emulator::directions_pressed`).
    pressed: u64,
    /// The last look she could walk at: each step's picture and who stood there, by
    /// steps from her.
    around: HashMap<Pos, u64>,
    people: Vec<Pos>,
    /// She has left the last place (by this step, through a door if it was dark), and the
    /// new one is named once she can walk there.
    left: Option<(Pos, bool)>,
    /// Memory when she first came into each place: to name the places by the area byte
    /// once it's found, and to check the byte when she comes back through a door.
    arrived: HashMap<String, Memory>,
    /// Her sprite's pictures, from where she stands whenever a press of hers moved her:
    /// a battle's pictures near the middle aren't her.
    her: HashSet<u64>,
    /// Where her sprite is on the screen, in games where it moves across a still screen
    /// (the middle of the sprites a step moved her way). `None`: in the middle.
    spot: Option<(i32, i32)>,
    /// Her sprite was lost and her pictures weren't anywhere: found again by pressing.
    lost: bool,
    /// Pressing just found her (`found_her_at`), after any fade since the last look.
    just_found: bool,
    /// She went through a door by her own presses and can't walk yet where it led.
    door_pending: bool,
    /// She's somewhere else, but not through a door: another screen was put up since she
    /// last walked (a game over, a scene), so where she was is no door.
    relocated: bool,
    /// The screen went dark while she couldn't walk, since she last could.
    faded_away: bool,
    /// Her sprite has been known on a screen she walks across: steps are counted from it.
    walks_across: bool,
    /// Where a sprite that moved her way once is, until it does again (see `confirmed`).
    candidate: Option<(i32, i32)>,
    /// Where the sprites were at the last look, to see which moved when she pressed.
    sprites: Vec<(i32, i32)>,
    /// Whether she could walk at the last look, and the screens recorded by then
    /// (`Machine::recorded`), to see whether it went dark in between.
    was_free: bool,
    recorded: u64,
    /// The frame of the last look (`Emulator::frames`), to follow her sprite from there.
    frame: u64,
    /// The scroll at the last look.
    scroll: (u8, u8),
    /// The tile numbers of each step at the last look she could walk at, by steps from
    /// her, to tell a picture moving from a tile changed.
    numbers: HashMap<Pos, [u8; 4]>,
    /// How often each change of picture was seen on a step while she stood still.
    moving: HashMap<(u64, u64), u32>,
    /// Her map has shown at every look since the last one she could walk at.
    on_map: bool,
    /// The screen scrolled further than steps go, or while she couldn't walk, since she
    /// last could.
    jumped: bool,
}

/// The screen the last time she could walk: its scroll, and each tile's number and
/// picture.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Calm {
    scroll: (u8, u8),
    tiles: Vec<(u8, u64)>,
}

/// How the screen's steps sit: the pixel the grid starts at, steps across and down, and
/// the step she stands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Steps {
    origin: (i32, i32),
    across: i32,
    down: i32,
    player: (i32, i32),
}

impl Sight {
    /// The cheat finder, while playing (for the measurements).
    #[cfg(test)]
    pub fn finder_now(&self) -> Option<&Finder> {
        self.live.finder.as_ref()
    }

    fn finder(&mut self) -> &mut Finder {
        let found = self.found;
        self.live.finder.get_or_insert_with(|| {
            let mut finder = Finder::default();
            if let (Some(x), Some(y)) = (found.x, found.y) {
                finder.position = Position::known(x, y);
            }
            if let Some(area) = found.area {
                finder.areas = Areas::known(area);
            }
            finder
        })
    }

    /// Where memory says she is. Not in a game where she walks across a still screen:
    /// there her steps are counted from her sprite, which beats a byte that may count
    /// pixels (and wrap every 16 steps) or miss the part steps against walls.
    fn position(&self, memory: &Memory) -> Option<Pos> {
        self.live
            .finder
            .as_ref()
            .filter(|_| self.live.spot.is_none())
            .and_then(|f| f.position.at(memory))
    }

    fn area_byte(&self) -> Option<u16> {
        self.live.finder.as_ref().and_then(|f| f.areas.found())
    }

    /// A new place's name.
    fn new_place(&mut self) -> String {
        self.places += 1;
        format!("Place {}", self.places)
    }

    /// The place whose look matches these steps, if one does.
    /// The places she has been, and memory as she came into each (for the measurements).
    #[cfg(test)]
    pub fn places(&self) -> Vec<(String, Memory)> {
        let mut places: Vec<_> = self
            .live
            .arrived
            .iter()
            .map(|(name, memory)| (name.clone(), memory.clone()))
            .collect();
        places.sort_by(|a, b| a.0.cmp(&b.0));
        places
    }

    /// How many pictures are known to move.
    #[cfg(test)]
    pub fn moving_pictures(&self) -> usize {
        self.lively.len()
    }

    /// The steps without the pictures that move.
    fn steady(&self, seen: Vec<(Pos, u64)>) -> Vec<(Pos, u64)> {
        seen.into_iter()
            .filter(|(_, key)| !self.lively.contains(key))
            .collect()
    }

    /// Whether a step would show which sprite is her (see `Live::finding`): until then
    /// she isn't free to walk, so walking takes the step anyway.
    pub fn finding_her(&self) -> bool {
        self.live.finding
    }

    /// Where she was at the last look.
    pub fn whereabouts(&self) -> Option<&Whereabouts> {
        self.whereabouts.as_ref()
    }

    /// Carries on from a save state where she was `at` (a checkpoint loaded): the next look
    /// starts there, as a session's first look does. What the cheat finder has found so
    /// far stays.
    pub fn carry_on_from(&mut self, at: Option<Whereabouts>) {
        let finder = self.live.finder.take();
        self.live = Live {
            finder,
            ..Live::default()
        };
        self.whereabouts = at;
    }

    /// Her sprite is the one at `spot` (`moves::which_is_her` found it).
    pub fn found_her_at(&mut self, spot: (i32, i32)) {
        self.live.spot = Some(spot);
        self.live.candidate = None;
        self.live.lost = false;
        self.live.just_found = true;
    }

    /// Where her sprite is on the screen, once a step has shown it moves across it.
    pub fn her_spot(&self) -> Option<(i32, i32)> {
        self.live.spot
    }

    /// The place these steps look like, other than the one she just left (Red's stairs
    /// lead to a room that looks much like the one above: she isn't where she was).
    /// A picture that covers much of the screen (plain floor, the dark past a room's walls)
    /// says nothing: rooms built alike share it at the same steps.
    fn known_place(&self, seen: &[(Pos, u64)], left: &str) -> Option<String> {
        let telling = without_common(seen);
        self.pictures
            .iter()
            .filter(|(name, _)| *name != left)
            .filter_map(|(name, pictures)| {
                let alike = |(same, both): (usize, usize), after| {
                    both < after || same as f32 >= both as f32 * KNOWN_SHARE
                };
                let (same, both) = compare(pictures, seen);
                (both >= KNOWN_AFTER
                    && alike((same, both), 0)
                    && alike(compare(pictures, &telling), TELLING_AFTER))
                .then(|| (same, name.clone()))
            })
            .max()
            .map(|(_, name)| name)
    }

    /// The same, for steps counted from wherever she came in (a game she walks across):
    /// the place, and how far its steps are from the ones she counted. Each picture seen
    /// before at some step suggests a shift; the ones most suggested are tried.
    fn known_place_shifted(
        &self,
        seen: &[(Pos, u64)],
        may_be: impl Fn(&str) -> bool,
    ) -> Option<(String, Pos)> {
        let telling = without_common(seen);
        let shifted = |seen: &[(Pos, u64)], by: Pos| -> Vec<(Pos, u64)> {
            seen.iter()
                .map(|&((x, y), key)| ((x + by.0, y + by.1), key))
                .collect()
        };
        let mut best: Option<(usize, String, Pos)> = None;
        for (name, pictures) in self.pictures.iter().filter(|(name, _)| may_be(name)) {
            let mut votes: HashMap<Pos, usize> = HashMap::new();
            for &((x, y), key) in &telling {
                for (&(px, py), _) in pictures.0.iter().filter(|(_, k)| **k == key) {
                    *votes.entry((px - x, py - y)).or_default() += 1;
                }
            }
            let mut shifts: Vec<(usize, Pos)> = votes.into_iter().map(|(by, n)| (n, by)).collect();
            shifts.sort_unstable_by(|a, b| b.cmp(a));
            for &(_, by) in shifts.iter().take(SHIFTS_TRIED) {
                let alike = |(same, both): (usize, usize), after| {
                    both < after || same as f32 >= both as f32 * KNOWN_SHARE
                };
                let (same, both) = compare(pictures, &shifted(seen, by));
                if both >= KNOWN_AFTER
                    && alike((same, both), 0)
                    && alike(compare(pictures, &shifted(&telling, by)), TELLING_AFTER)
                    && best.as_ref().is_none_or(|(most, _, _)| same > *most)
                {
                    best = Some((same, name.clone(), by));
                }
            }
        }
        best.map(|(_, name, by)| (name, by))
    }

    /// Names the places by the area byte, when it has just been found or a better one has
    /// replaced it.
    /// Two places the byte would give one name are checked first: if they look different,
    /// the byte (and every other that holds the same in both) is something else.
    fn name_places(&mut self, areas: &mut HashMap<String, AreaMap>) {
        loop {
            let byte = self.area_byte();
            if byte == self.found.area {
                return;
            }
            let Some(byte) = byte else {
                self.found.area = None;
                return;
            };
            let names = self.names_by(byte);
            if let Some((a, b)) = self.different_but_named_alike(&names) {
                let (a, b) = (self.live.arrived[&a].clone(), self.live.arrived[&b].clone());
                self.finder().areas.different_place(&a, &b);
                continue;
            }
            self.found.area = Some(byte);
            rename(self, areas, &names);
            return;
        }
    }

    /// Each place's name once this byte says the area: places it gives one value become
    /// one, under the oldest name, and every other name stays as it was (her notes use
    /// them).
    fn names_by(&self, byte: u16) -> HashMap<String, String> {
        let mut oldest: HashMap<u8, &String> = HashMap::new();
        for (name, memory) in &self.live.arrived {
            let first = oldest.entry(memory.get(byte)).or_insert(name);
            if place_number(name) < place_number(first) {
                *first = name;
            }
        }
        self.live
            .arrived
            .iter()
            .map(|(name, memory)| (name.clone(), oldest[&memory.get(byte)].clone()))
            .collect()
    }

    /// The place the area byte says she's in, if she's been there.
    fn place_by(&self, byte: u16, memory: &Memory) -> Option<String> {
        self.live
            .arrived
            .iter()
            .filter(|(_, then)| then.get(byte) == memory.get(byte))
            .map(|(name, _)| name)
            .min_by_key(|name| place_number(name))
            .cloned()
    }

    /// Two places that would get one name but look different.
    fn different_but_named_alike(
        &self,
        names: &HashMap<String, String>,
    ) -> Option<(String, String)> {
        let empty = Pictures::default();
        for (a, new_a) in names {
            for (b, new_b) in names {
                if a >= b || new_a != new_b {
                    continue;
                }
                let pa = self.pictures.get(a).unwrap_or(&empty);
                let pb = self.pictures.get(b).unwrap_or(&empty);
                let seen = self.steady(pb.0.iter().map(|(&pos, &key)| (pos, key)).collect());
                // Both seen in full, one place looks the same nearly everywhere.
                let (same, both) = compare(pa, &seen);
                let unlike = both >= KNOWN_AFTER && (same as f32) < both as f32 * KNOWN_SHARE;
                if unlike || looks_different(pa, &seen) {
                    return Some((a.clone(), b.clone()));
                }
            }
        }
        None
    }
}

/// The steps without the pictures that cover a fifth of them or more.
fn without_common(seen: &[(Pos, u64)]) -> Vec<(Pos, u64)> {
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for (_, key) in seen {
        *counts.entry(*key).or_default() += 1;
    }
    seen.iter()
        .filter(|(_, key)| counts[key] * COMMON_SHARE < seen.len())
        .copied()
        .collect()
}

/// The number in a place's name, for keeping the oldest name.
fn place_number(name: &str) -> (u32, String) {
    let number = name.rsplit(' ').next().and_then(|n| n.parse().ok());
    (number.unwrap_or(u32::MAX), name.to_string())
}

/// How many of these steps were seen at the same places before, and how many of those
/// looked the same.
fn compare(pictures: &Pictures, seen: &[(Pos, u64)]) -> (usize, usize) {
    let both: Vec<bool> = seen
        .iter()
        .filter_map(|(pos, key)| pictures.0.get(pos).map(|k| k == key))
        .collect();
    (both.iter().filter(|&&s| s).count(), both.len())
}

/// Whether these steps look like somewhere else than a place: different where both were
/// seen, or made of pictures hardly ever seen there (a shop beside a route), unless the
/// steps seen there before mostly look the same.
fn looks_different(pictures: &Pictures, seen: &[(Pos, u64)]) -> bool {
    let (same, both) = compare(pictures, seen);
    let (shared, kinds) = shared_pictures(pictures, seen);
    let vouched = both >= VOUCHED_BY && same * 4 >= both * 3;
    (both >= KNOWN_AFTER && (same as f32) < both as f32 * 0.5)
        || (!vouched
            && pictures.0.len() >= KNOWN_AFTER
            && kinds >= OTHER_KINDS
            && (shared as f32) < kinds as f32 * OTHER_SHARE)
}

/// Of the different pictures in `seen`, how many were seen anywhere in a place, and how
/// many there are.
fn shared_pictures(pictures: &Pictures, seen: &[(Pos, u64)]) -> (usize, usize) {
    let there: HashSet<u64> = pictures.0.values().copied().collect();
    let here: HashSet<u64> = seen.iter().map(|&(_, key)| key).collect();
    (here.intersection(&there).count(), here.len())
}

/// Looks at the game, learns from the step just taken, and marks what it sees on the
/// area's map.
pub fn look(world: &mut World, machine: &mut Machine) -> View {
    let sight = &mut world.sight;
    // A sprite that walks across the screen: the camera can stop anywhere, so the screen
    // is read in whole tiles of the map.
    let steady = sight.live.spot.is_some() || world.stepped.is_some_and(|s| s.her.is_some());
    let mut screen = screen_of(machine.emulator.video(), steady);
    let mut memory = Memory::read(&mut machine.emulator);

    if sight.live.memory.is_none() {
        // The first look: where she was when the state was saved, or wherever she is gets
        // a name, kept until memory gives one.
        if let Some(w) = sight.whereabouts.clone() {
            sight.live.area = w.area;
            sight.live.at = w.at;
            sight.live.spot = w.spot;
            sight.live.walks_across = w.walks_across || w.spot.is_some();
            sight.live.her = w.her.into_iter().collect();
        } else if sight.live.area.is_empty() {
            sight.live.area = sight.new_place();
        }
        sight
            .live
            .arrived
            .insert(sight.live.area.clone(), memory.clone());
    }
    let stepped = world.stepped.zip(sight.live.memory.clone());
    let mut dark = false;
    if let Some((step, _)) = &stepped {
        dark = step.outcome == Outcome::NewArea;
        // A step onto a picture not known as floor may be a door: the fade can start a
        // moment after the step ends.
        if let Outcome::Moved(n) = step.outcome {
            let (dx, dy) = offset(step.direction.button()).unwrap_or((0, 0));
            let floor = sight
                .live
                .around
                .get(&(dx * n, dy * n))
                .and_then(|key| sight.steps.get(key))
                .is_some_and(|l| l.doors == 0 && l.walked > 0);
            if !floor && settle(machine) {
                dark = true;
                screen = screen_of(machine.emulator.video(), steady);
                memory = Memory::read(&mut machine.emulator);
            }
        }
    }
    let faded = went_dark(machine, sight.live.recorded);
    // Where her sprite is, in a game where it walks across a still screen: where the
    // step left it, or found again by its pictures near where it was (her own presses
    // may have moved it).
    let spot = sight.live.spot;
    let pressed = machine.emulator.directions_pressed() != sight.live.pressed;
    match &stepped {
        Some((step, _)) if step.her.is_some() => {
            sight.live.spot = match spot {
                Some(_) => step.her,
                None => confirmed(&mut sight.live, step.her),
            };
        }
        _ => match spot {
            // Followed frame by frame first: a press of hers can take her further than her
            // pictures are looked for, facing a way not seen before.
            Some(last) => {
                sight.live.spot = machine
                    .sprites_since(sight.live.frame)
                    .and_then(|frames| super::moves::followed(&frames, last, &screen))
                    .or_else(|| find_her(&sight.live.her, &screen, last))
                    .or(spot);
            }
            // Nothing stands in the middle, and her own presses moved sprites across a
            // still screen: those are her.
            None if pressed
                && screen.drawn
                && (screen.registers.scx, screen.registers.scy) == sight.live.scroll
                && steps_of(&screen, None).is_none() =>
            {
                let found = moved_together(&sight.live.sprites, &screen);
                sight.live.spot = confirmed(&mut sight.live, found);
            }
            None => {}
        },
    }
    // Through a door's fade she comes in somewhere else on the screen: where she was says
    // nothing (in Link's Awakening's telephone hut her spot stayed on the telephone, and
    // she was never free there, so the hut was never a place).
    if (faded || dark)
        && !std::mem::take(&mut sight.live.just_found)
        && let Some(last) = sight.live.spot
    {
        sight.live.spot = her_anywhere(&sight.live.her, &screen, last);
        sight.live.lost = sight.live.spot.is_none();
    }
    // No sprite where she's thought to be: lost (a screen sliding faster than she can be
    // followed, a scene moving her). Found again by pressing, the way she was at first.
    if let Some(at) = sight.live.spot
        && screen.drawn
        && !screen.sprites.iter().any(|s| {
            (s.x + 4 - at.0).abs() <= HER_NEAR && (s.y + s.height / 2 - at.1).abs() <= HER_NEAR
        })
    {
        sight.live.spot = her_anywhere(&sight.live.her, &screen, at);
        sight.live.lost = sight.live.spot.is_none();
    }
    // Where she walks across the screen, her sprite not known is always lost, to be found
    // again by pressing: a save state taken after a game over had her neither found nor
    // lost, and she couldn't walk for a whole episode.
    sight.live.lost = sight.live.spot.is_none()
        && (sight.live.lost || (sight.live.walks_across && !sight.live.her.is_empty()));
    sight.live.sprites = screen.sprites.iter().map(|s| (s.x, s.y)).collect();
    // Her sprite, when a press of hers just moved her (a step code took, or one she
    // pressed herself).
    let scroll = (screen.registers.scx, screen.registers.scy);
    let walked = match &stepped {
        Some((step, _)) => matches!(step.outcome, Outcome::Moved(_)),
        None => {
            machine.emulator.directions_pressed() != sight.live.pressed
                && (scroll != sight.live.scroll || sight.live.spot != spot)
        }
    };
    let stood_still = stepped.is_none()
        && !walked
        && scroll == sight.live.scroll
        && !faded
        && sight.live.was_free;
    let was = (sight.live.area.clone(), sight.live.at);
    let scroll_was = sight.live.scroll;
    sight.live.scroll = scroll;
    // Whether her map still shows under whatever covers it (a menu, a text box), since
    // she last could walk: a battle or a scene puts another screen up.
    let on_map =
        sight.live.on_map
            && !faded
            && sight.live.calm.as_ref().is_some_and(|last| {
                changed_since(last, &calm_of(&screen), (0, 0)) <= COLS * ROWS / 2
            });
    // After a fade anything goes, whether a step code took went dark or her own presses
    // did between looks.
    let free = calm(sight, machine, &screen, dark || faded, stepped.is_some());
    let faded_away = std::mem::take(&mut sight.live.faded_away) || ((dark || faded) && !free);
    if !free {
        sight.live.faded_away = faded_away;
    }
    sight.live.finding = !free
        && sight.live.spot.is_none()
        && (sight.live.her.is_empty() || sight.live.lost)
        && screen.drawn
        && !screen.sprites.is_empty()
        && steps_of(&screen, None).is_none();
    // Only a look she can walk at: a battle's moves press directions and scroll too.
    if walked && free {
        sight
            .live
            .her
            .extend(her_sprites(&screen, sight.live.spot).collect::<Vec<_>>());
    }
    #[cfg(test)]
    let stepped_at_all = stepped.as_ref().map(|(s, _)| (s.direction, s.outcome));
    // Where she walks across the screen, her own presses (not a step code took) moved her
    // by as many steps as her sprite went, with the screen's scroll.
    // Where her sprite isn't followed (through a door's fade it shows up elsewhere), she's
    // still where she was, so a door she went through by her own presses still counts.
    let at = sight.position(&memory).or_else(|| {
        let from = spot.filter(|_| stepped.is_none())?;
        match sight.live.spot {
            Some(to) if pressed => {
                let (dx, dy) = pressed_steps((from, scroll_was), (to, scroll));
                Some((sight.live.at.0 + dx, sight.live.at.1 + dy))
            }
            _ => Some(sight.live.at),
        }
    });
    let mut seen = at
        .map(|at| sight.steady(seen_steps(&screen, at, sight.live.spot)))
        .unwrap_or_default();
    // Where she left the last place: the step she went into, and whether it was dark.
    let mut left = None;
    if let Some((mut step, before)) = stepped {
        let pressed = offset(step.direction.button()).unwrap_or((0, 0));
        let from = sight.live.at;
        // Where she walks across the screen, her steps are counted on the screen's own
        // grid from where her sprite was and is, as her own presses are, so what she
        // sees is marked where she stands (rounding a step's pixels can disagree with
        // the grid by one when she stands between two steps).
        // A step only takes her the way pressed (a hop, or something carrying her straight
        // on), about as far as the step saw her go (a slide is a room over, a step or
        // two): otherwise the sprite followed wasn't her (on Link's Awakening's beach a
        // crab, as the screen slid on, counted a step right as 7 left; in the village a
        // villager above her, a step up as 3), so it was one step and she's looked for
        // again.
        let mut astray = false;
        if let (Some(was), Some(now), Outcome::Moved(n)) = (spot, sight.live.spot, step.outcome) {
            let counted = pressed_steps((was, scroll_was), (now, scroll));
            let along = counted.0 * pressed.0 + counted.1 * pressed.1;
            let aside = counted.0 * pressed.1 - counted.1 * pressed.0;
            let as_far = if step.slid {
                along <= 2
            } else {
                (along - n).abs() <= 1
            };
            if along > 0 && aside == 0 && as_far {
                step.scrolled = counted;
            } else if counted != (0, 0) {
                step.scrolled = pressed;
                astray = true;
            }
        }
        if after_step(sight, &mut world.areas, step, &before, &memory, dark, free) {
            left = Some(((from.0 + pressed.0, from.1 + pressed.1), dark));
        }
        if astray && let Some(last) = sight.live.spot {
            sight.live.spot = her_anywhere(&sight.live.her, &screen, last);
            sight.live.lost = sight.live.spot.is_none();
        }
    } else if let Some(at) = at {
        // No step, but the game may have moved her: a scene, or a checkpoint loaded. If
        // the screen looks unlike where she last walked, it's somewhere else (a little
        // unlike says nothing: flowers and water move). If it went dark while her map
        // still showed since she last walked (a menu may have covered it), she walked
        // through a door by her own presses (Red's stairs look much like the room she
        // left); a battle ends dark too, but its own screen was up.
        // Where she walks across the screen, only a door counts, as for a step code takes:
        // a screen sliding on to the next room is the same place, and one that looked
        // unlike what was seen there had split Link's Awakening's village into five. But
        // once the screen went dark while she couldn't walk (a game over, a scene), she
        // may be anywhere: after Link's Awakening's game over she came back in Tarin's
        // house, still counted on the beach. (Her map not showing isn't enough: a slide
        // changes the whole screen, and session 10 split the outdoors into eight.)
        let through_door = faded && (sight.live.was_free || sight.live.on_map);
        // Where she walks across the screen, a fade seen while she couldn't walk yet
        // (the room still coming in) counts once she can: in Link's Awakening's
        // telephone hut, the look after the fade wasn't free, and the hut never became
        // a place.
        let door_pending = std::mem::take(&mut sight.live.door_pending)
            || (through_door && sight.live.spot.is_some());
        let through_door = through_door || door_pending;
        if !free && door_pending {
            sight.live.door_pending = true;
        }
        if free
            && sight.live.free.is_some()
            && sight.live.left.is_none()
            && (through_door
                || ((sight.live.spot.is_none() || faded_away)
                    && sight
                        .pictures
                        .get(&sight.live.area)
                        .is_some_and(|p| looks_different(p, &seen))))
        {
            left = Some((sight.live.at, true));
            sight.live.relocated = !through_door;
        }
        sight.live.at = at;
    }
    sight.live.walks_across |= sight.live.spot.is_some();
    // Where she walks across the screen, places are known by the steps she counted.
    if sight.live.spot.is_some() {
        seen = sight.steady(seen_steps(&screen, sight.live.at, sight.live.spot));
    }
    // Somewhere new is only looked at once she can walk there: a text box or a scene can
    // cover it as she comes in.
    if left.is_some() {
        sight.live.left = left;
    }
    if free {
        // What the area's byte did since she last walked: it changes going somewhere new,
        // and never otherwise (battles, menus and scenes included).
        let left = sight.live.left.take();
        if let Some(last) = sight.live.free.clone() {
            sight.finder().place(&last, &memory, left.is_some());
        }
        if let Some((gone, dark)) = left {
            let places = sight.places;
            arrive(sight, &mut world.areas, &memory, &seen, gone, dark);
            #[cfg(test)]
            if sight.places != places {
                trace_new_place(machine, sight, stepped_at_all, dark);
            }
            #[cfg(not(test))]
            let _ = places;
        }
    }
    sight.name_places(&mut world.areas);
    if let Some((x, y)) = sight
        .live
        .finder
        .as_ref()
        .filter(|_| sight.live.spot.is_none())
        .and_then(|f| f.position.found())
    {
        sight.found.x = Some(x);
        sight.found.y = Some(y);
    }

    let area = sight.live.area.clone();
    let at = sight.live.at;
    let steps = steps_of(&screen, sight.live.spot).filter(|_| free);
    if let Some(steps) = steps {
        let around = pictures_around(&screen, steps);
        let numbers = numbers_around(&screen, steps);
        // A step whose tiles kept their numbers but not their pictures while she stood
        // still is a picture moving (flowers, water). Many at once is the game loading
        // other pictures, which says nothing.
        if stood_still && was == (sight.live.area.clone(), sight.live.at) {
            let moving: Vec<(u64, u64)> = around
                .iter()
                .filter_map(|(offset, &key)| {
                    let old = *sight.live.around.get(offset)?;
                    (old != key && sight.live.numbers.get(offset) == numbers.get(offset))
                        .then_some((old, key))
                })
                .collect();
            if moving.len() <= MOVING_MOST {
                // Twice the same change: a picture loaded late changes once.
                for pair in moving {
                    let seen = sight.live.moving.entry(pair).or_default();
                    *seen += 1;
                    if *seen >= 2 {
                        sight.lively.extend([pair.0, pair.1]);
                    }
                }
            }
        }
        sight.live.numbers = numbers;
        sight.live.people = people(&screen, steps, sight.live.spot);
        let map = world.areas.entry(area.clone()).or_default();
        let pictures = sight.pictures.entry(area.clone()).or_default();
        for (&(dx, dy), &key) in &around {
            let pos = (at.0 + dx, at.1 + dy);
            pictures.0.insert(pos, key);
            if (dx, dy) == (0, 0) {
                continue;
            }
            if let Some(kind) = sight.steps.get(&key).and_then(|l| l.kind()) {
                map.see(pos, kind);
            }
        }
        map.see_people(
            sight
                .live
                .people
                .iter()
                .map(|&(dx, dy)| (at.0 + dx, at.1 + dy)),
        );
        sight.live.around = around;
    }
    if free {
        sight.live.free = Some(memory.clone());
    }
    sight.live.was_free = free;
    sight.live.on_map = free || on_map;
    sight.live.recorded = machine.recorded();
    sight.live.frame = machine.emulator.frames();
    sight.live.memory = Some(memory);
    let mut her: Vec<u64> = sight.live.her.iter().copied().collect();
    her.sort_unstable();
    sight.whereabouts = Some(Whereabouts {
        area: area.clone(),
        at,
        spot: sight.live.spot,
        her,
        walks_across: sight.live.walks_across,
    });
    View {
        area,
        at,
        can_walk: steps.is_some(),
        text: String::new(),
        facts: String::new(),
        grid: steps.and_then(|s| {
            (s.origin == (0, 0)).then_some(Grid {
                pixels: STEP,
                across: s.across,
                down: s.down,
                player: s.player,
            })
        }),
    }
}

/// How many steps her sprite went between two looks, from where it was on the screen and
/// the screen's scroll each time. The scroll wraps every 256 pixels: of the ways it could
/// have gone, the one that moved her least (a screen sliding on to the next room takes her
/// from one edge to the other while she walks a step or two).
fn pressed_steps(
    (from, scroll_from): ((i32, i32), (u8, u8)),
    (to, scroll_to): ((i32, i32), (u8, u8)),
) -> Pos {
    let axis = |from: i32, scroll_from: u8, to: i32, scroll_to: u8| {
        let turned = i32::from(scroll_to.wrapping_sub(scroll_from));
        let scrolled = [turned, turned - 256]
            .into_iter()
            .min_by_key(|s| (to - from + s).abs())
            .unwrap_or(0);
        let start = from + i32::from(scroll_from);
        (start + to - from + scrolled).div_euclid(STEP) - start.div_euclid(STEP)
    };
    (
        axis(from.0, scroll_from.0, to.0, scroll_to.0),
        axis(from.1, scroll_from.1, to.1, scroll_to.1),
    )
}

/// Learns from one step: what the step ahead was and where she is now. Says whether she
/// went somewhere new.
fn after_step(
    sight: &mut Sight,
    areas: &mut HashMap<String, AreaMap>,
    step: Stepped,
    before: &Memory,
    after: &Memory,
    dark: bool,
    free: bool,
) -> bool {
    let pressed = offset(step.direction.button()).unwrap_or((0, 0));
    let had_position = sight.position(before).is_some();
    // A hop or a carry is only learned where she's known to be, by memory or her sprite:
    // before Link's sprite was found, the steps that found him counted as being carried,
    // and a fresh start drew Link's Awakening's village as spinners.
    let knows_where = had_position || sight.live.spot.is_some();
    let dead_reckoned = (
        sight.live.at.0 + step.scrolled.0,
        sight.live.at.1 + step.scrolled.1,
    );
    // Where she walks across a still screen, her steps are counted from her sprite
    // (`position`), and somewhere new is through a door: a screen that slides on to the
    // next room is just a long way walked.
    let jumped = if sight.live.spot.is_some() {
        None
    } else {
        sight
            .finder()
            .stepped(before, after, pressed, step.scrolled, dark)
    };
    let now = sight.position(after);
    // Memory put her somewhere the step couldn't have, or the step went dark and left her
    // free to walk: a door, even to the same numbers (Red's stairs land on the same spot
    // a floor down, in a room much like the one above). The swirl into a battle goes
    // dark too, but she isn't free after it.
    let new_place = jumped == Some(true) || (dark && free);

    // What the step ahead turned out to be, by its picture.
    let ahead = |n: i32| (pressed.0 * n, pressed.1 * n);
    match step.outcome {
        // Two steps the way she pressed: she hopped over the step ahead (a ledge).
        Outcome::Moved(2)
            if !new_place
                && knows_where
                && step.scrolled == ahead(2)
                && !(step.slid && sight.live.spot.is_some()) =>
        {
            if let Some(&key) = sight.live.around.get(&ahead(1)) {
                sight.steps.entry(key).or_default().over = Some(step.direction);
            }
            if let Some(&key) = sight.live.around.get(&ahead(2)) {
                sight.steps.entry(key).or_default().walked += 1;
            }
        }
        // Anywhere else but the next step: the step ahead carried her on.
        // Where she walks across the screen, only straight on and further than a hop: games
        // like Link's Awakening nudge her round corners, a step aside.
        Outcome::Moved(_)
            if !new_place
                && knows_where
                && step.scrolled != ahead(1)
                && !(sight.live.spot.is_some()
                    && (step.slid
                        || step.scrolled.0 * pressed.1 != step.scrolled.1 * pressed.0
                        || step.scrolled.0 * pressed.0 + step.scrolled.1 * pressed.1 < 3)) =>
        {
            if let Some(&key) = sight.live.around.get(&ahead(1)) {
                sight.steps.entry(key).or_default().carried += 1;
            }
            if let Some(&key) = sight.live.around.get(&step.scrolled) {
                sight.steps.entry(key).or_default().walked += 1;
            }
        }
        Outcome::Moved(n) => {
            if let Some(&key) = sight.live.around.get(&ahead(n)) {
                let learned = sight.steps.entry(key).or_default();
                // Through a door: it went dark. Somewhere new with no fade is an edge,
                // which says nothing about the picture (it's ground like any other).
                if new_place && dark {
                    learned.doors += 1;
                } else if !new_place {
                    learned.walked += 1;
                }
            }
        }
        // Blocked, and still free: a wall. A scene that stops her (someone calling out)
        // says nothing about the step.
        Outcome::Blocked if free && !sight.live.people.contains(&ahead(1)) => {
            if let Some(&key) = sight.live.around.get(&ahead(1)) {
                sight.steps.entry(key).or_default().blocked += 1;
            }
        }
        _ => {}
    }

    // The position just found: what was drawn from counted steps moves to memory's
    // numbers. Each place was counted from (0, 0) where she came in, and memory then
    // says where that was; this one by where she is now, and it's forgotten if she
    // just left it.
    if !had_position && let Some(at) = now {
        let here = sight.live.area.clone();
        for (name, then) in std::mem::take(&mut sight.live.arrived) {
            let by = if name == here {
                (!new_place).then_some((at.0 - dead_reckoned.0, at.1 - dead_reckoned.1))
            } else {
                sight.position(&then)
            };
            let (map, pictures) = (areas.remove(&name), sight.pictures.remove(&name));
            let Some(by) = by else { continue };
            if let Some(map) = map {
                areas.insert(name.clone(), map.shifted(by));
            }
            if let Some(pictures) = pictures {
                let moved = pictures
                    .0
                    .into_iter()
                    .map(|((x, y), k)| ((x + by.0, y + by.1), k));
                sight
                    .pictures
                    .insert(name.clone(), Pictures(moved.collect()));
            }
            sight.live.arrived.insert(name, then);
        }
    }
    sight.live.at = now.unwrap_or(if new_place { (0, 0) } else { dead_reckoned });
    new_place
}

/// She has gone somewhere new, from the last place's step `gone` (a door if the screen
/// went dark, its edge if not): checks the area byte against the places she knows, and
/// names the new one.
fn arrive(
    sight: &mut Sight,
    areas: &mut HashMap<String, AreaMap>,
    memory: &Memory,
    seen: &[(Pos, u64)],
    gone: Pos,
    dark: bool,
) {
    sight.live.calm = None;
    // Not through a door (a game over, a scene): where she was leads nowhere, and she may
    // be somewhere else in the same place.
    let relocated = std::mem::take(&mut sight.live.relocated);
    let old_area = sight.live.area.clone();
    let led = areas
        .get(&old_area)
        .and_then(|map| map.doors.get(&gone))
        .filter(|_| !relocated)
        .cloned();
    // Back through a door she came through before: the area byte holds what it held
    // the first time she came in.
    // Or a place she knows by its look: the same.
    let back = led.or_else(|| sight.known_place(seen, &old_area));
    if let Some(then) = back.as_ref().and_then(|name| sight.live.arrived.get(name)) {
        let then = then.clone();
        sight.finder().areas.same_place(&then, memory);
    }
    // A place that looks different is another place: the area byte holds something else
    // here.
    let others: Vec<Memory> = sight
        .live
        .arrived
        .iter()
        .filter(|(name, _)| {
            sight
                .pictures
                .get(*name)
                .is_some_and(|p| looks_different(p, seen))
        })
        .map(|(_, m)| m.clone())
        .collect();
    for then in &others {
        sight.finder().areas.different_place(then, memory);
    }
    sight.name_places(areas);
    let old_area = sight.live.area.clone();
    let led = areas
        .get(&old_area)
        .and_then(|map| map.doors.get(&gone))
        .filter(|_| !relocated)
        .cloned();
    // Where she walks across the screen, her steps in a place she knows are counted on
    // from where she was there before: found by its look, or else where she came in by
    // this door last time. Through a known door, the steps she counted were the last
    // place's, and the outdoors of Link's Awakening was drawn into a house she went into.
    let walks_across = sight.live.walks_across;
    let shifted = walks_across
        .then(|| match &led {
            Some(place) => sight.known_place_shifted(seen, |name| name == place),
            None if relocated => sight.known_place_shifted(seen, |_| true),
            None => sight.known_place_shifted(seen, |name| name != old_area),
        })
        .flatten();
    let door = (old_area.clone(), gone);
    if let Some((_, by)) = shifted {
        sight.live.at = (sight.live.at.0 + by.0, sight.live.at.1 + by.1);
    } else if walks_across
        && led.is_some()
        && let Some(&came) = sight.came_in.get(&door)
    {
        sight.live.at = came;
    }
    let name = match sight.area_byte() {
        // The byte can't name the place she just left: she stepped somewhere new.
        Some(byte) => sight
            .place_by(byte, memory)
            .filter(|name| *name != old_area),
        None if shifted.is_some() => shifted.map(|(name, _)| name),
        None => led.or_else(|| sight.known_place(seen, &old_area)),
    }
    .unwrap_or_else(|| sight.new_place());
    if !relocated {
        let map = areas.entry(old_area).or_default();
        map.see(gone, if dark { Kind::Door } else { Kind::Edge });
        map.doors.insert(gone, name.clone());
        if walks_across {
            sight.came_in.insert(door, sight.live.at);
        }
    }
    sight
        .live
        .arrived
        .entry(name.clone())
        .or_insert(memory.clone());
    sight.live.area = name;
}

/// Renames places, merging two that turn out to be one.
fn rename(
    sight: &mut Sight,
    areas: &mut HashMap<String, AreaMap>,
    names: &HashMap<String, String>,
) {
    let mut arrived = HashMap::new();
    for (name, memory) in std::mem::take(&mut sight.live.arrived) {
        let new = names.get(&name).cloned().unwrap_or(name);
        arrived.entry(new).or_insert(memory);
    }
    sight.live.arrived = arrived;
    let mut maps = HashMap::new();
    let mut pictures = HashMap::new();
    for (old, new) in names {
        if let Some(map) = areas.remove(old) {
            maps.entry(new.clone())
                .or_insert_with(AreaMap::default)
                .merge(map);
        }
        if let Some(seen) = sight.pictures.remove(old) {
            let into: &mut Pictures = pictures.entry(new.clone()).or_default();
            for (pos, key) in seen.0 {
                into.0.entry(pos).or_insert(key);
            }
        }
    }
    for (name, map) in maps {
        areas.entry(name).or_default().merge(map);
    }
    for (name, seen) in pictures {
        let into = sight.pictures.entry(name).or_default();
        for (pos, key) in seen.0 {
            into.0.entry(pos).or_insert(key);
        }
    }
    for map in areas.values_mut() {
        for to in map.doors.values_mut() {
            if let Some(new) = names.get(to) {
                *to = new.clone();
            }
        }
    }
    if let Some(new) = names.get(&sight.live.area) {
        sight.live.area = new.clone();
    }
}

/// Whether the screen shows her free to walk: a picture with her sprite in the middle and
/// no window over her, and no more than a few tiles changed since the last calm screen
/// (allowing for a scroll of a step or two). After a dark screen anything goes, since
/// it's somewhere new; a scroll with no step taken means the game is moving her.
fn calm(
    sight: &mut Sight,
    machine: &mut Machine,
    screen: &Screen,
    dark: bool,
    stepped: bool,
) -> bool {
    // A direction pressed since the last look: she may have walked without a step code
    // took (a press she asked for).
    let pressed = machine.emulator.directions_pressed();
    let stepped = stepped || pressed != sight.live.pressed;
    sight.live.pressed = pressed;
    let video = machine.emulator.video().clone();
    let rgb = machine.emulator.screen();
    if frame_look(video.drawn, &video.registers, &rgb, video.color) != FrameLook::Picture
        || !screen.drawn
        || !screen.aligned
    {
        return false;
    }
    let spot = sight.live.spot;
    let Some(steps) = steps_of(screen, spot) else {
        return false;
    };
    let r = screen.registers;
    let her = (
        steps.origin.0 + steps.player.0 * STEP + STEP / 2,
        steps.origin.1 + steps.player.1 * STEP + STEP / 2,
    );
    if r.lcdc & 0x20 != 0 && i32::from(r.wy) <= her.1 && i32::from(r.wx) <= her.0 + 7 {
        return false;
    }
    let now = calm_of(screen);
    if dark {
        sight.live.calm = None;
    }
    let Some(last) = &sight.live.calm else {
        // Nothing calm to compare with (just after a fade, or all through a battle or a
        // scene): it must hold still a moment with her sprite where it is. A battle's
        // moves are sprites near the middle too, but they don't stay put.
        if !is_her(&sight.live.her, screen, spot) || !holds_still(machine, &now, steps, spot) {
            return false;
        }
        sight.live.calm = Some(now);
        sight.live.jumped = false;
        return true;
    };
    // Cells the scroll moved by: whole tiles of the map, read steady.
    let moved = |a: u8, b: u8| {
        if spot.is_some() {
            let tile = |v: u8| (i32::from(v) + 7) / 8;
            let d = (tile(b) - tile(a)).rem_euclid(32);
            if d >= 16 { d - 32 } else { d }
        } else {
            i32::from(b.wrapping_sub(a) as i8) / 8
        }
    };
    let by = (
        moved(last.scroll.0, now.scroll.0),
        moved(last.scroll.1, now.scroll.1),
    );
    // Scrolled by a step or two, the tiles still on screen have moved with it; a text box
    // that came up as the step ended hasn't. Scrolled further, nothing is left to compare:
    // her sprite must be in the middle (a battle's own screen has other sprites there),
    // there and at every look until she's free.
    let near = by.0.abs() <= 4 && by.1.abs() <= 4;
    if !near {
        sight.live.jumped = true;
    }
    let free = (by == (0, 0) || stepped)
        && (!near || changed_since(last, &now, by) <= CALM_CHANGES)
        && (!sight.live.jumped || is_her(&sight.live.her, screen, spot));
    if free {
        sight.live.jumped = false;
    }
    // Following a scroll she wasn't free at (a scene moving her, or a battle's own screen
    // sliding in with no fade): her sprite must be in the middle again before she is.
    if free || by != (0, 0) {
        sight.live.calm = Some(now);
        sight.live.jumped |= !free;
    }
    free
}

/// A sprite that moved her way is hers once it does it twice running, from where it
/// went the first time: a dog or a child wandering about can move her way once (in
/// Link's Awakening's village, a fox was followed as her and she never got free).
fn confirmed(live: &mut Live, found: Option<(i32, i32)>) -> Option<(i32, i32)> {
    let found = found?;
    let again = live
        .candidate
        .is_some_and(|c| (c.0 - found.0).abs() + (c.1 - found.1).abs() <= STEP + HER_NEAR);
    live.candidate = Some(found);
    again.then_some(found)
}

/// The pictures of the sprites where she stands: the middle of the screen, or her spot.
fn her_sprites(screen: &Screen, spot: Option<(i32, i32)>) -> impl Iterator<Item = u64> + '_ {
    let at = spot.unwrap_or(MIDDLE);
    screen
        .sprites
        .iter()
        .filter(move |s| (s.x + 4 - at.0).abs() + (s.y + s.height / 2 - at.1).abs() <= HER_NEAR)
        .map(picture_of)
}

/// A sprite's picture, the same facing left or right.
fn picture_of(s: &super::screen::Sprite) -> u64 {
    s.key ^ u64::from(s.flags & 0x20)
}

/// Whether a sprite where she stands is one of hers, or nothing of hers is known yet. In
/// a game where she walks across the screen, her spot follows her own sprites, so a
/// sprite there is her, whichever way she faces (a way she hasn't faced yet has
/// pictures not seen before).
fn is_her(her: &HashSet<u64>, screen: &Screen, spot: Option<(i32, i32)>) -> bool {
    her.is_empty()
        || (spot.is_some() && her_sprites(screen, spot).next().is_some())
        || her_sprites(screen, spot).any(|key| her.contains(&key))
}

/// The sprites that all moved the same way since `then` (two or more, a step's worth or
/// less), the most that did: the middle of them.
fn moved_together(then: &[(i32, i32)], screen: &Screen) -> Option<(i32, i32)> {
    // A sprite that stayed put explains itself better than a move.
    let still: HashSet<(i32, i32)> = then.iter().copied().collect();
    let mut counts: HashMap<(i32, i32), usize> = HashMap::new();
    for s in screen
        .sprites
        .iter()
        .filter(|s| !still.contains(&(s.x, s.y)))
    {
        let mut moves: Vec<(i32, i32)> = then
            .iter()
            .map(|t| (s.x - t.0, s.y - t.1))
            .filter(|d| *d != (0, 0) && d.0.abs() <= 2 * STEP && d.1.abs() <= 2 * STEP)
            .collect();
        moves.sort_unstable();
        moves.dedup();
        for d in moves {
            *counts.entry(d).or_default() += 1;
        }
    }
    let (&d, &n) = counts
        .iter()
        .max_by_key(|&(d, n)| (*n, -(d.0.abs() + d.1.abs())))?;
    if n < 2 {
        return None;
    }
    let group: Vec<_> = screen
        .sprites
        .iter()
        .filter(|s| !still.contains(&(s.x, s.y)) && then.contains(&(s.x - d.0, s.y - d.1)))
        .copied()
        .collect();
    if group.len() < 2 {
        return None;
    }
    middle_of(&touching(&group, &screen.sprites))
}

/// Her sprites near where they last were: the middle of them (and the sprites that touch
/// them), if any are there.
fn find_her(her: &HashSet<u64>, screen: &Screen, last: (i32, i32)) -> Option<(i32, i32)> {
    let hers: Vec<_> = screen
        .sprites
        .iter()
        .filter(|s| her.contains(&picture_of(s)))
        .filter(|s| (s.x + 4 - last.0).abs() + (s.y + s.height / 2 - last.1).abs() <= 2 * STEP)
        .copied()
        .collect();
    middle_of(&touching(&hers, &screen.sprites))
}

/// Her sprites anywhere on the screen, the group nearest `last`: where she went when a
/// slide or a scene carried her off where she was followed.
fn her_anywhere(her: &HashSet<u64>, screen: &Screen, last: (i32, i32)) -> Option<(i32, i32)> {
    let nearest = screen
        .sprites
        .iter()
        .filter(|s| her.contains(&picture_of(s)))
        .min_by_key(|s| (s.x + 4 - last.0).abs() + (s.y + s.height / 2 - last.1).abs())?;
    middle_of(&touching(&[*nearest], &screen.sprites))
}

/// Whether any screen recorded after `mark` was all one colour: a fade, or the screen off.
fn went_dark(machine: &Machine, mark: u64) -> bool {
    machine
        .screens_since(mark)
        .iter()
        .any(|rgb| rgb.chunks(3).all(|p| p == &rgb[..3]))
}

/// For finding out why a live run made a place: with `VIVY_GB_SIGHT_TRACE` set to a
/// folder, each new place's screen is saved there with what led to it.
#[cfg(test)]
fn trace_new_place(
    machine: &mut Machine,
    sight: &Sight,
    stepped: Option<(Dir, Outcome)>,
    dark: bool,
) {
    let Ok(folder) = std::env::var("VIVY_GB_SIGHT_TRACE") else {
        return;
    };
    let name = format!("{}/{}", folder, sight.live.area.replace(' ', "-"));
    if let Ok(png) = super::pictures::png_at(&machine.emulator.screen(), 2) {
        let _ = std::fs::write(format!("{name}.png"), png);
    }
    if let Ok(state) = machine.emulator.save_state() {
        let _ = std::fs::write(format!("{name}.state"), state);
    }
    let _ = std::fs::write(
        format!("{name}.txt"),
        format!(
            "step {stepped:?}, dark {dark}, frames {}, at {:?}\n",
            machine.emulator.frames(),
            sight.live.at
        ),
    );
}

/// The screen, in whole tiles of the map if `steady`.
fn screen_of(video: &super::emulator::Video, steady: bool) -> Screen {
    if steady {
        Screen::read_steady(video)
    } else {
        Screen::read(video)
    }
}

/// The screen as `calm` compares it.
fn calm_of(screen: &Screen) -> Calm {
    Calm {
        scroll: (screen.registers.scx, screen.registers.scy),
        tiles: screen.cells.iter().map(|c| (c.index, c.key)).collect(),
    }
}

/// Whether the screen is the same a moment later: still a picture, her sprite on the same
/// step, and no more than a few tiles changed.
fn holds_still(machine: &mut Machine, now: &Calm, steps: Steps, spot: Option<(i32, i32)>) -> bool {
    machine.run(STILL_FRAMES);
    let video = machine.emulator.video().clone();
    let rgb = machine.emulator.screen();
    let screen = screen_of(&video, spot.is_some());
    if frame_look(video.drawn, &video.registers, &rgb, video.color) != FrameLook::Picture
        || !screen.drawn
        || !screen.aligned
    {
        return false;
    }
    let again = calm_of(&screen);
    steps_of(&screen, spot).is_some_and(|s| s.player == steps.player && s.origin == steps.origin)
        && again.scroll == now.scroll
        && changed_since(now, &again, (0, 0)) <= CALM_CHANGES
}

/// How many tiles differ from the last calm screen, moved by `by` cells, where both have
/// one. A tile is the same if its number or its picture is: tiles whose pictures move
/// keep their numbers, and a map drawn again elsewhere keeps its pictures.
fn changed_since(last: &Calm, now: &Calm, by: (i32, i32)) -> usize {
    let (cols, rows) = (COLS as i32, ROWS as i32);
    let mut changed = 0;
    for row in 0..rows {
        for col in 0..cols {
            let (c, r) = (col + by.0, row + by.1);
            if !(0..cols).contains(&c) || !(0..rows).contains(&r) {
                continue;
            }
            let (a, b) = (
                now.tiles[(row * cols + col) as usize],
                last.tiles[(r * cols + c) as usize],
            );
            if a.0 != b.0 && a.1 != b.1 {
                changed += 1;
            }
        }
    }
    changed
}

/// How the screen's steps sit, from the scroll and her sprite: `None` when no sprite
/// stands in the middle of the screen (or at her spot, in a game where she walks across
/// it).
fn steps_of(screen: &Screen, spot: Option<(i32, i32)>) -> Option<Steps> {
    let r = screen.registers;
    let origin = (
        (STEP - i32::from(r.scx) % STEP) % STEP,
        (STEP - i32::from(r.scy) % STEP) % STEP,
    );
    // Her sprite is the one nearest the middle: just left of it and a little up, where
    // games with an even number of steps across put the player.
    let at = spot.unwrap_or(MIDDLE);
    let nearest = screen
        .sprites
        .iter()
        .map(|s| (s.x + 4, s.y + s.height / 2))
        .min_by_key(|&(x, y)| (x - at.0).abs() + (y - at.1).abs())
        .filter(|&(x, y)| (x - at.0).abs() + (y - at.1).abs() <= HER_NEAR)?;
    // Her spot is the middle of all her sprites; in the middle of the screen, the sprite
    // nearest it.
    let centre = if spot.is_some() { at } else { nearest };
    Some(Steps {
        origin,
        across: (COLS as i32 * 8 - origin.0) / STEP,
        down: (ROWS as i32 * 8 - origin.1) / STEP,
        player: (
            (centre.0 - origin.0).div_euclid(STEP),
            (centre.1 - origin.1).div_euclid(STEP),
        ),
    })
}

/// Each step's picture (its four tiles' fingerprints), by steps from her. Steps in the
/// window (a status bar) are left out.
fn pictures_around(screen: &Screen, steps: Steps) -> HashMap<Pos, u64> {
    let mut found = HashMap::new();
    for j in 0..steps.down {
        for i in 0..steps.across {
            let col = ((steps.origin.0 + i * STEP - screen.phase.0) / 8) as usize;
            let row = ((steps.origin.1 + j * STEP - screen.phase.1) / 8) as usize;
            let cells = [
                screen.cell(col, row),
                screen.cell(col + 1, row),
                screen.cell(col, row + 1),
                screen.cell(col + 1, row + 1),
            ];
            if cells.iter().any(|c| c.layer != Layer::Background) {
                continue;
            }
            let key = join_keys(cells.map(|c| c.key));
            found.insert((i - steps.player.0, j - steps.player.1), key);
        }
    }
    found
}

/// Each step's four tile numbers, by steps from her, like `pictures_around`.
fn numbers_around(screen: &Screen, steps: Steps) -> HashMap<Pos, [u8; 4]> {
    let mut found = HashMap::new();
    for j in 0..steps.down {
        for i in 0..steps.across {
            let col = ((steps.origin.0 + i * STEP - screen.phase.0) / 8) as usize;
            let row = ((steps.origin.1 + j * STEP - screen.phase.1) / 8) as usize;
            let numbers = [
                screen.cell(col, row).index,
                screen.cell(col + 1, row).index,
                screen.cell(col, row + 1).index,
                screen.cell(col + 1, row + 1).index,
            ];
            found.insert((i - steps.player.0, j - steps.player.1), numbers);
        }
    }
    found
}

/// The screen's steps where they are in the area, with her at `at`.
fn seen_steps(screen: &Screen, at: Pos, spot: Option<(i32, i32)>) -> Vec<(Pos, u64)> {
    let Some(steps) = steps_of(screen, spot) else {
        return Vec::new();
    };
    pictures_around(screen, steps)
        .into_iter()
        .filter(|&(offset, _)| offset != (0, 0))
        .map(|((dx, dy), key)| ((at.0 + dx, at.1 + dy), key))
        .collect()
}

/// Where the other people stand, by steps from her, from the sprites away from hers (a
/// sprite that walks across the screen is drawn in parts that can sit on two steps).
fn people(screen: &Screen, steps: Steps, spot: Option<(i32, i32)>) -> Vec<Pos> {
    let mut found: Vec<Pos> = screen
        .sprites
        .iter()
        .filter(|s| {
            spot.is_none_or(|at| {
                (s.x + 4 - at.0).abs() + (s.y + s.height / 2 - at.1).abs() > HER_NEAR
            })
        })
        .map(|s| {
            (
                (s.x + 4 - steps.origin.0).div_euclid(STEP) - steps.player.0,
                (s.y + s.height / 2 - steps.origin.1).div_euclid(STEP) - steps.player.1,
            )
        })
        .filter(|&step| step != (0, 0))
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// One fingerprint for four tiles' fingerprints (FNV-1a over their bytes).
fn join_keys(keys: [u64; 4]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for key in keys {
        for byte in key.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn her_own_presses_count_steps_across_the_screen_and_slides() {
        // Two steps right on a still screen, from the middle of a step.
        assert_eq!(
            pressed_steps(((40, 72), (0, 0)), ((72, 72), (0, 0))),
            (2, 0)
        );
        // A few pixels within one step: none.
        assert_eq!(
            pressed_steps(((40, 72), (0, 0)), ((46, 72), (0, 0))),
            (0, 0)
        );
        // Over the edge of a step: one.
        assert_eq!(
            pressed_steps(((44, 72), (0, 0)), ((50, 72), (0, 0))),
            (1, 0)
        );
        // The screen slides a room right (160 pixels) and her sprite goes from the right
        // edge to the left: a step on, not eight back.
        assert_eq!(
            pressed_steps(((150, 72), (0, 0)), ((14, 72), (160, 0))),
            (1, 0)
        );
        // And back left, the scroll wrapping past 0.
        assert_eq!(
            pressed_steps(((14, 72), (160, 0)), ((150, 72), (0, 0))),
            (-1, 0)
        );
        // A camera that follows her a little: the scroll counts with her sprite.
        assert_eq!(
            pressed_steps(((80, 72), (250, 0)), ((88, 72), (10, 0))),
            (2, 0)
        );
    }

    #[test]
    fn rooms_alike_but_for_their_walls_are_not_one_place() {
        let mut sight = Sight::default();
        // Two 8×8 rooms with the same floor at the same steps; only the walls and things
        // along the top differ.
        let room = |wall: u64| -> Vec<(Pos, u64)> {
            (0..64)
                .map(|i| {
                    let pos = (i % 8, i / 8);
                    (pos, if pos.1 < 2 { wall + i as u64 } else { 1 })
                })
                .collect()
        };
        sight.pictures.insert(
            "Place 1".to_string(),
            Pictures(room(100).into_iter().collect()),
        );
        assert_eq!(sight.known_place(&room(500), "Place 2"), None);
        assert_eq!(
            sight.known_place(&room(100), "Place 2"),
            Some("Place 1".to_string())
        );
    }

    #[test]
    fn pictures_that_move_dont_make_a_place_look_different() {
        let mut sight = Sight::default();
        let room: Vec<(Pos, u64)> = (0..20).map(|i| ((i % 5, i / 5), 100 + i as u64)).collect();
        sight.pictures.insert(
            "Place 1".to_string(),
            Pictures(room.iter().copied().collect()),
        );
        // Flowers on four of its steps showed another picture this time.
        let seen: Vec<(Pos, u64)> = room
            .iter()
            .map(|&(pos, key)| {
                (
                    pos,
                    if pos.1 == 0 && pos.0 < 4 {
                        key + 1000
                    } else {
                        key
                    },
                )
            })
            .collect();
        assert_eq!(
            sight.known_place(&sight.steady(seen.clone()), "Place 2"),
            None
        );
        sight.lively.extend((100..104).chain(1100..1104));
        assert_eq!(
            sight.known_place(&sight.steady(seen), "Place 2"),
            Some("Place 1".to_string())
        );
    }

    /// Red's bedroom, with the trainer card opened and closed between looks while she
    /// stands still: its pictures load into the map's tiles and back, which isn't the
    /// room moving, so nothing in it is learned as moving.
    #[test]
    #[ignore = "needs a ROM in VIVY_GB_ROM"]
    fn a_menu_s_pictures_are_not_the_room_moving() {
        use super::super::console::Timing;
        use super::super::emulator::Button;
        use super::super::presses::Press;
        let mut m = super::super::red::tests::new_game();
        let mut world = World::default();
        let press = |m: &mut Machine, button: Button| {
            m.press(&[Press(vec![button])], Timing { hold: 4, gap: 30 });
        };
        for button in [Button::Left, Button::Right, Button::Left, Button::Right] {
            press(&mut m, button);
            m.run(30);
            look(&mut world, &mut m);
        }
        for _ in 0..3 {
            for button in [Button::Start, Button::Down, Button::A] {
                press(&mut m, button);
                look(&mut world, &mut m);
            }
            for _ in 0..3 {
                press(&mut m, Button::B);
                m.run(20);
                look(&mut world, &mut m);
            }
            for _ in 0..4 {
                m.run(40);
                assert!(look(&mut world, &mut m).can_walk);
            }
        }
        assert!(world.sight.lively.is_empty(), "{:?}", world.sight.lively);
    }

    #[test]
    fn a_picture_is_what_trying_it_did() {
        let mut learned = Learned::default();
        assert_eq!(learned.kind(), None);
        learned.blocked = 2;
        assert_eq!(learned.kind(), Some(Kind::Wall));
        learned.walked = 1;
        assert_eq!(learned.kind(), Some(Kind::Floor), "walking onto one wins");
        learned.doors = 1;
        assert_eq!(learned.kind(), Some(Kind::Door));
        // Walked onto far more often than it went somewhere: ground at an edge.
        learned.walked = 40;
        assert_eq!(learned.kind(), Some(Kind::Floor));
        learned.carried = 41;
        assert_eq!(learned.kind(), Some(Kind::Mover));
        learned.over = Some(Dir::Down);
        assert_eq!(learned.kind(), Some(Kind::OneWay(Dir::Down)));
    }

    #[test]
    fn four_tiles_make_one_key_and_order_matters() {
        let a = join_keys([1, 2, 3, 4]);
        assert_eq!(a, join_keys([1, 2, 3, 4]));
        assert_ne!(a, join_keys([2, 1, 3, 4]));
    }

    #[test]
    fn a_place_is_known_again_by_its_look() {
        let mut sight = Sight::default();
        let room: Vec<(Pos, u64)> = (0..20).map(|i| ((i % 5, i / 5), i as u64)).collect();
        sight.pictures.insert(
            "Place 1".to_string(),
            Pictures(room.iter().copied().collect()),
        );
        assert_eq!(sight.known_place(&room, ""), Some("Place 1".to_string()));
        let place = &sight.pictures["Place 1"];
        assert!(!looks_different(place, &room));
        // A few steps different (flowers that move): still the same place.
        let moved: Vec<(Pos, u64)> = room
            .iter()
            .map(|&(p, k)| (p, if k < 4 { k + 100 } else { k }))
            .collect();
        assert_eq!(sight.known_place(&moved, ""), None);
        assert!(!looks_different(place, &moved));
        // Most steps different: somewhere else.
        let other: Vec<(Pos, u64)> = room.iter().map(|&(p, k)| (p, k + 100)).collect();
        assert_eq!(sight.known_place(&other, ""), None);
        assert!(looks_different(place, &other));
        // Too little seen before to tell.
        assert_eq!(sight.known_place(&room[..5], ""), None);
        assert!(!looks_different(place, &room[..5]));
        // Not the place she just left, however alike.
        assert_eq!(sight.known_place(&room, "Place 1"), None);
    }

    #[test]
    fn sight_saves_and_loads() {
        let mut sight = Sight::default();
        sight.steps.insert(
            7,
            Learned {
                walked: 1,
                ..Learned::default()
            },
        );
        sight.found.x = Some(Byte {
            address: 0xd362,
            per_step: 1,
        });
        sight
            .pictures
            .insert("Area 1".to_string(), Pictures(HashMap::from([((1, 2), 9)])));
        let json = serde_json::to_string(&sight).unwrap();
        let back: Sight = serde_json::from_str(&json).unwrap();
        assert_eq!(back.steps[&7].kind(), Some(Kind::Floor));
        assert_eq!(back.found, sight.found);
        assert_eq!(back.pictures["Area 1"].0[&(1, 2)], 9);
    }
}
