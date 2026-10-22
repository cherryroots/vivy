//! The puzzle helper, for real: its moves followed on Red, and GPT-6 Astra solving a maze
//! of arrows that push the player along, which code then plays out.

use std::collections::VecDeque;
use std::fmt::Write as _;

use serde_json::json;

use super::player::Local;
use super::tasks::load;
use super::{gate_on, http, openai, results, setting, spend_db};
use crate::features::games::gameboy::map::{AreaMap, Dir, Kind, Pos};
use crate::features::games::gameboy::moves::Outcome;
use crate::features::games::gameboy::notes::Note;
use crate::features::games::gameboy::puzzle::{self, Move, Wanted};
use crate::features::games::gameboy::supervisor::Player;
use crate::features::games::gameboy::world::Logged;

/// On Route 1 the ledges look like walls, but walking down onto one jumps it: one press,
/// two steps. The moves say so and are followed. Ledges only go down, so a move that says
/// she'll climb back up stops where she really is.
#[tokio::test]
#[ignore = "needs a ROM in VIVY_GB_ROM and the task states"]
async fn puzzle_moves_are_followed_on_red() {
    let mut local = Local::new(load("route1"));
    let acted = local
        .act("walk_to", &json!({"x": 4, "y": 26}))
        .await
        .unwrap();
    assert_eq!(acted.view.at, (4, 26), "{}", acted.outcome);
    let map = local.world.areas["Route 1"].clone();
    assert_eq!(
        map.kind((4, 27)),
        Some(Kind::Wall),
        "the ledge looks like a wall"
    );

    let jump = [Move {
        press: "down".to_string(),
        times: 1,
        x: 4,
        y: 28,
    }];
    assert_eq!(puzzle::check(&jump, &map, (4, 26), (4, 28)), Ok(()));
    let onto_the_ledge = [Move {
        y: 27,
        ..jump[0].clone()
    }];
    assert_eq!(
        puzzle::check(&onto_the_ledge, &map, (4, 26), (4, 27)),
        Err("move 1 ends at (4, 27), which the map shows as a wall".to_string())
    );

    let acted = local
        .act("follow_moves", &json!({ "moves": jump }))
        .await
        .unwrap();
    assert_eq!(acted.outcome, "Followed the move; she's at (4, 28).");
    let map = &local.world.areas["Route 1"];
    // The hop taught the map that this step is a ledge, crossed going down only.
    assert_eq!(map.kind((4, 27)), Some(Kind::OneWay(Dir::Down)));
    assert_eq!(map.step_to((4, 26), Dir::Down, false), Some((4, 28)));
    assert_eq!(map.step_to((4, 28), Dir::Up, false), None);
    assert_eq!(
        puzzle::surprises(&local.world.log, "Route 1", map),
        ["Route 1 (4, 26) down: moved to (4, 28)"]
    );

    let back_up = [Move {
        press: "up".to_string(),
        times: 1,
        x: 4,
        y: 26,
    }];
    let acted = local
        .act("follow_moves", &json!({ "moves": back_up }))
        .await
        .unwrap();
    assert_eq!(
        acted.outcome,
        "Move 1 of 1 (up ×1 to (4, 26)) ended at (4, 28) instead. Stopped there."
    );
}

/// A room of arrows: stepping onto one pushes the player its way until something stops
/// them, through other arrows too. Code's map shows them as floor, as it would in a game.
/// The spot is in a nook walking can't reach: the arrows keep pushing past it.
const MAZE: [&str; 9] = [
    "############",
    "#@..D...D#.#",
    "#..#..R.##T#",
    "#..D.....L.#",
    "#.....#..U.#",
    "#.....#D...#",
    "##D.#......#",
    "#....##....#",
    "############",
];

struct Maze {
    rows: Vec<Vec<char>>,
}

impl Maze {
    fn new() -> Maze {
        Maze {
            rows: MAZE.iter().map(|r| r.chars().collect()).collect(),
        }
    }

    fn at(&self, (x, y): Pos) -> char {
        self.rows[y as usize][x as usize]
    }

    fn find(&self, c: char) -> Pos {
        for (y, row) in self.rows.iter().enumerate() {
            if let Some(x) = row.iter().position(|&r| r == c) {
                return (x as i32, y as i32);
            }
        }
        panic!("no {c}");
    }

    fn arrow(&self, pos: Pos) -> Option<Dir> {
        match self.at(pos) {
            'R' => Some(Dir::Right),
            'L' => Some(Dir::Left),
            'U' => Some(Dir::Up),
            'D' => Some(Dir::Down),
            _ => None,
        }
    }

    /// Where one press of `dir` from `from` leaves the player.
    fn step(&self, from: Pos, dir: Dir) -> Pos {
        let mut at = dir.step(from);
        if self.at(at) == '#' {
            return from;
        }
        let mut pushed = Vec::new();
        while let Some(push) = self.arrow(at) {
            if pushed.contains(&at) || self.at(push.step(at)) == '#' {
                break;
            }
            pushed.push(at);
            at = push.step(at);
        }
        at
    }

