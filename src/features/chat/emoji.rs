//! The server's custom emoji, so Vivy can use them like a regular.
//!
//! A background task looks at each emoji's picture once and saves a short description
//! (`chat_emoji`): every emoji at the first start, then only new ones, checked at each
//! start and once a day ([`CHECK`]), since new emoji are rare. The `list_server_emoji` tool lists them with their descriptions.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use serenity::all::{Emoji, GuildId};
use tracing::{Instrument as _, error, info, info_span, warn};

use super::store;
use crate::ai::{ChatRequest, Input, Part, Role, ToolCall, ToolDef, ToolRunner, Turn, complete};
use crate::core::{Asker, BotCtx, Result};
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
    let emoji = server_emoji(ctx, guild);
    if emoji.is_empty() {
        return Ok("This server has no custom emoji.".to_string());
    }
    let id = guild.get();
    let described = ctx
        .db
        .call(move |conn| Ok(store::emoji_descriptions(conn, id)?))
        .await?;
    let lines: Vec<String> = emoji
        .iter()
        .take(MAX_LISTED)
        .map(|e| match described.get(&e.id.get()) {
            Some(text) => format!("{e}: {text}"),
            None => e.to_string(),
        })
        .collect();
    Ok(lines.join("\n"))
}

/// The usable custom emoji of a server, from the cache, by name.
fn server_emoji(ctx: &BotCtx, guild: GuildId) -> Vec<Emoji> {
    let Some(guild) = ctx.cache.guild(guild) else {
        return Vec::new();
    };
    let mut list: Vec<Emoji> = guild
        .emojis
        .values()
        .filter(|e| e.available)
        .cloned()
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
    for guild in ctx.cache.guilds() {
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
