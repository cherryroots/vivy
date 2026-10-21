//! A web page that shows her playing live: the Game Boy's screen as it plays, with what
//! code sees drawn over it (the steps it has marked as floor, walls, grass, doors and
//! people), the area's map so far, the words on screen and the facts from the game's
//! memory, beside what the dashboard shows. It's off unless `web_bind` is set, like
//! `"127.0.0.1:8787"`, with a web server in front for the public address (`web_url`).
//!
//! - `/` is the page (`web/index.html`, built into the bot).
//! - `/live` is a WebSocket. It sends each new screen as a PNG (after the screen's number,
//!   8 bytes), and JSON whenever her look or the dashboard's [`Now`] changes.
//! - `/state.json` is the latest look and `Now`, for checking by hand.
//!
//! One feed reads the Game Boy for every viewer, and only while someone is watching.

use std::net::SocketAddr;
use std::sync::LazyLock;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use serde::Serialize;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::console::console;
use super::map::Pos;
use super::pictures;
use super::session::Now;
use super::walk::look_for;
use super::world::Grid;

/// How often the feed checks the Game Boy for new screens, in milliseconds.
const CHECK_EVERY: u64 = 150;
/// Messages a slow viewer can fall behind before it misses some.
const BACKLOG: usize = 1024;

const PAGE: &str = include_str!("web/index.html");

/// What code sees when it looks at the game, as the page draws it.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Seen {
    /// Screens recorded when it looked: the page shows this over that screen.
    pub mark: u64,
    pub area: String,
    pub at: Pos,
    pub can_walk: bool,
    /// The words on screen.
    pub text: String,
    /// Facts from the game's memory, a line each.
    pub facts: Vec<String>,
    /// How the steps sit on the screen, while the map shows.
    pub grid: Option<Grid>,
    /// The steps on screen, a row of the map's letters each (`@` her), while the map shows.
    pub on_screen: Vec<String>,
    /// The whole area as seen so far: its top left step, then a row of letters each.
    pub map_from: Pos,
    pub map: Vec<String>,
}

/// What she's doing, from the session (`playing.rs` keeps it up to date).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Live {
    pub playing: bool,
    pub game: String,
    pub now: Now,
}

/// The latest [`Live`], for the page.
pub fn live() -> &'static watch::Sender<Live> {
    static LIVE: LazyLock<watch::Sender<Live>> =
        LazyLock::new(|| watch::Sender::new(Live::default()));
    &LIVE
}

/// Shows `now` on the page while a session plays, and that it's over once the session
/// drops its end of `now`.
pub async fn follow(game: String, mut now: watch::Receiver<Now>) {
    loop {
        let current = now.borrow_and_update().clone();
        live().send_replace(Live {
            playing: true,
            game: game.clone(),
            now: current,
        });
        if now.changed().await.is_err() {
            break;
        }
    }
    live().send_modify(|live| live.playing = false);
}

/// What the feed sends every viewer.
#[derive(Debug, Clone)]
enum Update {
    Screen(u64, Vec<u8>),
    Seen(Box<Seen>),
    Live(Box<Live>),
}

#[derive(Clone)]
struct Shared {
    updates: broadcast::Sender<Update>,
    seen: watch::Sender<Seen>,
}

/// Serves the page at `bind` until `shutdown`.
pub async fn serve(bind: &str, shutdown: CancellationToken) -> anyhow::Result<()> {
    let address: SocketAddr = bind
        .parse()
        .map_err(|err| anyhow::anyhow!("web_bind {bind:?} isn't an address: {err}"))?;
    let (updates, _) = broadcast::channel(BACKLOG);
    let shared = Shared {
        updates,
        seen: watch::Sender::new(Seen::default()),
    };
    tokio::spawn(feed(shared.clone(), shutdown.clone()));
    let app = Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route("/live", get(upgrade))
        .route("/state.json", get(state))
        .with_state(shared);
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!("the Game Boy web page is at http://{address}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
    Ok(())
}

async fn state(State(shared): State<Shared>) -> impl IntoResponse {
    let seen = shared.seen.borrow().clone();
    let live = live().borrow().clone();
    Json(serde_json::json!({ "seen": seen, "live": live }))
}

async fn upgrade(ws: WebSocketUpgrade, State(shared): State<Shared>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| watch_live(socket, shared))
}

