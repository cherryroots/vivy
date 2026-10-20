//! The `look_at_profile_picture` tool: shows the model someone's profile picture, or her
//! own. The picture goes to the model with the tool's answer, so it can see it the same way
//! it sees a picture in a message.
//!
//! In a server it uses the person's picture for that server when they have one, otherwise
//! their main one. Her own server picture is the face the mood check picked, so "me" shows
//! her as she looks right now.

use super::tools::{find_member, parse_user_id};
use crate::ai::{ToolDef, ToolReply};
use crate::core::{Asker, BotCtx, Result, user_error};
use crate::util::discord::{
    avatar_url, display_name, get_member, get_user, member_avatar_url, member_display_name,
};
use crate::util::media::{ModelImage, download, png_link};
use serde_json::{Value, json};

pub const TOOL: &str = "look_at_profile_picture";

/// Profile pictures are small; anything bigger than this isn't one.
const MAX_BYTES: usize = 8 * 1024 * 1024;
/// Discord makes the picture this many pixels wide, enough to see the details.
const SIZE: u32 = 512;

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL,
        description: "Shows you someone's profile picture, so you can see what it looks like. Use `me` for your own: in a server that's your face there right now. Look at the picture before you describe or draw someone, yourself included, so you get their looks right. Later in the conversation only the text of this answer is kept, so look again when you need the picture.",
        parameters: json!({
            "type": "object",
            "properties": {
                "user": {"type": "string", "description": "A mention, user ID or name, or `me` for yourself. Without it, the asker."}
            },
        }),
    }
}

pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<ToolReply> {
    let query = args["user"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let query = match query {
        Some(word) if is_herself(word) => Some(ctx.bot_id.to_string()),
        other => other.map(str::to_string),
    };

    // (who it is, the picture's link, whether it's their picture for this server)
    let (user_id, name, url, for_server) = match asker.guild {
        Some(guild) => {
            let member = match &query {
                None => get_member(&ctx.http, guild, asker.user).await?,
                Some(query) => find_member(ctx, guild, query).await?,
            };
            let (url, for_server) = match member_avatar_url(guild, member.user.id, member.avatar) {
                Some(url) => (url, true),
                None => (avatar_url(&member.user), false),
            };
            (
                member.user.id,
                member_display_name(&member).to_string(),
                url,
                for_server,
            )
        }
        // A DM has no member list to search by name.
        None => {
            let id = match &query {
                None => asker.user,
                Some(query) => parse_user_id(query).ok_or_else(|| {
                    user_error("In a DM I can only find people by mention or user ID.")
                })?,
            };
            let user = get_user(&ctx.http, id)
                .await
                .map_err(|_| user_error(format!("There's nobody with ID {id}.")))?;
            (
                user.id,
                display_name(&user).to_string(),
                avatar_url(&user),
                false,
            )
        }
    };

    let data = download(&png_link(&url, SIZE), MAX_BYTES).await?;
    let whose = if user_id == ctx.bot_id {
        None
    } else {
        Some(name.as_str())
    };
    Ok(ToolReply {
        text: describe(whose, for_server),
        images: vec![ModelImage {
            mime: "image/png",
            data,
        }],
    })
}

/// The words a model might use for itself.
fn is_herself(query: &str) -> bool {
    matches!(
        query.to_lowercase().as_str(),
        "me" | "myself" | "you" | "yourself" | "self"
    )
}

/// The tool's text. `whose` is the person's name, or `None` for her own picture.
fn describe(whose: Option<&str>, for_server: bool) -> String {
    let which = if for_server {
        "picture for this server"
    } else {
        "profile picture"
    };
    match whose {
        None => format!("This is your own {which}, as people see you."),
        Some(name) => format!("This is {name}'s {which}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_when_she_means_herself() {
        assert!(is_herself("me"));
        assert!(is_herself("Yourself"));
        assert!(!is_herself("Cherry"));
    }

    #[test]
    fn says_whose_picture_it_is() {
        assert_eq!(
            describe(None, true),
            "This is your own picture for this server, as people see you."
        );
        assert_eq!(
            describe(Some("Cherry"), false),
            "This is Cherry's profile picture."
        );
    }
}
