# Control panel

The control panel lets admins watch the bot from two Discord channels. The **log channel** gets warnings, errors and start/stop notices as they happen. The **status channel** holds one message with a picture of the bot's health: uptime, latency, memory, database size, errors, the AI provider, Claude's spend, a card of numbers per feature, and the background loops' timers. The bot redraws it every minute, and a stale "Updated" time shows at a glance when the bot has crashed.

## Using it

There are no commands. Both channels are set in `config.toml`:

- the log channel under `[logging]` (`discord_channel`, `discord_level`);
- the status channel under `[features.control_panel]` (`status_channel`, `status_interval_secs`).

`config.example.toml` explains each key. Setup is in the README. Without a `status_channel` the feature does nothing; without a `discord_channel` nothing is posted to Discord, but logs still go to stdout.

The two halves are independent. The log channel is part of the core logging setup, so it keeps working with the control panel turned off, and errors from startup (before any feature runs) still reach Discord.

## Files

| File | What it holds |
|---|---|
| `src/core/timers.rs` | `Timers` and `Timer`: how background loops report when they last ran and run next. Small and self-contained. |
| `src/features/control_panel/history.rs` | Saving and loading the numbers behind the graphs (`control_panel_samples`). |
| `src/features/control_panel/render.rs` | Lays out the status picture as SVG: `Dashboard` (everything it shows), `online_svg`, `offline_svg`, and the graph helpers. Pure functions. |
| `src/features/control_panel/status.rs` | The refresh loop (`run`): gathers the numbers, draws the picture, edits the message, saves samples, and the fallback embed. Also the migrations. |
| `src/features/control_panel/mod.rs` | The `ControlPanel` feature and its `Settings`. Starts the loop. |
| `src/core/logging.rs` | The `tracing` setup: readable lines on stdout, the Discord log channel, and the in-memory error counts the status picture shows. |

The picture is drawn as SVG text, then turned into a PNG by the shared `src/util/svg.rs` (`to_png`), the same code the movie wheel uses.

## How it works

### The log channel (`src/core/logging.rs`)

The bot logs with the `tracing` crate's macros (`info!`, `warn!`, `error!`). `logging::init` runs first thing in `main`, before the bot connects. It sets up two **layers** (outputs that each see every log event):

- **stdout**: one compact line per event (`Compact`), or JSON when the `LOG_FORMAT=json` environment variable is set. `RUST_LOG` picks what is shown.
- **Discord** (`DiscordLayer`): sees events at `discord_level` or above from any crate, plus every line with the target `"lifecycle"`, whatever its level.

The dispatcher wraps every handler in a **span** (a named scope that log lines inside it belong to) with the feature, server, channel and message ID. `DiscordLayer` walks a log line's spans to find these, so each posted line names its feature (or the module that logged it, outside a handler) and links to the message that triggered it. When a handler fails, the dispatcher logs `"<feature> failed: {err:#}"`; the `{:#}` format prints the whole error chain (each "while doing X" context the error picked up).

The layer only puts lines in a queue (capacity 1000; a full queue drops the line, and stdout still has it). `spawn_discord_poster` starts once the bot is connected and empties the queue every 5 seconds. So lines logged before the connection wait in the queue and are posted then. `render` groups identical lines in one batch into one line with a count (`**×12**`), puts an icon and the level in front (lifecycle lines are posted as they are), cuts each line to 1700 bytes, and packs the lines into messages under Discord's 2000-character limit. A failure to post goes to stderr with `eprintln!`, not to `tracing`, so it can't loop.

Lifecycle lines:

- 🟢 **Started**: version, git commit, the bot's name, the features that started, and what was imported from voltgpt's `old.db` (`src/main.rs`).
- 🔴 **Shutting down** (`src/main.rs`).
- 🔌 **Reconnected to Discord** after a resume, or "with a new session" after a full reconnect (`src/core/dispatcher.rs`).

Panics are logged as errors too (target `"panic"`), with the file and line.

The layer also counts errors for the status picture: `record_error` keeps the time of every error from the last day and the text of the newest one, and `error_stats` returns errors in the last hour, in the last day, per hour for the last 24 hours, and the last error. These counts live in memory, so they start at zero after a restart.

