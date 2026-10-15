//! Running a Hunger Games: `/games hungergames` opens the Reaping in its channel, a task
//! waits for the host to press Start, then posts each round Vivy writes until one tribute
//! is left. Button presses change the Reaping or move the game on.
//!
//! The games now running are kept in [`GAMES`], by channel, so a press can find its game
//! and a channel never has two at once. Nothing is saved: a restart ends the game.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use chrono::Utc;
use poise::CreateReply;
use serenity::all::{
    ChannelId, ComponentInteraction, ComponentInteractionDataKind, CreateAllowedMentions,
    CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, EditMessage,
    Message, UserId,
};
use tokio::sync::Notify;
use tracing::{info, warn};

use super::game::{Game, Kind};
use super::{Settings, ui, writer};
use crate::ai::ChatProvider;
use crate::core::{BotCtx, Context, Result, user_error};
use crate::features::who_said_it::sources::{self, From};

/// Discord's limit for a user menu, and more than a round's message can hold well.
const MOST_TRIBUTES: usize = 24;

/// Where the Reaping is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Open,
    Started,
    Cancelled,
}

/// Who's entering: the host's picks, then volunteers.
struct Lobby {
    picked: Vec<(u64, String)>,
    volunteers: Vec<(u64, String)>,
    state: State,
}

impl Lobby {
    /// Everyone in, picks first, nobody twice, at most `max`.
    fn tributes(&self, max: usize) -> Vec<(u64, String)> {
        let mut all = self.picked.clone();
        for v in &self.volunteers {
            if !all.iter().any(|(id, _)| *id == v.0) {
                all.push(v.clone());
            }
        }
        all.truncate(max);
        all
    }
}

/// A game, from the Reaping to the victor.
struct Running {
    /// Tells this game's buttons from an older game's in the same channel.
    id: u32,
    host: UserId,
    max: usize,
    /// When the Reaping closes, as a Unix time.
    closes_at: i64,
    lobby: Mutex<Lobby>,
    /// Wakes the game's task: Start or Cancel in the Reaping, Next during the game.
    wake: Notify,
    /// The round whose Next button moves the game on, while it waits for one.
    waiting_on: Mutex<Option<usize>>,
}

impl Running {
    /// The Reaping message as it is now.
    fn lobby_message(&self) -> CreateInteractionResponseMessage {
        let lobby = self.lobby.lock().unwrap();
        let picked: Vec<u64> = lobby.picked.iter().map(|(id, _)| *id).collect();
        let (embed, components) = ui::lobby(
            self.id,
            self.host.get(),
            &lobby.tributes(self.max),
            &picked,
            self.max,
            self.closes_at,
        );
        CreateInteractionResponseMessage::new()
            .embed(embed)
            .components(components)
    }
}

static GAMES: LazyLock<Mutex<HashMap<ChannelId, Arc<Running>>>> = LazyLock::new(Mutex::default);

/// Takes the channel off [`GAMES`] when dropped, however the game ends.
struct Leave(ChannelId);

impl Drop for Leave {
    fn drop(&mut self) {
        GAMES.lock().unwrap().remove(&self.0);
    }
}

/// A battle royale between members, written by Vivy, until one is left
#[poise::command(slash_command, guild_only, rename = "hungergames")]
pub async fn hunger_games(ctx: Context<'_>) -> Result<()> {
    let guild = ctx
        .guild_id()
        .ok_or_else(|| user_error("The Hunger Games only work in a server."))?;
    let channel = ctx.channel_id();
    let bot = ctx.data().clone();
    let settings: crate::features::who_said_it::Settings = bot.config.feature("who_said_it")?;
    let settings = settings.hunger_games;
    let provider = bot.ai.chat().ok_or_else(|| {
        user_error("The Hunger Games need an AI model to write them, and none is set up.")
    })?;
    let arena = arena(&bot, guild, channel)?;

    let max = settings.max_tributes.clamp(2, MOST_TRIBUTES);
    let closes_at = Utc::now().timestamp() + settings.lobby_minutes.max(1) as i64 * 60;
    let running = Arc::new(Running {
        id: fastrand::u32(..),
        host: ctx.author().id,
        max,
        closes_at,
        lobby: Mutex::new(Lobby {
            picked: Vec::new(),
            volunteers: Vec::new(),
            state: State::Open,
        }),
        wake: Notify::new(),
        waiting_on: Mutex::new(None),
    });
    {
        let mut games = GAMES.lock().unwrap();
        if games.contains_key(&channel) {
            return Err(user_error(
                "A Hunger Games is already running in this channel.",
            ));
        }
        games.insert(channel, running.clone());
    }
    let leave = Leave(channel);

    let (embed, components) = ui::lobby(running.id, running.host.get(), &[], &[], max, closes_at);
    let reply = ctx
        .send(CreateReply::default().embed(embed).components(components))
        .await?;
    let lobby_message = reply.into_message().await?;
    info!(max, "Hunger Games Reaping opened");

    bot.tasks.clone().spawn(async move {
        run(
            &bot,
            channel,
            &running,
            lobby_message,
            provider,
            arena,
            &settings,
        )
        .await;
        drop(leave);
    });
    Ok(())
}

