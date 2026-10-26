//! The strategist: once per episode, a stronger model (GPT-6 Astra on the free daily
//! tokens) reads the goal, her notes, her skills, what the last task did and where she is,
//! and answers with the notes to keep, changes to her skills and the supervisor's next
//! short task. No tools; the answer is JSON that fits a schema, which code reads.
//!
//! The supervisor drafts skills as it plays (`skills.rs`); the strategist edits them,
//! which a cheap model does badly: fixing one whose last use failed, merging near-copies,
//! deleting ones that never helped.
//!
//! It's asked in one call, or in two (`Calls::Split`): first the notes, then the task with
//! the new notes. Stage 3's gate found one call as good, for half the tokens, so the
//! session uses one. Once the free tokens are used up, the cheap model plans instead
//! (`plan_cheaply`).

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};

use super::insights::{self, Checked, Insight};
use super::notes::{Milestone, Note};
use super::practice::{self, Goal};
use super::puzzle::Wanted;
use super::skills::{self, Skill};
use super::supervisor::finish_stream;
use crate::ai::{ChatProvider, ChatRequest, Input, OpenAi, Part, Role, ToolDef, Turn};
use crate::util::media::ModelImage;

/// The fixed instructions.
const PROMPT: &str = "You plan a run of a Game Boy game. An executor carries out one short \
task at a time; you pick the next one.

You get the goal, your notes, what the last task did (its actions and all the text read) \
and where the player is now, with a picture of the screen as it is now. Facts read from \
the game's memory beat your notes; in a game with no facts, the picture is most of what \
you know about where the player is.

The next task:
- one thing, a few steps at most, described by what's on screen, never by buttons or counts;
- in one area, or from it into the next: the executor has 30 actions, and wild battles \
use them up;
- ends with \"until\" and what the screen will show when it's done; to reach an area, \
\"until you're in\" and its name as the game shows it, like \"until you're in Route 1\" \
(code checks it);
- different from a task that just failed, unless you can name what will go differently.

Essential tasks: when the next milestone needs one thing done that nothing else gets \
round (a key item to get, someone who must be talked to, an obstacle in the way), mark \
the task essential and keep at it until it's done. When it fails, try it another way \
(another route, another button, an item, a skill for it, puzzle help), never just go \
somewhere else; note what was tried and how it went, so the next try is different. \
Exploring elsewhere is for finding what the essential task needs.

Puzzle help: when the way on in this area needs more than walking (steps that carry the \
player somewhere else, things to push, switches) and walking or exploring has failed, \
ask for it: the spot to reach here as x and y, and in what, how you think it works. A \
solver gets the whole map and the steps that surprised code, and code follows its moves \
before the next task, checking each. Leave what empty for no help.

Practice: when the way on needs one exact move (a jump onto a ledge or across a gap, \
getting past an enemy) and it keeps missing in play, ask for practice before the task. \
Code saves the game, a model plans a try, code runs it and measures it against the goal, \
then loads the save back for the next try, up to 6; one that works is tried again from \
two later moments and kept as a skill saying how often it worked, and the game carries on \
from where it worked. It runs only when the task is essential and she has already been \
stuck on it an episode, at most 3 times a session; other asks are left out. Give what (the move, in a few words) and the goal code measures: \
stand (on x, y or a step from it, a second later and still), area (reach the one named in area), \
past_right or past_left (get past x), past_up or past_down (get past y), or watched (the \
watched value named in watched changes). survive true makes dying fail a try (words like \
game over, ending in another area, or the watched value named going down). Practise from \
where the move starts: the task before it gets her there. Leave what empty for none.

Notes: facts that make sense on their own (name the people, places and things), at most \
15. Keep the useful ones, fix or drop wrong ones, add what the last task taught. Each says \
where it came from: \"seen\" (on screen, in the text read or the facts), \"tried\" (a task \
showed it), \"hint from\" and the name (from someone watching), or \"guess\". Code checks \
the notes it can against its maps: [code checked: ...] means they agree, [code found it \
WRONG: ...] means fix or drop it. A note about where something is (a door, a person, a \
way on) names its area and spot as the facts write it, like \"<the place>'s door is at \
x <n>, y <n> in <area>\", so code can check it. Don't note what the facts show (HP, levels, money, where \
the player stands): they change by themselves.

Place names: in a game read with no memory map, code names places Place and a number. \
When the player is in one, say in place_name what it's called: its name as the game has \
shown it (a sign, a text box, a title), or until then a few words saying what it is, like \
\"the house she woke up in\". The facts show it as Called once given; give the game's name \
when it shows one. Notes and milestones can use these names. Leave it empty in a place \
that already has its right name, and in a game with real place names.

