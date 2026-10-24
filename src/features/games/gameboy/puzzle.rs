//! The puzzle helper: for a spot plain walking can't reach (steps that carry her somewhere
//! else, things to push, switches that change the way), the strategist asks for help, and
//! code hands a fresh model (GPT-6 Astra at high effort, the `game_puzzle` job) only what
//! the puzzle needs: the area's whole map, the steps in it whose results surprised code,
//! and what the strategist knows. It answers with moves, each a button pressed a few times
//! and where she'll stand after it.
//!
//! Code checks the moves against the map before pressing anything ([`check`]), and if they
//! don't fit, asks once more saying why. Then `actions::follow_moves` presses them one at a
//! time and stops at the first move that doesn't end where the answer said.
//!
//! Nothing here knows a game: the mechanics come from her own movement log and notes. The
//! idea is from Gemini's Pokémon run, whose fresh-context helper solved a maze in one go
//! after the main agent had been stuck on it for days.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::emulator::Button;
use super::map::{AreaMap, Dir, Kind, LEGEND, Pos};
use super::notes::Note;
use super::presses::{self, Press};
use super::world::Logged;
use crate::ai::OpenAi;

/// The solver's job: the strategist's model, at high effort.
pub const JOB: &str = "game_puzzle";

/// Moves in one answer at most, presses in one move, and presses in all.
const MOVES_MOST: usize = 30;
const TIMES_MOST: u32 = 20;
const PRESSES_MOST: u32 = 120;
/// Surprising steps the solver is told at most, the latest.
const SURPRISES_MOST: usize = 20;
/// Steps the map shows each way around her: enough for any Game Boy area.
const MAP_RADIUS: i32 = 80;

const PROMPT: &str = "You solve one puzzle in a Game Boy game: a way to a spot that plain \
walking can't find, because steps carry the player somewhere else, things must be pushed, \
or switches change the way.

You get the area's map as text, where the player stands, the spot to reach, the steps in \
this area whose results surprised code, and what the planner knows. Work out how the \
puzzle works from those, then the moves.

Each move is a press made some times in a row and the step the player stands on after \
it, as its x and y on the map. A press is up, down, left or right (one step), a (push or \
use what the player faces; it doesn't move the player), or buttons held together for a \
jump or a dash, like a+right, held longer with frames after a colon (60 a second). Each \
button can have its own frames: a:20+right:60 holds a for 20 frames and right for 60, so \
the jump's height and its distance are set apart; a+right:30 holds both 30.

Work the frames out from what the player's presses did: the jumps and dashes listed with \
how high they went, on which frame, where they ended and when they landed, and how many \
pixels each frame of holding a direction carries the player, on the ground and in the \
air. A jump goes as far as its \
direction is held while in the air, so hold the direction for the frames the distance \
needs, and past the landing frame only to keep walking. One unlike any tried yet is a \
guess, so end the moves soon after it.

Code presses the moves one at a time. A held press that ends one step from its spot walks \
that step; otherwise code stops at the first move that doesn't end where you said, so say \
where the player will really stand, the puzzle's effects included, and keep moves short \
past anything you're unsure of. The last move ends on the spot. At most 30 moves.

Games don't go exactly the same twice. When a move ends somewhere else, code asks again \
and says how the plans so far went: plan from where the player stands now.";

/// What the strategist asks for: the spot to reach in the area she's in, and what it
/// knows of how the puzzle works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wanted {
    pub x: i32,
    pub y: i32,
    pub what: String,
}

/// One move: `press` (up, down, left, right, a, or buttons held together like
/// `a+right:30`) `times` times, after which she stands at `x`, `y`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub press: String,
    pub times: u32,
    pub x: i32,
    pub y: i32,
}

impl Move {
    /// The direction it walks, or `None` for a press that doesn't walk a step.
    pub fn dir(&self) -> Option<Dir> {
        Dir::ALL.into_iter().find(|d| d.name() == self.press)
    }

    /// The press, for one that doesn't walk a step: a, or buttons held together.
    pub fn press(&self) -> Result<Press, String> {
        match presses::parse(&self.press)?.as_slice() {
            // Start and select open menus: no way through a puzzle.
            [one]
                if !one.buttons.is_empty()
                    && !one
                        .buttons
                        .iter()
                        .any(|b| matches!(b, Button::Start | Button::Select)) =>
            {
                Ok(one.clone())
            }
            _ => Err(
                "only up, down, left, right, a, or buttons held together like a+right:30"
                    .to_string(),
            ),
        }
    }

