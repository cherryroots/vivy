# Game Boy

Vivy will play Game Boy games by herself: work out the controls and what the game wants, play in short capped sessions, and post now and then in a Discord thread about how far she has got. This is being built in stages, one pull request each. **Stages 1 and 2 are done: the emulator, played by hand with admin commands, and code that reads Pokémon Red's screen and memory, keeps a map of each area and walks her anywhere on it. No AI yet.**

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

Save states go in `saves_dir` (default `gameboy/`), a folder per ROM named after its file: `gameboy/red/before Brock.state`. A state is the whole machine, about 50 KB, so it only loads into the same ROM. A save the game makes itself (the SAVE menu in Pokémon) lives in the cartridge's memory, which a state includes, so saving a state after it keeps it. Both folders are in `.gitignore`.

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
- **Pokémon Red's memory map** (`red/`). Addresses from the pret/pokered disassembly: map and position, facing, battle, names, badges, money (decimal digits), party (species, level, HP, status), bag, doors and signs, the tileset's list of tiles that can be walked on, tall grass, and 33 story flags in the order the game reaches them ("chose a starter", "beat Brock", ...). Names of species, items and maps are tables made from the disassembly. `State::facts` writes them as prompt lines. `Trust` checks memory against the screen: each step, the scroll and the position in memory must agree; after 3 agreements memory is trusted, and after one disagreement its facts are left out for good (a Japanese cartridge or a hack could keep things elsewhere).
- **Looking at Red** (`red/look.rs`). Red keeps its own copy of the screen's tiles and checks one tile per step against the walkable list, so code marks every step on screen as floor, wall, tall grass, door (from the doors in memory), sign, or edge (past the map's edge where the next area joins) exactly the way the game decides. People come from the sprites. A text box or menu is open when its corner tiles are on screen.
- **Area maps** (`map.rs`). A map per area that fills in as she goes: what each step is, steps she walked onto (always floor, whatever they look like), steps that blocked her from one side, doors and where they led, her own markers, and the people on the last screen (not saved, they move). `path` finds the shortest way (breadth-first), through unseen steps only when nothing known reaches; doors and edges can only be the last step, since stepping on one leaves the area. `frontier` is the unseen steps next to what she can reach, nearest first. `draw` writes it as text with x along the top and y down the side, with a legend.
- **Walking** (`walk.rs`). `walk_to` finds the way, takes one step, looks again, and repeats. A step that didn't move her marks that way as blocked (unless someone was standing there) and the way is found again. It stops when she arrives, walks into another area (waiting for the fade to finish, since Red changes its map number about 20 frames before fading), something takes over the screen (it waits up to 2 seconds for the words), there's no way, or 6 steps failed. `explore` walks toward the nearest unseen step until it has been seen. Every step goes in the movement log (`world.rs`), as lines like `Pallet Town (5, 6) up: moved to (5, 5)`.

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
| `commands.rs` | `/games gameboy ...` |
| `test_rom.rs` | A tiny Game Boy program, built byte by byte, that the tests run instead of a game |

## Tests

`cargo test gameboy` runs everything on `test_rom.rs`'s little program, so no ROM is needed: frames, buttons reaching the game, the screen, save states playing back the same (also on a second Game Boy), a Game Boy Color game, GIFs and PNGs, typed presses, folders and names, and a panic on the emulator's thread.

Six more need a real game and are `#[ignore]`d. With your own ROM:

```bash
VIVY_GB_ROM=roms/red.gb cargo test --release gameboy -- --ignored --nocapture
```

The stage 2 ones play Red's opening first (mashing A, picking the names RED and BLUE) and start in her bedroom: `walks_out_of_the_house_and_into_oak` walks to the stairs through steps not seen yet, out of the front door and north until Oak stops her, checking memory and the screen agree on every step and that the font reads Oak's words; `exploring_fills_in_the_bedroom` explores until every step of the room is seen; `look_and_walk_read_well_on_a_real_game` prints what `look` and `walk` would post.

`a_real_game_runs_fast_and_plays_back_the_same` plays 10 minutes of the game and prints how fast that went, then checks a saved state plays 600 frames of mixed inputs back exactly, twice and on a second Game Boy. `the_commands_gifs_on_a_real_game` makes the GIFs `load` and `press` would post and saves them in `VIVY_GB_OUT` (default: the temp folder) to look at.

## The stages

| Stage | What it adds |
|---|---|
| 1 | The emulator and manual control: load, press, screen, save and load states, GIFs |
| 2 (this) | Reading the game in code: screen changes and fades, the tile grid from video memory, text from the font, a log of every step, a map of each area that fills in, walking to a spot by pathfinding, exploring, and Pokémon Red's memory map (area, position, story flags, badges, party) checked against the screen. Red's own walkable-tile list sorts its tiles exactly, so sorting tiles with a model (for games with no memory map) moved to stage 7 |
| 3 | Short tasks played by a cheap model (Haiku) through a few actions, measured on 10 to 20 tasks from Red, each 3 times. A gate: if Haiku stays under the target, the fallbacks in the plan are tried first |
| 4 | The strategist (GPT-6 Astra on OpenAI's free daily tokens, Haiku once they're used), goals and notes in her memory folder, walkthrough milestones, checkpoints, getting unstuck, the session runner with its caps |
| 5 | Discord: `/games gameboy play` and `status`, a thread per game, Opus narrating as a voice message in her voice with one GIF and a short line (so posts stay compact), hints from replies starting `hint:` |
| 6 | Learning a new game by herself: fast exploration, an insights file she confirms in play, replaying what worked |
| 7 | Other games: tiles sorted by a model once per new tile, letters labelled the same way, memory maps for games that have one, side-view games, then the Game Boy Advance |