/// One viewer: what there is now, then every update.
async fn watch_live(mut socket: WebSocket, shared: Shared) {
    let mut updates = shared.updates.subscribe();
    let mut first = vec![
        Update::Live(Box::new(live().borrow().clone())),
        Update::Seen(Box::new(shared.seen.borrow().clone())),
    ];
    if let Some((mark, png)) = last_screen().await {
        first.push(Update::Screen(mark, png));
    }
    for update in first {
        if socket.send(message(&update)).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            update = updates.recv() => match update {
                Ok(update) => {
                    if socket.send(message(&update)).await.is_err() {
                        return;
                    }
                }
                // It fell behind and missed some: it skips ahead, with what code saw and
                // the dashboard as they are now, in case those were among them.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let now = [
                        Update::Live(Box::new(live().borrow().clone())),
                        Update::Seen(Box::new(shared.seen.borrow().clone())),
                    ];
                    for update in now {
                        if socket.send(message(&update)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
        }
    }
}

fn message(update: &Update) -> Message {
    match update {
        Update::Screen(mark, png) => {
            let mut bytes = mark.to_le_bytes().to_vec();
            bytes.extend_from_slice(png);
            Message::Binary(bytes.into())
        }
        Update::Seen(seen) => Message::Text(json("seen", seen).into()),
        Update::Live(live) => Message::Text(json("live", live).into()),
    }
}

fn json<T: Serialize>(kind: &str, value: &T) -> String {
    serde_json::json!({ "kind": kind, "value": value }).to_string()
}

/// Reads the Game Boy for the viewers: new screens as they're recorded, her look when the
/// screen settles, and the dashboard's `Now` when it changes. Idle while nobody watches.
async fn feed(shared: Shared, shutdown: CancellationToken) {
    let mut mark = None;
    let mut looked_at = None;
    let mut live_now = live().subscribe();
    loop {
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(CHECK_EVERY)) => {}
            () = shutdown.cancelled() => return,
        }
        if shared.updates.receiver_count() == 0 {
            mark = None;
            continue;
        }
        if live_now.has_changed().unwrap_or(false) {
            let current = live_now.borrow_and_update().clone();
            let _ = shared.updates.send(Update::Live(Box::new(current)));
        }
        let Some((recorded, screens)) = new_screens(mark).await else {
            continue;
        };
        mark = Some(recorded);
        for (i, png) in screens.iter().enumerate() {
            let at = recorded - (screens.len() - 1 - i) as u64;
            let _ = shared.updates.send(Update::Screen(at, png.clone()));
        }
        // Look once the screen has stopped changing for a check (between her actions),
        // the way she sees it when she decides.
        if screens.is_empty() && looked_at != Some(recorded) {
            looked_at = Some(recorded);
            if let Some(seen) = look(recorded).await
                && *shared.seen.borrow() != seen
            {
                shared.seen.send_replace(seen.clone());
                let _ = shared.updates.send(Update::Seen(Box::new(seen)));
            }
        }
    }
}

/// The screens recorded since `mark` (all that are kept, the first time) as PNGs, with how
/// many had been recorded. The PNGs are made off the Game Boy's thread.
async fn new_screens(mark: Option<u64>) -> Option<(u64, Vec<Vec<u8>>)> {
    let (recorded, screens) = console()
        .with(move |machine| {
            let m = machine.as_ref()?;
            let since = mark.unwrap_or(m.recorded().saturating_sub(1));
            Some((m.recorded(), m.screens_since(since)))
        })
        .await
        .ok()
        .flatten()?;
    let pngs = tokio::task::spawn_blocking(move || {
        screens
            .iter()
            .filter_map(|screen| pictures::png_at(screen, 1).ok())
            .collect::<Vec<_>>()
    })
    .await
    .ok()?;
    Some((recorded, pngs))
}

