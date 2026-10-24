//! Where she went during one press, frame by frame: how high a jump took her and where
//! she came down, or how far a dash, a slide or a knock back carried her. A look only
//! sees where she stands before and after; the frames in between
//! (`Machine::frames_since`) show the arc. Pure, from her sprite and the
//! background's scroll, so it reads any game the same way: a screen that scrolls under her
//! moves the background, not her sprite, and both count.
//!
//! What her presses did is kept with the game's world, the last of each press, so she
//! (and the puzzle helper) can plan a jump from one she has seen.

use serde::{Deserialize, Serialize};

/// Pixels a part of her can go in one frame and still be followed: a jump moves faster
/// than walking.
const FOLLOW: i32 = 8;
/// Parts this close to her spot on the first frame are hers.
const HERS: i32 = 5;
/// Further than `HERS`, the nearest part this close is hers.
const HER_NEAR: i32 = 12;
/// Frames in a row she can flicker out before she's lost.
const LOST_MOST: u32 = 8;
/// Pixels above both where she started and where she ended that make a press a jump.
const RISE: i32 = 8;
/// Half steps she must end from the start, either way, for a press to have carried her
/// further than a step.
const FAR: i32 = 3;
/// Motions kept, the latest of each press.
const KEPT: usize = 8;

/// Where the sprites were on one frame, with the background's scroll.
pub type Frame<'a> = (&'a [(i32, i32)], (u8, u8));

/// Her place each frame, from where she was on the first, in pixels (x right, y down),
/// followed from `spot` (the middle of her sprite on the first frame). `height` is the
/// sprites' height. `None` when no sprite was at the spot, or she was lost for long.
pub fn follow(frames: &[Frame], spot: (i32, i32), height: i32) -> Option<Vec<(i32, i32)>> {
    let (&(first, _), rest) = frames.split_first()?;
    let off = |p: &(i32, i32)| ((p.0 + 4 - spot.0).abs(), (p.1 + height / 2 - spot.1).abs());
    let mut parts: Vec<(i32, i32)> = first
        .iter()
        .filter(|p| off(p).0 <= HERS && off(p).1 <= HERS)
        .copied()
        .collect();
    if parts.is_empty() {
        parts.extend(
            first
                .iter()
                .filter(|p| off(p).0 + off(p).1 <= HER_NEAR)
                .min_by_key(|p| off(p).0 + off(p).1)
                .copied(),
        );
    }
    if parts.is_empty() {
        return None;
    }
    // Each part's start and where it is now.
    let mut parts: Vec<((i32, i32), (i32, i32))> = parts.into_iter().map(|p| (p, p)).collect();
    let scrolled = scrolled(frames);
    let mut lost = 0;
    let mut places = vec![(0, 0)];
    for (&(sprites, _), &scrolled) in rest.iter().zip(&scrolled[1..]) {
        let mut kept = 0;
        for (_, now) in &mut parts {
            // A part may flicker out for a frame; it's picked up again near where it was.
            if let Some(&next) = sprites
                .iter()
                .filter(|q| (q.0 - now.0).abs() <= FOLLOW && (q.1 - now.1).abs() <= FOLLOW)
                .min_by_key(|q| (q.0 - now.0).abs() + (q.1 - now.1).abs())
            {
                *now = next;
                kept += 1;
            }
        }
        lost = if kept == 0 { lost + 1 } else { 0 };
        if lost > LOST_MOST {
            return None;
        }
        // The middle way the parts went: one caught on something passing by doesn't pull
        // her along.
        let went = |axis: fn((i32, i32)) -> i32| {
            let mut all: Vec<i32> = parts
                .iter()
                .map(|(from, now)| axis(*now) - axis(*from))
                .collect();
            all.sort_unstable();
            all[all.len() / 2]
        };
        places.push((went(|p| p.0) + scrolled.0, went(|p| p.1) + scrolled.1));
    }
    Some(places)
}

