//! `/games gameboy ...`: playing the Game Boy by hand, for admins. There's one Game Boy for
//! the whole bot, and it only moves when someone presses something here.
//!
//! - `load`: put a ROM from `roms_dir` in and switch it on
//! - `press`: press buttons, like `a a up*3 start`, and see a GIF of what happened
//! - `wait`: let it run with nothing pressed
//! - `screen`: the screen as it is now
//! - `gif`: the last few seconds as a GIF
//! - `look`: what code reads from the game: where she is, the map so far, the words on
//!   screen and the last steps (Pokémon Red and Blue only, for now)
//! - `walk`: walk to a step on the map, with code finding the way, or to the nearest
//!   step not seen yet
//! - `save` and `restore`: save states, in a folder per ROM under `saves_dir`
//! - `off`: take the ROM out

use std::path::PathBuf;

use anyhow::Context as _;
use poise::CreateReply;
use serenity::all::{AutocompleteChoice, CreateAttachment, CreateAutocompleteResponse};

use super::console::{Machine, RECENT_SECONDS, RECORD_EVERY, Timing, console};
use super::map::LEGEND;
use super::walk::{Look, Walked, explore, look_for, walk_to};
use super::world::View;
use super::{Settings, files, pictures, presses};
use crate::core::{Context, Result, user_error};

/// Pictures in GIFs are this many times the screen's size (320×288). Smaller than PNGs,
/// since a GIF has many of them.
const GIF_SCALE: u32 = 2;

/// Steps shown each way around her on the map.
const MAP_RADIUS: i32 = 12;

/// Steps one `walk` takes at most.
const WALK_MOST: u32 = 60;

/// Play the Game Boy by hand (admins)
#[poise::command(
    slash_command,
    rename = "gameboy",
    subcommands(
        "load", "press", "wait", "screen", "gif", "look", "walk", "save", "restore", "off"
    ),
    subcommand_required
)]
pub async fn gameboy(_ctx: Context<'_>) -> Result<()> {
    Ok(())
}

/// Put a ROM in the Game Boy and switch it on
#[poise::command(slash_command)]
async fn load(
    ctx: Context<'_>,
    #[description = "A ROM from the roms folder"]
    #[autocomplete = "rom_names"]
    rom: String,
) -> Result<()> {
    let settings = admin(ctx)?;
    let path = files::rom_path(&settings.roms_dir(), &rom)
        .ok_or_else(|| user_error(format!("There's no ROM called `{rom}` in the roms folder.")))?;
    ctx.defer().await?;
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let name = rom.clone();
    let (title, gif) = console()
        .with(move |machine| -> Result<_> {
            let mut m = Machine::new(&name, &bytes).map_err(|e| user_error(format!("{e:#}")))?;
            // The boot logo and the first moments of the game.
            m.run(4 * 60);
            let gif = pictures::gif(&m.screens_since(0), RECORD_EVERY, GIF_SCALE)?;
            let title = m.emulator.info().title.clone();
            *machine = Some(m);
            Ok((title, gif))
        })
        .await??;
    let title = if title.is_empty() { rom.clone() } else { title };
    ctx.send(
        CreateReply::default()
            .content(format!(
                "🎮 **{title}** is on. Press buttons with `/games gameboy press`."
            ))
            .attachment(CreateAttachment::bytes(gif, "gameboy.gif")),
    )
    .await?;
    Ok(())
}

/// Press buttons, one after another, and see what happened
#[poise::command(slash_command)]
async fn press(
    ctx: Context<'_>,
    #[description = "Like `a a up*3 start`: a, b, start, select, up, down, left, right, a+b together, *3 repeats, wait"]
    buttons: String,
    #[description = "Frames each press is held (default 16, one step in most games)"]
    #[min = 1]
    #[max = 120]
    hold: Option<u32>,
    #[description = "Frames with nothing pressed after each press (default 16)"]
    #[max = 240]
    gap: Option<u32>,
) -> Result<()> {
    admin(ctx)?;
    let presses = presses::parse(&buttons).map_err(user_error)?;
    let defaults = Timing::default();
    let timing = Timing {
        hold: hold.unwrap_or(defaults.hold),
        gap: gap.unwrap_or(defaults.gap),
    };
    ctx.defer().await?;
    let typed = presses::describe(&presses);
    let gif = on_machine(move |m| {
        let mark = m.recorded();
        m.press(&presses, timing);
        // A beat afterwards, to see what the last press did.
        m.run(30);
        pictures::gif(&m.screens_since(mark), RECORD_EVERY, GIF_SCALE)
    })
    .await?;
    ctx.send(
        CreateReply::default()
            .content(format!("Pressed `{typed}`."))
            .attachment(CreateAttachment::bytes(gif, "gameboy.gif")),
    )
    .await?;
    Ok(())
}

