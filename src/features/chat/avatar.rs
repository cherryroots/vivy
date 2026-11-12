//! The `look_at_profile_picture` tool: shows the model someone's profile picture, or her
//! own. The picture goes to the model with the tool's answer, so it can see it the same way
//! it sees a picture in a message. Finding the person and the picture is in
//! `util/people.rs`, which drawing uses too.

use crate::ai::{ToolDef, ToolReply};
use crate::core::{Asker, BotCtx, Result};
use crate::util::people::{Lookup, profile_picture};
use serde_json::{Value, json};

pub const TOOL: &str = "look_at_profile_picture";

/// Discord makes the picture this many pixels wide, enough to see the details.
const SIZE: u32 = 512;

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL,
        description: "Shows you someone's profile picture, so you can see what it looks like. Use `me` for your own: in a server that's your face there right now. Look at the picture before you describe or draw someone, yourself included, so you get their looks right.",
        parameters: json!({
            "type": "object",
            "properties": {
                "user": {"type": "string", "description": "A mention, user ID or name, or `me` for yourself. Without it, the asker."}
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
    let picture = profile_picture(&ctx.http, &lookup, args["user"].as_str(), SIZE).await?;
    let whose = if picture.user == ctx.bot_id {
        None
    } else {
        Some(picture.name.as_str())
    };
    Ok(ToolReply {
        text: describe(whose, picture.for_server),
        images: vec![picture.image],
        files: Vec::new(),
    })
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
