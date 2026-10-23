//! The Game Boy itself: a thin wrapper over the `boytacean` crate (Game Boy and Game Boy
//! Color), run headless. Nothing here knows about Discord or threads; `console.rs` runs it
//! on its own thread.
//!
//! The emulator only moves when it's told to. `frame` runs exactly one frame (1/60 of a
//! second of game time), and the game is paused between calls, so a slow model never makes
//! Vivy miss anything.
//!
//! Two timing rules from the emulator test of 19 October:
//!
//! - **A frame ends at the end of V-Blank**, the point where `boytacean` starts the next
//!   frame. Saving a state anywhere else (say, right as V-Blank starts, when the game's
//!   V-Blank interrupt is pending) drops that interrupt, and a game loaded from the state
//!   drifts from the one that saved it. Since `frame` always stops there, a save is always
//!   safe.
//! - **The picture is complete when V-Blank starts** (the screen's line counter `LY` hits
//!   144). That's also when video memory still holds what the picture was drawn from, before
//!   the game's V-Blank code changes it for the next frame. `frame` passes through that
//!   moment, and stage 2 reads video memory there.

use anyhow::{Context as _, Result, anyhow, bail};
use boytacean::gb::{GameBoy, GameBoyMode};
use boytacean::pad::PadKey;
use boytacean::rom::Cartridge;
use boytacean::state::{FromGbOptions, SaveStateFormat, StateManager, ToGbOptions};

/// The screen's size in pixels.
pub const WIDTH: usize = 160;
pub const HEIGHT: usize = 144;

/// Clock cycles in one frame: 154 lines of 456 cycles.
const FRAME_CYCLES: u32 = 70_224;

/// The Game Boy's frames per second, a little under 60.
pub const FPS: f64 = 59.7275;

/// One of the eight buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    A,
    B,
    Start,
    Select,
    Up,
    Down,
    Left,
    Right,
}

impl Button {
    /// Whether it's on the d-pad.
    pub fn is_direction(self) -> bool {
        matches!(
            self,
            Button::Up | Button::Down | Button::Left | Button::Right
        )
    }

    pub const ALL: [Button; 8] = [
        Button::A,
        Button::B,
        Button::Start,
        Button::Select,
        Button::Up,
        Button::Down,
        Button::Left,
        Button::Right,
    ];

    /// The name people type, like `start` or `up`.
    pub fn name(self) -> &'static str {
        match self {
            Button::A => "a",
            Button::B => "b",
            Button::Start => "start",
            Button::Select => "select",
            Button::Up => "up",
            Button::Down => "down",
            Button::Left => "left",
            Button::Right => "right",
        }
    }

    /// A button from its name, ignoring case. `u`, `d`, `l` and `r` are short for the
    /// directions.
    pub fn parse(name: &str) -> Option<Button> {
        Some(match name.to_ascii_lowercase().as_str() {
            "a" => Button::A,
            "b" => Button::B,
            "start" => Button::Start,
            "select" => Button::Select,
            "up" | "u" => Button::Up,
            "down" | "d" => Button::Down,
            "left" | "l" => Button::Left,
            "right" | "r" => Button::Right,
            _ => return None,
        })
    }

    fn key(self) -> PadKey {
        match self {
            Button::A => PadKey::A,
            Button::B => PadKey::B,
            Button::Start => PadKey::Start,
            Button::Select => PadKey::Select,
            Button::Up => PadKey::Up,
            Button::Down => PadKey::Down,
            Button::Left => PadKey::Left,
            Button::Right => PadKey::Right,
        }
    }
}

/// What the cartridge header says about a ROM, read without starting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomInfo {
    /// The title in the header, like `POKEMON RED`.
    pub title: String,
    /// Whether it's made for the Game Boy Color (in colour, or colour only).
    pub color: bool,
}

impl RomInfo {
    /// Reads the header, or says why the file isn't a ROM.
    pub fn read(rom: &[u8]) -> Result<RomInfo> {
        // The header ends at 0x150; anything shorter can't be a ROM. `boytacean` would
        // panic on it rather than fail.
        if rom.len() < 0x8000 {
            bail!("too small for a Game Boy ROM ({} bytes)", rom.len());
        }
        let cartridge = Cartridge::from_data(rom).map_err(|e| anyhow!("not a ROM: {e}"))?;
        Ok(RomInfo {
            title: cartridge.title(),
            color: cartridge.gb_mode() == GameBoyMode::Cgb,
        })
    }
}

