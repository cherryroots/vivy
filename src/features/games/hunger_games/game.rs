//! The rules of a Hunger Games, with no Discord or AI in it, so the tests can play a whole
//! game in a few lines.
//!
//! Vivy writes every round (see `writer.rs`), and this file decides whether what she wrote
//! can happen: every living tribute is in exactly one event, only the living die, and about
//! as many die as the pacing asks for. A round that passes is applied: the dead are marked
//! and the killers get their kills.
//!
//! In what she writes, tributes are tags like `T3` (the third tribute), never names, so a
//! name can't be misspelled or mixed up. `{T3}` in an event's text becomes the name when
//! it's shown.

use serde::Deserialize;

/// Someone in the arena.
#[derive(Debug, Clone, PartialEq)]
pub struct Tribute {
    pub id: u64,
    pub name: String,
    pub kills: u32,
    /// The round they died in (0 is the first), or `None` while alive.
    pub died_in: Option<usize>,
}

impl Tribute {
    pub fn alive(&self) -> bool {
        self.died_in.is_none()
    }
}

/// What kind of round it is. Days and nights are numbered from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The first round: everyone runs for the supplies in the middle.
    Bloodbath,
    Day(u32),
    Night(u32),
    /// Once, instead of the second day: a feast of supplies that draws everyone in.
    Feast(u32),
    /// The last two, until one is left.
    Finale,
}

impl Kind {
    /// Whether Vivy says a Gamemaker line before this round: when the Games open, at the
    /// Feast, and for the final two.
    pub fn announced(self) -> bool {
        matches!(self, Kind::Bloodbath | Kind::Feast(_) | Kind::Finale)
    }

    /// Whether she draws a picture of this round, when pictures are on: the arena as the
    /// Games open, each day's most dramatic moment, and the victor. Nights aren't drawn.
    pub fn pictured(self) -> bool {
        !matches!(self, Kind::Night(_))
    }
}

/// Someone dying in an event: who (index into the tributes), and who killed them, if
/// anyone did.
#[derive(Debug, Clone, PartialEq)]
pub struct Death {
    pub victim: usize,
    pub by: Option<usize>,
}

/// One thing that happens to one or more tributes.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The text, with `{T1}`-style tags where names go.
    pub text: String,
    pub deaths: Vec<Death>,
}

/// A round as it's played.
#[derive(Debug, Clone, PartialEq)]
pub struct Round {
    pub kind: Kind,
    /// Vivy's Gamemaker line before the events (see [`Kind::announced`]).
    pub gamemaker: Option<String>,
    pub events: Vec<Event>,
    /// In the Finale, what she says to the winner.
    pub victory: Option<String>,
    /// What to draw for this round (see [`Kind::pictured`]), when she was asked to.
    pub picture: Option<String>,
}

/// A round as Vivy writes it: JSON, with tributes as tags.
#[derive(Debug, Deserialize)]
pub struct Written {
    #[serde(default)]
    pub gamemaker: Option<String>,
    pub events: Vec<WrittenEvent>,
    #[serde(default)]
    pub victory: Option<String>,
    #[serde(default)]
    pub picture: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WrittenEvent {
    pub text: String,
    pub tributes: Vec<String>,
    #[serde(default)]
    pub deaths: Vec<WrittenDeath>,
}

#[derive(Debug, Deserialize)]
pub struct WrittenDeath {
    pub tribute: String,
    #[serde(default)]
    pub by: Option<String>,
}

/// The longest an event's text may be, in characters.
const MAX_EVENT: usize = 400;
/// The most tributes in one event.
const MAX_IN_EVENT: usize = 6;

/// The whole game: the tributes and the rounds played so far.
#[derive(Debug, Clone)]
pub struct Game {
    pub tributes: Vec<Tribute>,
    pub rounds: Vec<Round>,
}

impl Game {
    /// A game with these tributes, as (user ID, name).
    pub fn new(tributes: Vec<(u64, String)>) -> Self {
        Game {
            tributes: tributes
                .into_iter()
                .map(|(id, name)| Tribute {
                    id,
                    name,
                    kills: 0,
                    died_in: None,
                })
                .collect(),
            rounds: Vec::new(),
        }
    }

    /// How many are still alive.
    pub fn alive(&self) -> usize {
        self.tributes.iter().filter(|t| t.alive()).count()
    }

