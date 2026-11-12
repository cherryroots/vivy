//! Chiming in: now and then Vivy reads along in a channel and maybe reacts to the last
//! message with an emoji, without anyone mentioning her. She never posts a line on her own:
//! Cherry's friends didn't like it, so only reactions are left.
//!
//! After each message there's a small chance (`chime_chance` in `[features.chat]`) that she
//! reads the last [`READ_MESSAGES`] messages, mostly to learn from them for her memory. A
//! second roll decides whether she also reacts that time (`chime_react_chance`); the rest she
//! only reads. A channel then waits `chime_cooldown_minutes` before she reads it again.
//!
//! Nobody asked, so she can only use [`UNPROMPTED_TOOLS`]: tools that read the server (any
//! channel chat isn't turned off in, see `tools::can_read`) and her own memory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::util::discord::{
    ChannelId, GuildId, Message, UserId, display_name, mentions_user, react,
};
use anyhow::Context as _;
use async_trait::async_trait;
use chrono::Utc;
use chrono_tz::Tz;
use serde::Deserialize;
use tracing::{info, warn};

use super::answer::{self, SYSTEM_PROMPT};
use super::emoji;
use super::look_back::one_line;
use super::store::{self, NewTurn, StoredPart, Written};
use super::tools::format_message;
use crate::ai::{ChatRequest, Input, Part, Role, ToolCall, ToolReply, ToolRunner, Turn, complete};
use crate::core::{Asker, BotCtx, Feature, Result};
use crate::util::shorten_at_word;

/// How many messages she reads before deciding.
const READ_MESSAGES: u16 = 15;
/// Rounds of tool calls (saving to memory) before she has to decide.
const MAX_ROUNDS: usize = 6;

/// The tools she may use when nobody asked. An allow-list, so a new tool stays out until
/// someone decides it's safe here. Left out: anything that acts for a person (reminders),
/// and `list_channels`, which lists what one person can see, not everyone here.
/// `get_message`, `search_messages` and `read_recent_messages` reach the channels chat may
/// read (`tools::can_read`). `memory` is in: what she saves is her own decision.
/// `tinyfish_search` and `tinyfish_fetch` only read the web, like the providers' own web
/// tools, which chime-ins have too.
const UNPROMPTED_TOOLS: &[&str] = &[
    "get_current_time",
    "get_channel_info",
    "get_user_info",
    "get_bot_owner",
    "read_recent_messages",
    "get_message",
    "search_messages",
    "get_pinned_messages",
    "list_server_events",
    "list_server_emoji",
    "get_wheel_status",
    "tinyfish_search",
    "tinyfish_fetch",
    "memory",
];

/// `[features.chat]` settings: reading along, and some limits on answers.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The most rounds of tool calls in one answer, so a confused model can't loop forever.
    pub max_tool_rounds: usize,
    /// How far up a reply chain the history goes, in messages.
    pub history_turns: usize,
    /// The chance, from 0 to 1, that a message makes her read along. 0 turns it off.
    pub chime_chance: f64,
    /// Of those reads, the share where she reacts to the last message (0 to 1).
    pub chime_react_chance: f64,
    /// How long a channel waits after she read along.
    pub chime_cooldown_minutes: u64,
    /// Other chances for some people's messages: `[user, chance]` anywhere, or
    /// `[user, chance, channel]` in one channel only. Used instead of `chime_chance`.
    pub chime_chance_per_user: Vec<PersonChance>,
}

/// One `chime_chance_per_user` entry: a user ID, a chance from 0 to 1, and maybe a channel ID.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PersonChance {
    InChannel(u64, f64, u64),
    Anywhere(u64, f64),
}

impl Settings {
    /// The chance, from 0 to 1, for a message from `user` in `channel`. An entry for that
    /// channel comes first, then one for anywhere, then `chime_chance`.
    fn chance_for(&self, user: UserId, channel: ChannelId) -> f64 {
        let (user, channel) = (user.get(), channel.get());
        let in_channel = self
            .chime_chance_per_user
            .iter()
            .find_map(|entry| match entry {
                PersonChance::InChannel(u, chance, c) if *u == user && *c == channel => {
                    Some(*chance)
                }
                _ => None,
            });
        let anywhere = || {
            self.chime_chance_per_user
                .iter()
                .find_map(|entry| match entry {
                    PersonChance::Anywhere(u, chance) if *u == user => Some(*chance),
                    _ => None,
                })
        };
        in_channel.or_else(anywhere).unwrap_or(self.chime_chance)
    }