/// The registers that say how the picture is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Registers {
    /// LCDC: screen on, which maps and tiles, window and sprites on.
    pub lcdc: u8,
    /// Where the background is scrolled to.
    pub scx: u8,
    pub scy: u8,
    /// Where the window (a second layer, for menus and text boxes) starts on the screen.
    pub wx: u8,
    pub wy: u8,
    /// The original Game Boy's palettes: which shade each colour number shows.
    pub bgp: u8,
    pub obp0: u8,
    pub obp1: u8,
}

/// What a picture was drawn from, read as V-Blank starts, before the game changes any of it
/// for the next frame. Stage 2's screen reading (`screen.rs`) works from this, not from the
/// pixels.
#[derive(Debug, Clone)]
pub struct Video {
    /// Video memory: tiles and the two tile maps. 8 KB, or 16 KB on a Game Boy Color, where
    /// the second bank holds more tiles and each map cell's attributes.
    pub vram: Vec<u8>,
    /// The 40 sprites, 4 bytes each.
    pub oam: Vec<u8>,
    pub registers: Registers,
    /// Whether the screen was drawn this frame (it's off between areas in many games).
    pub drawn: bool,
    pub color: bool,
}

pub struct Emulator {
    gb: GameBoy,
    info: RomInfo,
    /// The last frame's video memory, read as its V-Blank started.
    video: Video,
    /// Frames run since the ROM was loaded (or since the state was loaded).
    frames: u64,
    /// The buttons held down right now.
    held: Vec<Button>,
    /// How many times a direction has been pressed: a look can tell the screen scrolled
    /// because she walked, not because the game moved her.
    directions_pressed: u64,
    /// The direction pressed last, which way she last went.
    last_direction: Option<Button>,
}

impl Emulator {
    /// Powers on a Game Boy with this ROM in it. Color games get a Game Boy Color, the rest
    /// an original Game Boy. Both start from the emulator's own open-source boot ROM, so
    /// the game finds the machine just as a real boot would leave it.
    pub fn new(rom: &[u8]) -> Result<Emulator> {
        let info = RomInfo::read(rom)?;
        let mode = if info.color {
            GameBoyMode::Cgb
        } else {
            GameBoyMode::Dmg
        };
        let mut gb = GameBoy::new(Some(mode));
        gb.load(true)
            .map_err(|e| anyhow!("starting the emulator: {e}"))?;
        gb.load_rom(rom, None)
            .map_err(|e| anyhow!("loading the ROM: {e}"))?;
        // No sound: nobody hears it, and it's about a quarter of the work.
        gb.set_apu_enabled(false);
        let video = Video {
            vram: Vec::new(),
            oam: Vec::new(),
            registers: Registers::default(),
            drawn: false,
            color: info.color,
        };
        Ok(Emulator {
            gb,
            info,
            video,
            frames: 0,
            held: Vec::new(),
            directions_pressed: 0,
            last_direction: None,
        })
    }

    pub fn info(&self) -> &RomInfo {
        &self.info
    }

    /// Frames run since the ROM or the last state was loaded.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Runs one frame: to the start of V-Blank, where the picture is complete, then on to
    /// its end, where the next frame starts. When the game has turned the screen off
    /// (between areas, say), there's no V-Blank, so it runs one frame's worth of time.
    pub fn frame(&mut self) {
        let start = self.gb.ppu_frame();
        let mut cycles: u32 = 0;
        let mut drawn = false;
        loop {
            cycles += u32::from(self.gb.clock());
            if !drawn && self.gb.ppu_ly() == 144 {
                self.read_video();
                drawn = true;
            }
            if self.gb.ppu_frame() != start {
                break;
            }
            // With the screen off the frame counter stands still. Twice a frame's time is
            // a backstop in case the screen goes off and on in the middle.
            if cycles >= FRAME_CYCLES && (!self.screen_on() || cycles >= 2 * FRAME_CYCLES) {
                break;
            }
        }
        self.video.drawn = drawn;
        self.frames += 1;
    }

    fn read_video(&mut self) {
        let registers = Registers {
            lcdc: self.gb.read_memory(0xff40),
            scy: self.gb.read_memory(0xff42),
            scx: self.gb.read_memory(0xff43),
            bgp: self.gb.read_memory(0xff47),
            obp0: self.gb.read_memory(0xff48),
            obp1: self.gb.read_memory(0xff49),
            wy: self.gb.read_memory(0xff4a),
            wx: self.gb.read_memory(0xff4b),
        };
        let ppu = self.gb.ppu_i();
        let vram = if self.info.color {
            ppu.vram_cgb()
        } else {
            ppu.vram_dmg()
        };
        // Copied into the buffers already there, so a frame allocates nothing.
        self.video.vram.clear();
        self.video.vram.extend_from_slice(vram);
        self.video.oam.clear();
        self.video.oam.extend_from_slice(&ppu.oam()[..160]);
        self.video.registers = registers;
    }

