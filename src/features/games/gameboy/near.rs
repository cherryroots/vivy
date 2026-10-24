//! What's around her that moves: the other sprites near her on the screen, by steps from
//! her, and how each went over the last moment. Pure, from the sprites of two frames
//! (`Machine::sprites_since`), so it reads any game the same way.
//!
//! A thing is drawn in parts (two 8-pixel sprites side by side, or four in a game with
//! small sprites), so parts that touch are one thing. Which way it went is how it moved
//! from her: Red's people stand still on the screen as she walks past them, but they come
//! closer all the same.

/// Frames back to see which way things go: a fifth of a second.
pub const LOOK_BACK: u64 = 12;
/// Steps around her that count as near.
pub const NEAR: i32 = 4;
/// The most things said, nearest first.
const MOST: usize = 5;
/// Pixels in a step.
const STEP: i32 = 16;
/// Parts this close to her middle are hers (her shield held out, a sword swung).
const HERS: i32 = 14;
/// Parts this close are one thing.
const TOUCHING: i32 = 9;
/// Pixels a thing can go in `LOOK_BACK` frames and still be followed.
const FOLLOW: i32 = 16;
/// Pixels it must go to count as moving.
const MOVED: i32 = 2;

/// Something near her.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thing {
    /// Steps from her: x to the right, y down.
    pub at: (i32, i32),
    /// Pixels it went from her since `LOOK_BACK` frames ago (x right, y down); nothing for
    /// a thing standing still, or one that just showed up.
    pub went: (i32, i32),
    /// Pixels from her now and then, side plus down.
    pub now: i32,
    pub then: i32,
}

impl Thing {
    /// "2 right 1 up, moving left, coming closer".
    pub fn words(&self) -> String {
        let mut place = Vec::new();
        match self.at.0 {
            0 => {}
            x if x > 0 => place.push(format!("{x} right")),
            x => place.push(format!("{} left", -x)),
        }
        match self.at.1 {
            0 => {}
            y if y > 0 => place.push(format!("{y} down")),
            y => place.push(format!("{} up", -y)),
        }
        if place.is_empty() {
            place.push("on you".to_string());
        }
        let place = place.join(" ");
        if !self.moving() {
            return format!("{place}, still");
        }
        let (x, y) = self.went;
        let way = if x.abs() >= y.abs() {
            if x > 0 { "right" } else { "left" }
        } else if y > 0 {
            "down"
        } else {
            "up"
        };
        let how = if self.now < self.then - MOVED {
            "coming closer"
        } else if self.now > self.then + MOVED {
            "going away"
        } else {
            "going past"
        };
        format!("{place}, moving {way}, {how}")
    }

    pub fn moving(&self) -> bool {
        self.went.0.abs().max(self.went.1.abs()) >= MOVED
    }
}

/// The things near her now, nearest first: `then` and `now` are every sprite's top left
/// on two frames `LOOK_BACK` apart, `her` the middle of her sprite now, `height` the
/// sprites' height.
pub fn things(then: &[(i32, i32)], now: &[(i32, i32)], her: (i32, i32), height: i32) -> Vec<Thing> {
    let (then, now) = (middles(then, height), middles(now, height));
    let near = |p: &(i32, i32), at: (i32, i32)| (p.0 - at.0).abs() + (p.1 - at.1).abs();
    // Where she was then: her sprite nearest where she is now, if one is close.
    let her_then = then
        .iter()
        .copied()
        .filter(|p| near(p, her) <= FOLLOW)
        .min_by_key(|p| near(p, her))
        .unwrap_or(her);
    let mut found: Vec<Thing> = now
        .iter()
        .filter(|p| (p.0 - her.0).abs() > HERS || (p.1 - her.1).abs() > HERS)
        .map(|&p| {
            let from_her = (p.0 - her.0, p.1 - her.1);
            let was = then
                .iter()
                .filter(|q| near(q, her_then) > HERS)
                .filter(|q| (q.0 - p.0).abs() <= FOLLOW && (q.1 - p.1).abs() <= FOLLOW)
                .min_by_key(|q| near(q, p))
                .map(|q| (q.0 - her_then.0, q.1 - her_then.1));
            let was = was.unwrap_or(from_her);
            Thing {
                at: (steps(from_her.0), steps(from_her.1)),
                went: (from_her.0 - was.0, from_her.1 - was.1),
                now: from_her.0.abs() + from_her.1.abs(),
                then: was.0.abs() + was.1.abs(),
            }
        })
        .filter(|t| t.at.0.abs() <= NEAR && t.at.1.abs() <= NEAR)
        .collect();
    found.sort_by_key(|t| t.now);
    found.truncate(MOST);
    found
}

