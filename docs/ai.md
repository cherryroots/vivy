# The `ai` module

`src/ai/` is how the bot talks to AI models. Every provider (Claude and OpenAI today) implements one trait, `ChatProvider`. The rest of the bot only sees the types in `src/ai/mod.rs`: a conversation is a list of `Turn`s, and an answer arrives as a stream of `ChatEvent`s.

What stays outside the providers, so every provider reuses it: Discord, message splitting, media extraction, the tool loop, and the bot's own tools. The Discord side of chat is in [chat](features/chat.md); the core (config, database, `BotCtx`, utilities) is in [architecture](architecture.md).

Both providers are called over plain HTTP with `reqwest` and `serde_json`, with a small parser for the streamed answers in `sse.rs`. There is no client crate: the official SDKs aren't in Rust, and community crates tend to lag behind new API features.

## Files

| File | What it holds |
|---|---|
| `mod.rs` | The shared types (`Turn`, `Part`, `Input`, `ChatRequest`, `ChatEvent`, `Done`), the `ChatProvider` trait, `AiConfig` and `Provider` (the `[ai]` config), `Ai` (the providers the bot has), and `next_input` |
| `complete.rs` | `complete`: running a request to the end without showing it anywhere, for background jobs |
| `sse.rs` | `SseParser`: splits a streamed body into server-sent events |
| `fallback.rs` | `ApiError`, telling outages from bad requests, `send` (one retry without a connection), `State` and `Watched` (moving chat to the backup provider) |
| `spend.rs` | `Spend`: what the AI costs this month, budget warnings, reading the real bill |
| `openai.rs` | The OpenAI provider (Responses API) and `OpenAiConfig` |
| `claude/mod.rs` | The Claude provider (Messages API), `ClaudeConfig`, `ContentKey`, pause and retry handling |
| `claude/messages.rs` | Turns as Claude's messages, the tool list and the system block |
| `claude/collect.rs` | `Collector`: rebuilds Claude's streamed content blocks and turns them into `ChatEvent`s |
| `claude/uploads.rs` | Files API IDs kept in the `claude_uploads` table |
| `claude/price.rs` | List prices per model, and what one response cost |
| `claude/billing.rs` | Reading Anthropic's Cost API with an Admin key |

## The types

A `Turn` is one message in the conversation: a `Role` (`User` or `Assistant`) and a list of `Part`s.

```rust
pub enum Part {
    Text(String),
    Image(ModelImage),          // from util::media: a MIME type and the bytes
    File(ModelFile),            // any other attachment: name, MIME type, bytes
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
    pub job: &'static str,    // "chat", "chime", "emoji", "reflection", "mood", "diary"
}
```

