//! Vivy plays Game Boy games. Stages 1 to 3 of the plan: the emulator, admin commands to
//! play it by hand, code that reads the game and walks her around, and a cheap model that
//! carries out one short task at a time.
//!
//! - `emulator.rs`: the Game Boy (the `boytacean` crate), a frame at a time
//! - `console.rs`: the one Game Boy on its own thread, with its last seconds of screens
//! - `presses.rs`: reading `a a up*3 start` (pure)
//! - `pictures.rs`: screens as PNGs and GIFs (pure)
//! - `files.rs`: ROMs and save states on disk
//! - `screen.rs`, `change.rs`, `text.rs`: the tile grid, dark screens, the font (pure)
//! - `moves.rs`, `map.rs`, `world.rs`, `walk.rs`: steps, area maps, walking to a step
//! - `red/`: Pokémon Red's memory map, and looking at Red
//! - `actions.rs`, `supervisor.rs`, `player.rs`: the supervisor's tools, its loop, and the
//!   Game Boy it plays
//! - `strategist.rs`: the stronger model's call that picks the next task
//! - `gate/`: stage 3's measurements (tests only)
//! - `commands.rs`: `/games gameboy ...`
//!
//! The plan, stage by stage, is in `docs/features/gameboy.md`.

mod actions;
mod change;
mod commands;
mod console;
mod emulator;
mod files;
#[cfg(test)]
mod gate;
mod map;
mod moves;
mod pictures;
mod player;
mod presses;
mod red;
mod screen;
// The strategist is measured by stage 3's gate; the bot calls it from stage 4.
#[cfg_attr(not(test), allow(dead_code))]
mod strategist;
mod supervisor;
// Reading text from the font, for games with no memory map. Tested on Pokémon Red now;
// played games use it once the tile classifier arrives.
#[cfg(test)]
mod test_rom;
#[cfg_attr(not(test), allow(dead_code))]
mod text;
mod walk;
mod world;

use std::path::PathBuf;

use serde::Deserialize;

pub use commands::gameboy;

/// `[features.games.gameboy]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The folder with the ROMs (`.gb` and `.gbc` files). They must be your own copies.
    pub roms_dir: String,
    /// The folder for save states, a folder per ROM inside it.
    pub saves_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            roms_dir: "roms".to_string(),
            saves_dir: "gameboy".to_string(),
        }
    }
}

impl Settings {
    fn roms_dir(&self) -> PathBuf {
        PathBuf::from(&self.roms_dir)
    }

    fn saves_dir(&self) -> PathBuf {
        PathBuf::from(&self.saves_dir)
    }
}
