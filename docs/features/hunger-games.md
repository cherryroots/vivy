# Hunger Games

A battle royale between server members, played in plain Discord messages. The host picks the tributes (others can volunteer), and Vivy, as the Gamemaker, writes every round of what happens in the arena until one tribute is left. The arena's areas are the server's own channels.

It's one of the games in the games feature (`src/features/games/`), next to [who said it?](who-said-it.md): Discord needs one `/games` command to hold both games, so they share that feature and its gate, `[features.games]`. It borrows who said it?'s quote reader and channel rules for last words and the arena.

Stages 1 to 4 of the plan are built: the text game, last words, her memory notes about the tributes, the Gamemaker's voice, and pictures. Still to come: a leaderboard.

## Playing

1. `/games hungergames` opens **the Reaping** in the channel. Whoever ran it is the host.
2. The host picks tributes from the user menu. Anyone can press **Volunteer** to enter (and again to back out). Bots can't enter. 2 to 24 tributes (`max_tributes`).
3. The host can press **🧠 Memories** to turn Vivy's memory notes about the tributes on (they're off by default; see [Memory notes](#memory-notes)) and **🎨 Pictures** to have her draw a map of the arena after every round (off by default; see [Pictures](#pictures)), then **Start**, or **Cancel**. Nobody starting within `lobby_minutes` (10) closes the Reaping.
4. The rounds follow, one message each (two or more when a round is long): **the Bloodbath**, then days and nights, with **the Feast** instead of day 2 (when at least 4 are alive), and **the final showdown** once two are left. Each event is its own paragraph, 💀 when someone dies in it. Rounds, cannons and the victor are plain message text, not embeds, because embeds use Discord's smaller font; they ping nobody, whatever a name or her text says. Her Gamemaker line opens the Games, the Feast and the final showdown, and is also spoken as a voice message in the Gamemaker's voice just before that round (see [Gamemaker voice](#gamemaker-voice)).
5. After each night, **the fallen of the day** lists who died since the last list, each with their **last words** (a real message they once sent, with a link to it) when one was found, all behind spoilers so people still reading don't find out early. Each one's kills are next to their name. When the game ends, **the last of the fallen** covers the rest, the final showdown's loser among them.
6. The next round starts by itself after `auto_advance_seconds` (45). There's no Next button: Cherry turned manual advancing off so each round and its picture can go out together. Bot admins can press the host's buttons in the Reaping too.
7. At the end, **the victor** message: her line to the winner, and everyone's placing with their kills, all behind spoilers (the heading only says there is a victor). Her line to the winner follows as a voice message.

A channel has one Hunger Games at a time, and a restart ends the game (nothing is saved).

## Settings

Under `[features.games.hunger_games]` in config.toml: `max_tributes` (24, at most 24), `lobby_minutes` (10), `auto_advance_seconds` (45, at least 5).

Each round is one model call with the job `hunger_games`: the chat provider's model at low effort, with no web or code (see [ai.md](../ai.md#jobs)). `[ai.claude.jobs.hunger_games]` changes that.

## Files

All under `src/features/games/hunger_games/`.

| File | What it holds |
|---|---|
| `game.rs` | The rules: which round comes next, how many should die (`deaths_range`), checking a round she wrote (`check`), applying it (deaths and kills), the fallen and the placings. Pure. |
| `writer.rs` | What she's told each round (`brief`), reading her JSON (`parse`), and `write`, which asks her and sends a round that fails `check` back with the problem. |
| `ui.rs` | The Reaping (an embed with a menu and buttons), and the round, cannon and victor texts (split with `util::split` when a round is over 2000 characters), and the IDs of the buttons and the menu (`games:hunger:<game>:<action>`). Pure. |
| `last_words.rs` | Last words: searches each tribute's messages in the public channels with Discord's message search (`util::search`), with random reads of history (`quotes::read_once`) for anyone the search finds nothing for (`choose`, `pick` and `tidy`, pure). |
| `pictures.rs` | Her pictures of the Games, through the drawing feature's `draw_picture` tool (see below). |
| `looks.rs` | How the Games look in her pictures: one model call with the profile pictures and places when the Games start (`describe`, shared by every round's drawing through `Shared`), reading the answer (`parse`), the map's layout (`map`), putting the sprites into a picture prompt (`fill`, `prompt`), the zoomed-in final showdown (`showdown`) and the victor's picture (`victor`), all but `describe` pure. |
| `voice.rs` | The Gamemaker's lines as voice messages, through the voice feature's `send_voice_message` tool (see below). |
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
{"gamemaker": "…or null", "events": [{"text": "{T1} …", "tributes": ["T1", "T2"], "deaths": [{"tribute": "T2", "by": "T1"}]}], "victory": "…or null", "picture": "…or null"}
```

`parse` takes the object out of her answer (a code fence around it is fine), and `Game::check` turns it into a `Round` or a sentence saying what's wrong. A wrong round goes back to her with that sentence, as the next turn of the same conversation; after 3 tries the game is called off with a message.

### Last words

When the Games start, a task looks for each tribute's last words, 4 tributes at a time, with Discord's message search (`util::search`): their messages (`author_type` user, no links) in the channels everyone can read (the arena's channels and the one the game is played in), from `lookback_days` ago to an hour ago. One search finds how many there are (and the newest 25); then up to 3 pages at random offsets, stopping at the first with a message that fits the who-said-it rules (`min_length`, `max_length`, at least 3 words, no links) and is at most 200 characters. That's at most 4 Discord calls per tribute, no AI. Anyone the search finds nothing for (Discord may still be indexing the server) gets up to 3 random reads of 100 messages per tribute with the who-said-it quote reader. People in `exclude_users` get none. The words are put on one line. The first cannons wait for the task if it isn't done. Someone with nothing found falls without last words.

### Memory notes

Before the Bloodbath, if the host turned **Memories** on in the Reaping (it starts off: Cherry found the notes lame in practice), `notes::about` reads Vivy's notes about each tribute: the files in `/memories/users/<user id>/` of the server's folder, at most 6 files and 800 characters per tribute. It doesn't import the memory feature: it finds the feature that offers the `memory` chat tool and runs `view` with it as Vivy, so the memory feature's gate and rules still apply (memory off in that server or channel means no notes).

Each round's brief lists the notes of the living tributes, with the instruction to use a detail only where it makes a moment funnier or more personal, a few per round at most, never anything private, sensitive, hurtful or embarrassing (health, relationships, family, where they live, work or money troubles, anything told in confidence), and to leave them out when nothing fits (Cherry's call: fun things when there are some, nothing forced).

### Gamemaker voice

Her four or so Gamemaker lines a game (opening the Games, the Feast, the final showdown, and her words to the victor) are spoken in a voice of their own: `gamemaker` in `[features.voice.voices]`, `DucUc05HgR5M3wbY6WkG` by default, a voice Cherry made with ElevenLabs Voice Design. `voice::speak` doesn't import the voice feature: it finds the feature that offers `send_voice_message` and runs it as Vivy with `voice: "gamemaker"` and `reply: false`, so the voice message stands on its own in the channel and counts toward the voice feature's monthly limit. Her mood isn't in it.

The writer's format tells her the lines are spoken and lets her steer them with audio tags (`[booming]`, `[dramatic pause]`), each line under 300 characters. The round and victor texts show the line without the tags (`ui::unspoken`). Without an ElevenLabs key, with voice off in the channel, with the `gamemaker` voice empty, or when the month's characters are used up, the lines are text only; the round waits for the voice message (a few seconds, with the typing indicator) before it's posted.

### Pictures

When the host turns **🎨 Pictures** on in the Reaping (off by default, since each costs about $0.02), every round comes with a picture, nights included. Each round gets a map: a top-down 2D map like a retro strategy game, with a supply camp in the middle, a labelled zone for each of the arena's places, and the tributes as pixel-art sprites (a little bigger than usual, to read them) where each event happens. Cherry chose the map because it's the easiest to read (realistic and cartoon scenes came first). With each of those rounds she also writes a `picture`: every event's zone and what its tributes do there, with tags for the tributes and anyone who dies lying dead where they fell. The Bloodbath shows the race to the supply camp, and nights are dark with campfires and hiding spots. Her picture prompts use plain words ("the supply camp", "players"), never Hunger Games terms, since the image model drew a literal cornucopia. Each map also says exactly how many players are on it (everyone alive when the round began) and that nobody else is, and zone terrain is scenery only, since the first maps filled zones with crowds, ninjas and robots. The final showdown's map is zoomed in on the one zone where the last two fight (`looks::showdown`), so they're drawn big. Then the victor message gets a picture of its own, made by the bot (`looks::victor`): the winner as a large full-body figure, with everyone they killed lying dead around them. The showdown and the victor are drawn side by side. The pictures show deaths plainly, with some pixel-art blood (Cherry found knocked-out players sitting around awkward); the round texts still keep to cartoon violence. A 12-tribute game is about 11 pictures, about $0.22.

**Looks.** So the map and the tributes look the same in every picture, `looks::describe` runs once as the Games start, while she writes the Bloodbath: one model call (the `hunger_games` job) with every tribute's profile picture (256 px, their server one when set) and the arena's places. It describes a sprite for each tribute, inspired by their picture (bold colors, headgear, outfit, one signature item), and each zone's terrain, fitting its name when it can. A tribute whose picture can't be loaded gets a sprite invented from their name. `looks::map` lays the zones out around the Cornucopia in a fixed order (north, east, south, west, then the diagonals, then rings further out), labelled with the place's name in plain letters, and `looks::fill` puts each tribute's sprite in place of their tag in her prompt (or "a tribute" when there's none). The first picture waits for the looks if they aren't ready. Without the drawing tool there (drawing off in the channel, or no OpenAI key) the call isn't made and no prompts are asked for.

**Drawing.** Each map is drawn right after its round is written, in the same background task, so the round and its map are posted together in one message (the map on the round's last part). The Bloodbath's map is drawn once the round is posted instead, as its own message ("🗺️ The arena as the Games begin"), since nothing waits on it and people are already reading. `pictures::draw` doesn't import the drawing feature: it finds the feature that offers `draw_picture` and runs it with the host as the asker and arguments the chat model isn't told about: the style added to the prompt (`MAP`, `SHOWDOWN` or `VICTORY`), landscape, `post: false` so the picture comes back to be posted with the round, and the job `game_art`, so the pictures' cost has its own line in OpenAI's spend. The drawing feature's gate and limits apply, and the pictures count toward the host's monthly pictures. A picture that fails is simply missing, and a round whose picture Discord refuses is posted without it.

### Pace

While people read a round, the next one is already being written (and drawn), in a task started right after posting. After `auto_advance_seconds` the game waits for it if it isn't done yet, with Discord's typing indicator showing.

## Feature hooks it uses

Through the games feature (`games/mod.rs`): the `hungergames` subcommand of `/games`, and `on_component` for IDs starting with `games:hunger:`.

## Tables

None yet.

## Background loops

None: a task per running game.
