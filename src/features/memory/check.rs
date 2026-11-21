//! `check_messages`: a read-only tool the reflection has next to `memory`, so Vivy can look
//! up what people really said when a note seems doubtful (two notes disagree, a note is
//! old, or it says what someone likes "per" someone else). What people wrote in the server
//! is the ground truth; her notes are only what she made of it.
//!
//! It uses Discord's message search (`util::search`), keeps to the channels chat may read
//! (`[features.chat]` gating, threads counting as their parent), leaves out age-restricted
//! channels, and allows only a few searches per reflection, since each one adds a round.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::DateTime;
use serde_json::{Value, json};

use super::tool::FolderRunner;
use crate::ai::{ToolCall, ToolDef, ToolReply, ToolRunner};
use crate::core::BotCtx;
use crate::util::discord::{ChannelId, GuildId, display_name, get_channel};
use crate::util::search::{self, Search};
use crate::util::shorten;

pub const NAME: &str = "check_messages";
/// The most searches in one reflection.
const MAX_CHECKS: usize = 8;
/// Messages shown per search.
const LIMIT: u32 = 10;
/// The longest message text shown.
const MAX_MESSAGE: usize = 300;

pub fn def() -> ToolDef {
    ToolDef {
        name: NAME,
        description: "Search what people really wrote in this server, to check a note you doubt: two notes disagree, a note is old, or it's someone else's word about a person. Give `from` (a user id), `query` (words), or both. Answers with up to 10 messages, newest first. Only a few searches per reflection, so use it for notes that matter.",
        parameters: json!({
            "type": "object",
            "properties": {
                "from": {"type": "string", "description": "The user id whose messages to search."},
                "query": {"type": "string", "description": "Words the messages contain."}
            }
        }),
    }
}

/// The reflection's tools: `memory` (as herself) and `check_messages` in one server.
pub struct ReflectRunner {
    pub folder: FolderRunner,
    pub guild: GuildId,
    pub checks: AtomicUsize,
}

#[async_trait]
impl ToolRunner for ReflectRunner {
    async fn run(&self, call: &ToolCall) -> ToolReply {
        if call.name != NAME {
            return self.folder.run(call).await;
        }
        if self.checks.fetch_add(1, Ordering::Relaxed) >= MAX_CHECKS {
            return "Error: no searches left in this reflection. Decide from the notes you have."
                .to_string()
                .into();
        }
        match check(&self.folder.ctx, self.guild, &call.args).await {
            Ok(text) => text.into(),
            Err(err) => format!("Error: {err:#}").into(),
        }
    }
}

async fn check(ctx: &BotCtx, guild: GuildId, args: &Value) -> anyhow::Result<String> {
    let text = |key: &str| {
        args[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    // The model may write the id as a number, or as a mention.
    let from = match &args["from"] {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s
            .trim()
            .trim_start_matches("<@")
            .trim_start_matches('!')
            .trim_end_matches('>')
            .parse()
            .ok(),
        _ => None,
    };
    let query = text("query");
    if from.is_none() && query.is_none() {
        anyhow::bail!("give `from` (a user id), `query`, or both");
    }
    let found = search::search(
        &ctx.web,
        ctx.token(),
        guild,
        &Search {
            content: query,
            author_ids: from.into_iter().collect(),
            limit: Some(LIMIT),
            ..Default::default()
        },
    )
    .await?;

    // Discord searched every channel the bot can see; keep the ones chat may read.
    let mut readable: HashMap<ChannelId, bool> = HashMap::new();
    let mut lines = Vec::new();
    for message in &found.messages {
        let channel = message.channel_id;
        let allowed = match readable.get(&channel) {
            Some(allowed) => *allowed,
            None => {
                let allowed = chat_may_read(ctx, guild, channel).await;
                readable.insert(channel, allowed);
                allowed
            }
        };
        if !allowed {
            continue;
        }
        let time = DateTime::from_timestamp(message.timestamp.as_secs(), 0)
            .unwrap_or_default()
            .format("%Y-%m-%d");
        let place = ctx
            .channel_name(guild, channel)
            .or_else(|| {
                found
                    .threads
                    .iter()
                    .find(|thread| thread.id == channel)
                    .and_then(|thread| thread.name.clone())
            })
            .unwrap_or_else(|| "unknown".to_string());
        lines.push(format!(
            "[{time}] {} (user {}) in #{place}: {}",
            display_name(&message.author),
            message.author.id,
            shorten(message.content.trim(), MAX_MESSAGE)
        ));
    }
    if lines.is_empty() {
        return Ok("No messages found.".to_string());
    }
    Ok(format!(
        "{} messages match; the newest:\n{}",
        found.total,
        lines.join("\n")
    ))
}

/// Whether chat's gating allows `channel`, with a thread counting as its parent too. The
/// channel comes from Discord, since archived threads aren't in the cache. Age-restricted
/// channels never count: the search leaves them out, and this keeps it that way.
async fn chat_may_read(ctx: &BotCtx, guild: GuildId, channel: ChannelId) -> bool {
    let Ok(target) = get_channel(&ctx.http, channel).await else {
        return false;
    };
    if target.guild_id != Some(guild) || target.nsfw == Some(true) {
        return false;
    }
    let parent = target.parent_id.filter(|_| target.kind.is_thread());
    ctx.gate("chat").allows(Some(guild), channel, parent)
}
