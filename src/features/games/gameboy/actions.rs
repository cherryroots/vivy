//! What a model can do in the game, as tools: walk to a step, explore, press a few buttons,
//! read through text, and write and use her own skills. Code does the work; the model only
//! picks the action and where.
//!
//! Every action ends with a look, and its answer says what happened in a short line, then
//! where she is now ([`describe`]). The words seen along the way are kept, so code can
//! check afterwards what was said on screen.

use serde_json::{Value, json};

use super::console::{Machine, Timing};
use super::emulator::Button;
use super::map::{Dir, Kind, LEGEND};
use super::near::{self, Thing};
use super::path;
use super::presses::{self, Press};
use super::puzzle::Move;
use super::ram;
use super::reach::{self, Landing};
use super::sent;
use super::skills::{self, Around, Skill};
use super::walk::{Look, Walked, explore, take_step, walk_to};
use super::world::{View, World};
use crate::ai::ToolDef;

/// Steps one walk takes at most.
const WALK_MOST: u32 = 60;
/// Presses one `press` takes at most. A plan of presses written from one screen is wrong
/// by the second, so it stays short.
const PRESS_MOST: usize = 5;
/// Presses `read_text` makes at most.
const READ_MOST: u32 = 30;
/// After an action, how long to wait at most for the screen to settle, looking every few
/// frames.
const SETTLE_MOST: u32 = 120;
const LOOK_EVERY: u32 = 5;
/// How long she must still be able to walk to count as free.
const STILL_FREE: u32 = 20;
/// Steps the map shows each way around her.
const MAP_RADIUS: i32 = 7;

/// How much bigger the screen is sent to the model than the Game Boy's 160×144. Twice is
/// still small (about 120 tokens) and easier to make out.
pub const SCREEN_SCALE: u32 = 2;

/// The cursor menus show next to the chosen item: a choice to make, not text to press on.
const CURSOR: char = '▶';

/// What `holding` does, on every action that takes it.
const HOLDING: &str = "Buttons kept held down the whole time, like \"b\" (a shield up while \
    you walk, a run button). Let go when the action ends.";

/// The tools, always in the same order (they're part of the cached prompt).
pub fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "walk_to",
            description: "Walk to a step on the map, by its x and y (the map's top line and \
                left column). Code finds the way. Stops early in a new area, or when \
                something takes over the screen. Only works while you can walk.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "holding": {"type": "string", "description": HOLDING}
                },
                "required": ["x", "y"]
            }),
        },
        ToolDef {
            name: "explore",
            description: "Walk toward the nearest step of this area not seen yet (a ? on \
                the map), to see more of it. Never goes through doors.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "holding": {"type": "string", "description": HOLDING}
                }
            }),
        },
        ToolDef {
            name: "press",
            description: "Press 1 to 5 buttons, one after another: a, b, start, select, up, \
                down, left, right, like \"up a\". A direction is one step, or a turn when \
                you face another way. Join buttons with + to press them together \
                (\"b+right\"); add :frames to hold one longer than a step (\"a:40\", up to \
                120, 60 a second: a charged shot, a higher jump), and give each button its \
                own frames to let some go first (\"a:20+right:60\" jumps and keeps going \
                right). For menus, choices and talking (face someone, then a).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "buttons": {"type": "string"},
                    "holding": {"type": "string", "description": HOLDING}
                },
                "required": ["buttons"]
            }),
        },
        ToolDef {
            name: "read_text",
            description: "Press A through the text on screen until it's gone, or until a \
                choice or menu comes up. Tells you all the text it went through.",
            parameters: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "write_skill",
            description: "Write down a skill of your own for this game, to use later with \
                use_skill: know-how (notes you'll read then), a routine code runs as one \
                action, or both. Write one when you work something out, or catch yourself \
                repeating the same presses (a battle, a menu). A routine's rules: each round, \
                the first rule whose if_words are on screen (case doesn't matter; leave \
                if_words empty to fit any screen) has its 1 to 5 buttons pressed. It stops \
                when you can walk (unless stop_when_free is false), when any of stop_words \
                is on screen, after most rounds (default 30, at most 50), when no rule fits, \
                or when nothing changes 3 rounds running (stop words only count after its \
                first press). Stop words are words, never numbers: HP, levels and money \
                change. A reflex is a routine for fights and jumps: give a rule if_near, \
                what must be around you (the \"Near you\" line): near, ahead (the way you \
                face), behind, above, below, left, right or coming (moving closer), then \
                steps 1 to 4, like \"ahead 2\"; or blocked (your last direction didn't move \
                you). A reflex runs while you walk, pressing at once with no waiting, and \
                stops when words come up, when you can't walk for a moment, when no rule fits \
                or after most rounds; put a rule with no if_near last for what to do \
                otherwise. The same name replaces a skill: fix one that didn't work rather \
                than writing another beside it.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "use_when": {"type": "string"},
                    "notes": {"type": "string"},
                    "rules": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "if_words": {"type": "string"},
                                "if_near": {"type": "string"},
                                "press": {"type": "string"}
                            },
                            "required": ["press"]
                        }
                    },
                    "stop_words": {"type": "array", "items": {"type": "string"}},
                    "stop_when_free": {"type": "boolean"},
                    "most": {"type": "integer"},
                    "holding": {"type": "string", "description": HOLDING}
                },
                "required": ["name", "use_when"]
            }),
        },
        ToolDef {
            name: "use_skill",
            description: "Use one of your skills by its name, as one action: shows its \
                notes and runs its routine, telling you how many rounds it ran, why it \
                stopped and the text it went through.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                },
                "required": ["name"]
            }),
        },
        ram::search_tool(),
        ram::watch_tool(),
        ToolDef {
            name: "finish",
            description: "End the task. done: whether the screen shows the task is \
                complete. Say what happened in summary.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "done": {"type": "boolean"},
                    "summary": {"type": "string"}
                },
                "required": ["done", "summary"]
            }),
        },
    ]
}

