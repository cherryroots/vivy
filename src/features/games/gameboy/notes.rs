//! The strategist's notes and milestones, and what code checks them against.
//!
//! - A [`Note`] is a fact it keeps, with where the fact came from (`seen` on screen,
//!   `tried` in play, `hint from Cherry`, a `guess`) and what code found when it checked
//!   the note against play ([`Check`]). Code only checks what it can see for itself: a
//!   place and steps on the maps it has drawn, and facts that change by themselves (HP,
//!   money, where she stands) and so don't belong in a note.
//! - A [`Milestone`] is a step toward the goal, like "Reach Viridian City" or "Win the
//!   Boulder Badge", with a test code runs on what perception reads: being in an area, or
//!   a line of the game's facts saying something. Never her own claim.
//!
//! Pure: the journal keeps both, the session runs the checks after each episode.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::map::Kind;
use super::world::World;

/// Milestones not yet reached that the strategist can keep.
pub const MILESTONES_AHEAD: usize = 8;

/// One note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "NoteFile")]
pub struct Note {
    pub text: String,
    /// Where it came from: `seen`, `tried`, `hint from <name>`, `guess`. Empty for notes
    /// from before notes had sources.
    pub source: String,
    /// What code found when it last checked.
    pub check: Check,
}

/// What code found when it checked a note against play.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "is", content = "why")]
pub enum Check {
    /// Nothing code can check, or nothing seen yet.
    #[default]
    Unchecked,
    /// The maps agree, like a door where it says the entrance is.
    Agrees(String),
    /// The maps disagree, like a wall where it says the entrance is.
    Disagrees(String),
    /// It's about something that changes by itself (HP, money, where she stands): the
    /// game's facts show it as it is now, so it goes out of date.
    Changes,
}

/// A note as saved: an object, or a plain string from before notes had sources.
#[derive(Deserialize)]
#[serde(untagged)]
enum NoteFile {
    Plain(String),
    Full {
        text: String,
        #[serde(default)]
        source: String,
        #[serde(default)]
        check: Check,
    },
}

impl From<NoteFile> for Note {
    fn from(file: NoteFile) -> Note {
        match file {
            NoteFile::Plain(text) => Note::new(text, ""),
            NoteFile::Full {
                text,
                source,
                check,
            } => Note {
                text,
                source,
                check,
            },
        }
    }
}

impl Note {
    pub fn new(text: impl Into<String>, source: impl Into<String>) -> Note {
        Note {
            text: text.into(),
            source: source.into(),
            check: Check::Unchecked,
        }
    }

    /// For the strategist: the note, then where it came from and what code found.
    pub fn for_plan(&self) -> String {
        let mut about = Vec::new();
        if !self.source.is_empty() {
            about.push(self.source.clone());
        }
        match &self.check {
            Check::Unchecked => {}
            Check::Agrees(why) => about.push(format!("code checked: {why}")),
            Check::Disagrees(why) => about.push(format!("code found it WRONG: {why}")),
            Check::Changes => {
                about.push("changes by itself; the facts show it as it is now".to_string());
            }
        }
        if about.is_empty() {
            self.text.clone()
        } else {
            format!("{} [{}]", self.text, about.join("; "))
        }
    }

    /// For people watching: a mark for what code found, and who gave a hint.
    pub fn shown(&self) -> String {
        let mark = match self.check {
            Check::Agrees(_) => "✅ ",
            Check::Disagrees(_) => "❌ ",
            Check::Changes => "🔄 ",
            Check::Unchecked if self.source == "guess" => "❔ ",
            Check::Unchecked => "",
        };
        let from = if self.source.starts_with("hint") {
            format!(" ({})", self.source)
        } else {
            String::new()
        };
        format!("{mark}{}{from}", self.text)
    }
}

/// Checks each note against the maps, keeping the rest as they are.
pub fn check_all(notes: &mut [Note], world: &World) {
    for note in notes {
        note.check = check(&note.text, world);
    }
}

