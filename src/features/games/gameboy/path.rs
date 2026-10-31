//! Where she went during one press, frame by frame: how high a jump took her and where
//! she came down, or how far a dash, a slide or a knock back carried her. A look only
//! sees where she stands before and after; the frames in between
//! (`Machine::frames_since`) show the arc. Pure, from her sprite and the
//! background's scroll, so it reads any game the same way: a screen that scrolls under her
//! moves the background, not her sprite, and both count.
//!
//! What her presses did is kept with the game's world, the last of each press, so she
//! (and the puzzle helper) can plan a jump from one she has seen. So is her pace: how far a
//! frame of holding each direction carries her, so a press can be held for a distance.

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
/// Frames of a hold she must move in for it to show her pace.
const MOVING_LEAST: u32 = 8;
/// Frames in a row she stays at one height for a jump to have landed.
pub const LEVEL: usize = 12;
/// Paces kept for each direction on the ground and in the air, the latest.
const PACES_KEPT: usize = 6;

/// Where the sprites were on one frame, with the background's scroll.
pub type Frame<'a> = (&'a [(i32, i32)], (u8, u8));

/// Her place each frame, from where she was on the first, in pixels (x right, y down),
/// followed from `spot` (the middle of her sprite on the first frame). `height` is the
/// sprites' height. `None` when no sprite was at the spot, or she was lost for long.
pub fn follow(frames: &[Frame], spot: (i32, i32), height: i32) -> Option<Vec<(i32, i32)>> {
    follow_blinking(frames, spot, height).map(|(places, _)| places)
}

/// Like `follow`, with how many frames she was out of sight for a frame or a few: a game
/// makes her blink like that after something hurt her.
pub fn follow_blinking(
    frames: &[Frame],
    spot: (i32, i32),
    height: i32,
) -> Option<(Vec<(i32, i32)>, u32)> {
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
    let mut blinks = 0;
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
        blinks += u32::from(kept == 0);
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
    Some((places, blinks))
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
    /// Frames from the press to where she was highest, and to the first frame after that
    /// she stayed level from for `LEVEL` frames (landed).
    #[serde(default)]
    pub peak: usize,
    #[serde(default)]
    pub landed: usize,
}

impl Motion {
    /// The motion in `places` (from `follow`), if she went higher than both ends or
    /// further than `FAR` half steps.
    pub fn of(press: &str, places: &[(i32, i32)], step: i32) -> Option<Motion> {
        let ended = *places.last()?;
        let highest = places.iter().map(|p| p.1).min()?;
        let peak = places.iter().position(|p| p.1 == highest)?;
        // Landed: the first frame after the highest that she stayed level from.
        let landed = (peak..places.len())
            .find(|&i| places[i..].iter().take(LEVEL).all(|p| p.1 == places[i].1))
            .unwrap_or(places.len());
        // Going up while up is held, with no other button, is walking or climbing up, not
        // a jump: in a game seen from above, a step up onto a spinner that carried her
        // sideways had read as a jump, and the puzzle helper planned jumps in Red.
        let words: Vec<&str> = press.split([' ', '+', ':']).collect();
        let up_only = words.contains(&"up")
            && !words
                .iter()
                .any(|w| matches!(*w, "a" | "b" | "start" | "select"));
        let motion = Motion {
            press: press.to_string(),
            rise: if up_only { 0 } else { ended.1.min(0) - highest },
            top: -highest,
            ended,
            step,
            peak,
            landed,
        };
        let far = motion
            .halves(ended.0)
            .abs()
            .max(motion.halves(ended.1).abs())
            >= FAR;
        (motion.rise >= RISE || far).then_some(motion)
    }