### The status message (`status.rs`)

`ControlPanel::start` reads `Settings`, and if there is a `status_channel`, spawns `status::run`. The refresh interval is `status_interval_secs` (default 60), never below 10 seconds, to stay far under Discord's edit rate limit. A slow update skips ticks instead of catching up in a burst.

Each tick:

1. `gather` reads everything into a `Dashboard`: uptime, server count, gateway latency (the slowest shard), the process's memory (with the `sysinfo` crate), the database file size plus its `-wal` file, the error stats, the AI view, the spend view, every enabled feature's `stats()` and `panels()`, the timers, and the graph history.
2. Once per 15-minute slot, `save_samples` saves the numbers for the graphs (see below).
3. `online` draws the picture with `render::online_svg` and `svg::to_png` inside `spawn_blocking` (it takes CPU and reads font files, so it runs on a separate thread pool, not on the async threads that answer Discord). The fallback embed is built first, so if drawing fails, or even panics, the message shows the same information as an embed.
4. `show` edits the message: it replaces the text and removes the old picture or embed. If the message was deleted (Discord error 10008, Unknown Message), it posts a new one.

The text above the picture is `-# Updated <t:…:R>`. Discord renders that timestamp as "12 seconds ago" and keeps counting on its own, so if the bot dies without a goodbye, the message visibly goes stale.

The message's location is saved in `control_panel_state` under the key `status_message` as `"<channel>/<message>"`. After a restart the bot edits the same message. If `status_channel` was changed in the config, the saved one is ignored and a new message is posted.

On shutdown, `ctx.shutdown` is cancelled. The loop's `tokio::select!` is `biased` (it checks the shutdown first), so a tick that is due at the same moment can't skip the goodbye. It then swaps in an OFFLINE picture (`render::offline_svg`: name, version, when it stopped, how long it was up) with `-# Stopped <t:…:f>` above it, or a 🔴 Offline embed if drawing fails. `main` gives background tasks 10 seconds to finish this.

### What the picture shows (`render.rs`)

From top to bottom, as `online_svg` draws it:

- **Header**: the bot's name with an ONLINE label; version and git commit and "up 3d 4h" on the right; the server count and the time of the update (UTC) below. The git commit comes from `build.rs`, which bakes it into the binary as `GIT_COMMIT`.
- **Four tiles**: latency, memory and database, each with a graph of the last day; and errors, with the count for the last 24 hours, the count this hour, and a bar per hour.
- **AI**: the provider and model chat uses now, with a MAIN or FALLBACK label, and when on the fallback, why and when the main one is tried again.
- **Panels**: one box per title from the features' `panels()`. Panels with the same title are merged into one box by `merge_panels`. Today that is "Vivy's mind" (`Panel::about_bot`): memory adds her mood, what she is thinking about and what she wants to know, with her face in a circle on the right; chat adds her next check-in in a server, with whom, what about and how many are planned.
- **Claude spend this month** (only when Claude is set up): the total, a bar against the monthly budget (green, amber from 80% or when the pace runs over, red over budget), BILLED or ESTIMATE, the pace to the month's end (shown after the month's first day), a graph of the month so far with the pace (dotted) and the budget (dashed), and the admin key's state with the last bill read. Below, when turned on in `[ai.claude]`, the cache hit rate as a ring and the cost per job as bars. The numbers come from the AI spend tracking in `src/ai/spend.rs` (the per-job numbers from the core `ai_spend_jobs` table).
- **Features**: one card per enabled feature with stats, three per row. A stat whose value is a plain number gets a graph of the last 7 days and how much it changed. Other values (like `<t:…:R>` timestamps, which are written out as "in 3h 12m") show as text. A feature whose `stats()` failed shows the error on its card.
- **Timers**: one small card per background loop, two per row: its name, how often it runs, when it last ran, and when it runs next. "running" while it works, and "due" in amber when the next run is more than a minute late (a loop that is stuck or busy).
- **Last error**, in a red box, with when it happened.

### Graph history (`history.rs`)