/// What code can tell about a note: whether it's about something that changes by itself,
/// else whether the steps it names agree with the map of the area it names.
pub fn check(text: &str, world: &World) -> Check {
    if changes(text) {
        return Check::Changes;
    }
    let spots = spots(text);
    if spots.is_empty() {
        return Check::Unchecked;
    }
    let Some(area) = area_in(text, world.areas.keys()) else {
        return Check::Unchecked;
    };
    let map = &world.areas[area];
    let way_in = [
        "door", "entrance", "exit", "stairs", "warp", "way out", "way in",
    ]
    .iter()
    .any(|word| text.to_lowercase().contains(word));
    let mut agrees = Vec::new();
    for spot in spots {
        let Some(kind) = map.kind(spot) else {
            continue;
        };
        let at = format!("({}, {}) in {area}", spot.0, spot.1);
        match kind {
            Kind::Wall => return Check::Disagrees(format!("{at} is a wall on the map")),
            Kind::Door | Kind::Edge => agrees.push(format!("{at} is a way out on the map")),
            // A door is hard to tell from the floor in front of it, so a way in that's
            // floor isn't counted either way.
            _ if way_in => {}
            Kind::Sign => agrees.push(format!("{at} is a sign on the map")),
            _ => agrees.push(format!("{at} is on the map")),
        }
    }
    if agrees.is_empty() {
        Check::Unchecked
    } else {
        Check::Agrees(agrees.join(", "))
    }
}

/// Whether a note is about something the game's facts show as it is now: HP, levels,
/// money, or where she stands.
fn changes(text: &str) -> bool {
    let lower = text.to_lowercase();
    let now = [
        "can walk",
        "is standing",
        "is inside",
        "is currently",
        "right now",
    ];
    if now.iter().any(|said| lower.contains(said)) {
        return true;
    }
    // A number right before "hp", or right after "lv", "¥" or "$".
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|w| !w.is_empty())
        .collect();
    let number = |w: &str| w.chars().next().is_some_and(|c| c.is_ascii_digit());
    words.windows(2).any(|pair| {
        (number(pair[0]) && pair[1].starts_with("hp")) || (pair[0] == "lv" && number(pair[1]))
    }) || words.iter().any(|w| {
        let rest = w
            .strip_prefix('¥')
            .or_else(|| w.strip_prefix('$'))
            .or_else(|| w.strip_prefix("lv"));
        rest.is_some_and(number) || (w.ends_with("hp") && number(w))
    })
}

/// The steps a note names, written `(23, 25)` or `x 23, y 25`.
pub fn spots(text: &str) -> Vec<(i32, i32)> {
    let mut found = Vec::new();
    // `(23, 25)`
    for (start, _) in text.match_indices('(') {
        let rest = &text[start + 1..];
        let Some(end) = rest.find(')') else { continue };
        let inside: Vec<&str> = rest[..end].split(',').map(str::trim).collect();
        if let [x, y] = inside[..]
            && let (Ok(x), Ok(y)) = (x.parse(), y.parse())
        {
            found.push((x, y));
        }
    }
    // `x 23, y 25` and `x=23, y=25`
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || c == ',' || c == '=')
        .filter(|w| !w.is_empty())
        .collect();
    for w in words.windows(4) {
        if w[0] == "x"
            && w[2] == "y"
            && let (Ok(x), Ok(y)) = (w[1].parse(), w[3].trim_end_matches('.').parse())
        {
            found.push((x, y));
        }
    }
    found
}

/// The area a text names: the longest area name in it, whatever the case.
fn area_in<'a>(text: &str, areas: impl Iterator<Item = &'a String>) -> Option<&'a String> {
    let lower = text.to_lowercase();
    areas
        .filter(|area| !area.is_empty() && lower.contains(&area.to_lowercase()))
        .max_by_key(|area| area.len())
}

/// A step toward the goal, which code checks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Milestone {
    /// Like "Reach Viridian City".
    pub name: String,
    /// Reached once she has been in this area (empty: not this kind).
    pub area: String,
    /// Reached once the game's fact starting with this name (like `Badges`)...
    pub fact: String,
    /// ...has these words in it (like `Boulder`), whatever the case.
    pub has: String,
    /// Once reached: the episode, and what playing had cost by then (USD), for how long
    /// each milestone took.
    pub reached: Option<Reached>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Reached {
    pub episode: u32,
    pub usd: f64,
}

impl Milestone {
    /// Whether it can be checked at all: an area, or a fact and its words.
    pub fn checkable(&self) -> bool {
        !self.area.trim().is_empty()
            || (!self.fact.trim().is_empty() && !self.has.trim().is_empty())
    }

    /// Whether what perception reads says she's there: an area she has been in, or the
    /// fact line saying it.
    pub fn met(&self, areas: &BTreeSet<String>, facts: &str) -> bool {
        let area = self.area.trim().to_lowercase();
        if !area.is_empty() {
            return areas.iter().any(|a| a.to_lowercase() == area);
        }
        let (fact, has) = (
            self.fact.trim().to_lowercase(),
            self.has.trim().to_lowercase(),
        );
        if fact.is_empty() || has.is_empty() {
            return false;
        }
        facts.lines().any(|line| {
            let line = line.trim().to_lowercase();
            line.split_once(':')
                .is_some_and(|(name, value)| name.trim() == fact && value.contains(&has))
        })
    }