    /// What the last frame was drawn from (see [`Video`]).
    pub fn video(&self) -> &Video {
        &self.video
    }

    /// One byte of the Game Boy's memory, as the game would read it.
    pub fn read(&mut self, address: u16) -> u8 {
        self.gb.read_memory(address)
    }

    /// Whether the game has the screen switched on (LCDC bit 7).
    pub fn screen_on(&mut self) -> bool {
        self.gb.read_memory(0xff40) & 0x80 != 0
    }

    /// Holds these buttons down (and lets go of any others) until the next call.
    pub fn hold(&mut self, buttons: &[Button]) {
        for &button in &Button::ALL {
            let was = self.held.contains(&button);
            let now = buttons.contains(&button);
            if now && !was {
                self.gb.key_press(button.key());
                if button.is_direction() {
                    self.directions_pressed += 1;
                    self.last_direction = Some(button);
                }
            } else if was && !now {
                self.gb.key_lift(button.key());
            }
        }
        self.held = buttons.to_vec();
    }

    /// Lets go of every button.
    /// How many times a direction has been pressed since the Game Boy started.
    pub fn directions_pressed(&self) -> u64 {
        self.directions_pressed
    }

    pub fn release(&mut self) {
        self.hold(&[]);
    }

    /// The direction pressed last, if any.
    pub fn last_direction(&self) -> Option<Button> {
        self.last_direction
    }

    /// The screen as it was last drawn: 160×144 pixels, 3 bytes (red, green, blue) each.
    pub fn screen(&mut self) -> Vec<u8> {
        self.gb.frame_buffer().to_vec()
    }

    /// The whole machine as bytes (about 50 KB), to come back to with `load_state`. It
    /// doesn't hold the ROM, so it only loads into the same game. Held buttons aren't
    /// part of it.
    pub fn save_state(&mut self) -> Result<Vec<u8>> {
        StateManager::save(
            &mut self.gb,
            Some(SaveStateFormat::Bos),
            Some(FromGbOptions::new(false, None, None, None)),
        )
        .map_err(|e| anyhow!("saving the state: {e}"))
    }

