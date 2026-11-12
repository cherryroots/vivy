//! `read_recent_messages`: a stretch of a channel's messages, oldest first.
//!
//! Without options it reads the messages just before the one she's answering, in this
//! channel. With them she can read:
//!
//! - another channel or thread (`channel`), its latest messages;
//! - the conversation around a message (`around` a link), with the linked one marked,
//!   e.g. after a search hit or a linked message, which come without their context;
//! - the page before or after a message (`before`/`after` a link), to read further back
//!   or forward.
//!
//! Every place has to pass `tools::can_read`, like `get_message` and `search_messages`.

use serde_json::{Value, json};
use tracing::warn;

use super::search::find_channel;
use super::tools::{can_read, format_message, home_zone, message_link, parse_link};
use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Result, user_error};
use crate::util::discord::{ChannelId, Message, MessageId};

/// Most messages it returns.
const MAX_RECENT: u64 = 50;
/// How many when the model doesn't say.
const DEFAULT_COUNT: u64 = 20;
/// Marks the linked message in an `around` read.
const LINKED_MARK: &str = "→ ";

pub fn def() -> ToolDef {
    let link = |what: &str| json!({"type": "string", "description": what});
    ToolDef {
        name: "read_recent_messages",
        description: "Reads messages, oldest first. Without options: the ones in this channel just before the one you're answering. `channel` reads another channel or thread's latest messages. `around` a message link shows the conversation around it, like a search result or a linked message. `before`/`after` a link reads the page before or after it. Use at most one of `around`, `before` and `after`; the link decides the channel.",
        parameters: json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer", "description": "How many, 1 to 50. Default 20."},
                "channel": {"type": "string", "description": "A channel or thread: mention, ID or name. Optional, default here."},
                "around": link("A message link: the messages around it, that one marked with →."),
                "before": link("A message link: the messages just before it."),
                "after": link("A message link: the messages just after it."),
            },
        }),
    }
}

/// Which messages to read, from the tool's options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// The messages just before the one she's answering. The default, here only.
    BeforeQuestion,
    /// The newest messages of another channel.
    Latest,
    Around(u64),
    Before(u64),
    After(u64),
}

pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<String> {
    let count = args["count"]
        .as_u64()
        .unwrap_or(DEFAULT_COUNT)
        .clamp(1, MAX_RECENT);
    let (read, channel) = where_to_read(ctx, asker, args).await?;

    let messages = fetch(ctx, asker, channel, read, count as u16)
        .await
        .map_err(|err| {
            warn!(%channel, "couldn't read messages: {err:#}");
            user_error("I couldn't read that channel. I may not have access to it.")
        })?;
    if messages.is_empty() {
        return Ok(nothing_found(read).to_string());
    }

    let zone = home_zone(ctx, asker.user).await;
    let link = |m: &Message| message_link(asker.guild, channel, m.id);
    let mut lines = Vec::new();
    if read != Read::BeforeQuestion {
        lines.push(header(read, &channel_label(ctx, asker, channel)));
    }
    for m in &messages {
        let mut line = format_message(ctx, m, zone).await;
        if read == Read::Around(m.id.get()) {
            line.insert_str(0, LINKED_MARK);
        }
        // The default read stays as it always was; elsewhere each message has its link,
        // so she can point people at it.
        if read != Read::BeforeQuestion {
            line.push(' ');
            line.push_str(&link(m));
        }
        lines.push(line);
    }
    let oldest = messages.first().map(link).unwrap_or_default();
    let newest = messages.last().map(link).unwrap_or_default();
    lines.push(paging(read, &oldest, &newest));
    Ok(lines.join("\n"))
}

/// Which messages to read and in which channel, from the tool's options. Every channel
/// but the current one has to pass [`can_read`].
async fn where_to_read(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<(Read, ChannelId)> {
    let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    // The channel named, if any.
    let named = match text("channel") {
        Some(name) => Some(named_channel(ctx, asker, name)?),
        None => None,
    };

    // A link decides the channel.
    if let Some((kind, link)) = anchor(text("around"), text("before"), text("after"))? {
        let (guild, channel, message) = parse_link(link)
            .ok_or_else(|| user_error(format!("\"{link}\" isn't a Discord message link.")))?;
        let channel = ChannelId::new(channel);
        if named.is_some_and(|named| named != channel) {
            return Err(user_error(
                "That link is in another channel than `channel`. Leave `channel` out, the link decides.",
            ));
        }
        if !can_read(ctx, asker, guild, channel).await? {
            return Err(user_error(
                "I can't read that channel: it's in a DM or another server, or chat is turned off there.",
            ));
        }
        let read = match kind {
            "around" => Read::Around(message),
            "before" => Read::Before(message),
            _ => Read::After(message),
        };
        return Ok((read, channel));
    }

    // Another channel: its latest messages.
    if let Some(channel) = named
        && channel != asker.channel
    {
        if !can_read(ctx, asker, asker.guild.map(|g| g.get()), channel).await? {
            return Err(user_error(format!(
                "I can't read {}: it's in another server, or chat is turned off there.",
                text("channel").unwrap_or_default()
            )));
        }
        return Ok((Read::Latest, channel));
    }

    // The default: just before the question.
    Ok((Read::BeforeQuestion, asker.channel))
}

/// The one of `around`, `before` and `after` that's given, as its name and its link.
/// More than one is a mistake the model fixes.
fn anchor<'a>(
    around: Option<&'a str>,
    before: Option<&'a str>,
    after: Option<&'a str>,
) -> Result<Option<(&'static str, &'a str)>> {
    let given: Vec<(&str, &str)> = [("around", around), ("before", before), ("after", after)]
        .into_iter()
        .filter_map(|(name, link)| link.map(|link| (name, link)))
        .collect();
    match given.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        _ => Err(user_error(
            "Use only one of `around`, `before` and `after`.",
        )),
    }
}

