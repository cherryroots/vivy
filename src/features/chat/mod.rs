//! Chat: "@Vivy what's in this picture?" gets an answer from the AI model, streamed into a
//! reply. Replying to an answer continues the conversation.
//!
//! - `answer.rs`: one answer: streaming, the status line, tool calls, files
//! - `apps.rs`: private helpers in the Apps menu: Catch me up (and `/catchup`), Explain this,
//!   Translate this
//! - `channels.rs`: names for channel mentions and links in messages, threads included
//! - `history.rs`: Discord messages to chat turns, and stored turns to model input
//! - `store.rs`: the `chat_turns` and `chat_messages` tables
//! - `tools.rs`: chat's own tools (time, channel, users, messages, pins, events)
//! - `search.rs`: the `search_messages` tool
//! - `recent.rs`: the `read_recent_messages` tool, a stretch of a channel's messages
//! - `look_back.rs`: the `look_back_at_answers` tool, her own earlier answers step by step
//! - `chime.rs`: reading along now and then without being mentioned, and maybe reacting
//! - `emoji.rs`: the server's custom emoji, described once from their pictures
//! - `poll.rs`: the `create_poll` tool, a Discord poll when someone asks for one
//! - `react.rs`: the `react_to_message` tool, her own reactions while she answers
//! - `prompt.md`: the system prompt
//!
//! Reactions on an answer, from the person who asked: ❌ stops it while it's being written
//! and deletes it once it's done, 🔁 writes it again.

mod answer;
mod apps;
mod avatar;
mod blocklist;
mod channels;
pub(super) mod chime;
mod emoji;
mod history;
mod look_back;
mod open_files;
mod poll;
mod react;
mod recent;
mod search;
mod store;
mod tools;

use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use twilight_model::channel::message::{EmojiReactionType, MessageFlags};

