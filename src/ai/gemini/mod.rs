//! Google's Gemini through the Interactions API, called directly over HTTP.
//!
//! One request per round: `POST /v1beta/interactions` with `"stream": true`. Gemini stores
//! every interaction (`"store": true`, kept 55 days on a paid key), so like OpenAI a
//! follow-up only sends what's new plus `previous_interaction_id`, and Gemini keeps its own
//! thinking and tool calls in between.
//!
//! An interaction is a list of steps: what the user sent (`user_input`), the model's
//! thinking (`thought`), calls to built-in tools (`google_search_call`, `url_context_call`,
//! `code_execution_call` and their `..._result`), calls to the bot's tools
//! (`function_call`) and the answer (`model_output`). The stream sends each step as
//! `step.start`, a few `step.delta`s and `step.stop`; [`Collector`] puts them back
//! together and turns them into [`ChatEvent`]s.
//!
//! Gemini watches videos itself ([`ChatProvider::watches_video`]): a video someone posts
//! is uploaded with the Files API (`files.rs`) and sent by its link, and a YouTube link is
//! sent as it is, so the model sees the whole video with its sound, not a few frames.
//! PDFs are uploaded the same way, so the model reads them. Pictures go inline.
//!
//! What each interaction cost is added to [`Spend`] (see `price.rs`).
//!
//! API reference: <https://ai.google.dev/gemini-api/docs/interactions>,
//! streaming: <https://ai.google.dev/gemini-api/docs/streaming>,
//! videos: <https://ai.google.dev/gemini-api/docs/video-understanding>

mod files;
mod price;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, bail};
use async_trait::async_trait;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::api::{self, error_message};
use super::fallback::ApiError;
use super::spend::Spend;
use super::sse::SseParser;
use super::{
    Activity, ChatEvent, ChatProvider, ChatRequest, Done, GeneratedFile, Input, Job, ModelFile,
    Part, Role, ToolCall, Turn, job_settings,
};
use crate::util::media::ModelImage;
use files::Files;

/// Where the Gemini API lives.
const BASE_URL: &str = "https://generativelanguage.googleapis.com";
/// Waits before retrying when Gemini is busy or rate limited.
const RETRY_WAITS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(6)];

/// `[ai.gemini]` in config.toml.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GeminiConfig {
    pub model: String,
    /// How much it thinks: "low", "medium" or "high" ("minimal" too on Gemini 3 models).
    pub thinking_level: String,
    /// Show short summaries of its thinking in the reply's status line while it works
    /// (only for requests with the bot's tools: chat, chime-ins, reflection, diary).
    pub thought_summaries: bool,
    /// How a posted video is watched: "static" (frames once a second with the sound, in one
    /// go; best for short clips) or "agentic" (the model looks through the video for what
    /// it needs; cheaper on long ones, slower on short ones).
    pub video_processing: String,
    /// The same for YouTube links, which are often long.
    pub youtube_processing: String,
    /// USD a month; the log channel gets a warning at 80% and 100%. 0 turns that off.
    pub monthly_budget: f64,
    /// Show on the status message how much input came from the cache this month.
    pub show_cache_hits: bool,
    /// Show on the status message what each job (chat, diary, ...) cost this month.
    pub show_job_costs: bool,
    /// Settings per job, like `[ai.gemini.jobs.mood]` (see [`Job`]).
    pub jobs: HashMap<String, Job>,
}

impl Default for GeminiConfig {
    fn default() -> Self {
        GeminiConfig {
            model: "gemini-4-argon".to_string(),
            thinking_level: "medium".to_string(),
            thought_summaries: true,
            video_processing: "static".to_string(),
            youtube_processing: "agentic".to_string(),
            monthly_budget: 25.0,
            show_cache_hits: true,
            show_job_costs: true,
            jobs: HashMap::new(),
        }
    }
}

