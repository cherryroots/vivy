//! Plays episodes again the way she played them, with no model (`session::Replay`), to see
//! what code does now with the same presses: after a fix, the episode that went wrong
//! shows whether it's fixed, in seconds and for nothing.
//!
//! `VIVY_GB_REPLAY` is a replay's `<n>.json` from a session's `replays` folder, or the
//! folder for every episode in it; `VIVY_GB_ROM` is the game. Each action prints `=` when
//! it says what it said then and `≠` with both when not, and a screen after each goes to a
//! new folder in `VIVY_GB_RESULTS`; with `VIVY_GB_STEPS_LOG` set, every step each took. A test session's episodes play back exactly; the
//! bot's keep running while she thinks, so theirs are close. What code knew of her sprite
//! isn't saved, so the first step may find it again.
//!
//! With `VIVY_GB_WATCH` (addresses in hex), every look prints a `LOOK|` line: the place
//! and step code counted, whether she's free, her sprite's spot and those bytes. Given a
//! game's own numbers for where she is, a script can check code's counting against them
//! over many episodes.

use std::path::PathBuf;

use super::player::Local;
use super::results;
use crate::features::games::gameboy::console::Machine;
use crate::features::games::gameboy::session::{NAMED_LETTERS, Replay};
use crate::features::games::gameboy::supervisor::{Player, args_text};

/// The replays to play: one file, or every one in a folder by episode number.
fn replays(path: PathBuf) -> Vec<(u32, PathBuf)> {
    let number = |p: &PathBuf| p.file_stem()?.to_str()?.parse::<u32>().ok();
    if !path.is_dir() {
        return vec![(number(&path).unwrap_or(0), path)];
    }
    let mut found: Vec<(u32, PathBuf)> = std::fs::read_dir(&path)
        .unwrap()
        .filter_map(|entry| {
            let p = entry.ok()?.path();
            (p.extension()? == "json").then_some((number(&p)?, p))
        })
        .collect();
    found.sort();
    found
}

#[tokio::test]
#[ignore = "needs a ROM in VIVY_GB_ROM and a replay in VIVY_GB_REPLAY"]
async fn replay_episodes() {
    let path = PathBuf::from(std::env::var("VIVY_GB_REPLAY").expect("VIVY_GB_REPLAY"));
    let rom = crate::features::games::gameboy::emulator::tests::real_rom();
    let folder = results("replay");
    let (mut actions, mut differed) = (0, 0);
    for (n, json) in replays(path) {
        let replay: Replay = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
        let state = std::fs::read(json.with_extension("state")).unwrap();
        let mut local = Local::new(Machine::new("game.gb", &rom).unwrap());
        local.machine.emulator.load_state(&state).unwrap();
        local.world = replay.world;
        // The episode began with a look, before the model was first asked.
        let start = local.look().await.unwrap();
        println!(
            "Episode {n} in {} at {:?}: {}",
            start.view.area, start.view.at, replay.task
        );
        for (i, played) in replay.actions.iter().enumerate() {
            if played.name == NAMED_LETTERS {
                let votes: Vec<(u64, String)> =
                    serde_json::from_value(played.args["votes"].clone()).unwrap_or_default();
                local.name_letters(&votes, &[]).await.unwrap();
                continue;
            }
            local.world.log.clear();
            let acted = local.act(&played.name, &played.args).await.unwrap();
            // With `VIVY_GB_STEPS_LOG`, every step the action took.
            if std::env::var("VIVY_GB_STEPS_LOG").is_ok() {
                for step in &local.world.log {
                    println!("      {step}");
                }
            }
            let same = acted.outcome == played.outcome;
            actions += 1;
            differed += usize::from(!same);
            let asked = format!("{} {}", played.name, args_text(&played.args));
            // Where she is now, and whether she can walk: what a press changed.
            let at = format!(
                "[{} {:?}{}]",
                acted.view.area,
                acted.view.at,
                if acted.view.can_walk {
                    ""
                } else {
                    " can't walk"
                }
            );
            if same {
                println!("  = {}: {} {at}", asked.trim(), played.outcome);
            } else {
                println!("  ≠ {}", asked.trim());
                println!("      then: {}", played.outcome);
                println!("      now:  {} {at}", acted.outcome);
            }
            std::fs::write(
                folder.join(format!("{n:03}-{:02}.png", i + 1)),
                local.screen().await.unwrap(),
            )
            .unwrap();
        }
    }
    println!(
        "{differed} of {actions} actions said something different now. Screens in {}",
        folder.display()
    );
}

