# The `ai` module

`src/ai/` is how the bot talks to AI models. Every provider (Claude, OpenAI and Gemini) implements one trait, `ChatProvider`. The rest of the bot only sees the types in `src/ai/mod.rs`: a conversation is a list of `Turn`s, and an answer arrives as a stream of `ChatEvent`s.

What stays outside the providers, so every provider reuses it: Discord, message splitting, media extraction, the tool loop, and the bot's own tools. The Discord side of chat is in [chat](features/chat.md); the core (config, database, `BotCtx`, utilities) is in [architecture](architecture.md).

All three providers are called over plain HTTP with `reqwest` and `serde_json`, with a small parser for the streamed answers in `sse.rs`. There is no client crate: the official SDKs aren't in Rust, and community crates tend to lag behind new API features.

## Files

| File | What it holds |
|---|---|
| `mod.rs` | The shared types (`Turn`, `Part`, `Input`, `ChatRequest`, `ChatEvent`, `Done`), the `ChatProvider` trait, `AiConfig` and `Provider` (the `[ai]` config), `Ai` (the providers the bot has), and `next_input` |
| `complete.rs` | `complete`: running a request to the end without showing it anywhere, for background jobs |
| `sse.rs` | `SseParser`: splits a streamed body into server-sent events |
| `api.rs` | `send_retrying` (trying again when an API is busy, honouring `Retry-After`) and `error_message` (the message of an error answer), shared by Claude, OpenAI and the Image API |
| `fallback.rs` | `ApiError`, telling outages from bad requests, `send` (one retry without a connection), `State` and `Watched` (moving chat to the backup provider) |
| `spend.rs` | `Spend`: what one provider costs this month, budget warnings, reading the real bill through the `Bill` trait |
| `openai/mod.rs` | The OpenAI provider (Responses API) and `OpenAiConfig` |
| `openai/price.rs` | List prices per model, and what one response or picture cost |
| `openai/billing.rs` | Reading OpenAI's Costs API with an Admin key |
| `openai/images.rs` | `Images`: drawing pictures with the Image API (GPT Image 2.5) |
| `claude/mod.rs` | The Claude provider (Messages API), `ClaudeConfig`, `ContentKey`, pause and retry handling |
| `claude/messages.rs` | Turns as Claude's messages, the tool list and the system block |
| `claude/collect.rs` | `Collector`: rebuilds Claude's streamed content blocks and turns them into `ChatEvent`s |
| `claude/uploads.rs` | Files API IDs kept in the `claude_uploads` table |
| `claude/price.rs` | List prices per model, and what one response cost |
| `claude/billing.rs` | Reading Anthropic's Cost API with an Admin key |
| `gemini/mod.rs` | The Gemini provider (Interactions API), `GeminiConfig`, and `Collector`, which rebuilds the streamed steps |
| `gemini/files.rs` | `Files`: uploading videos and PDFs with Gemini's Files API |
| `gemini/price.rs` | List prices, and what one interaction cost |

## The types

A `Turn` is one message in the conversation: a `Role` (`User` or `Assistant`) and a list of `Part`s.

```rust
pub enum Part {
    Text(String),
    Image(ModelImage),          // from util::media: a MIME type and the bytes
    File(ModelFile),            // any other attachment: name, MIME type, bytes
    Video(ModelFile),           // a video as it is, only for a provider that watches videos (Gemini)
    YouTube(String),            // a YouTube link, the same
    ToolResult { call_id: String, output: String },
    Native { provider: String, model: String, rounds: Vec<NativeRound> },
}
```

`Native` is an answer as the provider wrote it, round by round. A `NativeRound` is the provider's own JSON for one round of the tool loop (`output`) plus what the bot's tools answered (`results`). See "History and native replay" below.

A request says what the model should continue from:

```rust
pub enum Input {
    Full(Vec<Turn>),                                 // the whole conversation, oldest first
    After { continuation: String, new: Vec<Turn> },  // only what's new since an earlier answer
}

pub struct ChatRequest {
    pub system: String,       // never changes between requests, so it can be cached
    pub input: Input,
    pub tools: Vec<ToolDef>,  // the bot's own tools, always in the same order
    pub cache_key: String,    // requests with the same key share a cache ("discord:<channel>")
    pub job: &'static str,    // "chat", "chime", "emoji", "reflection", "mood", "diary", "hunger_games"; picks the job's settings
}
```

