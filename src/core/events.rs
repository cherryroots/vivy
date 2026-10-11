//! Events that features publish for other features.
//!
//! Features never call each other. When something happens that another feature might care
//! about, the feature publishes a [`BotEvent`] with `ctx.publish(...)`, and every feature's
//! `on_bot_event` receives it. Nobody has to listen.
//!
//! **No feature listens yet.** Reminders publishes `ReminderFired` and the wheel publishes
//! `WheelRoundResolved`, but every feature still has the default `on_bot_event`, which does
//! nothing. The events are kept so the next feature that needs to react to them can just
//! implement `on_bot_event`, without touching reminders or the wheel.

use serenity::all::{ChannelId, GuildId, UserId};

#[derive(Debug, Clone)]
#[allow(dead_code, reason = "no feature listens yet")]
pub enum BotEvent {
    /// A reminder was delivered.
    ReminderFired {
        reminder_id: i64,
        user_id: UserId,
        channel_id: ChannelId,
    },
    /// A movie wheel round got its winner.
    WheelRoundResolved {
        guild_id: GuildId,
        round: i64,
        winner: UserId,
    },
}
