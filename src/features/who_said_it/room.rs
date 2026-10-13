//! Rooms: one running game per Activity window group. Everyone who opens the Activity in
//! the same channel shares a Discord "instance", and every instance gets a room.
//!
//! A room is a task that owns its [`Game`]. Players' connections send it [`Input`]s through
//! a channel, and it sends back what to show through a `watch` channel, which always holds
//! the latest view: a slow connection skips views instead of falling behind. Only the room
//! task touches the game, so it needs no lock.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use serenity::all::{ChannelId, CreateAllowedMentions, CreateMessage, GuildId};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tracing::{Instrument as _, info, info_span, warn};

use super::Settings;
use super::game::{Game, Person, Phase, make_rounds};
use super::quotes;
use crate::core::BotCtx;

/// How long a room stays open once everyone has left, so a reload doesn't lose the game.
const EMPTY_FOR: Duration = Duration::from_secs(120);

/// What a player's connection tells the room.
pub enum Input {
    Join(Person),
    Leave(u64),
    Start,
    Pick { player: u64, choice: u64 },
}

/// Where an instance runs.
#[derive(Clone, Copy)]
pub struct Place {
    pub guild: GuildId,
    pub channel: ChannelId,
}

/// A way into a running room.
#[derive(Clone)]
pub struct Room {
    pub inputs: mpsc::Sender<Input>,
    /// The latest view, as the JSON sent to the page.
    pub view: watch::Receiver<String>,
}

/// Every open room, by instance ID.
#[derive(Default)]
pub struct Rooms {
    open: Mutex<HashMap<String, Room>>,
}

impl Rooms {
    /// Joins `person` to the instance's room, opening it if needed.
    pub async fn join(
        self: &Arc<Self>,
        ctx: &BotCtx,
        settings: &Arc<Settings>,
        instance: &str,
        place: Place,
        person: Person,
    ) -> Option<Room> {
        // A room that is closing can still be in the list for a moment: then it's taken
        // off, and the second try opens a new one.
        for _ in 0..2 {
            let room = self.open_room(ctx, settings, instance, place);
            if room.inputs.send(Input::Join(person.clone())).await.is_ok() {
                return Some(room);
            }
            self.forget(instance, &room.inputs);
        }
        None
    }

    /// Takes the instance's room off the list, if it's still the one `inputs` belongs to
    /// (a new room may already have taken its place).
    fn forget(&self, instance: &str, inputs: &mpsc::Sender<Input>) {
        let mut open = self.open.lock().unwrap();
        if open
            .get(instance)
            .is_some_and(|room| room.inputs.same_channel(inputs))
        {
            open.remove(instance);
        }
    }

    fn open_room(
        self: &Arc<Self>,
        ctx: &BotCtx,
        settings: &Arc<Settings>,
        instance: &str,
        place: Place,
    ) -> Room {
        let mut open = self.open.lock().unwrap();
        if let Some(room) = open.get(instance) {
            return room.clone();
        }
        let (inputs, receiver) = mpsc::channel(64);
        let (view_sender, view) = watch::channel(String::new());
        let room = Room {
            inputs: inputs.clone(),
            view,
        };
        open.insert(instance.to_string(), room.clone());

        let span = info_span!(
            "who_said_it",
            guild = place.guild.get(),
            channel = place.channel.get(),
        );
        let listing = Listing {
            rooms: self.clone(),
            instance: instance.to_string(),
            inputs,
        };
        let task = run(
            ctx.clone(),
            settings.clone(),
            listing,
            place,
            receiver,
            view_sender,
        );
        ctx.tasks.spawn(task.instrument(span));
        room
    }
}

/// A room's entry in [`Rooms`], so its task can take itself off the list when it ends.
struct Listing {
    rooms: Arc<Rooms>,
    instance: String,
    inputs: mpsc::Sender<Input>,
}

