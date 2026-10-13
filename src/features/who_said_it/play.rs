//! Running games: `/whosaidit` starts one in its channel, a task posts each question and
//! its answer, and button presses are recorded as they come in.
//!
//! The games running now are kept in [`GAMES`], by channel, so a button press can find its
//! game and a channel never has two at once. Nothing is saved: a restart ends the game.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use poise::CreateReply;
use serenity::all::{
    ChannelId, ComponentInteraction, CreateInteractionResponse, CreateInteractionResponseMessage,
    CreateMessage, EditMessage,
};
use tracing::{info, warn};

use super::game::{Game, Pick, make_rounds};
use super::{Settings, quotes, ui};
use crate::core::{BotCtx, Context, Result, user_error};

/// The pause between an answer and the next question, to read who got it.
const BETWEEN: Duration = Duration::from_secs(5);

/// A game being played.
struct Running {
    /// Tells this game's buttons from an older game's in the same channel.
    id: u32,
    game: Mutex<Game>,
    /// When the current question was posted, for how long each answer took.
    asked_at: Mutex<Instant>,
}

/// The channels with a game: `None` while its quotes are being read, so a second
/// `/whosaidit` can't start another one meanwhile.
static GAMES: LazyLock<Mutex<HashMap<ChannelId, Option<Arc<Running>>>>> =
    LazyLock::new(Mutex::default);

/// Takes the channel off [`GAMES`] when dropped, however the game ends.
struct Leave(ChannelId);

impl Drop for Leave {
    fn drop(&mut self) {
        GAMES.lock().unwrap().remove(&self.0);
    }
}

/// Guess who wrote old messages from this channel
#[poise::command(slash_command, guild_only, rename = "whosaidit")]
pub async fn who_said_it(
    ctx: Context<'_>,
    #[description = "How many questions (default 10)"]
    #[min = 1]
    #[max = 20]
    questions: Option<usize>,
) -> Result<()> {
    let guild = ctx
        .guild_id()
        .ok_or_else(|| user_error("Who said it? only works in a server."))?;
    let channel = ctx.channel_id();
    let bot = ctx.data().clone();
    let settings: Settings = bot.config.feature("who_said_it")?;
    {
        let mut games = GAMES.lock().unwrap();
        if games.contains_key(&channel) {
            return Err(user_error("A game is already running in this channel."));
        }
        games.insert(channel, None);
    }
    let leave = Leave(channel);

    let reply = ctx
        .say("🗨️ **Who said it?** Digging through this channel's history…")
        .await?;
    let count = questions.unwrap_or(settings.rounds).clamp(1, 20);
    let rounds = match quotes::load(&bot, channel, &settings).await {
        Ok((quotes, people)) => make_rounds(quotes, &people, count, |list| {
            fastrand::shuffle(list);
        }),
        Err(err) => {
            warn!("couldn't read quotes for Who said it?: {err:#}");
            Vec::new()
        }
    };
    if rounds.is_empty() {
        let text = "🗨️ **Who said it?** There isn't enough chatter in this channel to play. \
                    Try a busier one.";
        reply
            .edit(ctx, CreateReply::default().content(text))
            .await?;
        return Ok(());
    }
    let mut rounds = rounds;
    quotes::use_nicknames(&bot, guild, &mut rounds).await;

    let text = format!(
        "🗨️ **Who said it?** {} questions from this channel's history, {} seconds each. \
         Press who you think wrote it. Faster right answers score more.",
        rounds.len(),
        settings.answer_seconds.max(5)
    );
    reply
        .edit(ctx, CreateReply::default().content(text))
        .await?;

    let running = Arc::new(Running {
        id: fastrand::u32(..),
        game: Mutex::new(Game::new(rounds, settings.answer_seconds)),
        asked_at: Mutex::new(Instant::now()),
    });
    GAMES.lock().unwrap().insert(channel, Some(running.clone()));
    info!("Who said it? started with {count} questions");
    let answer_for = Duration::from_secs(settings.answer_seconds.max(5));
    bot.tasks.clone().spawn(async move {
        play(&bot, channel, &running, answer_for).await;
        drop(leave);
    });
    Ok(())
}

/// Posts each question, waits, shows its answer, and at the end posts the scores.
async fn play(bot: &BotCtx, channel: ChannelId, running: &Running, answer_for: Duration) {
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
                CreateMessage::new().embed(embed).components(buttons),
            )
        };
        let mut message = match channel.send_message(&bot.http, question).await {
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
            (
                EditMessage::new().embed(embed).components(buttons),
                answers,
                more,
            )
        };
        if let Err(err) = message.edit(&bot.http, answer).await {
            warn!("couldn't show a Who said it? answer: {err}");
        }

        unanswered = if answers == 0 { unanswered + 1 } else { 0 };
        if unanswered >= 2 || !more {
            break;
        }
        tokio::select! {
            () = bot.shutdown.cancelled() => break,
            () = tokio::time::sleep(BETWEEN) => {}
        }
    }

    if bot.shutdown.is_cancelled() {
        return;
    }
    let standings = running.game.lock().unwrap().standings();
    let embed = ui::scores(&standings, unanswered >= 2);
    if let Err(err) = channel
        .send_message(&bot.http, CreateMessage::new().embed(embed))
        .await
    {
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
    let name = interaction
        .member
        .as_ref()
        .map_or(interaction.user.display_name(), |m| m.display_name())
        .to_string();

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
    let response = CreateInteractionResponseMessage::new()
        .content(text)
        .ephemeral(true);
    interaction
        .create_response(&bot.http, CreateInteractionResponse::Message(response))
        .await?;
    Ok(())
}