Insights: before she first played, code pressed buttons at random and a model wrote what \
the new screens suggested about how the game works (what buttons do, menus, doors). They \
start unconfirmed and can be wrong. When the last task relied on one, say in \
insights_checked whether it held (its number, worked true or false): one that works is \
confirmed, one that fails twice is struck. Leave out the ones the last task didn't test. \
Use them when picking tasks; don't copy them into the notes.

Milestones: up to 8 story steps toward the goal, next first, each bigger than a task (a \
new town or route, a key item got, a boss beaten; not a room or a staircase), and each \
one code can check: an area \
to go into (its name as the game shows it; being there already doesn't count), or a line \
of the facts and words in it (like fact \"Badges\" has \"Boulder\"). Leave area empty for \
a fact, fact and has empty for an area. Code keeps the reached ones; send the ones still \
ahead, changed as you learn more.

Memory search: the executor can find where the game keeps a number (HP, money, a \
counter) by searching memory like a cheat finder, and watch it; watched values are in the \
facts, as \"Watched: money 3175\". When a number that would help isn't in the facts, a task \
can be to find it: note its value on screen, do something that changes it, search again, \
until one place is left, then watch it.

Skills: the executor writes its own, with know-how notes and a routine code runs as one \
action. Each round of a routine, the first rule whose if_words are on screen (any case; \
empty fits any screen) presses its buttons (1 to 5, spaced, like \"down right a\"). It \
stops when the player can walk (unless stop_when_free is false), on a stop word once it \
has pressed something, after most rounds (at most 50), when no rule fits, or when 3 \
rounds change nothing. Stop words are words, never numbers: HP, levels and money change. \
A reflex is a routine for fights and jumps in real time: its rules have if_near, what must \
be around the player (near, ahead, behind, above, below, left, right or coming, then steps \
1 to 4, like \"ahead 2\"; or blocked, when the last direction didn't move it), and it runs \
while the player walks, stopping when words come up or it can't walk for a moment. holding keeps buttons held while a skill \
runs (a shield). Each skill shows how its last use ended. Review them: rewrite one \
whose last use went wrong, merge near-copies into one and delete the rest, delete one \
that never helped, add one when the last task repeated a job no skill covers. When something moving cost the player \
(knocked back, health or a life lost, a game over, a continue or password screen) and no \
reflex covers it, add a reflex for it: when it's ahead or coming, jump over it or attack it. Rules \
match words that are really on screen in the text read. Change nothing that works: an \
empty list keeps them all.";

/// How the strategist is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Calls {
    /// One call answers both the notes and the next task.
    One,
    /// One call for the notes, then one for the task. Only the gate asks this way.
    #[cfg_attr(not(test), allow(dead_code))]
    Split,
}

/// How hard it thinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Care {
    /// An ordinary episode: the `game_plan` job.
    Routine,
    /// She's stuck: a fresh-eyes look at the notes, the `game_review` job (high effort).
    Review,
}

impl Care {
    fn job(self) -> &'static str {
        match self {
            Care::Routine => "game_plan",
            Care::Review => "game_review",
        }
    }
}

/// Added for a review.
const REVIEW: &str = "Fresh eyes: the player has made no progress for several episodes. \
Read your notes as if someone else wrote them: drop or fix any that the text read or the \
facts don't back up, and pick a task unlike the recent ones.";

/// The strategist's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub notes: Vec<Note>,
    /// The milestones still ahead, as it sees them now.
    pub milestones: Vec<Milestone>,
    pub task: String,
    /// The task is the one way to the next milestone: when it fails, she keeps at it
    /// another way rather than going somewhere else.
    pub essential: bool,
    /// Puzzle help it asked for, to follow before the task (`puzzle.rs`).
    pub puzzle: Option<Wanted>,
    /// Practice it asked for, before the task and any puzzle help (`practice.rs`).
    pub practice: Option<Goal>,
    /// Changes to her skills, as `skills::review` takes them.
    pub skill_changes: Vec<Value>,
    /// Which insights the last task relied on, and whether they held.
    pub insights_checked: Vec<Checked>,
    /// What the place she's in is called, where code only numbers places (empty: as is).
    pub place_name: String,
    /// Tokens used, for the free daily tokens.
    pub tokens: u64,
    /// What it would cost at list prices.
    pub usd: f64,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    notes: Option<Vec<NoteAnswer>>,
    #[serde(default)]
    milestones: Option<Vec<Milestone>>,
    #[serde(default)]
    next_task: Option<String>,
    #[serde(default)]
    essential: Option<bool>,
    #[serde(default)]
    puzzle: Option<Wanted>,
    #[serde(default)]
    practice: Option<Goal>,
    #[serde(default)]
    skill_changes: Option<Vec<Value>>,
    #[serde(default)]
    insights_checked: Option<Vec<Checked>>,
    #[serde(default)]
    place_name: Option<String>,
}