/// The names of the channels everyone can read, other than this one, in random order. The game keeps as many of
/// them as it has tributes, as the arena's places for the whole game.
fn arena(bot: &BotCtx, guild: serenity::all::GuildId, here: ChannelId) -> Result<Vec<String>> {
    let cached = bot
        .cache
        .guild(guild)
        .ok_or_else(|| user_error("I don't know this server yet. Try again in a minute."))?;
    let picked = sources::pick(bot, &cached, here, From::Server)?;
    drop(cached);
    // The channel the game is played in has no name here, so it's left out: it's where
    // everyone watches from, not a place in the arena (often a test thread, too).
    let mut names: Vec<String> = picked
        .into_iter()
        .filter_map(|source| source.name)
        .collect();
    fastrand::shuffle(&mut names);
    Ok(names)
}

/// The Reaping, then the Games.
async fn run(
    bot: &BotCtx,
    channel: ChannelId,
    running: &Running,
    mut lobby_message: Message,
    provider: Arc<dyn ChatProvider>,
    arena: Vec<String>,
    settings: &Settings,
) {
    let closes_in = Duration::from_secs(settings.lobby_minutes.max(1) * 60);
    let closes = tokio::time::Instant::now() + closes_in;
    loop {
        match running.lobby.lock().unwrap().state {
            State::Open => {}
            State::Started => break,
            State::Cancelled => return,
        }
        tokio::select! {
            () = bot.shutdown.cancelled() => return,
            () = running.wake.notified() => {}
            () = tokio::time::sleep_until(closes) => {
                let tributes = {
                    let mut lobby = running.lobby.lock().unwrap();
                    // A Start pressed at the last moment still counts.
                    if lobby.state == State::Started {
                        break;
                    }
                    lobby.state = State::Cancelled;
                    lobby.tributes(running.max)
                };
                let embed = ui::lobby_closed(&tributes, "Nobody pressed Start in time, so the Games are off.");
                let edit = EditMessage::new().embed(embed).components(Vec::new());
                if let Err(err) = lobby_message.edit(&bot.http, edit).await {
                    warn!("couldn't close the Hunger Games Reaping: {err}");
                }
                return;
            }
        }
    }
    let tributes = running.lobby.lock().unwrap().tributes(running.max);
    info!(tributes = tributes.len(), "Hunger Games started");
    // One place per tribute, the same ones all game.
    let mut arena = arena;
    arena.truncate(tributes.len());
    let auto = Duration::from_secs(settings.auto_advance_seconds.max(5));
    play(
        bot,
        channel,
        running,
        Game::new(tributes),
        provider,
        arena,
        auto,
    )
    .await;
}