A `ToolDef` is a name, a description and a JSON Schema for the arguments. A `ToolCall` is the model asking to run one (an ID, the name, and the arguments parsed as JSON; arguments that aren't valid JSON arrive as a JSON string, so the tool can report the mistake).

The answer streams in as events:

```rust
pub enum ChatEvent {
    TextDelta(String),
    Activity(Activity),  // Thinking, SearchingWeb, RunningCode, Writing (for the status line)
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
- `sends_full_history` has a default body, so a provider only overrides it when it needs to. Claude returns `true`: every request carries the whole conversation, so history sends every turn's media and never changes earlier turns (see [chat](features/chat.md)).

## The tool loop

The tool loop lives outside the trait, so every provider gets the same tools. There are two copies of it:

- `answer::stream_answer` in `src/features/chat/answer.rs`, for answers in Discord: it also redraws the reply each second, shows tool names in the status line, and stops on ❌. At most 8 rounds.
- `ai::complete` in `complete.rs`, for work nobody watches: chime-ins and follow-ups (`chat/chime.rs`), emoji descriptions (`chat/emoji.rs`), and memory's reflection, mood and diary jobs. The caller passes a `ToolRunner` (a trait with one method, `run(&ToolCall) -> String`) and a round limit. It returns `Completed`: the last round's text, the continuation, and every round's `NativeRound`.

Both work the same way: send the request, read events until `Done`, and if `Done` lists tool calls, run them, then build the next input with `ai::next_input` and send again.

`next_input` (in `mod.rs`) is where the providers differ:

- With a `continuation` (OpenAI), the next input is `Input::After` with only the tool results. OpenAI remembers its own output.
- Without one (Claude), it is the whole conversation again, plus an assistant turn holding this round's `Part::Native` output, plus a user turn with the tool results. Claude needs its own tool calls (and the thinking before them) back, exactly as written.

## Claude

`claude/mod.rs`. One `POST /v1/messages` with `"stream": true` per round. Claude keeps nothing between requests, so each one carries the whole conversation.

**The request body** (`Claude::request_body`):

- `system`: one text block with `cache_control`, marking the end of the part every request shares.
- `tools`: Claude's built-in `web_search` and `web_fetch`, `code_execution` when `code_execution` is on, then the bot's tools with `eager_input_streaming` (`messages::tools`). The bot's `memory` tool is sent as a normal tool, not as Claude's own memory tool, because that one comes with Anthropic's instruction to log task progress in memory.
- `thinking`: adaptive (it can't be turned off; `effort` in `output_config` sets how much), with `block_binding.prefix_mismatch_behavior = "drop_block"`: if an earlier turn ever changes, Claude drops the thinking after it instead of failing the request. The response lists what it left out, and `collect.rs` logs it.
- `cache_control` at the top level, so the conversation up to its newest message is cached for the next request.
- `fallbacks: "default"` when `fallbacks` is on: if the model declines, the API retries on another Claude model. This is Anthropic's server-side model fallback, not the provider fallback below.
- `container`: Anthropic's skills (`xlsx`, `docx`, `pptx`, `pdf` by default) and the ID of the code execution container this `cache_key` used last, so files stay around between answers in a channel.

Beta headers: `thinking-binding-controls-2026-08-01`, and `server-side-fallback-2026-07-01` when `fallbacks` is on.

**Building the messages** (`messages::to_messages`): text, pictures and tool results become content blocks. An assistant turn with a `Part::Native` written by `"claude"` with the same model is replaced by its rounds, exactly as stored. Any other assistant turn sends only its text. A file becomes a note naming it, a `document` block for PDFs (so Claude reads them), and a `container_upload` block when code execution is on (so code can open it). If a chain starts with one of the bot's messages, a short user message is put first, since Claude requires the user to speak first.

**Uploads**: pictures and files go through the Files API: uploaded once, then sent by `file_id` (see "Uploads" below). Pictures over 2000 pixels a side or 5 MB are made smaller first (`shrink_image`), because Claude rejects big ones once a request has more than 20 pictures. A picture that fails to upload is sent inline as base64.

**Retries**: 429, 500, 502, 503 and 529 are retried twice, after 2 and 6 seconds. If the request fails because an upload is gone, those uploads are forgotten and sent again. If it fails because the code execution container is gone, the request is sent without a container ID.

**Reading the stream** (`collect.rs`): the answer arrives as content blocks (thinking, text, tool calls, built-in tools' results), each opened by `content_block_start`, filled by deltas and closed by `content_block_stop`. `Collector` rebuilds the blocks exactly as Claude wrote them, signatures included, because they go back into the conversation unchanged. On the way it sends `TextDelta`s and `Activity` events. Tool arguments stream as text and are parsed at the end of the block. An `error` event mid-stream becomes an `ApiError` (529 for `overloaded_error`, 500 for `api_error`), so the fallback reads it the same way as an HTTP error.

**When a response ends** (`read_answer`):

- `pause_turn` (a long run of built-in tools): the blocks so far are sent back as an assistant message to continue, at most 5 times.
- `refusal`: the answer fails with the category Claude gave.
- `max_tokens`: unfinished tool calls are removed, since the conversation can't hold a call without its result.
- After a server-side fallback (a `fallback` block), only text and finished built-in tool calls before the switch go back into the conversation.
- Files the code saved come from the code execution result blocks; their names are looked up with `GET /v1/files/{id}`, and `download_file` fetches `/v1/files/{id}/content`.

What each response cost is added to `Spend` (see below), even when it was stopped or failed partway.

## OpenAI

`openai.rs`. One `POST /v1/responses` with `"stream": true` per round. `OPENAI_BASE` in `.env` sets another base URL for an OpenAI-compatible endpoint; `/v1` is added when it's missing.

- The body has `instructions` (the system prompt), `input`, the built-in `web_search` and `code_interpreter` tools, then the bot's tools as `function` tools, plus `reasoning.effort`, `text.verbosity`, `service_tier`, `"store": true`, `"truncation": "auto"` and `prompt_cache_key` (the `cache_key`). OpenAI caches automatically.
- OpenAI stores every response, so `Done.continuation` is the response ID. The next request sends `previous_response_id` and only what's new. That already carries the earlier reasoning, so OpenAI ignores `Part::Native`.
- Pictures are sent inline as base64 data URLs.
- Attached files are uploaded first (`POST /v1/files`, kept 30 days so stored responses that point at them still work). PDFs and Office files are also given to the model to read as `input_file` (at most 50 MB per request). Types the code interpreter takes go into its container. Other files get a note saying they're attached but can't be opened.
- Files the code interpreter made come from `container_file_citation` annotations; `download_file` reads them from `/v1/containers/{container}/files/{file}/content`.
- 429, 500, 502 and 503 are retried twice, after 2 and 6 seconds. Stream errors become an `ApiError` (500 for `server_error`, 429 for rate limits and `insufficient_quota`, 400 otherwise).
- OpenAI's cost is not tracked; `Spend` is Claude only.

## History and native replay

Chat stores conversations itself, in `chat_turns` (see [chat](features/chat.md)), so they don't depend on any provider's memory. `history::build_input` picks:

- `Input::After` when an answer in the chain was written by this provider and model and has a `continuation_id` (OpenAI);
- otherwise `Input::Full` with the whole chain.

So switching providers in the middle of a conversation works: the new provider gets the neutral text of the earlier answers.

For answers, `native_json` keeps the provider's raw output round by round, with the tools' results. When the history goes back to Claude with the same model, the rounds are sent unchanged instead of the turn's text. That keeps the model's earlier thinking (Claude's rule is that thinking blocks only stay valid while everything before them is unchanged), and keeps the request prefix byte-identical so the prompt cache still hits. Any other provider or model gets only the text, because thinking blocks are tied to the model that wrote them. `NativeRound::parse_list` returns `None` for answers saved before rounds existed, which are then sent as text.

Thinking is never shown in Discord.

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

`spend.rs`, `claude/price.rs`, `claude/billing.rs`. `Spend` exists when Claude is the main provider or the fallback.

- After each Claude response, `add_spend` (in `claude/mod.rs`) prices its usage (input, cache writes and reads, output, web searches) with `price::cost`, from a table of list prices per model ID prefix. Cache writes cost 1.25 times the input price; web searches $10 per 1000. An unknown model is priced like Opus 5.5.
- `Spend::add` adds it to `ai_spend` (the month's total) and to `ai_spend_jobs` (per job). Months are calendar months in UTC.
- At 80% and 100% of `monthly_budget` under `[ai.claude]` there is one warning each per month (the `warned` column), which reaches the log channel. A budget of 0 turns them off.
- With `ANTHROPIC_ADMIN_KEY` in `.env` and `billed_spend` on, `main.rs` spawns `Spend::follow_bill`, which reads the Cost API (`claude/billing.rs`, the whole organization, daily buckets in cents) now and every hour, and replaces the month's total with it. Answers in between add their estimate on top until the next read. A failing read is warned about once, then logged quietly.
- `Spend::this_month` gives the control panel the month's total, the cost per job (`show_job_costs`) and the cache hit rate (`show_cache_hits`: input tokens read from the cache over all input tokens). `Spend::admin_key` says whether the bill is being read.

The Admin API can't create API keys, so the cost per job comes from the bot's own counts rather than one key per job.

## Tables

| Table | Owner | Columns | What for |
|---|---|---|---|
| `ai_spend` | core (`CORE_MIGRATIONS` 2 in `src/core/db.rs`) | month (`2026-10`), provider, USD, `warned` (highest budget share already warned about, in percent) | The month's total, for the budget warnings and the status message |
| `ai_spend_jobs` | core (`CORE_MIGRATIONS` 3) | month, provider, job, USD, uncached input tokens, cache read tokens, cache write tokens | Cost per job and cache hit rate |
| `claude_uploads` | `claude` (`claude::UPLOAD_MIGRATIONS`, run from `main.rs` with every owner's migrations) | content key (hash and length as text), Files API ID, expiry (unix seconds) | Reusing uploads across restarts |

Background loop: the timer `"Anthropic bill"` (`"hourly"`), from `Spend::follow_bill`.

## Configuration

`[ai]` in `config.toml` is read into `AiConfig` (`mod.rs`): `provider`, `fallback`, `[ai.claude]` (`ClaudeConfig`) and `[ai.openai]` (`OpenAiConfig`). `config.example.toml` explains each key. Keys come from `.env`: `ANTHROPIC_API_KEY`, `OPENAI_TOKEN` (and optional `OPENAI_BASE`), and the optional `ANTHROPIC_ADMIN_KEY`.

`ai_from_env` in `src/main.rs` builds the providers at startup. Only the main provider's key is read, plus the fallback's when one is set (and different from the main one). Without the main key, `ctx.ai.chat()` returns `None` and chat says it is turned off; a missing fallback key only means there is no fallback. All providers share one `reqwest::Client` with a 10-second connect timeout and a 5-minute read timeout.

## Adding a provider (for example Gemini)

1. **Write the provider.** Add `src/ai/gemini.rs` with a `GeminiConfig` (`#[derive(Deserialize)]`, `#[serde(default)]`, and a `Default` impl) and a `Gemini` struct that implements `ChatProvider`:
   - `name` returns `"gemini"`; `model` returns the configured model.
   - `stream` turns `request.input` into Gemini's format, posts it through `fallback::send` (so a dropped connection is retried once), and spawns a task that reads the stream with `SseParser` and sends `TextDelta`, `Activity` and finally one `Done` into an `mpsc` channel. Stop reading when sending fails.
   - Put tool calls in `Done.tool_calls` and the raw output in `Done.native`. If Gemini can continue a stored conversation, put its ID in `Done.continuation`; otherwise leave it `None` and override `sends_full_history` to return `true`.
   - Handle `Part::ToolResult` (results go back to the model) and `Part::Native` (replay your own rounds when `provider == "gemini"` and the model matches, or skip it and use the turn's text).
   - Return errors as `fallback::ApiError { provider: "gemini", status, message }`, so the fallback can tell outages from bad requests. If Gemini's out-of-credit wording differs, add it to `ApiError::outage`.
   - `download_file` fetches a `GeneratedFile` by the `location` you gave it.
2. **Register it in `src/ai/mod.rs`:** `pub mod gemini;`, re-export the types with `pub use`, add `Gemini` to the `Provider` enum (`#[serde(rename_all = "lowercase")]` makes it `"gemini"` in the config), add `pub gemini: GeminiConfig` to `AiConfig`, and add `"gemini" => "Gemini"` to `display_name`.
3. **Build it in `ai_from_env` (`src/main.rs`):** add the key's name (like `GEMINI_API_KEY`) to the first `match provider`, and the constructor to the second. Rust checks that every `match` on `Provider` covers the new variant, so the compiler points at any spot you missed.
4. **Document the settings:** add an `[ai.gemini]` section to `config.example.toml`, and the key to `.env.example` and the README.
5. **Spend (optional):** cost is only tracked for Claude. To track Gemini, price its usage and call `Spend::add("gemini", job, used)`; `ai_from_env` only creates `Spend` when Claude is used, and the control panel reads `this_month("claude")`, so both would need changing.

Chat, chime-ins, memory's jobs and the tool loops need no changes: they only use `ChatProvider`.

## Compared to voltgpt

voltgpt used the `openai-go` SDK and kept conversations on OpenAI's side through stored response IDs. Vivy has no SDK and keeps its own history, so Claude (which keeps no state between requests) can be a full provider, and the two can stand in for each other.

Claude is used through the Messages API, not the Agent SDK. There is no official Rust Agent SDK; the community crates start the Claude Code CLI as a subprocess, and the Agent SDK's built-in tools read and write files and run shell commands on the machine, which is the wrong thing to hand to Discord users.