/// A note in an answer: with its source, or (from a sloppy answer) only the text.
#[derive(Deserialize)]
#[serde(untagged)]
enum NoteAnswer {
    Full {
        text: String,
        #[serde(default)]
        source: String,
    },
    Plain(String),
}

impl Answer {
    fn notes(&mut self) -> Vec<Note> {
        self.notes
            .take()
            .unwrap_or_default()
            .into_iter()
            .map(|note| match note {
                NoteAnswer::Full { text, source } => Note::new(text, source),
                NoteAnswer::Plain(text) => Note::new(text, ""),
            })
            .filter(|note| !note.text.trim().is_empty())
            .collect()
    }

    /// The plan, with what the call used.
    fn plan(mut self, tokens: u64, usd: f64) -> Plan {
        Plan {
            notes: self.notes(),
            milestones: self.milestones.take().unwrap_or_default(),
            task: self.next_task.take().unwrap_or_default(),
            essential: self.essential.unwrap_or(false),
            puzzle: self.puzzle.take().filter(|p| !p.what.trim().is_empty()),
            practice: self.practice.take().filter(Goal::wanted),
            skill_changes: self.skill_changes.take().unwrap_or_default(),
            insights_checked: self.insights_checked.take().unwrap_or_default(),
            place_name: self.place_name.take().unwrap_or_default(),
            tokens,
            usd,
        }
    }
}

/// What the strategist is told, apart from its instructions.
pub struct Ask<'a> {
    pub goal: &'a str,
    pub notes: &'a [Note],
    /// Steps toward the goal, the reached ones first.
    pub milestones: &'a [Milestone],
    /// What messing around suggested, struck ones too (they keep their numbers).
    pub insights: &'a [Insight],
    pub skills: &'a [Skill],
    /// What the last task did.
    pub digest: &'a str,
    /// Where she is now, and anything code adds (the stuck ladder, the last episodes).
    pub now: &'a str,
    /// The screen as it is now: in a game with no memory map it's most of what code can
    /// say about where she is.
    pub screen: Option<&'a ModelImage>,
    pub care: Care,
}

