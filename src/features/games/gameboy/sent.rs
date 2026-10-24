//! What a press sent out: a shot, a thrown boomerang, a fireball. Frame by frame, a thing
//! that shows up right next to her and moves away from her came from her; code follows it
//! until it's gone, and says which way it went, how far, and how it ended. Pure, from the
//! sprites of each frame and her place in them (`path::follow`), so it reads any game the
//! same way: nothing knows what a shot looks like.

use serde::{Deserialize, Serialize};

use super::path::{Frame, place_words, scrolled};

/// Pixels from her middle that are her own (an arm held out to fire).
const HERS: i32 = 10;
/// A new thing this close to one of her parts (side plus down) came from her.
const SPAWN: i32 = 24;
/// Frames back a thing must not have been near, to be new: something flickering back
/// isn't.
pub const FRESH_FRAMES: usize = 8;
/// Pixels from anything on those frames that make a thing new.
const FRESH: i32 = 12;
/// Pixels a thing can stray in a frame from where it was heading and still be followed.
const FOLLOW: i32 = 6;
/// Pixels apart that make two parts that came out together one thing.
const TOGETHER: i32 = 9;
/// Frames it can be missing before it's gone.
const GONE: u32 = 3;
/// Pixels from something else, where it went missing, that make it gone by that thing.
const NEXT_TO: i32 = 16;
/// Pixels it must move from her (as she moves too), and on the map, to have been sent:
/// her own sword swings with her, and a thing standing still stays put.
const AWAY: i32 = 16;
/// The screen, and pixels from its edge where a thing going missing went off it.
const WIDTH: i32 = 160;
const HEIGHT: i32 = 144;
const EDGE: i32 = 6;
/// Things said for one press at most.
const MOST: usize = 3;
/// Presses kept, the latest of each.
const KEPT: usize = 8;

/// How a thing she sent ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum End {
    /// Still on screen when the press was over.
    Going,
    /// Gone, with nothing else near.
    Gone,
    /// Gone next to something else: it may have hit it.
    GoneBy,
    /// Gone off the edge of the screen.
    OffScreen,
}

/// Something a press sent out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    /// What was pressed, as she'd type it, with what was held.
    pub press: String,
    /// Pixels from where it came out to the furthest it got (x right, y down).
    pub went: (i32, i32),
    /// Whether it came back towards where it came out from.
    pub back: bool,
    pub end: End,
    /// Pixels in a step in that game.
    pub step: i32,
}

impl Sent {
    /// "6 right, and it was gone next to something else".
    pub fn words(&self) -> String {
        let went = place_words(self.went, self.step);
        let went = if went.is_empty() {
            "less than half a step".to_string()
        } else {
            went
        };
        let back = if self.back { ", came back" } else { "" };
        let end = match self.end {
            End::Going => "and was still going",
            End::Gone => "and was gone",
            End::GoneBy => "and was gone next to something else",
            End::OffScreen => "and went off the screen",
        };
        format!("{went}{back}, {end}")
    }
}

/// One thing followed through the frames.
struct Track {
    /// Where it was each frame it was seen, on the map (screen plus scroll).
    at: Vec<(i32, i32)>,
    /// The same, from her.
    from_her: Vec<(i32, i32)>,
    /// On screen, where it was last seen and how far it went that frame.
    last: (i32, i32),
    heading: (i32, i32),
    missing: u32,
    /// How it ends if it doesn't come back: by something else near where it went missing,
    /// or past the screen's edge.
    gone: End,
    end: Option<End>,
}