    /// Goes back to a state from `save_state`. Buttons are let go, since a state doesn't
    /// say which were held.
    pub fn load_state(&mut self, state: &[u8]) -> Result<()> {
        StateManager::load(
            state,
            &mut self.gb,
            None,
            Some(ToGbOptions::new(true, true)),
        )
        .map_err(|e| anyhow!("{e}"))
        .context("loading the state")?;
        self.held.clear();
        self.frames = 0;
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::features::games::gameboy::test_rom;

    // Only the tests use this; the bot runs frames through `Machine`, which records them.
    impl Emulator {
        /// Runs this many frames.
        pub fn run(&mut self, frames: u32) {
            for _ in 0..frames {
                self.frame();
            }
        }
    }

    /// Joypad byte the test ROM writes each frame (see `test_rom.rs`).
    const PAD: u16 = 0xc000;
    const COUNTER: u16 = 0xc001;

    fn booted() -> Emulator {
        let mut emu = Emulator::new(&test_rom::build()).unwrap();
        // The boot ROM takes a couple of seconds before the game starts.
        emu.run(300);
        emu
    }

    #[test]
    fn reads_the_header() {
        let info = RomInfo::read(&test_rom::build()).unwrap();
        assert_eq!(info.title, "VIVYTEST");
        assert!(!info.color);
        assert!(RomInfo::read(&[0; 100]).is_err());
    }

    #[test]
    fn runs_the_game_a_frame_at_a_time() {
        let mut emu = booted();
        let before = emu.read(COUNTER);
        emu.run(10);
        // The test ROM counts one per frame.
        assert_eq!(emu.read(COUNTER), before.wrapping_add(10));
        assert_eq!(emu.frames(), 310);
    }

    #[test]
    fn buttons_reach_the_game() {
        let mut emu = booted();
        assert_eq!(emu.read(PAD), 0);
        emu.hold(&[Button::A, Button::Right]);
        emu.run(2);
        // The standard joypad byte: A is bit 0, Right bit 4.
        assert_eq!(emu.read(PAD), 0b0001_0001);
        emu.hold(&[Button::Start]);
        emu.run(2);
        assert_eq!(emu.read(PAD), 0b0000_1000);
        emu.release();
        emu.run(2);
        assert_eq!(emu.read(PAD), 0);
    }

    #[test]
    fn the_screen_shows_what_the_game_drew() {
        let mut emu = booted();
        let screen = emu.screen();
        assert_eq!(screen.len(), WIDTH * HEIGHT * 3);
        // A black tile in the top left corner, white everywhere else.
        assert_eq!(&screen[0..3], &[0, 0, 0]);
        let right_of_tile = 8 * 3;
        assert_eq!(&screen[right_of_tile..right_of_tile + 3], &[255, 255, 255]);
        // Holding Right scrolls the background one pixel a frame, so the tile slides left.
        emu.hold(&[Button::Right]);
        emu.run(10);
        let screen = emu.screen();
        assert_eq!(&screen[0..3], &[255, 255, 255]);
    }

    #[test]
    fn a_saved_state_plays_back_the_same() {
        let mut emu = booted();
        let state = emu.save_state().unwrap();
        let inputs = [Button::Right, Button::A, Button::Left, Button::Down];
        let play = |emu: &mut Emulator| {
            let mut seen = Vec::new();
            for (i, &button) in inputs.iter().cycle().take(40).enumerate() {
                emu.hold(if i % 3 == 0 {
                    &[]
                } else {
                    std::slice::from_ref(&button)
                });
                emu.frame();
                seen.push((emu.screen(), emu.read(PAD), emu.read(COUNTER)));
            }
            emu.release();
            seen
        };
        let first = play(&mut emu);
        emu.load_state(&state).unwrap();
        let again = play(&mut emu);
        assert!(first == again, "the same inputs gave different frames");

        // A new Game Boy loading the state plays the same too.
        let mut other = Emulator::new(&test_rom::build()).unwrap();
        other.load_state(&state).unwrap();
        assert!(play(&mut other) == first);
    }

    #[test]
    fn a_color_game_gets_a_game_boy_color() {
        let mut emu = Emulator::new(&test_rom::build_color()).unwrap();
        assert!(emu.info().color);
        emu.run(300);
        let before = emu.read(COUNTER);
        emu.hold(&[Button::B]);
        emu.run(3);
        assert_eq!(emu.read(COUNTER), before.wrapping_add(3));
        assert_eq!(emu.read(PAD), 0b0000_0010);
        assert_eq!(emu.screen().len(), WIDTH * HEIGHT * 3);
    }

    /// The owner's own ROM, for the tests that need a real game: the path in `VIVY_GB_ROM`.
    /// Run them with `VIVY_GB_ROM=roms/red.gb cargo test --release gameboy -- --ignored`.
    pub fn real_rom() -> Vec<u8> {
        let path = std::env::var("VIVY_GB_ROM").expect("set VIVY_GB_ROM to a ROM's path");
        std::fs::read(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"))
    }

    #[test]
    #[ignore = "needs a ROM in VIVY_GB_ROM"]
    fn a_real_game_runs_fast_and_plays_back_the_same() {
        let rom = real_rom();
        let mut emu = Emulator::new(&rom).unwrap();
        println!("{:?}", emu.info());
        let started = std::time::Instant::now();
        // Ten minutes of game time, pressing Start and A now and then to get past the
        // title screen and into the game.
        for second in 0..600u32 {
            match second % 4 {
                0 => emu.hold(&[Button::Start]),
                2 => emu.hold(&[Button::A]),
                _ => emu.release(),
            }
            emu.run(60);
        }
        emu.release();
        let took = started.elapsed().as_secs_f64();
        println!(
            "10 min of play took {took:.1} s, {:.0}x real time",
            600.0 / took
        );

        let state = emu.save_state().unwrap();
        println!("a state is {} bytes", state.len());
        let play = |emu: &mut Emulator| {
            let mut screens = Vec::new();
            for i in 0..600u32 {
                let button = Button::ALL[(i / 20) as usize % 8];
                emu.hold(if i % 20 < 10 {
                    std::slice::from_ref(&button)
                } else {
                    &[]
                });
                emu.frame();
                screens.push(emu.screen());
            }
            emu.release();
            screens
        };
        let first = play(&mut emu);
        emu.load_state(&state).unwrap();
        assert!(play(&mut emu) == first, "the state played back differently");
        let mut other = Emulator::new(&rom).unwrap();
        other.load_state(&state).unwrap();
        assert!(
            play(&mut other) == first,
            "another Game Boy played it differently"
        );
    }

    #[test]
    fn a_broken_state_is_an_error() {
        let mut emu = booted();
        assert!(emu.load_state(b"not a state").is_err());
    }
}
