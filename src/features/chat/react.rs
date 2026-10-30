//! `react_to_message`: Vivy adds an emoji reaction while she answers, the way people react
//! to a message besides writing back. A plain emoji or one of the server's own (a bare
//! name works too, see [`emoji::reaction_code`]). By default it goes on the message she's
//! answering; a link picks another message in the same channel.
//!
//! Chime-ins don't get this tool: they react with `REACT` instead (see `chime.rs`).

use serde_json::{Value, json};

use super::emoji;
use super::tools::parse_link;
use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Result, user_error};
use crate::util::discord::{MessageId, react};

pub const TOOL: &str = "react_to_message";

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL,
        description: "Adds an emoji reaction to a message, like people do on Discord: to the message you're answering, or to another message in this channel by its link. Use it now and then when a reaction fits, as well as your answer in words, not instead of it. The emoji can be a plain one (😂) or one of this server's own, written as its code from list_server_emoji (<:name:123>).",
        parameters: json!({
            "type": "object",
            "properties": {
                "emoji": {"type": "string", "description": "One emoji: a plain one, or a server emoji's code."},
                "link": {"type": "string", "description": "A link to the message to react to, in this channel. Default: the message you're answering."}
            },
            "required": ["emoji"],
        }),
    }
}

pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<String> {
    let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    let picked = text("emoji").ok_or_else(|| user_error("`emoji` is required."))?;
    let message = match text("link") {
        None => asker.message,
        Some(link) => {
            let (_, channel, message) =
                parse_link(link).ok_or_else(|| user_error("That isn't a Discord message link."))?;
            if channel != asker.channel.get() {
                return Err(user_error(
                    "You can only react to messages in this channel.",
                ));
            }
            MessageId::new(message)
        }
    };
    let codes = emoji::codes(ctx, asker.guild);
    let emoji = emoji::reaction_code(picked, &codes);
    react(&ctx.http, asker.channel, message, &emoji)
        .await
        .map_err(|err| user_error(format!("Couldn't react with {emoji}: {err:#}")))?;
    Ok(format!("Reacted with {emoji}."))
}