/// What an action did.
#[derive(Debug, Clone)]
pub struct Acted {
    /// One short line: what happened.
    pub outcome: String,
    /// The look afterwards.
    pub view: View,
    /// Every different screen of words seen along the way, in order.
    pub words: Vec<String>,
}

/// Runs one action by its tool name. `finish` isn't an action: the caller handles it.
/// The values she watches are added to the facts.
pub fn run(
    world: &mut World,
    machine: &mut Machine,
    look: Look,
    name: &str,
    args: &Value,
) -> Acted {
    let takes_holding = matches!(name, "walk_to" | "explore" | "press");
    let holding = match args["holding"]
        .as_str()
        .filter(|_| takes_holding)
        .map(held_buttons)
    {
        Some(Err(why)) => return told(world, machine, look, &why),
        Some(Ok(buttons)) => buttons,
        None => Vec::new(),
    };
    machine.emulator.keep(&holding);
    let mut acted = act(world, machine, look, name, args);
    if !holding.is_empty() {
        machine.emulator.keep(&[]);
        let names: Vec<&str> = holding.iter().map(|b| b.name()).collect();
        acted.outcome = format!("{} (holding {})", acted.outcome, names.join("+"));
    }
    add_watched(world, machine, &mut acted.view);
    world.near = near_her(world, machine, &acted.view);
    world.jumps = jumps_from_here(world, machine, &acted.view);
    acted
}

/// Where her jumps would land from where she stands, while she can walk (`reach.rs`).
fn jumps_from_here(world: &World, machine: &mut Machine, view: &View) -> Vec<Landing> {
    if !view.can_walk {
        return Vec::new();
    }
    let Some(spot) = her_spot(world, view) else {
        return Vec::new();
    };
    let step = view.grid.map_or(16, |g| g.pixels);
    reach::from_here(world, machine, spot, view.at, step)
}

/// What's near her now and how it moves, while she can walk.
pub fn near_her(world: &World, machine: &Machine, view: &View) -> Vec<Thing> {
    if !view.can_walk {
        return Vec::new();
    }
    let Some(her) = her_spot(world, view) else {
        return Vec::new();
    };
    let now = machine.emulator.frames();
    let Some(frames) = machine.sprites_since(now.saturating_sub(near::LOOK_BACK)) else {
        return Vec::new();
    };
    let (Some(then), Some(last)) = (frames.first(), frames.last()) else {
        return Vec::new();
    };
    near::things(then, last, her, sprite_height(machine))
}

/// Where she is on the screen: her sprite, or the middle of her step on a screen that
/// scrolls under her.
fn her_spot(world: &World, view: &View) -> Option<(i32, i32)> {
    world.sight.her_spot().or_else(|| {
        view.grid.map(|g| {
            (
                g.player.0 * g.pixels + g.pixels / 2,
                g.player.1 * g.pixels + g.pixels / 2,
            )
        })
    })
}

