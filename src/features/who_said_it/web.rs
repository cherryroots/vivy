//! The web server the Activity window talks to. Discord shows the Activity as a web page
//! in an iframe, and every request it makes goes through Discord's proxy to this server
//! (the URL mappings in the Developer Portal say where it is).
//!
//! - `GET /` and the files next to it: the page itself (`page/`, built into the binary)
//! - `POST /api/token`: swaps the code from Discord's login for an access token. It needs
//!   the client secret, which must never be in the page.
//! - `GET /ws`: a WebSocket per player. The first message is the access token and the
//!   instance ID; after that, views go out and picks come in.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use serenity::all::{ChannelId, GuildId, UserId};
use tracing::{info, warn};

use super::Settings;
use super::game::Person;
use super::quotes;
use super::room::{Input, Place, Rooms};
use crate::core::{BotCtx, user_error};

const API: &str = "https://discord.com/api/v10";

/// What every request handler shares.
struct App {
    ctx: BotCtx,
    settings: Arc<Settings>,
    client_id: u64,
    client_secret: String,
    rooms: Arc<Rooms>,
}

/// Serves the Activity until the bot shuts down.
pub async fn serve(
    ctx: BotCtx,
    settings: Arc<Settings>,
    client_id: u64,
    client_secret: String,
) -> anyhow::Result<()> {
    let addr: SocketAddr = settings
        .listen
        .parse()
        .with_context(|| format!("`listen` isn't an address and port: {}", settings.listen))?;
    let shutdown = ctx.shutdown.clone();
    let app = Arc::new(App {
        ctx,
        settings,
        client_id,
        client_secret,
        rooms: Arc::default(),
    });
    let router = Router::new()
        .route("/", get(index))
        .route(
            "/app.js",
            get(|| file("text/javascript", include_str!("page/app.js"))),
        )
        .route(
            "/sdk.js",
            get(|| file("text/javascript", include_str!("page/sdk.js"))),
        )
        .route(
            "/style.css",
            get(|| file("text/css", include_str!("page/style.css"))),
        )
        .route("/api/token", post(token))
        .route("/ws", get(websocket))
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("listening on {addr}"))?;
    info!("Who said it? is served on http://{addr}");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await
        .context("serving the Activity")
}

/// The page, with the application's ID filled in for the SDK.
async fn index(State(app): State<Arc<App>>) -> Html<String> {
    Html(include_str!("page/index.html").replace("{{CLIENT_ID}}", &app.client_id.to_string()))
}

async fn file(kind: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, kind)], body).into_response()
}

#[derive(Deserialize)]
struct TokenRequest {
    code: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Step two of the login: the page got a one-time code from Discord, and this swaps it
/// for an access token using the client secret.
async fn token(
    State(app): State<Arc<App>>,
    axum::Json(request): axum::Json<TokenRequest>,
) -> Response {
    let client_id = app.client_id.to_string();
    let form = [
        ("client_id", client_id.as_str()),
        ("client_secret", app.client_secret.as_str()),
        ("grant_type", "authorization_code"),
        ("code", request.code.as_str()),
    ];
    let result = async {
        let response = app
            .ctx
            .web
            .post(format!("{API}/oauth2/token"))
            .form(&form)
            .send()
            .await?
            .error_for_status()?;
        anyhow::Ok(response.json::<TokenResponse>().await?)
    }
    .await;
    match result {
        Ok(token) => axum::Json(json!({ "access_token": token.access_token })).into_response(),
        Err(err) => {
            warn!("Who said it? login failed: {err:#}");
            (StatusCode::BAD_GATEWAY, "login failed").into_response()
        }
    }
}

async fn websocket(State(app): State<Arc<App>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| player(app, socket))
}

/// The page's first message on the WebSocket.
#[derive(Deserialize)]
struct Hello {
    access_token: String,
    instance_id: String,
}

/// What the page sends during a game.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Action {
    Start,
    Pick { choice: String },
}

