//! Finding quotes: messages from the channel the game is played in, from random moments in
//! its history, cleaned up so they read well on their own.
//!
//! Only the channel the Activity runs in is read. Everyone playing is in that channel, so
//! the game never shows a message someone couldn't already scroll up to.

use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use chrono::{DateTime, Duration, Utc};
use serenity::all::{ChannelId, GetMessages, GuildId, Message, MessageId, MessageType, UserId};
use tracing::debug;

use super::Settings;
use super::game::{Person, Quote, Round};
use crate::core::BotCtx;

/// How many moments in the channel's history are read, 100 messages each.
const READS: usize = 6;
/// Discord's first second (2015), which IDs count from.
const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

/// Reads the channel and returns the quotes found, in random order, and the people who
/// wrote them (with the name and avatar from their messages; [`use_nicknames`] swaps in
/// their server ones later, only for the people a game ends up showing).
pub async fn load(
    ctx: &BotCtx,
    channel: ChannelId,
    settings: &Settings,
) -> anyhow::Result<(Vec<Quote>, Vec<Person>)> {
    let now = Utc::now();
    // From when the channel was made (or `lookback_days` ago) to an hour ago, so the
    // conversation going on right now isn't in it.
    let made = *channel.created_at();
    let from = made.max(now - Duration::days(settings.lookback_days.max(1)));
    let to = now - Duration::hours(1);
    if to <= from {
        anyhow::bail!("this channel is too new");
    }

    let mut seen = HashSet::new();
    let mut quotes = Vec::new();
    let mut people: HashMap<u64, Person> = HashMap::new();
    for _ in 0..READS {
        let span = (to - from).num_milliseconds().max(1);
        let at = from + Duration::milliseconds(fastrand::i64(0..span));
        let messages = channel
            .messages(&ctx.http, GetMessages::new().before(id_at(at)).limit(100))
            .await
            .context("reading the channel")?;
        for msg in messages {
            if !seen.insert(msg.id) {
                continue;
            }
            if settings.exclude_users.contains(&msg.author.id.get()) {
                continue;
            }
            let Some(text) = quote_text(&msg, settings) else {
                continue;
            };
            people.entry(msg.author.id.get()).or_insert_with(|| Person {
                id: msg.author.id.get(),
                name: msg.author.display_name().to_string(),
                avatar: Some(proxied(&msg.author.face())),
            });
            quotes.push(Quote {
                author: msg.author.id.get(),
                text,
                posted: msg.timestamp.format("%B %Y").to_string(),
                link: msg.link(),
            });
        }
    }
    debug!("found {} quotes by {} people", quotes.len(), people.len());
    fastrand::shuffle(&mut quotes);
    Ok((quotes, people.into_values().collect()))
}

/// Swaps in server nicknames and avatars for everyone the rounds show. Someone who left the
/// server keeps the name from their message.
pub async fn use_nicknames(ctx: &BotCtx, guild: GuildId, rounds: &mut [Round]) {
    let mut found: HashMap<u64, Person> = HashMap::new();
    for round in rounds.iter_mut() {
        for choice in &mut round.choices {
            if let Some(person) = found.get(&choice.id) {
                *choice = person.clone();
                continue;
            }
            if let Some(person) = member(ctx, guild, UserId::new(choice.id)).await {
                *choice = person;
            }
            found.insert(choice.id, choice.clone());
        }
    }
}

/// A server member's nickname and avatar, or `None` when they aren't in the server.
pub async fn member(ctx: &BotCtx, guild: GuildId, user: UserId) -> Option<Person> {
    match guild.member(&ctx.http, user).await {
        Ok(member) => Some(Person {
            id: user.get(),
            name: member.display_name().to_string(),
            avatar: Some(proxied(&member.face())),
        }),
        Err(err) => {
            debug!("couldn't look up {user}: {err}");
            None
        }
    }
}

/// A Discord picture link the Activity can load. Its page may only reach its own server,
/// so pictures go through the `/cdn` URL mapping (see the feature's doc page).
pub fn proxied(url: &str) -> String {
    match url.strip_prefix("https://cdn.discordapp.com/") {
        Some(rest) => format!("/.proxy/cdn/{rest}"),
        None => url.to_string(),
    }
}

