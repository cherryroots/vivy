//! The pictures she drew, to count them against `monthly_pictures` and for the status.

use rusqlite::{Connection, params};

pub const MIGRATIONS: &[&str] = &[
    // 1
    "CREATE TABLE drawings (
        message_id INTEGER PRIMARY KEY,
        guild_id INTEGER,
        channel_id INTEGER NOT NULL,
        -- Who asked for it.
        user_id INTEGER NOT NULL,
        prompt TEXT NOT NULL,
        -- Estimated from the tokens it used.
        usd REAL NOT NULL,
        at INTEGER NOT NULL
    );
    CREATE INDEX drawings_at ON drawings (at);",
];

/// One picture she posted.
pub struct Drawn {
    pub message_id: u64,
    pub guild_id: Option<u64>,
    pub channel_id: u64,
    pub user_id: u64,
    pub prompt: String,
    pub usd: f64,
    pub at: i64,
}

pub fn record(conn: &Connection, drawn: &Drawn) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO drawings
         (message_id, guild_id, channel_id, user_id, prompt, usd, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            drawn.message_id,
            drawn.guild_id,
            drawn.channel_id,
            drawn.user_id,
            drawn.prompt,
            drawn.usd,
            drawn.at
        ],
    )?;
    Ok(())
}

/// How many pictures were drawn since `since` (Unix seconds), and what they cost.
pub fn drawn_since(conn: &Connection, since: i64) -> rusqlite::Result<(u64, f64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(usd), 0) FROM drawings WHERE at >= ?1",
        [since],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::test_connection;

    #[test]
    fn counts_this_month() {
        let conn = test_connection("drawing", MIGRATIONS);
        for (id, at, usd) in [(1, 50, 0.25), (2, 150, 0.5), (3, 200, 0.125)] {
            let drawn = Drawn {
                message_id: id,
                guild_id: None,
                channel_id: 9,
                user_id: 7,
                prompt: "a cat".into(),
                usd,
                at,
            };
            record(&conn, &drawn).unwrap();
        }
        assert_eq!(drawn_since(&conn, 100).unwrap(), (2, 0.625));
        assert_eq!(drawn_since(&conn, 500).unwrap(), (0, 0.0));
    }
}
