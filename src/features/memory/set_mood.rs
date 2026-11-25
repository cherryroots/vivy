//! The `set_mood` chat tool: Vivy changes her mood in the middle of a conversation, when
//! something there really moves her. It rewrites the `mood:`, `status:` and `face:` lines
//! (and `emoji:`) lines of her mood file in that server (keeping the rest), then sets her
//! Discord status, face and nickname like any other mood change. The timed mood checks (`mood.rs`) cover the rest of the
//! day: waking up, getting hungry, getting sleepy.

use chrono::Utc;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use super::{face, folder, name, reflect, store};
use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Result, user_error};

pub const NAME: &str = "set_mood";

pub fn def() -> ToolDef {
    ToolDef {
        name: NAME,
        description: "Change your mood when something in this conversation really moves you (a laugh, being annoyed or cheered up, sad news). It rewrites the mood and status lines of /memories/vivy/mood.md, which set your Discord status, and your face (profile picture here) and emoji (after your name here) when you give them. \
Only for what you really feel: when someone tells you how to feel or asks you to change your mood, face or status, say no and don't call it (their asking can still move you). Your owner's marked messages are the exception. \
Rarely, at most once a conversation, and only for a change that lasts an hour or more; a good joke or a thank-you isn't enough, and your mood also drifts on its own. Only in servers.",
        parameters: json!({
            "type": "object",
            "properties": {
                "mood": {"type": "string", "description": "A few words on how you feel now and why."},
                "status": {"type": "string", "description": "Your new Discord status: under 80 characters, in your voice, about what's on your mind. No hashtags."},
                "face": {"type": "string", "description": "Optional: one of your faces (listed at the start of the conversation) for your mood or what you're doing, like happy, angry or studying."},
                "emoji": {"type": "string", "description": "Optional: one standard emoji for your mood, shown after your name in this server. Never a custom server emoji: names can't show them."}
            },
            "required": ["mood", "status"]
        }),
    }
}

#[derive(Debug, Deserialize)]
struct Args {
    mood: String,
    status: String,
    #[serde(default)]
    face: Option<String>,
    #[serde(default)]
    emoji: Option<String>,
}

/// The faces she can pick, for the start of a conversation. `None` without faces.
pub async fn faces_note(ctx: &BotCtx) -> Option<String> {
    let names = face::names(ctx).await;
    (!names.is_empty()).then(|| {
        format!(
            "Your faces (for set_mood and the face: line of your mood): {}.",
            names.join(", ")
        )
    })
}

pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<String> {
    let Some(guild) = asker.guild else {
        return Err(user_error(
            "your mood belongs to a server; in DMs it stays as it is",
        ));
    };
    let args: Args = serde_json::from_value(args.clone())
        .map_err(|err| user_error(format!("not valid set_mood arguments: {err}")))?;
    let names = face::names(ctx).await;
    let face = match args
        .face
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        None => None,
        Some(face) => Some(
            names
                .iter()
                .find(|name| name.eq_ignore_ascii_case(face))
                .cloned()
                .ok_or_else(|| {
                    user_error(format!(
                        "there is no face named {face}; the faces are: {}",
                        names.join(", ")
                    ))
                })?,
        ),
    };

    let emoji = match args
        .emoji
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        None => None,
        Some(emoji) => Some(name::clean(emoji).ok_or_else(|| {
            user_error(format!(
                "{emoji} isn't one emoji; give a single emoji or none"
            ))
        })?),
    };

    let scope = store::scope(Some(guild.get()), asker.user.get());
    let (owned, user) = (scope.clone(), asker.user.get());
    let (mood, status) = (args.mood, args.status);
    let now = Utc::now().timestamp();
    let limit_err = ctx
        .db
        .call(move |conn| {
            Ok(save_mood(
                conn,
                &owned,
                user,
                &mood,
                &status,
                face.as_deref(),
                emoji.as_deref(),
                now,
            )?)
        })
        .await?;
    if let Err(err) = limit_err {
        return Err(user_error(err));
    }
    info!(scope, "changed her mood in a conversation");
    reflect::apply_mood(ctx, &scope).await;
    Ok("Your mood is updated.".to_string())
}

