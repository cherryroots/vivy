# Hunger Games

A battle royale between server members, played in plain Discord messages. The host picks the tributes (others can volunteer), and Vivy, as the Gamemaker, writes every round of what happens in the arena until one tribute is left. The arena's areas are the server's own channels.

It lives inside the [who said it?](who-said-it.md) feature, because Discord needs one `/games` command to hold both games. Its gate is that feature's gate.

This is stage 1 of the plan: the text game. Still to come: last words (a real message from each fallen tribute), fun memory notes about tributes, Gamemaker voice messages in their own voice, pictures she draws during the game, and a leaderboard.

## Playing

1. `/games hungergames` opens **the Reaping** in the channel. Whoever ran it is the host.
2. The host picks tributes from the user menu. Anyone can press **Volunteer** to enter (and again to back out). Bots can't enter. 2 to 24 tributes (`max_tributes`).
3. The host presses **Start**, or **Cancel**. Nobody starting within `lobby_minutes` (10) closes the Reaping.
4. The rounds follow, one message each (two or more when a round is long): **the Bloodbath**, then days and nights, with **the Feast** instead of day 2 (when at least 4 are alive), and **the final showdown** once two are left. Each event is its own paragraph, 💀 when someone dies in it. Rounds, cannons and the victor are plain message text, not embeds, because embeds use Discord's smaller font; they ping nobody, whatever a name or her text says. Her Gamemaker line opens the Games, the Feast and the final showdown.
5. After each night, **the fallen of the day** lists who died since the last list.
6. The host presses **Next** to move on, or the next round starts by itself after `auto_advance_seconds` (30). Bot admins can press the host's buttons too.
7. At the end, **the victor** message: her line to the winner, and everyone's placing with their kills.

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
| `play.rs` | `/games hungergames`, the task that runs the Reaping and then the game, and the presses. Running games are kept in `GAMES` by channel. |
| `mod.rs` | The settings. |

## How it works

### Who decides what

Vivy writes everything: which events happen, who meets whom, who dies and who kills them. The code only checks that what she wrote can happen, and sets the pace:

- every living tribute is in exactly one event, and nobody dead or unknown is;
- each event names its tributes in the text, and only them;
- a death is of someone in that event, and a killer is another tribute in it (or nobody, for an accident or the arena);
- about a quarter to a third of the living die each round (`deaths_range`), and until the final showdown at least two stay alive, so there's always a final two. 24 tributes take about 8 rounds.

Her mood isn't in the prompt, so it never changes what happens. The prompt asks for cartoon violence with no gore, nothing sexual or hateful, no jokes about anyone's real body, health, identity or private life, and no favourites.

### Tags instead of names

In what she writes, tributes are tags: `T1` is the first tribute. The text says `{T3} trips over a root`, and `ui.rs` fills in the name in bold when it's shown (`fill`). Names can't be misspelled or confused, and a name with Discord formatting in it stays as it is.

### One round

`writer::brief` tells her the arena (up to 40 channel names that everyone can read, the same rule as who said it?), each tribute's tag, name, and kills or death, the story so far with names filled in, what kind of round it is, and how many must die. She answers with one JSON object:

```json
{"gamemaker": "…or null", "events": [{"text": "{T1} …", "tributes": ["T1", "T2"], "deaths": [{"tribute": "T2", "by": "T1"}]}], "victory": "…or null"}
```

`parse` takes the object out of her answer (a code fence around it is fine), and `Game::check` turns it into a `Round` or a sentence saying what's wrong. A wrong round goes back to her with that sentence, as the next turn of the same conversation; after 3 tries the game is called off with a message.

### Pace

While people read a round, the next one is already being written (a task started right after posting). Next, or the timer, then waits for it if it isn't done yet, with Discord's typing indicator showing. The Next button comes off each round when the game moves on, and isn't on the last round.

## Feature hooks it uses

Through the who-said-it feature: the `hungergames` subcommand of `/games`, and `on_component` for IDs starting with `hunger:`.

## Tables

None yet.

## Background loops

None: a task per running game.
