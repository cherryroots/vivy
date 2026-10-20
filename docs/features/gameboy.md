# Game Boy

Vivy will play Game Boy games by herself: work out the controls and what the game wants, play in short capped sessions, and post now and then in a Discord thread about how far she has got. This is being built in stages, one pull request each. **Stages 1 to 3 are done: the emulator, played by hand with admin commands; code that reads Pokémon Red's screen and memory, keeps a map of each area and walks her anywhere on it; and a cheap model (the supervisor) that carries out one short task at a time, with the gate that measures how well it does. Stage 4 is under way: so far, skills she writes herself. She doesn't play on her own yet.**

The plan, with the research behind it and every decision, is the "Vivy plays any Game Boy game" doc in the project. The stages are at the bottom of this page.

## Playing by hand

There's one Game Boy for the whole bot. It only moves when someone presses something, so nothing runs in the background. Every command is for admins (`admins` in `config.toml`).

| Command | What it does |
|---|---|
| `/games gameboy load rom:` | Puts a ROM from `roms_dir` in and switches it on, with a GIF of the first 4 seconds. Whatever was in before is gone (save it first). |
| `/games gameboy press buttons:` | Presses buttons one after another and posts a GIF of what happened. `hold:` is how many frames each press is held (default 16, one step in most games that walk on a grid), `gap:` how many frames nothing is held after each (default 16). |
| `/games gameboy wait seconds:` | Lets the game run with nothing pressed (1 to 60 s, default 3), with a GIF. |
| `/games gameboy screen` | The screen as it is now, as a PNG, with how long it has been played. |
| `/games gameboy gif seconds:` | The last few seconds (at most 10) as a GIF. |
| `/games gameboy look` | What code reads from the game: where she is, the facts from its memory (badges, money, party, bag, story so far), the map of the area so far, the words on screen and the last 5 steps. Pokémon Red and Blue only for now. |
| `/games gameboy walk x: y:` | Walks to that step of the map (code finds the way and takes it), with a GIF and the `look` after. Leave out `x` and `y` to walk to the nearest step not seen yet. Stops early when she walks into another area or something takes over the screen, like a text box. |
| `/games gameboy task task: actions:` | A cheap model (the supervisor, below) carries out one short task by itself, like `go down the stairs, until you're on the floor below`, taking at most `actions:` actions (default 30). Posts whether it says it's done, its last actions, a GIF of the last seconds and the `look` after. Pokémon Red and Blue only for now. |
| `/games gameboy skills forget:` | The skills she has written for the game that's in (below), each with when it's for, its notes, its rules and how its last use went. `forget:` deletes one by name. |
| `/games gameboy save name:` | Saves the game exactly as it is, under a name like `before Brock`. |
| `/games gameboy restore name:` | Goes back to a save of the ROM that's in. |
| `/games gameboy off` | Takes the ROM out. |

What to press is typed as words, with spaces or commas between them:

- a button: `a`, `b`, `start`, `select`, `up`, `down`, `left`, `right` (or `u`, `d`, `l`, `r`)
- two at once: `a+b`
- a repeat: `up*3` (or `upx3`)
- a pause as long as a press: `wait`

So `start wait a a up*3 a` presses Start, waits, presses A twice, Up three times and A. One command takes at most 64 presses.

## ROMs and saves

ROMs are `.gb` and `.gbc` files in `roms_dir` (default `roms/`, next to the bot). They have to be your own copies of games you own; the bot never downloads any. Game Boy Color games run on a Game Boy Color, the rest on an original Game Boy. Game Boy Advance games don't run (that's stage 7).

Save states go in `saves_dir` (default `gameboy/`), a folder per ROM named after its file: `gameboy/red/before Brock.state`. A state is the whole machine, about 50 KB, so it only loads into the same ROM. A save the game makes itself (the SAVE menu in Pokémon) lives in the cartridge's memory, which a state includes, so saving a state after it keeps it. Her skills for a game are in the same folder, as `skills.json`. Going back to a save keeps them, since they're what she knows rather than where the game is. Both folders are in `.gitignore`.

