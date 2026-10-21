//! Chat's own tools: what the model can look up about Discord and the time.
//! `search_messages` lives in `search.rs`.
//!
//! Each tool runs as the person who asked ([`Asker`]): it only reads channels that person
//! can read. Results are plain text, which is what the model reads best.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};
use twilight_model::channel::message::EmojiReactionType;
use twilight_model::gateway::presence::{Activity, ActivityType, Status};
use twilight_model::guild::scheduled_event::Status as EventStatus;

use super::{channels, search};
use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Result, settings, user_error};
use crate::util::discord::{
    Channel, ChannelId, ChannelType, GuildId, Member, Message, MessageId, Permissions, UserId,
    display_name, get_channel, get_member, get_message, get_user, member_display_name, to_chrono,
};
use crate::util::shorten;

/// Most messages `read_recent_messages` returns.
const MAX_RECENT: u64 = 50;
/// Each message is cut to this many characters in tool results.
const MAX_MESSAGE_CHARS: usize = 600;
/// A linked message is cut to this many characters: the longest a Discord message can be,
/// since someone linked it for its text.
const MAX_LINKED_CHARS: usize = 4000;
/// Most pinned messages `get_pinned_messages` returns.
const MAX_PINS: usize = 25;

pub fn defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "get_current_time",
            description: "The current date and time. Use it when the date or time matters. Uses the asker's timezone (set with /timezone) unless one is given. Don't mention the time unless asked or clearly needed; when you do, say it naturally.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "timezone": {"type": "string", "description": "An IANA timezone like Europe/Oslo. Optional."}
                },
            }),
        },
        ToolDef {
            name: "get_channel_info",
            description: "The current channel's name, topic and type, its parent channel or category, and the server name.",
            parameters: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "list_channels",
            description: "Every channel in this server the asker can see, by category, with each channel's topic. Use it to learn what the channels are for.",
            parameters: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "get_user_info",
            description: "A server member's names, ID, roles, join date, timezone, online status and what they're doing right now (playing, listening to, custom status). Without `user`, the asker.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "user": {"type": "string", "description": "A mention, user ID or name. Optional."}
                },
            }),
        },
        ToolDef {
            name: "get_bot_owner",
            description: "Who your owner is: whoever owns your Discord application (the bot), with their names and IDs, and whether the asker is one of them. Use it when someone asks who owns or made you, or says they're your owner.",
            parameters: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "read_recent_messages",
            description: "The messages in this channel just before the one you're answering, oldest first. Use it to see what was said before.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "count": {"type": "integer", "description": "How many, 1 to 50. Default 20."}
                },
            }),
        },
        ToolDef {
            name: "get_message",
            description: "Opens a Discord message from its link (https://discord.com/channels/...): who wrote it, when, its text, attachments, embeds and reactions. Use it whenever someone links a message or asks you to look at one. Your web tools can't open Discord links; this tool can.",
            parameters: json!({
                "type": "object",
                "properties": {"link": {"type": "string"}},
                "required": ["link"],
            }),
        },
        search::def(),
        ToolDef {
            name: "get_pinned_messages",
            description: "The pinned messages of the current channel, newest first, with links.",
            parameters: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "list_server_events",
            description: "The server's upcoming and ongoing scheduled events: name, time, place, description and how many are interested.",
            parameters: json!({"type": "object", "properties": {}}),
        },
    ]
}

pub async fn run(ctx: &BotCtx, asker: &Asker, name: &str, args: &Value) -> Result<String> {
    let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    match name {
        "get_current_time" => current_time(ctx, asker, text("timezone")).await,
        "get_channel_info" => channel_info(ctx, asker).await,
        "list_channels" => list_channels(ctx, asker).await,
        "get_user_info" => user_info(ctx, asker, text("user")).await,
        "get_bot_owner" => bot_owner(ctx, asker).await,
        "read_recent_messages" => {
            let count = args["count"].as_u64().unwrap_or(20).clamp(1, MAX_RECENT);
            recent_messages(ctx, asker, count).await
        }
        "get_message" => {
            let link = text("link").ok_or_else(|| user_error("`link` is required."))?;
            message_from_link(ctx, asker, link).await
        }
        "search_messages" => search::run(ctx, asker, args).await,
        "get_pinned_messages" => pinned_messages(ctx, asker).await,
        "list_server_events" => server_events(ctx, asker).await,
        _ => Err(user_error(format!("chat has no tool named {name}"))),
    }
}

