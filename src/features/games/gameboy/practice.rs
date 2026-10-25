//! Practice on save states: when a move must be exact (a jump onto a ledge, across a gap,
//! past an enemy), the strategist asks for practice with a goal code can measure. Code
//! saves the game, a model plans one try, code runs it and measures it, then loads the save
//! back, so every try starts from the same moment. Up to [`TRIES_MOST`] tries.
//!
//! - The cheap model (the supervisor's) plans the first tries; after [`CHEAP_TRIES`] misses
//!   the strategist's model plans, on the free daily tokens while they last.
//! - A try is presses, as the `press` action takes them, or a routine of rules (a reflex
//!   for a few seconds of real-time play), as a skill's.
//! - A try that reaches the goal is tried again from two other moments (a few frames
//!   later), so a lucky one shows as unreliable, and is kept as her skill with how often
//!   it worked. The game carries on from where the first win left her.
//! - Code never makes up a try: it only runs and measures the model's.
//!
//! What a try measures is general: where she ends and whether she stays there, the area,
//! a watched value, words like "game over". Nothing here knows a game.

use std::fmt::Write as _;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;

use super::actions::Acted;
use super::budget::Today;
use super::emulator::Button;
use super::map::Pos;
use super::notes::Note;
use super::presses;
use super::puzzle;
use super::session::{Game, Now};
use super::skills::{self, Rule, Skill};
use super::strategist::strict;
use super::supervisor::finish_stream;
use super::world::{View, World};
use crate::ai::{ChatProvider, ChatRequest, Input, OpenAi, Part, Role, ToolDef, Turn};
use crate::util::media::ModelImage;

/// Tries in one round of practice at most.
pub const TRIES_MOST: usize = 6;
/// Tries the cheap model plans before the strategist's model takes over.
const CHEAP_TRIES: usize = 2;
/// Frames after the save the winner is tried from again, besides the save itself.
const LATER_STARTS: [u32; 2] = [5, 11];
/// Frames a try's presses may take at most: 20 seconds.
const FRAMES_MOST: u32 = 20 * 60;
/// Rounds a routine runs at most in a try.
const ROUNDS_MOST: u64 = 20;
/// Frames she must stay put on the spot: a second.
const STILL_FRAMES: u32 = 60;
/// Words that mean she died, in any game that says so.
const DIED: [&str; 3] = ["game over", "blacked out", "whited out"];

/// What the strategist asks her to practise, and how code knows it worked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    /// The move, in a few words, like "the jump onto the high ledge".
    pub what: String,
    /// `stand` (on x, y), `area` (reach it), `past_right`/`past_left` (past x),
    /// `past_up`/`past_down` (past y), or `watched` (the watched value changes).
    pub goal: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub area: String,
    /// A watched value's name: it must change for `watched`, and not go down otherwise.
    #[serde(default)]
    pub watched: String,
    /// Dying fails the try.
    #[serde(default)]
    pub survive: bool,
}

/// The goals a strategist can give.
pub const GOALS: [&str; 7] = [
    "stand",
    "area",
    "past_right",
    "past_left",
    "past_up",
    "past_down",
    "watched",
];

impl Goal {
    /// Whether it asks for practice: a move named and a goal code knows.
    pub fn wanted(&self) -> bool {
        !self.what.trim().is_empty()
            && GOALS.contains(&self.goal.as_str())
            && (self.goal != "area" || !self.area.trim().is_empty())
            && (self.goal != "watched" || !self.watched.trim().is_empty())
    }

    pub fn spot(&self) -> Pos {
        (self.x, self.y)
    }

    /// What code checks, like "stand on (83, 5), without dying".
    pub fn line(&self) -> String {
        let mut line = match self.goal.as_str() {
            "stand" => format!("stand on ({}, {})", self.x, self.y),
            "area" => format!("reach {}", self.area.trim()),
            "past_right" => format!("get right of x {}", self.x),
            "past_left" => format!("get left of x {}", self.x),
            "past_up" => format!("get above y {}", self.y),
            "past_down" => format!("get below y {}", self.y),
            _ => format!("change {}", self.watched.trim()),
        };
        if self.survive {
            line.push_str(", without dying");
        }
        line
    }
}