/// Let the game run with nothing pressed
#[poise::command(slash_command)]
async fn wait(
    ctx: Context<'_>,
    #[description = "How many seconds (default 3)"]
    #[min = 1]
    #[max = 60]
    seconds: Option<u32>,
) -> Result<()> {
    admin(ctx)?;
    let seconds = seconds.unwrap_or(3);
    ctx.defer().await?;
    let gif = on_machine(move |m| {
        let mark = m.recorded();
        m.run(seconds * 60);
        pictures::gif(&m.screens_since(mark), RECORD_EVERY, GIF_SCALE)
    })
    .await?;
    let shown = if seconds > RECENT_SECONDS {
        format!(" (the GIF is the last {RECENT_SECONDS})")
    } else {
        String::new()
    };
    ctx.send(
        CreateReply::default()
            .content(format!("Waited {seconds} s{shown}."))
            .attachment(CreateAttachment::bytes(gif, "gameboy.gif")),
    )
    .await?;
    Ok(())
}

/// The screen as it is now
#[poise::command(slash_command)]
async fn screen(ctx: Context<'_>) -> Result<()> {
    admin(ctx)?;
    let (png, rom, frames) = on_machine(|m| {
        let png = pictures::png(&m.emulator.screen())?;
        Ok((png, m.rom.clone(), m.emulator.frames()))
    })
    .await?;
    ctx.send(
        CreateReply::default()
            .content(format!(
                "`{rom}`, {} of play since it was loaded.",
                play_time(frames)
            ))
            .attachment(CreateAttachment::bytes(png, "gameboy.png")),
    )
    .await?;
    Ok(())
}

/// The last few seconds as a GIF
#[poise::command(slash_command)]
async fn gif(
    ctx: Context<'_>,
    #[description = "How many seconds (default and most: 10)"]
    #[min = 1]
    #[max = 10]
    seconds: Option<u32>,
) -> Result<()> {
    admin(ctx)?;
    let seconds = seconds.unwrap_or(RECENT_SECONDS).min(RECENT_SECONDS);
    ctx.defer().await?;
    let gif = on_machine(move |m| {
        let screens = (f64::from(seconds) * 60.0 / f64::from(RECORD_EVERY)) as u64;
        let shown = m.screens_since(m.recorded().saturating_sub(screens));
        if shown.is_empty() {
            return Err(user_error(
                "Nothing has happened yet. Press something first.",
            ));
        }
        pictures::gif(&shown, RECORD_EVERY, GIF_SCALE)
    })
    .await?;
    ctx.send(CreateReply::default().attachment(CreateAttachment::bytes(gif, "gameboy.gif")))
        .await?;
    Ok(())
}

/// What code reads from the game: where she is, the map so far, the words on screen
#[poise::command(slash_command)]
async fn look(ctx: Context<'_>) -> Result<()> {
    admin(ctx)?;
    let text = on_machine(|m| {
        let look = game_look(m)?;
        let mut world = std::mem::take(&mut m.world);
        let view = look(&mut world, m);
        m.world = world;
        Ok(describe(m, &view))
    })
    .await?;
    ctx.say(text).await?;
    Ok(())
}

/// Walk to a step on the map (code finds the way), or explore without one
#[poise::command(slash_command)]
async fn walk(
    ctx: Context<'_>,
    #[description = "The step's x, as `look` shows it (leave both out to explore)"] x: Option<i32>,
    #[description = "The step's y, as `look` shows it"] y: Option<i32>,
) -> Result<()> {
    admin(ctx)?;
    ctx.defer().await?;
    let (text, gif) = on_machine(move |m| {
        let look = game_look(m)?;
        let mark = m.recorded();
        let mut world = std::mem::take(&mut m.world);
        let walk = match (x, y) {
            (Some(x), Some(y)) => walk_to(&mut world, m, look, (x, y), WALK_MOST),
            (None, None) => explore(&mut world, m, look, WALK_MOST),
            _ => {
                m.world = world;
                return Err(user_error("Give both x and y, or neither to explore."));
            }
        };
        m.world = world;
        let result = match &walk.result {
            Walked::Arrived => "Got there".to_string(),
            Walked::NewArea(area) => format!("Walked into {area}"),
            Walked::Interrupted(words) if words.is_empty() => "Something stopped her".to_string(),
            Walked::Interrupted(words) => format!("Stopped by: {}", words.replace('\n', " ")),
            Walked::NoWay if x.is_none() => "Everything she can reach here is seen".to_string(),
            Walked::NoWay => "The map knows no way there".to_string(),
            Walked::Stuck => "Couldn't get there".to_string(),
        };
        let steps = if walk.steps == 1 { "step" } else { "steps" };
        let text = format!(
            "🚶 {result} ({} {steps}).\n{}",
            walk.steps,
            describe(m, &walk.view)
        );
        let gif = pictures::gif(&m.screens_since(mark), RECORD_EVERY, GIF_SCALE)?;
        Ok((text, gif))
    })
    .await?;
    ctx.send(
        CreateReply::default()
            .content(text)
            .attachment(CreateAttachment::bytes(gif, "gameboy.gif")),
    )
    .await?;
    Ok(())
}

