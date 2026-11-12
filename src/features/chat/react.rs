//! `react_to_message`: Vivy adds emoji reactions while she answers, the way people react
//! to a message besides writing back. Each is a plain emoji or one of the server's own (a
//! bare name works too, see [`emoji::reaction_code`]), up to [`MAX_EMOJI`] in one call,
//! added in order. By default they go on the message she's answering; a link picks another
//! message in the same channel. With `remove` she takes her own reactions back off.
//!
//! Chime-ins don't get this tool: they react with `REACT` instead (see `chime.rs`).

use serde_json::{Value, json};

use super::emoji;
use super::tools::parse_link;
use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Result, user_error};
use crate::util::discord::{MessageId, react, unreact};

pub const TOOL: &str = "react_to_message";

/// The most emoji one call adds or removes.
const MAX_EMOJI: usize = 3;

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL,
        description: "Adds emoji reactions to a message, like people do on Discord: to the message you're answering, or to another message in this channel by its link. Use it now and then when a reaction fits, as well as your answer in words, not instead of it. Each emoji can be a plain one (😂) or one of this server's own, written as its code from list_server_emoji (<:name:123>). Usually one is enough; up to 3 are added in order. Set remove to take your own reactions back off.",
        parameters: json!({
            "type": "object",
            "properties": {
                "emoji": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "The emoji, 1 to 3: each a plain one or a server emoji's code."
                },
                "link": {"type": "string", "description": "A link to the message to react to, in this channel. Default: the message you're answering."},
                "remove": {"type": "boolean", "description": "true takes these reactions of yours off the message instead. Default false."}
            },
            "required": ["emoji"],
        }),
    }
}

pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<String> {
    let picked = emoji_list(&args["emoji"]).map_err(user_error)?;
    let remove = args["remove"].as_bool().unwrap_or(false);
    let link = args["link"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let message = match link {
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

    // One line per emoji, so one that fails says why without stopping the rest.
    let codes = emoji::codes(ctx, asker.guild);
    let mut lines = Vec::new();
    let mut done = 0;
    for picked in picked {
        let emoji = emoji::reaction_code(&picked, &codes);
        let result = if remove {
            unreact(&ctx.http, asker.channel, message, &emoji).await
        } else {
            react(&ctx.http, asker.channel, message, &emoji).await
        };
        if result.is_ok() {
            done += 1;
        }
        lines.push(match (result, remove) {
            (Ok(()), false) => format!("Reacted with {emoji}."),
            (Ok(()), true) => format!("Took your {emoji} off."),
            (Err(err), false) => format!("Couldn't react with {emoji}: {err:#}"),
            (Err(err), true) => format!("Couldn't take {emoji} off: {err:#}"),
        });
    }
    let answer = lines.join("\n");
    if done == 0 {
        return Err(user_error(answer));
    }
    Ok(answer)
}

/// The emoji asked for: a list, or one string (tool calls saved before the list still
/// replay). Blank ones are skipped and repeats dropped; more than [`MAX_EMOJI`] is a
/// mistake rather than quietly cut, so she knows which ones didn't go on.
fn emoji_list(value: &Value) -> std::result::Result<Vec<String>, String> {
    let all: Vec<&str> = match value {
        Value::String(one) => vec![one.as_str()],
        Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let mut list: Vec<String> = Vec::new();
    for emoji in all.into_iter().map(str::trim).filter(|e| !e.is_empty()) {
        if !list.iter().any(|seen| seen == emoji) {
            list.push(emoji.to_string());
        }
    }
    if list.is_empty() {
        return Err("`emoji` is required.".to_string());
    }
    if list.len() > MAX_EMOJI {
        return Err(format!("At most {MAX_EMOJI} emoji in one call."));
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_string_still_works() {
        assert_eq!(emoji_list(&json!(" 😂 ")).unwrap(), ["😂"]);
    }

    #[test]
    fn a_list_keeps_its_order_without_blanks_or_repeats() {
        let list = emoji_list(&json!(["🐄", "", "<:pog:123>", "🐄", "😂"])).unwrap();
        assert_eq!(list, ["🐄", "<:pog:123>", "😂"]);
    }

    #[test]
    fn nothing_or_too_many_is_a_mistake() {
        assert!(emoji_list(&Value::Null).is_err());
        assert!(emoji_list(&json!([])).is_err());
        assert!(emoji_list(&json!(["  "])).is_err());
        assert!(emoji_list(&json!(["1️⃣", "2️⃣", "3️⃣", "4️⃣"])).is_err());
    }
}
