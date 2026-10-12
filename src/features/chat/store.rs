//! The chat history: every message the bot answered and every answer it gave.
//!
//! `chat_turns` holds one row per turn. Turns point at the turn they answer or reply to
//! (`parent_id`), so a conversation is the chain from a turn up to its root, and a
//! regenerated answer is a second child of the same question. Rows are never edited,
//! except that an answer's raw output (`native_json`) is cleared once it is
//! [`NATIVE_KEEP_DAYS`] old: see [`forget_old_rounds`].
//!
//! `chat_messages` maps Discord message IDs to turns. A long answer spans several Discord
//! messages, and replying to any of them continues the same conversation.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::ai::Role;
use crate::util::media::MediaKind;

pub const MIGRATIONS: &[&str] = &[
    // 1
    "CREATE TABLE chat_turns (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        parent_id INTEGER REFERENCES chat_turns (id),
        role TEXT NOT NULL,               -- 'user' or 'assistant'
        author_id INTEGER NOT NULL,       -- who wrote it; the bot for answers
        channel_id INTEGER NOT NULL,
        content_json TEXT NOT NULL,       -- provider-neutral, see StoredPart
        native_json TEXT,                 -- the provider's raw output, for answers
        provider TEXT,                    -- who wrote an answer: 'openai', ...
        model TEXT,
        continuation_id TEXT,             -- e.g. OpenAI's response ID
        created_at INTEGER NOT NULL
    );
    CREATE INDEX chat_turns_parent ON chat_turns (parent_id);
    CREATE TABLE chat_messages (
        message_id INTEGER PRIMARY KEY,
        turn_id INTEGER NOT NULL REFERENCES chat_turns (id)
    );
    CREATE INDEX chat_messages_turn ON chat_messages (turn_id);",
    // 2: check-ins Vivy planned for herself, and what the server's custom emoji look like.
    "CREATE TABLE chat_follow_ups (
        id INTEGER PRIMARY KEY,
        guild_id INTEGER,                 -- NULL in DMs
        channel_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,         -- who she checks in with
        message_id INTEGER NOT NULL,      -- the message she planned it from
        note TEXT NOT NULL,               -- what to ask about
        due_at INTEGER NOT NULL,          -- unix seconds
        created_at INTEGER NOT NULL
    );
    CREATE INDEX chat_follow_ups_due ON chat_follow_ups (due_at);
    CREATE TABLE chat_emoji (
        emoji_id INTEGER PRIMARY KEY,
        guild_id INTEGER NOT NULL,
        name TEXT NOT NULL,
        description TEXT NOT NULL
    );",
    // 3: for `look_back_at_answers`, how an answer ended and how long it took.
    "ALTER TABLE chat_turns ADD COLUMN error TEXT;          -- why an answer failed or stopped
    ALTER TABLE chat_turns ADD COLUMN started_at INTEGER;   -- when work on an answer began
    CREATE INDEX chat_turns_channel ON chat_turns (channel_id, role, id);",
];

/// One piece of a stored turn. Media is kept as a link and downloaded again when an old
/// conversation has to be sent in full.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredPart {
    Text {
        text: String,
    },
    Media {
        url: String,
        kind: StoredKind,
        mime: String,
    },
    /// What the features added to a question when it was asked (see
    /// [`Feature::chat_context`](crate::core::Feature::chat_context)). Kept, so the
    /// question reads the same every time the conversation is sent again.
    Context {
        text: String,
    },
    /// Any other attachment, like a PDF or a spreadsheet.
    File {
        url: String,
        name: String,
        mime: String,
    },
}

/// [`MediaKind`] as stored.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredKind {
    Image,
    Gif,
    Video,
}

impl From<MediaKind> for StoredKind {
    fn from(kind: MediaKind) -> Self {
        match kind {
            MediaKind::Image => StoredKind::Image,
            MediaKind::Gif => StoredKind::Gif,
            MediaKind::Video => StoredKind::Video,
        }
    }
}

impl From<StoredKind> for MediaKind {
    fn from(kind: StoredKind) -> Self {
        match kind {
            StoredKind::Image => MediaKind::Image,
            StoredKind::Gif => MediaKind::Gif,
            StoredKind::Video => MediaKind::Video,
        }
    }
}

