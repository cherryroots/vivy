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
use crate::ai::{ChatRequest, Input, Part, Role, ToolCall, ToolDef, ToolRunner, Turn, complete};
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

const SYSTEM: &str = "You describe Discord custom emoji. Answer with one short line, under 15 words: what the picture shows and the feeling or reaction it's used for. No preamble.";

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

/// The server's emoji for a chime-in that may react, so she can pick one of them without
/// looking them up first. Empty when the server has none (or in DMs).
pub async fn for_chime_in(ctx: &BotCtx, guild: Option<GuildId>) -> String {
    let Some(guild) = guild else {
        return String::new();
    };
    match described(ctx, guild).await {
        Ok(lines) if !lines.is_empty() => format!(
            "\n\nBesides plain emoji you can use this server's own, in a line or as your reaction, written as their code:\n{}",
            lines.join("\n")
        ),
        Ok(_) => String::new(),
        Err(err) => {
            warn!("listing the server's emoji: {err:#}");
            String::new()
        }
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
            Some(text) => format!("{e}: {text}"),
            None => e.to_string(),
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

/// Describes the emoji of every server chat is on in that don't have a description yet.
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
        let new: Vec<Emoji> = server_emoji(ctx, guild)
            .into_iter()
            .filter(|e| !described.contains_key(&e.id.get()) && !failed_before(e))
            .collect();
        if !new.is_empty() {
            info!("describing {} new emoji", new.len());
        }
        for emoji in new {
            if ctx.shutdown.is_cancelled() {
                return Ok(());
            }
            match describe(ctx, &emoji).await {
                Ok(text) => {
                    let (emoji_id, name) = (emoji.id.get(), emoji.name.clone());
                    ctx.db
                        .call(move |conn| {
                            Ok(store::set_emoji_description(
                                conn, emoji_id, id, &name, &text,
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
    }
    Ok(())
}

fn failed_before(emoji: &Emoji) -> bool {
    let failed = FAILED.lock().unwrap();
    failed
        .as_ref()
        .is_some_and(|ids| ids.contains(&emoji.id.get()))
}

/// Shows the model the emoji's picture (frames, for an animated one) and returns its line.
async fn describe(ctx: &BotCtx, emoji: &Emoji) -> Result<String> {
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
    let line = done.text.trim().lines().next().unwrap_or_default().trim();
    if line.is_empty() {
        anyhow::bail!("the model gave no description");
    }
    Ok(shorten(line, 150))
}

struct NoTools;

#[async_trait]
impl ToolRunner for NoTools {
    async fn run(&self, call: &ToolCall) -> String {
        format!("Error: there is no tool named {}.", call.name)
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
}
