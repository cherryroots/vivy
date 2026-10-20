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
use super::map::LEGEND;
use super::presses::{self, Press};
use super::skills::{self, Skill};
use super::walk::{Look, Walked, explore, walk_to};
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
                    "y": {"type": "integer"}
                },
                "required": ["x", "y"]
            }),
        },
        ToolDef {
            name: "explore",
            description: "Walk toward the nearest step of this area not seen yet (a ? on \
                the map), to see more of it. Never goes through doors.",
            parameters: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "press",
            description: "Press 1 to 5 buttons, one after another: a, b, start, select, up, \
                down, left, right, like \"up a\". A direction is one step, or a turn when \
                you face another way. For menus, choices and talking (face someone, then a).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "buttons": {"type": "string"}
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
                or when nothing changes 3 rounds running. The same name replaces a skill, \
                so fix one that didn't work.",
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
                                "press": {"type": "string"}
                            },
                            "required": ["press"]
                        }
                    },
                    "stop_words": {"type": "array", "items": {"type": "string"}},
                    "stop_when_free": {"type": "boolean"},
                    "most": {"type": "integer"}
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
pub fn run(
    world: &mut World,
    machine: &mut Machine,
    look: Look,
    name: &str,
    args: &Value,
) -> Acted {
    match name {
        "walk_to" => {
            let (Some(x), Some(y)) = (args["x"].as_i64(), args["y"].as_i64()) else {
                return told(world, machine, look, "walk_to needs whole numbers x and y.");
            };
            let walk = walk_to(world, machine, look, (x as i32, y as i32), WALK_MOST);
            let view = settle(world, machine, look, &mut Vec::new());
            walked(walk.result, walk.steps, view, (x as i32, y as i32), false)
        }
        "explore" => {
            let walk = explore(world, machine, look, WALK_MOST);
            let view = settle(world, machine, look, &mut Vec::new());
            let at = view.at;
            walked(walk.result, walk.steps, view, at, true)
        }
        "press" => press(world, machine, look, args["buttons"].as_str().unwrap_or("")),
        "read_text" => read_text(world, machine, look),
        "write_skill" => {
            let saved = Skill::from_args(args).and_then(|skill| {
                let name = skill.name.clone();
                skills::save(&mut world.skills, skill).map(|()| name)
            });
            match saved {
                Ok(name) => told(
                    world,
                    machine,
                    look,
                    &format!("Saved the skill \"{name}\". Use it with use_skill."),
                ),
                Err(why) => told(world, machine, look, &why),
            }
        }
        "use_skill" => use_skill(world, machine, look, args["name"].as_str().unwrap_or("")),
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
    let view = look(world, machine);
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
    for one in &presses {
        machine.press(std::slice::from_ref(one), Timing::default());
        view = settle(world, machine, look, &mut words);
    }
    let typed = presses::describe(&presses);
    let outcome = if view == before && machine.emulator.screen() == screen_before {
        format!("Pressed {typed}. Nothing changed.")
    } else {
        format!("Pressed {typed}.")
    };
    Acted {
        outcome,
        view,
        words,
    }
}

fn keep_words(words: &mut Vec<String>, view: &View) {
    if !view.text.is_empty() && words.last() != Some(&view.text) {
        words.push(view.text.clone());
    }
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
    let a = Press(vec![Button::A]);
    let mut presses = 0;
    while presses < READ_MOST && !view.can_walk && !view.text.contains(CURSOR) {
        machine.press(std::slice::from_ref(&a), Timing::default());
        presses += 1;
        view = settle(world, machine, look, &mut words);
    }
    let outcome = if presses == 0 && view.can_walk {
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

/// Rounds in a row that change nothing before a skill gives up.
const SKILL_STUCK: u32 = 3;

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
    let mut view = look(world, machine);
    let mut words = Vec::new();
    keep_words(&mut words, &view);
    let (mut rounds, mut presses, mut still) = (0, 0, 0);
    let stopped = loop {
        if let Some(why) = skill.stop(&view) {
            break why;
        }
        if rounds == skill.most {
            break format!("it ran its {} rounds", skill.most);
        }
        if still == SKILL_STUCK {
            break format!("nothing changed for {SKILL_STUCK} rounds");
        }
        let Some(rule) = skill.rule_for(&view) else {
            break "no rule fits the screen".to_string();
        };
        // Checked when it was written.
        let buttons = presses::parse(&rule.press).unwrap_or_default();
        let (before, screen_before) = (view.clone(), machine.emulator.screen());
        for one in &buttons {
            machine.press(std::slice::from_ref(one), Timing::default());
            view = settle(world, machine, look, &mut words);
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
    if let Some(kept) = skills::find(&mut world.skills, &skill.name) {
        kept.uses += 1;
        kept.last = summary.clone();
    }
    Acted {
        outcome: format!("Used {}: {presses} presses, {summary}.{notes}", skill.name),
        view,
        words,
    }
}

/// Where she is now, for the model: the facts, whether she can walk, the words on screen
/// and the map around her.
pub fn describe(world: &World, view: &View) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "Place: {}, standing at ({}, {}).",
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
        assert_eq!(tools.len(), 7);
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
