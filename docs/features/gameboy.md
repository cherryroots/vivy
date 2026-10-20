# Game Boy

Vivy will play Game Boy games by herself: work out the controls and what the game wants, play in short capped sessions, and post now and then in a Discord thread about how far she has got. This is being built in stages, one pull request each. **Stage 1 is done: the emulator, played by hand with admin commands. No AI yet.**

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
| `commands.rs` | `/games gameboy ...` |
| `test_rom.rs` | A tiny Game Boy program, built byte by byte, that the tests run instead of a game |

## Tests

`cargo test gameboy` runs everything on `test_rom.rs`'s little program, so no ROM is needed: frames, buttons reaching the game, the screen, save states playing back the same (also on a second Game Boy), a Game Boy Color game, GIFs and PNGs, typed presses, folders and names, and a panic on the emulator's thread.

Two more need a real game and are `#[ignore]`d. With your own ROM:

```bash
VIVY_GB_ROM=roms/red.gb cargo test --release gameboy -- --ignored --nocapture
```

`a_real_game_runs_fast_and_plays_back_the_same` plays 10 minutes of the game and prints how fast that went, then checks a saved state plays 600 frames of mixed inputs back exactly, twice and on a second Game Boy. `the_commands_gifs_on_a_real_game` makes the GIFs `load` and `press` would post and saves them in `VIVY_GB_OUT` (default: the temp folder) to look at.

## The stages

| Stage | What it adds |
|---|---|
| 1 (this) | The emulator and manual control: load, press, screen, save and load states, GIFs |
| 2 | Reading the screen in code: screen changes and fades, the tile grid from video memory, text from the font, tiles sorted into ground, walls, doors and people (the first model use, once per new tile), a log of every step, a map of each area that fills in, walking to a spot by pathfinding, and Pokémon Red's memory map (area, position, story flags, badges, party) checked against the screen |
| 3 | Short tasks played by a cheap model (Haiku) through a few actions, measured on 10 to 20 tasks from Red, each 3 times. A gate: if Haiku stays under the target, the fallbacks in the plan are tried first |
| 4 | The strategist (GPT-6 Astra on OpenAI's free daily tokens, Haiku once they're used), goals and notes in her memory folder, walkthrough milestones, checkpoints, getting unstuck, the session runner with its caps |
| 5 | Discord: `/games gameboy play` and `status`, a thread per game, Opus narrating in her voice with GIFs, hints from replies starting `hint:` |
| 6 | Learning a new game by herself: fast exploration, an insights file she confirms in play, replaying what worked |
| 7 | Memory maps for other games, side-view games, then the Game Boy Advance |
