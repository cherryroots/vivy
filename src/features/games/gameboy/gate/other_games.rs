//! Other games than Red, played by script with the look that has no memory map, to see
//! what it makes of them. `VIVY_GB_OTHER` is the ROM, `VIVY_GB_RESULTS` where screens go.

use super::results;
use crate::features::games::gameboy::console::Machine;
use crate::features::games::gameboy::emulator::{Button, RomInfo};
use crate::features::games::gameboy::pictures::png_at;

fn other() -> (String, Vec<u8>) {
    let path = std::env::var("VIVY_GB_OTHER").expect("VIVY_GB_OTHER");
    (path.clone(), std::fs::read(path).unwrap())
}

fn tap(m: &mut Machine, button: Button, after: u32) {
    m.emulator.hold(&[button]);
    m.run(6);
    m.emulator.release();
    m.run(after);
}

/// Boots the game and presses Start and A in turn, saving a screen every so often, so a
/// person can see where play starts.
#[test]
#[ignore = "needs a ROM in VIVY_GB_OTHER"]
fn boot_and_press_through() {
    let (_, rom) = other();
    let info = RomInfo::read(&rom).unwrap();
    let folder = results(&format!("boot-{}", info.title.trim().replace(' ', "_")));
    let mut m = Machine::new("other.gb", &rom).unwrap();
    println!("{info:?}");
    if let Ok(from) = std::env::var("VIVY_GB_FROM") {
        m.emulator
            .load_state(&std::fs::read(from).unwrap())
            .unwrap();
    }
    let script =
        std::env::var("VIVY_GB_SCRIPT").unwrap_or_else(|_| "w:300 start:120 a:120 ".repeat(20));
    for (i, word) in script.split_whitespace().enumerate() {
        let (name, wait) = word.split_once(':').unwrap_or((word, "60"));
        let wait: u32 = wait.parse().unwrap();
        match name {
            "w" => m.run(wait),
            name => {
                let button = Button::parse(name).unwrap();
                tap(&mut m, button, wait);
            }
        }
        let screen = m.emulator.screen();
        std::fs::write(
            folder.join(format!("{i:03}-{name}.png")),
            png_at(&screen, 2).unwrap(),
        )
        .unwrap();
    }
    std::fs::write(folder.join("end.state"), m.emulator.save_state().unwrap()).unwrap();
    println!("{}", folder.display());
}

/// Explores the game from `VIVY_GB_FROM` with the look that has no memory map, the way the
/// test game is explored: frontier first, then the door gone through least. Text gets A.
/// Prints what the cheat finder found and the map of every place, and saves a screen
/// every 25 walks.
#[test]
#[ignore = "needs a ROM in VIVY_GB_OTHER and a state in VIVY_GB_FROM"]
fn explore_another_game() {
    use std::fmt::Write as _;

    use crate::features::games::gameboy::walk::{Walked, explore, look_for, walk_to};
    use crate::features::games::gameboy::world::World;

    let (_, rom) = other();
    let info = RomInfo::read(&rom).unwrap();
    let look = look_for(&info);
    let folder = results(&format!("explore-{}", info.title.trim().replace(' ', "_")));
    let mut m = Machine::new("other.gb", &rom).unwrap();
    let from = std::env::var("VIVY_GB_FROM").expect("VIVY_GB_FROM");
    m.emulator
        .load_state(&std::fs::read(from).unwrap())
        .unwrap();
    m.run(2);
    let most: u32 = std::env::var("VIVY_GB_STEPS")
        .map(|s| s.parse().unwrap())
        .unwrap_or(600);
    let mut world = World::default();
    let (mut steps, mut doors, mut walks, mut texts, mut free) = (0, 0, 0, 0, 0);
    let mut used: std::collections::HashMap<(String, (i32, i32)), u32> = Default::default();
    let mut log = String::new();
    while steps < most && walks < 400 {
        walks += 1;
        let before = look(&mut world, &mut m);
        let target = world
            .area(&before.area)
            .frontier(before.at)
            .first()
            .copied();
        let walk = explore(&mut world, &mut m, look, (most - steps).min(40));
        steps += walk.steps.max(1);
        let area = walk.view.area.clone();
        free += usize::from(walk.view.can_walk);
        writeln!(
            log,
            "{walks:3} {:?} {} steps, at {area} {:?}, can walk {}, target {target:?} from {:?}",
            walk.result, walk.steps, walk.view.at, walk.view.can_walk, before.at
        )
        .unwrap();
        if matches!(walk.result, Walked::Stuck) {
            let at = walk.view.at;
            writeln!(log, "{}", world.area(&area).draw(at, 5)).unwrap();
        }
        if walks % 25 == 0 {
            let screen = m.emulator.screen();
            std::fs::write(
                folder.join(format!("walk-{walks:03}.png")),
                png_at(&screen, 2).unwrap(),
            )
            .unwrap();
        }
        match walk.result {
            Walked::NewArea(_) => doors += 1,
            Walked::Interrupted(_) => {
                // Text, or code doesn't know which sprite is her yet: A, then a direction
                // the way a player would press it.
                use crate::features::games::gameboy::console::Timing;
                use crate::features::games::gameboy::presses::Press;
                texts += 1;
                tap(&mut m, Button::A, 30);
                let _ = look(&mut world, &mut m);
                let button = [Button::Down, Button::Left, Button::Up, Button::Right][texts % 4];
                m.press(&[Press(vec![button])], Timing::default());
            }
            Walked::Arrived => {}
            Walked::NoWay | Walked::Stuck => {
                let map = world.area(&area);
                let door = map
                    .doors
                    .keys()
                    .min_by_key(|&&pos| (used.get(&(area.clone(), pos)).copied(), pos))
                    .copied();
                let Some(door) = door else {
                    // Nothing known to go to: a step each way, counted like any other.
                    use crate::features::games::gameboy::map::Dir;
                    use crate::features::games::gameboy::walk::take_step;
                    for dir in [Dir::Down, Dir::Left, Dir::Up, Dir::Right] {
                        let view = look(&mut world, &mut m);
                        if view.can_walk {
                            take_step(&mut world, &mut m, look, &view, dir);
                        }
                    }
                    tap(&mut m, Button::A, 30);
                    continue;
                };
                *used.entry((area.clone(), door)).or_default() += 1;
                let walk = walk_to(&mut world, &mut m, look, door, 60);
                steps += walk.steps.max(1);
                if matches!(walk.result, Walked::NewArea(_)) {
                    doors += 1;
                }
            }
        }
    }
    let found = world.sight.found;
    let mut report = format!(
        "{}: {steps} steps, {walks} walks ({free} ended free to walk), {doors} new areas, {texts} texts.\nFound x {:?}, y {:?}, area {:?}\n",
        info.title, found.x, found.y, found.area
    );
    let places = world.sight.places();
    writeln!(report, "{} places", places.len()).unwrap();
    for (name, _) in &places {
        let map = world.area(name);
        writeln!(report, "- {name}\n{}", map.draw((0, 0), 10)).unwrap();
    }
    let steps_log: String = world
        .log
        .iter()
        .map(|l| {
            format!(
                "{} {:?} {:?} {:?} -> {} {:?}\n",
                l.area, l.from, l.direction, l.outcome, l.to_area, l.to
            )
        })
        .collect();
    std::fs::write(
        folder.join("report.txt"),
        format!("{report}\n{log}\n{steps_log}"),
    )
    .unwrap();
    std::fs::write(folder.join("end.state"), m.emulator.save_state().unwrap()).unwrap();
    std::fs::write(
        folder.join("end.png"),
        png_at(&m.emulator.screen(), 2).unwrap(),
    )
    .unwrap();
    println!("{report}\n{}", folder.display());
}