pub struct Gemini {
    http: reqwest::Client,
    key: String,
    config: GeminiConfig,
    files: Files,
    /// The name of each of the bot's tools Gemini called, by call ID: a tool's answer has
    /// to name the tool again, and the tool loop only keeps the ID.
    call_names: Arc<Mutex<HashMap<String, String>>>,
    /// Where what each interaction cost is added up; `None` in tests.
    spend: Option<Spend>,
}

impl Gemini {
    pub fn new(
        http: reqwest::Client,
        key: String,
        config: GeminiConfig,
        spend: Option<Spend>,
    ) -> Gemini {
        Gemini {
            files: Files::new(http.clone(), key.clone()),
            http,
            key,
            config,
            call_names: Arc::default(),
            spend,
        }
    }

    /// The JSON body of a request. `uploads` has the link of each video and PDF in the
    /// order they appear (see [`Gemini::upload_files`]).
    fn request_body(&self, request: &ChatRequest, uploads: &[Option<String>]) -> Value {
        let (turns, previous) = match &request.input {
            Input::Full(turns) => (turns, None),
            Input::After { continuation, new } => (new, Some(continuation)),
        };
        let names = self.call_names.lock().unwrap().clone();
        let input = input_steps(turns, uploads, &self.config, &names);

        let job = job_settings(&self.config.jobs, request.job, None);
        let mut tools = Vec::new();
        if job.web.unwrap_or(true) {
            tools.push(json!({"type": "google_search"}));
            tools.push(json!({"type": "url_context"}));
        }
        if job.code.unwrap_or(true) {
            tools.push(json!({"type": "code_execution"}));
        }
        tools.extend(request.tools.iter().map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        }));

        let summaries = if self.shows_progress(request) {
            "auto"
        } else {
            "none"
        };
        let mut body = json!({
            "model": job.model.as_deref().unwrap_or(&self.config.model),
            "system_instruction": request.system,
            "input": input,
            "tools": tools,
            "generation_config": {
                "thinking_level": job.effort.as_deref().unwrap_or(&self.config.thinking_level),
                "thinking_summaries": summaries,
            },
            "store": true,
            "stream": true,
        });
        if let Some(previous) = previous {
            body["previous_interaction_id"] = json!(previous);
        }
        if request.answer_now {
            body["generation_config"]["tool_choice"] = json!("none");
        }
        body
    }

    /// Whether the summaries of its thinking go to the status line, like Claude's progress
    /// notes: only for requests with the bot's tools, where there's something to wait for.
    fn shows_progress(&self, request: &ChatRequest) -> bool {
        self.config.thought_summaries && !request.tools.is_empty()
    }

    /// Posts an interaction, retrying a couple of times when Gemini is busy.
    async fn interactions(&self, body: &Value) -> anyhow::Result<reqwest::Response> {
        let request = || {
            self.http
                .post(format!("{BASE_URL}/v1beta/interactions"))
                .header("x-goog-api-key", &self.key)
                .json(body)
        };
        let response =
            api::send_retrying("Gemini", request, &[429, 500, 502, 503, 504], &RETRY_WAITS)
                .await
                .context("couldn't reach Gemini")?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let text = response.text().await.unwrap_or_default();
        Err(ApiError {
            provider: "gemini",
            status,
            message: error_message(&text),
        }
        .into())
    }

    /// Uploads every video and PDF in the request, in order, a few at a time. One that
    /// fails to upload is `None`, and the model is told it couldn't be loaded.
    async fn upload_files(&self, request: &ChatRequest) -> Vec<Option<String>> {
        let turns = match &request.input {
            Input::Full(turns) => turns,
            Input::After { new, .. } => new,
        };
        let to_upload: Vec<&ModelFile> = turns
            .iter()
            .flat_map(|t| &t.parts)
            .filter_map(|part| match part {
                Part::Video(video) => Some(video),
                Part::File(file) if is_pdf(file) => Some(file),
                _ => None,
            })
            .collect();
        let mut uploads = Vec::new();
        for group in to_upload.chunks(4) {
            let results = futures::future::join_all(
                group
                    .iter()
                    .map(|file| self.files.upload(&file.name, &file.mime, &file.data)),
            )
            .await;
            for (file, result) in group.iter().zip(results) {
                match result {
                    Ok(uri) => uploads.push(Some(uri)),
                    Err(err) => {
                        warn!("couldn't upload {} to Gemini: {err:#}", file.name);
                        uploads.push(None);
                    }
                }
            }
        }
        uploads
    }
}

