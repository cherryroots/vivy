//! Events that features publish for other features.
//!
//! Features never call each other. When something happens that another feature might care
//! about, the feature publishes a [`BotEvent`] with `ctx.publish(...)`, and every feature's
//! `on_bot_event` receives it. Nobody has to listen.
//!
//! Reminders publishes `ReminderFired` (no feature listens yet) and the wheel publishes
//! `WheelRoundResolved` (memory listens and logs watched movies to
//! `/memories/server/movies-watched.md`).

use crate::util::discord::{ChannelId, GuildId, UserId};

#[derive(Debug, Clone)]
pub enum BotEvent {
    /// A reminder was delivered.
    #[allow(dead_code, reason = "no feature listens yet")]
    ReminderFired {
        reminder_id: i64,
        user_id: UserId,
        channel_id: ChannelId,
    },
    /// A movie wheel round got its winner.
    WheelRoundResolved {
        guild_id: GuildId,
        season: i64,
        round: i64,
        winner: UserId,
        winner_name: String,
        movie: Option<String>,
    },
}