A `ToolDef` is a name, a description and a JSON Schema for the arguments. A `ToolCall` is the model asking to run one (an ID, the name, and the arguments parsed as JSON; arguments that aren't valid JSON arrive as a JSON string, so the tool can report the mistake).

The answer streams in as events:

```rust
pub enum ChatEvent {
    TextDelta(String),
    Activity(Activity),  // Thinking, SearchingWeb, RunningCode, Writing (for the status line)
    Progress(String),    // Claude's note between tool calls, the whole note so far
    Done(Done),
}

pub struct Done {
    pub continuation: Option<String>,  // OpenAI's response ID; None for Claude
    pub tool_calls: Vec<ToolCall>,     // empty means the answer is final
    pub files: Vec<GeneratedFile>,     // files the model's code made
    pub native: serde_json::Value,     // the provider's raw output for this round
}
```

## The `ChatProvider` trait

From `src/ai/mod.rs` (doc comments trimmed):

```rust
#[async_trait]
pub trait ChatProvider: Send + Sync {
    fn name(&self) -> &'static str;   // "openai", "claude"
    fn model(&self) -> &str;

    fn sends_full_history(&self) -> bool {
        false
    }

    fn watches_video(&self) -> bool {
        false
    }

    async fn stream(
        &self,
        request: ChatRequest,
    ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>>;

    async fn download_file(&self, file: &GeneratedFile) -> anyhow::Result<Vec<u8>>;
}
```

Some Rust terms here:

- `#[async_trait]` (from the `async-trait` crate) lets a trait have `async fn` methods that can be called through `dyn ChatProvider`.
- `Send + Sync` means a provider can be shared between threads and tasks. The bot keeps providers as `Arc<dyn ChatProvider>`: a shared pointer to "some type that implements the trait".
- `stream` returns the receiving end of a `tokio::sync::mpsc` channel. The provider spawns a task that reads the HTTP stream and sends events into the channel until `Done` or an error. Dropping the receiver stops the request: the provider's task notices that sending failed and returns.
- `sends_full_history` and `watches_video` have a default body, so a provider only overrides them when it needs to. Claude returns `true`: every request carries the whole conversation, so history sends every turn's media and never changes earlier turns (see [chat](features/chat.md)). Gemini returns `true` from `watches_video`: history then sends posted videos as `Part::Video` (the file itself) and YouTube links as `Part::YouTube`, instead of frame grids. Claude and OpenAI never get those two parts and skip them.

## The tool loop

The tool loop lives outside the trait, so every provider gets the same tools. There are two copies of it:

- `answer::stream_answer` in `src/features/chat/answer.rs`, for answers in Discord: it also redraws the reply each second, shows what each tool is doing in the status line (`Feature::tool_status`), and stops on ❌. At most 8 rounds.
- `ai::complete` in `complete.rs`, for work nobody watches: chime-ins and follow-ups (`chat/chime.rs`), emoji descriptions (`chat/emoji.rs`), memory's reflection, mood and diary jobs, and Hunger Games rounds (`games/hunger_games/writer.rs`). The caller passes a `ToolRunner` (a trait with one method, `run(&ToolCall) -> String`) and a round limit. It returns `Completed`: the last round's text, the continuation, and every round's `NativeRound`.

Both work the same way: send the request, read events until `Done`, and if `Done` lists tool calls, run them, then build the next input with `ai::next_input` and send again.

`next_input` (in `mod.rs`) is where the providers differ:

- With a `continuation` (OpenAI), the next input is `Input::After` with only the tool results. OpenAI remembers its own output.
- Without one (Claude), it is the whole conversation again, plus an assistant turn holding this round's `Part::Native` output, plus a user turn with the tool results. Claude needs its own tool calls (and the thinking before them) back, exactly as written.

A tool result can carry pictures as well as text (`ToolOutput::images`, from a `ToolReply`), like the profile picture from `look_at_profile_picture`. Claude gets them as `image` blocks inside the `tool_result`; OpenAI gets a `function_call_output` whose output is a list of `input_text` and `input_image`. The pictures aren't saved with the answer's rounds, so when a conversation is read back later only the text is there.

## Jobs

Every request names its job, and each job runs with its own settings (`Job` in `mod.rs`): a model, an effort, and whether it gets the web (web search and fetch) and code (code execution, and on Claude the skills). `job_settings` takes them from `[ai.<provider>.jobs.<job>]` in config.toml, then from `Job::builtin`, then from the provider's own settings:

| Job | Model | Effort | Web | Code | Why |
|---|---|---|---|---|---|
| `chat`, `chime` | main | main | yes | yes | Her answers and lines. Chime-ins send exactly what answers send (every tool; `chat/chime.rs` refuses the ones she can't use), so they read the system prompt and tools from the same cache. Neither can change model: their history is saved and continued with the main one. |
| `emoji` | Claude Haiku (OpenAI, Gemini: main) | low | no | no | Describing a picture in one line. |
| `mood` | main | low | no | no | A short rewrite of her mood file. |
| `reflection`, `diary` | main | main | yes | no | Her judgement and her voice, so her own model; the web to look things up. |
| `hunger_games` | main | low | no | no | A Hunger Games round as JSON: her voice, but fast, so the next round is ready while people read. |

On Claude, a request whose job has no bot tools (emoji, mood, hunger_games) is one round, so only its system prompt is cached, not the conversation. Chat and chime-ins cache the system prompt and tools for an hour (`cache_prompt_for_an_hour`); everything else for 5 minutes.

### Keeping the chat cache warm

`claude/keep_warm.rs`. The hour counts from the start of the last request that read or wrote the cached prompt and tools, and a read starts it over for free. So `keep_cache_warm_minutes` (58) after the last use, a ping sends the last chat request's model, system prompt, tools, thinking, effort and skills with one `"."` message, `max_tokens: 0` and no streaming: Claude reads the cache and stops, billing only the read (about $0.003 for 14k tokens, against $0.11 to write them again). Every chat and chime-in request with the same prompt and tools (`prefix_key`) counts as a use, so while people talk no ping goes out. A chat request with a different prompt or tool list (an update) moves the pings to it; a chime-in with one doesn't count. Pings stop `keep_cache_warm_hours` (36) after the last chat, since 40 pings cost as much as one write, and start again with the next chat (after a restart too: a ping copies the last chat request). Pings are spent as the job `keep_warm`. Each day in `claude_keep_warm` counts pings and their cost, cold pings (a ping that had to write the cache, so the wait is too close to the hour; also logged as a warning), and saves (a chat request over an hour after the last chat that found the cache warm thanks to a ping, with the write it saved, `price::hour_write_saved`). The status picture shows the month's savings minus the pings' cost next to the cache hit rate, once a ping has run.

## Claude

`claude/mod.rs`. One `POST /v1/messages` with `"stream": true` per round. Claude keeps nothing between requests, so each one carries the whole conversation.

**The request body** (`Claude::request_body`):

The job's settings (see [Jobs](#jobs)) pick the model, the effort and the built-in tools; `Claude::run_for` works them out into a `Run`.

- `system`: one text block with `cache_control`, marking the end of the part every request shares. For chat and chime-ins it says `"ttl": "1h"`.
- `tools`: Claude's built-in `web_search` and `web_fetch` when the job has the web (with `response_inclusion: "excluded"`, so search results and pages her code already filtered aren't sent back, stored in the history, or re-sent later; web search also gets `max_uses` from `web_search_max_uses`, 5 by default, so one request makes at most 5 searches at $0.01 each, and past that a search returns a `max_uses_exceeded` error and she answers with what she found), `code_execution` when it has code (and `code_execution` is on), then the bot's tools with `eager_input_streaming` (`messages::tools`). The bot's `memory` tool is sent as a normal tool, not as Claude's own memory tool, because that one comes with Anthropic's instruction to log task progress in memory.
- `thinking`: adaptive (it can't be turned off; `effort` in `output_config` sets how much), with `block_binding.prefix_mismatch_behavior = "drop_block"`: if an earlier turn ever changes, Claude drops the thinking after it instead of failing the request. The response lists what it left out, and `collect.rs` logs it. When Claude says what changed (the unpublished `anthropic-thinking-prefix-mismatch` header), `claude/mod.rs` logs that too.
- `cache_control` at the top level, so the conversation up to its newest message is cached for the next request (left out when the job has no bot tools, since it's one round).
- `fallbacks: "default"` when `fallbacks` is on (not for Haiku, which has no model to fall back to): if the model declines, the API retries on another Claude model. This is Anthropic's server-side model fallback, not the provider fallback below.
- `container`: Anthropic's skills (`xlsx`, `docx`, `pptx`, `pdf` by default) and the ID of the code execution container this `cache_key` used last, so files stay around between answers in a channel.

When the request has bot tools (chat, chime-ins, reflection, diary), `thinking.display` is `"updates"`: Claude's short progress notes between tool calls come back as text in their `thinking` blocks, and `collect.rs` sends them on as `ChatEvent::Progress` for chat's status line. The rest of the thinking stays empty, as with the default `"omitted"`. Jobs without tools (mood, emoji) leave `display` out.

Beta headers: `thinking-binding-controls-2026-08-01`, `thinking-display-updates-2026-08-18`, and `server-side-fallback-2026-07-01` when `fallbacks` is on.

**Building the messages** (`messages::to_messages`): text, pictures and tool results become content blocks. An assistant turn with a `Part::Native` written by `"claude"` with the same model is replaced by its rounds, exactly as stored. Any other assistant turn sends only its text. A file becomes a note naming it, a `document` block for PDFs (so Claude reads them), and a `container_upload` block when code execution is on (so code can open it). If a chain starts with one of the bot's messages, a short user message is put first, since Claude requires the user to speak first.

**Uploads**: pictures and files go through the Files API: uploaded once, then sent by `file_id` (see "Uploads" below). Up to 4 are uploaded at once. Pictures over 2000 pixels a side or 5 MB are made smaller first (`shrink_image`), because Claude rejects big ones once a request has more than 20 pictures. A picture that fails to upload is sent inline as base64.

**Retries**: 429, 500, 502, 503 and 529 are retried twice, after 2 and 6 seconds, or after the wait the answer's `Retry-After` header asks for when that is longer (up to 60 seconds; see `api::send_retrying`). If the request fails because an upload is gone, those uploads are forgotten and sent again. If it fails because the code execution container is gone, the request is sent without a container ID.

**Reading the stream** (`collect.rs`): the answer arrives as content blocks (thinking, text, tool calls, built-in tools' results), each opened by `content_block_start`, filled by deltas and closed by `content_block_stop`. `Collector` rebuilds the blocks exactly as Claude wrote them, signatures included, because they go back into the conversation unchanged. On the way it sends `TextDelta`s, `Activity` events, and a `Progress` event whenever a thinking block's text grows (only progress notes have text). Tool arguments stream as text and are parsed at the end of the block. A block that comes with its whole input in `content_block_start` and no deltas keeps that input: an input changed on the way back makes Claude leave out all thinking after it. An `error` event mid-stream becomes an `ApiError` (529 for `overloaded_error`, 500 for `api_error`), so the fallback reads it the same way as an HTTP error.

**When a response ends** (`read_answer`):

- `pause_turn` (a long run of built-in tools): the blocks so far are sent back as an assistant message to continue, at most 5 times.
- `refusal`: the answer fails with the category Claude gave.
- `max_tokens`: unfinished tool calls are removed, since the conversation can't hold a call without its result.
- After a server-side fallback (a `fallback` block), only text and finished built-in tool calls before the switch go back into the conversation.
- Files the code saved come from the code execution result blocks; their names are looked up with `GET /v1/files/{id}`, and `download_file` fetches `/v1/files/{id}/content`.

What each response cost is added to `Spend` (see below), even when it was stopped or failed partway.

## OpenAI

`openai/mod.rs`. One `POST /v1/responses` with `"stream": true` per round. `OPENAI_BASE` in `.env` sets another base URL for an OpenAI-compatible endpoint; `/v1` is added when it's missing.

- The body has `instructions` (the system prompt), `input`, the built-in `web_search` and `code_interpreter` tools (as the job allows, see [Jobs](#jobs)), then the bot's tools as `function` tools, plus the job's model and `reasoning.effort`, `text.verbosity`, `service_tier` (chat answers only; other jobs use the account's default), `"store": true`, `"truncation": "auto"` and `prompt_cache_key`. OpenAI caches automatically. Answers and chime-ins all use the key `vivy:chat`, since every channel starts with the same instructions and tools, so a quiet channel reads what a busy one cached; other jobs use their `cache_key`.
- OpenAI stores every response, so `Done.continuation` is the response ID. The next request sends `previous_response_id` and only what's new. That already carries the earlier reasoning, so OpenAI ignores `Part::Native`.
- Pictures are sent inline as base64 data URLs.
- Attached files are uploaded first (`POST /v1/files`, kept 30 days so stored responses that point at them still work). PDFs and Office files are also given to the model to read as `input_file` (at most 50 MB per request). Types the code interpreter takes go into its container. Other files get a note saying they're attached but can't be opened.
- Files the code interpreter made come from `container_file_citation` annotations; `download_file` reads them from `/v1/containers/{container}/files/{file}/content`.
- 429, 500, 502 and 503 are retried twice, after 2 and 6 seconds (or a longer `Retry-After`/`retry-after-ms`, up to 60 seconds). Stream errors become an `ApiError` (500 for `server_error`, 429 for rate limits and `insufficient_quota`, 400 otherwise).
- When the stream ends with `response.completed` (or `response.incomplete`), `add_spend` prices the response's `usage` with `price::chat_cost` and adds it to OpenAI's `Spend` (see below).

## Gemini

`gemini/mod.rs`. One `POST https://generativelanguage.googleapis.com/v1beta/interactions` with `"stream": true` per round, with the key in the `x-goog-api-key` header. This is Google's Interactions API, the newer one that `generateContent` is being moved to. The model is Gemini 4 Argon (`gemini-4-argon`, 1M tokens of context, 64k out).

- The body has `system_instruction` (the system prompt), `input`, the job's `model`, `tools`, `generation_config` (`thinking_level` from the job's effort or `thinking_level`, and `thinking_summaries`) and `"store": true`. Gemini 4 ignores `temperature`, so it's left out. Gemini caches implicitly (there is no cache key to send).
- Built-in tools: `google_search` and `url_context` when the job has the web (search, then read the pages it found or the links people post), `code_execution` when it has code. Then the bot's tools as `function` tools.
- Gemini stores every interaction (55 days on a paid key), so like OpenAI `Done.continuation` is the interaction's ID, and the next request sends `previous_interaction_id` with only what's new. Gemini keeps its own thinking (and its thought signatures) on its side, so `Part::Native` is ignored.
- `input` is a list of steps: a person's turn is `user_input`, an earlier answer is `model_output` with its text (when the conversation started on another provider), and each tool result is a `function_result` with the call's `call_id` and `name`. The tool loop only keeps the call ID, so the provider remembers each call's name in `call_names`.
- Pictures go inline as `{"type": "image", "data": <base64>, "mime_type": ...}`.
- Videos people post are uploaded with the Files API (`files.rs`: a resumable upload, then the file is checked every 2 seconds until Google has processed it) and sent as `{"type": "video", "uri": ..., "processing": video_processing}`. The model watches the whole video with its sound, not a few frames. Google deletes uploads after 48 hours; uploads from the last 47 are reused by content (the same `ContentKey` hash as Claude's uploads), in memory only.
- YouTube links in people's messages (`youtube.com/watch`, `youtu.be`, shorts, live) are sent as videos by their link, `{"type": "video", "uri": "https://www.youtube.com/watch?v=...", "processing": youtube_processing}`: only public videos work, at most 10 per request, from the newest 4 messages that link any (`history::youtube_videos`). YouTube's preview thumbnail is still sent as a picture too.
- `processing` is `"static"` (frames once a second with the sound, all at once: best for short clips, the default for posted videos) or `"agentic"` (the model looks through the video and loads the parts it needs: up to 88% fewer tokens on long videos, but slower; the default for YouTube, since a long video watched statically could fill the context window).
- PDFs are uploaded the same way and sent as `document`s. Other files get a note saying they can't be opened; small text files are already in the message text.
- The stream sends `interaction.created`, then each step as `step.start`, `step.delta`s and `step.stop`, then `interaction.completed` with the `usage`. `Collector` rebuilds the steps: `text` deltas become `TextDelta`s, `thought_summary` deltas become `Progress` notes for the status line (only for requests with the bot's tools, when `thought_summaries` is on), function call arguments arrive as `arguments_delta` text and are parsed at `step.stop`, and Google Search and URL context steps show what they search for or read. Unknown event and step types are skipped, as Google asks.
- 429, 500, 502, 503 and 504 are retried twice, after 2 and 6 seconds. Errors become an `ApiError` with provider `"gemini"`; an `error` event mid-stream gets the status its code has as an HTTP answer (`gateway_timeout` 504, `internal` 500, `resource_exhausted` 429...), so the fallback reads it the same way. Gemini's "You exceeded your current quota" is OpenAI's wording, so out of credit is recognised already.
- Code execution makes no files to download, so `download_file` always fails.

### Pictures

`openai/images.rs`. `Ai::images` is set whenever `OPENAI_TOKEN` is in `.env`, whichever provider chats, so any feature can draw (the [drawing](features/drawing.md) feature's chat tool does). `Images::draw` takes a `PictureRequest` (model, prompt, pictures to start from, size, quality, format, moderation):

- No pictures: `POST https://api.openai.com/v1/images/generations` (JSON).
- With pictures: `POST /v1/images/edits`, also JSON, with up to 16 pictures as `{"image_url": "data:..."}` in `images`.
- The answer holds the picture in `data[0].b64_json` and the tokens in `usage`, which `price::image_cost` prices; the cost goes to OpenAI's `Spend` as the request's `job`: `"drawing"`, or `"game_art"` for pictures the games ask for, so they get their own line.
- 429, 500, 502 and 503 are retried twice, after 5 and 15 seconds (or a longer `Retry-After`, up to 60 seconds), and no connection is retried once, like chat (`fallback::send`). A 400 (the safety system refused, or a bad size) becomes a `user_error`, so the model reads why and the log channel isn't bothered.

GPT Image 2.5 comes as `gpt-image-2.5-flare` (fast, everyday pictures) and `gpt-image-2.5-sunburst` (the most capable, best at precise edits). Both cost the same per token: text in $5, pictures in $8, pictures out $30 per million (checked 2026-10-11). Quality goes `low`, `medium`, `high`, `xhigh`, `max`.

## History and native replay

Chat stores conversations itself, in `chat_turns` (see [chat](features/chat.md)), so they don't depend on any provider's memory. `history::build_input` picks:

- `Input::After` when an answer in the chain was written by this provider and model and has a `continuation_id` (OpenAI);
- otherwise `Input::Full` with the whole chain.

So switching providers in the middle of a conversation works: the new provider gets the neutral text of the earlier answers.

For answers, `native_json` keeps the provider's raw output round by round, with the tools' results. When the history goes back to Claude with the same model, the rounds are sent unchanged instead of the turn's text. That keeps the model's earlier thinking (Claude's rule is that thinking blocks only stay valid while everything before them is unchanged), and keeps the request prefix byte-identical so the prompt cache still hits. Any other provider or model gets only the text, because thinking blocks are tied to the model that wrote them. `NativeRound::parse_list` returns `None` for answers saved before rounds existed, which are then sent as text.

Claude's thinking is also tied to the system prompt and tools it was written with, so a new version of the bot that changes either would make Claude drop the thinking of every answer written before, and all thinking after it, on every later request in that conversation. To avoid that, each round saves `setup`, a fingerprint of the system prompt and tools it was sent with (`messages::setup`). `to_messages` takes the thinking out of every answer up to the last one written with a different setup; removing thinking from the start of a conversation is allowed, so newer answers keep theirs. A conversation that spans an update loses its older thinking, and Claude doesn't have to drop anything, so the log line "Claude left out earlier thinking … (prefix_binding_mismatch)" means something else changed an earlier turn. Answers saved before `setup` existed have none and are sent as before.

Thinking is never shown in Discord, apart from Claude's progress notes between tool calls (see [chat's status line](features/chat.md#the-status-line)).

## Uploads

`claude/uploads.rs`. If the same picture got a new `file_id` after a restart, the request would change partway and the prompt cache would miss from there on. So upload IDs are saved in `claude_uploads`, keyed by a `ContentKey`: a 128-bit FNV-1a hash of the bytes plus their length (`claude/mod.rs`). FNV-1a is used instead of Rust's `DefaultHasher` because the saved keys outlive the program, and `DefaultHasher` may change between Rust releases.

- Uploads ask Anthropic to delete them after 30 days.
- An ID is not reused in its last day, so it can't expire halfway through an answer.
- `Uploads` keeps the IDs it has seen in memory too, so most lookups don't touch the database.
- One that Claude reports as gone is dropped (`Uploads::forget`) and uploaded again.

The upload is found by the original's bytes, even when a smaller copy was uploaded, so a big picture is shrunk once.

## Fallback

`fallback.rs`. `provider` under `[ai]` names the main provider and `fallback` the backup, either way round. `Ai::new` wraps the main provider in `Watched`, which passes every event on and reports how each answer ends to a shared `State`.

Both providers return an `ApiError` (provider, status, message) for error answers, and `ApiError::outage` tells an outage from a bad request:

- **Out of credit**: Claude's "credit balance", a workspace spending limit ("API usage limits"), OpenAI's "quota" or "billing" wording. A 429 counts only with OpenAI's out-of-quota wording, since rate-limit messages can link to the billing page too. Chat moves to the backup for a day.
- **Down**: 500, 502, 503, 504 or 529 after the provider's retries, an overloaded error mid-stream, no connection twice in a row (`fallback::send` waits 2 seconds and tries once more), or a stream that sent nothing for 5 minutes (the HTTP client's read timeout, set in `main.rs`). Chat moves to the backup for an hour.
- Anything else, like a rate limit or a bad request, changes nothing.

Callers take `ctx.ai.chat()` once per answer and keep that provider for the whole answer, since its input is built for it. When the retry time comes, the next answer tries the main provider again; the first one that works moves chat back (`State::worked`), and a failure keeps it on the backup with a new retry time. Both switches are logged as warnings, so they reach the log channel. `Ai::switched` tells the control panel's status message about it, with when the main provider is tried again.

## Spend and billing

`spend.rs`, `claude/price.rs`, `claude/billing.rs`, `openai/price.rs`, `openai/billing.rs`. There is one `Spend` per provider, each with its own budget, in `Ai::spends`: Claude's when Claude is the main provider or the fallback, OpenAI's when OpenAI is one of them or `OPENAI_TOKEN` is set for pictures. The control panel shows each one.

- After each Claude response, `add_spend` (in `claude/mod.rs`) prices its usage (input, cache writes and reads, output, web searches) with `price::cost`, from a table of list prices per model ID prefix. Cache writes cost 1.25 times the input price for 5 minutes and 2 times for an hour (`usage.cache_creation.ephemeral_1h_input_tokens`); web searches $10 per 1000. An unknown model is priced like Opus 5.5.
- After each OpenAI response, `add_spend` (in `openai/mod.rs`) prices it with `price::chat_cost`: uncached input, cached input and output tokens from a table per model ID prefix, twice that on the `fast` service tier, plus $10 per 1000 web searches. Left out, so the estimate runs a little low: long-prompt prices, cache writes, and code interpreter containers ($0.03 a session). An unknown model is priced like gpt-6.1-sol. Pictures are priced by `price::image_cost`.
- After each Gemini interaction, `add_spend` (in `gemini/mod.rs`) prices its `usage` with `price::cost`: `total_input_tokens` plus `total_tool_use_tokens` (pages URL context read) as input, less `total_cached_tokens` at the cache price, and `total_output_tokens` plus `total_thought_tokens` as output, plus $14 per 1000 Google searches. Gemini 4 Argon costs $2 in, $0.10 cached, $10 out per million until 2026-11-30 and twice that from 2026-12-01 (checked 2026-10-14); the price table switches on that date. The first 5000 searches a month are free, which the estimate ignores, so it runs a little high. Google has no Admin API for the bill, so there's no `billed_spend` for Gemini.
- `Spend::add` adds it to `ai_spend` (the month's total) and to `ai_spend_jobs` (per job). Months are calendar months in UTC.
- At 80% and 100% of `monthly_budget` under `[ai.claude]` / `[ai.openai]` there is one warning each per month (the `warned` column), which reaches the log channel. A budget of 0 turns them off.
- With an Admin key in `.env` and `billed_spend` on, `main.rs` spawns `Spend::follow_bill` with that company's `Bill` on the shutdown tracker, which reads the bill now and every hour (until the bot stops) and replaces the month's total with it:
  - `ANTHROPIC_ADMIN_KEY`: Anthropic's Cost API (`claude/billing.rs`: `GET /v1/organizations/cost_report`, the whole organization, daily buckets, amounts in cents as strings).
  - `OPENAI_ADMIN_KEY`: OpenAI's Costs API (`openai/billing.rs`: `GET /v1/organization/costs` with `start_time` at the month's start, the whole organization, daily buckets only, `amount.value` in USD). An ordinary API key can't read it. Daily buckets mean today's spend can show up late, so the estimate fills the gap until the next read. Answers in between add their estimate on top until the next read. A failing read is warned about once, then logged quietly.
- `Spend::this_month` gives the control panel the month's total, the cost per job (`show_job_costs`) and the cache hit rate (`show_cache_hits`: input tokens read from the cache over all input tokens). `Spend::admin_key` says whether the bill is being read.

Neither Admin API creates API keys, so the cost per job comes from the bot's own counts rather than one key per job. OpenAI has no API for the prepaid credit balance, only for what was spent.

## Tables

| Table | Owner | Columns | What for |
|---|---|---|---|
| `ai_spend` | core (`CORE_MIGRATIONS` 2 in `src/core/db.rs`) | month (`2026-10`), provider, USD, `warned` (highest budget share already warned about, in percent) | The month's total, for the budget warnings and the status message |
| `ai_spend_jobs` | core (`CORE_MIGRATIONS` 3) | month, provider, job, USD, uncached input tokens, cache read tokens, cache write tokens | Cost per job and cache hit rate |
| `claude_uploads` | `claude` (`claude::UPLOAD_MIGRATIONS`, run from `main.rs` with every owner's migrations) | content key (hash and length as text), Files API ID, expiry (unix seconds) | Reusing uploads across restarts |
| `claude_keep_warm` | `claude_keep_warm` (`claude::KEEP_WARM_MIGRATIONS`) | day (UTC, `2026-10-11`), pings, ping USD, cold pings, saves, saved USD | What keeping the chat cache warm cost and saved |

Background loops: the timers `"Anthropic bill"` and `"OpenAI bill"` (`"hourly"`), from `Spend::follow_bill`, each only with its admin key; `"Chat cache warmth"` (`"before the cache expires"`), from `KeepWarm::run`, when Claude chats and the pings are on.

## Configuration

`[ai]` in `config.toml` is read into `AiConfig` (`mod.rs`): `provider`, `fallback`, `[ai.claude]` (`ClaudeConfig`), `[ai.openai]` (`OpenAiConfig`) and `[ai.gemini]` (`GeminiConfig`). `config.example.toml` explains each key. Keys come from `.env`: `ANTHROPIC_API_KEY`, `OPENAI_TOKEN` (and optional `OPENAI_BASE`), `GEMINI_API_KEY`, and the optional `ANTHROPIC_ADMIN_KEY` and `OPENAI_ADMIN_KEY`.

`ai_from_env` in `src/main.rs` builds the providers at startup. Only the main provider's key is read, plus the fallback's when one is set (and different from the main one). Without the main key, `ctx.ai.chat()` returns `None` and chat says it is turned off; a missing fallback key only means there is no fallback. All providers share one `reqwest::Client` with a 10-second connect timeout and a 5-minute read timeout.

## Adding a provider

Gemini was added with these steps, so `src/ai/gemini/` is a worked example.

1. **Write the provider.** Add `src/ai/<name>/mod.rs` with a `GeminiConfig` (`#[derive(Deserialize)]`, `#[serde(default)]`, and a `Default` impl) and a `Gemini` struct that implements `ChatProvider`:
   - `name` returns `"gemini"`; `model` returns the configured model.
   - `stream` turns `request.input` into Gemini's format, posts it through `fallback::send` (so a dropped connection is retried once), and spawns a task that reads the stream with `SseParser` and sends `TextDelta`, `Activity` and finally one `Done` into an `mpsc` channel. Stop reading when sending fails.
   - Put tool calls in `Done.tool_calls` and the raw output in `Done.native`. If Gemini can continue a stored conversation, put its ID in `Done.continuation`; otherwise leave it `None` and override `sends_full_history` to return `true`.
   - Handle `Part::ToolResult` (results go back to the model) and `Part::Native` (replay your own rounds when `provider == "gemini"` and the model matches, or skip it and use the turn's text).
   - Return errors as `fallback::ApiError { provider: "gemini", status, message }`, so the fallback can tell outages from bad requests. If Gemini's out-of-credit wording differs, add it to `ApiError::outage`.
   - `download_file` fetches a `GeneratedFile` by the `location` you gave it.
2. **Register it in `src/ai/mod.rs`:** `pub mod gemini;`, re-export the types with `pub use`, add `Gemini` to the `Provider` enum (`#[serde(rename_all = "lowercase")]` makes it `"gemini"` in the config), add `pub gemini: GeminiConfig` to `AiConfig`, and add `"gemini" => "Gemini"` to `display_name`.
3. **Build it in `ai_from_env` (`src/main.rs`):** add the key's name (like `GEMINI_API_KEY`) to the first `match provider`, and the constructor to the second. Rust checks that every `match` on `Provider` covers the new variant, so the compiler points at any spot you missed.
4. **Document the settings:** add an `[ai.gemini]` section to `config.example.toml`, and the key to `.env.example` and the README.
5. **Spend (optional):** give `Gemini` an `Option<Spend>`, price each response's usage (like `openai/price.rs`) and call `spend.add(job, used)`. In `ai_from_env`, make `Spend::new(db, "gemini", budget)` and push it to `spends`; the control panel then shows it. For the real bill, implement `Bill` for a Gemini billing reader and spawn `follow_bill` in `main.rs` like the other two.

Chat, chime-ins, memory's jobs and the tool loops need no changes: they only use `ChatProvider`.

## Compared to voltgpt

voltgpt used the `openai-go` SDK and kept conversations on OpenAI's side through stored response IDs. Vivy has no SDK and keeps its own history, so Claude (which keeps no state between requests) can be a full provider, and the two can stand in for each other.

Claude is used through the Messages API, not the Agent SDK. There is no official Rust Agent SDK; the community crates start the Claude Code CLI as a subprocess, and the Agent SDK's built-in tools read and write files and run shell commands on the machine, which is the wrong thing to hand to Discord users.