/// The asker's timezone, or UTC.
pub(super) async fn home_zone(ctx: &BotCtx, user: UserId) -> Tz {
    settings::timezone(&ctx.db, user)
        .await
        .ok()
        .flatten()
        .unwrap_or(Tz::UTC)
}

async fn current_time(ctx: &BotCtx, asker: &Asker, zone: Option<&str>) -> Result<String> {
    let zone = match zone {
        Some(name) => Tz::from_str_insensitive(name)
            .map_err(|_| user_error(format!("{name} isn't a timezone I know.")))?,
        None => home_zone(ctx, asker.user).await,
    };
    Ok(format_time(Utc::now(), zone))
}

fn format_time(now: DateTime<Utc>, zone: Tz) -> String {
    let local = now.with_timezone(&zone);
    format!(
        "{} in {} (UTC{}). Unix time {}.",
        local.format("%A %Y-%m-%d %H:%M"),
        zone.name(),
        local.format("%:z"),
        now.timestamp()
    )
}

async fn channel_info(ctx: &BotCtx, asker: &Asker) -> Result<String> {
    let channel = get_channel(&ctx.http, asker.channel).await?;
    let Some(guild_id) = channel.guild_id else {
        return Ok("A direct message conversation.".to_string());
    };
    let mut lines = vec![format!(
        "Channel: #{} ({})",
        channel.name.as_deref().unwrap_or_default(),
        kind_name(channel.kind)
    )];
    if let Some(topic) = channel.topic.as_deref().filter(|t| !t.is_empty()) {
        lines.push(format!("Topic: {topic}"));
    }
    if let Some(parent) = channel.parent_id
        && let Ok(parent) = get_channel(&ctx.http, parent).await
    {
        let what = if channel.kind.is_thread() {
            "In channel"
        } else {
            "In category"
        };
        lines.push(format!("{what}: {}", parent.name.unwrap_or_default()));
    }
    if let Some(guild) = ctx.cache.guild(guild_id) {
        lines.push(format!("Server: {}", guild.name()));
    }
    Ok(lines.join("\n"))
}

/// One channel in [`list_channels`], with only what the list shows.
struct ChannelEntry {
    id: u64,
    name: String,
    kind: ChannelType,
    topic: Option<String>,
    parent: Option<u64>,
    position: i32,
}

async fn list_channels(ctx: &BotCtx, asker: &Asker) -> Result<String> {
    let Some(guild_id) = asker.guild else {
        return Ok("A direct message conversation has no channels.".to_string());
    };
    let member = get_member(&ctx.http, guild_id, asker.user).await?;
    let server = ctx
        .cache
        .guild(guild_id)
        .map(|guild| guild.name().to_string())
        .ok_or_else(|| user_error("I don't know this server yet."))?;
    let can_view = |c: &Channel| {
        ctx.permissions_in(guild_id, asker.user, &member.roles, c)
            .is_some_and(|p| p.contains(Permissions::VIEW_CHANNEL))
    };
    let entries: Vec<ChannelEntry> = ctx
        .guild_channels(guild_id)
        .into_iter()
        .filter(|c| !c.kind.is_thread())
        .filter(|c| c.kind == ChannelType::GuildCategory || can_view(c))
        .map(|c| ChannelEntry {
            id: c.id.get(),
            name: c.name.unwrap_or_default(),
            kind: c.kind,
            topic: c.topic.filter(|t| !t.trim().is_empty()),
            parent: c.parent_id.map(|p| p.get()),
            position: c.position.unwrap_or_default(),
        })
        .collect();
    Ok(format!(
        "Server: {server}\n{}",
        render_channels(&entries, asker.channel.get())
    ))
}

