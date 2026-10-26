//! Running a request to the end without showing it anywhere: stream the answer, run the
//! tools the model asks for, and continue until it's done. For background work like
//! chiming in or tidying memory; chat's answers stream into Discord with their own loop.

use super::{ChatEvent, ChatProvider, ChatRequest, NativeRound, ToolCall, ToolOutput, next_input};
use async_trait::async_trait;

/// Runs the tool calls of [`complete`]. Whatever happens, the model gets text back.
#[async_trait]
pub trait ToolRunner: Send + Sync {
    async fn run(&self, call: &ToolCall) -> String;
}

/// A finished answer.
#[derive(Debug, Default)]
pub struct Completed {
    /// The text of the last round: the answer after any tool calls.
    pub text: String,
    /// How to continue from this answer.
    pub continuation: Option<String>,
    /// The provider's raw output of each round, with the tools' results.
    pub rounds: Vec<NativeRound>,
}

/// Tells the model, with the results of its last round of tools, that it answers next.
/// The next request has `answer_now` set, so it couldn't call more tools anyway, but this
/// way it knows and writes its answer instead of saying what it'd look up.
pub fn last_tools(results: &mut [ToolOutput]) {
    if let Some(last) = results.last_mut() {
        last.output
            .push_str("\n\n(That was your last round of tools this time. Answer now.)");
    }
}

/// Sends `request`, runs tools for at most `max_rounds` rounds, and returns the answer.
/// After the last round of tools the model has to answer without them, so a busy model
/// still answers instead of failing.
pub async fn complete(
    provider: &dyn ChatProvider,
    request: ChatRequest,
    tools: &dyn ToolRunner,
    max_rounds: usize,
) -> anyhow::Result<Completed> {
    let ChatRequest {
        system,
        mut input,
        tools: defs,
        cache_key,
        job,
        answer_now: _,
    } = request;
    let mut completed = Completed::default();
    // One round more than `max_rounds`: the answer after the last round of tools.
    for round in 0..=max_rounds {
        let request = ChatRequest {
            system: system.clone(),
            input: input.clone(),
            tools: defs.clone(),
            cache_key: cache_key.clone(),
            job,
            answer_now: round == max_rounds,
        };
        let mut events = provider.stream(request).await?;
        completed.text.clear();
        let done = loop {
            match events.recv().await {
                None => anyhow::bail!("the model's stream closed early"),
                Some(Err(err)) => return Err(err),
                Some(Ok(ChatEvent::TextDelta(delta))) => completed.text.push_str(&delta),
                Some(Ok(ChatEvent::Activity(_) | ChatEvent::Progress(_))) => {}
                Some(Ok(ChatEvent::Done(done))) => break done,
            }
        };
        completed.continuation = done.continuation.clone();
        if done.tool_calls.is_empty() {
            completed.rounds.push(NativeRound {
                output: done.native,
                results: Vec::new(),
                setup: done.setup,
            });
            return Ok(completed);
        }
        if round == max_rounds {
            // Asked to answer now and called tools anyway.
            break;
        }
        let mut results = Vec::new();
        for call in &done.tool_calls {
            results.push(ToolOutput {
                call_id: call.id.clone(),
                output: tools.run(call).await,
                images: Vec::new(),
            });
        }
        if round + 1 == max_rounds {
            last_tools(&mut results);
        }
        input = next_input(provider, input, &done, &results);
        completed.rounds.push(NativeRound {
            output: done.native,
            results,
            setup: done.setup,
        });
    }
    anyhow::bail!("the model kept calling tools ({max_rounds} rounds)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{Done, GeneratedFile, Input, Part, Role, Turn};
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    /// A model that calls a tool every round it's allowed to, and notes what it was sent.
    #[derive(Default)]
    struct Busy {
        /// `answer_now` of each request, and the text of its last tool result.
        sent: Mutex<Vec<(bool, String)>>,
    }

    #[async_trait]
    impl ChatProvider for Busy {
        fn name(&self) -> &'static str {
            "busy"
        }
        fn model(&self) -> &str {
            "m1"
        }
        async fn stream(
            &self,
            request: ChatRequest,
        ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>> {
            let Input::Full(turns) = &request.input else {
                panic!("expected the whole conversation");
            };
            let last_result = turns
                .last()
                .and_then(|turn| turn.parts.last())
                .map(|part| match part {
                    Part::ToolResult { output, .. } => output.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            self.sent
                .lock()
                .unwrap()
                .push((request.answer_now, last_result));
            let (sender, receiver) = mpsc::channel(4);
            if request.answer_now {
                sender.send(Ok(ChatEvent::TextDelta("PASS".into()))).await?;
                sender.send(Ok(ChatEvent::Done(Done::default()))).await?;
            } else {
                let done = Done {
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "memory".into(),
                        args: serde_json::json!({}),
                    }],
                    ..Done::default()
                };
                sender.send(Ok(ChatEvent::Done(done))).await?;
            }
            Ok(receiver)
        }
        async fn download_file(&self, _file: &GeneratedFile) -> anyhow::Result<Vec<u8>> {
            unimplemented!()
        }
    }

    struct Saved;

    #[async_trait]
    impl ToolRunner for Saved {
        async fn run(&self, _call: &ToolCall) -> String {
            "saved".into()
        }
    }

    #[tokio::test]
    async fn answers_after_the_last_round_of_tools() {
        let busy = Busy::default();
        let request = ChatRequest {
            system: "system".into(),
            input: Input::Full(vec![Turn {
                role: Role::User,
                parts: vec![Part::Text("hi".into())],
            }]),
            tools: Vec::new(),
            cache_key: "test".into(),
            job: "chime",
            answer_now: false,
        };
        let done = complete(&busy, request, &Saved, 2).await.unwrap();
        assert_eq!(done.text, "PASS");
        assert_eq!(done.rounds.len(), 3);
        let sent = busy.sent.lock().unwrap();
        let answer_now: Vec<bool> = sent.iter().map(|(now, _)| *now).collect();
        assert_eq!(answer_now, [false, false, true]);
        // The first round's results are plain; the last round's say she answers next.
        assert_eq!(sent[1].1, "saved");
        assert!(sent[2].1.starts_with("saved\n\n(That was your last round"));
    }
}
