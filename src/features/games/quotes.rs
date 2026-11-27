//! Finding quotes: messages from random moments in the history of the channels
//! `sources.rs` picked, cleaned up so they read well on their own.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use tracing::debug;
use twilight_model::channel::message::MessageType;

use super::sources::Source;
use crate::core::BotCtx;
use crate::util::discord::{
    GuildId, Message, MessageId, Page, UserId, created_at, display_name, get_member, get_messages,
    member_display_name, message_link, to_chrono,
};

/// Someone who wrote a quote (or can be offered as a choice in Who said it?).
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub id: u64,
    pub name: String,
}

/// A real message read from channel history.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub author: u64,
    pub text: String,
    /// When it was posted, shown under the quote ("March 2025").
    pub posted: String,
    /// The channel it was posted in, shown under the quote when it isn't the channel the
    /// game is played in.
    pub place: Option<String>,
    /// A link to the message, shown with the answer.
    pub link: String,
}

/// Filters for which messages count as quotes.
#[derive(Debug, Clone)]
pub struct QuoteRules {
    pub lookback_days: i64,
    pub min_length: usize,
    pub max_length: usize,
    pub exclude_users: Vec<u64>,
}

impl Default for QuoteRules {
    fn default() -> Self {
        QuoteRules {
            lookback_days: 365,
            min_length: 20,
            max_length: 300,
            exclude_users: Vec::new(),
        }
    }
}

/// How many moments in the channel's history are read, 100 messages each.
const READS: usize = 6;
/// The same, when quotes come from the whole server: more channels, more reads.
const SERVER_READS: usize = 10;
/// Discord's first second (2015), which IDs count from.
const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

/// Reads the sources and returns the quotes found, in random order, and the people who
/// wrote them (with the name from their messages; [`use_nicknames`] swaps in
/// their server ones later, only for the people a game ends up showing).
///
/// Each read picks one of `sources` at random. A source the bot can't read is skipped, so
/// one locked channel doesn't stop a whole-server game.
pub async fn load(
    ctx: &BotCtx,
    guild: GuildId,
    sources: &[Source],
    rules: &QuoteRules,
) -> anyhow::Result<(Vec<Quote>, Vec<Person>)> {
    let reads = if sources.len() > 1 {
        SERVER_READS
    } else {
        READS
    };
    let mut seen = HashSet::new();
    let mut quotes = Vec::new();
    let mut people: HashMap<u64, Person> = HashMap::new();
    let mut failed = None;
    for _ in 0..reads {
        let found = match read_once(ctx, guild, sources, rules).await {
            Ok(found) => found,
            Err(err) => {
                failed = Some(err);
                continue;
            }
        };
        for (quote, person) in found {
            if !seen.insert(quote.link.clone()) {
                continue;
            }
            people.entry(person.id).or_insert(person);
            quotes.push(quote);
        }
    }
    if quotes.is_empty()
        && let Some(err) = failed
    {
        return Err(err.context("reading the channel"));
    }
    debug!("found {} quotes by {} people", quotes.len(), people.len());
    fastrand::shuffle(&mut quotes);
    Ok((quotes, people.into_values().collect()))
}

/// One read: the 100 messages before a random moment in one of `sources` (picked at
/// random), from when the channel was made (or `lookback_days` ago) to an hour ago. Gives
/// the ones that make quotes, oldest last, each with who wrote it. Nothing when the source
/// is too new to read.
pub async fn read_once(
    ctx: &BotCtx,
    guild: GuildId,
    sources: &[Source],
    rules: &QuoteRules,
) -> anyhow::Result<Vec<(Quote, Person)>> {
    let Some(source) = sources.get(fastrand::usize(..sources.len().max(1))) else {
        return Ok(Vec::new());
    };
    let now = Utc::now();
    // Nothing from the last hour, so the conversation going on right now isn't in it.
    let to = now - Duration::hours(1);
    let oldest = now - Duration::days(rules.lookback_days.max(1));
    let from = created_at(source.channel).max(oldest);
    if to <= from {
        return Ok(Vec::new());
    }
    let span = (to - from).num_milliseconds().max(1);
    let at = from + Duration::milliseconds(fastrand::i64(0..span));
    let messages = get_messages(&ctx.http, source.channel, Page::Before(id_at(at)), 100)
        .await
        .inspect_err(|err| debug!("couldn't read {}: {err:#}", source.channel))?;
    Ok(extract_quotes(
        messages,
        guild,
        source.name.as_deref(),
        rules,
    ))
}

