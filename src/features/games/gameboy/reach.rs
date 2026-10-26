//! Where her jumps would land from where she stands, tried before she picks one. Once she
//! has jumped in a game (a press that took her up and down again, `path.rs`), each look
//! tries that button with the ways held for short to long in a copy of the game
//! (`Emulator::trying`), follows her through each try, and says where it put her. The game
//! she plays never moves on. So instead of guessing how long to hold right, she picks a
//! landing she can see.
//!
//! Nothing here knows a game: the button is the one she jumped with, and where she lands is
//! read from her sprite and the scroll, like any of her presses.

use super::console::Machine;
use super::emulator::{Button, Emulator};
use super::map::Pos;
use super::path::{self, Frame, LEVEL, Motion};
use super::presses::{self, Press};
use super::screen::sprite_places;
use super::world::World;

/// Frames the jump button is held: a hop, and as high as it goes (Mega Man's jump stops
/// rising after about 20).
const JUMP_HOLDS: [u32; 2] = [8, 24];
/// Frames a way is held with it, shortest first.
const WAY_HOLDS: [u32; 5] = [12, 24, 36, 48, 60];
/// Frames each try is followed for from the press: long enough for the longest hold and
/// the fall after.
const TRY_FRAMES: u32 = 100;
/// Jump buttons tried, the latest she used first.
const BUTTONS_MOST: usize = 2;
/// Landings said, nearest first.
const MOST: usize = 10;

/// Where one try put her.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    /// The press, as she'd type it.
    pub press: String,
    pub ended: Ended,
    /// Whether she blinked out of sight on the way, the way games show a hit (or too many
    /// sprites on a line): where she ended may be where something knocked her.
    pub blinked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Standing here (steps on her map), still for the last frames.
    On(Pos),
    /// Still going up or down here when the try ended: a long fall.
    Falling(Pos),
    /// Her sprite went out of sight: off the screen (a pit) or gone (hit, a game over).
    Gone,
}

/// The buttons she has jumped with in this game, the latest first.
pub fn jump_buttons(motions: &[Motion]) -> Vec<Button> {
    let mut buttons = Vec::new();
    for motion in motions.iter().rev().filter(|m| m.jumped()) {
        // "a+right:30 holding b": the press before what was held.
        let typed = motion.press.split(" holding").next().unwrap_or_default();
        let Ok(pressed) = presses::parse(typed) else {
            continue;
        };
        for button in pressed.iter().flat_map(|p| &p.buttons) {
            if !button.is_direction() && !buttons.contains(button) {
                buttons.push(*button);
            }
        }
    }
    buttons.truncate(BUTTONS_MOST);
    buttons
}

/// The presses tried for `button`, as she'd type them: each hold of it alone, then with
/// right and left held for longer and longer.
pub fn tries(button: Button) -> Vec<String> {
    let name = button.name();
    let mut typed = Vec::new();
    for jump in JUMP_HOLDS {
        typed.push(format!("{name}:{jump}"));
        for way in ["right", "left"] {
            for hold in WAY_HOLDS {
                typed.push(format!("{name}:{jump}+{way}:{hold}"));
            }
        }
    }
    typed
}

/// Where she ended, from her places each frame of a try (`path::follow`, `None` when she
/// was lost), starting `at` on her map, with `step` pixels a step.
pub fn ended(places: Option<&[(i32, i32)]>, at: Pos, step: i32) -> Ended {
    let Some(&last) = places.and_then(|p| p.last()) else {
        return Ended::Gone;
    };
    let steps = |pixels: i32| (pixels + pixels.signum() * step / 2) / step.max(1);
    let spot = (at.0 + steps(last.0), at.1 + steps(last.1));
    if standing(places.unwrap_or_default()) {
        Ended::On(spot)
    } else {
        Ended::Falling(spot)
    }
}

