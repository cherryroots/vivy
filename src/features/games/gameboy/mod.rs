//! Vivy plays Game Boy games. Stages 1 to 3 of the plan: the emulator, admin commands to
//! play it by hand, code that reads the game and walks her around, and a cheap model that
//! carries out one short task at a time. Stage 4 so far: skills she writes herself, and
//! playing by herself in sessions.
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
//! - `skills.rs`: her own skills, know-how and routines she writes for a game (pure)
//! - `strategist.rs`: the stronger model's call that picks the next task
//! - `journal.rs`, `budget.rs`: her notes and episode log, and what playing used today
//!   (pure)
//! - `session.rs`: playing by herself until a cap, with checkpoints and the stuck ladder
//! - `playing.rs`, `dashboard.rs`: `/games gameboy play` and the rest, and the dashboard
//!   message that shows her playing
//! - `gate/`: the measurements (tests only)
//! - `commands.rs`: `/games gameboy ...`
//!
//! The plan, stage by stage, is in `docs/features/gameboy.md`.

mod actions;
mod budget;
mod change;
mod commands;
mod console;
mod dashboard;
mod emulator;
mod files;
#[cfg(test)]
mod gate;
mod journal;
mod map;
mod moves;
mod notes;
mod pictures;
mod player;
mod playing;
mod presses;
mod red;
mod screen;
mod session;
mod skills;
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

pub use commands::{GameboyCommand, autocomplete, gameboy};
pub use playing::{on_component, on_modal};

/// `[features.games.gameboy]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The folder with the ROMs (`.gb` and `.gbc` files). They must be your own copies.
    pub roms_dir: String,
    /// The folder for save states, a folder per ROM inside it.
    pub saves_dir: String,
    /// Minutes one `/games gameboy play` lasts, unless it says.
    pub session_minutes: u32,
    /// USD a day the paid models may cost while she plays (Haiku playing, and planning
    /// once the free tokens are used up), at list prices.
    pub daily_usd: f64,
    /// OpenAI's free tokens a day for the strategist (the data-sharing tokens).
    pub free_tokens: u64,
    /// While she plays, the dashboard (a GIF of her play since the last update, and what
    /// she's doing) is updated this often, in seconds (at least 5; 0: only at the end).
    pub live_seconds: u64,
    /// Whether anyone can give her hints with the dashboard's Hint button, or only admins.
    pub hints_from_anyone: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            roms_dir: "roms".to_string(),
            saves_dir: "gameboy".to_string(),
            session_minutes: 60,
            daily_usd: 1.5,
            free_tokens: 1_000_000,
            live_seconds: 5,
            hints_from_anyone: true,
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
