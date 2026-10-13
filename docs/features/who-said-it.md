# Who said it?

A game in plain Discord messages. `/whosaidit` digs up messages from the channel's history, and for each one everyone races to press the button of the person who wrote it. Faster right answers score more. At the end, Vivy posts the scores.

## Playing it

`/whosaidit` in any channel or thread starts a game there (`questions` picks how many, 1 to 20, default `rounds`). Vivy posts one question at a time: the quote, when it was said, a live countdown, and four name buttons. Anyone can play by pressing one. Only your first press counts, and only you see the "Locked in" reply. When the time is up, the same message shows the answer: the author's button turns green, a link goes to the original message, and it lists who got it (with points) and who didn't. The next question follows 5 seconds later.

A right answer is worth 100 points plus up to 100 more for speed (all 100 at once, none at the buzzer). A channel has one game at a time. If nobody answers two questions in a row, the game stops there. Nothing is saved, so a restart ends the game.

## Where the quotes come from

Only from the channel the game is played in. Everyone who sees the game can already scroll up to those messages, so it never shows anything they couldn't see.

Each game reads 6 random moments of the last `lookback_days` (100 messages each, nothing from the last hour), and keeps messages that make a good quote:

- written by a person (not a bot, a webhook or a system message)
- between `min_length` and `max_length` characters and at least 3 words
- without links (they're rarely funny without the picture)
- not by anyone in `exclude_users`

Mentions, channels, custom emoji and times are turned into what Discord shows (`@Cherry`, `#channel`, `:wave:`, "(a time)"). Nobody is the answer more than twice while someone else could be. The other choices are other people who wrote quotes, shown by their server nickname.

The bot needs Read Message History in the channel.

## Settings

Under `[features.who_said_it]`, each explained in `config.example.toml`: `rounds` (10), `answer_seconds` (20, at least 5), `lookback_days` (365), `min_length` (20), `max_length` (300), `exclude_users`. The gate keys work too: `channels = [...]` limits where it can be played.

## Files

All under `src/features/who_said_it/`.

| File | What it holds |
|---|---|
| `game.rs` | The rules, with the time each pick took passed in so the tests can play a whole game: picks (first press counts), closing a round and scoring it, standings, and `make_rounds`. Pure. |
| `quotes.rs` | Reading the channel at random moments, `quote_text` (which messages make a quote), `clean` (Discord markup to text), and server nicknames for the people shown. |
| `ui.rs` | The question, answer and score messages, and the button IDs (`who_said_it:pick:<game>:<round>:<person>`). Pure. |
| `play.rs` | `/whosaidit`, the task that runs a game (post, wait, show the answer, pause, next), and button presses. Running games are kept in `GAMES` by channel, so a press finds its game. |
| `mod.rs` | The `WhoSaidIt` feature and its settings. |

## Why not a Discord Activity

It started as an Activity (a game window inside Discord), but Discord only lets unverified Activities be played by the app's developer team and up to 50 invited testers, in servers with fewer than 25 members ([Discord's article](https://support-dev.discord.com/hc/en-us/articles/26576097154199)). Messages and buttons work for everyone, with no web server.
