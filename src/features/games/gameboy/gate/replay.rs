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

use std::path::PathBuf;

use super::player::Local;
use super::results;
use crate::features::games::gameboy::console::Machine;
use crate::features::games::gameboy::session::Replay;
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
