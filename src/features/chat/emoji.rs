//! The server's custom emoji, so Vivy can use them like a regular.
//!
//! A background task looks at each emoji's picture once and saves a short description
//! (`chat_emoji`): every emoji at the first start, then only new ones, checked at each
//! start and once a day ([`CHECK`]), since new emoji are rare. The `list_server_emoji` tool lists them with their descriptions.
//!
//! Discord only shows a custom emoji written as its code, `<:name:123>`. Models often write
//! `:name:` the way people type it, so [`with_codes`] turns her `:name:` into the code before
//! her messages go out.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tracing::{Instrument as _, error, info, info_span, warn};

use super::store;
use crate::ai::{
    ChatRequest, Input, Part, Role, ToolCall, ToolDef, ToolReply, ToolRunner, Turn, complete,
};
use crate::core::{Asker, BotCtx, Result};
use crate::util::discord::{EmojiId, GuildId};
use crate::util::media::{self, Media, MediaKind};
use crate::util::shorten;

/// How often to look for new emoji.
const CHECK: Duration = Duration::from_secs(24 * 60 * 60);
/// Emoji that couldn't be described since the bot started (a picture the model can't read,
/// a refusal), so they aren't tried again every day. They get another try after a restart.
static FAILED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);
/// The most emoji `list_server_emoji` returns.
const MAX_LISTED: usize = 250;

const SYSTEM: &str = "You describe Discord custom emoji. Answer with exactly two lines, no preamble:\n\
SHORT: <3 to 6 word gloss of the vibe/meaning, no period>\n\
LONG: <one full sentence describing what it shows and when people use it>";

const SHORT_SYSTEM: &str = "Turn a Discord custom emoji description into a 3 to 6 word gloss of its vibe or meaning, with no period and no preamble.";

pub fn def() -> ToolDef {
    ToolDef {
        name: "list_server_emoji",
        description: "This server's custom emoji, each with the code to use it in a message or a reaction and what it shows. Use them like the regulars do.",
        parameters: json!({"type": "object", "properties": {}}),
    }
}

pub async fn list(ctx: &BotCtx, asker: &Asker) -> Result<String> {
    let Some(guild) = asker.guild else {
        return Ok("Direct messages have no server emoji.".to_string());
    };
    let lines = described(ctx, guild).await?;
    if lines.is_empty() {
        return Ok("This server has no custom emoji.".to_string());
    }
    Ok(lines.join("\n"))
}

/// The server's emoji for a chime-in reaction, in compact `:name: <short>` form (no
/// snowflake IDs; `reaction_code` resolves `:name:` to `<:name:id>`). Empty when the
/// server has none (or in DMs).
pub async fn for_chime_in(ctx: &BotCtx, guild: Option<GuildId>) -> String {
    let Some(guild) = guild else {
        return String::new();
    };
    match compact_list(ctx, guild).await {
        Ok(lines) if !lines.is_empty() => format!(
            "\n\nBesides plain emoji you can use this server's own as your reaction (write :name:):\n{}",
            lines.join("\n")
        ),
        Ok(_) => String::new(),
        Err(err) => {
            warn!("listing the server's emoji: {err:#}");
            String::new()
        }
    }
}

/// Compact list of the server's emoji for chime-ins: `:name: <short_description>` (no
/// snowflake IDs), falling back to `:name:` when `short_description` is empty.
pub async fn compact_list(ctx: &BotCtx, guild: GuildId) -> Result<Vec<String>> {
    let emoji = server_emoji(ctx, guild);
    if emoji.is_empty() {
        return Ok(Vec::new());
    }
    let id = guild.get();
    let described = ctx
        .db
        .call(move |conn| Ok(store::emoji_descriptions(conn, id)?))
        .await?;
    Ok(emoji
        .iter()
        .take(MAX_LISTED)
        .map(|e| {
            let short = described
                .get(&e.id.get())
                .map(|entry| entry.short_description.as_str());
            format_compact_line(&e.name, short)
        })
        .collect())
}

/// One compact emoji line for chime-ins: `:name: <short>` or `:name:` when empty.
fn format_compact_line(name: &str, short_description: Option<&str>) -> String {
    match short_description.map(str::trim).filter(|s| !s.is_empty()) {
        Some(short) => format!(":{name}: {short}"),
        None => format!(":{name}:"),
    }
}

