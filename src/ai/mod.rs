//! Talking to AI models.
//!
//! Every provider (OpenAI, Claude, Gemini) implements [`ChatProvider`]. The rest of the bot
//! only sees the types in this file: a conversation is a list of [`Turn`]s, and an answer
//! arrives as a stream of [`ChatEvent`]s. Discord, message splitting, the tool loop and the
//! bot's own tools live outside, so every provider reuses them.

mod api;
pub mod claude;
mod complete;
pub mod fallback;
pub mod gemini;
pub mod openai;
mod spend;
mod sse;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::util::media::ModelImage;

pub use claude::{Claude, ClaudeConfig};
pub use complete::{ToolRunner, complete};
pub use gemini::{Gemini, GeminiConfig};
pub use openai::images::{Images, PictureRequest};
pub use openai::{OpenAi, OpenAiConfig};
pub use spend::{AdminKey, Bill, Spend};

/// Who said something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One piece of a turn.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    Image(ModelImage),
    /// A file someone attached that isn't a picture or video: a PDF, a spreadsheet, a zip...
    File(ModelFile),
    /// A video as it is, for a provider that watches videos itself
    /// ([`ChatProvider::watches_video`]). Others get it as frame grids in `Image`s.
    Video(ModelFile),
    /// A YouTube video by its link, for a provider that watches videos itself.
    YouTube(String),
    /// The answer to a [`ToolCall`], sent back to the model.
    ToolResult {
        call_id: String,
        output: String,
        /// Pictures that go with the answer (see [`ToolOutput::images`]).
        images: Vec<ModelImage>,
    },
    /// An answer as the provider wrote it, round by round. A provider that has no memory of
    /// its own (Claude) sends this instead of the turn's text when it wrote the answer with
    /// the same model, so its thinking comes back exactly as it was. Others ignore it.
    Native {
        provider: String,
        model: String,
        rounds: Vec<NativeRound>,
    },
}

/// A file for the model, downloaded.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelFile {
    pub name: String,
    /// Like `application/pdf`.
    pub mime: String,
    pub data: Vec<u8>,
}

/// One round of an answer: what the model wrote (the provider's own JSON), and what the
/// bot's tools answered when it asked for them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NativeRound {
    pub output: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<ToolOutput>,
    /// Which system prompt and tools it was written with (see [`Done::setup`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
}

impl NativeRound {
    /// Reads rounds saved as JSON. `None` for anything else, like the plain list of outputs
    /// that answers were saved with before rounds existed.
    pub fn parse_list(json: &str) -> Option<Vec<NativeRound>> {
        let value: serde_json::Value = serde_json::from_str(json).ok()?;
        if !value.as_array()?.iter().all(serde_json::Value::is_object) {
            return None;
        }
        serde_json::from_value(value).ok()
    }
}

/// What one of the bot's tools answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub call_id: String,
    pub output: String,
    /// Pictures shown to the model with the text, like a profile picture. They aren't
    /// saved: when the conversation is read back later, only the text is there.
    #[serde(skip)]
    pub images: Vec<ModelImage>,
}

/// What one of the bot's tools answers: text, and sometimes pictures for the model to see.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolReply {
    pub text: String,
    pub images: Vec<ModelImage>,
}

impl From<String> for ToolReply {
    fn from(text: String) -> Self {
        ToolReply {
            text,
            images: Vec::new(),
        }
    }
}

/// One message in the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub role: Role,
    pub parts: Vec<Part>,
}

/// A function the model may call. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}

/// The model asking to run one of the bot's tools.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The arguments as the model wrote them, parsed as JSON.
    pub args: serde_json::Value,
}

/// A file the model made (for example with the code interpreter).
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedFile {
    /// Provider-specific location, passed back to [`ChatProvider::download_file`].
    pub location: String,
    pub filename: String,
}

/// What the model should continue from.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// The whole conversation, oldest first.
    Full(Vec<Turn>),
    /// Only what's new since an earlier answer, which the provider still remembers. OpenAI
    /// keeps conversations on its side, so a reply only has to send the new message.
    After {
        continuation: String,
        new: Vec<Turn>,
    },
}