    pub fn spot(&self) -> Pos {
        (self.x, self.y)
    }

    /// Like `right ×3 to (5, 4)`.
    pub fn line(&self) -> String {
        format!("{} ×{} to ({}, {})", self.press, self.times, self.x, self.y)
    }
}

/// The solver's answer, with what it used.
#[derive(Debug, Clone, PartialEq)]
pub struct Solution {
    pub moves: Vec<Move>,
    /// Tokens used, for the free daily tokens.
    pub tokens: u64,
    /// What it would cost at list prices.
    pub usd: f64,
}

/// The steps in `area` whose results surprised code, as the movement log writes them:
/// moved somewhere other than the next step that way (pushed along, a jump, a warp), or
/// blocked on a step the map shows can be walked on. The latest, without repeats.
pub fn surprises(log: &VecDeque<Logged>, area: &str, map: &AreaMap) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for step in log.iter().filter(|s| s.area == area && s.to_area == area) {
        let next = step.direction.step(step.from);
        let elsewhere = step.to != step.from && step.to != next;
        let walled = step.to == step.from
            && !map.person_at(next)
            && map.kind(next).is_some_and(Kind::walkable);
        if elsewhere || walled {
            let line = step.to_string();
            lines.retain(|l| *l != line);
            lines.push(line);
        }
    }
    let skip = lines.len().saturating_sub(SURPRISES_MOST);
    lines.split_off(skip)
}

/// What the solver is told. `presses` is what her presses did (`path::presses`).
pub fn ask(
    area: &str,
    map: &AreaMap,
    at: Pos,
    wanted: &Wanted,
    surprises: &[String],
    presses: &str,
    notes: &[Note],
) -> String {
    let size = map
        .size
        .map(|(w, h)| format!(" It's {w}×{h} steps (x 0 to {}, y 0 to {}).", w - 1, h - 1))
        .unwrap_or_default();
    let mut text = format!(
        "Area: {area}.{size}\nThe player is at ({}, {}); the spot to reach is ({}, {}).\n\
         What the planner says: {}\n\nThe map (the top line is each column's x, its last \
         digit; each row starts with its y). {LEGEND}.\n{}",
        at.0,
        at.1,
        wanted.x,
        wanted.y,
        wanted.what.trim(),
        map.draw(at, MAP_RADIUS)
    );
    text.push_str("\nSteps here whose results surprised code:\n");
    if surprises.is_empty() {
        text.push_str("- (none seen yet)\n");
    }
    for line in surprises {
        text.push_str(&format!("- {line}\n"));
    }
    if !presses.is_empty() {
        text.push_str(&format!("\n{presses}\n"));
    }
    if !notes.is_empty() {
        text.push_str("\nThe planner's notes:\n");
        for note in notes {
            text.push_str(&format!("- {}\n", note.for_plan()));
        }
    }
    text
}

/// Whether the moves fit the map, from `at` to `to`: few enough, each ending inside the
/// area and not on a wall or sign, a staying where it is, and the last ending on the spot.
/// Says what's wrong if they don't.
pub fn check(moves: &[Move], map: &AreaMap, at: Pos, to: Pos) -> Result<(), String> {
    if moves.is_empty() {
        return Err("there are no moves".to_string());
    }
    if moves.len() > MOVES_MOST {
        return Err(format!(
            "there are {} moves, more than {MOVES_MOST}",
            moves.len()
        ));
    }
    let presses: u32 = moves.iter().map(|m| m.times).sum();
    if presses > PRESSES_MOST {
        return Err(format!(
            "they press {presses} times, more than {PRESSES_MOST}"
        ));
    }
    let mut from = at;
    for (i, one) in moves.iter().enumerate() {
        let n = i + 1;
        let (x, y) = one.spot();
        if one.dir().is_none()
            && let Err(why) = one.press()
        {
            return Err(format!("move {n} presses \"{}\": {why}", one.press));
        }
        if !(1..=TIMES_MOST).contains(&one.times) {
            return Err(format!(
                "move {n} presses {} {} times: 1 to {TIMES_MOST}",
                one.press, one.times
            ));
        }
        if !map.inside(one.spot()) {
            return Err(format!("move {n} ends at ({x}, {y}), outside the area"));
        }
        if let Some(kind @ (Kind::Wall | Kind::Sign | Kind::OneWay(_))) = map.kind(one.spot()) {
            let what = match kind {
                Kind::Wall => "a wall",
                Kind::Sign => "a sign",
                _ => "a one-way step (hopped over, never stood on)",
            };
            return Err(format!(
                "move {n} ends at ({x}, {y}), which the map shows as {what}"
            ));
        }
        if one.press == "a" && one.spot() != from {
            return Err(format!(
                "move {n} presses a but ends at ({x}, {y}), not where it starts ({}, {})",
                from.0, from.1
            ));
        }
        from = one.spot();
    }
    if from != to {
        return Err(format!(
            "the last move ends at ({}, {}), not on the spot ({}, {})",
            from.0, from.1, to.0, to.1
        ));
    }
    Ok(())
}

