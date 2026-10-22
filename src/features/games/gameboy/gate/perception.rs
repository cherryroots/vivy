//! Perception with no memory map, measured where the answers are known: on Pokémon Red,
//! the cheat finder (`any/memory.rs`) watches Red's opening played by script and is
//! checked against Red's own addresses (wXCoord, wYCoord, wCurMap), and the look
//! (`any/`) is checked against Red's own look; on the test game (`test_game.rs`), the
//! look explores rooms it was never told about.

use std::fmt::Write as _;
use std::sync::Mutex;

use super::results;
use super::tasks::{Driver, play_to_the_parcel};
use crate::features::games::gameboy::any::memory::{Byte, Finder, Memory};
use crate::features::games::gameboy::console::Machine;
use crate::features::games::gameboy::emulator::RomInfo;
use crate::features::games::gameboy::map::Kind;
use crate::features::games::gameboy::moves::{Outcome, offset};
use crate::features::games::gameboy::red::{self, State};
use crate::features::games::gameboy::test_game;
use crate::features::games::gameboy::walk::{Walked, explore, look_for, walk_to};
use crate::features::games::gameboy::world::{Stepped, View, World};

/// Red's own addresses, from the disassembly.
const RED_X: u16 = 0xd362;
const RED_Y: u16 = 0xd361;
const RED_MAP: u16 = 0xd35e;

/// One look while the script played: memory, the step just taken (if the look came right
/// after one), and the truth from Red's memory map.
struct Looked {
    memory: Memory,
    stepped: Option<Stepped>,
    map: u8,
    at: (i32, i32),
    area: String,
}

static LOOKS: Mutex<Vec<Looked>> = Mutex::new(Vec::new());

/// Red's own look, keeping a copy of memory each time.
fn spy(world: &mut World, machine: &mut Machine) -> View {
    let stepped = world.stepped;
    let view = red::look(world, machine);
    let state = State::read(&mut machine.emulator);
    LOOKS.lock().unwrap().push(Looked {
        memory: Memory::read(&mut machine.emulator),
        stepped,
        map: state.map,
        at: (i32::from(state.x), i32::from(state.y)),
        area: view.area.clone(),
    });
    view
}

#[derive(Default)]
struct Measured {
    steps: u32,
    /// The step x and y were first found at, and the area.
    position_at: Option<u32>,
    area_at: Option<u32>,
    /// Steps after the position was found, and how many memory put her where Red does
    /// (allowing for a copy kept with an offset: the same offset within each area).
    after_found: u32,
    right_place: u32,
    /// New places: Red's map changed and the finder said so, said so wrongly, or missed it.
    new_right: u32,
    new_wrong: u32,
    new_missed: u32,
    /// Once the area byte was found: looks where it named an area Red names differently
    /// than before (two Red maps under one value, or one map under two).
    area_mixups: u32,
    lines: Vec<String>,
}