/// The sprites' height: 8, or 16 in a game that draws them tall.
fn sprite_height(machine: &Machine) -> i32 {
    if machine.emulator.video().registers.lcdc & 0x04 != 0 {
        16
    } else {
        8
    }
}

/// What `one`, pressed from frame `start`, did over time, from her `spot` then: how it
/// carried her, if further than a step, how far each frame of its directions took her, and
/// what it sent out. Kept with the game's world, and said as lines.
fn after_press(
    world: &mut World,
    machine: &Machine,
    start: u64,
    spot: Option<(i32, i32)>,
    one: &Press,
    view: &View,
) -> Vec<String> {
    let typed = presses::describe(std::slice::from_ref(one));
    let Some(frames) = machine.frames_since(start) else {
        return Vec::new();
    };
    let height = sprite_height(machine);
    let Some((spot, places)) = spot.and_then(|s| Some((s, path::follow(&frames, s, height)?)))
    else {
        return Vec::new();
    };
    let mut press = typed.to_string();
    let held = machine.emulator.kept();
    if !held.is_empty() {
        let names: Vec<&str> = held.iter().map(|b| b.name()).collect();
        press.push_str(&format!(" holding {}", names.join("+")));
    }
    let step = view.grid.map_or(16, |g| g.pixels);
    for way in one.buttons.iter().filter(|b| b.is_direction()) {
        let unit = match way {
            Button::Left => (-1, 0),
            Button::Right => (1, 0),
            Button::Up => (0, -1),
            _ => (0, 1),
        };
        let held = one.frames_of(*way, Timing::default().hold);
        for pace in path::paces_of(way.name(), unit, held, &places, step) {
            path::remember_pace(&mut world.paces, pace);
        }
    }
    let mut lines = Vec::new();
    if let Some(motion) = path::Motion::of(&press, &places, step) {
        lines.push(format!("{} {}.", motion.press, motion.words()));
        path::remember(&mut world.motions, motion);
    }
    let back = start.saturating_sub(sent::FRESH_FRAMES as u64);
    let before = machine.frames_since(back).unwrap_or_default();
    let before = &before[..before.len().saturating_sub(frames.len())];
    for one in sent::sent(&press, before, &frames, spot, &places, height, step) {
        lines.push(format!(
            "{} sent something out: {}.",
            one.press,
            one.words()
        ));
        sent::remember(&mut world.sent, one);
    }
    lines
}

/// The buttons in `holding`, like "b" or "a+b", or what's wrong with it.
fn held_buttons(text: &str) -> Result<Vec<Button>, String> {
    let mut buttons = Vec::new();
    for name in text
        .split(|c: char| c == '+' || c.is_whitespace() || c == ',')
        .filter(|n| !n.is_empty())
    {
        let button = Button::parse(name).ok_or_else(|| {
            format!(
                "`{name}` in holding isn't a button. The buttons are a, b, start, select, up, \
                 down, left and right."
            )
        })?;
        if !buttons.contains(&button) {
            buttons.push(button);
        }
    }
    Ok(buttons)
}

fn add_watched(world: &World, machine: &mut Machine, view: &mut View) {
    let watched = ram::watched(&world.watches, machine);
    if watched.is_empty() {
        return;
    }
    if !view.facts.is_empty() {
        view.facts.push('\n');
    }
    view.facts.push_str(&watched);
}

