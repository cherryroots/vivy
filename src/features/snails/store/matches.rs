//! Older messages with the same links or pictures, and the snails counted.

use rusqlite::{Connection, params};

use crate::features::snails::fingerprint::{
    Fingerprint, HASH_BYTES, HASHES, MATCH_DISTANCE, distance,
};

/// A stored message.
#[derive(Debug, Clone, PartialEq)]
pub struct Posted {
    pub message: u64,
    pub channel: u64,
    pub author: u64,
}

/// Older messages in the server with one of these link keys.
pub fn same_links(
    conn: &Connection,
    guild: u64,
    keys: &[String],
    before: u64,
) -> rusqlite::Result<Vec<Posted>> {
    let mut found = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT m.message_id, m.channel_id, m.author_id
         FROM snail_links l JOIN snail_messages m USING (message_id)
         WHERE l.guild_id = ?1 AND l.key = ?2 AND l.message_id < ?3
         ORDER BY l.message_id",
    )?;
    for key in keys {
        let rows = stmt.query_map(params![guild, key, before], |row| {
            Ok(Posted {
                message: row.get(0)?,
                channel: row.get(1)?,
                author: row.get(2)?,
            })
        })?;
        for row in rows {
            let posted = row?;
            if !found.contains(&posted) {
                found.push(posted);
            }
        }
    }
    Ok(found)
}

/// A stored picture that is close to one of the pictures being checked.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub posted: Posted,
    pub position: usize,
    /// Which of the checked pictures it is close to.
    pub checked: usize,
    pub distance: u32,
}

/// A stored picture as [`pictures_after`] reads it, for comparing.
#[derive(Debug, Clone)]
pub struct ScannedPicture {
    pub posted: Posted,
    pub position: usize,
    pub fp: Fingerprint,
}

/// How many stored pictures [`pictures_after`] reads per call, so other database work can
/// run in between on a big server. (About 2.6 KB each, so a chunk is about 13 MB.)
pub const SCAN_CHUNK: i64 = 5_000;

/// One chunk of the server's pictures older than message `before`: those after `rowid`,
/// plus the rowid to continue from, or None when there are no more. Only reads: the
/// comparing ([`close_to`]) happens off the database thread.
pub fn pictures_after(
    conn: &Connection,
    guild: u64,
    before: u64,
    after_rowid: i64,
) -> rusqlite::Result<(Vec<ScannedPicture>, Option<i64>)> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.rowid, p.message_id, p.position, p.aspect, p.hashes, m.channel_id, m.author_id
         FROM snail_pictures p JOIN snail_messages m USING (message_id)
         WHERE p.guild_id = ?1 AND p.rowid > ?2 AND p.message_id < ?3
         ORDER BY p.rowid LIMIT ?4",
    )?;
    let mut rows = stmt.query(params![guild, after_rowid, before, SCAN_CHUNK])?;
    let mut found = Vec::new();
    let (mut last, mut count) = (after_rowid, 0);
    while let Some(row) = rows.next()? {
        last = row.get(0)?;
        count += 1;
        let hashes: Vec<u8> = row.get(4)?;
        if hashes.len() != HASHES * HASH_BYTES {
            continue;
        }
        found.push(ScannedPicture {
            posted: Posted {
                message: row.get(1)?,
                channel: row.get(5)?,
                author: row.get(6)?,
            },
            position: row.get::<_, i64>(2)? as usize,
            fp: Fingerprint {
                aspect: row.get(3)?,
                hashes,
            },
        });
    }
    let next = (count == SCAN_CHUNK).then_some(last);
    Ok((found, next))
}

/// The stored pictures within [`MATCH_DISTANCE`] of any of `checked`. Pure CPU work: run
/// it on a blocking thread for a big chunk.
pub fn close_to(checked: &[Fingerprint], stored: &[ScannedPicture]) -> Vec<Candidate> {
    let mut found = Vec::new();
    for row in stored {
        for (i, fp) in checked.iter().enumerate() {
            let d = distance(fp, &row.fp);
            if d <= MATCH_DISTANCE {
                found.push(Candidate {
                    posted: row.posted.clone(),
                    position: row.position,
                    checked: i,
                    distance: d,
                });
            }
        }
    }
    found
}

/// Notes that a new message was a snail.
pub fn record_caught(
    conn: &Connection,
    guild: u64,
    message: u64,
    author: u64,
    at: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO snail_caught (message_id, guild_id, author_id, caught_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![message, guild, author, at],
    )?;
    Ok(())
}

/// Clears the server's counted snails, for a fresh leaderboard. The saved links and pictures
/// stay, so later reposts of older posts still count. Returns how many were cleared.
pub fn reset_caught(conn: &Connection, guild: u64) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM snail_caught WHERE guild_id = ?1", [guild])
}

/// Whether a message was already counted as a snail.
pub fn is_caught(conn: &Connection, message: u64) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM snail_caught WHERE message_id = ?1)",
        [message],
        |row| row.get(0),
    )
}

/// Snails caught since `since`, in every server.
pub fn caught_since(conn: &Connection, since: i64) -> rusqlite::Result<u64> {
    conn.query_row(
        "SELECT count(*) FROM snail_caught WHERE caught_at >= ?1",
        [since],
        |row| row.get(0),
    )
}