## How it works

`boytacean` (Apache-2.0, pinned to exactly 0.13.2, since save states may not load in another version) emulates the Game Boy and Game Boy Color in Rust, headless. It starts each game with its own open-source boot ROM, so no Nintendo boot ROM is needed. On Cherry's Pokémon Red it runs about 60 times real time on one core in a release build, with sound off.

- **One frame at a time.** `Emulator::frame` runs one frame (1/60 s of game time) and stops at the end of V-Blank, the point where the next frame starts. Saving a state anywhere else, for example right as V-Blank starts while the game's V-Blank interrupt is pending, loses that interrupt, and the game drifts from the one that saved it. Stopping there every time makes every save safe, and a loaded state plays back the same inputs frame for frame (tested on Red).
- **Its own thread.** `console.rs` runs the one `Machine` on a thread of its own and hands it jobs: `console().with(|machine| ...)` sends a closure there and waits for the answer, like `ctx.db.call` for the database. Emulating never holds up the async tasks. If the emulator panics, that job fails ("the Game Boy crashed... and was turned off"), the machine is taken out, and the thread carries on.
- **GIFs.** The machine keeps a screen every 3 frames (20 a second) for the last 10 seconds. A Game Boy shows 4 shades and a Game Boy Color at most 56 colours at once, so a GIF's 256-colour palette holds them exactly, without dithering. Screens the same as the one before are folded into one longer frame, so a GIF of 10 seconds of Red is about 150 KB. GIFs are twice the screen's size (320×288) and PNGs three times (480×432).
- `boytacean` prints `[WARNING] Writing to unknown Cartridge ROM location 0x6000` with `println!` (not the log) every time Pokémon Red touches its cartridge's clock latch, which Red's cartridge doesn't have. It's harmless but floods the journal, so once the Game Boy starts, `core::logging::drop_prints` points stdout at `/dev/null`. Log lines don't go through stdout itself: they use a copy of it taken at start.

## Reading the game (stage 2)

Code reads everything it can from the Game Boy itself, so a model is only asked what needs judgement. Nothing here calls a model.

