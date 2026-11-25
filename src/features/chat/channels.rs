//! Mentions ("<@123>", "<#123>") and channel links in message text, given names the model
//! can read. Threads are found too: from the cache, then from Discord (archived threads
//! aren't in the cache).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;

use tracing::warn;

use crate::core::BotCtx;
use crate::core::config::Gate;
use crate::util::discord::{
    Channel, ChannelId, ChannelType, GuildId, Message, RoleId, UserId, display_name, error_code,
    error_status, get_channel,
};

/// A channel named somewhere in a message.
#[derive(Debug, PartialEq)]
struct Found {
    channel: ChannelId,
    /// For a channel link, the server ID written in the URL.
    guild: Option<GuildId>,
    /// The bytes of the mention or link.
    span: Range<usize>,
    /// A mention is replaced by the name; a link keeps its text and gets the name after it.
    link: bool,
}

/// The server `msg` belongs to: from the message itself, or from the cache for its channel
/// (messages fetched over Discord's REST API have no `guild_id`).
pub(super) fn message_guild(ctx: &BotCtx, msg: &Message) -> Option<GuildId> {
    msg.guild_id
        .or_else(|| ctx.cache.channel(msg.channel_id).and_then(|c| c.guild_id))
}

/// `text` with "<@123>" replaced by "@name" (their nickname in the server when it's
/// known) and "<@&123>" by "@role name", so the model sees who was mentioned. Channel
/// mentions are left for [`name_channels`].
pub fn name_people(ctx: &BotCtx, msg: &Message, text: &str) -> String {
    let guild = message_guild(ctx, msg);
    replace_mentions(text, |mentioned| match mentioned {
        Mentioned::Person(user) => person_name(ctx, msg, guild, user),
        Mentioned::Role(role) => role_name(ctx, guild, role),
    })
}

/// Who a mention in message text points at.
#[derive(Debug, PartialEq)]
enum Mentioned {
    Person(UserId),
    Role(RoleId),
}

