# Who said it?

A game in plain Discord messages. `/games whosaidit` digs up messages from the channel's history, and for each one everyone races to press the button of the person who wrote it. Faster right answers score more. At the end, Vivy posts the scores.

## Playing it

`/games whosaidit` in any channel or thread starts a game there (`questions` picks how many, 1 to 20, default `rounds`; `channel` and `whole_server` pick where quotes come from, see below; `years`, 1 to 20, how far back they can go, default `lookback_days`). Vivy posts one question at a time: the quote, when it was said, a live countdown, and four name buttons. Anyone can play by pressing one. Only your first press counts, and only you see the "Locked in" reply. When the time is up, the same message shows the answer: the author's button turns green, a link goes to the original message, and it lists who got it (with points) and who didn't. The next question follows 5 seconds later (`pause_seconds`).

A right answer is worth 100 points plus up to 100 more for speed (all 100 at once, none at the buzzer). A channel has one game at a time. If nobody answers two questions in a row, the game stops there. Nothing is saved, so a restart ends the game.

## Where the quotes come from

By default, from the channel the game is played in. Two options of `/games whosaidit` change that:

- `channel`: another channel or thread instead.
- `whole_server`: every text and announcement channel, plus the one the game is played in. Each question says which channel its quote is from ("Said in #general, March 2025").

The game is seen by everyone in the channel it's played in, so it never shows them messages they couldn't read already. Its own channel is always fine. Any other channel must be readable by everyone in the server (the @everyone role can view it and read its history, after the channel's overwrite for @everyone) and not age-restricted, and the feature must be allowed there (`deny_channels` keeps a channel out). Picking a channel that doesn't qualify says why; the whole server simply leaves those out. A thread counts as its parent channel. `sources.rs` holds these rules.

Each game reads 6 random moments (10 for the whole server, each in a random channel) of the last `lookback_days`, 100 messages each and nothing from the last hour. A channel the bot can't read is skipped. It keeps messages that make a good quote:

- written by a person (not a bot, a webhook or a system message)
- between `min_length` and `max_length` characters and at least 3 words
- without links (they're rarely funny without the picture)
- not by anyone in `exclude_users`

Mentions, channels, custom emoji and times are turned into what Discord shows (`@Cherry`, `#channel`, `:wave:`, "(a time)"). Nobody is the answer more than twice while someone else could be. The other choices are other people who wrote quotes, shown by their server nickname.

The bot needs Read Message History in the channels it reads.

## Settings

Under `[features.games.who_said_it]`, each explained in `config.example.toml`: `rounds` (10), `answer_seconds` (20, at least 5), `pause_seconds` (5), `lookback_days` (365), `min_length` (20), `max_length` (300), `exclude_users`. The gate keys go in `[features.games]` and apply to every game: `channels = [...]` there limits where they can be played.

## Files

All under `src/features/games/who_said_it/`.

| File | What it holds |
|---|---|
| `game.rs` | The rules, with the time each pick took passed in so the tests can play a whole game: picks (first press counts), closing a round and scoring it, standings, and `make_rounds`. Pure. |
| `sources.rs` | Which channels quotes may come from: the game's own, or channels everyone can read (`everyone_can_read`, pure). |
| `quotes.rs` | Reading the sources at random moments, `quote_text` (which messages make a quote), `clean` (Discord markup to text), and server nicknames for the people shown. |
| `ui.rs` | The question, answer and score messages, and the button IDs (`games:who_said_it:pick:<game>:<round>:<person>`). Pure. |
| `play.rs` | The `whosaidit` subcommand, the task that runs a game (post, wait, show the answer, pause, next), and button presses. Running games are kept in `GAMES` by channel, so a press finds its game. |
| `mod.rs` | Its settings. |

## Why not a Discord Activity

It started as an Activity (a game window inside Discord), but Discord only lets unverified Activities be played by the app's developer team and up to 50 invited testers, in servers with fewer than 25 members ([Discord's article](https://support-dev.discord.com/hc/en-us/articles/26576097154199)). Messages and buttons work for everyone, with no web server.

## The games feature

`/games` is declared by the games feature (`src/features/games/mod.rs`), since Discord needs one command to hold the subcommands. Who said it? lives in `games/who_said_it/`, next to the second game, [the Hunger Games](hunger-games.md) in `games/hunger_games/`. The feature hands each button press to its game by the ID's second part (`games:who_said_it:...`, `games:hunger:...`). The Hunger Games use this game's `quotes.rs` and `sources.rs` for last words and the arena.