/// How a round of practice went.
#[derive(Debug, Clone, PartialEq)]
pub struct Practised {
    /// One line for the log, like "🎯 Practised the jump to the ledge (stand on (83, 5)):
    /// 4 tries, kept `jump_gap_right`, works 3 of 3".
    pub line: String,
    /// Every try and how it went, for the task after it and the strategist's next plan.
    pub told: String,
    /// The skill kept, if one worked.
    pub kept: Option<String>,
}

/// One try, as a model plans it.
#[derive(Debug, Clone, Default, Deserialize)]
struct Planned {
    #[serde(default)]
    name: String,
    #[serde(default)]
    use_when: String,
    #[serde(default)]
    presses: String,
    #[serde(default)]
    rules: Vec<Value>,
    #[serde(default)]
    most: u64,
}

/// A try checked and ready to run.
#[derive(Debug, Clone)]
enum Try {
    Presses(String),
    Routine(Skill),
}

impl Try {
    /// Like "presses `a:20+right:60`" or "a routine: when ahead 2 press `b`, otherwise
    /// press `right` (20 rounds)".
    fn words(&self) -> String {
        match self {
            Try::Presses(p) => format!("presses `{p}`"),
            Try::Routine(skill) => {
                let rules: Vec<String> = skill.rules.iter().map(rule_words).collect();
                format!("a routine: {} ({} rounds)", rules.join(", "), skill.most)
            }
        }
    }
}

fn rule_words(rule: &Rule) -> String {
    let mut when = Vec::new();
    if !rule.if_words.is_empty() {
        when.push(format!("on \"{}\"", rule.if_words));
    }
    if !rule.if_near.is_empty() {
        when.push(format!("when {}", rule.if_near));
    }
    if when.is_empty() {
        format!("otherwise press `{}`", rule.press)
    } else {
        format!("{} press `{}`", when.join(" and "), rule.press)
    }
}

/// A try from what a model planned, or why code turns it down.
fn checked(planned: &Planned) -> Result<Try, String> {
    if !planned.rules.is_empty() {
        let args = json!({
            "name": "practice",
            "use_when": "practice",
            "rules": planned.rules,
            "stop_when_free": false,
            "most": planned.most.clamp(1, ROUNDS_MOST),
        });
        return Skill::from_args(&args).map(Try::Routine);
    }
    let typed = planned.presses.trim();
    let parsed = presses::parse(typed)?;
    if parsed.is_empty() || parsed.len() > 5 {
        return Err("a try presses 1 to 5 times (or is a routine of rules)".to_string());
    }
    if parsed.iter().any(|p| {
        p.buttons
            .iter()
            .any(|b| matches!(b, Button::Start | Button::Select))
    }) {
        return Err("start and select open menus: no use in a move".to_string());
    }
    let usual = super::console::Timing::default().hold;
    let frames: u32 = parsed.iter().map(|p| p.frames.unwrap_or(usual)).sum();
    if frames > FRAMES_MOST {
        return Err(format!(
            "its presses hold for {frames} frames, more than {FRAMES_MOST} (20 seconds)"
        ));
    }
    Ok(Try::Presses(typed.to_string()))
}

/// Where a try left her: the area and spot, and what she saw on the way.
#[derive(Debug, Clone)]
struct Seen {
    area: String,
    at: Pos,
    /// The map was showing with her on it: no game over, scene or menu in the way.
    can_walk: bool,
    watched: Vec<(String, i64)>,
}

impl Seen {
    fn of(view: &View) -> Seen {
        Seen {
            area: view.area.clone(),
            at: view.at,
            can_walk: view.can_walk,
            watched: watched_values(&view.facts),
        }
    }

    fn value(&self, name: &str) -> Option<i64> {
        let name = name.trim();
        self.watched
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| *v)
    }
}