    /// The map code would have: walls, and floor everywhere else.
    fn map(&self) -> AreaMap {
        let mut map = AreaMap::default();
        map.size = Some((MAZE[0].len() as i32, MAZE.len() as i32));
        for (y, row) in self.rows.iter().enumerate() {
            for (x, &c) in row.iter().enumerate() {
                let kind = if c == '#' { Kind::Wall } else { Kind::Floor };
                map.see((x as i32, y as i32), kind);
            }
        }
        map
    }

    /// The movement log of someone who has stepped onto each arrow once, from its first
    /// open side.
    fn log(&self) -> VecDeque<Logged> {
        let mut log = VecDeque::new();
        for (y, row) in self.rows.iter().enumerate() {
            for x in 0..row.len() {
                let at = (x as i32, y as i32);
                if self.arrow(at).is_none() {
                    continue;
                }
                for dir in Dir::ALL {
                    let from = match dir {
                        Dir::Up => (at.0, at.1 + 1),
                        Dir::Down => (at.0, at.1 - 1),
                        Dir::Left => (at.0 + 1, at.1),
                        Dir::Right => (at.0 - 1, at.1),
                    };
                    if self.at(from) == '#' || self.arrow(from).is_some() {
                        continue;
                    }
                    let to = self.step(from, dir);
                    log.push_back(Logged {
                        area: "Arrow Room".to_string(),
                        from,
                        direction: dir,
                        outcome: Outcome::Moved((to.0 - from.0).abs() + (to.1 - from.1).abs()),
                        to_area: "Arrow Room".to_string(),
                        to,
                    });
                    break;
                }
            }
        }
        log
    }
}

#[test]
fn the_maze_needs_the_arrows() {
    let maze = Maze::new();
    // Walking the map's shortest way, as walk_to would, gets pushed off it.
    let map = maze.map();
    let (start, spot) = (maze.find('@'), maze.find('T'));
    let mut at = start;
    for _ in 0..40 {
        let Some(way) = map.path(at, spot, false) else {
            break;
        };
        let Some(&dir) = way.first() else { break };
        at = maze.step(at, dir);
    }
    assert_ne!(at, spot, "plain walking gets there");
    let surprises = puzzle::surprises(&maze.log(), "Arrow Room", &map);
    assert!(surprises.len() >= 5, "{surprises:#?}");
}

/// GPT-6 Astra solves the maze from the map and the surprises alone, and code plays its
/// moves out on the maze. The report goes in the project files.
#[tokio::test]
#[ignore = "calls OpenAI (the free daily tokens)"]
async fn the_puzzle_helper_solves_a_maze_of_arrows() {
    if !gate_on() {
        return;
    }
    let maze = Maze::new();
    let map = maze.map();
    let (start, spot) = (maze.find('@'), maze.find('T'));
    let wanted = Wanted {
        x: spot.0,
        y: spot.1,
        what: "Some floor steps here push the player along when stepped on, and keep \
               pushing until something stops them; the surprises show where. Walking \
               straight there gets pushed past the spot."
            .to_string(),
    };
    let surprises = puzzle::surprises(&maze.log(), "Arrow Room", &map);
    let notes = [Note::new(
        "Arrow Room: pushed steps can't be seen on the map.",
        "tried",
    )];
    let asked = puzzle::ask("Arrow Room", &map, start, &wanted, &surprises, &notes);
    let model = setting("VIVY_GB_STRATEGIST", "gpt-6-astra");
    let db = spend_db().await;
    let openai = openai(&http(), &db, &model, "high");

    let mut report =
        format!("# Puzzle helper: a maze of arrows ({model})\n\n## Asked\n\n```\n{asked}```\n");
    let mut rejected: Option<(Vec<Move>, String)> = None;
    let mut solved = None;
    let (mut tokens, mut usd) = (0, 0.0);
    for round in 1..=2 {
        let last = rejected
            .as_ref()
            .map(|(m, why)| (m.as_slice(), why.as_str()));
        let solution = puzzle::solve(&openai, &asked, last).await.unwrap();
        tokens += solution.tokens;
        usd += solution.usd;
        let lines: Vec<String> = solution.moves.iter().map(Move::line).collect();
        writeln!(report, "\n## Answer {round}\n\n{}\n", lines.join(", ")).unwrap();
        match puzzle::check(&solution.moves, &map, start, spot) {
            Ok(()) => {
                solved = Some(solution.moves);
                break;
            }
            Err(why) => {
                writeln!(report, "Code turned it down: {why}.").unwrap();
                rejected = Some((solution.moves, why));
            }
        }
    }
    let moves = solved.expect("code turned its moves down twice");

    // Played out with the maze's real rules, checked after each move as code does.
    let mut at = start;
    let mut played = String::new();
    for (i, one) in moves.iter().enumerate() {
        let dir = one.dir().expect("only walking here");
        for _ in 0..one.times {
            at = maze.step(at, dir);
        }
        writeln!(played, "- {}: at ({}, {})", one.line(), at.0, at.1).unwrap();
        if at != one.spot() {
            writeln!(played, "Move {} didn't end where it said.", i + 1).unwrap();
            break;
        }
    }
    writeln!(
        report,
        "\n## Played out\n\n{played}\n{} moves, {tokens} tokens, ${usd:.3} at list prices.",
        moves.len()
    )
    .unwrap();
    let folder = results("puzzle");
    std::fs::write(folder.join("report.md"), &report).unwrap();
    println!("{report}");
    assert_eq!(at, spot, "{report}");
}
