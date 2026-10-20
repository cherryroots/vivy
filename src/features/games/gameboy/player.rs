//! The supervisor's player in the bot: the one Game Boy, on its own thread (`console.rs`).
//! Each call is a job on that thread, with the world (area maps, movement log) the machine
//! keeps.

use anyhow::anyhow;
use async_trait::async_trait;
use serde_json::Value;

use super::actions::{self, Acted, SCREEN_SCALE};
use super::console::{Machine, console};
use super::pictures::png_at;
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
            let look =
                look_for(m.emulator.info()).ok_or_else(|| anyhow!("code can't read this game"))?;
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
