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
//! What each interaction cost is added to [`Spend`] (see `price.rs`). With a service
//! account key, `billing.rs` reads the real bill from the Cloud Billing export instead.
//!
//! API reference: <https://ai.google.dev/gemini-api/docs/interactions>,
//! streaming: <https://ai.google.dev/gemini-api/docs/streaming>,
//! videos: <https://ai.google.dev/gemini-api/docs/video-understanding>

pub mod billing;
mod files;
mod price;
mod starter;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
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
    Part, Role, ToolCall, ToolFile, Turn, job_settings,
};
use crate::util::media::ModelImage;
use files::Files;
use starter::Starters;

/// Where the Gemini API lives.
const BASE_URL: &str = "https://generativelanguage.googleapis.com";
/// Waits before retrying when Gemini is busy or rate limited.
const RETRY_WAITS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(6)];

/// `[ai.gemini]` in config.toml.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GeminiConfig {
    pub model: String,
    /// How much it thinks: "low", "medium", "high" or "xhigh" ("xhigh" only on Neon and
    /// Hydrogen; other models get "high" instead).
    pub thinking_level: String,
    /// Show short summaries of its thinking in the reply's status line while it works
    /// (only for requests with the bot's tools: chat, chime-ins, reflection, diary).
    pub thought_summaries: bool,
    /// Send most of the bot's tools as "deferred": the model sees only their names until it
    /// looks one up with Gemini's tool search, in the same round. About 4k fewer tokens on
    /// every request (see [`ALWAYS_LOADED`]).
    pub defer_tools: bool,
    /// How a posted video is watched: "static" (frames once a second with the sound, in one
    /// go; best for short clips) or "agentic" (the model looks through the video for what
    /// it needs; cheaper on long ones, slower on short ones).
    pub video_processing: String,
    /// The same for YouTube links, which are often long.
    pub youtube_processing: String,
    /// USD a month; the log channel gets a warning at 80% and 100%. 0 turns that off.
    pub monthly_budget: f64,
    /// With `GEMINI_BILLING_KEY_FILE` in `.env` and `billing_table` set, read Google
    /// Cloud's bill hourly from the Cloud Billing export and use it in place of the estimate.
    pub billed_spend: bool,
    /// The export's BigQuery table, `project.dataset.gcp_billing_export_v1_...`.
    pub billing_table: String,
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
            model: "gemini-4-neon".to_string(),
            thinking_level: "high".to_string(),
            thought_summaries: true,
            defer_tools: true,
            video_processing: "static".to_string(),
            youtube_processing: "agentic".to_string(),
            monthly_budget: 25.0,
            billed_spend: true,
            billing_table: String::new(),
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
    /// The stored interactions new conversations continue from (see `starter.rs`).
    starters: Starters,
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
            starters: Starters::default(),
        }
    }

    /// Sends a request built from `request`, with code execution when `code` is true.
    /// The first request of a conversation with the bot's tools continues the starter of
    /// its system prompt and tools, so Gemini can read those from its cache (see
    /// `starter.rs`). Returns the body that was sent, for the model and the cost.
    async fn send(
        &self,
        request: &ChatRequest,
        uploads: &[Option<String>],
        code: bool,
    ) -> anyhow::Result<(Value, reqwest::Response)> {
        let mut body = self.request_body(request, uploads, code);
        let new_conversation = body.get("previous_interaction_id").is_none();
        let starter = if new_conversation && !request.tools.is_empty() {
            self.starter(&body).await
        } else {
            None
        };
        if let Some(id) = &starter {
            body["previous_interaction_id"] = json!(id);
        }
        let mut result = self.interactions(&body).await;
        // A flex request that failed (flex is full: 503 "high demand") goes again on the
        // standard tier; Gemini never moves one there by itself.
        if let Err(err) = &result
            && body["service_tier"] == "flex"
            && !is_not_found(err)
        {
            warn!("Gemini's flex tier failed, asking on the standard tier: {err:#}");
            body.as_object_mut().unwrap().remove("service_tier");
            result = self.interactions(&body).await;
        }
        match result {
            Ok(response) => Ok((body, response)),
            // Gemini no longer has the starter (it keeps them 55 days): ask without it,
            // and make a new one next time.
            Err(err) if starter.is_some() && is_not_found(&err) => {
                self.starters.forget(starter::key(&body));
                body.as_object_mut()
                    .unwrap()
                    .remove("previous_interaction_id");
                let response = self.interactions(&body).await?;
                Ok((body, response))
            }
            Err(err) => Err(err),
        }
    }

    /// The ID of the starter for `body`'s model, system prompt and tools, made now if
    /// there's none yet. `None` when making one failed: the request then goes without.
    async fn starter(&self, body: &Value) -> Option<String> {
        let key = starter::key(body);
        if let Some(id) = self.starters.get(key) {
            return Some(id);
        }
        let made = async {
            let response = self.interactions(&starter::body(body)).await?;
            let interaction: Value = response.json().await.context("reading the starter")?;
            if let Some(spend) = &self.spend {
                let model = body["model"].as_str().unwrap_or_default();
                add_spend(spend, "starter", model, &interaction, &[], false).await;
            }
            interaction["id"]
                .as_str()
                .map(str::to_string)
                .context("the starter has no ID")
        };
        match made.await {
            Ok(id) => {
                debug!("made a Gemini starter for new conversations: {id}");
                self.starters.insert(key, id.clone());
                Some(id)
            }
            Err(err) => {
                warn!("couldn't make a Gemini starter: {err:#}");
                None
            }
        }
    }

    /// The JSON body of a request. `uploads` has the link of each video and PDF in the
    /// order they appear (see [`Gemini::upload_files`]). `code` is false to leave out code
    /// execution, which Gemini refuses next to videos and PDFs (see [`Gemini::stream`]).
    fn request_body(&self, request: &ChatRequest, uploads: &[Option<String>], code: bool) -> Value {
        let (turns, previous) = match &request.input {
            Input::Full(turns) => (turns, None),
            Input::After { continuation, new } => (new, Some(continuation)),
        };
        let names = self.call_names.lock().unwrap().clone();
        let input = input_steps(turns, uploads, &self.config, &names);

        let mut job = job_settings(&self.config.jobs, request.job, Some(CHEAP_MODEL));
        // Chime-ins only read and react, so the cheap model, unless config.toml names one.
        // On Gemini they'd share little cache with answers anyway.
        if request.job == "chime" && job.model.is_none() {
            job.model = Some(CHEAP_MODEL.to_string());
        }
        let mut tools = Vec::new();
        if job.web.unwrap_or(true) {
            tools.push(json!({"type": "google_search"}));
            tools.push(json!({"type": "url_context"}));
        }
        if code && job.code.unwrap_or(true) {
            tools.push(json!({"type": "code_execution"}));
        }
        // With many tools, most are deferred and Gemini's tool search loads them when the
        // model needs one. The list stays the same every time, so the cache still works.
        let defer = self.config.defer_tools && request.tools.len() >= DEFER_FROM;
        if defer {
            tools.push(json!({"type": "tool_search"}));
        }
        tools.extend(request.tools.iter().map(|tool| {
            let mut function = json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            });
            if defer && !ALWAYS_LOADED.contains(&tool.name) {
                function["defer_loading"] = json!(true);
            }
            function
        }));

        let summaries = if self.shows_progress(request) {
            "auto"
        } else {
            "none"
        };
        let model = job.model.as_deref().unwrap_or(&self.config.model);
        let level = job.effort.as_deref().unwrap_or(&self.config.thinking_level);
        let mut body = json!({
            "model": model,
            "system_instruction": request.system,
            "input": input,
            "tools": tools,
            "generation_config": {
                "thinking_level": thinking_level(model, level),
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
        // A job nobody waits on takes the flex tier: half price, and it may take minutes.
        if job.batch == Some(true) {
            body["service_tier"] = json!("flex");
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
        // Each file with the type it's uploaded as.
        // In the order `input_steps` reads them back.
        let to_upload: Vec<(&ModelFile, &str)> = turns
            .iter()
            .flat_map(|t| &t.parts)
            .flat_map(|part| match part {
                Part::Video(video) => vec![(video, video.mime.as_str())],
                Part::File(file) => document_mime(file)
                    .map(|mime| (file, mime))
                    .into_iter()
                    .collect(),
                Part::ToolResult { files, .. } => files
                    .iter()
                    .filter(|opened| opened_kind(&opened.file).is_some())
                    .map(|opened| {
                        let mime = document_mime(&opened.file).unwrap_or(&opened.file.mime);
                        (&opened.file, mime)
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        let mut uploads = Vec::new();
        for group in to_upload.chunks(4) {
            let results = futures::future::join_all(
                group
                    .iter()
                    .map(|(file, mime)| self.files.upload(&file.name, mime, &file.data)),
            )
            .await;
            for ((file, _), result) in group.iter().zip(results) {
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

/// The thinking level `model` takes for `wanted`. Only Neon and Hydrogen think at "xhigh";
/// the other Gemini 4 models refuse it, so a job that sets it on one of them thinks at
/// "high" instead. Anything else goes as it is (`config_warnings` checks the words).
fn thinking_level<'a>(model: &str, wanted: &'a str) -> &'a str {
    let takes_xhigh = model.starts_with("gemini-4-neon") || model.starts_with("gemini-4-hydrogen");
    if wanted == "xhigh" && !takes_xhigh {
        "high"
    } else {
        wanted
    }
}

/// The type Gemini needs for `file` when it's a document it reads ([`crate::ai::DOCUMENTS`]).
fn document_mime(file: &ModelFile) -> Option<&'static str> {
    crate::ai::document_mime(&file.name, &file.mime)
}

/// The kind of content block a file a tool hands over goes in, and its type: "document",
/// "video" or "audio". `None` for anything else.
fn opened_kind(file: &ModelFile) -> Option<(&'static str, String)> {
    if let Some(mime) = document_mime(file) {
        Some(("document", mime.to_string()))
    } else if file.mime.starts_with("video/") {
        Some(("video", file.mime.clone()))
    } else if file.mime.starts_with("audio/") {
        Some(("audio", file.mime.clone()))
    } else {
        None
    }
}

/// The content block for a file a tool handed over, uploaded to `uri`, with the part of it
/// the tool asked for as `processing` (Gemini bills only that part).
fn opened_block(opened: &ToolFile, uri: &str, config: &GeminiConfig) -> Option<Value> {
    let (kind, mime) = opened_kind(&opened.file)?;
    let part = &opened.part;
    let mut block = json!({"type": kind, "uri": uri, "mime_type": mime});
    let processing = match kind {
        "document" if crate::ai::is_spreadsheet(&mime) => {
            let mut options = serde_json::Map::new();
            if let Some(sheet) = part.sheet {
                options.insert("sheet_index".into(), json!(sheet));
            }
            if let Some(cells) = &part.cells {
                options.insert("visible_cells".into(), json!(cells));
            }
            (!options.is_empty()).then(|| json!({"type": "static", "spreadsheet_options": options}))
        }
        "document" => {
            let mut options = serde_json::Map::new();
            if let Some((first, last)) = part.pages {
                options.insert("start_page".into(), json!(first));
                options.insert("end_page".into(), json!(last.max(first)));
            }
            if part.text_only {
                options.insert("content_layers".into(), json!(["page_text"]));
            }
            (!options.is_empty()).then(|| json!({"type": "static", "page_options": options}))
        }
        // A video or audio: the stretch asked for, or a video watched as set in config.
        _ => match (part.from, part.to) {
            (None, None) if kind == "video" => Some(json!(config.video_processing)),
            (None, None) => None,
            (from, to) => {
                let mut processing = json!({"type": "static"});
                if let Some(from) = from {
                    processing["start_offset"] = json!(format!("{from}s"));
                }
                if let Some(to) = to {
                    processing["end_offset"] = json!(format!("{to}s"));
                }
                Some(processing)
            }
        },
    };
    if let Some(processing) = processing {
        block["processing"] = processing;
    }
    Some(block)
}

/// Whether the request's new turns hold a video, a YouTube link or a document. Gemini's
/// code execution refuses those ("The mime type: application/pdf is not supported for
/// code execution"), so such a request goes without it.
fn has_video_or_document(request: &ChatRequest) -> bool {
    let turns = match &request.input {
        Input::Full(turns) => turns,
        Input::After { new, .. } => new,
    };
    turns.iter().flat_map(|t| &t.parts).any(|part| match part {
        Part::Video(_) | Part::YouTube(_) => true,
        Part::File(file) => document_mime(file).is_some(),
        Part::ToolResult { files, .. } => !files.is_empty(),
        _ => false,
    })
}

/// Whether Gemini answered 404: an interaction it no longer has.
fn is_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ApiError>()
        .is_some_and(|api| api.status == reqwest::StatusCode::NOT_FOUND)
}

/// Whether Gemini refused the request because of code execution next to a video or PDF.
/// That also happens on a follow-up whose new turns have none, when an earlier interaction
/// it continues had one: Gemini still holds that video or PDF.
fn refuses_code(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ApiError>().is_some_and(|api| {
        api.status == reqwest::StatusCode::BAD_REQUEST
            && api.message.contains("not supported for code execution")
    })
}

/// The most of a text file that goes into the request, in bytes (about 25k tokens).
const MAX_TEXT_FILE: usize = 100_000;

/// A request with at least this many of the bot's tools defers them (`defer_tools`):
/// chat and chime-ins, not the memory jobs with a few.
const DEFER_FROM: usize = 8;

/// The tools that are never deferred: she views her memory on nearly every question.
/// Measured live on Neon (2026-11-21): a request with every tool loaded was 6,694 tokens,
/// with all deferred 2,822, and the model found the right tool in the same round.
const ALWAYS_LOADED: [&str; 1] = ["memory"];

/// How much of a YouTube video that starts partway (`t=`) she watches from there, in
/// seconds. Watched in full ("static"), about 100 tokens a second on Gemini 4 (measured
/// 2026-11-21), so 2 minutes is about 12k tokens.
const YOUTUBE_CLIP: u64 = 120;

/// The cheap model, for chime-ins, describing emoji and the Apps menu helpers.
const CHEAP_MODEL: &str = "gemini-4-hydrogen";

/// What the model reads of a file that isn't a picture, video or document. Gemini's Files API
/// refuses most other kinds and its code execution only takes text, so a text file (one
/// that is valid UTF-8, like a CSV, code or notes) goes in as text, which her code can use
/// too. Anything else gets a note.
fn file_text(file: &ModelFile) -> String {
    let Ok(text) = std::str::from_utf8(&file.data) else {
        return format!(
            "[{} is attached, but you can't open this kind of file here]",
            file.name
        );
    };
    let mut end = text.len().min(MAX_TEXT_FILE);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = if end < text.len() {
        format!("\n[cut here: the file is longer than {MAX_TEXT_FILE} bytes]")
    } else {
        String::new()
    };
    format!(
        "<file name=\"{}\">\n{}{cut}\n</file>",
        file.name.replace('"', "'"),
        &text[..end]
    )
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
                // A link that starts partway: the clip from there, with a note on where.
                Part::YouTube(link) => match crate::util::media::youtube_start(link) {
                    Some((plain, start)) => {
                        content.push(json!({
                            "type": "video",
                            "uri": plain,
                            "processing": {
                                "type": "static",
                                "start_offset": format!("{start}s"),
                                "end_offset": format!("{}s", start + YOUTUBE_CLIP),
                            },
                        }));
                        content.push(json!({
                            "type": "text",
                            "text": format!(
                                "[{link} starts at {}:{:02}; you see {} minutes from there]",
                                start / 60,
                                start % 60,
                                YOUTUBE_CLIP / 60
                            ),
                        }));
                    }
                    None => content.push(json!({
                        "type": "video",
                        "uri": link,
                        "processing": config.youtube_processing,
                    })),
                },
                Part::File(file) if document_mime(file).is_some() => match uploads
                    .next()
                    .and_then(Option::as_ref)
                {
                    Some(uri) => content.push(json!({
                        "type": "document",
                        "uri": uri,
                        "mime_type": document_mime(file),
                    })),
                    None => content.push(json!({
                        "type": "text",
                        "text": format!("[{} is attached, but it couldn't be uploaded]", file.name),
                    })),
                },
                Part::File(file) => content.push(json!({"type": "text", "text": file_text(file)})),
                Part::ToolResult {
                    call_id,
                    output,
                    images,
                    files,
                } => {
                    let mut result = vec![json!({"type": "text", "text": output})];
                    result.extend(images.iter().map(image));
                    for opened in files.iter().filter(|f| opened_kind(&f.file).is_some()) {
                        let block = uploads
                            .next()
                            .and_then(Option::as_ref)
                            .and_then(|uri| opened_block(opened, uri, config));
                        result.push(block.unwrap_or_else(|| {
                            json!({
                                "type": "text",
                                "text": format!("[{} couldn't be uploaded]", opened.file.name),
                            })
                        }));
                    }
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
                let files = take_pictures(&mut self.steps);
                events.push(ChatEvent::Done(Done {
                    continuation: id.or(self.id.take()),
                    tool_calls: tool_calls(&self.steps),
                    files,
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
            // A picture in the answer, like a chart its code drew. The same one can come
            // twice in a row.
            "image" => {
                if !step["content"].is_array() {
                    step["content"] = json!([]);
                }
                let content = step["content"].as_array_mut().unwrap();
                let picture = json!({
                    "type": "image",
                    "mime_type": delta["mime_type"],
                    "data": delta["data"],
                });
                if !content.contains(&picture) {
                    content.push(picture);
                }
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

/// Takes the pictures out of the answer's steps (charts its code drew) as files for the
/// reply. Gemini sends them inline, so a file's `location` is the picture itself in base64,
/// and [`Gemini::download_file`] only decodes it. The steps keep a note instead, so the
/// saved history stays small.
fn take_pictures(steps: &mut [Value]) -> Vec<GeneratedFile> {
    let mut files = Vec::new();
    for step in steps
        .iter_mut()
        .filter(|step| step["type"] == "model_output")
    {
        let Some(content) = step["content"].as_array_mut() else {
            continue;
        };
        for item in content.iter_mut() {
            let Some(data) = item["data"].as_str().filter(|_| item["type"] == "image") else {
                continue;
            };
            let extension = match item["mime_type"].as_str().unwrap_or_default() {
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                "image/gif" => "gif",
                _ => "png",
            };
            let filename = format!("chart-{}.{extension}", files.len() + 1);
            files.push(GeneratedFile {
                location: data.to_string(),
                filename: filename.clone(),
            });
            *item =
                json!({"type": "text", "text": format!("[{filename}, attached to the answer]")});
        }
    }
    files
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
            "not_found" => 404,
            "permission_denied" => 403,
            "unauthenticated" => 401,
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

/// Adds what the finished `interaction` cost to [`Spend`]. `flex` when it was sent on the
/// flex tier, for when the interaction doesn't say which tier answered it.
async fn add_spend(
    spend: &Spend,
    job: &'static str,
    model: &str,
    interaction: &Value,
    steps: &[Value],
    flex: bool,
) {
    let flex = match interaction["service_tier"].as_str() {
        Some(tier) => tier == "flex",
        None => flex,
    };
    let used = price::cost(
        model,
        &interaction["usage"],
        steps,
        flex,
        chrono::Utc::now(),
    );
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

    fn opens_files(&self) -> bool {
        true
    }

    fn watches_video(&self) -> bool {
        true
    }

    async fn stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>> {
        let uploads = self.upload_files(&request).await;
        let (body, mut response) = match self
            .send(&request, &uploads, !has_video_or_document(&request))
            .await
        {
            // An earlier turn's video or PDF is still in the conversation: ask again
            // without code execution.
            Err(err) if refuses_code(&err) => {
                debug!("Gemini refused code execution next to a video or PDF; asking without it");
                self.send(&request, &uploads, false).await?
            }
            result => result?,
        };

        let (sender, receiver) = mpsc::channel(64);
        let spend = self.spend.clone().map(|spend| (spend, request.job));
        let model = body["model"].as_str().unwrap_or_default().to_string();
        let flex = body["service_tier"] == "flex";
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
                            add_spend(spend, job, &model, interaction, &collector.steps, flex)
                                .await;
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
        // The only files Gemini makes are pictures in the answer, and the location is the
        // picture itself (see `take_pictures`).
        base64::engine::general_purpose::STANDARD
            .decode(&file.location)
            .with_context(|| format!("{} isn't a picture Gemini sent", file.filename))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{FilePart, ToolDef};

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
                            files: Vec::new(),
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
        let body = gemini.request_body(&request, &[], true);
        assert_eq!(body["model"], "gemini-4-neon");
        assert_eq!(body["previous_interaction_id"], "v1_abc");
        assert_eq!(body["system_instruction"], "Be nice.");
        assert_eq!(body["store"], true);
        assert_eq!(
            body["generation_config"],
            json!({"thinking_level": "high", "thinking_summaries": "auto"})
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
        assert!(body.get("service_tier").is_none());

        // Background jobs get only the tools they need, and no summaries without bot tools.
        let mood = ChatRequest {
            job: "mood",
            answer_now: false,
            tools: Vec::new(),
            input: Input::Full(Vec::new()),
            ..request
        };
        let body = gemini.request_body(&mood, &[], true);
        assert!(body.get("previous_interaction_id").is_none());
        assert_eq!(body["generation_config"]["thinking_level"], "low");
        assert_eq!(body["generation_config"]["thinking_summaries"], "none");
        assert_eq!(body["tools"], json!([]));
        assert_eq!(body["model"], gemini.config.model);
        // A mood check can wait: the flex tier, at half price. Chat answers can't.
        assert_eq!(body["service_tier"], "flex");

        // Chime-ins and the cheap jobs take Hydrogen.
        for job in ["chime", "emoji", "translate", "refusal"] {
            let cheap = ChatRequest {
                job,
                system: String::new(),
                input: Input::Full(Vec::new()),
                tools: Vec::new(),
                cache_key: String::new(),
                answer_now: false,
            };
            assert_eq!(gemini.request_body(&cheap, &[], true)["model"], CHEAP_MODEL);
        }
    }

    #[test]
    fn tool_results_carry_files() {
        let config = GeminiConfig::default();
        let opened = |name: &str, mime: &str, part: FilePart| ToolFile {
            file: file(name, mime),
            part,
        };
        let pages = FilePart {
            pages: Some((3, 5)),
            text_only: true,
            ..FilePart::default()
        };
        assert_eq!(
            opened_block(&opened("r.pdf", "application/pdf", pages), "u", &config),
            Some(
                json!({"type": "document", "uri": "u", "mime_type": "application/pdf",
                "processing": {"type": "static", "page_options":
                    {"start_page": 3, "end_page": 5, "content_layers": ["page_text"]}}})
            )
        );
        let cells = FilePart {
            sheet: Some(2),
            cells: Some("A1:B9".into()),
            ..FilePart::default()
        };
        let block = opened_block(&opened("b.xlsx", "", cells), "u", &config).unwrap();
        assert_eq!(
            block["processing"]["spreadsheet_options"],
            json!({"sheet_index": 2, "visible_cells": "A1:B9"})
        );
        let clip = FilePart {
            from: Some(90),
            ..FilePart::default()
        };
        let block = opened_block(&opened("c.mp3", "audio/mpeg", clip), "u", &config).unwrap();
        assert_eq!(block["type"], "audio");
        assert_eq!(
            block["processing"],
            json!({"type": "static", "start_offset": "90s"})
        );
        // A whole video is watched as config says; a zip isn't opened.
        let whole = opened_block(
            &opened("v.mp4", "video/mp4", FilePart::default()),
            "u",
            &config,
        );
        assert_eq!(whole.unwrap()["processing"], "static");
        assert_eq!(
            opened_block(
                &opened("x.zip", "application/zip", FilePart::default()),
                "u",
                &config
            ),
            None
        );

        // The upload links are read back in order, after the turn's own files.
        let turns = vec![Turn {
            role: Role::User,
            parts: vec![
                Part::File(file("a.pdf", "application/pdf")),
                Part::ToolResult {
                    call_id: "c1".into(),
                    output: "opened".into(),
                    images: Vec::new(),
                    files: vec![opened("b.docx", "", FilePart::default())],
                },
            ],
        }];
        let uploads = [Some("first".to_string()), Some("second".to_string())];
        let steps = input_steps(&turns, &uploads, &config, &HashMap::new());
        assert_eq!(steps[0]["result"][1]["uri"], "second");
        assert_eq!(steps[1]["content"][0]["uri"], "first");
    }

    #[test]
    fn many_tools_are_deferred() {
        let tool = |name| ToolDef {
            name,
            description: "A tool.",
            parameters: json!({"type": "object", "properties": {}}),
        };
        let names = ["memory", "a", "b", "c", "d", "e", "f", "g"];
        let request = ChatRequest {
            system: String::new(),
            input: Input::Full(Vec::new()),
            tools: names.iter().map(|name| tool(*name)).collect(),
            cache_key: String::new(),
            job: "chat",
            answer_now: false,
        };
        let mut gemini = client();
        let body = gemini.request_body(&request, &[], false);
        let tools = body["tools"].as_array().unwrap();
        assert!(tools.contains(&json!({"type": "tool_search"})));
        let deferred = |name: &str| {
            let tool = tools.iter().find(|t| t["name"] == name).unwrap();
            tool["defer_loading"] == true
        };
        assert!(!deferred("memory"));
        assert!(deferred("a"));

        // Fewer tools, or turned off: all loaded, and no tool search.
        let few = ChatRequest {
            tools: vec![tool("memory"), tool("a")],
            system: String::new(),
            input: Input::Full(Vec::new()),
            cache_key: String::new(),
            job: "chat",
            answer_now: false,
        };
        let body = gemini.request_body(&few, &[], false);
        assert!(!body["tools"].to_string().contains("defer_loading"));
        gemini.config.defer_tools = false;
        let body = gemini.request_body(&request, &[], false);
        assert!(!body["tools"].to_string().contains("tool_search"));
    }

    #[test]
    fn no_code_execution_next_to_videos_and_documents() {
        let request = |parts| ChatRequest {
            system: String::new(),
            input: Input::Full(vec![Turn {
                role: Role::User,
                parts,
            }]),
            tools: Vec::new(),
            cache_key: String::new(),
            job: "chat",
            answer_now: false,
        };
        assert!(!has_video_or_document(&request(vec![Part::Image(
            picture()
        )])));
        assert!(!has_video_or_document(&request(vec![Part::File(file(
            "a.mp3",
            "audio/mpeg"
        ))])));
        assert!(has_video_or_document(&request(vec![Part::File(file(
            "a.pdf",
            "application/pdf"
        ))])));
        // Office files count by type, or by name when Discord sends them as plain bytes.
        let deck = file("plan.PPTX", "application/octet-stream");
        assert_eq!(
            document_mime(&deck),
            Some("application/vnd.openxmlformats-officedocument.presentationml.presentation")
        );
        assert!(has_video_or_document(&request(vec![Part::File(deck)])));
        assert_eq!(document_mime(&file("notes.txt", "text/plain")), None);
        assert!(has_video_or_document(&request(vec![Part::YouTube(
            "https://youtu.be/a".into()
        )])));

        // Gemini's own words for it, from a live request on 2026-11-21.
        let refusal = |status: u16| -> anyhow::Error {
            ApiError {
                provider: "gemini",
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                message: "The mime type: application/pdf is not supported for code execution."
                    .into(),
            }
            .into()
        };
        assert!(refuses_code(&refusal(400)));
        assert!(!refuses_code(&refusal(500)));
        assert!(!refuses_code(&anyhow::anyhow!("no connection")));
    }

    #[test]
    fn thinking_levels_fit_the_model() {
        assert_eq!(thinking_level("gemini-4-neon", "xhigh"), "xhigh");
        assert_eq!(thinking_level("gemini-4-hydrogen", "xhigh"), "xhigh");
        assert_eq!(thinking_level("gemini-4-argon", "xhigh"), "high");
        assert_eq!(thinking_level("gemini-4-argon", "low"), "low");
    }

    /// Talks to the real API: a tool round with a PDF (so without code execution), then the
    /// tool's answer (with a picture) continued from the stored interaction, which Gemini
    /// first refuses with code execution because the PDF is still in it. Needs `GEMINI_API_KEY`; run
    /// with `cargo test live_tool_round -- --ignored --nocapture`, and `GEMINI_MODEL` to try
    /// another model than the default.
    #[tokio::test]
    #[ignore]
    async fn live_tool_round() {
        let key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY");
        let mut config = GeminiConfig::default();
        if let Ok(model) = std::env::var("GEMINI_MODEL") {
            config.model = model;
        }
        let gemini = Gemini::new(reqwest::Client::new(), key, config, None);
        let tools = vec![ToolDef {
            name: "get_time",
            description: "The time now in a time zone.",
            parameters: json!({"type": "object", "properties": {"zone": {"type": "string"}},
                "required": ["zone"]}),
        }];
        let pdf = ModelFile {
            name: "note.pdf".into(),
            mime: "application/pdf".into(),
            data: tiny_pdf("The secret word is pineapple."),
        };
        let first = ChatRequest {
            system: "You are Vivy, a Discord bot. Answer in one short line.".into(),
            input: Input::Full(vec![Turn {
                role: Role::User,
                parts: vec![
                    Part::Text("Look up the time in UTC with get_time first.".into()),
                    Part::File(pdf),
                ],
            }]),
            tools: tools.clone(),
            cache_key: "test".into(),
            job: "chat",
            answer_now: false,
        };
        let done = live_answer(&gemini, first).await.1;
        let call = done.tool_calls.first().expect("a get_time call");
        assert_eq!(call.name, "get_time");

        let mut png = Vec::new();
        image::RgbImage::from_pixel(16, 16, image::Rgb([255, 0, 0]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let red = ModelImage {
            mime: "image/png",
            data: png,
        };
        let second = ChatRequest {
            system: "You are Vivy, a Discord bot. Answer in one short line.".into(),
            input: Input::After {
                continuation: done.continuation.expect("an interaction ID"),
                new: vec![
                    Turn {
                        role: Role::User,
                        parts: vec![Part::ToolResult {
                            call_id: call.id.clone(),
                            output: "14:30 UTC, and a picture of the sky now".into(),
                            images: vec![red],
                            files: Vec::new(),
                        }],
                    },
                    Turn {
                        role: Role::User,
                        parts: vec![Part::Text(
                            "Now tell me the time, the sky's colour and the PDF's secret word."
                                .into(),
                        )],
                    },
                ],
            },
            tools,
            cache_key: "test".into(),
            job: "chat",
            answer_now: true,
        };
        let (text, done) = live_answer(&gemini, second).await;
        println!("{text}");
        assert!(done.tool_calls.is_empty());
        let text = text.to_lowercase();
        assert!(text.contains("14:30") && text.contains("red") && text.contains("pineapple"));
    }

    /// Talks to the real API: a CSV in the message, and a chart of it drawn with code,
    /// which comes back as a file for the reply. `cargo test live_chart -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_chart() {
        let key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY");
        let mut config = GeminiConfig::default();
        if let Ok(model) = std::env::var("GEMINI_MODEL") {
            config.model = model;
        }
        let gemini = Gemini::new(reqwest::Client::new(), key, config, None);
        let csv = ModelFile {
            name: "scores.csv".into(),
            mime: "text/csv".into(),
            data: b"name,score\nana,3\nbo,7\ncy,11\n".to_vec(),
        };
        let request = ChatRequest {
            system: "You are Vivy, a Discord bot. Answer in one short line.".into(),
            input: Input::Full(vec![Turn {
                role: Role::User,
                parts: vec![
                    Part::Text("Draw a bar chart of these scores with matplotlib.".into()),
                    Part::File(csv),
                ],
            }]),
            tools: Vec::new(),
            cache_key: "test".into(),
            job: "chat",
            answer_now: false,
        };
        let (text, done) = live_answer(&gemini, request).await;
        println!(
            "{text} {:?}",
            done.files.iter().map(|f| &f.filename).collect::<Vec<_>>()
        );
        let chart = done.files.first().expect("a chart");
        let png = gemini.download_file(chart).await.unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }

    /// Talks to the real API: two new conversations with the same prompt and tools share
    /// one starter, and both still get their own answer. `cargo test live_starter --
    /// --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_starter() {
        let key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY");
        let gemini = Gemini::new(reqwest::Client::new(), key, GeminiConfig::default(), None);
        let question = |text: &str| ChatRequest {
            system: "You are Vivy, a Discord bot. Answer in one word.".into(),
            input: Input::Full(vec![Turn {
                role: Role::User,
                parts: vec![Part::Text(text.into())],
            }]),
            tools: vec![ToolDef {
                name: "get_time",
                description: "The time now.",
                parameters: json!({"type": "object", "properties": {}}),
            }],
            cache_key: "test".into(),
            job: "chat",
            answer_now: false,
        };
        let (first, _) = live_answer(&gemini, question("Name a fruit.")).await;
        let starters = gemini.starters.ids.lock().unwrap().clone();
        assert_eq!(starters.len(), 1);
        let (second, _) = live_answer(&gemini, question("Name a planet.")).await;
        assert_eq!(*gemini.starters.ids.lock().unwrap(), starters);
        println!("{first} / {second}");
        assert!(!first.to_lowercase().contains("ok") && !second.is_empty());
    }

    /// Talks to the real API: a mood check on Hydrogen, which asks for the flex tier.
    /// Needs `GEMINI_API_KEY`; run with `cargo test live_flex -- --ignored --nocapture`.
    /// Takes a few minutes.
    #[tokio::test]
    #[ignore]
    async fn live_flex() {
        let key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY");
        let mut config = GeminiConfig::default();
        config.jobs.insert(
            "mood".into(),
            Job {
                model: Some(CHEAP_MODEL.into()),
                ..Job::default()
            },
        );
        let gemini = Gemini::new(reqwest::Client::new(), key, config, None);
        let request = ChatRequest {
            system: "Answer in one word.".into(),
            input: Input::Full(vec![Turn {
                role: Role::User,
                parts: vec![Part::Text("Say hi.".into())],
            }]),
            tools: Vec::new(),
            cache_key: "test".into(),
            job: "mood",
            answer_now: false,
        };
        assert_eq!(
            gemini.request_body(&request, &[], false)["service_tier"],
            "flex"
        );
        let started = std::time::Instant::now();
        let (text, _) = live_answer(&gemini, request).await;
        println!("{text} ({} s)", started.elapsed().as_secs());
        assert!(!text.is_empty());
    }

    async fn live_answer(gemini: &Gemini, request: ChatRequest) -> (String, Done) {
        let mut events = gemini.stream(request).await.expect("the request");
        let mut text = String::new();
        while let Some(event) = events.recv().await {
            match event.expect("the stream") {
                ChatEvent::TextDelta(delta) => text.push_str(&delta),
                ChatEvent::Done(done) => return (text, done),
                _ => {}
            }
        }
        panic!("the stream ended without an answer")
    }

    /// A one-page PDF with one line of text.
    fn tiny_pdf(line: &str) -> Vec<u8> {
        let stream = format!("BT /F1 18 Tf 40 100 Td ({line}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 200] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
        }
        let xref = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        pdf.into_bytes()
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
                    Part::File(ModelFile {
                        data: vec![0xff, 0xfe],
                        ..file("song.mp3", "audio/mpeg")
                    }),
                    Part::File(file("scores.csv", "text/csv")),
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
                    {"type": "text", "text": "<file name=\"scores.csv\">\ndata\n</file>"},
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

    #[tokio::test]
    async fn pictures_from_code_go_on_the_reply() {
        let mut collector = Collector::default();
        // The shapes of a live answer that plotted a chart (2026-11-21): the same picture
        // twice, then the text.
        let chart = json!({"type": "image", "mime_type": "image/png", "data": "AQI="});
        collect(
            &mut collector,
            &[
                json!({"event_type": "step.start", "index": 0, "step": {"type": "code_execution_call", "id": "c1"}}),
                json!({"event_type": "step.start", "index": 1, "step": {"type": "model_output"}}),
                json!({"event_type": "step.delta", "index": 1, "delta": chart}),
                json!({"event_type": "step.delta", "index": 1, "delta": chart}),
                json!({"event_type": "step.start", "index": 2, "step": {"type": "model_output"}}),
                json!({"event_type": "step.delta", "index": 2, "delta": {"type": "text", "text": "Here it is."}}),
            ],
        );
        let (events, _) = collector
            .event(&json!({"event_type": "interaction.completed", "interaction": {"id": "v1_2", "status": "completed"}}))
            .unwrap();
        let [ChatEvent::Done(done)] = events.as_slice() else {
            panic!("expected Done, got {events:?}");
        };
        assert_eq!(done.files.len(), 1);
        assert_eq!(done.files[0].filename, "chart-1.png");
        assert_eq!(
            client().download_file(&done.files[0]).await.unwrap(),
            [1, 2]
        );
        // The saved steps keep a note, not the picture.
        assert_eq!(
            done.native[1]["content"],
            json!([{"type": "text", "text": "[chart-1.png, attached to the answer]"}])
        );
    }

    #[test]
    fn long_text_files_are_cut() {
        let long = ModelFile {
            data: "é".repeat(MAX_TEXT_FILE).into_bytes(),
            ..file("notes.txt", "text/plain")
        };
        let text = file_text(&long);
        assert!(text.contains("[cut here: the file is longer than 100000 bytes]"));
        assert!(text.len() < MAX_TEXT_FILE + 200);
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
