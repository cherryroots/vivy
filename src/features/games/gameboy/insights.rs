//! Insights (stage 6): what messing around (`novelty.rs`) suggests about how a game works.
//! Before she first plays a game, code presses at random for a while and keeps the
//! moments where something new showed up; the supervisor's model looks at them once and
//! writes a few insights, like "Select shows the date and money" or "A in front of the
//! bed asks whether to sleep".
//!
//! They start unconfirmed. After each episode the strategist says which ones the last
//! task relied on and whether they held; one that works is confirmed, one that fails twice
//! is struck. Both models see the ones not struck. The journal keeps them.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::novelty::Moment;
use super::pictures::png_pair;
use super::strategist::strict;
use super::supervisor::finish_stream;
use crate::ai::{ChatProvider, ChatRequest, Input, Part, Role, ToolDef, Turn};
use crate::util::media::ModelImage;

/// Game minutes of messing around: about 20 seconds on the server.
pub const MESS_MINUTES: u64 = 20;
/// Moments the model looks at.
pub const MOMENTS: usize = 24;
/// Insights kept at most.
pub const INSIGHTS_MOST: usize = 8;
/// Fails that strike an insight.
pub const STRIKE_AT: u32 = 2;

/// Frames of messing around (about 60 a second).
pub const MESS_FRAMES: u64 = MESS_MINUTES * 60 * 60;

const PROMPT: &str = "You look at moments from a Game Boy game, found by pressing buttons \
at random: each is a moment where something new showed up. Each picture is one moment, the \
screen before on the left and after on the right, with the presses that led to it, oldest \
first (like a:8 for A held 8 frames). Say what they show about how this game works, for \
someone about to play it: what the buttons do (open a menu, talk, use an item or tool, \
run, jump), what menus and text boxes come up and how to leave them, how doors and new \
areas are reached, what parts of the screen mean (a status bar, a clock, money). Only what \
the pictures show; when a moment is unclear, leave it out. At most 8 insights, each one \
sentence that makes sense on its own, naming the moments that show it. Answer by calling \
insights.";

/// One insight, with how it has held up in play.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Insight {
    pub text: String,
    /// Times the strategist said it worked, and didn't.
    #[serde(default)]
    pub worked: u32,
    #[serde(default)]
    pub failed: u32,
}

/// What an insight is, by how it has held up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Unconfirmed,
    Confirmed,
    Struck,
}

impl Insight {
    pub fn new(text: impl Into<String>) -> Insight {
        Insight {
            text: text.into(),
            worked: 0,
            failed: 0,
        }
    }

    pub fn state(&self) -> State {
        if self.failed >= STRIKE_AT {
            State::Struck
        } else if self.worked > 0 {
            State::Confirmed
        } else {
            State::Unconfirmed
        }
    }

    /// For people watching: ✅ confirmed, ❔ unconfirmed, struck through when struck.
    pub fn shown(&self) -> String {
        match self.state() {
            State::Confirmed => format!("✅ {}", self.text),
            State::Unconfirmed => format!("❔ {}", self.text),
            State::Struck => format!("~~{}~~", self.text),
        }
    }
}

/// The strategist's word on one insight the last task relied on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Checked {
    /// Its number, from 1, as [`for_plan`] lists it.
    pub number: usize,
    pub worked: bool,
}

/// Counts the strategist's word on each insight. Numbers that aren't there are skipped.
pub fn check(insights: &mut [Insight], checked: &[Checked]) {
    for c in checked {
        if let Some(insight) = c.number.checked_sub(1).and_then(|i| insights.get_mut(i)) {
            if c.worked {
                insight.worked += 1;
            } else {
                insight.failed += 1;
            }
        }
    }
}

/// The ones not struck, numbered for the strategist: the numbers stay the same when one
/// is struck, so an answer names the right one.
pub fn for_plan(insights: &[Insight]) -> Vec<String> {
    insights
        .iter()
        .enumerate()
        .filter_map(|(i, insight)| {
            let state = match insight.state() {
                State::Struck => return None,
                State::Unconfirmed => "unconfirmed".to_string(),
                State::Confirmed => format!(
                    "confirmed: worked {}, failed {}",
                    insight.worked, insight.failed
                ),
            };
            Some(format!("{}. {} [{state}]", i + 1, insight.text))
        })
        .collect()
}

