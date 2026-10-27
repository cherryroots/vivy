//! The Game Boy on a thread of its own, so emulating never holds up the bot's async tasks.
//!
//! `Machine` is the emulator plus what Vivy needs around it: which ROM is in it, and the
//! last few seconds of screens for GIFs. `Console` owns the one machine on its thread and
//! runs jobs on it: `console().with(|machine| ...)` sends a closure there and waits for its
//! answer, the same way `ctx.db.call` works for the database.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use anyhow::{Context as _, Result, anyhow};

use super::emulator::{Button, Emulator, FPS};
use super::path::Frame;
use super::presses::Press;
use super::screen::sprite_places;
use super::world::World;

/// A screen is kept for GIFs every this many frames: 20 a second.
pub const RECORD_EVERY: u32 = 3;

/// How many seconds of screens are kept. A screen is 69 KB, so 10 s is about 14 MB.
pub const RECENT_SECONDS: u32 = 10;

const RECENT_FRAMES: usize = (RECENT_SECONDS as f64 * FPS) as usize;

const RECENT_SCREENS: usize = (RECENT_SECONDS as f64 * FPS / RECORD_EVERY as f64) as usize;

/// Whether the web page has someone watching (`web.rs`): only then is every frame kept for
/// it.
pub static WEB_WATCHING: AtomicBool = AtomicBool::new(false);

/// One frame for the web page, small to keep: its colours, and each pixel's colour by
/// number (a Game Boy shows 4 at a time, a Game Boy Color at most 56).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexed {
    pub colours: Vec<[u8; 3]>,
    pub pixels: Vec<u8>,
}

impl Indexed {
    /// A screen's pixels (3 bytes each) by colour number. Past 256 colours, which no Game
    /// Boy shows at once, the rest read as the first.
    pub fn of(screen: &[u8]) -> Indexed {
        let mut colours: Vec<[u8; 3]> = Vec::new();
        let pixels = screen
            .chunks_exact(3)
            .map(|p| {
                let colour = [p[0], p[1], p[2]];
                match colours.iter().position(|c| *c == colour) {
                    Some(i) => i as u8,
                    None if colours.len() < 256 => {
                        colours.push(colour);
                        (colours.len() - 1) as u8
                    }
                    None => 0,
                }
            })
            .collect();
        Indexed { colours, pixels }
    }

    /// Back to 3 bytes a pixel.
    pub fn rgb(&self) -> Vec<u8> {
        self.pixels
            .iter()
            .flat_map(|&i| {
                self.colours
                    .get(usize::from(i))
                    .copied()
                    .unwrap_or_default()
            })
            .collect()
    }
}

/// How long each press lasts, in frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Frames the buttons are held. 16 is one step in most games that walk on a grid.
    pub hold: u32,
    /// Frames with nothing held after letting go, so the game sees the next press as new.
    pub gap: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { hold: 16, gap: 16 }
    }
}

/// One frame's sprites, by the emulator's frame number, with the background's scroll.
type Recorded = (u64, Vec<(i32, i32)>, (u8, u8));

pub struct Machine {
    pub emulator: Emulator,
    /// The ROM's file name in `roms_dir`.
    pub rom: String,
    /// Recent screens, oldest first, one every `RECORD_EVERY` frames.
    recent: VecDeque<Vec<u8>>,
    /// Screens recorded since the ROM was put in, so a caller can ask for the ones after
    /// some point.
    recorded: u64,
    /// Where the sprites were each frame of the last `RECENT_SECONDS`, by the emulator's
    /// frame number, with the background's scroll, to follow one through frames no look saw
    /// (`moves::followed`, `path::follow`).
    sprite_frames: VecDeque<Recorded>,
    /// What code has learned about the game's world: area maps and the movement log.
    pub world: World,
    /// Every frame of the last `RECENT_SECONDS` by its number, while the web page has
    /// someone watching ([`WEB_WATCHING`]), for 60 frames a second there.
    web_frames: VecDeque<(u64, Indexed)>,
}