/// The landings worth saying: one press for each place (the first tried, so the shortest
/// hold, and one she didn't blink on before one she did), leaving out the ones back where
/// she stands, nearest first.
pub fn kept(mut landings: Vec<Landing>, at: Pos) -> Vec<Landing> {
    landings.sort_by_key(|l| l.blinked);
    let mut kept: Vec<Landing> = Vec::new();
    for landing in landings {
        if landing.ended == Ended::On(at) || kept.iter().any(|k| k.ended == landing.ended) {
            continue;
        }
        kept.push(landing);
    }
    let far = |l: &Landing| match l.ended {
        Ended::On(p) | Ended::Falling(p) => (p.0 - at.0).abs() + (p.1 - at.1).abs(),
        Ended::Gone => i32::MAX,
    };
    kept.sort_by_key(far);
    kept.truncate(MOST);
    kept
}

/// The landings as lines for a model, or nothing when there are none.
pub fn list(landings: &[Landing], at: Pos) -> String {
    if landings.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = landings
        .iter()
        .map(|l| {
            let words = match l.ended {
                Ended::On(p) => format!("lands on ({}, {}){}", p.0, p.1, from(p, at)),
                Ended::Falling(p) => {
                    format!("still falling at ({}, {}){}", p.0, p.1, from(p, at))
                }
                Ended::Gone => "you go out of sight (a pit, or hit)".to_string(),
            };
            let blinked = if l.blinked {
                ", but you blinked on the way (hit?)"
            } else {
                ""
            };
            format!("- {}: {words}{blinked}", l.press)
        })
        .collect();
    format!(
        "Jumps from here, tried in a copy of the game (nothing happened in yours):\n{}",
        lines.join("\n")
    )
}

/// ", 2 right 1 up" from `at` to `p`.
fn from(p: Pos, at: Pos) -> String {
    let words = path::place_words(((p.0 - at.0) * 16, (p.1 - at.1) * 16), 16);
    if words.is_empty() {
        String::new()
    } else {
        format!(", {words}")
    }
}

/// Tries her jumps from where she stands now, at `at` on her map with her sprite's middle
/// at `spot` on the screen, and gives where each landed worth saying. Nothing until she
/// has jumped in this game, when none of the tries jumped from here, or when the game
/// can't be put back.
pub fn from_here(
    world: &World,
    machine: &mut Machine,
    spot: (i32, i32),
    at: Pos,
    step: i32,
) -> Vec<Landing> {
    let height = if machine.emulator.video().registers.lcdc & 0x04 != 0 {
        16
    } else {
        8
    };
    let mut landings = Vec::new();
    let mut any_jumped = false;
    for button in jump_buttons(&world.motions) {
        for typed in tries(button) {
            let Some(press) = presses::parse(&typed)
                .ok()
                .and_then(|p| p.into_iter().next())
            else {
                continue;
            };
            let Ok(followed) = machine
                .emulator
                .trying(|emulator| follow_try(emulator, &press, spot, height))
            else {
                return Vec::new();
            };
            let (places, blinks) = followed.map_or((None, 0), |(p, b)| (Some(p), b));
            // A try that didn't take her up into the air isn't a jump from here (a wall
            // over her, or a button that only jumps somewhere else).
            let jumped = places
                .as_deref()
                .and_then(|p| Motion::of(&typed, p, step))
                .is_some_and(|m| m.jumped());
            if jumped || places.is_none() {
                landings.push(Landing {
                    press: typed,
                    ended: ended(places.as_deref(), at, step),
                    blinked: blinks > 0,
                });
            }
            any_jumped |= jumped;
        }
    }
    // Where none of them jumped, going out of sight wasn't a jump either.
    if !any_jumped {
        return Vec::new();
    }
    kept(landings, at)
}

