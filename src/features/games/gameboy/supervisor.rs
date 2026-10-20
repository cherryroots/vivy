//! The supervisor: a cheap model (Haiku) carries out one short task, like "go downstairs",
//! by picking actions (`actions.rs`) one at a time and seeing what each did. It ends the
//! task itself with `finish`, but code checks what really happened: "judge the screen, not
//! the claim".
//!
//! The prompt names the game and nothing else game-specific, stays the same for every task
//! (so it's cached), and asks for one action at a time. Each answer to an action is a short
//! line of what happened, where she is now with the map, and a small picture of the screen.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde_json::Value;

use super::actions::{self, Acted};
use crate::ai::{
    ChatEvent, ChatProvider, ChatRequest, Input, NativeRound, Part, Role, ToolOutput, Turn,
    next_input,
};
use crate::util::media::ModelImage;

/// The fixed instructions. The game and the task come in the first message.
const PROMPT: &str = "You play a Game Boy game through an emulator, one short task at a time.

Each turn, call one tool. After each one you're told what happened, where you are now, the \
words on screen, a map of the area around you (when you can walk) and a picture of the \
screen.

- Name the step to walk to and code walks there; never count presses for walking.
- Text boxes: use read_text. Choices and menus: move the cursor with press, then press a.
- To talk to someone or read a sign, stand next to it, face it (press that direction), \
then press a.
- Judge the screen, not your own claims. When nothing changes, check which way you're \
facing and try something different.
- When the screen shows the task is done, call finish with done: true. If it can't be done \
from here, call finish with done: false and say why.";

/// What the supervisor plays: a game it can look at and act in. The gate plays a Game Boy
/// of its own; the bot plays the one in `console.rs` (`player.rs`).
#[async_trait]
pub trait Player: Send {
    /// Runs one action (not `finish`).
    async fn act(&mut self, name: &str, args: &Value) -> anyhow::Result<Acted>;
    /// Looks without doing anything.
    async fn look(&mut self) -> anyhow::Result<Acted>;
    /// Where she is now, as text for the model (see `actions::describe`).
    async fn describe(&mut self, acted: &Acted) -> anyhow::Result<String>;
    /// The screen now, as a small PNG.
    async fn screen(&mut self) -> anyhow::Result<Vec<u8>>;
}

/// How a task went.
#[derive(Debug, Clone, Default)]
pub struct Episode {
    /// What `finish` said: done or not, and the summary. `None` if it never finished.
    pub finished: Option<(bool, String)>,
    /// Actions taken (not counting `finish`).
    pub actions: u32,
    /// Every screen of words seen, in order.
    pub words: Vec<String>,
    /// One line per action: what was asked and what happened.
    pub log: Vec<String>,
    /// Model requests made.
    pub requests: u32,
}

impl Episode {
    /// What happened, for the strategist and the narrator: the actions and their outcomes,
    /// then all the text read.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn digest(&self) -> String {
        let mut out = String::new();
        match &self.finished {
            Some((true, summary)) => writeln!(out, "Finished, done: {summary}").unwrap(),
            Some((false, summary)) => writeln!(out, "Finished, not done: {summary}").unwrap(),
            None => writeln!(out, "Ran out of actions before finishing.").unwrap(),
        }
        writeln!(out, "Actions:").unwrap();
        for line in &self.log {
            writeln!(out, "- {line}").unwrap();
        }
        if !self.words.is_empty() {
            writeln!(out, "Text read:").unwrap();
            for words in &self.words {
                writeln!(out, "- {}", words.replace('\n', " / ")).unwrap();
            }
        }
        out
    }
}