fn is_pdf(file: &ModelFile) -> bool {
    file.mime == "application/pdf"
}

fn base64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn image(image: &ModelImage) -> Value {
    json!({"type": "image", "data": base64(&image.data), "mime_type": image.mime})
}

/// Turns become the interaction's input steps: what people said as `user_input`, earlier
/// answers as `model_output` (their text: Gemini keeps its own, and other providers' are
/// only text), and tool results as `function_result`s. `uploads` is what
/// [`Gemini::upload_files`] returned for the same turns, and `names` the name of each tool
/// call.
fn input_steps(
    turns: &[Turn],
    uploads: &[Option<String>],
    config: &GeminiConfig,
    names: &HashMap<String, String>,
) -> Vec<Value> {
    let mut uploads = uploads.iter();
    let mut steps = Vec::new();
    for turn in turns {
        let mut content = Vec::new();
        for part in &turn.parts {
            match part {
                Part::Text(text) if text.trim().is_empty() => {}
                Part::Text(text) => content.push(json!({"type": "text", "text": text})),
                Part::Image(picture) => content.push(image(picture)),
                Part::Video(video) => match uploads.next().and_then(Option::as_ref) {
                    Some(uri) => content.push(json!({
                        "type": "video",
                        "uri": uri,
                        "mime_type": video.mime,
                        "processing": config.video_processing,
                    })),
                    None => content.push(json!({
                        "type": "text",
                        "text": format!("[the video {}, which couldn't be loaded]", video.name),
                    })),
                },
                Part::YouTube(link) => content.push(json!({
                    "type": "video",
                    "uri": link,
                    "processing": config.youtube_processing,
                })),
                Part::File(file) if is_pdf(file) => match uploads.next().and_then(Option::as_ref) {
                    Some(uri) => content.push(json!({
                        "type": "document",
                        "uri": uri,
                        "mime_type": file.mime,
                    })),
                    None => content.push(json!({
                        "type": "text",
                        "text": format!("[{} is attached, but it couldn't be uploaded]", file.name),
                    })),
                },
                Part::File(file) => content.push(json!({
                    "type": "text",
                    "text": format!(
                        "[{} is attached, but you can't open this kind of file here]",
                        file.name
                    ),
                })),
                Part::ToolResult {
                    call_id,
                    output,
                    images,
                } => {
                    let mut result = vec![json!({"type": "text", "text": output})];
                    result.extend(images.iter().map(image));
                    let mut step = json!({
                        "type": "function_result",
                        "call_id": call_id,
                        "result": result,
                    });
                    if let Some(name) = names.get(call_id) {
                        step["name"] = json!(name);
                    }
                    steps.push(step);
                }
                // Gemini keeps its own answers; the turn's text is enough.
                Part::Native { .. } => {}
            }
        }
        if !content.is_empty() {
            let kind = match turn.role {
                Role::User => "user_input",
                Role::Assistant => "model_output",
            };
            steps.push(json!({"type": kind, "content": content}));
        }
    }
    steps
}

/// Rebuilds the steps of a streamed interaction and turns its events into [`ChatEvent`]s.
#[derive(Default)]
struct Collector {
    /// The interaction's ID, from `interaction.created`.
    id: Option<String>,
    /// Each step so far, by its `index`.
    steps: Vec<Value>,
    /// The arguments of each function call so far, by step index (they stream as text).
    arguments: HashMap<usize, String>,
    /// The summary of the thinking step being written, for the status line.
    summary: String,
    /// Whether summaries go out as [`ChatEvent::Progress`].
    progress: bool,
}

