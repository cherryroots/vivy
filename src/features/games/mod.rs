//! Games: the `/games` command and the games under it. Discord gives a command's
//! subcommands to one handler, so the games share this feature, and its settings and gate
//! (`[features.games]`).
//!
//! - `who_said_it/`: `/games whosaidit`, guessing who wrote old messages from the server
//! - `hunger_games/`: `/games hungergames`, a battle royale between members that Vivy writes,
//!   and `/games leaderboard`, its wins and kills
//! - `gameboy/`: `/games gameboy ...`, a Game Boy emulator admins play by hand, and Vivy
//!   plays by herself in sessions
//!
//! Each game's buttons start with `games:<game>:`, and `on_component` hands a press to its
//! game. The Hunger Games borrow who said it?'s quote reader and channel rules for last
//! words and the arena.

mod gameboy;
mod hunger_games;
mod who_said_it;

use async_trait::async_trait;
use serde::Deserialize;
use serenity::all::ComponentInteraction;

use crate::core::{BotCtx, Command, Context, Feature, Result};

/// `[features.games]` settings: one table per game.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `[features.games.who_said_it]`
    pub who_said_it: who_said_it::Settings,
    /// `[features.games.hunger_games]`
    pub hunger_games: hunger_games::Settings,
    /// `[features.games.gameboy]`
    pub gameboy: gameboy::Settings,
}

pub struct Games;

#[async_trait]
impl Feature for Games {
    fn name(&self) -> &'static str {
        "games"
    }

    fn commands(&self) -> Vec<Command> {
        vec![games()]
    }

    fn migrations(&self) -> &'static [&'static str] {
        hunger_games::MIGRATIONS
    }

    async fn on_component(
        &self,
        ctx: &BotCtx,
        interaction: &ComponentInteraction,
        action: &str,
    ) -> Result<()> {
        if let Some(action) = action.strip_prefix("who_said_it:") {
            return who_said_it::on_component(ctx, interaction, action).await;
        }
        if action.starts_with("hunger:") {
            return hunger_games::on_component(ctx, interaction, action).await;
        }
        Ok(())
    }
}

/// Games to play in the channel
#[poise::command(
    slash_command,
    guild_only,
    subcommands(
        "who_said_it::who_said_it",
        "hunger_games::hunger_games",
        "hunger_games::leaderboard",
        "hunger_games::reset_leaderboard",
        "gameboy::gameboy"
    ),
    subcommand_required
)]
pub async fn games(_ctx: Context<'_>) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;

    #[test]
    fn the_example_config_sets_every_game() {
        let config = Config::parse(include_str!("../../../config.example.toml")).unwrap();
        let settings: Settings = config.feature("games").unwrap();
        assert_eq!(settings.who_said_it.rounds, 10);
        assert_eq!(settings.who_said_it.lookback_days, 365);
        assert_eq!(settings.hunger_games.max_tributes, 24);
        assert_eq!(settings.hunger_games.auto_advance_seconds, 45);
        assert_eq!(settings.hunger_games.lobby_minutes, 10);
        assert_eq!(settings.gameboy.roms_dir, "roms");
        assert_eq!(settings.gameboy.saves_dir, "gameboy");
    }
}