/// How code looks at this game, or a friendly no.
fn game_look(m: &Machine) -> Result<Look> {
    look_for(m.emulator.info()).ok_or_else(|| {
        user_error("Code can only read Pokémon Red and Blue so far. Other games come later.")
    })
}

/// A look as a message: the facts, the map around her, the words on screen, the last
/// steps. Kept under Discord's 2000 characters.
fn describe(m: &Machine, view: &View) -> String {
    let mut parts = Vec::new();
    if !view.facts.is_empty() {
        parts.push(view.facts.clone());
    }
    if let Some(map) = m.world.areas.get(&view.area) {
        let mut radius = MAP_RADIUS;
        let drawn = loop {
            let drawn = map.draw(view.at, radius);
            if drawn.len() < 900 || radius <= 4 {
                break drawn;
            }
            radius -= 2;
        };
        // Subtext (`-#`) only works at the start of a line.
        parts.push(format!("```\n{drawn}```\n-# {LEGEND}"));
    }
    if !view.text.is_empty() {
        parts.push(format!("On screen: {}", view.text.replace('\n', " / ")));
    }
    let trust = m.world.trust;
    if trust.disagreed > 0 {
        parts.push(format!(
            "⚠️ The game's memory disagreed with the screen {} times, so its facts are left out.",
            trust.disagreed
        ));
    } else if !view.facts.is_empty() && !trust.trusted() {
        parts.push(format!(
            "-# Memory checked against the screen on {} of {} steps so far.",
            trust.agreed,
            super::red::Trust::NEEDED
        ));
    }
    let recent = m.world.recent(5);
    if !recent.is_empty() {
        parts.push(format!("Last steps:\n{}", recent.join("\n")));
    }
    if parts.is_empty() {
        // The title screen, a menu before the game starts: nothing code can read.
        parts.push(
            "Code can't see her on a map and there are no words on screen (the title screen, \
             or a menu?). `/games gameboy screen` shows what's there."
                .to_string(),
        );
    }
    let mut text = parts.join("\n");
    if text.chars().count() > 1900 {
        text = text.chars().take(1900).collect::<String>() + "…";
    }
    text
}

/// Save the game exactly as it is now
#[poise::command(slash_command)]
async fn save(
    ctx: Context<'_>,
    #[description = "A name for it, like `before Brock`"]
    #[autocomplete = "save_names"]
    name: String,
) -> Result<()> {
    let settings = admin(ctx)?;
    let (rom, state) = on_machine(|m| Ok((m.rom.clone(), m.emulator.save_state()?))).await?;
    let path = files::save_path(&settings.saves_dir(), &rom, &name).map_err(user_error)?;
    write_file(&path, &state).await?;
    ctx.say(format!(
        "💾 Saved `{}` for `{rom}`.",
        files::save_name(&name).unwrap_or(&name)
    ))
    .await?;
    Ok(())
}

/// Go back to a saved game
#[poise::command(slash_command)]
async fn restore(
    ctx: Context<'_>,
    #[description = "One of this ROM's saves"]
    #[autocomplete = "save_names"]
    name: String,
) -> Result<()> {
    let settings = admin(ctx)?;
    let rom = on_machine(|m| Ok(m.rom.clone())).await?;
    let path = files::save_path(&settings.saves_dir(), &rom, &name).map_err(user_error)?;
    let state = match tokio::fs::read(&path).await {
        Ok(state) => state,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(user_error(format!("`{rom}` has no save called `{name}`.")));
        }
        Err(e) => return Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    };
    ctx.defer().await?;
    let png = on_machine(move |m| {
        m.emulator
            .load_state(&state)
            .map_err(|e| user_error(format!("That save doesn't load: {e:#}")))?;
        m.clear_recording();
        // One frame, so the screen shows the saved game rather than the one before.
        m.run(1);
        pictures::png(&m.emulator.screen())
    })
    .await?;
    ctx.send(
        CreateReply::default()
            .content(format!(
                "⏪ Back to `{}`.",
                files::save_name(&name).unwrap_or(&name)
            ))
            .attachment(CreateAttachment::bytes(png, "gameboy.png")),
    )
    .await?;
    Ok(())
}

/// Take the ROM out (anything not saved is lost)
#[poise::command(slash_command)]
async fn off(ctx: Context<'_>) -> Result<()> {
    admin(ctx)?;
    let rom = console()
        .with(|machine| machine.take().map(|m| m.rom))
        .await?;
    ctx.say(match rom {
        Some(rom) => format!("Switched off `{rom}`."),
        None => "The Game Boy was already off.".to_string(),
    })
    .await?;
    Ok(())
}