impl Collector {
    /// Reads one event's JSON. Returns the events for the caller, ending with
    /// [`ChatEvent::Done`] once the interaction is complete, and the finished interaction
    /// then too (for the cost).
    fn event(&mut self, data: &Value) -> anyhow::Result<(Vec<ChatEvent>, Option<Value>)> {
        let index = data["index"].as_u64().unwrap_or(0) as usize;
        let mut events = Vec::new();
        match data["event_type"].as_str().unwrap_or_default() {
            "interaction.created" => {
                self.id = data["interaction"]["id"].as_str().map(str::to_string);
            }
            "step.start" => {
                let step = data["step"].clone();
                events.extend(start_activity(&step));
                if step["type"] == "thought" {
                    self.summary.clear();
                }
                if self.steps.len() <= index {
                    self.steps.resize(index + 1, Value::Null);
                }
                self.steps[index] = step;
            }
            "step.delta" => events.extend(self.delta(index, &data["delta"])),
            "step.stop" => {
                if let (Some(arguments), Some(step)) =
                    (self.arguments.remove(&index), self.steps.get_mut(index))
                {
                    // Bad JSON from the model reaches the tool as a string, which then
                    // reports a clear error back to the model.
                    step["arguments"] =
                        serde_json::from_str(&arguments).unwrap_or(Value::String(arguments));
                }
            }
            "interaction.completed" => {
                let interaction = &data["interaction"];
                if interaction["status"] == "failed" {
                    return Err(stream_error(&interaction["error"]).into());
                }
                // The finished interaction may list its steps; otherwise the streamed ones.
                if let Some(steps) = interaction["steps"].as_array()
                    && !steps.is_empty()
                {
                    self.steps = steps.clone();
                }
                self.steps.retain(|step| !step.is_null());
                let id = interaction["id"].as_str().map(str::to_string);
                events.push(ChatEvent::Done(Done {
                    continuation: id.or(self.id.take()),
                    tool_calls: tool_calls(&self.steps),
                    native: Value::Array(self.steps.clone()),
                    ..Done::default()
                }));
                return Ok((events, Some(interaction.clone())));
            }
            "error" => return Err(stream_error(&data["error"]).into()),
            // `interaction.status_update`, and kinds added later.
            _ => {}
        }
        Ok((events, None))
    }

    /// One `step.delta`: a piece of a step, added to the step it belongs to.
    fn delta(&mut self, index: usize, delta: &Value) -> Vec<ChatEvent> {
        if self.steps.len() <= index {
            self.steps.resize(index + 1, Value::Null);
        }
        let step = &mut self.steps[index];
        match delta["type"].as_str().unwrap_or_default() {
            "text" => {
                let text = delta["text"].as_str().unwrap_or_default();
                append_text(step, text);
                vec![ChatEvent::TextDelta(text.to_string())]
            }
            "thought_summary" => {
                let text = delta["content"]["text"].as_str().unwrap_or_default();
                self.summary.push_str(text);
                step["summary"] = json!([{"type": "text", "text": self.summary}]);
                if self.progress && !self.summary.trim().is_empty() {
                    vec![ChatEvent::Progress(self.summary.trim().to_string())]
                } else {
                    Vec::new()
                }
            }
            "thought_signature" => {
                step["signature"] = delta["signature"].clone();
                Vec::new()
            }
            "arguments_delta" => {
                let piece = delta["arguments"].as_str().unwrap_or_default();
                self.arguments.entry(index).or_default().push_str(piece);
                Vec::new()
            }
            // A built-in tool's call, with what it looks for.
            "google_search_call" | "url_context_call" => {
                step["arguments"] = delta["arguments"].clone();
                tool_activity(step).into_iter().collect()
            }
            _ => Vec::new(),
        }
    }
}