use crate::util::discord::{
    ChannelId, GatewayReaction, Message, MessageId, UserId, get_message, remove_reaction,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use self::answer::{End, Job};
use self::store::{NewTurn, StoredPart};
use crate::ai::{ChatProvider, Input, Part, Role, ToolDef, ToolReply};
use crate::core::{Asker, BotCtx, Command, Feature, Mention, Result, Stat, user_error};
use crate::util::media;
use crate::util::reply::LiveReply;

const STOP: &str = "❌";
const REGENERATE: &str = "🔁";

#[derive(Default)]
pub struct Chat {
    /// Answers being written right now, so ❌ can stop them.
    running: Mutex<Vec<Running>>,
    chime: chime::Chime,
}

struct Running {
    requester: UserId,
    /// The question being answered, once known. Keeps concurrent 🔁 runs from overlapping
    /// even when a voice-only answer has no text messages left to reuse.
    question_id: Option<i64>,
    cancel: CancellationToken,
    /// The answer's Discord messages, filled in as they're sent.
    message_ids: Arc<Mutex<Vec<MessageId>>>,
}

#[async_trait]
impl Feature for Chat {
    fn name(&self) -> &'static str {
        "chat"
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            apps::catch_me_up(),
            apps::explain_this(),
            apps::translate_this(),
            apps::catchup(),
        ]
    }

    fn migrations(&self) -> &'static [&'static str] {
        store::MIGRATIONS
    }

    /// The empty prefix: every mention no other feature claimed.
    fn mention_prefixes(&self) -> &'static [&'static str] {
        &[""]
    }

    async fn stats(&self, ctx: &BotCtx) -> Result<Vec<Stat>> {
        let answers = ctx.db.call(|conn| Ok(store::answers(conn)?)).await?;
        Ok(vec![Stat::new("Answers", answers)])
    }

    fn tools(&self) -> Vec<ToolDef> {
        let mut defs = tools::defs();
        defs.push(avatar::def());
        defs.push(emoji::def());
        defs.push(emoji::describe_def());
        defs.push(look_back::def());
        defs.push(react::def());
        defs.push(poll::def());
        defs.push(blocklist::def());
        defs
    }

    fn tool_status(&self, tool: &str, _args: &Value) -> Option<&'static str> {
        Some(match tool {
            "get_current_time" => "🕒 Checking the time…",
            "get_channel_info" | "list_channels" => "🧭 Looking around the server…",
            "get_user_info" => "👤 Looking someone up…",
            "get_bot_owner" => "👑 Checking who my owner is…",
            avatar::TOOL => "🖼️ Looking at a profile picture…",
            "read_recent_messages" => "📜 Reading the chat…",
            "get_message" => "🔗 Opening a message…",
            "search_messages" => "🔍 Searching messages…",
            "get_pinned_messages" => "📌 Reading the pins…",
            "list_server_events" => "📅 Checking the events…",
            "list_server_emoji" => "😀 Looking at the emoji…",
            emoji::DESCRIBE_TOOL => "😀 Checking an emoji…",
            look_back::TOOL => "🪞 Looking back at what I did…",
            react::TOOL => "✨ Reacting…",
            poll::TOOL => "📊 Making a poll…",
            blocklist::TOOL => "🚫 Checking my blocklist…",
            _ => return None,
        })
    }

    async fn run_tool(
        &self,
        ctx: &BotCtx,
        asker: &Asker,
        name: &str,
        args: &Value,
    ) -> Result<String> {
        match name {
            "list_server_emoji" => emoji::list(ctx, asker).await,
            emoji::DESCRIBE_TOOL => emoji::describe_one(ctx, asker, args).await,
            look_back::TOOL => look_back::run(ctx, asker, args).await,
            react::TOOL => react::run(ctx, asker, args).await,
            poll::TOOL => poll::run(ctx, asker, args).await,
            blocklist::TOOL => blocklist::run(ctx, asker, args).await,
            _ => tools::run(ctx, asker, name, args).await,
        }
    }

    async fn run_tool_with_pictures(
        &self,
        ctx: &BotCtx,
        asker: &Asker,
        name: &str,
        args: &Value,
    ) -> Result<ToolReply> {
        match name {
            avatar::TOOL => avatar::run(ctx, asker, args).await,
            "get_message" => tools::message_with_pictures(ctx, asker, args).await,
            _ => Ok(self.run_tool(ctx, asker, name, args).await?.into()),
        }
    }

    async fn start(&self, ctx: &BotCtx) -> Result<()> {
        // Read the chime-in settings now, so typos in them are warned about at startup.
        self.chime.settings(ctx);
        emoji::spawn(ctx);
        Ok(())
    }

    async fn on_mention(&self, ctx: &BotCtx, msg: &Message, rest: &str) -> Result<Mention> {
        // People who lost chat on her blocklist get a curt answer from the cheap model.
        if blocklist::blocks(ctx, msg.author.id, "chat").await {
            blocklist::answer_curtly(ctx, msg).await?;
            return Ok(Mention::Handled);
        }
        let provider = provider(ctx)?;
        // The reply starts now, as "💭 Thinking…", before the slow part: waiting for link
        // previews, downloading pictures, reading video frames. Otherwise a question with a
        // few phone photos shows nothing for many seconds.
        let mut reply = LiveReply::new(msg.channel_id, Some(msg.id));
        if let Err(err) = reply.show(&ctx.http, &answer::placeholder()).await {
            warn!("couldn't start the reply: {err:#}");
        }
        // Register right away so ❌ on "💭 Thinking…" cancels before answering starts.
        let Some(registered) = self.try_register(msg.author.id, None, reply.message_ids()) else {
            return Ok(Mention::Handled);
        };

        let setup = async {
            let msg = media::with_previews(&ctx.http, msg).await;
            if registered.cancel.is_cancelled() {
                return Ok(None);
            }
            // The message this one replies to, if any, is the conversation so far.
            let parent_id = match &msg.referenced_message {
                Some(referenced) => {
                    Some(history::turn_for_reference(ctx, referenced, msg.guild_id).await?)
                }
                None => None,
            };
            let asker = Asker {
                user: msg.author.id,
                guild: msg.guild_id,
                channel: msg.channel_id,
                message: msg.id,
                posted: Default::default(),
            };
            let owner = history::is_owner_command(ctx, &msg).await;
            let mut parts = history::read_message(ctx, &msg, rest, false, owner).await;
            // Messages the question links to come with it, so she reads them without asking.
            parts.extend(history::linked_messages(ctx, &asker, rest).await);
            if registered.cancel.is_cancelled() {
                return Ok(None);
            }
            let question = NewTurn {
                parent_id,
                role: Role::User,
                author_id: msg.author.id.get(),
                channel_id: msg.channel_id.get(),
                parts,
                written: None,
                started_at: None,
                error: None,
                created_at: Utc::now().timestamp(),
            };
            let message_id = msg.id.get();
            let question_id = ctx
                .db
                .call(move |conn| store::add_turn(conn, &question, &[message_id]))
                .await
                .context("saving the question")?;
            Ok::<_, anyhow::Error>(Some((asker, question_id)))
        }
        .await;

        let (asker, question_id) = match setup {
            Ok(Some(ready)) => ready,
            Ok(None) => {
                cleanup_reply(ctx, msg.channel_id, &reply.message_ids()).await;
                info!("answer stopped with ❌");
                return Ok(Mention::Handled);
            }
            Err(err) => {
                cleanup_reply(ctx, msg.channel_id, &reply.message_ids()).await;
                return Err(err);
            }
        };
        registered.set_question_id(question_id);

        self.answer(
            ctx,
            provider.as_ref(),
            asker,
            question_id,
            reply,
            registered,
            true,
        )
        .await?;
        Ok(Mention::Handled)
    }

    /// Every message might make her chime in.
    async fn on_message(&self, ctx: &BotCtx, msg: &Message) -> Result<()> {
        self.chime.on_message(ctx, msg).await
    }

    async fn on_reaction_add(&self, ctx: &BotCtx, reaction: &GatewayReaction) -> Result<()> {
        let EmojiReactionType::Unicode { name: emoji } = &reaction.emoji else {
            return Ok(());
        };
        let user = reaction.user_id;
        // Discord says whose message it is; skip the lookups for everyone else's.
        if reaction
            .message_author_id
            .is_some_and(|author| author != ctx.bot_id)
        {
            return Ok(());
        }
        match emoji.as_str() {
            STOP => {
                if self.is_running(reaction.message_id) {
                    if self.stop(reaction.message_id, user) {
                        // Taken away again, like 🔁: the status says a second ❌ deletes
                        // the answer, and that one should be a fresh press.
                        let _ = remove_reaction(&ctx.http, reaction).await;
                    }
                    Ok(())
                } else {
                    self.delete(ctx, reaction, user).await
                }
            }
            REGENERATE => self.regenerate(ctx, reaction, user).await,
            _ => Ok(()),
        }
    }
}