impl Ask<'_> {
    fn situation(&self, notes: &[Note]) -> String {
        let lines = |items: Vec<String>| {
            if items.is_empty() {
                "(none yet)".to_string()
            } else {
                items
                    .iter()
                    .map(|i| format!("- {i}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let notes = lines(notes.iter().map(Note::for_plan).collect());
        let milestones = lines(self.milestones.iter().map(Milestone::for_plan).collect());
        let insights = match insights::for_plan(self.insights) {
            shown if shown.is_empty() => String::new(),
            shown => format!(
                "\n\nInsights from pressing around (numbered):\n{}",
                lines(shown)
            ),
        };
        let mut text = format!(
            "Goal: {}\n\nMilestones (code checks them):\n{milestones}\n\nYour notes:\n\
             {notes}{insights}\n\nThe executor's skills:\n{}\n\nThe last task:\n{}\n\n\
             Now:\n{}",
            self.goal,
            skills::for_review(self.skills),
            self.digest,
            self.now
        );
        if self.care == Care::Review {
            text.push_str("\n\n");
            text.push_str(REVIEW);
        }
        text
    }
}

/// Asks for the next task.
pub async fn plan(openai: &OpenAi, calls: Calls, ask: &Ask<'_>) -> anyhow::Result<Plan> {
    let job = ask.care.job();
    match calls {
        Calls::One => {
            let answer = openai
                .structured(
                    job,
                    PROMPT,
                    &ask.situation(ask.notes),
                    ask.screen,
                    "plan",
                    &schema(true, true),
                )
                .await?;
            let parsed: Answer = serde_json::from_value(answer.value)?;
            Ok(parsed.plan(answer.tokens, answer.used.usd))
        }
        Calls::Split => {
            let first = openai
                .structured(
                    job,
                    PROMPT,
                    &format!(
                        "{}\n\nFirst, only the notes and the skills.",
                        ask.situation(ask.notes)
                    ),
                    ask.screen,
                    "notes",
                    &schema(true, false),
                )
                .await?;
            let mut first_answer = serde_json::from_value::<Answer>(first.value)?;
            let new_notes = first_answer.notes();
            let second = openai
                .structured(
                    job,
                    PROMPT,
                    &format!("{}\n\nNow only the next task.", ask.situation(&new_notes)),
                    ask.screen,
                    "task",
                    &schema(false, true),
                )
                .await?;
            let (tokens, usd) = (
                first.tokens + second.tokens,
                first.used.usd + second.used.usd,
            );
            let second = serde_json::from_value::<Answer>(second.value)?.plan(0, 0.0);
            Ok(Plan {
                notes: new_notes,
                milestones: first_answer.milestones.unwrap_or_default(),
                task: second.task,
                essential: second.essential,
                puzzle: second.puzzle,
                practice: second.practice,
                skill_changes: first_answer.skill_changes.unwrap_or_default(),
                insights_checked: first_answer.insights_checked.unwrap_or_default(),
                place_name: second.place_name,
                tokens,
                usd,
            })
        }
    }
}

/// The job that plans once the free tokens are used up: `[ai.<provider>.jobs.game_plan_paid]`
/// picks its model (Claude Haiku or GPT-6.1 Sol unless set).
pub const PAID_JOB: &str = "game_plan_paid";

/// Asks a paid model instead (`planner` in the session, the `game_plan_paid` job), once
/// the free tokens are used up or OpenAI isn't set up: the same instructions, with the
/// answer as a call to its one tool, `plan`. Its cost shows on the spend card under
/// `game_plan_paid`.
pub async fn plan_cheaply(provider: &dyn ChatProvider, ask: &Ask<'_>) -> anyhow::Result<Plan> {
    let request = ChatRequest {
        system: format!("{PROMPT}\n\nAnswer by calling plan."),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts: std::iter::once(Part::Text(ask.situation(ask.notes)))
                .chain(ask.screen.cloned().map(Part::Image))
                .collect(),
        }]),
        tools: vec![ToolDef {
            name: "plan",
            description: "Your answer: the notes to keep, the milestones ahead, changes to \
                the executor's skills (an empty list keeps them all) and its next task.",
            parameters: without_reasoning(schema(true, true)),
        }],
        cache_key: "vivy:gameboy-plan".to_string(),
        job: PAID_JOB,
        answer_now: false,
    };
    let done = finish_stream(provider, request).await?;
    let call = done
        .tool_calls
        .into_iter()
        .find(|call| call.name == "plan")
        .context("the cheap planner answered without calling plan")?;
    let parsed: Answer = serde_json::from_value(call.args)?;
    Ok(parsed.plan(0, 0.0))
}

