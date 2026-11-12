//! The `look_at_profile_picture` tool: shows the model someone's profile picture, or her
//! own. The picture goes to the model with the tool's answer, so it can see it the same way
//! it sees a picture in a message. Finding the person and the picture is in
//! `util/people.rs`, which drawing uses too.

use futures::future::join_all;
use serde_json::{Value, json};

use super::tools::{left_out_note, list_arg};
use crate::ai::{ToolDef, ToolReply};
use crate::core::{Asker, BotCtx, Result, user_error};
use crate::util::discord::UserId;
use crate::util::people::{Lookup, ProfilePicture, profile_picture};

pub const TOOL: &str = "look_at_profile_picture";

/// Discord makes the picture this many pixels wide, enough to see the details.
const SIZE: u32 = 512;
/// Most profile pictures one call shows (about 350 tokens each to Claude).
const MAX_PICTURES: usize = 4;

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL,
        description: "Shows you people's profile pictures, so you can see what they look like. Use `me` for your own: in a server that's your face there right now. Look at the picture before you describe or draw someone, yourself included, so you get their looks right. To compare pictures, look at them all in one call.",
        parameters: json!({
            "type": "object",
            "properties": {
                "users": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "1 to 4 people: mentions, user IDs or names, or `me` for yourself. Without it, the asker."
                }
            },
        }),
    }
}

pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<ToolReply> {
    let lookup = Lookup {
        bot: ctx.bot_id,
        guild: asker.guild,
        asker: asker.user,
    };
    // `users`, or `user` in older calls; nobody named means the asker.
    let named = list_arg(args, "users", "user");
    let queries: Vec<Option<&str>> = if named.is_empty() {
        vec![None]
    } else {
        named.iter().take(MAX_PICTURES).map(|q| Some(*q)).collect()
    };
    let found = join_all(
        queries
            .iter()
            .map(|query| profile_picture(&ctx.http, &lookup, *query, SIZE)),
    )
    .await;

    // One person answers as it always did: the picture, or the error.
    if queries.len() == 1 {
        let picture = found.into_iter().next().expect("one person")?;
        return Ok(ToolReply {
            text: describe(whose(ctx, &picture), picture.for_server),
            images: vec![picture.image],
            files: Vec::new(),
        });
    }
    let mut lines = Vec::new();
    let mut shown: Vec<UserId> = Vec::new();
    let mut images = Vec::new();
    for (query, result) in queries.iter().zip(found) {
        let query = query.unwrap_or_default();
        match result {
            // Asking for `me` and her name finds the same picture twice.
            Ok(picture) if shown.contains(&picture.user) => {
                let at = shown.iter().position(|&u| u == picture.user).unwrap_or(0) + 1;
                lines.push(format!("{query}: the same person as picture {at}."));
            }
            Ok(picture) => {
                let text = describe(whose(ctx, &picture), picture.for_server);
                shown.push(picture.user);
                images.push(picture.image);
                lines.push(format!("Picture {}: {text}", shown.len()));
            }
            Err(err) => lines.push(format!("{query}: Error: {err:#}")),
        }
    }
    if images.is_empty() {
        return Err(user_error(lines.join("\n")));
    }
    if images.len() > 1 {
        lines.push("The pictures are below, in this order.".to_string());
    }
    let mut text = lines.join("\n");
    text.push_str(&left_out_note(
        named.len(),
        MAX_PICTURES,
        "people were looked at",
    ));
    Ok(ToolReply {
        text,
        images,
        files: Vec::new(),
    })
}

/// The person's name for [`describe`], or `None` when it's her own picture.
fn whose<'a>(ctx: &BotCtx, picture: &'a ProfilePicture) -> Option<&'a str> {
    (picture.user != ctx.bot_id).then_some(picture.name.as_str())
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