/// Turns a page of Discord messages into `(Quote, Person)` pairs using the known `guild`
/// (messages read over REST omit `guild_id`, and archived threads may not be in the cache).
fn extract_quotes(
    messages: Vec<Message>,
    guild: GuildId,
    place: Option<&str>,
    rules: &QuoteRules,
) -> Vec<(Quote, Person)> {
    let mut found = Vec::new();
    for msg in messages {
        if rules.exclude_users.contains(&msg.author.id.get()) {
            continue;
        }
        let Some(text) = quote_text(&msg, rules) else {
            continue;
        };
        let person = Person {
            id: msg.author.id.get(),
            name: display_name(&msg.author).to_string(),
        };
        let quote = Quote {
            author: person.id,
            text,
            posted: to_chrono(msg.timestamp).format("%B %Y").to_string(),
            place: place.map(str::to_string),
            link: message_link(Some(guild), msg.channel_id, msg.id),
        };
        found.push((quote, person));
    }
    found
}

/// Swaps in server nicknames for `people`. Someone who left the server keeps the name from
/// their message.
pub async fn use_nicknames(ctx: &BotCtx, guild: GuildId, people: &mut [&mut Person]) {
    let mut found: HashMap<u64, Person> = HashMap::new();
    for person in people.iter_mut() {
        if let Some(cached) = found.get(&person.id) {
            **person = cached.clone();
            continue;
        }
        if let Some(member) = member(ctx, guild, UserId::new(person.id)).await {
            **person = member;
        }
        found.insert(person.id, (*person).clone());
    }
}

/// A server member's nickname, or `None` when they aren't in the server.
async fn member(ctx: &BotCtx, guild: GuildId, user: UserId) -> Option<Person> {
    match get_member(&ctx.http, guild, user).await {
        Ok(member) => Some(Person {
            id: user.get(),
            name: member_display_name(&member).to_string(),
        }),
        Err(err) => {
            debug!("couldn't look up {user}: {err:#}");
            None
        }
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
pub fn quote_text(msg: &Message, rules: &QuoteRules) -> Option<String> {
    if msg.author.bot || msg.webhook_id.is_some() {
        return None;
    }
    if !matches!(msg.kind, MessageType::Regular | MessageType::Reply) {
        return None;
    }
    let names: HashMap<u64, String> = msg
        .mentions
        .iter()
        .map(|m| {
            let nick = m.member.as_ref().and_then(|member| member.nick.clone());
            (m.id.get(), nick.unwrap_or_else(|| m.name.clone()))
        })
        .collect();
    let text = clean(&msg.content, &names);
    let chars = text.chars().count();
    let words = text.split_whitespace().count();
    let fits = chars >= rules.min_length && chars <= rules.max_length && words >= 3;
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
        assert_eq!(created_at(id_at(at)), at);
    }

    #[test]
    fn extract_quotes_uses_known_guild_id_never_at_me() {
        let msg: Message = serde_json::from_value(serde_json::json!({
            "id": "99",
            "channel_id": "7",
            "author": {
                "id": "5",
                "username": "cherry",
                "discriminator": "0",
                "bot": false
            },
            "content": "this is a long enough quote from an archived thread",
            "timestamp": "2025-03-10T12:00:00.000000+00:00",
            "tts": false,
            "mention_everyone": false,
            "mentions": [],
            "mention_roles": [],
            "attachments": [],
            "embeds": [],
            "pinned": false,
            "type": 0
        }))
        .unwrap();
        let found = extract_quotes(
            vec![msg],
            GuildId::new(42),
            Some("archived-thread"),
            &QuoteRules::default(),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.link, "https://discord.com/channels/42/7/99");
        assert!(!found[0].0.link.contains("@me"));
    }
}
