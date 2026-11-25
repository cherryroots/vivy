//! Who said it?: a game in plain Discord messages. `/games whosaidit` digs up messages from the
//! channel's history; for each one, everyone races to press the button of who wrote it.
//!
//! - `game.rs`: the rules: rounds, picks, points (pure)
//! - `ui.rs`: the question, answer and score messages, and the button IDs (pure)
//! - `play.rs`: `/games whosaidit`, the task that runs a game, and button presses
//!
//! Channel picking (`sources.rs`) and quote reading (`quotes.rs`) live in `games/` because
//! the Hunger Games share them for last words and the arena.

mod game;
mod play;
mod ui;

use serde::Deserialize;

use crate::features::games::quotes::QuoteRules;
pub use play::{WhoSaidIt, on_component, who_said_it};

/// `[features.games.who_said_it]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Questions per game, when `/games whosaidit` doesn't say.
    pub rounds: usize,
    /// Seconds to answer each question (at least 5).
    pub answer_seconds: u64,
    /// Seconds between an answer and the next question, to read who got it.
    pub pause_seconds: u64,
    /// How far back quotes come from.
    pub lookback_days: i64,
    /// The shortest and longest messages used, in characters.
    pub min_length: usize,
    pub max_length: usize,
    /// People whose messages are never used. They aren't offered as choices either, since
    /// the choices come from the people who wrote the quotes.
    pub exclude_users: Vec<u64>,
}

impl Settings {
    /// The quote-reading filters shared with `games::quotes`.
    pub fn quote_rules(&self) -> QuoteRules {
        QuoteRules {
            lookback_days: self.lookback_days,
            min_length: self.min_length,
            max_length: self.max_length,
            exclude_users: self.exclude_users.clone(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            rounds: 10,
            answer_seconds: 20,
            pause_seconds: 5,
            lookback_days: 365,
            min_length: 20,
            max_length: 300,
            exclude_users: Vec::new(),
        }
    }
}
