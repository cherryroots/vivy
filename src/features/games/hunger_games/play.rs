//! Running a Hunger Games: `/games hungergames` opens the Reaping in its channel, a task
//! waits for the host to press Start, then posts each round Vivy writes until one tribute
//! is left. Button presses change the Reaping or move the game on.
//!
//! The games now running are kept in [`GAMES`], by channel, so a press can find its game
//! and a channel never has two at once. Nothing is saved: a restart ends the game.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use chrono::Utc;
use poise::CreateReply;
use serenity::all::{
    ChannelId, ChannelType, ComponentInteraction, ComponentInteractionDataKind,
    CreateAllowedMentions, CreateAttachment, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditMessage, GuildId, Message, MessageId,
    UserId,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::game::{Game, Kind};
use super::last_words::{self, LastWords};
use super::pictures::{self, Easel};
use super::{Settings, ui, writer};
use super::{looks, notes, stats, voice};
use crate::ai::ChatProvider;
use crate::core::{BotCtx, Context, Result, user_error};
use crate::features::games::who_said_it::sources::{self, From, Source};
use crate::util::media::ModelImage;

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
    /// Wakes the game's task: Start or Cancel in the Reaping.
    wake: Notify,
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
    /// The arena's possible places, name and channel ID (see [`arena`]).
    arena: Vec<Place>,
    /// Where last words are read from.
    sources: Vec<Source>,
    quote_settings: crate::features::games::who_said_it::Settings,
}

/// A place in the arena: its channel's name and ID.
type Place = (String, u64);

/// The kinds of channel that can be places in the arena.
const PLACES: [ChannelType; 5] = [
    ChannelType::Text,
    ChannelType::News,
    ChannelType::Forum,
    ChannelType::Voice,
    ChannelType::Stage,
];

/// The channels everyone can read, for last words, and the arena's possible places in
/// random order: the name and ID of every channel Vivy can see but this one (Cherry: nothing
/// in the server is really hidden, and only the names and links are used). The game keeps
/// as many of those as it has tributes, as the arena's places for the whole game.
fn arena(bot: &BotCtx, guild: GuildId, here: ChannelId) -> Result<(Vec<Source>, Vec<Place>)> {
    let me = bot.cache.current_user().id;
    let cached = bot
        .cache
        .guild(guild)
        .ok_or_else(|| user_error("I don't know this server yet. Try again in a minute."))?;
    let sources = sources::pick(bot, &cached, here, From::Server)?;
    // Without her own member in the cache, every channel the server lists counts.
    let member = cached.members.get(&me);
    // The channel the game is played in is left out: it's where everyone watches from,
    // not a place in the arena (often a test thread, too).
    let mut places: Vec<Place> = cached
        .channels
        .values()
        .filter(|c| c.id != here && PLACES.contains(&c.kind))
        .filter(|c| bot.allows("games", Some(guild), c.id))
        .filter(|c| member.is_none_or(|m| cached.user_permissions_in(c, m).view_channel()))
        .map(|c| (c.name.clone(), c.id.get()))
        .collect();
    drop(cached);
    fastrand::shuffle(&mut places);
    Ok((sources, places))
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
        arena: mut links,
        sources,
        quote_settings,
    } = setup;
    // One place per tribute, the same ones all game.
    links.truncate(tributes.len());
    let arena: Vec<String> = links.iter().map(|(name, _)| name.clone()).collect();
    let ids: Vec<u64> = tributes.iter().map(|(id, _)| *id).collect();
    // Last words take a while to read, and aren't needed until the first cannons.
    let words = bot.tasks.spawn(last_words::load(
        bot.clone(),
        guild,
        sources,
        quote_settings,
        ids.clone(),
    ));
    // How the tributes and the arena look in her pictures, worked out while she writes the
    // Scatter.
    // Nothing is drawn when the drawing tool isn't there, so then she isn't asked.
    let drawing = extras.pictures && pictures::can_draw(bot, guild, channel);
    let looks = drawing.then(|| {
        bot.tasks.spawn(looks::describe(
            bot.clone(),
            provider.clone(),
            guild,
            tributes.clone(),
            arena.clone(),
        ))
    });
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
            links,
            notes: Arc::new(notes),
            looks,
        },
        words,
        auto,
    )
    .await;
}