/// The answer's JSON schema: reasoning first, then the notes and skill changes and/or the
/// task. Strict schemas need every property required and no others allowed.
fn schema(notes: bool, task: bool) -> Value {
    let mut properties = json!({"reasoning": {"type": "string"}});
    let mut required = vec!["reasoning"];
    if notes {
        let note = strict(json!({
            "text": {"type": "string"},
            "source": {"type": "string"}
        }));
        let milestone = strict(json!({
            "name": {"type": "string"},
            "area": {"type": "string"},
            "fact": {"type": "string"},
            "has": {"type": "string"}
        }));
        properties["notes"] = json!({"type": "array", "items": note});
        properties["milestones"] = json!({"type": "array", "items": milestone});
        properties["skill_changes"] = json!({"type": "array", "items": skill_change()});
        let checked = strict(json!({
            "number": {"type": "integer"},
            "worked": {"type": "boolean"}
        }));
        properties["insights_checked"] = json!({"type": "array", "items": checked});
        required.extend(["notes", "milestones", "skill_changes", "insights_checked"]);
    }
    if task {
        properties["next_task"] = json!({"type": "string"});
        properties["essential"] = json!({"type": "boolean"});
        properties["puzzle"] = strict(json!({
            "what": {"type": "string"},
            "x": {"type": "integer"},
            "y": {"type": "integer"}
        }));
        let mut goals = vec![""];
        goals.extend(practice::GOALS);
        properties["practice"] = strict(json!({
            "what": {"type": "string"},
            "goal": {"type": "string", "enum": goals},
            "x": {"type": "integer"},
            "y": {"type": "integer"},
            "area": {"type": "string"},
            "watched": {"type": "string"},
            "survive": {"type": "boolean"}
        }));
        properties["place_name"] = json!({"type": "string"});
        required.extend(["next_task", "essential", "puzzle", "practice", "place_name"]);
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// The schema without its `reasoning` field, for a model that thinks before it answers
/// (Claude): asked to write its reasoning into the answer, Claude Opus declined to plan
/// at all (`reasoning_extraction`).
fn without_reasoning(mut schema: Value) -> Value {
    if let Some(properties) = schema["properties"].as_object_mut() {
        properties.remove("reasoning");
    }
    if let Some(required) = schema["required"].as_array_mut() {
        required.retain(|name| name != "reasoning");
    }
    schema
}

/// An object with these properties, all required and no others (a strict schema).
pub(super) fn strict(properties: Value) -> Value {
    let required: Vec<String> = properties
        .as_object()
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// One change to a skill: `write_skill`'s arguments, all required (strict), and whether
/// it rewrites or deletes. A delete leaves the rest empty.
fn skill_change() -> Value {
    let rule = strict(json!({
        "if_words": {"type": "string"},
        "if_near": {"type": "string"},
        "press": {"type": "string"}
    }));
    strict(json!({
        "name": {"type": "string"},
        "change": {"type": "string", "enum": ["rewrite", "delete"]},
        "use_when": {"type": "string"},
        "notes": {"type": "string"},
        "rules": {"type": "array", "items": rule},
        "stop_words": {"type": "array", "items": {"type": "string"}},
        "stop_when_free": {"type": "boolean"},
        "most": {"type": "integer"},
        "holding": {"type": "string"}
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_ask_for_reasoning_first_and_allow_nothing_else() {
        let both = schema(true, true);
        assert_eq!(
            both["required"],
            json!([
                "reasoning",
                "notes",
                "milestones",
                "skill_changes",
                "insights_checked",
                "next_task",
                "essential",
                "puzzle",
                "practice",
                "place_name"
            ])
        );
        let change = &both["properties"]["skill_changes"]["items"];
        assert_eq!(change["additionalProperties"], false);
        assert_eq!(change["required"].as_array().unwrap().len(), 9);
        assert_eq!(
            change["properties"]["rules"]["items"]["required"],
            json!(["if_near", "if_words", "press"])
        );
        assert_eq!(both["additionalProperties"], false);
        for (field, keys) in [("notes", 2), ("milestones", 4), ("insights_checked", 2)] {
            let item = &both["properties"][field]["items"];
            assert_eq!(item["additionalProperties"], false, "{field}");
            assert_eq!(item["required"].as_array().unwrap().len(), keys, "{field}");
        }
        let notes = schema(true, false);
        assert!(notes["properties"].get("next_task").is_none());
        assert_eq!(
            schema(false, true)["required"],
            json!([
                "reasoning",
                "next_task",
                "essential",
                "puzzle",
                "practice",
                "place_name"
            ])
        );
        // The plan tool for models that think first leaves reasoning out.
        let plan = without_reasoning(schema(false, true));
        assert!(plan["properties"].get("reasoning").is_none());
        assert_eq!(plan["required"][0], "next_task");
        let puzzle = &both["properties"]["puzzle"];
        assert_eq!(puzzle["required"], json!(["what", "x", "y"]));
        assert_eq!(puzzle["additionalProperties"], false);
        let practice = &both["properties"]["practice"];
        assert_eq!(practice["required"].as_array().unwrap().len(), 7);
        assert_eq!(practice["properties"]["goal"]["enum"][0], "");
    }

    #[test]
    fn answers_with_or_without_sources_and_milestones_parse() {
        let answer: Answer = serde_json::from_value(json!({
            "notes": [{"text": "The Mart is in Viridian City.", "source": "seen"}, "Old", " "],
            "milestones": [{"name": "Reach Viridian City", "area": "Viridian City",
                "fact": "", "has": ""}],
            "next_task": "go"
        }))
        .unwrap();
        let plan = answer.plan(5, 0.1);
        assert_eq!(
            plan.notes,
            [
                Note::new("The Mart is in Viridian City.", "seen"),
                Note::new("Old", "")
            ]
        );
        assert_eq!(plan.milestones[0].area, "Viridian City");
        assert_eq!(plan.task, "go");
        let bare: Answer = serde_json::from_value(json!({"next_task": "go"})).unwrap();
        assert!(bare.plan(0, 0.0).milestones.is_empty());
        let checked: Answer = serde_json::from_value(json!({
            "insights_checked": [{"number": 2, "worked": false}]
        }))
        .unwrap();
        assert_eq!(
            checked.plan(0, 0.0).insights_checked,
            [Checked {
                number: 2,
                worked: false
            }]
        );
    }
}
