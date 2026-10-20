//! The strategist: once per episode, a stronger model (GPT-6 Astra on the free daily
//! tokens) reads the goal, her notes, what the last task did and where she is, and answers
//! with the notes to keep and the supervisor's next short task. No tools; the answer is
//! JSON that fits a schema, which code reads.
//!
//! It's asked in one call, or in two (`Calls::Split`): first the notes, then the task with
//! the new notes. Stage 3's gate measures which works better.

use serde::Deserialize;
use serde_json::{Value, json};

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
15. Keep the useful ones, fix or drop wrong ones, add what the last task taught.";

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
}

/// Asks for the next task.
pub async fn plan(
    openai: &OpenAi,
    calls: Calls,
    goal: &str,
    notes: &[String],
    digest: &str,
    now: &str,
) -> anyhow::Result<Plan> {
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
        format!("Goal: {goal}\n\nYour notes:\n{notes}\n\nThe last task:\n{digest}\n\nNow:\n{now}")
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
                tokens: answer.tokens,
                usd: answer.used.usd,
            })
        }
        Calls::Split => {
            let first = openai
                .structured(
                    "game_plan",
                    PROMPT,
                    &format!("{}\n\nFirst, only the notes.", situation(notes)),
                    "notes",
                    &schema(true, false),
                )
                .await?;
            let new_notes = serde_json::from_value::<Answer>(first.value)?
                .notes
                .unwrap_or_default();
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
                tokens: first.tokens + second.tokens,
                usd: first.used.usd + second.used.usd,
            })
        }
    }
}

/// The answer's JSON schema: reasoning first, then the notes and/or the task. Strict
/// schemas need every property required and no others allowed.
fn schema(notes: bool, task: bool) -> Value {
    let mut properties = json!({"reasoning": {"type": "string"}});
    let mut required = vec!["reasoning"];
    if notes {
        properties["notes"] = json!({"type": "array", "items": {"type": "string"}});
        required.push("notes");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_ask_for_reasoning_first_and_allow_nothing_else() {
        let both = schema(true, true);
        assert_eq!(both["required"], json!(["reasoning", "notes", "next_task"]));
        assert_eq!(both["additionalProperties"], false);
        let notes = schema(true, false);
        assert!(notes["properties"].get("next_task").is_none());
        assert_eq!(
            schema(false, true)["required"],
            json!(["reasoning", "next_task"])
        );
    }
}