    /// For the strategist: the name and its test.
    pub fn for_plan(&self) -> String {
        let test = if !self.area.trim().is_empty() {
            format!("in area \"{}\"", self.area.trim())
        } else {
            format!("fact \"{}\" has \"{}\"", self.fact.trim(), self.has.trim())
        };
        match self.reached {
            Some(r) => format!("{} ({test}): reached in episode {}", self.name, r.episode),
            None => format!("{} ({test})", self.name),
        }
    }
}

/// Takes the strategist's milestones: the reached ones stay as they are, first, and its
/// list (without any that can't be checked or repeat a reached one) replaces the rest.
/// An empty list keeps them all, as an answer without any does.
pub fn replace_ahead(kept: &mut Vec<Milestone>, theirs: Vec<Milestone>) {
    if theirs.is_empty() {
        return;
    }
    kept.retain(|m| m.reached.is_some());
    let same = |a: &Milestone, b: &Milestone| a.name.trim().eq_ignore_ascii_case(b.name.trim());
    for mut milestone in theirs {
        if !milestone.checkable() || kept.iter().any(|k| same(k, &milestone)) {
            continue;
        }
        if kept.iter().filter(|m| m.reached.is_none()).count() == MILESTONES_AHEAD {
            break;
        }
        milestone.reached = None;
        kept.push(milestone);
    }
}

/// Marks the milestones perception says she has reached. Gives back their names.
pub fn reach(
    milestones: &mut [Milestone],
    areas: &BTreeSet<String>,
    facts: &str,
    at: Reached,
) -> Vec<String> {
    let mut reached = Vec::new();
    for milestone in milestones.iter_mut().filter(|m| m.reached.is_none()) {
        if milestone.met(areas, facts) {
            milestone.reached = Some(at);
            reached.push(milestone.name.clone());
        }
    }
    reached
}