pub struct ChatRequest {
    /// Never changes between requests, so the provider can cache it.
    pub system: String,
    pub input: Input,
    /// The bot's own tools, always in the same order (they're part of the cached prefix).
    pub tools: Vec<ToolDef>,
    /// Requests with the same key share a cache, for example one per channel.
    pub cache_key: String,
    /// What the request is for ("chat", "diary", ...): picks the job's settings (see
    /// [`Job`]) and is what each job costs on the status picture.
    pub job: &'static str,
}

/// The Game Boy strategist's model, unless `[ai.openai.jobs.game_plan]` says otherwise.
const STRATEGIST_MODEL: &str = "gpt-6-astra";

/// How one kind of request runs: `[ai.claude.jobs.<job>]` or `[ai.openai.jobs.<job>]` in
/// config.toml. Anything left out comes from the job's built-in default, and then from the
/// provider's own settings, so background work can run cheaper than chat answers.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Job {
    /// Another model for this job.
    pub model: Option<String>,
    /// How much it thinks ("low", "medium", ...).
    pub effort: Option<String>,
    /// Whether it gets web search and web fetch.
    pub web: Option<bool>,
    /// Whether it gets code execution (and, on Claude, the document skills).
    pub code: Option<bool>,
}

impl Job {
    /// The built-in settings of a job, before config.toml. Chat answers and chime-ins use
    /// the provider's own settings and every tool, so they share the prompt cache.
    /// Background jobs nobody waits on get only what they use:
    /// - "emoji": describing a server emoji is simple, so a cheap model at low effort
    ///   (Claude Haiku; `cheap_model` is the provider's cheap model, if it has one);
    /// - "mood": a short rewrite of her mood file, at low effort, in her own model;
    /// - "reflection", "diary": her own model and effort (they're her judgement and her
    ///   voice), with the web for looking things up, but no code;
    /// - "hunger_games": a Hunger Games round, in her own model at low effort so the next
    ///   round is ready while people read this one, with no web or code;
    /// - "game_narrate": what she says in the channel while she plays a Game Boy game,
    ///   the same (it's her voice, and short);
    /// - "game_play": a Game Boy move (the supervisor), on the cheap model at low effort,
    ///   with no web or code;
    /// - "game_plan": the Game Boy strategist (OpenAI only), on GPT-6 Astra, which shares
    ///   the free daily data-sharing tokens and generalised best to new games;
    /// - "game_review": its fresh look at its notes when she's stuck, the same model at
    ///   high effort;
    /// - "game_puzzle": the Game Boy puzzle helper, the same.
    pub fn builtin(job: &str, cheap_model: Option<&str>) -> Job {
        let off = Some(false);
        match job {
            "emoji" => Job {
                model: cheap_model.map(str::to_string),
                effort: Some("low".to_string()),
                web: off,
                code: off,
            },
            "mood" => Job {
                model: None,
                effort: Some("low".to_string()),
                web: off,
                code: off,
            },
            "hunger_games" | "game_narrate" => Job {
                model: None,
                effort: Some("low".to_string()),
                web: off,
                code: off,
            },
            "game_play" => Job {
                model: cheap_model.map(str::to_string),
                effort: Some("low".to_string()),
                web: off,
                code: off,
            },
            "game_plan" => Job {
                model: Some(STRATEGIST_MODEL.to_string()),
                effort: None,
                web: off,
                code: off,
            },
            "game_review" | "game_puzzle" => Job {
                model: Some(STRATEGIST_MODEL.to_string()),
                effort: Some("high".to_string()),
                web: off,
                code: off,
            },
            "reflection" | "diary" => Job {
                model: None,
                effort: None,
                web: Some(true),
                code: off,
            },
            _ => Job::default(),
        }
    }

    /// `self` (from config.toml) with the gaps filled from `defaults`.
    pub fn or(self, defaults: Job) -> Job {
        Job {
            model: self.model.or(defaults.model),
            effort: self.effort.or(defaults.effort),
            web: self.web.or(defaults.web),
            code: self.code.or(defaults.code),
        }
    }
}

