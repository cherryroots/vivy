//! Taking one step and seeing what happened: moved, turned, blocked, or somewhere new.
//! Every step goes in the movement log, so walls, ledges and one-way doors show up in any
//! game without anyone describing them.
//!
//! Most games that walk on a grid scroll the screen under a player who stays in the
//! middle, so the step's result is in the scroll: 16 pixels (in most games) is one step,
//! 32 is a jump down a ledge, none is blocked. Others keep the screen still and move the
//! player's sprite across it (Link's Awakening, Harvest Moon), so how far she went is the
//! scroll plus how far the sprites that moved her way went. A changed player sprite with no
//! scroll is a turn. A dark spell (`change.rs`) means a door or stairs. In Pokémon Red, memory says it
//! too, and the two are compared (see `red::Trust`).

use super::change::{Look, Transitions, look};
use super::console::Machine;
use super::emulator::Button;
use super::screen::{Screen, Sprite};

/// Pixels in one step of a grid game. Pokémon and most games like it use 16.
pub const STEP_PIXELS: i32 = 16;

/// How long a direction is held at most, waiting for the step to start. A turn in Red
/// takes a few frames before the step begins.
const HOLD_MOST: u32 = 24;

/// How long to wait for a step (or a door) to finish after letting go.
const SETTLE_MOST: u32 = 120;

/// Pixels a sprite can move in a frame and still be followed.
const FOLLOW_MOST: i32 = 4;

/// Pixels from where she was that her sprites' middle can be, once it's known.
const HER_NEAR: i32 = 12;

/// Frames held again with nothing moving before a step that stopped short is given up.
const STUCK_FRAMES: u32 = 6;

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
    /// How far her sprite moved across the screen, in pixels (none in games that scroll
    /// under her).
    pub walked: (i32, i32),
    /// Where her sprite ended up on the screen (the middle of the sprites that moved her
    /// way), if any did.
    pub her: Option<(i32, i32)>,
    /// Frames it took, from pressing to settled.
    pub frames: u32,
}

