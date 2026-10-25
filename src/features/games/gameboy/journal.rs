//! Her game memory between episodes and sessions: the goal, the strategist's notes and
//! milestones (`notes.rs`), a line for each episode, and what code counts as progress. Pure; the session keeps it in
//! `saves_dir/<rom>/journal.json`, next to her skills.
//!
//! Progress is what code sees, never what she claims: a new area, at least
//! [`STEPS_FOR_PROGRESS`] new steps on the maps, or words on screen never read before.
//! Episodes in a row without any climb the stuck ladder ([`Rung`]). The first finished
//! episode after progress is let off, since walking back out of a building she just went
//! into finds nothing new by nature.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::insights::Insight;
use super::notes::{Milestone, Note};
use super::world::World;

/// Episode lines kept.
pub const LOG_MOST: usize = 200;
/// Screens of words remembered as read, so reading one again isn't progress.
const TEXTS_MOST: usize = 20_000;
/// New steps seen in one episode that count as progress.
pub const STEPS_FOR_PROGRESS: usize = 8;
/// New steps on the maps since the last checkpoint that make a checkpoint, even in an
/// area she was in before: a rollback then goes back over ground covered, not to the
/// last door she went through, which in a big outdoors can be the start of the game.
pub const STEPS_FOR_CHECKPOINT: usize = 80;
/// Episodes without progress at which the ladder's rungs start (see [`Rung`]).
pub const CHANGE_AT: u32 = 3;
pub const REVIEW_AT: u32 = 6;
pub const ROLLBACK_AT: u32 = 9;
pub const GIVE_UP_AT: u32 = 15;
/// The goal when nobody gives one.
pub const DEFAULT_GOAL: &str =
    "Play the game through to the end credits, one story step at a time.";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Journal {
    pub goal: String,
    /// The strategist's notes, as it last wrote them, with what code found checking them.
    pub notes: Vec<Note>,
    /// Steps toward the goal, reached ones first, checked by code after each episode.
    pub milestones: Vec<Milestone>,
    /// What messing around suggested about how the game works (`insights.rs`).
    pub insights: Vec<Insight>,
    /// Whether she has messed around in this game: once, before her first session.
    pub messed: bool,
    /// What the paid models have cost while she played this game, ever (USD).
    pub usd: f64,
    /// Sessions started, ever.
    pub sessions: u32,
    /// Episodes played, ever.
    pub episodes: u32,
    /// One line per episode (and per rollback), oldest first.
    pub log: Vec<String>,
    /// Episodes in a row without progress.
    pub stuck: u32,
    /// Whether a finished episode with nothing new has been let off since the last
    /// progress.
    pub let_off: bool,
    /// Rollbacks since the last progress: each goes one checkpoint further back.
    pub rollbacks: u32,
    /// The last task was essential (`Plan::essential`): stuck on it, she keeps at it
    /// another way, and isn't taken back to a checkpoint away from it.
    pub essential: bool,
    /// What the last task did, for the next plan (also after a restart).
    pub digest: String,
    /// Areas she has been in.
    pub areas: BTreeSet<String>,
    /// Steps seen on all the maps.
    pub seen: usize,
    /// The screens of words read so far, as fingerprints.
    pub texts: BTreeSet<u64>,
    /// Checkpoints made, ever: the next one's number.
    pub checkpoints: u32,
    /// Steps seen on all the maps at the last checkpoint: a game whose whole outdoors is
    /// one place (Link's Awakening) gets a checkpoint for ground covered, too.
    pub seen_at_checkpoint: usize,
}

/// What one episode found that's new.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub areas: Vec<String>,
    pub steps: usize,
    pub texts: usize,
}

impl Found {
    pub fn progress(&self) -> bool {
        !self.areas.is_empty() || self.steps >= STEPS_FOR_PROGRESS || self.texts > 0
    }

    /// Like "new: Route 1, 40 steps, 3 texts", or "nothing new".
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        if !self.areas.is_empty() {
            parts.push(self.areas.join(", "));
        }
        if self.steps > 0 {
            let s = if self.steps == 1 { "" } else { "s" };
            parts.push(format!("{} step{s}", self.steps));
        }
        if self.texts > 0 {
            let s = if self.texts == 1 { "" } else { "s" };
            parts.push(format!("{} text{s}", self.texts));
        }
        if parts.is_empty() {
            "nothing new".to_string()
        } else {
            format!("new: {}", parts.join(", "))
        }
    }
}

/// The stuck ladder: what the session does before planning, by episodes without progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Making progress: plan as usual.
    Play,
    /// 3 or more: tell the strategist, and ask for a different task, exploring first.
    Change,
    /// At 6: a fresh-eyes review of the notes, at high effort.
    Review,
    /// At 9: back to the last checkpoint, keeping her skills.
    Rollback,
    /// At 15: the session ends rather than spend more.
    GiveUp,
}

impl Journal {
    pub fn new(goal: &str) -> Journal {
        Journal {
            goal: goal.to_string(),
            ..Journal::default()
        }
    }

