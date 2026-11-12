//! Private helpers in the Apps menu: a cheap model answers only the person who asked.
//!
//! - "Catch me up" (right-click a message → Apps): what was said from that message on.
//! - `/catchup`: what was said since your last message in this channel, or in the last
//!   `hours`.
//! - "Explain this": what a message means (an in-joke, a reference, a long post, a meme),
//!   with the messages before it as context.
//! - "Translate this": the message in your Discord language.
//!
//! Each is one request without tools (jobs `catch_up`, `explain` and `translate`, Claude
//! Haiku by default, see `ai::Job::builtin`). The answer is ephemeral and nothing is saved,
//! so nobody else's chat fills up and it isn't part of her conversations.

use chrono::{Duration, Utc};
use chrono_tz::Tz;
use futures::future::join_all;
use tracing::warn;
use twilight_interactions::command::{CommandModel, CreateCommand};

use super::emoji::NoTools;
use super::tools::{format_message, home_zone};
use crate::ai::{ChatRequest, Input, Part, Role, Turn, complete};
use crate::core::{BotCtx, Command, CommandCtx, Result, user_error};
use crate::util::discord::{
    ChannelId, Draft, GuildId, Message, Page, display_name, get_messages, message_link, no_pings,
};
use crate::util::media;
use crate::util::split::{DISCORD_LIMIT, split_message};

/// The most messages a catch-up reads (5 pages). Haiku reads 500 messages for well under a
/// cent.
const MAX_CATCH_UP: usize = 500;
/// Messages Discord gives per page.
const PAGE: u16 = 100;
/// The most hours `/catchup` looks back.
const MAX_HOURS: i64 = 72;
/// Messages before the one to explain or translate, for context.
const EXPLAIN_CONTEXT: u16 = 15;
const TRANSLATE_CONTEXT: u16 = 5;
/// Pictures of the message that are shown to the model, each shrunk to this size.
const MAX_PICTURES: usize = 3;
const PICTURE_SIDE: u32 = 1024;

const CATCH_UP_SYSTEM: &str = "You catch someone up on a Discord conversation they missed. \
You get the messages, oldest first, each with its time, author and link. Write a short summary \
for the person who asked: the topics, what was decided or planned, questions aimed at them, \
links or files people shared. Group by topic, not by time. Use short bullet points, no \
headings, at most about 250 words, less for a quiet stretch. Name people as they're named in \
the messages. Link the one or two messages most worth reading, by their link, inline. Write \
in the language most of the messages use. Don't add anything that isn't in the messages.";

const EXPLAIN_SYSTEM: &str = "Someone on a Discord server asks what a message means. You get \
the messages before it for context, and the message itself, marked with →, with any pictures \
it has. Explain what it means or refers to: the joke, the reference, the slang, the meme, \
or a plain summary if it's long. Be short and direct, a few sentences, no headings. If it \
depends on something you can't see, say what you'd need. Answer in the language the asker \
uses on Discord, which is given.";

const TRANSLATE_SYSTEM: &str = "You translate Discord messages. You get a few earlier \
messages for context, then the message to translate, marked with →, with any pictures it has. \
Translate the marked message (and text in its pictures) into the language you're given. Keep \
the tone, the slang and the emoji. Write only the translation, then, if it helps, one short \
line in italics about a phrase that doesn't translate well. If the message is already in that \
language, say so in one line.";

/// "Catch me up" in a message's Apps menu.
pub fn catch_me_up() -> Command {
    Command::message("Catch me up", |ctx| Box::pin(catch_up_from_message(ctx)))
}

/// "Explain this" in a message's Apps menu.
pub fn explain_this() -> Command {
    Command::message("Explain this", |ctx| Box::pin(explain(ctx)))
}

/// "Translate this" in a message's Apps menu.
pub fn translate_this() -> Command {
    Command::message("Translate this", |ctx| Box::pin(translate(ctx)))
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "catchup",
    desc = "A private summary of what you missed in this channel"
)]
pub struct CatchUpCommand {
    /// How many hours back. Leave empty for everything since your last message here.
    #[command(min_value = 1, max_value = 72)]
    hours: Option<i64>,
}