/// The watched values in the facts' "Watched: money 3175, lives 2" line.
fn watched_values(facts: &str) -> Vec<(String, i64)> {
    let Some(line) = facts
        .lines()
        .find_map(|l| l.trim().strip_prefix("Watched: "))
    else {
        return Vec::new();
    };
    line.split(", ")
        .filter_map(|part| {
            let (name, value) = part.rsplit_once(' ')?;
            Some((name.to_string(), value.parse().ok()?))
        })
        .collect()
}

/// Where `at` is from `spot`, like "2 steps left of the spot and 1 below it".
fn off(at: Pos, spot: Pos) -> String {
    let steps = |n: i32| {
        if n == 1 {
            "1 step".to_string()
        } else {
            format!("{n} steps")
        }
    };
    let mut parts = Vec::new();
    let (dx, dy) = (at.0 - spot.0, at.1 - spot.1);
    if dx != 0 {
        let way = if dx < 0 { "left of" } else { "right of" };
        parts.push(format!("{} {way} the spot", steps(dx.abs())));
    }
    if dy != 0 {
        let way = if dy < 0 { "above" } else { "below" };
        let it = if parts.is_empty() { "the spot" } else { "it" };
        parts.push(format!("{} {way} {it}", steps(dy.abs())));
    }
    parts.join(" and ")
}

/// Whether the goal was met, from where she started and where she ended, or why not.
/// Standing on a spot counts within a step of it (a platform's top can be a row from the
/// spot named); that she's still there is checked by [`measure`]. A goal of hers on the
/// map needs the map showing at the end: where code last counted her means nothing on a
/// game over's password screen.
fn check(goal: &Goal, start: &Seen, end: &Seen, words: &[String]) -> Result<(), String> {
    let said = words.join(" ").to_lowercase();
    if goal.survive
        && let Some(died) = DIED.iter().find(|w| said.contains(*w))
    {
        return Err(format!("she died (\"{died}\" came up)"));
    }
    if goal.goal != "watched" && !end.can_walk {
        return Err(
            "the map wasn't showing at the end, so she wasn't on it (a game over, a scene or a \
             menu?)"
                .to_string(),
        );
    }
    if goal.survive && goal.goal != "area" && end.area != start.area {
        return Err(format!(
            "she ended in {}, not {} (a fall or a game over?)",
            end.area, start.area
        ));
    }
    if goal.survive
        && goal.goal != "watched"
        && !goal.watched.trim().is_empty()
        && let (Some(was), Some(now)) = (start.value(&goal.watched), end.value(&goal.watched))
        && now < was
    {
        return Err(format!(
            "{} went down from {was} to {now}",
            goal.watched.trim()
        ));
    }
    let (x, y) = end.at;
    match goal.goal.as_str() {
        "stand" if (x - goal.x).abs() <= 1 && (y - goal.y).abs() <= 1 && end.area == start.area => {
            Ok(())
        }
        "stand" if end.area != start.area => Err(format!("she ended in {}", end.area)),
        "stand" => Err(off(end.at, goal.spot())),
        "area" if end.area.eq_ignore_ascii_case(goal.area.trim()) => Ok(()),
        "area" => Err(format!("she was in {}, not {}", end.area, goal.area.trim())),
        "past_right" if x > goal.x => Ok(()),
        "past_left" if x < goal.x => Ok(()),
        "past_up" if y < goal.y => Ok(()),
        "past_down" if y > goal.y => Ok(()),
        "past_right" | "past_left" => Err(format!("x was {x}, not past {}", goal.x)),
        "past_up" | "past_down" => Err(format!("y was {y}, not past {}", goal.y)),
        _ => match (start.value(&goal.watched), end.value(&goal.watched)) {
            (Some(was), Some(now)) if was != now => Ok(()),
            (Some(was), _) => Err(format!("{} stayed {was}", goal.watched.trim())),
            _ => Err(format!("{} isn't watched", goal.watched.trim())),
        },
    }
}

