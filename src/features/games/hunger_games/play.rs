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
    CreateAttachment, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
    EditMessage, GuildId, Message, MessageId, UserId,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::game::{Game, Kind};
use super::last_words::{self, LastWords};
use super::pictures::{self, Picture};
use super::{Settings, ui, writer};
use super::{fallen, notes, voice};
use crate::ai::ChatProvider;
use crate::core::{BotCtx, Context, Result, user_error};
use crate::features::games::who_said_it::sources::{self, From, Source};

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
    /// Memories and pictures: the host's toggles, off at first.
    extras: ui::Extras,
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
            lobby.extras,
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
    // The who-said-it settings are for the quote reader (last words).
    let crate::features::games::Settings {
        who_said_it: quote_settings,
        hunger_games: settings,
    } = bot.config.feature("games")?;
    let provider = bot.ai.chat().ok_or_else(|| {
        user_error("The Hunger Games need an AI model to write them, and none is set up.")
    })?;
    let (sources, arena) = arena(&bot, guild, channel)?;

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
            extras: ui::Extras::default(),
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

    let (embed, components) = ui::lobby(
        running.id,
        running.host.get(),
        &[],
        &[],
        max,
        closes_at,
        ui::Extras::default(),
    );
    let reply = ctx
        .send(CreateReply::default().embed(embed).components(components))
        .await?;
    let lobby_message = reply.into_message().await?;
    info!(max, "Hunger Games Reaping opened");

    bot.tasks.clone().spawn(async move {
        let setup = Setup {
            guild,
            channel,
            provider,
            arena,
            sources,
            quote_settings,
        };
        run(&bot, setup, &running, lobby_message, &settings).await;
        drop(leave);
    });
    Ok(())
}

/// What a game needs besides its players.
struct Setup {
    guild: GuildId,
    channel: ChannelId,
    provider: Arc<dyn ChatProvider>,
    /// The arena's possible places (see [`arena`]).
    arena: Vec<String>,
    /// Where last words are read from.
    sources: Vec<Source>,
    quote_settings: crate::features::games::who_said_it::Settings,
}

/// The channels everyone can read (for last words), and their names other than this
/// channel's, in random order: the game keeps as many of those as it has tributes, as the
/// arena's places for the whole game.
fn arena(bot: &BotCtx, guild: GuildId, here: ChannelId) -> Result<(Vec<Source>, Vec<String>)> {
    let cached = bot
        .cache
        .guild(guild)
        .ok_or_else(|| user_error("I don't know this server yet. Try again in a minute."))?;
    let sources = sources::pick(bot, &cached, here, From::Server)?;
    drop(cached);
    // The channel the game is played in has no name here, so it's left out: it's where
    // everyone watches from, not a place in the arena (often a test thread, too).
    let mut names: Vec<String> = sources.iter().filter_map(|s| s.name.clone()).collect();
    fastrand::shuffle(&mut names);
    Ok((sources, names))
}

/// The Reaping, then the Games.
async fn run(
    bot: &BotCtx,
    setup: Setup,
    running: &Running,
    mut lobby_message: Message,
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
    let (tributes, extras) = {
        let lobby = running.lobby.lock().unwrap();
        (lobby.tributes(running.max), lobby.extras)
    };
    info!(
        tributes = tributes.len(),
        memories = extras.memories,
        pictures = extras.pictures,
        "Hunger Games started"
    );
    let Setup {
        guild,
        channel,
        provider,
        mut arena,
        sources,
        quote_settings,
    } = setup;
    // One place per tribute, the same ones all game.
    arena.truncate(tributes.len());
    let ids: Vec<u64> = tributes.iter().map(|(id, _)| *id).collect();
    // Last words take a while to read, and aren't needed until the first cannons.
    let words = bot.tasks.spawn(last_words::load(
        bot.clone(),
        guild,
        sources,
        quote_settings,
        ids.clone(),
    ));
    let notes = if extras.memories {
        notes::about(bot, guild, channel, lobby_message.id, &ids).await
    } else {
        vec![None; ids.len()]
    };
    let auto = Duration::from_secs(settings.auto_advance_seconds.max(5));
    let game = Game::new(tributes);
    let stage = Stage {
        guild,
        channel,
        lobby: lobby_message.id,
    };
    play(
        bot,
        stage,
        running,
        game,
        Writing {
            provider,
            arena,
            notes: Arc::new(notes),
            pictures: extras.pictures,
        },
        words,
        auto,
    )
    .await;
}