/// Posts each round as Vivy writes it, the fallen after each night, and the victor.
async fn play(
    bot: &BotCtx,
    channel: ChannelId,
    running: &Running,
    mut game: Game,
    provider: Arc<dyn ChatProvider>,
    arena: Vec<String>,
    auto: Duration,
) {
    let write = |game: &Game, kind: Kind| {
        let (provider, game, arena) = (provider.clone(), game.clone(), arena.clone());
        bot.tasks
            .spawn(writer::write(provider, game, kind, arena, channel.get()))
    };
    let intro = format!(
        "🎥 The cameras are rolling: **{} tributes** enter the arena. Vivy is writing the Bloodbath…",
        game.tributes.len()
    );
    if let Err(err) = channel.say(&bot.http, intro).await {
        warn!("couldn't post the Hunger Games intro: {err}");
    }
    let mut pending = None;
    // The first round the next cannon message covers.
    let mut cannons_from = 0;
    while let Some(kind) = game.next_kind() {
        let mut writing = pending.take().unwrap_or_else(|| write(&game, kind));
        let written = {
            let _typing = channel.start_typing(&bot.http);
            tokio::select! {
                () = bot.shutdown.cancelled() => {
                    writing.abort();
                    return;
                }
                written = &mut writing => written,
            }
        };
        let round = match written {
            Ok(Ok(round)) => round,
            Ok(Err(err)) => {
                warn!("couldn't write a Hunger Games round: {err:#}");
                called_off(bot, channel).await;
                return;
            }
            Err(err) => {
                warn!("the Hunger Games writer stopped: {err}");
                called_off(bot, channel).await;
                return;
            }
        };
        let index = game.rounds.len();
        let victory = round.victory.clone();
        game.apply(round);
        let over = game.winner().is_some();

        // The Next button goes on the round's last part.
        let parts = ui::round(&game, &game.rounds[index]);
        let count = parts.len();
        let mut posted = None;
        for (i, part) in parts.into_iter().enumerate() {
            let mut message = text(part);
            if !over && i + 1 == count {
                message = message.components(ui::next_button(running.id, index));
            }
            match channel.send_message(&bot.http, message).await {
                Ok(sent) => posted = Some(sent),
                Err(err) => warn!("couldn't post a Hunger Games round: {err}"),
            }
        }
        if over {
            for part in ui::victor(&game, victory.as_deref()) {
                if let Err(err) = channel.send_message(&bot.http, text(part)).await {
                    warn!("couldn't post the Hunger Games victor: {err}");
                }
            }
            let winner = game.winner().map(|t| t.id);
            info!(winner, rounds = game.rounds.len(), "Hunger Games ended");
            return;
        }
        if let Kind::Night(day) = kind {
            let fallen: Vec<String> = game
                .fallen_since(cannons_from)
                .iter()
                .map(|t| t.name.clone())
                .collect();
            cannons_from = game.rounds.len();
            let message = text(ui::cannons(day, &fallen));
            if let Err(err) = channel.send_message(&bot.http, message).await {
                warn!("couldn't post the Hunger Games cannons: {err}");
            }
        }

        // She writes the next round while people read this one.
        if let Some(next) = game.next_kind() {
            pending = Some(write(&game, next));
        }
        *running.waiting_on.lock().unwrap() = Some(index);
        tokio::select! {
            () = bot.shutdown.cancelled() => {
                if let Some(writing) = pending.take() {
                    writing.abort();
                }
                return;
            }
            () = running.wake.notified() => {}
            () = tokio::time::sleep(auto) => {}
        }
        *running.waiting_on.lock().unwrap() = None;
        if let Some(posted) = posted.as_mut() {
            let edit = EditMessage::new().components(Vec::new());
            if let Err(err) = posted.edit(&bot.http, edit).await {
                warn!("couldn't take the Next button off a Hunger Games round: {err}");
            }
        }
    }
}

/// A game message: plain text that pings nobody, even when a name or her text has an
/// @everyone or a mention in it.
fn text(content: String) -> CreateMessage {
    CreateMessage::new()
        .content(content)
        .allowed_mentions(CreateAllowedMentions::new())
}

async fn called_off(bot: &BotCtx, channel: ChannelId) {
    let text = "📡 The arena's cameras broke down, so these Games are called off. Sorry! Try again in a bit.";
    if let Err(err) = channel.say(&bot.http, text).await {
        warn!("couldn't call off the Hunger Games: {err}");
    }
}

