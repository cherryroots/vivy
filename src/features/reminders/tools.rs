//! Reminder tools for chat: "@Vivy remind me to stretch every... no, in an hour" works in a
//! normal conversation, because the model can call these.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};

use super::parse;
use super::store::{self, NewReminder, Reschedule};
use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Result, settings, user_error};

pub fn defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "create_reminder",
            description: "Reminds the asker of something later, in this channel, once or on a repeating schedule. Use it when someone asks to be reminded of something.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "when": {
                        "type": "string",
                        "description": "When, in English, in one of these forms: \"in 2h30m\", \"in an hour and a half\", \"at 16:30 CET\", \"tomorrow at 9am\", \"tomorrow evening\", \"tonight\", \"this friday\", \"next week\", \"on the 15th\", \"march 3\", \"on 2026-12-24 at noon\". For a recurring reminder: \"every day at 9:00\", \"every weekday at 8:30\", \"every monday\", \"every other week\", \"every 2 hours\". Rules: clock times need minutes or am/pm (\"7:00\" or \"7pm\", never \"7\"); one weekday per repeat (\"every monday and wednesday\" is two reminders); only one time per reminder. Turn the asker's words into one of these forms. Times without a zone use the asker's timezone."
                    },
                    "message": {"type": "string", "description": "What to remind them of."}
                },
                "required": ["when", "message"],
            }),
        },
        ToolDef {
            name: "list_reminders",
            description: "The asker's pending reminders, soonest first, with their IDs and how recurring ones repeat.",
            parameters: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "edit_reminder",
            description: "Changes one of the asker's pending reminders: its time, its text, or both. It keeps its ID, channel and pictures, and whatever you leave out. A new `when` replaces the whole schedule: a repeat form sets a new repeat, a one-off form turns a recurring reminder into a single one at that time. Use it instead of cancelling and creating again.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "description": "From list_reminders."},
                    "when": {"type": "string", "description": "The new time, in the same forms as create_reminder's `when`. Leave it out to keep the time and repeat."},
                    "message": {"type": "string", "description": "The new text. Leave it out to keep the text."}
                },
                "required": ["id"],
            }),
        },
        ToolDef {
            name: "cancel_reminder",
            description: "Deletes one of the asker's pending reminders. For a recurring one, this stops it.",
            parameters: json!({
                "type": "object",
                "properties": {"id": {"type": "integer", "description": "From list_reminders."}},
                "required": ["id"],
            }),
        },
    ]
}