    /// What she may do on a read, from a second roll `dice` between 0 and 1.
    fn allowed(&self, dice: f64) -> Allowed {
        if dice < self.chime_react_chance {
            Allowed::React
        } else {
            Allowed::ReadOnly
        }
    }
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            max_tool_rounds: 8,
            history_turns: 40,
            chime_chance: 0.05,
            chime_react_chance: 0.40,
            chime_cooldown_minutes: 30,
            chime_chance_per_user: Vec::new(),
        }
    }
}

/// What she may do after reading along, decided by the dice before she reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Allowed {
    /// Only read, and save to memory what's worth keeping.
    ReadOnly,
    /// React to the last message with one emoji (or pass).
    React,
}

/// The `[features.chat]` settings, read once.
static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// The `[features.chat]` settings, read the first time they're needed.
pub(super) fn settings(ctx: &BotCtx) -> &'static Settings {
    SETTINGS.get_or_init(|| {
        ctx.config.feature("chat").unwrap_or_else(|err| {
            warn!("chat uses its default settings: {err:#}");
            Settings::default()
        })
    })
}

#[derive(Default)]
pub struct Chime {
    /// When she last considered each channel.
    last: Mutex<HashMap<ChannelId, Instant>>,
}

impl Chime {
    pub(super) fn settings(&self, ctx: &BotCtx) -> &'static Settings {
        settings(ctx)
    }

    /// Rolls the dice for a message, and reads along if it hits. A second roll decides
    /// whether she may also react.
    pub async fn on_message(&self, ctx: &BotCtx, msg: &Message) -> Result<()> {
        let Some(guild) = msg.guild_id.filter(|_| ctx.ai.chat().is_some()) else {
            return Ok(());
        };
        // Messages to her get a real answer instead.
        let to_her = mentions_user(msg, ctx.bot_id)
            || msg
                .referenced_message
                .as_ref()
                .is_some_and(|r| r.author.id == ctx.bot_id);
        if to_her || !self.roll(ctx, msg.author.id, msg.channel_id) {
            return Ok(());
        }

        let asker = Asker {
            user: msg.author.id,
            guild: Some(guild),
            channel: msg.channel_id,
            message: msg.id,
            posted: Default::default(),
        };
        let allowed = self.settings(ctx).allowed(fastrand::f64());
        let mut instructions = format!("{READ_ALONG}{}", allowed_instructions(allowed));
        // When she may react, the server's own emoji are hers to pick from too.
        if allowed == Allowed::React {
            instructions.push_str(&emoji::for_chime_in(ctx, asker.guild).await);
        }
        read_and_react(ctx, &asker, msg, &instructions, allowed).await
    }

    /// Whether to read along now: the dice hit (with the author's own chance, if they have
    /// one) and the channel isn't cooling down. A hit starts the cooldown right away, so two
    /// quick messages can't both start one.
    fn roll(&self, ctx: &BotCtx, author: UserId, channel: ChannelId) -> bool {
        let settings = self.settings(ctx);
        if fastrand::f64() >= settings.chance_for(author, channel) {
            return false;
        }
        let cooldown = Duration::from_secs(settings.chime_cooldown_minutes * 60);
        let mut last = self.last.lock().unwrap();
        if last.get(&channel).is_some_and(|at| at.elapsed() < cooldown) {
            return false;
        }
        last.insert(channel, Instant::now());
        true
    }
}

/// What she's asked after reading along, before what she may do this time
/// ([`allowed_instructions`]). In the question, not the system prompt, so the system prompt
/// stays the same as for answers and shares their cache.
const READ_ALONG: &str = "You're reading along in this channel; nobody asked you anything. \
Reactions after a message show how people took it, your own included. \
When someone answers one of your questions later, save what you learned. \
If you noticed something lasting about the server, its people or yourself, save it with the memory tool. ";

/// What she may do this time, after reading.
fn allowed_instructions(allowed: Allowed) -> &'static str {
    match allowed {
        Allowed::ReadOnly => {
            "This time you only read: nothing you write is posted. When you're done, answer DONE."
        }
        Allowed::React => {
            "This time, react to the last message: answer REACT followed by one emoji that fits it."
        }
    }
}