    /// "went up 2.5 steps at its highest (frame 20) and ended 3 right 1 up from the start
    /// (landed by frame 44)", or "went 5 right from the start".
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
        let timed = if self.landed > 0 {
            format!(
                " (frame {}) and {ended} (landed by frame {})",
                self.peak, self.landed
            )
        } else {
            format!(" and {ended}")
        };
        format!("went up {} {unit} at its highest{timed}", steps(top))
    }

    /// Whether it took her up into the air and down again: a jump.
    pub fn jumped(&self) -> bool {
        self.rise >= RISE
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

/// How far `frames` of holding one direction carried her, on the ground or in the air.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pace {
    /// The direction's name.
    pub way: String,
    /// Frames she was going up or down in (a jump, a fall), for a sideways direction.
    #[serde(default)]
    pub air: bool,
    pub frames: u32,
    pub pixels: i32,
    pub step: i32,
}

/// Her pace holding `way` (pointing `unit`, x right and y down) for `held` frames, from her
/// places that press (`follow`): the frames of the hold she moved that way in and how far
/// they took her, on the ground and in the air apart, so a hold that ran into a wall or
/// landed part way counts only the frames she moved. Each needs `MOVING_LEAST` frames.
pub fn paces_of(
    way: &str,
    unit: (i32, i32),
    held: u32,
    places: &[(i32, i32)],
    step: i32,
) -> Vec<Pace> {
    let along = |p: &(i32, i32)| p.0 * unit.0 + p.1 * unit.1;
    let end = (held as usize).min(places.len().saturating_sub(1));
    // Frames and pixels on the ground, then in the air.
    let mut moved = [(0, 0); 2];
    for w in places[..=end].windows(2) {
        let went = along(&w[1]) - along(&w[0]);
        if went > 0 {
            let air = unit.1 == 0 && w[1].1 != w[0].1;
            let one = &mut moved[usize::from(air)];
            one.0 += 1;
            one.1 += went;
        }
    }
    [false, true]
        .into_iter()
        .zip(moved)
        .filter(|&(_, (frames, _))| frames >= MOVING_LEAST)
        .map(|(air, (frames, pixels))| Pace {
            way: way.to_string(),
            air,
            frames,
            pixels,
            step,
        })
        .collect()
}

/// Keeps `pace` with the latest few of its direction, on the ground or in the air.
pub fn remember_pace(paces: &mut Vec<Pace>, pace: Pace) {
    let same = |p: &Pace, q: &Pace| p.way == q.way && p.air == q.air;
    let kept = pace.clone();
    paces.push(pace);
    if paces.iter().filter(|p| same(p, &kept)).count() > PACES_KEPT
        && let Some(oldest) = paces.iter().position(|p| same(p, &kept))
    {
        paces.remove(oldest);
    }
}

/// Her pace each way as lines for a model, from the middle of the ones measured, or
/// nothing when none are.
pub fn paces(paces: &[Pace]) -> String {
    let mut ways: Vec<&str> = Vec::new();
    for pace in paces {
        if !ways.contains(&pace.way.as_str()) {
            ways.push(&pace.way);
        }
    }
    let lines: Vec<String> = ways
        .into_iter()
        .map(|way| {
            let parts: Vec<String> = [(false, "on the ground"), (true, "in the air")]
                .into_iter()
                .filter_map(|(air, words)| {
                    let mut each: Vec<&Pace> = paces
                        .iter()
                        .filter(|p| p.way == way && p.air == air)
                        .collect();
                    each.sort_by(|a, b| {
                        (a.pixels * b.frames as i32).cmp(&(b.pixels * a.frames as i32))
                    });
                    let mid = each.get(each.len() / 2)?;
                    let per_frame = mid.pixels as f32 / mid.frames as f32;
                    let frames = (mid.step as f32 / per_frame).round().max(1.0);
                    Some(format!(
                        "{words} {per_frame:.1} pixels a frame (a step every {frames} frames)"
                    ))
                })
                .collect();
            let presses = paces.iter().filter(|p| p.way == way).count();
            format!("- {way}: {} (from {presses} measured)", parts.join(", "))
        })
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "How far each frame of holding a direction carries you (60 frames a second):\n{}",
        lines.join("\n")
    )
}

