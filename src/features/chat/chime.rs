//! Chiming in: now and then Vivy reads along in a channel and, if she has something to say,
//! adds one short line or a reaction, without anyone mentioning her.
//!
//! After each message there's a small chance (`chime_chance` in `[features.chat]`) that she
//! reads the last [`READ_MESSAGES`] messages, mostly to learn from them for her memory. A
//! second roll decides what else she does that time: `chime_say_chance` of the reads she
//! posts a line, `chime_react_chance` she reacts to the last message, and the rest she only
//! reads. A channel then waits `chime_cooldown_minutes` before she reads it again. Her line
//! is saved like an answer, so replying to it continues the conversation.
//!
//! Nobody asked for these lines (nor for check-ins, which use [`speak`] too), and they're
//! posted for the whole channel, so she can only use [`UNPROMPTED_TOOLS`]: tools that read
//! what people in this channel could see anyway, and her own memory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::Utc;
use chrono_tz::Tz;
use serde::Deserialize;
use serde_json::json;
use serenity::all::{
    ChannelId, CreateAllowedMentions, CreateMessage, GetMessages, GuildId, Message, ReactionType,
    UserId,
};
use tracing::{info, warn};

use super::answer::{self, SYSTEM_PROMPT};
use super::store::{self, NewTurn, StoredPart, Written};
use super::tools::{format_message, parse_link};
use crate::ai::{ChatRequest, Input, Part, Role, ToolCall, ToolRunner, Turn, complete};
use crate::core::{Asker, BotCtx, Feature, Result};
use crate::util::shorten_at_word;

/// How many messages she reads before deciding.
const READ_MESSAGES: u8 = 15;
/// Rounds of tool calls (saving to memory) before she has to decide.
const MAX_ROUNDS: usize = 4;
/// The longest line she posts.
const MAX_LINE: usize = 400;

/// The tools she may use when nobody asked. An allow-list, so a new tool stays out until
/// someone decides it's safe here. Left out: anything that acts for a person (reminders,
/// check-ins), and `list_channels`, which lists what one person can see, not everyone here.
/// `get_message` and `search_messages` only reach this channel (see [`Runner`]). `memory`
/// is in: what she saves is her own decision. `tinyfish_search` and `tinyfish_fetch` only
/// read the web, like the providers' own web tools, which chime-ins have too.
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

/// `[features.chat]` settings: chiming in, and some limits on answers.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The most rounds of tool calls in one answer, so a confused model can't loop forever.
    pub max_tool_rounds: usize,
    /// How far up a reply chain the history goes, in messages.
    pub history_turns: usize,
    /// The most check-ins (`schedule_follow_up`) planned with one person at a time.
    pub follow_ups_per_person: i64,
    /// The chance, from 0 to 1, that a message makes her read along. 0 turns it off.
    pub chime_chance: f64,
    /// Of those reads, the share where she posts a line (0 to 1).
    pub chime_say_chance: f64,
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
        if dice < self.chime_say_chance {
            Allowed::Anything
        } else if dice < self.chime_say_chance + self.chime_react_chance {
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
            follow_ups_per_person: 5,
            chime_chance: 0.05,
            chime_say_chance: 0.10,
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
    /// React to the last message with one emoji.
    React,
    /// Post a line (or react, or pass).
    Anything,
}

/// What she does after reading along.
#[derive(Debug, PartialEq)]
enum Decision {
    Pass,
    React(String),
    /// A line to post, maybe as a reply to one of the messages she read (its number).
    Say {
        line: String,
        reply_to: Option<usize>,
    },
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
    /// whether she may also post a line or react.
    pub async fn on_message(&self, ctx: &BotCtx, msg: &Message) -> Result<()> {
        let Some(guild) = msg.guild_id.filter(|_| ctx.ai.chat().is_some()) else {
            return Ok(());
        };
        // Messages to her get a real answer instead.
        let to_her = msg.mentions_user_id(ctx.bot_id)
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
        let instructions = format!("{READ_ALONG}{}", allowed_instructions(allowed));
        speak(ctx, &asker, Some(msg), &instructions, None, allowed).await
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
Reactions after a message show how people took it, your own lines included. \
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
        Allowed::Anything => {
            "This time, chime in with something that fits: a joke, a reaction, a fact, an opinion, or a short question about what they're talking about when you don't know it and are curious (what a name or in-joke means, how something turned out). \
Answer with one short line the way people write on Discord, with no greeting. If no line fits, answer REACT followed by one emoji, or PASS. \
When your line answers one message in particular, start it with REPLY and that message's number (like REPLY 7 then your line), so it's posted as a reply to it."
        }
    }
}

