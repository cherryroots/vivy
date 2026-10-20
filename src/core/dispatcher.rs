//! Routes Discord events to features.
//!
//! - A message that mentions the bot goes to the feature with the longest matching mention
//!   prefix ("remind me" beats "remind"). An empty prefix matches everything, so a feature
//!   that declares `""` (chat) gets every mention nobody else claimed, and every mention a
//!   feature passed on ([`Mention::PassOn`]).
//! - Every message also goes to every feature's `on_message`, and every reaction to
//!   `on_reaction_add`.
//! - Buttons, select menus and modals have IDs like `reminders:snooze:12:10`. The part before
//!   the first `:` picks the feature; the rest is passed to it.
//! - Slash commands go to the feature that declared them (see `core::interaction`), only
//!   where that feature is allowed. Autocomplete requests go to the command's suggester.
//!
//! Each handler runs in its own task, inside a tracing span that names the feature, server,
//! channel and message. If it fails, the error is logged and the user gets a short reply.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{Instrument as _, Span, debug, error, info, info_span, warn};
use twilight_model::application::interaction::{Interaction, InteractionData, InteractionType};
use twilight_model::gateway::GatewayReaction;
use twilight_model::gateway::event::Event;

use super::errors::{is_user_error, user_error, user_message};
use super::interaction::{
    self, CommandCtx, ComponentInteraction, FeatureCommand, ModalInteraction, Respond as _,
    Response,
};
use super::{BotCtx, Error, Feature, Mention, Result};
use crate::util::discord::{
    ChannelId, Draft, GuildId, InteractionId, Message, MessageId, UserId, mentions_user, no_pings,
};

/// Called for every gateway event, after the cache has seen it.
pub async fn handle_event(bot: &BotCtx, event: Event) {
    match event {
        Event::MessageCreate(msg) => on_message(bot, &msg.0),
        Event::ReactionAdd(reaction) => on_reaction_add(bot, &reaction.0),
        Event::MessageUpdate(msg) => on_message_edit(bot, msg.0),
        Event::MessageDelete(event) => {
            on_messages_deleted(bot, event.guild_id, event.channel_id, vec![event.id])
        }
        Event::MessageDeleteBulk(event) => {
            on_messages_deleted(bot, event.guild_id, event.channel_id, event.ids)
        }
        Event::InteractionCreate(interaction) => on_interaction(bot, interaction.0).await,
        Event::Ready(_) => {
            // The first Ready is the start itself; later ones are full reconnects.
            static SEEN: AtomicBool = AtomicBool::new(false);
            if SEEN.swap(true, Ordering::Relaxed) {
                info!(target: "lifecycle", "🔌 Reconnected to Discord with a new session");
            } else {
                // The config is read before logging starts, so its warnings wait until now.
                let names: Vec<&str> = bot.features.iter().map(|f| f.name()).collect();
                for warning in bot.config.startup_warnings(&names) {
                    warn!(target: "config", "{warning}");
                }
            }
        }
        Event::Resumed => info!(target: "lifecycle", "🔌 Reconnected to Discord"),
        _ => {}
    }
}

fn on_message(bot: &BotCtx, msg: &Message) {
    if msg.author.bot {
        return;
    }
    let msg = Arc::new(msg.clone());
    let allowed = enabled_features(bot, msg.guild_id, msg.channel_id);

    for feature in &allowed {
        let (bot2, feature, msg2) = (bot.clone(), feature.clone(), msg.clone());
        run(
            bot,
            feature.name(),
            message_span(feature.name(), &msg),
            ReplyTo::Nobody,
            async move { feature.on_message(&bot2, &msg2).await },
        );
    }

    if !mentions_user(&msg, bot.bot_id) {
        return;
    }
    let text = strip_mentions(&msg.content, bot.bot_id);
    // Only features that are on here can claim a mention, so a turned-off feature's
    // prefix ("remind me ...") falls through to chat instead of getting no answer.
    let Some((feature, rest)) = route_mention(&allowed, &text) else {
        debug!("no feature claimed the mention {text:?}");
        return;
    };
    // Chat, which gets the mentions a feature passes on.
    let fallback = allowed
        .iter()
        .find(|f| f.mention_prefixes().contains(&"") && f.name() != feature.name())
        .cloned();
    let (bot2, feature2, msg2, rest) =
        (bot.clone(), feature.clone(), msg.clone(), rest.to_string());
    run(
        bot,
        feature.name(),
        message_span(feature.name(), &msg),
        ReplyTo::Message(msg.channel_id, msg.id),
        async move {
            if feature2.on_mention(&bot2, &msg2, &rest).await? == Mention::Handled {
                return Ok(());
            }
            let Some(fallback) = fallback else {
                // Chat is off here, so nobody else can answer it.
                return Err(user_error("Sorry, I couldn't make sense of that."));
            };
            info!(
                "{} passed the mention on to {}",
                feature2.name(),
                fallback.name()
            );
            // The whole text, prefix included: "remind me what the movie was called".
            fallback
                .on_mention(&bot2, &msg2, &text)
                .instrument(message_span(fallback.name(), &msg2))
                .await?;
            Ok(())
        },
    );
}

