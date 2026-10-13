# How Vivy is put together

This is a guide to the code: how the parts fit, what happens when the bot starts, and how a Discord event reaches a feature. Each feature has its own page under [`features/`](features/), and [`ai.md`](ai.md) covers the AI providers. [`feature-map.md`](feature-map.md) is the overview of what exists.

## The four parts

```
src/
  main.rs        start-up: config, database, AI, Discord
  core/          what every feature shares: the Feature trait, the dispatcher, config, db, logging
  ai/            talking to AI models (Claude, OpenAI), behind one trait
  util/          helpers with no feature knowledge: splitting messages, media, pictures
  features/      one folder per feature, listed in features/mod.rs
```

| Part | May use | Knows about features? |
|---|---|---|
| `core` | `ai` | No. It only sees the `Feature` trait. |
| `ai` | `util` | No |
| `util` | nothing of ours | No |
| a feature | `core`, `ai`, `util` | Only itself |

## The three rules

1. **Features never import each other.** A feature may use `core`, `ai` and `util`, nothing else. When features need each other, they go through the core: chat tools (a feature offers a tool, chat runs it), chat context (a feature adds a few lines to each question), bot events (a feature publishes, others may listen), and the status picture (each feature adds its own stats). Chat never imports reminders, and reminders never import chat.
2. **A feature declares everything it adds in one place**, its `impl Feature`: commands, mention prefixes, tools, tables, background tasks, stats. Nothing is registered by hand anywhere else.
3. **Logic, storage and Discord are separate files.** Pure logic (the reminder parser `reminders/parse.rs`, the wheel ledger `wheel/ledger.rs`, memory's folder commands `memory/folder.rs`, snail link keys `snails/links.rs`) takes plain values and returns plain values, so it is tested without Discord or a database. SQL lives in each feature's `store.rs` (or a `store/` folder once it grows big, like snails'). The Discord code in between only turns events into calls to those two.

## What happens at startup

All of this is in `src/main.rs`, top to bottom:

1. **Secrets and settings.** `dotenvy` loads `.env` into the environment. `Config::load` reads `config.toml` (or the file `VIVY_CONFIG` names). A missing file isn't an error: every feature is then on everywhere, with a warning.
2. **Logging.** `logging::init` sets up `tracing`. Lines meant for the Discord log channel wait in a queue until the bot is connected, so even startup errors reach Discord.
3. **Features.** `features::all()` builds the list of every feature.
4. **Database.** `Db::open` opens the SQLite file. Migrations run for the core, for Claude's upload table, then for every feature. If voltgpt's `old.db` is there, its reminders and wheel game are imported (see "Importing from voltgpt" below).
5. **AI.** `ai_from_env` builds the chat provider that `provider` under `[ai]` names, plus the `fallback` one, from the keys in `.env`. With `ANTHROPIC_ADMIN_KEY`, a task starts that reads Anthropic's bill hourly.
6. **Commands.** Every feature's slash commands are collected. Each gets its feature's name as its poise `category`, so the gate check knows whose command it is.
7. **Connect.** serenity connects to Discord. When Discord says the bot is ready, poise runs `setup` once: it builds the `BotCtx`, starts posting to the log channel, registers the slash commands globally (and clears voltgpt's old per-server ones), starts delivering bot events, and calls `start` on every enabled feature. Then the 🟢 Started line goes to the log channel.
8. **Shutdown.** Ctrl+C or SIGTERM (`systemctl stop`) cancels the `shutdown` token. Background tasks see it and stop, and the bot waits up to 10 seconds for them and for event handlers still running (the control panel uses that time to show OFFLINE).

## The `Feature` trait

Every feature is a struct that implements `Feature` (`src/core/feature.rs`). Every method has a default that does nothing, so a feature only writes the ones it needs. Trimmed:

```rust
#[async_trait]
pub trait Feature: Send + Sync + 'static {
    /// Also the `[features.<name>]` config section, the start of its button IDs, and its log label.
    fn name(&self) -> &'static str;

    // Declarations, read once at startup.
    fn commands(&self) -> Vec<Command>;                          // slash commands (poise)
    fn migrations(&self) -> &'static [&'static str];             // its tables
    fn mention_prefixes(&self) -> &'static [&'static str];       // "remind me", ...
    fn legacy_import(&self) -> Option<LegacyImport>;             // voltgpt's data
    fn tools(&self) -> Vec<ToolDef>;                             // functions the chat model may call
    fn tool_status(&self, tool: &str, args: &Value) -> Option<&'static str>;  // "🎨 Drawing…" while one runs
    async fn run_tool(&self, ctx, asker: &Asker, name, args) -> Result<String>;
    async fn run_tool_with_pictures(&self, ctx, asker, name, args) -> Result<ToolReply>; // text + pictures
    async fn chat_context(&self, ctx, asker: &Asker, fresh: bool) -> Result<Option<String>>;
    async fn stats(&self, ctx) -> Result<Vec<Stat>>;             // a card on the status picture
    async fn panels(&self, ctx) -> Result<Vec<Panel>>;           // a box of text on it

    // Lifecycle.
    async fn start(&self, ctx) -> Result<()>;                    // spawn background tasks here

    // Events.
    async fn on_mention(&self, ctx, msg, rest: &str) -> Result<Mention>;
    async fn on_message(&self, ctx, msg) -> Result<()>;
    async fn on_reaction_add(&self, ctx, reaction) -> Result<()>;
    async fn on_message_edit(&self, ctx, event) -> Result<()>;
    async fn on_messages_deleted(&self, ctx, channel, messages) -> Result<()>;
    async fn on_component(&self, ctx, interaction, action: &str) -> Result<()>;
    async fn on_modal(&self, ctx, interaction, action: &str) -> Result<()>;
    async fn on_bot_event(&self, ctx, event: &BotEvent) -> Result<()>;
}
```

`#[async_trait]` is a macro that lets a trait have `async fn`s and still be used as `dyn Feature`. `Send + Sync + 'static` means a feature can be shared between threads, which tokio needs.

The whole bot is the list in `src/features/mod.rs`:

```rust
pub fn all() -> Vec<Arc<dyn Feature>> {
    vec![
        Arc::new(reminders::Reminders::default()),
        Arc::new(wheel::Wheel),
        Arc::new(control_panel::ControlPanel),
        Arc::new(memory::Memory),
        Arc::new(snails::Snails::default()),
        Arc::new(voice::Voice),
        Arc::new(drawing::Drawing),
        Arc::new(who_said_it::WhoSaidIt),
        Arc::new(chat::Chat::default()),
    ]
}
```

`Arc<dyn Feature>` is a shared pointer to "some type that implements `Feature`", so different structs can sit in one list.

Which feature uses which hook:

| Hook | chat | reminders | wheel | memory | snails | voice | drawing | control_panel | who_said_it |
|---|---|---|---|---|---|---|---|---|---|
| `commands` | | ✓ | ✓ | ✓ | ✓ | | | | |
| `migrations` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | |
| `mention_prefixes` | `""` | `remind me`, ... | | | | | | | |
| `legacy_import` | | ✓ | ✓ | | | | | | |
| `tools` / `run_tool` / `tool_status` | ✓ | ✓ | ✓ | ✓ | | ✓ | ✓ | | |
| `chat_context` | | | | ✓ | | | | | |
| `stats` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | | |
| `panels` | ✓ | | | ✓ | | | | | |
| `start` | ✓ | ✓ | ✓ | ✓ | ✓ | | | ✓ | ✓ |
| `on_mention` | ✓ | ✓ | | | | | | |
| `on_message` | ✓ | | | | ✓ | | | |
| `on_reaction_add` | ✓ | | | | | | | |
| `on_message_edit` | | | | | ✓ | | | |
| `on_messages_deleted` | | | | | ✓ | | | |
| `on_component` | | ✓ | ✓ | | | | | |
| `on_modal` | | ✓ | ✓ | | | | | |

## How events reach features

The dispatcher (`src/core/dispatcher.rs`) gets every Discord event from poise and hands it to the features that are allowed where it happened (see "Config and gating").

| Event | Who gets it |
|---|---|
| A message from a human | Every feature's `on_message`. Messages from bots are dropped. |
| A message that @-mentions the bot | Also: the mention at the start is removed (later ones stay, so "stop being mean to @Vivy" keeps its last word), and the feature whose `mention_prefixes` match the start of the text gets `on_mention` with the rest. Matching ignores case, needs a whole word ("remind" doesn't match "reminders") and the longest prefix wins. Chat declares the empty prefix `""`, so it gets every mention nobody else claimed. A feature that can't use the mention returns `Mention::PassOn`, and chat gets the whole text instead: "remind me what the movie was called" gets an answer instead of an error. |
| A reaction | Every feature's `on_reaction_add`, except the bot's own reactions. |
| An edit (or a link preview Discord adds later) | Every feature's `on_message_edit`, except edits of bot messages. |
| A deleted message, or many at once | Every feature's `on_messages_deleted`. |
| A button, select menu or modal | IDs look like `reminders:snooze:12:10`. The part before the first `:` picks the feature, and it gets the rest (`snooze:12:10`) as `action`. An ID matching no feature (an old voltgpt button) gets "This button is from an older version of the bot". |
| A slash command | poise runs it. `command_check` refuses it where its feature is turned off. |
| A `BotEvent` | Every enabled feature's `on_bot_event` (see "How features talk to each other"). |

**Every handler runs in its own tokio task** (`dispatcher::run`), inside a tracing span that names the feature, server, channel, user and message. A slow handler never holds up the others, and a panic only ends that one task (the panic hook logs it). In voltgpt a panic in a handler crashed the whole bot. The tasks are spawned on `ctx.tasks`, so at shutdown the bot waits for them (within the same 10 seconds) instead of cutting off an answer or a database write halfway.

**Errors.** Handlers return `anyhow::Result`. When one fails, the dispatcher logs the error with its whole chain of causes and tells the user something: a reply for a message, a private (ephemeral) message for a button or command. Most errors become "Something went wrong. The error was logged." An error made with `user_error("...")` (`src/core/errors.rs`) is meant for the user and is shown as written, without an error in the log.

## `BotCtx`: what every handler gets

`BotCtx` (`src/core/ctx.rs`) holds every shared service. Cloning it is cheap, since every field is a handle to something shared.

| Field | What it is |
|---|---|
| `http` | Discord's REST API: send, edit, delete, respond to interactions |
| `cache` | What the gateway told us about servers, channels, users |
| `shard_manager` | The gateway connections (latency on the status picture, the bot's custom status) |
| `db` | The SQLite database (see below) |
| `config` | `config.toml` |
| `ai` | The AI providers. `ctx.ai.chat()` gives the one to use for this answer (the fallback while the main one is down). Any feature can use it, not just chat. |
| `web` | A shared `reqwest` HTTP client for API calls, built from `util::http::client_builder`. Links from messages use `util::media::safe_client` instead. |
| `events` | Where `BotEvent`s are published (`ctx.publish(...)`) |
| `shutdown` | A `CancellationToken`, cancelled when the bot stops |
| `tasks` | A `TaskTracker`: tasks spawned on it are waited for at shutdown |
| `timers` | The background loops' timers, shown on the status picture |
| `features` | Every feature, in the order of `features::all()` |
| `bot_id`, `started_at` | The bot's own user ID, and when it started |

Useful methods: `ctx.allows(feature, guild, channel)` (the gate check), `ctx.channel_name(guild, channel)` (a channel's or thread's name from the cache), `ctx.is_admin(user)` (listed in `admins`), `ctx.set_status(text)` (the line under the bot's name), `ctx.publish(event)`.

## Config and gating

Secrets (tokens) live in `.env`. Everything else is in `config.toml`, explained line by line in `config.example.toml`. `src/core/config.rs` reads it.

Each feature has a `[features.<name>]` section. The core reads the gate keys from every section into a `Gate`: `enabled`, `guilds`, `deny_guilds`, `channels` and `deny_channels`. An empty list means no limit. The dispatcher checks the gate before calling a feature, so a feature never checks it for events. A thread counts as itself and as its parent channel: allowing a channel allows its threads, and denying either denies it. Work not tied to a channel (a daily job per server) uses `gate.allows_guild(guild)`.

A feature reads its own keys into its own struct with `ctx.config.feature::<Settings>("name")`. The struct derives `serde::Deserialize` and `Default`, so a missing key takes its default.

Config mistakes warn instead of passing silently, and never stop the bot. They are posted once the log channel is up: a missing `config.toml`, a `[features.x]` section that matches no feature, a gate key typo like `enable` or `deny_channel`, and any key nothing reads (found with the `serde_ignored` crate). Memory splits its section over several structs, so it uses `feature_part` and checks the leftovers with `warn_unknown` itself.

## The database

One SQLite file (`vivy.db` by default) in WAL mode, which lets reads happen while something writes. Raw SQL, no ORM, through `rusqlite`.

`rusqlite` blocks while it works, which would stall the async code. `tokio-rusqlite` runs the connection on its own thread, and `ctx.db.call(...)` sends a closure there and waits for the answer:

```rust
let pending: i64 = ctx.db.call(|conn| {
    Ok(conn.query_row("SELECT count(*) FROM reminders WHERE sent_at IS NULL", [], |row| row.get(0))?)
}).await?;
```

Inside the closure everything is ordinary synchronous Rust, and the connection is never held across an `.await`. Keep these closures short (a few queries, no slow work like hashing pictures or calling an API): there is one connection thread for every feature, so while one closure runs, every other feature's database call waits. That's why store functions are plain `fn`s taking `&Connection`: they are easy to test.

**Migrations** (`db::migrate`). Each feature returns a list of SQL strings from `migrations()`; entry `n` is version `n + 1`. The `schema_migrations` table remembers the newest version that ran per owner (`core`, `claude`, or a feature name), so each runs once, in a transaction. **Never edit a migration that has shipped**: add a new one to the end of the list. Each feature's tables start with its name (`reminders`, `wheel_bets`, `chat_turns`, `snail_links`).

Core tables, shared by everyone: `user_settings` (per-user timezone, read through `core::settings`), `legacy_imports`, `ai_spend` and `ai_spend_jobs` (see [ai.md](ai.md)). `claude_uploads` belongs to the `claude` owner.

For tests, `db::test_connection(owner, migrations)` gives an in-memory database with the core tables and one feature's tables.

## Background tasks and timers

A feature starts its loops in `start`, with `ctx.tasks.spawn(...)`, and stops them when `ctx.shutdown` is cancelled. A typical loop waits with `tokio::select!` on either the sleep or the shutdown token.

Every loop reports to the status picture's Timers box through `ctx.timers` (`src/core/timers.rs`): `add(name, every)` once, `running()` when it wakes up, and `sleeping(wait)` or `next_at(time)` before it sleeps again. New loops should do the same.

| Timer | Feature | Runs |
|---|---|---|
| Reminders | reminders | at the next reminder |
| Check-ins | chat | at the next check-in |
| Emoji descriptions | chat | daily |
| Memory upkeep | memory | hourly: reflection, mood checks, faces, banners, diary |
| Snail picture retries | snails | every 5 minutes |
| Status graphs | control_panel | every 15 minutes |
| Anthropic bill | core (`ai/spend.rs`) | hourly, only with `ANTHROPIC_ADMIN_KEY` |
| OpenAI bill | core (`ai/spend.rs`) | hourly, only with `OPENAI_ADMIN_KEY` |

## How features talk to each other

Since features never import each other, the core gives them four ways to work together:

- **Chat tools.** A feature lists functions in `tools()` and runs them in `run_tool`. Chat offers the model the tools of every feature allowed in that channel and sends each call back to the feature that owns it. A tool runs as the person who asked (`Asker`: user, server, channel, message), so it only sees what they can see and only changes their own things. A tool's error goes back to the model as the result, so it can fix its call. Example: reminders offers `create_reminder`; memory offers `memory` and `set_mood`; voice offers `send_voice_message`; drawing offers `draw_picture`.
- **Chat context.** `chat_context` returns a few lines chat adds after the newest question, for that answer only. Memory uses it for the list of memory files and Vivy's mood. It goes after the question so the system prompt and earlier turns stay the same and the prompt cache keeps working.
- **Bot events.** `ctx.publish(BotEvent::...)` sends an event to every feature's `on_bot_event` (`src/core/events.rs`, a `tokio::sync::broadcast` channel). Reminders publishes `ReminderFired` and the wheel `WheelRoundResolved`. No feature listens yet; they're there for the next one that needs them.
- **The status picture.** `stats` and `panels` let each feature add its own numbers and text to the control panel without the control panel knowing about it. See [control-panel.md](features/control-panel.md).

## Logging

`tracing` everywhere: `info!`, `warn!`, `error!`. Because the dispatcher opens a span per handler, every line inside one already says which feature, server, channel and message it came from.

- **stdout** gets short readable lines (or JSON with `LOG_FORMAT=json`). `RUST_LOG` picks the level, for example `RUST_LOG=info,vivy::features::chat=debug`. Under systemd, `journalctl -u vivy -f` shows them.
- **The log channel** (`[logging]` in config.toml) gets warnings and errors (or the level in `log_level`), with a link to the message that triggered them, posted every 5 seconds. The same line repeated is grouped as "×12". Lines with the target `"lifecycle"` (🟢 Started, 🔴 Shutting down, 🔌 Reconnected) are always posted. This is part of the core, so it works with the control panel turned off.
- **Errors keep their cause.** Add `.context("what we were doing")` where an error happens, and the log reads "sending reminder 12: Missing Access" instead of only "Missing Access".

## Shared helpers (`util`)

| File | What it gives |
|---|---|
| `split.rs` | `split_message`: long text into Discord-sized parts, cut at Markdown boundaries by the `text-splitter` crate, never inside an emoji, with code blocks closed and reopened across parts |
| `reply.rs` | `LiveReply`: a reply that can span several messages and change over time. `show(parts)` edits, sends or deletes messages until Discord matches. Chat streams answers through it. |
| `media.rs` | `find_media` (pictures and videos in attachments, embeds and links), `find_files` (other attachments), `load_for_model` (download and turn into images a model reads), `preview_links` (the links Discord may preview: not `<link>` ones) and `with_previews` (wait for late link previews), and `safe_client` / `check_url`, which refuse local and private addresses so a link can't reach the server's own network |
| `frames.rs` | `frame_grids`: one ffmpeg run turns a GIF or video into a few grid pictures of its frames. At most 2 files go through ffmpeg at once (`MAX_RUNS`); the rest wait their turn. A GIF ffmpeg can't read falls back to its first frame, decoded by `media.rs`. |
| `svg.rs` | Drawing pictures as SVG and turning them into PNG with `resvg`, with the Inter font built in, plus the parts both pictures draw, like the rounded `pill` label. The wheel and the status picture use it. |
| `http.rs` | `client_builder`: the user agent and timeouts every HTTP client starts from |
| `text.rs` | Text from text-file attachments and embeds, for chat |
| `mod.rs` | `shorten`: cut text to a number of characters, ending in "…"; `env_key`: a key from `.env`, `None` when missing or empty; `month_start`: where this month's limits start counting |

## Importing from voltgpt

If `old.db` (voltgpt's database, `old_database` in config.toml) exists at startup, `core/legacy.rs` opens it read-only and runs every feature's `legacy_import`, each in its own transaction. `legacy_imports` records the parts that are done, so a restart never imports twice. Once every part in `legacy::PARTS` (`reminders`, `wheel`) is done, the file is renamed to `old.db.imported`. The results are listed in the 🟢 Started line.

This code is temporary. Once Cherry has confirmed the imports, it goes: `core/legacy.rs`, `Feature::legacy_import` and `LegacyImport`, each feature's `import.rs`, the call in `main.rs`, and the `old_database` setting. The `legacy_imports` table stays, since dropping it would need a new migration.

## Adding a feature

Say a feature called `quotes`:

1. Make `src/features/quotes/mod.rs` with a struct that implements `Feature`:

   ```rust
   use async_trait::async_trait;
   use serenity::all::Message;

   use crate::core::{BotCtx, Feature, Mention, Result};

   pub struct Quotes;

   #[async_trait]
   impl Feature for Quotes {
       fn name(&self) -> &'static str {
           "quotes"
       }

       fn mention_prefixes(&self) -> &'static [&'static str] {
           &["quote"]
       }

       async fn on_mention(&self, ctx: &BotCtx, msg: &Message, rest: &str) -> Result<Mention> {
           msg.reply(&ctx.http, format!("Saved: {rest}")).await?;
           Ok(Mention::Handled)
       }
   }
   ```

2. Add `mod quotes;` and `Arc::new(quotes::Quotes)` to `src/features/mod.rs`. Put it before chat, which goes last.
3. Fill in only what it needs: tables in `migrations()` (SQL in a `store.rs`), commands in a `commands.rs`, tools in a `tools.rs`, a loop in `start`.
4. If it has settings, add a `[features.quotes]` section to `config.example.toml` and read it with `ctx.config.feature::<Settings>("quotes")`.
5. Give it a page under `docs/features/` and a row in [feature-map.md](feature-map.md).

Nothing else changes: the dispatcher, chat and the control panel find it through the trait.

## Testing

`cargo test` runs every test; none needs Discord or an API key. Pure functions have unit tests next to them (`#[cfg(test)] mod tests` at the bottom of a file). Store functions are tested on `db::test_connection`. One test is `#[ignore]`d because it's a simulation rather than a check: `wheel::sim::compare_rules`, run it with `cargo test compare_rules -- --ignored --nocapture`. `cargo clippy` and `cargo fmt` keep the style.

## Where to start reading

A path through the code for learning it, from small to large:

1. `src/features/mod.rs` and `src/core/feature.rs`: the list and the trait.
2. `src/core/dispatcher.rs`: how a message finds its feature. `route_mention` is a pure function with tests at the bottom.
3. `src/features/reminders/`: the smallest full feature. `parse.rs` (a grammar built from small `winnow` parsers, with many tests), `store.rs` (SQL), `scheduler.rs` (one background task that sleeps until the next reminder), `mod.rs` (wiring).
4. `src/core/db.rs`: migrations in 30 lines.
5. `src/features/wheel/ledger.rs`: all the game's money rules as one pure function.
6. `src/util/split.rs` and `src/util/reply.rs`: sending long messages.
7. `src/ai/mod.rs`, then `src/features/chat/answer.rs`: one chat answer from start to end.

## Crates

| Crate | Used for |
|---|---|
| `serenity`, `poise` | Discord: serenity for the gateway and REST API, poise for slash commands |
| `tokio`, `tokio-util` | The async runtime; `CancellationToken` and `TaskTracker` |
| `rusqlite`, `tokio-rusqlite` | SQLite, on its own thread |
| `reqwest` | HTTP: the AI APIs, ElevenLabs, downloads |
| `serde`, `serde_json`, `toml`, `serde_ignored` | JSON and config, and finding unread config keys |
| `anyhow` | Errors with a chain of causes |
| `tracing`, `tracing-subscriber`, `tracing-log` | Logging; `tracing-log` passes on lines from crates that log with `log` instead |
| `chrono`, `chrono-tz` | Times and timezones |
| `winnow` | The reminder time grammar |
| `text-splitter` | Splitting long messages |
| `resvg`, `skrifa` | Drawing SVG pictures as PNG, and reading fonts |
| `image`, `image_hasher` | Decoding, cropping and scaling pictures (snails, faces), and fingerprinting them (snails) |
| `sysinfo` | Memory use on the status picture |
| `dotenvy` | Loading `.env` |
| `fastrand` | Dice rolls, like whether Vivy chimes in |
| `tempfile` | Temporary files for ffmpeg |
| `axum` | The web server for the Who said it? Activity: its page, login and WebSockets |
