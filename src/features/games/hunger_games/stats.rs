//! Hunger Games stats: every game that reaches a victor is saved, with each tribute's
//! place, kills, Octagons won and days lived through, and `/games leaderboard` shows the server's best,
//! or one member's record.
//!
//! Who killed whom is saved too, with the weapon and the place, so the next Games can bring
//! up old grudges and how they happened, and a member's record names their nemesis.
//!
//! Games that are cancelled or called off aren't saved, so only finished games count.

use anyhow::Context as _;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::id::Id;
use twilight_model::id::marker::UserMarker;

use super::game::{Blow, Game, Kind, PastKills};
use super::ui;
use crate::core::{CommandCtx, Result, user_error};
use crate::util::count;
use crate::util::discord::{Draft, no_pings};

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
    // 2
    "ALTER TABLE games_hunger_tributes ADD COLUMN octagons INTEGER NOT NULL DEFAULT 0;",
    // 3: who killed whom, for grudges and nemeses. One row per kill.
    "CREATE TABLE games_hunger_kills (
        game_id INTEGER NOT NULL REFERENCES games_hunger_games (id) ON DELETE CASCADE,
        killer_id INTEGER NOT NULL,
        victim_id INTEGER NOT NULL
    );
    CREATE INDEX games_hunger_kills_game ON games_hunger_kills (game_id);",
    // 4: how each kill happened, so revenge can call back to it. Kills saved before this
    // have no weapon (NULL); '' is bare hands.
    "ALTER TABLE games_hunger_kills ADD COLUMN weapon TEXT;
    ALTER TABLE games_hunger_kills ADD COLUMN place_id INTEGER;
    ALTER TABLE games_hunger_kills ADD COLUMN octagon INTEGER NOT NULL DEFAULT 0;",
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
    /// Octagons won.
    pub octagons: u32,
    pub days: u32,
}

/// How everyone did in a finished game, from the victors down. In a team Games both
/// winners are 1st, and the next is 3rd.
pub fn placings(game: &Game) -> Vec<Placing> {
    let winners = game.winners();
    game.placings()
        .into_iter()
        .enumerate()
        .map(|(place, i)| {
            let t = &game.tributes[i];
            Placing {
                user: t.id,
                place: if winners.contains(&i) {
                    1
                } else {
                    place as u32 + 1
                },
                kills: t.kills,
                octagons: game.octagon_wins(i),
                days: days_lived(game, t.died_in),
            }
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
            Kind::Scatter => 0,
        })
        .max()
        .unwrap_or(0)
}

/// Saves a finished game. Does nothing for a game with no victor. A team Games keeps its
/// first winner as the game's winner, and both as 1st in the tributes. `places` are the
/// arena's places (name and channel ID), to save where each kill happened.
pub fn record(
    conn: &mut Connection,
    guild: u64,
    channel: u64,
    game: &Game,
    places: &[(String, u64)],
) -> Result<()> {
    let Some(&winner) = game.winners().first() else {
        return Ok(());
    };
    let winner = &game.tributes[winner];
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
            "INSERT OR REPLACE INTO games_hunger_tributes
                 (game_id, user_id, place, kills, octagons, days)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, p.user, p.place, p.kills, p.octagons, p.days],
        )?;
    }
    for kill in game.kills() {
        let round = &game.rounds[kill.round];
        let text = &round.events[kill.event].text;
        tx.execute(
            "INSERT INTO games_hunger_kills (game_id, killer_id, victim_id, weapon, place_id, octagon)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                game.tributes[kill.killer].id,
                game.tributes[kill.victim].id,
                kill.weapon.unwrap_or_default(),
                ui::first_place(text, places),
                matches!(round.kind, Kind::Octagon(_)),
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Who killed whom in a server's earlier Games, among `ids`: (killer, victim, times, how
/// the last time happened), by killer and victim.
pub fn grudges(conn: &Connection, guild: u64, ids: &[u64]) -> rusqlite::Result<Vec<PastKills>> {
    let mut stmt = conn.prepare(
        "SELECT k.killer_id, k.victim_id, k.weapon, k.place_id, k.octagon
         FROM games_hunger_kills k JOIN games_hunger_games g ON g.id = k.game_id
         WHERE g.guild_id = ?1
         ORDER BY g.at, k.rowid",
    )?;
    let rows = stmt.query_map([guild], |row| {
        let weapon: Option<String> = row.get(2)?;
        let blow = match weapon {
            None => None,
            Some(weapon) => Some(Blow {
                weapon: (!weapon.is_empty()).then_some(weapon),
                place: row.get(3)?,
                octagon: row.get(4)?,
            }),
        };
        Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?, blow))
    })?;
    // Oldest first, so the last kill of each pair is the one kept.
    let mut pairs: std::collections::BTreeMap<(u64, u64), (u32, Option<Blow>)> =
        std::collections::BTreeMap::new();
    for row in rows {
        let (killer, victim, blow) = row?;
        if ids.contains(&killer) && ids.contains(&victim) {
            let pair = pairs.entry((killer, victim)).or_default();
            pair.0 += 1;
            if blow.is_some() {
                pair.1 = blow;
            }
        }
    }
    Ok(pairs
        .into_iter()
        .map(|((killer, victim), (times, last))| (killer, victim, times, last))
        .collect())
}

