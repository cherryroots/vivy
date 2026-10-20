//! Running games: `/games whosaidit` starts one in its channel, a task posts each question and
//! its answer, and button presses are recorded as they come in.
//!
//! The games running now are kept in [`GAMES`], by channel, so a button press can find its
//! game and a channel never has two at once. Nothing is saved: a restart ends the game.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use tracing::{info, warn};
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::application::interaction::InteractionChannel;

use super::game::{Game, Pick, make_rounds};
use super::sources::{self, From};
use super::{quotes, ui};
use crate::core::{
    BotCtx, CommandCtx, ComponentInteraction, Respond, Response, Result, user_error,
};
use crate::util::discord::{ChannelId, Draft, get_channel, member_name};

/// A game being played.
struct Running {
    /// Tells this game's buttons from an older game's in the same channel.
    id: u32,
    game: Mutex<Game>,
    /// When the current question was posted, for how long each answer took.
    asked_at: Mutex<Instant>,
}

/// The channels with a game: `None` while its quotes are being read, so a second
/// `/games whosaidit` can't start another one meanwhile.
static GAMES: LazyLock<Mutex<HashMap<ChannelId, Option<Arc<Running>>>>> =
    LazyLock::new(Mutex::default);

/// Takes the channel off [`GAMES`] when dropped, however the game ends.
struct Leave(ChannelId);

impl Drop for Leave {
    fn drop(&mut self) {
        GAMES.lock().unwrap().remove(&self.0);
    }
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "whosaidit",
    desc = "Guess who wrote old messages from this channel, another one, or the whole server"
)]
pub struct WhoSaidIt {
    /// How many questions (default 10)
    #[command(min_value = 1, max_value = 20)]
    questions: Option<i64>,
    /// Take quotes from this channel instead (everyone must be able to read it)
    #[command(channel_types = "guild_text guild_announcement public_thread announcement_thread")]
    channel: Option<InteractionChannel>,
    /// Take quotes from every channel everyone can read
    whole_server: Option<bool>,
    /// How many years back quotes can come from (default: lookback_days in the config, 1 year)
    #[command(min_value = 1, max_value = 20)]
    years: Option<i64>,
}

/// `/games whosaidit`
pub async fn who_said_it(ctx: &CommandCtx, args: WhoSaidIt) -> Result<()> {
    let WhoSaidIt {
        questions,
        channel,
        whole_server,
        years,
    } = args;
    let guild = ctx
        .guild_id()
        .ok_or_else(|| user_error("Who said it? only works in a server."))?;
    let here = ctx.channel_id();
    let bot = ctx.bot.clone();
    let mut settings = bot
        .config
        .feature::<crate::features::games::Settings>("games")?
        .who_said_it;
    if let Some(years) = years {
        settings.lookback_days = years * 365;
    }
    let from = match (channel, whole_server.unwrap_or(false)) {
        (Some(_), true) => {
            return Err(user_error("Pick a channel or the whole server, not both."));
        }
        (Some(picked), false) => {
            // The command only says the channel's name and type; the cache (or Discord, for
            // an archived thread) has who may read it.
            let channel = match bot.cache.channel(picked.id) {
                Some(cached) => cached.clone(),
                None => get_channel(&bot.http, picked.id).await?,
            };
            From::Channel(Box::new(channel))
        }
        (None, true) => From::Server,
        (None, false) => From::Here,
    };
    let what = match &from {
        From::Here => "this channel's history".to_string(),
        From::Channel(c) if c.id == here => "this channel's history".to_string(),
        From::Channel(c) => format!("<#{}>", c.id),
        From::Server => "the whole server".to_string(),
    };
    if bot.cache.guild(guild).is_none() {
        return Err(user_error(
            "I don't know this server yet. Try again in a minute.",
        ));
    }
    let sources = sources::pick(&bot, guild, here, from)?;
    let channel = here;
    {
        let mut games = GAMES.lock().unwrap();
        if games.contains_key(&channel) {
            return Err(user_error("A game is already running in this channel."));
        }
        games.insert(channel, None);
    }
    let leave = Leave(channel);

    let reply = ctx
        .say(format!("🗨️ **Who said it?** Digging through {what}…"))
        .await?;
    let count = questions
        .map_or(settings.rounds, |questions| questions as usize)
        .clamp(1, 20);
    let rounds = match quotes::load(&bot, &sources, &settings).await {
        Ok((quotes, people)) => make_rounds(quotes, &people, count, |list| {
            fastrand::shuffle(list);
        }),
        Err(err) => {
            warn!("couldn't read quotes for Who said it?: {err:#}");
            Vec::new()
        }
    };
    if rounds.is_empty() {
        let text = format!(
            "🗨️ **Who said it?** There isn't enough chatter in {what} to play. Try a busier \
             channel."
        );
        reply.edit(ctx, Draft::new().content(text)).await?;
        return Ok(());
    }
    let mut rounds = rounds;
    quotes::use_nicknames(&bot, guild, &mut rounds).await;

    let text = format!(
        "🗨️ **Who said it?** {} questions from {what}{}, {} seconds each. \
         Press who you think wrote it. Faster right answers score more.",
        rounds.len(),
        match years {
            Some(1) => " (the last year)".to_string(),
            Some(years) => format!(" (the last {years} years)"),
            None => String::new(),
        },
        settings.answer_seconds.max(5)
    );
    reply.edit(ctx, Draft::new().content(text)).await?;

    let running = Arc::new(Running {
        id: fastrand::u32(..),
        game: Mutex::new(Game::new(rounds, settings.answer_seconds)),
        asked_at: Mutex::new(Instant::now()),
    });
    GAMES.lock().unwrap().insert(channel, Some(running.clone()));
    info!("Who said it? started with {count} questions");
    let answer_for = Duration::from_secs(settings.answer_seconds.max(5));
    let between = Duration::from_secs(settings.pause_seconds);
    bot.tasks.clone().spawn(async move {
        play(&bot, channel, &running, answer_for, between).await;
        drop(leave);
    });
    Ok(())
}

