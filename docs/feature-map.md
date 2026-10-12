# Vivy feature map

What Vivy does today, where each feature lives in the code, what happened to each part of voltgpt (the Go bot Vivy replaced), and what is left to build.

## The docs

| Doc | What's in it |
|---|---|
| [README](../README.md) | What each feature does for people, and how to set the bot up |
| `config.example.toml` | Every setting, explained |
| This page | The overview: features, tables, decisions, history, what's next |
| [architecture.md](architecture.md) | How the code fits together: startup, the `Feature` trait, the dispatcher, config, database, logging, adding a feature, where to start reading |
| [ai.md](ai.md) | The AI providers (Claude, OpenAI), pictures, conversation history, fallback and spend |
| [features/chat.md](features/chat.md) | Chat: answers, tools, chime-ins, follow-ups, server emoji |
| [features/reminders.md](features/reminders.md) | Reminders: the time grammar, the scheduler, snooze |
| [features/wheel.md](features/wheel.md) | The movie wheel: the money rules, seasons, the round picture |
| [features/memory.md](features/memory.md) | Memory: the notes folder, reflection, mood, faces, banners, nickname, diary |
| [features/voice.md](features/voice.md) | Voice messages through ElevenLabs |
| [features/drawing.md](features/drawing.md) | Drawing pictures with GPT Image 2.5 |
| [features/snails.md](features/snails.md) | Repost detection: link keys, picture fingerprints, backfill |
| [features/control-panel.md](features/control-panel.md) | The status picture and the log channel |

## Features

Every feature is a folder under `src/features/`, listed in `features::all()` (`src/features/mod.rs`), with a `[features.<name>]` section in `config.toml` that can turn it off or limit it to some servers and channels.

| Feature | Folder | What it does | How people use it |
|---|---|---|---|
| Chat | `chat/` | Answers when someone @-mentions Vivy, with web search, code, files and the bot's own tools; chimes in now and then; checks in on people later; knows the server's emoji | @-mention, reply to an answer, ❌ and 🔁 reactions |
| Reminders | `reminders/` | Pings you at a time you name, in plain words, with the pictures you attached, once or on repeat | `@Vivy remind me ...`, `/reminders`, `/timezone`, Cancel and snooze buttons, chat tools |
| Movie wheel | `wheel/` | A betting game for movie night, drawn as a picture, in seasons | `/wheel status`, `/wheel add`, `/wheel movie`, `/wheel bet`, `/wheel reset`, buttons |
| Memory | `memory/` | A folder of notes per server (and per person in DMs) that Vivy keeps herself; her own notes, mood, face, banner, nickname, and a weekly diary | The `memory` and `set_mood` chat tools, `/memory show`, `/memory forget`, `/memory delete` |
| Voice | `voice/` | Vivy sends Discord voice messages in her own voice | The `send_voice_message` chat tool |
| Drawing | `drawing/` | Vivy draws pictures, or changes ones people post, with GPT Image 2.5 | The `draw_picture` chat tool |
| Snails | `snails/` | Quietly notes every link and picture, so reposts can be found and counted | "Check Snail" (right-click a message → Apps), `/snails top`, `/snails backfill` |
| Control panel | `control_panel/` | A status picture kept up to date in a channel | Watching the channel |

Not features, but parts of the core everyone uses: the **log channel** (`src/core/logging.rs`), the **AI providers** (`src/ai/`), and the **shared helpers** (`src/util/`).

### What each feature listens to

| Feature | Discord events | Background loops |
|---|---|---|
| Chat | mentions nobody else claimed (and ones passed on), every message (chime-ins), reactions (❌ 🔁) | Check-ins, Emoji descriptions |
| Reminders | mentions starting with `remind me`, `reminder`, `remind`; its buttons and menu | Reminders |
| Movie wheel | its buttons, menus and modals | none (it looks names up once at start) |
| Memory | none (it works through chat tools and its loop) | Memory upkeep (hourly) |
| Voice | none (only its chat tool) | none |
| Drawing | none (only its chat tool) | none |
| Snails | every message, edits, deletes | Snail picture retries, plus the backfill and the startup catch-up |
| Control panel | none | the status refresh (every 60 s) and Status graphs (every 15 min) |