/// The member's nemesis in a server: whoever killed them most, at least twice (the most
/// recent first in a tie), and how many times.
fn nemesis(conn: &Connection, guild: u64, user: u64) -> rusqlite::Result<Option<Nemesis>> {
    conn.query_row(
        "SELECT k.killer_id, COUNT(*),
                (SELECT last.weapon
                 FROM games_hunger_kills last JOIN games_hunger_games lg ON lg.id = last.game_id
                 WHERE lg.guild_id = ?1 AND last.victim_id = ?2 AND last.killer_id = k.killer_id
                 ORDER BY lg.at DESC, last.rowid DESC
                 LIMIT 1)
         FROM games_hunger_kills k JOIN games_hunger_games g ON g.id = k.game_id
         WHERE g.guild_id = ?1 AND k.victim_id = ?2
         GROUP BY k.killer_id
         HAVING COUNT(*) >= 2
         ORDER BY 2 DESC, MAX(g.at) DESC
         LIMIT 1",
        params![guild, user],
        |row| {
            Ok(Nemesis {
                killer: row.get(0)?,
                times: row.get(1)?,
                weapon: row.get(2)?,
            })
        },
    )
    .optional()
}

/// Whoever killed a member most.
#[derive(Debug, Clone, PartialEq)]
struct Nemesis {
    killer: u64,
    times: u64,
    /// The weapon of their last kill of the member: '' for bare hands, `None` when it's
    /// from before kills kept it.
    weapon: Option<String>,
}

/// One line of the leaderboard.
#[derive(Debug, PartialEq)]
struct Ranked {
    user: u64,
    games: u64,
    wins: u64,
    kills: u64,
    octagons: u64,
}

/// The server's best: most wins, then most kills, then most Octagons won, then fewest
/// games.
fn top(conn: &Connection, guild: u64, limit: usize) -> rusqlite::Result<Vec<Ranked>> {
    let mut stmt = conn.prepare(
        "SELECT t.user_id, COUNT(*), SUM(t.place = 1), SUM(t.kills), SUM(t.octagons)
         FROM games_hunger_tributes t JOIN games_hunger_games g ON g.id = t.game_id
         WHERE g.guild_id = ?1
         GROUP BY t.user_id
         ORDER BY 3 DESC, 4 DESC, 5 DESC, 2 ASC
         LIMIT ?2",
    )?;
    stmt.query_map(params![guild, limit as i64], |row| {
        Ok(Ranked {
            user: row.get(0)?,
            games: row.get(1)?,
            wins: row.get(2)?,
            kills: row.get(3)?,
            octagons: row.get(4)?,
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
#[derive(Debug, Clone, PartialEq)]
struct Record {
    games: u64,
    wins: u64,
    kills: u64,
    most_kills: u64,
    octagons: u64,
    best_place: u64,
    average_place: f64,
    most_days: u64,
    /// Whoever killed them most, at least twice.
    nemesis: Option<Nemesis>,
}

fn record_of(conn: &Connection, guild: u64, user: u64) -> rusqlite::Result<Option<Record>> {
    conn.query_row(
        "SELECT COUNT(*), SUM(t.place = 1), SUM(t.kills), MAX(t.kills), MIN(t.place),
                AVG(t.place), MAX(t.days), SUM(t.octagons)
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
                octagons: row.get(7)?,
                nemesis: None,
            }))
        },
    )?
    .map(|record| {
        Ok(Record {
            nemesis: nemesis(conn, guild, user)?,
            ..record
        })
    })
    .transpose()
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "leaderboard",
    desc = "Hunger Games wins, kills and Octagons won in this server"
)]
pub struct Leaderboard {
    /// Show one member's record instead
    member: Option<Id<UserMarker>>,
}