    /// The winner, once only one is left.
    pub fn winner(&self) -> Option<&Tribute> {
        let mut alive = self.tributes.iter().filter(|t| t.alive());
        let first = alive.next()?;
        alive.next().is_none().then_some(first)
    }

    /// The round to play next, or `None` when the game is over.
    pub fn next_kind(&self) -> Option<Kind> {
        let alive = self.alive();
        if alive <= 1 {
            return None;
        }
        if alive == 2 {
            return Some(Kind::Finale);
        }
        let feast_done = self.rounds.iter().any(|r| matches!(r.kind, Kind::Feast(_)));
        Some(match self.rounds.last().map(|r| r.kind) {
            None => Kind::Bloodbath,
            Some(Kind::Bloodbath) => Kind::Day(1),
            Some(Kind::Day(day) | Kind::Feast(day)) => Kind::Night(day),
            Some(Kind::Night(day)) if day + 1 == 2 && !feast_done && alive >= 4 => {
                Kind::Feast(day + 1)
            }
            Some(Kind::Night(day)) => Kind::Day(day + 1),
            // A Finale always leaves one, so nothing follows it.
            Some(Kind::Finale) => return None,
        })
    }

    /// How many should die in the next round, of `kind`, at least and at most. At most
    /// about a fifth of the living in the Bloodbath, a quarter at the Feast, and an eighth
    /// to a sixth on days and nights, so 24 tributes take around 15 rounds and 5 take
    /// around 6. A day or night may pass with nobody dying, but never two in a row. Until
    /// the Finale at least two stay alive, so there's always a final two.
    pub fn deaths_range(&self, kind: Kind) -> (usize, usize) {
        let alive = self.alive();
        if kind == Kind::Finale || alive <= 2 {
            return (1, 1);
        }
        let share = match kind {
            Kind::Bloodbath => 0.2,
            Kind::Day(_) => 0.12,
            Kind::Night(_) => 0.15,
            Kind::Feast(_) => 0.25,
            Kind::Finale => unreachable!(),
        };
        let most = alive - 2;
        let max = ((alive as f64 * share).ceil() as usize).clamp(1, most);
        let last_was_quiet = self
            .rounds
            .last()
            .is_none_or(|r| r.events.iter().all(|e| e.deaths.is_empty()));
        let may_be_quiet = matches!(kind, Kind::Day(_) | Kind::Night(_)) && !last_was_quiet;
        let floor = max / 2;
        let min = if floor == 0 && may_be_quiet {
            0
        } else {
            floor.max(1)
        };
        (min, max)
    }

    /// The tag of a tribute in what Vivy writes: `T1` for the first.
    pub fn tag(index: usize) -> String {
        format!("T{}", index + 1)
    }

    /// The tribute a tag points at, if it's one of them.
    fn index_of(&self, tag: &str) -> Option<usize> {
        let number: usize = tag
            .trim()
            .trim_matches(['{', '}'])
            .strip_prefix('T')?
            .parse()
            .ok()?;
        (1..=self.tributes.len())
            .contains(&number)
            .then(|| number - 1)
    }