/// `/catchup`.
pub fn catchup() -> Command {
    Command::slash::<CatchUpCommand>(|ctx| Box::pin(catch_up_command(ctx)))
}

// ---------------------------------------------------------------------------------------
// Catching up
// ---------------------------------------------------------------------------------------

async fn catch_up_from_message(ctx: CommandCtx) -> Result<()> {
    let ctx = ctx.ephemeral();
    let start = target(&ctx)?;
    ctx.defer().await?;
    let channel = ctx.channel_id();
    let mut messages = vec![start.clone()];
    let mut after = start.id;
    // Whether the channel ended before the cap.
    let mut ended = false;
    while messages.len() < MAX_CATCH_UP {
        let mut page = get_messages(&ctx.bot.http, channel, Page::After(after), PAGE).await?;
        page.sort_by_key(|m| m.id);
        let full = page.len() == usize::from(PAGE);
        if let Some(last) = page.last() {
            after = last.id;
        }
        messages.extend(page);
        if !full {
            ended = true;
            break;
        }
    }
    let more = !ended || messages.len() > MAX_CATCH_UP;
    messages.truncate(MAX_CATCH_UP);
    let guild = ctx.guild_id();
    let note = if more {
        let last = messages.last().map(|m| m.id).unwrap_or(start.id);
        format!(
            "-# The first {MAX_CATCH_UP} messages from {}, up to {}. Use Catch me up on that one for the rest.",
            message_link(guild, channel, start.id),
            message_link(guild, channel, last)
        )
    } else {
        format!(
            "-# {} since {}",
            count_messages(messages.len()),
            message_link(guild, channel, start.id)
        )
    };
    summarize(&ctx, &messages, note).await
}

async fn catch_up_command(ctx: CommandCtx) -> Result<()> {
    let ctx = ctx.ephemeral();
    let CatchUpCommand { hours } = ctx.parse()?;
    ctx.defer().await?;
    let channel = ctx.channel_id();
    let me = ctx.author_id();
    let since = hours.map(|h| Utc::now() - Duration::hours(h.clamp(1, MAX_HOURS)));

    // Newest first, until their own message, the hour limit or the cap.
    let mut messages: Vec<Message> = Vec::new();
    let mut found_mine = false;
    let mut page_start = Page::Newest;
    'pages: while messages.len() < MAX_CATCH_UP {
        let mut page = get_messages(&ctx.bot.http, channel, page_start, PAGE).await?;
        page.sort_by_key(|m| std::cmp::Reverse(m.id));
        let full = page.len() == usize::from(PAGE);
        let Some(oldest) = page.last().map(|m| m.id) else {
            break;
        };
        for msg in page {
            let stop = match since {
                Some(since) => msg.timestamp.as_secs() < since.timestamp(),
                None => msg.author.id == me,
            };
            if stop {
                found_mine = since.is_none();
                break 'pages;
            }
            messages.push(msg);
            if messages.len() >= MAX_CATCH_UP {
                break 'pages;
            }
        }
        if !full {
            break;
        }
        page_start = Page::Before(oldest);
    }
    messages.reverse();

    if messages.is_empty() {
        let text = match hours {
            Some(h) => format!("Nothing was said here in the last {}.", count_hours(h)),
            None => "Nothing new since your last message here.".to_string(),
        };
        ctx.say(text).await?;
        return Ok(());
    }
    let n = count_messages(messages.len());
    let note = match (hours, found_mine) {
        (Some(h), _) if messages.len() >= MAX_CATCH_UP => {
            format!("-# The latest {n} (the last {} has more).", count_hours(h))
        }
        (Some(h), _) => format!("-# {n} from the last {}", count_hours(h)),
        (None, true) => format!("-# {n} since your last message"),
        (None, false) => {
            format!("-# The latest {n}: you haven't written here in that long.")
        }
    };
    summarize(&ctx, &messages, note).await
}

