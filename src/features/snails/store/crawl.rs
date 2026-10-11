//! The backlog crawl: which channels of a server to read, how far each got, and where the
//! catch-up after a restart starts.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// Adds channels to the crawl. Channels already in it keep their progress.
pub fn add_crawl_channels(
    tx: &Transaction,
    guild: u64,
    channels: &[(u64, String)],
) -> rusqlite::Result<usize> {
    let mut added = 0;
    for (channel, name) in channels {
        added += tx.execute(
            "INSERT OR IGNORE INTO snail_crawl (channel_id, guild_id, name) VALUES (?1, ?2, ?3)",
            params![channel, guild, name],
        )?;
    }
    Ok(added)
}

pub fn set_running(conn: &Connection, guild: u64, running: bool) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO snail_backfills (guild_id, running) VALUES (?1, ?2)
         ON CONFLICT (guild_id) DO UPDATE SET running = excluded.running",
        params![guild, running],
    )?;
    Ok(())
}

pub fn is_running(conn: &Connection, guild: u64) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT running FROM snail_backfills WHERE guild_id = ?1",
            [guild],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

pub fn running_guilds(conn: &Connection) -> rusqlite::Result<Vec<u64>> {
    let mut stmt = conn.prepare("SELECT guild_id FROM snail_backfills WHERE running = 1")?;
    stmt.query_map([], |row| row.get(0))?.collect()
}

/// The next channel to read: (channel, read messages older than this).
pub fn next_crawl_channel(
    conn: &Connection,
    guild: u64,
) -> rusqlite::Result<Option<(u64, Option<u64>)>> {
    conn.query_row(
        "SELECT channel_id, before_id FROM snail_crawl
         WHERE guild_id = ?1 AND finished = 0 ORDER BY channel_id LIMIT 1",
        [guild],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// Records one page read from a channel.
pub fn crawl_progress(
    conn: &Connection,
    channel: u64,
    before: Option<u64>,
    messages: usize,
    pictures: usize,
    finished: bool,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE snail_crawl SET before_id = coalesce(?2, before_id), messages = messages + ?3,
             pictures = pictures + ?4, finished = ?5
         WHERE channel_id = ?1",
        params![channel, before, messages as i64, pictures as i64, finished],
    )?;
    Ok(())
}

/// Notes the newest message read in a channel (by the backfill or the catch-up).
pub fn crawl_newest(conn: &Connection, channel: u64, newest: u64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE snail_crawl SET newest_id = max(coalesce(newest_id, 0), ?2) WHERE channel_id = ?1",
        params![channel, newest],
    )?;
    Ok(())
}

/// The channels of a server the backfill has read before (all of it, or at least one
/// page), each with the message to catch up after: the newest one read or saved there. A
/// channel with neither starts after the channel's own ID, which is older than any message
/// in it.
pub fn catch_up_channels(conn: &Connection, guild: u64) -> rusqlite::Result<Vec<(u64, u64)>> {
    conn.prepare(
        "SELECT c.channel_id, max(c.channel_id, coalesce(c.newest_id, 0),
                coalesce((SELECT max(m.message_id) FROM snail_messages m
                          WHERE m.channel_id = c.channel_id), 0))
         FROM snail_crawl c
         WHERE c.guild_id = ?1 AND c.error IS NULL AND (c.finished = 1 OR c.before_id IS NOT NULL)
         ORDER BY c.channel_id",
    )?
    .query_map([guild], |row| Ok((row.get(0)?, row.get(1)?)))?
    .collect()
}

/// Servers with any backfill history, for the startup catch-up.
pub fn crawled_guilds(conn: &Connection) -> rusqlite::Result<Vec<u64>> {
    conn.prepare("SELECT DISTINCT guild_id FROM snail_crawl")?
        .query_map([], |row| row.get(0))?
        .collect()
}