impl Step {
    /// How far she went, in pixels: the scroll and her sprite's own move.
    pub fn moved(&self) -> (i32, i32) {
        (
            self.scrolled.0 + self.walked.0,
            self.scrolled.1 + self.walked.1,
        )
    }
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

/// The screen pixel the middle of the player's sprite is nearest, in games that scroll
/// under her.
pub const MIDDLE: (i32, i32) = (72, 68);

/// Whether a sprite stands in the middle of the screen.
fn in_the_middle(screen: &Screen) -> bool {
    screen
        .sprites
        .iter()
        .any(|s| (s.x + 4 - MIDDLE.0).abs() + (s.y + s.height / 2 - MIDDLE.1).abs() <= 12)
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

/// The sprites that moved the way she was pressed from one frame to the next: where they
/// are now (the middle of them all) and how far they moved: the move most of them made,
/// nearest `near` (where she was). With `only_near`, sprites anywhere else never count (a
/// game that scrolls under her: the sprite in the middle is her). Each sprite
/// is matched with the nearest one a frame before (games shuffle the sprite list, so the
/// list's order says nothing). On a screen that scrolls under her, her sprite stays put and
/// the others move the other way, so none count.
fn follow(
    before: &Screen,
    after: &Screen,
    along: (i32, i32),
    near: Option<(i32, i32)>,
    only_near: bool,
) -> Option<((i32, i32), (i32, i32))> {
    let mut moved: Vec<(&Sprite, (i32, i32))> = Vec::new();
    for s in &after.sprites {
        let Some(d) = before
            .sprites
            .iter()
            .map(|b| (s.x - b.x, s.y - b.y))
            .filter(|d| d.0.abs() <= FOLLOW_MOST && d.1.abs() <= FOLLOW_MOST)
            .min_by_key(|d| d.0.abs() + d.1.abs())
        else {
            continue;
        };
        // Her way, and hardly across it (a walk can bob a pixel).
        let ahead = d.0 * along.0 + d.1 * along.1;
        let across = (d.0 * along.1).abs() + (d.1 * along.0).abs();
        if ahead > 0 && across <= 1 {
            moved.push((s, d));
        }
    }
    // The move most of them made, then the group nearest where she was. Once she's known,
    // the nearest first: a fox walking beside her is as many sprites as she is.
    // Ranked by: near where she was, how near once known, how many sprites, how near.
    type Ranked = ((bool, i32, usize, i32), (i32, i32), (i32, i32));
    let mut best: Option<Ranked> = None;
    let mut moves: Vec<(i32, i32)> = moved.iter().map(|m| m.1).collect();
    moves.sort_unstable();
    moves.dedup();
    for d in moves {
        let group: Vec<_> = moved.iter().filter(|m| m.1 == d).map(|m| *m.0).collect();
        // Her whole sprite: parts drawn the same can match a frame back where they were.
        let Some(middle) = middle_of(&touching(&group, &after.sprites)) else {
            continue;
        };
        let far = near.map_or(0, |n| (middle.0 - n.0).abs() + (middle.1 - n.1).abs());
        if only_near && far > HER_NEAR {
            continue;
        }
        // Once she's known, where she is moves by what her sprites did: someone passing
        // close by would pull the middle of everything touching her off her.
        let middle = match near {
            Some(n) if only_near => (n.0 + d.0, n.1 + d.1),
            _ => middle,
        };
        let nearest = if only_near { -far } else { 0 };
        let this = ((far <= HER_NEAR, nearest, group.len(), -far), middle, d);
        if best.is_none_or(|b| this.0 > b.0) {
            best = Some(this);
        }
    }
    best.map(|(_, middle, d)| (middle, d))
}

/// Frames each way is held while finding which sprite is hers, and waited after.
const PROBE_HOLD: u32 = 10;
const PROBE_WAIT: u32 = 6;

/// Which sprite is hers on a screen that stays still under her, found the way a cheat
/// finder narrows down bytes: each way is pressed in turn (right, left, down, up, so she
/// ends about where she began), every sprite is followed frame by frame, and only those
/// that went the way pressed whenever they moved, and moved at least twice, are kept.
/// Children and animals wandering about go some other way sooner or later. Where her
/// sprites are, or `None` if no one sprite (with those touching it) is left, or the
/// screen scrolled.
pub fn which_is_her(machine: &mut Machine) -> Option<(i32, i32)> {
    let mut screen = Screen::read(machine.emulator.video());
    let scroll = (screen.registers.scx, screen.registers.scy);
    // Where each sprite is now, how many presses it followed, and whether it went
    // another way.
    let mut tracks: Vec<((i32, i32), u32, bool)> = screen
        .sprites
        .iter()
        .map(|s| ((s.x, s.y), 0, false))
        .collect();
    for direction in [Button::Right, Button::Left, Button::Down, Button::Up] {
        let along = offset(direction).unwrap_or((0, 0));
        let from: Vec<(i32, i32)> = tracks.iter().map(|t| t.0).collect();
        machine.emulator.hold(&[direction]);
        for frame in 0..PROBE_HOLD + PROBE_WAIT {
            if frame == PROBE_HOLD {
                machine.emulator.release();
            }
            machine.frame();
            let next = Screen::read(machine.emulator.video());
            if !next.drawn || (next.registers.scx, next.registers.scy) != scroll {
                machine.emulator.release();
                return None;
            }
            for track in &mut tracks {
                // The nearest sprite a frame on. One that's gone for a frame (games
                // flicker sprites when there are many) is looked for again next frame.
                let at = track.0;
                if let Some(now) = next
                    .sprites
                    .iter()
                    .map(|s| (s.x, s.y))
                    .filter(|p| {
                        (p.0 - at.0).abs() <= FOLLOW_MOST && (p.1 - at.1).abs() <= FOLLOW_MOST
                    })
                    .min_by_key(|p| (p.0 - at.0).abs() + (p.1 - at.1).abs())
                {
                    track.0 = now;
                }
            }
            screen = next;
        }
        for (track, from) in tracks.iter_mut().zip(&from) {
            let d = (track.0.0 - from.0, track.0.1 - from.1);
            let ahead = d.0 * along.0 + d.1 * along.1;
            let across = (d.0 * along.1).abs() + (d.1 * along.0).abs();
            if ahead >= 4 && across <= 2 {
                track.1 += 1;
            } else if d.0.abs() + d.1.abs() > 1 {
                track.2 = true;
            }
        }
    }
    let kept: Vec<Sprite> = screen
        .sprites
        .iter()
        .filter(|s| tracks.iter().any(|t| t.1 >= 2 && !t.2 && t.0 == (s.x, s.y)))
        .copied()
        .collect();
    let her = touching(&kept[..1.min(kept.len())], &screen.sprites);
    // Every sprite kept must be part of her: two that did as she was pressed are too many
    // to tell apart.
    if kept.iter().any(|k| !her.contains(k)) {
        return None;
    }
    middle_of(&her)
}

/// Her sprites, each followed from frame to frame to the nearest sprite a frame on: once
/// she's known, the middle of everything moving near her would drift onto whoever walks
/// beside her (a fox in Link's Awakening's village).
struct Parts(Vec<Sprite>);

impl Parts {
    /// The sprites that make her up: those in a 16-pixel square around `at` (the halves or
    /// quarters of one person), or else the one nearest. Not everything touching her: a
    /// child standing right beside her is somebody else.
    fn of(screen: &Screen, at: (i32, i32)) -> Parts {
        let off = |s: &Sprite| ((s.x + 4 - at.0).abs(), (s.y + s.height / 2 - at.1).abs());
        let mut parts: Vec<Sprite> = screen
            .sprites
            .iter()
            .filter(|s| off(s).0 <= 5 && off(s).1 <= 5)
            .copied()
            .collect();
        if parts.is_empty() {
            parts.extend(
                screen
                    .sprites
                    .iter()
                    .min_by_key(|s| off(s).0 + off(s).1)
                    .copied(),
            );
        }
        Parts(parts)
    }

    /// Follows them a frame on: the move most of them made, if it was her way, and where
    /// they are now (their middle, so a walk's bob doesn't add up).
    fn follow(&mut self, after: &Screen, along: (i32, i32)) -> Option<((i32, i32), (i32, i32))> {
        let mut moves: Vec<(i32, i32)> = Vec::new();
        for part in &mut self.0 {
            let at = (part.x, part.y);
            let Some(now) = after
                .sprites
                .iter()
                .map(|s| (s.x, s.y))
                .filter(|p| (p.0 - at.0).abs() <= FOLLOW_MOST && (p.1 - at.1).abs() <= FOLLOW_MOST)
                .min_by_key(|p| (p.0 - at.0).abs() + (p.1 - at.1).abs())
            else {
                continue;
            };
            moves.push((now.0 - at.0, now.1 - at.1));
            (part.x, part.y) = now;
        }
        let d = *moves
            .iter()
            .max_by_key(|d| moves.iter().filter(|m| m == d).count())?;
        let ahead = d.0 * along.0 + d.1 * along.1;
        let across = (d.0 * along.1).abs() + (d.1 * along.0).abs();
        (ahead > 0 && across <= 1).then_some((self.middle()?, d))
    }

    /// Where she is: the middle of her sprites.
    fn middle(&self) -> Option<(i32, i32)> {
        middle_of(&self.0)
    }
}

/// Follows her sprites from `spot` through every frame since a look
/// (`Machine::sprites_since`), the way a step follows them: where her sprite is on `now`'s
/// screen after her own presses (or the game) moved it further than a look can match.
/// `None` when a frame had none of them near where they were.
pub fn followed(frames: &[&[(i32, i32)]], spot: (i32, i32), now: &Screen) -> Option<(i32, i32)> {
    let (first, rest) = frames.split_first()?;
    let height = now.sprites.first().map_or(8, |s| s.height);
    let off = |p: &(i32, i32)| ((p.0 + 4 - spot.0).abs(), (p.1 + height / 2 - spot.1).abs());
    let mut parts: Vec<(i32, i32)> = first
        .iter()
        .filter(|p| off(p).0 <= 5 && off(p).1 <= 5)
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
    for frame in rest {
        let mut kept = 0;
        for part in &mut parts {
            // A part may flicker out for a frame; it's picked up again near where it was.
            if let Some(&next) = frame
                .iter()
                .filter(|q| {
                    (q.0 - part.0).abs() <= FOLLOW_MOST && (q.1 - part.1).abs() <= FOLLOW_MOST
                })
                .min_by_key(|q| (q.0 - part.0).abs() + (q.1 - part.1).abs())
            {
                *part = next;
                kept += 1;
            }
        }
        if kept == 0 {
            return None;
        }
    }
    let hers: Vec<Sprite> = now
        .sprites
        .iter()
        .filter(|s| parts.contains(&(s.x, s.y)))
        .copied()
        .collect();
    middle_of(&hers)
}

/// The sprites in `group` and every one that touches them, and those that touch those: a
/// whole person drawn in parts.
pub fn touching(group: &[Sprite], all: &[Sprite]) -> Vec<Sprite> {
    let mut whole: Vec<Sprite> = group.to_vec();
    loop {
        let more: Vec<Sprite> = all
            .iter()
            .filter(|s| !whole.contains(s))
            .filter(|s| {
                whole.iter().any(|w| {
                    // Side by side on one row, or one 8-pixel row on another. Sprites that
                    // only overlap are two people: a child standing just above her in
                    // Link's Awakening was taken as part of her.
                    let near = s.x <= w.x + 8 && w.x <= s.x + 8;
                    let beside = (s.y - w.y).abs() <= 2;
                    let stacked =
                        s.height == 8 && w.height == 8 && (s.y == w.y + 8 || w.y == s.y + 8);
                    near && (beside || stacked)
                })
            })
            .copied()
            .collect();
        if more.is_empty() {
            return whole;
        }
        whole.extend(more);
    }
}

/// The middle of the box around these sprites.
pub fn middle_of(sprites: &[Sprite]) -> Option<(i32, i32)> {
    let left = sprites.iter().map(|s| s.x).min()?;
    let right = sprites.iter().map(|s| s.x + 8).max()?;
    let top = sprites.iter().map(|s| s.y).min()?;
    let bottom = sprites.iter().map(|s| s.y + s.height).max()?;
    Some(((left + right) / 2, (top + bottom) / 2))
}

/// Scroll difference between two register values, allowing for the wrap at 256.
fn scroll_delta(before: u8, after: u8) -> i32 {
    i32::from(after.wrapping_sub(before) as i8)
}

/// Takes one step in `direction`: holds it until she starts to move (or long enough that
/// she won't), lets go, and waits until the screen is still again. Games that only move
/// her while the button is down stop short when it's let go: it's held again until she's
/// gone a whole step, or stops.
///
/// `her` is where her sprite is on the screen, if the look knows; otherwise a sprite in the
/// middle is her, and with none there the sprites that move her way are. Once known, only
/// sprites there count (a dog walking the same way isn't her).
pub fn step(machine: &mut Machine, direction: Button, her: Option<(i32, i32)>) -> Step {
    let along = offset(direction).unwrap_or((0, 0));
    let walks_across = her.is_some();
    let mut screen = Screen::read(machine.emulator.video());
    let start_look = player_look(&screen);
    let mut scrolled = (0, 0);
    let mut walked = (0, 0);
    // She's the sprite in the middle of a screen that scrolls under her (Red), unless the
    // look knows better.
    let middle = !walks_across && in_the_middle(&screen);
    let mut her = her.or_else(|| middle.then_some(MIDDLE));
    let only_near = her.is_some();
    // Her own sprites, followed one by one, once she's known on a screen she walks across.
    let mut parts = her
        .filter(|_| walks_across)
        .map(|at| Parts::of(&screen, at));
    let start_at = parts.as_ref().and_then(Parts::middle);
    let mut transitions = Transitions::default();
    let mut new_area = false;
    let mut frames = 0;
    let mut still = 0;
    // Held again after stopping short, and frames since she last moved while held. She
    // goes on a little after letting go (`coast`, from the first time), so it's let go
    // that much early.
    let mut again = false;
    let mut stuck = 0;
    let mut let_go_at = 0;
    let mut coast = 0;
    // Held again until she stopped by herself: pressed against a wall.
    let mut against_wall = false;

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
        let picture = frame_look == Look::Picture && next.drawn && screen.drawn;
        let scroll = if picture {
            (
                scroll_delta(screen.registers.scx, next.registers.scx),
                scroll_delta(screen.registers.scy, next.registers.scy),
            )
        } else {
            (0, 0)
        };
        // Her sprite's own move, on a screen that didn't scroll a whole screen at once.
        let mut sprite = (0, 0);
        if picture
            && scroll.0.abs() < 8
            && scroll.1.abs() < 8
            && let Some((now, d)) = match (&mut parts, her) {
                (Some(parts), Some(_)) => parts.follow(&next, along),
                _ => follow(&screen, &next, along, her, only_near),
            }
        {
            her = Some(now);
            // From the first sprite that moves her way on, those sprites are followed and
            // no others: a fox hopping past isn't her.
            if parts.is_none() && !middle {
                parts = Some(Parts::of(&next, now));
            }
            // Only along her way: a walk can bob a pixel across it.
            let ahead = d.0 * along.0 + d.1 * along.1;
            sprite = (ahead * along.0, ahead * along.1);
        }
        // A text box or menu in the window doesn't scroll anything, and the background
        // under it keeps its scroll, so the scroll registers are safe to follow.
        scrolled.0 += scroll.0;
        scrolled.1 += scroll.1;
        walked.0 += sprite.0;
        walked.1 += sprite.1;
        let moved = (scroll.0 + sprite.0, scroll.1 + sprite.1);
        let pixels = (scrolled.0 + walked.0)
            .abs()
            .max((scrolled.1 + walked.1).abs());
        if frame_look == Look::Picture {
            screen = next;
        }
        if holding && again {
            stuck = if moved == (0, 0) { stuck + 1 } else { 0 };
            if pixels + coast >= STEP_PIXELS || stuck >= STUCK_FRAMES || transitions.dark() {
                machine.emulator.release();
                holding = false;
                against_wall = stuck >= STUCK_FRAMES;
            }
        } else if holding && (moved != (0, 0) || transitions.dark() || frames >= HOLD_MOST) {
            machine.emulator.release();
            holding = false;
            let_go_at = pixels;
        }
        if !holding {
            still = if moved == (0, 0) && !transitions.dark() {
                still + 1
            } else {
                0
            };
            // Stopped short of a step: she only moves while it's held.
            if still >= 2 && !again && !new_area && pixels > 0 && pixels < STEP_PIXELS - 2 {
                again = true;
                coast = pixels - let_go_at;
                still = 0;
                machine.emulator.hold(&[direction]);
                holding = true;
                continue;
            }
            // Settled: a few still frames after the scroll stopped.
            if still >= 4 && (pixels > 0 || new_area || frames >= HOLD_MOST + 4) {
                break;
            }
        }
    }
    machine.emulator.release();
    // Her own sprites were followed whichever way they went: a screen sliding on to the
    // next room carries her across it the other way.
    if let Some(at) = parts.as_ref().and_then(Parts::middle) {
        her = Some(at);
        // How far she went across the screen is where her sprites are now, whichever
        // way they moved: during a slide she goes against the press, nearly as far as
        // the screen scrolls, and she has only walked the difference.
        if let Some(from) = start_at
            && scrolled != (0, 0)
        {
            let ahead = (at.0 - from.0) * along.0 + (at.1 - from.1) * along.1;
            walked = (ahead * along.0, ahead * along.1);
        }
    }
    // On a screen that scrolled under her, her sprite only bobs (a hop down a ledge).
    if middle && scrolled != (0, 0) {
        walked = (0, 0);
    }
    // A sprite that walks across the screen can stop a little short of a step, or go a
    // little past it: it's walked on to the end of the step, or back to it. One pressed
    // against a wall part way stays there: walls sit on the game's own grid, so standing
    // against one lines her steps up with it. Walking her back would turn her round, and
    // never lands exactly where she started.
    let gone = (scrolled.0 + walked.0) * along.0 + (scrolled.1 + walked.1) * along.1;
    let part = gone.rem_euclid(STEP_PIXELS);
    let rest = if part < STEP_PIXELS / 2 {
        -part
    } else {
        STEP_PIXELS - part
    };
    if let Some(spot) = her
        .filter(|_| !new_area && !against_wall && rest != 0 && (walks_across || walked != (0, 0)))
    {
        let (more, rolled, now) = finish_step(machine, &mut screen, direction, spot, coast, rest);
        walked.0 += more.0;
        walked.1 += more.1;
        scrolled.0 += rolled.0;
        scrolled.1 += rolled.1;
        her = Some(now);
    }

    let pixels = (scrolled.0 + walked.0)
        .abs()
        .max((scrolled.1 + walked.1).abs());
    let outcome = if new_area {
        Outcome::NewArea
    } else if pixels >= STEP_PIXELS / 2 {
        Outcome::Moved((pixels + STEP_PIXELS / 2) / STEP_PIXELS)
    } else if against_wall {
        Outcome::Blocked
    } else if player_look(&screen) != start_look || pixels > 0 {
        Outcome::Turned
    } else {
        Outcome::Blocked
    };
    Step {
        direction,
        outcome,
        scrolled,
        walked,
        // Her spot is only news when she walked across the screen.
        her: her.filter(|_| !middle || walked != (0, 0)),
        frames,
    }
}

/// Walks her `rest` pixels along `direction` (back the other way when it's less than
/// 0), counting the scroll as she goes: how far her sprite went, how far the screen
/// scrolled, and where her sprite is.
fn finish_step(
    machine: &mut Machine,
    screen: &mut Screen,
    direction: Button,
    spot: (i32, i32),
    coast: i32,
    rest: i32,
) -> ((i32, i32), (i32, i32), (i32, i32)) {
    let along = offset(direction).unwrap_or((0, 0));
    let (button, way) = if rest < 0 {
        (opposite(direction), (-along.0, -along.1))
    } else {
        (direction, along)
    };
    let wanted = rest.abs();
    let (mut walked, mut scrolled, mut her) = ((0, 0), (0, 0), spot);
    let mut parts = Parts::of(screen, spot);
    let (mut gone, mut stuck, mut still) = (0, 0, 0);
    machine.emulator.hold(&[button]);
    let mut holding = true;
    for _ in 0..SETTLE_MOST {
        machine.frame();
        let next = Screen::read(machine.emulator.video());
        let scroll = (
            scroll_delta(screen.registers.scx, next.registers.scx),
            scroll_delta(screen.registers.scy, next.registers.scy),
        );
        let step = parts.follow(&next, way);
        *screen = next;
        scrolled.0 += scroll.0;
        scrolled.1 += scroll.1;
        let mut moved = scroll.0 * way.0 + scroll.1 * way.1;
        if let Some((now, d)) = step {
            her = now;
            let ahead = d.0 * way.0 + d.1 * way.1;
            walked.0 += ahead * way.0;
            walked.1 += ahead * way.1;
            moved += ahead;
        }
        gone += moved;
        if moved != 0 {
            (stuck, still) = (0, 0);
        } else {
            stuck += 1;
            still += 1;
        }
        if holding && (gone + coast.min(wanted - 1) >= wanted || stuck >= STUCK_FRAMES) {
            machine.emulator.release();
            holding = false;
            still = 0;
        }
        if !holding && still >= 4 {
            break;
        }
    }
    machine.emulator.release();
    (walked, scrolled, her)
}

/// The direction the other way.
fn opposite(direction: Button) -> Button {
    match direction {
        Button::Up => Button::Down,
        Button::Down => Button::Up,
        Button::Left => Button::Right,
        Button::Right => Button::Left,
        other => other,
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
        // The test ROM scrolls a pixel a frame only while Right is held, so a step that
        // lets go as soon as the scroll starts stops short, and Right is held again (the
        // test ROM's picture runs out after a few more pixels, so it isn't a whole step).
        let right = step(&mut m, Button::Right, None);
        assert!(right.scrolled.0 > 2 && right.scrolled.1 == 0, "{right:?}");
        assert_ne!(right.outcome, Outcome::Blocked);
        // Nothing scrolls for Left in the test ROM, and there's no player sprite.
        let left = step(&mut m, Button::Left, None);
        assert_eq!(left.outcome, Outcome::Blocked);
        assert_eq!(left.scrolled, (0, 0));
    }
}
