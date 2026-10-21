//! The gate's tasks: a save state where each starts, what the supervisor is told, and how
//! code checks it was done. The states come from playing Red's opening by script.

use std::path::{Path, PathBuf};

use crate::features::games::gameboy::console::{Machine, Timing};
use crate::features::games::gameboy::emulator::Button;
use crate::features::games::gameboy::map::{Dir, Pos};
use crate::features::games::gameboy::presses::Press;
use crate::features::games::gameboy::red::{self, State};
use crate::features::games::gameboy::walk::{Look, Walked, take_step, walk_to};
use crate::features::games::gameboy::world::{View, World};

/// Where the states go: `VIVY_GB_TASKS`, or the project's shared folder.
pub fn folder() -> PathBuf {
    std::env::var("VIVY_GB_TASKS")
        .unwrap_or_else(|_| "/mnt/project-files/gameboy/tasks".to_string())
        .into()
}

/// Plays Red by script: walking with code, and pressing through text and battles.
pub struct Driver {
    pub m: Machine,
    pub world: World,
    /// How code looks at the game: `red::look`, or another look to measure.
    pub look: Look,
}

impl Driver {
    pub fn new(m: Machine) -> Driver {
        Driver {
            m,
            world: World::default(),
            look: red::look,
        }
    }

    pub fn view(&mut self) -> View {
        (self.look)(&mut self.world, &mut self.m)
    }

    fn tap(&mut self, button: Button) {
        self.m
            .press(&[Press(vec![button])], Timing { hold: 6, gap: 20 });
    }

    /// Presses through text, choices and battles until she can walk. A nickname question
    /// gets "no"; everything else the first choice (yes, fight, the first move).
    pub fn free(&mut self) -> View {
        for _ in 0..600 {
            let view = self.view();
            if view.can_walk {
                // Make sure it stays free: some scenes start a beat later.
                self.m.run(40);
                let again = self.view();
                if again.can_walk {
                    return again;
                }
                continue;
            }
            if view.text.contains("nickname") && view.text.contains('▶') {
                self.tap(Button::Down);
            }
            self.tap(Button::A);
        }
        panic!("never got free: {:?}", self.view());
    }

    /// Walks to `target`, pressing through anything that stops her, until she's there or
    /// in another area.
    pub fn go(&mut self, target: Pos) -> View {
        let start = self.view().area;
        for _ in 0..40 {
            let walk = walk_to(&mut self.world, &mut self.m, self.look, target, 80);
            match walk.result {
                Walked::Arrived => return walk.view,
                Walked::NewArea(_) => return walk.view,
                Walked::Interrupted(_) => {
                    let view = self.free();
                    if view.area != start {
                        return view;
                    }
                }
                Walked::Stuck => {}
                Walked::NoWay => panic!("no way to {target:?} from {:?}", walk.view.at),
            }
        }
        panic!("never got to {target:?}: {:?}", self.view());
    }

    /// One step in `dir`, then presses through anything it starts.
    pub fn step(&mut self, dir: Dir) -> View {
        let view = self.view();
        let (next, _) = take_step(&mut self.world, &mut self.m, self.look, &view, dir);
        if next.can_walk { next } else { self.free() }
    }

    pub fn state(&mut self) -> State {
        State::read(&mut self.m.emulator)
    }

    pub fn save(&mut self, folder: &Path, name: &str) {
        let state = self.m.emulator.save_state().unwrap();
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join(format!("{name}.state")), state).unwrap();
        let view = self.view();
        println!("saved {name}: {} {:?}", view.area, view.at);
    }
}

/// Plays from a new game to Viridian City with Oak's Parcel, saving a state at each start.
pub fn build(folder: &Path) {
    let mut d = Driver::new(red::tests::new_game());
    play_to_the_parcel(&mut d, |d, name| d.save(folder, name));
}

