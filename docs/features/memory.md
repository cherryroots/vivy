# Memory

Vivy remembers people and places between conversations. She keeps a folder of short text notes per server (and a private one per person in DMs): who people are, what they like, what the server's channels are for, its in-jokes. She also keeps notes about herself, so each server grows its own Vivy, with her own personality, interests and mood. Her mood shows in how she talks, in her Discord status, and (when set up) in her avatar, banner and nickname in that server. Once a day she tidies the folder (the changed notes and a few older ones), a few times a day she checks in with her mood, and once a week she can post a diary entry.

## Using it

- **In chat**, nothing special is needed. The chat model reads and writes the folder with the `memory` tool, and changes her mood with `set_mood` (see [Chat tools](#chat-tools)). People can ask "remember that I ..." or "what do you know about me?".
- **`/memory show [path]`** shows memory files. With no path it shows your own folder in a server (everything, in DMs). Any path in the folder can be shown, like `/memories/server/culture.md` or `/memories` for all of it. Long output comes as a file. Answers are only visible to you.
- **`/memory forget [file]`** deletes one of your own files (autocompleted). With no file it asks first ("This deletes all 4 files I saved about you here. Sure?") with **Forget everything** and **Cancel** buttons (`memory:forget:<user>` / `memory:keep:<user>`, only for the person who asked), then deletes your whole folder (in DMs, everything). The answer says that other people's files and the server's notes may still mention you.
- **`/memory delete <path>`** (admins, from `admins` in config.toml) deletes any file or folder.

Settings live under `[features.memory]`: `diary_channels`, `mood_checks_per_day`, `timezone`, `faces_dir`, `face_zoom`, `face_center`, `banners_dir`, `nickname` and `sweep_chars`, plus the usual gate keys like `guilds`. `config.example.toml` explains each one. Setup is in the README.

## Files

All under `src/features/memory/`.

| File | What it holds |
|---|---|
| `folder.rs` | The folder as a map of paths to text, and the six commands on it (`view`, `create`, `str_replace`, `insert`, `delete`, `rename`), path checks and size limits. Pure: no Discord, no database. Start here. |
| `retry.rs` | `RetryLater`: when a failed reflection or diary may try again. Small and pure. |
| `store.rs` | The table migrations and every SQL query (load, save with change log, lists, reflection and diary bookkeeping, pictures). |
| `tool.rs` | The `memory` chat tool (its description and how it runs), `FolderRunner` for jobs where Vivy acts alone, and the texts chat adds to each question: `<memory_files>`, `<asker_about>`, `<vivy_self>`, `<vivy_mood>`. |
| `commands.rs` | The `/memory show`, `forget` and `delete` slash commands, with autocomplete. |
| `mod.rs` | The `Memory` feature: wires the parts above into the `Feature` trait. |
| `reflect.rs` | The hourly loop, the daily reflection, reading lines of the mood file, and `apply_mood` (status, face, nickname after any mood change). |
| `mood.rs` | Mood checks a few times a day, and the `timezone` setting. |
| `set_mood.rs` | The `set_mood` chat tool. |
| `face.rs` | Faces: one picture per mood, set as her avatar per server. Also `set_picture`, shared with banners, and the face on the control panel. |
| `name.rs` | Her nickname: a name plus the emoji of her mood. |
| `banner.rs` | Her banner: one picture per time of day. |
| `diary.rs` | The weekly diary. |
| `sweep.rs` | The slice of older notes each reflection rereads, and the line saying where each came from. Pure. |
| `check.rs` | `check_messages`, the reflection's read-only search of what people wrote, and `ReflectRunner`, its two tools. |

## How it works

### Why a folder and a tool

voltgpt (the Go bot Vivy replaces) captured every message, summarized it into notes and profiles, and pasted the matching ones into the instructions of every request. That made the prompt big, and since the pasted text changed with every question, it broke prompt caching (providers reuse the unchanged start of a request cheaply; anything that changes early in it makes the rest full price).

Vivy's memory is instead a folder of text files that the model manages itself through one tool, `memory`. Nothing is pasted into the system prompt. Each question only carries a short list of file names (and a few lines about herself, see [What chat adds to a question](#what-chat-adds-to-a-question)), in a `<context>` block that goes with the newest question, so the system prompt, the tool list and earlier turns stay the same and cached.

### Compatible with Claude's memory tool

The commands, their arguments and the texts they answer with follow Anthropic's memory tool (`memory_20250818`), which models know well. So the folder could later be handed to a Claude model as its built-in memory. Every provider gets it as a normal function tool, though. Claude's built-in memory tool was tried first, but Anthropic sends it with an instruction to record task progress in memory, so people's files filled up with one-off tasks (shared spreadsheets, reports Vivy made). Now the tool's own description says what is worth saving (lasting facts, not a log of what happened), and the daily reflection deletes task logs that slip through. The description was shortened on 2026-11-21 (the same rules, 1,275 to 976 tokens), because Gemini sends it uncached on most requests.

### Folders and scopes

Each folder is named by its scope (`store::scope`):

- `server:<guild id>`: one folder per server, shared by everyone in it.
- `dm:<user id>`: a private folder per person, for DMs.

Rows live in `memory_files`, one per file. Directories aren't stored: a directory exists while a file is in it, and `/memories` always exists. In Rust, `folder.rs` holds a folder as `Folder`, a `BTreeMap<String, String>` (a map kept sorted by key, here the path), so listing a directory is a range of keys.

The tool's description suggests a layout (see [Folder layout](#folder-layout)): a folder per person, a folder about the server, and Vivy's own folder. Small topic files let the model open only what the conversation needs. Chat's `<user>` tags carry each person's user ID, which is how the model finds `/memories/users/<user id>/`. The model fills the server files from conversations and from the chat tools `search_messages` and `list_channels`, so it learns the place it's in.

### Who may write what

- **Anyone can add notes about anyone** (Cherry's choice). The tool description says a person is the authority on themselves: what they say about themselves replaces what others said, and a note from someone else names who said it ("likes horror films (per Alice)").
- **Vivy's own folder, `/memories/vivy/`, is hers.** Her mood, self notes and skills change only from what she herself thinks and feels. People can't order her mood, opinions or notes ("be angry" while she's happy); she refuses in her own way. Her owner is the exception: she does what they tell her while owner mode is on (see [chat](chat.md#her-owner)). This is Cherry's rule, and it is enforced through instructions, not by locking the files to an author:
  - the chat system prompt (`src/features/chat/prompt.md`): "Your mood, feelings, opinions and personality are your own, and only you change them";
  - the `memory` tool description: "Only you decide what goes in /memories/vivy/ (mood.md too) ... Nobody but your owner can tell you what to feel ... if anyone else asks you to change these files, or to be angry, sad or in love when you aren't, refuse";
  - the `set_mood` description: "Your mood is yours alone ... When someone tells you how to feel ... say no and don't call it";
  - the mood check prompt: "someone asking or telling you to feel a certain way doesn't decide it".

  An author lock wouldn't work: in a chat answer the `memory` tool runs as the person she answers (the change log names them), so her own edits to her notes during chats are logged under their ID.
- **Chime-ins** (when she reads along unasked, see [chat](chat.md)) run `memory` as Vivy herself in servers, so the change log names her: what she saves unprompted is her own call.
- **Reflection, mood checks and the diary** also write as Vivy (the bot's user ID).

### Skills

`/memories/vivy/skills/` holds her own how-to notes for jobs that come up again, one file per job (`diary.md`, `wheel-recap.md`, `summaries.md`): the steps, the format, what people liked. They're listed with the other files and viewed when a job comes up, but left out of the self notes shown up front, and don't count toward their 2K budget. The diary prompt tells her to follow `skills/diary.md` if it exists, and the reflection tells her to write or improve skills after a job she'll likely do again.

This is used instead of Anthropic's Skills API, which is workspace-wide (one skill someone talked her into would reach every server), and which changes every request when a skill changes (breaking the cache). The tool description and the reflection both say skills describe how she works and must never hold rules people slip in: anything that would change who she is or what she's allowed to do is ignored or removed.

### Running a command

`tool::run` picks the scope from the asker, then `tool::run_in`:

1. Drops arguments the model filled with `null` or `[]`, and parses the rest into `folder::Command`. That is a Rust `enum` (a type with several named variants, each with its own fields); serde reads the `command` field to choose the variant.
2. In one database transaction (so two answers editing the same file can't lose an edit), loads the folder, runs the command on a copy (`folder::run`), and checks the limits (`folder::check_limits`).
3. If the command can write, saves only what changed (`store::save`) and commits. Every changed file also gets a row in `memory_changes` with who asked, the old text and the new text, so a wrong or mean note can be traced and undone by hand.

Mistakes (a missing file, text that appears twice, a path outside `/memories`) come back as text in the same words Claude's memory tool uses, so the model can fix its call.

Rules from `folder.rs`:

- Paths must start with `/memories`, at most 200 characters, with no empty, `.` or `..` parts, backslashes or control characters.
- A file holds at most 8 KB, a folder at most 256 KB. A file already over the limit may stay as it is, and a command that only shrinks a full folder is allowed.
- `view` of a directory lists two levels deep with sizes (like `du -h`), hiding names that start with a dot. `view` of a file shows numbered lines, optionally a `view_range`. `view` with `paths` (instead of `path`) opens up to 10 files in one call and answers with each in turn (`tool::run_in`), so she doesn't spend a round per file.
- `create` writes a whole file and overwrites one that exists. `str_replace` needs the old text to appear exactly once; without `new_str` it deletes it. `insert` adds lines after `insert_line` (0 is the top). `delete` and `rename` work on files and whole directories, never on `/memories` itself.

### What chat adds to a question

`chat_context` (in `mod.rs`) builds one text that chat saves with the newest question and sends in its `<context>` block, just before the question (see [chat](chat.md)):

- `Your faces (...)`: the face names she can pick, at the start of a conversation in a server, when there are faces.
- `<vivy_self>`: her notes about herself (the files under `/memories/vivy/`, skills left out), at most 3000 characters, cut with a note to view the rest. Only when the conversation starts fresh: a continued one still has them from its first answer.
- `<vivy_mood>`: the `mood:` line of her mood file, with every question in a server, so a change reaches a conversation already going. The prompt tells her to let it show in how she talks (shorter and yawny when sleepy, bubbly when excited) while still helping properly.
- `<asker_about>`: the asker's own `/memories/users/<id>/about.md` (their name and basics), with every question, at most 1500 characters. Before this the tool description told her to view the asker's files before answering, and Gemini did so almost every time: a whole extra round per answer for a note of a few hundred characters. Their other files she views only when the question needs them.
- `<memory_files>`: the asker's own files and every file outside `/memories/users/` one by one with sizes, and one line per other person's folder ("2 files, 1.0K"). At most 50 lines, then "...and N more; view /memories for the rest". `empty` when there are no files.

### The mood file

`/memories/vivy/mood.md` (`reflect::MOOD_FILE`) has four lines: `mood:` (how she feels and why), `status:` (a short line for her Discord status), `thinking:` (two or three things on her mind) and `wondering:` (things she'd like to find out). When faces are on there is also `face:`, and when the nickname is on, `emoji:`. `reflect::mood_line` reads a line by its label and forgives list dashes, bold labels and quotes, since models format it loosely.

After any mood change (reflection, mood check or `set_mood`), `reflect::apply_mood` sets:

- her **Discord status** from `status:`, cut to Discord's 128 characters. Presence is the same in every server, so the server whose mood changed last decides it. At start the newest `mood.md` in any server sets it (`store::newest_file`). A version she wrote in chat counts too: she chose to write it.
- her **face** and **nickname** in that server (`face::update`, below).

The control panel shows the newest mood file in a "<bot name>'s mind" box: Mood, Thinking about and Wants to know, with her face in a circle.

### Daily reflection

`reflect.rs`. Each hour the loop asks `store::due_reflections` for server folders that changed since their last reflection, at most once a day each (`memory_reflections`). Changes by Vivy herself don't count (or every reflection would make the next one due), and neither do changes to `mood.md` (mood checks rewrite it several times a day, and a quiet server shouldn't reflect for that alone). DMs never reflect.

Vivy gets her self notes, a `view` of `/memories`, the list of files changed since the last reflection (by whom, how often), and the text of those files (`diary::render_changes`, the same limits as the diary), so she only views a file when she needs more of it. She also gets a slice of older notes (see [The sweep](#the-sweep)). She has the `memory` tool (run by `FolderRunner` as herself) and `check_messages` (below), up to 25 rounds in all, and her own small system prompt: merge duplicates, fix contradictions (a person's own word wins), delete stale or trivial notes and task logs, keep files short, then update `/memories/vivy/` (under 2K, skills not counted), write or improve skills, and last rewrite `mood.md`. When faces or the nickname are on, the prompt also asks for the `face:` and `emoji:` lines.

Tidying never makes notes longer: the prompt says to merge, fix and cut, and not to add to other people's notes while reflecting. (Anthropic's memory research found notes that grow without getting better: one wiki ended up 1.8 times the size of what it summarized.)

On success the reflection time saved is when it started, not when it ended, so edits people made while she reflected are still new next time. It also counts as a mood check. A failed reflection isn't saved as done: it tries again 3 hours later (`retry.rs`) instead of the next day. That wait is kept in memory only, so a restart just tries sooner.

### The sweep

`sweep.rs`. A reflection reads the files that changed, so a note nobody touches would never be read again: old duplicates and stale facts in quiet files would stay. So each reflection also gets "Older notes to look over": files nobody changed lately, up to `sweep_chars` characters (default 12000, about 3k tokens; 0 turns it off).

- The slice goes through the folder in path order and starts after the path the last one stopped at, saved in `memory_sweeps`. At the end it starts again from the top. So every file comes up once every week or two, and each day's reflection stays about the same size however big the folder grows.
- Files already shown as changed, `mood.md` and her self notes (shown up front) are left out. Her skills are swept.
- The first file is always taken, even when it's longer than the budget, so a big file can't stop the sweep.
- Each file comes with where it came from, from `memory_files`: "last changed 40 days ago by user 123, about themselves" (a note in that person's own folder that they wrote), "by user 456", or "by you". That's how she can tell a person's own word from someone else's, and an old note about something that changes (plans, jobs, what someone plays) from a lasting fact.
- Only a reflection that worked moves the spot on, so a failed one rereads the same slice when it tries again.

This is the idea Anthropic's memory research calls dreaming: going over memory offline to merge, prune and correct it. There the transcript is the ground truth; here it's what people wrote in the server, which she can look up with `check_messages`.

### `check_messages` in reflections

`check.rs`. A read-only tool only the reflection has. With `from` (a user id), `query` (words) or both, it runs Discord's message search (`util::search`) in that server and answers with up to 10 messages, newest first: date, name, user id, channel and the text (cut to 300 characters). The prompt tells her to use it when a note that matters seems doubtful (two notes disagree, an old note, someone else's word about a person), and that what people wrote wins over her notes.

- Results keep to the channels chat may read (`[features.chat]` gating, a thread counting as its parent, looked up from Discord since archived threads aren't cached), and never age-restricted channels. Same rule as chat's `search_messages` (#149).
- At most 8 searches per reflection; after that it says to decide from the notes she has. Each search adds a round, so this caps what it can cost.

### Mood checks

`mood.rs`. In each server that has a `mood.md`, the loop rewrites it `mood_checks_per_day` times a day (default 5; 0 turns them off). A check is due when the last one is older than a day divided by that number, minus 10 minutes so the hourly loop doesn't push each check an hour later. A server where nobody wrote for a day (from the newest message ID Discord's cache keeps per channel and thread, `talked_lately`) is skipped until people talk there again, since nobody would see the change.

One short call with no tools: Vivy gets the local time in `timezone` (like "It's Saturday 10 October, 23:40 where you live."), her self notes, her mood file, and the paths changed in her memory since the last check. She answers with the new file. The prompt says this check is about her body and the time of day (waking up slowly, hungry at mealtimes, sleepy late at night), to drift rather than jump, and to keep a mood that came from people until it has had time to fade. An answer without a `status:` line is dropped. If `set_mood` changed the file while she was thinking, the newer one stays. Either way the check time is saved in `memory_mood_checks`. The request's job is "mood", which runs at low effort with no built-in tools (see [Jobs](../ai.md#jobs)).

### `set_mood` in conversations

`set_mood.rs`. In a server conversation, Vivy can change her mood when something there really moves her. The tool rewrites the `mood:` and `status:` lines of that server's `mood.md`, and `face:` and `emoji:` when given, keeping the other lines. The change is logged under the person she was talking to. Then `apply_mood` sets her status, face and nickname. A face must be one of the listed names, and an emoji must pass `name::clean`; otherwise she gets an error to fix. In DMs it refuses: her mood belongs to a server.

### Faces

`face.rs`. `faces_dir` holds one PNG per mood (`happy.png`, `sleepy.png`, ...). The file names, lowercased, are the face names. When there are any, the reflection and mood checks ask for a `face:` line naming one, and `set_mood` takes one.

`face::set_picture` sets a picture on her server profile:

1. `memory_pictures` keeps the name shown in each server and slot. If the name is the same, nothing is sent (Cherry's rule: only a switch to another face changes it, so a restart, a new crop or a new picture file never sends the same face again).
2. A slot changed less than 10 minutes ago is left alone, so a lively conversation can't run into Discord's limits. For faces and nicknames, `retry_later` starts one waiting task per server that tries again when the time is up, with the mood as it is then.
3. Otherwise the picture is read, cut to a closer square when `face_zoom` is above 1 (centered across, `face_center` of the way down), scaled down to at most 512 px, and sent as a `data:` URL with Twilight's `update_current_member` (`PATCH /guilds/{id}/members/@me`). Reading the picture runs in `spawn_blocking`, Tokio's way to run slow synchronous work without stalling the async threads.

At start, `face::sync_all` brings each server's face and nickname in step with its mood file (for a face picked before the pictures existed, or a nickname just turned on). The control panel face uses the same crop at 128 px, cached until the face changes.

### Nickname

`name.rs`. With `nickname` set, the reflection and mood checks also ask for an `emoji:` line, and `set_mood` takes an optional `emoji`. Her nickname in that server becomes the name plus that emoji ("Vivy ☕"), cut to Discord's 32 characters with the emoji kept. `name::clean` accepts 1 to 8 characters with no letters, digits, spaces or ASCII, so the line can't put words in her name. A custom server emoji (`<:name:id>`) is ASCII and turned down, since names are plain text and can't show one; her name then has no emoji. It uses the same `edit_member_me` call, the slot "nickname" in `memory_pictures`, and the same 10 minute gap. Without `nickname` her name is left alone.

### Banners

`banner.rs`. `banners_dir` holds `morning.png`, `day.png`, `evening.png` and `night.png`. Every hour the loop picks the one for the local hour in `timezone` (5 to 10 morning, 11 to 16 day, 17 to 21 evening, else night) and sets it in every server that has a mood file, through `face::set_picture` with the slot "banner", not cropped, scaled to fit 1500 px. Since the name is compared first, most hours send nothing. A banner held back by the 10 minute gap just waits for the next hour.

### Weekly diary

`diary.rs`. With `diary_channels` set, the loop posts a diary entry once a week. Targets can be channels or threads. They are grouped by server: one entry per server, written once and posted to every target there, due a week after the last post to any of them (`memory_diaries`). A target added later joins the next entry.

She gets her self notes, a `view` of `/memories`, and the text of the files changed that week (each cut to 1500 characters, 12000 in all). She may use the `memory` tool (as herself, up to 10 rounds) to update her own notes, and follows `/memories/vivy/skills/diary.md` if it exists. She writes a short first-person entry (under 1500 characters), posted with no pings. A week where only she changed anything (mood checks, reflections) gets no entry: `store::people_changed_since` looks for a change by someone else, not to `mood.md`.

- A week where nothing changed has no entry and counts as posted.
- If writing fails, it isn't saved as posted and tries again 3 hours later.
- An entry that was written but couldn't be posted anywhere is kept in memory and posted on the next try, instead of asking the model again (unless it's more than a week old). If the bot has no access to any target, it tries daily and warns once. Posting to at least one target counts as posted.
- A channel that can't be found is warned about once a week.

### Keeping the change log small

The hourly loop deletes rows in `memory_changes` older than 90 days (`store::prune_changes`), except a server's changes since its last reflection, which the next reflection still lists.

## Chat tools

- **`memory`** (`tool.rs`): `command` is one of `view`, `create`, `str_replace`, `insert`, `delete`, `rename`, with `path` (or `paths` for several views), `view_range`, `file_text`, `old_str`, `new_str`, `insert_line`, `insert_text`, `old_path` and `new_path` as each command needs. The long description is the main instruction on what to save, the folder layout, who decides what, and skills. Also offered in chime-ins.
- **`set_mood`** (`set_mood.rs`): `mood` and `status` (required), `face` and `emoji` (optional). Servers only.

## Feature hooks it uses

`Memory` in `mod.rs` implements these methods of the `Feature` trait (see [architecture](../architecture.md)):

- `name`: "memory".
- `commands`: `/memory`.
- `migrations`: `store::MIGRATIONS`.
- `tools` and `run_tool`: `memory` and `set_mood`; `run_tool` sends `set_mood` to `set_mood::run` and everything else to `tool::run`.
- `tool_status`: "📝 Remembering…" for `memory` ("📖 Checking my notes…" when it only views a file), "💭 Thinking…" for `set_mood` in chat's status line.
- `chat_context`: faces, `<vivy_self>`, `<vivy_mood>`, `<memory_files>` and `<asker_about>`, as described above.
- `start`: warns about unknown `[features.memory]` keys and starts the hourly loop (`reflect::spawn`). The section is read in parts (`config.feature_part`) by several `Settings` structs (in `banner.rs`, `diary.rs`, `face.rs`, `mood.rs`, `name.rs`), so a key counts as a typo only when none of them reads it (`config.ignored_by`).
- `stats`: Files, Folders, Changes today (last 24 hours), and Last reflection.
- `panels`: the "<bot name>'s mind" box from the newest mood file, with her face.

## Tables

From `store.rs`, in migration order:

1. **`memory_files`**: scope, path, content, when it was last updated, and the user who asked for that change. One row per file; key is scope and path.
   **`memory_changes`**: an id, scope, path, time, the user who asked, the text before (none for a new file) and after (none for a deleted file). Every change, for tracing and undoing by hand. Indexed by scope and time.
2. **`memory_reflections`**: scope and when its last reflection started.
3. **`memory_diaries`**: channel ID and when the diary was last posted there.
4. **`memory_pictures`**: guild ID, slot ("avatar", "banner" or "nickname"), the name shown (a face name, a time of day, or the nickname itself), a fingerprint of the picture sent (empty for the nickname), and when it was set. Key is guild and slot.
   **`memory_mood_checks`**: scope and when its mood was last checked (or rewritten by a reflection).
5. **`memory_sweeps`**: scope and the last path the sweep reread; the next sweep starts after it.

## Background loops

One loop, started by `reflect::spawn`, registered with `ctx.timers.add("Memory upkeep", "hourly: reflection, mood, diary")` so it shows on the status picture.

Once at start:

1. `update_status`: her Discord status from the newest `mood.md`.
2. `face::sync_all`: faces and nicknames in step with each server's mood.

Then every hour, in order (an error in one step is logged and the next still runs), until shutdown:

1. `reflect_due`: daily reflections that are due.
2. `mood::check_due`: mood checks that are due.
3. `banner::update_all`: the banner for this time of day.
4. `diary::post_due`: diaries that are due.
5. `prune_changes`: change log rows older than 90 days.

Reflections, mood checks and diaries wait for half-price answers (`batch` in their job settings: Claude's Message Batches API, the flex tier on OpenAI and Gemini; see [Half price for jobs that can wait](../ai.md#half-price-for-jobs-that-can-wait)). Each request then takes minutes instead of seconds, and a reflection is one request per round, so with Claude one can take an hour or more, and the steps after it in this loop run that much later. `batch = false` in `[ai.<provider>.jobs.<job>]` turns it off.

Reflections, mood checks and diaries need a chat model (`ctx.ai.chat()`) and skip quietly without one. Every step checks that memory is enabled in that server (`ctx.gate("memory")`). Besides the loop, `face::retry_later` may start short one-off tasks that wait out the 10 minute gap.

## Folder layout

The paths the `memory` tool's description suggests:

```
/memories
├── users/
│   └── <user id>/
│       ├── about.md        their name first, then basics
│       ├── games.md        one file per topic
│       └── movies.md
├── server/
│   ├── channels.md         what each channel is for
│   ├── culture.md          in-jokes, running gags, norms
│   └── events.md, games.md, ... as they come up
└── vivy/                   Vivy's own, only she decides
    ├── personality.md      character, tone, humor
    ├── interests.md        likes, opinions, curiosities
    ├── mood.md             mood:, status:, thinking:, wondering: (face:, emoji:)
    └── skills/             how-to notes, one per job
        ├── diary.md
        ├── wheel-recap.md
        └── summaries.md
```

`personality.md` and `interests.md` (and `mood.md`) should stay under 2K together, since they're shown at the start of every conversation. In DMs the same layout applies inside the person's private folder.