    /// Checks a round Vivy wrote for `kind`. `Err` says what's wrong in a sentence she can
    /// fix it from.
    pub fn check(&self, kind: Kind, written: Written) -> Result<Round, String> {
        if written.events.is_empty() {
            return Err("There are no events.".to_string());
        }
        let mut seen = vec![false; self.tributes.len()];
        let mut events = Vec::new();
        let mut dead = 0;
        for event in written.events {
            let text = event.text.trim().to_string();
            if text.is_empty() {
                return Err("An event has no text.".to_string());
            }
            if text.chars().count() > MAX_EVENT {
                return Err(format!(
                    "An event is longer than {MAX_EVENT} characters: keep each to a sentence or two."
                ));
            }
            if event.tributes.is_empty() || event.tributes.len() > MAX_IN_EVENT {
                return Err(format!(
                    "Each event has 1 to {MAX_IN_EVENT} tributes; one has {}.",
                    event.tributes.len()
                ));
            }
            let mut members = Vec::new();
            for tag in &event.tributes {
                let Some(index) = self.index_of(tag) else {
                    return Err(format!("{tag} isn't one of the tributes."));
                };
                if !self.tributes[index].alive() {
                    return Err(format!("{tag} is already dead."));
                }
                if seen[index] {
                    return Err(format!(
                        "{tag} is in more than one event; each living tribute is in exactly one."
                    ));
                }
                seen[index] = true;
                members.push(index);
            }
            let tagged = tags_in(&text);
            for index in &members {
                if !tagged.contains(index) {
                    return Err(format!(
                        "{} is in an event's tributes but its text has no {{{}}}.",
                        Game::tag(*index),
                        Game::tag(*index)
                    ));
                }
            }
            if let Some(stray) = tagged.iter().find(|i| !members.contains(i)) {
                return Err(format!(
                    "An event's text names {{{}}}, who isn't in its tributes.",
                    Game::tag(*stray)
                ));
            }
            let mut deaths: Vec<Death> = Vec::new();
            for death in event.deaths {
                let victim = self
                    .index_of(&death.tribute)
                    .filter(|i| members.contains(i))
                    .ok_or_else(|| format!("{} dies in an event they aren't in.", death.tribute))?;
                if deaths.iter().any(|d| d.victim == victim) {
                    return Err(format!("{} dies twice.", death.tribute));
                }
                let by = match death.by.as_deref().map(str::trim) {
                    None | Some("") => None,
                    Some(tag) => Some(
                        self.index_of(tag)
                            .filter(|i| members.contains(i) && *i != victim)
                            .ok_or_else(|| {
                                format!(
                                    "{tag} kills {} but isn't another tribute in that event.",
                                    death.tribute
                                )
                            })?,
                    ),
                };
                deaths.push(Death { victim, by });
            }
            dead += deaths.len();
            events.push(Event { text, deaths });
        }
        if let Some(missing) =
            (0..self.tributes.len()).find(|&i| self.tributes[i].alive() && !seen[i])
        {
            return Err(format!(
                "{} isn't in any event; every living tribute is in exactly one.",
                Game::tag(missing)
            ));
        }
        let (min, max) = self.deaths_range(kind);
        if dead < min || dead > max {
            let wanted = if min == max {
                format!("exactly {min}")
            } else {
                format!("{min} to {max}")
            };
            return Err(format!("{dead} died, but this round {wanted} should die."));
        }
        let clean =
            |line: Option<String>| line.map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
        Ok(Round {
            kind,
            gamemaker: clean(written.gamemaker).filter(|_| kind.announced()),
            events,
            victory: clean(written.victory).filter(|_| kind == Kind::Finale),
            picture: clean(written.picture).filter(|_| kind.pictured()),
        })
    }

    /// Plays a checked round: the dead die, killers get their kills.
    pub fn apply(&mut self, round: Round) {
        let index = self.rounds.len();
        for death in round.events.iter().flat_map(|e| &e.deaths) {
            self.tributes[death.victim].died_in = Some(index);
            if let Some(by) = death.by {
                self.tributes[by].kills += 1;
            }
        }
        self.rounds.push(round);
    }

    /// Who died in the rounds from `from` on, in the order they died.
    pub fn fallen_since(&self, from: usize) -> Vec<&Tribute> {
        self.rounds
            .iter()
            .skip(from)
            .flat_map(|r| &r.events)
            .flat_map(|e| &e.deaths)
            .map(|d| &self.tributes[d.victim])
            .collect()
    }