/// The server's top snailers since `since` (0 for all time): (author, snails), most first,
/// at most `limit`.
pub fn top_snailers(
    conn: &Connection,
    guild: u64,
    since: i64,
    limit: usize,
) -> rusqlite::Result<Vec<(u64, u64)>> {
    let mut stmt = conn.prepare(
        "SELECT author_id, count(*) AS snails FROM snail_caught
         WHERE guild_id = ?1 AND caught_at >= ?2
         GROUP BY author_id ORDER BY snails DESC, author_id LIMIT ?3",
    )?;
    stmt.query_map(params![guild, since, limit as i64], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::test_connection;

    #[test]
    fn leaderboard_counts_per_server() {
        let conn = test_connection("snails", MIGRATIONS);
        // Author 7 caught twice in server 1, once long ago; author 8 once; server 2 apart.
        for (message, guild, author, at) in [
            (1, 1, 7, 1000),
            (2, 1, 7, 2000),
            (3, 1, 7, 10),
            (4, 1, 8, 2000),
            (5, 2, 9, 2000),
        ] {
            record_caught(&conn, guild, message, author, at).unwrap();
        }
        assert_eq!(top_snailers(&conn, 1, 0, 10).unwrap(), [(7, 3), (8, 1)]);
        assert_eq!(top_snailers(&conn, 1, 500, 10).unwrap(), [(7, 2), (8, 1)]);
        assert_eq!(top_snailers(&conn, 1, 0, 1).unwrap(), [(7, 3)]);
        assert_eq!(top_snailers(&conn, 2, 0, 10).unwrap(), [(9, 1)]);
        assert!(top_snailers(&conn, 1, 5000, 10).unwrap().is_empty());
    }
    use crate::features::snails::store::testing::*;
    // The rest of the store, which some of these tests use too.
    use crate::features::snails::store::*;

    #[test]
    fn links_and_pictures() {
        let mut conn = test_connection("snails", MIGRATIONS);
        save_now(&mut conn, &indexed(5, &["x:1"], vec![fp(0)]), 0);
        save_now(
            &mut conn,
            &indexed(7, &["x:1", "youtube:a"], vec![fp(0b1111)]),
            0,
        );
        // Nothing to store: not saved.
        save_now(&mut conn, &indexed(8, &[], vec![]), 0);
        assert!(is_indexed(&conn, 5).unwrap());
        assert!(!is_indexed(&conn, 8).unwrap());

        let keys = vec!["x:1".to_string()];
        let older = same_links(&conn, 1, &keys, 9).unwrap();
        assert_eq!(older.iter().map(|p| p.message).collect::<Vec<_>>(), [5, 7]);
        // Only older messages, only this server.
        assert_eq!(same_links(&conn, 1, &keys, 7).unwrap().len(), 1);
        assert!(same_links(&conn, 2, &keys, 9).unwrap().is_empty());

        // fp(0) vs fp(0b1111): 4 bits per byte differ, far over the limit.
        let (stored, next) = pictures_after(&conn, 1, 9, 0).unwrap();
        assert_eq!(next, None);
        assert_eq!(stored.len(), 2);
        let found = close_to(&[fp(0)], &stored);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].posted.message, found[0].distance), (5, 0));

        forget(&conn, 5).unwrap();
        assert!(
            same_links(&conn, 1, &keys, 9)
                .unwrap()
                .iter()
                .all(|p| p.message == 7)
        );
        let (stored, _) = pictures_after(&conn, 1, 9, 0).unwrap();
        assert!(close_to(&[fp(0)], &stored).is_empty());
    }

    #[test]
    fn counts_caught_snails() {
        let conn = test_connection("snails", MIGRATIONS);
        record_caught(&conn, 1, 10, 100, 1_000).unwrap();
        record_caught(&conn, 1, 11, 100, 5_000).unwrap();
        // The same message counts once.
        record_caught(&conn, 1, 11, 100, 5_000).unwrap();
        assert_eq!(caught_since(&conn, 0).unwrap(), 2);
        assert!(is_caught(&conn, 11).unwrap() && !is_caught(&conn, 12).unwrap());
        assert_eq!(caught_since(&conn, 2_000).unwrap(), 1);
    }

    #[test]
    fn reset_clears_one_server() {
        let mut conn = test_connection("snails", MIGRATIONS);
        save_now(&mut conn, &indexed(5, &["x:1"], vec![]), 0);
        record_caught(&conn, 1, 10, 100, 1_000).unwrap();
        record_caught(&conn, 1, 11, 101, 1_000).unwrap();
        record_caught(&conn, 2, 12, 100, 1_000).unwrap();
        assert_eq!(reset_caught(&conn, 1).unwrap(), 2);
        assert!(top_snailers(&conn, 1, 0, 10).unwrap().is_empty());
        assert_eq!(top_snailers(&conn, 2, 0, 10).unwrap(), [(100, 1)]);
        // The index stays.
        assert!(is_indexed(&conn, 5).unwrap());
    }
}