/// Pixels as whole steps, the nearest.
fn steps(pixels: i32) -> i32 {
    (pixels + pixels.signum() * STEP / 2) / STEP
}

/// The middle of each thing: parts that touch, and parts that touch those, are one.
fn middles(parts: &[(i32, i32)], height: i32) -> Vec<(i32, i32)> {
    let mut group: Vec<usize> = (0..parts.len()).collect();
    fn root(group: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while group[i] != i {
            group[i] = group[group[i]];
            i = group[i];
        }
        i
    }
    for i in 0..parts.len() {
        for j in i + 1..parts.len() {
            let (a, b) = (parts[i], parts[j]);
            if (a.0 - b.0).abs() <= TOUCHING && (a.1 - b.1).abs() <= height.max(TOUCHING) {
                let (ri, rj) = (root(&mut group, i), root(&mut group, j));
                group[ri] = rj;
            }
        }
    }
    let mut sums: std::collections::BTreeMap<usize, (i32, i32, i32)> = Default::default();
    for (i, &(x, y)) in parts.iter().enumerate() {
        let sum = sums.entry(root(&mut group, i)).or_default();
        *sum = (sum.0 + x, sum.1 + y, sum.2 + 1);
    }
    sums.values()
        .map(|&(x, y, n)| (x / n + 4, y / n + height / 2))
        .collect()
}

/// The things as one line for a model: "Near you (steps from you): 2 right, moving left,
/// coming closer; 3 up, still." Empty when nothing is near.
pub fn line(things: &[Thing]) -> String {
    if things.is_empty() {
        return String::new();
    }
    let all: Vec<String> = things.iter().map(Thing::words).collect();
    format!("Near you (steps from you): {}.", all.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thing drawn as two 8×16 sprites side by side, its middle at `(x, y)`.
    fn thing(x: i32, y: i32) -> [(i32, i32); 2] {
        [(x - 8, y - 8), (x, y - 8)]
    }

    #[test]
    fn says_where_things_are_and_how_they_go() {
        let her = (80, 72);
        let mut then = Vec::new();
        let mut now = Vec::new();
        now.extend(thing(80, 72)); // her
        then.extend(thing(80, 72));
        // Two steps right, coming at her.
        then.extend(thing(120, 72));
        now.extend(thing(112, 72));
        // Three steps up, standing still.
        then.extend(thing(80, 24));
        now.extend(thing(80, 24));
        // Far away: not near.
        now.extend(thing(0, 140));
        let found = things(&then, &now, her, 16);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].words(), "2 right, moving left, coming closer");
        assert_eq!(found[1].words(), "3 up, still");
        assert_eq!(
            line(&found),
            "Near you (steps from you): 2 right, moving left, coming closer; 3 up, still."
        );
    }

    #[test]
    fn things_are_seen_from_her_as_she_walks() {
        // She walks right across a still screen; a person standing still comes closer.
        let then = [thing(60, 72), thing(120, 72)].concat();
        let now = [thing(70, 72), thing(120, 72)].concat();
        let found = things(&then, &now, (70, 72), 16);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].at, (3, 0));
        assert_eq!(found[0].words(), "3 right, moving left, coming closer");
        // Her own parts held out (a shield) aren't something near her.
        let now = [(66, 64), (74, 64), (84, 66)];
        assert!(things(&now, &now, (74, 72), 16).is_empty());
    }

    #[test]
    fn steps_round_to_the_nearest() {
        assert_eq!(steps(7), 0);
        assert_eq!(steps(8), 1);
        assert_eq!(steps(-24), -2);
        assert_eq!(steps(-23), -1);
    }
}