    /// Everyone from first place to last: the winner, then whoever died last, and so on.
    pub fn placings(&self) -> Vec<&Tribute> {
        let mut order: Vec<&Tribute> = self.tributes.iter().collect();
        order.sort_by_key(|t| std::cmp::Reverse(t.died_in.unwrap_or(usize::MAX)));
        order
    }
}

/// The tributes an event's text names with `{T1}`-style tags, by index.
fn tags_in(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{T") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find('}') else { break };
        if let Ok(number) = rest[..end].parse::<usize>()
            && number >= 1
            && !found.contains(&(number - 1))
        {
            found.push(number - 1);
        }
        rest = &rest[end + 1..];
    }
    found
}

/// An event's text with each `{T1}`-style tag replaced by `name(index)`.
pub fn fill(text: &str, name: impl Fn(usize) -> String) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{T") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after
            .find('}')
            .and_then(|end| Some((end, after[..end].parse::<usize>().ok()?)))
        {
            Some((end, number)) if number >= 1 => {
                out.push_str(&name(number - 1));
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str("{T");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(count: u64) -> Game {
        Game::new((1..=count).map(|id| (id, format!("P{id}"))).collect())
    }

    fn event(text: &str, tributes: &[&str], deaths: &[(&str, Option<&str>)]) -> WrittenEvent {
        WrittenEvent {
            text: text.to_string(),
            tributes: tributes.iter().map(|t| t.to_string()).collect(),
            deaths: deaths
                .iter()
                .map(|(tribute, by)| WrittenDeath {
                    tribute: tribute.to_string(),
                    by: by.map(str::to_string),
                })
                .collect(),
        }
    }

    fn written(events: Vec<WrittenEvent>) -> Written {
        Written {
            gamemaker: Some("Welcome!".into()),
            events,
            victory: None,
            picture: Some("A forest".into()),
        }
    }

    #[test]
    fn rounds_go_bloodbath_day_night_feast() {
        let mut g = game(8);
        assert_eq!(g.next_kind(), Some(Kind::Bloodbath));
        let empty = |kind| Round {
            kind,
            gamemaker: None,
            events: Vec::new(),
            victory: None,
            picture: None,
        };
        g.apply(empty(Kind::Bloodbath));
        assert_eq!(g.next_kind(), Some(Kind::Day(1)));
        g.apply(empty(Kind::Day(1)));
        assert_eq!(g.next_kind(), Some(Kind::Night(1)));
        g.apply(empty(Kind::Night(1)));
        assert_eq!(g.next_kind(), Some(Kind::Feast(2)));
        g.apply(empty(Kind::Feast(2)));
        assert_eq!(g.next_kind(), Some(Kind::Night(2)));
        g.apply(empty(Kind::Night(2)));
        assert_eq!(g.next_kind(), Some(Kind::Day(3)));
    }

    #[test]
    fn two_left_is_the_finale_and_one_left_is_the_end() {
        let mut g = game(2);
        assert_eq!(g.next_kind(), Some(Kind::Finale));
        g.tributes[1].died_in = Some(0);
        assert_eq!(g.next_kind(), None);
        assert_eq!(g.winner().map(|t| t.id), Some(1));
    }

    /// A round in which `deaths` tributes died, for the pacing tests.
    fn round_with(kind: Kind, deaths: usize) -> Round {
        Round {
            kind,
            gamemaker: None,
            events: (0..deaths)
                .map(|victim| Event {
                    text: String::new(),
                    deaths: vec![Death { victim, by: None }],
                })
                .collect(),
            victory: None,
            picture: None,
        }
    }

    #[test]
    fn pacing_keeps_a_final_two() {
        let mut g = game(24);
        assert_eq!(g.deaths_range(Kind::Bloodbath), (2, 5));
        assert_eq!(g.deaths_range(Kind::Feast(2)), (3, 6));
        g.rounds.push(round_with(Kind::Bloodbath, 0));
        // The first day can't be quiet after a quiet Bloodbath.
        assert_eq!(g.deaths_range(Kind::Day(1)), (1, 3));
        let mut g = game(3);
        assert_eq!(g.deaths_range(Kind::Bloodbath), (1, 1));
        g.rounds.push(round_with(Kind::Bloodbath, 1));
        assert_eq!(g.deaths_range(Kind::Day(1)), (0, 1));
        assert_eq!(g.deaths_range(Kind::Feast(2)), (1, 1));
        assert_eq!(game(2).deaths_range(Kind::Finale), (1, 1));
        for alive in 3..=24 {
            let mut g = game(alive as u64);
            g.rounds.push(round_with(Kind::Bloodbath, 1));
            for kind in [
                Kind::Bloodbath,
                Kind::Day(1),
                Kind::Night(1),
                Kind::Feast(2),
            ] {
                let (min, max) = g.deaths_range(kind);
                assert!(
                    min <= max && max >= 1 && max <= alive - 2,
                    "{kind:?} {alive}"
                );
            }
        }
    }

    #[test]
    fn a_good_round_is_applied() {
        let mut g = game(4);
        let round = g
            .check(
                Kind::Bloodbath,
                written(vec![
                    event(
                        "{T1} pushes {T2} off a cliff.",
                        &["T1", "T2"],
                        &[("T2", Some("T1"))],
                    ),
                    event("{T3} and {T4} share a tent.", &["T3", "T4"], &[]),
                ]),
            )
            .unwrap();
        assert_eq!(round.gamemaker.as_deref(), Some("Welcome!"));
        assert_eq!(round.picture.as_deref(), Some("A forest"));
        g.apply(round);
        assert_eq!(g.alive(), 3);
        assert_eq!(g.tributes[0].kills, 1);
        assert_eq!(g.tributes[1].died_in, Some(0));
        assert_eq!(g.fallen_since(0)[0].name, "P2");
    }

    #[test]
    fn nights_arent_drawn() {
        let g = game(3);
        let night = g
            .check(
                Kind::Night(1),
                written(vec![event(
                    "{T1} and {T2} sleep while {T3} falls off a cliff.",
                    &["T1", "T2", "T3"],
                    &[("T3", None)],
                )]),
            )
            .unwrap();
        assert_eq!(night.picture, None);
        assert!(Kind::Day(1).pictured() && Kind::Finale.pictured());
    }

    #[test]
    fn everyone_must_be_in_exactly_one_event() {
        let g = game(3);
        let missing = g.check(
            Kind::Night(1),
            written(vec![event(
                "{T1} kills {T2}.",
                &["T1", "T2"],
                &[("T2", Some("T1"))],
            )]),
        );
        assert!(missing.unwrap_err().contains("T3 isn't in any event"));
        let twice = g.check(
            Kind::Night(1),
            written(vec![
                event("{T1} kills {T2}.", &["T1", "T2"], &[("T2", Some("T1"))]),
                event("{T1} and {T3} nap.", &["T1", "T3"], &[]),
            ]),
        );
        assert!(twice.unwrap_err().contains("more than one event"));
    }

    #[test]
    fn deaths_must_fit_the_pacing() {
        let g = game(3);
        let nobody = g.check(
            Kind::Night(1),
            written(vec![event(
                "{T1}, {T2} and {T3} hide.",
                &["T1", "T2", "T3"],
                &[],
            )]),
        );
        assert_eq!(
            nobody.unwrap_err(),
            "0 died, but this round exactly 1 should die."
        );
    }

    #[test]
    fn tags_must_match_the_text_and_the_living() {
        let mut g = game(3);
        let stray = g.check(
            Kind::Night(1),
            written(vec![
                event("{T1} thinks of {T2}.", &["T1"], &[]),
                event("{T2} and {T3} fight.", &["T2", "T3"], &[("T3", Some("T2"))]),
            ]),
        );
        assert!(stray.unwrap_err().contains("isn't in its tributes"));
        g.tributes[2].died_in = Some(0);
        let ghost = g.check(
            Kind::Finale,
            written(vec![event("{T3} haunts.", &["T3"], &[])]),
        );
        assert_eq!(ghost.unwrap_err(), "T3 is already dead.");
        let unknown = g.check(
            Kind::Finale,
            written(vec![event("{T9} appears.", &["T9"], &[])]),
        );
        assert_eq!(unknown.unwrap_err(), "T9 isn't one of the tributes.");
    }

    #[test]
    fn a_killer_must_be_someone_else_in_the_event() {
        let g = game(2);
        let suicide = g.check(
            Kind::Finale,
            written(vec![event(
                "{T1} and {T2} duel.",
                &["T1", "T2"],
                &[("T2", Some("T2"))],
            )]),
        );
        assert!(suicide.unwrap_err().contains("isn't another tribute"));
        let accident = g.check(
            Kind::Finale,
            written(vec![event(
                "{T1} and {T2} duel.",
                &["T1", "T2"],
                &[("T2", None)],
            )]),
        );
        assert!(accident.is_ok());
    }

    #[test]
    fn placings_put_the_winner_first_and_the_first_dead_last() {
        let mut g = game(3);
        g.tributes[0].died_in = Some(0);
        g.tributes[2].died_in = Some(1);
        let names: Vec<&str> = g.placings().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["P2", "P3", "P1"]);
    }

    #[test]
    fn tags_are_filled_with_names() {
        let filled = fill("{T1} hugs {T12}. {T} {Tx} {T0}", |i| format!("<{i}>"));
        assert_eq!(filled, "<0> hugs <11>. {T} {Tx} {T0}");
        assert_eq!(tags_in("{T2} and {T1} and {T2}"), vec![1, 0]);
    }
}
