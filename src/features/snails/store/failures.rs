//! Pictures that failed to download: the list the retry loop works through, and what it
//! gave up on.

use rusqlite::{Connection, Transaction, params};

use super::{StoredPicture, insert_picture};

/// A failed picture is tried this many times in all, then dropped: the retry loop takes it
/// off the list and counts it for its host (see [`drop_given_up`]).
pub const MAX_ATTEMPTS: u32 = 3;

/// How long to wait before trying a failed picture again: 10 minutes after the first
/// failure, then 20 (half an hour from the first to the last try).
pub fn retry_delay(attempts: u32) -> i64 {
    600 << attempts.clamp(1, MAX_ATTEMPTS).saturating_sub(1)
}

/// A failed picture whose next try is due.
#[derive(Debug, Clone, PartialEq)]
pub struct DueFailure {
    pub message: u64,
    pub channel: u64,
    pub guild: u64,
    pub author: u64,
    pub source: String,
}

/// Up to `limit` failed pictures due for another try, the longest waiting first.
pub fn due_failures(conn: &Connection, now: i64, limit: i64) -> rusqlite::Result<Vec<DueFailure>> {
    conn.prepare(
        "SELECT message_id, channel_id, guild_id, author_id, source FROM snail_failures
         WHERE next_try <= ?1 AND attempts < ?2 ORDER BY next_try LIMIT ?3",
    )?
    .query_map(params![now, MAX_ATTEMPTS, limit], |row| {
        Ok(DueFailure {
            message: row.get(0)?,
            channel: row.get(1)?,
            guild: row.get(2)?,
            author: row.get(3)?,
            source: row.get(4)?,
        })
    })?
    .collect()
}

/// Sources of a message's failed pictures that the retry loop still tries (not given up).
pub fn waiting_failures(conn: &Connection, message: u64) -> rusqlite::Result<Vec<String>> {
    conn.prepare("SELECT source FROM snail_failures WHERE message_id = ?1 AND attempts < ?2")?
        .query_map(params![message, MAX_ATTEMPTS], |row| row.get(0))?
        .collect()
}

/// A retried picture downloaded: saves its fingerprint and takes it off the list. Returns
/// false (and saves nothing) when the failure is gone: the message was deleted or edited
/// meanwhile, and saving would bring a deleted message back.
pub fn retry_succeeded(
    tx: &Transaction,
    failure: &DueFailure,
    picture: &StoredPicture,
) -> rusqlite::Result<bool> {
    let still_failing: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM snail_failures WHERE message_id = ?1 AND source = ?2)",
        params![failure.message, failure.source],
        |row| row.get(0),
    )?;
    if !still_failing {
        return Ok(false);
    }
    tx.execute(
        "INSERT OR IGNORE INTO snail_messages (message_id, guild_id, channel_id, author_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            failure.message,
            failure.guild,
            failure.channel,
            failure.author
        ],
    )?;
    insert_picture(tx, failure.message, failure.guild, picture)?;
    drop_failure(tx, failure.message, &failure.source)?;
    Ok(true)
}

/// A retry failed again: count it and wait longer before the next one.
pub fn retry_failed(
    conn: &Connection,
    message: u64,
    source: &str,
    error: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE snail_failures SET attempts = attempts + 1, error = ?3, last_tried = ?4,
             next_try = ?4 + (600 << (min(attempts + 1, ?5) - 1))
         WHERE message_id = ?1 AND source = ?2",
        params![message, source, error, now, MAX_ATTEMPTS],
    )?;
    Ok(())
}

/// Moves a message's failed pictures back to `until` without counting a try (used while
/// snails is turned off in their channel).
pub fn postpone_failures(conn: &Connection, message: u64, until: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE snail_failures SET next_try = ?2 WHERE message_id = ?1",
        params![message, until],
    )?;
    Ok(())
}

/// Takes a picture off the retry list, like one an edit removed.
pub fn drop_failure(conn: &Connection, message: u64, source: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM snail_failures WHERE message_id = ?1 AND source = ?2",
        params![message, source],
    )?;
    Ok(())
}