fn on_reaction_add(bot: &BotCtx, reaction: &GatewayReaction) {
    if reaction.user_id == bot.bot_id {
        return;
    }
    let reaction = Arc::new(reaction.clone());
    for feature in enabled_features(bot, reaction.guild_id, reaction.channel_id) {
        let span = info_span!(
            "reaction",
            feature = feature.name(),
            guild = reaction.guild_id.map(|g| g.get()),
            channel = reaction.channel_id.get(),
            message_id = reaction.message_id.get(),
            user = reaction.user_id.get(),
        );
        let (bot2, reaction2) = (bot.clone(), reaction.clone());
        run(bot, feature.name(), span, ReplyTo::Nobody, async move {
            feature.on_reaction_add(&bot2, &reaction2).await
        });
    }
}

fn on_message_edit(bot: &BotCtx, event: Message) {
    // The bot's own edits (streaming answers) come here too; nobody needs those.
    if event.author.bot {
        return;
    }
    let event = Arc::new(event);
    for feature in enabled_features(bot, event.guild_id, event.channel_id) {
        let span = info_span!(
            "message_edit",
            feature = feature.name(),
            guild = event.guild_id.map(|g| g.get()),
            channel = event.channel_id.get(),
            message_id = event.id.get(),
        );
        let (bot2, event2) = (bot.clone(), event.clone());
        run(bot, feature.name(), span, ReplyTo::Nobody, async move {
            feature.on_message_edit(&bot2, &event2).await
        });
    }
}

fn on_messages_deleted(
    bot: &BotCtx,
    guild: Option<GuildId>,
    channel: ChannelId,
    messages: Vec<MessageId>,
) {
    let messages = Arc::new(messages);
    for feature in enabled_features(bot, guild, channel) {
        let span = info_span!(
            "messages_deleted",
            feature = feature.name(),
            guild = guild.map(|g| g.get()),
            channel = channel.get(),
            count = messages.len(),
        );
        let (bot2, messages2) = (bot.clone(), messages.clone());
        run(bot, feature.name(), span, ReplyTo::Nobody, async move {
            feature
                .on_messages_deleted(&bot2, channel, &messages2)
                .await
        });
    }
}

async fn on_interaction(bot: &BotCtx, interaction: Interaction) {
    match (&interaction.kind, &interaction.data) {
        (InteractionType::ApplicationCommand, Some(InteractionData::ApplicationCommand(_))) => {
            on_command(bot, interaction)
        }
        (
            InteractionType::ApplicationCommandAutocomplete,
            Some(InteractionData::ApplicationCommand(_)),
        ) => on_autocomplete(bot, interaction),
        (_, Some(InteractionData::MessageComponent(_))) => {
            if let Some(i) = ComponentInteraction::from_interaction(&interaction) {
                on_component(bot, i).await;
            }
        }
        (_, Some(InteractionData::ModalSubmit(_))) => {
            if let Some(i) = ModalInteraction::from_interaction(&interaction) {
                on_modal(bot, i).await;
            }
        }
        _ => {}
    }
}

/// The feature's command with this name.
fn find_command<'a>(bot: &'a BotCtx, name: &str) -> Option<&'a FeatureCommand> {
    bot.commands
        .iter()
        .find(|command| command.command.definition.name == name)
}