/// Channels without a category first, then each category with its channels, in Discord's
/// order. Categories the asker sees nothing in are left out.
fn render_channels(entries: &[ChannelEntry], here: u64) -> String {
    let by_position = |list: &mut Vec<&ChannelEntry>| list.sort_by_key(|c| (c.position, c.id));
    let line = |c: &ChannelEntry| {
        let mut text = format!("#{} ({})", c.name, kind_name(c.kind));
        if c.id == here {
            text.push_str(" [you are here]");
        }
        if let Some(topic) = &c.topic {
            text.push_str(&format!(": {}", shorten(&topic.replace('\n', " "), 200)));
        }
        text
    };
    let in_category = |parent: Option<u64>| {
        let mut list: Vec<&ChannelEntry> = entries
            .iter()
            .filter(|c| c.kind != ChannelType::GuildCategory && c.parent == parent)
            .collect();
        by_position(&mut list);
        list
    };
    let mut lines: Vec<String> = in_category(None).into_iter().map(line).collect();
    let mut categories: Vec<&ChannelEntry> = entries
        .iter()
        .filter(|c| c.kind == ChannelType::GuildCategory)
        .collect();
    by_position(&mut categories);
    for category in categories {
        let channels = in_category(Some(category.id));
        if channels.is_empty() {
            continue;
        }
        lines.push(format!("Category {}:", category.name));
        lines.extend(channels.into_iter().map(|c| format!("  {}", line(c))));
    }
    lines.join("\n")
}

fn kind_name(kind: ChannelType) -> &'static str {
    match kind {
        ChannelType::GuildText => "text channel",
        ChannelType::GuildVoice => "voice channel",
        ChannelType::GuildAnnouncement => "announcement channel",
        ChannelType::GuildStageVoice => "stage channel",
        ChannelType::GuildForum => "forum",
        kind if kind.is_thread() => "thread",
        _ => "channel",
    }
}

async fn user_info(ctx: &BotCtx, asker: &Asker, user: Option<&str>) -> Result<String> {
    let Some(guild_id) = asker.guild else {
        let user = get_user(&ctx.http, asker.user).await?;
        return Ok(format!(
            "{} (@{}, ID {})",
            display_name(&user),
            user.name,
            user.id
        ));
    };

    let member = match user {
        None => get_member(&ctx.http, guild_id, asker.user).await?,
        Some(query) => find_member(ctx, guild_id, query).await?,
    };

    // Presences come from the gateway (the Presence intent), so only the cache has them.
    let roles: Vec<String> = member
        .roles
        .iter()
        .filter_map(|&id| ctx.cache.role(id).map(|r| r.resource().name.clone()))
        .collect();
    let presence = match ctx.cache.presence(guild_id, member.user.id) {
        Some(presence) => presence_lines(Some((presence.status(), presence.activities()))),
        None => presence_lines(None),
    };
    let zone = settings::timezone(&ctx.db, member.user.id).await?;
    let mut lines = vec![format!(
        "{} (@{}, ID {})",
        member_display_name(&member),
        member.user.name,
        member.user.id
    )];
    if let Some(joined) = member.joined_at {
        lines.push(format!("Joined: {}", to_chrono(joined).format("%Y-%m-%d")));
    }
    if !roles.is_empty() {
        lines.push(format!("Roles: {}", roles.join(", ")));
    }
    lines.push(match zone {
        Some(zone) => format!("Timezone: {}", zone.name()),
        None => "Timezone: not set".to_string(),
    });
    lines.extend(presence);
    Ok(lines.join("\n"))
}

/// One line per owner of the bot's Discord application, then whether the asker is one.
async fn bot_owner(ctx: &BotCtx, asker: &Asker) -> Result<String> {
    if ctx.app_owners.is_empty() {
        return Ok("Nobody is set as your owner.".to_string());
    }
    let mut lines = Vec::new();
    for &user in ctx.app_owners.iter() {
        lines.push(match get_user(&ctx.http, user).await {
            Ok(found) => format!("{} (@{}, ID {user})", display_name(&found), found.name),
            Err(_) => format!("ID {user}"),
        });
    }
    lines.push(if ctx.is_owner(asker.user) {
        "The asker is your owner.".to_string()
    } else {
        "The asker isn't your owner.".to_string()
    });
    Ok(lines.join("\n"))
}

/// "Status: online" and one line per activity. Discord only sends presences for people
/// who are online, so no presence means offline.
fn presence_lines(presence: Option<(Status, &[Activity])>) -> Vec<String> {
    let Some((status, activities)) = presence else {
        return vec!["Status: offline".to_string()];
    };
    let status = match status {
        Status::Online => "online",
        Status::Idle => "idle",
        Status::DoNotDisturb => "do not disturb",
        // Invisible people look offline to everyone else, so say the same.
        _ => "offline",
    };
    let mut lines = vec![format!("Status: {status}")];
    lines.extend(activities.iter().filter_map(activity_line));
    lines
}