How these reach the feature is in [architecture.md](architecture.md#how-events-reach-features).

### Tables

One SQLite file. Each feature owns its tables and migrations; the names start with the feature's name.

| Owner | Tables |
|---|---|
| core | `schema_migrations`, `user_settings`, `legacy_imports`, `ai_spend`, `ai_spend_jobs` |
| claude (in `ai/`) | `claude_uploads` |
| chat | `chat_turns`, `chat_messages`, `chat_follow_ups`, `chat_emoji` |
| reminders | `reminders`, `reminder_images` |
| wheel | `wheel_seasons`, `wheel_options`, `wheel_rounds`, `wheel_claims`, `wheel_bets`, `wheel_names`, `wheel_round_ids`, `wheel_status_messages` |
| memory | `memory_files`, `memory_changes`, `memory_reflections`, `memory_diaries`, `memory_pictures`, `memory_mood_checks` |
| voice | `voice_messages` |
| drawing | `drawings` |
| snails | `snail_messages`, `snail_links`, `snail_pictures`, `snail_crawl`, `snail_backfills`, `snail_caught`, `snail_failures`, `snail_dropped` |
| control_panel | `control_panel_state`, `control_panel_samples` |

## Decisions

| Topic | Decision |
|---|---|
| Discord library | `serenity` for events and the REST API, `poise` for slash commands |
| Structure | One crate, plain modules. Features never import each other; they work together through chat tools, chat context, bot events and the status picture. Each declares everything it adds in its `Feature` impl. A Cargo workspace with a crate per feature would let the compiler enforce that, but isn't worth the build setup yet. |
| AI | One `ChatProvider` trait in `ai`, so any feature can call a model. Claude is the main provider, through the plain Messages API over `reqwest` (no Rust Agent SDK exists, and the Agent SDK is built for coding agents). OpenAI is the fallback, since its free data-sharing tokens would train on members' messages. Each provider's built-in tools are used fully: web search, web fetch, code execution, the Files API. |
| Prompt caching | The system prompt and tool list never change. Anything that changes (the time, memory's file list, Vivy's mood) comes from tools or goes after the newest question. |
| Storage | One SQLite file in WAL mode, raw SQL through `rusqlite` on its own thread (`tokio-rusqlite`), per-feature migrations. No ORM, like voltgpt. |
| Config | Secrets in `.env`, everything else in `config.toml`. Per-feature gating is config, not code (voltgpt hardcoded its main server and blocked channels). Mistakes warn, never stop the bot. |
| Memory | A folder of notes the model manages itself with one tool, instead of voltgpt's summaries pasted into every prompt. The bot's own `memory` tool on both providers, not Claude's built-in one (that one filled people's files with one-off tasks). |
| Skills | Per-server how-to notes in `/memories/vivy/skills/`, not Anthropic's Skills API (workspace-wide, and changing a skill changes every request). |
| Vivy's own notes | `/memories/vivy/` (her personality, mood, face, skills) changes only from what she herself thinks. People can't order her mood. This is done through her instructions, not an author lock. |
| Logging | `tracing`, plus a log channel in Discord. Sentry is put off for now. |
| Running it | A systemd service (`deploy/vivy.service`, `/opt/vivy`) is the main way, but any way works, since the files are found relative to each other. |

## From voltgpt

| voltgpt | Vivy | Where |
|---|---|---|
| AI chat (mention the bot, streamed answer) | Ported and extended: provider trait, own history, tools, ❌/🔁, chime-ins, follow-ups | `chat/`, `ai/` |
| Reminders | Ported with a real grammar, per-user timezones, snooze buttons, retries | `reminders/` |
| Movie wheel (`gamble/`) | Ported with tables instead of a JSON blob, seasons, pool betting, a picture, undo winner | `wheel/` |
| Image hashing (`hasher/`, `/hash_server`, Check Snail) | Redesigned: link keys plus crop-tolerant picture fingerprints, admin-started backfill | `snails/` |
| Long-term memory (notes, profiles, vector search, `memory_*` commands) | Replaced by the notes folder and `memory` tool | `memory/` |
| Image and video generation (`/draw`, `/video`, Wavespeed) | Pictures came back as a chat tool on GPT Image 2.5; video is dropped | `drawing/`, `ai/openai/images.rs` |
| `response_ids` (OpenAI response IDs) | Replaced by `chat_turns`, which works with any provider | `chat/store.rs` |
| New in Vivy | Voice messages, moods, faces, banners, nickname, diary, the control panel, Claude, spend tracking for Claude and OpenAI | |

voltgpt's reminders and running wheel game are imported once from `old.db` (see [architecture.md](architecture.md#importing-from-voltgpt)). Its image hashes, chat history and memory are not.

## How it was built

The port went in stages, one pull request each, all merged into `main`.