/// Runs one try from where the game is now.
async fn run<G: Game>(game: &mut G, one: &Try) -> anyhow::Result<Acted> {
    match one {
        Try::Presses(p) => game.act("press", &json!({ "buttons": p })).await,
        Try::Routine(skill) => {
            // Run as a skill for the one use, then her skills are as they were.
            let skills = game.world().await?.skills;
            let mut with = skills.clone();
            skills::forget(&mut with, &skill.name);
            with.push(skill.clone());
            game.set_skills(with).await?;
            let acted = game.act("use_skill", &json!({ "name": skill.name })).await;
            game.set_skills(skills).await?;
            acted
        }
    }
}

/// Measures a try just run: she waits a second (a spot must hold her), then code checks
/// the goal. Says how it ended, and whether the goal was met.
async fn measure<G: Game>(
    game: &mut G,
    goal: &Goal,
    start: &Seen,
    acted: &Acted,
) -> anyhow::Result<(bool, String, (String, Pos))> {
    let mut words = acted.words.clone();
    let landed = Seen::of(&acted.view);
    let waited = game
        .act(
            "press",
            &json!({ "buttons": format!("wait:{STILL_FRAMES}") }),
        )
        .await?;
    words.extend(waited.words.iter().cloned());
    let end = Seen::of(&waited.view);
    let still = end.at == landed.at && end.area == landed.area;
    let mut line = format!(
        "{} Ended at ({}, {}) in {}{}.",
        acted.outcome.trim(),
        end.at.0,
        end.at.1,
        end.area,
        if still {
            ", still"
        } else {
            ", still moving a second later"
        }
    );
    let checked = if goal.goal == "stand" && !still {
        Err("she didn't stay put on a spot".to_string())
    } else {
        check(goal, start, &end, &words)
    };
    let met = match checked {
        Ok(()) => {
            line.push_str(" It worked.");
            true
        }
        Err(why) => {
            write!(line, " Missed: {why}.").unwrap();
            false
        }
    };
    Ok((met, line, (end.area, end.at)))
}

/// The game and what code knew, at one moment, to go back to.
struct Saved {
    state: Vec<u8>,
    world: World,
}

async fn save<G: Game>(game: &mut G) -> anyhow::Result<Saved> {
    Ok(Saved {
        state: game.save_state().await?,
        world: game.world().await?,
    })
}

/// Goes back to `saved`: the game as it was, and code's sight of it, keeping what the
/// tries taught about her presses (how far they carry her) and her skills.
async fn load<G: Game>(game: &mut G, saved: &Saved) -> anyhow::Result<()> {
    let now = game.world().await?;
    let mut back = saved.world.clone();
    back.skills = now.skills;
    back.watches = now.watches;
    back.motions = now.motions;
    back.sent = now.sent;
    back.paces = now.paces;
    game.load_state(saved.state.clone()).await?;
    game.set_world(back).await?;
    game.look().await?;
    Ok(())
}

/// Who planned a try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Planner {
    Cheap,
    Strategist,
}