fn measure(looks: &[Looked]) -> (Finder, Measured) {
    let mut finder = Finder::default();
    let mut m = Measured::default();
    let mut offsets = std::collections::HashMap::new();
    let mut names = std::collections::HashMap::<u8, u8>::new();
    for pair in looks.windows(2) {
        let (before, after) = (&pair[0], &pair[1]);
        let Some(stepped) = after.stepped else {
            continue;
        };
        m.steps += 1;
        let pressed = offset(stepped.direction.button()).unwrap_or((0, 0));
        let dark = stepped.outcome == Outcome::NewArea;
        let new_place = finder
            .stepped(
                &before.memory,
                &after.memory,
                pressed,
                stepped.scrolled,
                dark,
            )
            .unwrap_or(dark);
        finder.place(&before.memory, &after.memory, new_place);
        let found = finder.position.found().is_some();
        if found && m.position_at.is_none() {
            m.position_at = Some(m.steps);
            m.lines.push(format!(
                "Step {}: position found, {:?} (guesses left {:?}), in {}",
                m.steps,
                finder.position.found().unwrap(),
                finder.position.left(),
                after.area
            ));
        }
        if let Some(area) = finder.areas.found()
            && m.area_at.is_none()
        {
            m.area_at = Some(m.steps);
            m.lines.push(format!(
                "Step {}: area found at {area:04X} ({} guesses left), after {} changes",
                m.steps,
                finder.areas.left(),
                finder.areas.changes
            ));
        }
        let truly_new = before.map != after.map;
        if found {
            match (new_place, truly_new) {
                (true, true) => m.new_right += 1,
                (true, false) => {
                    m.new_wrong += 1;
                    m.lines.push(format!(
                        "Step {}: called new wrongly, {} {:?} -> {:?} {:?}",
                        m.steps, after.area, before.at, after.at, stepped
                    ));
                }
                (false, true) => {
                    m.new_missed += 1;
                    m.lines.push(format!(
                        "Step {}: missed {} -> {} ({:?})",
                        m.steps, before.area, after.area, stepped
                    ));
                }
                (false, false) => {}
            }
            m.after_found += 1;
            let at = finder.position.at(&after.memory).unwrap();
            let offset = (at.0 - after.at.0, at.1 - after.at.1);
            if *offsets.entry(after.map).or_insert(offset) == offset {
                m.right_place += 1;
            }
        }
        if let Some(area) = finder.areas.found() {
            let value = after.memory.get(area);
            if *names.entry(value).or_insert(after.map) != after.map {
                m.area_mixups += 1;
            }
        }
    }
    (finder, m)
}

/// Plays Red's opening by script to Oak's Parcel, with the cheat finder watching, and
/// checks it finds Red's own addresses. The report goes in the project files.
#[test]
#[ignore = "needs a ROM in VIVY_GB_ROM"]
fn the_cheat_finder_finds_reds_own_addresses() {
    LOOKS.lock().unwrap().clear();
    let mut d = Driver::new(red::tests::new_game());
    d.look = spy;
    let mut reached = Vec::new();
    play_to_the_parcel(&mut d, |_, name| {
        reached.push((name.to_string(), LOOKS.lock().unwrap().len()));
    });
    let looks = std::mem::take(&mut *LOOKS.lock().unwrap());
    let (finder, m) = measure(&looks);

    let mut report = String::from("# Cheat finder on Pokémon Red\n\n");
    writeln!(
        report,
        "Red's opening by script, from her bedroom to Oak's Parcel: {} looks, {} steps.\n",
        looks.len(),
        m.steps
    )
    .unwrap();
    let (x, y) = finder.position.found().expect("x and y found");
    let area = finder.areas.found();
    writeln!(
        report,
        "- x: {:04X} ({} per step), Red's wXCoord is {RED_X:04X}, still a guess: {}",
        x.address,
        x.per_step,
        finder.position.holds(RED_X).0
    )
    .unwrap();
    writeln!(
        report,
        "- y: {:04X} ({} per step), Red's wYCoord is {RED_Y:04X}, still a guess: {}",
        y.address,
        y.per_step,
        finder.position.holds(RED_Y).1
    )
    .unwrap();
    writeln!(
        report,
        "- area: {}, Red's wCurMap is {RED_MAP:04X}, still a guess: {}",
        area.map_or("not found".to_string(), |a| format!("{a:04X}")),
        finder.areas.holds(RED_MAP)
    )
    .unwrap();
    writeln!(
        report,
        "- guesses left: x {}, y {}, area {}",
        finder.position.left().0,
        finder.position.left().1,
        finder.areas.left()
    )
    .unwrap();
    writeln!(
        report,
        "- found after: position step {:?}, area step {:?}",
        m.position_at, m.area_at
    )
    .unwrap();
    writeln!(
        report,
        "- once found, memory put her in the right place {} of {} steps",
        m.right_place, m.after_found
    )
    .unwrap();
    writeln!(
        report,
        "- new places: {} right, {} wrongly, {} missed",
        m.new_right, m.new_wrong, m.new_missed
    )
    .unwrap();
    writeln!(report, "- area mix-ups once found: {}", m.area_mixups).unwrap();
    writeln!(report, "\n## Along the way\n").unwrap();
    for line in &m.lines {
        writeln!(report, "- {line}").unwrap();
    }
    writeln!(report, "\nTask starts (look number): {reached:?}").unwrap();
    let folder = results("perception");
    std::fs::write(folder.join("report.md"), &report).unwrap();
    println!("{report}");

    assert!(finder.position.holds(RED_X).0, "{report}");
    assert!(finder.position.holds(RED_Y).1, "{report}");
    assert_eq!(m.right_place, m.after_found, "{report}");
    // Memory alone can't tell the area's byte from others that change with it (the
    // people loaded, the map's blocks): the look tells them apart by the pictures, in
    // the next test. Here the area's byte must only never be dropped.
    assert!(finder.areas.holds(RED_MAP), "{report}");
}