/// Which provider wrote an answer, and how to continue from it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Written {
    pub provider: String,
    pub model: String,
    pub continuation_id: Option<String>,
    pub native_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewTurn {
    pub parent_id: Option<i64>,
    pub role: Role,
    pub author_id: u64,
    pub channel_id: u64,
    pub parts: Vec<StoredPart>,
    /// Set for answers.
    pub written: Option<Written>,
    /// For answers: when work on it began (`created_at` is when it was saved, at the end).
    pub started_at: Option<i64>,
    /// For answers that didn't finish: the error, or that ❌ stopped it.
    pub error: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub role: Role,
    pub author_id: u64,
    pub parts: Vec<StoredPart>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub continuation_id: Option<String>,
    /// The provider's raw output, for answers.
    pub native_json: Option<String>,
    pub channel_id: u64,
    pub started_at: Option<i64>,
    pub error: Option<String>,
    pub created_at: i64,
}

/// Adds a turn and links its Discord messages to it. Linking a message that already
/// belongs to a turn moves it (a regenerated answer reuses the old answer's messages).
pub fn add_turn(conn: &mut Connection, turn: &NewTurn, messages: &[u64]) -> anyhow::Result<i64> {
    let tx = conn.transaction()?;
    let role = match turn.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let written = turn.written.clone().unwrap_or_default();
    let optional = |s: String| (!s.is_empty()).then_some(s);
    tx.execute(
        "INSERT INTO chat_turns (parent_id, role, author_id, channel_id, content_json,
                                 native_json, provider, model, continuation_id, created_at,
                                 started_at, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            turn.parent_id,
            role,
            turn.author_id,
            turn.channel_id,
            serde_json::to_string(&turn.parts)?,
            written.native_json,
            optional(written.provider),
            optional(written.model),
            written.continuation_id,
            turn.created_at,
            turn.started_at,
            turn.error,
        ],
    )?;
    let id = tx.last_insert_rowid();
    for message in messages {
        tx.execute(
            "INSERT OR REPLACE INTO chat_messages (message_id, turn_id) VALUES (?1, ?2)",
            params![message, id],
        )?;
    }
    tx.commit()?;
    Ok(id)
}

/// Adds parts to the end of a stored turn.
pub fn add_parts(conn: &Connection, turn_id: i64, parts: &[StoredPart]) -> anyhow::Result<()> {
    let content: String = conn.query_row(
        "SELECT content_json FROM chat_turns WHERE id = ?1",
        [turn_id],
        |row| row.get(0),
    )?;
    let mut stored: Vec<StoredPart> = serde_json::from_str(&content)?;
    stored.extend_from_slice(parts);
    conn.execute(
        "UPDATE chat_turns SET content_json = ?1 WHERE id = ?2",
        params![serde_json::to_string(&stored)?, turn_id],
    )?;
    Ok(())
}

/// The turn a Discord message belongs to.
pub fn turn_for_message(conn: &Connection, message_id: u64) -> anyhow::Result<Option<Turn>> {
    let turn_id: Option<i64> = conn
        .query_row(
            "SELECT turn_id FROM chat_messages WHERE message_id = ?1",
            [message_id],
            |row| row.get(0),
        )
        .optional()?;
    match turn_id {
        Some(id) => get_turn(conn, id),
        None => Ok(None),
    }
}

pub fn get_turn(conn: &Connection, id: i64) -> anyhow::Result<Option<Turn>> {
    let row = conn
        .query_row(
            &format!("SELECT {TURN_COLUMNS} FROM chat_turns WHERE id = ?1"),
            [id],
            read_row,
        )
        .optional()?;
    row.map(into_turn).transpose()
}

/// Her answers, newest first: in one channel or everywhere, and only those older than
/// answer `before` when it's given.
pub fn recent_answers(
    conn: &Connection,
    channel: Option<u64>,
    before: Option<i64>,
    limit: usize,
) -> anyhow::Result<Vec<Turn>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TURN_COLUMNS} FROM chat_turns
         WHERE role = 'assistant' AND (?1 IS NULL OR channel_id = ?1) AND (?2 IS NULL OR id < ?2)
         ORDER BY id DESC LIMIT ?3"
    ))?;
    let rows = stmt
        .query_map(params![channel, before, limit as i64], read_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(into_turn).collect()
}

/// The newest answer to a question turn.
pub fn answer_to(conn: &Connection, question_id: i64) -> anyhow::Result<Option<Turn>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT {TURN_COLUMNS} FROM chat_turns
                 WHERE parent_id = ?1 AND role = 'assistant' ORDER BY id DESC LIMIT 1"
            ),
            [question_id],
            read_row,
        )
        .optional()?;
    row.map(into_turn).transpose()
}

