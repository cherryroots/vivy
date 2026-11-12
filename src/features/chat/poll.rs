//! `create_poll`: Vivy posts a real Discord poll when someone asks for one ("make a poll
//! for movie night, Friday or Saturday").
//!
//! The poll is its own message in the channel she's answering in. Twilight's `Poll` type has
//! an `expiry` time, but creating a poll takes a `duration` in hours, so the message is sent
//! as hand-written JSON with `send_raw`. [`payload`] builds that JSON from the tool's
//! arguments and checks Discord's limits, so it's tested without Discord.
//!
//! Chime-ins never get this tool (it isn't in `UNPROMPTED_TOOLS`): a poll only appears when
//! someone asked for it.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::emoji;
use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Result, user_error};
use crate::util::discord::{message_link, send_raw};

pub const TOOL: &str = "create_poll";

/// Discord's limits for a poll.
const MAX_QUESTION: usize = 300;
const MAX_ANSWER: usize = 55;
const MIN_ANSWERS: usize = 2;
const MAX_ANSWERS: usize = 10;
const MAX_HOURS: u64 = 768;
/// How long a poll runs when she doesn't say.
const DEFAULT_HOURS: u64 = 24;

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL,
        description: "Posts a Discord poll in this channel, as its own message. Use it only when someone asks for a poll or a vote. Keep the question and answers short. Each answer can have an emoji: a plain one (🍿) or one of this server's own, as its code from list_server_emoji. Say in your answer that you made it; the result has its link.",
        parameters: json!({
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "The question, at most 300 characters."},
                "answers": {
                    "type": "array",
                    "description": "2 to 10 answers.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": {"type": "string", "description": "At most 55 characters."},
                            "emoji": {"type": "string", "description": "Optional."}
                        },
                        "required": ["text"]
                    }
                },
                "hours": {"type": "integer", "description": "How long people can vote, 1 to 768 hours (32 days). Default 24."},
                "multiple": {"type": "boolean", "description": "Whether people can pick more than one answer. Default false."}
            },
            "required": ["question", "answers"],
        }),
    }
}

pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<String> {
    let codes = emoji::codes(ctx, asker.guild);
    let payload = payload(args, &codes).map_err(user_error)?;
    let posted = send_raw(&ctx.http, asker.channel, &payload, &[])
        .await
        .map_err(|err| {
            user_error(format!(
                "Discord didn't take the poll: {err:#}. I may lack the Create Polls permission here."
            ))
        })?;
    let hours = payload["poll"]["duration"]
        .as_u64()
        .unwrap_or(DEFAULT_HOURS);
    Ok(format!(
        "Posted the poll, open for {hours} hours: {}",
        message_link(asker.guild, asker.channel, posted.id)
    ))
}

/// Discord's JSON for the poll message, from the tool's arguments. An error says what to
/// fix, for the model. `codes` turns server emoji names into their codes.
fn payload(args: &Value, codes: &HashMap<String, String>) -> std::result::Result<Value, String> {
    let question = args["question"].as_str().map(str::trim).unwrap_or_default();
    if question.is_empty() {
        return Err("The poll needs a question.".into());
    }
    if question.chars().count() > MAX_QUESTION {
        return Err(format!("The question is over {MAX_QUESTION} characters."));
    }
    let given = args["answers"].as_array().cloned().unwrap_or_default();
    if !(MIN_ANSWERS..=MAX_ANSWERS).contains(&given.len()) {
        return Err(format!(
            "A poll takes {MIN_ANSWERS} to {MAX_ANSWERS} answers, not {}.",
            given.len()
        ));
    }
    let mut answers = Vec::new();
    for answer in &given {
        // A plain string works too.
        let (text, emoji) = match answer {
            Value::String(text) => (text.trim(), None),
            _ => (
                answer["text"].as_str().map(str::trim).unwrap_or_default(),
                answer["emoji"]
                    .as_str()
                    .map(str::trim)
                    .filter(|e| !e.is_empty()),
            ),
        };
        if text.is_empty() {
            return Err("Every answer needs text.".into());
        }
        if text.chars().count() > MAX_ANSWER {
            return Err(format!(
                "\"{text}\" is over {MAX_ANSWER} characters; shorten it."
            ));
        }
        let mut media = json!({"text": text});
        if let Some(emoji) = emoji {
            media["emoji"] = poll_emoji(&emoji::reaction_code(emoji, codes));
        }
        answers.push(json!({"poll_media": media}));
    }
    let hours = args["hours"]
        .as_u64()
        .unwrap_or(DEFAULT_HOURS)
        .clamp(1, MAX_HOURS);
    Ok(json!({
        "poll": {
            "question": {"text": question},
            "answers": answers,
            "duration": hours,
            "allow_multiselect": args["multiple"].as_bool().unwrap_or(false),
            "layout_type": 1,
        },
        "allowed_mentions": {"parse": []},
    }))
}

/// A poll answer's emoji: `{"id": ...}` for a server emoji's code (`<:name:123>`),
/// `{"name": ...}` for a plain one.
fn poll_emoji(emoji: &str) -> Value {
    let inner = emoji.trim_start_matches('<').trim_end_matches('>');
    if emoji.starts_with('<')
        && let Some((_, id)) = inner.rsplit_once(':')
        && id.parse::<u64>().is_ok()
    {
        return json!({"id": id});
    }
    json!({"name": emoji})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes() -> HashMap<String, String> {
        HashMap::from([("pog".to_string(), "<:pog:123>".to_string())])
    }

    #[test]
    fn builds_a_poll() {
        let args = json!({
            "question": "Movie night?",
            "answers": [{"text": "Friday", "emoji": "🍿"}, {"text": "Saturday", "emoji": "pog"}, "Sunday"],
            "hours": 48,
            "multiple": true,
        });
        let poll = &payload(&args, &codes()).unwrap()["poll"];
        assert_eq!(poll["question"]["text"], "Movie night?");
        assert_eq!(poll["duration"], 48);
        assert_eq!(poll["allow_multiselect"], true);
        let answers = poll["answers"].as_array().unwrap();
        assert_eq!(answers.len(), 3);
        assert_eq!(answers[0]["poll_media"]["emoji"], json!({"name": "🍿"}));
        assert_eq!(answers[1]["poll_media"]["emoji"], json!({"id": "123"}));
        assert_eq!(answers[2]["poll_media"], json!({"text": "Sunday"}));
    }

    #[test]
    fn defaults_and_clamps_the_duration() {
        let args = |hours: Value| json!({"question": "Q", "answers": ["a", "b"], "hours": hours});
        let duration = |args: Value| payload(&args, &codes()).unwrap()["poll"]["duration"].clone();
        assert_eq!(duration(args(Value::Null)), 24);
        assert_eq!(duration(args(json!(5000))), 768);
        assert_eq!(duration(args(json!(0))), 1);
    }

    #[test]
    fn refuses_what_discord_would() {
        let refused = |args: Value| payload(&args, &codes()).is_err();
        assert!(refused(json!({"question": "", "answers": ["a", "b"]})));
        assert!(refused(json!({"question": "Q", "answers": ["only one"]})));
        assert!(refused(json!({"question": "Q", "answers": vec!["x"; 11]})));
        assert!(refused(
            json!({"question": "Q", "answers": ["a", "x".repeat(56)]})
        ));
        assert!(refused(
            json!({"question": "x".repeat(301), "answers": ["a", "b"]})
        ));
        assert!(refused(
            json!({"question": "Q", "answers": [{"text": " "}, "b"]})
        ));
    }
}