/// The look with no memory map, on a world of its own, beside Red's.
static SHADOW: Mutex<Option<World>> = Mutex::new(None);

#[derive(Default)]
struct Compared {
    looks: u32,
    /// Looks where both could walk, where only Red's could, where only the other could.
    both_walk: u32,
    only_red: u32,
    only_any: u32,
    /// Looks where both could walk and the position was found: in the right place.
    placed: u32,
    right_place: u32,
    /// Names the look gave to each of Red's maps, and the other way round.
    names: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    last_names: Option<(String, String)>,
    lines: Vec<String>,
}

static COMPARED: Mutex<Option<Compared>> = Mutex::new(None);

/// Looks with no memory map first (it may wait for a fade), then with Red's, and keeps
/// how the two compare. The script plays on Red's look.
fn both(world: &mut World, machine: &mut Machine) -> View {
    let mut shadow = SHADOW.lock().unwrap();
    let shadow = shadow.get_or_insert_with(World::default);
    shadow.stepped = world.stepped;
    let mut any = crate::features::games::gameboy::any::look(shadow, machine);
    // A look again a little later with no step between, as she often does: the screen
    // has moved on (tiles that animate, like flowers and water), but she hasn't.
    if shadow.stepped.take().is_some() {
        machine.run(37);
        any = crate::features::games::gameboy::any::look(shadow, machine);
    }
    let red = red::look(world, machine);
    let mut compared = COMPARED.lock().unwrap();
    let c = compared.get_or_insert_with(Compared::default);
    c.looks += 1;
    match (red.can_walk, any.can_walk) {
        (true, true) => c.both_walk += 1,
        (true, false) => {
            c.only_red += 1;
            if c.lines.len() < 200 {
                c.lines.push(format!(
                    "look {}: only Red's can walk, in {}",
                    c.looks, red.area
                ));
            }
        }
        (false, true) => {
            c.only_any += 1;
            if c.lines.len() < 200 {
                c.lines.push(format!(
                    "look {}: only the other can walk, in {} ({:?})",
                    c.looks,
                    red.area,
                    red.text.lines().next()
                ));
            }
        }
        _ => {}
    }
    if red.can_walk && any.can_walk && shadow.sight.found.x.is_some() {
        c.placed += 1;
        if red.at == any.at {
            c.right_place += 1;
        } else if c.lines.len() < 200 {
            c.lines.push(format!(
                "look {}: at {:?}, Red says {:?} in {}",
                c.looks, any.at, red.at, red.area
            ));
        }
    }
    if c.last_names.as_ref() != Some(&(red.area.clone(), any.area.clone())) {
        c.lines.push(format!(
            "look {}: now {} for Red's {} at {:?} (Red {:?}), can walk {}/{}",
            c.looks, any.area, red.area, any.at, red.at, any.can_walk, red.can_walk
        ));
        c.last_names = Some((red.area.clone(), any.area.clone()));
    }
    if red.can_walk && any.can_walk {
        c.names
            .entry(red.area.clone())
            .or_default()
            .insert(any.area.clone());
    }
    red
}