/// The Discord messages of a turn, in order.
pub fn messages_of(conn: &Connection, turn_id: i64) -> anyhow::Result<Vec<u64>> {
    let mut stmt = conn
        .prepare("SELECT message_id FROM chat_messages WHERE turn_id = ?1 ORDER BY message_id")?;
    let ids = stmt
        .query_map([turn_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}

/// A turn and the turns above it, oldest first, at most `limit` long.
pub fn chain(conn: &Connection, turn_id: i64, limit: usize) -> anyhow::Result<Vec<Turn>> {
    let mut turns = Vec::new();
    let mut next = Some(turn_id);
    while let Some(id) = next {
        if turns.len() >= limit {
            break;
        }
        let Some(turn) = get_turn(conn, id)? else {
            break;
        };
        next = turn.parent_id;
        turns.push(turn);
    }
    turns.reverse();
    Ok(turns)
}

/// How long an answer's raw provider output (`native_json`) is kept.
pub const NATIVE_KEEP_DAYS: i64 = 60;

/// Clears the raw output (`native_json`: thinking, tool calls and their results) of answers
/// written before `before`. It is only needed to replay recent conversations exactly; an
/// answer without it is sent again as its text, like one from another provider. Returns
/// how many answers were cleared.
pub fn forget_old_rounds(conn: &Connection, before: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE chat_turns SET native_json = NULL
         WHERE native_json IS NOT NULL AND created_at < ?1",
        [before],
    )
}

/// The columns [`read_row`] reads, in its order.
const TURN_COLUMNS: &str = "id, parent_id, role, author_id, content_json, provider, model,
    continuation_id, native_json, channel_id, started_at, error, created_at";

/// A row as SQLite returns it; [`into_turn`] then reads the JSON, which can fail in its
/// own way.
fn read_row(row: &Row) -> rusqlite::Result<(Turn, String)> {
    let role: String = row.get(2)?;
    let turn = Turn {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        role: if role == "assistant" {
            Role::Assistant
        } else {
            Role::User
        },
        author_id: row.get(3)?,
        parts: Vec::new(),
        provider: row.get(5)?,
        model: row.get(6)?,
        continuation_id: row.get(7)?,
        native_json: row.get(8)?,
        channel_id: row.get(9)?,
        started_at: row.get(10)?,
        error: row.get(11)?,
        created_at: row.get(12)?,
    };
    Ok((turn, row.get(4)?))
}

fn into_turn((mut turn, content): (Turn, String)) -> anyhow::Result<Turn> {
    turn.parts = serde_json::from_str(&content)?;
    Ok(turn)
}

/// A check-in Vivy planned for herself.
#[derive(Debug, Clone, PartialEq)]
pub struct FollowUp {
    pub id: i64,
    pub guild_id: Option<u64>,
    pub channel_id: u64,
    pub user_id: u64,
    pub message_id: u64,
    pub note: String,
    pub due_at: i64,
}

pub fn add_follow_up(conn: &Connection, f: &FollowUp, now: i64) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO chat_follow_ups
            (guild_id, channel_id, user_id, message_id, note, due_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            f.guild_id,
            f.channel_id,
            f.user_id,
            f.message_id,
            f.note,
            f.due_at,
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// How many check-ins are planned with this person.
pub fn pending_follow_ups(conn: &Connection, user: u64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT count(*) FROM chat_follow_ups WHERE user_id = ?1",
        [user],
        |row| row.get(0),
    )
}

/// The check-ins that are due, and removes them: each one is tried once.
pub fn take_due_follow_ups(conn: &mut Connection, now: i64) -> rusqlite::Result<Vec<FollowUp>> {
    let tx = conn.transaction()?;
    let due: Vec<FollowUp> = {
        let mut stmt = tx.prepare(
            "SELECT id, guild_id, channel_id, user_id, message_id, note, due_at
             FROM chat_follow_ups WHERE due_at <= ?1 ORDER BY due_at",
        )?;
        let rows = stmt.query_map([now], |row| {
            Ok(FollowUp {
                id: row.get(0)?,
                guild_id: row.get(1)?,
                channel_id: row.get(2)?,
                user_id: row.get(3)?,
                message_id: row.get(4)?,
                note: row.get(5)?,
                due_at: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    tx.execute("DELETE FROM chat_follow_ups WHERE due_at <= ?1", [now])?;
    tx.commit()?;
    Ok(due)
}

/// When the next check-in is due.
pub fn next_follow_up(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT min(due_at) FROM chat_follow_ups", [], |row| {
        row.get(0)
    })
}

/// The next planned check-in in a server: when, with whom, and what about. Check-ins from
/// DMs stay private.
pub fn upcoming_follow_up(conn: &Connection) -> rusqlite::Result<Option<(i64, u64, String)>> {
    conn.query_row(
        "SELECT due_at, user_id, note FROM chat_follow_ups WHERE guild_id IS NOT NULL
         ORDER BY due_at LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
}

/// For the control panel: answers given so far, and check-ins still planned.
pub fn stats(conn: &Connection) -> rusqlite::Result<(i64, i64)> {
    conn.query_row(
        "SELECT (SELECT count(*) FROM chat_turns WHERE role = 'assistant'),
                (SELECT count(*) FROM chat_follow_ups)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

/// The emoji of a server that have a description, by ID.
pub fn emoji_descriptions(
    conn: &Connection,
    guild: u64,
) -> rusqlite::Result<std::collections::HashMap<u64, String>> {
    let mut stmt =
        conn.prepare("SELECT emoji_id, description FROM chat_emoji WHERE guild_id = ?1")?;
    let rows = stmt.query_map([guild], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

pub fn set_emoji_description(
    conn: &Connection,
    emoji: u64,
    guild: u64,
    name: &str,
    description: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO chat_emoji (emoji_id, guild_id, name, description) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (emoji_id) DO UPDATE SET name = excluded.name,
            description = excluded.description",
        params![emoji, guild, name, description],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::test_connection;

    fn new_turn(parent_id: Option<i64>, role: Role, text: &str) -> NewTurn {
        NewTurn {
            parent_id,
            role,
            author_id: 1,
            channel_id: 2,
            parts: vec![StoredPart::Text { text: text.into() }],
            written: (role == Role::Assistant).then(|| Written {
                provider: "openai".into(),
                model: "m".into(),
                continuation_id: Some(format!("resp_{text}")),
                native_json: Some("[]".into()),
            }),
            started_at: None,
            error: None,
            created_at: 0,
        }
    }

    #[test]
    fn old_rounds_are_forgotten() {
        let mut conn = test_connection("chat", MIGRATIONS);
        let mut old = new_turn(None, Role::Assistant, "old");
        old.created_at = 100;
        let old = add_turn(&mut conn, &old, &[]).unwrap();
        let mut recent = new_turn(None, Role::Assistant, "recent");
        recent.created_at = 300;
        let recent = add_turn(&mut conn, &recent, &[]).unwrap();

        assert_eq!(forget_old_rounds(&conn, 200).unwrap(), 1);
        let old = get_turn(&conn, old).unwrap().unwrap();
        assert_eq!(old.native_json, None);
        // The rest of the answer stays.
        assert_eq!(old.continuation_id.as_deref(), Some("resp_old"));
        assert!(
            get_turn(&conn, recent)
                .unwrap()
                .unwrap()
                .native_json
                .is_some()
        );
        // Nothing left to clear.
        assert_eq!(forget_old_rounds(&conn, 200).unwrap(), 0);
    }

    #[test]
    fn follow_ups_are_taken_once_when_due() {
        let mut conn = test_connection("chat", MIGRATIONS);
        let follow_up = |due_at| FollowUp {
            id: 0,
            guild_id: Some(1),
            channel_id: 2,
            user_id: 3,
            message_id: 4,
            note: "the interview".into(),
            due_at,
        };
        add_follow_up(&conn, &follow_up(100), 0).unwrap();
        add_follow_up(&conn, &follow_up(200), 0).unwrap();
        assert_eq!(pending_follow_ups(&conn, 3).unwrap(), 2);
        assert_eq!(next_follow_up(&conn).unwrap(), Some(100));
        assert!(take_due_follow_ups(&mut conn, 50).unwrap().is_empty());
        let due = take_due_follow_ups(&mut conn, 150).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].note, "the interview");
        assert!(take_due_follow_ups(&mut conn, 150).unwrap().is_empty());
        assert_eq!(next_follow_up(&conn).unwrap(), Some(200));
    }

    #[test]
    fn emoji_descriptions_by_server() {
        let conn = test_connection("chat", MIGRATIONS);
        set_emoji_description(&conn, 10, 1, "pog", "a surprised face").unwrap();
        set_emoji_description(&conn, 10, 1, "pog2", "a very surprised face").unwrap();
        set_emoji_description(&conn, 11, 2, "kek", "laughing").unwrap();
        let first = emoji_descriptions(&conn, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[&10], "a very surprised face");
    }

    #[test]
    fn chains_and_messages() {
        let mut conn = test_connection("chat", MIGRATIONS);
        let question = add_turn(&mut conn, &new_turn(None, Role::User, "q"), &[10]).unwrap();
        let answer = add_turn(
            &mut conn,
            &new_turn(Some(question), Role::Assistant, "a"),
            &[11, 12],
        )
        .unwrap();
        let follow_up =
            add_turn(&mut conn, &new_turn(Some(answer), Role::User, "f"), &[13]).unwrap();

        // Any part of a long answer finds the answer.
        let found = turn_for_message(&conn, 12).unwrap().unwrap();
        assert_eq!(found.id, answer);
        assert_eq!(found.continuation_id.as_deref(), Some("resp_a"));
        assert_eq!(messages_of(&conn, answer).unwrap(), [11, 12]);

        let chain = chain(&conn, follow_up, 50).unwrap();
        assert_eq!(
            chain.iter().map(|t| t.id).collect::<Vec<_>>(),
            [question, answer, follow_up]
        );
        assert_eq!(chain[0].provider, None);
        assert_eq!(super::chain(&conn, follow_up, 2).unwrap().len(), 2);

        // A regenerated answer takes over the old answer's messages.
        let again = add_turn(
            &mut conn,
            &new_turn(Some(question), Role::Assistant, "b"),
            &[11],
        )
        .unwrap();
        assert_eq!(turn_for_message(&conn, 11).unwrap().unwrap().id, again);
        assert_eq!(turn_for_message(&conn, 12).unwrap().unwrap().id, answer);
    }

    #[test]
    fn recent_answers_by_channel() {
        let mut conn = test_connection("chat", MIGRATIONS);
        let question = add_turn(&mut conn, &new_turn(None, Role::User, "q"), &[10]).unwrap();
        let mut first = new_turn(Some(question), Role::Assistant, "a");
        first.started_at = Some(5);
        first.error = Some("stopped with ❌".into());
        let first = add_turn(&mut conn, &first, &[11]).unwrap();
        let mut elsewhere = new_turn(None, Role::Assistant, "b");
        elsewhere.channel_id = 3;
        let elsewhere = add_turn(&mut conn, &elsewhere, &[]).unwrap();
        let again = add_turn(
            &mut conn,
            &new_turn(Some(question), Role::Assistant, "c"),
            &[],
        )
        .unwrap();

        let ids = |turns: Vec<Turn>| turns.iter().map(|t| t.id).collect::<Vec<_>>();
        assert_eq!(
            ids(recent_answers(&conn, None, None, 10).unwrap()),
            [again, elsewhere, first]
        );
        assert_eq!(
            ids(recent_answers(&conn, Some(2), None, 10).unwrap()),
            [again, first]
        );
        assert_eq!(
            ids(recent_answers(&conn, Some(2), Some(again), 10).unwrap()),
            [first]
        );
        assert_eq!(ids(recent_answers(&conn, None, None, 1).unwrap()), [again]);

        let saved = get_turn(&conn, first).unwrap().unwrap();
        assert_eq!(saved.started_at, Some(5));
        assert_eq!(saved.error.as_deref(), Some("stopped with ❌"));
        assert_eq!(saved.channel_id, 2);
        // A question leads to its newest answer.
        assert_eq!(answer_to(&conn, question).unwrap().unwrap().id, again);
        assert!(answer_to(&conn, again).unwrap().is_none());
    }

    #[test]
    fn parts_round_trip() {
        let mut conn = test_connection("chat", MIGRATIONS);
        let mut turn = new_turn(None, Role::User, "look");
        turn.parts.push(StoredPart::Media {
            url: "https://x/a.png".into(),
            kind: StoredKind::Image,
            mime: "image/png".into(),
        });
        let id = add_turn(&mut conn, &turn, &[1]).unwrap();
        assert_eq!(get_turn(&conn, id).unwrap().unwrap().parts, turn.parts);

        let context = StoredPart::Context {
            text: "<memory_files/>".into(),
        };
        add_parts(&conn, id, std::slice::from_ref(&context)).unwrap();
        turn.parts.push(context);
        assert_eq!(get_turn(&conn, id).unwrap().unwrap().parts, turn.parts);
    }
}