/// One player's connection, from hello to goodbye.
async fn player(app: Arc<App>, mut socket: WebSocket) {
    let hello = match tokio::time::timeout(Duration::from_secs(15), socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<Hello>(&text).ok(),
        _ => None,
    };
    let Some(hello) = hello else {
        return;
    };
    let (person, place) = match check(&app, &hello).await {
        Ok(found) => found,
        Err(err) => {
            let message = crate::core::errors::user_message(&err);
            if !crate::core::errors::is_user_error(&err) {
                warn!("Who said it? couldn't let someone in: {err:#}");
            }
            let text = json!({ "type": "error", "message": message }).to_string();
            let _ = socket.send(Message::Text(text.into())).await;
            return;
        }
    };
    let me = person.id;
    let welcome = json!({ "type": "welcome", "you": me.to_string() }).to_string();
    if socket.send(Message::Text(welcome.into())).await.is_err() {
        return;
    }
    let Some(room) = app
        .rooms
        .join(&app.ctx, &app.settings, &hello.instance_id, place, person)
        .await
    else {
        return;
    };
    let mut view = room.view.clone();
    // The room's first view may not be there yet; `changed` below sends it when it is.
    view.mark_changed();

    loop {
        tokio::select! {
            () = app.ctx.shutdown.cancelled() => break,
            changed = view.changed() => {
                if changed.is_err() {
                    break;
                }
                let text = view.borrow_and_update().clone();
                if !text.is_empty() && socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => {
                    let input = match serde_json::from_str::<Action>(&text) {
                        Ok(Action::Start) => Input::Start,
                        Ok(Action::Pick { choice }) => match choice.parse() {
                            Ok(choice) => Input::Pick { player: me, choice },
                            Err(_) => continue,
                        },
                        Err(_) => continue,
                    };
                    if room.inputs.send(input).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                // Pings are answered by axum itself.
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = room.inputs.send(Input::Leave(me)).await;
}

#[derive(Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
    global_name: Option<String>,
}

#[derive(Deserialize)]
struct Instance {
    location: Location,
    /// The users who have the Activity open.
    users: Vec<String>,
}

#[derive(Deserialize)]
struct Location {
    channel_id: String,
    guild_id: Option<String>,
}

/// Who is connecting, and where: the access token says who they are, and Discord's record
/// of the instance says which channel it's in and that they're really in it. The page's
/// word is never taken for either, so nobody can play with another channel's messages.
async fn check(app: &App, hello: &Hello) -> anyhow::Result<(Person, Place)> {
    let user: DiscordUser = app
        .ctx
        .web
        .get(format!("{API}/users/@me"))
        .bearer_auth(&hello.access_token)
        .send()
        .await?
        .error_for_status()
        .context("reading who logged in")?
        .json()
        .await?;
    let user_id: u64 = user.id.parse()?;

    // Discord can take a moment to list someone who just opened the Activity.
    let mut instance = read_instance(app, &hello.instance_id).await?;
    if !instance.users.contains(&user.id) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        instance = read_instance(app, &hello.instance_id).await?;
    }
    if !instance.users.contains(&user.id) {
        return Err(user_error("You aren't in this Activity."));
    }
    let Some(guild) = instance.location.guild_id else {
        return Err(user_error("Who said it? only works in servers."));
    };
    let guild = GuildId::new(guild.parse()?);
    let channel = ChannelId::new(instance.location.channel_id.parse()?);
    if !app.ctx.allows("who_said_it", Some(guild), channel) {
        return Err(user_error("Who said it? is turned off in this channel."));
    }

    let fallback = Person {
        id: user_id,
        name: user.global_name.unwrap_or(user.username),
        avatar: None,
    };
    let person = quotes::member(&app.ctx, guild, UserId::new(user_id))
        .await
        .unwrap_or(fallback);
    Ok((person, Place { guild, channel }))
}

/// Discord's record of an Activity instance, read with the bot's token.
async fn read_instance(app: &App, instance: &str) -> anyhow::Result<Instance> {
    let mut url = reqwest::Url::parse(&format!(
        "{API}/applications/{}/activity-instances",
        app.client_id
    ))?;
    // Added as one path segment, so an odd instance ID can't point somewhere else.
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("bad API URL"))?
        .push(instance);
    let response = app
        .ctx
        .web
        .get(url)
        .header(header::AUTHORIZATION.as_str(), app.ctx.http.token())
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(user_error("This Activity has ended. Open it again."));
    }
    Ok(response
        .error_for_status()
        .context("reading the Activity instance")?
        .json()
        .await?)
}