/// Summarizes `messages` (oldest first) for the person who asked, under `note`.
async fn summarize(ctx: &CommandCtx, messages: &[Message], note: String) -> Result<()> {
    let bot = &ctx.bot;
    let guild = ctx.guild_id();
    let channel = ctx.channel_id();
    let zone = home_zone(bot, ctx.author_id()).await;
    let lines = join_all(
        messages
            .iter()
            .map(|m| linked_line(bot, guild, channel, m, zone)),
    )
    .await;
    let question = format!(
        "{}\n\nCatch up {} on these {} messages{}.",
        lines.join("\n"),
        display_name(ctx.author()),
        messages.len(),
        channel_words(bot, guild, channel),
    );
    let answer = ask(bot, "catch_up", CATCH_UP_SYSTEM, vec![Part::Text(question)]).await?;
    send_long(ctx, &format!("{answer}\n{note}")).await
}

/// A message's line with its link, so the summary can point at it.
async fn linked_line(
    ctx: &BotCtx,
    guild: Option<GuildId>,
    channel: ChannelId,
    msg: &Message,
    zone: Tz,
) -> String {
    format!(
        "{} {}",
        format_message(ctx, msg, zone).await,
        message_link(guild, channel, msg.id)
    )
}

// ---------------------------------------------------------------------------------------
// Explaining and translating
// ---------------------------------------------------------------------------------------

async fn explain(ctx: CommandCtx) -> Result<()> {
    let ctx = ctx.ephemeral();
    let msg = target(&ctx)?;
    ctx.defer().await?;
    let language = language_name(ctx.interaction.locale.as_deref());
    let mut parts = marked_conversation(&ctx, &msg, EXPLAIN_CONTEXT).await?;
    parts.push(Part::Text(format!(
        "{} asks what the marked message means. Their Discord language: {language}.",
        display_name(ctx.author())
    )));
    let answer = ask(&ctx.bot, "explain", EXPLAIN_SYSTEM, parts).await?;
    send_long(&ctx, &answer).await
}

async fn translate(ctx: CommandCtx) -> Result<()> {
    let ctx = ctx.ephemeral();
    let msg = target(&ctx)?;
    if msg.content.trim().is_empty() && media::find_media(&msg).is_empty() {
        return Err(user_error("That message has nothing to translate."));
    }
    ctx.defer().await?;
    let language = language_name(ctx.interaction.locale.as_deref());
    let mut parts = marked_conversation(&ctx, &msg, TRANSLATE_CONTEXT).await?;
    parts.push(Part::Text(format!(
        "Translate the marked message into {language}."
    )));
    let answer = ask(&ctx.bot, "translate", TRANSLATE_SYSTEM, parts).await?;
    send_long(&ctx, &answer).await
}

/// The `before` messages before `msg`, then `msg` marked with →, then its pictures.
async fn marked_conversation(ctx: &CommandCtx, msg: &Message, before: u16) -> Result<Vec<Part>> {
    let bot = &ctx.bot;
    let zone = home_zone(bot, ctx.author_id()).await;
    let mut earlier = get_messages(&bot.http, ctx.channel_id(), Page::Before(msg.id), before)
        .await
        .unwrap_or_else(|err| {
            warn!("couldn't read the messages before {}: {err:#}", msg.id);
            Vec::new()
        });
    earlier.sort_by_key(|m| m.id);
    let mut lines = Vec::new();
    for m in &earlier {
        lines.push(format_message(bot, m, zone).await);
    }
    if let Some(parent) = &msg.referenced_message
        && !earlier.iter().any(|m| m.id == parent.id)
    {
        lines.push(format!(
            "(the marked message replies to: {})",
            format_message(bot, parent, zone).await
        ));
    }
    lines.push(format!("→ {}", format_message(bot, msg, zone).await));
    let mut parts = vec![Part::Text(lines.join("\n"))];
    parts.extend(pictures(msg).await.into_iter().map(Part::Image));
    Ok(parts)
}

