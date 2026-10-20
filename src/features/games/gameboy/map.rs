//! A map of each area that fills in as she plays, kept by code. Pure.
//!
//! Each area is a grid of steps (the squares the player walks between). A step is unseen
//! until it has been on screen; then it's known as floor, wall, grass, a door, a sign, the
//! edge of the map, and so on. What the movement log learns beats what the tiles suggest:
//! a step that blocked her is a wall from that side, and a step she walked onto is floor,
//! whatever it looked like. People move, so they're kept apart and only block the way while
//! they're seen.
//!
//! Code keeps the map, not the model: Gemini's Pokémon run dropped its model-kept world
//! graph because the model never kept it up to date.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::emulator::Button;
use super::moves::offset;

/// A step's place in its area: x right, y down.
pub type Pos = (i32, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Floor,
    Wall,
    /// Tall grass, where wild Pokémon live.
    Grass,
    /// A door, stairs, or another way to a different area.
    Door,
    /// Something to read (it can't be walked onto).
    Sign,
    /// Past the edge of the map: walking on goes to the next area.
    Edge,
}

impl Kind {
    /// The letter on the map text.
    fn symbol(self) -> char {
        match self {
            Kind::Floor => '.',
            Kind::Wall => '#',
            Kind::Grass => '"',
            Kind::Door => 'D',
            Kind::Sign => 'S',
            Kind::Edge => '>',
        }
    }

    /// Whether the player can stand on it, as far as its look says.
    pub fn walkable(self) -> bool {
        matches!(self, Kind::Floor | Kind::Grass | Kind::Door | Kind::Edge)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AreaMap {
    /// What each seen step is.
    #[serde(with = "as_list")]
    kinds: HashMap<Pos, Kind>,
    /// Steps tried and blocked: (from, direction).
    blocked: HashSet<(Pos, Dir)>,
    /// Steps she walked onto, whatever their look.
    walked: HashSet<Pos>,
    /// Where each door led, by its position.
    #[serde(with = "as_list")]
    pub doors: HashMap<Pos, String>,
    /// Her own markers ("locked door, needs a key").
    #[serde(with = "as_list")]
    pub markers: HashMap<Pos, String>,
    /// People seen on the last screen. Not saved: they move.
    #[serde(skip)]
    people: HashSet<Pos>,
}

/// A direction, as stored in the map (buttons aren't serializable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    pub const ALL: [Dir; 4] = [Dir::Up, Dir::Down, Dir::Left, Dir::Right];

    pub fn button(self) -> Button {
        match self {
            Dir::Up => Button::Up,
            Dir::Down => Button::Down,
            Dir::Left => Button::Left,
            Dir::Right => Button::Right,
        }
    }

    pub fn step(self, (x, y): Pos) -> Pos {
        let (dx, dy) = offset(self.button()).unwrap_or((0, 0));
        (x + dx, y + dy)
    }

    pub fn name(self) -> &'static str {
        self.button().name()
    }
}

impl AreaMap {
    /// Notes what a step looks like. A step she has walked onto stays walkable.
    pub fn see(&mut self, pos: Pos, kind: Kind) {
        if self.walked.contains(&pos) && !kind.walkable() {
            return;
        }
        // A door stays a door: the tiles under some doors look like floor or wall.
        if self.kinds.get(&pos) == Some(&Kind::Door) {
            return;
        }
        self.kinds.insert(pos, kind);
    }

    /// The people on the screen now, replacing the last screen's.
    pub fn see_people(&mut self, people: impl IntoIterator<Item = Pos>) {
        self.people = people.into_iter().collect();
    }

    /// A step from `from` in `dir` was blocked.
    pub fn blocked(&mut self, from: Pos, dir: Dir) {
        self.blocked.insert((from, dir));
    }

    /// She stands at `pos`, having walked there.
    pub fn walked(&mut self, pos: Pos) {
        self.walked.insert(pos);
        if !self.kinds.get(&pos).is_some_and(|k| k.walkable()) {
            self.kinds.insert(pos, Kind::Floor);
        }
        // Walking away from a step that blocked her before may mean the blocker (a
        // person) has gone; only blocks out of her own step are kept as they are.
    }

    pub fn kind(&self, pos: Pos) -> Option<Kind> {
        self.kinds.get(&pos).copied()
    }

    /// Whether a step from `from` in `dir` can be tried: not blocked before, not into a
    /// person, onto a walkable step (or an unseen one, when `explore`).
    pub fn can_step(&self, from: Pos, dir: Dir, explore: bool) -> bool {
        let to = dir.step(from);
        if self.blocked.contains(&(from, dir)) || self.people.contains(&to) {
            return false;
        }
        if self.walked.contains(&to) {
            return true;
        }
        match self.kinds.get(&to) {
            Some(kind) => kind.walkable(),
            None => explore,
        }
    }

