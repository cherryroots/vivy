//! Discord message links: reading them out of text, the rule for which messages Vivy may
//! open for someone, and fetching the message a link points to.
//!
//! Chat (`get_message`, `react_to_message`, links in a message, search) and drawing
//! (pictures from a linked message) all open links, so the rule lives here, in one place:
//!
//! - In a DM only that DM.
//! - In a server any channel of the same server that chat isn't turned off in by the
//!   config (`[features.chat]` gating; a thread counts as its parent channel too), whether
//!   or not the asker can see it (Cherry's rule, #149).
//! - Never another server, or someone else's DM.

use tracing::{info, warn};

use crate::core::config::Gate;
use crate::core::{Asker, BotCtx, Result, user_error};
use crate::util::discord::{
    ChannelId, GuildId, Message, MessageId, get_channel, get_message, message_link,
};

/// The (server, channel, message) of a message link. The server is `None` for DMs.
pub fn parse_link(link: &str) -> Option<(Option<u64>, u64, u64)> {
    let url = reqwest::Url::parse(link.trim_matches(['<', '>'])).ok()?;
    let host = url.host_str()?;
    // discord.com, or a part of it like ptb.discord.com (but not "notdiscord.com").
    let is_discord = ["discord.com", "discordapp.com"]
        .iter()
        .any(|site| host == *site || host.ends_with(&format!(".{site}")));
    if !is_discord {
        return None;
    }
    let parts: Vec<&str> = url.path_segments()?.collect();
    let ["channels", guild, channel, message] = parts.as_slice() else {
        return None;
    };
    // Discord IDs are never 0, and twilight panics on an ID of 0.
    let guild = if *guild == "@me" {
        None
    } else {
        Some(guild.parse::<u64>().ok().filter(|id| *id != 0)?)
    };
    let channel: u64 = channel.parse().ok().filter(|id| *id != 0)?;
    let message: u64 = message.parse().ok().filter(|id| *id != 0)?;
    Some((guild, channel, message))
}

