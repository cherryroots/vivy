//! Vivy plays Game Boy games. This is stage 1 of the plan: the emulator, and admin commands
//! to play it by hand. No AI yet.
//!
//! - `emulator.rs`: the Game Boy (the `boytacean` crate), a frame at a time
//! - `console.rs`: the one Game Boy on its own thread, with its last seconds of screens
//! - `presses.rs`: reading `a a up*3 start` (pure)
//! - `pictures.rs`: screens as PNGs and GIFs (pure)
//! - `files.rs`: ROMs and save states on disk
//! - `commands.rs`: `/games gameboy ...`
//!
//! The plan, stage by stage, is in `docs/features/gameboy.md`.

mod commands;
mod console;
mod emulator;
mod files;
mod pictures;
mod presses;
#[cfg(test)]
mod test_rom;

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
