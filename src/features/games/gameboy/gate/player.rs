//! The gate's player: a Game Boy of its own, right here, instead of the bot's console.

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;

use crate::features::games::gameboy::actions::{self, Acted};
use crate::features::games::gameboy::any::{self, Sight};
use crate::features::games::gameboy::console::Machine;
use crate::features::games::gameboy::map::AreaMap;
use crate::features::games::gameboy::pictures::png_marked;
use crate::features::games::gameboy::red::State;
use crate::features::games::gameboy::screen::TileKey;
use crate::features::games::gameboy::session::Game;
use crate::features::games::gameboy::skills::Skill;
use crate::features::games::gameboy::supervisor::Player;
use crate::features::games::gameboy::text::{Page, Unread};
use crate::features::games::gameboy::walk::{Look, look_for};
use crate::features::games::gameboy::world::World;

pub struct Local {
    pub machine: Machine,
    pub world: World,
    /// Red's own look, or with `VIVY_GB_LOOK=any` the look with no memory map, to see how
    /// she plays on perception alone.
    pub look: Look,
}

impl Local {
    pub fn new(machine: Machine) -> Local {
        // Red's own look for Red unless VIVY_GB_LOOK=any; every other game has only the
        // look with no memory map.
        let look: Look = match std::env::var("VIVY_GB_LOOK").as_deref() {
            Ok("any") => any::look,
            _ => look_for(machine.emulator.info()),
        };
        Local {
            machine,
            world: World::default(),
            look,
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
            self.look,
            name,
            args,
        ))
    }

    async fn look(&mut self) -> anyhow::Result<Acted> {
        Ok(actions::look(&mut self.world, &mut self.machine, self.look))
    }

    async fn describe(&mut self, acted: &Acted) -> anyhow::Result<String> {
        Ok(actions::describe(&self.world, &acted.view))
    }

    async fn screen(&mut self) -> anyhow::Result<Vec<u8>> {
        png_marked(
            &self.machine.emulator.screen(),
            actions::SCREEN_SCALE,
            self.world.sight.her_spot(),
        )
    }

    async fn unread(&mut self) -> anyhow::Result<Vec<Unread>> {
        Ok(self.world.sight.take_unread())
    }

    async fn name_letters(
        &mut self,
        votes: &[(TileKey, String)],
        pages: &[Page],
    ) -> anyhow::Result<(Vec<String>, String)> {
        Ok(self.world.sight.name_letters(votes, pages))
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

    async fn set_sight(&mut self, sight: Sight) -> anyhow::Result<()> {
        self.world.sight = sight;
        Ok(())
    }

    async fn set_world(&mut self, world: World) -> anyhow::Result<()> {
        self.world = world;
        Ok(())
    }

    async fn save_state(&mut self) -> anyhow::Result<Vec<u8>> {
        self.machine.emulator.save_state()
    }

    async fn load_state(&mut self, state: Vec<u8>) -> anyhow::Result<()> {
        self.machine.load_state(&state)
    }
}