fn on_command(bot: &BotCtx, interaction: Interaction) {
    let Some(InteractionData::ApplicationCommand(data)) = interaction.data.clone() else {
        return;
    };
    let ctx = CommandCtx::new(bot.clone(), Arc::new(interaction), Arc::new(*data));
    let Some(command) = find_command(bot, &ctx.data.name) else {
        warn!("no command is called /{}", ctx.data.name);
        return;
    };
    let feature = command.feature;
    let run = command.command.run;
    let span = info_span!(
        "command",
        feature,
        command = %ctx.data.name,
        guild = ctx.guild_id().map(|g| g.get()),
        channel = ctx.channel_id().get(),
        user = ctx.author_id().get(),
    );
    let bot2 = bot.clone();
    bot.tasks.spawn(
        async move {
            // A command is only allowed where its feature is.
            if !bot2.allows(feature, ctx.guild_id(), ctx.channel_id()) {
                let draft = Draft::new().content(turned_off(feature)).ephemeral();
                if let Err(err) = ctx.send(draft).await {
                    warn!("couldn't say the command is turned off: {err:#}");
                }
                return;
            }
            if let Err(err) = run(ctx.clone()).await {
                let command = &ctx.data.name;
                if is_user_error(&err) {
                    debug!("/{command}: told the user: {err}");
                } else {
                    error!("/{command} failed: {err:#}");
                }
                let draft = Draft::new().content(user_message(&err)).ephemeral();
                if let Err(err) = ctx.send(draft).await {
                    warn!("couldn't tell the user what went wrong: {err:#}");
                }
            }
        }
        .instrument(span),
    );
}

fn on_autocomplete(bot: &BotCtx, interaction: Interaction) {
    let Some(InteractionData::ApplicationCommand(data)) = interaction.data.clone() else {
        return;
    };
    let Some(command) = find_command(bot, &data.name) else {
        return;
    };
    let (Some(suggest), Some(typed)) = (command.command.autocomplete, interaction::typed(&data))
    else {
        return;
    };
    let interaction = Arc::new(interaction);
    let ctx = CommandCtx::new(bot.clone(), interaction.clone(), Arc::new(*data));
    let bot2 = bot.clone();
    bot.tasks.spawn(async move {
        let choices = match suggest(ctx, typed).await {
            Ok(choices) => choices,
            Err(err) => {
                warn!("autocomplete failed: {err:#}");
                Vec::new()
            }
        };
        if let Err(err) = interaction::suggest(&bot2, &interaction, choices).await {
            debug!("couldn't send suggestions: {err:#}");
        }
    });
}

async fn on_component(bot: &BotCtx, i: ComponentInteraction) {
    let reply = ReplyTo::Interaction {
        id: i.id,
        token: i.token.clone(),
    };
    let Some((feature, action)) = route_custom_id(bot, &i.custom_id) else {
        // Most likely a button on a message from voltgpt.
        tell_user(bot, &reply, STALE).await;
        return;
    };
    if !bot.allows(feature.name(), i.guild_id, i.channel_id) {
        tell_user(bot, &reply, &turned_off(feature.name())).await;
        return;
    }
    let span = info_span!(
        "component",
        feature = feature.name(),
        guild = i.guild_id.map(|g| g.get()),
        channel = i.channel_id.get(),
        message_id = i.message.id.get(),
        user = i.user.id.get(),
        custom_id = %i.custom_id,
    );
    let (bot2, action) = (bot.clone(), action.to_string());
    run(bot, feature.name(), span, reply, async move {
        feature.on_component(&bot2, &i, &action).await
    });
}

async fn on_modal(bot: &BotCtx, i: ModalInteraction) {
    let reply = ReplyTo::Interaction {
        id: i.id,
        token: i.token.clone(),
    };
    let Some((feature, action)) = route_custom_id(bot, &i.custom_id) else {
        tell_user(bot, &reply, STALE).await;
        return;
    };
    if !bot.allows(feature.name(), i.guild_id, i.channel_id) {
        tell_user(bot, &reply, &turned_off(feature.name())).await;
        return;
    }
    let span = info_span!(
        "modal",
        feature = feature.name(),
        guild = i.guild_id.map(|g| g.get()),
        channel = i.channel_id.get(),
        user = i.user.id.get(),
        custom_id = %i.custom_id,
    );
    let (bot2, action) = (bot.clone(), action.to_string());
    run(bot, feature.name(), span, reply, async move {
        feature.on_modal(&bot2, &i, &action).await
    });
}