/// From the start of a replay (`VIVY_GB_REPLAY`, one `<n>.json`), code explores the area by
/// itself, with no model, for `VIVY_GB_STEPS` steps (300 by default), then walks to each way
/// out it knows (or `VIVY_GB_WANT` as "x,y"), and prints the map and where the walks went.
/// For a floor of arrows or spinners: whether riding them teaches the map the way through.
#[tokio::test]
#[ignore = "needs a ROM in VIVY_GB_ROM and a replay in VIVY_GB_REPLAY"]
async fn exploring_from_an_episode() {
    use crate::features::games::gameboy::emulator::Button;
    use crate::features::games::gameboy::presses::Press;
    use crate::features::games::gameboy::walk::{Walked, explore, look_for, walk_to};
    let json = PathBuf::from(std::env::var("VIVY_GB_REPLAY").expect("VIVY_GB_REPLAY"));
    let most: u32 = std::env::var("VIVY_GB_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let rom = crate::features::games::gameboy::emulator::tests::real_rom();
    let replay: Replay = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut machine = Machine::new("game.gb", &rom).unwrap();
    machine
        .emulator
        .load_state(&std::fs::read(json.with_extension("state")).unwrap())
        .unwrap();
    let look = look_for(machine.emulator.info());
    let mut world = replay.world;
    let start = look(&mut world, &mut machine);
    let area = start.area.clone();
    println!("{} at {:?}", area, start.at);
    let mut steps = 0;
    while steps < most {
        let view = look(&mut world, &mut machine);
        if view.area != area {
            println!("Left for {} at {:?}", view.area, view.at);
            break;
        }
        if !view.can_walk {
            // A battle or words: pressed through, as a player would.
            machine.press(&[Press::of(vec![Button::B])], Default::default());
            machine.run(30);
            steps += 1;
            continue;
        }
        let walk = explore(&mut world, &mut machine, look, (most - steps).min(60));
        steps += walk.steps.max(1);
        if walk.result == Walked::NoWay {
            break;
        }
    }
    let map = world.area(&area);
    println!("After {steps} steps, rides known: {}", map.rides());
    let mut wanted: Vec<(i32, i32)> = map.ways_out().into_iter().flat_map(|(_, s)| s).collect();
    if let Ok(want) = std::env::var("VIVY_GB_WANT") {
        let (x, y) = want.split_once(',').unwrap();
        wanted = vec![(x.trim().parse().unwrap(), y.trim().parse().unwrap())];
    }
    let view = look(&mut world, &mut machine);
    println!("{}", world.area(&area).draw(view.at, 40));
    for to in wanted {
        let way = world.area(&area).path(view.at, to, false);
        println!(
            "A way from {:?} to {to:?}: {:?}",
            view.at,
            way.map(|w| w.len())
        );
        world.log.clear();
        let walk = walk_to(&mut world, &mut machine, look, to, 200);
        if std::env::var("VIVY_GB_STEPS_LOG").is_ok() {
            for step in &world.log {
                println!("      {step}");
            }
        }
        println!(
            "Walked there: {:?} after {} steps, at {} {:?}",
            walk.result, walk.steps, walk.view.area, walk.view.at
        );
        if walk.view.area != area {
            break;
        }
        // What kept her from walking, and for how long.
        let mut frames = 0;
        while !look(&mut world, &mut machine).can_walk && frames < 3000 {
            machine.run(10);
            frames += 10;
        }
        let now = look(&mut world, &mut machine);
        println!(
            "Free again after {frames} frames at {:?}: {:?}",
            now.at, now.text
        );
    }
}
