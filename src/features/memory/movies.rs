//! Logs resolved movie wheel rounds into `/memories/server/movies-watched.md`.
//!
//! The wheel publishes [`crate::core::BotEvent::WheelRoundResolved`] when an admin picks a
//! winner; memory listens here so Vivy remembers what the server watched without the two
//! features importing each other.

use chrono::Utc;
use rusqlite::Connection;
use tracing::{info, warn};

use super::folder::{self, MAX_FILE};
use super::store;
use crate::core::{BotCtx, Result};
use crate::util::discord::{GuildId, UserId};

/// The server memory file where watched movies are recorded.
pub const PATH: &str = "/memories/server/movies-watched.md";
const HEADER: &str = "# Watched movies (movie wheel)";

/// Records a resolved wheel round in the server's memory folder, when `memory` is allowed in
/// that server.
pub async fn record(
    ctx: &BotCtx,
    guild_id: GuildId,
    season: i64,
    round: i64,
    winner: UserId,
    winner_name: &str,
    movie: Option<&str>,
) -> Result<()> {
    if !ctx.gate("memory").allows_guild(guild_id) {
        return Ok(());
    }
    let scope = store::scope(Some(guild_id.get()), ctx.bot_id.get());
    let bot_id = ctx.bot_id.get();
    let now = Utc::now();
    let date = now.format("%Y-%m-%d").to_string();
    let timestamp = now.timestamp();
    let winner_name = winner_name.to_string();
    let movie = movie.map(str::to_string);

    let saved = ctx
        .db
        .call(move |conn| {
            record_in_db(
                conn,
                &scope,
                bot_id,
                timestamp,
                &date,
                season,
                round,
                &winner_name,
                movie.as_deref(),
            )
        })
        .await?;

    if saved {
        info!(
            guild = guild_id.get(),
            season,
            round,
            winner = winner.get(),
            "logged wheel round to {PATH}"
        );
    }
    Ok(())
}

/// Loads the folder, updates [`PATH`], checks folder limits, and saves inside one SQLite
/// transaction. Returns `false` if the folder's size or file-count limit prevented saving.
#[allow(clippy::too_many_arguments)]
fn record_in_db(
    conn: &mut Connection,
    scope: &str,
    bot_id: u64,
    now: i64,
    date: &str,
    season: i64,
    round: i64,
    winner_name: &str,
    movie: Option<&str>,
) -> Result<bool> {
    let tx = conn.transaction()?;
    let before = store::load(&tx, scope)?;
    let mut after = before.clone();
    let updated = update_content(
        before.get(PATH).map(String::as_str),
        date,
        season,
        round,
        winner_name,
        movie,
    );
    after.insert(PATH.to_string(), updated);
    if let Err(err) = folder::check_limits(&before, &after) {
        warn!("skipping {PATH} update in {scope}: {err}");
        return Ok(false);
    }
    store::save(&tx, scope, &before, &after, bot_id, now)?;
    tx.commit()?;
    Ok(true)
}

/// Formats one bullet entry for a resolved round.
fn format_entry(
    date: &str,
    season: i64,
    round: i64,
    winner_name: &str,
    movie: Option<&str>,
) -> String {
    match movie.map(str::trim).filter(|m| !m.is_empty()) {
        Some(movie) => {
            format!("- {date}: **{movie}** (picked by {winner_name}, S{season} R{round})")
        }
        None => format!("- {date}: {winner_name} won the wheel (S{season} R{round})"),
    }
}

/// Whether `line` is the entry for `S<season> R<round>`.
fn is_same_round(line: &str, season: i64, round: i64) -> bool {
    let tag = format!("S{season} R{round})");
    line.starts_with("- ")
        && (line.ends_with(&format!(", {tag}")) || line.ends_with(&format!("({tag}")))
}