/// Prints the sprites and scroll every frame while a direction is held, from
/// `VIVY_GB_FROM`.
#[test]
#[ignore = "needs a ROM in VIVY_GB_OTHER and a state in VIVY_GB_FROM"]
fn sprites_while_holding() {
    use crate::features::games::gameboy::screen::Screen;
    let (_, rom) = other();
    let mut m = Machine::new("other.gb", &rom).unwrap();
    m.emulator
        .load_state(&std::fs::read(std::env::var("VIVY_GB_FROM").unwrap()).unwrap())
        .unwrap();
    let button = Button::parse(&std::env::var("VIVY_GB_HOLD").unwrap_or("right".into())).unwrap();
    let frames: u32 = std::env::var("VIVY_GB_FRAMES")
        .map(|f| f.parse().unwrap())
        .unwrap_or(40);
    m.run(2);
    m.emulator.hold(&[button]);
    for frame in 0..frames {
        m.frame();
        let screen = Screen::read(m.emulator.video());
        let r = screen.registers;
        let sprites: Vec<String> = screen
            .sprites
            .iter()
            .map(|s| format!("{},{}#{:02x}", s.x, s.y, s.tile))
            .collect();
        println!(
            "{frame:3} scroll {},{} win {},{} lcdc {:02x}: {}",
            r.scx,
            r.scy,
            r.wx,
            r.wy,
            r.lcdc,
            sprites.join(" ")
        );
    }
    m.emulator.release();
}

/// Takes the steps in `VIVY_GB_WALK` (like "u u l d") from `VIVY_GB_FROM` with the look
/// that has no memory map, printing what each step and look gave.
#[test]
#[ignore = "needs a ROM in VIVY_GB_OTHER and a state in VIVY_GB_FROM"]
fn steps_by_hand() {
    use crate::features::games::gameboy::map::Dir;
    use crate::features::games::gameboy::walk::{look_for, take_step};
    use crate::features::games::gameboy::world::World;
    let (_, rom) = other();
    let look = look_for(&RomInfo::read(&rom).unwrap());
    let mut m = Machine::new("other.gb", &rom).unwrap();
    m.emulator
        .load_state(&std::fs::read(std::env::var("VIVY_GB_FROM").unwrap()).unwrap())
        .unwrap();
    m.run(2);
    let folder = results("walk");
    let mut world = World::default();
    let mut view = look(&mut world, &mut m);
    println!("start {view:?} spot {:?}", world.sight.her_spot());
    std::fs::write(
        folder.join("000.png"),
        png_at(&m.emulator.screen(), 2).unwrap(),
    )
    .unwrap();
    for (i, word) in std::env::var("VIVY_GB_WALK")
        .unwrap()
        .split_whitespace()
        .enumerate()
    {
        let dir = match word {
            "u" => Dir::Up,
            "d" => Dir::Down,
            "l" => Dir::Left,
            _ => Dir::Right,
        };
        let (next, outcome) = take_step(&mut world, &mut m, look, &view, dir);
        println!(
            "{word}: {outcome:?} -> {} {:?} walk {} spot {:?} found {:?}",
            next.area,
            next.at,
            next.can_walk,
            world.sight.her_spot(),
            world.sight.found
        );
        let screen = m.emulator.screen();
        std::fs::write(
            folder.join(format!("{:03}-{word}.png", i + 1)),
            png_at(&screen, 2).unwrap(),
        )
        .unwrap();
        view = next;
    }
    let area = view.area.clone();
    println!("{}", world.area(&area).draw(view.at, 6));
}
