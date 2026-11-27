//! Voice: Vivy sends Discord voice messages in her own voice, made with ElevenLabs.
//!
//! Chat gets one tool, `send_voice_message`: she writes a short script with audio tags
//! (`[whispers]`, `[laughs]`), ElevenLabs speaks it in the voice `voice_id`, and the bot
//! posts it as a real voice message (the kind with a play button and bars) replying to the
//! asker. Only her script is sent to ElevenLabs.
//!
//! - `speech.rs`: the ElevenLabs request
//! - `audio.rs`: OGG Opus, length and waveform with ffmpeg
//! - `store.rs`: what was sent, for the monthly character limit
//!
//! The key goes in .env as `ELEVENLABS_API_KEY`. Without it the tool isn't offered.
//!
//! Other features can send voice messages through the same tool (`run_tool`), so they count
//! toward the monthly limit too. They can pass two arguments the chat model isn't told
//! about: `voice`, the name of another voice from `[features.voice.voices]` (the Hunger
//! Games' Gamemaker speaks with `gamemaker`), and `reply: false` to post it on its own.

mod audio;
mod speech;
mod store;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use async_trait::async_trait;
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Feature, Result, Stat, user_error};
use crate::util::discord::send_raw;
use crate::util::search::snowflake_at;
use crate::util::{env_key, month_start};

const KEY_VAR: &str = "ELEVENLABS_API_KEY";
pub const TOOL: &str = "send_voice_message";
/// The Hunger Games' Gamemaker voice, made by Cherry with ElevenLabs Voice Design.
const GAMEMAKER_VOICE: &str = "DucUc05HgR5M3wbY6WkG";
/// Discord's flag for a voice message.
const IS_VOICE_MESSAGE: u64 = 1 << 13;

/// `[features.voice]` settings.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The ElevenLabs voice she speaks with.
    pub voice_id: String,
    /// Other voices by name, for other features (`gamemaker` for the Hunger Games). An
    /// empty ID turns that voice off.
    pub voices: HashMap<String, String>,
    /// "eleven_v4", or "eleven_v4_turbo" for faster and half the price.
    pub model: String,
    /// 0 to 1: lower is more expressive and varied, higher is steadier.
    pub stability: f64,
    /// 0 to 1: how closely it sticks to the voice.
    pub similarity: f64,
    /// A language code like "en" to force a language. Left out, it follows the text.
    pub language: Option<String>,
    /// The most characters a month, all voice messages together. 0 for no limit.
    pub monthly_characters: u64,
    /// The most characters in one voice message.
    pub max_characters: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            voice_id: String::new(),
            voices: HashMap::from([("gamemaker".to_string(), GAMEMAKER_VOICE.to_string())]),
            model: "eleven_v4".to_string(),
            stability: 0.5,
            similarity: 0.75,
            language: None,
            monthly_characters: 50_000,
            max_characters: 1_000,
        }
    }
}

/// The ElevenLabs key from .env, if it's set.
fn key() -> Option<String> {
    env_key(KEY_VAR)
}

/// Tracks characters currently in flight to ElevenLabs so concurrent calls cannot race past
/// `monthly_characters` before the finished call is written to SQLite.
#[derive(Default)]
struct Reservations {
    in_flight: Arc<Mutex<u64>>,
}

/// Releases reserved characters when a call finishes recording, fails, or is cancelled.
struct Reservation {
    in_flight: Arc<Mutex<u64>>,
    characters: u64,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *in_flight = in_flight.saturating_sub(self.characters);
    }
}

impl Reservations {
    /// Checks `used + in_flight + characters <= limit` (when `limit > 0`) and reserves
    /// `characters` until the returned guard is dropped.
    fn try_reserve(&self, used: u64, characters: u64, limit: u64) -> Result<Reservation> {
        let mut in_flight = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let total = used.saturating_add(*in_flight);
        if limit > 0 && total.saturating_add(characters) > limit {
            return Err(user_error(format!(
                "your voice is used up for this month ({total} of {limit} characters). Answer in text."
            )));
        }
        *in_flight = in_flight.saturating_add(characters);
        Ok(Reservation {
            in_flight: Arc::clone(&self.in_flight),
            characters,
        })
    }
}

#[derive(Default)]
pub struct Voice {
    reservations: Reservations,
}

