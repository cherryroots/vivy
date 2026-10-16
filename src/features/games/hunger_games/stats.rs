//! Hunger Games stats: every game that reaches a victor is saved, with each tribute's
//! place, kills and days lived through, and `/games leaderboard` shows the server's best,
//! or one member's record.
//!
//! Games that are cancelled or called off aren't saved, so only finished games count.

use anyhow::Context as _;
use chrono::Utc;
use poise::CreateReply;
use rusqlite::{Connection, OptionalExtension, params};
use serenity::all::{CreateAllowedMentions, User};

use super::game::{Game, Kind};
use crate::core::{Context, Result};
use crate::util::count;

pub const MIGRATIONS: &[&str] = &[
    // 1
    "CREATE TABLE games_hunger_games (
        id INTEGER PRIMARY KEY,
        guild_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL,
        winner_id INTEGER NOT NULL,
        rounds INTEGER NOT NULL,
        at INTEGER NOT NULL
    );
    CREATE INDEX games_hunger_games_guild ON games_hunger_games (guild_id);
    CREATE TABLE games_hunger_tributes (
        game_id INTEGER NOT NULL REFERENCES games_hunger_games (id) ON DELETE CASCADE,
        user_id INTEGER NOT NULL,
        -- 1 is the victor.
        place INTEGER NOT NULL,
        kills INTEGER NOT NULL,
        -- Days lived through: 0 for dying in the Bloodbath.
        days INTEGER NOT NULL,
        PRIMARY KEY (game_id, user_id)
    );",
];

/// How many people the leaderboard shows.
const TOP: usize = 10;

/// How one tribute did in a finished game.
#[derive(Debug, Clone, PartialEq)]
pub struct Placing {
    pub user: u64,
    /// 1 is the victor.
    pub place: u32,
    pub kills: u32,
    pub days: u32,
}

/// How everyone did in a finished game, from the victor down.
pub fn placings(game: &Game) -> Vec<Placing> {
    game.placings()
        .iter()
        .enumerate()
        .map(|(i, t)| Placing {
            user: t.id,
            place: i as u32 + 1,
            kills: t.kills,
            days: days_lived(game, t.died_in),
        })
        .collect()
}

/// The days someone lived through, dying in round `died_in` (or never): the highest day of
/// the rounds before that one. A Day or Octagon and its Night both count as that day, so dying
/// on night 2 is 2 days and dying on day 2 is 1.
fn days_lived(game: &Game, died_in: Option<usize>) -> u32 {
    let end = died_in.unwrap_or(game.rounds.len());
    game.rounds[..end.min(game.rounds.len())]
        .iter()
        .map(|r| match r.kind {
            Kind::Day(n) | Kind::Night(n) | Kind::Octagon(n) => n,
            Kind::Scatter | Kind::Finale => 0,
        })
        .max()
        .unwrap_or(0)
}

