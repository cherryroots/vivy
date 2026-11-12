//! Chat's own tools: what the model can look up about Discord and the time.
//! `search_messages` lives in `search.rs`, `read_recent_messages` in `recent.rs`.
//!
//! Each tool runs for the person who asked ([`Asker`]): it reads the channels
//! `util::message_links::can_read` allows for them. Results are plain text, which is what the model reads best; `get_message`
//! also shows the message's pictures.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use futures::future::join_all;
use serde_json::{Value, json};
use tracing::warn;
use twilight_model::channel::message::EmojiReactionType;
use twilight_model::gateway::presence::{Activity, ActivityType, Status};
use twilight_model::guild::scheduled_event::Status as EventStatus;

use super::{channels, recent, search};
use crate::ai::{ToolDef, ToolReply};
use crate::core::errors::is_user_error;
use crate::core::{Asker, BotCtx, Result, settings, user_error};
use crate::util::discord::{
    Channel, ChannelType, Message, Permissions, UserId, display_name, get_channel, get_member,
    get_user, member_display_name, to_chrono,
};
use crate::util::media::{self, ModelImage};
// Message links and the rule for which messages she may read live in `util`, since
// drawing opens links too. Re-exported so chat's files keep using them from here.
pub(super) use crate::util::discord::message_link;
pub(super) use crate::util::message_links::{
    can_read, find_message_links, linked_message, parse_link,
};
use crate::util::people::{find_member, parse_user_id};
use crate::util::shorten;

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
            description: "Server members' names, ID, roles, join date, timezone, online status and what they're doing right now (playing, listening to, custom status). Without `users`, the asker. Look up several people in one call.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "users": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "1 to 5 people: mentions, user IDs or names. Optional."
                    }
                },
            }),
        },
        ToolDef {
            name: "get_bot_owner",
            description: "Who your owner is: whoever owns your Discord application (the bot), with their names and IDs, and whether the asker is one of them. Use it when someone asks who owns or made you, or says they're your owner.",
            parameters: json!({"type": "object", "properties": {}}),
        },
        recent::def(),
        ToolDef {
            name: "get_message",
            description: "Opens Discord messages from their links (https://discord.com/channels/...): who wrote each, when, its text, attachments, embeds and reactions, and shows you their pictures, GIFs and videos (at most 4 in all). Use it whenever someone links a message or asks you to look at one; open several in one call. Your web tools can't open Discord links; this tool can.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "links": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "1 to 5 Discord message links."
                    }
                },
                "required": ["links"],
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
        "get_user_info" => users_info(ctx, asker, args).await,
        "get_bot_owner" => bot_owner(ctx, asker).await,
        "read_recent_messages" => recent::run(ctx, asker, args).await,
        // Chat runs it with its pictures (`run_tool_with_pictures`); this is its text alone.
        "get_message" => Ok(message_with_pictures(ctx, asker, args).await?.text),
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

/// Most people one `get_user_info` call looks up.
const MAX_USERS: usize = 5;

/// `get_user_info` for everyone the call names (`users`, or the older `user`), looked up
/// at once; the asker when it names nobody. Each one that can't be found says why.
async fn users_info(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<String> {
    let queries = list_arg(args, "users", "user");
    match queries.as_slice() {
        [] => return user_info(ctx, asker, None).await,
        [one] => return user_info(ctx, asker, Some(one)).await,
        _ => {}
    }
    let given = queries.len();
    let queries = &queries[..given.min(MAX_USERS)];
    let found = join_all(queries.iter().map(|q| user_info(ctx, asker, Some(q)))).await;
    let blocks = queries.iter().map(|q| q.to_string()).zip(found).collect();
    let mut text = several(blocks)?;
    text.push_str(&left_out_note(given, MAX_USERS, "people were looked up"));
    Ok(text)
}

/// The strings a tool call gives under `plural`, a list (or one string), or else the one
/// string under `singular`: older calls in saved conversations use that. Trimmed, without
/// blanks or repeats, in order.
pub(super) fn list_arg<'a>(args: &'a Value, plural: &str, singular: &str) -> Vec<&'a str> {
    let given: Vec<&Value> = match &args[plural] {
        Value::Array(items) => items.iter().collect(),
        Value::Null => vec![&args[singular]],
        one => vec![one],
    };
    let mut found = Vec::new();
    for text in given.into_iter().filter_map(Value::as_str).map(str::trim) {
        if !text.is_empty() && !found.contains(&text) {
            found.push(text);
        }
    }
    found
}

