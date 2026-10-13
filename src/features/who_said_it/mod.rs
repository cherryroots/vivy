//! Who said it?: a Discord Activity (a game window inside Discord) where everyone in a
//! channel sees a message from the channel's history and races to pick who wrote it.
//!
//! Vivy serves the page and runs the games. Discord shows the page in an iframe and sends
//! its requests through its proxy to `listen`, by way of the URL mappings in the Developer
//! Portal. The setup is in `docs/features/who-said-it.md`.
//!
//! - `game.rs`: the rules: rounds, picks, points (pure)
//! - `quotes.rs`: reading the channel's history and cleaning messages into quotes
//! - `room.rs`: one task per game, which players' connections talk to
//! - `web.rs`: the web server: the page, the login, the WebSockets
//! - `page/`: the page itself (HTML, JavaScript, CSS, and Discord's SDK in one file)
//!
//! It needs `DISCORD_CLIENT_SECRET` in .env. Without it nothing is served.

mod game;
mod quotes;
mod room;
mod web;

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{error, info};

use crate::core::{BotCtx, Feature, Result};
use crate::util::env_key;

/// `[features.who_said_it]` settings.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The address and port the page is served on. Your web server (or tunnel) forwards
    /// the Activity's requests here.
    pub listen: String,
    /// Questions per game.
    pub rounds: usize,
    /// Seconds to answer each question.
    pub answer_seconds: u64,
    /// How far back quotes come from.
    pub lookback_days: i64,
    /// The shortest and longest messages used, in characters.
    pub min_length: usize,
    pub max_length: usize,
    /// People whose messages are never used. They aren't offered as choices either, since
    /// the choices come from the people who wrote the quotes.
    pub exclude_users: Vec<u64>,
    /// Post the final scores in the channel when a game ends.
    pub post_results: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            listen: "127.0.0.1:8787".to_string(),
            rounds: 10,
            answer_seconds: 20,
            lookback_days: 365,
            min_length: 20,
            max_length: 300,
            exclude_users: Vec::new(),
            post_results: true,
        }
    }
}

pub struct WhoSaidIt;

#[async_trait]
impl Feature for WhoSaidIt {
    fn name(&self) -> &'static str {
        "who_said_it"
    }

    /// Starts the web server, when the client secret is there.
    async fn start(&self, ctx: &BotCtx) -> Result<()> {
        let settings: Settings = ctx.config.feature(self.name())?;
        let Some(secret) = env_key("DISCORD_CLIENT_SECRET") else {
            info!("DISCORD_CLIENT_SECRET is not set in .env, so Who said it? is off");
            return Ok(());
        };
        // A bot's application ID, which the Activity's login needs. Serenity learns it when
        // the bot connects; for bots it's the same as the bot's user ID.
        let client_id = ctx
            .http
            .application_id()
            .map_or(ctx.bot_id.get(), |id| id.get());
        let ctx2 = ctx.clone();
        ctx.tasks.spawn(async move {
            if let Err(err) = web::serve(ctx2, Arc::new(settings), client_id, secret).await {
                error!("Who said it? stopped: {err:#}");
            }
        });
        Ok(())
    }
}