/// Takes every picture that failed [`MAX_ATTEMPTS`] times off the list and counts it in
/// `snail_dropped` for its server and host. Returns how many were dropped.
pub fn drop_given_up(tx: &Transaction) -> rusqlite::Result<usize> {
    tx.execute(
        "INSERT INTO snail_dropped (guild_id, host, count)
         SELECT guild_id, host, count(*) FROM snail_failures WHERE attempts >= ?1
         GROUP BY guild_id, host
         ON CONFLICT (guild_id, host) DO UPDATE SET count = count + excluded.count",
        [MAX_ATTEMPTS],
    )?;
    tx.execute(
        "DELETE FROM snail_failures WHERE attempts >= ?1",
        [MAX_ATTEMPTS],
    )
}

/// How many pictures of each host were dropped in a server, the most first.
pub fn dropped_by_host(conn: &Connection, guild: u64) -> rusqlite::Result<Vec<(String, u64)>> {
    conn.prepare(
        "SELECT host, count FROM snail_dropped WHERE guild_id = ?1 ORDER BY count DESC, host",
    )?
    .query_map([guild], |row| Ok((row.get(0)?, row.get(1)?)))?
    .collect()
}

/// Failed pictures of one host in a server, waiting for another try.
#[derive(Debug, Clone, PartialEq)]
pub struct HostFailures {
    pub host: String,
    pub waiting: u64,
    /// The newest error, as an example.
    pub error: String,
}

