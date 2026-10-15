//! Pictures during the Games, when the host turns them on in the Reaping: a map of the
//! arena after every round, with each event where it happened, and the final showdown with
//! the victor. Vivy writes what happens on the map with the round (`picture` in her JSON);
//! `looks.rs` adds the arena's layout and how each tribute looks, and [`STYLE`] keeps the
//! pictures of a game looking alike.
//!
//! Like `voice.rs`, this doesn't import the drawing feature: it finds whichever feature
//! offers the `draw_picture` chat tool and runs it with arguments the chat model isn't told
//! about (no reply, a caption, a spoiler, and `game_art` as the job, so their cost has its
//! own line in OpenAI's spend). The host is the asker, so the pictures count toward the
//! host's monthly pictures and the server's.
//! Drawing takes about 15 to 20 seconds, so it runs in the background: the game never
//! waits for a picture, and one that fails is simply missing.

use serde_json::json;
use serenity::all::{ChannelId, GuildId, MessageId, UserId};
use tracing::{debug, info};

use crate::core::{Asker, BotCtx};

/// The drawing feature's tool.
const TOOL: &str = "draw_picture";
/// Added to every prompt, so a game's pictures share one style.
const STYLE: &str = "Style: a clean top-down 2D map of the whole arena, like a retro 16-bit strategy game: every zone clearly bordered and labelled, the tributes as small, colorful pixel-art sprites in the zones where they are, fights shown as sprites clashing with simple action marks. Easy to read at a glance. No blood or gore, and no words or letters except the zone labels, written exactly as given.";

/// One picture to draw.
pub struct Picture {
    /// The map and her prompt, with the tributes' looks filled in.
    pub prompt: String,
    /// The line above it, like "🎨 Day 2".
    pub caption: String,
    /// Behind a spoiler, for a picture that gives away who won.
    pub spoiler: bool,
}

/// Whether pictures can be drawn in `channel`: drawing is on there, and its tool is
/// offered (it needs an OpenAI key).
pub fn can_draw(ctx: &BotCtx, guild: GuildId, channel: ChannelId) -> bool {
    ctx.allows("drawing", Some(guild), channel)
        && ctx
            .features
            .iter()
            .any(|f| f.tools().iter().any(|t| t.name == TOOL))
}

/// Draws `picture` and posts it in `channel`, in the background.
pub fn draw(
    ctx: &BotCtx,
    guild: GuildId,
    channel: ChannelId,
    message: MessageId,
    host: UserId,
    picture: Picture,
) {
    let ctx = ctx.clone();
    ctx.tasks.clone().spawn(async move {
        if !ctx.allows("drawing", Some(guild), channel) {
            return;
        }
        let Some(drawing) = ctx
            .features
            .iter()
            .find(|f| f.tools().iter().any(|t| t.name == TOOL))
        else {
            return;
        };
        let asker = Asker {
            user: host,
            guild: Some(guild),
            channel,
            message,
            posted: Default::default(),
        };
        let args = json!({
            "prompt": format!("{}\n\n{STYLE}", picture.prompt),
            "shape": "landscape",
            "reply": false,
            "caption": picture.caption,
            "spoiler": picture.spoiler,
            "job": "game_art",
        });
        match drawing.run_tool(&ctx, &asker, TOOL, &args).await {
            Ok(_) => info!(caption = picture.caption, "drew a Hunger Games picture"),
            Err(err) => debug!("couldn't draw a Hunger Games picture: {err:#}"),
        }
    });
}