/// "Playing Minecraft (Survival, In a world)", "Custom status: 🍜 lunch", …
fn activity_line(activity: &Activity) -> Option<String> {
    if activity.kind == ActivityType::Custom {
        // The name is always "Custom Status"; the text is in `state`.
        let emoji = activity.emoji.as_ref().map(|e| e.name.as_str());
        let text: Vec<&str> = emoji.into_iter().chain(activity.state.as_deref()).collect();
        return (!text.is_empty()).then(|| format!("Custom status: {}", text.join(" ")));
    }
    let verb = match activity.kind {
        ActivityType::Playing => "Playing",
        ActivityType::Streaming => "Streaming",
        ActivityType::Listening => "Listening to",
        ActivityType::Watching => "Watching",
        ActivityType::Competing => "Competing in",
        _ => "Doing",
    };
    // For Spotify, details is the song and state the artist.
    let extra: Vec<&str> = [activity.details.as_deref(), activity.state.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    let mut line = format!("{verb} {}", activity.name);
    if !extra.is_empty() {
        line.push_str(&format!(" ({})", extra.join(", ")));
    }
    Some(line)
}

/// A server member from a mention, user ID or name.
pub(super) async fn find_member(ctx: &BotCtx, guild: GuildId, query: &str) -> Result<Member> {
    match parse_user_id(query) {
        Some(id) => get_member(&ctx.http, guild, id)
            .await
            .map_err(|_| user_error(format!("Nobody with ID {id} is in this server."))),
        None => ctx
            .http
            .search_guild_members(guild, query)
            .limit(5)
            .await?
            .models()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| user_error(format!("No member matches \"{query}\"."))),
    }
}

/// "<@123>", "<@!123>" or "123" → the ID.
pub(super) fn parse_user_id(text: &str) -> Option<UserId> {
    let digits = text
        .trim_start_matches("<@")
        .trim_start_matches('!')
        .trim_end_matches('>');
    digits
        .parse::<u64>()
        .ok()
        .filter(|&id| id > 0)
        .map(UserId::new)
}

async fn recent_messages(ctx: &BotCtx, asker: &Asker, count: u64) -> Result<String> {
    let mut messages = ctx
        .http
        .channel_messages(asker.channel)
        .before(asker.message)
        .limit(count as u16)
        .await?
        .models()
        .await?;
    if messages.is_empty() {
        return Ok("There are no earlier messages.".to_string());
    }
    // Discord returns newest first.
    messages.reverse();
    let zone = home_zone(ctx, asker.user).await;
    let mut lines = Vec::new();
    for m in &messages {
        lines.push(format_message(ctx, m, zone).await);
    }
    Ok(lines.join("\n"))
}

/// "[2026-10-09 14:02] Cherry: text (2 attachments)"
pub(super) async fn format_message(ctx: &BotCtx, msg: &Message, zone: Tz) -> String {
    format_message_up_to(ctx, msg, zone, MAX_MESSAGE_CHARS).await
}

/// [`format_message`] with the text cut to `max_chars` characters.
async fn format_message_up_to(ctx: &BotCtx, msg: &Message, zone: Tz, max_chars: usize) -> String {
    let time = DateTime::from_timestamp(msg.timestamp.as_secs(), 0)
        .unwrap_or_default()
        .with_timezone(&zone)
        .format("%Y-%m-%d %H:%M");
    let name = msg
        .member
        .as_ref()
        .and_then(|m| m.nick.as_deref())
        .unwrap_or(display_name(&msg.author));
    let text = channels::name_people(ctx, msg, &msg.content);
    let text = channels::name_channels(ctx, &text).await;
    let mut line = format!("[{time}] {name}: {}", shorten(&text, max_chars));
    if !msg.attachments.is_empty() {
        line.push_str(&format!(" ({} attachments)", msg.attachments.len()));
    }
    for embed in &msg.embeds {
        if let Some(title) = &embed.title {
            line.push_str(&format!(" [embed: {}]", shorten(title, 100)));
        }
    }
    let reactions: Vec<(String, u64, bool)> = msg
        .reactions
        .iter()
        .map(|r| {
            let emoji = match &r.emoji {
                EmojiReactionType::Custom {
                    name: Some(name), ..
                } => format!(":{name}:"),
                EmojiReactionType::Custom { id, .. } => format!("<:_:{id}>"),
                EmojiReactionType::Unicode { name } => name.clone(),
            };
            (emoji, r.count, r.me)
        })
        .collect();
    line.push_str(&reaction_text(&reactions));
    line
}