/// Carries out `task` in `game`, taking at most `most` actions. `hint`, if any, is told
/// as something she knows.
pub async fn run_task(
    provider: &dyn ChatProvider,
    player: &mut dyn Player,
    game: &str,
    task: &str,
    hint: Option<&str>,
    most: u32,
) -> anyhow::Result<Episode> {
    let mut episode = Episode::default();
    let start = player.look().await?;
    keep_words(&mut episode.words, &start.words);
    let mut first = format!("Game: {game}\nTask: {task}\n");
    if let Some(hint) = hint {
        writeln!(first, "Something you know: {hint}").unwrap();
    }
    write!(first, "\n{}", player.describe(&start).await?).unwrap();
    let mut input = Input::Full(vec![Turn {
        role: Role::User,
        parts: vec![Part::Text(first), Part::Image(png(player.screen().await?))],
    }]);
    let tools = actions::tools();
    let mut nudges = 0;
    let mut unchanged = 0;

    while episode.actions < most {
        let request = ChatRequest {
            system: PROMPT.to_string(),
            input: input.clone(),
            tools: tools.clone(),
            cache_key: "vivy:gameboy".to_string(),
            job: "game_play",
        };
        episode.requests += 1;
        let done = finish_stream(provider, request).await?;
        if done.tool_calls.is_empty() {
            // It answered in words instead of acting: remind it, twice at most.
            nudges += 1;
            if nudges > 2 {
                break;
            }
            input = with_reminder(provider, input, &done);
            continue;
        }
        let mut results = Vec::new();
        for call in &done.tool_calls {
            if call.name == "finish" {
                let summary = call.args["summary"].as_str().unwrap_or("").to_string();
                let finished = call.args["done"].as_bool().unwrap_or(false);
                episode.finished = Some((finished, summary));
                return Ok(episode);
            }
            let acted = player.act(&call.name, &call.args).await?;
            episode.actions += 1;
            keep_words(&mut episode.words, &acted.words);
            episode.log.push(format!(
                "{} {}: {}",
                call.name,
                args_text(&call.args),
                acted.outcome
            ));
            unchanged = if acted.outcome.contains("Nothing changed") {
                unchanged + 1
            } else {
                0
            };
            let mut output = format!("{}\n\n{}", acted.outcome, player.describe(&acted).await?);
            if unchanged >= 3 {
                output.push_str(
                    "\n\nNothing has changed for 3 actions. Check which way you're facing, \
                     and try something different.",
                );
            }
            results.push(ToolOutput {
                call_id: call.id.clone(),
                output,
                images: vec![png(player.screen().await?)],
            });
        }
        input = next_input(provider, input, &done, &results);
    }
    Ok(episode)
}

fn png(data: Vec<u8>) -> ModelImage {
    ModelImage {
        mime: "image/png",
        data,
    }
}

fn keep_words(all: &mut Vec<String>, new: &[String]) {
    for words in new {
        if all.last() != Some(words) {
            all.push(words.clone());
        }
    }
}

/// The arguments as they'd be typed: `(5, 3)` or `"up a"`.
fn args_text(args: &Value) -> String {
    if let (Some(x), Some(y)) = (args["x"].as_i64(), args["y"].as_i64()) {
        return format!("({x}, {y})");
    }
    match args["buttons"].as_str() {
        Some(buttons) => format!("\"{buttons}\""),
        None => String::new(),
    }
}

/// Streams a request to the end.
pub(super) async fn finish_stream(
    provider: &dyn ChatProvider,
    request: ChatRequest,
) -> anyhow::Result<crate::ai::Done> {
    let mut events = provider.stream(request).await?;
    loop {
        match events.recv().await {
            None => anyhow::bail!("the model's stream closed early"),
            Some(Err(err)) => return Err(err),
            Some(Ok(ChatEvent::Done(done))) => return Ok(done),
            Some(Ok(_)) => {}
        }
    }
}