/// One round of practice toward `goal`, from where the game is now. `openai` plans the
/// later tries on the free tokens (`free_tokens` a day, counted in `today`); `now`, if
/// given, shows on the dashboard that she's practising.
#[allow(clippy::too_many_arguments)]
pub async fn practise<G: Game>(
    provider: &dyn ChatProvider,
    openai: Option<&OpenAi>,
    free_tokens: u64,
    today: &mut Today,
    game: &mut G,
    goal: &Goal,
    notes: &[Note],
    now: Option<&watch::Sender<Now>>,
) -> anyhow::Result<Practised> {
    let show = |task: String| {
        if let Some(now) = now {
            now.send_modify(|now| {
                now.task = task;
                now.actions = 0;
            });
        }
    };
    // Loading a save forgets the frames before it, which code follows her by, so a look
    // after a load can read her a step from the look before it. The first try starts
    // from the save loaded back too, so every try starts from the same reading.
    let state = game.save_state().await?;
    game.load_state(state).await?;
    let first = game.look().await?;
    let start = Seen::of(&first.view);
    let situation = game.describe(&first).await?;
    let screen = game.screen().await.ok().map(|data| ModelImage {
        mime: "image/png",
        data,
    });
    let saved = save(game).await?;
    let what = goal.what.trim().to_string();
    let mut tried: Vec<String> = Vec::new();
    // Where each try ended, to say when one changed nothing.
    let mut ends: Vec<(usize, (String, Pos))> = Vec::new();
    let mut won: Option<(Try, String, String, Saved)> = None;
    for n in 1..=TRIES_MOST {
        show(format!("🎯 Practising {what}: try {n} of {TRIES_MOST}"));
        let world = game.world().await?;
        let asked = ask(goal, &start, &situation, &world, notes, &tried, n);
        let strong = n > CHEAP_TRIES && today.free_call_fits(free_tokens, true);
        let (planned, by) = match (openai, strong) {
            (Some(openai), true) => match plan_strongly(openai, &asked, screen.as_ref()).await {
                Ok((planned, tokens)) => {
                    today.used_free(tokens, true);
                    (planned, Planner::Strategist)
                }
                Err(err) => {
                    tracing::warn!("the practice planner failed: {err:#}");
                    (
                        plan_cheaply(provider, &asked, screen.as_ref()).await?,
                        Planner::Cheap,
                    )
                }
            },
            _ => (
                plan_cheaply(provider, &asked, screen.as_ref()).await?,
                Planner::Cheap,
            ),
        };
        let one = match checked(&planned) {
            Ok(one) => one,
            Err(why) => {
                tried.push(format!("- Try {n}: code turned it down: {why}."));
                continue;
            }
        };
        let acted = run(game, &one).await?;
        let (met, mut how, end) = measure(game, goal, &start, &acted).await?;
        if let Some((same, _)) = ends.iter().find(|(_, e)| *e == end) {
            write!(
                how,
                " The same end as try {same}: what changed from it made no difference."
            )
            .unwrap();
        }
        ends.push((n, end));
        let by = match by {
            Planner::Cheap => "",
            Planner::Strategist => " (planned by the strategist's model)",
        };
        tried.push(format!("- Try {n}{by}: {}. {how}", one.words()));
        if met {
            won = Some((one, planned.name, planned.use_when, save(game).await?));
            break;
        }
        load(game, &saved).await?;
    }
    let count = |n: usize| {
        if n == 1 {
            "1 try".to_string()
        } else {
            format!("{n} tries")
        }
    };
    let tries = tried.len();
    let Some((one, name, use_when, after)) = won else {
        let line = format!(
            "🎯 Practised {what} ({}): {}, none worked.",
            goal.line(),
            count(tries)
        );
        return Ok(Practised {
            line,
            told: format!(
                "Practised {what} on a save state ({}); none worked, and the game went back \
                 to where it was. The tries:\n{}",
                goal.line(),
                tried.join("\n")
            ),
            kept: None,
        });
    };

    // From two other moments, so a try that only works on one frame shows.
    let mut works = 1;
    for (i, frames) in LATER_STARTS.into_iter().enumerate() {
        show(format!(
            "🎯 Testing {what} from another start ({} of 3)",
            i + 2
        ));
        load(game, &saved).await?;
        game.act("press", &json!({ "buttons": format!("wait:{frames}") }))
            .await?;
        let acted = run(game, &one).await?;
        if measure(game, goal, &start, &acted).await?.0 {
            works += 1;
        }
    }
    load(game, &after).await?;
    let record = format!(
        "{}works {works} of 3, practised at {} ({}, {})",
        if works == 1 { "unreliable: " } else { "" },
        start.area,
        start.at.0,
        start.at.1
    );
    let name = skill_name(&name, &what);
    let mut skill = match one {
        Try::Presses(p) => Skill {
            name: name.clone(),
            use_when: String::new(),
            notes: String::new(),
            rules: vec![Rule {
                if_words: String::new(),
                if_near: String::new(),
                press: p,
            }],
            stop_words: Vec::new(),
            stop_when_free: false,
            most: 1,
            holding: String::new(),
            uses: 0,
            last: String::new(),
            practised: String::new(),
        },
        Try::Routine(skill) => skill,
    };
    skill.name.clone_from(&name);
    skill.use_when = if use_when.trim().is_empty() {
        format!(
            "for {what} at {} ({}, {})",
            start.area, start.at.0, start.at.1
        )
    } else {
        use_when.trim().to_string()
    };
    skill.notes = format!("Practised on a save state: {}. To {}.", record, goal.line());
    skill.practised.clone_from(&record);
    let mut skills = game.world().await?.skills;
    let kept = match skills::save(&mut skills, skill) {
        Ok(()) => {
            game.set_skills(skills).await?;
            format!("kept `{name}`, {record}")
        }
        Err(why) => format!("it worked but couldn't be kept: {why}"),
    };
    Ok(Practised {
        line: format!(
            "🎯 Practised {what} ({}): {}, {kept}.",
            goal.line(),
            count(tries)
        ),
        told: format!(
            "Practised {what} on a save state ({}) and {kept}; the game carries on from the \
             try that worked. The tries:\n{}",
            goal.line(),
            tried.join("\n")
        ),
        kept: Some(name),
    })
}

