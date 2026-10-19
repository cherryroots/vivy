# Reminders

Reminders let people ask Vivy to ping them later. `@Vivy remind me in 2h30m to check the oven` saves a reminder, with any pictures attached to the message, and when it is due Vivy posts it in the same channel, pings the person, and links back to the message that set it. The delivered reminder has snooze buttons. People can list and delete their pending reminders, set their own timezone, and set reminders through normal chat as well.

## Using it

- **Mention**: `@Vivy remind me ...`, `@Vivy reminder ...` or `@Vivy remind ...`, followed by a time and the text, in either order: `remind me in 2h to check the oven` or `remind me to check the oven in 2h`. Times can be written the way people say them: `in an hour and a half`, `in half an hour`, `tomorrow evening`, `tonight`, `this friday`, `next week`, `on the 15th`, `march 3`, `24.12`, `at 5pm ... tomorrow`.
- **Recurring reminders**: `remind me every day at 9:00 to stretch`, `every weekday at 8:30`, `every monday`, `every weekend`, `every other week`, `every 2 hours`, `every 3 days`, `daily`, `weekly`, `monthly`. After each one is delivered the next is queued. Deleting it in `/reminders` stops it. A repeat holds one weekday, so `every monday and wednesday` is refused with a note to set two.
- **Cancel** under the "I'll remind you" reply deletes that reminder. Only the person who set it can use it.
- **`/reminders`**: a private list of your pending reminders (soonest first, up to 25 shown, each with "in 3 days" and the full date, recurring ones marked 🔁 with how they repeat, in your own timezone without the zone's name) with a menu to delete one; each menu entry shows its date in your timezone under the text, so two "stretch" reminders can be told apart. The list stays open after a delete, reloaded, so you can delete several.
- **`/timezone [zone]`**: sets the timezone used for times without one, like `Europe/Oslo` or `CET`. The option autocompletes the abbreviations Vivy knows first, then zone names. An abbreviation is echoed back with the place it means (`CET (Europe/Paris)`). Without a zone it tells you your current setting and the local time there.
- **Snooze buttons** under a delivered reminder: 10 min, 1 hour, and **Pick a time…**, which opens a box to type a time like `in 30m`, `at 18:00` or `friday`. Only the person the reminder is for can use them.
- **Chat**: in a normal conversation the model can create, list and cancel reminders with its [chat tools](#chat-tools).

Settings: the feature has a `[features.reminders]` section with only the common keys (`enabled`, `guilds`, and so on); `config.example.toml` explains them. Setup is in the README.

## Files

All in `src/features/reminders/`, in a reading order that starts with the pure logic.

| File | What it holds |
|---|---|
| `parse.rs` | The time grammar, written with the `winnow` crate. Pure: text in, time out, no Discord or database. Also the timezone lookup and the unit tests. |
| `store.rs` | The migrations and every SQL query, as plain synchronous `rusqlite` functions, with tests on an in-memory database. |
| `ui.rs` | Message texts, the Cancel and snooze buttons, the snooze modal, the `/reminders` list and delete menu, and the custom IDs (the `Action` enum). |
| `scheduler.rs` | The one background task that delivers due reminders, with retries. |
| `commands.rs` | The slash commands `/reminders` and `/timezone`, written with `poise`. |
| `tools.rs` | The chat tools `create_reminder`, `list_reminders`, `cancel_reminder`. |
| `import.rs` | Reading reminders from voltgpt's database. |
| `mod.rs` | Wires it all in: the `Reminders` struct, its `Feature` implementation, the mention handler, the cancel, snooze and delete handlers, and image downloads. |

## How it works

### Parsing a mention

The dispatcher strips the bot mention and the prefix ("remind me"), and `on_mention` in `mod.rs` gets the rest, for example `in 2h to check the oven`. `parse::parse_reminder` splits it into a `When` (the time as written) and the message text:

1. It tries to read a time at the start of the text.
2. If that fails, `time_at_end` tries each word as the start of a time that runs to the end of the text (trailing `.`, `!`, `?` are ignored).
3. If the start gave only a day ("tomorrow call mom at 5pm"), `day_then_time` takes the clock time from the end and joins them; if it gave only a clock time ("at 5pm call mom tomorrow"), it takes the day from the end.
4. If the message still starts with more schedule ("in 2 weeks on friday …", "every monday and wednesday …"), `more_schedule` turns the whole thing into an error that quotes the extra words and asks which time was meant, instead of saving "on friday …" as the text.

`clean_message` then strips glue from the text: leading `:`, `-` or `,`, then a leading "and" or "to" ("in 2h and call mom" becomes "call mom").

The grammar is a set of small parser functions. Each has the shape `fn(&mut &str) -> ModalResult<T>`: it reads from the front of the input, moves the input past what it read, and returns what it found. ("Parser combinators" just means small parsers glued together into bigger ones with helpers like `alt`, which tries several in order.) The pieces:

- `when` is `alt((in_duration, at_time, every, shorthand_repeat, tonight, on_day))`.
- `in_duration`: "in" plus `duration`, which is one or more `duration_part`s, separated by spaces, commas or "and". A part is a number and a `unit` (`2h`, `30 minutes`), or `an hour`, `half an hour`, `1.5 hours`, `1h30`, `an hour and a half`; fractions become the next smaller unit, so the parts stay whole numbers. Units go from years to seconds, with short forms (`2h30m`, `1 week 2 days`, `3 mins`).
- `at_time`: "at" plus a `clock`, or a `date` and a `clock` (`at 2026-03-01 15:04 EST`), then an optional `zone` and an optional `day` (`at 9:00 tomorrow`).
- `on_day`: a `day` with an optional `day_clock` (`friday at 3pm`, `next friday 9:30 CET`, `tomorrow evening`, `tomorrow in the morning`). `day_clock` reads a clock or a `day_part` (morning 09:00, afternoon 14:00, evening 18:00, night 20:00); a bare hour after "at" (`tomorrow at 7`) is an error asking for `7:00` or `7pm`, rather than silently 09:00 with "at 7" left in the message.
- `tonight`: today at 20:00.
- `every`: "every" plus either `repeat_days` (`day`, `weekday`, `weekend` (Saturday), or a weekday name) with an optional `day_clock`, or `repeat_interval` (an optional number or "other" and a unit: `2 hours`, `hour`, `other week`, `3 days`), then an optional zone. Minutes and seconds are refused, so a reminder can't fire every few minutes. `every other monday` and `every monday and wednesday` are refused with a hint (`refuse`), since the scheduler has no rule for them. It gives `When::Every(Repeat)`.
- `shorthand_repeat`: `daily` (with an optional clock), `weekly`, `monthly`, `hourly`. Only `daily` takes a clock; `weekly at 10:00` asks for `every monday at 10:00` instead.
- `clock` reads `16:30`, `08:30`, `16:30:15`, `3pm`, `3:30 pm`, `noon`, `midnight`. A bare number like `at 5` is rejected as too vague.
- `day` reads `today`, `tomorrow`, `next week` (seven days on), `next <weekday>`, `this <weekday>`, `on <weekday>`, `on <date>`, a date `2026-12-24`, a `calendar_day` (`the 15th`, `march 3`, `3rd of march`, `3 mar`, `24.12`, `24.12.2026`), or a weekday (full or short names like `fri`, `thurs`). A bare number is never a date, so "5 things" stays text.
- `zone` reads a timezone name and checks it with `lookup_zone`.
- `word` matches a whole word, ignoring case. An apostrophe counts as part of a word, so "tomorrow's standup" is not read as "tomorrow".

After "in" or "at", the rest is wrapped in `cut_err`. That stops `alt` from trying other forms, so the error can say exactly where it got stuck (`I got stuck at "xyz": I expected a duration like 2h30m or 3 days.`). For a mention this text goes to the log; for the chat tool, `parse_when` gives a similar error back to the model.

If nothing can be made into a reminder (no time, no message text, or a time that has already passed), `on_mention` returns `Mention::PassOn` and the dispatcher gives the whole message to [chat](chat.md), which can answer it or set the reminder with its tools. The reason goes to the log at `info` level.

### Turning the time into an instant

`parse::resolve(when, now, home)` turns a `When` into a UTC time. `home` is the user's saved timezone, or UTC when they have none.

- **Durations** are added part by part. Years, months, weeks and days move the calendar date in the user's zone and keep the wall-clock time, so summer time does not shift the hour, and "in 1 month" on January 31 lands on February 28. Hours, minutes and seconds are exact.
- **A clock time without a day** means today, or tomorrow if that time has passed.
- **A day without a time** means 09:00.
- **A day of the month** (`the 15th`) means the next 15th, this month or next; **a month and day** (`march 3`) means the next such date, this year or next.
- **A weekday** means the next one; "friday" on a Friday means today if the time is still ahead. "next friday" on a Friday means a week later.
- **A dated or day-based time in the past** is an error ("... has already passed").
- **A repeat** (`next_repeat`): a day rule takes the first wanted day, from today, whose clock time (09:00 if none was given) is still ahead, in its zone, so summer time doesn't move the hour. An interval adds itself, like a duration.
- **Summer time gaps**: a wall-clock time that does not exist (skipped hour) uses the hour after; one that happens twice uses the first.

`lookup_zone` accepts IANA names that contain a `/` (`Europe/Oslo`, any case) and a fixed list of abbreviations, each mapped to a real place so summer time follows: `UTC`/`GMT`/`Z`, `EST`/`EDT`/`ET` (New York), `CST`/`CDT`/`CT` (Chicago), `MST`/`MDT`/`MT` (Denver), `PST`/`PDT`/`PT` (Los Angeles), `AKST`/`AKDT` (Anchorage), `HST` (Honolulu), `WET`/`WEST` (Lisbon), `CET`/`CEST` (Paris), `EET`/`EEST` (Helsinki), `BST` (London), `MSK` (Moscow), `SAST` (Johannesburg), `HKT` (Hong Kong), `SGT` (Singapore), `JST` (Tokyo), `KST` (Seoul), `AEST`/`AEDT` (Sydney), `NZST`/`NZDT` (Auckland). Two-letter ones count only in capitals, so "at 5pm et voilà" stays in the user's zone. Single words that are not in that list are rejected on purpose, because zone names like "Japan" are also ordinary words in a reminder. `ZONE_ABBREVIATIONS` lists them for `/timezone`'s suggestions.

### Recurring reminders

A recurring reminder is a row like any other, plus a `repeat` column with how it repeats, as text the grammar reads back: `describe_repeat` writes `every monday at 9:00 Europe/Oslo` or `every 2 hours`, and `parse_repeat` reads it. The zone is written out (the user's zone when they set it), so a later `/timezone` change doesn't move it; hours are exact, so they need no zone. When a repeat is shown to a person, `repeat_for_display` drops the zone if it is the viewer's own (`every day at 9:00`) and otherwise says `every day at 9:00 in Europe/Oslo`.

When the scheduler delivers one, `scheduler::next_time` works out the next time after the one it was due (or after now, when the bot was offline past it), and `store::repeat` queues a copy (images and all) in the same transaction that marks the delivered one sent. The delivered one keeps its snooze buttons: a snooze brings it back once, with `repeat` cleared, since the series goes on in the copy. The next one is queued even when the delivery is given up after 10 tries, so one bad day doesn't end the series. Deleting the pending copy (in `/reminders`, or with `cancel_reminder`) stops it. The confirmation, the delivered message and `/reminders` show `🔁` and the repeat.

### Saving a reminder

`on_mention` downloads the message's images and videos (attachments with a width). Each one must fit the bot's upload limit in that server, which follows its boost level (`upload_limit`): 20 MiB normally and in DMs, 50 MiB at boost level 2, 100 MiB at level 3. Bigger files, and files whose download failed, are skipped and counted as `missing_images`. The reminder and its images are saved in one transaction (`store::add`), the scheduler is woken, and Vivy replies with a confirmation: the time as a Discord timestamp, the text, the names of skipped files, and a hint about `/timezone` when a clock time was read as UTC only because the user has no zone set (for a repeat, the hint says to set it again afterwards, since a repeat saves its zone). The confirmation pings nobody and has a **Cancel** button: `cancel` checks that the presser set the reminder, deletes it with `store::delete_pending` (the same call as the `/reminders` menu), and adds "❌ Cancelled" under the reply. A reminder that was already sent can't be cancelled there.

### The scheduler

`scheduler.rs` runs one background task for all reminders (voltgpt had a timer per reminder). Each loop it:

1. purges reminders sent more than a week ago (`KEEP_SENT_SECS`),
2. delivers up to 50 due reminders (`store::due`, oldest first), loading each one's images (`store::images`) just before sending it, so only one reminder's images are in memory at a time,
3. sleeps until the next `next_try_at`, at most an hour, so old reminders still get purged and a changed system clock is noticed.

The sleep is a `tokio::select!`, which waits on several things at once and continues with whichever finishes first: the sleep, the `wake` signal, or the shutdown token. `wake` is a `tokio::sync::Notify` shared with the feature. Adding, deleting or snoozing a reminder calls `notify_one()` on it, so the scheduler never sleeps past a new reminder.

### Delivery

`ui::fired_message` builds the message: `<@user> ⏰ text`, then a small line with a "Reminder set" link to the original message and when it was set (and how it repeats, read in the owner's timezone). When the reminder is more than 5 minutes late (for example after the bot was offline) it adds when it was due. Only the owner is pinged. The images are attached and the snooze buttons are added.

A reminder is marked sent only after Discord accepted the message:

- If Discord refuses it as too big (HTTP 413, or Discord error 40005) and it has images, it is sent again without them. The text then counts them as `[image missing]` or `[N images missing]`; the link to the original message still has them. Other errors, like a timeout, are not blamed on the images.
- If sending still fails, the reminder stays pending and is retried later: 1 minute, then doubling (2, 4, 8 ...) up to 6 hours (`retry_delay`).
- After 10 failed sends (`MAX_ATTEMPTS`, about 8 hours) it is deleted and an `error` is logged with the user, channel and text. Errors reach the log channel through the core logging setup (see [architecture](../architecture.md)).

### Snooze and delete

A snooze button does not create a new reminder. "Pick a time…" opens a modal; what is typed there goes through `parse::parse_when` and `parse::resolve`, like a reminder's time (a repeat is refused). Older messages still have a 1 day button, which adds 24 hours. The "Tomorrow 9:00" button was removed (#83); on messages from before that, it now answers with an error. `store::snooze` moves the delivered reminder (with its images) back into the queue: it sets a new `fire_at` and clears `sent_at` and `attempts`. The `UPDATE` only matches a sent reminder of that user, so a double press or a retry snoozes it once. The delivered message then loses its buttons and gets a "Snoozed until ..." line. Sent reminders are kept for a week for this; after that the button answers "This reminder is too old to snooze."

The `/reminders` menu calls `store::delete_pending`, which only deletes a pending reminder of the person who picked it, then shows the list again with "✅ Reminder deleted." on top.

## Chat tools

From `tools.rs`. They act as the asker (see `Asker` in `src/core/feature.rs`) and read times in the asker's timezone.

| Tool | What it does |
|---|---|
| `create_reminder` | Takes `when` (English, like "in 2h30m", "tomorrow evening" or "every monday at 9:00", parsed by `parse::parse_when`; the description lists the forms and the rules, like one weekday per repeat) and `message`. Sets a reminder in the current channel, linked to the asking message, without images. The answer tells the model the time and says when it was read as UTC because no zone is set. |
| `list_reminders` | The asker's pending reminders with IDs, soonest first, and how recurring ones repeat. |
| `cancel_reminder` | Deletes one of the asker's pending reminders by ID (which stops a recurring one). |

`run_tool` in `mod.rs` wakes the scheduler after a create or cancel.

## Feature hooks it uses

`Reminders` implements these `Feature` trait methods (see `src/core/feature.rs`):

| Method | What it does here |
|---|---|
| `name` | `"reminders"`: the config section, the custom ID prefix and the log label. |
| `commands` | `/reminders` and `/timezone`. |
| `migrations` | `store::MIGRATIONS`. |
| `mention_prefixes` | `remind me`, `reminder`, `remind` (the dispatcher picks the longest match). |
| `legacy_import` | The `reminders` part, run by `import::import`. |
| `tools`, `run_tool` | The three chat tools. |
| `tool_status` | "⏰ Setting a reminder…" and similar in chat's status line. |
| `stats` | Control panel lines: Pending, Next, Sent this week, and Failing to send when any are retrying. |
| `start` | Spawns the scheduler. |
| `on_mention` | Parses and saves a reminder, or passes the mention on to chat. |
| `on_component` | The delete menu, the Cancel button and the snooze buttons. |
| `on_modal` | The "Pick a time…" modal. |

After each delivery it publishes `BotEvent::ReminderFired { reminder_id, user_id, channel_id }`. No feature listens to it yet.

## Custom IDs

Built and parsed only in `ui::Action`. The dispatcher strips the `reminders:` prefix before `on_component` sees the rest.

| ID | Where |
|---|---|
| `reminders:delete` | The `/reminders` select menu. The reminder ID is the selected value. |
| `reminders:snooze:<reminder id>:<minutes>` | The 10 min and 1 hour snooze buttons (older messages also have 1440, a day). |
| `reminders:asksnooze:<reminder id>` | "Pick a time…": opens the modal. |
| `reminders:snoozeto:<reminder id>` | The snooze modal. |
| `reminders:cancel:<reminder id>` | Cancel under the "I'll remind you" reply. |

## Tables

From `store::MIGRATIONS`. Times are unix seconds; Discord IDs are integers.

**Migration 1**

- `reminders`: `id`, the user, channel and server (`guild_id` is NULL in DMs), the `message` text, `fire_at`, `created_at`, `next_try_at` (starts as `fire_at`, moved later after a failed send), `attempts` (failed sends), and `sent_at` (set once delivered; the row is kept a week for snoozing). Indexes on `(sent_at, next_try_at)` for the scheduler and `(user_id, sent_at, fire_at)` for the list.
- `reminder_images`: `reminder_id`, `position`, `filename`, and the file as a `BLOB`. Deleted together with their reminder (`ON DELETE CASCADE`).

**Migration 2** adds two columns to `reminders`: `source_message_id` (the message that set it, for the link; NULL for imported reminders) and `missing_images` (attachments that could not be saved).

**Migration 3** adds `repeat` to `reminders`: how a recurring reminder repeats (see [Recurring reminders](#recurring-reminders)), NULL for a one-off.

The timezone lives in `user_settings (user_id, timezone)`. That table belongs to the core (`CORE_MIGRATIONS` in `src/core/db.rs`) and is read and written through `src/core/settings.rs`, so other features can use the zone too.

## Background loops

One timer, registered in `scheduler::run` as `ctx.timers.add("Reminders", "at the next reminder")`. It shows on the status picture, running while it delivers and sleeping until the next reminder.

## Importing from voltgpt

`import.rs` copies voltgpt's `reminders` table from `old.db` (see [architecture](../architecture.md) for how the import runs). voltgpt stored IDs as text, an empty `guild_id` in DMs, and images as JSON with base64 data. For each row the import reads the IDs as numbers (empty becomes none), decodes the images into `reminder_images` blobs, and keeps `fire_at` and `created_at`. A broken row is logged and skipped instead of failing the whole part. Imported reminders have no `source_message_id`, so their "Reminder set" line has no link. Reminders that came due while the bot was down go out right after startup.

The old.db import code is planned for removal once the imports are confirmed: `src/core/legacy.rs`, `Feature::legacy_import`, each feature's `import.rs`, the call in `main.rs`, and the `old_database` config key. The `legacy_imports` table stays.

## Compared to voltgpt

- **A grammar instead of regexes and prefix checks.** The time can come before or after the text, the log says where parsing got stuck, and anything that is not a reminder goes to chat instead of failing.
- **Real timezones.** voltgpt mapped abbreviations to fixed offsets, so `CET` in summer was an hour off. Here `CET` means Paris time and follows summer time. Users can also save their own zone with `/timezone`; voltgpt had none.
- **One scheduler instead of a timer per reminder.** voltgpt kept a Go timer for each reminder in memory. Here the next due time is read from SQLite, so there is nothing to rebuild on startup.
- **Delivery is confirmed.** voltgpt deleted a reminder after trying to send it, even when sending failed. Here it is retried and only given up on after 10 tries, with the text in the log.
- **Images as BLOBs** instead of base64 JSON in a text column.
- **New**: snooze buttons (with a typed time), Cancel, recurring reminders, the link to the original message, the late and missing-image notes, and the chat tools.