/// Like "2 of 5 · next: Reach Viridian City", or empty without milestones.
pub fn progress(milestones: &[Milestone]) -> String {
    if milestones.is_empty() {
        return String::new();
    }
    let done = milestones.iter().filter(|m| m.reached.is_some()).count();
    match milestones.iter().find(|m| m.reached.is_none()) {
        Some(next) => format!("{done} of {} · next: {}", milestones.len(), next.name),
        None => format!("all {done} reached"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World {
        let mut world = World::default();
        let city = world.area("Viridian City");
        city.see((23, 25), Kind::Door);
        city.see((29, 19), Kind::Wall);
        city.see((10, 10), Kind::Floor);
        world
            .area("Viridian Pokémon Center")
            .see((3, 6), Kind::Floor);
        world
    }

    #[test]
    fn old_notes_load_as_plain_text_and_new_ones_round_trip() {
        let old: Vec<Note> = serde_json::from_str(r#"["The Mart is north."]"#).unwrap();
        assert_eq!(old, vec![Note::new("The Mart is north.", "")]);
        let mut note = Note::new("A door at (23, 25) in Viridian City", "seen");
        note.check = Check::Agrees("yes".to_string());
        let json = serde_json::to_string(&note).unwrap();
        assert_eq!(serde_json::from_str::<Note>(&json).unwrap(), note);
    }

    #[test]
    fn notes_naming_steps_are_checked_against_the_map() {
        let world = world();
        assert_eq!(
            check(
                "Viridian City's Pokémon Center entrance is at (23, 25).",
                &world
            ),
            Check::Agrees("(23, 25) in Viridian City is a way out on the map".to_string())
        );
        assert_eq!(
            check(
                "The Mart entrance in Viridian City is at x 29, y 19.",
                &world
            ),
            Check::Disagrees("(29, 19) in Viridian City is a wall on the map".to_string())
        );
        // A way in on plain floor isn't counted either way; a place on floor agrees.
        assert_eq!(
            check("Viridian City has a door at (10, 10)", &world),
            Check::Unchecked
        );
        assert!(matches!(
            check("Viridian City's old man sits at (10, 10)", &world),
            Check::Agrees(_)
        ));
        // Steps not seen yet, no steps, or no area she knows: nothing to check.
        assert_eq!(
            check("Viridian City: a tree at (90, 90)", &world),
            Check::Unchecked
        );
        assert_eq!(check("Viridian City has a Mart", &world), Check::Unchecked);
        assert_eq!(
            check("Pewter City's gym is at (16, 17)", &world),
            Check::Unchecked
        );
    }

    #[test]
    fn notes_about_what_changes_by_itself_are_marked() {
        let world = world();
        for text in [
            "SQUIRTLE is Lv8 with 5/25 HP and needs healing.",
            "SQUIRTLE is lv 8.",
            "RED has no badges and ¥3175.",
            "RED is inside Viridian Pokémon Center at (3, 6) and can walk.",
        ] {
            assert_eq!(check(text, &world), Check::Changes, "{text}");
        }
        assert_eq!(
            check("Squirtle knows TACKLE and BUBBLE.", &world),
            Check::Unchecked
        );
        assert_eq!(
            check("HP comes back at a Pokémon Center.", &world),
            Check::Unchecked
        );
    }

    #[test]
    fn notes_show_where_they_came_from_and_what_code_found() {
        let mut note = Note::new("The Mart is north", "hint from Cherry");
        assert_eq!(note.shown(), "The Mart is north (hint from Cherry)");
        assert_eq!(note.for_plan(), "The Mart is north [hint from Cherry]");
        note.check = Check::Disagrees("(1, 2) is a wall".to_string());
        assert_eq!(note.shown(), "❌ The Mart is north (hint from Cherry)");
        assert_eq!(
            note.for_plan(),
            "The Mart is north [hint from Cherry; code found it WRONG: (1, 2) is a wall]"
        );
        assert_eq!(Note::new("Maybe", "guess").shown(), "❔ Maybe");
        assert_eq!(Note::new("Plain", "").for_plan(), "Plain");
        assert_eq!(
            spots("(1, 2) and (3,4); x 5, y 6. x=7, y=8"),
            vec![(1, 2), (3, 4), (5, 6), (7, 8)]
        );
    }

    fn milestone(name: &str, area: &str, fact: &str, has: &str) -> Milestone {
        Milestone {
            name: name.to_string(),
            area: area.to_string(),
            fact: fact.to_string(),
            has: has.to_string(),
            reached: None,
        }
    }

    #[test]
    fn milestones_are_reached_by_what_perception_reads() {
        let mut list = vec![
            milestone("Reach Viridian City", "viridian city", "", ""),
            milestone("Win the Boulder Badge", "", "Badges", "boulder"),
            milestone("Get the Pokédex", "", "Story so far", "got the Pokédex"),
        ];
        let areas: BTreeSet<String> = ["Viridian City".to_string()].into();
        let facts =
            "Location: Viridian City at x 1, y 2\nBadges: none\nStory so far: got the Pokédex";
        let at = Reached {
            episode: 7,
            usd: 0.04,
        };
        assert_eq!(
            reach(&mut list, &areas, facts, at),
            vec!["Reach Viridian City", "Get the Pokédex"]
        );
        assert_eq!(list[0].reached, Some(at));
        assert_eq!(progress(&list), "2 of 3 · next: Win the Boulder Badge");
        // Reached once stays reached; nothing new the second time.
        assert!(reach(&mut list, &BTreeSet::new(), "", at).is_empty());
        assert_eq!(
            list[1].for_plan(),
            "Win the Boulder Badge (fact \"Badges\" has \"boulder\")"
        );
        assert_eq!(
            list[0].for_plan(),
            "Reach Viridian City (in area \"viridian city\"): reached in episode 7"
        );
    }

    #[test]
    fn the_strategist_replaces_the_milestones_ahead_but_not_the_reached_ones() {
        let mut kept = vec![milestone("Reach Route 1", "Route 1", "", "")];
        kept[0].reached = Some(Reached {
            episode: 3,
            usd: 0.0,
        });
        let mut theirs = vec![
            milestone("reach route 1", "Route 1", "", ""),
            milestone("Win a battle", "", "", ""),
            milestone("Reach Pewter City", "Pewter City", "", ""),
        ];
        theirs.extend((0..10).map(|i| milestone(&format!("m{i}"), &format!("A{i}"), "", "")));
        replace_ahead(&mut kept, theirs);
        let names: Vec<&str> = kept.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names[..3], ["Reach Route 1", "Reach Pewter City", "m0"]);
        assert_eq!(
            kept.len(),
            1 + MILESTONES_AHEAD,
            "a reached one and 8 ahead"
        );
        assert!(kept[0].reached.is_some());
        // An answer without milestones keeps them.
        replace_ahead(&mut kept, Vec::new());
        assert_eq!(kept.len(), 1 + MILESTONES_AHEAD);
        assert_eq!(progress(&[]), "");
    }
}
