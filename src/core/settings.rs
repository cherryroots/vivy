//! Settings shared by all features: each user's timezone, and owner mode for the whole bot.

use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension};
use crate::util::discord::UserId;

use super::db::Db;

pub fn timezone_sync(conn: &Connection, user: UserId) -> anyhow::Result<Option<Tz>> {
    let name: Option<Option<String>> = conn
        .query_row(
            "SELECT timezone FROM user_settings WHERE user_id = ?1",
            [user.get()],
            |row| row.get(0),
        )
        .optional()?;
    // A name that no longer parses (renamed zone) is treated as unset.
    Ok(name.flatten().and_then(|name| name.parse().ok()))
}

pub fn set_timezone_sync(conn: &Connection, user: UserId, zone: Tz) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO user_settings (user_id, timezone) VALUES (?1, ?2)
         ON CONFLICT (user_id) DO UPDATE SET timezone = excluded.timezone",
        (user.get(), zone.name()),
    )?;
    Ok(())
}

/// The user's timezone, if they set one with `/timezone`.
pub async fn timezone(db: &Db, user: UserId) -> anyhow::Result<Option<Tz>> {
    db.call(move |conn| timezone_sync(conn, user)).await
}

pub async fn set_timezone(db: &Db, user: UserId, zone: Tz) -> anyhow::Result<()> {
    db.call(move |conn| set_timezone_sync(conn, user, zone))
        .await
}

/// Whether owner mode is on: while it is, her owner's messages are marked so she follows
/// them (see chat's `history::user_tag`). Off until the owner turns it on with the button
/// on the status message.
pub fn owner_mode_sync(conn: &Connection) -> anyhow::Result<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM bot_settings WHERE key = 'owner_mode'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.as_deref() == Some("on"))
}

pub fn set_owner_mode_sync(conn: &Connection, on: bool) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO bot_settings (key, value) VALUES ('owner_mode', ?1)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [if on { "on" } else { "off" }],
    )?;
    Ok(())
}

pub async fn owner_mode(db: &Db) -> anyhow::Result<bool> {
    db.call(|conn| owner_mode_sync(conn)).await
}

pub async fn set_owner_mode(db: &Db, on: bool) -> anyhow::Result<()> {
    db.call(move |conn| set_owner_mode_sync(conn, on)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::test_connection;

    #[test]
    fn timezone_round_trip() {
        let conn = test_connection("none", &[]);
        let user = UserId::new(42);
        assert_eq!(timezone_sync(&conn, user).unwrap(), None);
        set_timezone_sync(&conn, user, Tz::Europe__Oslo).unwrap();
        set_timezone_sync(&conn, user, Tz::Asia__Tokyo).unwrap();
        assert_eq!(timezone_sync(&conn, user).unwrap(), Some(Tz::Asia__Tokyo));
    }

    #[test]
    fn owner_mode_starts_off_and_toggles() {
        let conn = test_connection("none", &[]);
        assert!(!owner_mode_sync(&conn).unwrap());
        set_owner_mode_sync(&conn, true).unwrap();
        assert!(owner_mode_sync(&conn).unwrap());
        set_owner_mode_sync(&conn, false).unwrap();
        assert!(!owner_mode_sync(&conn).unwrap());
    }
}