/// The answer of a tool given several things at once: one block per thing, its label then
/// its text, or why it failed, so one failure doesn't hide the others. Fails only when
/// every one failed.
pub(super) fn several(blocks: Vec<(String, Result<String>)>) -> Result<String> {
    let all_failed = blocks.iter().all(|(_, result)| result.is_err());
    let text = blocks
        .into_iter()
        .map(|(label, result)| match result {
            Ok(text) => format!("{label}:\n{text}"),
            Err(err) => {
                if !is_user_error(&err) {
                    warn!("a chat tool failed for {label}: {err:#}");
                }
                format!("{label}: Error: {err:#}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if all_failed {
        return Err(user_error(text));
    }
    Ok(text)
}

/// A note that only the first `max` of the `given` things were done (`what`: "links were
/// opened"), or nothing.
pub(super) fn left_out_note(given: usize, max: usize, what: &str) -> String {
    if given <= max {
        return String::new();
    }
    format!(
        "\n\n[Only the first {max} {what}; {} left out. Ask again for the rest.]",
        given - max
    )
}

async fn user_info(ctx: &BotCtx, asker: &Asker, user: Option<&str>) -> Result<String> {
    let Some(guild_id) = asker.guild else {
        // A DM has no member list to search by name, so only a mention or ID finds someone.
        let id = match user {
            None => asker.user,
            Some(query) => parse_user_id(query).ok_or_else(|| {
                user_error("In a DM I can only find people by mention or user ID.")
            })?,
        };
        let user = get_user(&ctx.http, id)
            .await
            .map_err(|_| user_error(format!("There's nobody with ID {id}.")))?;
        return Ok(format!(
            "{} (@{}, ID {})",
            display_name(&user),
            user.name,
            user.id
        ));
    };

    let member = match user {
        None => get_member(&ctx.http, guild_id, asker.user).await?,
        Some(query) => find_member(&ctx.http, guild_id, query).await?,
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

/// "[2026-10-09 14:02] Cherry: text (2 attachments: cat.png, notes.pdf)"
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
        let names: Vec<&str> = msg
            .attachments
            .iter()
            .map(|a| a.filename.as_str())
            .collect();
        let word = if names.len() == 1 {
            "attachment"
        } else {
            "attachments"
        };
        line.push_str(&format!(" ({} {word}: {})", names.len(), names.join(", ")));
    }
    for sticker in &msg.sticker_items {
        line.push_str(&format!(" [sticker: {}]", sticker.name));
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

/// A linked message as the model reads it: [`format_message`] with its whole text.
pub(super) async fn format_linked(ctx: &BotCtx, asker: &Asker, msg: &Message) -> String {
    let zone = home_zone(ctx, asker.user).await;
    format_message_up_to(ctx, msg, zone, MAX_LINKED_CHARS).await
}

/// Most messages one `get_message` call opens.
const MAX_LINKS: usize = 5;
/// At most this many pictures, GIFs or videos are shown by one `get_message` call, shared
/// between its messages ([`share_pictures`]).
const LINKED_PICTURES: usize = 4;
/// Each is shrunk to fit in this many pixels: enough to read a screenshot.
const LINKED_PICTURE_SIDE: u32 = 1024;
/// A GIF or video shows at most this many frame grids.
const LINKED_GRIDS: usize = 2;

/// `get_message`: the messages the call links (`links`, or the older `link`), opened at
/// once, with their pictures, GIFs and videos for the model to look at. A link that can't
/// be opened says why without failing the others.
pub(super) async fn message_with_pictures(
    ctx: &BotCtx,
    asker: &Asker,
    args: &Value,
) -> Result<ToolReply> {
    let links = list_arg(args, "links", "link");
    if links.is_empty() {
        return Err(user_error("`links` is required."));
    }
    let given = links.len();
    let links = &links[..given.min(MAX_LINKS)];
    let messages = join_all(links.iter().map(|link| linked_message(ctx, asker, link))).await;
    let counts: Vec<usize> = messages
        .iter()
        .map(|msg| msg.as_ref().map_or(0, |msg| media::find_media(msg).len()))
        .collect();
    let shares = share_pictures(&counts, LINKED_PICTURES);
    let shown =
        messages
            .into_iter()
            .zip(shares)
            .zip(links)
            .map(|((msg, share), link)| async move {
                let msg = msg?;
                Ok(show_message(ctx, asker, link, &msg, share).await)
            });
    let shown: Vec<Result<(String, Vec<ModelImage>)>> = join_all(shown).await;

    // One link answers as it always did: the message, or the error.
    if links.len() == 1 {
        let (text, images) = shown.into_iter().next().expect("one link")?;
        return Ok(ToolReply {
            text,
            images,
            files: Vec::new(),
        });
    }
    let mut images = Vec::new();
    let mut with_pictures = 0;
    let mut blocks = Vec::new();
    for (i, (result, link)) in shown.into_iter().zip(links).enumerate() {
        let label = format!("Message {} ({link})", i + 1);
        blocks.push((
            label,
            result.map(|(text, loaded)| {
                with_pictures += usize::from(!loaded.is_empty());
                images.extend(loaded);
                text
            }),
        ));
    }
    let mut text = several(blocks)?;
    text.push_str(&left_out_note(given, MAX_LINKS, "links were opened"));
    if with_pictures > 1 {
        text.push_str("\n\n[The pictures below are in the order of the messages.]");
    }
    Ok(ToolReply {
        text,
        images,
        files: Vec::new(),
    })
}

/// One opened message for `get_message`: its whole text with a note on its pictures, and
/// the first `share` of them loaded.
async fn show_message(
    ctx: &BotCtx,
    asker: &Asker,
    link: &str,
    msg: &Message,
    share: usize,
) -> (String, Vec<ModelImage>) {
    let mut text = format_linked(ctx, asker, msg).await;
    let found = media::find_media(msg);
    let loads = found
        .iter()
        .take(share)
        .map(|m| media::load_small(m, LINKED_PICTURE_SIDE, LINKED_GRIDS));
    let mut images = Vec::new();
    let mut failed = 0;
    for result in join_all(loads).await {
        match result {
            Ok(loaded) => images.extend(loaded),
            Err(err) => {
                warn!("couldn't load a picture of {link}: {err:#}");
                failed += 1;
            }
        }
    }
    text.push_str(&picture_note(found.len(), share, failed));
    (text, images)
}

/// How many pictures each message may show when they have `counts` pictures and the call
/// may show `budget` in all: one each in turn, so every message with pictures gets one
/// before any gets a second.
fn share_pictures(counts: &[usize], budget: usize) -> Vec<usize> {
    let mut shares = vec![0; counts.len()];
    let mut left = budget;
    loop {
        let mut gave = false;
        for (share, &count) in shares.iter_mut().zip(counts) {
            if left > 0 && *share < count {
                *share += 1;
                left -= 1;
                gave = true;
            }
        }
        if !gave {
            return shares;
        }
    }
}

/// What the model is told about a message's `found` pictures when at most `shown` are
/// shown and `failed` of those couldn't be loaded, so it doesn't answer as if it saw them
/// all. Empty without pictures.
fn picture_note(found: usize, shown: usize, failed: usize) -> String {
    if found == 0 {
        return String::new();
    }
    let word = if found == 1 { "picture" } else { "pictures" };
    let shown = found.min(shown);
    if shown == 0 {
        // The other messages of the same call used up the pictures.
        return format!("\n[{found} {word}: none shown, the call's limit was used up]");
    }
    let mut parts = vec![if shown == failed {
        "none could be shown".to_string()
    } else {
        format!("{} shown below", shown - failed)
    }];
    if failed > 0 && shown > failed {
        parts.push(format!("{failed} couldn't be loaded"));
    }
    if found > shown {
        parts.push(format!("{} more left out", found - shown));
    }
    format!("\n[{found} {word}: {}]", parts.join(", "))
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
    fn time_format() {
        let now = DateTime::from_timestamp(1_791_500_000, 0).unwrap();
        assert_eq!(
            format_time(now, Tz::Europe__Oslo),
            "Friday 2026-10-09 00:53 in Europe/Oslo (UTC+02:00). Unix time 1791500000."
        );
    }

    #[test]
    fn says_which_pictures_she_got() {
        assert_eq!(picture_note(0, 4, 0), "");
        assert_eq!(picture_note(1, 4, 0), "\n[1 picture: 1 shown below]");
        assert_eq!(
            picture_note(6, 4, 1),
            "\n[6 pictures: 3 shown below, 1 couldn't be loaded, 2 more left out]"
        );
        assert_eq!(picture_note(2, 4, 2), "\n[2 pictures: none could be shown]");
        // Other messages of the same call used up the pictures.
        assert_eq!(
            picture_note(2, 0, 0),
            "\n[2 pictures: none shown, the call's limit was used up]"
        );
    }

    #[test]
    fn list_arg_takes_a_list_one_string_or_the_old_field() {
        let args = json!({"links": [" a ", "b", "", "a", 3]});
        assert_eq!(list_arg(&args, "links", "link"), ["a", "b"]);
        let args = json!({"links": "a"});
        assert_eq!(list_arg(&args, "links", "link"), ["a"]);
        // Saved conversations replay calls made with the old single field.
        let args = json!({"link": " a "});
        assert_eq!(list_arg(&args, "links", "link"), ["a"]);
        assert!(list_arg(&json!({}), "links", "link").is_empty());
        assert!(list_arg(&json!({"links": []}), "links", "link").is_empty());
    }

    #[test]
    fn pictures_are_shared_one_each_in_turn() {
        assert_eq!(share_pictures(&[6], 4), [4]);
        assert_eq!(share_pictures(&[1, 1], 4), [1, 1]);
        assert_eq!(share_pictures(&[5, 0, 1], 4), [3, 0, 1]);
        assert_eq!(share_pictures(&[3, 3, 3, 3, 3], 4), [1, 1, 1, 1, 0]);
        assert_eq!(share_pictures(&[0, 0], 4), [0, 0]);
        assert_eq!(share_pictures(&[], 4), Vec::<usize>::new());
    }

    #[test]
    fn several_answers_keep_going_past_a_failure() {
        let text = several(vec![
            ("a".to_string(), Ok("found a".to_string())),
            ("b".to_string(), Err(user_error("no b"))),
        ])
        .unwrap();
        assert_eq!(text, "a:\nfound a\n\nb: Error: no b");
        let err = several(vec![("b".to_string(), Err(user_error("no b")))]).unwrap_err();
        assert_eq!(err.to_string(), "b: Error: no b");
    }

    #[test]
    fn left_out_note_only_past_the_cap() {
        assert_eq!(left_out_note(5, 5, "links were opened"), "");
        assert_eq!(
            left_out_note(7, 5, "links were opened"),
            "\n\n[Only the first 5 links were opened; 2 left out. Ask again for the rest.]"
        );
    }
}
