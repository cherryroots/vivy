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

/// How many stored pictures [`close_pictures`] reads per call, so other database work can
/// run in between on a big server.
pub const SCAN_CHUNK: i64 = 20_000;

/// One chunk of the scan for older pictures within [`MATCH_DISTANCE`] of `checked`. Reads
/// pictures after `rowid` and returns the candidates plus the rowid to continue from, or
/// None when the scan is done.
pub fn close_pictures(
    conn: &Connection,
    guild: u64,
    checked: &[Fingerprint],
    before: u64,
    after_rowid: i64,
) -> rusqlite::Result<(Vec<Candidate>, Option<i64>)> {
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
        let stored = Fingerprint {
            aspect: row.get(3)?,
            hashes,
        };
        for (i, fp) in checked.iter().enumerate() {
            let d = distance(fp, &stored);
            if d <= MATCH_DISTANCE {
                found.push(Candidate {
                    posted: Posted {
                        message: row.get(1)?,
                        channel: row.get(5)?,
                        author: row.get(6)?,
                    },
                    position: row.get::<_, i64>(2)? as usize,
                    checked: i,
                    distance: d,
                });
            }
        }
    }
    let next = (count == SCAN_CHUNK).then_some(last);
    Ok((found, next))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::test_connection;
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
        let (found, next) = close_pictures(&conn, 1, &[fp(0)], 9, 0).unwrap();
        assert_eq!(next, None);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].posted.message, found[0].distance), (5, 0));

        forget(&conn, 5).unwrap();
        assert!(
            same_links(&conn, 1, &keys, 9)
                .unwrap()
                .iter()
                .all(|p| p.message == 7)
        );
        assert!(
            close_pictures(&conn, 1, &[fp(0)], 9, 0)
                .unwrap()
                .0
                .is_empty()
        );
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
}
