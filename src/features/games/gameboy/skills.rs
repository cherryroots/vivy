//! Her own skills: what she writes down for a game while playing. Pure; `actions.rs` runs
//! them, and they're kept in a file per game (`files::skills_path`).
//!
//! A skill has a line saying when it's for (always in her prompt), and know-how, a
//! routine, or both. Know-how is notes she reads when she uses the skill, like how a puzzle
//! works. A routine is a few rules ("when these words are on screen, press these buttons")
//! and when to stop, which code runs as one action. Code knows nothing about what a skill
//! is for: everything is in terms any game has, the words on screen and whether she can
//! walk. So a battle routine in Pokémon Red is one she made herself, and in another game
//! she makes others.
//!
//! A rule can also fit what's around her (`near.rs`): something near, ahead, coming
//! closer, or her last press not moving her. A routine with such rules is a reflex: it
//! runs in real time, a look after each press with no waiting for the screen to settle,
//! while she walks, so she can fight or jump without a model call between presses.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::map::Pos;
use super::near::{self, Thing};
use super::presses;
use super::world::View;

/// Skills kept for one game at most.
pub const SKILLS_MOST: usize = 20;
/// Rules in one skill at most.
const RULES_MOST: usize = 8;
/// Presses in one rule at most, like `press`.
const PRESSES_MOST: usize = 5;
/// Rounds one use runs at most, and when the skill doesn't say.
const ROUNDS_MOST: u32 = 50;
const ROUNDS_DEFAULT: u32 = 30;
/// Characters in a name, and in `use_when` and the words of a rule or stop.
const NAME_MOST: usize = 40;
const TEXT_MOST: usize = 200;
/// Characters of know-how at most.
const NOTES_MOST: usize = 1500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    /// When she means to use it, in her words.
    pub use_when: String,
    /// What she knows, read when she uses it.
    #[serde(default)]
    pub notes: String,
    /// Each round, the first rule that fits the screen is pressed. None: it's know-how only.
    pub rules: Vec<Rule>,
    /// It stops when any of these is on screen.
    pub stop_words: Vec<String>,
    /// It stops when she can walk.
    pub stop_when_free: bool,
    /// Rounds one use runs at most.
    pub most: u32,
    /// Buttons kept held the whole time it runs, like `b`.
    #[serde(default)]
    pub holding: String,
    /// Times it's been used, and how the last use ended.
    pub uses: u32,
    pub last: String,
    /// How it did in practice on a save state, like "works 3 of 3, practised at Place 10
    /// (78, 7)" (`practice.rs`); empty for one written in play. A rewrite starts fresh.
    #[serde(default)]
    pub practised: String,
    /// Where it was practised: the area and her step there. It only runs from within a
    /// step of it, since a move practised once is exact for that spot (a jump kept in one
    /// place and used in another killed her).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub practised_at: Option<(String, Pos)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Words that must be on screen; empty fits any screen.
    pub if_words: String,
    /// What must be around her (`near`), like `ahead 2`; empty fits anything.
    #[serde(default)]
    pub if_near: String,
    /// Buttons to press, like `down right a`.
    pub press: String,
}

/// The words `if_near` takes before a number of steps (1 by default): something within
/// that many steps of her, in the way she faces, behind her, above, below, to either
/// side, or moving and coming closer. `blocked` takes no number: her last press of a
/// direction didn't move her.
const NEAR_WORDS: [&str; 9] = [
    "near", "ahead", "behind", "above", "below", "left", "right", "coming", "blocked",
];

/// What's around her for a rule's `if_near`.
#[derive(Debug, Clone, Default)]
pub struct Around {
    pub things: Vec<Thing>,
    /// The way she faces, as a step (x right, y down), if known.
    pub facing: Option<(i32, i32)>,
    /// Her last press of a direction didn't move her.
    pub blocked: bool,
}