/// Runs a tool. Returns whether a reminder was added or removed (the scheduler must wake),
/// and the text for the model.
pub async fn run(ctx: &BotCtx, asker: &Asker, name: &str, args: &Value) -> Result<(bool, String)> {
    let zone = settings::timezone(&ctx.db, asker.user).await?;
    let home = zone.unwrap_or(Tz::UTC);
    let user = asker.user.get();
    match name {
        "create_reminder" => {
            let when = args["when"].as_str().unwrap_or_default();
            let message = args["message"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            if message.is_empty() {
                return Err(user_error("`message` is empty."));
            }
            let Reschedule { fire_at, repeat } = reschedule(when, home)?;
            let new = NewReminder {
                user_id: user,
                channel_id: asker.channel.get(),
                guild_id: asker.guild.map(|g| g.get()),
                message,
                fire_at,
                created_at: Utc::now().timestamp(),
                source_message_id: Some(asker.message.get()),
                missing_images: 0,
                images: Vec::new(),
                repeat: repeat.clone(),
            };
            let id = ctx.db.call(move |conn| Ok(store::add(conn, &new)?)).await?;
            let mut text = format!(
                "Reminder {id} set for {}. In Discord, <t:{fire_at}:f> shows that time to each reader.",
                local(fire_at, home)
            );
            if let Some(repeat) = repeat {
                text.push_str(&format!(" It repeats {repeat}, until it's cancelled."));
            }
            if zone.is_none() {
                text.push_str(" The asker has no timezone set, so times were read as UTC; they can set one with /timezone.");
            }
            Ok((true, text))
        }
        "list_reminders" => {
            let list = ctx
                .db
                .call(move |conn| Ok(store::list_pending(conn, user)?))
                .await?;
            if list.is_empty() {
                return Ok((false, "No pending reminders.".to_string()));
            }
            let lines: Vec<String> = list
                .iter()
                .map(|r| {
                    let repeat = match &r.repeat {
                        Some(repeat) => format!(" (repeats {repeat})"),
                        None => String::new(),
                    };
                    format!(
                        "ID {}: {}{repeat}: {}",
                        r.id,
                        local(r.fire_at, home),
                        r.message
                    )
                })
                .collect();
            Ok((false, lines.join("\n")))
        }
        "edit_reminder" => {
            let id = id_arg(args)?;
            let message = match args["message"].as_str().map(str::trim) {
                Some("") => return Err(user_error("`message` is empty.")),
                other => other.map(str::to_string),
            };
            let when = match args["when"].as_str() {
                Some(when) => Some(reschedule(when, home)?),
                None => None,
            };
            if message.is_none() && when.is_none() {
                return Err(user_error("Give a new `when`, a new `message`, or both."));
            }
            let (new_message, new_when) = (message.clone(), when.clone());
            // The reminder as it was, to answer with what changed. `list_pending` only has
            // the asker's pending reminders, so a wrong or foreign ID isn't found.
            let before = ctx
                .db
                .call(move |conn| {
                    let before = store::list_pending(conn, user)?
                        .into_iter()
                        .find(|r| r.id == id);
                    if before.is_some() {
                        store::edit_pending(
                            conn,
                            id,
                            user,
                            new_message.as_deref(),
                            new_when.as_ref(),
                        )?;
                    }
                    Ok(before)
                })
                .await?;
            let Some(before) = before else {
                return Err(user_error(format!(
                    "The asker has no pending reminder {id}."
                )));
            };
            let message = message.unwrap_or(before.message);
            let (fire_at, repeat) = match &when {
                Some(when) => (when.fire_at, when.repeat.clone()),
                None => (before.fire_at, before.repeat.clone()),
            };
            let mut text = format!(
                "Reminder {id} is now set for {}: {message}. In Discord, <t:{fire_at}:f> shows that time to each reader.",
                local(fire_at, home)
            );
            match (&repeat, &before.repeat) {
                (Some(repeat), _) => text.push_str(&format!(" It repeats {repeat}.")),
                (None, Some(old)) => {
                    text.push_str(&format!(" It no longer repeats (it was {old})."));
                }
                (None, None) => {}
            }
            if when.is_some() && zone.is_none() {
                text.push_str(" The asker has no timezone set, so times were read as UTC; they can set one with /timezone.");
            }
            Ok((true, text))
        }
        "cancel_reminder" => {
            let id = id_arg(args)?;
            let deleted = ctx
                .db
                .call(move |conn| Ok(store::delete_pending(conn, id, user)?))
                .await?;
            if deleted {
                Ok((true, format!("Reminder {id} deleted.")))
            } else {
                Err(user_error(format!(
                    "The asker has no pending reminder {id}."
                )))
            }
        }
        _ => Err(user_error(format!("reminders has no tool named {name}"))),
    }
}

/// Reads a `when` like "tomorrow at 9am" or "every monday" into when it's next due and how
/// it repeats (`None` for a one-off). A time that has already passed is an error.
fn reschedule(when: &str, home: Tz) -> Result<Reschedule> {
    let parsed = parse::parse_when(when).map_err(user_error)?;
    let fire_at = parse::resolve(&parsed, Utc::now(), home)
        .map_err(user_error)?
        .timestamp();
    let repeat = match &parsed {
        parse::When::Every(repeat) => Some(parse::describe_repeat(repeat, home)),
        _ => None,
    };
    Ok(Reschedule { fire_at, repeat })
}

/// The reminder ID. Models sometimes send it as text ("12"), so that's read too.
fn id_arg(args: &Value) -> Result<i64> {
    let id = &args["id"];
    id.as_i64()
        .or_else(|| id.as_str().and_then(|s| s.trim().parse().ok()))
        .ok_or_else(|| user_error("`id` must be a number from list_reminders."))
}

/// "2026-10-10 09:00 Europe/Oslo"
fn local(timestamp: i64, zone: Tz) -> String {
    let time = DateTime::from_timestamp(timestamp, 0).unwrap_or_default();
    format!(
        "{} {}",
        time.with_timezone(&zone).format("%Y-%m-%d %H:%M"),
        zone.name()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_as_numbers_or_text() {
        assert_eq!(id_arg(&json!({"id": 12})).unwrap(), 12);
        assert_eq!(id_arg(&json!({"id": " 12 "})).unwrap(), 12);
        assert!(id_arg(&json!({"id": "twelve"})).is_err());
        assert!(id_arg(&json!({})).is_err());
    }

    #[test]
    fn reschedule_sets_or_clears_the_repeat() {
        let daily = reschedule("every day at 9:00", Tz::UTC).unwrap();
        assert_eq!(daily.repeat.as_deref(), Some("every day at 9:00 UTC"));
        assert!(daily.fire_at > Utc::now().timestamp());
        let once = reschedule("in 2h", Tz::UTC).unwrap();
        assert_eq!(once.repeat, None);
        // A time that has passed, or no time at all.
        assert!(reschedule("on 2020-01-01 at noon", Tz::UTC).is_err());
        assert!(reschedule("whenever", Tz::UTC).is_err());
    }
}
