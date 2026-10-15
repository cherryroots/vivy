//! The Hunger Games: a battle royale between server members, written by Vivy as the
//! Gamemaker. `/games hungergames` opens the Reaping, where the host picks tributes and
//! others volunteer. Then each round Vivy writes what happens in the arena, until one is
//! left.
//!
//! - `game.rs`: the rules: rounds, pacing, checking what she wrote, kills (pure)
//! - `writer.rs`: what she's told each round, and reading her answer
//! - `ui.rs`: the Reaping, round, cannon and victor messages, and the button IDs (pure)
//! - `play.rs`: `/games hungergames`, the task that runs a game, and button presses

mod game;
mod play;
mod ui;
mod writer;

use serde::Deserialize;

pub use play::{hunger_games, on_component};

/// `[features.who_said_it.hunger_games]` settings.
#[derive(Debug, Deserialize)]
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

#[cfg(test)]
mod tests {
    use crate::core::config::Config;
    use crate::features::who_said_it;

    #[test]
    fn the_example_config_sets_them() {
        let config = Config::parse(include_str!("../../../../config.example.toml")).unwrap();
        let settings: who_said_it::Settings = config.feature("who_said_it").unwrap();
        assert_eq!(settings.hunger_games.max_tributes, 24);
        assert_eq!(settings.hunger_games.auto_advance_seconds, 30);
        assert_eq!(settings.hunger_games.lobby_minutes, 10);
    }
}