#[async_trait]
impl Feature for Voice {
    fn name(&self) -> &'static str {
        "voice"
    }

    fn migrations(&self) -> &'static [&'static str] {
        store::MIGRATIONS
    }

    fn tools(&self) -> Vec<ToolDef> {
        if key().is_none() {
            return Vec::new();
        }
        vec![ToolDef {
            name: TOOL,
            description: "Send a Discord voice message in your own voice, as a reply to the asker. Use it when someone sends you a voice message or asks to hear you, and now and then when saying something fits better than typing it: a greeting, a laugh, a sleepy good night, a reaction. If voice is unavailable or over quota, answer in text instead. Keep it short, one to four sentences. \
Write it as spoken words: no markdown, links, emoji or lists. Steer how you say it with audio tags in square brackets before the words they're for, in plain English: [whispers], [excited], [sighs], [laughs], [yawns], [sarcastic], [sleepy, slow]. A tag lasts until the next one; put several in one bracket with commas; don't mix opposite ones like [whispers, shouting]. Let your current mood show in how you sound. \
The voice message is your whole answer, so usually write no text with it.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "script": {"type": "string", "description": "What you say, with audio tags."}
                },
                "required": ["script"]
            }),
        }]
    }

    fn tool_status(&self, _tool: &str, _args: &Value) -> Option<&'static str> {
        Some("🎙️ Recording a voice message…")
    }

    async fn run_tool(
        &self,
        ctx: &BotCtx,
        asker: &Asker,
        _name: &str,
        args: &Value,
    ) -> Result<String> {
        let key = key().ok_or_else(|| user_error("voice messages aren't set up"))?;
        let settings: Settings = ctx.config.feature("voice")?;
        let voice_id = match args.get("voice").and_then(Value::as_str) {
            Some(name) => settings.voices.get(name).map_or("", |id| id.trim()),
            None => settings.voice_id.trim(),
        };
        if voice_id.is_empty() {
            return Err(user_error(
                "voice messages aren't set up (no voice ID in config.toml)",
            ));
        }
        let voice_id = voice_id.to_string();
        let reply = args.get("reply").and_then(Value::as_bool).unwrap_or(true);
        let script = args
            .get("script")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| user_error("give the script to say"))?
            .to_string();
        let characters = script.chars().count();
        if characters > settings.max_characters {
            return Err(user_error(format!(
                "that's {characters} characters; a voice message can have at most {}. Say less.",
                settings.max_characters
            )));
        }
        let since = month_start(Utc::now());
        let (_, used) = ctx
            .db
            .call(move |conn| Ok(store::used_since(conn, since)?))
            .await?;
        let reservation =
            self.reservations
                .try_reserve(used, characters as u64, settings.monthly_characters)?;

        let audio = speech::speak(&ctx.web, &key, &settings, &voice_id, &script).await?;
        // ElevenLabs bills when TTS succeeds, so record the characters right away (under a
        // temporary ID) before ffmpeg or Discord upload can fail.
        let pending_id = snowflake_at(Utc::now()) + fastrand::u64(..1 << 22);
        let sent = store::Sent {
            message_id: pending_id,
            guild_id: asker.guild.map(|g| g.get()),
            channel_id: asker.channel.get(),
            user_id: asker.user.get(),
            characters,
            seconds: 0.0,
            at: Utc::now().timestamp(),
        };
        ctx.db
            .call(move |conn| Ok(store::record(conn, &sent)?))
            .await?;
        drop(reservation);

        let clip = audio::prepare(&audio).await?;
        let mut payload = json!({
            "flags": IS_VOICE_MESSAGE,
            "attachments": [{
                "id": 0,
                "filename": "voice-message.ogg",
                "duration_secs": clip.seconds,
                "waveform": BASE64_STANDARD.encode(&clip.waveform),
            }],
            "allowed_mentions": {"parse": [], "replied_user": true},
        });
        if reply {
            payload["message_reference"] = json!({
                "message_id": asker.message.get(),
                "channel_id": asker.channel.get(),
                "fail_if_not_exists": false,
            });
        }
        let file = ("voice-message.ogg".to_string(), clip.ogg);
        let message = send_raw(&ctx.http, asker.channel, &payload, &[file])
            .await
            .context("sending the voice message")?;
        info!(channel = %asker.channel, seconds = clip.seconds, characters, "sent a voice message");
        asker
            .posted
            .add(message.id, format!("[Voice message] {script}"));

        let message_id = message.id.get();
        let seconds = clip.seconds;
        if let Err(err) = ctx
            .db
            .call(move |conn| Ok(store::update_sent(conn, pending_id, message_id, seconds)?))
            .await
        {
            warn!("couldn't update voice message record: {err:#}");
        }
        Ok(format!(
            "Your voice message is sent ({:.1} seconds). It is your answer: write no text after it, unless something can only be shown in writing (a link, code).",
            clip.seconds
        ))
    }

    async fn stats(&self, ctx: &BotCtx) -> Result<Vec<Stat>> {
        if key().is_none() {
            return Ok(Vec::new());
        }
        let settings: Settings = ctx.config.feature("voice")?;
        let since = month_start(Utc::now());
        let (sent, used) = ctx
            .db
            .call(move |conn| Ok(store::used_since(conn, since)?))
            .await?;
        let characters = match settings.monthly_characters {
            0 => used.to_string(),
            limit => format!("{used} / {limit}"),
        };
        Ok(vec![
            Stat::new("Sent this month", sent),
            Stat::new("Characters", characters),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_have_defaults() {
        let settings = Settings::default();
        assert_eq!(settings.model, "eleven_v4");
        assert!(settings.voice_id.is_empty());
        assert_eq!(settings.voices["gamemaker"], GAMEMAKER_VOICE);
    }

    #[test]
    fn reservations_block_concurrent_overspend_and_release_on_drop() {
        let reservations = Reservations::default();
        // 20 already recorded in SQLite, monthly limit 100.
        let first = reservations.try_reserve(20, 50, 100).unwrap();
        // 20 + 50 in flight + 40 would exceed 100.
        let err = reservations.try_reserve(20, 40, 100).err().unwrap();
        assert!(err.to_string().contains("70 of 100"));
        // 20 + 50 + 30 fits exactly.
        let second = reservations.try_reserve(20, 30, 100).unwrap();
        assert!(reservations.try_reserve(20, 1, 100).is_err());

        // Dropping the first guard (e.g. TTS failed or cancelled) frees its 50 chars.
        drop(first);
        let third = reservations.try_reserve(20, 50, 100).unwrap();
        drop(second);
        drop(third);
        assert_eq!(*reservations.in_flight.lock().unwrap(), 0);
    }

    #[test]
    fn reservations_allow_unlimited_when_limit_is_zero() {
        let reservations = Reservations::default();
        let guard = reservations.try_reserve(100_000, 50_000, 0).unwrap();
        assert_eq!(*reservations.in_flight.lock().unwrap(), 50_000);
        drop(guard);
        assert_eq!(*reservations.in_flight.lock().unwrap(), 0);
    }
}