    /// Counts what's new since the last call, from the maps and the words she read, and
    /// moves the stuck count. `done` is whether she finished the task.
    pub fn found(&mut self, world: &World, words: &[String], done: bool) -> Found {
        let mut found = Found::default();
        let mut areas: Vec<&String> = world.areas.keys().collect();
        areas.sort();
        for area in areas {
            if self.areas.insert(area.clone()) {
                found.areas.push(area.clone());
            }
        }
        let seen: usize = world.areas.values().map(|map| map.seen()).sum();
        found.steps = seen.saturating_sub(self.seen);
        self.seen = self.seen.max(seen);
        for words in words {
            if self.texts.len() < TEXTS_MOST && self.texts.insert(fingerprint(words)) {
                found.texts += 1;
            }
        }
        if found.progress() {
            self.stuck = 0;
            self.rollbacks = 0;
            self.let_off = false;
        } else if done && self.stuck == 0 && !self.let_off {
            self.let_off = true;
        } else {
            self.stuck += 1;
        }
        found
    }

    pub fn rung(&self) -> Rung {
        match self.stuck {
            n if n >= GIVE_UP_AT => Rung::GiveUp,
            ROLLBACK_AT if self.essential => Rung::Change,
            ROLLBACK_AT => Rung::Rollback,
            REVIEW_AT => Rung::Review,
            n if n >= CHANGE_AT => Rung::Change,
            _ => Rung::Play,
        }
    }

    pub fn log(&mut self, line: String) {
        if self.log.len() == LOG_MOST {
            self.log.remove(0);
        }
        self.log.push(line);
    }

    /// The last `n` lines, oldest first.
    pub fn recent(&self, n: usize) -> &[String] {
        &self.log[self.log.len().saturating_sub(n)..]
    }
}

/// A screen of words as a number that stays the same across runs (FNV-1a), ignoring case
/// and how the lines break.
fn fingerprint(words: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    for byte in words.to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::map::Kind;

    #[test]
    fn stuck_on_an_essential_task_she_stays_rather_than_going_back() {
        let at = |stuck, essential| {
            Journal {
                stuck,
                essential,
                ..Journal::default()
            }
            .rung()
        };
        assert_eq!(at(ROLLBACK_AT, false), Rung::Rollback);
        assert_eq!(at(ROLLBACK_AT, true), Rung::Change);
        assert_eq!(at(REVIEW_AT, true), Rung::Review);
        assert_eq!(at(GIVE_UP_AT, true), Rung::GiveUp);
    }

    fn world(areas: &[(&str, i32)]) -> World {
        let mut world = World::default();
        for &(name, steps) in areas {
            for x in 0..steps {
                world.area(name).see((x, 0), Kind::Floor);
            }
        }
        world
    }

    #[test]
    fn new_areas_steps_and_words_are_progress() {
        let mut journal = Journal::new("win");
        let words = vec!["Hello there!".to_string()];
        let found = journal.found(&world(&[("Town", 20)]), &words, false);
        assert_eq!(found.areas, vec!["Town"]);
        assert_eq!((found.steps, found.texts), (20, 1));
        assert_eq!(found.line(), "new: Town, 20 steps, 1 text");
        assert_eq!(journal.stuck, 0);

        // The same words with other line breaks and case aren't new.
        let again = vec!["hello\nTHERE!".to_string()];
        let found = journal.found(&world(&[("Town", 25)]), &again, false);
        assert!(!found.progress(), "5 steps and no new words: {found:?}");
        assert_eq!(found.line(), "new: 5 steps");
        assert_eq!(journal.stuck, 1);

        let found = journal.found(&world(&[("Town", 25)]), &["Bye".to_string()], false);
        assert!(found.progress());
        assert_eq!(journal.stuck, 0);
        assert_eq!(
            Found::default().line(),
            "nothing new",
            "an episode with nothing"
        );
    }

    #[test]
    fn one_finished_episode_after_progress_is_let_off() {
        let mut journal = Journal::new("win");
        let town = world(&[("Town", 20)]);
        journal.found(&town, &[], false);
        // Out of the building she just went into: done, nothing new, let off.
        assert!(!journal.found(&town, &[], true).progress());
        assert_eq!(journal.stuck, 0);
        // The next one counts, done or not.
        journal.found(&town, &[], true);
        assert_eq!(journal.stuck, 1);
        // Progress lets the next one off again.
        journal.found(&world(&[("Town", 20), ("Hall", 9)]), &[], false);
        journal.found(&town, &[], true);
        assert_eq!(journal.stuck, 0);
        // A task not done always counts.
        journal.found(&world(&[("Town", 20), ("Hall", 18)]), &[], false);
        journal.found(&town, &[], false);
        assert_eq!(journal.stuck, 1);
    }

    #[test]
    fn the_ladder_climbs_with_episodes_without_progress() {
        let mut journal = Journal::new("win");
        let rungs: Vec<Rung> = (0..16)
            .map(|stuck| {
                journal.stuck = stuck;
                journal.rung()
            })
            .collect();
        use Rung::*;
        assert_eq!(
            rungs,
            vec![
                Play, Play, Play, Change, Change, Change, Review, Change, Change, Rollback, Change,
                Change, Change, Change, Change, GiveUp
            ]
        );
    }

    #[test]
    fn the_log_keeps_the_last_lines_and_the_journal_round_trips() {
        let mut journal = Journal::new("win");
        for i in 0..LOG_MOST + 3 {
            journal.log(format!("#{i}"));
        }
        assert_eq!(journal.log.len(), LOG_MOST);
        assert_eq!(journal.recent(2), ["#201", "#202"]);
        journal.found(&world(&[("Town", 3)]), &["Hi".to_string()], false);
        let json = serde_json::to_string(&journal).unwrap();
        assert_eq!(serde_json::from_str::<Journal>(&json).unwrap(), journal);
        // Fields added later read as empty.
        assert_eq!(serde_json::from_str::<Journal>("{}").unwrap().goal, "");
    }
}
