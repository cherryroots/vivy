//! Who said it?: a game in plain Discord messages. `/games whosaidit` digs up messages from the
//! channel's history; for each one, everyone races to press the button of who wrote it.
//!
//! - `game.rs`: the rules: rounds, picks, points (pure)
//! - `sources.rs`: which channels quotes may come from
//! - `quotes.rs`: reading their history and cleaning messages into quotes
//! - `ui.rs`: the question, answer and score messages, and the button IDs (pure)
//! - `play.rs`: `/games whosaidit`, the task that runs a game, and button presses

mod game;
mod play;
mod quotes;
mod sources;
mod ui;

use async_trait::async_trait;
use serde::Deserialize;
use serenity::all::ComponentInteraction;

use crate::core::{BotCtx, Command, Feature, Result};

/// `[features.who_said_it]` settings.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Questions per game, when `/games whosaidit` doesn't say.
    pub rounds: usize,
    /// Seconds to answer each question (at least 5).
    pub answer_seconds: u64,
    /// How far back quotes come from.
    pub lookback_days: i64,
    /// The shortest and longest messages used, in characters.
    pub min_length: usize,
    pub max_length: usize,
    /// People whose messages are never used. They aren't offered as choices either, since
    /// the choices come from the people who wrote the quotes.
    pub exclude_users: Vec<u64>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            rounds: 10,
            answer_seconds: 20,
            lookback_days: 365,
            min_length: 20,
            max_length: 300,
            exclude_users: Vec::new(),
        }
    }
}

pub struct WhoSaidIt;

#[async_trait]
impl Feature for WhoSaidIt {
    fn name(&self) -> &'static str {
        "who_said_it"
    }

    fn commands(&self) -> Vec<Command> {
        vec![play::games()]
    }

    async fn on_component(
        &self,
        ctx: &BotCtx,
        interaction: &ComponentInteraction,
        action: &str,
    ) -> Result<()> {
        play::on_component(ctx, interaction, action).await
    }
}