/// Reads the last messages of `asker.channel` (ending with `last`, when given), asks the
/// model what to do with `instructions`, and does it: posts a line, reacts to `last`, or
/// nothing. `allowed` limits that: a line when she may only react is dropped, and when she
/// may only read nothing is posted. A posted line is saved like an answer, so a reply to it continues the
/// conversation. `mention` is the one person the line may ping.
pub async fn speak(
    ctx: &BotCtx,
    asker: &Asker,
    last: Option<&Message>,
    instructions: &str,
    mention: Option<UserId>,
    allowed: Allowed,
) -> Result<()> {
    let started_at = Utc::now().timestamp();
    let provider = ctx.ai.chat().context("chat has no model")?;
    let (transcript, messages) = read_along(ctx, asker.guild, asker.channel, last).await?;
    let question = format!("{transcript}\n{instructions}");
    let context = answer::context_for(ctx, asker, true).await;
    let mut parts = vec![Part::Text(question.clone())];
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
    let (line, reply_to) = match (decide(&done.text), last) {
        (Decision::Say { line, reply_to }, _) if allowed == Allowed::Anything => (line, reply_to),
        (Decision::React(emoji), Some(msg)) => {
            let reaction = ReactionType::try_from(emoji.as_str())
                .with_context(|| format!("{emoji:?} isn't an emoji"))?;
            msg.react(&ctx.http, reaction).await?;
            info!("reacted with {emoji}");
            return Ok(());
        }
        _ => {
            info!("read along and passed");
            return Ok(());
        }
    };
    // The message she answers, if she named one: numbers start at 1, oldest first.
    let target = reply_to
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| messages.get(i));
    let pings = match mention {
        Some(user) => CreateAllowedMentions::new().users([user]),
        None => CreateAllowedMentions::new(),
    };
    // A reply, but without pinging its author: Discord pings unless told not to.
    let mut message = CreateMessage::new()
        .content(answer::discord_links(&line))
        .allowed_mentions(pings.replied_user(false));
    if let Some(target) = target {
        message = message.reference_message(target);
    }
    let sent = asker.channel.send_message(&ctx.http, message).await?;
    let ids = vec![sent.id.get()];
    match target {
        Some(target) => info!("spoke up on her own, replying to {}", target.id),
        None => info!("spoke up on her own"),
    }
    // Saved like a question and its answer, exactly as the model read it, so a reply to
    // her line continues the same conversation.
    let mut stored = vec![StoredPart::Text { text: question }];
    stored.extend(context.into_iter().map(|text| StoredPart::Context { text }));
    let question = NewTurn {
        parent_id: None,
        role: Role::User,
        // Saved as the bot's own, because nobody asked: nobody can 🔁 or ❌ her line as
        // if it were their answer (🔁 would run it as a normal answer and post "PASS").
        author_id: ctx.bot_id.get(),
        channel_id: asker.channel.get(),
        parts: stored,
        written: None,
        started_at: None,
        error: None,
        created_at: Utc::now().timestamp(),
    };
    let written = Written {
        provider: provider.name().to_string(),
        model: provider.model().to_string(),
        continuation_id: done.continuation,
        native_json: serde_json::to_string(&done.rounds).ok(),
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
                parts: vec![StoredPart::Text { text: line }],
                written: Some(written),
                started_at: Some(started_at),
                error: None,
                created_at: Utc::now().timestamp(),
            };
            store::add_turn(conn, &answer, &ids)
        })
        .await
        .context("saving what she said")?;
    Ok(())
}

/// The last [`READ_MESSAGES`] messages of the channel, oldest first, ending with `last`
/// when given (otherwise with the newest): the numbered transcript she reads, and the
/// messages themselves, so `REPLY 3` can find the third one.
async fn read_along(
    ctx: &BotCtx,
    guild: Option<GuildId>,
    channel: ChannelId,
    last: Option<&Message>,
) -> Result<(String, Vec<Message>)> {
    let mut messages = match last {
        Some(msg) => {
            let mut before = channel
                .messages(
                    &ctx.http,
                    GetMessages::new().before(msg.id).limit(READ_MESSAGES - 1),
                )
                .await?;
            before.insert(0, msg.clone());
            before
        }
        None => {
            channel
                .messages(&ctx.http, GetMessages::new().limit(READ_MESSAGES))
                .await?
        }
    };
    // Discord gives the newest first.
    messages.reverse();
    let name = guild
        .and_then(|guild| ctx.cache.guild(guild))
        .and_then(|guild| {
            guild
                .channels
                .get(&channel)
                .or_else(|| guild.threads.iter().find(|t| t.id == channel))
                .map(|c| c.name.clone())
        })
        .unwrap_or_default();
    let mut lines = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        lines.push(format!(
            "{}. {}",
            i + 1,
            format_message(ctx, m, Tz::UTC).await
        ));
    }
    let transcript = format!(
        "<recent_messages channel=\"#{name}\">\n{}\n</recent_messages>",
        lines.join("\n")
    );
    Ok((transcript, messages))
}