/// Posts each round as Vivy writes it, with its map when pictures are on, the fallen after
/// each night, and the victor. The next round starts by itself after `auto`.
async fn play(
    bot: &BotCtx,
    stage: Stage,
    running: &Running,
    mut game: Game,
    mut writing: Writing,
    words: JoinHandle<HashMap<u64, LastWords>>,
    auto: Duration,
) {
    let channel = stage.channel;
    // Her Gamemaker line, out loud too.
    let speak = |line: String| voice::speak(bot, stage.guild, channel, stage.lobby, line);
    let looks = writing.looks.take().map(looks::Shared::new);
    let places = writing.arena.clone();
    let links = writing.links.clone();
    let easel = Easel {
        guild: stage.guild,
        channel,
        message: stage.lobby,
        host: running.host,
    };
    // Whether the channel was told why a picture is missing: once a game is enough.
    let told = Arc::new(AtomicBool::new(false));
    // Writes a round and, with pictures on, draws its map right after, so the two are
    // posted together. The Scatter's map is drawn once it's posted instead: nobody waits
    // on a next round then, and people are already reading.
    let write = |game: &Game, kind: Kind| {
        let (provider, game) = (writing.provider.clone(), game.clone());
        let game_before = game.clone();
        let (arena, notes) = (writing.arena.clone(), writing.notes.clone());
        let (ctx, looks) = (bot.clone(), looks.clone());
        bot.tasks.spawn(async move {
            let drawn = looks.is_some();
            let round = writer::write(
                provider,
                game,
                kind,
                arena.clone(),
                notes,
                drawn,
                channel.get(),
            )
            .await?;
            let Some(looks) = looks.filter(|_| kind != Kind::Scatter) else {
                return anyhow::Ok((round, Pictures::default()));
            };
            let looks = looks.get().await;
            // The game once this round is played: who carries what by the end of it.
            let mut after = game_before.clone();
            after.apply(round.clone());
            let weapons = after.weapons();
            // The final showdown is zoomed in on its zone; other rounds show the whole map.
            let map = async {
                let Some(picture) = round.picture.as_deref() else {
                    return Ok(None);
                };
                let (prompt, style) = match kind {
                    Kind::Finale => (
                        looks::showdown(&arena, looks, &weapons, picture),
                        pictures::SHOWDOWN,
                    ),
                    _ => (
                        looks::prompt(&arena, looks, &weapons, picture, game_before.alive()),
                        pictures::MAP,
                    ),
                };
                pictures::draw(&ctx, easel, &prompt, style).await.map(Some)
            };
            // After the final showdown, the victor up close, with everyone they killed.
            let victor = async {
                let winner = match after.winner_index() {
                    Some(winner) if kind == Kind::Finale => winner,
                    _ => return Ok(None),
                };
                let prompt = looks::victor(looks, &weapons, winner, &after.victims(winner));
                pictures::draw(&ctx, easel, &prompt, pictures::VICTORY)
                    .await
                    .map(Some)
            };
            let (map, victor) = tokio::join!(map, victor);
            let missing = map.as_ref().err().or(victor.as_ref().err()).copied();
            let (map, victor) = (map.ok().flatten(), victor.ok().flatten());
            anyhow::Ok((
                round,
                Pictures {
                    map,
                    victor,
                    missing,
                },
            ))
        })
    };
    let mut words = Later::Working(words);
    let intro = format!(
        "🎥 The cameras are rolling: **{} tributes** enter the arena. Vivy is writing the Scatter…",
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
        let (round, mut drawn) = match written {
            Ok(Ok(written)) => written,
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

        // The map goes with the round's last part.
        let parts = ui::round(&game, &game.rounds[index], &links);
        let count = parts.len();
        for (i, part) in parts.into_iter().enumerate() {
            let picture = if i + 1 == count {
                drawn.map.take()
            } else {
                None
            };
            post(bot, channel, part, picture).await;
        }
        if let Some(reason) = drawn.missing {
            no_picture(bot, channel, &told, reason).await;
        }
        if kind == Kind::Scatter
            && let Some(looks) = looks.clone()
            && let Some(prompt) = game.rounds[index].picture.clone()
        {
            let (ctx, arena, told) = (bot.clone(), places.clone(), told.clone());
            let everyone = game.tributes.len();
            let weapons = game.weapons();
            bot.tasks.spawn(async move {
                let looks = looks.get().await;
                let prompt = looks::prompt(&arena, looks, &weapons, &prompt, everyone);
                match pictures::draw(&ctx, easel, &prompt, pictures::MAP).await {
                    Ok(picture) => {
                        let caption = "🗺️ The arena as the Games begin".to_string();
                        post(&ctx, easel.channel, caption, Some(picture)).await;
                    }
                    Err(reason) => no_picture(&ctx, easel.channel, &told, reason).await,
                }
            });
        }
        if over {
            // The fallen since the last cannons, the loser of the final showdown among them.
            if cannons_from < game.rounds.len() {
                let title = "The last of the fallen";
                post_cannons(bot, channel, &game, cannons_from, title, &mut words).await;
            }
            // The victor's picture goes with the message's last part.
            let parts = ui::victor(&game, victory.as_deref());
            let count = parts.len();
            for (i, part) in parts.into_iter().enumerate() {
                let picture = if i + 1 == count {
                    drawn.victor.take()
                } else {
                    None
                };
                post(bot, channel, part, picture).await;
            }
            if let Some(line) = victory {
                speak(line).await;
            }
            let winner = game.winner().map(|t| t.id);
            info!(winner, rounds = game.rounds.len(), "Hunger Games ended");
            // For `/games leaderboard`.
            let (guild, finished) = (stage.guild.get(), game.clone());
            let saved = bot
                .db
                .call(move |conn| stats::record(conn, guild, channel.get(), &finished))
                .await;
            if let Err(err) = saved {
                warn!("couldn't save the Hunger Games stats: {err:#}");
            }
            return;
        }
        if let Kind::Night(day) = kind {
            let title = format!("The fallen of day {day}");
            post_cannons(bot, channel, &game, cannons_from, &title, &mut words).await;
            cannons_from = game.rounds.len();
        }

        // She writes (and draws) the next round while people read this one.
        if let Some(next) = game.next_kind() {
            pending = Some(write(&game, next));
        }
        tokio::select! {
            () = bot.shutdown.cancelled() => {
                if let Some(writing) = pending.take() {
                    writing.abort();
                }
                return;
            }
            () = tokio::time::sleep(auto) => {}
        }
    }
}

/// The pictures that go with a round, when pictures are on.
#[derive(Default)]
struct Pictures {
    /// The map, posted with the round.
    map: Option<ModelImage>,
    /// After the final showdown, the victor, posted with the victor message.
    victor: Option<ModelImage>,
    /// Why a picture couldn't be drawn, when one couldn't.
    missing: Option<&'static str>,
}

/// Tells the channel why a picture is missing, the first time one is in a game.
async fn no_picture(bot: &BotCtx, channel: ChannelId, told: &AtomicBool, reason: &str) {
    if told.swap(true, Ordering::Relaxed) {
        return;
    }
    let line = format!("-# 🗺️ No picture this time: {reason}.");
    post(bot, channel, line, None).await;
}

/// Posts one message of the game, with a picture when there is one. If Discord won't take
/// the picture, the text goes on its own.
async fn post(bot: &BotCtx, channel: ChannelId, content: String, picture: Option<ModelImage>) {
    let mut message = text(content.clone());
    if let Some(picture) = picture {
        let name = pictures::file_name(&picture);
        message = message.add_file(CreateAttachment::bytes(picture.data, name));
        match channel.send_message(&bot.http, message).await {
            Ok(_) => return,
            Err(err) => warn!("couldn't post a Hunger Games message with its map: {err}"),
        }
        message = text(content);
    }
    if let Err(err) = channel.send_message(&bot.http, message).await {
        warn!("couldn't post a Hunger Games message: {err}");
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
    /// The arena's places with their channel IDs, to show them as links.
    links: Vec<Place>,
    /// Her notes about each tribute, in the game's order.
    notes: Arc<Vec<Option<String>>>,
    /// How each tribute looks, being described, when pictures are on: then she writes
    /// picture prompts, and they're drawn.
    looks: Option<JoinHandle<looks::Looks>>,
}

/// Something a background task is still working out, like the last words, then what it
/// found.
enum Later<T> {
    Working(JoinHandle<T>),
    Done(T),
}

impl<T: Default> Later<T> {
    /// What the task found, waiting for it the first time. A task that failed found nothing.
    async fn get(&mut self) -> &T {
        if let Later::Working(task) = self {
            let found = task.await.unwrap_or_default();
            *self = Later::Done(found);
        }
        match self {
            Later::Done(found) => found,
            Later::Working(_) => unreachable!("just finished"),
        }
    }
}

/// Posts the cannons for everyone who fell in the rounds from `from` on, with their kills
/// and last words.
async fn post_cannons(
    bot: &BotCtx,
    channel: ChannelId,
    game: &Game,
    from: usize,
    title: &str,
    words: &mut Later<HashMap<u64, LastWords>>,
) {
    let words = words.get().await;
    let fallen: Vec<(u64, u32, Option<LastWords>)> = game
        .fallen_since(from)
        .iter()
        .map(|t| (t.id, t.kills, words.get(&t.id).cloned()))
        .collect();
    let message = text(ui::cannons(title, &fallen));
    if let Err(err) = channel.send_message(&bot.http, message).await {
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

/// A press in the Reaping.
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