impl Machine {
    pub fn new(rom: &str, bytes: &[u8]) -> Result<Machine> {
        Ok(Machine {
            emulator: Emulator::new(bytes)?,
            rom: rom.to_string(),
            recent: VecDeque::with_capacity(RECENT_SCREENS),
            recorded: 0,
            sprite_frames: VecDeque::with_capacity(RECENT_FRAMES),
            world: World::default(),
            web_frames: VecDeque::new(),
        })
    }

    /// Runs one frame, keeping its screen if it's one to record.
    pub fn frame(&mut self) {
        self.emulator.frame();
        if self.sprite_frames.len() == RECENT_FRAMES {
            self.sprite_frames.pop_front();
        }
        let video = self.emulator.video();
        let scroll = (video.registers.scx, video.registers.scy);
        self.sprite_frames
            .push_back((self.emulator.frames(), sprite_places(video), scroll));
        if self
            .emulator
            .frames()
            .is_multiple_of(u64::from(RECORD_EVERY))
        {
            if self.recent.len() == RECENT_SCREENS {
                self.recent.pop_front();
            }
            self.recent.push_back(self.emulator.screen());
            self.recorded += 1;
        }
        if WEB_WATCHING.load(Ordering::Relaxed) {
            if self.web_frames.len() == RECENT_FRAMES {
                self.web_frames.pop_front();
            }
            let frame = Indexed::of(&self.emulator.screen());
            self.web_frames.push_back((self.emulator.frames(), frame));
        } else if !self.web_frames.is_empty() {
            self.web_frames.clear();
        }
    }

    /// The frames kept for the web page after frame `after` (`Emulator::frames`), or the
    /// newest alone when `after` is `None`.
    pub fn web_frames_after(&self, after: Option<u64>) -> Vec<(u64, Indexed)> {
        match after {
            Some(after) => self
                .web_frames
                .iter()
                .filter(|(n, _)| *n > after)
                .cloned()
                .collect(),
            None => self.web_frames.back().cloned().into_iter().collect(),
        }
    }

    pub fn run(&mut self, frames: u32) {
        for _ in 0..frames {
            self.frame();
        }
    }

    /// Presses each in turn: held for `timing.hold` frames (or the press's own length),
    /// then nothing for `timing.gap`. Buttons a press holds for less than all of it are let
    /// go on their frame.
    pub fn press(&mut self, presses: &[Press], timing: Timing) {
        for press in presses {
            let whole = press.frames.unwrap_or(timing.hold);
            if press.shorter.is_empty() {
                self.hold_for(&press.buttons, whole);
            } else {
                for frame in 0..whole {
                    let held: Vec<Button> = press
                        .buttons
                        .iter()
                        .copied()
                        .filter(|&b| press.frames_of(b, timing.hold) > frame)
                        .collect();
                    self.emulator.hold(&held);
                    self.frame();
                }
                self.emulator.release();
            }
            self.hold_for(&[], timing.gap);
        }
    }

    fn hold_for(&mut self, buttons: &[Button], frames: u32) {
        self.emulator.hold(buttons);
        self.run(frames);
        self.emulator.release();
    }

    /// How many screens have been recorded, to pass to `screens_since` later.
    pub fn recorded(&self) -> u64 {
        self.recorded
    }

    /// The screens recorded after `mark` (from `recorded`), as many as are still kept.
    pub fn screens_since(&self, mark: u64) -> Vec<Vec<u8>> {
        let wanted = (self.recorded.saturating_sub(mark) as usize).min(self.recent.len());
        self.recent
            .iter()
            .skip(self.recent.len() - wanted)
            .cloned()
            .collect()
    }

    /// Where the sprites were each frame from `frame` (`Emulator::frames`) on, that frame
    /// first, or `None` when it isn't kept any more.
    pub fn sprites_since(&self, frame: u64) -> Option<Vec<&[(i32, i32)]>> {
        Some(
            self.frames_since(frame)?
                .into_iter()
                .map(|(places, _)| places)
                .collect(),
        )
    }