/// " [reactions: 😂 3, :pog: 1 (one is yours)]", or nothing without reactions. Her own
/// reactions are marked, since some come from chiming in and she may not remember them.
fn reaction_text(reactions: &[(String, u64, bool)]) -> String {
    if reactions.is_empty() {
        return String::new();
    }
    let list: Vec<String> = reactions
        .iter()
        .map(|(emoji, count, mine)| {
            let mine = if *mine { " (one is yours)" } else { "" };
            format!("{emoji} {count}{mine}")
        })
        .collect();
    format!(" [reactions: {}]", list.join(", "))
}

/// The link that opens a message in Discord.
pub(super) fn message_link(
    guild: Option<GuildId>,
    channel: ChannelId,
    message: MessageId,
) -> String {
    let guild = guild.map_or("@me".to_string(), |g| g.to_string());
    format!("https://discord.com/channels/{guild}/{channel}/{message}")
}

/// The (server, channel, message) of a message link. The server is `None` for DMs.
pub(super) fn parse_link(link: &str) -> Option<(Option<u64>, u64, u64)> {
    let url = reqwest::Url::parse(link.trim_matches(['<', '>'])).ok()?;
    let host = url.host_str()?;
    if !(host.ends_with("discord.com") || host.ends_with("discordapp.com")) {
        return None;
    }
    let parts: Vec<&str> = url.path_segments()?.collect();
    let ["channels", guild, channel, message] = parts.as_slice() else {
        return None;
    };
    let guild = if *guild == "@me" {
        None
    } else {
        Some(guild.parse().ok()?)
    };
    // Discord IDs are never 0, and twilight panics on an ID of 0.
    let channel: u64 = channel.parse().ok().filter(|id| *id != 0)?;
    let message: u64 = message.parse().ok().filter(|id| *id != 0)?;
    Some((guild, channel, message))
}

/// The Discord message links in `text`, in order, each once. Finds them bare, as `<link>`
/// and as `[text](link)`, and writes each the plain way (`https://discord.com/channels/...`),
/// so nothing else that was in the link comes along.
pub(super) fn find_message_links(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let words =
        text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')' | '[' | ']'));
    for word in words {
        // "look at https://discord.com/channels/1/2/3!" ends a sentence with the link.
        let word = word.trim_end_matches(['.', ',', '!', '?', ':', ';']);
        let Some((guild, channel, message)) = parse_link(word) else {
            continue;
        };
        let link = message_link(
            guild.map(GuildId::new),
            ChannelId::new(channel),
            MessageId::new(message),
        );
        if !found.contains(&link) {
            found.push(link);
        }
    }
    found
}

/// The message `link` points to, if the asker can read the place it's in.
pub(super) async fn linked_message(ctx: &BotCtx, asker: &Asker, link: &str) -> Result<Message> {
    let (guild, channel, message) =
        parse_link(link).ok_or_else(|| user_error("That isn't a Discord message link."))?;
    let channel = ChannelId::new(channel);
    if !can_read(ctx, asker, guild, channel).await? {
        return Err(user_error(
            "That message is in a place the asker can't read.",
        ));
    }
    get_message(&ctx.http, channel, MessageId::new(message))
        .await
        .map_err(|_| user_error("I couldn't load that message. It may be deleted."))
}

/// A linked message as the model reads it: [`format_message`] with its whole text.
pub(super) async fn format_linked(ctx: &BotCtx, asker: &Asker, msg: &Message) -> String {
    let zone = home_zone(ctx, asker.user).await;
    format_message_up_to(ctx, msg, zone, MAX_LINKED_CHARS).await
}

async fn message_from_link(ctx: &BotCtx, asker: &Asker, link: &str) -> Result<String> {
    let msg = linked_message(ctx, asker, link).await?;
    Ok(format_linked(ctx, asker, &msg).await)
}