/// The newest screen as a PNG, for a viewer who just came.
async fn last_screen() -> Option<(u64, Vec<u8>)> {
    let (recorded, screen) = console()
        .with(|machine| {
            let m = machine.as_ref()?;
            Some((
                m.recorded(),
                m.screens_since(m.recorded().saturating_sub(1)).pop()?,
            ))
        })
        .await
        .ok()
        .flatten()?;
    let png = tokio::task::spawn_blocking(move || pictures::png_at(&screen, 1))
        .await
        .ok()?
        .ok()?;
    Some((recorded, png))
}

/// Looks at the game the way her actions do, and gathers what to draw.
async fn look(mark: u64) -> Option<Seen> {
    console()
        .with(move |machine| {
            let m = machine.as_mut()?;
            let look = look_for(m.emulator.info())?;
            // A copy, so watching never changes what she has seen: the page only looks.
            let mut world = m.world.clone();
            let view = look(&mut world, m);
            Some(seen(&mut world, view, mark))
        })
        .await
        .ok()
        .flatten()
}

fn seen(world: &mut super::world::World, view: super::world::View, mark: u64) -> Seen {
    let map = world.area(&view.area);
    let at = view.at;
    let on_screen = view
        .grid
        .map(|grid| {
            (0..grid.down)
                .map(|j| {
                    (0..grid.across)
                        .map(|i| {
                            let pos = (at.0 + i - grid.player.0, at.1 + j - grid.player.1);
                            map.letter(pos, at)
                        })
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default();
    let (map_from, rows) = match map.bounds() {
        Some((from, to)) => {
            let (from, to) = (
                (from.0.min(at.0), from.1.min(at.1)),
                (to.0.max(at.0), to.1.max(at.1)),
            );
            let rows = (from.1..=to.1)
                .map(|y| (from.0..=to.0).map(|x| map.letter((x, y), at)).collect())
                .collect();
            (from, rows)
        }
        None => (at, Vec::new()),
    };
    Seen {
        mark,
        area: view.area,
        at,
        can_walk: view.can_walk,
        text: view.text,
        facts: view
            .facts
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(String::from)
            .collect(),
        grid: view.grid,
        on_screen,
        map_from,
        map: rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::map::Kind;
    use crate::features::games::gameboy::world::{View, World};

    #[test]
    fn what_code_sees_is_the_steps_on_screen_and_the_area_so_far() {
        let mut world = World::default();
        let map = world.area("Route 1");
        map.see((5, 5), Kind::Floor);
        map.see((6, 5), Kind::Wall);
        map.see((5, 4), Kind::Grass);
        let view = View {
            area: "Route 1".to_string(),
            at: (5, 5),
            can_walk: true,
            text: String::new(),
            facts: "Money: ¥3175\n\nBadges: none".to_string(),
            grid: Some(Grid {
                pixels: 16,
                across: 3,
                down: 2,
                player: (1, 1),
            }),
        };
        let seen = seen(&mut world, view, 42);
        assert_eq!(seen.mark, 42);
        assert_eq!(seen.on_screen, vec!["?\"?", "?@#"]);
        assert_eq!(seen.map_from, (5, 4));
        assert_eq!(seen.map, vec!["\"?", "@#"]);
        assert_eq!(seen.facts, vec!["Money: ¥3175", "Badges: none"]);
    }

    #[test]
    fn a_screen_goes_out_after_its_number() {
        let Message::Binary(bytes) = message(&Update::Screen(7, vec![1, 2])) else {
            panic!("not binary");
        };
        assert_eq!(&bytes[..], &[7, 0, 0, 0, 0, 0, 0, 0, 1, 2]);
        let Message::Text(text) = message(&Update::Live(Box::default())) else {
            panic!("not text");
        };
        assert!(text.starts_with(r#"{"kind":"live","value":{"#), "{text}");
    }
}
