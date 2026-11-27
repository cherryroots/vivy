//! Sending one request through the Message Batches API, for jobs nobody waits on (see
//! [`Job::batch`](crate::ai::Job)). A batch costs half of every token, but its answer comes
//! when Anthropic gets to it: usually within minutes, at most 24 hours later.
//!
//! The request is the same body a normal one would be, without `"stream"`. Vivy creates
//! a batch of that one request, asks every [`POLL`] whether it has ended, then downloads
//! its result: the whole message at once. [`events`] turns that message into the events a
//! stream would have sent, so `collect.rs` reads both the same way.
//!
//! Anything that goes wrong (the batch is refused, errors, or takes longer than
//! `batch_wait_minutes`) is returned as an error, and the caller sends the request the
//! normal way. A batch that runs too long is canceled first, so it isn't paid twice.
//!
//! API reference: <https://platform.claude.com/docs/en/build-with-claude/batch-processing>

use std::time::Duration;

use anyhow::{Context as _, bail};
use serde_json::{Value, json};
use tracing::{debug, info};

use super::Sender;
use super::api::error_message;

/// How often to ask whether the batch has ended. Asking is free.
const POLL: Duration = Duration::from_secs(30);

/// The batch of one for `body`: the same request, minus what batches don't take.
fn batch_request(body: &Value) -> Value {
    let mut params = body.clone();
    if let Some(fields) = params.as_object_mut() {
        fields.remove("stream");
        // Batches refuse `fallbacks` with a 400, so a batched job stays on its own model.
        fields.remove("fallbacks");
    }
    json!({"requests": [{"custom_id": "vivy", "params": params}]})
}

/// Sends `body` as a batch of one, waits up to `wait` for it, and returns Claude's
/// message.
pub async fn send(sender: &Sender, body: &Value, wait: Duration) -> anyhow::Result<Value> {
    let request = batch_request(body);
    let created = sender
        .json(reqwest::Method::POST, "/messages/batches", Some(&request))
        .await
        .context("creating the batch")?;
    let id = created["id"].as_str().context("the batch has no ID")?;
    debug!("sent a Claude batch: {id}");

    let started = tokio::time::Instant::now();
    let batch = loop {
        tokio::time::sleep(POLL).await;
        // A failed check just waits for the next one: the batch runs (and is paid) anyway.
        match sender
            .json(
                reqwest::Method::GET,
                &format!("/messages/batches/{id}"),
                None,
            )
            .await
        {
            Ok(batch) if batch["processing_status"] == "ended" => break batch,
            Ok(_) => {}
            Err(err) => debug!("couldn't check Claude's batch {id}: {err:#}"),
        }
        if started.elapsed() >= wait {
            // Canceled requests that haven't started aren't billed.
            let cancel = format!("/messages/batches/{id}/cancel");
            let _ = sender.json(reqwest::Method::POST, &cancel, None).await;
            bail!("the batch took over {} minutes", wait.as_secs() / 60);
        }
    };
    info!(
        "Claude's batch {id} ended after {} s",
        started.elapsed().as_secs()
    );

    let url = batch["results_url"]
        .as_str()
        .context("the batch has no results")?;
    check_results_url(url)?;
    let request = || {
        sender
            .http
            .get(url)
            .header("x-api-key", &sender.key)
            .header("anthropic-version", super::VERSION)
    };
    let response = super::api::send_retrying(
        "Claude",
        request,
        &[429, 500, 502, 503, 529],
        &super::RETRY_WAITS,
    )
    .await
    .context("downloading the batch's result failed")?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!(
            "downloading the batch's result failed with {status}: {}",
            error_message(&text)
        );
    }
    let text = response.text().await?;
    let line = text.lines().next().context("the batch's result is empty")?;
    result_message(serde_json::from_str(line).context("the batch's result isn't JSON")?)
}

/// Checks that `url` points at Anthropic's API before sending our API key to it.
fn check_results_url(url: &str) -> anyhow::Result<()> {
    if url
        .strip_prefix(super::API)
        .is_some_and(|rest| rest.starts_with('/'))
    {
        return Ok(());
    }
    bail!("unexpected batch results URL: {url}");
}

/// The message in one line of a batch's results, or why there is none.
fn result_message(line: Value) -> anyhow::Result<Value> {
    let result = &line["result"];
    match result["type"].as_str() {
        Some("succeeded") => Ok(result["message"].clone()),
        Some("errored") => {
            let error = &result["error"]["error"];
            bail!(
                "the batch's request failed: {}",
                error["message"].as_str().unwrap_or("no reason given")
            )
        }
        other => bail!("the batch's request {}", other.unwrap_or("has no result")),
    }
}