fn provider(ctx: &BotCtx) -> Result<Arc<dyn ChatProvider>> {
    ctx.ai
        .chat()
        .ok_or_else(|| user_error("Chat is turned off: the bot has no AI key."))
}

impl Chat {
    /// Registers an in-flight answer in `self.running` if no overlapping answer is already
    /// running for the same question or any of `initial_ids`.
    fn try_register(
        &self,
        requester: UserId,
        question_id: Option<i64>,
        initial_ids: Vec<MessageId>,
    ) -> Option<Registered<'_>> {
        let cancel = CancellationToken::new();
        let message_ids = Arc::new(Mutex::new(initial_ids.clone()));
        let mut running = self.running.lock().unwrap();
        let busy = running.iter().any(|r| {
            let same_question = question_id.is_some() && r.question_id == question_id;
            let ids = r.message_ids.lock().unwrap();
            same_question || initial_ids.iter().any(|id| ids.contains(id))
        });
        if busy {
            return None;
        }
        running.push(Running {
            requester,
            question_id,
            cancel: cancel.clone(),
            message_ids: message_ids.clone(),
        });
        Some(Registered {
            running: &self.running,
            cancel,
            message_ids,
        })
    }

    /// Answers the question turn `question_id` into `reply`, and saves the answer.
    #[allow(clippy::too_many_arguments)]
    async fn answer(
        &self,
        ctx: &BotCtx,
        provider: &dyn ChatProvider,
        asker: Asker,
        question_id: i64,
        reply: LiveReply,
        registered: Registered<'_>,
        cleanup_placeholder_on_error: bool,
    ) -> Result<()> {
        let cancel = registered.cancel.clone();
        let message_ids = registered.message_ids.clone();
        let _registered = registered;

        let prepare = async {
            let turns = chime::settings(ctx).history_turns.max(1);
            let chain = ctx
                .db
                .call(move |conn| store::chain(conn, question_id, turns))
                .await?;
            let voice_available = answer::has_voice_tool(ctx, &asker);
            let mut input = history::build_input(ctx, provider, &chain, voice_available).await;
            // The features' context is saved with the question the first time it's answered,
            // so a later request (🔁, or the rest of the conversation) sends the question
            // exactly as before.
            let has_context = chain.last().is_some_and(|question| {
                question
                    .parts
                    .iter()
                    .any(|p| matches!(p, StoredPart::Context { .. }))
            });
            if !has_context {
                let fresh = !chain.iter().any(|turn| turn.role == Role::Assistant);
                let texts = answer::context_for(ctx, &asker, fresh).await;
                let parts: Vec<StoredPart> = texts
                    .iter()
                    .map(|text| StoredPart::Context { text: text.clone() })
                    .collect();
                ctx.db
                    .call(move |conn| store::add_parts(conn, question_id, &parts))
                    .await
                    .context("saving the question's context")?;
                add_context(&mut input, &texts);
            }
            Ok::<_, anyhow::Error>(input)
        }
        .await;

        let input = match prepare {
            Ok(input) => input,
            Err(err) => {
                if cleanup_placeholder_on_error {
                    cleanup_reply(ctx, asker.channel, &reply.message_ids()).await;
                }
                return Err(err);
            }
        };

        let started_at = Utc::now().timestamp();
        let job = Job {
            asker: asker.clone(),
            reply,
            input,
            cancel,
            message_ids: message_ids.clone(),
        };
        let outcome = answer::run(ctx, provider, job).await;

        match &outcome.end {
            End::Finished => info!("answered"),
            End::Stopped => info!("answer stopped with ❌"),
            // The error is already shown in the reply's status line, so only log it.
            End::Failed(err) => error!("answering failed: {err:#}"),
        }
        if outcome.message_ids.is_empty() {
            return Ok(());
        }
        // Kept so she can look back at what went wrong (`look_back_at_answers`).
        let error = match &outcome.end {
            End::Finished => None,
            End::Stopped => Some("stopped with ❌".to_string()),
            End::Failed(err) => Some(format!("{err:#}")),
        };
        let answer_turn = NewTurn {
            parent_id: Some(question_id),
            role: Role::Assistant,
            author_id: ctx.bot_id.get(),
            channel_id: asker.channel.get(),
            parts: vec![StoredPart::Text { text: outcome.text }],
            written: outcome.written,
            started_at: Some(started_at),
            error,
            created_at: Utc::now().timestamp(),
        };
        let ids: Vec<u64> = outcome.message_ids.iter().map(|id| id.get()).collect();
        ctx.db
            .call(move |conn| store::add_turn(conn, &answer_turn, &ids))
            .await
            .context("saving the answer")?;
        Ok(())
    }

    /// ❌: stops the answer that `message` belongs to, if `user` asked for it. Returns
    /// whether there was one to stop.
    fn stop(&self, message: MessageId, user: UserId) -> bool {
        let mut stopped = false;
        for running in self.running.lock().unwrap().iter() {
            if running.requester == user && running.message_ids.lock().unwrap().contains(&message) {
                running.cancel.cancel();
                stopped = true;
            }
        }
        stopped
    }

    fn is_running(&self, message: MessageId) -> bool {
        self.running
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.message_ids.lock().unwrap().contains(&message))
    }

    /// 🔁: writes the answer again, in the same messages, if `user` asked the question.
    async fn regenerate(
        &self,
        ctx: &BotCtx,
        reaction: &GatewayReaction,
        user: UserId,
    ) -> Result<()> {
        if self.is_running(reaction.message_id) || blocklist::blocks(ctx, user, "chat").await {
            return Ok(());
        }
        let Some(found) = find_answer(ctx, reaction.message_id, user).await? else {
            return Ok(());
        };
        let provider = provider(ctx)?;
        // Register before removing reactions or inspecting/deleting voice messages, so a
        // second 🔁 or an ❌ in the meantime sees this regeneration as running.
        let Some(registered) =
            self.try_register(user, Some(found.question_id), found.answer_messages.clone())
        else {
            return Ok(());
        };
        // Take the reaction away again, so it can be used for the next try.
        let _ = remove_reaction(&ctx.http, reaction).await;

        let question_message = found.question_messages.first().copied();
        let asker = Asker {
            user,
            guild: reaction.guild_id,
            channel: reaction.channel_id,
            message: question_message.unwrap_or(reaction.message_id),
            posted: Default::default(),
        };
        // Voice messages can't be edited into text: they go, and the new answer may send
        // new ones.
        let mut text_messages = Vec::new();
        for id in found.answer_messages {
            let voice = match get_message(&ctx.http, reaction.channel_id, id).await {
                Ok(message) => message
                    .flags
                    .is_some_and(|flags| flags.contains(MessageFlags::IS_VOICE_MESSAGE)),
                // Already deleted: nothing to reuse.
                Err(_) => continue,
            };
            if voice {
                let _ = ctx.http.delete_message(reaction.channel_id, id).await;
            } else {
                text_messages.push(id);
            }
        }
        *registered.message_ids.lock().unwrap() = text_messages.clone();
        if registered.cancel.is_cancelled() {
            info!("answer stopped with ❌");
            return Ok(());
        }
        let reply = LiveReply::resume(reaction.channel_id, question_message, &text_messages);
        info!("writing an answer again for 🔁");
        self.answer(
            ctx,
            provider.as_ref(),
            asker,
            found.question_id,
            reply,
            registered,
            false,
        )
        .await
    }

    /// ❌ on a finished answer: deletes its messages, if `user` asked the question.
    async fn delete(&self, ctx: &BotCtx, reaction: &GatewayReaction, user: UserId) -> Result<()> {
        let Some(found) = find_answer(ctx, reaction.message_id, user).await? else {
            return Ok(());
        };
        for message in found.answer_messages {
            // A part someone already deleted is fine.
            if let Err(err) = ctx.http.delete_message(reaction.channel_id, message).await {
                warn!("couldn't delete an answer message: {err}");
            }
        }
        info!("answer deleted with ❌");
        Ok(())
    }
}