/// The settings `job` runs with: what config.toml says in `jobs`, then the built-in
/// default (see [`Job::builtin`]). Chat answers and chime-ins always use the provider's
/// own model: their history is saved and continued with it.
pub fn job_settings(
    jobs: &std::collections::HashMap<String, Job>,
    job: &str,
    cheap_model: Option<&str>,
) -> Job {
    let mut settings = jobs
        .get(job)
        .cloned()
        .unwrap_or_default()
        .or(Job::builtin(job, cheap_model));
    if matches!(job, "chat" | "chime") {
        settings.model = None;
    }
    settings
}

/// Something the model is busy with, shown in the reply's status line.
#[derive(Debug, Clone, PartialEq)]
pub enum Activity {
    Thinking,
    SearchingWeb,
    /// A web search for this, once its query is known.
    Searching(String),
    /// Reading a page from this site.
    Reading(String),
    RunningCode,
    Writing,
}

/// One step of an answer as it streams in.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    TextDelta(String),
    Activity(Activity),
    /// A short note on what the model found and does next, written between tool calls
    /// (Claude's progress updates). Each one carries the whole note so far.
    Progress(String),
    /// The answer is complete.
    Done(Done),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Done {
    /// Pass this as [`Input::After`] to continue the conversation (OpenAI's response ID).
    pub continuation: Option<String>,
    /// Tools the model wants run before it can finish. Empty means the answer is final.
    pub tool_calls: Vec<ToolCall>,
    pub files: Vec<GeneratedFile>,
    /// The provider's raw output for this answer, kept in the history.
    pub native: serde_json::Value,
    /// A fingerprint of the system prompt and tools it was written with, for a provider
    /// whose earlier output only stays valid with the same ones (Claude's thinking).
    pub setup: Option<String>,
    /// The model the request asked for, when a job ran on another model than the
    /// provider's own (see [`Job`]), so its rounds go back to that model.
    pub model: Option<String>,
}

#[async_trait]
pub trait ChatProvider: Send + Sync {
    /// "openai", "claude", ...
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;

    /// Whether every request carries the whole conversation (Claude). History then sends
    /// earlier turns exactly as before, media included, because changing them loses the
    /// prompt cache and the model's earlier thinking.
    fn sends_full_history(&self) -> bool {
        false
    }

    /// Whether the model watches videos itself (Gemini). History then sends a posted video
    /// as [`Part::Video`] and YouTube links as [`Part::YouTube`], instead of frame grids.
    fn watches_video(&self) -> bool {
        false
    }

    /// Starts an answer. Events arrive on the returned channel until [`ChatEvent::Done`] or
    /// an error. Dropping the receiver stops the request.
    async fn stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>>;

    /// Downloads a file the model made.
    async fn download_file(&self, file: &GeneratedFile) -> anyhow::Result<Vec<u8>>;
}

/// `[ai]` in config.toml: which provider chats, and one section per provider.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// Which provider chats. Only this one's key is read from `.env`.
    pub provider: Provider,
    /// The provider chat moves to while the main one is out of credit or down. Left out,
    /// there's none.
    pub fallback: Option<Provider>,
    pub openai: OpenAiConfig,
    pub claude: ClaudeConfig,
    pub gemini: GeminiConfig,
}

const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const OPENAI_EFFORTS: [&str; 5] = ["none", "low", "medium", "high", "xhigh"];
const GEMINI_LEVELS: [&str; 4] = ["minimal", "low", "medium", "high"];
const VERBOSITIES: [&str; 3] = ["low", "medium", "high"];
const VIDEO_PROCESSING: [&str; 2] = ["static", "agentic"];