/// Only admins play by hand. Gives the settings, since every command needs a folder.
fn admin(ctx: Context<'_>) -> Result<Settings> {
    if !ctx.data().is_admin(ctx.author().id) {
        return Err(user_error("Only admins can use the Game Boy commands."));
    }
    Ok(settings(ctx))
}

fn settings(ctx: Context<'_>) -> Settings {
    ctx.data()
        .config
        .feature::<crate::features::games::Settings>("games")
        .map(|s| s.gameboy)
        .unwrap_or_default()
}

/// Runs `job` on the machine, or says there's no ROM in.
async fn on_machine<T, F>(job: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut Machine) -> Result<T> + Send + 'static,
{
    console()
        .with(move |machine| match machine {
            Some(m) => job(m),
            None => Err(user_error(
                "The Game Boy is off. Put a ROM in with `/games gameboy load`.",
            )),
        })
        .await?
}

async fn write_file(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    if let Some(folder) = path.parent() {
        tokio::fs::create_dir_all(folder)
            .await
            .with_context(|| format!("making {}", folder.display()))?;
    }
    tokio::fs::write(path, bytes)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Frames of play as minutes and seconds, like `3 min 20 s`.
fn play_time(frames: u64) -> String {
    let seconds = (frames as f64 / super::emulator::FPS).round() as u64;
    match (seconds / 60, seconds % 60) {
        (0, s) => format!("{s} s"),
        (m, s) => format!("{m} min {s} s"),
    }
}

async fn rom_names(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let roms = files::roms(&settings(ctx).roms_dir());
    choices(roms, partial)
}

async fn save_names(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let saves_dir = settings(ctx).saves_dir();
    let saves = console()
        .with(|machine| machine.as_ref().map(|m| m.rom.clone()))
        .await
        .ok()
        .flatten()
        .map(|rom| files::saves(&saves_dir, &rom))
        .unwrap_or_default();
    choices(saves, partial)
}

/// The names containing what was typed so far, at most Discord's 25.
fn choices(names: Vec<String>, partial: &str) -> CreateAutocompleteResponse {
    let partial = partial.to_lowercase();
    CreateAutocompleteResponse::new().set_choices(
        names
            .into_iter()
            .filter(|name| name.to_lowercase().contains(&partial))
            .take(25)
            .map(|name| AutocompleteChoice::new(name.clone(), name))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_time_reads_like_a_clock() {
        assert_eq!(play_time(0), "0 s");
        assert_eq!(play_time(600), "10 s");
        assert_eq!(play_time(60 * 200), "3 min 21 s");
    }

    #[test]
    fn a_look_at_nothing_still_says_something_and_the_legend_is_subtext() {
        let mut m = Machine::new(
            "test.gb",
            &crate::features::games::gameboy::test_rom::build(),
        )
        .unwrap();
        let mut view = View {
            area: "map 0".to_string(),
            at: (0, 0),
            can_walk: false,
            text: String::new(),
            facts: String::new(),
        };
        assert!(describe(&m, &view).starts_with("Code can't see her on a map"));

        m.world.area("Room").walked((1, 1));
        view.area = "Room".to_string();
        let text = describe(&m, &view);
        assert!(text.contains("```\n-# @ you"), "{text}");
    }

    #[test]
    #[ignore = "needs a ROM in VIVY_GB_ROM"]
    fn look_and_walk_read_well_on_a_real_game() {
        let mut m = crate::features::games::gameboy::red::tests::new_game();
        let look = game_look(&m).unwrap();
        let mut world = std::mem::take(&mut m.world);
        let view = look(&mut world, &mut m);
        m.world = world;
        let first = describe(&m, &view);
        println!("{first}\n---");
        assert!(first.contains("Red's House 2F") && first.chars().count() < 2000);

        let mark = m.recorded();
        let mut world = std::mem::take(&mut m.world);
        let walk = walk_to(&mut world, &mut m, look, (7, 1), WALK_MOST);
        m.world = world;
        assert_eq!(walk.result, Walked::NewArea("Red's House 1F".to_string()));
        let after = describe(&m, &walk.view);
        println!("{after}");
        assert!(after.contains("Last steps:") && after.chars().count() < 2000);
        let gif = pictures::gif(&m.screens_since(mark), RECORD_EVERY, GIF_SCALE).unwrap();
        if let Ok(out) = std::env::var("VIVY_GB_OUT") {
            std::fs::write(format!("{out}/walk.gif"), &gif).unwrap();
            std::fs::write(format!("{out}/look.txt"), format!("{first}\n\n{after}\n")).unwrap();
        }
    }
}
