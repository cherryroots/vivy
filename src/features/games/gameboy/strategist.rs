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
//! the new notes. Stage 3's gate measures which works better.

use serde::Deserialize;
use serde_json::{Value, json};

use super::skills::{self, Skill};
use crate::ai::OpenAi;

/// The fixed instructions.
const PROMPT: &str = "You plan a run of a Game Boy game. An executor carries out one short \
task at a time; you pick the next one.

You get the goal, your notes, what the last task did (its actions and all the text read) \
and where the player is now. Facts read from the game's memory beat your notes.

The next task:
- one thing, a few steps at most, described by what's on screen, never by buttons or counts;
- ends with \"until\" and what the screen will show when it's done;
- different from a task that just failed, unless you can name what will go differently.

Notes: facts that make sense on their own (name the people, places and things), at most \
15. Keep the useful ones, fix or drop wrong ones, add what the last task taught.

Skills: the executor writes its own, with know-how notes and a routine code runs as one \
action. Each round of a routine, the first rule whose if_words are on screen (any case; \
empty fits any screen) presses its buttons (1 to 5, spaced, like \"down right a\"). It \
stops when the player can walk (unless stop_when_free is false), on a stop word once it \
has pressed something, after most rounds (at most 50), when no rule fits, or when 3 \
rounds change nothing. Each skill shows how its last use ended. Review them: rewrite one \
whose last use went wrong, merge near-copies into one and delete the rest, delete one \
that never helped, add one when the last task repeated a job no skill covers. Rules \
match words that are really on screen in the text read. Change nothing that works: an \
empty list keeps them all.";

/// How the strategist is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Calls {
    /// One call answers both the notes and the next task.
    One,
    /// One call for the notes, then one for the task.
    Split,
}

/// The strategist's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub notes: Vec<String>,
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
    notes: Option<Vec<String>>,
    #[serde(default)]
    next_task: Option<String>,
    #[serde(default)]
    skill_changes: Option<Vec<Value>>,
}

/// Asks for the next task.
pub async fn plan(
    openai: &OpenAi,
    calls: Calls,
    goal: &str,
    notes: &[String],
    skills: &[Skill],
    digest: &str,
    now: &str,
) -> anyhow::Result<Plan> {
    let skills = skills::for_review(skills);
    let situation = |notes: &[String]| {
        let notes = if notes.is_empty() {
            "(none yet)".to_string()
        } else {
            notes
                .iter()
                .map(|n| format!("- {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "Goal: {goal}\n\nYour notes:\n{notes}\n\nThe executor's skills:\n{skills}\n\n\
             The last task:\n{digest}\n\nNow:\n{now}"
        )
    };
    match calls {
        Calls::One => {
            let answer = openai
                .structured(
                    "game_plan",
                    PROMPT,
                    &situation(notes),
                    "plan",
                    &schema(true, true),
                )
                .await?;
            let parsed: Answer = serde_json::from_value(answer.value)?;
            Ok(Plan {
                notes: parsed.notes.unwrap_or_default(),
                task: parsed.next_task.unwrap_or_default(),
                skill_changes: parsed.skill_changes.unwrap_or_default(),
                tokens: answer.tokens,
                usd: answer.used.usd,
            })
        }
        Calls::Split => {
            let first = openai
                .structured(
                    "game_plan",
                    PROMPT,
                    &format!(
                        "{}\n\nFirst, only the notes and the skills.",
                        situation(notes)
                    ),
                    "notes",
                    &schema(true, false),
                )
                .await?;
            let first_answer = serde_json::from_value::<Answer>(first.value)?;
            let new_notes = first_answer.notes.unwrap_or_default();
            let second = openai
                .structured(
                    "game_plan",
                    PROMPT,
                    &format!("{}\n\nNow only the next task.", situation(&new_notes)),
                    "task",
                    &schema(false, true),
                )
                .await?;
            let task = serde_json::from_value::<Answer>(second.value)?
                .next_task
                .unwrap_or_default();
            Ok(Plan {
                notes: new_notes,
                task,
                skill_changes: first_answer.skill_changes.unwrap_or_default(),
                tokens: first.tokens + second.tokens,
                usd: first.used.usd + second.used.usd,
            })
        }
    }
}

/// The answer's JSON schema: reasoning first, then the notes and skill changes and/or the
/// task. Strict schemas need every property required and no others allowed.
fn schema(notes: bool, task: bool) -> Value {
    let mut properties = json!({"reasoning": {"type": "string"}});
    let mut required = vec!["reasoning"];
    if notes {
        properties["notes"] = json!({"type": "array", "items": {"type": "string"}});
        properties["skill_changes"] = json!({"type": "array", "items": skill_change()});
        required.extend(["notes", "skill_changes"]);
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

/// One change to a skill: `write_skill`'s arguments, all required (strict), and whether
/// it rewrites or deletes. A delete leaves the rest empty.
fn skill_change() -> Value {
    let strict = |properties: Value| {
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
    };
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
            json!(["reasoning", "notes", "skill_changes", "next_task"])
        );
        let change = &both["properties"]["skill_changes"]["items"];
        assert_eq!(change["additionalProperties"], false);
        assert_eq!(change["required"].as_array().unwrap().len(), 8);
        assert_eq!(
            change["properties"]["rules"]["items"]["required"],
            json!(["if_words", "press"])
        );
        assert_eq!(both["additionalProperties"], false);
        let notes = schema(true, false);
        assert!(notes["properties"].get("next_task").is_none());
        assert_eq!(
            schema(false, true)["required"],
            json!(["reasoning", "next_task"])
        );
    }
}