/// The ID a message posted at `at` would have. Reading "before" it starts there.
fn id_at(at: DateTime<Utc>) -> MessageId {
    let ms = (at.timestamp_millis() - DISCORD_EPOCH_MS).max(1) as u64;
    MessageId::new(ms << 22)
}

/// A message's text as a quote, or `None` when it doesn't make one: from a bot, a webhook
/// or a system message, with a link (they're rarely funny without the picture), too short
/// or too long.
fn quote_text(msg: &Message, settings: &Settings) -> Option<String> {
    if msg.author.bot || msg.webhook_id.is_some() {
        return None;
    }
    if !matches!(msg.kind, MessageType::Regular | MessageType::InlineReply) {
        return None;
    }
    let names: HashMap<u64, String> = msg
        .mentions
        .iter()
        .map(|u| (u.id.get(), u.display_name().to_string()))
        .collect();
    let text = clean(&msg.content, &names);
    let chars = text.chars().count();
    let words = text.split_whitespace().count();
    let fits = chars >= settings.min_length && chars <= settings.max_length && words >= 3;
    (fits && !text.contains("http://") && !text.contains("https://")).then_some(text)
}

/// Turns Discord's markup into what people see: `<@123>` into `@name`, `<#123>` into
/// `#channel`, `<:wave:123>` into `:wave:`, and times into "(a time)". Spaces are trimmed.
fn clean(content: &str, names: &HashMap<u64, String>) -> String {
    let mut out = String::new();
    let mut rest = content;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let inner = &after[..end];
        match tag(inner, names) {
            Some(text) => out.push_str(&text),
            // Not markup, like "<3" or "a < b > c": kept as written.
            None => out.push_str(&rest[start..start + 1 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// What one `<...>` reads as, or `None` when it isn't Discord markup.
fn tag(inner: &str, names: &HashMap<u64, String>) -> Option<String> {
    let id = |s: &str| s.parse::<u64>().ok();
    if let Some(user) = inner.strip_prefix("@!").or_else(|| inner.strip_prefix('@')) {
        if let Some(role) = user.strip_prefix('&') {
            id(role)?;
            return Some("@role".into());
        }
        let user = id(user)?;
        let name = names.get(&user).map_or("someone", String::as_str);
        return Some(format!("@{name}"));
    }
    if let Some(channel) = inner.strip_prefix('#') {
        id(channel)?;
        return Some("#channel".into());
    }
    if let Some(time) = inner.strip_prefix("t:") {
        id(time.split(':').next()?)?;
        return Some("(a time)".into());
    }
    // Custom emoji: <:name:id> or <a:name:id>.
    let emoji = inner
        .strip_prefix("a:")
        .or_else(|| inner.strip_prefix(':'))?;
    let (name, emoji_id) = emoji.split_once(':')?;
    id(emoji_id)?;
    Some(format!(":{name}:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_reads_like_discord_shows_it() {
        let names = HashMap::from([(42, "Cherry".to_string())]);
        assert_eq!(
            clean("<@42> look <#7> <:wave:99> <a:dance:98>", &names),
            "@Cherry look #channel :wave: :dance:"
        );
        assert_eq!(clean("hi <@!5> and <@&6>", &names), "hi @someone and @role");
        assert_eq!(clean("at <t:1700000000:R> ok", &names), "at (a time) ok");
    }

    #[test]
    fn text_that_isnt_markup_stays() {
        let names = HashMap::new();
        assert_eq!(clean("i <3 this", &names), "i <3 this");
        assert_eq!(clean("a < b > c", &names), "a < b > c");
        assert_eq!(clean("<not:markup>", &names), "<not:markup>");
        assert_eq!(clean("  spaced  ", &names), "spaced");
    }

    #[test]
    fn ids_follow_the_clock() {
        let at = DateTime::from_timestamp_millis(DISCORD_EPOCH_MS + 1000).unwrap();
        assert_eq!(id_at(at).get(), 1000 << 22);
        assert_eq!(*id_at(at).created_at(), at);
    }

    #[test]
    fn avatars_go_through_the_mapping() {
        assert_eq!(
            proxied("https://cdn.discordapp.com/avatars/1/abc.webp?size=1024"),
            "/.proxy/cdn/avatars/1/abc.webp?size=1024"
        );
    }
}
