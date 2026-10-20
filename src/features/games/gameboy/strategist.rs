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

use super::notes::{Milestone, Note};
use super::skills::{self, Skill};
use super::supervisor::finish_stream;
use crate::ai::{ChatProvider, ChatRequest, Input, OpenAi, Part, Role, ToolDef, Turn};

/// The fixed instructions.
const PROMPT: &str = "You plan a run of a Game Boy game. An executor carries out one short \
task at a time; you pick the next one.

You get the goal, your notes, what the last task did (its actions and all the text read) \
and where the player is now. Facts read from the game's memory beat your notes.

The next task:
- one thing, a few steps at most, described by what's on screen, never by buttons or counts;
- in one area, or from it into the next: the executor has 30 actions, and wild battles \
use them up;
- ends with \"until\" and what the screen will show when it's done; to reach an area, \
\"until you're in\" and its name as the game shows it, like \"until you're in Route 1\" \
(code checks it);
- different from a task that just failed, unless you can name what will go differently.

Notes: facts that make sense on their own (name the people, places and things), at most \
15. Keep the useful ones, fix or drop wrong ones, add what the last task taught. Each says \
where it came from: \"seen\" (on screen, in the text read or the facts), \"tried\" (a task \
showed it), \"hint from\" and the name (from someone watching), or \"guess\". Code checks \
the notes it can against its maps: [code checked: ...] means they agree, [code found it \
WRONG: ...] means fix or drop it. Don't note what the facts show (HP, levels, money, where \
the player stands): they change by themselves.

Milestones: up to 8 steps toward the goal, next first, each one code can check: an area \
to be in (its name as the game shows it), or a line of the facts and words in it (like \
fact \"Badges\" has \"Boulder\"). Leave area empty for a fact, fact and has empty for an \
area. Code keeps the reached ones; send the ones still ahead, changed as you learn more.

Skills: the executor writes its own, with know-how notes and a routine code runs as one \
action. Each round of a routine, the first rule whose if_words are on screen (any case; \
empty fits any screen) presses its buttons (1 to 5, spaced, like \"down right a\"). It \
stops when the player can walk (unless stop_when_free is false), on a stop word once it \
has pressed something, after most rounds (at most 50), when no rule fits, or when 3 \
rounds change nothing. Stop words are words, never numbers: HP, levels and money change. \
Each skill shows how its last use ended. Review them: rewrite one \
whose last use went wrong, merge near-copies into one and delete the rest, delete one \
that never helped, add one when the last task repeated a job no skill covers. Rules \
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
    /// Changes to her skills, as `skills::review` takes them.
    pub skill_changes: Vec<Value>,
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
    skill_changes: Option<Vec<Value>>,
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
            skill_changes: self.skill_changes.take().unwrap_or_default(),
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
    pub skills: &'a [Skill],
    /// What the last task did.
    pub digest: &'a str,
    /// Where she is now, and anything code adds (the stuck ladder, the last episodes).
    pub now: &'a str,
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
        let mut text = format!(
            "Goal: {}\n\nMilestones (code checks them):\n{milestones}\n\nYour notes:\n\
             {notes}\n\nThe executor's skills:\n{}\n\nThe last task:\n{}\n\nNow:\n{}",
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
                    "task",
                    &schema(false, true),
                )
                .await?;
            let task = serde_json::from_value::<Answer>(second.value)?
                .next_task
                .unwrap_or_default();
            Ok(Plan {
                notes: new_notes,
                milestones: first_answer.milestones.unwrap_or_default(),
                task,
                skill_changes: first_answer.skill_changes.unwrap_or_default(),
                tokens: first.tokens + second.tokens,
                usd: first.used.usd + second.used.usd,
            })
        }
    }
}

/// Asks the cheap model (the supervisor's, the `game_play` job) instead, once the free
/// tokens are used up or OpenAI isn't set up: the same instructions, with the answer as
/// a call to its one tool, `plan`. Its cost shows on the spend card under `game_play`.
pub async fn plan_cheaply(provider: &dyn ChatProvider, ask: &Ask<'_>) -> anyhow::Result<Plan> {
    let request = ChatRequest {
        system: format!("{PROMPT}\n\nAnswer by calling plan."),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts: vec![Part::Text(ask.situation(ask.notes))],
        }]),
        tools: vec![ToolDef {
            name: "plan",
            description: "Your answer: your reasoning, the notes to keep, the milestones \
                ahead, changes to the executor's skills (an empty list keeps them all) and \
                its next task.",
            parameters: schema(true, true),
        }],
        cache_key: "vivy:gameboy-plan".to_string(),
        job: "game_play",
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
        required.extend(["notes", "milestones", "skill_changes"]);
    }
    if task {
        properties["next_task"] = json!({"type": "string"});
        required.push("next_task");
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// An object with these properties, all required and no others (a strict schema).
fn strict(properties: Value) -> Value {
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
        "most": {"type": "integer"}
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
                "next_task"
            ])
        );
        let change = &both["properties"]["skill_changes"]["items"];
        assert_eq!(change["additionalProperties"], false);
        assert_eq!(change["required"].as_array().unwrap().len(), 8);
        assert_eq!(
            change["properties"]["rules"]["items"]["required"],
            json!(["if_words", "press"])
        );
        assert_eq!(both["additionalProperties"], false);
        for (field, keys) in [("notes", 2), ("milestones", 4)] {
            let item = &both["properties"][field]["items"];
            assert_eq!(item["additionalProperties"], false, "{field}");
            assert_eq!(item["required"].as_array().unwrap().len(), keys, "{field}");
        }
        let notes = schema(true, false);
        assert!(notes["properties"].get("next_task").is_none());
        assert_eq!(
            schema(false, true)["required"],
            json!(["reasoning", "next_task"])
        );
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
    }
}
