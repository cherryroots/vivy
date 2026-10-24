//! What code knows about the game world while she plays: a map of each area and the
//! movement log. Pure.
//!
//! A game's own module fills it in (`red/look.rs` for Pokémon Red): after every look it
//! marks what's on screen on the area's map, and after every step it logs where she was,
//! which way she went and where she ended up. The models read it; they never write it,
//! except for her own skills (`skills.rs`).

use std::collections::{HashMap, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::any::Sight;
use super::map::{AreaMap, Dir, Pos};
use super::moves::Outcome;
use super::near::Thing;
use super::ram::{Search, Watch};
use super::red::Trust;
use super::skills::Skill;

/// Steps kept in the movement log.
pub const LOG_MOST: usize = 100;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct World {
    /// A map for each area, by its name.
    pub areas: HashMap<String, AreaMap>,
    /// The last steps, oldest first. Not saved: it's only for the next few prompts.
    #[serde(skip)]
    pub log: VecDeque<Logged>,
    /// Whether the game's memory has agreed with the screen so far.
    #[serde(skip)]
    pub trust: Trust,
    /// The routines she has written for this game.
    pub skills: Vec<Skill>,
    /// The values she found in memory and named (`ram.rs`), saved with her skills.
    #[serde(default)]
    pub watches: Vec<Watch>,
    /// Her memory search under way, if any. Not saved: memory moves on without her.
    #[serde(skip)]
    pub search: Option<Search>,
    /// What code has learned about a game with no memory map (`any/`).
    #[serde(default)]
    pub sight: Sight,
    /// The step just taken, for the look right after it (`walk::take_step` leaves it
    /// here; a look that learns from steps takes it).
    #[serde(skip)]
    pub stepped: Option<Stepped>,
    /// What was near her at the last action's end (`near.rs`). Not saved.
    #[serde(skip)]
    pub near: Vec<Thing>,
}

/// A step, as the look after it is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stepped {
    pub direction: Dir,
    pub outcome: Outcome,
    /// How far she went, in steps (x, y): the screen's scroll, plus her sprite's own move
    /// in games where it walks across a still screen.
    pub scrolled: (i32, i32),
    /// Where her sprite ended up on the screen, when it moved across it.
    pub her: Option<(i32, i32)>,
    /// Whether the screen itself scrolled: where she walks across a still screen, it only
    /// does to slide on to the next room, which is no ledge however far it took her.
    pub slid: bool,
}

/// One step in the movement log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logged {
    pub area: String,
    pub from: Pos,
    pub direction: Dir,
    pub outcome: Outcome,
    /// The area she ended up in, and where.
    pub to_area: String,
    pub to: Pos,
}

impl fmt::Display for Logged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (x, y) = self.from;
        write!(f, "{} ({x}, {y}) {}: ", self.area, self.direction.name())?;
        if self.to_area != self.area {
            return write!(f, "went to {} ({}, {})", self.to_area, self.to.0, self.to.1);
        }
        match self.outcome {
            _ if self.to != self.from => write!(f, "moved to ({}, {})", self.to.0, self.to.1),
            Outcome::Turned => write!(f, "turned"),
            _ => write!(f, "blocked"),
        }
    }
}

/// What one look at the game gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// The area's name.
    pub area: String,
    /// Where she stands in it.
    pub at: Pos,
    /// Whether she can walk right now: the map is showing with no text box, menu or
    /// battle in the way.
    pub can_walk: bool,
    /// The words on the screen.
    pub text: String,
    /// Facts read from the game's memory, as prompt lines (empty if there are none).
    pub facts: String,
    /// How the map's steps sit on the screen, while the map is showing.
    pub grid: Option<Grid>,
}

/// How the map's steps sit on the screen, for drawing what code sees over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Grid {
    /// Pixels a step is across and down.
    pub pixels: i32,
    /// Steps across and down the screen.
    pub across: i32,
    pub down: i32,
    /// The screen step she stands on, counted from the top left.
    pub player: (i32, i32),
}

impl World {
    /// The map of an area, made empty the first time.
    pub fn area(&mut self, name: &str) -> &mut AreaMap {
        self.areas.entry(name.to_string()).or_default()
    }

    pub fn log(&mut self, step: Logged) {
        if self.log.len() == LOG_MOST {
            self.log.pop_front();
        }
        self.log.push_back(step);
    }

    /// The last `n` steps as lines, oldest first.
    pub fn recent(&self, n: usize) -> Vec<String> {
        let skip = self.log.len().saturating_sub(n);
        self.log.iter().skip(skip).map(Logged::to_string).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logged(from: Pos, outcome: Outcome, to_area: &str, to: Pos) -> Logged {
        Logged {
            area: "Pallet Town".to_string(),
            from,
            direction: Dir::Up,
            outcome,
            to_area: to_area.to_string(),
            to,
        }
    }

    #[test]
    fn log_lines_say_what_happened() {
        let town = "Pallet Town";
        let moved = logged((5, 6), Outcome::Moved(1), town, (5, 5));
        assert_eq!(moved.to_string(), "Pallet Town (5, 6) up: moved to (5, 5)");
        let blocked = logged((5, 6), Outcome::Blocked, town, (5, 6));
        assert_eq!(blocked.to_string(), "Pallet Town (5, 6) up: blocked");
        let turned = logged((5, 6), Outcome::Turned, town, (5, 6));
        assert_eq!(turned.to_string(), "Pallet Town (5, 6) up: turned");
        let away = logged((5, 6), Outcome::NewArea, "Route 1", (5, 35));
        assert_eq!(
            away.to_string(),
            "Pallet Town (5, 6) up: went to Route 1 (5, 35)"
        );
    }

    #[test]
    fn the_log_keeps_the_last_steps() {
        let mut world = World::default();
        for i in 0..LOG_MOST as i32 + 5 {
            world.log(logged((0, i), Outcome::Moved(1), "Pallet Town", (0, i - 1)));
        }
        assert_eq!(world.log.len(), LOG_MOST);
        let recent = world.recent(2);
        assert_eq!(recent.len(), 2);
        assert!(recent[1].starts_with(&format!("Pallet Town (0, {})", LOG_MOST + 4)));
    }
}