fn act(world: &mut World, machine: &mut Machine, look: Look, name: &str, args: &Value) -> Acted {
    match name {
        "walk_to" => {
            let (Some(x), Some(y)) = (args["x"].as_i64(), args["y"].as_i64()) else {
                return told(world, machine, look, "walk_to needs whole numbers x and y.");
            };
            let (x, y) = (x as i32, y as i32);
            let view = look(world, machine);
            // Outside the area only its edge steps can be walked to, and only where the step
            // inside them isn't a wall: they're the ways out.
            if let Some(map) = world.areas.get(&view.area)
                && let Some((w, h)) = map.size
                && !map.inside((x, y))
                && map.kind((x, y)) == Some(Kind::Edge)
                && map.kind((x.clamp(0, w - 1), y.clamp(0, h - 1))) == Some(Kind::Wall)
            {
                return told(
                    world,
                    machine,
                    look,
                    &format!(
                        "({x}, {y}) is past the edge of {} behind a wall, so there's no way out \
                         there. The ways out are edge steps (>) next to floor or grass.",
                        view.area
                    ),
                );
            }
            if let Some(map) = world.areas.get(&view.area)
                && let Some((w, h)) = map.size
                && !map.inside((x, y))
                && map.kind((x, y)) != Some(Kind::Edge)
            {
                return told(
                    world,
                    machine,
                    look,
                    &format!(
                        "({x}, {y}) is outside {}, which is {w}×{h} steps (x 0 to {}, y 0 to \
                         {}). To leave it, walk to an edge step (>) and step on through it.",
                        view.area,
                        w - 1,
                        h - 1
                    ),
                );
            }
            let walk = walk_to(world, machine, look, (x, y), WALK_MOST);
            let view = settle(world, machine, look, &mut Vec::new());
            let finding = world.sight.finding_her();
            unseen(
                walked(walk.result, walk.steps, view, (x, y), false),
                finding,
            )
        }
        "explore" => {
            let walk = explore(world, machine, look, WALK_MOST);
            let view = settle(world, machine, look, &mut Vec::new());
            let at = view.at;
            let finding = world.sight.finding_her();
            unseen(walked(walk.result, walk.steps, view, at, true), finding)
        }
        "press" => press(world, machine, look, args["buttons"].as_str().unwrap_or("")),
        "read_text" => read_text(world, machine, look),
        "write_skill" => {
            let saved = Skill::from_args(args).and_then(|skill| {
                let (name, clash) = (skill.name.clone(), skill.clash());
                skills::save(&mut world.skills, skill).map(|()| (name, clash))
            });
            match saved {
                Ok((name, clash)) => {
                    let warning = clash.map(|c| format!(" Careful: {c}")).unwrap_or_default();
                    told(
                        world,
                        machine,
                        look,
                        &format!("Saved the skill \"{name}\". Use it with use_skill.{warning}"),
                    )
                }
                Err(why) => told(world, machine, look, &why),
            }
        }
        "use_skill" => use_skill(world, machine, look, args["name"].as_str().unwrap_or("")),
        "ram_search" => {
            let found = ram::search(&mut world.search, machine, args);
            told(world, machine, look, &found)
        }
        "ram_watch" => {
            let line = ram::watch(&mut world.watches, args).unwrap_or_else(|why| why);
            told(world, machine, look, &line)
        }
        // Only code calls it, with the puzzle helper's moves: it isn't one of her tools.
        "follow_moves" => match serde_json::from_value::<Vec<Move>>(args["moves"].clone()) {
            Ok(moves) => follow_moves(world, machine, look, &moves),
            Err(err) => told(world, machine, look, &format!("Unreadable moves: {err}")),
        },
        _ => told(
            world,
            machine,
            look,
            &format!("There's no action called {name}."),
        ),
    }
}

/// Looks without doing anything.
pub fn look(world: &mut World, machine: &mut Machine, look: Look) -> Acted {
    let mut view = look(world, machine);
    add_watched(world, machine, &mut view);
    world.jumps = jumps_from_here(world, machine, &view);
    Acted {
        outcome: String::new(),
        words: words_of(&view),
        view,
    }
}

/// Looks, and tells her `line` about it.
fn told(world: &mut World, machine: &mut Machine, look: Look, line: &str) -> Acted {
    let view = look(world, machine);
    Acted {
        outcome: line.to_string(),
        words: words_of(&view),
        view,
    }
}

fn words_of(view: &View) -> Vec<String> {
    (!view.text.is_empty())
        .then(|| view.text.clone())
        .into_iter()
        .collect()
}

/// A walk that couldn't start because code hasn't seen which sprite is her yet (nothing
/// on screen moved with a press): says so, instead of blaming text that isn't there.
fn unseen(mut acted: Acted, finding: bool) -> Acted {
    if finding && acted.view.text.is_empty() && acted.outcome.starts_with("Can't walk now") {
        acted.outcome = "Can't walk yet: which sprite is you isn't known, and pressing each \
                         way didn't show it (two moved as pressed, or none did). Walk again, or \
                         press a direction toward open floor."
            .to_string();
    }
    acted
}