/// The Discord message links in `text`, in order, each once. Finds them bare, as `<link>`
/// and as `[text](link)`, and writes each the plain way (`https://discord.com/channels/...`),
/// so nothing else that was in the link comes along.
pub fn find_message_links(text: &str) -> Vec<String> {
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

/// The message `link` points to, if she may read the place it's in ([`can_read`]).
/// Errors are for the model: not a link, a place she may not read, or a message that
/// couldn't be loaded.
pub async fn linked_message(ctx: &BotCtx, asker: &Asker, link: &str) -> Result<Message> {
    let (guild, channel, message) =
        parse_link(link).ok_or_else(|| user_error("That isn't a Discord message link."))?;
    let channel = ChannelId::new(channel);
    if let Some(why) = why_not_readable(ctx, asker, guild, channel).await? {
        info!(%channel, "not opening a linked message: {why}");
        return Err(user_error(why));
    }
    get_message(&ctx.http, channel, MessageId::new(message))
        .await
        .map_err(|err| {
            // Deleted, or the bot lacks View Channel or Read Message History there.
            warn!(%channel, %message, "couldn't load a linked message: {err:#}");
            user_error("I couldn't load that message. It may be deleted, or I may not have access to that channel.")
        })
}

/// Whether she may read messages in `channel` for the asker: the rule at the top of this
/// file. `guild` is the server a link names.
pub async fn can_read(
    ctx: &BotCtx,
    asker: &Asker,
    guild: Option<u64>,
    channel: ChannelId,
) -> Result<bool> {
    Ok(why_not_readable(ctx, asker, guild, channel)
        .await?
        .is_none())
}

/// Why she may not reach `channel` for the asker ([`can_read`]), or `None` when she may.
/// Each reason is its own sentence, so her answer (and the log) says which check said no.
pub async fn why_not_readable(
    ctx: &BotCtx,
    asker: &Asker,
    guild: Option<u64>,
    channel: ChannelId,
) -> Result<Option<String>> {
    let guild_id = match check_link_scope(asker.guild, asker.channel, guild, channel) {
        Ok(None) => return Ok(None),
        Ok(Some(guild_id)) => guild_id,
        Err(why) => return Ok(Some(why)),
    };
    let target = get_channel(&ctx.http, channel).await?;
    // A thread also counts as its parent channel; the parent comes from Discord, since
    // archived threads aren't in the cache. Any other channel's parent is its category,
    // which the config's channel lists don't mean.
    let parent = target.parent_id.filter(|_| target.kind.is_thread());
    Ok(check_channel_access(
        ctx.gate("chat"),
        guild_id,
        channel,
        target.guild_id,
        parent,
    ))
}

/// Checks a link before asking Discord about the channel:
/// - `Ok(None)` when the link is in the asker's own channel (no fetch needed).
/// - `Ok(Some(guild_id))` when it's another channel in the asker's server.
/// - `Err(why)` when it's already disallowed (DM asker, DM link, or another server).
fn check_link_scope(
    asker_guild: Option<GuildId>,
    asker_channel: ChannelId,
    link_guild: Option<u64>,
    channel: ChannelId,
) -> std::result::Result<Option<GuildId>, String> {
    if channel == asker_channel {
        return Ok(None);
    }
    let Some(guild_id) = asker_guild else {
        return Err("In a DM I can only read this DM.".to_string());
    };
    let Some(link_guild) = link_guild else {
        return Err("That message is in someone's DM with me.".to_string());
    };
    if guild_id.get() != link_guild {
        return Err(format!(
            "That message is in another server ({link_guild}, this one is {guild_id})."
        ));
    }
    Ok(Some(guild_id))
}

/// Checks the real channel Discord returned against the asker's server and `[features.chat]`
/// gating. The link's server part is just text, so `target_guild` must match `asker_guild`.
fn check_channel_access(
    gate: &Gate,
    asker_guild: GuildId,
    channel: ChannelId,
    target_guild: Option<GuildId>,
    thread_parent: Option<ChannelId>,
) -> Option<String> {
    if target_guild != Some(asker_guild) {
        return Some(format!(
            "The channel {channel} is in another server ({:?}).",
            target_guild.map(|g| g.get())
        ));
    }
    if !gate.allows(Some(asker_guild), channel, thread_parent) {
        let parent = thread_parent
            .map(|p| format!(" (thread of {p})"))
            .unwrap_or_default();
        return Some(format!(
            "Chat is turned off in channel {channel}{parent} by the bot's config."
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(parse_link("https://notdiscord.com/channels/1/2/3"), None);
        assert_eq!(parse_link("https://discord.com/channels/1/2"), None);
        assert_eq!(parse_link("https://discord.com/channels/1/0/0"), None);
        assert_eq!(parse_link("https://discord.com/channels/0/1/2"), None);
    }

    #[test]
    fn finds_message_links() {
        let text = "look https://discord.com/channels/1/2/3 and <https://discord.com/channels/1/2/4>, \
                    [this](https://ptb.discord.com/channels/@me/5/6?x=\"y) https://discord.com/channels/1/2/3. \
                    but not https://discord.com/channels/1/2 or https://discord.com/channels/0/1/2 \
                    or https://example.com/channels/1/2/3";
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
    fn link_scope_allows_same_channel_or_same_server_only() {
        let g1 = GuildId::new(1);
        let c1 = ChannelId::new(10);
        let c2 = ChannelId::new(20);

        // Same channel in a server or DM skips Discord lookup.
        assert_eq!(check_link_scope(Some(g1), c1, Some(1), c1), Ok(None));
        assert_eq!(check_link_scope(None, c1, None, c1), Ok(None));

        // Another channel in the same server needs Discord channel verification.
        assert_eq!(check_link_scope(Some(g1), c1, Some(1), c2), Ok(Some(g1)));

        // Cross-DM, DM link in a server, and another server are refused.
        assert!(check_link_scope(None, c1, None, c2).is_err());
        assert!(check_link_scope(None, c1, Some(1), c2).is_err());
        assert!(check_link_scope(Some(g1), c1, None, c2).is_err());
        assert!(check_link_scope(Some(g1), c1, Some(2), c2).is_err());
    }

    #[test]
    fn channel_access_checks_real_guild_and_chat_gate() {
        let g1 = GuildId::new(1);
        let g2 = GuildId::new(2);
        let allowed = ChannelId::new(10);
        let denied = ChannelId::new(20);
        let thread = ChannelId::new(30);
        let gate = Gate {
            enabled: true,
            guilds: vec![1],
            deny_guilds: vec![],
            channels: vec![],
            deny_channels: vec![20],
        };

        assert_eq!(
            check_channel_access(&gate, g1, allowed, Some(g1), None),
            None
        );
        // Spoofed guild ID in link text: Discord says the channel belongs to g2.
        assert!(check_channel_access(&gate, g1, allowed, Some(g2), None).is_some());
        // Channel explicitly denied by [features.chat].
        assert!(check_channel_access(&gate, g1, denied, Some(g1), None).is_some());
        // Thread under a denied parent channel is also refused.
        assert!(check_channel_access(&gate, g1, thread, Some(g1), Some(denied)).is_some());
    }
}