/// The channel `name` names. In a DM there's only the DM itself.
fn named_channel(ctx: &BotCtx, asker: &Asker, name: &str) -> Result<ChannelId> {
    let found = match asker.guild {
        Some(guild) => find_channel(ctx, asker, guild, name),
        None if matches!(name, "here" | "this" | "this channel") => Some(asker.channel),
        None => return Err(user_error("In a DM I can only read this DM.")),
    };
    found.ok_or_else(|| user_error(format!("No channel matches \"{name}\".")))
}

/// The messages, oldest first.
async fn fetch(
    ctx: &BotCtx,
    asker: &Asker,
    channel: ChannelId,
    read: Read,
    count: u16,
) -> Result<Vec<Message>> {
    let request = ctx.http.channel_messages(channel);
    let mut messages = match read {
        Read::BeforeQuestion => request.before(asker.message).limit(count).await?,
        Read::Latest => request.limit(count).await?,
        Read::Around(id) => request.around(MessageId::new(id)).limit(count).await?,
        Read::Before(id) => request.before(MessageId::new(id)).limit(count).await?,
        Read::After(id) => request.after(MessageId::new(id)).limit(count).await?,
    }
    .models()
    .await?;
    // Discord returns newest first; IDs grow with time.
    messages.sort_by_key(|m| m.id);
    Ok(messages)
}

/// "#general", or the channel's mention when it isn't in the cache (an archived thread).
fn channel_label(ctx: &BotCtx, asker: &Asker, channel: ChannelId) -> String {
    asker
        .guild
        .and_then(|guild| ctx.channel_name(guild, channel))
        .map_or(format!("<#{channel}>"), |name| format!("#{name}"))
}

/// The first line, saying what she's reading (not for the default read).
fn header(read: Read, channel: &str) -> String {
    match read {
        Read::BeforeQuestion | Read::Latest => format!("The latest messages in {channel}:"),
        Read::Around(_) => format!(
            "Messages around the linked one in {channel}; it starts with {}:",
            LINKED_MARK.trim()
        ),
        Read::Before(_) => format!("Messages before the linked one in {channel}:"),
        Read::After(_) => format!("Messages after the linked one in {channel}:"),
    }
}

/// The last line: links to read further back, and forward where there may be more.
fn paging(read: Read, oldest: &str, newest: &str) -> String {
    let earlier = format!("For earlier messages: before {oldest}");
    match read {
        // Nothing newer than the question or the latest message to page to.
        Read::BeforeQuestion | Read::Latest => earlier,
        Read::Around(_) | Read::Before(_) | Read::After(_) => {
            format!("{earlier}. For later ones: after {newest}")
        }
    }
}

/// The answer when there are no messages.
fn nothing_found(read: Read) -> &'static str {
    match read {
        Read::BeforeQuestion => "There are no earlier messages.",
        Read::Latest => "That channel has no messages.",
        Read::Around(_) => "There are no messages around that one.",
        Read::Before(_) => "There are no messages before that one.",
        Read::After(_) => "There are no messages after that one.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_most_one_anchor() {
        assert!(anchor(None, None, None).unwrap().is_none());
        assert_eq!(
            anchor(None, Some("x"), None).unwrap(),
            Some(("before", "x"))
        );
        assert_eq!(
            anchor(Some("y"), None, None).unwrap(),
            Some(("around", "y"))
        );
        assert!(anchor(Some("a"), None, Some("b")).is_err());
    }

    #[test]
    fn header_says_what_is_shown() {
        assert_eq!(
            header(Read::Around(1), "#general"),
            "Messages around the linked one in #general; it starts with →:"
        );
        assert_eq!(
            header(Read::Latest, "#memes"),
            "The latest messages in #memes:"
        );
    }

    #[test]
    fn paging_offers_later_only_where_there_can_be_more() {
        assert_eq!(
            paging(Read::BeforeQuestion, "old", "new"),
            "For earlier messages: before old"
        );
        assert_eq!(
            paging(Read::After(1), "old", "new"),
            "For earlier messages: before old. For later ones: after new"
        );
    }
}