/// Posts each round as Vivy writes it, the fallen after each night, and the victor.
async fn play(
    bot: &BotCtx,
    stage: Stage,
    running: &Running,
    mut game: Game,
    writing: Writing,
    words: JoinHandle<HashMap<u64, LastWords>>,
    auto: Duration,
) {
    let channel = stage.channel;
    // Her Gamemaker line, out loud too.
    let speak = |line: String| voice::speak(bot, stage.guild, channel, stage.lobby, line);
    let write = |game: &Game, kind: Kind| {
        let (provider, game) = (writing.provider.clone(), game.clone());
        let (arena, notes) = (writing.arena.clone(), writing.notes.clone());
        bot.tasks.spawn(writer::write(
            provider,
            game,
            kind,
            arena,
            notes,
            writing.pictures,
            channel.get(),
        ))
    };
    let draws = writing.pictures;
    let mut words = Words::Reading(words);
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
        if let Some(line) = round.gamemaker.clone() {
            let _typing = channel.start_typing(&bot.http);
            speak(line).await;
        }
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
        // Her picture of the round, drawn in the background while people read.
        if draws && let Some(prompt) = game.rounds[index].picture.clone() {
            let picture = Picture {
                prompt,
                caption: if over {
                    "🎨 The victor".to_string()
                } else {
                    format!("🎨 {}", writer::title(kind))
                },
                // The victor is drawn from their profile picture.
                of: game.winner().filter(|_| over).map(|t| t.id),
                spoiler: over,
            };
            pictures::draw(
                bot,
                stage.guild,
                channel,
                stage.lobby,
                running.host,
                picture,
            );
        }
        if over {
            // The fallen since the last cannons, the loser of the final showdown among them.
            if cannons_from < game.rounds.len() {
                let title = "The last of the fallen";
                post_cannons(bot, &stage, &game, cannons_from, title, &mut words).await;
            }
            for part in ui::victor(&game, victory.as_deref()) {
                if let Err(err) = channel.send_message(&bot.http, text(part)).await {
                    warn!("couldn't post the Hunger Games victor: {err}");
                }
            }
            if let Some(line) = victory {
                speak(line).await;
            }
            let winner = game.winner().map(|t| t.id);
            info!(winner, rounds = game.rounds.len(), "Hunger Games ended");
            return;
        }
        if let Kind::Night(day) = kind {
            let title = format!("The fallen of day {day}");
            post_cannons(bot, &stage, &game, cannons_from, &title, &mut words).await;
            cannons_from = game.rounds.len();
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

/// Where the game is played.
struct Stage {
    guild: GuildId,
    channel: ChannelId,
    /// The Reaping message.
    lobby: MessageId,
}

/// What Vivy gets for writing each round, besides the game.
struct Writing {
    provider: Arc<dyn ChatProvider>,
    /// The arena's places.
    arena: Vec<String>,
    /// Her notes about each tribute, in the game's order.
    notes: Arc<Vec<Option<String>>>,
    /// Whether she writes picture prompts, and they're drawn.
    pictures: bool,
}

/// The last words: still being read, or read.
enum Words {
    Reading(JoinHandle<HashMap<u64, LastWords>>),
    Read(HashMap<u64, LastWords>),
}

impl Words {
    /// The last words, waiting for the reading to finish the first time.
    async fn get(&mut self) -> &HashMap<u64, LastWords> {
        if let Words::Reading(task) = self {
            let read = task.await.unwrap_or_default();
            *self = Words::Read(read);
        }
        match self {
            Words::Read(read) => read,
            Words::Reading(_) => unreachable!("just read"),
        }
    }
}

/// Posts the cannons for everyone who fell in the rounds from `from` on, with their last
/// words and the picture of them in the night sky.
async fn post_cannons(
    bot: &BotCtx,
    stage: &Stage,
    game: &Game,
    from: usize,
    title: &str,
    words: &mut Words,
) {
    let tributes = game.fallen_since(from);
    let faces = tributes
        .iter()
        .map(|t| (t.id, t.name.clone(), t.kills))
        .collect();
    let picture = fallen::picture(bot, stage.guild, title, faces).await;
    let words = words.get().await;
    let fallen: Vec<(String, Option<LastWords>)> = tributes
        .iter()
        .map(|t| (t.name.clone(), words.get(&t.id).cloned()))
        .collect();
    let mut message = text(ui::cannons(title, &fallen));
    if let Some(png) = picture {
        message = message.add_file(CreateAttachment::bytes(png, fallen::FILE));
    }
    if let Err(err) = stage.channel.send_message(&bot.http, message).await {
        warn!("couldn't post the Hunger Games cannons: {err}");
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
        ui::Action::Memories => {
            if !in_charge {
                return tell(
                    bot,
                    interaction,
                    "Only the host turns Vivy's memories on or off.",
                )
                .await;
            }
            let mut lobby = running.lobby.lock().unwrap();
            lobby.extras.memories = !lobby.extras.memories;
        }
        ui::Action::Pictures => {
            if !in_charge {
                return tell(
                    bot,
                    interaction,
                    "Only the host turns Vivy's pictures on or off.",
                )
                .await;
            }
            let mut lobby = running.lobby.lock().unwrap();
            lobby.extras.pictures = !lobby.extras.pictures;
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
