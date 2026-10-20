//! Taking one step and seeing what happened: moved, turned, blocked, or somewhere new.
//! Every step goes in the movement log, so walls, ledges and one-way doors show up in any
//! game without anyone describing them.
//!
//! Most games that walk on a grid scroll the screen under a player who stays in the
//! middle, so the step's result is in the scroll: 16 pixels (in most games) is one step,
//! 32 is a jump down a ledge, none is blocked. A changed player sprite with no scroll is a
//! turn. A dark spell (`change.rs`) means a door or stairs. In Pokémon Red, memory says it
//! too, and the two are compared (see `red::Trust`).

use super::change::{Look, Transitions, look};
use super::console::Machine;
use super::emulator::Button;
use super::screen::Screen;

/// Pixels in one step of a grid game. Pokémon and most games like it use 16.
pub const STEP_PIXELS: i32 = 16;

/// How long a direction is held at most, waiting for the step to start. A turn in Red
/// takes a few frames before the step begins.
const HOLD_MOST: u32 = 24;

/// How long to wait for a step (or a door) to finish after letting go.
const SETTLE_MOST: u32 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Moved this many steps (2 for a ledge jump).
    Moved(i32),
    /// The screen didn't scroll, but the player turned to face that way.
    Turned,
    /// Nothing moved: a wall, a person, or the game not taking input.
    Blocked,
    /// The screen went dark and came back: through a door, stairs or a warp.
    NewArea,
}

/// One step, as the movement log keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub direction: Button,
    pub outcome: Outcome,
    /// How far the screen scrolled, in pixels (x, y): positive is right and down.
    pub scrolled: (i32, i32),
    /// Frames it took, from pressing to settled.
    pub frames: u32,
}

/// The unit offset of a direction (x, y), or `None` for a button that isn't one.
pub fn offset(direction: Button) -> Option<(i32, i32)> {
    match direction {
        Button::Up => Some((0, -1)),
        Button::Down => Some((0, 1)),
        Button::Left => Some((-1, 0)),
        Button::Right => Some((1, 0)),
        _ => None,
    }
}

/// The sprites in the middle of the screen, where the player stands in most grid games:
/// their pictures, to tell a turn from nothing.
fn player_look(screen: &Screen) -> Vec<u64> {
    let mut keys: Vec<u64> = screen
        .sprites
        .iter()
        .filter(|s| (56..=88).contains(&(s.x + 4)) && (52..=84).contains(&(s.y + 4)))
        .map(|s| s.key ^ u64::from(s.flags & 0x20))
        .collect();
    keys.sort_unstable();
    keys
}

/// Scroll difference between two register values, allowing for the wrap at 256.
fn scroll_delta(before: u8, after: u8) -> i32 {
    i32::from(after.wrapping_sub(before) as i8)
}

/// Takes one step in `direction`: holds it until the screen starts to scroll (or long
/// enough that it won't), lets go, and waits until the screen is still again.
pub fn step(machine: &mut Machine, direction: Button) -> Step {
    let mut screen = Screen::read(machine.emulator.video());
    let start_look = player_look(&screen);
    let mut scrolled = (0, 0);
    let mut transitions = Transitions::default();
    let mut new_area = false;
    let mut frames = 0;
    let mut still = 0;

    machine.emulator.hold(&[direction]);
    let mut holding = true;
    while frames < HOLD_MOST + SETTLE_MOST {
        machine.frame();
        frames += 1;
        let video = machine.emulator.video().clone();
        let rgb = machine.emulator.screen();
        let frame_look = look(video.drawn, &video.registers, &rgb, video.color);
        new_area |= transitions.feed(frame_look);
        let next = Screen::read(&video);
        let moved = if frame_look == Look::Picture && next.drawn && screen.drawn {
            (
                scroll_delta(screen.registers.scx, next.registers.scx),
                scroll_delta(screen.registers.scy, next.registers.scy),
            )
        } else {
            (0, 0)
        };
        // A text box or menu in the window doesn't scroll anything, and the background
        // under it keeps its scroll, so the scroll registers are safe to follow.
        scrolled.0 += moved.0;
        scrolled.1 += moved.1;
        if frame_look == Look::Picture {
            screen = next;
        }
        if holding && (moved != (0, 0) || transitions.dark() || frames >= HOLD_MOST) {
            machine.emulator.release();
            holding = false;
        }
        if !holding {
            still = if moved == (0, 0) && !transitions.dark() {
                still + 1
            } else {
                0
            };
            // Settled: a few still frames after the scroll stopped.
            if still >= 4 && (scrolled != (0, 0) || new_area || frames >= HOLD_MOST + 4) {
                break;
            }
        }
    }
    machine.emulator.release();

    let outcome = if new_area {
        Outcome::NewArea
    } else if scrolled != (0, 0) {
        let pixels = scrolled.0.abs().max(scrolled.1.abs());
        Outcome::Moved(((pixels + STEP_PIXELS / 2) / STEP_PIXELS).max(1))
    } else if player_look(&screen) != start_look {
        Outcome::Turned
    } else {
        Outcome::Blocked
    };
    Step {
        direction,
        outcome,
        scrolled,
        frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::test_rom;

    #[test]
    fn scroll_wraps_around() {
        assert_eq!(scroll_delta(250, 2), 8);
        assert_eq!(scroll_delta(2, 250), -8);
        assert_eq!(scroll_delta(16, 16), 0);
    }

    #[test]
    fn directions_have_offsets() {
        assert_eq!(offset(Button::Up), Some((0, -1)));
        assert_eq!(offset(Button::Right), Some((1, 0)));
        assert_eq!(offset(Button::A), None);
    }

    #[test]
    fn a_scrolling_screen_is_a_move_and_a_still_one_is_blocked() {
        let mut m = Machine::new("test.gb", &test_rom::build()).unwrap();
        m.run(300);
        // The test ROM scrolls a pixel a frame while Right is held, so a step that lets go
        // as soon as the scroll starts moves a pixel or two: rounded up, one step.
        let right = step(&mut m, Button::Right);
        assert_eq!(right.outcome, Outcome::Moved(1));
        assert!(right.scrolled.0 > 0 && right.scrolled.1 == 0);
        // Nothing scrolls for Left in the test ROM, and there's no player sprite.
        let left = step(&mut m, Button::Left);
        assert_eq!(left.outcome, Outcome::Blocked);
        assert_eq!(left.scrolled, (0, 0));
    }
}