| PR | Stage | What it added |
|---|---|---|
| #1 | Plan | This feature map, as a plan |
| #2 | 1 | Core (config, database, dispatcher, logging), the control panel, reminders |
| #3 | 2 | Shared helpers: splitting, live replies, media, ffmpeg frames |
| #4 | 3 | Chat on OpenAI, with tools and reactions |
| #5 | 4 | The movie wheel |
| #6 | 5 | Memory, chime-ins, follow-ups, emoji, reflection, mood, diary |
| #7 | | Docs: Cherry's name |
| #8 | 6 | Snail detection |
| #9 | | Attached files for OpenAI's model and code interpreter |
| #10 | 7 | The Claude provider |
| #11 | | Memory: the bot's own memory tool instead of Claude's |
| #12 | | Memory: the skills folder |
| #13 | | Fallback between providers, spend tracking, diary in several channels |
| #14 | | The status message as a picture with graphs |
| #15 | | Mood faces, banners, voice messages |
| #16 | | Fixes from a full bug check, with Cherry's decisions |
| #17 | | Config typo check for memory's split settings |
| #18 | | Mood in chat, the nickname, the chat prompt's style rules removed |
| #19 | | Only standard emoji in the nickname |
| #20 | | Renamed from VoltBot to Vivy |
| #21 | | README rewrite, version 0.2.0 |
| #22 | | Timers on the status picture; snail picture retries capped at 3 |
| #23 | | Docs: the feature map as an overview, a page per feature |
| #24 | | Drawing with GPT Image 2.5; OpenAI spend and bill next to Claude's |
| #25 | | Claude's thinking survives bot updates that change the prompt or tools |
| #26 | | Background AI jobs run cheaper (settings per job); chime-ins share the chat cache; hour-long prompt cache |
| #27 | | Speed and reliability: shared retries with Retry-After, parallel loading, snail scan off the database thread |
| #28 | | Wheel menus past 25 names, reminder Cancel and Snooze… buttons, repeating reminders, friendly tool status lines, `/snails` leaderboard |
| #29 | | Code cleanup: big files split, shared helpers, tracing-log, less work per message |
| #31 | | `look_at_profile_picture`: Vivy can see people's profile pictures and her own face; tool results can carry pictures |
| #32 | | Wheel: the movie each person picked, set with `/wheel_add` or `/wheel_movie`, shown on the picture and in the menus |
| #36 | | Newer web search and fetch tools: filtered results stay out of the chat history |
| | | Chat's prompt cache kept warm with a ping 58 minutes after the last use; savings on the status picture |
| #35 | | Vivy's progress notes between tool calls, in her own voice, show above the chat status line |
| | | Chat status names the web search query and the site being read; text after a search starts a new paragraph |
| #38 | | OpenAI: one prompt cache key for chat in every channel, so a quiet channel reads what a busy one cached |
| #39 | | Claude: at most 5 web searches per request (`web_search_max_uses`) |
| #40 | | UX run: shorter button labels, plainer messages and errors, first-person `/memory` replies, Check Snail lines that fit a phone |
| #41 | | UX run: the new round posted after a winner, admin commands redraw the wheel, `/memory forget` asks first, dates in `/reminders` |
| #42 | | UX run: reminder times the way people say them (`in an hour and a half`, `tomorrow evening`, `on the 15th`, `every other week`), clearer refusals, repeats shown in your own timezone, `/timezone` suggests abbreviations |
| #43 | | UX run: chat replies start with "Thinking…" before downloads, the status line stays out of code blocks, long answers fill each message, HEIC/AVIF photos converted, "make it night" finds the picture from any message of the answer, ❌ taken off after a stop |
| #44 | | UX run: startup warnings for an empty `admins`, an unreadable `faces_dir`/`banners_dir` and an `effort` the API would refuse; a bad `log_level` falls back to warn; a wrong status channel warns once; `log_channel`/`log_level`/`status_interval_seconds` key names (old ones still work); config warnings labelled `config`; the example config no longer turns on `fallback`, `service_tier` or a `voice_id` |
| #45 | | UX run: the wheel picture drawn for a phone: bigger text, no money bar, "no bets yet" rows, the tax rule as one line |
| #47 | | Command names: the wheel commands are `/wheel status`, `/wheel add`, `/wheel movie`, `/wheel bet` and `/wheel reset` (were `/wheel_status`, `/wheel_add`, `/wheel_movie`, `/insert_bet`, `/reset_wheel`); `/snails` became `/snails top` and `/snail_backfill` became `/snails backfill` |
| #48 | | Chime-ins: she reads the last 15 messages (was 25), 4% chance per message (was 3%), 30-minute cooldown per channel (was 60) |
| #49 | | Chat: Slack-style `<url\|text>` links become Discord masked links, so they no longer show as one broken link; the prompt asks for `[text](url)` |
| #50 | | Snails: a flat picture under 3 pixels on a side (a 1×1 pixel) no longer crashes the fingerprint; it used to be trimmed down to nothing |
| #51 | | Chat: `get_user_info` shows a member's online status and current activities (playing, listening, custom status); needs the Presence Intent turned on in the Developer Portal |
| #52 | | Chat: a progress note with nothing written after it (her answer, when a tool call followed it) stays in the reply instead of disappearing |
| #53 | | Chat: threads and forum posts mentioned or linked in a message show their real name to her (with the channel they're in), instead of `#deleted-channel`; a channel she can't see says so |

## Not built yet

| Idea | Notes |
|---|---|
| Remove the `old.db` import | Once Cherry has confirmed the imports. What to delete is listed in [architecture.md](architecture.md#importing-from-voltgpt). |
| Gemini provider | Another `ChatProvider`; [ai.md](ai.md) has the steps. |
| Sentry | Error grouping through the `sentry` crate's `tracing` integration, on when `SENTRY_DSN` is set. GlitchTip is a self-hostable server for it. |
| Wheel: `/wheel_spin` and balance history | A random winner with an animation, and each player's money over time |
| More faces | Mischievous, grossed out, lonely, celebrating |
| Listeners for bot events | `ReminderFired` and `WheelRoundResolved` are published, but nothing listens yet |
| Voice channels | Not asked for; voice is messages only |
