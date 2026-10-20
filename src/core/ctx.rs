//! `BotCtx`: the shared services every feature handler receives.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::warn;
use twilight_cache_inmemory::InMemoryCache;
use twilight_model::oauth::team::TeamMembershipState;
use twilight_util::permission_calculator::PermissionCalculator;

use super::config::{Config, Gate};
use super::db::Db;
use super::gateway::Gateway;
use super::interaction::FeatureCommand;
use super::{BotEvent, Feature, Timers};
use crate::ai::Ai;
use crate::util::discord::{
    ApplicationId, Channel, ChannelId, GuildId, Http, Permissions, Role, RoleId, UserId,
    get_member, member_display_name, member_name,
};

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

    /// The name a member goes by in a server, from the cache, else asked from Discord.
    /// `None` when they aren't in the server (or Discord can't be asked).
    pub async fn member_name(&self, guild: GuildId, user: UserId) -> Option<String> {
        if let Some(member) = self.cache.member(guild, user)
            && let Some(cached) = self.cache.user(user)
        {
            return Some(member_name(member.nick(), &cached).to_string());
        }
        let member = get_member(&self.http, guild, user).await.ok()?;
        Some(member_display_name(&member).to_string())
    }

    /// Sets the bot's custom status, the line under its name, on every gateway connection.
    /// The servers the bot is in, from the cache.
    pub fn guild_ids(&self) -> Vec<GuildId> {
        self.cache
            .iter()
            .guilds()
            .map(|guild| *guild.key())
            .collect()
    }

    /// The server's channels and threads from the cache.
    pub fn guild_channels(&self, guild: GuildId) -> Vec<Channel> {
        let Some(ids) = self.cache.guild_channels(guild) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|&id| self.cache.channel(id).map(|c| c.clone()))
            .collect()
    }

    /// The server's roles from the cache.
    pub fn guild_roles(&self, guild: GuildId) -> Vec<Role> {
        let Some(ids) = self.cache.guild_roles(guild) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|&id| self.cache.role(id).map(|r| r.resource().clone()))
            .collect()
    }

    /// What someone with `roles` may do in `channel`, worked out from the cached roles
    /// and the channel's overwrites, the way Discord does. A thread has its parent
    /// channel's permissions, so pass the parent. `None` when the server isn't cached.
    pub fn permissions_in(
        &self,
        guild: GuildId,
        user: UserId,
        roles: &[RoleId],
        channel: &Channel,
    ) -> Option<Permissions> {
        let owner = self.cache.guild(guild)?.owner_id();
        // The @everyone role has the server's ID.
        let everyone = self.cache.role(guild.cast())?.resource().permissions;
        let roles: Vec<(RoleId, Permissions)> = roles
            .iter()
            .filter_map(|&id| Some((id, self.cache.role(id)?.resource().permissions)))
            .collect();
        let overwrites = channel.permission_overwrites.as_deref().unwrap_or_default();
        Some(
            PermissionCalculator::new(guild, user, everyone, &roles)
                .owner_id(owner)
                .in_channel(channel.kind, overwrites),
        )
    }

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