/// Removes a running answer from the list when dropped.
struct Registered<'a> {
    running: &'a Mutex<Vec<Running>>,
    cancel: CancellationToken,
    message_ids: Arc<Mutex<Vec<MessageId>>>,
}

impl Registered<'_> {
    fn set_question_id(&self, question_id: i64) {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = running
            .iter_mut()
            .find(|r| Arc::ptr_eq(&r.message_ids, &self.message_ids))
        {
            entry.question_id = Some(question_id);
        }
    }
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        // A poisoned lock only means another task panicked; the list is still usable.
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|r| !Arc::ptr_eq(&r.message_ids, &self.message_ids));
    }
}

/// Deletes the "💭 Thinking…" placeholder when pre-answer setup fails or is cancelled.
async fn cleanup_reply(ctx: &BotCtx, channel: ChannelId, ids: &[MessageId]) {
    for &id in ids {
        if let Err(err) = ctx.http.delete_message(channel, id).await {
            warn!("couldn't delete placeholder reply: {err}");
        }
    }
}

/// Puts the features' context in front of the question, the last turn of the input, the
/// same way [`history`] sends it again later.
fn add_context(input: &mut Input, texts: &[String]) {
    let turns = match input {
        Input::Full(turns) => turns,
        Input::After { new, .. } => new,
    };
    if let (Some(question), Some(block)) = (turns.last_mut(), history::context_block(texts)) {
        question.parts.insert(0, Part::Text(block));
    }
}