    /// Like `sprites_since`, with where the background was scrolled to each frame.
    pub fn frames_since(&self, frame: u64) -> Option<Vec<Frame<'_>>> {
        let first = self.sprite_frames.iter().position(|(n, ..)| *n == frame)?;
        Some(
            self.sprite_frames
                .iter()
                .skip(first)
                .map(|(_, places, scroll)| (places.as_slice(), *scroll))
                .collect(),
        )
    }

    /// Forgets the recorded screens, after loading a state, so a GIF never jumps between
    /// two games.
    pub fn clear_recording(&mut self) {
        self.recent.clear();
        self.sprite_frames.clear();
    }

    /// Goes back to a saved state: the frames recorded since are of another game now, so
    /// they're forgotten (else following her through a press could read a try that was
    /// loaded away). Then one frame, so the screen shows the saved game.
    pub fn load_state(&mut self, state: &[u8]) -> Result<()> {
        self.emulator.load_state(state)?;
        self.clear_recording();
        self.run(1);
        Ok(())
    }
}

type Job = Box<dyn FnOnce(&mut Option<Machine>) + Send>;

pub struct Console {
    jobs: mpsc::Sender<Job>,
}

/// The one Game Boy, started the first time it's used.
pub fn console() -> &'static Console {
    static CONSOLE: LazyLock<Console> = LazyLock::new(Console::start);
    &CONSOLE
}

impl Console {
    fn start() -> Console {
        // The emulator prints warnings instead of logging them, a line every time Pokémon
        // Red touches a cartridge address it doesn't know. (Tests keep stdout: the test
        // runner reports on it.)
        #[cfg(not(test))]
        crate::core::logging::drop_prints();
        let (jobs, inbox) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("gameboy".to_string())
            .spawn(move || {
                let mut machine: Option<Machine> = None;
                for job in inbox {
                    // A panic inside the emulator ends that job, not the thread. The
                    // machine may be half-way through something, so it's taken out.
                    if catch_unwind(AssertUnwindSafe(|| job(&mut machine))).is_err() {
                        machine = None;
                    }
                }
            })
            .expect("starting the Game Boy thread");
        Console { jobs }
    }

    /// Runs `job` on the Game Boy's thread with the machine (`None` until a ROM is put in)
    /// and waits for its answer.
    pub async fn with<T, F>(&self, job: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Option<Machine>) -> T + Send + 'static,
    {
        let (answer, wait) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Box::new(move |machine| {
                let _ = answer.send(job(machine));
            }))
            .map_err(|_| anyhow!("the Game Boy thread has stopped"))?;
        wait.await
            .context("the Game Boy crashed while doing that, and was turned off")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::{presses, test_rom};

    fn machine() -> Machine {
        Machine::new("test.gb", &test_rom::build()).unwrap()
    }

    #[test]
    fn every_frame_is_kept_for_the_web_page_only_while_someone_watches() {
        let mut m = machine();
        m.run(3);
        assert_eq!(m.web_frames_after(None), vec![]);
        WEB_WATCHING.store(true, Ordering::Relaxed);
        m.run(5);
        let first = m.emulator.frames() - 4;
        let numbers =
            |kept: Vec<(u64, Indexed)>| kept.into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(
            numbers(m.web_frames_after(Some(first))),
            (first + 1..first + 5).collect::<Vec<_>>()
        );
        assert_eq!(numbers(m.web_frames_after(None)), vec![first + 4]);
        let (_, newest) = m.web_frames_after(None).pop().unwrap();
        assert_eq!(newest.rgb(), m.emulator.screen());
        assert!(newest.colours.len() <= 4);
        WEB_WATCHING.store(false, Ordering::Relaxed);
        m.run(1);
        assert_eq!(m.web_frames_after(None), vec![]);
    }

    #[test]
    fn records_a_screen_every_few_frames_and_keeps_the_last_seconds() {
        let mut m = machine();
        m.run(30);
        assert_eq!(m.recorded(), 10);
        let mark = m.recorded();
        m.run(9);
        assert_eq!(m.screens_since(mark).len(), 3);
        // Far more than is kept: only the newest stay.
        m.run(RECORD_EVERY * RECENT_SCREENS as u32 * 2);
        assert_eq!(m.screens_since(0).len(), RECENT_SCREENS);
        assert_eq!(RECENT_SCREENS, 199);
        m.clear_recording();
        assert!(m.screens_since(0).is_empty());
    }

