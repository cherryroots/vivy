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

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// Times it's been used, and how the last use ended.
    pub uses: u32,
    pub last: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Words that must be on screen; empty fits any screen.
    pub if_words: String,
    /// Buttons to press, like `down right a`.
    pub press: String,
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
            uses: 0,
            last: String::new(),
        })
    }

    /// The rule to press on this screen: the first whose words are on it.
    pub fn rule_for(&self, view: &View) -> Option<&Rule> {
        self.rules
            .iter()
            .find(|rule| rule.if_words.is_empty() || on_screen(&view.text, &rule.if_words))
    }

    /// Why it stops on this screen, if it does. Stop words only count once it has pressed
    /// something: words already on screen when it starts are what it was used for.
    pub fn stop(&self, view: &View, pressed: bool) -> Option<String> {
        if self.stop_when_free && view.can_walk {
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
        format!("- {}: {} ({used})", self.name, self.use_when)
    }

    /// The whole skill, for people to read: when, the notes, the rules and how it stops.
    pub fn written(&self) -> String {
        let mut lines = vec![format!("**{}**: {}", self.name, self.use_when)];
        if !self.notes.is_empty() {
            lines.push(self.notes.clone());
        }
        for rule in &self.rules {
            lines.push(if rule.if_words.is_empty() {
                format!("- otherwise press `{}`", rule.press)
            } else {
                format!("- on \"{}\" press `{}`", rule.if_words, rule.press)
            });
        }
        if !self.know_how() {
            let mut stops = Vec::new();
            if self.stop_when_free {
                stops.push("she can walk".to_string());
            }
            stops.extend(self.stop_words.iter().map(|w| format!("\"{w}\"")));
            stops.push(format!("{} rounds", self.most));
            lines.push(format!("- stops at: {}", stops.join(", ")));
            lines.push(match self.uses {
                0 => "-# not used yet".to_string(),
                n => format!("-# used {n} times, last {}", self.last),
            });
        }
        lines.join("\n")
    }
}

/// Whether `words` are on the screen, ignoring case and line breaks.
fn on_screen(text: &str, words: &str) -> bool {
    let flat = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    flat(text).contains(&flat(words))
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
    fn rules_match_words_ignoring_case_and_line_breaks() {
        let skill = run_away();
        let menu = view("▶FIGHT PKMN\nITEM RUN", false);
        // "fight run" isn't in the words as they are, so the second rule fits.
        assert_eq!(skill.rule_for(&menu).unwrap().press, "a");
        let menu = view("What will\nRED do? ▶FIGHT\nrun", false);
        assert_eq!(skill.rule_for(&menu).unwrap().press, "down right a");
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