/// The ones not struck, for the supervisor's task, or `None` when there are none.
pub fn for_task(insights: &[Insight]) -> Option<String> {
    let lines: Vec<String> = insights
        .iter()
        .filter(|i| i.state() != State::Struck)
        .map(|i| {
            let sure = if i.state() == State::Confirmed {
                ""
            } else {
                " (unconfirmed)"
            };
            format!("- {}{sure}", i.text)
        })
        .collect();
    (!lines.is_empty()).then(|| {
        format!(
            "What pressing around in this game suggested (unconfirmed ones may be wrong):\n{}",
            lines.join("\n")
        )
    })
}

#[derive(Deserialize)]
struct Answer {
    insights: Vec<Written>,
}

#[derive(Deserialize)]
struct Written {
    text: String,
}

/// Has the model look at the moments and write the insights.
pub async fn learn(
    provider: &dyn ChatProvider,
    game: &str,
    moments: &[Moment],
) -> anyhow::Result<Vec<Insight>> {
    anyhow::ensure!(!moments.is_empty(), "messing around found nothing new");
    let mut parts = vec![Part::Text(format!("Game: {game}"))];
    for (i, moment) in moments.iter().enumerate() {
        parts.push(Part::Text(format!(
            "Moment {}: after {}",
            i + 1,
            moment.presses.join(" ")
        )));
        parts.push(Part::Image(ModelImage {
            mime: "image/png",
            data: png_pair(&moment.before, &moment.after, 1)?,
        }));
    }
    let request = ChatRequest {
        system: PROMPT.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts,
        }]),
        tools: vec![ToolDef {
            name: "insights",
            description: "What the moments show about how the game works: each insight \
                one sentence, with the moments that show it.",
            parameters: strict(json!({
                "insights": {"type": "array", "items": strict(json!({
                    "text": {"type": "string"},
                    "moments": {"type": "array", "items": {"type": "integer"}}
                }))}
            })),
        }],
        cache_key: "vivy:gameboy-insights".to_string(),
        job: "game_play",
        answer_now: false,
    };
    let done = finish_stream(provider, request).await?;
    let call = done
        .tool_calls
        .into_iter()
        .find(|call| call.name == "insights")
        .ok_or_else(|| anyhow::anyhow!("the model answered without calling insights"))?;
    let answer: Answer = serde_json::from_value(call.args)?;
    Ok(answer
        .insights
        .into_iter()
        .map(|w| w.text.trim().to_string())
        .filter(|t| !t.is_empty())
        .take(INSIGHTS_MOST)
        .map(Insight::new)
        .collect())
}

/// The log's line for what messing around found.
pub fn line(screens: usize, insights: &[Insight]) -> String {
    let mut line = format!(
        "🔎 **She messed around first:** {MESS_MINUTES} game minutes of pressing at random, \
         {screens} different screens."
    );
    if insights.is_empty() {
        line.push_str(" Nothing to learn from them.");
    } else {
        line.push_str(" What they suggest:");
        for insight in insights {
            write!(line, "\n- {}", insight.shown()).unwrap();
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insights_are_confirmed_when_they_work_and_struck_after_two_fails() {
        let mut insights = vec![
            Insight::new("Select shows the date."),
            Insight::new("B runs."),
            Insight::new("A talks."),
        ];
        check(
            &mut insights,
            &[
                Checked {
                    number: 1,
                    worked: true,
                },
                Checked {
                    number: 2,
                    worked: false,
                },
                // Not there: skipped.
                Checked {
                    number: 9,
                    worked: true,
                },
                Checked {
                    number: 0,
                    worked: true,
                },
            ],
        );
        let states: Vec<State> = insights.iter().map(Insight::state).collect();
        assert_eq!(
            states,
            [State::Confirmed, State::Unconfirmed, State::Unconfirmed]
        );
        check(
            &mut insights,
            &[Checked {
                number: 2,
                worked: false,
            }],
        );
        assert_eq!(insights[1].state(), State::Struck);
        // The struck one is left out, and the others keep their numbers.
        assert_eq!(
            for_plan(&insights),
            [
                "1. Select shows the date. [confirmed: worked 1, failed 0]",
                "3. A talks. [unconfirmed]"
            ]
        );
        assert_eq!(
            for_task(&insights).unwrap(),
            "What pressing around in this game suggested (unconfirmed ones may be wrong):\n\
             - Select shows the date.\n- A talks. (unconfirmed)"
        );
        assert_eq!(insights[1].shown(), "~~B runs.~~");
        assert!(for_task(&[]).is_none());
    }

    #[test]
    fn insights_from_before_counts_load() {
        let insight: Insight = serde_json::from_value(json!({"text": "A talks."})).unwrap();
        assert_eq!(insight, Insight::new("A talks."));
        let line = line(40, &[insight]);
        assert!(line.contains("40 different screens"));
        assert!(line.ends_with("\n- ❔ A talks."));
    }
}