/// Reads the last messages of `asker.channel` (ending with `last`), asks the model what to
/// do with `instructions`, and maybe reacts to `last`. Nothing else is ever posted: on a
/// read-only roll not even a reaction, and any text she writes is dropped.
async fn read_and_react(
    ctx: &BotCtx,
    asker: &Asker,
    last: &Message,
    instructions: &str,
    allowed: Allowed,
) -> Result<()> {
    let started_at = Utc::now().timestamp();
    let provider = ctx.ai.chat().context("chat has no model")?;
    let transcript = read_along(ctx, asker.guild, asker.channel, last).await?;
    let context = answer::context_for(ctx, asker, true).await;
    // The transcript, then what she's asked. No pictures: they cost more tokens than they
    // were worth here. Message lines name attachments, and `get_message` shows them if she
    // wants to look.
    let mut parts = vec![Part::Text(transcript.clone())];
    let mut stored = vec![StoredPart::Text { text: transcript }];
    let instructions = instructions.to_string();
    parts.push(Part::Text(instructions.clone()));
    stored.push(StoredPart::Text { text: instructions });
    parts.extend(context.iter().cloned().map(Part::Text));
    // Every tool goes in the request, as for an answer, so the request starts the same as
    // an answer's and reads the system prompt and tools from the prompt cache. The runner
    // turns down the ones she can't use here.
    let (tools, owners) = answer::tools_for(ctx, asker);
    let request = ChatRequest {
        system: SYSTEM_PROMPT.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts,
        }]),
        tools,
        cache_key: format!("discord:{}", asker.channel),
        job: "chime",
        answer_now: false,
    };
    let runner = Runner {
        ctx: ctx.clone(),
        asker: asker.clone(),
        owners,
    };
    let done = complete(provider.as_ref(), request, &runner, MAX_ROUNDS).await?;

    if allowed == Allowed::ReadOnly {
        info!("read along without posting");
        return Ok(());
    }
    let Some(picked) = reaction(&done.text) else {
        info!("read along and passed");
        return Ok(());
    };
    // Her `:name:` for a server emoji becomes its code, so Discord shows it.
    let codes = emoji::codes(ctx, asker.guild);
    let emoji = emoji::reaction_code(&picked, &codes);
    react(&ctx.http, last.channel_id, last.id, &emoji).await?;
    info!("reacted with {emoji}");
    // Saved too, so `look_back_at_answers` lists it when someone asks why she reacted.
    stored.extend(context.into_iter().map(|text| StoredPart::Context { text }));
    let written = Written {
        provider: provider.name().to_string(),
        model: provider.model().to_string(),
        continuation_id: done.continuation,
        native_json: serde_json::to_string(&done.rounds).ok(),
    };
    let note = reaction_note(&emoji, display_name(&last.author), &last.content);
    save(ctx, asker.channel, stored, written, started_at, note).await
}

/// What a chime-in reaction is saved as: `(reacted 🐄 to RickyWL: "I Am One Of…")`.
fn reaction_note(emoji: &str, author: &str, text: &str) -> String {
    let text = one_line(text);
    let quote = if text.is_empty() {
        String::new()
    } else {
        format!(": \"{}\"", shorten_at_word(&text, 80))
    };
    format!("(reacted {emoji} to {author}{quote})")
}

/// Saves a chime-in reaction like a question and its answer: what she read, exactly as the
/// model read it, and `said`, a note on her reaction. There's no message of hers to link it to.
async fn save(
    ctx: &BotCtx,
    channel: ChannelId,
    stored: Vec<StoredPart>,
    written: Written,
    started_at: i64,
    said: String,
) -> Result<()> {
    let question = NewTurn {
        parent_id: None,
        role: Role::User,
        // Saved as the bot's own, because nobody asked: nobody can 🔁 it as if it were
        // their answer (🔁 would run it as a normal answer and post "REACT 😂").
        author_id: ctx.bot_id.get(),
        channel_id: channel.get(),
        parts: stored,
        written: None,
        started_at: None,
        error: None,
        created_at: Utc::now().timestamp(),
    };
    let bot_id = ctx.bot_id.get();
    ctx.db
        .call(move |conn| {
            let question_id = store::add_turn(conn, &question, &[])?;
            let answer = NewTurn {
                parent_id: Some(question_id),
                role: Role::Assistant,
                author_id: bot_id,
                channel_id: question.channel_id,
                parts: vec![StoredPart::Text { text: said }],
                written: Some(written),
                started_at: Some(started_at),
                error: None,
                created_at: Utc::now().timestamp(),
            };
            store::add_turn(conn, &answer, &[])
        })
        .await
        .context("saving what she did")?;
    Ok(())
}

/// The last [`READ_MESSAGES`] messages of the channel as a transcript, oldest first, ending
/// with `last`.
async fn read_along(
    ctx: &BotCtx,
    guild: Option<GuildId>,
    channel: ChannelId,
    last: &Message,
) -> Result<String> {
    let mut messages = ctx
        .http
        .channel_messages(channel)
        .before(last.id)
        .limit(READ_MESSAGES - 1)
        .await?
        .models()
        .await?;
    messages.insert(0, last.clone());
    // Discord gives the newest first.
    messages.reverse();

    let name = guild
        .and_then(|guild| ctx.channel_name(guild, channel))
        .unwrap_or_default();
    let mut lines = Vec::new();
    for m in &messages {
        lines.push(format_message(ctx, m, Tz::UTC).await);
    }
    Ok(format!(
        "<recent_messages channel=\"#{name}\">\n{}\n</recent_messages>",
        lines.join("\n")
    ))
}