/// `/games leaderboard`
pub async fn leaderboard(ctx: &CommandCtx, args: Leaderboard) -> Result<()> {
    let guild = ctx
        .guild_id()
        .context("server-only command without a server")?
        .get();
    let text = match args.member {
        Some(member) => {
            let user = member.get();
            let record = ctx
                .bot
                .db
                .call(move |conn| Ok(record_of(conn, guild, user)?))
                .await?;
            member_text(user, record.as_ref())
        }
        None => {
            let (ranked, games, best) = ctx
                .bot
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
    ctx.send(Draft::new().content(text).allowed_mentions(no_pings()))
        .await?;
    Ok(())
}

/// Deletes every saved game of a server, so the leaderboard starts over. How many games
/// were deleted.
fn reset(conn: &mut Connection, guild: u64) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM games_hunger_tributes WHERE game_id IN
             (SELECT id FROM games_hunger_games WHERE guild_id = ?1)",
        [guild],
    )?;
    let games = tx.execute(
        "DELETE FROM games_hunger_games WHERE guild_id = ?1",
        [guild],
    )?;
    tx.commit()?;
    Ok(games)
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "resetleaderboard",
    desc = "Start the Hunger Games leaderboard over (admins)"
)]
pub struct ResetLeaderboard;

/// `/games resetleaderboard`. Answers privately.
pub async fn reset_leaderboard(ctx: &CommandCtx) -> Result<()> {
    if !ctx.bot.is_admin(ctx.author_id()) {
        return Err(user_error("Only admins can use this command."));
    }
    let guild = ctx
        .guild_id()
        .context("server-only command without a server")?
        .get();
    let cleared = ctx.bot.db.call(move |conn| Ok(reset(conn, guild)?)).await?;
    tracing::info!(guild, cleared, "Hunger Games leaderboard reset");
    ctx.say(format!(
        "Reset: deleted {}. Everyone's wins, kills and Octagons start from zero.",
        count(cleared, "saved game")
    ))
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
        // Octagon wins only when there are some, so most lines stay short on a phone.
        let octagons = match r.octagons {
            0 => String::new(),
            n => format!(" · 🏟️ {}", count(n as usize, "Octagon")),
        };
        text.push_str(&format!(
            "{}. <@{}> · 👑 {} · 🗡️ {}{octagons} · {}\n",
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
    let nemesis = match &r.nemesis {
        Some(n) => {
            let last = match n.weapon.as_deref() {
                None => String::new(),
                Some(weapon) => format!(", the last time {}", ui::with_weapon(weapon)),
            };
            format!(
                "\n😈 Nemesis: <@{}>, who killed them {} times{last}",
                n.killer, n.times
            )
        }
        None => String::new(),
    };
    format!(
        "## 🏆 <@{user}> in the Hunger Games\n\
         👑 {} in {}\n\
         🗡️ {} (most in one game: {})\n\
         🏟️ {} won\n\
         🥇 Best finish: {} · average place {:.1}\n\
         📅 Longest run: {}{nemesis}",
        count(r.wins as usize, "win"),
        count(r.games as usize, "game"),
        count(r.kills as usize, "kill"),
        r.most_kills,
        count(r.octagons as usize, "Octagon"),
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

    fn places() -> Vec<(String, u64)> {
        vec![("den".into(), 50)]
    }

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
            reward: None,
            theme: None,
        }
    }

    /// Moss kills Ash in the Scatter and Fern on night 1, and wins; Reed wins the Octagon.
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
        // Moss grabs a spoon and kills Ash with it at once.
        let mut scatter = round(Kind::Scatter, vec![kill(1, 0)]);
        scatter.events[0].finds = vec![(0, "spoon".into())];
        game.apply(scatter);
        game.apply(round(Kind::Day(1), vec![]));
        // Reed wins the Octagon against Moss; Fern hangs back.
        game.apply(Round {
            entrants: vec![0, 3],
            champion: Some(3),
            ..round(Kind::Octagon(1), vec![])
        });
        let mut night = round(Kind::Night(1), vec![kill(2, 0)]);
        night.events[0].text = "{T1} gets {T3} in #den.".into();
        game.apply(night);
        // Reed falls to the arena in the showdown.
        game.apply(round(
            Kind::Day(2),
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
        let summary: Vec<(u64, u32, u32, u32, u32)> = placings
            .iter()
            .map(|p| (p.user, p.place, p.kills, p.octagons, p.days))
            .collect();
        assert_eq!(
            summary,
            vec![
                // Moss wins the showdown on day 2.
                (1, 1, 2, 0, 2),
                (4, 2, 0, 1, 1),
                (3, 3, 0, 0, 1),
                (2, 4, 0, 0, 0)
            ]
        );
    }

    #[test]
    fn both_of_a_winning_team_are_first() {
        let mut game = finished();
        // Moss and Ash were a team: Ash fell first but wins with Moss.
        game.tributes[0].partner = Some(1);
        game.tributes[1].partner = Some(0);
        let places: Vec<(u64, u32)> = placings(&game).iter().map(|p| (p.user, p.place)).collect();
        assert_eq!(places, vec![(1, 1), (2, 1), (4, 3), (3, 4)]);
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
        record(&mut conn, 7, 1, &finished(), &places()).unwrap();
        record(&mut conn, 7, 1, &finished(), &places()).unwrap();
        // Another server's game doesn't count here.
        record(&mut conn, 8, 1, &finished(), &places()).unwrap();
        // Nor does one without a victor.
        let unfinished = Game::new(vec![(1, "Moss".into()), (2, "Ash".into())]);
        record(&mut conn, 7, 1, &unfinished, &places()).unwrap();

        let ranked = top(&conn, 7, 10).unwrap();
        assert_eq!(
            ranked[0],
            Ranked {
                user: 1,
                games: 2,
                wins: 2,
                kills: 4,
                octagons: 0
            }
        );
        assert_eq!(ranked.len(), 4);
        assert_eq!(games_played(&conn, 7).unwrap(), 2);
        assert_eq!(most_kills(&conn, 7).unwrap(), Some((1, 2)));
        assert_eq!(most_kills(&conn, 9).unwrap(), None);

        // Resetting one server leaves the other alone.
        let mut other = test_connection("games", MIGRATIONS);
        record(&mut other, 7, 1, &finished(), &places()).unwrap();
        record(&mut other, 8, 1, &finished(), &places()).unwrap();
        assert_eq!(reset(&mut other, 7).unwrap(), 1);
        assert!(top(&other, 7, 10).unwrap().is_empty());
        assert_eq!(games_played(&other, 8).unwrap(), 1);
        assert_eq!(top(&other, 8, 10).unwrap().len(), 4);
        // The reset server's kills go with its games.
        assert!(grudges(&other, 7, &[1, 2, 3, 4]).unwrap().is_empty());
        assert_eq!(grudges(&other, 8, &[1, 2, 3, 4]).unwrap().len(), 2);

        let reed = record_of(&conn, 7, 4).unwrap().unwrap();
        assert_eq!(
            (reed.games, reed.wins, reed.octagons, reed.best_place),
            (2, 0, 2, 2)
        );
        assert_eq!(record_of(&conn, 7, 99).unwrap(), None);
    }

    #[test]
    fn remembers_who_killed_whom() {
        let mut conn = test_connection("games", MIGRATIONS);
        record(&mut conn, 7, 1, &finished(), &places()).unwrap();
        // Moss killed Ash and Fern; Reed died to the arena, so nobody gets a grudge for it.
        // Moss grabbed the spoon in the Scatter, then kept it for Fern in #den.
        let spoon = |place| Blow {
            weapon: Some("spoon".into()),
            place,
            octagon: false,
        };
        assert_eq!(
            grudges(&conn, 7, &[1, 2, 3, 4]).unwrap(),
            vec![
                (1, 2, 1, Some(spoon(None))),
                (1, 3, 1, Some(spoon(Some(50))))
            ]
        );
        // Only pairs who are both in the coming game.
        assert_eq!(
            grudges(&conn, 7, &[1, 3]).unwrap(),
            vec![(1, 3, 1, Some(spoon(Some(50))))]
        );
        assert!(grudges(&conn, 8, &[1, 2, 3, 4]).unwrap().is_empty());
        // One kill isn't a nemesis yet; two are.
        assert_eq!(nemesis(&conn, 7, 2).unwrap(), None);
        record(&mut conn, 7, 1, &finished(), &places()).unwrap();
        let moss = Nemesis {
            killer: 1,
            times: 2,
            weapon: Some("spoon".into()),
        };
        assert_eq!(nemesis(&conn, 7, 2).unwrap(), Some(moss.clone()));
        assert_eq!(record_of(&conn, 7, 2).unwrap().unwrap().nemesis, Some(moss));
        // Kills saved before weapons were kept still count, with no weapon to tell.
        conn.execute("UPDATE games_hunger_kills SET weapon = NULL", [])
            .unwrap();
        assert_eq!(nemesis(&conn, 7, 2).unwrap().unwrap().weapon, None);
        assert_eq!(grudges(&conn, 7, &[1, 2]).unwrap(), vec![(1, 2, 2, None)]);
        assert_eq!(record_of(&conn, 7, 1).unwrap().unwrap().nemesis, None);
    }

    #[test]
    fn writes_the_board() {
        let ranked = vec![
            Ranked {
                user: 5,
                games: 3,
                wins: 1,
                kills: 4,
                octagons: 0,
            },
            Ranked {
                user: 6,
                games: 3,
                wins: 0,
                kills: 1,
                octagons: 2,
            },
        ];
        let text = board(&ranked, 3, Some((5, 3)));
        assert!(text.contains("1. <@5> · 👑 1 win · 🗡️ 4 kills · 3 games"));
        assert!(text.contains("2. <@6> · 👑 0 wins · 🗡️ 1 kill · 🏟️ 2 Octagons · 3 games"));
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
            octagons: 1,
            best_place: 1,
            average_place: 2.75,
            most_days: 4,
            nemesis: None,
        };
        let text = member_text(5, Some(&record));
        assert!(!text.contains("Nemesis"));
        let haunted = Record {
            nemesis: Some(Nemesis {
                killer: 9,
                times: 3,
                weapon: None,
            }),
            ..record.clone()
        };
        assert!(
            member_text(5, Some(&haunted)).ends_with("\n😈 Nemesis: <@9>, who killed them 3 times")
        );
        let axed = Record {
            nemesis: Some(Nemesis {
                killer: 9,
                times: 3,
                weapon: Some("axe".into()),
            }),
            ..record
        };
        assert!(member_text(5, Some(&axed)).ends_with("3 times, the last time with an axe"));
        assert!(text.contains("👑 1 win in 4 games"));
        assert!(text.contains("🏟️ 1 Octagon won"));
        assert!(text.contains("Best finish: 1st · average place 2.8"));
        assert!(member_text(5, None).contains("hasn't finished"));
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(22), "22nd");
    }
}
