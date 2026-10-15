# Hunger Games

A battle royale between server members, played in plain Discord messages. The host picks the tributes (others can volunteer), and Vivy, as the Gamemaker, writes every round of what happens in the arena until one tribute is left. The arena's areas are the server's own channels.

It lives inside the [who said it?](who-said-it.md) feature, because Discord needs one `/games` command to hold both games. Its gate is that feature's gate.

Stages 1 and 2 of the plan are built: the text game, last words, and her memory notes about the tributes. Still to come: Gamemaker voice messages in their own voice, pictures she draws during the game, and a leaderboard.

## Playing

1. `/games hungergames` opens **the Reaping** in the channel. Whoever ran it is the host.
2. The host picks tributes from the user menu. Anyone can press **Volunteer** to enter (and again to back out). Bots can't enter. 2 to 24 tributes (`max_tributes`).
3. The host presses **Start**, or **Cancel**. Nobody starting within `lobby_minutes` (10) closes the Reaping.
4. The rounds follow, one message each (two or more when a round is long): **the Bloodbath**, then days and nights, with **the Feast** instead of day 2 (when at least 4 are alive), and **the final showdown** once two are left. Each event is its own paragraph, 💀 when someone dies in it. Rounds, cannons and the victor are plain message text, not embeds, because embeds use Discord's smaller font; they ping nobody, whatever a name or her text says. Her Gamemaker line opens the Games, the Feast and the final showdown.
5. After each night, **the fallen of the day** lists who died since the last list, each with their **last words** (a real message they once sent, with a link to it) when one was found, all behind spoilers so people still reading don't find out early. When the game ends, **the last of the fallen** covers the rest, the final showdown's loser among them.
6. The host presses **Next** to move on, or the next round starts by itself after `auto_advance_seconds` (30). Bot admins can press the host's buttons too.
7. At the end, **the victor** message: her line to the winner, and everyone's placing with their kills, all behind spoilers (the heading only says there is a victor).

A channel has one Hunger Games at a time, and a restart ends the game (nothing is saved).

## Settings

Under `[features.who_said_it.hunger_games]` in config.toml: `max_tributes` (24, at most 24), `lobby_minutes` (10), `auto_advance_seconds` (30, at least 5).

Each round is one model call with the job `hunger_games`: the chat provider's model at low effort, with no web or code (see [ai.md](../ai.md#jobs)). `[ai.claude.jobs.hunger_games]` changes that.

## Files

All under `src/features/who_said_it/hunger/`.

| File | What it holds |
|---|---|
| `game.rs` | The rules: which round comes next, how many should die (`deaths_range`), checking a round she wrote (`check`), applying it (deaths and kills), the fallen and the placings. Pure. |
| `writer.rs` | What she's told each round (`brief`), reading her JSON (`parse`), and `write`, which asks her and sends a round that fails `check` back with the problem. |
| `ui.rs` | The Reaping (an embed with a menu and buttons), and the round, cannon and victor texts (split with `util::split` when a round is over 2000 characters), and the IDs of the buttons and the menu (`who_said_it:hunger:<game>:<action>`). Pure. |
| `last_words.rs` | Last words: reads the public channels' history with the who-said-it quote reader (`quotes::read_once`, 100 messages a read, 4 at once) until every tribute has a short quote, up to 5 reads per tribute (`pick`, pure). |
| `notes.rs` | Her memory notes about each tribute, read through the `memory` chat tool (see below). `files_in` and `file_text` read the tool's answers (pure). |
| `play.rs` | `/games hungergames`, the task that runs the Reaping and then the game, and the presses. Running games are kept in `GAMES` by channel. |
| `mod.rs` | The settings. |

## How it works

### Who decides what

Vivy writes everything: which events happen, who meets whom, who dies and who kills them. The code only checks that what she wrote can happen, and sets the pace:

- every living tribute is in exactly one event, and nobody dead or unknown is;
- each event names its tributes in the text, and only them;
- a death is of someone in that event, and a killer is another tribute in it (or nobody, for an accident or the arena);
- the pace (`deaths_range`): at most about a fifth of the living die in the Bloodbath, a quarter at the Feast, and an eighth to a sixth on days and nights, with at least half of that. A day or night may pass with nobody dying when the most is 1, but never two in a row. Until the final showdown at least two stay alive, so there's always a final two. In a simulation, 8 tributes took about 7 or 8 rounds, 12 about 10, and 24 about 16.

Her mood isn't in the prompt, so it never changes what happens. The prompt asks for cartoon violence with no gore, nothing sexual or hateful, no jokes about anyone's real body, health, identity or private life, and no favourites.

### Tags instead of names

In what she writes, tributes are tags: `T1` is the first tribute. The text says `{T3} trips over a root`, and `ui.rs` fills in the name in bold when it's shown (`fill`). Names can't be misspelled or confused, and a name with Discord formatting in it stays as it is.

### One round

`writer::brief` tells her the arena's places: one per tribute, picked at random when the game starts from the channels everyone can read (the same rule as who said it?), leaving out the channel the game is played in, and the same places all game. It also tells her each tribute's tag, name, and kills or death, the story so far with names filled in, what kind of round it is, and how many must die. She answers with one JSON object:

```json
{"gamemaker": "…or null", "events": [{"text": "{T1} …", "tributes": ["T1", "T2"], "deaths": [{"tribute": "T2", "by": "T1"}]}], "victory": "…or null"}
```

`parse` takes the object out of her answer (a code fence around it is fine), and `Game::check` turns it into a `Round` or a sentence saying what's wrong. A wrong round goes back to her with that sentence, as the next turn of the same conversation; after 3 tries the game is called off with a message.

### Last words

When the Games start, a task reads the history of every channel everyone can read (the arena's channels and the one the game is played in) at random moments, 100 messages a read, with the same rules as who said it? (`lookback_days`, `min_length`, `max_length`, `exclude_users`, no links, no bots). It reads 4 at a time and keeps going until every tribute has a quote up to 200 characters, up to 5 reads per tribute (120 for 24 tributes). A read is one Discord call and no AI, so this is free and takes seconds. Each tribute gets a random one of the quotes found for them, on one line. The first cannons wait for the task if it isn't done. Someone with nothing in what was read falls without last words. Reads stop early if a whole batch fails (the channels can't be read).

### Memory notes

Before the Bloodbath, `notes::about` reads Vivy's notes about each tribute: the files in `/memories/users/<user id>/` of the server's folder, at most 6 files and 800 characters per tribute. It doesn't import the memory feature: it finds the feature that offers the `memory` chat tool and runs `view` with it as Vivy, so the memory feature's gate and rules still apply (memory off in that server or channel means no notes).

Each round's brief lists the notes of the living tributes, with the instruction to use a detail only where it makes a moment funnier or more personal, a few per round at most, never anything private, sensitive, hurtful or embarrassing (health, relationships, family, where they live, work or money troubles, anything told in confidence), and to leave them out when nothing fits (Cherry's call: fun things when there are some, nothing forced).

### Pace

While people read a round, the next one is already being written (a task started right after posting). Next, or the timer, then waits for it if it isn't done yet, with Discord's typing indicator showing. The Next button comes off each round when the game moves on, and isn't on the last round.

## Feature hooks it uses

Through the who-said-it feature: the `hungergames` subcommand of `/games`, and `on_component` for IDs starting with `hunger:`.

## Tables

None yet.

## Background loops

None: a task per running game.