/// What a walk did, in a line. `exploring` is for `explore`, where `NoWay` means
/// everything she can reach is seen.
fn walked(result: Walked, steps: u32, view: View, target: (i32, i32), exploring: bool) -> Acted {
    let (x, y) = target;
    let outcome = match result {
        Walked::Arrived => format!("Walked to ({x}, {y}) in {steps} steps."),
        Walked::NewArea(area) => format!("Walked into {area} after {steps} steps."),
        Walked::Interrupted(_) if steps == 0 => {
            "Can't walk now: something is on screen (read it, or press b to close it).".to_string()
        }
        Walked::Interrupted(_) => format!("Stopped after {steps} steps: something came up."),
        Walked::NoWay if exploring => {
            format!("Every step you can reach here has been seen ({steps} steps).")
        }
        Walked::NoWay if steps == 0 => {
            format!("There's no known way to ({x}, {y}). It may be a wall, or behind one.")
        }
        Walked::NoWay => format!(
            "Walked {steps} steps toward ({x}, {y}), then found no way there. It's a wall, or behind one."
        ),
        Walked::Stuck => format!("Couldn't get to ({x}, {y}): blocked after {steps} steps."),
    };
    Acted {
        outcome,
        words: words_of(&view),
        view,
    }
}

fn press(world: &mut World, machine: &mut Machine, look: Look, buttons: &str) -> Acted {
    let presses = match presses::parse(buttons) {
        Ok(p) if p.is_empty() => {
            return told(world, machine, look, "Say which buttons to press.");
        }
        Ok(p) if p.len() > PRESS_MOST => {
            return told(
                world,
                machine,
                look,
                &format!("At most {PRESS_MOST} presses at a time, then look again."),
            );
        }
        Ok(p) => p,
        Err(why) => return told(world, machine, look, &why),
    };
    let before = look(world, machine);
    let screen_before = machine.emulator.screen();
    let mut words = Vec::new();
    let mut view = before.clone();
    let mut carried_lines = Vec::new();
    for one in &presses {
        let spot = view.can_walk.then(|| her_spot(world, &view)).flatten();
        let start = machine.emulator.frames();
        machine.press(std::slice::from_ref(one), Timing::default());
        view = settle(world, machine, look, &mut words);
        carried_lines.extend(after_press(world, machine, start, spot, one, &view));
    }
    // Her own presses haven't shown which sprite is her: pressing each way does.
    if !view.can_walk
        && view.text.is_empty()
        && world.sight.finding_her()
        && let Some(her) = super::moves::which_is_her(machine)
    {
        world.sight.found_her_at(her);
        view = settle(world, machine, look, &mut words);
    }
    let typed = presses::describe(&presses);
    let still = carried_lines.is_empty() && machine.emulator.screen() == screen_before;
    let mut outcome = if view == before && still {
        format!("Pressed {typed}. Nothing changed.")
    } else {
        format!("Pressed {typed}.")
    };
    for line in carried_lines {
        outcome.push(' ');
        outcome.push_str(&line);
    }
    Acted {
        outcome,
        view,
        words,
    }
}

fn keep_words(words: &mut Vec<String>, view: &View) {
    if view.text.is_empty() || words.last() == Some(&view.text) {
        return;
    }
    // Caught half typed a look ago: the whole of it replaces that.
    if words
        .last()
        .is_some_and(|last| view.text.starts_with(last.as_str()))
    {
        words.pop();
    }
    words.push(view.text.clone());
}

/// Lets the screen settle after an action and looks: until she can walk, the words have
/// stopped changing (text is typed a letter at a time) or a short while has passed. Keeps
/// the words it ends on.
fn settle(world: &mut World, machine: &mut Machine, look: Look, words: &mut Vec<String>) -> View {
    let mut view = look(world, machine);
    let mut same = 0;
    for _ in 0..SETTLE_MOST / LOOK_EVERY {
        if same >= 3 {
            break;
        }
        if view.can_walk {
            // A scene can start a moment after a text box closes (the rival walking up):
            // she can only walk if she still can a beat later.
            machine.run(STILL_FREE);
            view = look(world, machine);
            if view.can_walk {
                break;
            }
            continue;
        }
        machine.run(LOOK_EVERY);
        let next = look(world, machine);
        same = if !next.text.is_empty() && next.text == view.text {
            same + 1
        } else {
            0
        };
        view = next;
    }
    keep_words(words, &view);
    view
}

