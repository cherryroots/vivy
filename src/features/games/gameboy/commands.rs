//! `/games gameboy ...`: playing the Game Boy by hand, for admins. There's one Game Boy for
//! the whole bot, and it only moves when someone presses something here.
//!
//! - `load`: put a ROM from `roms_dir` in and switch it on
//! - `press`: press buttons, like `a a up*3 start`, and see a GIF of what happened
//! - `wait`: let it run with nothing pressed
//! - `screen`: the screen as it is now
//! - `gif`: the last few seconds as a GIF
//! - `save` and `restore`: save states, in a folder per ROM under `saves_dir`
//! - `off`: take the ROM out

use std::path::PathBuf;

use anyhow::Context as _;
use poise::CreateReply;
use serenity::all::{AutocompleteChoice, CreateAttachment, CreateAutocompleteResponse};

use super::console::{Machine, RECENT_SECONDS, RECORD_EVERY, Timing, console};
use super::{Settings, files, pictures, presses};
use crate::core::{Context, Result, user_error};

/// Pictures in GIFs are this many times the screen's size (320×288). Smaller than PNGs,
/// since a GIF has many of them.
const GIF_SCALE: u32 = 2;

/// Play the Game Boy by hand (admins)
#[poise::command(
    slash_command,
    rename = "gameboy",
    subcommands("load", "press", "wait", "screen", "gif", "save", "restore", "off"),
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
}
