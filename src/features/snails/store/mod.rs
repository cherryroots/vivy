//! The tables and queries. Synchronous; run them with `ctx.db.call`.
//!
//! This file has the tables and saving what a message holds. The rest is split by what it's
//! for, and reached as `store::name` like everything here:
//! - `matches.rs`: older messages with the same links or pictures, and the snails counted
//! - `failures.rs`: pictures that failed to download, waiting for another try
//! - `crawl.rs`: the backlog crawl through a server's channels

mod crawl;
mod failures;
mod matches;

pub use self::crawl::*;
pub use self::failures::*;
pub use self::matches::*;

use rusqlite::{Connection, Transaction, params};

use super::fingerprint::Fingerprint;

pub const MIGRATIONS: &[&str] = &[
    // 1
    "CREATE TABLE snail_messages (
        message_id INTEGER PRIMARY KEY,
        guild_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL,
        author_id INTEGER NOT NULL
    );
    -- Link keys (see links.rs) in each message.
    CREATE TABLE snail_links (
        guild_id INTEGER NOT NULL,
        key TEXT NOT NULL,
        message_id INTEGER NOT NULL REFERENCES snail_messages (message_id) ON DELETE CASCADE,
        PRIMARY KEY (guild_id, key, message_id)
    );
    -- Fingerprints (see fingerprint.rs) of the pictures in each message. `position` is the
    -- picture's place in `collect::pictures`, to find it again.
    CREATE TABLE snail_pictures (
        message_id INTEGER NOT NULL REFERENCES snail_messages (message_id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        guild_id INTEGER NOT NULL,
        aspect REAL NOT NULL,
        hashes BLOB NOT NULL,
        PRIMARY KEY (message_id, position)
    );
    CREATE INDEX snail_pictures_guild ON snail_pictures (guild_id);
    -- The backlog crawl: one row per channel or thread, read from newest to oldest.
    CREATE TABLE snail_crawl (
        channel_id INTEGER PRIMARY KEY,
        guild_id INTEGER NOT NULL,
        name TEXT NOT NULL,
        before_id INTEGER,
        finished INTEGER NOT NULL DEFAULT 0,
        messages INTEGER NOT NULL DEFAULT 0,
        pictures INTEGER NOT NULL DEFAULT 0,
        error TEXT
    );
    -- Servers whose crawl an admin started and hasn't paused.
    CREATE TABLE snail_backfills (
        guild_id INTEGER PRIMARY KEY,
        running INTEGER NOT NULL
    );",
    // 2: new messages that turned out to be snails, for the control panel's count.
    "CREATE TABLE snail_caught (
        message_id INTEGER PRIMARY KEY,
        guild_id INTEGER NOT NULL,
        author_id INTEGER NOT NULL,
        caught_at INTEGER NOT NULL        -- unix seconds
    );",
    // 3: edits, deletes, failed downloads and the startup catch-up.
    "-- Where each picture came from (host and path of the original, without Discord's signed
    -- query), so an edit keeps the fingerprints of pictures that didn't change.
    ALTER TABLE snail_pictures ADD COLUMN source TEXT;
    -- Edits and deletes remove a message's links by message.
    CREATE INDEX snail_links_message ON snail_links (message_id);
    -- The startup catch-up looks for the newest saved message per channel.
    CREATE INDEX snail_messages_channel ON snail_messages (channel_id, message_id);
    -- Pictures that failed to download, retried later. Not tied to snail_messages: a message
    -- whose only picture failed has no row there.
    CREATE TABLE snail_failures (
        message_id INTEGER NOT NULL,
        source TEXT NOT NULL,             -- like snail_pictures.source
        position INTEGER NOT NULL,
        guild_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL,
        author_id INTEGER NOT NULL,
        host TEXT NOT NULL,               -- who served it, to spot a provider with trouble
        error TEXT NOT NULL,
        first_failed INTEGER NOT NULL,    -- unix seconds
        last_tried INTEGER NOT NULL,
        next_try INTEGER NOT NULL,
        attempts INTEGER NOT NULL,
        PRIMARY KEY (message_id, source)
    );
    CREATE INDEX snail_failures_due ON snail_failures (next_try);
    -- The newest message the backfill or the catch-up read in each channel.
    ALTER TABLE snail_crawl ADD COLUMN newest_id INTEGER;",
    // 4: pictures dropped after their last try.
    "-- Pictures dropped after their last failed try, counted per host for
    -- `/snail_backfill status`.
    CREATE TABLE snail_dropped (
        guild_id INTEGER NOT NULL,
        host TEXT NOT NULL,
        count INTEGER NOT NULL,
        PRIMARY KEY (guild_id, host)
    );",
];