/// A skill's name from the model's, or from the move: letters, digits and `_`.
fn skill_name(named: &str, what: &str) -> String {
    let from = if named.trim().is_empty() { what } else { named };
    let name: String = from
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let name: Vec<&str> = name.split('_').filter(|p| !p.is_empty()).collect();
    let name = name.join("_");
    let name: String = name.chars().take(40).collect();
    if name.is_empty() {
        "practised_move".to_string()
    } else {
        name
    }
}

/// The fixed instructions for planning a try.
const PROMPT: &str = "You practise one move in a Game Boy game on a save state. Code saved \
the game; you plan one try, code runs it from the save and measures whether it reached the \
goal, then loads the save back, so every try starts from the same moment. You get the \
goal, where the player is, what her presses did before, and every try so far with where \
it ended.

A try is either presses or a routine.
- Presses: 1 to 5, as the press action takes them: up, down, left, right, a, b, buttons \
held together like a+right, held longer with frames after a colon (60 a second), each \
button its own frames like a:20+right:60 (a for 20 frames, right for 60), and wait:30 to \
let 30 frames pass. Leave rules empty.
- A routine, for a few seconds of real-time play (getting past enemies): rules, the first \
that fits pressing its buttons each round, like a skill's: if_near is what must be around \
the player (near, ahead, behind, above, below, left, right or coming, then steps 1 to 4, \
like \"ahead 2\"; or blocked), if_words words on screen, press 1 to 5 buttons; a rule with \
both empty fits always (put it last, like \"right\" to keep walking). most is the rounds, \
1 to 20. Leave presses empty.

Learn from the tries: each says what code saw (how high and far a jump went, when it \
landed, where she ended). Change what the last tries show was off, by about how much it \
was off (a few frames more or less of a direction, starting a step further back), rather \
than starting over; when tries keep missing the same way, try something unlike them.

Name the try as the skill it becomes if it works: short, in snake_case, like \
jump_gap_right; use_when says when it's for, with the place.";

/// What the try's planner is told.
fn ask(
    goal: &Goal,
    start: &Seen,
    situation: &str,
    world: &World,
    notes: &[Note],
    tried: &[String],
    n: usize,
) -> String {
    let mut text = format!(
        "Practise: {}. The goal code measures: {} (from {} ({}, {})).\n",
        goal.what.trim(),
        goal.line(),
        start.area,
        start.at.0,
        start.at.1
    );
    if goal.goal == "stand" {
        text.push_str(
            "Standing counts when she's on the spot or a step from it a second after the \
             try, and still.\n",
        );
    }
    write!(text, "\nWhere she is, at the save:\n{situation}\n").unwrap();
    let presses = super::path::presses(&world.motions, &world.paces);
    if !presses.is_empty() {
        write!(text, "\n{presses}\n").unwrap();
    }
    if !notes.is_empty() {
        text.push_str("\nThe planner's notes:\n");
        for note in notes {
            writeln!(text, "- {}", note.for_plan()).unwrap();
        }
    }
    if tried.is_empty() {
        text.push_str("\nNo tries yet.\n");
    } else {
        write!(
            text,
            "\nThe tries so far, each from the save:\n{}\n",
            tried.join("\n")
        )
        .unwrap();
    }
    write!(text, "\nThis is try {n} of {TRIES_MOST}.").unwrap();
    text
}

