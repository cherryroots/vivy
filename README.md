# Vivy

Vivy is a Discord bot written in Rust: a chat companion with her own memory, moods and voice, plus reminders, a movie night betting game and repost detection. It is a rewrite of [voltgpt](https://github.com/cherryroots/voltgpt), the older Go bot, ported one feature at a time.

The code aims to be:

- **Event driven.** Each feature is its own module that listens to the Discord events it cares about, with guards deciding when it runs. Features never import each other.
- **Provider agnostic.** AI chat sits behind one trait, so Claude, OpenAI or Gemini can be swapped in.
- **Simple and readable.** Easy to study for someone learning Rust.

Built with [Twilight](https://github.com/twilight-rs/twilight) for Discord, [tokio](https://tokio.rs), and one SQLite file. [`docs/feature-map.md`](docs/feature-map.md) is the overview of every feature and what's next, [`docs/architecture.md`](docs/architecture.md) explains how the code fits together, and `docs/features/` has a page per feature.

## Features

Every feature has a section in `config.toml` and can be turned off or limited to some servers or channels. `config.example.toml` explains every setting.

### Chat

@-mention Vivy to talk to her; replying to one of her answers continues the conversation. `provider` under `[ai]` picks Claude or OpenAI, and `fallback` names a backup that takes over when the main one is out of credit (tried again a day later) or down (an hour later).

- **Tools:** web search and web pages, code in a sandbox that keeps its files per channel (on Claude, with Anthropic's skills for spreadsheets, documents, slides and PDFs), attached files, message search, pins and server events.
- **Controls:** ❌ under an answer stops or deletes it, 🔁 asks again. Only the person who asked can use them.
- **Chime-ins:** now and then (5% of messages, then a half-hour cooldown per channel) she reads the last 15 messages to learn from them; 10% of those times she adds a line and 40% she reacts.
- **Her owner:** whoever owns the bot's Discord application can tell her what to do, even her mood and personality, which nobody else can, while owner mode is on (the 👑 button on the status message, which only the owner can press). Ask her who her owner is and she checks.
- **Follow-ups:** when someone mentions something coming up, she can check in afterwards to ask how it went.
- **Cost:** Claude's and OpenAI's spend are tracked per month, each against its own `monthly_budget`, with warnings in the log channel at 80% and 100%. Optional Admin keys (Anthropic's and OpenAI's) read the real bills; read the caution in `.env.example` first.

### Memory and personality

Vivy keeps notes between conversations in a folder per server (and a private one per person in DMs): a file per person and topic under `/memories/users/<id>/`, server notes like `channels.md` and `culture.md`, and notes about herself under `/memories/vivy/`, so she grows her own personality with each server. A person's own word about themselves overrules what others said.

- `/memory show` shows what she saved about you, `/memory forget` deletes it, and admins can `/memory delete` anything.
- Once a day she tidies each server's notes and updates her notes about herself. Once a week she writes a diary entry and posts it in `diary_channels`.
- Her mood sets her Discord status, and optionally her avatar (`faces_dir`), her nickname emoji (`nickname`) and the tone of her answers. Her banner follows the time of day (`banners_dir`).
- She learns the server's custom emoji and uses them like a regular.

### Voice messages

With an ElevenLabs key, Vivy can answer with a Discord voice message in her own voice. `monthly_characters` under `[features.voice]` caps the use.

### Web search with TinyFish

With a TinyFish key, Vivy can also search the web and read whole pages with TinyFish, with every chat provider. It reaches sites the providers' own web tools often can't, like Reddit. It's free up to 12,000 searches and 1,000 pages a day. `[features.tinyfish]` sets how many results and how much of each page she reads.

### Drawing

With `OPENAI_TOKEN` set, Vivy can draw pictures with GPT Image 2.5 (on either chat provider), or change a picture someone posts or replies to. `monthly_pictures` under `[features.drawing]` caps the use (and `per_user_monthly_pictures` each person's share), and each picture's cost counts toward OpenAI's spend.

### Reminders

`@Vivy remind me in 2h30m to …`, `in an hour and a half`, `at 16:30 CET`, `tomorrow evening`, `tonight`, `this friday`, `next week`, `on the 15th`, `march 3` or `on 2026-12-24 at noon`, or on repeat: `every day at 9:00`, `every weekday at 8:30`, `every monday`, `every other week`, `every 2 hours`, `daily`. The reply has a Cancel button, `/reminders` lists and deletes them (deleting a recurring one stops it), `/timezone` sets your zone, and delivered reminders have snooze buttons (10 minutes, an hour, tomorrow at 9:00, or a time you type).

### Movie wheel

voltgpt's betting game for movie night. `/wheel status` shows the round as a picture with buttons to claim, bet and set the winner. Bets and taxes go into a pot that the bets on the winner share by stake, and a player can bet at most half their money per round. Admins use `/wheel add` (up to five people at once, each with the movie they picked), `/wheel movie` (sets or changes a pick later), `/wheel bet` and `/wheel reset` (which starts a new season). The Help button explains the rules.

### Snail detection

Links and pictures are remembered as they're posted. Right-click a message, then Apps, then **Check Snail** to see who posted it first. `/snails top` shows the server's top snailers this week and of all time. Admins can read older history with `/snails backfill start` and start the leaderboard over with `/snails reset`.

### Who said it?

`/games whosaidit` digs up old messages from the channel (or another one, or the whole server), and everyone races to press the button of who wrote each one. Faster right answers score more, and Vivy posts the scores at the end.

### Hunger Games

`/games hungergames` opens the Reaping: the host picks tributes from a menu and anyone can volunteer. Then Vivy, as the Gamemaker, writes each round of a battle royale set in the server's own channels (the Scatter from the Octagon, days and nights, the boldest fighting for a prize in the Octagon, the final showdown in it), where weapons are found in the places themed to them and alliances form and break until one tribute wins. The fallen get last words (a real message of theirs), the host can let her work in fun things she remembers about the tributes, and with an ElevenLabs key her Gamemaker announcements are also voice messages in a Gamemaker voice of their own. The host can also have her draw a map of the arena with every round, with the tributes as sprites where the action is, and the victor surrounded by everyone they killed. Each round follows the last by itself, and `/games leaderboard` keeps the server's wins, kills and Octagons won (admins can start it over with `/games resetleaderboard`). The victor message names the cowards who never dared to step into the Octagon.

### Game Boy

Vivy is learning to play Game Boy games. For now admins play by hand: `/games gameboy load` puts in a ROM from the `roms` folder (your own copies), `/games gameboy press buttons:a a up*3 start` presses buttons and posts a GIF of what happened, and `screen`, `wait`, `gif`, `save` and `restore` do what they say. In Pokémon Red, `/games gameboy look` shows what code reads from the game (where she is, her party, the map so far, the words on screen) and `/games gameboy walk x:5 y:3` walks her there. `/games gameboy task` has a cheap model carry out one short task by itself, like `go down the stairs, until you're on the floor below`. While playing she writes her own skills for the game, like a routine for battles, and `/games gameboy skills` shows them. Later stages let her play by herself and tell the server how it's going ([docs/features/gameboy.md](docs/features/gameboy.md)).

### Control panel

A status picture in a channel of your choice, refreshed every minute: uptime, version, the AI provider and its spend, Vivy's mood, per-feature stats and graphs. Warnings, errors and start and stop notices go to a log channel.

## Setup

### 1. Create the Discord application

1. Create an application at <https://discord.com/developers/applications> and add a bot to it.
2. Under **Bot**, turn on the **Presence Intent** and the **Message Content Intent**, and copy the token. Without them the bot can't connect.
3. Invite the bot with the `bot` and `applications.commands` scopes and these permissions: View Channels, Send Messages, Send Messages in Threads, Embed Links, Attach Files, Read Message History, Add Reactions, Manage Messages (to take people's ❌ and 🔁 reactions off chat answers again). Add Change Nickname if you use `nickname`, and Send Voice Messages if you use voice.
4. Create two private channels for the control panel, for example `#bot-logs` and `#bot-status`, add the bot to both (it can't post in a private channel it isn't a member of), and copy their IDs (turn on Developer Mode in Discord, then right-click the channel).

### 2. Build

You need [Rust](https://rustup.rs) (stable) and `ffmpeg`. For emoji and unusual letters in the pictures, also install `fonts-noto-color-emoji` and `fonts-noto-core`.

```bash
git clone https://github.com/cherryroots/Vivy.git
cd Vivy
cargo build --release
```

### 3. Install

```bash
sudo useradd --system --home /opt/vivy --shell /usr/sbin/nologin vivy
sudo mkdir -p /opt/vivy
sudo cp target/release/vivy /opt/vivy/
sudo cp config.example.toml /opt/vivy/config.toml
sudo cp .env.example /opt/vivy/.env
sudo chown -R vivy:vivy /opt/vivy
sudo chmod 600 /opt/vivy/.env
```

Then fill in the two files:

- `.env` holds the secrets: `DISCORD_TOKEN`, plus `ANTHROPIC_API_KEY`, `OPENAI_TOKEN` and/or `GEMINI_API_KEY` for whichever providers `[ai]` names (`OPENAI_TOKEN` also turns on drawing), and optionally `ELEVENLABS_API_KEY`, `TINYFISH_API_KEY` and the two admin keys.
- `config.toml` holds everything else: your user ID in `admins`, `main_server`, the log and status channel IDs, and per-feature settings.

### 4. Run with systemd

```bash
sudo cp deploy/vivy.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now vivy
```

`systemctl status vivy` shows whether it's running and `journalctl -u vivy -f` follows the logs. Set `RUST_LOG` in the service file to change the log level, for example `RUST_LOG=info,vivy::features::chat=debug`. The database, `vivy.db`, is created on the first start, and migrations run automatically.

**Any other way works too.** Every file the bot uses (`.env`, `config.toml`, `vivy.db`, `faces_dir`, `banners_dir`) is found relative to the folder it runs in, so you can also skip the install and run `./target/release/vivy` straight from the repo folder, in `tmux`, or however you like. `VIVY_CONFIG` points at a config file somewhere else.

If you run it from the repo folder, make the files there and keep the secrets private:

```bash
cp .env.example .env
cp config.example.toml config.toml
chmod 600 .env
```

### Updating

```bash
git pull
cargo build --release
sudo install -o vivy -g vivy target/release/vivy /opt/vivy/vivy
sudo systemctl restart vivy
```

When running from the repo folder, just rebuild and restart the bot.

### Coming from voltgpt

Copy voltgpt's `voltgpt.db` into the bot's folder (`/opt/vivy`) as `old.db` before starting. Its reminders and movie wheel game are imported once (the wheel into `main_server`), then the file is renamed to `old.db.imported`.