/// Plays Red's opening by script with the look that has no memory map watching too, and
/// compares the two: whether she can walk, where she is, the names of places, and the
/// maps they draw. The report goes in the project files.
#[test]
#[ignore = "needs a ROM in VIVY_GB_ROM"]
fn the_look_with_no_map_agrees_with_reds() {
    *SHADOW.lock().unwrap() = None;
    *COMPARED.lock().unwrap() = None;
    let mut d = Driver::new(red::tests::new_game());
    d.look = both;
    play_to_the_parcel(&mut d, |_, _| {});
    let shadow = SHADOW.lock().unwrap().take().unwrap();
    let c = COMPARED.lock().unwrap().take().unwrap();

    let mut report = String::from("# The look with no memory map, beside Red's\n\n");
    writeln!(
        report,
        "Red's opening by script, from her bedroom to Oak's Parcel: {} looks.\n",
        c.looks
    )
    .unwrap();
    writeln!(report, "- found: {:?}", shadow.sight.found).unwrap();
    writeln!(
        report,
        "- can walk: both {}, only Red's {}, only the other {}",
        c.both_walk, c.only_red, c.only_any
    )
    .unwrap();
    writeln!(
        report,
        "- right place once found: {} of {}",
        c.right_place, c.placed
    )
    .unwrap();
    writeln!(report, "\n## Names for Red's places\n").unwrap();
    for (red_name, names) in &c.names {
        writeln!(report, "- {red_name}: {names:?}").unwrap();
    }
    // The places at the end, after any renaming, by Red's map number as she came in.
    writeln!(report, "\n## Places at the end\n").unwrap();
    for (name, memory) in shadow.sight.places() {
        let map = memory.get(RED_MAP);
        writeln!(report, "- {name}: came in to {}", red::map_name(map)).unwrap();
    }
    // The maps: every step both have seen, compared by whether it can be walked on.
    writeln!(report, "\n## Maps\n").unwrap();
    let (mut same, mut differ, mut unseen) = (0, 0, 0);
    for (name, memory) in shadow.sight.places() {
        let red_name = red::map_name(memory.get(RED_MAP));
        let Some(red_map) = d.world.areas.get(&red_name) else {
            continue;
        };
        {
            let Some(map) = shadow.areas.get(&name) else {
                continue;
            };
            let Some(((x0, y0), (x1, y1))) = red_map.bounds() else {
                continue;
            };
            let (mut s, mut dif, mut u) = (0, 0, 0);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let Some(truth) = red_map.kind((x, y)) else {
                        continue;
                    };
                    match map.kind((x, y)) {
                        None => u += 1,
                        Some(kind) if kind.walkable() == truth.walkable() => s += 1,
                        Some(_) => dif += 1,
                    }
                }
            }
            writeln!(
                report,
                "- {red_name} as {name}: {s} steps agree, {dif} disagree, {u} not learned yet"
            )
            .unwrap();
            same += s;
            differ += dif;
            unseen += u;
        }
    }
    writeln!(
        report,
        "\nAll: {same} agree, {differ} disagree, {unseen} not learned yet. Pictures learned: {}.",
        shadow.sight.steps.len()
    )
    .unwrap();
    if let Some((name, map)) = shadow.areas.iter().max_by_key(|(_, m)| m.seen()) {
        let at = map.bounds().map(|(a, _)| a).unwrap_or((0, 0));
        writeln!(
            report,
            "\n### {name}, as drawn\n\n```\n{}```",
            map.draw(at, 40)
        )
        .unwrap();
    }
    if let Some(finder) = shadow.sight.finder_now() {
        writeln!(
            report,
            "\n## Area guesses\n\nRed's wCurMap {RED_MAP:04X}: still a guess {}, changed while walking {}. Best:\n",
            finder.areas.holds(RED_MAP),
            finder.areas.restless(RED_MAP)
        )
        .unwrap();
        for line in finder.areas.best(8) {
            writeln!(report, "- {line}").unwrap();
        }
    }
    writeln!(report, "\n## Along the way\n").unwrap();
    for line in &c.lines {
        writeln!(report, "- {line}").unwrap();
    }
    let folder = results("perception-look");
    std::fs::write(folder.join("report.md"), &report).unwrap();
    println!("{report}");
    assert_eq!(
        shadow.sight.found.x.map(|b| b.address),
        Some(RED_X),
        "{report}"
    );
    assert_eq!(
        shadow.sight.found.y.map(|b| b.address),
        Some(RED_Y),
        "{report}"
    );
    // One name for each of Red's places, and one of Red's places for each name.
    let places = shadow.sight.places();
    let maps: std::collections::HashSet<u8> = places.iter().map(|(_, m)| m.get(RED_MAP)).collect();
    assert_eq!(maps.len(), places.len(), "{report}");
    assert!(places.len() >= 7, "{report}");
}