/// The answer's strict schema.
fn schema() -> Value {
    let rule = strict(json!({
        "if_words": {"type": "string"},
        "if_near": {"type": "string"},
        "press": {"type": "string"}
    }));
    strict(json!({
        "reasoning": {"type": "string"},
        "presses": {"type": "string"},
        "rules": {"type": "array", "items": rule},
        "most": {"type": "integer"},
        "name": {"type": "string"},
        "use_when": {"type": "string"}
    }))
}

/// The strategist's model plans the try, on the free tokens. Gives the tokens used.
async fn plan_strongly(
    openai: &OpenAi,
    asked: &str,
    screen: Option<&ModelImage>,
) -> anyhow::Result<(Planned, u64)> {
    let answer = openai
        .structured(puzzle::JOB, PROMPT, asked, screen, "practice", &schema())
        .await?;
    Ok((serde_json::from_value(answer.value)?, answer.tokens))
}

/// The cheap model (the supervisor's, the `game_play` job) plans the try, answering
/// through its one tool.
async fn plan_cheaply(
    provider: &dyn ChatProvider,
    asked: &str,
    screen: Option<&ModelImage>,
) -> anyhow::Result<Planned> {
    let request = ChatRequest {
        system: format!("{PROMPT}\n\nAnswer by calling try_move."),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts: std::iter::once(Part::Text(asked.to_string()))
                .chain(screen.cloned().map(Part::Image))
                .collect(),
        }]),
        tools: vec![ToolDef {
            name: "try_move",
            description: "The try: your reasoning, then presses or rules, the rounds for \
                rules, and the skill's name and use_when if it works.",
            parameters: schema(),
        }],
        cache_key: "vivy:gameboy-practice".to_string(),
        job: "game_play",
    };
    let done = finish_stream(provider, request).await?;
    let call = done
        .tool_calls
        .into_iter()
        .find(|call| call.name == "try_move")
        .context("the practice planner answered without calling try_move")?;
    Ok(serde_json::from_value(call.args)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(kind: &str) -> Goal {
        Goal {
            what: "the jump".to_string(),
            goal: kind.to_string(),
            x: 83,
            y: 5,
            ..Goal::default()
        }
    }

    fn seen(area: &str, at: Pos, watched: &str) -> Seen {
        Seen {
            area: area.to_string(),
            at,
            can_walk: true,
            watched: watched_values(watched),
        }
    }

    #[test]
    fn goals_are_checked_from_where_she_ends() {
        let start = seen("Place 10", (78, 7), "Watched: lives 3, money 20");
        let stand = goal("stand");
        assert!(check(&stand, &start, &seen("Place 10", (83, 5), ""), &[]).is_ok());
        assert!(check(&stand, &start, &seen("Place 10", (83, 6), ""), &[]).is_ok());
        assert_eq!(
            check(&stand, &start, &seen("Place 10", (81, 6), ""), &[]).unwrap_err(),
            "2 steps left of the spot and 1 step below it"
        );
        assert!(
            check(
                &goal("past_right"),
                &start,
                &seen("Place 10", (84, 9), ""),
                &[]
            )
            .is_ok()
        );
        assert!(
            check(
                &goal("past_up"),
                &start,
                &seen("Place 10", (84, 9), ""),
                &[]
            )
            .is_err()
        );
        let area = Goal {
            area: "Place 11".to_string(),
            ..goal("area")
        };
        assert!(check(&area, &start, &seen("place 11", (0, 0), ""), &[]).is_ok());
        let money = Goal {
            watched: "money".to_string(),
            ..goal("watched")
        };
        assert!(
            check(
                &money,
                &start,
                &seen("Place 10", (78, 7), "Watched: lives 3, money 25"),
                &[]
            )
            .is_ok()
        );
        assert_eq!(
            check(
                &money,
                &start,
                &seen("Place 10", (78, 7), "Watched: money 20"),
                &[]
            )
            .unwrap_err(),
            "money stayed 20"
        );
    }

    #[test]
    fn dying_fails_a_try_that_must_survive() {
        let start = seen("Place 10", (78, 7), "Watched: lives 3");
        let survive = Goal {
            survive: true,
            watched: "lives".to_string(),
            ..goal("past_right")
        };
        let past = seen("Place 10", (90, 7), "Watched: lives 3");
        assert!(check(&survive, &start, &past, &[]).is_ok());
        let over = ["GAME OVER".to_string()];
        assert!(
            check(&survive, &start, &past, &over)
                .unwrap_err()
                .contains("died")
        );
        let lost = seen("Place 10", (90, 7), "Watched: lives 2");
        assert_eq!(
            check(&survive, &start, &lost, &[]).unwrap_err(),
            "lives went down from 3 to 2"
        );
        let fell = seen("Place 3", (90, 7), "Watched: lives 3");
        assert!(
            check(&survive, &start, &fell, &[])
                .unwrap_err()
                .contains("Place 3")
        );
        // Without survive, the same fall still gets past.
        assert!(check(&goal("past_right"), &start, &fell, &[]).is_ok());
        // A game over's screen, read as where she last stood: not past anything.
        let over = Seen {
            can_walk: false,
            ..past.clone()
        };
        assert!(
            check(&goal("past_right"), &start, &over, &[])
                .unwrap_err()
                .contains("map wasn't showing")
        );
        // A watched value can change in a menu.
        let money = Goal {
            watched: "lives".to_string(),
            ..goal("watched")
        };
        let menu = Seen {
            can_walk: false,
            ..seen("Place 10", (78, 7), "Watched: lives 4")
        };
        assert!(check(&money, &start, &menu, &[]).is_ok());
    }

    #[test]
    fn tries_are_checked_before_they_run() {
        let presses = |p: &str| Planned {
            presses: p.to_string(),
            ..Planned::default()
        };
        assert!(matches!(
            checked(&presses("right a:20+right:60")),
            Ok(Try::Presses(_))
        ));
        assert!(checked(&presses("")).is_err());
        assert!(checked(&presses("a a a a a a")).is_err());
        assert!(checked(&presses("start")).unwrap_err().contains("menus"));
        assert!(
            checked(&presses("right:900 right:400"))
                .unwrap_err()
                .contains("frames")
        );
        let routine = Planned {
            rules: vec![
                json!({"if_words": "", "if_near": "ahead 2", "press": "b"}),
                json!({"if_words": "", "if_near": "", "press": "right"}),
            ],
            most: 99,
            ..Planned::default()
        };
        let Ok(Try::Routine(skill)) = checked(&routine) else {
            panic!("a routine")
        };
        assert!(skill.reflex());
        assert_eq!(skill.most, ROUNDS_MOST as u32);
        assert_eq!(
            Try::Routine(skill).words(),
            "a routine: when ahead 2 press `b`, otherwise press `right` (20 rounds)"
        );
    }

    #[test]
    fn goals_need_what_they_measure() {
        assert!(goal("stand").wanted());
        assert!(
            !Goal {
                what: " ".to_string(),
                ..goal("stand")
            }
            .wanted()
        );
        assert!(!goal("fly").wanted());
        assert!(!goal("area").wanted());
        assert!(!goal("watched").wanted());
        assert_eq!(
            Goal {
                survive: true,
                ..goal("stand")
            }
            .line(),
            "stand on (83, 5), without dying"
        );
    }

    #[test]
    fn skill_names_are_tidy() {
        assert_eq!(skill_name("Jump Gap-Right!", ""), "jump_gap_right");
        assert_eq!(
            skill_name("", "the jump onto the ledge"),
            "the_jump_onto_the_ledge"
        );
        assert_eq!(skill_name("", "?"), "practised_move");
    }

    #[test]
    fn the_schema_is_strict() {
        let schema = schema();
        assert_eq!(schema["required"].as_array().unwrap().len(), 6);
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"].as_object().unwrap().len(), 6);
    }
}