/// Reads her answer: `REACT 😂` gives the emoji, anything else (`PASS`, a line) is nothing.
fn reaction(text: &str) -> Option<String> {
    let text = text.trim().trim_matches('"').trim();
    let rest = text
        .strip_prefix("REACT")
        .or_else(|| text.strip_prefix("React"))?;
    let emoji = rest.trim_start_matches(':').split_whitespace().next()?;
    Some(emoji.to_string())
}

/// Runs her tool calls with [`UNPROMPTED_TOOLS`], as the author of the last message. What
/// she reads may come from any channel chat may read (Cherry's rule).
///
/// `memory` runs as Vivy herself: what she saves unprompted is her own call, so the change
/// log names her, not whoever wrote last. The folder is the server's either way.
struct Runner {
    ctx: BotCtx,
    asker: Asker,
    owners: HashMap<&'static str, Arc<dyn Feature>>,
}

#[async_trait]
impl ToolRunner for Runner {
    async fn run(&self, call: &ToolCall) -> ToolReply {
        if !UNPROMPTED_TOOLS.contains(&call.name.as_str()) {
            return format!(
                "Error: {} can't be used when nobody asked you anything. Here you can only \
                 read the server and use your memory.",
                call.name
            )
            .into();
        }
        if call.name == "memory" {
            let herself = Asker {
                user: self.ctx.bot_id,
                ..self.asker.clone()
            };
            return answer::run_tool(&self.ctx, &herself, &self.owners, call).await;
        }
        answer::run_tool(&self.ctx, &self.asker, &self.owners, call).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_her_reaction() {
        assert_eq!(reaction("REACT 😂"), Some("😂".into()));
        assert_eq!(reaction("REACT: 🍿"), Some("🍿".into()));
        assert_eq!(reaction("\"React 🐄\""), Some("🐄".into()));
        assert_eq!(reaction("REACT"), None);
        assert_eq!(reaction("PASS"), None);
        assert_eq!(reaction(""), None);
        // A line is never posted, not even one that mentions reacting.
        assert_eq!(reaction("that movie was mid tbh"), None);
        assert_eq!(reaction("I'd REACT 😂 to that"), None);
    }

    #[test]
    fn reactions_are_saved_as_a_note() {
        assert_eq!(
            reaction_note("🐄", "RickyWL", "I Am One Of\nThe People"),
            "(reacted 🐄 to RickyWL: \"I Am One Of The People\")"
        );
        assert_eq!(reaction_note("😂", "Cherry", ""), "(reacted 😂 to Cherry)");
    }

    #[test]
    fn no_tools_that_act_for_someone() {
        for name in [
            "create_reminder",
            "edit_reminder",
            "cancel_reminder",
            "list_reminders",
        ] {
            assert!(!UNPROMPTED_TOOLS.contains(&name), "{name}");
        }
    }

    #[test]
    fn settings_default_to_a_small_chance() {
        let settings: Settings = toml::from_str("enabled = true").unwrap();
        assert_eq!(settings.chime_chance, 0.05);
        assert_eq!(settings.chime_react_chance, 0.40);
        assert_eq!(settings.chime_cooldown_minutes, 30);
        let off: Settings = toml::from_str("chime_chance = 0.0").unwrap();
        assert_eq!(off.chime_chance, 0.0);
    }

    #[test]
    fn second_roll_picks_reaction_or_reading() {
        let settings = Settings::default();
        assert_eq!(settings.allowed(0.0), Allowed::React);
        assert_eq!(settings.allowed(0.39), Allowed::React);
        assert_eq!(settings.allowed(0.40), Allowed::ReadOnly);
        assert_eq!(settings.allowed(0.99), Allowed::ReadOnly);
    }

    #[test]
    fn some_people_get_their_own_chance() {
        let settings: Settings = toml::from_str(
            "chime_chance = 0.05\n\
             chime_chance_per_user = [[123, 0.5], [123, 0.0, 999], [456, 1]]",
        )
        .unwrap();
        let (user, other) = (UserId::new(123), UserId::new(1));
        assert_eq!(settings.chance_for(user, ChannelId::new(7)), 0.5);
        assert_eq!(settings.chance_for(user, ChannelId::new(999)), 0.0);
        assert_eq!(
            settings.chance_for(UserId::new(456), ChannelId::new(7)),
            1.0
        );
        assert_eq!(settings.chance_for(other, ChannelId::new(999)), 0.05);
    }
}