/// Reads her answer: PASS, REACT 😂, or the line to post (maybe after REPLY 7).
fn decide(text: &str) -> Decision {
    let text = unquote(text);
    let word = text
        .trim_end_matches(|c: char| c.is_ascii_punctuation())
        .to_ascii_uppercase();
    let starts_with_pass = text
        .strip_prefix("PASS")
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()));
    if text.is_empty() || word == "PASS" || starts_with_pass {
        return Decision::Pass;
    }
    if let Some(rest) = text
        .strip_prefix("REACT")
        .or_else(|| text.strip_prefix("React"))
    {
        let emoji = rest.trim_start_matches(':').trim();
        return match emoji.split_whitespace().next() {
            Some(emoji) => Decision::React(emoji.to_string()),
            None => Decision::Pass,
        };
    }
    let (reply_to, line) = reply_number(text);
    let line = unquote(line);
    if line.is_empty() {
        return Decision::Pass;
    }
    Decision::Say {
        line: shorten_at_word(line, MAX_LINE),
        reply_to,
    }
}

/// Takes off quotes around the whole answer, but only when they wrap all of it: a line
/// that ends with a quote of its own (`the move is "stairs"`) keeps both quotes.
fn unquote(text: &str) -> &str {
    let text = text.trim();
    match text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        Some(inner) if !inner.contains('"') => inner.trim(),
        _ => text,
    }
}

/// Splits `REPLY 7 the line` into the message number and the line. Without a number, the
/// whole text is the line.
fn reply_number(text: &str) -> (Option<usize>, &str) {
    let Some(rest) = text.strip_prefix("REPLY") else {
        return (None, text);
    };
    let rest = rest.trim_start_matches([':', ' ', '#']);
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    match rest[..digits].parse() {
        Ok(number) => (Some(number), rest[digits..].trim_start_matches([':', ' '])),
        Err(_) => (None, text),
    }
}

/// Runs her tool calls with [`UNPROMPTED_TOOLS`], as the author of the last message (or the
/// person she checks in with), but kept to this channel: what she reads ends up in a public
/// line, so it mustn't come from a channel only that person can read.
///
/// In a server, `memory` runs as Vivy herself: what she saves unprompted is her own call, so
/// the change log names her, not whoever wrote last. The folder is the server's either way.
/// In a DM (a follow-up) the folder is the person's, so it stays theirs.
struct Runner {
    ctx: BotCtx,
    asker: Asker,
    owners: HashMap<&'static str, Arc<dyn Feature>>,
}

#[async_trait]
impl ToolRunner for Runner {
    async fn run(&self, call: &ToolCall) -> String {
        if !UNPROMPTED_TOOLS.contains(&call.name.as_str()) {
            return format!(
                "Error: {} can't be used when nobody asked you anything. Here you can only \
                 read this channel and use your memory.",
                call.name
            );
        }
        match here_only(call, self.asker.channel) {
            Ok(call) if call.name == "memory" && self.asker.guild.is_some() => {
                let herself = Asker {
                    user: self.ctx.bot_id,
                    ..self.asker.clone()
                };
                answer::run_tool(&self.ctx, &herself, &self.owners, &call)
                    .await
                    .text
            }
            Ok(call) => {
                answer::run_tool(&self.ctx, &self.asker, &self.owners, &call)
                    .await
                    .text
            }
            Err(text) => text,
        }
    }
}