/// One line per emoji of the server: its code and what it shows, when that's known yet.
async fn described(ctx: &BotCtx, guild: GuildId) -> Result<Vec<String>> {
    let emoji = server_emoji(ctx, guild);
    if emoji.is_empty() {
        return Ok(Vec::new());
    }
    let id = guild.get();
    let described = ctx
        .db
        .call(move |conn| Ok(store::emoji_descriptions(conn, id)?))
        .await?;
    Ok(emoji
        .iter()
        .take(MAX_LISTED)
        .map(|e| match described.get(&e.id.get()) {
            Some(entry) if !entry.description.is_empty() => {
                format!("{e}: {}", entry.description)
            }
            _ => e.to_string(),
        })
        .collect())
}

/// The codes of a server's custom emoji by name ("pog" → "<:pog:123>"), for
/// [`with_codes`]. Empty in DMs.
pub fn codes(ctx: &BotCtx, guild: Option<GuildId>) -> HashMap<String, String> {
    let Some(guild) = guild else {
        return HashMap::new();
    };
    server_emoji(ctx, guild)
        .into_iter()
        .map(|e| (e.name.clone(), e.to_string()))
        .collect()
}

/// `text` with every server emoji she wrote by name, `:pog:`, turned into its code,
/// `<:pog:123>`, so Discord shows it. A code she wrote herself gets the right id and the
/// `a` of an animated emoji, in case she guessed them. Names the server has no emoji for
/// stay as they are.
pub fn with_codes(text: &str, codes: &HashMap<String, String>) -> String {
    if codes.is_empty() {
        return text.to_string();
    }
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(':') {
        let after = &rest[start + 1..];
        // The name runs to the next colon.
        let code = after
            .find(':')
            .and_then(|end| Some((codes.get(&after[..end])?, end)));
        let Some((code, end)) = code else {
            result.push_str(&rest[..=start]);
            rest = after;
            continue;
        };
        let before = &rest[..start];
        let mut tail = &after[end + 1..];
        // A code she wrote herself, `<:pog:123>`: replace all of it.
        let opened = before
            .strip_suffix("<a")
            .or_else(|| before.strip_suffix('<'));
        if let Some(opened) = opened {
            result.push_str(opened);
            tail = tail.trim_start_matches(|c: char| c.is_ascii_digit());
            tail = tail.strip_prefix('>').unwrap_or(tail);
        } else {
            result.push_str(before);
        }
        result.push_str(code);
        rest = tail;
    }
    result.push_str(rest);
    result
}

/// A reaction she picked, with a server emoji's bare name ("pog") turned into its code too.
pub fn reaction_code(emoji: &str, codes: &HashMap<String, String>) -> String {
    match codes.get(emoji.trim()) {
        Some(code) => code.clone(),
        None => with_codes(emoji, codes),
    }
}

/// A server's custom emoji, with what this file needs.
#[derive(Clone)]
struct Emoji {
    id: EmojiId,
    name: String,
    animated: bool,
}

/// The code that shows the emoji in a message: "<:name:123>", or "<a:name:123>" when
/// it moves.
impl fmt::Display for Emoji {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let a = if self.animated { "a" } else { "" };
        write!(f, "<{a}:{}:{}>", self.name, self.id)
    }
}

