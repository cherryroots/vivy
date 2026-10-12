//! Channel mentions ("<#123>") and channel links in message text, given names the model
//! can read. serenity's `content_safe` only knows the server's channels, so it called
//! every thread and forum post "#deleted-channel". Here threads are found too: from the
//! cache, then from Discord (archived threads aren't in the cache).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;

use serenity::all::{Channel, ChannelId, ChannelType, ContentSafeOptions, GuildChannel, Message};
use serenity::http::HttpError;
use tracing::warn;

use crate::core::BotCtx;

/// A channel named somewhere in a message.
#[derive(Debug, PartialEq)]
struct Found {
    channel: ChannelId,
    /// The bytes of the mention or link.
    span: Range<usize>,
    /// A mention is replaced by the name; a link keeps its text and gets the name after it.
    link: bool,
}

/// How `content_safe` should treat `msg`: people by their server nickname when the cache
/// has them, and channel mentions left for [`name_channels`].
pub fn mention_options(msg: &Message) -> ContentSafeOptions {
    let options = ContentSafeOptions::default().clean_channel(false);
    match msg.guild_id {
        Some(guild) => options.display_as_member_from(guild),
        None => options,
    }
}

/// `text` with "<#123>" replaced by "#name", and " (#name)" after each link to a channel
/// (not to a message: those are read with `read_message_link`).
pub async fn name_channels(ctx: &BotCtx, text: &str) -> String {
    let found = find_channels(text);
    if found.is_empty() {
        return text.to_string();
    }
    let mut names: HashMap<ChannelId, String> = HashMap::new();
    for f in &found {
        if let Entry::Vacant(slot) = names.entry(f.channel) {
            slot.insert(describe(ctx, f.channel).await);
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

/// What the model reads for a channel: "#general", "#clips (a thread in #art)",
/// "#my-game (a post in #gamedev)", or a note when it can't be named.
pub async fn describe(ctx: &BotCtx, channel: ChannelId) -> String {
    let found = match cached(ctx, channel) {
        Some(found) => found,
        None => match ctx.http.get_channel(channel).await {
            Ok(Channel::Guild(found)) => found,
            Ok(Channel::Private(_)) => return "a DM".to_string(),
            Ok(_) => return "#unknown-channel".to_string(),
            Err(err) => return missing(channel, &err),
        },
    };
    let is_thread = matches!(
        found.kind,
        ChannelType::PublicThread | ChannelType::PrivateThread | ChannelType::NewsThread
    );
    let parent = found
        .parent_id
        .filter(|_| is_thread)
        .and_then(|parent| cached(ctx, parent));
    match parent {
        Some(parent) if matches!(parent.kind, ChannelType::Forum) => {
            format!("#{} (a post in #{})", found.name, parent.name)
        }
        Some(parent) => format!("#{} (a thread in #{})", found.name, parent.name),
        None if is_thread => format!("#{} (a thread)", found.name),
        None => format!("#{}", found.name),
    }
}

/// A channel or thread from the cache, in any server the bot is in.
fn cached(ctx: &BotCtx, channel: ChannelId) -> Option<GuildChannel> {
    ctx.cache.guilds().into_iter().find_map(|guild| {
        let guild = ctx.cache.guild(guild)?;
        guild
            .channels
            .get(&channel)
            .or_else(|| guild.threads.iter().find(|t| t.id == channel))
            .cloned()
    })
}

/// What to call a channel Discord wouldn't give us: gone, or there but hidden from the bot.
fn missing(channel: ChannelId, err: &serenity::Error) -> String {
    const UNKNOWN_CHANNEL: isize = 10003;
    match err {
        serenity::Error::Http(HttpError::UnsuccessfulRequest(response))
            if response.error.code == UNKNOWN_CHANNEL =>
        {
            "#deleted-channel".to_string()
        }
        serenity::Error::Http(HttpError::UnsuccessfulRequest(response))
            if response.status_code.as_u16() == 403 =>
        {
            "#hidden-channel (you can't see this one)".to_string()
        }
        _ => {
            warn!("couldn't look up channel {channel}: {err}");
            "#unknown-channel (couldn't look it up)".to_string()
        }
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
            if let Some(channel) = channel_link(&rest[..len]) {
                // "<link>" and "[text](link)" keep their closing bracket before the name.
                if matches!(text[end..].chars().next(), Some('>' | ')')) {
                    end += 1;
                }
                found.push(Found {
                    channel,
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

/// The channel in "https://discord.com/channels/<server>/<channel>". `None` for message
/// links, DM links and anything else.
fn channel_link(link: &str) -> Option<ChannelId> {
    let url = reqwest::Url::parse(link).ok()?;
    let host = url.host_str()?;
    if !(host.ends_with("discord.com") || host.ends_with("discordapp.com")) {
        return None;
    }
    let parts: Vec<&str> = url.path_segments()?.filter(|p| !p.is_empty()).collect();
    let ["channels", guild, channel] = parts.as_slice() else {
        return None;
    };
    parse_id(guild)?;
    parse_id(channel)
}

/// A Discord ID. Never 0: serenity panics on an ID of 0.
fn parse_id(text: &str) -> Option<ChannelId> {
    text.parse::<u64>()
        .ok()
        .filter(|&id| id > 0)
        .map(ChannelId::new)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(ids("https://discord.com/channels/1/0").is_empty());
    }
}
