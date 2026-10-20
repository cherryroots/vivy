//! `BotCtx`: the shared services every feature handler receives.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::broadcast;
use twilight_cache_inmemory::InMemoryCache;
use twilight_model::oauth::team::TeamMembershipState;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::warn;

use super::config::{Config, Gate};
use super::db::Db;
use super::gateway::Gateway;
use super::interaction::FeatureCommand;
use super::{BotEvent, Feature, Timers};
use crate::ai::Ai;
use crate::util::discord::{ApplicationId, ChannelId, GuildId, Http, UserId};

/// Cheap to clone: every field is a handle to something shared.
#[derive(Clone)]
pub struct BotCtx {
    /// Discord's REST API: send messages, edit them, respond to interactions.
    pub http: Arc<Http>,
    /// What the gateway has told us about servers, channels and users.
    pub cache: Arc<InMemoryCache>,
    /// The gateway connections, for the status line and latency.
    pub gateway: Arc<Gateway>,
    /// The bot's Discord application, which interactions are answered as.
    pub application_id: ApplicationId,
    /// Every feature's slash commands.
    pub commands: Arc<Vec<FeatureCommand>>,
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
    /// The bot's username, like "Vivy".
    pub bot_name: String,
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
        guild?;
        let channel = self.cache.channel(channel)?;
        if !channel.kind.is_thread() {
            return None;
        }
        channel.parent_id
    }

    /// A channel's or thread's name, from the cache. `None` when the cache doesn't know it.
    pub fn channel_name(&self, guild: GuildId, channel: ChannelId) -> Option<String> {
        let channel = self.cache.channel(channel)?;
        if channel.guild_id != Some(guild) {
            return None;
        }
        channel.name.clone()
    }

    /// Sets the bot's custom status, the line under its name, on every gateway connection.
    pub async fn set_status(&self, text: &str) {
        self.gateway.set_status(text);
    }

    /// The bot's token as Discord's API wants it ("Bot ..."), for calls Twilight doesn't
    /// have, like message search.
    pub fn token(&self) -> &str {
        self.http.token().unwrap_or_default()
    }

    /// Whether the user is listed in `admins` in config.toml.
    pub fn is_admin(&self, user: UserId) -> bool {
        self.config.admins.contains(&user.get())
    }

    /// Whether the user is Vivy's owner: they own the bot's Discord application, or are on
    /// the team that does. `admins` don't count. While owner mode is on, she follows her
    /// owner's messages where other people can't direct her (see chat's prompt).
    pub fn is_owner(&self, user: UserId) -> bool {
        self.app_owners.contains(&user)
    }
}

/// Who owns the bot's Discord application, asked from Discord: the owner, or the team's
/// members when a team owns it. Empty, with a warning, when Discord can't be asked.
pub async fn app_owners(http: &Http) -> Vec<UserId> {
    let info = match http.current_user_application().await {
        Ok(response) => match response.model().await {
            Ok(info) => info,
            Err(err) => {
                warn!("couldn't read who owns the bot's Discord application: {err}");
                return Vec::new();
            }
        },
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
            .filter(|member| member.membership_state == TeamMembershipState::Accepted)
            .map(|member| member.user.id)
            .collect(),
        None => info.owner.map(|owner| owner.id).into_iter().collect(),
    }
}