/// Saves a finished game. Does nothing for a game with no victor.
pub fn record(conn: &mut Connection, guild: u64, channel: u64, game: &Game) -> Result<()> {
    let Some(winner) = game.winner() else {
        return Ok(());
    };
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO games_hunger_games (guild_id, channel_id, winner_id, rounds, at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            guild,
            channel,
            winner.id,
            game.rounds.len(),
            Utc::now().timestamp()
        ],
    )?;
    let id = tx.last_insert_rowid();
    for p in placings(game) {
        tx.execute(
            "INSERT OR REPLACE INTO games_hunger_tributes (game_id, user_id, place, kills, days)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, p.user, p.place, p.kills, p.days],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// One line of the leaderboard.
#[derive(Debug, PartialEq)]
struct Ranked {
    user: u64,
    games: u64,
    wins: u64,
    kills: u64,
}

/// The server's best: most wins, then most kills, then fewest games.
fn top(conn: &Connection, guild: u64, limit: usize) -> rusqlite::Result<Vec<Ranked>> {
    let mut stmt = conn.prepare(
        "SELECT t.user_id, COUNT(*), SUM(t.place = 1), SUM(t.kills)
         FROM games_hunger_tributes t JOIN games_hunger_games g ON g.id = t.game_id
         WHERE g.guild_id = ?1
         GROUP BY t.user_id
         ORDER BY 3 DESC, 4 DESC, 2 ASC
         LIMIT ?2",
    )?;
    stmt.query_map(params![guild, limit as i64], |row| {
        Ok(Ranked {
            user: row.get(0)?,
            games: row.get(1)?,
            wins: row.get(2)?,
            kills: row.get(3)?,
        })
    })?
    .collect()
}

/// How many games the server has finished.
fn games_played(conn: &Connection, guild: u64) -> rusqlite::Result<u64> {
    conn.query_row(
        "SELECT COUNT(*) FROM games_hunger_games WHERE guild_id = ?1",
        [guild],
        |row| row.get(0),
    )
}

/// The most kills anyone got in one game, and who (the first to get that many).
fn most_kills(conn: &Connection, guild: u64) -> rusqlite::Result<Option<(u64, u64)>> {
    conn.query_row(
        "SELECT t.user_id, t.kills
         FROM games_hunger_tributes t JOIN games_hunger_games g ON g.id = t.game_id
         WHERE g.guild_id = ?1 AND t.kills > 0
         ORDER BY t.kills DESC, g.at ASC
         LIMIT 1",
        [guild],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// One member's record in a server.
#[derive(Debug, PartialEq)]
struct Record {
    games: u64,
    wins: u64,
    kills: u64,
    most_kills: u64,
    best_place: u64,
    average_place: f64,
    most_days: u64,
}

fn record_of(conn: &Connection, guild: u64, user: u64) -> rusqlite::Result<Option<Record>> {
    conn.query_row(
        "SELECT COUNT(*), SUM(t.place = 1), SUM(t.kills), MAX(t.kills), MIN(t.place),
                AVG(t.place), MAX(t.days)
         FROM games_hunger_tributes t JOIN games_hunger_games g ON g.id = t.game_id
         WHERE g.guild_id = ?1 AND t.user_id = ?2",
        params![guild, user],
        |row| {
            let games: u64 = row.get(0)?;
            if games == 0 {
                return Ok(None);
            }
            Ok(Some(Record {
                games,
                wins: row.get(1)?,
                kills: row.get(2)?,
                most_kills: row.get(3)?,
                best_place: row.get(4)?,
                average_place: row.get(5)?,
                most_days: row.get(6)?,
            }))
        },
    )
}

/// Hunger Games wins and kills in this server
#[poise::command(slash_command, guild_only)]
pub async fn leaderboard(
    ctx: Context<'_>,
    #[description = "Show one member's record instead"] member: Option<User>,
) -> Result<()> {
    let guild = ctx
        .guild_id()
        .context("guild_only command without a server")?
        .get();
    let text = match member {
        Some(member) => {
            let user = member.id.get();
            let record = ctx
                .data()
                .db
                .call(move |conn| Ok(record_of(conn, guild, user)?))
                .await?;
            member_text(user, record.as_ref())
        }
        None => {
            let (ranked, games, best) = ctx
                .data()
                .db
                .call(move |conn| {
                    Ok((
                        top(conn, guild, TOP)?,
                        games_played(conn, guild)?,
                        most_kills(conn, guild)?,
                    ))
                })
                .await?;
            board(&ranked, games, best)
        }
    };
    // Mentions show names without pinging anyone.
    ctx.send(
        CreateReply::default()
            .content(text)
            .allowed_mentions(CreateAllowedMentions::new()),
    )
    .await?;
    Ok(())
}

/// The leaderboard message.
fn board(ranked: &[Ranked], games: u64, best: Option<(u64, u64)>) -> String {
    if ranked.is_empty() {
        return "🏆 No Hunger Games finished here yet. Start one with `/games hungergames`."
            .to_string();
    }
    let mut text = String::from("## 🏆 Hunger Games leaderboard\n");
    for (i, r) in ranked.iter().enumerate() {
        text.push_str(&format!(
            "{}. <@{}> · 👑 {} · 🗡️ {} · {}\n",
            i + 1,
            r.user,
            count(r.wins as usize, "win"),
            count(r.kills as usize, "kill"),
            count(r.games as usize, "game"),
        ));
    }
    text.push_str(&format!("-# {} played", count(games as usize, "game")));
    if let Some((user, kills)) = best {
        text.push_str(&format!(". Most kills in one game: <@{user}> with {kills}"));
    }
    text.push('.');
    text
}

/// One member's record as a message.
fn member_text(user: u64, record: Option<&Record>) -> String {
    let Some(r) = record else {
        return format!("🏆 <@{user}> hasn't finished a Hunger Games here yet.");
    };
    format!(
        "## 🏆 <@{user}> in the Hunger Games\n\
         👑 {} in {}\n\
         🗡️ {} (most in one game: {})\n\
         🥇 Best finish: {} · average place {:.1}\n\
         📅 Longest run: {}",
        count(r.wins as usize, "win"),
        count(r.games as usize, "game"),
        count(r.kills as usize, "kill"),
        r.most_kills,
        ordinal(r.best_place),
        r.average_place,
        count(r.most_days as usize, "day"),
    )
}

/// 1st, 2nd, 3rd, 4th, ..., 11th, 12th, 13th, 21st.
fn ordinal(n: u64) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::test_connection;
    use crate::features::games::hunger_games::game::{Death, Event, Round};

    fn round(kind: Kind, deaths: Vec<Death>) -> Round {
        Round {
            kind,
            gamemaker: None,
            events: vec![Event {
                text: String::new(),
                deaths,
                ..Default::default()
            }],
            victory: None,
            picture: None,
            entrants: Vec::new(),
            champion: None,
        }
    }

    /// Moss kills Ash in the Scatter and Fern on night 1, and wins.
    fn finished() -> Game {
        let mut game = Game::new(vec![
            (1, "Moss".into()),
            (2, "Ash".into()),
            (3, "Fern".into()),
            (4, "Reed".into()),
        ]);
        let kill = |victim, by| Death {
            victim,
            by: Some(by),
        };
        game.apply(round(Kind::Scatter, vec![kill(1, 0)]));
        game.apply(round(Kind::Day(1), vec![]));
        game.apply(round(Kind::Night(1), vec![kill(2, 0)]));
        game.apply(round(
            Kind::Finale,
            vec![Death {
                victim: 3,
                by: None,
            }],
        ));
        game
    }

    #[test]
    fn places_everyone_with_their_days() {
        let placings = placings(&finished());
        let summary: Vec<(u64, u32, u32, u32)> = placings
            .iter()
            .map(|p| (p.user, p.place, p.kills, p.days))
            .collect();
        assert_eq!(
            summary,
            vec![(1, 1, 2, 1), (4, 2, 0, 1), (3, 3, 0, 1), (2, 4, 0, 0)]
        );
    }

    #[test]
    fn counts_days_lived() {
        let mut game = Game::new(vec![(1, "Moss".into()), (2, "Ash".into())]);
        for kind in [
            Kind::Scatter,
            Kind::Day(1),
            Kind::Night(1),
            Kind::Octagon(2),
            Kind::Night(2),
            Kind::Day(3),
        ] {
            game.rounds.push(round(kind, vec![]));
        }
        assert_eq!(days_lived(&game, Some(0)), 0);
        assert_eq!(days_lived(&game, Some(1)), 0);
        assert_eq!(days_lived(&game, Some(3)), 1);
        assert_eq!(days_lived(&game, Some(5)), 2);
        assert_eq!(days_lived(&game, None), 3);
    }

    #[test]
    fn ranks_wins_then_kills() {
        let mut conn = test_connection("games", MIGRATIONS);
        record(&mut conn, 7, 1, &finished()).unwrap();
        record(&mut conn, 7, 1, &finished()).unwrap();
        // Another server's game doesn't count here.
        record(&mut conn, 8, 1, &finished()).unwrap();
        // Nor does one without a victor.
        let unfinished = Game::new(vec![(1, "Moss".into()), (2, "Ash".into())]);
        record(&mut conn, 7, 1, &unfinished).unwrap();

        let ranked = top(&conn, 7, 10).unwrap();
        assert_eq!(
            ranked[0],
            Ranked {
                user: 1,
                games: 2,
                wins: 2,
                kills: 4
            }
        );
        assert_eq!(ranked.len(), 4);
        assert_eq!(games_played(&conn, 7).unwrap(), 2);
        assert_eq!(most_kills(&conn, 7).unwrap(), Some((1, 2)));
        assert_eq!(most_kills(&conn, 9).unwrap(), None);

        let reed = record_of(&conn, 7, 4).unwrap().unwrap();
        assert_eq!((reed.games, reed.wins, reed.best_place), (2, 0, 2));
        assert_eq!(record_of(&conn, 7, 99).unwrap(), None);
    }

    #[test]
    fn writes_the_board() {
        let ranked = vec![Ranked {
            user: 5,
            games: 3,
            wins: 1,
            kills: 4,
        }];
        let text = board(&ranked, 3, Some((5, 3)));
        assert!(text.contains("1. <@5> · 👑 1 win · 🗡️ 4 kills · 3 games"));
        assert!(text.ends_with("-# 3 games played. Most kills in one game: <@5> with 3."));
        assert!(board(&[], 0, None).contains("No Hunger Games finished"));
    }

    #[test]
    fn writes_a_members_record() {
        let record = Record {
            games: 4,
            wins: 1,
            kills: 5,
            most_kills: 3,
            best_place: 1,
            average_place: 2.75,
            most_days: 4,
        };
        let text = member_text(5, Some(&record));
        assert!(text.contains("👑 1 win in 4 games"));
        assert!(text.contains("Best finish: 1st · average place 2.8"));
        assert!(member_text(5, None).contains("hasn't finished"));
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(22), "22nd");
    }
}
