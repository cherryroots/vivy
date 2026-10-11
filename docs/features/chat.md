# Chat

Chat is how people talk to Vivy. Mention her with a question (with pictures, GIFs, videos or files if you like) and she answers in a reply that streams in as it's written. She can look things up with tools: the time, the channel, older messages, the server's emoji, reminders, her memory. Replying to one of her answers continues the conversation. Now and then she also chimes in on her own, and she can plan to check in with someone later.

The AI providers themselves (Claude, OpenAI, fallback, spend) live in the `ai` module, described in [ai](../ai.md). This doc is about the Discord side.

## Using it

- **Ask:** `@Vivy what's in this picture?` Any mention that no other feature claims comes to chat (reminders claims "remind me", for example). A mention another feature can't make sense of is passed on to chat too.
- **Continue:** reply to any message of an answer, mentioning her (Discord's reply ping counts). You can also reply to someone else's message, or an old voltgpt answer, and mention her: that message becomes the start of the conversation.
- **❌** on an answer: stops it while it's being written, deletes it once it's done.
- **🔁** on an answer: writes it again in the same messages. The bot removes your 🔁 so you can use it again.
- Only the person who asked can use ❌ and 🔁.

Settings live under `[features.chat]` in `config.toml`: the common `enabled` and server/channel gating keys, plus `chime_chance` and `chime_cooldown_minutes`. `config.example.toml` explains each one. The AI keys and provider settings are under `[ai]` (see [ai](../ai.md)). Setup is in the README.

## Files

All under `src/features/chat/`, in a good reading order:

| File | What it holds |
|---|---|
| `mod.rs` | The `Chat` struct and its `Feature` impl: `on_mention`, reactions (❌ / 🔁), the list of running answers, saving the question and answer turns |
| `answer.rs` | One answer from start to end: the streaming loop, the status line, running tool calls, attaching generated files. Also `tools_for` and `context_for`, which collect tools and context from every feature |
| `history.rs` | Turning a Discord message into stored parts (`read_message`), and a stored chain into model input (`build_input`), including refreshing expired Discord links |
| `store.rs` | The migrations and every SQL query: `chat_turns`, `chat_messages`, `chat_follow_ups`, `chat_emoji` |
| `tools.rs` | Chat's own tools (time, channel, users, messages, pins, events) and helpers other files reuse (`format_message`, `parse_link`, `can_read`) |
| `search.rs` | The `search_messages` tool, which calls Discord's search endpoint directly |
| `chime.rs` | Chiming in without being mentioned, and `speak`, which follow-ups reuse |
| `follow_up.rs` | The `schedule_follow_up` tool and the task that delivers check-ins |
| `emoji.rs` | The `list_server_emoji` tool and the task that describes each custom emoji once |
| `prompt.md` | The system prompt, built into the binary with `include_str!` |

Chat also leans on a few helpers in `src/util/` (described in [architecture](../architecture.md)): `util::reply::LiveReply` (a reply that spans several messages and is edited in place), `util::split::split_message` (Discord-sized parts that keep code blocks intact), `util::media` (finding and loading pictures, GIFs, videos and files) and `util::text` (attachment and embed text).

## How it works

### An answer from start to end

1. The dispatcher (`src/core/dispatcher.rs`) sees a message from a human that mentions the bot. It strips the mention, finds the feature with the longest matching mention prefix, and calls its `on_mention`. Chat declares the empty prefix `""`, so it gets every mention nobody else claimed, and every mention a feature returned `Mention::PassOn` for.
2. `Chat::on_mention` (`mod.rs`) takes a provider with `ctx.ai.chat()`. With no AI key it returns a user error: "Chat is turned off: the bot has no AI key."
3. `media::with_previews` waits two seconds and reads the message again if it has links but no embeds yet, since Discord adds link previews (and the GIF of a GIF link) a moment later. Links written as `<link>` never get a preview, so a message with only those (or with previews turned off) isn't waited for.
4. If the message is a reply, `history::turn_for_reference` finds the replied-to message's turn, or saves that message as a new turn without a parent.
5. The question is saved as a turn (`store::add_turn`), with the message's text and links to its media and files (`history::read_message`).
6. `Chat::answer` registers the answer as running (for ❌), loads the chain of turns up to 40 long (`store::chain`), and builds the model input (`history::build_input`).
7. The first time a question is answered, chat asks every enabled feature for its `chat_context` (memory's file list and mood, for example) and saves the result on the question as `Context` parts. Later requests (🔁, or the rest of the conversation) send the question exactly as before, which keeps the prompt cache valid.
8. `answer::run` streams the answer into Discord, runs tools, and attaches files (see below).
9. The answer is saved as a turn whose parent is the question, with every Discord message it used linked to it in `chat_messages`.

### The streaming loop and tools

`answer::stream_answer` is chat's own tool loop. Each round sends a `ChatRequest` with:

- `system`: `SYSTEM_PROMPT` from `prompt.md`. It never changes, so every request starts the same and hits the prompt cache.
- `tools`: from `answer::tools_for`, the tools of every feature enabled in this server and channel, in feature order.
- `cache_key`: `discord:<channel id>`, so one channel shares a cache.
- `job`: `"chat"`, for the cost per job (see [ai](../ai.md)).

Inside a round, a `tokio::select!` waits on three things at once: the ❌ cancellation token, a one-second tick that redraws the reply, and the next event from the provider. (`select!` runs whichever finishes first.) Text deltas are appended; a new round of text starts a new paragraph. When the `Done` event lists tool calls, each one runs through `answer::run_tool`, which finds the feature that owns the tool and calls its `run_tool`. Errors go back to the model as `Error: ...` text, so it can fix its call. Then `ai::next_input` builds the next round's input and the loop goes on. After 8 rounds (`MAX_ROUNDS`) the answer fails.

When the model is done, files it made (code interpreter or code execution) are downloaded with `provider.download_file` and attached to the last message, at most 10. `sandbox:/mnt/data/plot.png` links in the text are pointed at the matching Discord attachment (`link_files`). An answer that is only tools and no text shows 🤷, unless a tool already posted something (like a voice message).

### The status line

The reply's last line is a small status in Discord's `-#` subtext markdown, redrawn with the same once-a-second edit that streams the text:

| Status | When |
|---|---|
| `-# 💭 Thinking…` | Before any text, and after tools ran |
| `-# 🔎 Searching the web…` | The provider's web search or fetch |
| `-# 🐍 Running code…` | The code interpreter or code execution |
| `-# ✍️ Writing…` | While text streams |
| `-# 🔧 Read recent messages…` | One of the bot's tools runs, named after the tool (`tool_status` turns `read_recent_messages` into "Read recent messages") |

When the answer is done the line goes away, or becomes `-# ⏹️ Stopped` (❌) or `-# ⚠️ Something went wrong: <short reason>`. Only the last message of a long answer carries it; `render` keeps 100 characters free there (`STATUS_ROOM`). `LiveReply::show` only edits messages whose text changed, so a tick with nothing new costs no API call.

Replies ping only the person they reply to, whatever the text says, and are sent with the `SUPPRESS_EMBEDS` flag so links don't make previews (`src/util/reply.rs`).

### History: `chat_turns` and `chat_messages`

Every message the bot answers and every answer gets a turn in `chat_turns`. A turn has its own ID rather than a Discord message ID, because a long answer spans several messages (all of them point at the turn through `chat_messages`, so replying to any part continues the conversation), and a regenerated answer reuses the old answer's messages.

`content_json` holds `StoredPart`s (`store.rs`), which are provider-neutral:

- `Text`: the message text. Messages from people are wrapped as `<user name="..." id="...">...</user>`, with attachment and embed text inside. `break_user_tags` escapes `<user` and `</user` in the text, so nobody can fake a message from someone else. Mentions become names through serenity's `content_safe`.
- `Media`: a link to a picture, GIF or video (at most 8 per message), with its kind and MIME type.
- `File`: a link to any other attachment, like a PDF or spreadsheet (at most 10 per message, 50 MB each).
- `Context`: what features added to the question when it was first answered.

Answers also store `provider`, `model`, `continuation_id` (OpenAI's response ID) and `native_json`: the provider's raw output round by round, with the tools' results. The [ai](../ai.md) doc explains how providers replay it. `native_json` is cleared once an answer is 60 days old (`store::forget_old_rounds`, `NATIVE_KEEP_DAYS`, run by the daily emoji loop): it is only needed to replay recent conversations exactly, and an answer without it is sent as its text.

`history::build_input` picks what to send:

- If an answer in the chain was written by this same provider and model and has a `continuation_id`, it sends `Input::After` with that ID and only the turns after it (OpenAI).
- Otherwise it sends `Input::Full` with the whole chain.

Turning stored turns into model turns (`to_model_turns`) downloads the media again. Discord attachment links expire, so they are refreshed first through Discord's `attachments/refresh-urls` endpoint. A provider that is sent everything every time (`sends_full_history`, Claude) gets every turn's media, so earlier turns never change. Others get media only for the newest 4 turns that have some (`MEDIA_TURNS`); older ones say `[an earlier image]`. Files are always downloaded. Up to 4 downloads run at once (`PARALLEL_LOADS`). Media that can't be loaded becomes a short note, so the model knows something was there.

Turns are never deleted or rewritten, with one exception: the `Context` parts are appended to a question once, the first time it's answered (`store::add_parts`). 🔁 adds a new answer turn under the same question, and `add_turn` moves the old answer's messages over to it (`INSERT OR REPLACE` into `chat_messages`).

### Reactions

`Chat::on_reaction_add` only looks at unicode reactions on the bot's own messages.

- **❌ on a running answer** cancels its `CancellationToken` (from `tokio-util`; anything waiting on `cancelled()` wakes up). Running answers are kept in `Chat::running` with the asker and the answer's message IDs. A `Registered` guard removes the entry when `answer` returns, even after an error or a panic (its `Drop` impl runs either way).
- **❌ on a finished answer** looks up the answer through `chat_messages` (`find_answer`), checks the asker wrote the question, and deletes every message of the answer, including ones tools posted.
- **🔁** finds the answer the same way, removes the reaction, and calls `answer` again with `LiveReply::resume`, which takes over the old messages and edits them. Voice messages can't be edited into text, so they are deleted. A 🔁 while the answer is still running does nothing.

### Media and GIFs

`util::media::find_media` finds pictures, GIFs and videos in attachments and embeds. GIFs from Discord's picker (Klipy, Tenor, Giphy) are a link to the GIF's page plus a `gifv` embed with an MP4 `video` and a still `thumbnail`; the MP4 is read and the still is skipped. `media::load_for_model` turns GIFs and videos into a few PNG grids of frames with one `ffmpeg` run (`util::frames`), since models read still images. Other attachments (`find_files`) become `File` parts; what each provider does with them is in [ai](../ai.md).

### Chiming in

`chime.rs`. After every message in a server (`on_message`), `Chime::roll` rolls `chime_chance` (default 0.03). A hit starts the channel's cooldown (`chime_cooldown_minutes`, default 60) right away, so two quick messages can't both start one. Messages that mention her or reply to her are skipped, since they get a real answer.

On a hit, `speak`:

1. Reads the last 25 messages (`read_along`) into a `<recent_messages>` transcript.
2. Builds one question: the transcript plus `INSTRUCTIONS` (pass, react, or one short line; or a short question when she doesn't know what people mean). The instructions are in the question, not the system prompt, so the system prompt is the same as for answers and shares their cache. Features' `chat_context` is added with `fresh = true`.
3. Runs it with `ai::complete` (the tool loop without Discord output, at most 4 rounds) and `job: "chime"`.
4. Reads the answer with `decide`: `PASS`, `REACT <emoji>` (reacts to the last message), or a line (cut to 400 characters) that is posted to the channel.

Nobody asked and the line is public, so she only gets `UNPROMPTED_TOOLS`, an allow-list: no reminders, no `schedule_follow_up`, no `list_channels` (it lists what one person can see). The `Runner` keeps `get_message` and `search_messages` to the current channel (`here_only`). Tools run as the author of the last message, except `memory` in a server, which runs as Vivy herself, so the memory change log names her.

A posted line is saved as a question turn (the transcript, exactly as the model read it) and an answer turn, so replying to her line continues the conversation. The question's author is the bot, so nobody can 🔁 or ❌ her line as if it were their answer.

### Follow-ups

`follow_up.rs`. The `schedule_follow_up` tool takes `hours_from_now` (0.1 to 1440, so at most 60 days) and a `note`. It is always with the person who asked: there is no `user` argument, because delivery reads the channel as that person. At most 5 can be planned per person. The row goes into `chat_follow_ups` and the delivery task is woken through a `tokio::sync::Notify`.

The delivery task sleeps until the next one is due (at most an hour), takes every due row out of the table (`take_due_follow_ups`), and for each one calls `chime::speak` in that channel. The instructions tell the model to answer `PASS` if it was already talked about, otherwise to write one warm message starting with the person's mention, which is the only mention allowed. Each check-in is tried once, and skipped if chat is no longer enabled in that server or channel. Since it goes through `speak`, it gets the same limited tools as a chime-in.

### Server emoji

`emoji.rs`. A task waits 30 seconds after start (so the cache has the servers), then once a day describes every custom emoji that has no row in `chat_emoji` yet, in every server where chat is enabled. It loads the picture from Discord's CDN (frames for an animated one) and asks the model for one line with a small system prompt of its own, no tools and `job: "emoji"`. So every emoji is described on the first start, and later only new ones. `list_server_emoji` returns the server's emoji codes (at most 250) with their descriptions.

## Chat tools

Besides the provider's built-in tools (web search and code, see [ai](../ai.md)), the model gets the bot's own function tools. Each feature contributes tools through `Feature::tools`, and chat offers the tools of every feature enabled where the conversation happens. A tool runs as the person who asked (`Asker`, in `src/core/feature.rs`): it only reads channels they can read and only changes their own things.

Chat's own tools:

| Tool | File | What it returns or does |
|---|---|---|
| `get_current_time` | `tools.rs` | The date and time in the asker's timezone (set with `/timezone`), or in an IANA zone the model gives |
| `get_channel_info` | `tools.rs` | The channel's name, topic and type, its parent channel or category, and the server name |
| `list_channels` | `tools.rs` | Every channel the asker can see, by category, with topics |
| `get_user_info` | `tools.rs` | A member's names, ID, roles, join date and timezone (the asker when no `user` is given) |
| `read_recent_messages` | `tools.rs` | The messages just before the one being answered, 1 to 50 (default 20), oldest first |
| `get_message` | `tools.rs` | One message from a Discord message link, if the asker can read that channel |
| `search_messages` | `search.rs` | Discord's server-wide search (words, author, channel, attachment type, dates, sort), filtered to channels the asker can read, with message links. Waits once if Discord is still indexing |
| `get_pinned_messages` | `tools.rs` | The current channel's pins (at most 25), newest first, with links |
| `list_server_events` | `tools.rs` | The server's upcoming and ongoing scheduled events |
| `list_server_emoji` | `emoji.rs` | The server's custom emoji, with a description of each picture |
| `schedule_follow_up` | `follow_up.rs` | Plans a check-in with the asker after something they mentioned |

Tools from other features:

| Tool | Feature | What it does |
|---|---|---|
| `create_reminder` | [reminders](reminders.md) | Creates a reminder; `when` is text like "in 2h30m" or "next friday" |
| `list_reminders` | [reminders](reminders.md) | The asker's pending reminders, with IDs |
| `cancel_reminder` | [reminders](reminders.md) | Deletes one of the asker's reminders |
| `get_wheel_status` | [wheel](wheel.md) | The movie wheel's round, options, players, bets and payouts (read only) |
| `memory` | [memory](memory.md) | View, create, str_replace, insert, delete and rename notes under `/memories` |
| `set_mood` | [memory](memory.md) | Changes her mood, status and face in this server when a conversation moves her |
| `send_voice_message` | [voice](voice.md) | Speaks a short script and posts it as a voice message replying to the asker. Only offered when `ELEVENLABS_API_KEY` is set |

A tool that posts its own message (like the voice tool) records it in `Asker::posted`. Chat then treats it as part of the answer: it's linked to the answer turn, ❌ deletes it, and its text goes into the history.

## Feature hooks it uses

| Hook | What chat does with it |
|---|---|
| `name` | `"chat"` |
| `migrations` | `store::MIGRATIONS` (two migrations, see below) |
| `mention_prefixes` | `[""]`: every mention no other feature claimed |
| `stats` | "Answers" (answer turns so far) and "Follow-ups" (check-ins planned) for the status message |
| `panels` | The next planned check-in in a server, in the box about Vivy on the status message |
| `tools` | Chat's tools (`tools::defs`, plus `emoji::def` and `follow_up::def`) |
| `run_tool` | Runs them: `list_server_emoji` and `schedule_follow_up` directly, the rest through `tools::run` |
| `start` | Reads the chime settings (so typos are warned about at startup), starts the follow-up and emoji tasks |
| `on_mention` | Answers a question |
| `on_message` | Maybe chimes in |
| `on_reaction_add` | ❌ and 🔁 |

## Tables

From `store::MIGRATIONS` in `store.rs`:

| Migration | Table | Columns | What for |
|---|---|---|---|
| 1 | `chat_turns` | ID, parent turn ID, role (`user`/`assistant`), author, channel, `content_json` (stored parts), `native_json` (the provider's raw output), provider, model, `continuation_id`, created time | One row per question or answer; a conversation is the chain from a turn up through `parent_id` |
| 1 | `chat_messages` | Discord message ID, turn ID | Which turn each Discord message belongs to |
| 2 | `chat_follow_ups` | ID, server (NULL in DMs), channel, user, the message it was planned from, note, due time, created time | Planned check-ins |
| 2 | `chat_emoji` | Emoji ID, server, name, description | One line describing each custom emoji |

## Background loops

| Timer (`ctx.timers.add`) | Schedule | File |
|---|---|---|
| `"Check-ins"` | `"at the next check-in"` (at most an hour; woken when one is planned) | `follow_up.rs` |
| `"Emoji descriptions"` | `"daily"` (first run 30 seconds after start) | `emoji.rs` |

Chime-ins have no loop: they run from `on_message`.

## Compared to voltgpt

- voltgpt kept one OpenAI response ID per Discord message (`response_ids`) and continued from it, which only works with OpenAI. Vivy keeps its own provider-neutral history in `chat_turns`, so switching providers in the middle of a conversation works, and OpenAI's response ID is an optional shortcut stored next to it.
- voltgpt added ⏳ and ✅ reactions while working. Vivy shows the state in the status line instead, which also saves two reaction API calls per answer.
- voltgpt appended the time, channel name and memory context to the system prompt, so the prompt cache broke right there. Vivy's system prompt is static: the model looks up the time and channel with tools, and per-question context goes after the question.
