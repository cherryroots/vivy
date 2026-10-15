//! The Hunger Games: a battle royale between server members, written by Vivy as the
//! Gamemaker. `/games hungergames` opens the Reaping, where the host picks tributes and
//! others volunteer. Then each round Vivy writes what happens in the arena, until one is
//! left.
//!
//! - `game.rs`: the rules: rounds, pacing, checking what she wrote, kills (pure)
//! - `writer.rs`: what she's told each round, and reading her answer
//! - `ui.rs`: the Reaping, round, cannon and victor messages, and the button IDs (pure)
//! - `last_words.rs`: a real message from each tribute, shown when they fall
//! - `notes.rs`: Vivy's memory notes about the tributes, through the `memory` tool
//! - `voice.rs`: the Gamemaker's lines as voice messages, through the voice feature's tool
//! - `pictures.rs`: pictures she draws during the game, through the drawing feature's tool
//! - `looks.rs`: how each tribute looks in her pictures, described once from their profile picture
//! - `play.rs`: `/games hungergames`, the task that runs a game, and button presses

mod game;
mod last_words;
mod looks;
mod notes;
mod pictures;
mod play;
mod ui;
mod voice;
mod writer;

use serde::Deserialize;

pub use play::{hunger_games, on_component};

/// `[features.games.hunger_games]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The most tributes in one game (at most 24).
    pub max_tributes: usize,
    /// How long the Reaping stays open for the host to press Start.
    pub lobby_minutes: u64,
    /// Seconds before the next round starts by itself, when the host doesn't press Next.
    pub auto_advance_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_tributes: 24,
            lobby_minutes: 10,
            auto_advance_seconds: 30,
        }
    }
}