/// `text` with each "<@123>", "<@!123>" (an old way to write it) and "<@&123>" (a role)
/// replaced by "@" and what `name` gives for it.
fn replace_mentions(text: &str, mut name: impl FnMut(Mentioned) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<@") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let (role, digits) = match after.strip_prefix('&') {
            Some(digits) => (true, digits),
            None => (false, after.strip_prefix('!').unwrap_or(after)),
        };
        let id = digits.find('>').and_then(|end| {
            let id = digits[..end].parse::<u64>().ok().filter(|&id| id > 0)?;
            Some((end, id))
        });
        match id {
            Some((end, id)) => {
                out.push('@');
                out.push_str(&name(if role {
                    Mentioned::Role(RoleId::new(id))
                } else {
                    Mentioned::Person(UserId::new(id))
                }));
                rest = &digits[end + 1..];
            }
            None => {
                out.push_str("<@");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What to call someone mentioned in `msg`.
fn person_name(ctx: &BotCtx, msg: &Message, guild: Option<GuildId>, user: UserId) -> String {
    if let Some(guild) = guild
        && let Some(member) = ctx.cache.member(guild, user)
        && let Some(nick) = member.nick()
    {
        return nick.to_string();
    }
    let mention = msg.mentions.iter().find(|m| m.id == user);
    if let Some(nick) = mention.and_then(|m| m.member.as_ref()?.nick.clone()) {
        return nick;
    }
    if let Some(cached) = ctx.cache.user(user) {
        return display_name(&cached).to_string();
    }
    match mention {
        Some(mention) => mention.name.clone(),
        None => "invalid-user".to_string(),
    }
}

/// What to call a mentioned role. Only roles in `guild` are named; roles from other
/// servers (or in DMs) stay hidden.
fn role_name(ctx: &BotCtx, guild: Option<GuildId>, role: RoleId) -> String {
    let Some(guild) = guild else {
        return "deleted-role".to_string();
    };
    match ctx.cache.role(role) {
        Some(cached) if cached.guild_id() == guild => cached.resource().name.clone(),
        _ => "deleted-role".to_string(),
    }
}

/// `text` with "<#123>" replaced by "#name", and " (#name)" after each link to a channel
/// (not to a message: those are read with `read_message_link`). Only channels in `guild`
/// that chat gating allows are named; other servers, DMs and chat-denied channels become
/// `#unknown-channel`.
pub async fn name_channels(ctx: &BotCtx, guild: Option<GuildId>, text: &str) -> String {
    let found = find_channels(text);
    if found.is_empty() {
        return text.to_string();
    }
    let mut names: HashMap<ChannelId, String> = HashMap::new();
    for f in &found {
        // A channel link that already names another server (or in a DM) is refused
        // without asking Discord.
        if f.guild.is_some_and(|link_guild| Some(link_guild) != guild) {
            names
                .entry(f.channel)
                .or_insert_with(|| "#unknown-channel".to_string());
            continue;
        }
        if let Entry::Vacant(slot) = names.entry(f.channel) {
            slot.insert(describe(ctx, guild, f.channel).await);
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for f in &found {
        let name = &names[&f.channel];
        if f.link {
            out.push_str(&text[at..f.span.end]);
            out.push_str(&format!(" ({name})"));
        } else {
            out.push_str(&text[at..f.span.start]);
            out.push_str(name);
        }
        at = f.span.end;
    }
    out.push_str(&text[at..]);
    out
}

/// What the model reads for a channel in `guild`: "#general", "#clips (a thread in #art)",
/// "#my-game (a post in #gamedev)", or a note when it can't be named. Refuses DMs,
/// channels in other servers, and channels turned off by `[features.chat]` gating.
pub async fn describe(ctx: &BotCtx, guild: Option<GuildId>, channel: ChannelId) -> String {
    let Some(guild) = guild else {
        return "#unknown-channel".to_string();
    };
    let found = match cached(ctx, channel) {
        Some(found) => found,
        None => match get_channel(&ctx.http, channel).await {
            Ok(found) => found,
            Err(err) => return missing(channel, &err),
        },
    };
    if !allows_channel(ctx.gate("chat"), guild, &found) {
        return "#unknown-channel".to_string();
    }
    let name = found.name.as_deref().unwrap_or("unknown-channel");
    let is_thread = found.kind.is_thread();
    let parent = found
        .parent_id
        .filter(|_| is_thread)
        .and_then(|parent| cached(ctx, parent));
    let parent_name = |parent: &Channel| parent.name.clone().unwrap_or_default();
    match parent {
        Some(parent) if parent.kind == ChannelType::GuildForum => {
            format!("#{name} (a post in #{})", parent_name(&parent))
        }
        Some(parent) => format!("#{name} (a thread in #{})", parent_name(&parent)),
        None if is_thread => format!("#{name} (a thread)"),
        None => format!("#{name}"),
    }
}

/// Whether `found` belongs to `guild` and is allowed by chat's `gate`. A thread checks its
/// parent channel too. Within the same server, any channel chat gating allows is fine even
/// if the asker can't see it (Cherry's rule).
fn allows_channel(gate: &Gate, guild: GuildId, found: &Channel) -> bool {
    if found.kind == ChannelType::Private || found.guild_id != Some(guild) {
        return false;
    }
    let parent = found.parent_id.filter(|_| found.kind.is_thread());
    gate.allows(Some(guild), found.id, parent)
}

/// A channel or thread from the cache.
fn cached(ctx: &BotCtx, channel: ChannelId) -> Option<Channel> {
    ctx.cache.channel(channel).map(|found| found.clone())
}

/// What to call a channel Discord wouldn't give us: gone, or there but hidden from the bot.
fn missing(channel: ChannelId, err: &anyhow::Error) -> String {
    const UNKNOWN_CHANNEL: u64 = 10003;
    if error_code(err) == Some(UNKNOWN_CHANNEL) {
        "#deleted-channel".to_string()
    } else if error_status(err) == Some(403) {
        "#hidden-channel (you can't see this one)".to_string()
    } else {
        warn!("couldn't look up channel {channel}: {err:#}");
        "#unknown-channel (couldn't look it up)".to_string()
    }
}

/// The channel mentions and channel links in `text`, in order.
fn find_channels(text: &str) -> Vec<Found> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if rest.starts_with("<#")
            && let Some(len) = rest[2..].find('>')
            && let Some(channel) = parse_id(&rest[2..2 + len])
        {
            let end = at + 2 + len + 1;
            found.push(Found {
                channel,
                guild: None,
                span: at..end,
                link: false,
            });
            at = end;
            continue;
        }
        if rest.starts_with("https://") || rest.starts_with("http://") {
            let len = rest
                .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')'))
                .unwrap_or(rest.len());
            let mut end = at + len;
            if let Some((guild, channel)) = channel_link(&rest[..len]) {
                // "<link>" and "[text](link)" keep their closing bracket before the name.
                if matches!(text[end..].chars().next(), Some('>' | ')')) {
                    end += 1;
                }
                found.push(Found {
                    channel,
                    guild: Some(guild),
                    span: at..end,
                    link: true,
                });
            }
            at = end;
            continue;
        }
        at += rest.chars().next().map_or(1, char::len_utf8);
    }
    found
}

