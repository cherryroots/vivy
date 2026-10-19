//! The rules of a Hunger Games, with no Discord or AI in it, so the tests can play a whole
//! game in a few lines.
//!
//! Vivy writes every round (see `writer.rs`), and this file decides whether what she wrote
//! can happen: every living tribute is in exactly one event, only the living die, and about
//! as many die as the pacing asks for. A round that passes is applied: the dead are marked,
//! the killers get their kills, weapons change hands and alliances form and break.
//!
//! In what she writes, tributes are tags like `T3` (the third tribute), never names, so a
//! name can't be misspelled or mixed up. `{T3}` in an event's text becomes the name when
//! it's shown.

use std::ops::Range;

use serde::Deserialize;

/// Someone in the arena.
#[derive(Debug, Clone, PartialEq)]
pub struct Tribute {
    pub id: u64,
    pub name: String,
    pub kills: u32,
    /// The round they died in (0 is the first), or `None` while alive.
    pub died_in: Option<usize>,
    /// The weapon they carry, or `None` while unarmed. A new find replaces it, and the dead
    /// keep theirs, so Vivy knows what there is to loot.
    pub weapon: Option<String>,
    /// Their alliance, or `None` on their own: allies share a number.
    pub team: Option<u32>,
    /// How bold they are, 0 to 99, rolled when the game starts and never shown: the bold
    /// step into the Octagon, the cowards hang back. Kills add to it (see [`Game::entrants`]).
    pub nerve: u32,
}

impl Tribute {
    pub fn alive(&self) -> bool {
        self.died_in.is_none()
    }
}

/// What kind of round it is. Days and nights are numbered from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The first round: everyone starts in a ring around the Octagon in the middle and runs
    /// for the zones, unarmed.
    Scatter,
    Day(u32),
    Night(u32),
    /// After a day, at most twice a game (see [`Game::octagon_due`]): the bold fight
    /// one-on-one in the Octagon in the middle for a prize, until one champion is left.
    Octagon(u32),
    /// The last two, until one is left.
    Finale,
}

impl Kind {
    /// Whether Vivy says a Gamemaker line before this round: when the Games open, when the
    /// Octagon opens, and for the final two.
    pub fn announced(self) -> bool {
        matches!(self, Kind::Scatter | Kind::Octagon(_) | Kind::Finale)
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Event {
    /// The text, with `{T1}`-style tags where names go.
    pub text: String,
    pub deaths: Vec<Death>,
    /// Weapons found, stolen or looted from the dead: who, and what.
    pub finds: Vec<(usize, String)>,
    /// Who teams up in this event: they share an alliance from now on.
    pub allies: Vec<usize>,
    /// Who leaves their alliance in this event.
    pub leaves: Vec<usize>,
}

/// What the champion of an Octagon wins, as the crowd votes before it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reward {
    /// The deadliest weapon in the arena: whoever carries it wins nearly every fight, and
    /// it can be looted.
    Weapon,
    /// The champion can't die in the next two rounds (not counting the final showdown).
    Armour,
    /// The champion tracks someone down and kills them in the next round.
    Hunt,
}

impl Reward {
    pub const ALL: [Reward; 3] = [Reward::Weapon, Reward::Armour, Reward::Hunt];

    /// Its name in buttons and messages.
    pub fn name(self) -> &'static str {
        match self {
            Reward::Weapon => "Legendary weapon",
            Reward::Armour => "Golden armour",
            Reward::Hunt => "The hunt",
        }
    }

    pub fn emoji(self) -> &'static str {
        match self {
            Reward::Weapon => "⚔️",
            Reward::Armour => "🛡️",
            Reward::Hunt => "🐺",
        }
    }

    /// What it does, in a few words for people.
    pub fn blurb(self) -> &'static str {
        match self {
            Reward::Weapon => "the deadliest weapon in the arena, wins nearly every fight",
            Reward::Armour => "can't die for the next two rounds",
            Reward::Hunt => "hunts someone down and kills them the next round",
        }
    }
}

/// How many rounds after the Octagon its golden armour lasts.
const ARMOUR_ROUNDS: usize = 2;

/// A grudge from earlier Games in this server: `killer` killed `victim` `times` times
/// before. Both are indexes into this game's tributes.
#[derive(Debug, Clone, PartialEq)]
pub struct Grudge {
    pub killer: usize,
    pub victim: usize,
    pub times: u32,
}