/// Gives up on a channel the bot can't read.
pub fn crawl_failed(conn: &Connection, channel: u64, error: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE snail_crawl SET finished = 1, error = ?2 WHERE channel_id = ?1",
        params![channel, error],
    )?;
    Ok(())
}

#[derive(Debug, Default, PartialEq)]
pub struct CrawlStatus {
    pub running: bool,
    pub channels: u64,
    pub finished: u64,
    pub failed: u64,
    pub messages: u64,
    pub pictures: u64,
}

pub fn crawl_status(conn: &Connection, guild: u64) -> rusqlite::Result<CrawlStatus> {
    let mut status = conn.query_row(
        "SELECT count(*), coalesce(sum(finished), 0), count(error),
                coalesce(sum(messages), 0), coalesce(sum(pictures), 0)
         FROM snail_crawl WHERE guild_id = ?1",
        [guild],
        |row| {
            Ok(CrawlStatus {
                running: false,
                channels: row.get(0)?,
                finished: row.get(1)?,
                failed: row.get(2)?,
                messages: row.get(3)?,
                pictures: row.get(4)?,
            })
        },
    )?;
    status.running = is_running(conn, guild)?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::test_connection;
    use crate::features::snails::store::testing::*;
    // The rest of the store, which some of these tests use too.
    use crate::features::snails::store::*;

    #[test]
    fn catch_up_starts_after_the_newest_message() {
        let mut conn = test_connection("snails", MIGRATIONS);
        let tx = conn.transaction().unwrap();
        let channels = vec![
            (2, "read".to_string()),
            (3, "never read".to_string()),
            (4, "no access".to_string()),
        ];
        add_crawl_channels(&tx, 1, &channels).unwrap();
        tx.commit().unwrap();
        // Channel 10 (from `indexed`) isn't in the crawl; channel 2 is.
        crawl_progress(&conn, 2, Some(50), 100, 0, false).unwrap();
        crawl_newest(&conn, 2, 900).unwrap();
        crawl_failed(&conn, 4, "Missing Access").unwrap();
        assert_eq!(catch_up_channels(&conn, 1).unwrap(), [(2, 900)]);
        // A newer saved message moves the start forward; an older newest_id doesn't move back.
        let mut found = indexed(950, &["x:1"], vec![]);
        found.channel = 2;
        save_now(&mut conn, &found, 0);
        crawl_newest(&conn, 2, 100).unwrap();
        assert_eq!(catch_up_channels(&conn, 1).unwrap(), [(2, 950)]);
        assert_eq!(crawled_guilds(&conn).unwrap(), [1]);
    }

    #[test]
    fn crawl_bookkeeping() {
        let mut conn = test_connection("snails", MIGRATIONS);
        let tx = conn.transaction().unwrap();
        let channels = vec![(20, "general".to_string()), (30, "memes".to_string())];
        assert_eq!(add_crawl_channels(&tx, 1, &channels).unwrap(), 2);
        assert_eq!(add_crawl_channels(&tx, 1, &channels).unwrap(), 0);
        tx.commit().unwrap();

        set_running(&conn, 1, true).unwrap();
        assert_eq!(running_guilds(&conn).unwrap(), [1]);
        assert_eq!(next_crawl_channel(&conn, 1).unwrap(), Some((20, None)));
        crawl_progress(&conn, 20, Some(500), 100, 3, false).unwrap();
        assert_eq!(next_crawl_channel(&conn, 1).unwrap(), Some((20, Some(500))));
        crawl_progress(&conn, 20, None, 40, 1, true).unwrap();
        crawl_failed(&conn, 30, "Missing Access").unwrap();
        assert_eq!(next_crawl_channel(&conn, 1).unwrap(), None);

        let status = crawl_status(&conn, 1).unwrap();
        assert_eq!(
            status,
            CrawlStatus {
                running: true,
                channels: 2,
                finished: 2,
                failed: 1,
                messages: 140,
                pictures: 4
            }
        );
        set_running(&conn, 1, false).unwrap();
        assert!(running_guilds(&conn).unwrap().is_empty());
    }
}