/// The room's task: plays the game until everyone has been gone for [`EMPTY_FOR`].
async fn run(
    ctx: BotCtx,
    settings: Arc<Settings>,
    listing: Listing,
    place: Place,
    mut inputs: mpsc::Receiver<Input>,
    view: watch::Sender<String>,
) {
    info!("Who said it? room opened");
    let opened = Instant::now();
    // The game counts time in milliseconds since the room opened.
    let now = || opened.elapsed().as_millis() as u64;
    let mut game = Game::new(settings.answer_seconds);
    let mut empty_since: Option<Instant> = None;

    loop {
        // Wake up at the next deadline, or when the empty room should close.
        let deadline = game.deadline().map(|ms| opened + Duration::from_millis(ms));
        let close_at = empty_since.map(|since| since + EMPTY_FOR);
        let wake = [deadline, close_at].into_iter().flatten().min();
        let was_done = game.phase == Phase::Done;

        tokio::select! {
            () = ctx.shutdown.cancelled() => break,
            input = inputs.recv() => match input {
                None => break,
                Some(Input::Join(person)) => game.join(person),
                Some(Input::Leave(player)) => game.leave(player),
                Some(Input::Pick { player, choice }) => {
                    if game.pick(player, choice, now()) {
                        game.reveal(now());
                    }
                }
                Some(Input::Start) => {
                    if game.start_loading() {
                        send_view(&view, &game, now());
                        load(&ctx, &settings, place, &mut game, now).await;
                    }
                }
            },
            () = sleep_until(wake) => {
                if close_at.is_some_and(|at| Instant::now() >= at) {
                    break;
                }
                game.tick(now());
            }
        }

        empty_since = match (game.anyone_here(), empty_since) {
            (true, _) => None,
            (false, None) => Some(Instant::now()),
            (false, since) => since,
        };
        if !was_done && game.phase == Phase::Done && settings.post_results {
            post_results(&ctx, place.channel, &game).await;
        }
        send_view(&view, &game, now());
    }

    // Joins sent from now on fail, and `Rooms::join` opens a new room for them.
    inputs.close();
    listing.rooms.forget(&listing.instance, &listing.inputs);
    info!("Who said it? room closed");
}

/// Reads quotes and starts the first question, or goes back to the lobby saying why not.
async fn load(
    ctx: &BotCtx,
    settings: &Settings,
    place: Place,
    game: &mut Game,
    now: impl Fn() -> u64,
) {
    let (quotes, people) = match quotes::load(ctx, place.channel, settings).await {
        Ok(found) => found,
        Err(err) => {
            warn!("couldn't read quotes for Who said it?: {err:#}");
            game.load_failed("I couldn't read this channel's history.".into());
            return;
        }
    };
    let mut rounds = make_rounds(quotes, &people, settings.rounds, |list| {
        fastrand::shuffle(list);
    });
    if rounds.is_empty() {
        game.load_failed("There isn't enough chatter here to play. Try a busier channel.".into());
        return;
    }
    quotes::use_nicknames(ctx, place.guild, &mut rounds).await;
    info!("Who said it? started with {} rounds", rounds.len());
    game.begin(rounds, now());
}

fn send_view(view: &watch::Sender<String>, game: &Game, now: u64) {
    let text = json!({ "type": "view", "view": game.view(now) }).to_string();
    view.send_replace(text);
}

/// Posts the final scores in the channel, without pinging anyone.
async fn post_results(ctx: &BotCtx, channel: ChannelId, game: &Game) {
    let lines: Vec<String> = game
        .standings()
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, p)| format!("{}. **{}** {}", i + 1, p.person.name, p.score))
        .collect();
    if lines.is_empty() {
        return;
    }
    let message = CreateMessage::new()
        .content(format!("🗨️ **Who said it?** results\n{}", lines.join("\n")))
        .allowed_mentions(CreateAllowedMentions::new());
    if let Err(err) = channel.send_message(&ctx.http, message).await {
        warn!("couldn't post the Who said it? results: {err}");
    }
}

/// Sleeps until `at`, or forever when there's nothing to wait for.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