/// What came out of her during a press: `frames` from its start (`Machine::frames_since`)
/// and a few `before` it (so a shot already flying that flickers back isn't new),
/// `spot` her middle on the first, `places` her own path through them (`path::follow`),
/// `height` the sprites' height and `step` the pixels in a step. At most `MOST`, the
/// first sent first.
pub fn sent(
    press: &str,
    before: &[Frame],
    frames: &[Frame],
    spot: (i32, i32),
    places: &[(i32, i32)],
    height: i32,
    step: i32,
) -> Vec<Sent> {
    let scrolled = scrolled(frames);
    // Each part on its own, by its middle: a shot is often one part, and one that touched
    // what it hit would be taken into it.
    let things: Vec<Vec<(i32, i32)>> = frames
        .iter()
        .map(|f| {
            f.0.iter()
                .map(|p| (p.0 + 4, p.1 + height / 2))
                .collect::<Vec<_>>()
        })
        .collect();
    let apart = |a: (i32, i32), b: (i32, i32)| (a.0 - b.0).abs() + (a.1 - b.1).abs();
    let frames_seen = frames.len().min(places.len());
    // Her middle on the screen each frame.
    let hers: Vec<(i32, i32)> = (0..frames_seen)
        .map(|i| {
            (
                spot.0 + places[i].0 - scrolled[i].0,
                spot.1 + places[i].1 - scrolled[i].1,
            )
        })
        .collect();
    // Which parts are hers on each frame.
    let mine: Vec<Vec<bool>> = (0..frames_seen)
        .map(|k| her_parts(&things[k], hers[k], height))
        .collect();
    // What wasn't her on each frame, those before the press first, for what's new.
    let mut others: Vec<Vec<(i32, i32)>> = before
        .iter()
        .map(|f| {
            let parts: Vec<(i32, i32)> = f.0.iter().map(|p| (p.0 + 4, p.1 + height / 2)).collect();
            let hers = her_parts(&parts, spot, height);
            parts
                .into_iter()
                .zip(hers)
                .filter(|(_, h)| !h)
                .map(|(t, _)| t)
                .collect()
        })
        .collect();
    others.extend((0..frames_seen).map(|k| {
        things[k]
            .iter()
            .zip(&mine[k])
            .filter(|(_, h)| !**h)
            .map(|(t, _)| *t)
            .collect()
    }));
    let mut tracks: Vec<Track> = Vec::new();
    for i in 1..frames_seen {
        let map = |p: (i32, i32)| (p.0 + scrolled[i].0, p.1 + scrolled[i].1);
        let her_on_map = map(hers[i]);
        let now = &things[i];
        let mut taken = mine[i].clone();
        for track in tracks.iter_mut().filter(|t| t.end.is_none()) {
            let ahead = (
                track.last.0 + track.heading.0,
                track.last.1 + track.heading.1,
            );
            let next = now
                .iter()
                .enumerate()
                .filter(|&(j, &t)| {
                    !taken[j] && (t.0 - ahead.0).abs() <= FOLLOW && (t.1 - ahead.1).abs() <= FOLLOW
                })
                .min_by_key(|&(_, &t)| apart(t, ahead));
            match next {
                Some((j, &t)) => {
                    taken[j] = true;
                    track.heading = (t.0 - track.last.0, t.1 - track.last.1);
                    track.last = t;
                    track.missing = 0;
                    let on_map = map(t);
                    track.at.push(on_map);
                    track
                        .from_her
                        .push((on_map.0 - her_on_map.0, on_map.1 - her_on_map.1));
                }
                None => {
                    if track.missing == 0 {
                        let by_thing = now
                            .iter()
                            .zip(&mine[i])
                            .any(|(&t, h)| !h && apart(t, ahead) <= NEXT_TO);
                        let (x, y) = ahead;
                        let off = !(EDGE..=WIDTH - EDGE).contains(&x)
                            || !(EDGE..=HEIGHT - EDGE).contains(&y);
                        track.gone = match () {
                            _ if off => End::OffScreen,
                            _ if by_thing => End::GoneBy,
                            _ => End::Gone,
                        };
                    }
                    track.missing += 1;
                    track.last = ahead;
                    if track.missing >= GONE {
                        track.end = Some(track.gone);
                    }
                }
            }
        }
        for (j, &t) in now.iter().enumerate() {
            // Something other than her was here just before.
            let at = before.len() + i;
            let seen_before = others[at.saturating_sub(FRESH_FRAMES)..at]
                .iter()
                .flatten()
                .any(|&b| apart(t, b) <= FRESH);
            // Next to one of her parts, now or a frame ago.
            let by_her = [i - 1, i].iter().any(|&k| {
                things[k]
                    .iter()
                    .zip(&mine[k])
                    .any(|(&b, &h)| h && apart(t, b) <= SPAWN)
            });
            if taken[j] || seen_before || !by_her {
                continue;
            }
            let on_map = map(t);
            tracks.push(Track {
                at: vec![on_map],
                from_her: vec![(on_map.0 - her_on_map.0, on_map.1 - her_on_map.1)],
                last: t,
                heading: (0, 0),
                missing: 0,
                gone: End::Gone,
                end: None,
            });
        }
    }
    // A thing drawn in parts came out as a track for each: one is enough.
    let mut kept: Vec<Track> = Vec::new();
    for track in tracks {
        let same = kept.iter().any(|k| {
            k.at.len() == track.at.len()
                && (k.at[0].0 - track.at[0].0).abs() <= TOGETHER
                && (k.at[0].1 - track.at[0].1).abs() <= TOGETHER
        });
        if !same {
            kept.push(track);
        }
    }
    kept.into_iter()
        // It moved away from her, and went somewhere itself: something standing still
        // that she walked away from wasn't sent.
        .filter(|t| {
            let moved = |all: &[(i32, i32)]| {
                let first = all[0];
                all.iter()
                    .any(|p| (p.0 - first.0).abs().max((p.1 - first.1).abs()) >= AWAY)
            };
            moved(&t.from_her) && moved(&t.at)
        })
        .take(MOST)
        .map(|t| {
            let start = t.at[0];
            let from_start = |p: &(i32, i32)| (p.0 - start.0, p.1 - start.1);
            let went =
                t.at.iter()
                    .map(from_start)
                    .max_by_key(|p| p.0.abs() + p.1.abs())
                    .unwrap_or((0, 0));
            let last = t.at.last().map(from_start).unwrap_or((0, 0));
            Sent {
                press: press.to_string(),
                went,
                back: last.0.abs() + last.1.abs() + step <= went.0.abs() + went.1.abs(),
                end: t.end.unwrap_or(End::Going),
                step,
            }
        })
        .collect()
}