/// Presses A while there's text and no choice to make.
fn read_text(world: &mut World, machine: &mut Machine, look: Look) -> Acted {
    let mut view = look(world, machine);
    let mut words = Vec::new();
    keep_words(&mut words, &view);
    let a = Press::of(vec![Button::A]);
    let mut presses = 0;
    while presses < READ_MOST && !view.can_walk && !view.text.contains(CURSOR) {
        machine.press(std::slice::from_ref(&a), Timing::default());
        presses += 1;
        view = settle(world, machine, look, &mut words);
    }
    let outcome = if !view.can_walk && view.text.is_empty() && world.sight.finding_her() {
        "No text was read. Which sprite is you isn't known yet: walk_to or explore finds it \
         (they press each way and see which one does as pressed)."
            .to_string()
    } else if presses == 0 && view.can_walk {
        "There's no text on screen.".to_string()
    } else if view.text.contains(CURSOR) {
        format!("Read the text ({presses} presses). A choice or menu is up now.")
    } else if view.can_walk {
        format!("Read the text ({presses} presses). It's gone; you can walk.")
    } else {
        format!("Pressed A {presses} times and the screen still isn't free.")
    };
    Acted {
        outcome,
        view,
        words,
    }
}

/// Presses the puzzle helper's moves (`puzzle.rs`) one at a time, learning the map as
/// walking does, and checks after each move that she stands where it said. A held press
/// that ends a step from its spot (a jump landing a little short or long) walks that last
/// step. Stops at the first move that still doesn't end there, when she leaves the area, or
/// when a choice comes up; text that comes up along the way is read through. What the
/// presses did over time (how a jump went) is said after the outcome.
fn follow_moves(world: &mut World, machine: &mut Machine, look: Look, moves: &[Move]) -> Acted {
    let mut view = look(world, machine);
    let mut words = Vec::new();
    let mut carried = Vec::new();
    let area = view.area.clone();
    let all = moves.len();
    let a = Press::of(vec![Button::A]);
    let told = |outcome: String, carried: &[String]| {
        let mut outcome = outcome;
        for line in carried {
            outcome.push(' ');
            outcome.push_str(line);
        }
        outcome
    };
    for (i, one) in moves.iter().enumerate() {
        let n = i + 1;
        for _ in 0..one.times {
            if !view.can_walk {
                let read = read_text(world, machine, look);
                words.extend(read.words);
                view = read.view;
                if !view.can_walk {
                    let outcome = format!(
                        "Stopped at move {n} of {all} ({}): a choice or menu is up.",
                        one.line()
                    );
                    return Acted {
                        outcome: told(outcome, &carried),
                        view,
                        words,
                    };
                }
            }
            match one.dir() {
                Some(dir) => view = step_once(world, machine, look, &view, dir, &mut words),
                None => {
                    let press = one.press().unwrap_or_else(|_| a.clone());
                    let spot = view.can_walk.then(|| her_spot(world, &view)).flatten();
                    let start = machine.emulator.frames();
                    machine.press(std::slice::from_ref(&press), Timing::default());
                    view = settle(world, machine, look, &mut words);
                    carried.extend(after_press(world, machine, start, spot, &press, &view));
                }
            }
            if view.area != area {
                let outcome = format!(
                    "Move {n} of {all} ({}) took her into {}.",
                    one.line(),
                    view.area
                );
                return Acted {
                    outcome: told(outcome, &carried),
                    view,
                    words,
                };
            }
        }
        let (x, y) = view.at;
        if let Some(dir) = Dir::ALL
            .into_iter()
            .find(|d| d.step(view.at) == one.spot())
            .filter(|_| view.can_walk && one.dir().is_none())
        {
            carried.push(format!(
                "Move {n} ended a step away at ({x}, {y}), so she walked {}.",
                dir.name()
            ));
            view = step_once(world, machine, look, &view, dir, &mut words);
        }
        if view.at != one.spot() {
            let outcome = format!(
                "Move {n} of {all} ({}) ended at ({}, {}) instead. Stopped there.",
                one.line(),
                view.at.0,
                view.at.1
            );
            return Acted {
                outcome: told(outcome, &carried),
                view,
                words,
            };
        }
    }
    let (x, y) = view.at;
    let outcome = match all {
        1 => format!("Followed the move; she's at ({x}, {y})."),
        _ => format!("Followed all {all} moves; she's at ({x}, {y})."),
    };
    Acted {
        outcome: told(outcome, &carried),
        view,
        words,
    }
}