/// The test game, explored with the look that has no memory map: through every door, and
/// back. It must find where the game keeps her x (in pixels), her y and the room, give
/// each room one name, and draw the rooms' walls where they are.
#[test]
fn the_look_with_no_map_learns_a_game_it_was_never_told_about() {
    let rom = test_game::build();
    let look = look_for(&RomInfo::read(&rom).unwrap());
    let mut m = Machine::new("rooms.gb", &rom).unwrap();
    m.run(120);
    let mut world = World::default();
    let (mut steps, mut doors) = (0, 0);
    // Through the doors in turn, the one gone through least first.
    let mut used: std::collections::HashMap<(String, (i32, i32)), u32> = Default::default();
    while steps < 900 && doors < 12 {
        let walk = explore(&mut world, &mut m, look, 900 - steps);
        steps += walk.steps.max(1);
        let area = walk.view.area.clone();
        match walk.result {
            Walked::NewArea(_) => doors += 1,
            Walked::NoWay => {
                let map = world.area(&area);
                let Some(&door) = map
                    .doors
                    .keys()
                    .min_by_key(|&&pos| (used.get(&(area.clone(), pos)).copied(), pos))
                else {
                    break;
                };
                *used.entry((area.clone(), door)).or_default() += 1;
                let walk = walk_to(&mut world, &mut m, look, door, 60);
                steps += walk.steps.max(1);
                if matches!(walk.result, Walked::NewArea(_)) {
                    doors += 1;
                }
            }
            _ => {}
        }
    }

    let found = world.sight.found;
    let mut report = format!(
        "{steps} steps, {doors} doors. Found x {:?}, y {:?}, area {:?}\n",
        found.x, found.y, found.area
    );
    let places = world.sight.places();
    let mut rooms = std::collections::HashSet::new();
    let (mut agree, mut differ) = (0, 0);
    for (name, memory) in &places {
        let number = memory.get(test_game::ROOM);
        rooms.insert(number);
        let room = test_game::ROOMS.iter().position(|r| r.0 == number).unwrap();
        let map = world.area(name);
        let (mut a, mut d) = (0, 0);
        for y in 0..12 {
            for x in 0..16 {
                let Some(kind) = map.kind((x, y)) else {
                    continue;
                };
                let truth = test_game::step_byte(room, (x as usize, y as usize));
                let right = match truth {
                    test_game::WALL => kind == Kind::Wall,
                    test_game::LEDGE => !kind.walkable(),
                    0x80.. => kind == Kind::Door,
                    _ => kind.walkable(),
                };
                if right {
                    a += 1;
                } else {
                    d += 1;
                }
            }
        }
        writeln!(
            report,
            "- {name}: room {number:02X}, {a} steps right, {d} wrong"
        )
        .unwrap();
        writeln!(report, "{}", map.draw((8, 6), 8)).unwrap();
        agree += a;
        differ += d;
    }
    println!("{report}");
    assert_eq!(
        found.x,
        Some(Byte {
            address: test_game::X_PIXELS,
            per_step: 16
        }),
        "{report}"
    );
    assert_eq!(
        found.y,
        Some(Byte {
            address: test_game::Y,
            per_step: 1
        }),
        "{report}"
    );
    assert_eq!(found.area, Some(test_game::ROOM), "{report}");
    assert_eq!(rooms.len(), 3, "{report}");
    assert_eq!(places.len(), 3, "one name for each room: {report}");
    assert!(agree > 150 && differ * 20 <= agree, "{report}");
}