/// A press in the Reaping or on a round's Next button.
pub async fn on_component(
    bot: &BotCtx,
    interaction: &ComponentInteraction,
    action: &str,
) -> Result<()> {
    let Some((id, action)) = ui::parse_id(action) else {
        return Ok(());
    };
    let running = GAMES
        .lock()
        .unwrap()
        .get(&interaction.channel_id)
        .cloned()
        .filter(|r| r.id == id);
    let Some(running) = running else {
        return tell(bot, interaction, "These Games are over.").await;
    };
    let user = interaction.user.id;
    let in_charge = user == running.host || bot.is_admin(user);

    if let ui::Action::Next(round) = action {
        if !in_charge {
            return tell(
                bot,
                interaction,
                "Only the host moves the Games on. The next round starts by itself soon.",
            )
            .await;
        }
        {
            let mut waiting = running.waiting_on.lock().unwrap();
            if *waiting == Some(round) {
                *waiting = None;
                running.wake.notify_one();
            }
        }
        interaction
            .create_response(&bot.http, CreateInteractionResponse::Acknowledge)
            .await?;
        return Ok(());
    }

    if running.lobby.lock().unwrap().state != State::Open {
        return tell(bot, interaction, "The Reaping is over.").await;
    }
    match action {
        ui::Action::Pick => {
            if !in_charge {
                return tell(
                    bot,
                    interaction,
                    "Only the host picks tributes. Press **Volunteer** to enter yourself.",
                )
                .await;
            }
            let ComponentInteractionDataKind::UserSelect { values } = &interaction.data.kind else {
                return Ok(());
            };
            let resolved = &interaction.data.resolved;
            let picked: Vec<(u64, String)> = values
                .iter()
                .filter(|id| !resolved.users.get(id).is_some_and(|u| u.bot))
                .map(|id| {
                    let name = resolved
                        .members
                        .get(id)
                        .and_then(|m| m.nick.clone())
                        .or_else(|| resolved.users.get(id).map(|u| u.display_name().to_string()))
                        .unwrap_or_else(|| format!("Tribute {id}"));
                    (id.get(), name)
                })
                .take(running.max)
                .collect();
            running.lobby.lock().unwrap().picked = picked;
        }
        ui::Action::Volunteer => {
            let name = interaction
                .member
                .as_ref()
                .map_or(interaction.user.display_name(), |m| m.display_name())
                .to_string();
            let refusal = {
                let mut lobby = running.lobby.lock().unwrap();
                let me = user.get();
                if let Some(at) = lobby.volunteers.iter().position(|(id, _)| *id == me) {
                    lobby.volunteers.remove(at);
                    None
                } else if lobby.picked.iter().any(|(id, _)| *id == me) {
                    Some("The host already picked you, so you're in.")
                } else if lobby.tributes(running.max).len() >= running.max {
                    Some("The arena is full.")
                } else {
                    lobby.volunteers.push((me, name));
                    None
                }
            };
            if let Some(refusal) = refusal {
                return tell(bot, interaction, refusal).await;
            }
        }
        ui::Action::Start | ui::Action::Cancel => {
            if !in_charge {
                return tell(
                    bot,
                    interaction,
                    "Only the host can start or cancel the Games.",
                )
                .await;
            }
            let start = action == ui::Action::Start;
            let tributes = {
                let mut lobby = running.lobby.lock().unwrap();
                let tributes = lobby.tributes(running.max);
                if !start || tributes.len() >= 2 {
                    lobby.state = if start {
                        State::Started
                    } else {
                        State::Cancelled
                    };
                }
                tributes
            };
            if start && tributes.len() < 2 {
                return tell(bot, interaction, "The Games need at least 2 tributes.").await;
            }
            running.wake.notify_one();
            let why = if start {
                "The Reaping is over. Let the Games begin!"
            } else {
                "The host called off the Games."
            };
            let message = CreateInteractionResponseMessage::new()
                .embed(ui::lobby_closed(&tributes, why))
                .components(Vec::new());
            interaction
                .create_response(&bot.http, CreateInteractionResponse::UpdateMessage(message))
                .await?;
            return Ok(());
        }
        ui::Action::Next(_) => unreachable!("handled above"),
    }
    interaction
        .create_response(
            &bot.http,
            CreateInteractionResponse::UpdateMessage(running.lobby_message()),
        )
        .await?;
    Ok(())
}

/// Answers a press with a note only the presser sees.
async fn tell(bot: &BotCtx, interaction: &ComponentInteraction, text: &str) -> Result<()> {
    let message = CreateInteractionResponseMessage::new()
        .content(text)
        .ephemeral(true);
    interaction
        .create_response(&bot.http, CreateInteractionResponse::Message(message))
        .await?;
    Ok(())
}