/// Presses `press` and follows her from `spot` (`path::follow`) until she has stood still
/// for a while after it, or for `TRY_FRAMES` frames. `None` when she was lost.
fn follow_try(
    emulator: &mut Emulator,
    press: &Press,
    spot: (i32, i32),
    height: i32,
) -> Option<(Vec<(i32, i32)>, u32)> {
    let frame = |emulator: &Emulator| {
        let video = emulator.video();
        (
            sprite_places(video),
            (video.registers.scx, video.registers.scy),
        )
    };
    let whole = press.frames.unwrap_or(TRY_FRAMES);
    let mut seen = vec![frame(emulator)];
    let mut places = None;
    for n in 0..TRY_FRAMES {
        let held: Vec<Button> = press
            .buttons
            .iter()
            .copied()
            .filter(|&b| press.frames_of(b, whole) > n)
            .collect();
        emulator.hold(&held);
        emulator.frame();
        seen.push(frame(emulator));
        // Once she's let go of it all and stood still for a while, it's over.
        if n >= whole + LEVEL as u32 || n + 1 == TRY_FRAMES {
            let frames: Vec<Frame> = seen.iter().map(|(s, sc)| (s.as_slice(), *sc)).collect();
            places = path::follow_blinking(&frames, spot, height);
            match &places {
                Some((p, _)) if !standing(p) => {}
                _ => break,
            }
        }
    }
    emulator.release();
    places
}

/// Whether she has stayed at one height for the last `LEVEL` frames.
fn standing(places: &[(i32, i32)]) -> bool {
    let Some(last) = places.last() else {
        return false;
    };
    places.len() > LEVEL && places[places.len() - LEVEL..].iter().all(|p| p.1 == last.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(press: &str, rise: i32) -> Motion {
        Motion {
            press: press.to_string(),
            rise,
            top: rise,
            ended: (32, 0),
            step: 16,
            peak: 20,
            landed: 44,
        }
    }

    #[test]
    fn the_jump_buttons_are_the_ones_she_jumped_with() {
        let motions = [
            motion("b+right:30", 0),
            motion("a+right:30 holding b", 40),
            motion("a:20+left:50", 40),
        ];
        assert_eq!(jump_buttons(&motions), vec![Button::A]);
        assert!(jump_buttons(&[motion("b+right", 0)]).is_empty());
    }

    #[test]
    fn every_try_is_a_press_she_could_type() {
        let typed = tries(Button::A);
        assert_eq!(typed.len(), 22);
        assert_eq!(typed[0], "a:8");
        assert!(typed.contains(&"a:24+left:60".to_string()));
        for one in &typed {
            assert_eq!(presses::parse(one).unwrap().len(), 1, "{one}");
        }
    }

    #[test]
    fn where_a_try_ended() {
        let mut landed: Vec<(i32, i32)> = (0..20).map(|i| (i * 2, -i)).collect();
        landed.extend(std::iter::repeat_n((40, -16), LEVEL + 1));
        assert_eq!(ended(Some(&landed), (10, 6), 16), Ended::On((13, 5)));
        let falling: Vec<(i32, i32)> = (0..30).map(|i| (8, i * 3)).collect();
        assert_eq!(ended(Some(&falling), (10, 6), 16), Ended::Falling((11, 11)));
        assert_eq!(ended(None, (10, 6), 16), Ended::Gone);
    }

    #[test]
    fn one_press_is_said_for_each_landing_nearest_first() {
        let landing = |press: &str, ended, blinked| Landing {
            press: press.to_string(),
            ended,
            blinked,
        };
        let kept = kept(
            vec![
                landing("a:8", Ended::On((10, 6)), false),
                landing("a:8+right:12", Ended::On((12, 5)), true),
                landing("a:8+right:24", Ended::On((12, 5)), false),
                landing("a:24+right:60", Ended::Gone, false),
                landing("a:24+right:12", Ended::On((11, 6)), false),
                landing("a:24+left:12", Ended::On((9, 5)), true),
            ],
            (10, 6),
        );
        let pressed: Vec<&str> = kept.iter().map(|l| l.press.as_str()).collect();
        assert_eq!(
            pressed,
            [
                "a:24+right:12",
                "a:24+left:12",
                "a:8+right:24",
                "a:24+right:60"
            ]
        );
        let said = list(&kept, (10, 6));
        assert!(
            said.contains("- a:8+right:24: lands on (12, 5), 2 right 1 up\n"),
            "{said}"
        );
        assert!(
            said.contains("- a:24+left:12: lands on (9, 5), 1 left 1 up, but you blinked"),
            "{said}"
        );
        assert!(
            said.contains("- a:24+right:60: you go out of sight"),
            "{said}"
        );
    }
}