/// Plays from her bedroom to Viridian City with Oak's Parcel, calling `reached` at each
/// task's start.
pub fn play_to_the_parcel(d: &mut Driver, mut reached: impl FnMut(&mut Driver, &str)) {
    reached(d, "bedroom");

    let view = d.go((7, 1));
    assert_eq!(view.area, "Red's House 1F");
    reached(d, "downstairs");

    d.go((3, 7));
    let view = d.step(Dir::Down);
    assert_eq!(view.area, "Pallet Town", "{view:?}");
    reached(d, "pallet");

    // North into the grass: Oak stops her and takes her to his lab.
    let view = d.go((10, 1));
    let view = if view.area == "Pallet Town" {
        d.step(Dir::Up)
    } else {
        view
    };
    let view = if view.area == "Oak's Lab" {
        view
    } else {
        d.free()
    };
    assert_eq!(view.area, "Oak's Lab", "{view:?}");
    reached(d, "lab");

    // Charmander's ball is on the table at (6, 3).
    d.go((6, 4));
    d.tap(Button::Up);
    d.tap(Button::A);
    d.free();
    assert_eq!(d.state().party.len(), 1, "no starter");
    reached(d, "starter");

    // Leaving, the rival stops her for a battle.
    let view = d.go((5, 10));
    println!("after the walk to the door: {view:?}");
    let view = d.free();
    assert!(
        d.state().story.contains(&"BATTLED_RIVAL_IN_OAKS_LAB"),
        "{view:?}"
    );
    reached(d, "after_rival");

    d.go((5, 11));
    let view = d.step(Dir::Down);
    assert_eq!(view.area, "Pallet Town", "{view:?}");
    reached(d, "pallet_starter");

    let view = d.go((10, 0));
    let view = if view.area == "Pallet Town" {
        d.step(Dir::Up)
    } else {
        view
    };
    assert_eq!(view.area, "Route 1", "{view:?}");
    reached(d, "route1");

    let mut view = view;
    for _ in 0..20 {
        if view.area != "Route 1" {
            break;
        }
        view = d.go((11, 0));
        if view.area == "Route 1" && view.at.1 == 0 {
            view = d.step(Dir::Up);
        }
    }
    assert_eq!(view.area, "Viridian City", "{view:?}");
    reached(d, "viridian");

    let view = d.go((29, 19));
    let view = if view.area == "Viridian Mart" {
        d.free()
    } else {
        view
    };
    assert!(d.state().story.contains(&"GOT_OAKS_PARCEL"), "{view:?}");
    reached(d, "parcel");
}

/// One task of the gate: where it starts, what the supervisor is told, and how code checks
/// it was done.
pub struct Task {
    pub name: &'static str,
    /// The save state it starts from (see [`build`]).
    pub state: &'static str,
    /// What the supervisor is told, as the strategist would put it.
    pub text: &'static str,
    /// Actions it may take.
    pub most: u32,
    /// Whether it was done, from the game's memory and the words seen.
    pub done: fn(&Check) -> bool,
}

/// What code knows after a task.
pub struct Check<'a> {
    pub after: &'a State,
    /// Every screen of words seen during the task.
    pub words: &'a [String],
}

impl Check<'_> {
    fn area(&self) -> String {
        self.after.map_name()
    }

    fn saw(&self, words: &str) -> bool {
        self.words
            .iter()
            .any(|w| w.replace('\n', " ").contains(words))
    }

    fn story(&self, flag: &str) -> bool {
        self.after.story.contains(&flag)
    }
}