/// A press that carried her somewhere a plain step doesn't: through the air, or further
/// than a step (a dash, a slide, a fall, a knock back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Motion {
    /// What was pressed, as she'd type it, with what was held.
    pub press: String,
    /// Pixels above both ends she went at the highest; nothing for a press that kept low.
    pub rise: i32,
    /// Pixels above where she started at the highest.
    pub top: i32,
    /// Pixels from where she started to where she ended (x right, y down).
    pub ended: (i32, i32),
    /// Pixels in a step in that game.
    pub step: i32,
}

impl Motion {
    /// The motion in `places` (from `follow`), if she went higher than both ends or
    /// further than `FAR` half steps.
    pub fn of(press: &str, places: &[(i32, i32)], step: i32) -> Option<Motion> {
        let ended = *places.last()?;
        let highest = places.iter().map(|p| p.1).min()?;
        let motion = Motion {
            press: press.to_string(),
            rise: ended.1.min(0) - highest,
            top: -highest,
            ended,
            step,
        };
        let far = motion
            .halves(ended.0)
            .abs()
            .max(motion.halves(ended.1).abs())
            >= FAR;
        (motion.rise >= RISE || far).then_some(motion)
    }

    /// "went up 2.5 steps at its highest and ended 3 right 1 up from the start", or
    /// "went 5 right from the start".
    pub fn words(&self) -> String {
        let (x, y) = self.ended;
        let place = place_words((x, y), self.step);
        if self.rise < RISE {
            return format!("went {place} from the start");
        }
        let ended = if place.is_empty() {
            "came down where it started".to_string()
        } else {
            format!("ended {place} from the start")
        };
        let top = self.halves(self.top);
        let unit = if top == 2 { "step" } else { "steps" };
        format!("went up {} {unit} at its highest and {ended}", steps(top))
    }

    /// Pixels in half steps, the nearest.
    fn halves(&self, pixels: i32) -> i32 {
        halves(pixels, self.step)
    }
}

/// Pixels in half steps of `step` pixels, the nearest.
fn halves(pixels: i32, step: i32) -> i32 {
    let half = step.max(2) / 2;
    (pixels + pixels.signum() * half / 2) / half
}

/// Pixels (x right, y down) as steps in words: "3 right 1 up", or nothing for under half
/// a step.
pub fn place_words((x, y): (i32, i32), step: i32) -> String {
    let mut place = Vec::new();
    match halves(x, step) {
        0 => {}
        h if h > 0 => place.push(format!("{} right", steps(h))),
        h => place.push(format!("{} left", steps(-h))),
    }
    match halves(y, step) {
        0 => {}
        h if h > 0 => place.push(format!("{} down", steps(h))),
        h => place.push(format!("{} up", steps(-h))),
    }
    place.join(" ")
}

/// How far the background has scrolled by each frame since the first, in pixels: it
/// wraps at 256, and goes a little at a time.
pub fn scrolled(frames: &[Frame]) -> Vec<(i32, i32)> {
    let mut total = (0, 0);
    let mut last = frames.first().map_or((0, 0), |f| f.1);
    frames
        .iter()
        .map(|&(_, scroll)| {
            total.0 += i32::from(scroll.0.wrapping_sub(last.0) as i8);
            total.1 += i32::from(scroll.1.wrapping_sub(last.1) as i8);
            last = scroll;
            total
        })
        .collect()
}

/// Half steps as words: "2", "2.5".
fn steps(halves: i32) -> String {
    if halves % 2 == 0 {
        format!("{}", halves / 2)
    } else {
        format!("{}.5", halves / 2)
    }
}

/// Keeps `motion` as the latest of its press, and only the latest few presses.
pub fn remember(motions: &mut Vec<Motion>, motion: Motion) {
    motions.retain(|m| m.press != motion.press);
    motions.push(motion);
    let extra = motions.len().saturating_sub(KEPT);
    motions.drain(..extra);
}