impl Around {
    /// Whether `wanted` (an `if_near`, checked when written) is around her.
    pub fn has(&self, wanted: &str) -> bool {
        let Ok((word, steps)) = near_parts(wanted) else {
            return false;
        };
        let way = match word {
            "blocked" => return self.blocked,
            "near" => None,
            "coming" => {
                return self
                    .things
                    .iter()
                    .any(|t| t.moving() && t.now < t.then && reach(t.at) <= steps);
            }
            "ahead" => self.facing,
            "behind" => self.facing.map(|(x, y)| (-x, -y)),
            "above" => Some((0, -1)),
            "below" => Some((0, 1)),
            "left" => Some((-1, 0)),
            _ => Some((1, 0)),
        };
        self.things.iter().any(|t| match way {
            None => reach(t.at) <= steps,
            // On that side, within the steps, and at most a step off the line.
            Some((x, y)) => {
                let along = t.at.0 * x + t.at.1 * y;
                let off = (t.at.0 * y - t.at.1 * x).abs();
                (1..=steps).contains(&along) && off <= 1
            }
        })
    }
}

/// Steps away, the most of across and down.
fn reach(at: (i32, i32)) -> i32 {
    at.0.abs().max(at.1.abs())
}

/// An `if_near`'s word and steps, or why it isn't one.
fn near_parts(wanted: &str) -> Result<(&str, i32), String> {
    let mut words = wanted.split_whitespace();
    let word = words.next().unwrap_or("");
    let steps = match words.next() {
        None => 1,
        Some(n) => n
            .parse()
            .ok()
            .filter(|n| (1..=near::NEAR).contains(n))
            .ok_or_else(|| format!("if_near \"{wanted}\": the steps are 1 to {}.", near::NEAR))?,
    };
    if !NEAR_WORDS.contains(&word) || words.next().is_some() {
        return Err(format!(
            "if_near \"{wanted}\" isn't one: a word ({}) and the steps, like \"ahead 2\".",
            NEAR_WORDS.join(", ")
        ));
    }
    Ok((word, steps))
}