/// A fingerprinted picture of a message.
#[derive(Debug, Clone)]
pub struct StoredPicture {
    /// Its place in `collect::pictures`.
    pub position: usize,
    /// Host and path of the original (see `collect::Picture::source`).
    pub source: String,
    pub fp: Fingerprint,
}

/// A picture that couldn't be downloaded.
#[derive(Debug, Clone)]
pub struct Failure {
    pub position: usize,
    pub source: String,
    pub host: String,
    pub error: String,
}

/// What was found in one message.
pub struct Indexed {
    pub guild: u64,
    pub channel: u64,
    pub message: u64,
    pub author: u64,
    pub links: Vec<String>,
    pub pictures: Vec<StoredPicture>,
    /// Pictures that failed to download, to try again later.
    pub failed: Vec<Failure>,
    /// Sources of pictures that failed before and weren't tried this time: left to the
    /// retry loop, their rows stay as they are.
    pub waiting: Vec<String>,
}

/// Whether a message was read before: it has saved links or pictures, or a picture waiting
/// to be retried.
pub fn is_indexed(conn: &Connection, message: u64) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM snail_messages WHERE message_id = ?1)
             OR EXISTS (SELECT 1 FROM snail_failures WHERE message_id = ?1)",
        [message],
        |row| row.get(0),
    )
}

/// Saves what a message holds now. It replaces what was saved for it before, so links and
/// pictures an edit removed stop counting. A message with no links or pictures left has no
/// row at all. Failed pictures keep their attempt count across saves; ones that are no
/// longer failing (or no longer in the message) are dropped from the retry list.
pub fn save(tx: &Transaction, found: &Indexed, now: i64) -> rusqlite::Result<()> {
    let id = found.message;
    tx.execute("DELETE FROM snail_links WHERE message_id = ?1", [id])?;
    tx.execute("DELETE FROM snail_pictures WHERE message_id = ?1", [id])?;
    if found.links.is_empty() && found.pictures.is_empty() {
        tx.execute("DELETE FROM snail_messages WHERE message_id = ?1", [id])?;
    } else {
        tx.execute(
            "INSERT OR IGNORE INTO snail_messages (message_id, guild_id, channel_id, author_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, found.guild, found.channel, found.author],
        )?;
    }
    for key in &found.links {
        tx.execute(
            "INSERT OR IGNORE INTO snail_links (guild_id, key, message_id) VALUES (?1, ?2, ?3)",
            params![found.guild, key, id],
        )?;
    }
    for picture in &found.pictures {
        insert_picture(tx, id, found.guild, picture)?;
    }

    // Earlier failures of this message: (source, first failed, attempts).
    let earlier: Vec<(String, i64, u32)> = tx
        .prepare("SELECT source, first_failed, attempts FROM snail_failures WHERE message_id = ?1")?
        .query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // Rows of pictures no longer failing (or no longer in the message) go. Waiting ones stay
    // untouched; failed ones are written again below.
    for (source, _, _) in &earlier {
        if !found.waiting.contains(source) {
            drop_failure(tx, id, source)?;
        }
    }
    for failure in &found.failed {
        let (first, attempts) = earlier
            .iter()
            .find(|(source, _, _)| *source == failure.source)
            .map(|(_, first, attempts)| (*first, attempts + 1))
            .unwrap_or((now, 1));
        tx.execute(
            "INSERT OR REPLACE INTO snail_failures (message_id, source, position, guild_id,
                 channel_id, author_id, host, error, first_failed, last_tried, next_try, attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                failure.source,
                failure.position as i64,
                found.guild,
                found.channel,
                found.author,
                failure.host,
                failure.error,
                first,
                now,
                now + retry_delay(attempts),
                attempts
            ],
        )?;
    }
    Ok(())
}

pub(super) fn insert_picture(
    conn: &Connection,
    message: u64,
    guild: u64,
    picture: &StoredPicture,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO snail_pictures (message_id, position, guild_id, aspect, hashes, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            message,
            picture.position as i64,
            guild,
            picture.fp.aspect,
            picture.fp.hashes,
            picture.source
        ],
    )?;
    Ok(())
}