/// One step `dir` the way walking takes it, learning the map: a wall where she didn't
/// move, a walked step where she did.
fn step_once(
    world: &mut World,
    machine: &mut Machine,
    look: Look,
    view: &View,
    dir: Dir,
    words: &mut Vec<String>,
) -> View {
    let (next, outcome) = take_step(world, machine, look, view, dir);
    let map = world.area(&view.area);
    if next.area == view.area && next.at == view.at && next.can_walk {
        if !map.person_at(dir.step(view.at)) {
            map.blocked(view.at, dir);
        }
    } else if matches!(outcome, super::moves::Outcome::Moved(_)) && next.area == view.area {
        map.walked(next.at);
    }
    settle(world, machine, look, words)
}

/// Rounds in a row that change nothing before a skill gives up.
const SKILL_STUCK: u32 = 3;
/// How a reflex presses: the usual hold, and only a short gap so the game sees the next
/// press as new.
const REFLEX: Timing = Timing { hold: 16, gap: 4 };
/// Rounds in a row she can't walk before a reflex stops: a moment (the camera catching
/// up, a hit knocking her back) doesn't stop it, a game over or a scene does.
const REFLEX_UNFREE: u32 = 3;

/// Where the background is scrolled to: a screen that scrolls under her moves it when she
/// walks.
fn scroll_of(machine: &Machine) -> (u8, u8) {
    let r = &machine.emulator.video().registers;
    (r.scx, r.scy)
}

/// Runs one of her skills: each round, the buttons of the first rule that fits the screen,
/// until it says to stop. Notes how it went on the skill, for her list.
fn use_skill(world: &mut World, machine: &mut Machine, look: Look, name: &str) -> Acted {
    let Some(skill) = skills::find(&mut world.skills, name).cloned() else {
        let why = if world.skills.is_empty() {
            "You have no skills yet: write one with write_skill.".to_string()
        } else {
            let names: Vec<&str> = world.skills.iter().map(|s| s.name.as_str()).collect();
            format!(
                "There's no skill called {name}. Yours: {}.",
                names.join(", ")
            )
        };
        return told(world, machine, look, &why);
    };
    let notes = if skill.notes.is_empty() {
        String::new()
    } else {
        format!("\nYour notes: {}", skill.notes)
    };
    if skill.know_how() {
        return told(world, machine, look, &format!("{}{notes}", skill.name));
    }
    if let Some((area, spot)) = &skill.practised_at {
        let view = look(world, machine);
        if let Some(why) = skills::away_from_practice(&skill.name, area, *spot, &view) {
            return told(world, machine, look, &why);
        }
    }
    // Checked when it was written.
    let holding = held_buttons(&skill.holding).unwrap_or_default();
    machine.emulator.keep(&holding);
    let reflex = skill.reflex();
    let mut view = look(world, machine);
    let mut words = Vec::new();
    keep_words(&mut words, &view);
    let mut around = Around {
        things: near_her(world, machine, &view),
        facing: machine
            .emulator
            .last_direction()
            .and_then(super::moves::offset),
        blocked: false,
    };
    let (mut rounds, mut presses, mut still, mut unfree) = (0, 0, 0, 0);
    let stopped = loop {
        if let Some(why) = skill.stop(&view, rounds > 0) {
            break why;
        }
        unfree = if view.can_walk { 0 } else { unfree + 1 };
        if reflex && unfree == REFLEX_UNFREE {
            break format!("you couldn't walk for {REFLEX_UNFREE} rounds");
        }
        if rounds == skill.most {
            break format!("it ran its {} rounds", skill.most);
        }
        // A reflex runs while things move around her, so a still screen is no stop.
        if still == SKILL_STUCK && !reflex {
            break format!("nothing changed for {SKILL_STUCK} rounds");
        }
        let Some(rule) = skill.rule_for(&view, &around) else {
            let says = if view.text.is_empty() {
                "has no words".to_string()
            } else {
                format!("says \"{}\"", view.text.replace('\n', " / "))
            };
            break format!("no rule fits the screen, which {says}");
        };
        // Checked when it was written.
        let buttons = presses::parse(&rule.press).unwrap_or_default();
        let (before, screen_before) = (view.clone(), machine.emulator.screen());
        let (spot, scroll) = (world.sight.her_spot(), scroll_of(machine));
        for one in &buttons {
            if reflex {
                // No waiting for the screen to settle: it never does in a fight.
                machine.press(std::slice::from_ref(one), REFLEX);
                view = look(world, machine);
                keep_words(&mut words, &view);
            } else {
                machine.press(std::slice::from_ref(one), Timing::default());
                view = settle(world, machine, look, &mut words);
            }
        }
        if reflex {
            let pressed = buttons
                .iter()
                .flat_map(|p| &p.buttons)
                .find(|b| b.is_direction());
            let moved = world.sight.her_spot() != spot || scroll_of(machine) != scroll;
            around = Around {
                things: near_her(world, machine, &view),
                facing: machine
                    .emulator
                    .last_direction()
                    .and_then(super::moves::offset)
                    .or(around.facing),
                blocked: pressed.is_some() && !moved,
            };
        }
        rounds += 1;
        presses += buttons.len();
        still = if view == before && machine.emulator.screen() == screen_before {
            still + 1
        } else {
            0
        };
    };
    let summary = format!("stopped after {rounds} rounds: {stopped}");
    // Rewriting it under its name keeps one skill per job, rather than near-copies.
    let fix = if rounds == 0 {
        " It pressed nothing: rewrite it under the same name to fix it (a rule with empty \
         if_words fits any screen)."
    } else {
        ""
    };
    machine.emulator.keep(&[]);
    if let Some(kept) = skills::find(&mut world.skills, &skill.name) {
        kept.uses += 1;
        kept.last = summary.clone();
    }
    Acted {
        outcome: format!(
            "Used {}: {presses} presses, {summary}.{fix}{notes}",
            skill.name
        ),
        view,
        words,
    }
}