/// Settings the APIs would refuse, found at startup instead of on the first answer: an
/// `effort` or `verbosity` that isn't one of the allowed words. Each gives one warning
/// naming the key.
pub fn config_warnings(config: &AiConfig) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut check = |key: String, value: &str, allowed: &[&str]| {
        if !allowed.contains(&value) {
            let words: Vec<String> = allowed.iter().map(|word| format!("{word:?}")).collect();
            warnings.push(format!(
                "config.toml: `{key} = {value:?}` isn't one of {}, so the API will refuse those requests",
                words.join(", ")
            ));
        }
    };
    check(
        "ai.claude.effort".into(),
        &config.claude.effort,
        &CLAUDE_EFFORTS,
    );
    for (name, job) in &config.claude.jobs {
        if let Some(effort) = &job.effort {
            check(
                format!("ai.claude.jobs.{name}.effort"),
                effort,
                &CLAUDE_EFFORTS,
            );
        }
    }
    check(
        "ai.openai.reasoning_effort".into(),
        &config.openai.reasoning_effort,
        &OPENAI_EFFORTS,
    );
    for (name, job) in &config.openai.jobs {
        if let Some(effort) = &job.effort {
            check(
                format!("ai.openai.jobs.{name}.effort"),
                effort,
                &OPENAI_EFFORTS,
            );
        }
    }
    check(
        "ai.openai.verbosity".into(),
        &config.openai.verbosity,
        &VERBOSITIES,
    );
    check(
        "ai.gemini.thinking_level".into(),
        &config.gemini.thinking_level,
        &GEMINI_LEVELS,
    );
    for (name, job) in &config.gemini.jobs {
        if let Some(effort) = &job.effort {
            check(
                format!("ai.gemini.jobs.{name}.effort"),
                effort,
                &GEMINI_LEVELS,
            );
        }
    }
    check(
        "ai.gemini.video_processing".into(),
        &config.gemini.video_processing,
        &VIDEO_PROCESSING,
    );
    check(
        "ai.gemini.youtube_processing".into(),
        &config.gemini.youtube_processing,
        &VIDEO_PROCESSING,
    );
    warnings
}

/// `provider = "openai"`, `"claude"` or `"gemini"` under `[ai]`. Any other value stops the
/// bot at startup with a config error. Left out, it's OpenAI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Openai,
    Claude,
    Gemini,
}

/// "Claude" for "claude", "OpenAI" for "openai", "Gemini" for "gemini".
pub fn display_name(name: &str) -> &str {
    match name {
        "claude" => "Claude",
        "openai" => "OpenAI",
        "gemini" => "Gemini",
        other => other,
    }
}

/// The AI services the bot has, set up from `.env` and `config.toml` at startup.
#[derive(Clone, Default)]
pub struct Ai {
    /// `None` when no API key is configured; chat then explains that it's off.
    main: Option<Arc<dyn ChatProvider>>,
    /// Takes over while the main provider is out (see [`fallback`]).
    backup: Option<Arc<dyn ChatProvider>>,
    fallback: Option<Arc<fallback::State>>,
    /// What each provider that's set up cost this month: Claude, OpenAI, then Gemini.
    pub spends: Vec<Spend>,
    /// Drawing pictures with OpenAI's Image API, when `OPENAI_TOKEN` is set.
    pub images: Option<Images>,
    /// OpenAI for one-off structured answers (the Game Boy strategist), when
    /// `OPENAI_TOKEN` is set, whichever provider chats. Always OpenAI's own endpoint.
    pub openai: Option<Arc<OpenAi>>,
}

impl Ai {
    pub fn new(
        main: Option<Arc<dyn ChatProvider>>,
        backup: Option<Arc<dyn ChatProvider>>,
        spends: Vec<Spend>,
        images: Option<Images>,
    ) -> Ai {
        let (Some(inner), Some(backup)) = (main.clone(), backup) else {
            return Ai {
                main,
                spends,
                images,
                ..Ai::default()
            };
        };
        // The main provider reports every answer, so a failure can move chat over.
        let state = Arc::new(fallback::State::new(inner.name(), backup.name()));
        let main: Arc<dyn ChatProvider> = Arc::new(fallback::Watched {
            inner,
            state: state.clone(),
        });
        Ai {
            main: Some(main),
            backup: Some(backup),
            fallback: Some(state),
            spends,
            images,
            openai: None,
        }
    }