/// Posts each question, waits `answer_for`, shows its answer, pauses `between`, and at the
/// end posts the scores.
async fn play(
    bot: &BotCtx,
    channel: ChannelId,
    running: &Running,
    answer_for: Duration,
    between: Duration,
) {
    // Rounds in a row that nobody answered. Two and the game stops.
    let mut unanswered = 0;
    loop {
        let ends_at = Utc::now().timestamp() + answer_for.as_secs() as i64;
        let (index, total, question) = {
            let game = running.game.lock().unwrap();
            let Some((index, round)) = game.round() else {
                break;
            };
            let (embed, buttons) = ui::question(running.id, index, game.total(), round, ends_at);
            (
                index,
                game.total(),
                Draft::new().embed(embed).components(buttons),
            )
        };
        let message = match question.send(&bot.http, channel).await {
            Ok(message) => message,
            Err(err) => {
                warn!("couldn't post a Who said it? question: {err}");
                break;
            }
        };
        *running.asked_at.lock().unwrap() = Instant::now();

        tokio::select! {
            () = bot.shutdown.cancelled() => break,
            () = tokio::time::sleep(answer_for) => {}
        }

        let (answer, answers, more) = {
            let mut game = running.game.lock().unwrap();
            let answers = game.answers();
            let outcome = game.close_round();
            let Some((_, round)) = game.round() else {
                break;
            };
            let (embed, buttons) = ui::answer(running.id, index, total, round, &outcome);
            let more = game.next_round();
            (Draft::new().embed(embed).components(buttons), answers, more)
        };
        if let Err(err) = answer.edit(&bot.http, channel, message.id).await {
            warn!("couldn't show a Who said it? answer: {err}");
        }

        unanswered = if answers == 0 { unanswered + 1 } else { 0 };
        if unanswered >= 2 || !more {
            break;
        }
        tokio::select! {
            () = bot.shutdown.cancelled() => break,
            () = tokio::time::sleep(between) => {}
        }
    }

    if bot.shutdown.is_cancelled() {
        return;
    }
    let standings = running.game.lock().unwrap().standings();
    let embed = ui::scores(&standings, unanswered >= 2);
    if let Err(err) = Draft::new().embed(embed).send(&bot.http, channel).await {
        warn!("couldn't post the Who said it? scores: {err}");
    }
    info!("Who said it? ended with {} players", standings.len());
}

/// A button press: records the answer and tells only the presser how it went.
pub async fn on_component(
    bot: &BotCtx,
    interaction: &ComponentInteraction,
    action: &str,
) -> Result<()> {
    let Some((id, round, choice)) = ui::parse_button(action) else {
        return Ok(());
    };
    let running = GAMES
        .lock()
        .unwrap()
        .get(&interaction.channel_id)
        .cloned()
        .flatten()
        .filter(|r| r.id == id);
    let player = interaction.user.id.get();
    let nick = interaction.member.as_ref().and_then(|m| m.nick.as_deref());
    let name = member_name(nick, &interaction.user).to_string();

    let text = match running {
        None => "This game is over.".to_string(),
        Some(running) => {
            let took = running.asked_at.lock().unwrap().elapsed().as_millis() as u64;
            let mut game = running.game.lock().unwrap();
            let result = game.pick(round, player, &name, choice, took);
            let name_of = |id: u64| {
                game.round()
                    .and_then(|(_, r)| r.choices.iter().find(|c| c.id == id))
                    .map_or_else(String::new, |c| c.name.clone())
            };
            match result {
                Pick::Locked => format!("Locked in: **{}**.", name_of(choice)),
                Pick::Already(earlier) => format!("You already picked **{}**.", name_of(earlier)),
                Pick::TooLate => "Too late, that question is over.".to_string(),
            }
        }
    };
    let response = Draft::new().content(text).ephemeral();
    interaction
        .create_response(bot, Response::Message(response))
        .await?;
    Ok(())
}
