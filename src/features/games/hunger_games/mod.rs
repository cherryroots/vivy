//! The Hunger Games: a battle royale between server members, written by Vivy as the
//! Gamemaker. `/games hungergames` opens the Reaping, where the host picks tributes and
//! others volunteer. Then each round Vivy writes what happens in the arena, until one is
//! left.
//!
//! - `game.rs`: the rules: rounds, pacing, checking what she wrote, kills (pure)
//! - `writer.rs`: what she's told each round, and reading her answer
//! - `ui.rs`: the Reaping, round, cannon and victor messages, and the button IDs (pure)
//! - `last_words.rs`: a real message from each tribute, shown when they fall
//! - `notes.rs`: Vivy's memory notes about the tributes and the channels, through the
//!   `memory` tool
//! - `areas.rs`: what each of the arena's places is like, described once per game
//! - `voice.rs`: the Gamemaker's lines as voice messages, through the voice feature's tool
//! - `pictures.rs`: pictures she draws during the game, through the drawing feature's tool
//! - `looks.rs`: how the arena map and the tributes' sprites look, described once per game
//! - `play.rs`: `/games hungergames`, the task that runs a game, and button presses
//! - `stats.rs`: finished games saved, and `/games leaderboard`

mod areas;
mod game;
mod last_words;
mod looks;
mod notes;
mod pictures;
mod play;
mod stats;
mod ui;
mod voice;
mod writer;

use serde::Deserialize;

pub use play::{HungerGames, hunger_games, on_component};
pub use stats::{Leaderboard, MIGRATIONS, ResetLeaderboard, leaderboard, reset_leaderboard};

/// `[features.games.hunger_games]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The most tributes in one game (at most 24).
    pub max_tributes: usize,
    /// How long the Reaping stays open for the host to press Start.
    pub lobby_minutes: u64,
    /// Seconds people get to read a round before the next one is posted.
    pub auto_advance_seconds: u64,
    /// The most places in the arena (one per tribute up to this). More than 8 adds a ring
    /// further out on the map, which is harder to read.
    pub max_places: usize,
    /// Whether Vivy describes each place as the Games start (see `areas.rs`).
    pub describe_areas: bool,
    /// With pictures on: the most events drawn up close each round (0 for none).
    pub close_ups: usize,
    /// The quality of the maps and close-ups, and of the victor's picture: "low",
    /// "medium", "high", "xhigh" or "max".
    pub picture_quality: String,
    pub victor_picture_quality: String,
    /// The most who step into one Octagon (at least 2).
    pub most_in_octagon: usize,
    /// The most rounds the last two sides wander before they must meet.
    pub wander_rounds: usize,
}

impl Default for Settings {
    fn default() -> Self {
        let rules = game::Rules::default();
        Settings {
            max_tributes: 24,
            lobby_minutes: 10,
            auto_advance_seconds: 45,
            max_places: 8,
            describe_areas: true,
            close_ups: rules.close_ups,
            picture_quality: "medium".to_string(),
            victor_picture_quality: "high".to_string(),
            most_in_octagon: rules.most_in_octagon,
            wander_rounds: rules.wander_rounds,
        }
    }
}

impl Settings {
    /// The game rules these settings tune, within what the game can handle.
    fn rules(&self) -> game::Rules {
        game::Rules {
            close_ups: self.close_ups.min(pictures::MOST_CLOSE_UPS),
            most_in_octagon: self.most_in_octagon.max(2),
            wander_rounds: self.wander_rounds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_tune_the_rules() {
        assert_eq!(Settings::default().rules(), game::Rules::default());
        let settings: Settings =
            toml::from_str("close_ups = 20\nmost_in_octagon = 1\nwander_rounds = 0").unwrap();
        let rules = settings.rules();
        assert_eq!(rules.close_ups, pictures::MOST_CLOSE_UPS);
        assert_eq!(rules.most_in_octagon, 2);
        assert_eq!(rules.wander_rounds, 0);
        assert_eq!(settings.picture_quality, "medium");
    }
}
