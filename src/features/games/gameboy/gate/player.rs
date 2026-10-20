//! The gate's player: a Game Boy of its own, right here, instead of the bot's console.

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;

use crate::features::games::gameboy::actions::{self, Acted};
use crate::features::games::gameboy::console::Machine;
use crate::features::games::gameboy::map::AreaMap;
use crate::features::games::gameboy::pictures::png_at;
use crate::features::games::gameboy::red::{self, State};
use crate::features::games::gameboy::session::Game;
use crate::features::games::gameboy::skills::Skill;
use crate::features::games::gameboy::supervisor::Player;
use crate::features::games::gameboy::world::World;

pub struct Local {
    pub machine: Machine,
    pub world: World,
}

impl Local {
    pub fn new(machine: Machine) -> Local {
        Local {
            machine,
            world: World::default(),
        }
    }

    pub fn state(&mut self) -> State {
        State::read(&mut self.machine.emulator)
    }
}

#[async_trait]
impl Player for Local {
    async fn act(&mut self, name: &str, args: &Value) -> anyhow::Result<Acted> {
        Ok(actions::run(
            &mut self.world,
            &mut self.machine,
            red::look,
            name,
            args,
        ))
    }

    async fn look(&mut self) -> anyhow::Result<Acted> {
        Ok(actions::look(&mut self.world, &mut self.machine, red::look))
    }

    async fn describe(&mut self, acted: &Acted) -> anyhow::Result<String> {
        Ok(actions::describe(&self.world, &acted.view))
    }

    async fn screen(&mut self) -> anyhow::Result<Vec<u8>> {
        png_at(&self.machine.emulator.screen(), actions::SCREEN_SCALE)
    }
}

#[async_trait]
impl Game for Local {
    async fn world(&mut self) -> anyhow::Result<World> {
        Ok(self.world.clone())
    }

    async fn set_skills(&mut self, skills: Vec<Skill>) -> anyhow::Result<()> {
        self.world.skills = skills;
        Ok(())
    }

    async fn set_areas(&mut self, areas: HashMap<String, AreaMap>) -> anyhow::Result<()> {
        self.world.areas = areas;
        Ok(())
    }

    async fn save_state(&mut self) -> anyhow::Result<Vec<u8>> {
        self.machine.emulator.save_state()
    }

    async fn load_state(&mut self, state: Vec<u8>) -> anyhow::Result<()> {
        self.machine.emulator.load_state(&state)?;
        self.machine.run(1);
        Ok(())
    }
}