/// What her presses did, for a model: the motions and her paces, or nothing.
pub fn presses(motions: &[Motion], paces: &[Pace]) -> String {
    let lists = [list(motions), self::paces(paces)];
    let lists: Vec<&String> = lists.iter().filter(|l| !l.is_empty()).collect();
    lists
        .iter()
        .map(|l| l.as_str())
        .collect::<Vec<_>>()
        .join("\n")
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
        assert_eq!((jump.peak, jump.landed), (6, 15));
        assert_eq!(
            jump.words(),
            "went up 2.5 steps at its highest (frame 6) and ended 3.5 right 1 up from the \
             start (landed by frame 15)"
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
            "went up 1 step at its highest (frame 1) and came down where it started (landed \
             by frame 2)"
        );
        // A step up that a spinner carried on sideways and back down a step (Red's Rocket
        // Hideout): a ride, not a jump.
        let ride = [(0, 0), (0, -16), (-48, -16), (-112, 0)];
        let carried = Motion::of("up", &ride, 16).unwrap();
        assert!(!carried.jumped());
        assert_eq!(carried.words(), "went 7 left from the start");
        assert!(!Motion::of("up holding up", &ride, 16).unwrap().jumped());
        // Jumping while holding up is still a jump.
        assert!(Motion::of("a+up", &hop, 16).unwrap().jumped());
    }

    #[test]
    fn keeps_the_latest_of_each_press() {
        let jump = |press: &str, x| Motion {
            press: press.to_string(),
            rise: 16,
            top: 16,
            ended: (x, 0),
            step: 16,
            peak: 8,
            landed: 16,
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
    fn the_frames_she_moved_in_show_her_pace() {
        let walk: Vec<(i32, i32)> = (0..30).map(|i| (i * 3 / 2, 0)).collect();
        let right = paces_of("right", (1, 0), 20, &walk, 16);
        assert_eq!(right.len(), 1);
        assert_eq!(
            (right[0].air, right[0].frames, right[0].pixels),
            (false, 20, 30)
        );
        // Into a wall after 10 frames: only the frames she moved count.
        let walled: Vec<(i32, i32)> = (0..30).map(|i| (i.min(10), 0)).collect();
        let wall = &paces_of("right", (1, 0), 20, &walled, 16)[0];
        assert_eq!((wall.frames, wall.pixels), (10, 10));
        // A jump: 10 frames on the ground, then 12 going up or down.
        let jump: Vec<(i32, i32)> = (0..30)
            .map(|i| (2 * i.min(10) + (i - 10).max(0), -(i - 10).clamp(0, 12)))
            .collect();
        let both = paces_of("right", (1, 0), 22, &jump, 16);
        assert_eq!(
            both.iter()
                .map(|p| (p.air, p.frames, p.pixels))
                .collect::<Vec<_>>(),
            vec![(false, 10, 20), (true, 12, 12)]
        );
        // The wrong way, or too short a hold to tell.
        assert!(paces_of("left", (-1, 0), 20, &walk, 16).is_empty());
        assert!(paces_of("right", (1, 0), 6, &walk, 16).is_empty());
        let mut paces = Vec::new();
        for frames in [10, 20, 30, 40, 50, 60, 70] {
            let right = Pace {
                frames,
                pixels: frames as i32,
                ..right[0].clone()
            };
            remember_pace(&mut paces, right);
        }
        assert_eq!(paces.len(), PACES_KEPT);
        assert_eq!(paces[0].frames, 20);
        remember_pace(&mut paces, both[1].clone());
        assert_eq!(paces.len(), PACES_KEPT + 1);
        assert_eq!(
            super::paces(&paces),
            "How far each frame of holding a direction carries you (60 frames a second):\n- \
             right: on the ground 1.0 pixels a frame (a step every 16 frames), in the air 1.0 \
             pixels a frame (a step every 16 frames) (from 7 measured)"
        );
        assert_eq!(super::paces(&[]), "");
    }

    #[test]
    fn no_sprite_at_her_spot_is_no_path() {
        let f = frame(80, 100);
        assert_eq!(follow(&[(f.as_slice(), (0, 0))], (140, 40), 16), None);
    }
}