/// Whether the asker may read messages in `channel`. In DMs only the DM itself; in a
/// server, channels in the same server where they can view and read history.
pub(super) async fn can_read(
    ctx: &BotCtx,
    asker: &Asker,
    guild: Option<u64>,
    channel: ChannelId,
) -> Result<bool> {
    if channel == asker.channel {
        return Ok(true);
    }
    let (Some(guild_id), Some(link_guild)) = (asker.guild, guild) else {
        return Ok(false);
    };
    if guild_id.get() != link_guild {
        return Ok(false);
    }
    let target = get_channel(&ctx.http, channel).await?;
    // The link's server part is just text, so check the channel really is in the
    // asker's server. Otherwise a link could reach into another server.
    if target.guild_id != Some(guild_id) {
        return Ok(false);
    }
    // Threads use their parent channel's permissions. Private threads also need
    // membership, which isn't checked here, so they're refused.
    let target = match target.kind {
        ChannelType::PrivateThread => return Ok(false),
        ChannelType::PublicThread | ChannelType::AnnouncementThread => {
            let Some(parent) = target.parent_id else {
                return Ok(false);
            };
            get_channel(&ctx.http, parent).await?
        }
        _ => target,
    };
    let member = get_member(&ctx.http, guild_id, asker.user).await?;
    let Some(permissions) = ctx.permissions_in(guild_id, asker.user, &member.roles, &target) else {
        return Ok(false);
    };
    Ok(permissions.contains(Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY))
}

async fn pinned_messages(ctx: &BotCtx, asker: &Asker) -> Result<String> {
    let pins: Vec<Message> = ctx
        .http
        .pins(asker.channel)
        .limit(MAX_PINS as u16)
        .await?
        .model()
        .await?
        .items
        .into_iter()
        .map(|pin| pin.message)
        .collect();
    if pins.is_empty() {
        return Ok("This channel has no pinned messages.".to_string());
    }
    let zone = home_zone(ctx, asker.user).await;
    let mut lines = Vec::new();
    for m in pins.iter().take(MAX_PINS) {
        lines.push(format!(
            "{} {}",
            format_message(ctx, m, zone).await,
            message_link(asker.guild, m.channel_id, m.id)
        ));
    }
    Ok(lines.join("\n"))
}

