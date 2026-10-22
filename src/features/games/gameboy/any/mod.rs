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
use super::map::{AreaMap, Kind, Pos, as_list};
use super::moves::{Outcome, offset};
use super::screen::{COLS, Layer, ROWS, Screen};
use super::walk::settle;
use super::world::{Grid, Stepped, View, World};
use memory::{Areas, Byte, Finder, Memory, Position};

/// Pixels in a step.
const STEP: i32 = 16;

/// The screen pixel her sprite's middle is nearest.
const MIDDLE: (i32, i32) = (72, 68);

/// Tiles that may change with no scroll while she can still walk (a clock, a counter).
/// Tile numbers are compared, so tiles whose pictures move (water, flowers) don't count.
const CALM_CHANGES: usize = 8;

/// To know a place again by its look: steps of the screen that must have been seen there
/// before, and how many of them must look the same.
const KNOWN_AFTER: usize = 12;
const KNOWN_SHARE: f32 = 0.9;

/// A place is another one when, of at least this many different pictures on screen, less
/// than this share were ever seen there.
const OTHER_KINDS: usize = 6;
const OTHER_SHARE: f32 = 0.25;

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
    #[serde(skip)]
    live: Live,
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
}

impl Learned {
    /// What a step with this picture is, once she has tried one.
    pub fn kind(self) -> Option<Kind> {
        if self.doors > 0 {
            Some(Kind::Door)
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

    fn position(&self, memory: &Memory) -> Option<Pos> {
        self.live
            .finder
            .as_ref()
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

    /// The place these steps look like, other than the one she just left (Red's stairs
    /// lead to a room that looks much like the one above: she isn't where she was).
    fn known_place(&self, seen: &[(Pos, u64)], left: &str) -> Option<String> {
        self.pictures
            .iter()
            .filter(|(name, _)| *name != left)
            .filter_map(|(name, pictures)| {
                let (same, both) = compare(pictures, seen);
                (both >= KNOWN_AFTER && same as f32 >= both as f32 * KNOWN_SHARE)
                    .then(|| (same, name.clone()))
            })
            .max()
            .map(|(_, name)| name)
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
                let seen: Vec<(Pos, u64)> = pb.0.iter().map(|(&pos, &key)| (pos, key)).collect();
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
    let mut screen = Screen::read(machine.emulator.video());
    let mut memory = Memory::read(&mut machine.emulator);

    if sight.live.memory.is_none() {
        // The first look: wherever she is gets a name, kept until memory gives one.
        if sight.live.area.is_empty() {
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
                screen = Screen::read(machine.emulator.video());
                memory = Memory::read(&mut machine.emulator);
            }
        }
    }
    let free = calm(sight, machine, &screen, dark, stepped.is_some());
    let at = sight.position(&memory);
    let seen = at.map(|at| seen_steps(&screen, at)).unwrap_or_default();
    // Where she left the last place: the step she went into, and whether it was dark.
    let mut left = None;
    if let Some((step, before)) = stepped {
        let pressed = offset(step.direction.button()).unwrap_or((0, 0));
        let from = sight.live.at;
        if after_step(sight, &mut world.areas, step, &before, &memory, dark, free) {
            left = Some(((from.0 + pressed.0, from.1 + pressed.1), dark));
        }
    } else if let Some(at) = at {
        // No step, but the game may have moved her: a scene, or a checkpoint loaded. If
        // the screen looks unlike where she last walked, it's somewhere else (a little
        // unlike says nothing: flowers and water move).
        if free
            && sight.live.free.is_some()
            && sight.live.left.is_none()
            && sight
                .pictures
                .get(&sight.live.area)
                .is_some_and(|p| looks_different(p, &seen))
        {
            left = Some((sight.live.at, true));
        }
        sight.live.at = at;
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
            arrive(sight, &mut world.areas, &memory, &seen, gone, dark);
        }
    }
    sight.name_places(&mut world.areas);
    if let Some((x, y)) = sight.live.finder.as_ref().and_then(|f| f.position.found()) {
        sight.found.x = Some(x);
        sight.found.y = Some(y);
    }

    let area = sight.live.area.clone();
    let at = sight.live.at;
    let steps = steps_of(&screen).filter(|_| free);
    if let Some(steps) = steps {
        let around = pictures_around(&screen, steps);
        sight.live.people = people(&screen, steps);
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
    sight.live.memory = Some(memory);
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
    let dead_reckoned = (
        sight.live.at.0 + step.scrolled.0,
        sight.live.at.1 + step.scrolled.1,
    );
    let jumped = sight
        .finder()
        .stepped(before, after, pressed, step.scrolled, dark);
    let now = sight.position(after);
    // Memory put her somewhere the step couldn't have, or the step went dark and left her
    // free to walk: a door, even to the same numbers (Red's stairs land on the same spot
    // a floor down, in a room much like the one above). The swirl into a battle goes
    // dark too, but she isn't free after it.
    let new_place = jumped == Some(true) || (dark && free);

    // What the step ahead turned out to be, by its picture.
    let ahead = |n: i32| (pressed.0 * n, pressed.1 * n);
    match step.outcome {
        Outcome::Moved(n) => {
            if let Some(&key) = sight.live.around.get(&ahead(n)) {
                let learned = sight.steps.entry(key).or_default();
                if new_place {
                    learned.doors += 1;
                } else {
                    learned.walked += 1;
                }
            }
        }
        Outcome::Blocked if !sight.live.people.contains(&ahead(1)) => {
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
    let old_area = sight.live.area.clone();
    let led = areas
        .get(&old_area)
        .and_then(|map| map.doors.get(&gone))
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
        .cloned();
    let name = match sight.area_byte() {
        Some(byte) => sight.place_by(byte, memory),
        None => led.or_else(|| sight.known_place(seen, &old_area)),
    }
    .unwrap_or_else(|| sight.new_place());
    let map = areas.entry(old_area).or_default();
    map.see(gone, if dark { Kind::Door } else { Kind::Edge });
    map.doors.insert(gone, name.clone());
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
    let Some(steps) = steps_of(screen) else {
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
    let now = Calm {
        scroll: (r.scx, r.scy),
        tiles: screen.cells.iter().map(|c| (c.index, c.key)).collect(),
    };
    if dark {
        sight.live.calm = None;
    }
    let Some(last) = &sight.live.calm else {
        sight.live.calm = Some(now);
        return true;
    };
    let moved = |a: u8, b: u8| i32::from(b.wrapping_sub(a) as i8) / 8;
    let by = (
        moved(last.scroll.0, now.scroll.0),
        moved(last.scroll.1, now.scroll.1),
    );
    let free = if by != (0, 0) && !stepped {
        false
    } else {
        // Scrolled by a step or two, the tiles still on screen have moved with it; a text
        // box that came up as the step ended hasn't.
        let near = by.0.abs() <= 4 && by.1.abs() <= 4;
        !near || changed_since(last, &now, by) <= CALM_CHANGES
    };
    if free || by != (0, 0) {
        sight.live.calm = Some(now);
    }
    free
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
/// stands in the middle of the screen.
fn steps_of(screen: &Screen) -> Option<Steps> {
    let r = screen.registers;
    let origin = (
        (STEP - i32::from(r.scx) % STEP) % STEP,
        (STEP - i32::from(r.scy) % STEP) % STEP,
    );
    // Her sprite is the one nearest the middle: just left of it and a little up, where
    // games with an even number of steps across put the player.
    let centre = screen
        .sprites
        .iter()
        .map(|s| (s.x + 4, s.y + s.height / 2))
        .min_by_key(|&(x, y)| (x - MIDDLE.0).abs() + (y - MIDDLE.1).abs())
        .filter(|&(x, y)| (x - MIDDLE.0).abs() + (y - MIDDLE.1).abs() <= 12)?;
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
            let col = ((steps.origin.0 + i * STEP) / 8) as usize;
            let row = ((steps.origin.1 + j * STEP) / 8) as usize;
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

/// The screen's steps where they are in the area, with her at `at`.
fn seen_steps(screen: &Screen, at: Pos) -> Vec<(Pos, u64)> {
    let Some(steps) = steps_of(screen) else {
        return Vec::new();
    };
    pictures_around(screen, steps)
        .into_iter()
        .filter(|&(offset, _)| offset != (0, 0))
        .map(|((dx, dy), key)| ((at.0 + dx, at.1 + dy), key))
        .collect()
}

/// Where the other people stand, by steps from her, from the sprites away from hers.
fn people(screen: &Screen, steps: Steps) -> Vec<Pos> {
    let mut found: Vec<Pos> = screen
        .sprites
        .iter()
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
    fn a_picture_is_what_trying_it_did() {
        let mut learned = Learned::default();
        assert_eq!(learned.kind(), None);
        learned.blocked = 2;
        assert_eq!(learned.kind(), Some(Kind::Wall));
        learned.walked = 1;
        assert_eq!(learned.kind(), Some(Kind::Floor), "walking onto one wins");
        learned.doors = 1;
        assert_eq!(learned.kind(), Some(Kind::Door));
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