- **The screen from video memory** (`screen.rs`). Each of the 20×18 cells comes from the background or the window layer, the way the Game Boy picks (LCDC bits, signed tile numbers, scrolling that wraps at the edge), plus the sprites. Each tile also gets a fingerprint of its pixels (FNV-1a, so it never changes between builds). A test redraws the screen from the cells and matches the real one pixel for pixel. The emulator copies video memory as V-Blank starts (LY 144), when the frame is finished.
- **Fades and area changes** (`change.rs`). A frame is a picture, off, or blank (every pixel one colour, or a palette that maps every colour to one shade). A dark spell of at least 4 frames is an area change; Red's doors are dark for about 18.
- **Text** (`text.rs`). Font tiles are labelled by fingerprint, so text reads exactly once each letter is known. A tile given two different labels reads as neither. In Red the tile numbers are the character codes, so `Font::learn_from_numbers` labels them; a test checks the font read from video memory gives the same words as Red's memory. Other games will need a model to label each new letter once (stage 7).
- **Steps** (`moves.rs`). A step holds the direction until the screen starts to scroll (at most 24 frames), lets go and waits for the screen to be still. A 16-pixel scroll is a step, 32 a ledge jump, no scroll with a changed player sprite a turn, nothing a block, a dark spell somewhere new. Walking tries again after a turn, since the first press only turns her.
- **Pokémon Red's memory map** (`red/`). Addresses from the pret/pokered disassembly: map and position, facing, battle, names, badges, money (decimal digits), party (species, level, HP, status), bag, doors and signs, the tileset's list of tiles that can be walked on, tall grass, 33 story flags in the order the game reaches them ("chose a starter", "beat Brock", ...), and in a battle both Pokémon (species, level, HP, status) and her moves with the PP left, once both are out (until then the battle copies hold the last battle's). Names of species, items and maps are tables made from the disassembly. `State::facts` writes them as prompt lines. `Trust` checks memory against the screen: each step, the scroll and the position in memory must agree; after 3 agreements memory is trusted, and after one disagreement its facts are left out for good (a Japanese cartridge or a hack could keep things elsewhere).
- **Looking at Red** (`red/look.rs`). Red keeps its own copy of the screen's tiles and checks one tile per step against the walkable list, so code marks every step on screen as floor, wall, tall grass, door (from the doors in memory), sign, or edge (past the map's edge where the next area joins) exactly the way the game decides. People come from the sprites. A text box or menu is open when its corner tiles are on screen. She can only walk on the map itself: every tile a map tile (so not the Pokédex page or the swirl into a battle), her sprite in the middle, standing on a walkable tile, and no scene running (Red's `wJoyIgnore` and its scripted-movement flags), so nothing else is ever marked on the map.
- **Area maps** (`map.rs`). A map per area that fills in as she goes: what each step is, steps she walked onto (always floor, whatever they look like), steps that blocked her from one side, doors and where they led, her own markers, and the people on the last screen (not saved, they move). `path` finds the shortest way (breadth-first), through unseen steps only when nothing known reaches, and never through unseen steps past the area's edges when the game gives its size (Red does), since that wanders around the outside of the map; doors and edges can only be the last step, since stepping on one leaves the area. `frontier` is the unseen steps next to what she can reach, nearest first. `draw` writes it as text with x along the top and y down the side, with a legend.
- **Walking** (`walk.rs`). `walk_to` finds the way, takes one step, looks again, and repeats. A step that didn't move her marks that way as blocked (unless someone was standing there, or something took over the screen) and the way is found again. It stops when she arrives, walks into another area (waiting for the fade to finish, since Red changes its map number about 20 frames before fading), something takes over the screen (it waits up to 2 seconds for the words), there's no way, or 6 steps failed. `explore` walks toward the nearest unseen step until it has been seen. Every step goes in the movement log (`world.rs`), as lines like `Pallet Town (5, 6) up: moved to (5, 5)`.

## Short tasks (stage 3)

The supervisor carries out one short task at a time, like "take the Potion out of the PC in your room, until it's in your bag". It's the `game_play` job: Claude Haiku at low effort with no web or code, unless `[ai.claude.jobs.game_play]` says otherwise. Its prompt names no game and stays the same for every task, so it's cached; the game and the task come in the first message, with where she is and a picture of the screen.

- **Actions** (`actions.rs`). The model picks one tool at a time and code does the work: `walk_to x y` (walking as in stage 2), `explore`, `press` (1 to 5 buttons, like `up a`), `read_text` (presses A until the text is gone or a choice comes up, at most 30 times), `write_skill` and `use_skill` (stage 4, below), and `finish done summary`. After each one the screen settles first: she only counts as free to walk if she still is a third of a second later (the rival walks up a moment after his words close), and typed-out text is waited for until it stops changing. `walk_to` a step outside the area (when its size is known) is refused with the area's size and how to leave by an edge, unless it's an edge step with no wall inside it: those are the ways out. Exploring never aims past the edges, and never tries the same unreachable step twice, so it always ends. Finishing as done while text is still up gets one "read it first" back, since what the game does after the text can be what finishes the task (the rival battle is only saved after its last words). The answer is one line of what happened, then where she is (with the area's size): the facts from memory, whether she can walk, the words on screen and the map 7 steps around her, plus the screen at twice its size (about 120 tokens). Words only count inside a box, so the swirl into a battle doesn't read as letters.
- **The loop** (`supervisor.rs`). `run_task` asks for an action, runs it, sends back what happened, and repeats until `finish` or the action limit. Answering in words gets two reminders to use a tool; three actions in a row that change nothing get a warning to check which way she's facing and try something else. The `Episode` keeps every action and its outcome and all the words seen, and `digest` writes that for the strategist. Code, not the model's `finish`, decides whether a task was really done.
- **Where it plays** (`player.rs`). The supervisor plays through the `Player` trait: in the bot that's the one Game Boy on its thread; the gate has a Game Boy of its own.
- **The strategist** (`strategist.rs`). Once per episode a stronger OpenAI model reads the goal, its notes, the last episode's digest and where she is, and answers with up to 15 notes and the next short task, as JSON that fits a strict schema (`OpenAi::structured`, nothing stored). It's asked in one call, or in two (notes first, then the task with the new notes). The gate measures which works; the bot uses it from stage 4.

## The gate

Before anything is built on the supervisor, stage 3 measures it on 15 tasks from Red's opening, each run 3 times. The gate passes when at least half the runs are done. If Haiku stays under that, the plan's fallbacks are tried in cost order: Haiku with hints from the strategist, a model from OpenAI's free group playing, Gemini 3.8 Flash playing, Sonnet playing.

The tasks start from save states that `build_the_task_states` makes by playing a new game by script (walking with code, mashing through text and battles), from her bedroom to Viridian City with Oak's Parcel: `bedroom`, `downstairs`, `pallet`, `lab`, `starter`, `after_rival`, `pallet_starter`, `route1`, `viridian` and `parcel`. Each task is checked in code from the game's memory and the words seen: the area she ends in, the bag, the party, a story flag, or words like "Shades of" from the town sign. They are going downstairs, the Potion from the PC, talking to Mom, going outside, reading the town sign, heading north until Oak stops her, choosing a starter, the rival battle, leaving the lab, onto Route 1, up Route 1 to Viridian City (with wild Pokémon in the grass), into the Mart for the parcel, healing at the Pokémon Center, saving the game, and leaving the Mart. `actions_carry_out_tasks_by_script` checks the actions and the checks with no model, on five of them.

The runs are `#[ignore]`d tests that only call models with `VIVY_GB_GATE=1`. They need your ROM, the keys (`VIVY_ANTHROPIC_API_KEY` or `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`) and the states (in `VIVY_GB_TASKS`, default the project's shared folder):

```bash
export VIVY_GB_ROM=roms/red.gb VIVY_GB_GATE=1
cargo test --release build_the_task_states -- --ignored     # the states, once
cargo test --release gate_keys_work -- --ignored --nocapture # one small call to each
cargo test --release gate_tasks -- --ignored --nocapture     # the supervisor on the tasks
cargo test --release gate_strategists -- --ignored --nocapture
```

`gate_tasks` takes `VIVY_GB_PLAYER` (the model playing, default `claude-haiku-5-5`; `gemini-...` and `gpt-...` models work too), `VIVY_GB_EFFORT` (`low`), `VIVY_GB_RUNS` (3), `VIVY_GB_ONLY` (only tasks whose names contain it) and `VIVY_GB_HINTS` (an OpenAI model that writes a hint before each task). `gate_strategists` plays the long goal, bringing Oak's Parcel back to Oak, from `parcel` with each model in `VIVY_GB_STRATEGISTS` (`gpt-6-astra,gpt-6.1-sol`) asked in one call and in two: at most 15 episodes of 30 actions. Each run stops at `VIVY_GB_GATE_USD` dollars (15) at list prices, and writes `report.md`, `log.md` (every episode's actions and words) and each run's last screen to a new folder in `VIVY_GB_RESULTS` (default `/mnt/project-files/gameboy/gate`). A cloud session's proxy re-signs TLS, so the gate also trusts the certificate in `SSL_CERT_FILE`.

**Results (20 October 2026).** Haiku 5.5 at low effort passed: 43 of 45 runs done (96%), never claiming a task done when it wasn't, for $0.10 in all (about $0.002 a task). Every task went 3 for 3 except up Route 1 (1 of 3): both misses used up their 50 actions in wild battles, since she reads each battle turn as separate actions. On the long goal, Astra and 6.1 Sol both got the parcel to Oak in all four runs (5 or 6 episodes, 64 to 77 actions), planning the same five legs; two calls doubled the strategist's tokens for no gain, so the strategist is asked in one. None of the fallbacks were needed. The reports are in the project's shared folder under `gameboy/gate/`.

## Her own skills (stage 4)

The gate's one weak spot was battles: she plays each turn as separate actions, so wild battles on Route 1 ate her action budget. Coding a battle action would fix that for Red only, and it would be code playing for her. Instead she writes skills herself (`skills.rs`), and code only follows them.

- **What a skill is.** A name, a line saying when it's for, and know-how, a routine, or both. Know-how is notes she reads when she uses the skill (how a puzzle works, which moves are strong). A routine is up to 8 rules, each "when these words are on screen, press these 1 to 5 buttons" (whole words, any case: `RED` isn't in "appeared"; only marks, like the cursor `▶`, are looked for as they are) (the first rule that fits wins; one without words fits any screen), and when to stop: when she can walk (unless she says not to), when any of her stop words is on screen, after a number of rounds (30 unless she says, at most 50), when no rule fits, or when 3 rounds in a row change nothing. Stop words only count after its first press, so words already up when it starts (`RUN` on the battle menu it's meant for) don't stop it at once, and `write_skill` warns when a stop word is also a rule's words.
- **Nothing in code knows what a skill is for.** Rules and stops only use what every game has, the words on screen and whether she can walk, so a battle routine in Red is hers, and in another game she writes others. `skills_she_writes_carry_out_battles` runs three a model might write on the real game: pressing A through the rival battle, running from a wild Pokémon (`down right a` on the battle menu) and fighting one.
- **Writing and using them.** `write_skill` saves one (the same name replaces it, and its record starts over); `use_skill` shows its notes and runs its routine as one action, answering with the presses, the rounds, why it stopped and the words it went through. When no rule fits, it says what the screen says; when it pressed nothing, it says to rewrite it under the same name, so she fixes a skill instead of writing near-copies beside it. Each skill keeps how many times it was used and how the last use ended. Her list of skills (name, when it's for, how the last use went) comes with every look, and the prompt asks her to write one when she catches herself repeating presses, and to rewrite one that didn't do what she meant. At most 20 per game.
- **Astra reviews them.** The supervisor (Haiku) is good at noticing a repeated job and drafting a skill on the spot, and bad at editing them. So the strategist's one call per episode (same model, Cherry's call) also gets her skills in full, with how each last use went, and answers with changes (`skill_changes` in its schema: rewrite or delete, with `write_skill`'s fields). It fixes a skill whose last use went wrong, merges near-copies, deletes ones that never helped and adds one for a repeated job no skill covers; an empty list keeps them all. `skills::review` applies the changes, checked like any skill she writes.
- **Where they're kept.** `skills.json` next to the game's save states, read when the ROM is loaded and written after each `task` that changed them (even one that failed partway). A file that doesn't read is logged and counts as none.

`gate_learning` measures whether she gets better by writing them: the rival battle, then up Route 1, 5 times in a row (`VIVY_GB_RUNS`), each run starting from the same state but with the skills written in the runs before. Its report has each run's actions and the skills written and used, and her skills at the end (also as `skills.json`). `VIVY_GB_LEARN` names other tasks, and `VIVY_GB_REVIEWER` (an OpenAI model, like `gpt-6-astra`) has it review her skills after each task, to compare runs with and without the review.

**First run (20 October 2026, before the fixes above).** The rival battle was done 4 of 5 times, up Route 1 2 of 5 ($0.06 in all). She wrote skills in four runs without being told how, and a wild battle went from about 10 single presses to 2 skill uses. Route 1's misses came from walking, not fighting: `walk_to` targets past the top edge wandered around the outside of the map. She also finished once with the battle's last words still up, wasted skills whose stop word (`RUN`) was on the very menu they were for, never learned why a skill matched nothing, and wrote 5 near-copies rather than fixing one. The fixes since are the refused targets and bounded paths, the "read it first" guard, stop words after the first press with a warning, the screen's words in "no rule fits", and a nudge to rewrite under the same name. (A press like `aa` now says to put spaces between them.) The same day's baseline with no skills did up Route 1 3 of 3 in 45 actions each.

**With the fixes, and with the review (20 October 2026).** Without the review she did all 10 tasks (up from 6), ending with 2 battle skills and no near-copies. With Astra reviewing after each task she did 9 of 10; the miss was Claude overloaded before her first action, which the gate now plays again once. Astra changed her skills twice in 10 reviews, merging them into one battle skill that won the rival battle in one action, and it dropped a stop word, `RED`, that had stopped every wild battle after one press. That was a matching bug, `RED` inside "appeared", now fixed by matching whole words. Haiku cost about $0.03 a gate run; each review about $0.03 at list prices, on the free daily tokens. Up Route 1's action counts mostly show whether the same path from the same state meets a wild Pokémon, so they say little about skills; longer play will. That run also found exploring could loop forever at the top of a route, now fixed.

## Settings

Under `[features.games.gameboy]`, each explained in `config.example.toml`: `roms_dir` (`"roms"`), `saves_dir` (`"gameboy"`). The gate keys are `[features.games]`'s, shared with the other games.

## Files

All under `src/features/games/gameboy/`.

| File | What it holds |
|---|---|
| `mod.rs` | The settings, and the list of files |
| `emulator.rs` | `Emulator`: the Game Boy, a frame at a time; buttons, the screen, memory, save states; `RomInfo` from the cartridge header |
| `console.rs` | `Machine` (the emulator, its ROM's name and its recent screens) and `Console`, the thread it runs on |
| `presses.rs` | Reading `a a up*3 start` into presses, and writing them back (pure) |
| `pictures.rs` | Screens as a PNG or a GIF (pure) |
| `files.rs` | The ROMs in `roms_dir`, save states in `saves_dir`, and which names are allowed |
| `screen.rs` | The 20×18 cells and the sprites, read from video memory; tile fingerprints (pure) |
| `change.rs` | Telling pictures from dark frames, and when a dark spell means a new area (pure) |
| `text.rs` | The font: labelled tiles, reading the screen's words (pure) |
| `moves.rs` | One step and what happened: moved, turned, blocked, somewhere new |
| `map.rs` | `AreaMap`: what each step is, blocked steps, doors, markers; the shortest way, the frontier, the map as text (pure) |
| `world.rs` | `World`: the area maps, the movement log and the memory check; `View`, one look at the game (pure) |
| `walk.rs` | Walking to a step and exploring, a step at a time |
| `red/mod.rs` | Pokémon Red's memory map: `State`, its facts as prompt lines, the character codes, `Trust` |
| `red/look.rs` | Looking at Red: marking the screen's steps on the area map from the game's own walkable list |
| `red/names.rs` | Species, item and map names, made from the disassembly |
| `red/story.rs` | The story flags worth knowing |
| `actions.rs` | What the supervisor can do, as tools: walk, explore, press, read text, write and use skills; settling the screen after each; where she is, as text for the model |
| `skills.rs` | Her skills: checking one she writes, which rule fits a screen, when it stops, her list as prompt lines (pure) |
| `supervisor.rs` | `run_task`: the supervisor carrying out one task; the `Player` trait; `Episode` and its digest |
| `player.rs` | The supervisor's player in the bot: the Game Boy on its thread |
| `strategist.rs` | The strategist's call: the next task, notes and changes to her skills, as JSON in a strict schema |
| `commands.rs` | `/games gameboy ...` |
| `gate/mod.rs` | The gate (tests only): the key check, the task runs, the strategist runs, the learning runs, their reports |
| `gate/tasks.rs` | The task states, made by playing Red by script; the 15 tasks and their checks; the long goal |
| `gate/player.rs` | The gate's own Game Boy for the supervisor |
| `test_rom.rs` | A tiny Game Boy program, built byte by byte, that the tests run instead of a game |

## Tests

`cargo test gameboy` runs everything on `test_rom.rs`'s little program, so no ROM is needed: frames, buttons reaching the game, the screen, save states playing back the same (also on a second Game Boy), a Game Boy Color game, GIFs and PNGs, typed presses, folders and names, and a panic on the emulator's thread.

Nine more need a real game and are `#[ignore]`d. With your own ROM:

```bash
VIVY_GB_ROM=roms/red.gb cargo test --release gameboy -- --ignored --nocapture
```

That includes `build_the_task_states` and `actions_carry_out_tasks_by_script` from stage 3 (see "The gate") and `skills_she_writes_carry_out_battles` from stage 4; the gate's model runs skip themselves without `VIVY_GB_GATE=1`. `supervisor.rs` tests the task loop with a scripted model and a pretend game: reminders, the loop warning, the action limit.

The stage 2 ones play Red's opening first (mashing A, picking the names RED and BLUE) and start in her bedroom: `walks_out_of_the_house_and_into_oak` walks to the stairs through steps not seen yet, out of the front door and north until Oak stops her, checking memory and the screen agree on every step and that the font reads Oak's words; `exploring_fills_in_the_bedroom` explores until every step of the room is seen; `look_and_walk_read_well_on_a_real_game` prints what `look` and `walk` would post.

`a_real_game_runs_fast_and_plays_back_the_same` plays 10 minutes of the game and prints how fast that went, then checks a saved state plays 600 frames of mixed inputs back exactly, twice and on a second Game Boy. `the_commands_gifs_on_a_real_game` makes the GIFs `load` and `press` would post and saves them in `VIVY_GB_OUT` (default: the temp folder) to look at.

## The stages

| Stage | What it adds |
|---|---|
| 1 | The emulator and manual control: load, press, screen, save and load states, GIFs |
| 2 | Reading the game in code: screen changes and fades, the tile grid from video memory, text from the font, a log of every step, a map of each area that fills in, walking to a spot by pathfinding, exploring, and Pokémon Red's memory map (area, position, story flags, badges, party) checked against the screen. Red's own walkable-tile list sorts its tiles exactly, so sorting tiles with a model (for games with no memory map) moved to stage 7 |
| 3 | Short tasks played by a cheap model (Haiku) through a few actions, measured on 15 tasks from Red, each 3 times, and the strategist's call measured on a longer goal (Astra and 6.1 Sol, one call and two). A gate: if Haiku stays under half, the fallbacks in the plan are tried first. `/games gameboy task` tries a task by hand |
| 4 (under way) | Skills she writes herself, know-how and routines code runs as one action, kept per game, instead of a coded battle action (the gate's one weak spot), with a gate that measures whether later runs get better; then the strategist (GPT-6 Astra on OpenAI's free daily tokens, Haiku once they're used), goals and notes in her memory folder, walkthrough milestones, checkpoints, getting unstuck, the session runner with its caps |
| 5 | Discord: `/games gameboy play` and `status`, a thread per game, Opus narrating as a voice message in her voice with one GIF and a short line (so posts stay compact), hints from replies starting `hint:` |
| 6 | Learning a new game by herself: fast exploration, an insights file she confirms in play, replaying what worked, and reading a game's memory without a hand-written map: code compares memory before and after her actions ("what changed when I stepped right", "which byte is the 19 HP on screen"), she names what she finds, and code keeps checking it, measured on whether she finds Red's own addresses |
| 7 | Other games: tiles sorted by a model once per new tile, letters labelled the same way, memory maps for games that have one, side-view games, then the Game Boy Advance |