/// Keeps a tool call to `channel`: a search only searches it, and a message link must point
/// into it. Returns the call to run, or the error to give the model instead.
fn here_only(call: &ToolCall, channel: ChannelId) -> Result<ToolCall, String> {
    let mut call = call.clone();
    match call.name.as_str() {
        "search_messages" => match call.args.as_object_mut() {
            Some(args) => {
                args.insert("channel".to_string(), json!("here"));
            }
            None => call.args = json!({"channel": "here"}),
        },
        "get_message" => {
            let link = call.args["link"].as_str().unwrap_or_default();
            // A link that doesn't parse is left to the tool, which explains the mistake.
            if let Some((_, linked, _)) = parse_link(link)
                && linked != channel.get()
            {
                return Err("Error: right now you can only read messages in this channel.".into());
            }
        }
        _ => {}
    }
    Ok(call)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_her_decision() {
        assert_eq!(decide("PASS"), Decision::Pass);
        assert_eq!(decide(" pass. "), Decision::Pass);
        assert_eq!(decide(""), Decision::Pass);
        assert_eq!(decide("PASS - nothing to add"), Decision::Pass);
        assert_eq!(decide("PASSWORD reset day"), say("PASSWORD reset day"));
        assert_eq!(decide("REACT 😂"), Decision::React("😂".into()));
        assert_eq!(decide("REACT: 🍿"), Decision::React("🍿".into()));
        assert_eq!(decide("REACT"), Decision::Pass);
        assert_eq!(
            decide("\"that movie was mid tbh\""),
            say("that movie was mid tbh")
        );
        // A quote at the end of the line is part of it.
        assert_eq!(
            decide("and the move is \"so what's your relationship with stairs\""),
            say("and the move is \"so what's your relationship with stairs\"")
        );
        assert_eq!(
            decide("\"stairs\" is the move, not \"elevators\""),
            say("\"stairs\" is the move, not \"elevators\"")
        );
        let Decision::Say { line: long, .. } = decide(&"a".repeat(1000)) else {
            panic!("not a line");
        };
        assert!(long.chars().count() <= MAX_LINE);
    }

    fn say(line: &str) -> Decision {
        Decision::Say {
            line: line.into(),
            reply_to: None,
        }
    }

    #[test]
    fn reads_which_message_she_answers() {
        let reply = |n, line: &str| Decision::Say {
            line: line.into(),
            reply_to: Some(n),
        };
        assert_eq!(decide("REPLY 7 netflix has it"), reply(7, "netflix has it"));
        assert_eq!(decide("REPLY 12: same"), reply(12, "same"));
        assert_eq!(decide("REPLY #3 \"lol\""), reply(3, "lol"));
        assert_eq!(decide("REPLY 3"), Decision::Pass);
        assert_eq!(decide("REPLY all of you"), say("REPLY all of you"));
    }

    #[test]
    fn tools_stay_in_this_channel() {
        let here = ChannelId::new(10);
        let call = |name: &str, args| ToolCall {
            id: "1".into(),
            name: name.into(),
            args,
        };
        let search = here_only(&call("search_messages", json!({"channel": "secret"})), here);
        assert_eq!(search.unwrap().args["channel"], "here");
        let search = here_only(&call("search_messages", json!(null)), here);
        assert_eq!(search.unwrap().args["channel"], "here");

        let link = |channel| json!({"link": format!("https://discord.com/channels/1/{channel}/5")});
        assert!(here_only(&call("get_message", link(10)), here).is_ok());
        assert!(here_only(&call("get_message", link(11)), here).is_err());
        assert!(here_only(&call("get_message", json!({"link": "nonsense"})), here).is_ok());
    }

    #[test]
    fn no_tools_that_act_for_someone() {
        for name in [
            "create_reminder",
            "cancel_reminder",
            "list_reminders",
            "schedule_follow_up",
        ] {
            assert!(!UNPROMPTED_TOOLS.contains(&name), "{name}");
        }
    }

    #[test]
    fn settings_default_to_a_small_chance() {
        let settings: Settings = toml::from_str("enabled = true").unwrap();
        assert_eq!(settings.chime_chance, 0.05);
        assert_eq!(settings.chime_say_chance, 0.10);
        assert_eq!(settings.chime_react_chance, 0.40);
        assert_eq!(settings.chime_cooldown_minutes, 30);
        let off: Settings = toml::from_str("chime_chance = 0.0").unwrap();
        assert_eq!(off.chime_chance, 0.0);
    }

    #[test]
    fn second_roll_picks_line_reaction_or_reading() {
        let settings = Settings::default();
        assert_eq!(settings.allowed(0.0), Allowed::Anything);
        assert_eq!(settings.allowed(0.09), Allowed::Anything);
        assert_eq!(settings.allowed(0.10), Allowed::React);
        assert_eq!(settings.allowed(0.49), Allowed::React);
        assert_eq!(settings.allowed(0.50), Allowed::ReadOnly);
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