/// The usable custom emoji of a server, from the cache, by name.
fn server_emoji(ctx: &BotCtx, guild: GuildId) -> Vec<Emoji> {
    let Some(ids) = ctx.cache.guild_emojis(guild) else {
        return Vec::new();
    };
    let mut list: Vec<Emoji> = ids
        .iter()
        .filter_map(|&id| ctx.cache.emoji(id))
        .filter(|e| e.available())
        .map(|e| Emoji {
            id: e.id(),
            name: e.name().to_string(),
            animated: e.animated(),
        })
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

pub fn spawn(ctx: &BotCtx) {
    let ctx = ctx.clone();
    let span = info_span!("emoji", feature = "chat");
    ctx.tasks.clone().spawn(run(ctx).instrument(span));
}

async fn run(ctx: BotCtx) {
    let timer = ctx.timers.add("Emoji descriptions", "daily");
    // Give the gateway a moment to fill the cache with servers.
    let first = Duration::from_secs(30);
    timer.sleeping(first);
    tokio::select! {
        () = tokio::time::sleep(first) => {}
        () = ctx.shutdown.cancelled() => return,
    }
    loop {
        timer.running();
        if let Err(err) = describe_new(&ctx).await {
            error!("describing emoji: {err:#}");
        }
        // Daily chat upkeep that fits here: old answers' raw output isn't needed any more.
        let before = chrono::Utc::now().timestamp() - store::NATIVE_KEEP_DAYS * 86_400;
        match ctx
            .db
            .call(move |conn| Ok(store::forget_old_rounds(conn, before)?))
            .await
        {
            Ok(0) => {}
            Ok(cleared) => info!("cleared the raw output of {cleared} old answers"),
            Err(err) => error!("clearing old answers' raw output: {err:#}"),
        }
        timer.sleeping(CHECK);
        tokio::select! {
            () = tokio::time::sleep(CHECK) => {}
            () = ctx.shutdown.cancelled() => break,
        }
    }
}

/// Describes the emoji of every server chat is on in that don't have a description yet,
/// and backfills missing short descriptions from existing long descriptions.
async fn describe_new(ctx: &BotCtx) -> Result<()> {
    if ctx.ai.chat().is_none() {
        return Ok(());
    }
    for guild in ctx.guild_ids() {
        if !ctx.gate("chat").allows_guild(guild) {
            continue;
        }
        let id = guild.get();
        let described = ctx
            .db
            .call(move |conn| Ok(store::emoji_descriptions(conn, id)?))
            .await?;
        let all = server_emoji(ctx, guild);
        let new: Vec<Emoji> = all
            .iter()
            .filter(|e| {
                !failed_before(e)
                    && described
                        .get(&e.id.get())
                        .is_none_or(|entry| entry.description.is_empty())
            })
            .cloned()
            .collect();
        if !new.is_empty() {
            info!("describing {} new emoji", new.len());
        }
        for emoji in new {
            if ctx.shutdown.is_cancelled() {
                return Ok(());
            }
            match describe(ctx, &emoji).await {
                Ok((short, long)) => {
                    let (emoji_id, name) = (emoji.id.get(), emoji.name.clone());
                    ctx.db
                        .call(move |conn| {
                            Ok(store::set_emoji_description(
                                conn, emoji_id, id, &name, &long, &short,
                            )?)
                        })
                        .await?;
                }
                Err(err) => {
                    warn!("couldn't describe :{}: {err:#}", emoji.name);
                    let mut failed = FAILED.lock().unwrap();
                    failed.get_or_insert_default().insert(emoji.id.get());
                }
            }
        }

        // Backfill existing rows that have a long description but no short gloss yet,
        // using a cheap text-only call so we don't re-download every emoji image.
        let need_short: Vec<(Emoji, String)> = all
            .into_iter()
            .filter(|e| !failed_before(e))
            .filter_map(|e| {
                let entry = described.get(&e.id.get())?;
                (!entry.description.is_empty() && entry.short_description.is_empty())
                    .then(|| (e, entry.description.clone()))
            })
            .collect();
        if !need_short.is_empty() {
            info!("backfilling {} short emoji descriptions", need_short.len());
        }
        for (emoji, long) in need_short {
            if ctx.shutdown.is_cancelled() {
                return Ok(());
            }
            match condense_short(ctx, &emoji.name, &long).await {
                Ok(short) => {
                    let emoji_id = emoji.id.get();
                    ctx.db
                        .call(move |conn| {
                            Ok(store::set_emoji_short_description(conn, emoji_id, &short)?)
                        })
                        .await?;
                }
                Err(err) => {
                    warn!(
                        "couldn't backfill short description for :{}: {err:#}",
                        emoji.name
                    );
                    let mut failed = FAILED.lock().unwrap();
                    failed.get_or_insert_default().insert(emoji.id.get());
                }
            }
        }
    }
    Ok(())
}

fn failed_before(emoji: &Emoji) -> bool {
    let failed = FAILED.lock().unwrap();
    failed
        .as_ref()
        .is_some_and(|ids| ids.contains(&emoji.id.get()))
}

/// Shows the model the emoji's picture (frames, for an animated one) and returns
/// `(short_description, long_description)`.
async fn describe(ctx: &BotCtx, emoji: &Emoji) -> Result<(String, String)> {
    let provider = ctx.ai.chat().ok_or_else(|| anyhow::anyhow!("no model"))?;
    let media = if emoji.animated {
        Media {
            url: format!("https://cdn.discordapp.com/emojis/{}.gif", emoji.id),
            kind: MediaKind::Gif,
            mime: "image/gif",
        }
    } else {
        Media {
            url: format!(
                "https://cdn.discordapp.com/emojis/{}.png?size=128",
                emoji.id
            ),
            kind: MediaKind::Image,
            mime: "image/png",
        }
    };
    let mut parts = vec![Part::Text(format!("The emoji :{}:", emoji.name))];
    parts.extend(
        media::load_for_model(&media)
            .await?
            .into_iter()
            .map(Part::Image),
    );
    let request = ChatRequest {
        system: SYSTEM.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts,
        }]),
        tools: Vec::new(),
        cache_key: "emoji".to_string(),
        job: "emoji",
        answer_now: false,
    };
    let done = complete(provider.as_ref(), request, &NoTools, 1).await?;
    parse_description(&done.text)
}