    /// The provider for a new answer: the main one, or the backup while the main one is
    /// out. Use the same provider for the whole answer, since its input is built for it.
    pub fn chat(&self) -> Option<Arc<dyn ChatProvider>> {
        match (&self.fallback, &self.backup) {
            (Some(state), Some(backup)) if state.use_backup(chrono::Utc::now()) => {
                Some(backup.clone())
            }
            _ => self.main.clone(),
        }
    }

    /// The main provider's name, and why chat moved to the backup if it did.
    pub fn switched(&self) -> Option<(&'static str, fallback::Switched)> {
        let main = self.main.as_ref()?;
        let switched = self.fallback.as_ref()?.switched()?;
        Some((main.name(), switched))
    }
}

/// The input for the next round, after the model asked for tools in `done` and they
/// answered `results`. OpenAI continues its stored response, so only the results are sent.
/// A provider without a continuation gets the whole conversation again, with its own
/// output of this round added before the results.
pub fn next_input(
    provider: &dyn ChatProvider,
    mut input: Input,
    done: &Done,
    results: &[ToolOutput],
) -> Input {
    let answers = Turn {
        role: Role::User,
        parts: results
            .iter()
            .map(|r| Part::ToolResult {
                call_id: r.call_id.clone(),
                output: r.output.clone(),
                images: r.images.clone(),
            })
            .collect(),
    };
    if let Some(continuation) = &done.continuation {
        return Input::After {
            continuation: continuation.clone(),
            new: vec![answers],
        };
    }
    let turns = match &mut input {
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
    turns.push(answers);
    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fake;

    #[async_trait]
    impl ChatProvider for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn model(&self) -> &str {
            "m1"
        }
        async fn stream(
            &self,
            _request: ChatRequest,
        ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>> {
            unimplemented!()
        }
        async fn download_file(&self, _file: &GeneratedFile) -> anyhow::Result<Vec<u8>> {
            unimplemented!()
        }
    }

    fn question() -> Turn {
        Turn {
            role: Role::User,
            parts: vec![Part::Text("hi".into())],
        }
    }

    fn results() -> Vec<ToolOutput> {
        vec![ToolOutput {
            call_id: "c1".into(),
            output: "12:00".into(),
            images: Vec::new(),
        }]
    }

    #[test]
    fn next_input_continues_a_stored_response() {
        let done = Done {
            continuation: Some("resp_1".into()),
            ..Done::default()
        };
        let next = next_input(&Fake, Input::Full(vec![question()]), &done, &results());
        assert_eq!(
            next,
            Input::After {
                continuation: "resp_1".into(),
                new: vec![Turn {
                    role: Role::User,
                    parts: vec![Part::ToolResult {
                        call_id: "c1".into(),
                        output: "12:00".into(),
                        images: Vec::new(),
                    }],
                }],
            }
        );
    }

    #[test]
    fn next_input_resends_everything_without_a_continuation() {
        let done = Done {
            native: json!([{"type": "tool_use", "id": "c1"}]),
            ..Done::default()
        };
        let Input::Full(turns) =
            next_input(&Fake, Input::Full(vec![question()]), &done, &results())
        else {
            panic!("expected the whole conversation");
        };
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0], question());
        assert_eq!(
            turns[1].parts,
            [Part::Native {
                provider: "fake".into(),
                model: "m1".into(),
                rounds: vec![NativeRound {
                    output: json!([{"type": "tool_use", "id": "c1"}]),
                    ..NativeRound::default()
                }],
            }]
        );
        assert_eq!(turns[2].role, Role::User);
    }

    #[test]
    fn rounds_round_trip_as_json() {
        let rounds = vec![NativeRound {
            output: json!([{"type": "text", "text": "hi"}]),
            results: results(),
            setup: Some("0123".into()),
        }];
        let text = serde_json::to_string(&rounds).unwrap();
        assert_eq!(NativeRound::parse_list(&text), Some(rounds));
        // Answers saved before rounds were stored don't parse, and are skipped.
        assert_eq!(NativeRound::parse_list("[[{\"type\":\"message\"}]]"), None);
        assert_eq!(NativeRound::parse_list("not json"), None);
    }
}