/// Asks the solver. `rejected` is why code turned down its last answer, if it did.
pub async fn solve(
    openai: &OpenAi,
    asked: &str,
    rejected: Option<(&[Move], &str)>,
) -> anyhow::Result<Solution> {
    let mut input = asked.to_string();
    if let Some((moves, why)) = rejected {
        let lines: Vec<String> = moves.iter().map(Move::line).collect();
        input.push_str(&format!(
            "\nYour last answer was {}, and code turned it down: {why}. Answer again.\n",
            lines.join(", ")
        ));
    }
    let answer = openai
        .structured(JOB, PROMPT, &input, None, "puzzle", &schema())
        .await?;
    let moves = serde_json::from_value(answer.value["moves"].clone())?;
    Ok(Solution {
        moves,
        tokens: answer.tokens,
        usd: answer.used.usd,
    })
}

/// The answer's strict schema: reasoning first, then the moves.
fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "reasoning": {"type": "string"},
            "moves": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "press": {"type": "string"},
                    "times": {"type": "integer"},
                    "x": {"type": "integer"},
                    "y": {"type": "integer"}
                },
                "required": ["press", "times", "x", "y"],
                "additionalProperties": false
            }}
        },
        "required": ["reasoning", "moves"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::moves::Outcome;
    use crate::features::games::gameboy::path::{self, Motion, Pace};

    fn one(press: &str, times: u32, x: i32, y: i32) -> Move {
        Move {
            press: press.to_string(),
            times,
            x,
            y,
        }
    }

    /// A 6×3 room: floor, with a wall at (2, 1).
    fn room() -> AreaMap {
        let mut map = AreaMap::default();
        map.size = Some((6, 3));
        for x in 0..6 {
            for y in 0..3 {
                map.see((x, y), Kind::Floor);
            }
        }
        map.see((2, 1), Kind::Wall);
        map
    }

    #[test]
    fn code_checks_the_moves_against_the_map() {
        let map = room();
        let fine = [
            one("right", 3, 3, 0),
            one("down", 2, 3, 2),
            one("a", 1, 3, 2),
        ];
        assert_eq!(check(&fine, &map, (0, 0), (3, 2)), Ok(()));
        // Pushed along further than it pressed: the puzzle's effects are the solver's to say.
        assert_eq!(
            check(&[one("right", 1, 5, 0)], &map, (0, 0), (5, 0)),
            Ok(())
        );

        let wrong = |moves: &[Move]| check(moves, &map, (0, 0), (3, 2)).unwrap_err();
        assert_eq!(wrong(&[]), "there are no moves");
        assert_eq!(
            wrong(&[one("right", 2, 2, 1), one("right", 1, 3, 2)]),
            "move 1 ends at (2, 1), which the map shows as a wall"
        );
        assert_eq!(
            wrong(&[one("down", 1, 0, 3)]),
            "move 1 ends at (0, 3), outside the area"
        );
        assert_eq!(
            wrong(&[one("a", 1, 0, 1)]),
            "move 1 presses a but ends at (0, 1), not where it starts (0, 0)"
        );
        assert_eq!(
            wrong(&[one("start", 1, 0, 0)]),
            "move 1 presses \"start\": only up, down, left, right, a, or buttons held \
             together like a+right:30"
        );
        assert_eq!(
            wrong(&[one("a+right", 2, 0, 0), one("wait:30", 1, 0, 0)]),
            "move 2 presses \"wait:30\": only up, down, left, right, a, or buttons held \
             together like a+right:30"
        );
        // A jump can end anywhere that isn't a wall.
        assert_eq!(
            check(&[one("a+right:30", 1, 3, 2)], &map, (0, 0), (3, 2)),
            Ok(())
        );
        assert_eq!(
            wrong(&[one("right", 0, 0, 0)]),
            "move 1 presses right 0 times: 1 to 20"
        );
        assert_eq!(
            wrong(&[one("right", 3, 3, 0)]),
            "the last move ends at (3, 0), not on the spot (3, 2)"
        );
        let many = vec![one("a", 1, 0, 0); 31];
        assert_eq!(wrong(&many), "there are 31 moves, more than 30");
        let long = vec![one("a", 20, 0, 0); 7];
        assert_eq!(wrong(&long), "they press 140 times, more than 120");
    }

    fn logged(from: Pos, direction: Dir, to: Pos) -> Logged {
        Logged {
            area: "Hideout".to_string(),
            from,
            direction,
            outcome: Outcome::Moved(1),
            to_area: "Hideout".to_string(),
            to,
        }
    }

    #[test]
    fn surprises_are_steps_that_went_somewhere_else_or_hit_floor() {
        let map = room();
        let log: VecDeque<Logged> = [
            logged((0, 0), Dir::Right, (1, 0)),
            // Carried along four steps.
            logged((1, 0), Dir::Right, (5, 0)),
            // Blocked on floor: something unseen in the way.
            logged((0, 2), Dir::Right, (0, 2)),
            // Blocked by the wall the map shows: no surprise.
            logged((1, 1), Dir::Right, (1, 1)),
            // The same surprise again counts once, as the latest.
            logged((1, 0), Dir::Right, (5, 0)),
            Logged {
                area: "Elsewhere".to_string(),
                ..logged((1, 0), Dir::Right, (5, 0))
            },
        ]
        .into();
        assert_eq!(
            surprises(&log, "Hideout", &map),
            [
                "Hideout (0, 2) right: blocked",
                "Hideout (1, 0) right: moved to (5, 0)"
            ]
        );
    }

    #[test]
    fn the_solver_gets_the_whole_map_and_what_is_known() {
        let map = room();
        let wanted = Wanted {
            x: 3,
            y: 2,
            what: "The floor arrows push you along.".to_string(),
        };
        let notes = [Note::new("Arrows push you to the next wall.", "tried")];
        let text = ask(
            "Hideout",
            &map,
            (0, 0),
            &wanted,
            &["Hideout (1, 0) right: moved to (5, 0)".to_string()],
            &path::presses(
                &[Motion {
                    press: "a+right".to_string(),
                    rise: 32,
                    top: 32,
                    ended: (48, 0),
                    step: 16,
                    peak: 12,
                    landed: 26,
                }],
                &[Pace {
                    way: "right".to_string(),
                    air: false,
                    frames: 30,
                    pixels: 42,
                    step: 16,
                }],
            ),
            &notes,
        );
        assert!(
            text.starts_with(
                "Area: Hideout. It's 6×3 steps (x 0 to 5, y 0 to 2).\nThe player is at (0, 0); \
                 the spot to reach is (3, 2).\nWhat the planner says: The floor arrows push you \
                 along.\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("    012345\n  0 @.....\n  1 ..#...\n  2 ......\n"),
            "{text}"
        );
        assert!(
            text.contains("- Hideout (1, 0) right: moved to (5, 0)\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "- a+right went up 2 steps at its highest (frame 12) and ended 3 right from \
                 the start (landed by frame 26)\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "- right: on the ground 1.4 pixels a frame (a step every 11 frames) (from 1 \
                 measured)"
            ),
            "{text}"
        );
        assert!(
            text.contains("- Arrows push you to the next wall."),
            "{text}"
        );
    }

    #[test]
    fn the_schema_is_strict() {
        let schema = schema();
        assert_eq!(schema["required"], json!(["reasoning", "moves"]));
        let item = &schema["properties"]["moves"]["items"];
        assert_eq!(item["required"], json!(["press", "times", "x", "y"]));
        assert_eq!(item["additionalProperties"], false);
    }
}