/// Condenses an existing long emoji description into a 3–6 word gloss (no image needed).
async fn condense_short(ctx: &BotCtx, name: &str, long: &str) -> Result<String> {
    let provider = ctx.ai.chat().ok_or_else(|| anyhow::anyhow!("no model"))?;
    let request = ChatRequest {
        system: SHORT_SYSTEM.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts: vec![Part::Text(format!(":{name}: {long}"))],
        }]),
        tools: Vec::new(),
        cache_key: "emoji".to_string(),
        job: "emoji",
        answer_now: false,
    };
    let done = complete(provider.as_ref(), request, &NoTools, 1).await?;
    let line = done.text.trim().lines().next().unwrap_or_default().trim();
    let cleaned = strip_label(line, "SHORT:")
        .unwrap_or(line)
        .trim_end_matches('.');
    if cleaned.is_empty() {
        anyhow::bail!("the model gave no short description");
    }
    Ok(shorten(cleaned, 60))
}

/// Parses the two-line `SHORT:` / `LONG:` output from [`describe`].
/// Falls back cleanly if a model returns a single line or omits one label.
fn parse_description(text: &str) -> Result<(String, String)> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        anyhow::bail!("the model gave no description");
    }

    let mut short: Option<&str> = None;
    let mut long: Option<&str> = None;
    for line in &lines {
        if let Some(rest) = strip_label(line, "SHORT:") {
            if !rest.is_empty() {
                short = Some(rest);
            }
        } else if let Some(rest) = strip_label(line, "LONG:")
            && !rest.is_empty()
        {
            long = Some(rest);
        }
    }

    match (short, long) {
        (Some(s), Some(l)) => Ok((clean_short(s), shorten(l, 150))),
        (None, Some(l)) => {
            let long = shorten(l, 150);
            Ok((short_fallback(&long), long))
        }
        (Some(s), None) => {
            let short = clean_short(s);
            let long = shorten(s, 150);
            Ok((short, long))
        }
        (None, None) => {
            let long = shorten(lines[0], 150);
            Ok((short_fallback(&long), long))
        }
    }
}

/// Strips a case-insensitive prefix like `"SHORT:"` and trims the remainder.
fn strip_label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let prefix = line.get(..label.len())?;
    prefix
        .eq_ignore_ascii_case(label)
        .then(|| line[label.len()..].trim())
}

fn clean_short(text: &str) -> String {
    shorten(text.trim().trim_end_matches('.'), 60)
}

/// Shortens a full sentence to its first 6 words for a safe fallback gloss.
fn short_fallback(long: &str) -> String {
    let words: Vec<&str> = long.split_whitespace().take(6).collect();
    let joined = words.join(" ");
    let trimmed = joined.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    shorten(trimmed, 60)
}

