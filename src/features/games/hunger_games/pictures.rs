//! Pictures during the Games, when the host turns them on in the Reaping: a map of the
//! arena after every round, with each event where it happened. Vivy writes what happens on
//! the map with the round (`picture` in her JSON); `looks.rs` adds the arena's layout and
//! how each tribute looks, and [`MAP`] keeps the maps of a game looking alike. The victor's
//! picture is made by the bot instead: the winner up close, with everyone they killed
//! lying dead around them ([`VICTORY`]).
//!
//! Like `voice.rs`, this doesn't import the drawing feature: it finds whichever feature
//! offers the `draw_picture` chat tool and runs it with arguments the chat model isn't told
//! about: `post: false`, so the picture comes back to be posted with its round, and
//! `game_art` as the job, so their cost has its own line in OpenAI's spend. The host is
//! the asker, so the pictures count toward the host's monthly pictures and the server's.
//! Games ask for OpenAI's less strict safety filter, since the host chose a game with
//! deaths in it. A picture that fails is missing, and [`draw`] says why, so the game can
//! tell the channel once.

use serde_json::json;
use serenity::all::{ChannelId, GuildId, MessageId, UserId};
use tracing::{info, warn};

use crate::core::{Asker, BotCtx};
use crate::util::media::ModelImage;

/// The drawing feature's tool.
const TOOL: &str = "draw_picture";
/// Added to each round's map, so a game's maps share one style.
pub const MAP: &str = "Style: a clean top-down 2D map of the whole battlefield, like a retro 16-bit strategy game: every zone clearly bordered and labelled, the players as colorful pixel-art sprites in the zones where they are, drawn a bit bigger than usual for a map so each one's colors and item are easy to make out, fights shown as sprites clashing with simple action marks. Easy to read at a glance. Deaths are shown plainly: the fallen lie dead with some pixel-art blood, nothing more graphic than that. No words or letters except the zone labels, written exactly as given.";
/// Added to the Octagon rounds: the map's pixel art, zoomed in on the cage in the middle.
pub const CAGE: &str = "Style: the same retro 16-bit pixel art as a top-down strategy game map, zoomed in close on the fighting cage in the middle so the players are large, detailed sprites mid-fight, with simple action marks. Deaths are shown plainly: the fallen lie dead with some pixel-art blood, nothing more graphic than that. No words or letters anywhere in the picture.";
/// Added to the victor's picture: the same pixel art, a full-size figure.
pub const VICTORY: &str = "Style: a detailed retro 16-bit pixel-art scene, like the victory screen of a classic game, with the winner as a large full-body figure and everything drawn clear. Deaths are shown plainly: the fallen lie dead with some pixel-art blood, nothing more graphic than that. No words or letters anywhere in the picture.";

/// Where a game's pictures are drawn: its server and channel, the Reaping message, and the
/// host, who asks for them.
#[derive(Clone, Copy)]
pub struct Easel {
    pub guild: GuildId,
    pub channel: ChannelId,
    pub message: MessageId,
    pub host: UserId,
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

/// Draws `prompt` (with the looks in) in `style` ([`MAP`], [`CAGE`] or [`VICTORY`]), without posting
/// it. When it can't be drawn, why, in a few words for the channel. Takes 15 to 20 seconds
/// or so.
pub async fn draw(
    ctx: &BotCtx,
    easel: Easel,
    prompt: &str,
    style: &str,
) -> Result<ModelImage, &'static str> {
    let drawing = ctx
        .features
        .iter()
        .find(|f| f.tools().iter().any(|t| t.name == TOOL))
        .filter(|_| can_draw(ctx, easel.guild, easel.channel))
        .ok_or("drawing is off here now")?;
    let asker = Asker {
        user: easel.host,
        guild: Some(easel.guild),
        channel: easel.channel,
        message: easel.message,
        posted: Default::default(),
    };
    let args = json!({
        "prompt": format!("{prompt}\n\n{style}"),
        "shape": "landscape",
        "post": false,
        "job": "game_art",
    });
    match drawing
        .run_tool_with_pictures(ctx, &asker, TOOL, &args)
        .await
    {
        Ok(reply) => {
            // The whole prompt, to see what a strange picture was asked for.
            info!(prompt, "drew a Hunger Games picture");
            reply.images.into_iter().next().ok_or("the drawing failed")
        }
        Err(err) => {
            warn!("couldn't draw a Hunger Games picture: {err:#}");
            Err(why(&format!("{err:#}")))
        }
    }
}

/// Why a picture wasn't drawn, from the drawing tool's error, in a few words for people.
fn why(error: &str) -> &'static str {
    if error.contains("pictures are used up") || error.contains("pictures for this month") {
        "this month's pictures are used up"
    } else if error.contains("won't draw that") {
        "OpenAI's safety filter turned it down"
    } else {
        "the drawing failed"
    }
}

/// A file name for a picture: `arena.png`, or the picture's own kind.
pub fn file_name(picture: &ModelImage) -> String {
    let extension = picture.mime.strip_prefix("image/").unwrap_or("png");
    format!("arena.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_why_a_picture_is_missing() {
        assert_eq!(
            why("the month's pictures are used up (100 of 100, all servers together)"),
            "this month's pictures are used up"
        );
        assert_eq!(
            why("the asker has had their 20 pictures for this month (20 drawn)"),
            "this month's pictures are used up"
        );
        assert_eq!(
            why("OpenAI won't draw that: Your request was rejected by the safety system."),
            "OpenAI's safety filter turned it down"
        );
        assert_eq!(why("OpenAI answered 500: oops"), "the drawing failed");
    }
}