/// Delivers published [`super::BotEvent`]s to every enabled feature until shutdown.
pub fn spawn_bot_events(bot: &BotCtx) {
    let bot = bot.clone();
    let mut receiver = bot.events.subscribe();
    bot.tasks.clone().spawn(async move {
        loop {
            let event = tokio::select! {
                event = receiver.recv() => event,
                _ = bot.shutdown.cancelled() => break,
            };
            let event = match event {
                Ok(event) => Arc::new(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("dropped {n} bot events because handlers were too slow");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            for feature in bot.features.iter().filter(|f| bot.gate(f.name()).enabled) {
                let (bot2, feature, event) = (bot.clone(), feature.clone(), event.clone());
                let span = info_span!("bot_event", feature = feature.name(), event = ?event);
                run(&bot, feature.name(), span, ReplyTo::Nobody, async move {
                    feature.on_bot_event(&bot2, &event).await
                });
            }
        }
    });
}

// ---------------------------------------------------------------------------------------
// Running handlers and reporting errors
// ---------------------------------------------------------------------------------------

const STALE: &str = "This button is from an older version of the bot and no longer works.";
/// "The wheel is turned off in this channel.": names the feature, so the person knows what.
fn turned_off(feature: &str) -> String {
    format!(
        "The {} is turned off in this channel.",
        feature.replace('_', " ")
    )
}

/// Where to tell the user that something went wrong.
enum ReplyTo {
    Nobody,
    Message(ChannelId, MessageId),
    /// A button, select menu or modal: its ID and token are all a response needs.
    Interaction {
        id: InteractionId,
        token: String,
    },
}

/// Runs a handler in its own task. A slow or failing handler never blocks the others, and
/// a panic ends only that task (the panic hook in `logging` logs it). The task is on the
/// shutdown tracker, so stopping the bot waits for it (up to 10 seconds) instead of cutting
/// off an answer or a database write halfway.
fn run<F>(bot: &BotCtx, feature: &'static str, span: Span, reply: ReplyTo, handler: F)
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    let bot = bot.clone();
    let tasks = bot.tasks.clone();
    tasks.spawn(
        async move {
            if let Err(err) = handler.await {
                report(&bot, feature, &reply, &err).await;
            }
        }
        .instrument(span),
    );
}

async fn report(bot: &BotCtx, feature: &str, reply: &ReplyTo, err: &Error) {
    if is_user_error(err) {
        debug!("{feature}: told the user: {err}");
    } else {
        error!("failed: {err:#}");
    }
    tell_user(bot, reply, &user_message(err)).await;
}

/// Tells the user something: privately (ephemeral) for interactions, as a reply for
/// messages.
async fn tell_user(bot: &BotCtx, reply: &ReplyTo, text: &str) {
    // An interaction can only be responded to once. If the handler already did, follow up.
    let result = match reply {
        ReplyTo::Nobody => return,
        ReplyTo::Message(channel, id) => Draft::new()
            .content(text)
            .reply_to(*id)
            .allowed_mentions(no_pings())
            .send(&bot.http, *channel)
            .await
            .map(|_| ()),
        ReplyTo::Interaction { id, token } => {
            let handle = Handle(*id, token);
            let draft = Draft::new().content(text).ephemeral();
            match handle
                .create_response(bot, Response::Message(draft.clone()))
                .await
            {
                Ok(()) => Ok(()),
                Err(_) => handle.create_followup(bot, draft).await.map(|_| ()),
            }
        }
    };
    if let Err(err) = result {
        warn!("couldn't tell the user what went wrong: {err:#}");
    }
}

/// An interaction known only by its ID and token, which is all a response needs.
struct Handle<'a>(InteractionId, &'a str);

impl super::interaction::Respond for Handle<'_> {
    fn handle(&self) -> (InteractionId, &str) {
        (self.0, self.1)
    }
}

fn message_span(feature: &'static str, msg: &Message) -> Span {
    info_span!(
        "message",
        feature,
        guild = msg.guild_id.map(|g| g.get()),
        channel = msg.channel_id.get(),
        message_id = msg.id.get(),
        user = msg.author.id.get(),
    )
}

// ---------------------------------------------------------------------------------------
// Routing (pure functions, tested below)
// ---------------------------------------------------------------------------------------

/// The features allowed in this server and channel. The same check as [`BotCtx::allows`],
/// with a thread's parent channel looked up once for all features instead of once each.
fn enabled_features(
    bot: &BotCtx,
    guild: Option<GuildId>,
    channel: ChannelId,
) -> Vec<Arc<dyn Feature>> {
    let parent = bot.thread_parent(guild, channel);
    bot.features
        .iter()
        .filter(|f| bot.gate(f.name()).allows(guild, channel, parent))
        .cloned()
        .collect()
}

fn route_custom_id<'a>(bot: &BotCtx, custom_id: &'a str) -> Option<(Arc<dyn Feature>, &'a str)> {
    let (name, action) = custom_id.split_once(':')?;
    let feature = bot.features.iter().find(|f| f.name() == name)?;
    Some((feature.clone(), action))
}