/// The events a stream would have sent for `message`: its start, each block whole, and
/// its end. `Collector` reads blocks given whole in `content_block_start` as they are.
pub fn events(message: &Value) -> Vec<Value> {
    let mut start = message.clone();
    start["content"] = json!([]);
    let mut events = vec![json!({"type": "message_start", "message": start})];
    for (index, block) in message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        events.push(json!({"type": "content_block_start", "index": index, "content_block": block}));
        events.push(json!({"type": "content_block_stop", "index": index}));
    }
    events.push(json!({
        "type": "message_delta",
        "delta": {
            "stop_reason": message["stop_reason"],
            "stop_details": message["stop_details"],
            "container": message["container"],
        },
        "usage": message["usage"],
    }));
    events.push(json!({"type": "message_stop"}));
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ChatEvent;
    use crate::ai::claude::collect::Collector;

    #[test]
    fn a_batched_message_reads_like_a_stream() {
        let message = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-5-5",
            "content": [
                {"type": "thinking", "thinking": "", "signature": "sig"},
                {"type": "text", "text": "Tidying up."},
                {"type": "tool_use", "id": "toolu_1", "name": "memory", "input": {"command": "view", "path": "/memories"}},
                {"type": "tool_use", "id": "toolu_2", "name": "get_time", "input": {}},
            ],
            "stop_reason": "tool_use",
            "stop_details": null,
            "usage": {"input_tokens": 100, "cache_read_input_tokens": 50, "output_tokens": 20},
        });
        let mut collector = Collector::default();
        let mut text = String::new();
        for event in events(&message) {
            for chat_event in collector.push(&event).unwrap() {
                if let ChatEvent::TextDelta(delta) = chat_event {
                    text.push_str(&delta);
                }
            }
        }
        assert_eq!(text, "Tidying up.");
        assert_eq!(collector.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(collector.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(
            (
                collector.usage.input,
                collector.usage.cache_read,
                collector.usage.output
            ),
            (100, 50, 20)
        );
        let finished = collector.finish();
        // The blocks go back into the conversation exactly as Claude wrote them.
        assert_eq!(
            finished.content,
            message["content"].as_array().unwrap().clone()
        );
        let calls: Vec<_> = finished
            .tool_calls
            .iter()
            .map(|call| (call.name.as_str(), call.args.clone()))
            .collect();
        assert_eq!(
            calls,
            [
                ("memory", json!({"command": "view", "path": "/memories"})),
                ("get_time", json!({})),
            ]
        );
    }

    #[test]
    fn a_batch_drops_stream_and_fallbacks() {
        let body = json!({"model": "claude-opus-5-5", "stream": true, "fallbacks": "default",
            "max_tokens": 10});
        let params = &batch_request(&body)["requests"][0]["params"];
        assert!(params.get("stream").is_none());
        assert!(params.get("fallbacks").is_none());
        assert_eq!(params["model"], "claude-opus-5-5");
    }

    #[test]
    fn reads_a_result_line() {
        let ok = json!({"custom_id": "vivy", "result": {"type": "succeeded", "message": {"id": "msg_1"}}});
        assert_eq!(result_message(ok).unwrap()["id"], "msg_1");
        let failed = json!({"custom_id": "vivy", "result": {"type": "errored",
            "error": {"type": "error", "error": {"type": "invalid_request_error", "message": "bad"}}}});
        assert!(format!("{:#}", result_message(failed).unwrap_err()).contains("bad"));
        let expired = json!({"custom_id": "vivy", "result": {"type": "expired"}});
        assert!(format!("{:#}", result_message(expired).unwrap_err()).contains("expired"));
    }

    #[test]
    fn results_url_must_be_on_anthropics_api() {
        assert!(
            check_results_url("https://api.anthropic.com/v1/messages/batches/b1/results").is_ok()
        );
        assert!(check_results_url("https://evil.example/v1/messages/batches/b1/results").is_err());
        assert!(check_results_url("https://api.anthropic.com/v1.evil.example/results").is_err());
        assert!(
            check_results_url("http://api.anthropic.com/v1/messages/batches/b1/results").is_err()
        );
    }

    /// Talks to the real API: a reflection-like job on Haiku that calls a tool, run with
    /// `complete`, so both rounds go through batches. Needs `VIVY_ANTHROPIC_API_KEY`; run
    /// with `cargo test live_batch -- --ignored --nocapture`. Takes a few minutes.
    #[tokio::test]
    #[ignore]
    async fn live_batch() {
        use crate::ai::claude::{Claude, ClaudeConfig};
        use crate::ai::{
            ChatRequest, Input, Job, Part, Role, ToolCall, ToolDef, ToolReply, ToolRunner, Turn,
            complete,
        };

        struct Clock;
        #[async_trait::async_trait]
        impl ToolRunner for Clock {
            async fn run(&self, _call: &ToolCall) -> ToolReply {
                "12:34".to_string().into()
            }
        }

        let key = std::env::var("VIVY_ANTHROPIC_API_KEY").expect("VIVY_ANTHROPIC_API_KEY");
        let mut config = ClaudeConfig::default();
        config.jobs.insert(
            "reflection".into(),
            Job {
                model: Some("claude-haiku-5-5".into()),
                effort: Some("low".into()),
                web: Some(false),
                ..Job::default()
            },
        );
        // The cloud sandbox's proxy re-signs Anthropic's certificate with its own CA.
        let mut http = reqwest::Client::builder();
        if let Ok(path) = std::env::var("SSL_CERT_FILE") {
            let pem = std::fs::read(path).expect("the CA bundle");
            for cert in reqwest::Certificate::from_pem_bundle(&pem).expect("PEM") {
                http = http.add_root_certificate(cert);
            }
        }
        let claude = Claude::new(http.build().unwrap(), key, config, None);
        let request = ChatRequest {
            system: "Answer in one short line.".into(),
            input: Input::Full(vec![Turn {
                role: Role::User,
                parts: vec![Part::Text("What time is it? Use get_time.".into())],
            }]),
            tools: vec![ToolDef {
                name: "get_time",
                description: "The time now.",
                parameters: json!({"type": "object", "properties": {}}),
            }],
            cache_key: "test".into(),
            job: "reflection",
            answer_now: false,
        };
        let started = std::time::Instant::now();
        let done = complete(&claude, request, &Clock, 2)
            .await
            .expect("the answer");
        println!("{} ({} s)", done.text, started.elapsed().as_secs());
        assert_eq!(done.rounds.len(), 2);
        assert!(done.text.contains("12:34"));
        // Batches answer no sooner than the first check, 30 seconds in.
        assert!(started.elapsed() >= 2 * POLL);
    }
}
