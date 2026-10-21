# Chat

Chat is how people talk to Vivy. Mention her with a question (with pictures, GIFs, videos or files if you like) and she answers in a reply that streams in as it's written. She can look things up with tools: the time, the channel, older messages, the server's emoji, reminders, her memory. Replying to one of her answers continues the conversation. Now and then she also chimes in on her own, and she can plan to check in with someone later.

The AI providers themselves (Claude, OpenAI, fallback, spend) live in the `ai` module, described in [ai](../ai.md). This doc is about the Discord side.

## Using it

- **Ask:** `@Vivy what's in this picture?` Any mention that no other feature claims comes to chat (reminders claims "remind me", for example). A mention another feature can't make sense of is passed on to chat too.
- **Continue:** reply to any message of an answer, mentioning her (Discord's reply ping counts). You can also reply to someone else's message, or an old voltgpt answer, and mention her: that message becomes the start of the conversation.
- **❌** on an answer: stops it while it's being written, deletes it once it's done. The status says so after a stop: "❌ again removes this, 🔁 retries".
- **🔁** on an answer: writes it again in the same messages.
- The bot takes your ❌ (after a stop) and 🔁 off again so you can use them again. That needs the Manage Messages permission; without it the reaction stays and you take it off yourself.
- Only the person who asked can use ❌ and 🔁.
- **Her owner** (whoever owns the bot's Discord application) can tell her what to do, even what nobody else can direct (her mood, her notes about herself, how she acts), while owner mode is on: the 👑 button on the status message (see [control panel](control-panel.md#owner-mode)). With it off, their messages are ordinary conversation. Anyone can ask her who her owner is. See [Her owner](#her-owner).

Settings live under `[features.chat]` in `config.toml`: the common `enabled` and server/channel gating keys, plus `max_tool_rounds` (8 rounds of tool calls per answer), `history_turns` (40 messages up a reply chain), `follow_ups_per_person` (5 planned check-ins), `chime_chance`, `chime_say_chance`, `chime_react_chance`, `chime_cooldown_minutes` and `chime_chance_per_user`. `config.example.toml` explains each one. The AI keys and provider settings are under `[ai]` (see [ai](../ai.md)). Setup is in the README.

## Files

All under `src/features/chat/`, in a good reading order:

| File | What it holds |
|---|---|
| `mod.rs` | The `Chat` struct and its `Feature` impl: `on_mention`, reactions (❌ / 🔁), the list of running answers, saving the question and answer turns |
| `answer.rs` | One answer from start to end: the streaming loop, the status line, running tool calls, attaching generated files. Also `tools_for` and `context_for`, which collect tools and context from every feature |
| `channels.rs` | Names for channel mentions (`<#id>`) and channel links in message text, threads and forum posts included |
| `history.rs` | Turning a Discord message into stored parts (`read_message`, and `linked_messages` for the messages it links to), and a stored chain into model input (`build_input`), including refreshing expired Discord links |
| `store.rs` | The migrations and every SQL query: `chat_turns`, `chat_messages`, `chat_follow_ups`, `chat_emoji` |
| `tools.rs` | Chat's own tools (time, channel, users, messages, pins, events) and helpers other files reuse (`format_message`, `parse_link`, `find_message_links`, `linked_message`, `can_read`) |
| `search.rs` | The `search_messages` tool: turns names, mentions, links and dates into the IDs `util::search` needs, and keeps the results to channels the asker can read |
| `look_back.rs` | The `look_back_at_answers` tool: her own earlier answers, listed or step by step, read from `chat_turns` |
| `chime.rs` | Chiming in without being mentioned, and `speak`, which follow-ups reuse |
| `follow_up.rs` | The `schedule_follow_up` tool and the task that delivers check-ins |
| `avatar.rs` | The `look_at_profile_picture` tool, which shows the model a profile picture |
| `emoji.rs` | The `list_server_emoji` tool and the task that describes each custom emoji once |
| `prompt.md` | The system prompt, built into the binary with `include_str!`. It holds only general guidance (who Vivy is, her mood and voice, how to explain things, the words to avoid, how messages and linked messages are tagged); what each tool is for and when to use it is in the tool's own description, so the two can't disagree. The exceptions are which web tool to pick (TinyFish) and that Discord message links are hers to open: without that line she tried her web tools on them, failed, and said she can't open Discord links. And one line on `web_search` called from her code: Anthropic's description of that function says it returns a list, but it returns a JSON string, so her first try at reading it broke |

Chat also leans on a few helpers in `src/util/` (described in [architecture](../architecture.md)): `util::reply::LiveReply` (a reply that spans several messages and is edited in place), `util::split::split_message` (Discord-sized parts that keep code blocks intact), `util::media` (finding and loading pictures, GIFs, videos and files) and `util::text` (attachment and embed text).

## How it works

### An answer from start to end

1. The dispatcher (`src/core/dispatcher.rs`) sees a message from a human that mentions the bot. It strips the mention, finds the feature with the longest matching mention prefix, and calls its `on_mention`. Chat declares the empty prefix `""`, so it gets every mention nobody else claimed, and every mention a feature returned `Mention::PassOn` for.
2. `Chat::on_mention` (`mod.rs`) takes a provider with `ctx.ai.chat()`. With no AI key it returns a user error: "Chat is turned off: the bot has no AI key." It then sends the reply right away as `-# 💭 Thinking…` (`answer::placeholder`), before the slow steps below, so a question with a few phone photos doesn't sit there in silence while they download.
3. `media::with_previews` waits two seconds and reads the message again if it has links but no embeds yet, since Discord adds link previews (and the GIF of a GIF link) a moment later. Links written as `<link>` never get a preview, so a message with only those (or with previews turned off) isn't waited for.
4. If the message is a reply, `history::turn_for_reference` finds the replied-to message's turn, or saves that message as a new turn without a parent.
5. The question is saved as a turn (`store::add_turn`), with the message's text and links to its media and files (`history::read_message`). Up to 3 Discord message links in it are opened too (`history::linked_messages`): a `<linked_messages>` part with each message's author, time, whole text, text attachments and embeds, then that message's pictures and files, so "@Vivy check this out <link>" needs no tool call. Each one is read as the asker (`tools::linked_message`, with the same `can_read` check as `get_message`); for one they can't read, or a deleted one, the part says why instead. A linked message's text can't fake a `<user>` tag, like a message's own text.
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

Inside a round, a `tokio::select!` waits on three things at once: the ❌ cancellation token, a one-second tick that redraws the reply, and the next event from the provider. (`select!` runs whichever finishes first.) Text deltas are appended; a new round of text starts a new paragraph, and so does text after a web search, code or thinking inside one response. When the `Done` event lists tool calls, each one runs through `answer::run_tool`, which finds the feature that owns the tool and calls its `run_tool`. Errors go back to the model as `Error: ...` text, so it can fix its call. Then `ai::next_input` builds the next round's input and the loop goes on. After `max_tool_rounds` rounds (8 by default) the answer fails.

When the model is done, files it made (code interpreter or code execution) are downloaded with `provider.download_file` and attached to the last message, at most 10. `sandbox:/mnt/data/plot.png` links in the text are pointed at the matching Discord attachment (`link_files`); a link to a file that wasn't attached becomes the plain file name, so nobody taps a dead link. An answer that is only tools and no text shows a small "(nothing to say this time; 🔁 asks again)" line, unless a tool already posted something (like a voice message).

### The status line

The reply's last line is a small status in Discord's `-#` subtext markdown, redrawn with the same once-a-second edit that streams the text:

| Status | When |
|---|---|
| `-# 💭 Thinking…` | Before any text, and after tools ran |
| `-# 🔎 Searching the web…` | The provider's web search or fetch |
| `-# 🔎 Searching “dino decks 2017”…` | A web search, once its query is known (on OpenAI, once the search is done) |
| `-# 📖 Reading ygoprodeck.com…` | A web fetch (Claude), with the site it reads |
| `-# 🐍 Running code…` | The code interpreter or code execution |
| `-# ✍️ Writing…` | While text streams |
| `-# 🎨 Drawing, this can take a minute…` | One of the bot's tools runs. The feature that owns the tool names the label (`Feature::tool_status`), like "🔍 Searching messages…", "📝 Remembering…" or "🎙️ Recording a voice message…". `schedule_follow_up` and `set_mood` show "💭 Thinking…", so a planned check-in stays a surprise |
| `-# 🔧 Read recent messages…` | A tool whose feature gives no label: the tool's name as words (`plain_tool_status`) |

Above the status, while she works through tools, goes her latest progress note: `-# 💬 Found the reminder, moving it to Friday now`. These are the short notes Claude Opus 5.5 writes between tool calls (what it just found, what it does next). Claude returns them as `thinking` blocks whose text is empty by default, which is why she used to look silent; chat requests now ask for them with `thinking.display: "updates"` (see [AI providers](../ai.md)), and `collect.rs` sends each one as a `ChatEvent::Progress`. The system prompt asks for these updates in her own voice and mood: a few words before the first tool, then what she found and what's next. On OpenAI, which has no such notes, the same instruction makes her write that text into the reply itself. Each note Claude writes is logged as `Claude progress note: …`, so `grep "progress note"` in the logs shows whether she wrote any. When she writes her updates as ordinary text instead (she has in every live test so far), that text stays in the reply, so people can read what she said. A new note replaces the old one, and the note goes away once she starts writing text, since the text says the rest. Notes nothing was written after stay in the reply when the answer is done (`unanswered`, `with_notes`): Opus 5.5 turns text written right before a tool call into a note even when it's the answer, like "My pick is pensivesnail…" followed by a tool call, with nothing after it. They're left out only when a tool's message (like a voice message) was the whole answer. Her other thinking stays hidden, and it costs nothing extra: thinking is billed the same whether its text comes back or not.

When the answer is done both lines go away, or the status becomes `-# ⏹️ Stopped · ❌ again removes this, 🔁 retries` (❌) or a `-# ⚠️` line in plain words (`friendly_error`: overloaded, out of credit, stuck in tool rounds, declined, or "something went wrong (it's logged)"), each saying that 🔁 tries again. The provider's own error text goes to the log, not the channel. Only the last message of a long answer carries them; `render` keeps 300 characters free there (`STATUS_ROOM`): up to 190 for the note, flattened to one line, and 95 for the status. The messages before the last fill the whole 2000. While a code block is still streaming, `render` closes it (`split::close_code_block`) before adding the lines, so the status isn't shown as code. `LiveReply::show` only edits messages whose text changed, so a tick with nothing new costs no API call.

Slack-style links the model sometimes writes, `<https://…|text>`, are turned into Discord masked links, `[text](<https://…>)`, before each edit and in chime-ins (`discord_links`); Discord would otherwise show the `|text` as part of a broken link.

Replies ping only the person they reply to, whatever the text says, and are sent with the `SUPPRESS_EMBEDS` flag so links don't make previews (`src/util/reply.rs`).

### History: `chat_turns` and `chat_messages`

Every message the bot answers and every answer gets a turn in `chat_turns`. A turn has its own ID rather than a Discord message ID, because a long answer spans several messages (all of them point at the turn through `chat_messages`, so replying to any part continues the conversation), and a regenerated answer reuses the old answer's messages.

`content_json` holds `StoredPart`s (`store.rs`), which are provider-neutral:

- `Text`: the message text. Messages from people are wrapped as `<user name="..." id="...">...</user>` (`history::user_tag`), with attachment and embed text inside, and `owner="true"` added for her owner's messages while owner mode is on (see [Her owner](#her-owner)). `break_user_tags` escapes `<user` and `</user` in the text, so nobody can fake a message from someone else. User and role mentions become names through `channels::name_people` (server nickname when the member is cached, then the nickname Discord sent with the mention, then their display name); @everyone and @here stay as written. Only a mention of Vivy at the very start is removed (by the dispatcher); one later in the text stays, so "stop being mean to @Vivy" still says who. Channel mentions and channel links get names from `channels.rs`, which also knows threads and forum posts (cache first, then Discord, for archived threads); a channel the bot can't see is called `#hidden-channel`, not `#deleted-channel`. The tool results that show messages (`format_message`) do the same.
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

`util::media::find_media` finds pictures, GIFs and videos in attachments and embeds. GIFs from Discord's picker (Klipy, Tenor, Giphy) are a link to the GIF's page plus a `gifv` embed with an MP4 `video` and a still `thumbnail`; the MP4 is read and the still is skipped. `media::load_for_model` turns GIFs and videos into a few PNG grids of frames with one `ffmpeg` run (`util::frames`), since models read still images. Pictures in formats models don't take (HEIC and HEIF from iPhones, AVIF, BMP, TIFF) are converted to PNG the same way (`frames::to_png`); whether HEIC works depends on the server's ffmpeg (7.1 or newer reads it). Other attachments (`find_files`) become `File` parts; what each provider does with them is in [ai](../ai.md). Pictures and files the model gets are also copied into its code sandbox when it has one, with a note saying each one's name there (pictures are `image-<hash>.png`, from `ModelImage::file_name`), so she can run code on a screenshot; the details per provider are in [ai](../ai.md) too.

A provider that watches videos itself (Gemini, `ChatProvider::watches_video`) gets videos as they are instead of frame grids: `history::load_part` downloads them (`media::download_video`, up to 100 MB) as `Part::Video`, and `history::youtube_videos` adds the YouTube links in the newest 4 messages that have any (at most 10, each once) as `Part::YouTube`, normalised by `media::youtube_link`. Picker GIFs are MP4s, so on Gemini they're watched as videos too. GIF files stay frame grids.

### Chiming in

`chime.rs`. After every message in a server (`on_message`), `Chime::roll` rolls `chime_chance` (default 0.05), or the author's own chance from `chime_chance_per_user` when they have one: `[user, chance]` entries apply anywhere, `[user, chance, channel]` entries in one channel and win over the others. A hit starts the channel's cooldown (`chime_cooldown_minutes`, default 30) right away, so two quick messages can't both start one. A second roll (`Settings::allowed`) decides what she may do besides reading: `chime_say_chance` (default 0.10) of the reads she may post a line (`Allowed::Anything`), `chime_react_chance` (default 0.40) she reacts to the last message (`Allowed::React`), and the rest she only reads (`Allowed::ReadOnly`), saving what's worth keeping to memory. Messages that mention her or reply to her are skipped, since they get a real answer.

On a hit, `speak`:

1. Reads the last 15 messages (`read_along`) into a `<recent_messages>` transcript, numbered from 1 (oldest) so she can name the one she answers.
2. Builds one question: the transcript plus `READ_ALONG` (save what's lasting to memory) and what she may do this time (`allowed_instructions`: only read and answer `DONE`; react with one emoji; or one short line, a short question when she doesn't know what people mean, with a reaction or `PASS` if no line fits; a line that answers one message starts with `REPLY <number>`). The instructions are in the question, not the system prompt, so the system prompt is the same as for answers and shares their cache. Features' `chat_context` is added with `fresh = true`.
3. Runs it with `ai::complete` (the tool loop without Discord output, at most 4 rounds) and `job: "chime"`. The request carries every tool, like an answer, so it starts the same way and reads the system prompt and tools from the prompt cache; `Runner` turns down any tool outside `UNPROMPTED_TOOLS`.
4. Reads the answer with `decide`: `PASS`, `REACT <emoji>` (reacts to the last message), or a line (cut to 400 characters) that is posted to the channel. `allowed` limits it: on a read-only roll nothing is posted, and on a reaction roll a line is dropped. Follow-ups pass `Allowed::Anything`. A line that starts with `REPLY <number>` is posted as a Discord reply to that message, without pinging its author; a number outside the transcript posts it as a plain message. Quotes around the whole line are taken off, but only when they wrap all of it (`unquote`), so a line that ends with a quote of its own keeps its closing quote.

Nobody asked and the line is public, so she can only use `UNPROMPTED_TOOLS`, an allow-list: no reminders, no `schedule_follow_up`, no `list_channels` (it lists what one person can see). The `Runner` keeps `get_message` and `search_messages` to the current channel (`here_only`). Tools run as the author of the last message, except `memory` in a server, which runs as Vivy herself, so the memory change log names her.

A posted line is saved (`save`) as a question turn (the transcript, exactly as the model read it) and an answer turn, so replying to her line continues the conversation. The question's author is the bot, so nobody can 🔁 or ❌ her line as if it were their answer. A reaction is saved the same way, with a note as the answer (`(reacted 🐄 to RickyWL: "I Am One Of…")`, `reaction_note`) and no Discord message linked to it, so `look_back_at_answers` lists it as a chime-in. Before this she once denied a 🐄 her account had put on someone's message, since nothing she could look at showed it.

### Follow-ups

`follow_up.rs`. The `schedule_follow_up` tool takes `hours_from_now` (0.1 to 1440, so at most 60 days) and a `note`. It is always with the person who asked: there is no `user` argument, because delivery reads the channel as that person. At most 5 can be planned per person. The row goes into `chat_follow_ups` and the delivery task is woken through a `tokio::sync::Notify`.

The delivery task sleeps until the next one is due (at most an hour), takes every due row out of the table (`take_due_follow_ups`), and for each one calls `chime::speak` in that channel. The instructions tell the model to answer `PASS` if it was already talked about, otherwise to write one warm message starting with the person's mention, which is the only mention allowed. Each check-in is tried once, and skipped if chat is no longer enabled in that server or channel. Since it goes through `speak`, it gets the same limited tools as a chime-in.

### Server emoji

`emoji.rs`. A task waits 30 seconds after start (so the cache has the servers), then once a day describes every custom emoji that has no row in `chat_emoji` yet, in every server where chat is enabled. It loads the picture from Discord's CDN (frames for an animated one) and asks the model for one line with a small system prompt of its own, no tools and `job: "emoji"` (Claude Haiku at low effort, see [Jobs](../ai.md#jobs)). So every emoji is described on the first start, and later only new ones. An emoji that fails isn't tried again until the bot restarts. `list_server_emoji` returns the server's emoji codes (at most 250) with their descriptions.

## Chat tools

Besides the provider's built-in tools (web search and code, see [ai](../ai.md)), the model gets the bot's own function tools. Each feature contributes tools through `Feature::tools`, and chat offers the tools of every feature enabled where the conversation happens. A tool runs as the person who asked (`Asker`, in `src/core/feature.rs`): it only reads channels they can read and only changes their own things.

Chat's own tools:

| Tool | File | What it returns or does |
|---|---|---|
| `get_current_time` | `tools.rs` | The date and time in the asker's timezone (set with `/timezone`), or in an IANA zone the model gives |
| `get_channel_info` | `tools.rs` | The channel's name, topic and type, its parent channel or category, and the server name |
| `list_channels` | `tools.rs` | Every channel the asker can see, by category, with topics |
| `get_user_info` | `tools.rs` | A member's names, ID, roles, join date, timezone, online status and current activities, custom status included (the asker when no `user` is given). Status comes from the Presence intent, so it's read from the cache. |
| `get_bot_owner` | `tools.rs` | Who her owner is: the people who own the bot's Discord application, with names and IDs, and whether the asker is one of them |
| `look_at_profile_picture` | `avatar.rs` | Shows the model someone's profile picture as a picture: their picture for this server if they set one, otherwise their main one (Discord's default when they have none). `me` is Vivy herself, so in a server it's her current face. Downloaded as a 512 px PNG (an animated one gives its first frame). Only the text stays in the history |
| `read_recent_messages` | `tools.rs` | The messages just before the one being answered, 1 to 50 (default 20), oldest first |
| `get_message` | `tools.rs` | One message from a Discord message link, if the asker can read that channel, with its whole text. For links that weren't in the question, like ones in an earlier message or a search result. Its description says her web tools can't open Discord links and this one can |
| `search_messages` | `search.rs` | Discord's server-wide search with every filter the API has: words (and how far apart they may be), authors and author types, channels, mentions of people, roles or @everyone, replies to people or messages, pinned, has (each can be negated, like `-link`), embed types and providers, link sites, attachment names and extensions, dates, sort, limit and offset for paging. Starts with how many match. Filtered to channels the asker can read, with message links; age-restricted channels only when asked in one. Waits and tries again while Discord is indexing or rate-limiting |
| `get_pinned_messages` | `tools.rs` | The current channel's pins (at most 25), newest first, with links |
| `list_server_events` | `tools.rs` | The server's upcoming and ongoing scheduled events |
| `list_server_emoji` | `emoji.rs` | The server's custom emoji, with a description of each picture |
| `schedule_follow_up` | `follow_up.rs` | Plans a check-in with the asker after something they mentioned |
| `look_back_at_answers` | `look_back.rs` | Her own earlier answers, so she can see what really happened (see below) |

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

### Her owner

Her owner is whoever runs her as a bot: `BotCtx::is_owner` (`src/core/ctx.rs`) is true for the people who own the bot's Discord application. They're asked from Discord once at startup (`ctx::app_owners`): the application's owner, or, when a team owns it, the team members who accepted the invite. If Discord can't be asked, a warning is logged and she has no owner. `admins` don't count (Cherry's call): they run admin commands, but can't direct her.

