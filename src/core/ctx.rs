//! `BotCtx`: the shared services every feature handler receives.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serenity::all::{
    ActivityData, Cache, ChannelId, GuildId, Http, MembershipState, ShardManager, UserId,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::warn;

use super::config::{Config, Gate};
use super::db::Db;
use super::{BotEvent, Feature, Timers};
use crate::ai::Ai;

/// Cheap to clone: every field is a handle to something shared.
#[derive(Clone)]
pub struct BotCtx {
    /// Discord's REST API: send messages, edit them, respond to interactions.
    pub http: Arc<Http>,
    /// What the gateway has told us about servers, channels and users.
    pub cache: Arc<Cache>,
    /// The gateway connections, for latency and shutdown.
    pub shard_manager: Arc<ShardManager>,
    pub db: Db,
    pub config: Arc<Config>,
    /// AI models (chat, and one-off calls any feature can make).
    pub ai: Ai,
    /// For downloading files from the web (attachments, links). Shares connections.
    pub web: reqwest::Client,
    /// Publish with [`BotCtx::publish`]; the dispatcher delivers to every feature.
    pub events: broadcast::Sender<BotEvent>,
    /// Cancelled when the bot shuts down. Background tasks stop when they see it.
    pub shutdown: CancellationToken,
    /// Background tasks. Shutdown waits for them, so they can finish what they're doing.
    pub tasks: TaskTracker,
    /// When each background loop last ran and runs next, for the status picture.
    pub timers: Timers,
    /// Every feature, in the order of `features::all()`.
    pub features: Arc<Vec<Arc<dyn Feature>>>,
    pub bot_id: UserId,
    /// Who owns the bot's Discord application: its owner, or the members of the team that
    /// owns it. Read once at startup with [`app_owners`].
    pub app_owners: Arc<Vec<UserId>>,
    pub started_at: DateTime<Utc>,
}

impl BotCtx {
    /// Sends an event to every feature's `on_bot_event`.
    pub fn publish(&self, event: BotEvent) {
        // `send` only fails when nobody is subscribed, which is fine.
        let _ = self.events.send(event);
    }

    pub fn gate(&self, feature: &str) -> &Gate {
        self.config.gate(feature)
    }

    /// Whether `feature` may run in this server (`None` in DMs) and channel. Use this rather
    /// than `gate(..).allows(..)`: it also applies the gate of a thread's parent channel.
    pub fn allows(&self, feature: &str, guild: Option<GuildId>, channel: ChannelId) -> bool {
        self.gate(feature)
            .allows(guild, channel, self.thread_parent(guild, channel))
    }

    /// The channel a thread is in, or `None` when `channel` isn't a thread the cache knows.
    pub fn thread_parent(&self, guild: Option<GuildId>, channel: ChannelId) -> Option<ChannelId> {
        let guild = self.cache.guild(guild?)?;
        let thread = guild.threads.iter().find(|t| t.id == channel)?;
        thread.parent_id
    }

    /// A channel's or thread's name, from the cache. `None` when the cache doesn't know it.
    pub fn channel_name(&self, guild: GuildId, channel: ChannelId) -> Option<String> {
        let guild = self.cache.guild(guild)?;
        guild
            .channels
            .get(&channel)
            .or_else(|| guild.threads.iter().find(|t| t.id == channel))
            .map(|c| c.name.clone())
    }

    /// Sets the bot's custom status, the line under its name, on every gateway connection.
    pub async fn set_status(&self, text: &str) {
        let activity = ActivityData::custom(text);
        for runner in self.shard_manager.runners.lock().await.values() {
            runner.runner_tx.set_activity(Some(activity.clone()));
        }
    }

    /// Whether the user is listed in `admins` in config.toml.
    pub fn is_admin(&self, user: UserId) -> bool {
        self.config.admins.contains(&user.get())
    }

    /// Whether the user is Vivy's owner: they own the bot's Discord application (or are on
    /// the team that does), or they're listed in `admins`. She follows her owner's
    /// instructions where other people can't direct her (see chat's prompt).
    pub fn is_owner(&self, user: UserId) -> bool {
        self.app_owners.contains(&user) || self.is_admin(user)
    }
}

/// Who owns the bot's Discord application, asked from Discord: the owner, or the team's
/// members when a team owns it. Empty, with a warning, when Discord can't be asked.
pub async fn app_owners(http: &Http) -> Vec<UserId> {
    let info = match http.get_current_application_info().await {
        Ok(info) => info,
        Err(err) => {
            warn!("couldn't read who owns the bot's Discord application: {err}");
            return Vec::new();
        }
    };
    match info.team {
        // For a team, `owner` is a made-up user standing for the team, so use its members.
        // Only people who accepted the invite are on the team.
        Some(team) => team
            .members
            .iter()
            .filter(|member| member.membership_state == MembershipState::Accepted)
            .map(|member| member.user.id)
            .collect(),
        None => info.owner.map(|owner| owner.id).into_iter().collect(),
    }
}