/// Where she is now, for the model: the facts, whether she can walk, the words on screen
/// and the map around her.
pub fn describe(world: &World, view: &View) -> String {
    let mut lines = Vec::new();
    let size = world
        .areas
        .get(&view.area)
        .and_then(|map| map.size)
        .map(|(w, h)| format!(" ({w}×{h} steps)"))
        .unwrap_or_default();
    lines.push(format!(
        "Place: {}{size}, standing at ({}, {}).",
        view.area, view.at.0, view.at.1
    ));
    if !view.facts.is_empty() {
        lines.push(view.facts.clone());
    }
    lines.push(if view.can_walk {
        "You can walk.".to_string()
    } else {
        "You can't walk: text, a menu or a battle is on screen.".to_string()
    });
    if !view.text.is_empty() {
        lines.push(format!("Words on screen:\n{}", view.text));
    }
    if view.can_walk
        && let Some(map) = world.areas.get(&view.area)
    {
        lines.push(format!(
            "Map ({LEGEND}):\n{}",
            map.draw(view.at, MAP_RADIUS).trim_end()
        ));
        let near = near::line(&world.near);
        if !near.is_empty() {
            lines.push(near);
        }
        for list in [
            path::presses(&world.motions, &world.paces),
            sent::list(&world.sent),
            reach::list(&world.jumps, view.at),
        ] {
            if !list.is_empty() {
                lines.push(list);
            }
        }
    }
    let skills = skills::list(&world.skills);
    if !skills.is_empty() {
        lines.push(skills);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_way_reads_differently_when_exploring() {
        let view = View {
            area: "Room".to_string(),
            at: (1, 1),
            can_walk: true,
            text: String::new(),
            facts: String::new(),
            grid: None,
        };
        let walk = |steps, exploring| {
            walked(Walked::NoWay, steps, view.clone(), (3, 1), exploring).outcome
        };
        assert!(walk(0, true).starts_with("Every step you can reach"));
        assert!(walk(4, false).starts_with("Walked 4 steps toward (3, 1), then found no way"));
        assert!(walk(0, false).starts_with("There's no known way to (3, 1)"));
    }

    #[test]
    fn the_tools_have_object_schemas() {
        let tools = tools();
        assert_eq!(tools.len(), 9);
        for tool in &tools {
            assert_eq!(tool.parameters["type"], "object", "{}", tool.name);
        }
    }

    #[test]
    fn a_description_has_the_map_only_while_walking() {
        let mut world = World::default();
        world.area("Pallet Town").walked((1, 1));
        let mut view = View {
            area: "Pallet Town".to_string(),
            at: (1, 1),
            can_walk: true,
            text: String::new(),
            facts: "Badges: none".to_string(),
            grid: None,
        };
        let walking = describe(&world, &view);
        assert!(walking.contains("Place: Pallet Town, standing at (1, 1)."));
        assert!(walking.contains("Badges: none") && walking.contains("Map ("));
        view.can_walk = false;
        view.text = "OAK: Hey! Wait!".to_string();
        let talking = describe(&world, &view);
        assert!(talking.contains("can't walk") && talking.contains("OAK: Hey! Wait!"));
        assert!(!talking.contains("Map ("));
    }
}