/// The failed pictures of a server grouped by host, the most first.
pub fn failures_by_host(conn: &Connection, guild: u64) -> rusqlite::Result<Vec<HostFailures>> {
    // SQLite takes the bare `error` from the row with the max(last_tried).
    conn.prepare(
        "SELECT host, count(*), error, max(last_tried)
         FROM snail_failures WHERE guild_id = ?1 AND attempts < ?2
         GROUP BY host ORDER BY count(*) DESC, host",
    )?
    .query_map(params![guild, MAX_ATTEMPTS], |row| {
        Ok(HostFailures {
            host: row.get(0)?,
            waiting: row.get(1)?,
            error: row.get(2)?,
        })
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::test_connection;
    use crate::features::snails::store::testing::*;
    // The rest of the store, which some of these tests use too.
    use crate::features::snails::store::*;

    #[test]
    fn failed_pictures_are_retried_and_catalogued() {
        let mut conn = test_connection("snails", MIGRATIONS);
        // A message whose only picture failed is still "read before".
        let mut found = indexed(5, &[], vec![]);
        found.failed = vec![failure("pbs.twimg.com/a.jpg", "pbs.twimg.com")];
        save_now(&mut conn, &found, 1_000);
        assert!(is_indexed(&conn, 5).unwrap());
        // Not due before the first wait is over.
        assert!(due_failures(&conn, 1_000, 10).unwrap().is_empty());
        let due = due_failures(&conn, 1_000 + retry_delay(1), 10).unwrap();
        assert_eq!(
            due,
            [DueFailure {
                message: 5,
                channel: 10,
                guild: 1,
                author: 105,
                source: "pbs.twimg.com/a.jpg".into()
            }]
        );

        // Saving the message again (an edit) keeps counting attempts.
        save_now(&mut conn, &found, 2_000);
        let attempts: u32 = conn
            .query_row("SELECT attempts FROM snail_failures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(attempts, 2);
        // Fails until it's given up on.
        for _ in 2..MAX_ATTEMPTS {
            retry_failed(&conn, 5, "pbs.twimg.com/a.jpg", "still 403", 3_000).unwrap();
        }
        assert!(due_failures(&conn, i64::MAX, 10).unwrap().is_empty());
        assert!(failures_by_host(&conn, 1).unwrap().is_empty());
        let drop = |conn: &mut Connection| {
            let tx = conn.transaction().unwrap();
            let dropped = drop_given_up(&tx).unwrap();
            tx.commit().unwrap();
            dropped
        };
        assert_eq!(drop(&mut conn), 1);
        assert_eq!(drop(&mut conn), 0);
        assert_eq!(
            dropped_by_host(&conn, 1).unwrap(),
            [("pbs.twimg.com".to_string(), 1)]
        );
        // Back on the list, to test the rest.
        save_now(&mut conn, &found, 3_000);

        // Another message with a Discord picture that later downloads.
        let mut other = indexed(7, &["x:1"], vec![]);
        other.failed = vec![failure("cdn.discordapp.com/b.png", "cdn.discordapp.com")];
        save_now(&mut conn, &other, 1_000);
        let hosts = failures_by_host(&conn, 1).unwrap();
        // One each, so sorted by name.
        let summary: Vec<(&str, u64)> =
            hosts.iter().map(|h| (h.host.as_str(), h.waiting)).collect();
        assert_eq!(summary, [("cdn.discordapp.com", 1), ("pbs.twimg.com", 1)]);

        let due = due_failures(&conn, i64::MAX, 10).unwrap();
        let picture = StoredPicture {
            position: 0,
            source: "cdn.discordapp.com/b.png".into(),
            fp: fp(0),
        };
        let tx = conn.transaction().unwrap();
        assert!(retry_succeeded(&tx, &due[0], &picture).unwrap());
        tx.commit().unwrap();
        assert_eq!(message_pictures(&conn, 7).unwrap().len(), 1);
        assert_eq!(failures_by_host(&conn, 1).unwrap().len(), 1);

        // Deleting the message forgets its failures too.
        let due5 = DueFailure {
            message: 5,
            channel: 10,
            guild: 1,
            author: 105,
            source: "pbs.twimg.com/a.jpg".into(),
        };
        forget(&conn, 5).unwrap();
        assert!(failures_by_host(&conn, 1).unwrap().is_empty());
        assert!(!is_indexed(&conn, 5).unwrap());
        // A retry that finishes after the delete doesn't bring the message back.
        let tx = conn.transaction().unwrap();
        assert!(!retry_succeeded(&tx, &due5, &picture).unwrap());
        tx.commit().unwrap();
        assert!(!is_indexed(&conn, 5).unwrap());
    }

    #[test]
    fn waiting_failures_are_left_alone() {
        let mut conn = test_connection("snails", MIGRATIONS);
        let mut found = indexed(5, &["x:1"], vec![]);
        found.failed = vec![failure("cdn/a.png", "cdn")];
        save_now(&mut conn, &found, 1_000);
        assert_eq!(waiting_failures(&conn, 5).unwrap(), ["cdn/a.png"]);
        // Saved again (the re-read) with the picture left to the retry loop: no try counted,
        // and its next try doesn't move.
        found.failed = Vec::new();
        found.waiting = vec!["cdn/a.png".to_string()];
        save_now(&mut conn, &found, 2_000);
        let (attempts, next): (u32, i64) = conn
            .query_row("SELECT attempts, next_try FROM snail_failures", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((attempts, next), (1, 1_000 + retry_delay(1)));
        // Gone from the message (neither failed nor waiting): dropped.
        found.waiting = Vec::new();
        save_now(&mut conn, &found, 3_000);
        assert!(waiting_failures(&conn, 5).unwrap().is_empty());
    }

    #[test]
    fn postponed_failures_go_to_the_back() {
        let mut conn = test_connection("snails", MIGRATIONS);
        for (message, at) in [(5, 1_000), (7, 2_000)] {
            let mut found = indexed(message, &[], vec![]);
            found.failed = vec![failure("cdn/x.png", "cdn")];
            save_now(&mut conn, &found, at);
        }
        let now = 10_000;
        let first = |conn: &Connection| due_failures(conn, now, 1).unwrap()[0].message;
        assert_eq!(first(&conn), 5);
        // Postponed (its channel is turned off): the other one is next, and no try counted.
        postpone_failures(&conn, 5, now + 3_600).unwrap();
        assert_eq!(first(&conn), 7);
        let attempts: u32 = conn
            .query_row(
                "SELECT attempts FROM snail_failures WHERE message_id = 5",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(attempts, 1);
    }

    #[test]
    fn retry_waits_grow() {
        assert_eq!(retry_delay(1), 600);
        assert_eq!(retry_delay(2), 1_200);
        assert_eq!(retry_delay(MAX_ATTEMPTS), 600 << (MAX_ATTEMPTS - 1));
        assert_eq!(retry_delay(100), retry_delay(MAX_ATTEMPTS));
    }
}