/// The test game's ledge and floor arrows, with the look that has no memory map: one hop
/// down the ledge and one ride on the arrows teach every ledge and arrow step that looks
/// the same, and the map then hops down where it can and goes around on the way up.
#[test]
fn ledges_and_arrows_are_learned_by_trying_them() {
    use crate::features::games::gameboy::map::Dir::{Down, Right};
    use crate::features::games::gameboy::walk::take_step;
    let rom = test_game::build();
    let look = look_for(&RomInfo::read(&rom).unwrap());
    let mut m = Machine::new("rooms.gb", &rom).unwrap();
    m.run(120);
    let mut world = World::default();
    // Walk about first, so code knows where she stands, then through the first door.
    explore(&mut world, &mut m, look, 40);
    let walk = walk_to(&mut world, &mut m, look, (15, 4), 60);
    assert!(
        matches!(walk.result, Walked::NewArea(_)),
        "{:?}",
        walk.result
    );
    let room = walk.view.area.clone();
    assert_eq!(
        walk_to(&mut world, &mut m, look, (4, 6), 30).result,
        Walked::Arrived
    );
    let view = look(&mut world, &mut m);
    let (view, _) = take_step(&mut world, &mut m, look, &view, Down);
    assert_eq!(view.at, (4, 8), "hopped");
    let map = world.area(&room);
    assert_eq!(map.kind((4, 7)), Some(Kind::OneWay(Down)));
    assert_eq!(map.kind((6, 7)), Some(Kind::OneWay(Down)), "by its look");
    // Up again: around the end of the ledge, never over it.
    let up = map.path((5, 8), (5, 6), false).unwrap();
    assert!(up.len() > 2, "{up:?}");
    // Down from another spot above it: one press.
    assert_eq!(map.path((5, 6), (5, 8), false).map(|w| w.len()), Some(1));

    assert_eq!(
        walk_to(&mut world, &mut m, look, (2, 9), 30).result,
        Walked::Arrived
    );
    let view = look(&mut world, &mut m);
    let (view, _) = take_step(&mut world, &mut m, look, &view, Right);
    assert_eq!(view.at, (7, 9), "carried");
    assert_eq!(m.emulator.read(test_game::X_PIXELS), 7 * 16);
    let map = world.area(&room);
    assert_eq!(map.kind((3, 9)), Some(Kind::Mover));
    assert_eq!(map.kind((5, 9)), Some(Kind::Mover), "by its look");
    assert_eq!(map.step_to((2, 9), Right, false), Some((7, 9)));
    println!("{}", map.draw((5, 8), 8));
}

/// Walking by pressing a direction herself (not a step code took) scrolls the screen too,
/// and she's still free to walk after it: only the game moving her (a scene) isn't.
#[test]
fn a_step_she_presses_herself_leaves_her_free_to_walk() {
    use crate::features::games::gameboy::emulator::Button;
    let rom = test_game::build();
    let look = look_for(&RomInfo::read(&rom).unwrap());
    let mut m = Machine::new("rooms.gb", &rom).unwrap();
    m.run(120);
    let mut world = World::default();
    assert!(look(&mut world, &mut m).can_walk);
    m.emulator.hold(&[Button::Right]);
    m.run(4);
    m.emulator.release();
    m.run(30);
    assert_eq!(
        m.emulator.read(test_game::X_PIXELS),
        4 * 16,
        "she walked a step"
    );
    assert!(look(&mut world, &mut m).can_walk);
}

