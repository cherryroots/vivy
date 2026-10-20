# Vivy feature map

What Vivy does today, where each feature lives in the code, what happened to each part of voltgpt (the Go bot Vivy replaced), and what is left to build.

## The docs

| Doc | What's in it |
|---|---|
| [README](../README.md) | What each feature does for people, and how to set the bot up |
| `config.example.toml` | Every setting, explained |
| This page | The overview: features, tables, decisions, history, what's next |
| [architecture.md](architecture.md) | How the code fits together: startup, the `Feature` trait, the dispatcher, config, database, logging, adding a feature, where to start reading |
| [ai.md](ai.md) | The AI providers (Claude, OpenAI, Gemini), pictures, conversation history, fallback and spend |
| [features/chat.md](features/chat.md) | Chat: answers, tools, chime-ins, follow-ups, server emoji |
| [features/reminders.md](features/reminders.md) | Reminders: the time grammar, the scheduler, snooze |
| [features/wheel.md](features/wheel.md) | The movie wheel: the money rules, seasons, the round picture |
| [features/memory.md](features/memory.md) | Memory: the notes folder, reflection, mood, faces, banners, nickname, diary |
| [features/voice.md](features/voice.md) | Voice messages through ElevenLabs |
| [features/drawing.md](features/drawing.md) | Drawing pictures with GPT Image 2.5 |
| [features/snails.md](features/snails.md) | Repost detection: link keys, picture fingerprints, backfill |
| [features/control-panel.md](features/control-panel.md) | The status picture and the log channel |
| [features/tinyfish.md](features/tinyfish.md) | Web search and reading pages with TinyFish |
| [features/who-said-it.md](features/who-said-it.md) | Who said it?: the game, where quotes come from, why it isn't an Activity |
| [features/hunger-games.md](features/hunger-games.md) | The Hunger Games: the Reaping, rounds Vivy writes, the rules that check them |
| [features/gameboy.md](features/gameboy.md) | The Game Boy: the emulator, playing it by hand, and the stages to Vivy playing by herself |

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
| Snails | `snails/` | Quietly notes every link and picture, so reposts can be found and counted | "Check Snail" (right-click a message → Apps), `/snails top`, `/snails reset`, `/snails backfill` |
| Control panel | `control_panel/` | A status picture kept up to date in a channel | Watching the channel |
| TinyFish | `tinyfish/` | Vivy searches the web and reads pages (Reddit too) with TinyFish, with any chat provider | The `tinyfish_search` and `tinyfish_fetch` chat tools |
| Games | `games/` | `/games` and its games: who said it? (`who_said_it/`), where everyone in a channel guesses who wrote old messages from it, another public channel or the whole server; the Hunger Games (`hunger_games/`), a battle royale between members that Vivy writes as the Gamemaker; and a Game Boy emulator (`gameboy/`) that admins play by hand for now, with code that reads Pokémon Red and walks her anywhere on a map, until Vivy learns to play it | `/games whosaidit`, `/games hungergames`, `/games leaderboard`, `/games gameboy ...` (with `look` and `walk`), their buttons |

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
| TinyFish | none (only its chat tools) | none |
| Who said it? | its buttons | a task per running game |

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
| AI | One `ChatProvider` trait in `ai`, so any feature can call a model. Claude is the main provider, through the plain Messages API over `reqwest` (no Rust Agent SDK exists, and the Agent SDK is built for coding agents). OpenAI is the fallback, since its free data-sharing tokens would train on members' messages. Gemini (Interactions API) can be picked as either, and watches videos and YouTube links itself. Each provider's built-in tools are used fully: web search, web fetch, code execution, the Files API. |
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
| #54 | | Chat: `look_back_at_answers` lets Vivy read her own earlier answers (tools called, what came back, errors, model, time taken) to explain or debug what she did; failed answers keep their error |
| #55 | | Chat: a mention of Vivy later in a message ("stop being mean to @Vivy") is no longer removed, so she knows the message is about her; mentioned people show by their server nickname |
| #56 | | Chat: a still (one-frame) GIF reaches her as a picture instead of "ffmpeg found no frames"; a GIF ffmpeg can't read sends its first frame |
| #57 | | Chime-ins: `chime_chance_per_user` gives some people their own chance, `[user, chance]` anywhere or `[user, chance, channel]` in one channel |
| #58 | | Snails: `/snails reset` (admins) starts the leaderboard over and keeps the saved links and pictures; custom emoji, stickers, avatars and other Discord-owned pictures sent as links, and GIFs uploaded to or linked from Discord, no longer count as snails; only messages from the last 7 days count, so old messages reached by an edit, a retried picture or the catch-up no longer land on the board |
| #59 | | Who said it?: `/games whosaidit`, the channel guesses who wrote old messages with buttons, scores at the end |
| #60 | | Who said it?: `channel` and `whole_server` options; only channels everyone can read, and questions say which channel they're from |
| #61 | | Chime-ins: a line that ends with a quote keeps its closing quote (it was cut off, so lines looked unfinished); a line answering one of the messages she read is posted as a reply to it (no ping) |
| #62 | | Chime-ins: 5% of messages make her read along to learn for her memory; of those reads 10% she posts a line, 40% she reacts, the rest she only reads (`chime_say_chance`, `chime_react_chance`) |
| #63 | | Who said it?: `years` option (1 to 20) for how far back quotes can come from; empty uses `lookback_days` (1 year) |
| #64 | | Gemini provider (`provider = "gemini"`, Gemini 4 Argon through the Interactions API): Google Search, URL context and code execution; posted videos uploaded with the Files API and YouTube links watched as videos |
| #65 | | TinyFish: chat's `tinyfish_search` and `tinyfish_fetch` tools (any provider, chime-ins too) when `TINYFISH_API_KEY` is in `.env`, so she can search and read Reddit and other sites the built-in web tools can't reach; `[features.tinyfish]` sets results per search, location, language and how much of a page she reads; her prompt sends Reddit and pages her own fetch can't read to TinyFish |
| #66 | | Snails: `/snails recent` lists the server's last 10 snails with jump links to each message and the first post it repeated (new columns on `snail_caught`) |
| #67 | | Hunger Games (stage 1): `/games hungergames` opens the Reaping (host picks tributes, others volunteer), then Vivy writes every round as the Gamemaker, checked by the rules (everyone in one event, pacing, a final two), with cannons after each night and a victor with placings; the arena is the server's public channels |
| #68 | | Hunger Games (stage 2): the fallen get last words (a real message of theirs, found with Discord's message search in the public channels, behind spoilers), the final showdown's loser too; each round she gets her memory notes about the living tributes, through the `memory` tool, to use only where they're fun (the host can turn them off with a Memories button in the Reaping). Message search moved to `util::search` with every API filter, and `search_messages` offers them all |
| #69 | | Hunger Games (stage 3): her Gamemaker lines (opening, Feast, final showdown, victor) also become voice messages in the Gamemaker's own voice (`[features.voice.voices]`, `gamemaker`), through the voice tool so the monthly limit counts them; audio tags steer them and aren't shown. Both games move into a games feature (`games/who_said_it/`, `games/hunger_games/`) with settings at `[features.games]`; the Reaping's Memories button now starts off |
| #70 | | Hunger Games (stage 4): cannons show each fallen tribute's kills; rounds move on by themselves after 45 s (no Next button); with the Reaping's new Pictures button (off by default) each round's message comes with a top-down 2D map of the arena (labelled zones, the tributes as pixel-art sprites where each event happens, nights included), drawn while people read the round before, the final showdown's map is zoomed in on its zone, and the victor message gets a full-body picture of the winner with everyone they killed lying dead around them; each tribute's sprite and each zone's terrain are described once from the profile pictures and places so every picture matches; the drawing tool gains hidden `post: false` and `job` arguments, and the games' pictures cost on their own `game_art` line in OpenAI's spend
| #71 | | Hunger Games (stage 5): finished games are saved (`games_hunger_games`, `games_hunger_tributes`: each tribute's place, kills and days lived), and `/games leaderboard` shows the server's top 10 by wins then kills, or one `member`'s record; the victor message points to it |
| #72 | | Status picture: the line under each spend card's bar (share, pace, cache hits, keep-warm) breaks between its parts when it's wider than the card, instead of running past its edge |
| #73 | | Hunger Games (Octagon part 1): the Bloodbath becomes the Scatter (everyone starts unarmed around the Octagon and runs for the zones, at most 2 die); weapons themed to the place they're found in are kept by the code (`finds`) and held by the sprites in pictures; alliances (`allies`, `leaves`, killing an ally ends yours) and about half of each round's events quiet ones; the Octagon replaces the supply camp in the middle of the map; arena places come from every channel Vivy can see (last words still only from public ones) |
| #74 | | Hunger Games (Octagon part 2): the Feast is gone; the Octagon opens after a day, by deaths (two in games of 12 or more, at about a third and two thirds fallen, one in games of 6 to 11, none below); the code picks who steps in by hidden nerve plus kills (the two boldest at least, four at most), they fight one-on-one bouts for a prize until a `champion` is left (losers die or yield), and nobody else dies; the final showdown is in the Octagon; tributes and places show as blue mentions and channel links in game messages without pinging; the cannons and the victor message are no longer behind spoilers |
| #75 | | Hunger Games (Octagon part 3): Octagon rounds and the final showdown are drawn zoomed in on the cage, with only its fighters (`looks::cage`); Octagon wins are saved (`octagons` column, migration 2), shown in the victor message and on `/games leaderboard` (and a member's record), and break ties after kills; the victor message names the cowards, everyone alive when an Octagon opened who never stepped in; voice and stage channels are no longer arena places; every champion wins a legendary prize weapon that wins nearly every fight for whoever carries it |
| #76 | | Chat: Vivy knows her owner (whoever owns the bot's Discord application; `admins` don't count); while owner mode is on (a 👑 button on the status message only the owner can press, saved in the core `bot_settings` table) their messages carry `owner="true"` and she follows them, mood and self notes included; with it off they're ordinary conversation; the `get_bot_owner` tool says who her owner is |
| #77 | | Hunger Games: before each Octagon the channel votes on the champion's prize with buttons (⚔️ legendary weapon, 🛡️ golden armour that keeps them alive for two rounds, 🐺 the hunt that makes them kill next round), enforced by the round check; the vote takes two thirds of the wait between rounds, with a countdown, and she writes the Octagon in the last third; the games' pictures are drawn at high quality; `/games resetleaderboard` (admins) starts the leaderboard over |
| #78 | | Hunger Games: the arena shrinks once half the tributes have fallen (places close by fire as more fall, the outer ones first, two stay open; shown in the round, the brief and the map); every kill is saved (`games_hunger_kills`, migration 3), so the next Games bring up old grudges in the intro and in her brief, mark revenge kills, and a member's record names their nemesis |
| #79 | | Hunger Games: team Games, a 👥 Teams toggle in the Reaping (6 or more tributes): random pairs (one alone when odd) who never kill each other, only the bolder of a pair steps into the Octagon, the final showdown is between the last two teams, and a team wins together (both 👑 and saved as 1st, a fallen teammate too); also: the arena starts shrinking at a third fallen and is down to two places by the end (4 alive in team Games), at most 8 places, games of 6 to 11 open their Octagon at a third fallen, the intro pings nobody, pictures must tag their tributes (prompts are logged), and a tribute she forgot to list in an event is filled in instead of costing a retry |
| #80 | | Hunger Games: each saved kill keeps the weapon, the place and whether it was in the Octagon (migration 4), so revenge lines, old grudges, her brief and the nemesis line call back to how it happened; no final showdown in the Octagon: the last two sides wander the burning arena (they may meet and settle it, or not yet; one more place burns each round) until one place is left (or 2 rounds), then must meet there for the showdown; zones that burned the round before are left off the map |
| #81 | | Hunger Games: a light theme each Games: Vivy picks a season name, a twist everyone knows and a surprise she springs once later, as she writes the Scatter; every later brief reminds her, and the Scatter's message shows the name and twist. Pictures: a medium map each round plus medium close-ups of the 3 most interesting events, drawn side by side; the victor stays high; Octagon close-ups only outside the cage; each area described at the start from her channel notes |
| #82 | | Settings to tune things that were fixed in code: Hunger Games `close_ups`, `picture_quality`, `victor_picture_quality`, `max_places`, `describe_areas`, `most_in_octagon`, `wander_rounds`; chat `max_tool_rounds`, `history_turns`, `follow_ups_per_person`; snails `count_within_days`; who said it `pause_seconds` |
| #83 | | Reminders: the "Tomorrow 9:00" snooze button is removed; 10 min, 1 hour and Pick a time… stay |
| #84 | | Chat: Discord message links in a question are opened for her (up to 3, as the asker, only channels they can read): author, time, whole text, text attachments, embeds and pictures come with the question as `<linked_messages>`; the prompt and `get_message`'s description say Discord links are hers to open and her web tools can't, so she stops saying she can't open them |
| #87 | | Game Boy (stage 1): `/games gameboy load`, `press`, `wait`, `screen`, `gif`, `save`, `restore` and `off` (admins) play a Game Boy or Game Boy Color ROM from `roms_dir` by hand, with GIFs of what happened; the `boytacean` emulator on its own thread, a frame at a time, save states in `saves_dir` |

## Not built yet

| Idea | Notes |
|---|---|
| Remove the `old.db` import | Once Cherry has confirmed the imports. What to delete is listed in [architecture.md](architecture.md#importing-from-voltgpt). |
| Sentry | Error grouping through the `sentry` crate's `tracing` integration, on when `SENTRY_DSN` is set. GlitchTip is a self-hostable server for it. |
| Wheel: `/wheel_spin` and balance history | A random winner with an animation, and each player's money over time |
| More faces | Mischievous, grossed out, lonely, celebrating |
| Listeners for bot events | `ReminderFired` and `WheelRoundResolved` are published, but nothing listens yet |
| Voice channels | Not asked for; voice is messages only |