/// Adds streamed text to a step's last text content.
fn append_text(step: &mut Value, text: &str) {
    if !step["content"].is_array() {
        step["content"] = json!([]);
    }
    let content = step["content"].as_array_mut().unwrap();
    match content.last_mut() {
        Some(last) if last["type"] == "text" => {
            let joined = format!("{}{text}", last["text"].as_str().unwrap_or_default());
            last["text"] = json!(joined);
        }
        _ => content.push(json!({"type": "text", "text": text})),
    }
}

/// What the model is busy with when a step starts.
fn start_activity(step: &Value) -> Option<ChatEvent> {
    let activity = match step["type"].as_str()? {
        "thought" => Activity::Thinking,
        "google_search_call" | "url_context_call" => {
            return tool_activity(step).or(Some(ChatEvent::Activity(Activity::SearchingWeb)));
        }
        "code_execution_call" => Activity::RunningCode,
        "model_output" => Activity::Writing,
        _ => return None,
    };
    Some(ChatEvent::Activity(activity))
}

/// The search a Google Search step runs, or the site a URL context step reads, once its
/// arguments are known.
fn tool_activity(step: &Value) -> Option<ChatEvent> {
    let arguments = &step["arguments"];
    let activity = match step["type"].as_str()? {
        "google_search_call" => Activity::Searching(arguments["queries"][0].as_str()?.to_string()),
        "url_context_call" => {
            let url = arguments["urls"][0]
                .as_str()
                .or(arguments["url"].as_str())?;
            let site = reqwest::Url::parse(url).ok()?.host_str()?.to_string();
            Activity::Reading(site.trim_start_matches("www.").to_string())
        }
        _ => return None,
    };
    Some(ChatEvent::Activity(activity))
}

/// The bot's tools the model asked for.
fn tool_calls(steps: &[Value]) -> Vec<ToolCall> {
    steps
        .iter()
        .filter(|step| step["type"] == "function_call")
        .map(|step| ToolCall {
            id: step["id"].as_str().unwrap_or_default().to_string(),
            name: step["name"].as_str().unwrap_or_default().to_string(),
            args: match &step["arguments"] {
                Value::Null => json!({}),
                args => args.clone(),
            },
        })
        .collect()
}

/// An error that came in the stream, as an [`ApiError`] with the status the same error
/// has as an HTTP answer, so the fallback reads it the same way.
fn stream_error(error: &Value) -> ApiError {
    let status = match &error["code"] {
        Value::Number(code) => code.as_u64().unwrap_or(500) as u16,
        Value::String(code) => match code.to_lowercase().as_str() {
            "resource_exhausted" | "rate_limit_exceeded" => 429,
            "unavailable" | "overloaded" => 503,
            "gateway_timeout" | "deadline_exceeded" => 504,
            "internal" | "server_error" => 500,
            _ => 400,
        },
        _ => 500,
    };
    ApiError {
        provider: "gemini",
        status: reqwest::StatusCode::from_u16(status)
            .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
        message: error["message"]
            .as_str()
            .unwrap_or("no reason given")
            .to_string(),
    }
}

/// Keeps the name of each function call in `steps`, for its result in the next round.
fn remember_names(names: &Mutex<HashMap<String, String>>, steps: &[Value]) {
    let mut names = names.lock().unwrap();
    // Old answers' calls aren't needed again; don't let the list grow.
    if names.len() > 1000 {
        names.clear();
    }
    for call in tool_calls(steps) {
        names.insert(call.id, call.name);
    }
}

/// Adds what the finished `interaction` cost to [`Spend`].
async fn add_spend(
    spend: &Spend,
    job: &'static str,
    model: &str,
    interaction: &Value,
    steps: &[Value],
) {
    let used = price::cost(model, &interaction["usage"], steps, chrono::Utc::now());
    debug!(
        "Gemini used {} input tokens ({} read from cache) and cost ${:.4}",
        used.input + used.cache_read,
        used.cache_read,
        used.usd
    );
    spend.add(job, used).await;
}

