//! Walking to a step: code finds the way on the area's map and takes it one step at a
//! time, looking after each one. A model only names where to go; it's never asked to
//! count presses.
//!
//! Every step goes in the movement log. A step that didn't move her marks that way as
//! blocked (unless someone was standing there), and the way is found again, so walls the
//! map got wrong are learned by bumping into them. Walking stops when she gets there, when
//! the area changes, or when something else takes over the screen: a text box, a battle.

use super::change::{Transitions, look as frame_look};
use super::console::Machine;
use super::emulator::RomInfo;
use super::map::{Dir, Pos};
use super::moves::{Outcome, STEP_PIXELS, step};
use super::red;
use super::world::{Logged, View, World};

/// How a game is looked at: `red::look` for Pokémon Red.
pub type Look = fn(&mut World, &mut Machine) -> View;

/// The way to look at this game, if code knows one. Only Pokémon Red and Blue so far;
/// other games need the tile classifier of a later stage.
pub fn look_for(info: &RomInfo) -> Option<Look> {
    red::is_red(info).then_some(red::look as Look)
}

/// Steps that fail before walking gives up.
const MOST_FAILED: u32 = 6;

/// After the area changes: how long to wait for a fade to start, and how long the picture
/// must be back before the new area counts as loaded.
const FADE_STARTS_WITHIN: u32 = 60;
const BACK_FOR: u32 = 20;

/// When something stops her walking, how long to wait for its words (or for it to pass).
const WORDS_WITHIN: u32 = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Walked {
    /// She's there.
    Arrived,
    /// She walked into another area (through a door, or off the edge).
    NewArea(String),
    /// Something took over the screen: these words, or none.
    Interrupted(String),
    /// The map knows no way there, not even through unseen steps.
    NoWay,
    /// Too many steps failed, or the step limit ran out.
    Stuck,
}

#[derive(Debug, Clone)]
pub struct Walk {
    pub result: Walked,
    /// Steps tried.
    pub steps: u32,
    /// The last look, after walking.
    pub view: View,
}

/// Walks to `target` in the current area, trying at most `most` steps.
pub fn walk_to(
    world: &mut World,
    machine: &mut Machine,
    look: Look,
    target: Pos,
    most: u32,
) -> Walk {
    let mut view = look(world, machine);
    let mut steps = 0;
    let mut failed = 0;
    let done = |result, steps, view| Walk {
        result,
        steps,
        view,
    };
    loop {
        if !view.can_walk {
            // Whatever stopped her may take a moment to put its words up, or be over in
            // a moment (the screen settling after stairs).
            for _ in 0..WORDS_WITHIN / 10 {
                if !view.text.is_empty() || view.can_walk {
                    break;
                }
                machine.run(10);
                view = look(world, machine);
            }
            if !view.can_walk {
                return done(Walked::Interrupted(view.text.clone()), steps, view);
            }
            continue;
        }
        if view.at == target {
            return done(Walked::Arrived, steps, view);
        }
        if steps >= most || failed >= MOST_FAILED {
            return done(Walked::Stuck, steps, view);
        }
        let map = world.area(&view.area);
        let way = map
            .path(view.at, target, false)
            .or_else(|| map.path(view.at, target, true));
        let Some(&dir) = way.as_ref().and_then(|w| w.first()) else {
            return done(Walked::NoWay, steps, view);
        };

        let (next, outcome) = take_step(world, machine, look, &view, dir);
        steps += 1;
        if next.area != view.area {
            return done(Walked::NewArea(next.area.clone()), steps, next);
        }
        // A step cut short by something taking over (a scene, a text box) says nothing
        // about the way.
        if next.at == view.at && next.can_walk {
            failed += 1;
            let map = world.area(&view.area);
            if !map.person_at(dir.step(view.at)) {
                map.blocked(view.at, dir);
            }
        } else if matches!(outcome, Outcome::Moved(_)) {
            world.area(&next.area).walked(next.at);
        }
        view = next;
    }
}

/// Takes one step (a second try if the first only turned her), logs it, and checks the
/// game's memory against the screen. Returns the look after it and how it went.
pub fn take_step(
    world: &mut World,
    machine: &mut Machine,
    look: Look,
    view: &View,
    dir: Dir,
) -> (View, Outcome) {
    let mut taken = step(machine, dir.button());
    if taken.outcome == Outcome::Turned {
        taken = step(machine, dir.button());
    }
    let mut next = look(world, machine);
    if next.area != view.area || taken.outcome == Outcome::NewArea {
        settle(machine);
        next = look(world, machine);
    }
    if let Outcome::Moved(n) = taken.outcome
        && next.area == view.area
    {
        let screen = (
            (taken.scrolled.0 as f32 / STEP_PIXELS as f32).round() as i32,
            (taken.scrolled.1 as f32 / STEP_PIXELS as f32).round() as i32,
        );
        let memory = (next.at.0 - view.at.0, next.at.1 - view.at.1);
        // A scroll of n steps that memory saw as n steps the same way.
        if screen.0.abs().max(screen.1.abs()) == n {
            world.trust.check(memory, screen);
        }
    }
    world.log(Logged {
        area: view.area.clone(),
        from: view.at,
        direction: dir,
        outcome: taken.outcome,
        to_area: next.area.clone(),
        to: next.at,
    });
    (next, taken.outcome)
}

/// Lets an area change finish. Red changes its map number as the step ends, about 20
/// frames before the screen fades, and loads the new area's doors and tiles while it's
/// dark; towns and routes join with no fade at all.
fn settle(machine: &mut Machine) {
    let mut transitions = Transitions::default();
    let mut was_dark = false;
    let mut back = 0;
    for frame in 0..FADE_STARTS_WITHIN + 240 {
        machine.frame();
        let video = machine.emulator.video().clone();
        let rgb = machine.emulator.screen();
        transitions.feed(frame_look(video.drawn, &video.registers, &rgb, video.color));
        if transitions.dark() {
            was_dark = true;
            back = 0;
        } else if was_dark {
            back += 1;
            if back >= BACK_FOR {
                return;
            }
        } else if frame >= FADE_STARTS_WITHIN {
            return;
        }
    }
}

/// Walks toward the nearest step she hasn't seen yet, to fill in the map, until it's been
/// seen (it may turn out to be a wall). `NoWay` when every step she can reach has been
/// seen.
pub fn explore(world: &mut World, machine: &mut Machine, look: Look, most: u32) -> Walk {
    let mut steps = 0;
    loop {
        let view = look(world, machine);
        let map = world.area(&view.area);
        let target = map.frontier(view.at).first().copied();
        let Some(target) = target.filter(|_| view.can_walk && steps < most) else {
            // Nothing left to see, or nothing she can walk to: walk_to says which.
            let result = match target {
                None if view.can_walk => Walked::NoWay,
                _ => walk_to(world, machine, look, view.at, 0).result,
            };
            return Walk {
                result,
                steps,
                view,
            };
        };
        let mut walk = walk_to(world, machine, look, target, most - steps);
        steps += walk.steps;
        walk.steps = steps;
        let seen = world.area(&walk.view.area).kind(target).is_some();
        match walk.result {
            Walked::Arrived => return walk,
            // It came into view and isn't somewhere to stand: that's seen too.
            Walked::NoWay | Walked::Stuck if seen => {
                walk.result = Walked::Arrived;
                return walk;
            }
            Walked::NoWay => continue,
            _ => return walk,
        }
    }
}