fn render(lines: &[String]) -> String {
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Produces the updated contents of `/memories/server/movies-watched.md`:
/// - Creates the file with [`HEADER`] when it doesn't exist yet.
/// - Replaces an existing `S<season> R<round>` line in place (e.g. after Undo Winner).
/// - Trims the oldest `- ...` entries from the top if needed to stay within [`MAX_FILE`].
fn update_content(
    existing: Option<&str>,
    date: &str,
    season: i64,
    round: i64,
    winner_name: &str,
    movie: Option<&str>,
) -> String {
    let mut lines: Vec<String> = match existing {
        Some(text) if !text.trim().is_empty() => text.lines().map(str::to_string).collect(),
        _ => vec![HEADER.to_string()],
    };

    let entry = format_entry(date, season, round, winner_name, movie);
    let mut written_idx = if let Some(pos) = lines
        .iter()
        .position(|line| is_same_round(line, season, round))
    {
        lines[pos] = entry;
        pos
    } else {
        lines.push(entry);
        lines.len() - 1
    };

    while render(&lines).len() > MAX_FILE {
        // Drop the oldest bullet entry from the top, keeping the entry we just wrote.
        let Some(oldest) = lines
            .iter()
            .enumerate()
            .position(|(idx, line)| idx != written_idx && line.starts_with("- "))
        else {
            break;
        };
        lines.remove(oldest);
        if oldest < written_idx {
            written_idx -= 1;
        }
    }

    render(&lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::memory::folder::Folder;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::core::db::migrate(&mut conn, "memory", store::MIGRATIONS).unwrap();
        conn
    }

    #[test]
    fn creates_file_with_header_and_movie() {
        let text = update_content(None, "2026-11-25", 1, 1, "Alice", Some("Spirited Away"));
        assert_eq!(
            text,
            "# Watched movies (movie wheel)\n\
             - 2026-11-25: **Spirited Away** (picked by Alice, S1 R1)\n"
        );
    }

    #[test]
    fn appends_with_and_without_movie() {
        let first = update_content(None, "2026-11-18", 2, 1, "Alice", Some("Akira"));
        let second = update_content(Some(&first), "2026-11-25", 2, 2, "Bob", None);
        assert_eq!(
            second,
            "# Watched movies (movie wheel)\n\
             - 2026-11-18: **Akira** (picked by Alice, S2 R1)\n\
             - 2026-11-25: Bob won the wheel (S2 R2)\n"
        );
    }

    #[test]
    fn replaces_existing_round_in_place_without_duplicating() {
        let initial = "\
# Watched movies (movie wheel)
- 2026-11-10: **Old Pick** (picked by Alice, S1 R1)
- 2026-11-17: **Second Movie** (picked by Bob, S1 R2)
- 2026-11-24: **Tenth Movie** (picked by Cara, S1 R10)
";
        let updated = update_content(Some(initial), "2026-11-11", 1, 1, "Dana", None);
        assert_eq!(
            updated,
            "# Watched movies (movie wheel)\n\
             - 2026-11-11: Dana won the wheel (S1 R1)\n\
             - 2026-11-17: **Second Movie** (picked by Bob, S1 R2)\n\
             - 2026-11-24: **Tenth Movie** (picked by Cara, S1 R10)\n"
        );

        // Replacing a no-movie entry with one that has a movie also works in place.
        let again = update_content(
            Some(&updated),
            "2026-11-11",
            1,
            1,
            "Dana",
            Some("Perfect Blue"),
        );
        assert_eq!(
            again,
            "# Watched movies (movie wheel)\n\
             - 2026-11-11: **Perfect Blue** (picked by Dana, S1 R1)\n\
             - 2026-11-17: **Second Movie** (picked by Bob, S1 R2)\n\
             - 2026-11-24: **Tenth Movie** (picked by Cara, S1 R10)\n"
        );
    }

    #[test]
    fn trims_oldest_entries_to_stay_within_max_file() {
        let mut content = String::new();
        for round in 1..=160 {
            let title = format!("Movie Title Number {round:03} — The Extended Cut");
            content = update_content(
                if content.is_empty() {
                    None
                } else {
                    Some(&content)
                },
                "2026-11-25",
                1,
                round,
                "Alice",
                Some(&title),
            );
            assert!(
                content.len() <= MAX_FILE,
                "round {round} exceeded MAX_FILE ({} > {MAX_FILE})",
                content.len()
            );
        }

        // Header stays, oldest entries were trimmed, newest entry is preserved.
        assert!(content.starts_with("# Watched movies (movie wheel)\n"));
        assert!(!content.contains("S1 R1)"));
        assert!(content.contains("S1 R160)"));

        // Verify check_limits accepts the trimmed file.
        let before = Folder::new();
        let mut after = Folder::new();
        after.insert(PATH.to_string(), content);
        assert!(folder::check_limits(&before, &after).is_ok());
    }

    #[test]
    fn saves_in_db_under_bot_id() {
        let mut conn = db();
        let ok = record_in_db(
            &mut conn,
            "server:42",
            999,
            1_700_000_000,
            "2026-11-25",
            3,
            4,
            "Cherry",
            Some("Paprika"),
        )
        .unwrap();
        assert!(ok);

        let loaded = store::load(&conn, "server:42").unwrap();
        assert_eq!(
            loaded.get(PATH).map(String::as_str),
            Some(
                "# Watched movies (movie wheel)\n\
                 - 2026-11-25: **Paprika** (picked by Cherry, S3 R4)\n"
            )
        );
        // Written by bot_id (999), so it doesn't falsely trigger a reflection by itself.
        assert!(
            store::due_reflections(&conn, 1_700_100_000, 86_400, 999)
                .unwrap()
                .is_empty()
        );
    }
}