#[async_trait]
impl ChatProvider for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    fn watches_video(&self) -> bool {
        true
    }

    async fn stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>> {
        let uploads = self.upload_files(&request).await;
        let body = self.request_body(&request, &uploads);
        let mut response = self.interactions(&body).await?;

        let (sender, receiver) = mpsc::channel(64);
        let spend = self.spend.clone().map(|spend| (spend, request.job));
        let model = body["model"].as_str().unwrap_or_default().to_string();
        let call_names = self.call_names.clone();
        let mut collector = Collector {
            progress: self.shows_progress(&request),
            ..Collector::default()
        };
        tokio::spawn(async move {
            let mut parser = SseParser::default();
            loop {
                let chunk = match response.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    // Also a stream that sent nothing for a while (the HTTP client's read
                    // timeout, see main.rs), which the fallback counts as down.
                    Err(err) => {
                        let err = anyhow::Error::new(err).context("reading Gemini's answer failed");
                        let _ = sender.send(Err(err)).await;
                        return;
                    }
                };
                for event in parser.push(&chunk) {
                    // The stream ends with `data: [DONE]`.
                    let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
                        continue;
                    };
                    let (events, finished) = match collector.event(&data) {
                        Ok(result) => result,
                        Err(err) => {
                            let _ = sender.send(Err(err)).await;
                            return;
                        }
                    };
                    if let Some(interaction) = &finished {
                        remember_names(&call_names, &collector.steps);
                        if let Some((spend, job)) = &spend {
                            add_spend(spend, job, &model, interaction, &collector.steps).await;
                        }
                    }
                    for event in events {
                        if sender.send(Ok(event)).await.is_err() {
                            // The receiver is gone (the answer was stopped): stop reading.
                            return;
                        }
                    }
                    if finished.is_some() {
                        return;
                    }
                }
            }
            let _ = sender
                .send(Err(anyhow::anyhow!(
                    "Gemini's stream ended before the answer did"
                )))
                .await;
        });
        Ok(receiver)
    }

    async fn download_file(&self, file: &GeneratedFile) -> anyhow::Result<Vec<u8>> {
        // Gemini's code execution doesn't make files to download.
        bail!("Gemini has no file {} to download", file.filename)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ToolDef;

    fn client() -> Gemini {
        Gemini::new(
            reqwest::Client::new(),
            "key".into(),
            GeminiConfig::default(),
            None,
        )
    }

    fn picture() -> ModelImage {
        ModelImage {
            mime: "image/png",
            data: vec![1],
        }
    }

    fn file(name: &str, mime: &str) -> ModelFile {
        ModelFile {
            name: name.into(),
            mime: mime.into(),
            data: b"data".to_vec(),
        }
    }

    #[test]
    fn request_body() {
        let gemini = client();
        gemini
            .call_names
            .lock()
            .unwrap()
            .insert("call_1".into(), "get_time".into());
        let request = ChatRequest {
            system: "Be nice.".into(),
            input: Input::After {
                continuation: "v1_abc".into(),
                new: vec![
                    Turn {
                        role: Role::User,
                        parts: vec![Part::Text("look".into()), Part::Image(picture())],
                    },
                    Turn {
                        role: Role::User,
                        parts: vec![Part::ToolResult {
                            call_id: "call_1".into(),
                            output: "12:00".into(),
                            images: Vec::new(),
                        }],
                    },
                ],
            },
            tools: vec![ToolDef {
                name: "get_time",
                description: "Time.",
                parameters: json!({"type": "object", "properties": {}}),
            }],
            cache_key: "discord:1".into(),
            job: "chat",
            answer_now: false,
        };
        let body = gemini.request_body(&request, &[]);
        assert_eq!(body["model"], "gemini-4-argon");
        assert_eq!(body["previous_interaction_id"], "v1_abc");
        assert_eq!(body["system_instruction"], "Be nice.");
        assert_eq!(body["store"], true);
        assert_eq!(
            body["generation_config"],
            json!({"thinking_level": "medium", "thinking_summaries": "auto"})
        );
        assert_eq!(
            body["input"],
            json!([
                {"type": "user_input", "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image", "data": "AQ==", "mime_type": "image/png"},
                ]},
                {"type": "function_result", "call_id": "call_1", "name": "get_time",
                 "result": [{"type": "text", "text": "12:00"}]},
            ])
        );
        let tools = body["tools"].as_array().unwrap();
        let kinds: Vec<&str> = tools.iter().map(|t| t["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            ["google_search", "url_context", "code_execution", "function"]
        );
        assert_eq!(tools[3]["name"], "get_time");

        // Background jobs get only the tools they need, and no summaries without bot tools.
        let mood = ChatRequest {
            job: "mood",
            answer_now: false,
            tools: Vec::new(),
            input: Input::Full(Vec::new()),
            ..request
        };
        let body = gemini.request_body(&mood, &[]);
        assert!(body.get("previous_interaction_id").is_none());
        assert_eq!(body["generation_config"]["thinking_level"], "low");
        assert_eq!(body["generation_config"]["thinking_summaries"], "none");
        assert_eq!(body["tools"], json!([]));
    }

    #[test]
    fn videos_youtube_and_files() {
        let turns = vec![
            Turn {
                role: Role::User,
                parts: vec![
                    Part::Text("what happens?".into()),
                    Part::Video(file("clip.mp4", "video/mp4")),
                    Part::YouTube("https://www.youtube.com/watch?v=abc".into()),
                    Part::File(file("report.pdf", "application/pdf")),
                    Part::File(file("song.mp3", "audio/mpeg")),
                    Part::Video(file("broken.webm", "video/webm")),
                ],
            },
            Turn {
                role: Role::Assistant,
                parts: vec![Part::Text("A cat jumps.".into())],
            },
        ];
        let uploads = [
            Some("https://files/clip".to_string()),
            Some("https://files/report".to_string()),
            None,
        ];
        let steps = input_steps(&turns, &uploads, &GeminiConfig::default(), &HashMap::new());
        assert_eq!(
            Value::Array(steps),
            json!([
                {"type": "user_input", "content": [
                    {"type": "text", "text": "what happens?"},
                    {"type": "video", "uri": "https://files/clip", "mime_type": "video/mp4", "processing": "static"},
                    {"type": "video", "uri": "https://www.youtube.com/watch?v=abc", "processing": "agentic"},
                    {"type": "document", "uri": "https://files/report", "mime_type": "application/pdf"},
                    {"type": "text", "text": "[song.mp3 is attached, but you can't open this kind of file here]"},
                    {"type": "text", "text": "[the video broken.webm, which couldn't be loaded]"},
                ]},
                {"type": "model_output", "content": [{"type": "text", "text": "A cat jumps."}]},
            ])
        );
    }

    /// Feeds events to a collector and returns everything it sent.
    fn collect(collector: &mut Collector, events: &[Value]) -> Vec<ChatEvent> {
        events
            .iter()
            .flat_map(|data| collector.event(data).unwrap().0)
            .collect()
    }

    #[test]
    fn reads_a_streamed_interaction() {
        let mut collector = Collector {
            progress: true,
            ..Collector::default()
        };
        let events = collect(
            &mut collector,
            &[
                json!({"event_type": "interaction.created", "interaction": {"id": "v1_1", "status": "in_progress"}}),
                json!({"event_type": "step.start", "index": 0, "step": {"type": "thought"}}),
                json!({"event_type": "step.delta", "index": 0, "delta": {"type": "thought_summary", "content": {"type": "text", "text": "Checking the time."}}}),
                json!({"event_type": "step.delta", "index": 0, "delta": {"type": "thought_signature", "signature": "sig"}}),
                json!({"event_type": "step.stop", "index": 0}),
                json!({"event_type": "step.start", "index": 1, "step": {"type": "google_search_call", "id": "s1"}}),
                json!({"event_type": "step.delta", "index": 1, "delta": {"type": "google_search_call", "arguments": {"queries": ["time in Oslo"]}}}),
                json!({"event_type": "step.start", "index": 2, "step": {"type": "url_context_call", "arguments": {"urls": ["https://www.time.is/Oslo"]}}}),
                json!({"event_type": "step.start", "index": 3, "step": {"type": "function_call", "id": "call_1", "name": "get_time", "arguments": {}}}),
                json!({"event_type": "step.delta", "index": 3, "delta": {"type": "arguments_delta", "arguments": "{\"zone\":"}}),
                json!({"event_type": "step.delta", "index": 3, "delta": {"type": "arguments_delta", "arguments": "\"UTC\"}"}}),
                json!({"event_type": "step.stop", "index": 3}),
                json!({"event_type": "step.start", "index": 4, "step": {"type": "model_output"}}),
                json!({"event_type": "step.delta", "index": 4, "delta": {"type": "text", "text": "One "}}),
                json!({"event_type": "step.delta", "index": 4, "delta": {"type": "text", "text": "moment."}}),
                json!({"event_type": "interaction.status_update", "status": "requires_action"}),
            ],
        );
        assert_eq!(
            events,
            [
                ChatEvent::Activity(Activity::Thinking),
                ChatEvent::Progress("Checking the time.".into()),
                ChatEvent::Activity(Activity::SearchingWeb),
                ChatEvent::Activity(Activity::Searching("time in Oslo".into())),
                ChatEvent::Activity(Activity::Reading("time.is".into())),
                ChatEvent::Activity(Activity::Writing),
                ChatEvent::TextDelta("One ".into()),
                ChatEvent::TextDelta("moment.".into()),
            ]
        );
        let (events, finished) = collector
            .event(
                &json!({"event_type": "interaction.completed", "interaction": {
                "id": "v1_1", "status": "requires_action", "usage": {"total_input_tokens": 10}}}),
            )
            .unwrap();
        assert!(finished.is_some());
        let [ChatEvent::Done(done)] = events.as_slice() else {
            panic!("expected Done, got {events:?}");
        };
        assert_eq!(done.continuation.as_deref(), Some("v1_1"));
        assert_eq!(
            done.tool_calls,
            [ToolCall {
                id: "call_1".into(),
                name: "get_time".into(),
                args: json!({"zone": "UTC"}),
            }]
        );
        let steps = done.native.as_array().unwrap();
        assert_eq!(steps.len(), 5);
        assert_eq!(steps[0]["signature"], "sig");
        assert_eq!(
            steps[4]["content"],
            json!([{"type": "text", "text": "One moment."}])
        );
    }

    #[test]
    fn stream_errors_tell_outages_apart() {
        use crate::ai::fallback::{Outage, outage};
        let mut collector = Collector::default();
        let err = collector
            .event(&json!({"event_type": "error", "error": {"code": "gateway_timeout", "message": "took too long"}}))
            .unwrap_err();
        assert_eq!(outage(&err), Some(Outage::Down));
        assert!(err.to_string().contains("took too long"));
        let err = collector
            .event(&json!({"event_type": "error", "error": {"code": 400, "message": "bad input"}}))
            .unwrap_err();
        assert_eq!(outage(&err), None);
        let err = collector
            .event(
                &json!({"event_type": "interaction.completed", "interaction": {
                "status": "failed", "error": {"code": "internal", "message": "oops"}}}),
            )
            .unwrap_err();
        assert_eq!(outage(&err), Some(Outage::Down));
    }
}