/// The (server, channel) in "https://discord.com/channels/<server>/<channel>". `None` for
/// message links, DM links and anything else.
fn channel_link(link: &str) -> Option<(GuildId, ChannelId)> {
    let url = reqwest::Url::parse(link).ok()?;
    let host = url.host_str()?;
    // discord.com or a subdomain like ptb.discord.com (never "notdiscord.com").
    let is_discord = ["discord.com", "discordapp.com"]
        .iter()
        .any(|site| host == *site || host.ends_with(&format!(".{site}")));
    if !is_discord {
        return None;
    }
    let parts: Vec<&str> = url.path_segments()?.filter(|p| !p.is_empty()).collect();
    let ["channels", guild, channel] = parts.as_slice() else {
        return None;
    };
    let guild = GuildId::new(parse_id(guild)?.get());
    let channel = parse_id(channel)?;
    Some((guild, channel))
}

/// A Discord ID. Never 0: twilight panics on an ID of 0.
fn parse_id(text: &str) -> Option<ChannelId> {
    text.parse::<u64>()
        .ok()
        .filter(|&id| id > 0)
        .map(ChannelId::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_get_names() {
        let named = replace_mentions("hi <@1> and <@!2>, <@&3> <@x> <@4", |m| match m {
            Mentioned::Person(user) => format!("user{user}"),
            Mentioned::Role(role) => format!("role{role}"),
        });
        assert_eq!(named, "hi @user1 and @user2, @role3 <@x> <@4");
    }

    fn ids(text: &str) -> Vec<(u64, bool)> {
        find_channels(text)
            .into_iter()
            .map(|f| (f.channel.get(), f.link))
            .collect()
    }

    #[test]
    fn finds_mentions() {
        assert_eq!(ids("see <#12> and <#34>!"), [(12, false), (34, false)]);
        let found = find_channels("é <#12>");
        assert_eq!(&"é <#12>"[found[0].span.clone()], "<#12>");
    }

    #[test]
    fn skips_things_that_arent_mentions() {
        assert!(ids("<#> <#0> <#abc> <#12 <@12> #general").is_empty());
    }

    #[test]
    fn finds_channel_links() {
        assert_eq!(ids("https://discord.com/channels/1/2"), [(2, true)]);
        assert_eq!(
            ids("go to <https://ptb.discord.com/channels/1/2/>"),
            [(2, true)]
        );
        assert_eq!(
            ids("[art](https://discordapp.com/channels/1/2)"),
            [(2, true)]
        );
    }

    #[test]
    fn link_span_keeps_its_closing_bracket() {
        let text = "[art](https://discord.com/channels/1/2) ok";
        let found = find_channels(text);
        assert_eq!(&text[found[0].span.end..], " ok");
        let text = "<https://discord.com/channels/1/2>";
        assert_eq!(find_channels(text)[0].span.end, text.len());
    }

    #[test]
    fn skips_other_links() {
        assert!(ids("https://discord.com/channels/1/2/3").is_empty());
        assert!(ids("https://discord.com/channels/@me/2").is_empty());
        assert!(ids("https://example.com/channels/1/2").is_empty());
        assert!(ids("https://notdiscord.com/channels/1/2").is_empty());
        assert!(ids("https://evildiscordapp.com/channels/1/2").is_empty());
        assert!(ids("https://discord.com/channels/1/0").is_empty());
    }

    #[test]
    fn allows_channel_enforces_guild_and_chat_gating() {
        let gate: Gate = toml::from_str("deny_channels = [20]").unwrap();
        let make =
            |id: u64, kind: ChannelType, guild: Option<u64>, parent: Option<u64>| -> Channel {
                serde_json::from_value(serde_json::json!({
                    "id": id.to_string(),
                    "type": u8::from(kind),
                    "guild_id": guild.map(|g| g.to_string()),
                    "parent_id": parent.map(|p| p.to_string()),
                    "name": "chan",
                }))
                .unwrap()
            };
        let g1 = GuildId::new(1);
        // Same server, allowed by chat gating.
        assert!(allows_channel(
            &gate,
            g1,
            &make(10, ChannelType::GuildText, Some(1), None)
        ));
        // Same server, denied by chat gating.
        assert!(!allows_channel(
            &gate,
            g1,
            &make(20, ChannelType::GuildText, Some(1), None)
        ));
        // Thread whose parent is denied by chat gating.
        assert!(!allows_channel(
            &gate,
            g1,
            &make(21, ChannelType::PublicThread, Some(1), Some(20))
        ));
        // Another server.
        assert!(!allows_channel(
            &gate,
            g1,
            &make(30, ChannelType::GuildText, Some(2), None)
        ));
        // A DM channel.
        assert!(!allows_channel(
            &gate,
            g1,
            &make(40, ChannelType::Private, None, None)
        ));
    }
}