/// An answer and the question it answers, found from one of the answer's messages.
struct FoundAnswer {
    question_id: i64,
    question_messages: Vec<MessageId>,
    answer_messages: Vec<MessageId>,
}

/// The answer that `message` is part of, if it is an answer and `user` asked the question.
async fn find_answer(
    ctx: &BotCtx,
    message: MessageId,
    user: UserId,
) -> Result<Option<FoundAnswer>> {
    let message = message.get();
    let found = ctx
        .db
        .call(move |conn| {
            let Some(answer) = store::turn_for_message(conn, message)? else {
                return Ok(None);
            };
            let Some(question_id) = answer.parent_id.filter(|_| answer.role == Role::Assistant)
            else {
                return Ok(None);
            };
            let Some(question) = store::get_turn(conn, question_id)? else {
                return Ok(None);
            };
            if question.author_id != user.get() {
                return Ok(None);
            }
            let ids = |turn| -> anyhow::Result<Vec<MessageId>> {
                Ok(store::messages_of(conn, turn)?
                    .into_iter()
                    .map(MessageId::new)
                    .collect())
            };
            Ok(Some(FoundAnswer {
                question_id,
                question_messages: ids(question_id)?,
                answer_messages: ids(answer.id)?,
            }))
        })
        .await?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_register_blocks_overlapping_messages_and_voice_only_same_question() {
        let chat = Chat::default();
        let alice = UserId::new(1);
        let bob = UserId::new(2);
        let m1 = MessageId::new(100);
        let m2 = MessageId::new(101);

        // Voice-only regeneration: empty message list, keyed by question_id.
        let voice_reg = chat.try_register(alice, Some(42), vec![]).unwrap();
        assert!(chat.try_register(alice, Some(42), vec![]).is_none());
        // A different question with empty message_ids is not blocked.
        let other_reg = chat.try_register(alice, Some(43), vec![]).unwrap();
        drop(other_reg);
        drop(voice_reg);
        // Once dropped, the same question can register again.
        assert!(chat.try_register(alice, Some(42), vec![]).is_some());

        // Overlapping message IDs are blocked and ❌ cancels only for the requester.
        let reg = chat.try_register(alice, None, vec![m1]).unwrap();
        assert!(chat.is_running(m1));
        assert!(chat.try_register(alice, None, vec![m1, m2]).is_none());
        assert!(!chat.stop(m1, bob));
        assert!(!reg.cancel.is_cancelled());
        assert!(chat.stop(m1, alice));
        assert!(reg.cancel.is_cancelled());
        drop(reg);
        assert!(!chat.is_running(m1));
    }
}
