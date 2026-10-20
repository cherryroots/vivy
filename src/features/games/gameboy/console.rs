//! The Game Boy on a thread of its own, so emulating never holds up the bot's async tasks.
//!
//! `Machine` is the emulator plus what Vivy needs around it: which ROM is in it, and the
//! last few seconds of screens for GIFs. `Console` owns the one machine on its thread and
//! runs jobs on it: `console().with(|machine| ...)` sends a closure there and waits for its
//! answer, the same way `ctx.db.call` works for the database.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::LazyLock;
use std::sync::mpsc;

use anyhow::{Context as _, Result, anyhow};

use super::emulator::{Button, Emulator, FPS};
use super::presses::Press;

/// A screen is kept for GIFs every this many frames: 20 a second.
pub const RECORD_EVERY: u32 = 3;

/// How many seconds of screens are kept. A screen is 69 KB, so 10 s is about 14 MB.
pub const RECENT_SECONDS: u32 = 10;

const RECENT_SCREENS: usize = (RECENT_SECONDS as f64 * FPS / RECORD_EVERY as f64) as usize;

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

pub struct Machine {
    pub emulator: Emulator,
    /// The ROM's file name in `roms_dir`.
    pub rom: String,
    /// Recent screens, oldest first, one every `RECORD_EVERY` frames.
    recent: VecDeque<Vec<u8>>,
    /// Screens recorded since the ROM was put in, so a caller can ask for the ones after
    /// some point.
    recorded: u64,
}

impl Machine {
    pub fn new(rom: &str, bytes: &[u8]) -> Result<Machine> {
        Ok(Machine {
            emulator: Emulator::new(bytes)?,
            rom: rom.to_string(),
            recent: VecDeque::with_capacity(RECENT_SCREENS),
            recorded: 0,
        })
    }

    /// Runs one frame, keeping its screen if it's one to record.
    pub fn frame(&mut self) {
        self.emulator.frame();
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
    }

    pub fn run(&mut self, frames: u32) {
        for _ in 0..frames {
            self.frame();
        }
    }

    /// Presses each in turn: held for `timing.hold` frames, then nothing for `timing.gap`.
    pub fn press(&mut self, presses: &[Press], timing: Timing) {
        for Press(buttons) in presses {
            self.hold_for(buttons, timing.hold);
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

    /// Forgets the recorded screens, after loading a state, so a GIF never jumps between
    /// two games.
    pub fn clear_recording(&mut self) {
        self.recent.clear();
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