The first refresh in each 15-minute slot (`EVERY_SECS`) saves one sample per number in `control_panel_samples`: `latency_ms`, `memory_mb`, `database_mb`, `spend:claude` (this month's spend), and `stat:<feature>:<stat name>` for every feature stat that parses as a number. All samples of a slot get the slot's start time, so they line up. Samples older than 32 days (`KEEP_DAYS`) are deleted.

Each refresh loads only what the graphs draw (`load_for_graphs`): the last 7 days of everything, plus this month's spend samples. A graph needs two points (the current value counts as one), so a new install shows plain numbers for its first 15 minutes. A stat's history is keyed by its name, so renaming a stat starts its graph over.

### Timers (`src/core/timers.rs`)

`ctx.timers` is a `Timers`: a list shared by everyone, behind `Arc<Mutex<..>>` (`Arc` lets many owners share one value; `Mutex` lets only one of them change it at a time). A background loop adds itself once and gets a `Timer` back:

```rust
let timer = ctx.timers.add("Snail picture retries", "every 5m");
loop {
    timer.sleeping(CHECK_EVERY);   // or timer.next_at(time)
    tokio::time::sleep(CHECK_EVERY).await;
    timer.running();               // woke up, working now
    // ... do the work ...
}
```

- `add(name, every)` puts the loop on the list (both are `&'static str`, text fixed at compile time). Adding a name that is already there starts its timer over, so a restarted loop doesn't appear twice.
- `running()` records the time it last ran and clears "next".
- `sleeping(wait)` or `next_at(time)` records when it runs next.

Only loops that actually started are listed, so a turned-off feature's loop doesn't appear. The control panel adds its own timer, `Status graphs` (`every 15m`), which reports when samples are saved.

## Feature hooks it uses

| Method | What it does here |
|---|---|
| `name` | `"control_panel"` |
| `migrations` | `status::MIGRATIONS` |
| `start` | Reads `Settings` and spawns the status loop when there is a `status_channel`. |

It doesn't implement `stats` or `panels` itself. Instead it calls them on every other feature. The log channel uses no hooks at all: it is core code, set up in `src/main.rs`.

## Tables

From `status::MIGRATIONS`:

- **Migration 1**, `control_panel_state`: key and value. Holds the status message's location (`status_message` → `"<channel>/<message>"`).
- **Migration 2**, `control_panel_samples`: name, time (`at`, unix seconds) and value; one row per name per slot. The graphs' history.

The cache hit rate and cost per job are read from the core `ai_spend_jobs` table (see `src/core/db.rs` and `src/ai/spend.rs`), which this feature doesn't own.

## Background loops

| Timer name | Every | What |
|---|---|---|
| `Status graphs` | `every 15m` | Marks when the status loop saves graph samples. |

The status refresh itself (every `status_interval_secs`) and the log poster (every 5 seconds) are background tasks without a timer.

## Adding to the status picture

A feature gets on the status picture through its `Feature` methods (`src/core/feature.rs`). Nothing needs registering; the control panel asks every enabled feature on every refresh.

- **A card**: implement `stats` and return a list of `Stat::new("Name", value)`. `value` can be anything that turns into text. A feature with no stats gets no card. Keep stats cheap: they run every minute.
- **A graph on the card**: give a stat a plain number as its value (like `Stat::new("Pending", 4)`). It is saved every 15 minutes and drawn over 7 days. Keep the stat's name stable, since the history is stored under it.
- **A box of longer text**: implement `panels` and return `Panel`s, each a title, rows (`Stat`s, wrapped to up to three lines), and an optional picture as a PNG `data:` URL. Use `Panel::about_bot(ctx, rows)` to add rows to the shared "Vivy's mind" box.
- **A timer**: in a loop started from `start`, call `ctx.timers.add(name, every)` once and report with `running()`, `sleeping()` or `next_at()` as shown above.
- **Something on the log channel**: just log it. `warn!` and `error!` reach the channel at the default `discord_level`; use `info!(target: "lifecycle", ...)` only for start and stop notices.

The fallback embed (`online_embed`) shows stats, panels and timers too, so these appear even when the picture can't be drawn.