/// For one-round requests: there are no tools to call.
pub(super) struct NoTools;

#[async_trait]
impl ToolRunner for NoTools {
    async fn run(&self, call: &ToolCall) -> ToolReply {
        format!("Error: there is no tool named {}.", call.name).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_codes() -> HashMap<String, String> {
        HashMap::from([
            ("pog".to_string(), "<:pog:123>".to_string()),
            ("dance".to_string(), "<a:dance:456>".to_string()),
        ])
    }

    #[test]
    fn names_become_codes() {
        let codes = test_codes();
        assert_eq!(with_codes("nice :pog:", &codes), "nice <:pog:123>");
        assert_eq!(
            with_codes(":dance::pog: yes", &codes),
            "<a:dance:456><:pog:123> yes"
        );
    }

    #[test]
    fn written_codes_get_the_right_id() {
        let codes = test_codes();
        assert_eq!(with_codes("<:pog:123>", &codes), "<:pog:123>");
        assert_eq!(with_codes("<:pog:999> hi", &codes), "<:pog:123> hi");
        assert_eq!(with_codes("<:pog:>", &codes), "<:pog:123>");
        assert_eq!(with_codes("<:dance:456>", &codes), "<a:dance:456>");
        assert_eq!(with_codes("<a:dance:456>", &codes), "<a:dance:456>");
    }

    #[test]
    fn other_colons_stay() {
        let codes = test_codes();
        for text in [
            "at 10:30: lunch",
            "note: :unknown: here",
            "<:other:77>",
            "https://example.com:8080/pog",
            "pog",
            "a:pog",
        ] {
            assert_eq!(with_codes(text, &codes), text);
        }
        assert_eq!(with_codes(":pog:", &HashMap::new()), ":pog:");
    }

    #[test]
    fn reactions_by_name() {
        let codes = test_codes();
        assert_eq!(reaction_code("pog", &codes), "<:pog:123>");
        assert_eq!(reaction_code(":dance:", &codes), "<a:dance:456>");
        assert_eq!(reaction_code("😂", &codes), "😂");
    }

    #[test]
    fn parses_short_and_long_descriptions() {
        let (short, long) = parse_description(
            "SHORT: smug cheeky cat grin.\nLONG: A cat smirking knowingly, used for teasing or smug wins.",
        )
        .unwrap();
        assert_eq!(short, "smug cheeky cat grin");
        assert_eq!(
            long,
            "A cat smirking knowingly, used for teasing or smug wins."
        );

        // Case-insensitive labels.
        let (short, long) =
            parse_description("short: hyped shock\nlong: Wide-eyed excited face.").unwrap();
        assert_eq!(short, "hyped shock");
        assert_eq!(long, "Wide-eyed excited face.");
    }

    #[test]
    fn parse_description_falls_back_on_single_line() {
        let (short, long) =
            parse_description("A tired frog sipping coffee, used when waking up early.").unwrap();
        assert_eq!(short, "A tired frog sipping coffee, used");
        assert_eq!(
            long,
            "A tired frog sipping coffee, used when waking up early."
        );

        // Only LONG provided.
        let (short, long) = parse_description("LONG: Laughing dog rolling on the floor.").unwrap();
        assert_eq!(short, "Laughing dog rolling on the floor");
        assert_eq!(long, "Laughing dog rolling on the floor.");

        // Only SHORT provided.
        let (short, long) = parse_description("SHORT: pure chaos energy.").unwrap();
        assert_eq!(short, "pure chaos energy");
        assert_eq!(long, "pure chaos energy.");

        assert!(parse_description("   \n  ").is_err());
    }

    #[test]
    fn compact_line_uses_short_without_snowflake_ids() {
        assert_eq!(
            format_compact_line("pog", Some("surprised hype")),
            ":pog: surprised hype"
        );
        assert_eq!(format_compact_line("pog", Some("")), ":pog:");
        assert_eq!(format_compact_line("pog", Some("   ")), ":pog:");
        assert_eq!(format_compact_line("pog", None), ":pog:");
        assert!(!format_compact_line("pog", Some("surprised hype")).contains("<:"));
    }
}