/// The tasks, in the order the game reaches them.
pub const TASKS: &[Task] = &[
    Task {
        name: "go downstairs",
        state: "bedroom",
        text: "Go down the stairs, until you're on the floor below.",
        most: 15,
        done: |c| c.area() == "Red's House 1F",
    },
    Task {
        name: "potion from the PC",
        state: "bedroom",
        text: "Take the Potion out of the PC in your room, until it's in your bag.",
        most: 30,
        done: |c| c.after.bag.iter().any(|(item, _)| item == "Potion"),
    },
    Task {
        name: "talk to Mom",
        state: "downstairs",
        text: "Talk to your Mom, until she has said something to you.",
        most: 15,
        done: |c| c.saw("leave home"),
    },
    Task {
        name: "go outside",
        state: "downstairs",
        text: "Leave the house by the front door, until you're outside in town.",
        most: 15,
        done: |c| c.area() == "Pallet Town",
    },
    Task {
        name: "read the town sign",
        state: "pallet",
        text: "Read the town's own sign, until its words are on screen.",
        most: 20,
        done: |c| c.saw("Shades of"),
    },
    Task {
        name: "head north",
        state: "pallet",
        text: "Walk north out of town toward the tall grass, until someone stops you.",
        most: 20,
        done: |c| c.saw("OAK") || c.story("FOLLOWED_OAK_INTO_LAB"),
    },
    Task {
        name: "choose a starter",
        state: "lab",
        text: "Choose one of the three Poké Balls on the table, until that Pokémon is yours.",
        most: 25,
        done: |c| !c.after.party.is_empty(),
    },
    Task {
        name: "rival battle",
        state: "starter",
        text: "Head for the lab's door, and battle your rival when he stops you, until the \
            battle is over.",
        most: 40,
        done: |c| c.story("BATTLED_RIVAL_IN_OAKS_LAB"),
    },
    Task {
        name: "leave the lab",
        state: "after_rival",
        text: "Leave Oak's lab, until you're outside in town.",
        most: 15,
        done: |c| c.area() == "Pallet Town",
    },
    Task {
        name: "onto Route 1",
        state: "pallet_starter",
        text: "Go north out of town, until you're on Route 1.",
        most: 20,
        done: |c| c.area() == "Route 1",
    },
    Task {
        name: "up Route 1",
        state: "route1",
        text: "Follow Route 1 north to the next town, until you're in Viridian City. Wild \
            Pokémon may attack in the grass.",
        most: 50,
        done: |c| c.area() == "Viridian City",
    },
    Task {
        name: "into the Mart",
        state: "viridian",
        text: "Go into the Poké Mart, until the clerk has given you something.",
        most: 25,
        done: |c| c.story("GOT_OAKS_PARCEL"),
    },
    Task {
        name: "heal at the Center",
        state: "viridian",
        text: "Go into the Pokémon Center and have your Pokémon healed, until the nurse is \
            done.",
        most: 30,
        done: |c| c.saw("fighting fit") || c.saw("see you again"),
    },
    Task {
        name: "save the game",
        state: "parcel",
        text: "Save the game from the Start menu, until the screen says it's saved.",
        most: 20,
        done: |c| c.saw("saved the"),
    },
    Task {
        name: "leave the Mart",
        state: "parcel",
        text: "Leave the Poké Mart, until you're outside in Viridian City.",
        most: 15,
        done: |c| c.area() == "Viridian City",
    },
];

/// The goal of the gate's long runs, and how code knows it's reached.
pub const LONG_GOAL: &str = "Bring Oak's Parcel from Viridian City back to Professor Oak in \
    his lab in Pallet Town, and hand it to him.";
pub const LONG_START: &str = "parcel";

pub fn long_done(after: &State) -> bool {
    after.story.contains(&"OAK_GOT_PARCEL")
}

/// A Game Boy at one of the saved states.
pub fn load(state: &str) -> Machine {
    let path = folder().join(format!("{state}.state"));
    let mut m = Machine::new(
        "red.gb",
        &crate::features::games::gameboy::emulator::tests::real_rom(),
    )
    .unwrap();
    let saved = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e} (build the states first)", path.display()));
    m.emulator.load_state(&saved).unwrap();
    m.run(5);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a ROM in VIVY_GB_ROM; writes the gate's states"]
    fn build_the_task_states() {
        build(&folder());
    }

    #[test]
    fn task_names_are_different_and_start_from_built_states() {
        let built = [
            "bedroom",
            "downstairs",
            "pallet",
            "lab",
            "starter",
            "after_rival",
            "pallet_starter",
            "route1",
            "viridian",
            "parcel",
        ];
        let mut names: Vec<_> = TASKS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TASKS.len());
        for task in TASKS {
            assert!(built.contains(&task.state), "{}", task.name);
        }
        assert!(built.contains(&LONG_START));
    }
}