    /// The shortest way from `from` to `to`, as directions, through known walkable steps
    /// (and unseen ones too when `explore`). `None` when there's no way. Doors and edges
    /// can only be the last step, since stepping onto one leaves the area.
    pub fn path(&self, from: Pos, to: Pos, explore: bool) -> Option<Vec<Dir>> {
        if from == to {
            return Some(Vec::new());
        }
        let mut came: HashMap<Pos, (Pos, Dir)> = HashMap::new();
        let mut queue = VecDeque::from([from]);
        // Big enough for any Game Boy area, small enough to stop on an open edge.
        const MOST: usize = 20_000;
        while let Some(pos) = queue.pop_front() {
            if came.len() > MOST {
                break;
            }
            if pos != from && matches!(self.kind(pos), Some(Kind::Door | Kind::Edge)) {
                continue;
            }
            for dir in Dir::ALL {
                let next = dir.step(pos);
                if next == from || came.contains_key(&next) || !self.can_step(pos, dir, explore) {
                    continue;
                }
                came.insert(next, (pos, dir));
                if next == to {
                    let mut way = Vec::new();
                    let mut at = to;
                    while at != from {
                        let (back, dir) = came[&at];
                        way.push(dir);
                        at = back;
                    }
                    way.reverse();
                    return Some(way);
                }
                queue.push_back(next);
            }
        }
        None
    }

    /// Unseen steps next to steps she can reach from `from`: where exploring goes next,
    /// nearest first.
    pub fn frontier(&self, from: Pos) -> Vec<Pos> {
        let mut found = Vec::new();
        let mut reached = HashSet::from([from]);
        let mut queue = VecDeque::from([from]);
        while let Some(pos) = queue.pop_front() {
            if pos != from && matches!(self.kind(pos), Some(Kind::Door | Kind::Edge)) {
                continue;
            }
            for dir in Dir::ALL {
                let next = dir.step(pos);
                if reached.contains(&next) || self.blocked.contains(&(pos, dir)) {
                    continue;
                }
                if self.kind(next).is_none() && !self.walked.contains(&next) {
                    if !found.contains(&next) {
                        found.push(next);
                    }
                    continue;
                }
                if self.can_step(pos, dir, false) {
                    reached.insert(next);
                    queue.push_back(next);
                }
            }
        }
        found
    }

    /// The map as text, one character per step, around `at` (her position, `@`), at most
    /// `radius` steps each way. Unseen steps are `?`, people `P`, her markers `!`. The top
    /// line gives each column's x (its last digit) and each row starts with its y, so a
    /// model can name a step to walk to.
    pub fn draw(&self, at: Pos, radius: i32) -> String {
        let known: Vec<Pos> = self
            .kinds
            .keys()
            .copied()
            .chain(std::iter::once(at))
            .collect();
        let clamp = |v: i32, lo: i32, hi: i32| v.max(lo).min(hi);
        let min_x = clamp(
            known.iter().map(|p| p.0).min().unwrap_or(0),
            at.0 - radius,
            at.0,
        );
        let max_x = clamp(
            known.iter().map(|p| p.0).max().unwrap_or(0),
            at.0,
            at.0 + radius,
        );
        let min_y = clamp(
            known.iter().map(|p| p.1).min().unwrap_or(0),
            at.1 - radius,
            at.1,
        );
        let max_y = clamp(
            known.iter().map(|p| p.1).max().unwrap_or(0),
            at.1,
            at.1 + radius,
        );
        let mut out = String::from("    ");
        for x in min_x..=max_x {
            out.push_str(&x.rem_euclid(10).to_string());
        }
        out.push('\n');
        for y in min_y..=max_y {
            out.push_str(&format!("{y:>3} "));
            for x in min_x..=max_x {
                let pos = (x, y);
                out.push(if pos == at {
                    '@'
                } else if self.people.contains(&pos) {
                    'P'
                } else if self.markers.contains_key(&pos) {
                    '!'
                } else {
                    self.kind(pos).map(Kind::symbol).unwrap_or('?')
                });
            }
            out.push('\n');
        }
        out
    }

    /// Whether someone was standing at `pos` on the last screen.
    pub fn person_at(&self, pos: Pos) -> bool {
        self.people.contains(&pos)
    }
}

/// JSON keys must be strings, so maps keyed by a position are saved as lists of pairs.
mod as_list {
    use std::collections::HashMap;
    use std::hash::Hash;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(
        map: &HashMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K, V, D>(deserializer: D) -> Result<HashMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        let pairs = Vec::<(K, V)>::deserialize(deserializer)?;
        Ok(pairs.into_iter().collect())
    }
}

/// What the map's letters mean, for a prompt.
pub const LEGEND: &str = "@ you, . floor, # wall, \" tall grass, D door or stairs, \
S sign, > edge (walk on to the next area), P person, ! your marker, ? not seen yet";

