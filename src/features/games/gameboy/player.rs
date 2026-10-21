//! The supervisor's player in the bot: the one Game Boy, on its own thread (`console.rs`).
//! Each call is a job on that thread, with the world (area maps, movement log) the machine
//! keeps. A session (`session.rs`) plays it too.

use std::collections::HashMap;

use anyhow::anyhow;
use async_trait::async_trait;
use serde_json::Value;

use super::actions::{self, Acted, SCREEN_SCALE};
use super::any::Sight;
use super::console::{Machine, console};
use super::map::AreaMap;
use super::pictures::png_at;
use super::session::Game;
use super::skills::Skill;
use super::supervisor::Player;
use super::walk::{Look, look_for};
use super::world::World;

pub struct OnConsole;

/// Runs `job` on the Game Boy's thread with its world, the machine and how to look at the
/// game.
async fn on_game<T, F>(job: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut World, &mut Machine, Look) -> T + Send + 'static,
{
    console()
        .with(move |machine| {
            let m = machine
                .as_mut()
                .ok_or_else(|| anyhow!("the Game Boy was turned off"))?;
            let look = look_for(m.emulator.info());
            let mut world = std::mem::take(&mut m.world);
            let out = job(&mut world, m, look);
            m.world = world;
            Ok(out)
        })
        .await?
}

#[async_trait]
impl Player for OnConsole {
    async fn act(&mut self, name: &str, args: &Value) -> anyhow::Result<Acted> {
        let (name, args) = (name.to_string(), args.clone());
        on_game(move |world, m, look| actions::run(world, m, look, &name, &args)).await
    }

    async fn look(&mut self) -> anyhow::Result<Acted> {
        on_game(actions::look).await
    }

    async fn describe(&mut self, acted: &Acted) -> anyhow::Result<String> {
        let view = acted.view.clone();
        on_game(move |world, _, _| actions::describe(world, &view)).await
    }

    async fn screen(&mut self) -> anyhow::Result<Vec<u8>> {
        on_game(|_, m, _| png_at(&m.emulator.screen(), SCREEN_SCALE)).await?
    }
}

#[async_trait]
impl Game for OnConsole {
    async fn world(&mut self) -> anyhow::Result<World> {
        on_game(|world, _, _| world.clone()).await
    }

    async fn set_skills(&mut self, skills: Vec<Skill>) -> anyhow::Result<()> {
        on_game(move |world, _, _| world.skills = skills).await
    }

    async fn set_areas(&mut self, areas: HashMap<String, AreaMap>) -> anyhow::Result<()> {
        on_game(move |world, _, _| world.areas = areas).await
    }

    async fn set_sight(&mut self, sight: Sight) -> anyhow::Result<()> {
        on_game(move |world, _, _| world.sight = sight).await
    }

    async fn save_state(&mut self) -> anyhow::Result<Vec<u8>> {
        on_game(|_, m, _| m.emulator.save_state()).await?
    }

    async fn load_state(&mut self, state: Vec<u8>) -> anyhow::Result<()> {
        on_game(move |_, m, _| {
            m.emulator.load_state(&state)?;
            // One frame, so the screen shows the saved game.
            m.run(1);
            Ok(())
        })
        .await?
    }
}