impl Grudge {
    /// "once", "twice" or "3 times".
    pub fn how_often(&self) -> String {
        match self.times {
            1 => "once".to_string(),
            2 => "twice".to_string(),
            n => format!("{n} times"),
        }
    }
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
    /// What to draw on the arena map for this round, when she was asked to.
    pub picture: Option<String>,
    /// In an Octagon round, who stepped in (indexes), who won it, and what they won.
    pub entrants: Vec<usize>,
    pub champion: Option<usize>,
    pub reward: Option<Reward>,
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
    #[serde(default)]
    pub champion: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct WrittenEvent {
    pub text: String,
    pub tributes: Vec<String>,
    #[serde(default)]
    pub deaths: Vec<WrittenDeath>,
    #[serde(default)]
    pub finds: Vec<WrittenFind>,
    #[serde(default)]
    pub allies: Vec<String>,
    #[serde(default)]
    pub leaves: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct WrittenFind {
    pub tribute: String,
    pub weapon: String,
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
/// The longest a weapon's name may be, in characters.
const MAX_WEAPON: usize = 60;
/// The nerve it takes to step into the Octagon.
const BRAVE: u32 = 60;
/// How much nerve each kill adds.
const KILL_NERVE: u32 = 15;
/// The most who fight in one Octagon, so its round stays readable.
const MOST_IN_OCTAGON: usize = 4;
/// The fewest alive for the Octagon to open: two to fight and someone to watch.
const OCTAGON_ALIVE: usize = 4;
/// The most grudges from earlier Games she's told about, so the brief stays short.
const MOST_GRUDGES: usize = 8;
/// How many of the arena's places stay open to the end, however small it gets.
const ALWAYS_OPEN: usize = 2;

/// The whole game: the tributes and the rounds played so far.
#[derive(Debug, Clone)]
pub struct Game {
    pub tributes: Vec<Tribute>,
    pub rounds: Vec<Round>,
    /// The prize for the coming Octagon, once the crowd has voted. A legendary weapon
    /// when there was no vote.
    pub prize: Option<Reward>,
    /// How many places the arena has (see [`Game::closed`]). 0 means it never shrinks.
    pub places: usize,
    /// Who killed whom in earlier Games, among these tributes, most often first.
    pub grudges: Vec<Grudge>,
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
                    weapon: None,
                    team: None,
                    nerve: fastrand::u32(..100),
                })
                .collect(),
            rounds: Vec::new(),
            prize: None,
            places: 0,
            grudges: Vec::new(),
        }
    }

    /// Takes in who killed whom in earlier Games, as (killer's user ID, victim's user ID,
    /// times). Only pairs where both are in this game count, the most often first.
    pub fn remember(&mut self, past: &[(u64, u64, u32)]) {
        let index = |id: u64| self.tributes.iter().position(|t| t.id == id);
        let mut grudges: Vec<Grudge> = past
            .iter()
            .filter_map(|&(killer, victim, times)| {
                Some(Grudge {
                    killer: index(killer)?,
                    victim: index(victim)?,
                    times,
                })
            })
            .filter(|g| g.killer != g.victim && g.times > 0)
            .collect();
        grudges.sort_by_key(|g| (std::cmp::Reverse(g.times), g.killer, g.victim));
        grudges.truncate(MOST_GRUDGES);
        self.grudges = grudges;
    }

    /// The grudges that can still play out: both of them are alive.
    pub fn open_grudges(&self) -> Vec<&Grudge> {
        self.grudges
            .iter()
            .filter(|g| self.tributes[g.killer].alive() && self.tributes[g.victim].alive())
            .collect()
    }

    /// Whether `killer` killing `victim` is revenge: `victim` killed them in earlier Games.
    pub fn revenge(&self, killer: usize, victim: usize) -> bool {
        self.grudges
            .iter()
            .any(|g| g.killer == victim && g.victim == killer)
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

    /// The winner's index, once one is left.
    pub fn winner_index(&self) -> Option<usize> {
        let winner = self.winner()?;
        self.tributes.iter().position(|t| t.id == winner.id)
    }

    /// Everyone `killer` killed (indexes), in the order they died.
    pub fn victims(&self, killer: usize) -> Vec<usize> {
        self.rounds
            .iter()
            .flat_map(|r| &r.events)
            .flat_map(|e| &e.deaths)
            .filter(|d| d.by == Some(killer))
            .map(|d| d.victim)
            .collect()
    }

    /// How many Octagons the tribute at `index` won.
    pub fn octagon_wins(&self, index: usize) -> u32 {
        self.rounds
            .iter()
            .filter(|r| r.champion == Some(index))
            .count() as u32
    }

    /// Whether the tribute at `index` wears golden armour from an Octagon they won, in the
    /// round at `at` (an index into the rounds).
    pub fn armoured(&self, index: usize, at: usize) -> bool {
        self.rounds.iter().enumerate().any(|(won_in, r)| {
            r.champion == Some(index)
                && r.reward == Some(Reward::Armour)
                && (won_in + 1..=won_in + ARMOUR_ROUNDS).contains(&at)
        })
    }

    /// Who must kill someone in the round at `at`: the champion of the Octagon just before
    /// it, when they won the hunt.
    pub fn hunter(&self, at: usize) -> Option<usize> {
        let before = self.rounds.get(at.checked_sub(1)?)?;
        let hunter = before
            .champion
            .filter(|_| before.reward == Some(Reward::Hunt))?;
        self.tributes[hunter].alive().then_some(hunter)
    }

    /// The weapons won as Octagon prizes: what each champion found in the round they won.
    /// A prize stays one when it's looted, so it can change hands.
    pub fn prizes(&self) -> Vec<&str> {
        self.rounds
            .iter()
            .filter(|r| r.reward == Some(Reward::Weapon))
            .filter_map(|r| {
                let champion = r.champion?;
                r.events
                    .iter()
                    .flat_map(|e| &e.finds)
                    .filter(|(who, _)| *who == champion)
                    .map(|(_, weapon)| weapon.as_str())
                    .next_back()
            })
            .collect()
    }

    /// Whether the tribute at `index` carries an Octagon prize.
    pub fn holds_prize(&self, index: usize) -> bool {
        self.tributes[index]
            .weapon
            .as_deref()
            .is_some_and(|weapon| self.prizes().contains(&weapon))
    }

    /// The cowards, by index: everyone who was alive when an Octagon opened but never
    /// stepped into one. Empty when no Octagon was held.
    pub fn cowards(&self) -> Vec<usize> {
        let octagons: Vec<(usize, &Round)> = self
            .rounds
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.kind, Kind::Octagon(_)))
            .collect();
        (0..self.tributes.len())
            .filter(|&i| {
                // Dying in a round means being alive when it began.
                let there = |at: usize| self.tributes[i].died_in.is_none_or(|died| died >= at);
                octagons.iter().any(|&(at, _)| there(at))
                    && !octagons.iter().any(|(_, r)| r.entrants.contains(&i))
            })
            .collect()
    }

    /// Each tribute's weapon, in the game's order.
    pub fn weapons(&self) -> Vec<Option<String>> {
        self.tributes.iter().map(|t| t.weapon.clone()).collect()
    }

    /// Each tribute's weapon as drawn on the map of the last round played: what they carry
    /// by the end of it, except that someone who died in it is drawn empty-handed when their
    /// weapon was likely looted: someone else in the same event found a weapon, or one of
    /// the living carries one by the same name. Otherwise the weapon would be drawn twice,
    /// in the looter's hands and by the body (Cherry saw it).
    pub fn weapons_drawn(&self) -> Vec<Option<String>> {
        let mut weapons = self.weapons();
        let Some(round) = self.rounds.last() else {
            return weapons;
        };
        for event in &round.events {
            for death in &event.deaths {
                let victim = death.victim;
                let found_by_another = event.finds.iter().any(|(who, _)| *who != victim);
                let carried = self.tributes[victim]
                    .weapon
                    .as_deref()
                    .is_some_and(|weapon| {
                        self.tributes
                            .iter()
                            .any(|t| t.alive() && t.weapon.as_deref() == Some(weapon))
                    });
                if found_by_another || carried {
                    weapons[victim] = None;
                }
            }
        }
        weapons
    }

    /// The living allies of tribute `index`.
    pub fn allies_of(&self, index: usize) -> Vec<usize> {
        let Some(team) = self.tributes[index].team else {
            return Vec::new();
        };
        (0..self.tributes.len())
            .filter(|&i| {
                i != index && self.tributes[i].alive() && self.tributes[i].team == Some(team)
            })
            .collect()
    }

    /// The places (indexes into the arena's places) closed off in the round at `at` (an
    /// index into the rounds, or the next round's). The arena shrinks once half the
    /// tributes have fallen: from then on, places close as more fall, the last ones first,
    /// until only two are left open for the final two. Places only close when a day or
    /// night begins, so an Octagon or the final showdown keeps the arena as it was.
    pub fn closed(&self, at: usize) -> Range<usize> {
        let count = (0..=at)
            .rev()
            .find(|&j| matches!(self.kind_at(j), Some(Kind::Day(_) | Kind::Night(_))))
            .map_or(0, |j| self.closed_after(self.fallen_before(j)));
        self.places - count..self.places
    }

    /// The places that close as the round at `at` begins: closed now, but not in the round
    /// before.
    pub fn closing(&self, at: usize) -> Range<usize> {
        let now = self.closed(at);
        let before = match at.checked_sub(1) {
            Some(last) => self.closed(last),
            None => self.places..self.places,
        };
        now.start..before.start
    }

    /// How many places are closed once `fallen` tributes have fallen. The first close as
    /// soon as half have fallen (Cherry: with 6 tributes it has to start at 3 left), then an
    /// even share more with each death, so the last two alive have only [`ALWAYS_OPEN`]
    /// places left.
    fn closed_after(&self, fallen: usize) -> usize {
        let total = self.tributes.len();
        let half = total / 2;
        // The deaths from half fallen up to the final two, each closing a share.
        let steps = total.saturating_sub(1 + half);
        let closable = self.places.saturating_sub(ALWAYS_OPEN);
        if fallen < half || steps == 0 {
            return 0;
        }
        ((fallen - half + 1) * closable / steps).min(closable)
    }

    /// How many fell before the round at `at`.
    fn fallen_before(&self, at: usize) -> usize {
        self.tributes
            .iter()
            .filter(|t| t.died_in.is_some_and(|died| died < at))
            .count()
    }

    /// The kind of the round at `at`: one played, or the next one.
    fn kind_at(&self, at: usize) -> Option<Kind> {
        match self.rounds.get(at) {
            Some(round) => Some(round.kind),
            None if at == self.rounds.len() => self.next_kind(),
            None => None,
        }
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
        Some(match self.rounds.last().map(|r| r.kind) {
            None => Kind::Scatter,
            Some(Kind::Scatter) => Kind::Day(1),
            Some(Kind::Day(day)) if self.octagon_due() => Kind::Octagon(day),
            Some(Kind::Day(day) | Kind::Octagon(day)) => Kind::Night(day),
            Some(Kind::Night(day)) => Kind::Day(day + 1),
            // A Finale always leaves one, so nothing follows it.
            Some(Kind::Finale) => return None,
        })
    }

    /// Whether the Octagon opens after this day. It's rare, so each one is a big moment
    /// (Cherry's call): 12 or more tributes get two, once about a third and again about two
    /// thirds have fallen; 6 to 11 get one, at about half; fewer get none, since the final
    /// showdown is in the Octagon anyway. Counted by deaths, so it follows the game's pace,
    /// and only with at least [`OCTAGON_ALIVE`] alive.
    pub fn octagon_due(&self) -> bool {
        let total = self.tributes.len();
        let marks = match total {
            12.. => vec![total.div_ceil(3), (total * 2).div_ceil(3)],
            6..=11 => vec![total.div_ceil(2)],
            _ => Vec::new(),
        };
        let held = self
            .rounds
            .iter()
            .filter(|r| matches!(r.kind, Kind::Octagon(_)))
            .count();
        let fallen = total - self.alive();
        marks
            .get(held)
            .is_some_and(|&mark| fallen >= mark && self.alive() >= OCTAGON_ALIVE)
    }

    /// Who steps into the Octagon, by index: the living whose nerve (with 15 for each kill)
    /// is at least 60, boldest first, but never fewer than the two boldest or more than four.
    /// Worked out from the game, so the brief and the check agree.
    pub fn entrants(&self) -> Vec<usize> {
        let mut living: Vec<(u32, usize)> = (0..self.tributes.len())
            .filter(|&i| self.tributes[i].alive())
            .map(|i| {
                let t = &self.tributes[i];
                (t.nerve + KILL_NERVE * t.kills, i)
            })
            .collect();
        living.sort_by_key(|&(nerve, i)| (std::cmp::Reverse(nerve), i));
        let brave = living.iter().filter(|(nerve, _)| *nerve >= BRAVE).count();
        let count = brave.clamp(2, MOST_IN_OCTAGON).min(living.len());
        let mut entrants: Vec<usize> = living[..count].iter().map(|&(_, i)| i).collect();
        entrants.sort_unstable();
        entrants
    }

    /// How many should die in the next round, of `kind`, at least and at most. At most 2 in
    /// the Scatter, since everyone runs, one fewer than the fighters in the Octagon (losers
    /// may yield instead), and an eighth
    /// to a sixth on days and nights, so 24 tributes take around 15 rounds and 5 take
    /// around 6. A day or night may pass with nobody dying, but never two in a row. Until
    /// the Finale at least two stay alive, so there's always a final two.
    pub fn deaths_range(&self, kind: Kind) -> (usize, usize) {
        let alive = self.alive();
        if kind == Kind::Finale || alive <= 2 {
            return (1, 1);
        }
        if kind == Kind::Scatter {
            return (0, (alive - 2).min(2));
        }
        if let Kind::Octagon(_) = kind {
            let bouts = self.entrants().len().saturating_sub(1);
            return (1, bouts.clamp(1, alive - 2));
        }
        // A hunt from the Octagon means at least one dies.
        let hunted = self.hunter(self.rounds.len()).is_some();
        let share = match kind {
            Kind::Day(_) => 0.12,
            Kind::Night(_) => 0.15,
            Kind::Scatter | Kind::Octagon(_) | Kind::Finale => unreachable!(),
        };
        let most = alive - 2;
        let max = ((alive as f64 * share).ceil() as usize).clamp(1, most);
        let last_was_quiet = self
            .rounds
            .last()
            .is_none_or(|r| r.events.iter().all(|e| e.deaths.is_empty()));
        let may_be_quiet = matches!(kind, Kind::Day(_) | Kind::Night(_)) && !last_was_quiet;
        let floor = max / 2;
        let min = if floor == 0 && may_be_quiet && !hunted {
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
            let dies = |i: usize| deaths.iter().any(|d| d.victim == i);
            let member = |tag: &str, what: &str| {
                self.index_of(tag)
                    .filter(|i| members.contains(i))
                    .ok_or_else(|| format!("{tag} {what} in an event they aren't in."))
            };
            let mut finds: Vec<(usize, String)> = Vec::new();
            for find in event.finds {
                let who = member(&find.tribute, "finds a weapon")?;
                if dies(who) {
                    return Err(format!(
                        "{} finds a weapon but dies in the same event; only the living keep weapons.",
                        find.tribute
                    ));
                }
                let weapon = find.weapon.trim().to_string();
                if weapon.is_empty() || weapon.chars().count() > MAX_WEAPON {
                    return Err(format!(
                        "A weapon's name is 1 to {MAX_WEAPON} characters; {}'s isn't.",
                        find.tribute
                    ));
                }
                // One weapon each: the last find in an event is the one kept.
                finds.retain(|(i, _)| *i != who);
                finds.push((who, weapon));
            }
            let mut allies = Vec::new();
            for tag in &event.allies {
                let who = member(tag, "teams up")?;
                if dies(who) {
                    return Err(format!("{tag} teams up but dies in the same event."));
                }
                if !allies.contains(&who) {
                    allies.push(who);
                }
            }
            if allies.len() == 1 {
                return Err("`allies` needs at least two tributes who team up.".to_string());
            }
            let mut leaves = Vec::new();
            for tag in &event.leaves {
                let who = member(tag, "leaves an alliance")?;
                if allies.contains(&who) {
                    return Err(format!("{tag} can't both team up and leave in one event."));
                }
                if !leaves.contains(&who) {
                    leaves.push(who);
                }
            }
            dead += deaths.len();
            events.push(Event {
                text,
                deaths,
                finds,
                allies,
                leaves,
            });
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
        // What earlier Octagon champions won.
        let now = self.rounds.len();
        for death in events.iter().flat_map(|e| &e.deaths) {
            if kind != Kind::Finale && self.armoured(death.victim, now) {
                return Err(format!(
                    "{} wears the golden armour from the Octagon and can't die this round.",
                    Game::tag(death.victim)
                ));
            }
        }
        if let Some(hunter) = self.hunter(now).filter(|_| kind != Kind::Finale) {
            let killed = events
                .iter()
                .flat_map(|e| &e.deaths)
                .any(|d| d.by == Some(hunter));
            if !killed {
                return Err(format!(
                    "{} won the hunt in the Octagon, so this round they track someone down and kill them.",
                    Game::tag(hunter)
                ));
            }
        }
        let reward = self.prize.unwrap_or(Reward::Weapon);
        let (entrants, champion) = match kind {
            Kind::Octagon(_) => {
                let entrants = self.entrants();
                let tag = written.champion.unwrap_or_default();
                let champion = self
                    .index_of(&tag)
                    .filter(|i| entrants.contains(i))
                    .ok_or_else(|| {
                        "`champion` must be the tag of the one who steps into the Octagon and wins it.".to_string()
                    })?;
                for death in events.iter().flat_map(|e| &e.deaths) {
                    if death.victim == champion {
                        return Err(format!("{tag} is the champion but dies this round."));
                    }
                    if !entrants.contains(&death.victim) {
                        return Err(format!(
                            "{} dies, but only those in the Octagon may die this round.",
                            Game::tag(death.victim)
                        ));
                    }
                }
                // The weapon is what makes winning worth it, so there always is one.
                let won = events
                    .iter()
                    .flat_map(|e| &e.finds)
                    .any(|(who, _)| *who == champion);
                if reward == Reward::Weapon && !won {
                    return Err(format!(
                        "{tag} wins the Octagon but gets no prize: put the legendary weapon they win in their `finds`."
                    ));
                }
                (entrants, Some(champion))
            }
            _ => (Vec::new(), None),
        };
        let clean =
            |line: Option<String>| line.map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
        Ok(Round {
            kind,
            gamemaker: clean(written.gamemaker).filter(|_| kind.announced()),
            events,
            victory: clean(written.victory).filter(|_| kind == Kind::Finale),
            picture: clean(written.picture),
            reward: champion.map(|_| reward),
            entrants,
            champion,
        })
    }

    /// Plays a checked round: the dead die, killers get their kills, the finders get their
    /// weapons, and alliances form and break. Killing an ally ends the killer's alliance.
    pub fn apply(&mut self, round: Round) {
        let index = self.rounds.len();
        for event in &round.events {
            for death in &event.deaths {
                self.tributes[death.victim].died_in = Some(index);
                if let Some(by) = death.by {
                    self.tributes[by].kills += 1;
                    let team = self.tributes[by].team;
                    if team.is_some() && team == self.tributes[death.victim].team {
                        self.tributes[by].team = None;
                    }
                }
            }
            for (who, weapon) in &event.finds {
                self.tributes[*who].weapon = Some(weapon.clone());
            }
            for &who in &event.leaves {
                self.tributes[who].team = None;
            }
            if !event.allies.is_empty() {
                // Joining someone who already has allies brings you into their alliance.
                let next = self
                    .tributes
                    .iter()
                    .filter_map(|t| t.team)
                    .max()
                    .unwrap_or(0)
                    + 1;
                let team = event
                    .allies
                    .iter()
                    .find_map(|&i| self.tributes[i].team)
                    .unwrap_or(next);
                for &who in &event.allies {
                    self.tributes[who].team = Some(team);
                }
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
            ..Default::default()
        }
    }

    fn written(events: Vec<WrittenEvent>) -> Written {
        Written {
            gamemaker: Some("Welcome!".into()),
            events,
            victory: None,
            picture: Some("A forest".into()),
            champion: None,
        }
    }

    #[test]
    fn rounds_go_scatter_day_night() {
        let mut g = game(8);
        assert_eq!(g.next_kind(), Some(Kind::Scatter));
        g.apply(round_with(Kind::Scatter, 0));
        assert_eq!(g.next_kind(), Some(Kind::Day(1)));
        g.apply(round_with(Kind::Day(1), 0));
        assert_eq!(g.next_kind(), Some(Kind::Night(1)));
        g.apply(round_with(Kind::Night(1), 0));
        assert_eq!(g.next_kind(), Some(Kind::Day(2)));
    }

    /// `game(total)` with the first `dead` tributes fallen and `held` Octagons played.
    fn with_fallen(total: u64, dead: usize, held: usize) -> Game {
        let mut g = game(total);
        for t in &mut g.tributes[..dead] {
            t.died_in = Some(0);
        }
        for _ in 0..held {
            g.rounds.push(round_with(Kind::Octagon(1), 0));
        }
        g
    }

    #[test]
    fn the_octagon_opens_by_deaths() {
        // 24 tributes: at 8 fallen, then at 16.
        assert!(!with_fallen(24, 7, 0).octagon_due());
        assert!(with_fallen(24, 8, 0).octagon_due());
        assert!(!with_fallen(24, 15, 1).octagon_due());
        assert!(with_fallen(24, 16, 1).octagon_due());
        assert!(!with_fallen(24, 20, 2).octagon_due());
        // 12 tributes: at 4 and 8, while 4 are alive.
        assert!(with_fallen(12, 4, 0).octagon_due());
        assert!(with_fallen(12, 8, 1).octagon_due());
        assert!(!with_fallen(12, 9, 1).octagon_due());
        // 8 tributes: once, at 4 fallen.
        assert!(!with_fallen(8, 3, 0).octagon_due());
        assert!(with_fallen(8, 4, 0).octagon_due());
        assert!(!with_fallen(8, 4, 1).octagon_due());
        // 5 tributes: never.
        assert!(!with_fallen(5, 1, 0).octagon_due());
        // It comes after a day, then the night follows.
        let mut g = with_fallen(8, 4, 0);
        g.rounds.push(round_with(Kind::Day(2), 0));
        assert_eq!(g.next_kind(), Some(Kind::Octagon(2)));
        g.rounds.push(round_with(Kind::Octagon(2), 0));
        assert_eq!(g.next_kind(), Some(Kind::Night(2)));
    }

    #[test]
    fn the_boldest_step_in() {
        let mut g = game(6);
        for (t, nerve) in g.tributes.iter_mut().zip([10, 70, 20, 65, 30, 5]) {
            t.nerve = nerve;
        }
        assert_eq!(g.entrants(), vec![1, 3]);
        // Nobody brave: still the two boldest.
        g.tributes[1].nerve = 50;
        g.tributes[3].nerve = 40;
        assert_eq!(g.entrants(), vec![1, 3]);
        // Kills add nerve: two kills make P3 (20 + 30) bolder than P2 (50).
        g.tributes[2].kills = 2;
        assert_eq!(g.entrants(), vec![1, 2]);
        // Everyone brave: at most four.
        for t in &mut g.tributes {
            t.nerve = 90;
        }
        assert_eq!(g.entrants(), vec![0, 1, 2, 3]);
        assert_eq!(g.deaths_range(Kind::Octagon(2)), (1, 3));
        // The dead don't step in.
        g.tributes[0].died_in = Some(0);
        assert_eq!(g.entrants(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn the_octagon_needs_a_champion_from_inside() {
        let mut g = game(5);
        for (t, nerve) in g.tributes.iter_mut().zip([90, 80, 10, 10, 10]) {
            t.nerve = nerve;
        }
        let round = |champion: Option<&str>, victim: &str| {
            let mut w = written(vec![
                event(
                    "{T1} and {T2} fight in the cage, and one falls.",
                    &["T1", "T2"],
                    &[(victim, None)],
                ),
                event("{T3}, {T4} and {T5} watch.", &["T3", "T4", "T5"], &[]),
            ]);
            w.champion = champion.map(str::to_string);
            w
        };
        let kind = Kind::Octagon(2);
        assert!(
            g.check(kind, round(None, "T2"))
                .unwrap_err()
                .contains("`champion`")
        );
        assert!(
            g.check(kind, round(Some("T3"), "T2"))
                .unwrap_err()
                .contains("`champion`")
        );
        assert!(
            g.check(kind, round(Some("T1"), "T1"))
                .unwrap_err()
                .contains("dies this round")
        );
        // The champion always wins a prize.
        assert!(
            g.check(kind, round(Some("T1"), "T2"))
                .unwrap_err()
                .contains("no prize")
        );
        let prized = |champion: Option<&str>, victim: &str| {
            let mut w = round(champion, victim);
            w.events[0].finds = vec![WrittenFind {
                tribute: "T1".into(),
                weapon: "the golden halberd".into(),
            }];
            w
        };
        let played = g.check(kind, prized(Some("T1"), "T2")).unwrap();
        assert_eq!(played.entrants, vec![0, 1]);
        assert_eq!(played.champion, Some(0));
        // The prize is kept, and marks whoever carries it.
        let mut after = g.clone();
        after.apply(played);
        assert_eq!(after.prizes(), vec!["the golden halberd"]);
        assert!(after.holds_prize(0));
        assert!(!after.holds_prize(2));
        // Outside the Octagon, nobody dies.
        let mut w = written(vec![
            event("{T1} and {T2} spar, and {T2} yields.", &["T1", "T2"], &[]),
            event(
                "{T3} trips over {T4} and {T5}.",
                &["T3", "T4", "T5"],
                &[("T3", None)],
            ),
        ]);
        w.champion = Some("T1".into());
        assert!(
            g.check(kind, w)
                .unwrap_err()
                .contains("only those in the Octagon")
        );
        // Other rounds have no champion.
        let mut w = round(Some("T1"), "T2");
        w.gamemaker = None;
        let day = g.check(Kind::Day(2), w).unwrap();
        assert_eq!(day.champion, None);
        assert!(day.entrants.is_empty());
    }

    /// A game of 6 where P1 just won an Octagon (round 0) with `reward`.
    fn after_octagon(reward: Reward) -> Game {
        let mut g = game(6);
        g.rounds.push(Round {
            entrants: vec![0, 1],
            champion: Some(0),
            reward: Some(reward),
            ..round_with(Kind::Octagon(1), 0)
        });
        g
    }

    /// Everyone alive in their own event, with `victim` killed by `by`.
    fn night(g: &Game, victim: &str, by: Option<&str>) -> Written {
        let mut w = written(
            (1..=g.tributes.len())
                .filter(|&n| g.tributes[n - 1].alive())
                .filter(|&n| format!("T{n}") != victim && Some(format!("T{n}").as_str()) != by)
                .map(|n| {
                    let tag = format!("T{n}");
                    event(&format!("{{{tag}}} sleeps."), &[tag.as_str()], &[])
                })
                .collect(),
        );
        let mut tags = vec![victim];
        tags.extend(by);
        let text = match by {
            Some(by) => format!("{{{by}}} gets {{{victim}}}."),
            None => format!("{{{victim}}} falls off a cliff."),
        };
        w.events.push(event(&text, &tags, &[(victim, by)]));
        w.gamemaker = None;
        w
    }

    #[test]
    fn golden_armour_keeps_the_champion_alive_for_two_rounds() {
        let mut g = after_octagon(Reward::Armour);
        assert!(g.armoured(0, 1) && g.armoured(0, 2) && !g.armoured(0, 3));
        assert!(!g.armoured(1, 1));
        let err = g
            .check(Kind::Night(1), night(&g, "T1", Some("T2")))
            .unwrap_err();
        assert!(err.contains("golden armour"), "{err}");
        assert!(g.check(Kind::Night(1), night(&g, "T3", Some("T2"))).is_ok());
        // It doesn't decide the final showdown.
        for t in &mut g.tributes[2..] {
            t.died_in = Some(0);
        }
        assert!(g.check(Kind::Finale, night(&g, "T1", Some("T2"))).is_ok());
    }

    #[test]
    fn the_hunt_makes_the_champion_kill_next_round() {
        let g = after_octagon(Reward::Hunt);
        assert_eq!(g.hunter(1), Some(0));
        assert_eq!(g.hunter(2), None);
        // Someone has to die, even when the night could have been quiet.
        assert!(g.deaths_range(Kind::Night(1)).0 >= 1);
        let err = g
            .check(Kind::Night(1), night(&g, "T3", Some("T2")))
            .unwrap_err();
        assert!(err.contains("won the hunt"), "{err}");
        assert!(g.check(Kind::Night(1), night(&g, "T3", Some("T1"))).is_ok());
    }

    #[test]
    fn only_a_weapon_prize_needs_a_find() {
        let mut g = game(5);
        for (t, nerve) in g.tributes.iter_mut().zip([90, 80, 10, 10, 10]) {
            t.nerve = nerve;
        }
        let mut w = written(vec![
            event("{T1} beats {T2}.", &["T1", "T2"], &[("T2", Some("T1"))]),
            event("{T3}, {T4} and {T5} watch.", &["T3", "T4", "T5"], &[]),
        ]);
        w.champion = Some("T1".into());
        assert!(
            g.check(Kind::Octagon(2), w)
                .unwrap_err()
                .contains("no prize")
        );
        g.prize = Some(Reward::Hunt);
        let mut w = written(vec![
            event("{T1} beats {T2}.", &["T1", "T2"], &[("T2", Some("T1"))]),
            event("{T3}, {T4} and {T5} watch.", &["T3", "T4", "T5"], &[]),
        ]);
        w.champion = Some("T1".into());
        let played = g.check(Kind::Octagon(2), w).unwrap();
        assert_eq!(played.reward, Some(Reward::Hunt));
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
    #[test]
    fn the_arena_shrinks_after_half_have_fallen() {
        // 14 tributes in 14 places, after night 1: with `dead` fallen, how many places
        // are closed as day 2 begins.
        let closed = |dead: usize| {
            let mut g = game(14);
            g.places = 14;
            g.apply(round_with(Kind::Night(1), dead));
            g.closed(1)
        };
        assert_eq!(closed(6), 14..14);
        // From half fallen, 12 places close over the 6 deaths to the final two, the last
        // places first.
        assert_eq!(closed(7), 12..14);
        assert_eq!(closed(8), 10..14);
        assert_eq!(closed(10), 6..14);
        assert_eq!(closed(11), 4..14);
        // 6 tributes: half of the places close at 3 left.
        let mut six = game(6);
        six.places = 6;
        six.apply(round_with(Kind::Night(1), 3));
        assert_eq!(six.closed(1), 4..6);
        // An arena with no places never shrinks.
        let mut g = game(14);
        g.apply(round_with(Kind::Night(1), 11));
        assert!(g.closed(1).is_empty());
    }

    #[test]
    fn places_close_only_as_days_and_nights_begin() {
        let mut g = game(14);
        g.places = 14;
        g.apply(round_with(Kind::Night(1), 8));
        // Day 2 closes four places.
        assert_eq!(g.closing(1), 10..14);
        g.apply(round_with(Kind::Day(2), 0));
        g.tributes[8].died_in = Some(1);
        // The Octagon that follows keeps the arena as it was, though one more fell.
        assert_eq!(g.next_kind(), Some(Kind::Octagon(2)));
        assert_eq!(g.closed(2), 10..14);
        assert!(g.closing(2).is_empty());
        g.apply(round_with(Kind::Octagon(2), 0));
        // Night 2 closes the next ones.
        assert_eq!(g.next_kind(), Some(Kind::Night(2)));
        assert_eq!(g.closed(3), 8..14);
        assert_eq!(g.closing(3), 8..10);
        // The first rounds close nothing.
        assert!(g.closing(0).is_empty());
    }

    #[test]
    fn grudges_come_from_earlier_games() {
        let mut g = game(4);
        // P1 killed P2 twice and P3 once; someone not here killed P4.
        g.remember(&[(1, 3, 1), (1, 2, 2), (99, 4, 5), (2, 2, 1)]);
        assert_eq!(
            g.grudges,
            vec![
                Grudge {
                    killer: 0,
                    victim: 1,
                    times: 2
                },
                Grudge {
                    killer: 0,
                    victim: 2,
                    times: 1
                },
            ]
        );
        assert_eq!(g.grudges[0].how_often(), "twice");
        // P2 killing P1 now is revenge; the other way round isn't.
        assert!(g.revenge(1, 0));
        assert!(!g.revenge(0, 1));
        // Once either is dead, the grudge can't play out anymore.
        g.tributes[2].died_in = Some(0);
        assert_eq!(g.open_grudges().len(), 1);
    }

    #[test]
    fn a_looted_weapon_is_drawn_once() {
        let mut g = game(4);
        g.tributes[0].weapon = Some("ban hammer".into());
        g.tributes[2].weapon = Some("spoon".into());
        let mut round = round_with(Kind::Day(1), 0);
        round.events = vec![
            // P2 kills P1 and takes the hammer.
            Event {
                deaths: vec![Death {
                    victim: 0,
                    by: Some(1),
                }],
                finds: vec![(1, "ban hammer".into())],
                ..Default::default()
            },
            // P4 kills P3 and takes nothing: the spoon lies by the body.
            Event {
                deaths: vec![Death {
                    victim: 2,
                    by: Some(3),
                }],
                ..Default::default()
            },
        ];
        g.apply(round);
        assert_eq!(
            g.weapons_drawn(),
            vec![None, Some("ban hammer".into()), Some("spoon".into()), None]
        );
    }

    fn round_with(kind: Kind, deaths: usize) -> Round {
        Round {
            kind,
            gamemaker: None,
            events: (0..deaths)
                .map(|victim| Event {
                    text: String::new(),
                    deaths: vec![Death { victim, by: None }],
                    ..Default::default()
                })
                .collect(),
            victory: None,
            picture: None,
            entrants: Vec::new(),
            champion: None,
            reward: None,
        }
    }

    #[test]
    fn pacing_keeps_a_final_two() {
        let mut g = game(24);
        // Everyone runs in the Scatter, so few die.
        assert_eq!(g.deaths_range(Kind::Scatter), (0, 2));
        g.rounds.push(round_with(Kind::Scatter, 0));
        // The first day can't be quiet after a quiet Scatter.
        assert_eq!(g.deaths_range(Kind::Day(1)), (1, 3));
        let mut g = game(3);
        assert_eq!(g.deaths_range(Kind::Scatter), (0, 1));
        g.rounds.push(round_with(Kind::Scatter, 1));
        assert_eq!(g.deaths_range(Kind::Day(1)), (0, 1));
        assert_eq!(g.deaths_range(Kind::Octagon(2)), (1, 1));
        assert_eq!(game(2).deaths_range(Kind::Finale), (1, 1));
        for alive in 3..=24 {
            let mut g = game(alive as u64);
            g.rounds.push(round_with(Kind::Scatter, 1));
            for kind in [
                Kind::Scatter,
                Kind::Day(1),
                Kind::Night(1),
                Kind::Octagon(2),
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
    fn weapons_and_alliances_carry_over() {
        let mut g = game(5);
        let mut found = event(
            "{T1} pulls a vinyl razor from a crate while {T2} and {T3} shake hands.",
            &["T1", "T2", "T3"],
            &[],
        );
        found.finds = vec![WrittenFind {
            tribute: "T1".into(),
            weapon: " vinyl razor ".into(),
        }];
        found.allies = vec!["T2".into(), "T3".into()];
        let round = g
            .check(
                Kind::Scatter,
                written(vec![found, event("{T4} and {T5} run.", &["T4", "T5"], &[])]),
            )
            .unwrap();
        g.apply(round);
        assert_eq!(g.tributes[0].weapon.as_deref(), Some("vinyl razor"));
        assert_eq!(g.allies_of(1), vec![2]);
        assert!(g.allies_of(0).is_empty());

        // T4 joins them; then T2 kills their ally T3, which ends T2's alliance.
        let mut joined = event("{T4} joins {T3}.", &["T3", "T4"], &[]);
        joined.allies = vec!["T4".into(), "T3".into()];
        let betrayal = event("{T2} drowns {T5}.", &["T2", "T5"], &[("T5", Some("T2"))]);
        let round = g
            .check(
                Kind::Day(1),
                written(vec![joined, betrayal, event("{T1} naps.", &["T1"], &[])]),
            )
            .unwrap();
        g.apply(round);
        assert_eq!(g.allies_of(2), vec![1, 3]);
        let mut stab = event("{T2} betrays {T3}.", &["T2", "T3"], &[("T3", Some("T2"))]);
        stab.finds = vec![WrittenFind {
            tribute: "T2".into(),
            weapon: "T3's spoon".into(),
        }];
        let rest = event("{T1} and {T4} hide.", &["T1", "T4"], &[]);
        let round = g.check(Kind::Night(1), written(vec![stab, rest])).unwrap();
        g.apply(round);
        assert!(g.allies_of(1).is_empty(), "the betrayer is on their own");
        assert_eq!(g.tributes[1].weapon.as_deref(), Some("T3's spoon"));
        assert_eq!(g.weapons()[0].as_deref(), Some("vinyl razor"));
    }

    #[test]
    fn finds_and_alliances_are_checked() {
        let g = game(3);
        let check = |e: WrittenEvent| {
            let rest = event("{T3} waits.", &["T3"], &[]);
            g.check(Kind::Scatter, written(vec![e, rest])).unwrap_err()
        };
        let mut outside = event("{T1} and {T2} run.", &["T1", "T2"], &[]);
        outside.finds = vec![WrittenFind {
            tribute: "T3".into(),
            weapon: "rock".into(),
        }];
        assert!(check(outside).contains("T3 finds a weapon in an event they aren't in"));
        let mut dying = event("{T1} kills {T2}.", &["T1", "T2"], &[("T2", Some("T1"))]);
        dying.finds = vec![WrittenFind {
            tribute: "T2".into(),
            weapon: "rock".into(),
        }];
        assert!(check(dying).contains("dies in the same event"));
        let mut long = event("{T1} and {T2} run.", &["T1", "T2"], &[]);
        long.finds = vec![WrittenFind {
            tribute: "T1".into(),
            weapon: "a".repeat(61),
        }];
        assert!(check(long).contains("1 to 60 characters"));
        let mut alone = event("{T1} and {T2} run.", &["T1", "T2"], &[]);
        alone.allies = vec!["T1".into()];
        assert!(check(alone).contains("at least two"));
        let mut both = event("{T1} and {T2} run.", &["T1", "T2"], &[]);
        both.allies = vec!["T1".into(), "T2".into()];
        both.leaves = vec!["T2".into()];
        assert!(check(both).contains("both team up and leave"));
    }

    #[test]
    fn a_good_round_is_applied() {
        let mut g = game(4);
        let round = g
            .check(
                Kind::Scatter,
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
        assert_eq!(g.victims(0), vec![1]);
        assert_eq!(g.victims(2), Vec::<usize>::new());
        assert_eq!(g.winner_index(), None);
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
