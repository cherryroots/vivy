//! Pictures during the Games, when the host turns them on in the Reaping: the arena as the
//! Games open, the most dramatic moment of each day, and the victor's triumph, drawn from
//! their profile picture. Vivy writes each picture's prompt with the round (`picture` in
//! her JSON); [`STYLE`] keeps the pictures of a game looking alike.
//!
//! Like `voice.rs`, this doesn't import the drawing feature: it finds whichever feature
//! offers the `draw_picture` chat tool and runs it with arguments the chat model isn't told
//! about (no reply, a caption, a spoiler, a profile picture to draw from). The host is the
//! asker, so the pictures count toward the host's monthly pictures and the server's.
//! Drawing takes about 15 to 20 seconds, so it runs in the background: the game never
//! waits for a picture, and one that fails is simply missing.

use serde_json::json;
use serenity::all::{ChannelId, GuildId, MessageId, UserId};
use tracing::{debug, info};

use crate::core::{Asker, BotCtx};

/// The drawing feature's tool.
const TOOL: &str = "draw_picture";
/// Added to every prompt, so a game's pictures share one style.
const STYLE: &str = "Style: a bright, colorful cartoon illustration, like a storybook about a playful battle royale, wide shot. No blood or gore, and no words or letters anywhere in the picture.";

/// One picture to draw.
pub struct Picture {
    /// Her prompt.
    pub prompt: String,
    /// The line above it, like "🎨 Day 2".
    pub caption: String,
    /// Someone whose profile picture is sent along to draw them from (the victor).
    pub of: Option<u64>,
    /// Behind a spoiler, for a picture that gives away who won.
    pub spoiler: bool,
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
        let mut args = json!({
            "prompt": format!("{}\n\n{STYLE}", picture.prompt),
            "shape": "landscape",
            "reply": false,
            "caption": picture.caption,
            "spoiler": picture.spoiler,
        });
        if let Some(of) = picture.of {
            args["from_avatars"] = json!([of.to_string()]);
        }
        match drawing.run_tool(&ctx, &asker, TOOL, &args).await {
            Ok(_) => info!(caption = picture.caption, "drew a Hunger Games picture"),
            Err(err) => debug!("couldn't draw a Hunger Games picture: {err:#}"),
        }
    });
}