/// The motions as lines for a model, or nothing when there are none.
pub fn list(motions: &[Motion]) -> String {
    if motions.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = motions
        .iter()
        .map(|m| format!("- {} {}", m.press, m.words()))
        .collect();
    format!(
        "Presses that carried you further than a step (the last of each, in steps):\n{}",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Her two 8×16 parts with her middle at `(x, y)`, and something else far off.
    fn frame(x: i32, y: i32) -> Vec<(i32, i32)> {
        vec![(x - 8, y - 8), (x, y - 8), (20, 120)]
    }

    #[test]
    fn follows_her_through_a_jump_on_a_scrolling_screen() {
        // She rises 40 pixels and comes down 16 higher, while the screen scrolls 48
        // pixels under her (she stays in the middle of it) and she drifts 4 right on it.
        let mut frames = Vec::new();
        for i in 0..=24 {
            let y = 100
                - [
                    0, 8, 16, 24, 32, 36, 40, 40, 38, 34, 30, 26, 22, 20, 18, 16, 16, 16, 16, 16,
                    16, 16, 16, 16, 16,
                ][i as usize];
            let x = 80 + i / 6;
            frames.push((frame(x, y), (250u8.wrapping_add(2 * i as u8), 0u8)));
        }
        let frames: Vec<Frame> = frames.iter().map(|(f, s)| (f.as_slice(), *s)).collect();
        let places = follow(&frames, (80, 100), 16).unwrap();
        assert_eq!(places.len(), 25);
        assert_eq!(*places.last().unwrap(), (52, -16));
        let jump = Motion::of("a+right", &places, 16).unwrap();
        assert_eq!(jump.top, 40);
        assert_eq!(jump.ended, (52, -16));
        assert_eq!(
            jump.words(),
            "went up 2.5 steps at its highest and ended 3.5 right 1 up from the start"
        );
    }

    #[test]
    fn a_plain_step_is_no_motion_but_a_dash_or_a_fall_is() {
        let walked = [(0, 0), (8, 0), (16, 0)];
        assert_eq!(Motion::of("right", &walked, 16), None);
        let dashed = [(0, 0), (30, 0), (80, 0)];
        assert_eq!(
            Motion::of("b+right", &dashed, 16).unwrap().words(),
            "went 5 right from the start"
        );
        let fell = [(0, 0), (4, 8), (8, 40)];
        assert_eq!(
            Motion::of("right", &fell, 16).unwrap().words(),
            "went 0.5 right 2.5 down from the start"
        );
        let hop = [(0, 0), (0, -12), (0, 0)];
        assert_eq!(
            Motion::of("a", &hop, 16).unwrap().words(),
            "went up 1 step at its highest and came down where it started"
        );
    }

    #[test]
    fn keeps_the_latest_of_each_press() {
        let jump = |press: &str, x| Motion {
            press: press.to_string(),
            rise: 16,
            top: 16,
            ended: (x, 0),
            step: 16,
        };
        let mut jumps = Vec::new();
        remember(&mut jumps, jump("a", 0));
        remember(&mut jumps, jump("a+right", 32));
        remember(&mut jumps, jump("a", 16));
        assert_eq!(jumps, vec![jump("a+right", 32), jump("a", 16)]);
        for i in 0..10 {
            remember(&mut jumps, jump(&format!("a:{}", 10 + i), 0));
        }
        assert_eq!(jumps.len(), KEPT);
        assert_eq!(jumps[0].press, "a:12");
        assert!(list(&jumps).starts_with("Presses that carried you"));
        assert_eq!(list(&[]), "");
    }

    #[test]
    fn no_sprite_at_her_spot_is_no_path() {
        let f = frame(80, 100);
        assert_eq!(follow(&[(f.as_slice(), (0, 0))], (140, 40), 16), None);
    }
}