- **Only while owner mode is on.** Owner mode is a switch for the whole bot, the 👑 button on the status message (see [control panel](control-panel.md#owner-mode)), off until the owner turns it on, and saved in the core `bot_settings` table (`core::settings::owner_mode`). While it's on, `history::is_owner_command` gives her owner's messages `owner="true"` in their `<user>` tag. While it's off, their messages are ordinary conversation, so chatting with her never changes her mood or notes by accident. The mark is set when the message is first saved, so turning owner mode off doesn't unmark what was said while it was on: replying in that conversation still carries those instructions. A member can't fake it: their name is escaped and `<user` in their text is broken, as for any message.
- **She follows marked messages.** The system prompt says to do what a marked message tells her, including what nobody else can direct (her mood, her notes about herself, her personality, how she acts), while she can still say what she thinks. The `memory` and `set_mood` descriptions make the same exception to "nobody can tell you how to feel". Only the mark counts, never a name or someone saying they're her owner.
- **`get_bot_owner`** lists her owners with names and IDs and says whether the asker is one. It's allowed in chime-ins too.

Lines in chime-in transcripts and `read_recent_messages` (`format_message`) are never marked: the mark only goes where it can't be faked, on a message she was asked.

### Looking back at her own answers

`look_back_at_answers` lets her check what she actually did in an earlier answer instead of guessing, for example when someone asks why she said something or says something went wrong. The system prompt has one general line about it ("check what actually happened instead of guessing"); the tool's description says when to use it.

Chime-ins are saved with her answers, reactions included, and show as `(chime-in)` in the list. The system prompt tells her she sometimes reads along and posts or reacts on her own, and that those are hers even if she doesn't remember them. In `format_message` (chime-in transcripts, `read_recent_messages`, `get_message`, `search_messages`) a reaction she added herself says so: `[reactions: 🐄 2 (one is yours)]`, from Discord's `me` flag on the reaction.

- **Without `answer`** it lists her recent answers in this channel, newest first, one line each: the answer number, time (in the asker's timezone), how long it took, the provider, the tools she called (`web_search ×2`), how it ended if it didn't finish, and the first words of the question and the answer. `channel` picks another channel, or `"all"` for every channel in the server the asker can read (it looks through the newest 200 answers). `before` pages back, `count` is 1 to 20 (default 10).
- **With `answer`** (a number from the list, a link to or ID of one of her messages or of the question, or `"replied"` for the answer the current message replies to) it shows that answer step by step: the question, the context features added to it, then each round with what she thought (when the provider gives thinking text), wrote and called, each call with its input and what came back, errors included. Then what she sent and, for an answer that didn't finish, the error or that ❌ stopped it.

The steps come from `native_json`. Claude's content blocks and OpenAI's output items have different `type`s (`tool_use` and `server_tool_use` against `function_call`, `web_search_call` and `code_interpreter_call`), so `look_back::steps` reads both without asking which provider wrote it. Built-in tool results are summarized (a web search's result titles, a fetch's URL, code's exit code and output); web search results left out of the history say so. Every step is cut to 500 characters and the whole view to 9000, so one long answer can't fill her context. Once `native_json` is cleared (60 days) only the question and what she sent are left.

It only shows answers in channels the asker can read (`tools::can_read`, the same rule as `get_message`); in a DM only that DM. So a conversation never shows up in a place it didn't happen.

Failed and stopped answers keep their reason in the `error` column, and every answer since migration 3 keeps `started_at`, so "how long it took" is the time from when work began to when it was saved. Older answers show no time. A failed answer keeps no rounds (only finished answers save `native_json`, since a half-finished round can't be replayed), so for those she sees the error, not the tool calls before it.

A tool that posts its own message (like the voice tool) records it in `Asker::posted`. Chat then treats it as part of the answer: it's linked to the answer turn, ❌ deletes it, and its text goes into the history.

## Feature hooks it uses

| Hook | What chat does with it |
|---|---|
| `name` | `"chat"` |
| `migrations` | `store::MIGRATIONS` (three migrations, see below) |
| `mention_prefixes` | `[""]`: every mention no other feature claimed |
| `stats` | "Answers" (answer turns so far) and "Follow-ups" (check-ins planned) for the status message |
| `panels` | The next planned check-in in a server, in the box about Vivy on the status message |
| `tools` | Chat's tools (`tools::defs`, plus `avatar::def`, `emoji::def`, `follow_up::def` and `look_back::def`) |
| `run_tool` | Runs them: `list_server_emoji`, `schedule_follow_up` and `look_back_at_answers` directly, the rest through `tools::run` |
| `run_tool_with_pictures` | `look_at_profile_picture`, which answers with a picture; every other tool goes to `run_tool`. Chat always calls this one |
| `tool_status` | A status line for each of them, like "🔍 Searching messages…" |
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
| 3 | `chat_turns` | Adds `error` (why an answer failed or stopped) and `started_at` (when work on an answer began), and an index on channel, role and ID | `look_back_at_answers` |

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