/// Up to [`MAX_PICTURES`] of the message's pictures, GIFs or videos, made small.
async fn pictures(msg: &Message) -> Vec<media::ModelImage> {
    let found = media::find_media(msg);
    let loads = found
        .iter()
        .take(MAX_PICTURES)
        .map(|m| media::load_small(m, PICTURE_SIDE, 1));
    let mut images = Vec::new();
    for result in join_all(loads).await {
        match result {
            Ok(loaded) => images.extend(loaded),
            Err(err) => warn!("couldn't load a picture of {}: {err:#}", msg.id),
        }
    }
    images
}

// ---------------------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------------------

/// The message an Apps command was used on.
fn target(ctx: &CommandCtx) -> Result<Message> {
    ctx.target_message()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("an Apps command without a message"))
}

/// One request to the model, no tools, the text of its answer.
async fn ask(ctx: &BotCtx, job: &'static str, system: &str, parts: Vec<Part>) -> Result<String> {
    let provider = ctx
        .ai
        .chat()
        .ok_or_else(|| user_error("Chat is turned off: there's no AI model set up."))?;
    let request = ChatRequest {
        system: system.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts,
        }]),
        tools: Vec::new(),
        cache_key: job.to_string(),
        job,
        answer_now: false,
    };
    let done = complete(provider.as_ref(), request, &NoTools, 1).await?;
    let text = done.text.trim().to_string();
    if text.is_empty() {
        return Err(user_error("The model had nothing to say. Try again?"));
    }
    Ok(text)
}

/// Sends `text` privately, split into Discord-sized messages, without pinging anyone.
async fn send_long(ctx: &CommandCtx, text: &str) -> Result<()> {
    for part in split_message(text, DISCORD_LIMIT) {
        ctx.send(Draft::new().content(part).allowed_mentions(no_pings()))
            .await?;
    }
    Ok(())
}

/// " in #general", or nothing when the channel isn't in the cache.
fn channel_words(ctx: &BotCtx, guild: Option<GuildId>, channel: ChannelId) -> String {
    guild
        .and_then(|guild| ctx.channel_name(guild, channel))
        .map(|name| format!(" in #{name}"))
        .unwrap_or_default()
}

fn count_messages(n: usize) -> String {
    crate::util::count(n, "message")
}

fn count_hours(h: i64) -> String {
    crate::util::count(usize::try_from(h).unwrap_or(0), "hour")
}

/// The English name of a Discord locale ("nb" or "no" → "Norwegian"), English when unknown.
fn language_name(locale: Option<&str>) -> &'static str {
    match locale.unwrap_or("en-US") {
        "id" => "Indonesian",
        "da" => "Danish",
        "de" => "German",
        "en-GB" => "British English",
        "es-ES" => "Spanish",
        "es-419" => "Latin American Spanish",
        "fr" => "French",
        "hr" => "Croatian",
        "it" => "Italian",
        "lt" => "Lithuanian",
        "hu" => "Hungarian",
        "nl" => "Dutch",
        "no" | "nb" | "nn" => "Norwegian",
        "pl" => "Polish",
        "pt-BR" => "Brazilian Portuguese",
        "ro" => "Romanian",
        "fi" => "Finnish",
        "sv-SE" => "Swedish",
        "vi" => "Vietnamese",
        "tr" => "Turkish",
        "cs" => "Czech",
        "el" => "Greek",
        "bg" => "Bulgarian",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        "hi" => "Hindi",
        "th" => "Thai",
        "zh-CN" => "Simplified Chinese",
        "ja" => "Japanese",
        "zh-TW" => "Traditional Chinese",
        "ko" => "Korean",
        _ => "English",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_locales_name_languages() {
        assert_eq!(language_name(Some("no")), "Norwegian");
        assert_eq!(language_name(Some("ja")), "Japanese");
        assert_eq!(language_name(Some("en-US")), "English");
        assert_eq!(language_name(Some("xx")), "English");
        assert_eq!(language_name(None), "English");
    }

    #[test]
    fn the_catchup_command_is_valid() {
        let command = catchup();
        assert_eq!(command.definition.name, "catchup");
    }
}