/// Which of the parts (by their middles) are hers: those at her middle, and those that
/// touch them, and those that touch those (a tall hero is drawn in rows of parts). A shot
/// coming out of her is hers until it's clear of her.
fn her_parts(parts: &[(i32, i32)], her: (i32, i32), height: i32) -> Vec<bool> {
    let mut mine: Vec<bool> = parts
        .iter()
        .map(|&t| (t.0 - her.0).abs() + (t.1 - her.1).abs() <= HERS)
        .collect();
    loop {
        let mut more = false;
        for i in 0..parts.len() {
            if mine[i] {
                continue;
            }
            let touches = parts.iter().zip(&mine).any(|(&b, &m)| {
                m && (b.0 - parts[i].0).abs() <= TOGETHER && (b.1 - parts[i].1).abs() <= height
            });
            if touches {
                mine[i] = true;
                more = true;
            }
        }
        if !more {
            return mine;
        }
    }
}

/// Keeps `sent` as the latest of its press, and only the latest few presses.
pub fn remember(all: &mut Vec<Sent>, sent: Sent) {
    all.retain(|s| s.press != sent.press);
    all.push(sent);
    let extra = all.len().saturating_sub(KEPT);
    all.drain(..extra);
}

/// The things sent as lines for a model, or nothing when there are none.
pub fn list(all: &[Sent]) -> String {
    if all.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = all
        .iter()
        .map(|s| format!("- {}: {}", s.press, s.words()))
        .collect();
    format!(
        "What your presses sent out (the last of each, in steps from where it came out):\n{}",
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Her two 8×16 parts with her middle at `(x, y)`.
    fn her(x: i32, y: i32) -> Vec<(i32, i32)> {
        vec![(x - 8, y - 8), (x, y - 8)]
    }

    /// A one-part thing with its middle at `(x, y)`.
    fn thing(x: i32, y: i32) -> (i32, i32) {
        (x - 4, y - 8)
    }

    fn frames(all: &[Vec<(i32, i32)>], scroll: impl Fn(usize) -> u8) -> Vec<Frame<'_>> {
        all.iter()
            .enumerate()
            .map(|(i, f)| (f.as_slice(), (scroll(i), 0)))
            .collect()
    }

    #[test]
    fn a_shot_is_followed_until_it_hits_something() {
        // She stands at (80, 100); an enemy waits at (130, 100). From frame 3 a shot comes
        // out of her right side and goes 6 pixels a frame until it reaches the enemy.
        let mut all = Vec::new();
        for i in 0..20 {
            let mut f = her(80, 100);
            f.push(thing(130, 100));
            if (3..10).contains(&i) {
                f.push(thing(96 + 6 * (i - 3), 100));
            }
            all.push(f);
        }
        let places = vec![(0, 0); 20];
        let found = sent("b", &[], &frames(&all, |_| 0), (80, 100), &places, 16, 16);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].went, (36, 0));
        assert_eq!(found[0].end, End::GoneBy);
        assert_eq!(
            found[0].words(),
            "2.5 right, and was gone next to something else"
        );
    }

    #[test]
    fn a_shot_can_go_off_the_screen() {
        let mut all = Vec::new();
        for i in 0..12 {
            let mut f = her(30, 100);
            if (2..7).contains(&i) {
                f.push(thing(14 - 4 * (i - 2), 100));
            }
            all.push(f);
        }
        let places = vec![(0, 0); 12];
        let found = sent("b", &[], &frames(&all, |_| 0), (30, 100), &places, 16, 16);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].words(), "1 left, and went off the screen");
    }

    #[test]
    fn a_boomerang_comes_back_while_the_screen_scrolls() {
        // She walks right with the screen scrolling under her (her sprite stays put); a
        // thing goes out 40 pixels ahead of her and comes back into her.
        let mut all = Vec::new();
        let mut places = Vec::new();
        for i in 0..33 {
            let mut f = her(80, 100);
            let out = if i < 15 {
                16 + 3 * i
            } else {
                16 + 3 * (30 - i) - 3
            };
            if (1..28).contains(&i) {
                f.push(thing(80 + out, 100));
            }
            all.push(f);
            places.push((i, 0));
        }
        let found = sent(
            "b",
            &[],
            &frames(&all, |i| i as u8),
            (80, 100),
            &places,
            16,
            16,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].back, "{found:?}");
        assert_eq!(found[0].end, End::Gone);
    }

    #[test]
    fn her_own_parts_and_things_already_there_are_not_sent() {
        // A sword held out moves with her; an enemy flickering beside her was there before;
        // something that shows up behind her stands still as she walks away from it.
        let mut all = Vec::new();
        let mut places = Vec::new();
        for i in 0..20 {
            let x = 80 + 2 * i;
            let mut f = her(x, 100);
            f.push(thing(x + 14, 100));
            if i % 3 != 0 {
                f.push(thing(110, 100));
            }
            if i >= 4 {
                f.push(thing(70, 100));
            }
            all.push(f);
            places.push((2 * i, 0));
        }
        let found = sent("a", &[], &frames(&all, |_| 0), (80, 100), &places, 16, 16);
        assert_eq!(found, Vec::new());
    }

    #[test]
    fn keeps_the_latest_of_each_press() {
        let one = |press: &str, x| Sent {
            press: press.to_string(),
            went: (x, 0),
            back: false,
            end: End::Going,
            step: 16,
        };
        let mut all = Vec::new();
        remember(&mut all, one("b", 16));
        remember(&mut all, one("b", 96));
        assert_eq!(all, vec![one("b", 96)]);
        assert_eq!(
            list(&all),
            "What your presses sent out (the last of each, in steps from where it came \
             out):\n- b: 6 right, and was still going"
        );
    }
}