/// The conversation with the model's words-only answer and a reminder to act.
fn with_reminder(provider: &dyn ChatProvider, input: Input, done: &crate::ai::Done) -> Input {
    let reminder = Turn {
        role: Role::User,
        parts: vec![Part::Text(
            "Call one of the tools. Call finish when the task is done.".to_string(),
        )],
    };
    if let Some(continuation) = &done.continuation {
        return Input::After {
            continuation: continuation.clone(),
            new: vec![reminder],
        };
    }
    let mut turns = match input {
        Input::Full(turns) => turns,
        Input::After { new, .. } => new,
    };
    turns.push(Turn {
        role: Role::Assistant,
        parts: vec![Part::Native {
            provider: provider.name().to_string(),
            model: done
                .model
                .clone()
                .unwrap_or_else(|| provider.model().to_string()),
            rounds: vec![NativeRound {
                output: done.native.clone(),
                results: Vec::new(),
                setup: done.setup.clone(),
            }],
        }],
    });
    turns.push(reminder);
    Input::Full(turns)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::sync::mpsc;

    use super::*;
    use crate::ai::{Done, GeneratedFile, ToolCall};
    use crate::features::games::gameboy::world::View;

    /// Answers with each of its tool calls in turn (`None`: words only), and keeps what it
    /// was asked.
    struct Scripted {
        answers: Mutex<Vec<Option<(&'static str, Value)>>>,
        asked: Mutex<Vec<Input>>,
    }

    #[async_trait]
    impl ChatProvider for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn model(&self) -> &str {
            "m1"
        }
        async fn stream(
            &self,
            request: ChatRequest,
        ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>> {
            let asked = {
                let mut asked = self.asked.lock().unwrap();
                asked.push(request.input);
                asked.len()
            };
            let tool_calls = match self.answers.lock().unwrap().remove(0) {
                Some((name, args)) => vec![ToolCall {
                    id: format!("call{asked}"),
                    name: name.to_string(),
                    args,
                }],
                None => Vec::new(),
            };
            let (tx, rx) = mpsc::channel(4);
            tx.send(Ok(ChatEvent::Done(Done {
                tool_calls,
                ..Done::default()
            })))
            .await?;
            Ok(rx)
        }
        async fn download_file(&self, _file: &GeneratedFile) -> anyhow::Result<Vec<u8>> {
            unimplemented!()
        }
    }

    /// A game where pressing does nothing, and the screen always reads "Hello".
    struct Still;

    #[async_trait]
    impl Player for Still {
        async fn act(&mut self, name: &str, _args: &Value) -> anyhow::Result<Acted> {
            Ok(Acted {
                outcome: format!("Pressed {name}. Nothing changed."),
                ..self.look().await?
            })
        }
        async fn look(&mut self) -> anyhow::Result<Acted> {
            Ok(Acted {
                outcome: String::new(),
                view: View {
                    area: "Room".to_string(),
                    at: (1, 1),
                    can_walk: false,
                    text: "Hello".to_string(),
                    facts: String::new(),
                },
                words: vec!["Hello".to_string()],
            })
        }
        async fn describe(&mut self, _acted: &Acted) -> anyhow::Result<String> {
            Ok("Place: Room.".to_string())
        }
        async fn screen(&mut self) -> anyhow::Result<Vec<u8>> {
            Ok(vec![1, 2, 3])
        }
    }

    fn texts(input: &Input) -> String {
        let turns = match input {
            Input::Full(turns) => turns,
            Input::After { new, .. } => new,
        };
        let mut out = String::new();
        for part in turns.iter().flat_map(|t| &t.parts) {
            match part {
                Part::Text(text) => out.push_str(text),
                Part::ToolResult { output, images, .. } => {
                    out.push_str(output);
                    out.push_str(&format!(" [{} image]", images.len()));
                }
                _ => {}
            }
            out.push('\n');
        }
        out
    }

    #[tokio::test]
    async fn a_task_runs_until_finish_with_reminders_and_a_loop_warning() {
        let press = || Some(("press", serde_json::json!({"buttons": "a"})));
        let provider = Scripted {
            answers: Mutex::new(vec![
                None,
                press(),
                press(),
                press(),
                Some((
                    "finish",
                    serde_json::json!({"done": false, "summary": "stuck"}),
                )),
            ]),
            asked: Mutex::new(Vec::new()),
        };
        let episode = run_task(
            &provider,
            &mut Still,
            "Test Game",
            "say hi",
            Some("a hint"),
            10,
        )
        .await
        .unwrap();
        assert_eq!(episode.finished, Some((false, "stuck".to_string())));
        assert_eq!((episode.actions, episode.requests), (3, 5));
        assert_eq!(
            episode.words,
            vec!["Hello".to_string()],
            "the same words once"
        );
        let asked = provider.asked.lock().unwrap();
        let first = texts(&asked[0]);
        assert!(first.contains("Game: Test Game\nTask: say hi\nSomething you know: a hint"));
        assert!(texts(&asked[1]).contains("Call one of the tools."));
        let last = texts(&asked[4]);
        assert!(last.contains("Pressed press. Nothing changed.\n\nPlace: Room. [1 image]"));
        assert!(
            last.contains("Nothing has changed for 3 actions."),
            "{last}"
        );
    }

    #[tokio::test]
    async fn a_task_stops_after_its_actions() {
        let press = || Some(("press", serde_json::json!({"buttons": "a"})));
        let provider = Scripted {
            answers: Mutex::new(vec![press(), press(), press()]),
            asked: Mutex::new(Vec::new()),
        };
        let episode = run_task(&provider, &mut Still, "Test Game", "say hi", None, 2)
            .await
            .unwrap();
        assert_eq!(episode.finished, None);
        assert_eq!(episode.actions, 2);
        assert!(episode.digest().starts_with("Ran out of actions"));
    }

    #[test]
    fn a_digest_lists_actions_and_text() {
        let episode = Episode {
            finished: Some((true, "went downstairs".to_string())),
            actions: 1,
            words: vec!["MOM: Right.\nAll boys leave home".to_string()],
            log: vec!["walk_to (7, 1): Walked into Red's House 1F after 9 steps.".to_string()],
            requests: 2,
        };
        let digest = episode.digest();
        assert!(digest.starts_with("Finished, done: went downstairs"));
        assert!(digest.contains("- walk_to (7, 1): Walked into"));
        assert!(digest.contains("- MOM: Right. / All boys leave home"));
    }

    #[test]
    fn arguments_read_as_typed() {
        assert_eq!(args_text(&serde_json::json!({"x": 5, "y": 3})), "(5, 3)");
        assert_eq!(
            args_text(&serde_json::json!({"buttons": "up a"})),
            "\"up a\""
        );
        assert_eq!(args_text(&serde_json::json!({})), "");
    }
}