/// Red's stairs land on the same numbers a floor down, in a room much like the one above:
/// once her position is known, the step onto them must still count as somewhere new.
#[test]
#[ignore = "needs a ROM in VIVY_GB_ROM"]
fn stairs_to_the_same_spot_are_somewhere_new() {
    let mut m = red::tests::new_game();
    let mut world = World::default();
    let look: crate::features::games::gameboy::walk::Look =
        crate::features::games::gameboy::any::look;
    // Walk about the bedroom until code knows where she stands, away from the stairs.
    use crate::features::games::gameboy::map::Dir::{Down, Left, Right, Up};
    use crate::features::games::gameboy::walk::take_step;
    for dir in [
        Up, Up, Left, Left, Down, Right, Right, Right, Up, Up, Left, Down, Right, Down,
    ] {
        let view = look(&mut world, &mut m);
        take_step(&mut world, &mut m, look, &view, dir);
    }
    assert!(world.sight.found.x.is_some() && world.sight.found.y.is_some());
    let before = look(&mut world, &mut m);
    assert_eq!(m.emulator.read(RED_MAP), 38, "still upstairs");
    walk_to(&mut world, &mut m, look, (6, 1), 20);
    let walk = walk_to(&mut world, &mut m, look, (7, 1), 5);
    assert_eq!(m.emulator.read(RED_MAP), 37, "downstairs");
    assert!(
        matches!(walk.result, Walked::NewArea(_)),
        "{:?} in {} (was {})",
        walk.result,
        walk.view.area,
        before.area
    );
}

/// Up and down Red's stairs a few times: the bedroom is known again each time she comes
/// back up (the room downstairs looks much like it, but it's where she just was), and
/// each floor keeps one name.
#[test]
#[ignore = "needs a ROM in VIVY_GB_ROM"]
fn up_and_down_the_stairs_keeps_one_name_for_each_floor() {
    let mut m = red::tests::new_game();
    let mut world = World::default();
    let look: crate::features::games::gameboy::walk::Look =
        crate::features::games::gameboy::any::look;
    use crate::features::games::gameboy::map::Dir::{Down, Left, Right, Up};
    use crate::features::games::gameboy::walk::take_step;
    let steps = |world: &mut World, m: &mut Machine, dirs: &[_]| {
        for &dir in dirs {
            let view = look(world, m);
            take_step(world, m, look, &view, dir);
        }
    };
    steps(
        &mut world,
        &mut m,
        &[
            Up, Up, Left, Left, Down, Right, Right, Right, Up, Up, Left, Down, Right, Down,
        ],
    );
    let mut names: std::collections::HashMap<u8, std::collections::HashSet<String>> =
        Default::default();
    for _ in 0..3 {
        for _ in 0..2 {
            walk_to(&mut world, &mut m, look, (6, 1), 20);
            let walk = walk_to(&mut world, &mut m, look, (7, 1), 5);
            assert!(
                matches!(walk.result, Walked::NewArea(_)),
                "{:?}",
                walk.result
            );
            steps(&mut world, &mut m, &[Down, Left, Down]);
            let view = look(&mut world, &mut m);
            names
                .entry(m.emulator.read(RED_MAP))
                .or_default()
                .insert(view.area);
        }
    }
    // Names may change once, when the area byte is found; never back and forth.
    for (map, seen) in &names {
        assert!(seen.len() <= 2, "map {map} was called {seen:?}");
    }
    let places = world.sight.places();
    let maps: std::collections::HashSet<u8> = places.iter().map(|(_, m)| m.get(RED_MAP)).collect();
    assert_eq!(places.len(), 2, "{places:?}");
    assert_eq!(maps.len(), 2, "{places:?}");
}