/// Removes the `<@bot>` or `<@!bot>` a message starts with, so "@Vivy remind me ..." routes
/// on "remind me". Mentions of her later in the text stay: in "stop being mean to @Vivy"
/// she is who the message is about, and chat shows them as "@Vivy".
pub fn strip_mentions(content: &str, bot: UserId) -> String {
    let mut text = content.trim();
    loop {
        let rest = text
            .strip_prefix(&format!("<@{bot}>"))
            .or_else(|| text.strip_prefix(&format!("<@!{bot}>")));
        match rest {
            Some(rest) => text = rest.trim_start(),
            None => return text.trim_end().to_string(),
        }
    }
}

/// Picks the feature whose mention prefix matches the start of `text`, and returns the text
/// after the prefix. The longest prefix wins.
pub fn route_mention<'f, 't>(
    features: &'f [Arc<dyn Feature>],
    text: &'t str,
) -> Option<(&'f Arc<dyn Feature>, &'t str)> {
    let mut best: Option<(&Arc<dyn Feature>, usize, &str)> = None;
    for feature in features {
        for prefix in feature.mention_prefixes() {
            if let Some(rest) = strip_prefix_word(text, prefix)
                && best.is_none_or(|(_, len, _)| prefix.len() > len)
            {
                best = Some((feature, prefix.len(), rest));
            }
        }
    }
    best.map(|(feature, _, rest)| (feature, rest))
}

/// `strip_prefix` that ignores case and only matches whole words: "remind" matches
/// "remind me" and "Remind", but not "reminders".
fn strip_prefix_word<'t>(text: &'t str, prefix: &str) -> Option<&'t str> {
    if prefix.is_empty() {
        return Some(text);
    }
    let head = text.get(..prefix.len())?;
    let rest = &text[prefix.len()..];
    if head.eq_ignore_ascii_case(prefix)
        && (rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        Some(rest.trim_start())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct Prefixes(&'static str, &'static [&'static str]);

    #[async_trait]
    impl Feature for Prefixes {
        fn name(&self) -> &'static str {
            self.0
        }
        fn mention_prefixes(&self) -> &'static [&'static str] {
            self.1
        }
    }

    fn features() -> Vec<Arc<dyn Feature>> {
        vec![
            Arc::new(Prefixes("reminders", &["remind", "remind me", "reminder"])),
            Arc::new(Prefixes("chat", &[""])),
        ]
    }

    fn route(text: &str) -> (&'static str, String) {
        let features = features();
        let (feature, rest) = route_mention(&features, text).unwrap();
        (feature.name(), rest.to_string())
    }

    // Ported from voltgpt's TestTrigger.
    #[test]
    fn mention_routing() {
        assert_eq!(
            route("remind me in 2h do the thing"),
            ("reminders", "in 2h do the thing".into())
        );
        assert_eq!(
            route("reminder in 30m meeting"),
            ("reminders", "in 30m meeting".into())
        );
        assert_eq!(
            route("remind in 1h call dad"),
            ("reminders", "in 1h call dad".into())
        );
        assert_eq!(
            route("REMIND ME in 2h uppercase"),
            ("reminders", "in 2h uppercase".into())
        );
        assert_eq!(
            route("Reminder in 1h mixed case"),
            ("reminders", "in 1h mixed case".into())
        );
        assert_eq!(route("what time is it"), ("chat", "what time is it".into()));
        assert_eq!(
            route("remindme no space"),
            ("chat", "remindme no space".into())
        );
        assert_eq!(
            route("reminders are great"),
            ("chat", "reminders are great".into())
        );
        assert_eq!(route(""), ("chat", "".into()));
    }

    #[test]
    fn no_fallback_without_empty_prefix() {
        let features: Vec<Arc<dyn Feature>> = vec![Arc::new(Prefixes("reminders", &["remind"]))];
        assert!(route_mention(&features, "hello").is_none());
    }

    #[test]
    fn mention_stripping() {
        let bot = UserId::new(42);
        assert_eq!(strip_mentions("<@42> remind me", bot), "remind me");
        assert_eq!(strip_mentions("  <@!42>  hi <@7>", bot), "hi <@7>");
        assert_eq!(strip_mentions("<@42> <@42> hi", bot), "hi");
        assert_eq!(
            strip_mentions("stop being mean to <@42>", bot),
            "stop being mean to <@42>"
        );
        assert_eq!(strip_mentions("<@42> is <@42> ok?", bot), "is <@42> ok?");
    }
}