impl Skill {
    /// A skill from `write_skill`'s arguments, or why it can't be one.
    pub fn from_args(args: &Value) -> Result<Skill, String> {
        let text = |key: &str| args[key].as_str().unwrap_or("").trim().to_string();
        let name = text("name");
        if name.is_empty() || name.chars().count() > NAME_MOST {
            return Err(format!(
                "A skill needs a name of 1 to {NAME_MOST} characters."
            ));
        }
        let use_when = text("use_when");
        if use_when.is_empty() {
            return Err("Say in use_when when the skill is for.".to_string());
        }
        let rules: Vec<Rule> = args["rules"]
            .as_array()
            .map(|rules| {
                rules
                    .iter()
                    .map(|rule| Rule {
                        if_words: rule["if_words"].as_str().unwrap_or("").trim().to_string(),
                        if_near: rule["if_near"].as_str().unwrap_or("").trim().to_lowercase(),
                        press: rule["press"].as_str().unwrap_or("").trim().to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let notes = text("notes");
        if notes.chars().count() > NOTES_MOST {
            return Err(format!("Keep notes under {NOTES_MOST} characters."));
        }
        if rules.is_empty() && notes.is_empty() {
            return Err("A skill needs notes, rules, or both.".to_string());
        }
        if rules.len() > RULES_MOST {
            return Err(format!("A skill has at most {RULES_MOST} rules."));
        }
        for rule in &rules {
            match presses::parse(&rule.press) {
                Ok(p) if p.is_empty() || p.len() > PRESSES_MOST => {
                    return Err(format!(
                        "Each rule presses 1 to {PRESSES_MOST} buttons (\"{}\" doesn't).",
                        rule.press
                    ));
                }
                Ok(_) => {}
                Err(why) => return Err(why),
            }
            if !rule.if_near.is_empty() {
                near_parts(&rule.if_near)?;
            }
        }
        let holding = text("holding").to_lowercase();
        for name in holding.split(['+', ' ', ',']).filter(|n| !n.is_empty()) {
            if super::emulator::Button::parse(name).is_none() {
                return Err(format!("`{name}` in holding isn't a button."));
            }
        }
        let stop_words: Vec<String> = args["stop_words"]
            .as_array()
            .map(|words| {
                words
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|w| w.trim().to_string())
                    .filter(|w| !w.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(number) = stop_words
            .iter()
            .find(|w| w.contains(|c: char| c.is_ascii_digit()))
        {
            return Err(format!(
                "Stop words can't have numbers in them (\"{number}\"): HP, levels and money \
                 change, so it would stop at the wrong time."
            ));
        }
        let too_long = std::iter::once(&use_when)
            .chain(stop_words.iter())
            .chain(rules.iter().map(|r| &r.if_words))
            .any(|t| t.chars().count() > TEXT_MOST);
        if too_long {
            return Err(format!(
                "Keep each piece of text under {TEXT_MOST} characters."
            ));
        }
        let most = args["most"].as_u64().map_or(ROUNDS_DEFAULT, |n| {
            n.clamp(1, u64::from(ROUNDS_MOST)) as u32
        });
        Ok(Skill {
            name,
            use_when,
            notes,
            rules,
            stop_words,
            stop_when_free: args["stop_when_free"].as_bool().unwrap_or(true),
            most,
            holding,
            uses: 0,
            last: String::new(),
            practised: String::new(),
            practised_at: None,
        })
    }

    /// The rule to press now: the first whose words are on screen and whose `if_near` is
    /// around her.
    pub fn rule_for(&self, view: &View, around: &Around) -> Option<&Rule> {
        self.rules.iter().find(|rule| {
            (rule.if_words.is_empty() || on_screen(&view.text, &rule.if_words))
                && (rule.if_near.is_empty() || around.has(&rule.if_near))
        })
    }

    /// Whether it's a reflex: a rule looks at what's around her, so it runs in real time
    /// while she walks.
    pub fn reflex(&self) -> bool {
        self.rules.iter().any(|rule| !rule.if_near.is_empty())
    }

    /// Why it stops on this screen, if it does. Stop words only count once it has pressed
    /// something: words already on screen when it starts are what it was used for. A
    /// reflex stops when words come up instead (the code running it also stops it when she
    /// hasn't been able to walk for a few rounds: knocked out, or a scene).
    pub fn stop(&self, view: &View, pressed: bool) -> Option<String> {
        if self.reflex() {
            if !view.text.is_empty() && pressed {
                return Some(format!(
                    "words came up: \"{}\"",
                    view.text.replace('\n', " / ")
                ));
            }
        } else if self.stop_when_free && view.can_walk {
            return Some("you can walk".to_string());
        }
        if !pressed {
            return None;
        }
        self.stop_words
            .iter()
            .find(|words| on_screen(&view.text, words))
            .map(|words| format!("\"{words}\" is on screen"))
    }

    /// A warning about a stop word that is also a rule's words: it would stop the routine
    /// on the very screen that rule is for.
    pub fn clash(&self) -> Option<String> {
        let word = self.stop_words.iter().find(|stop| {
            self.rules.iter().any(|rule| {
                !rule.if_words.is_empty()
                    && (on_screen(&rule.if_words, stop) || on_screen(stop, &rule.if_words))
            })
        })?;
        Some(format!(
            "\"{word}\" is a stop word and also a rule's words, so it stops on the screen \
             that rule is for. Pick a stop word that only shows once it's done."
        ))
    }

    /// Whether it's know-how only, with no routine to run.
    pub fn know_how(&self) -> bool {
        self.rules.is_empty()
    }

    /// One line for her list of skills.
    fn line(&self) -> String {
        let used = match (self.know_how(), self.uses) {
            (true, _) => "know-how".to_string(),
            (false, 0) => "not used yet".to_string(),
            (false, 1) => format!("used once, last: {}", self.last),
            (false, n) => format!("used {n} times, last: {}", self.last),
        };
        let practised = if self.practised.is_empty() {
            String::new()
        } else {
            format!("practised: {}; ", self.practised)
        };
        format!("- {}: {} ({practised}{used})", self.name, self.use_when)
    }

    /// The whole skill, for people to read: when, the notes, the rules and how it stops.
    pub fn written(&self) -> String {
        let mut lines = vec![format!("**{}**: {}", self.name, self.use_when)];
        if !self.notes.is_empty() {
            lines.push(self.notes.clone());
        }
        for rule in &self.rules {
            let mut when = Vec::new();
            if !rule.if_words.is_empty() {
                when.push(format!("on \"{}\"", rule.if_words));
            }
            if !rule.if_near.is_empty() {
                when.push(format!("when {}", rule.if_near));
            }
            lines.push(if when.is_empty() {
                format!("- otherwise press `{}`", rule.press)
            } else {
                format!("- {} press `{}`", when.join(" and "), rule.press)
            });
        }
        if !self.holding.is_empty() {
            lines.push(format!("- holding `{}`", self.holding));
        }
        if !self.know_how() {
            let mut stops = Vec::new();
            if self.reflex() {
                stops.push("words come up".to_string());
            } else if self.stop_when_free {
                stops.push("she can walk".to_string());
            }
            stops.extend(self.stop_words.iter().map(|w| format!("\"{w}\"")));
            stops.push(format!("{} rounds", self.most));
            lines.push(format!("- stops at: {}", stops.join(", ")));
            if !self.practised.is_empty() {
                lines.push(format!("-# practised: {}", self.practised));
            }
            lines.push(match self.uses {
                0 => "-# not used yet".to_string(),
                n => format!("-# used {n} times, last {}", self.last),
            });
        }
        lines.join("\n")
    }
}

/// Whether `words` are on the screen as whole words, in order, ignoring case, line breaks
/// and punctuation: `RED` is in "RED defeated BLUE!" but not in "PIDGEY appeared!". Words
/// that are only marks (the cursor `▶`) are looked for as they are.
fn on_screen(text: &str, words: &str) -> bool {
    let words_of = |s: &str| -> Vec<String> {
        s.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '’'))
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let wanted = words_of(words);
    if wanted.is_empty() {
        // Only marks, like the cursor `▶`: those are looked for as they are.
        let marks = words.trim();
        return !marks.is_empty() && text.contains(marks);
    }
    words_of(text)
        .windows(wanted.len())
        .any(|w| w == wanted.as_slice())
}

/// Why a practised skill can't run from where she is, if it can't: it only worked from
/// within a step of `spot` in `area`.
pub fn away_from_practice(name: &str, area: &str, spot: Pos, view: &View) -> Option<String> {
    let (x, y) = spot;
    let steps = (view.at.0 - x).abs().max((view.at.1 - y).abs());
    if view.area == area && steps <= 1 {
        return None;
    }
    let away = if view.area == area {
        format!("{steps} steps away")
    } else {
        format!("in {}", view.area)
    };
    Some(format!(
        "Didn't use {name}: it was practised at {area} ({x}, {y}) and only worked from there, \
         and you're at ({}, {}), {away}. Get within a step of that spot first, or press the \
         move yourself and check where it takes you.",
        view.at.0, view.at.1
    ))
}

/// Saves `skill`, replacing one with the same name, or says why it can't.
pub fn save(skills: &mut Vec<Skill>, skill: Skill) -> Result<(), String> {
    if let Some(old) = skills
        .iter_mut()
        .find(|s| s.name.eq_ignore_ascii_case(&skill.name))
    {
        // The record of uses is for the old rules; a rewrite starts fresh.
        *old = skill;
        return Ok(());
    }
    if skills.len() >= SKILLS_MOST {
        return Err(format!(
            "You have {SKILLS_MOST} skills already: rewrite one of them instead."
        ));
    }
    skills.push(skill);
    Ok(())
}

/// Applies the strategist's review of her skills: each change rewrites a skill (or adds
/// one, under a new name) or deletes one, with `write_skill`'s arguments plus `change`.
/// Skills it doesn't name stay as they are. Says what happened to each, for the log.
pub fn review(skills: &mut Vec<Skill>, changes: &[Value]) -> Vec<String> {
    changes
        .iter()
        .map(|change| {
            let name = change["name"].as_str().unwrap_or("").trim();
            if change["change"].as_str() == Some("delete") {
                return match forget(skills, name) {
                    Some(_) => format!("deleted {name}"),
                    None => format!("no skill {name} to delete"),
                };
            }
            match Skill::from_args(change).and_then(|skill| save(skills, skill)) {
                Ok(()) => format!("rewrote {name}"),
                Err(why) => format!("couldn't rewrite {name}: {why}"),
            }
        })
        .collect()
}

/// Her skills in full, with how each went, for the strategist to review.
pub fn for_review(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return "(none yet)".to_string();
    }
    serde_json::to_string_pretty(skills).unwrap_or_default()
}

/// Takes out the skill called `name`, if there's one.
pub fn forget(skills: &mut Vec<Skill>, name: &str) -> Option<Skill> {
    let at = skills
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(name.trim()))?;
    Some(skills.remove(at))
}

/// Her skills from their file's contents. A file that doesn't read gives none, so a
/// broken file never stops her playing.
pub fn from_json(json: &str) -> Vec<Skill> {
    serde_json::from_str(json).unwrap_or_else(|e| {
        tracing::warn!("couldn't read the Game Boy skills: {e}");
        Vec::new()
    })
}

pub fn find<'a>(skills: &'a mut [Skill], name: &str) -> Option<&'a mut Skill> {
    skills
        .iter_mut()
        .find(|s| s.name.eq_ignore_ascii_case(name.trim()))
}

/// Her skills as prompt lines, or nothing when she has none.
pub fn list(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = skills.iter().map(Skill::line).collect();
    format!("Your skills (use_skill):\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(text: &str, can_walk: bool) -> View {
        View {
            area: "Route 1".to_string(),
            at: (1, 1),
            can_walk,
            text: text.to_string(),
            facts: String::new(),
            grid: None,
        }
    }

    fn run_away() -> Skill {
        Skill::from_args(&json!({
            "name": "run away",
            "use_when": "a wild battle I don't need",
            "rules": [
                {"if_words": "fight run", "press": "down right a"},
                {"press": "a"}
            ],
            "stop_words": ["Can't escape"]
        }))
        .unwrap()
    }

    #[test]
    fn a_practised_skill_runs_only_from_where_it_was_practised() {
        let at = |area: &str, x, y| View {
            area: area.to_string(),
            at: (x, y),
            ..view("", true)
        };
        let from = |v: &View| away_from_practice("jump_gap", "Place 3", (20, 1), v);
        assert_eq!(from(&at("Place 3", 20, 1)), None);
        assert_eq!(from(&at("Place 3", 21, 0)), None);
        assert!(
            from(&at("Place 3", 25, 1))
                .unwrap()
                .contains("5 steps away")
        );
        assert!(from(&at("Place 4", 20, 1)).unwrap().contains("in Place 4"));
        // Kept with the skill, and old skills without it still load.
        let mut skill = run_away();
        skill.practised_at = Some(("Place 3".to_string(), (20, 1)));
        let back: Skill = serde_json::from_str(&serde_json::to_string(&skill).unwrap()).unwrap();
        assert_eq!(back.practised_at, skill.practised_at);
        let old = serde_json::to_string(&run_away()).unwrap();
        assert!(!old.contains("practised_at"));
        assert_eq!(
            serde_json::from_str::<Skill>(&old).unwrap().practised_at,
            None
        );
    }

    #[test]
    fn rules_match_words_ignoring_case_and_line_breaks() {
        let skill = run_away();
        let menu = view("▶FIGHT PKMN\nITEM RUN", false);
        // "fight run" isn't in the words as they are, so the second rule fits.
        assert_eq!(
            skill.rule_for(&menu, &Around::default()).unwrap().press,
            "a"
        );
        let menu = view("What will\nRED do? ▶FIGHT\nrun", false);
        assert_eq!(
            skill.rule_for(&menu, &Around::default()).unwrap().press,
            "down right a"
        );
    }

    #[test]
    fn words_match_whole_words_only() {
        assert!(on_screen("RED defeated\nBLUE!", "red"));
        assert!(!on_screen("Wild PIDGEY\nappeared!", "RED"));
        assert!(on_screen("▶FIGHT PKMN\nITEM RUN", "fight pkmn"));
        assert!(on_screen("Can't\nescape!", "can't escape"));
        assert!(on_screen("TYPE/\nNORMAL", "TYPE/"));
        assert!(on_screen("▶YES\nNO", "▶") && !on_screen("YES\nNO", "▶"));
        assert!(!on_screen("anything", " "), "nothing is never on screen");
    }

    #[test]
    fn it_stops_when_free_or_on_a_stop_word() {
        let skill = run_away();
        assert_eq!(
            skill.stop(&view("", true), false).as_deref(),
            Some("you can walk")
        );
        assert_eq!(
            skill.stop(&view("can't\nescape!", false), true).as_deref(),
            Some("\"Can't escape\" is on screen")
        );
        assert_eq!(
            skill.stop(&view("Wild PIDGEY appeared!", false), true),
            None
        );
        // Stop words already up when it starts don't count.
        assert_eq!(skill.stop(&view("can't escape", false), false), None);
        let mut keeps_going = skill.clone();
        keeps_going.stop_when_free = false;
        assert_eq!(keeps_going.stop(&view("", true), true), None);
    }

    #[test]
    fn a_stop_word_a_rule_uses_is_warned_about() {
        assert_eq!(run_away().clash(), None);
        let clashing = Skill::from_args(&json!({
            "name": "fight",
            "use_when": "a battle",
            "rules": [{"if_words": "FIGHT", "press": "a"}],
            "stop_words": ["fight run"]
        }))
        .unwrap();
        assert!(
            clashing
                .clash()
                .unwrap()
                .starts_with("\"fight run\" is a stop word")
        );
    }

    #[test]
    fn a_review_rewrites_merges_and_deletes() {
        let mut skills = vec![run_away()];
        let mut copy = run_away();
        copy.name = "run away 2".to_string();
        skills.push(copy);
        skills[0].uses = 5;
        let lines = review(
            &mut skills,
            &[
                json!({"name": "run away 2", "change": "delete"}),
                json!({
                    "name": "Run Away", "change": "rewrite", "use_when": "any wild battle",
                    "notes": "", "rules": [{"if_words": "", "press": "down right a"}],
                    "stop_words": [], "stop_when_free": true, "most": 10
                }),
                json!({"name": "ghost", "change": "delete"}),
                json!({"name": "bad", "change": "rewrite", "use_when": "x", "rules": []}),
            ],
        );
        assert_eq!(
            lines[..3],
            [
                "deleted run away 2",
                "rewrote Run Away",
                "no skill ghost to delete"
            ]
        );
        assert!(lines[3].starts_with("couldn't rewrite bad: "));
        assert_eq!(skills.len(), 1);
        assert_eq!(
            (skills[0].use_when.as_str(), skills[0].uses),
            ("any wild battle", 0)
        );
        assert!(for_review(&skills).contains("\"use_when\": \"any wild battle\""));
        assert_eq!(for_review(&[]), "(none yet)");
    }

    #[test]
    fn reflex_rules_fit_what_is_around_her() {
        let skill = Skill::from_args(&json!({
            "name": "fight",
            "use_when": "something comes at me",
            "holding": "b",
            "rules": [
                {"if_near": "blocked", "press": "a:20"},
                {"if_near": "Ahead 2", "press": "a"},
                {"if_near": "coming 3", "press": "left"},
                {"press": "right"}
            ]
        }))
        .unwrap();
        assert!(skill.reflex());
        assert_eq!(skill.holding, "b");
        let walking = view("", true);
        let thing = |at, went: (i32, i32)| Thing {
            at,
            went,
            now: (at.0.abs() + at.1.abs()) * 16,
            then: (at.0.abs() + at.1.abs()) * 16 - went.0.signum() * at.0.signum() * 8,
        };
        let press = |around: &Around| skill.rule_for(&walking, around).unwrap().press.clone();
        let facing_right = Around {
            facing: Some((1, 0)),
            ..Around::default()
        };
        assert_eq!(press(&facing_right), "right");
        // Two steps ahead and one off the line is ahead; behind her isn't.
        let ahead = Around {
            things: vec![thing((2, 1), (0, 0))],
            ..facing_right.clone()
        };
        assert_eq!(press(&ahead), "a");
        let behind = Around {
            things: vec![thing((-2, 0), (0, 0))],
            ..facing_right.clone()
        };
        assert_eq!(press(&behind), "right");
        // Behind her and coming closer.
        let coming = Around {
            things: vec![thing((-2, 0), (4, 0))],
            ..facing_right.clone()
        };
        assert_eq!(press(&coming), "left");
        let blocked = Around {
            blocked: true,
            ..ahead
        };
        assert_eq!(press(&blocked), "a:20");
        // A reflex keeps going while she walks and stops when she can't.
        assert_eq!(skill.stop(&walking, true), None);
        assert_eq!(skill.stop(&view("", false), true), None);
        assert_eq!(
            skill.stop(&view("Game over", false), true).as_deref(),
            Some("words came up: \"Game over\"")
        );
        assert!(skill.written().contains("- when ahead 2 press `a`"));
        assert!(skill.written().contains("- holding `b`"));
        assert!(!run_away().reflex());
    }

    #[test]
    fn bad_reflexes_say_why() {
        let with = |rule: Value, holding: &str| {
            Skill::from_args(&json!({
                "name": "x", "use_when": "y", "rules": [rule], "holding": holding
            }))
        };
        let bad = with(json!({"if_near": "ahead 9", "press": "a"}), "").unwrap_err();
        assert!(bad.contains("the steps are 1 to 4"), "{bad}");
        let bad = with(json!({"if_near": "enemy", "press": "a"}), "").unwrap_err();
        assert!(bad.contains("like \"ahead 2\""), "{bad}");
        let bad = with(json!({"press": "a"}), "shield").unwrap_err();
        assert!(bad.contains("`shield` in holding isn't a button"), "{bad}");
        assert!(with(json!({"if_near": "near", "press": "a"}), "a+b").is_ok());
    }

    #[test]
    fn bad_skills_say_why() {
        let bad = |args: Value| Skill::from_args(&args).unwrap_err();
        assert!(bad(json!({"use_when": "x", "rules": [{"press": "a"}]})).contains("name"));
        assert!(bad(json!({"name": "x", "rules": [{"press": "a"}]})).contains("use_when"));
        assert!(bad(json!({"name": "x", "use_when": "x", "rules": []})).contains("notes, rules"));
        assert!(
            bad(json!({"name": "x", "use_when": "x", "rules": [{"press": "a a a a a a"}]}))
                .contains("1 to 5")
        );
        assert!(
            !bad(json!({"name": "x", "use_when": "x", "rules": [{"press": "jump"}]})).is_empty()
        );
        let numbers = json!({"name": "x", "use_when": "x", "rules": [{"press": "a"}],
            "stop_words": ["fainted", "9/ 31"]});
        assert!(bad(numbers).contains("numbers in them (\"9/ 31\")"));
        let most = Skill::from_args(&json!({
            "name": "x", "use_when": "x", "rules": [{"press": "a"}], "most": 500
        }))
        .unwrap();
        assert_eq!(most.most, ROUNDS_MOST);
    }

    #[test]
    fn saving_replaces_by_name_and_keeps_a_limit() {
        let mut skills = Vec::new();
        let mut used = run_away();
        used.uses = 3;
        save(&mut skills, used).unwrap();
        let mut renamed = run_away();
        renamed.name = "RUN AWAY".to_string();
        save(&mut skills, renamed).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].uses, 0, "a rewrite starts its record fresh");
        for i in 1..SKILLS_MOST {
            let mut s = run_away();
            s.name = format!("skill {i}");
            save(&mut skills, s).unwrap();
        }
        let mut one_more = run_away();
        one_more.name = "one more".to_string();
        assert!(save(&mut skills, one_more).is_err());
        assert!(find(&mut skills, " Run Away ").is_some());
    }

    #[test]
    fn the_list_says_how_each_went() {
        assert_eq!(list(&[]), "");
        let mut skill = run_away();
        let text = list(std::slice::from_ref(&skill));
        assert!(text.contains("- run away: a wild battle I don't need (not used yet)"));
        skill.uses = 1;
        skill.last = "you can walk".to_string();
        assert!(list(std::slice::from_ref(&skill)).contains("(used once, last: you can walk)"));
        skill.uses = 2;
        skill.last = "you can walk after 4 rounds".to_string();
        assert!(list(&[skill]).contains("(used 2 times, last: you can walk after 4 rounds)"));
    }

    #[test]
    fn know_how_is_notes_with_no_routine() {
        let skill = Skill::from_args(&json!({
            "name": "boulders",
            "use_when": "a cave with boulders",
            "notes": "Push each boulder onto a switch; leave the cave to reset them."
        }))
        .unwrap();
        assert!(skill.know_how());
        assert!(list(std::slice::from_ref(&skill)).contains("(know-how)"));
        assert!(skill.written().contains("Push each boulder"));
        assert!(!skill.written().contains("stops at"));
    }

    #[test]
    fn skills_go_through_their_file_and_a_broken_file_is_none() {
        let mut skills = vec![run_away()];
        skills[0].uses = 4;
        let json = serde_json::to_string_pretty(&skills).unwrap();
        assert_eq!(from_json(&json), skills);
        assert!(from_json("{not json").is_empty());
        assert!(forget(&mut skills, "RUN AWAY").is_some());
        assert!(skills.is_empty());
    }

    #[test]
    fn written_out_for_people() {
        let text = run_away().written();
        assert!(
            text.starts_with("**run away**: a wild battle I don't need"),
            "{text}"
        );
        assert!(text.contains("- on \"fight run\" press `down right a`"));
        assert!(text.contains("- otherwise press `a`"));
        assert!(text.contains("- stops at: she can walk, \"Can't escape\", 30 rounds"));
    }
}