#[cfg(test)]
mod tests {
    use super::*;

    /// A map from rows of letters, with (0, 0) at the top left.
    fn map_of(rows: &[&str]) -> AreaMap {
        let mut map = AreaMap::default();
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                let kind = match c {
                    '.' => Kind::Floor,
                    '#' => Kind::Wall,
                    '"' => Kind::Grass,
                    'D' => Kind::Door,
                    'S' => Kind::Sign,
                    '>' => Kind::Edge,
                    _ => continue,
                };
                map.see((x as i32, y as i32), kind);
            }
        }
        map
    }

    fn names(way: &[Dir]) -> String {
        way.iter().map(|d| &d.name()[..1]).collect()
    }

    #[test]
    fn finds_the_shortest_way_around_walls() {
        let map = map_of(&[
            "....#....",
            ".##.#.##.",
            ".#.....#.",
            ".#.###.#D",
            ".........",
        ]);
        let way = map.path((0, 0), (8, 3), false).unwrap();
        // Straight across is 11, but the walls make every way at least 13.
        assert_eq!(way.len(), 13);
        let mut at = (0, 0);
        for dir in &way {
            at = dir.step(at);
            assert!(map.kind(at).unwrap().walkable(), "{at:?}");
        }
        assert_eq!(at, (8, 3));
        assert_eq!(map.path((0, 0), (4, 0), false), None, "walked into a wall");
    }

    #[test]
    fn a_blocked_step_and_people_change_the_way() {
        let mut map = map_of(&["...", "...", "..."]);
        assert_eq!(names(&map.path((0, 0), (2, 0), false).unwrap()), "rr");
        map.blocked((1, 0), Dir::Right);
        let way = map.path((0, 0), (2, 0), false).unwrap();
        assert_eq!(way.len(), 4);
        map.see_people([(1, 1)]);
        assert_eq!(names(&map.path((0, 1), (2, 1), false).unwrap()).len(), 4);
        map.see_people([]);
        assert_eq!(names(&map.path((0, 1), (2, 1), false).unwrap()), "rr");
    }

    #[test]
    fn exploring_goes_through_unseen_steps_but_not_through_doors() {
        let map = map_of(&[".D.", "...", "#.#"]);
        assert_eq!(map.path((0, 0), (5, 0), false), None);
        assert!(map.path((0, 0), (5, 0), true).is_some());
        // The way to (2, 0) goes around the door, not through it.
        let way = map.path((0, 0), (2, 0), false).unwrap();
        assert_eq!(names(&way), "drru");
        // A door can be where she's going.
        assert_eq!(names(&map.path((0, 0), (1, 0), false).unwrap()), "r");
    }

    #[test]
    fn walking_beats_looks() {
        let mut map = map_of(&[".#."]);
        assert!(!map.can_step((0, 0), Dir::Right, false));
        map.walked((1, 0));
        assert!(map.can_step((0, 0), Dir::Right, false));
        // Seeing it as a wall again doesn't undo it.
        map.see((1, 0), Kind::Wall);
        assert_eq!(map.kind((1, 0)), Some(Kind::Floor));
    }

    #[test]
    fn the_frontier_is_the_unseen_edge_of_what_she_can_reach() {
        let map = map_of(&["..#", "..#", "###"]);
        let frontier = map.frontier((0, 0));
        // Above and left of the reachable 2×2 square.
        assert!(frontier.contains(&(0, -1)) && frontier.contains(&(-1, 1)));
        assert!(!frontier.contains(&(3, 0)), "behind a wall");
        assert_eq!(frontier.len(), 4);
    }

    #[test]
    fn draws_the_map_around_her() {
        let mut map = map_of(&["#####", "#...#", "#.S.D", "#####"]);
        map.see_people([(3, 1)]);
        map.markers.insert((1, 2), "start".to_string());
        assert_eq!(
            map.draw((2, 1), 10),
            "    01234\n  0 #####\n  1 #.@P#\n  2 #!S.D\n  3 #####\n"
        );
        // Cut to one step each way (nothing is known above or left of the corner).
        assert_eq!(map.draw((0, 0), 1), "    01\n  0 @#\n  1 #.\n");
    }

    #[test]
    fn saves_and_loads() {
        let mut map = map_of(&[".#D"]);
        map.blocked((0, 0), Dir::Up);
        map.walked((0, 0));
        map.doors.insert((2, 0), "Pallet Town".to_string());
        let json = serde_json::to_string(&map).unwrap();
        let back: AreaMap = serde_json::from_str(&json).unwrap();
        assert_eq!(back.draw((0, 0), 5), map.draw((0, 0), 5));
        assert!(!back.can_step((0, 0), Dir::Up, true));
        assert_eq!(back.doors.get(&(2, 0)).unwrap(), "Pallet Town");
    }
}