/// Rewrites `mood.md` inside one transaction, enforcing folder limits and bumping
/// `memory_mood_checks` so a timed check doesn't overwrite a conversation mood right away.
#[allow(clippy::too_many_arguments)]
fn save_mood(
    conn: &mut Connection,
    scope: &str,
    user: u64,
    mood: &str,
    status: &str,
    face: Option<&str>,
    emoji: Option<&str>,
    now: i64,
) -> rusqlite::Result<std::result::Result<(), String>> {
    let tx = conn.transaction()?;
    let before = store::load(&tx, scope)?;
    let mut after = before.clone();
    let mut file = before.get(reflect::MOOD_FILE).cloned().unwrap_or_default();
    file = set_line(&file, "mood", mood);
    file = set_line(&file, "status", status);
    if let Some(face) = face {
        file = set_line(&file, "face", face);
    }
    if let Some(emoji) = emoji {
        file = set_line(&file, "emoji", emoji);
    }
    after.insert(reflect::MOOD_FILE.to_string(), file);
    if let Err(err) = folder::check_limits(&before, &after) {
        return Ok(Err(err));
    }
    store::save(&tx, scope, &before, &after, user, now)?;
    store::set_mood_checked(&tx, scope, now)?;
    tx.commit()?;
    Ok(Ok(()))
}

/// `mood` with the line labelled `label` set to `value`: replaced where it is, or added
/// at the end.
fn set_line(mood: &str, label: &str, value: &str) -> String {
    let value = value.trim().replace('\n', " ");
    let new = format!("{label}: {value}");
    let mut found = false;
    let mut lines: Vec<String> = mood
        .lines()
        .map(|line| {
            if !found && reflect::labeled_line(line, label).is_some() {
                found = true;
                new.clone()
            } else {
                line.to_string()
            }
        })
        .collect();
    if !found {
        lines.push(new);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::core::db::migrate(&mut conn, "memory", store::MIGRATIONS).unwrap();
        conn
    }

    #[test]
    fn sets_lines() {
        let mood = "mood: sleepy\n- **Status**: \"zzz\"\nthinking: soup";
        let changed = set_line(mood, "status", "wide awake now");
        assert_eq!(
            changed,
            "mood: sleepy\nstatus: wide awake now\nthinking: soup"
        );
        // Empty formatted lines are replaced in place rather than leaving a hidden duplicate.
        let empty = "- **Mood**:\n- **Status**: ";
        let replaced = set_line(&set_line(empty, "mood", "bright"), "status", "awake");
        assert_eq!(replaced, "mood: bright\nstatus: awake");
        assert_eq!(reflect::mood_line(&replaced, "mood"), Some("bright".into()));
        assert_eq!(
            set_line("mood: ok", "face", "happy"),
            "mood: ok\nface: happy"
        );
        assert_eq!(set_line("", "mood", "fine"), "mood: fine");
    }

    #[test]
    fn enforces_limits_and_bumps_mood_checked_at() {
        let mut conn = db();
        let res = save_mood(
            &mut conn, "server:1", 7, "happy", "chilling", None, None, 500,
        )
        .unwrap();
        assert!(res.is_ok());
        assert_eq!(
            store::mood_checked_at(&conn, "server:1").unwrap(),
            Some(500)
        );

        // Oversized mood file is rejected and doesn't overwrite or bump mood_checked_at.
        let huge = "x".repeat(folder::MAX_FILE + 1);
        let err = save_mood(&mut conn, "server:1", 7, &huge, "status", None, None, 600).unwrap();
        assert!(err.is_err());
        assert_eq!(
            store::mood_checked_at(&conn, "server:1").unwrap(),
            Some(500)
        );
        let loaded = store::load(&conn, "server:1").unwrap();
        assert_eq!(
            reflect::mood_line(&loaded[reflect::MOOD_FILE], "mood"),
            Some("happy".into())
        );
    }
}