    /// Practice loads its save after every try: what code follows her by must never come
    /// from a try that was loaded away, even when the next try runs as long.
    #[test]
    fn loading_a_state_forgets_the_frames_after_it() {
        let mut m = machine();
        m.run(10);
        let state = m.emulator.save_state().unwrap();
        let saved_at = m.emulator.frames();
        m.run(20);
        let tried = m.emulator.frames();
        m.load_state(&state).unwrap();
        // The count goes on, so no frame number is used twice.
        assert!(m.emulator.frames() > tried);
        assert!(m.sprites_since(saved_at).is_none());
        assert!(m.sprites_since(tried).is_none());
        m.run(20);
        assert!(m.sprites_since(tried).is_none());
        assert_eq!(m.sprites_since(tried + 1).map(|f| f.len()), Some(21));
    }

    #[test]
    fn a_press_holds_then_lets_go() {
        let mut m = machine();
        m.run(300);
        let start = m.emulator.frames();
        let presses = presses::parse("right a").unwrap();
        m.press(&presses, Timing { hold: 4, gap: 2 });
        assert_eq!(m.emulator.frames() - start, 12);
        // Nothing is held after the last press.
        m.run(2);
        assert_eq!(m.emulator.read(0xc000), 0);
        // Right was held for 4 frames, so the background scrolled 4 pixels (or 3, as the
        // ROM reads the pad once a frame).
        let scrolled = m.emulator.read(0xff43);
        assert!((3..=4).contains(&scrolled), "scrolled {scrolled}");
        // A let go early doesn't cut the right short.
        let start = m.emulator.frames();
        m.press(
            &presses::parse("a:2+right:6").unwrap(),
            Timing { hold: 4, gap: 2 },
        );
        assert_eq!(m.emulator.frames() - start, 8);
        let more = m.emulator.read(0xff43).wrapping_sub(scrolled);
        assert!((5..=6).contains(&more), "scrolled {more} more");
    }

    /// What the commands do, on a real game: the GIFs `load` and `press` would post, saved
    /// in `VIVY_GB_OUT` (default: the system's temp folder) to look at.
    #[test]
    #[ignore = "needs a ROM in VIVY_GB_ROM"]
    fn the_commands_gifs_on_a_real_game() {
        use crate::features::games::gameboy::{emulator, pictures};
        let out = std::env::var("VIVY_GB_OUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let mut m = Machine::new("red.gb", &emulator::tests::real_rom()).unwrap();
        m.run(4 * 60);
        let gif = pictures::gif(&m.screens_since(0), RECORD_EVERY, 2).unwrap();
        println!(
            "load: {} screens, GIF {} KB",
            m.screens_since(0).len(),
            gif.len() / 1024
        );
        std::fs::write(out.join("load.gif"), gif).unwrap();
        for (name, typed) in [
            ("title", "wait*8 start wait*4 a wait*4"),
            ("menu", "a wait*2 a*12"),
            ("long", &"a ".repeat(40)),
        ] {
            let mark = m.recorded();
            let started = std::time::Instant::now();
            m.press(&presses::parse(typed).unwrap(), Timing::default());
            m.run(30);
            let screens = m.screens_since(mark);
            let gif = pictures::gif(&screens, RECORD_EVERY, 2).unwrap();
            println!(
                "{name}: {} screens, GIF {} KB, {:.2} s to play and encode",
                screens.len(),
                gif.len() / 1024,
                started.elapsed().as_secs_f64()
            );
            assert!(gif.len() < 8 * 1024 * 1024, "too big for Discord");
            std::fs::write(out.join(format!("{name}.gif")), gif).unwrap();
        }
        std::fs::write(
            out.join("screen.png"),
            pictures::png(&m.emulator.screen()).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn jobs_run_on_the_thread_and_a_panic_only_ends_its_job() {
        let console = Console::start();
        let loaded = console
            .with(|m| {
                *m = Some(machine());
                m.as_mut().unwrap().run(5);
                m.as_ref().unwrap().emulator.frames()
            })
            .await
            .unwrap();
        assert_eq!(loaded, 5);
        let crashed = console.with(|_m| -> u8 { panic!("emulator bug") }).await;
        assert!(crashed.unwrap_err().to_string().contains("crashed"));
        // The thread is still there, and the machine was taken out.
        assert!(console.with(|m| m.is_none()).await.unwrap());
    }
}