/// The link keys saved for a message.
pub fn message_links(conn: &Connection, message: u64) -> rusqlite::Result<Vec<String>> {
    conn.prepare("SELECT key FROM snail_links WHERE message_id = ?1")?
        .query_map([message], |row| row.get(0))?
        .collect()
}

/// A picture saved for a message. `source` is None for pictures saved before it was kept.
pub struct SavedPicture {
    pub position: usize,
    pub source: Option<String>,
    pub fp: Fingerprint,
}

pub fn message_pictures(conn: &Connection, message: u64) -> rusqlite::Result<Vec<SavedPicture>> {
    conn.prepare(
        "SELECT position, source, aspect, hashes FROM snail_pictures WHERE message_id = ?1",
    )?
    .query_map([message], |row| {
        Ok(SavedPicture {
            position: row.get::<_, i64>(0)? as usize,
            source: row.get(1)?,
            fp: Fingerprint {
                aspect: row.get(2)?,
                hashes: row.get(3)?,
            },
        })
    })?
    .collect()
}

/// Forgets a message that no longer exists: its links, pictures and failed pictures. A
/// snail it was counted as stays counted (it was posted, even if it's gone now).
pub fn forget(conn: &Connection, message: u64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM snail_messages WHERE message_id = ?1",
        [message],
    )?;
    conn.execute(
        "DELETE FROM snail_failures WHERE message_id = ?1",
        [message],
    )?;
    Ok(())
}

/// Totals for the control panel: (links, pictures, crawl channels finished, crawl channels).
pub fn stats(conn: &Connection) -> rusqlite::Result<(u64, u64, u64, u64)> {
    conn.query_row(
        "SELECT (SELECT count(*) FROM snail_links), (SELECT count(*) FROM snail_pictures),
                (SELECT coalesce(sum(finished), 0) FROM snail_crawl),
                (SELECT count(*) FROM snail_crawl)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::core::db::test_connection;

    #[test]
    fn edits_replace_what_was_saved() {
        let mut conn = test_connection("snails", MIGRATIONS);
        save_now(&mut conn, &indexed(5, &["x:1", "x:2"], vec![fp(0)]), 0);
        assert_eq!(message_pictures(&conn, 5).unwrap().len(), 1);
        // The edit dropped x:1 and the picture and added x:3.
        save_now(&mut conn, &indexed(5, &["x:2", "x:3"], vec![]), 0);
        let mut links = message_links(&conn, 5).unwrap();
        links.sort();
        assert_eq!(links, ["x:2", "x:3"]);
        assert!(message_pictures(&conn, 5).unwrap().is_empty());
        let keys = vec!["x:1".to_string()];
        assert!(same_links(&conn, 1, &keys, 9).unwrap().is_empty());
        // Nothing left: the message is gone.
        save_now(&mut conn, &indexed(5, &[], vec![]), 0);
        assert!(!is_indexed(&conn, 5).unwrap());
    }
}

/// Test helpers the tests of every store file share.
#[cfg(test)]
pub(super) mod testing {
    use super::*;
    use crate::features::snails::fingerprint::{HASH_BYTES, HASHES};

    pub fn fp(byte: u8) -> Fingerprint {
        Fingerprint {
            aspect: 1.0,
            hashes: vec![byte; HASHES * HASH_BYTES],
        }
    }

    pub fn indexed(message: u64, links: &[&str], pictures: Vec<Fingerprint>) -> Indexed {
        Indexed {
            guild: 1,
            channel: 10,
            message,
            author: 100 + message,
            links: links.iter().map(|s| s.to_string()).collect(),
            pictures: pictures
                .into_iter()
                .enumerate()
                .map(|(position, fp)| StoredPicture {
                    position,
                    source: format!("cdn/{message}/{position}.png"),
                    fp,
                })
                .collect(),
            failed: Vec::new(),
            waiting: Vec::new(),
        }
    }

    pub fn failure(source: &str, host: &str) -> Failure {
        Failure {
            position: 0,
            source: source.to_string(),
            host: host.to_string(),
            error: "403 Forbidden".to_string(),
        }
    }

    pub fn save_now(conn: &mut Connection, found: &Indexed, now: i64) {
        let tx = conn.transaction().unwrap();
        save(&tx, found, now).unwrap();
        tx.commit().unwrap();
    }
}