async fn server_events(ctx: &BotCtx, asker: &Asker) -> Result<String> {
    let Some(guild) = asker.guild else {
        return Ok("Direct messages have no server events.".to_string());
    };
    let mut events = ctx
        .http
        .guild_scheduled_events(guild)
        .with_user_count(true)
        .await?
        .models()
        .await?;
    events.retain(|e| matches!(e.status, EventStatus::Scheduled | EventStatus::Active));
    if events.is_empty() {
        return Ok("The server has no upcoming events.".to_string());
    }
    events.sort_by_key(|e| e.scheduled_start_time.as_micros());
    let zone = home_zone(ctx, asker.user).await;
    let local = |timestamp: i64| {
        DateTime::from_timestamp(timestamp, 0)
            .unwrap_or_default()
            .with_timezone(&zone)
            .format("%A %Y-%m-%d %H:%M")
            .to_string()
    };
    let mut blocks = Vec::new();
    for event in &events {
        let start = event.scheduled_start_time.as_secs();
        let mut lines = vec![format!("{} (ID {})", event.name, event.id)];
        let mut when = format!("Starts: {} {} (<t:{start}:F>)", local(start), zone.name());
        if let Some(end) = event.scheduled_end_time {
            when.push_str(&format!(", ends {}", local(end.as_secs())));
        }
        if event.status == EventStatus::Active {
            when.push_str(", happening now");
        }
        lines.push(when);
        if let Some(channel) = event.channel_id {
            lines.push(format!("Where: {}", channels::describe(ctx, channel).await));
        } else if let Some(place) = event
            .entity_metadata
            .as_ref()
            .and_then(|m| m.location.clone())
        {
            lines.push(format!("Where: {place}"));
        }
        if let Some(count) = event.user_count {
            lines.push(format!("Interested: {count}"));
        }
        if let Some(description) = event.description.as_deref().filter(|d| !d.is_empty()) {
            lines.push(format!("Description: {}", shorten(description, 300)));
        }
        blocks.push(lines.join("\n"));
    }
    Ok(blocks.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence(status: &str, activities: Value) -> (Status, Vec<Activity>) {
        (
            serde_json::from_value(json!(status)).unwrap(),
            serde_json::from_value(activities).unwrap(),
        )
    }

    #[test]
    fn presence_status_and_activities() {
        assert_eq!(presence_lines(None), ["Status: offline"]);
        let (status, activities) = presence("invisible", json!([]));
        assert_eq!(
            presence_lines(Some((status, &activities))),
            ["Status: offline"]
        );

        let (status, activities) = presence(
            "dnd",
            json!([
                {"type": 4, "name": "Custom Status", "state": "lunch", "emoji": {"name": "🍜"}, "created_at": 0},
                {"type": 2, "name": "Spotify", "details": "Song", "state": "Artist", "created_at": 0},
                {"type": 0, "name": "Minecraft", "created_at": 0},
                {"type": 4, "name": "Custom Status", "created_at": 0}
            ]),
        );
        assert_eq!(
            presence_lines(Some((status, &activities))),
            [
                "Status: do not disturb",
                "Custom status: 🍜 lunch",
                "Listening to Spotify (Song, Artist)",
                "Playing Minecraft",
            ]
        );
    }

    #[test]
    fn reactions_after_the_message() {
        assert_eq!(reaction_text(&[]), "");
        let list = [("😂".to_string(), 3, false), (":pog:".to_string(), 1, true)];
        assert_eq!(
            reaction_text(&list),
            " [reactions: 😂 3, :pog: 1 (one is yours)]"
        );
    }

    #[test]
    fn channels_by_category() {
        let entry = |id, name: &str, kind, topic: Option<&str>, parent, position| ChannelEntry {
            id,
            name: name.to_string(),
            kind,
            topic: topic.map(str::to_string),
            parent,
            position,
        };
        let entries = vec![
            entry(1, "Voice", ChannelType::GuildCategory, None, None, 2),
            entry(2, "Text", ChannelType::GuildCategory, None, None, 1),
            entry(3, "Empty", ChannelType::GuildCategory, None, None, 3),
            entry(4, "lounge", ChannelType::GuildVoice, None, Some(1), 0),
            entry(
                5,
                "memes",
                ChannelType::GuildText,
                Some("only\nmemes"),
                Some(2),
                1,
            ),
            entry(6, "general", ChannelType::GuildText, None, Some(2), 0),
            entry(7, "rules", ChannelType::GuildText, Some("read me"), None, 0),
        ];
        assert_eq!(
            render_channels(&entries, 6),
            "#rules (text channel): read me
Category Text:
  #general (text channel) [you are here]
  #memes (text channel): only memes
Category Voice:
  #lounge (voice channel)"
        );
    }

    #[test]
    fn message_links() {
        assert_eq!(
            parse_link("https://discord.com/channels/1/2/3"),
            Some((Some(1), 2, 3))
        );
        assert_eq!(
            parse_link("<https://ptb.discord.com/channels/@me/2/3>"),
            Some((None, 2, 3))
        );
        assert_eq!(
            parse_link("https://discordapp.com/channels/1/2/3"),
            Some((Some(1), 2, 3))
        );
        assert_eq!(parse_link("https://example.com/channels/1/2/3"), None);
        assert_eq!(parse_link("https://discord.com/channels/1/2"), None);
        assert_eq!(parse_link("https://discord.com/channels/1/0/0"), None);
    }

    #[test]
    fn finds_message_links() {
        let text = "look https://discord.com/channels/1/2/3 and <https://discord.com/channels/1/2/4>, \
                    [this](https://ptb.discord.com/channels/@me/5/6?x=\"y) https://discord.com/channels/1/2/3. \
                    but not https://discord.com/channels/1/2 or https://example.com/channels/1/2/3";
        assert_eq!(
            find_message_links(text),
            [
                "https://discord.com/channels/1/2/3",
                "https://discord.com/channels/1/2/4",
                "https://discord.com/channels/@me/5/6",
            ]
        );
        assert!(find_message_links("no links here").is_empty());
    }

    #[test]
    fn links_to_messages() {
        let link = message_link(Some(GuildId::new(1)), ChannelId::new(2), MessageId::new(3));
        assert_eq!(link, "https://discord.com/channels/1/2/3");
        assert_eq!(parse_link(&link), Some((Some(1), 2, 3)));
    }

    #[test]
    fn user_ids() {
        assert_eq!(parse_user_id("<@123>"), Some(UserId::new(123)));
        assert_eq!(parse_user_id("<@!123>"), Some(UserId::new(123)));
        assert_eq!(parse_user_id("123"), Some(UserId::new(123)));
        assert_eq!(parse_user_id("cherry"), None);
    }

    #[test]
    fn time_format() {
        let now = DateTime::from_timestamp(1_791_500_000, 0).unwrap();
        assert_eq!(
            format_time(now, Tz::Europe__Oslo),
            "Friday 2026-10-09 00:53 in Europe/Oslo (UTC+02:00). Unix time 1791500000."
        );
    }
}
