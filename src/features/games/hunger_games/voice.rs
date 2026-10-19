//! The Gamemaker's voice: her announcements (opening the Games, the Octagon, the
//! showdown) and her words to the victor, also sent as Discord voice messages in a voice of
//! their own (`gamemaker` in `[features.voice.voices]`).
//!
//! This file doesn't import the voice feature: it finds whichever feature offers the
//! `send_voice_message` chat tool and runs it as Vivy, with the voice's name and no reply,
//! the way `notes.rs` reads memory. So the voice feature's gate and monthly limit apply,
//! and without an ElevenLabs key, or with voice off in the channel, the lines are text only.

use serde_json::json;
use serenity::all::{ChannelId, GuildId, MessageId};
use tracing::{debug, info};

use crate::core::{Asker, BotCtx};

/// The voice feature's tool.
const TOOL: &str = "send_voice_message";
/// The Gamemaker's voice in `[features.voice.voices]`.
const VOICE: &str = "gamemaker";

/// Says `line` (audio tags included) in the Gamemaker's voice, as a voice message of its
/// own in `channel`. `message` is any message of the game, for the voice feature's records.
/// Fails quietly: the line is in the round's text anyway.
pub async fn speak(
    ctx: &BotCtx,
    guild: GuildId,
    channel: ChannelId,
    message: MessageId,
    line: String,
) {
    if !ctx.allows("voice", Some(guild), channel) {
        return;
    }
    let Some(voice) = ctx
        .features
        .iter()
        .find(|f| f.tools().iter().any(|t| t.name == TOOL))
    else {
        return;
    };
    let herself = Asker {
        user: ctx.bot_id,
        guild: Some(guild),
        channel,
        message,
        posted: Default::default(),
    };
    let characters = line.chars().count();
    let args = json!({"script": line, "voice": VOICE, "reply": false});
    match voice.run_tool(ctx, &herself, TOOL, &args).await {
        Ok(_) => info!(characters, "the Gamemaker spoke"),
        Err(err) => debug!("the Gamemaker couldn't speak: {err:#}"),
    }
}
