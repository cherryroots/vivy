//! `/games gameboy ...`: playing the Game Boy by hand, for admins. There's one Game Boy for
//! the whole bot, and it only moves when someone presses something here, or while she
//! plays by herself (`playing.rs`: `play`, `stop`, `status`, `dashboard`).
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
//! - `task`: a cheap model (the supervisor) carries out one short task by itself, like
//!   "go downstairs, until you're on the floor below"
//! - `skills`: the skills she has written for this game, and forgetting one
//! - `play`, `stop`, `status`, `dashboard`: she plays by herself for a while (`playing.rs`)
//! - `save` and `restore`: save states, in a folder per ROM under `saves_dir`
//! - `off`: take the ROM out

use std::path::PathBuf;

use anyhow::Context as _;
use twilight_interactions::command::{CommandModel, CreateCommand};

use super::console::{Machine, RECENT_SECONDS, RECORD_EVERY, Timing, console};
use super::map::LEGEND;
use super::player::OnConsole;
use super::playing::{self, not_playing};
use super::supervisor::run_task;
use super::walk::{Look, Walked, explore, look_for, walk_to};
use super::world::View;
use super::{Settings, files, pictures, presses, skills};
use crate::core::interaction::{Choice, Typed, choice};
use crate::core::{CommandCtx, Result, user_error};
use crate::util::discord::Draft;

/// Pictures in GIFs are this many times the screen's size (320×288). Smaller than PNGs,
/// since a GIF has many of them.
pub(super) const GIF_SCALE: u32 = 2;

/// Steps shown each way around her on the map.
const MAP_RADIUS: i32 = 12;

/// Steps one `walk` takes at most.
const WALK_MOST: u32 = 60;

/// Actions one `task` takes, unless told otherwise.
const TASK_ACTIONS: u32 = 30;
/// Lines of what the supervisor did shown after a task.
const TASK_LINES: usize = 6;

/// `/games gameboy`: each subcommand is a variant, with a struct of its options.
#[derive(CommandModel, CreateCommand)]
#[command(name = "gameboy", desc = "Play the Game Boy by hand (admins)")]
pub enum GameboyCommand {
    #[command(name = "load")]
    Load(GameboyLoad),
    #[command(name = "press")]
    Press(GameboyPress),
    #[command(name = "wait")]
    Wait(GameboyWait),
    #[command(name = "screen")]
    Screen(GameboyScreen),
    #[command(name = "gif")]
    Gif(GameboyGif),
    #[command(name = "look")]
    Look(GameboyLook),
    #[command(name = "walk")]
    Walk(GameboyWalk),
    #[command(name = "task")]
    Task(GameboyTask),
    #[command(name = "skills")]
    Skills(GameboySkills),
    #[command(name = "play")]
    Play(playing::GameboyPlay),
    #[command(name = "stop")]
    Stop(playing::GameboyStop),
    #[command(name = "status")]
    Status(playing::GameboyStatus),
    #[command(name = "dashboard")]
    Dashboard(playing::GameboyDashboard),
    #[command(name = "save")]
    Save(GameboySave),
    #[command(name = "restore")]
    Restore(GameboyRestore),
    #[command(name = "off")]
    Off(GameboyOff),
}

pub async fn gameboy(ctx: &CommandCtx, command: GameboyCommand) -> Result<()> {
    match command {
        GameboyCommand::Load(args) => load(ctx, args).await,
        GameboyCommand::Press(args) => press(ctx, args).await,
        GameboyCommand::Wait(args) => wait(ctx, args).await,
        GameboyCommand::Screen(_) => screen(ctx).await,
        GameboyCommand::Gif(args) => gif(ctx, args).await,
        GameboyCommand::Look(_) => look(ctx).await,
        GameboyCommand::Walk(args) => walk(ctx, args).await,
        GameboyCommand::Task(args) => task(ctx, args).await,
        GameboyCommand::Skills(args) => skills(ctx, args).await,
        GameboyCommand::Play(args) => playing::play(ctx, args).await,
        GameboyCommand::Stop(_) => playing::stop(ctx).await,
        GameboyCommand::Status(_) => playing::status(ctx).await,
        GameboyCommand::Dashboard(_) => playing::dashboard(ctx).await,
        GameboyCommand::Save(args) => save(ctx, args).await,
        GameboyCommand::Restore(args) => restore(ctx, args).await,
        GameboyCommand::Off(_) => off(ctx).await,
    }
}

/// Suggestions for `load`'s ROM and the save names of `save` and `restore`.
pub async fn autocomplete(ctx: &CommandCtx, typed: &Typed) -> Vec<Choice> {
    match typed.option.as_str() {
        "rom" => rom_names(ctx, &typed.value).await,
        "name" => save_names(ctx, &typed.value).await,
        _ => Vec::new(),
    }
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "load", desc = "Put a ROM in the Game Boy and switch it on")]
pub struct GameboyLoad {
    /// A ROM from the roms folder
    #[command(autocomplete = true)]
    rom: String,
}

async fn load(ctx: &CommandCtx, args: GameboyLoad) -> Result<()> {
    let GameboyLoad { rom } = args;
    let settings = changes(ctx)?;
    let path = files::rom_path(&settings.roms_dir(), &rom)
        .ok_or_else(|| user_error(format!("There's no ROM called `{rom}` in the roms folder.")))?;
    ctx.defer().await?;
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let known = read_skills(&settings, &rom).await?;
    let name = rom.clone();
    let (title, gif) = console()
        .with(move |machine| -> Result<_> {
            let mut m = Machine::new(&name, &bytes).map_err(|e| user_error(format!("{e:#}")))?;
            m.world.skills = known;
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
        Draft::new()
            .content(format!(
                "🎮 **{title}** is on. Press buttons with `/games gameboy press`."
            ))
            .file("gameboy.gif", gif),
    )
    .await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "press",
    desc = "Press buttons, one after another, and see what happened"
)]
pub struct GameboyPress {
    /// Like `a a up*3 start`: a, b, start, select, up, down, left, right, a+b together, *3 repeats, wait
    buttons: String,
    /// Frames each press is held (default 16, one step in most games)
    #[command(min_value = 1, max_value = 120)]
    hold: Option<i64>,
    /// Frames with nothing pressed after each press (default 16)
    #[command(min_value = 0, max_value = 240)]
    gap: Option<i64>,
}

async fn press(ctx: &CommandCtx, args: GameboyPress) -> Result<()> {
    let GameboyPress { buttons, hold, gap } = args;
    changes(ctx)?;
    let presses = presses::parse(&buttons).map_err(user_error)?;
    let defaults = Timing::default();
    let timing = Timing {
        hold: hold.map_or(defaults.hold, |frames| frames as u32),
        gap: gap.map_or(defaults.gap, |frames| frames as u32),
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
        Draft::new()
            .content(format!("Pressed `{typed}`."))
            .file("gameboy.gif", gif),
    )
    .await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "wait", desc = "Let the game run with nothing pressed")]
pub struct GameboyWait {
    /// How many seconds (default 3)
    #[command(min_value = 1, max_value = 60)]
    seconds: Option<i64>,
}

async fn wait(ctx: &CommandCtx, args: GameboyWait) -> Result<()> {
    changes(ctx)?;
    let seconds = args.seconds.map_or(3, |seconds| seconds as u32);
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
        Draft::new()
            .content(format!("Waited {seconds} s{shown}."))
            .file("gameboy.gif", gif),
    )
    .await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "screen", desc = "The screen as it is now")]
pub struct GameboyScreen;

async fn screen(ctx: &CommandCtx) -> Result<()> {
    admin(ctx)?;
    let (png, rom, frames) = on_machine(|m| {
        let png = pictures::png(&m.emulator.screen())?;
        Ok((png, m.rom.clone(), m.emulator.frames()))
    })
    .await?;
    ctx.send(
        Draft::new()
            .content(format!(
                "`{rom}`, {} of play since it was loaded.",
                play_time(frames)
            ))
            .file("gameboy.png", png),
    )
    .await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "gif", desc = "The last few seconds as a GIF")]
pub struct GameboyGif {
    /// How many seconds (default and most: 10)
    #[command(min_value = 1, max_value = 10)]
    seconds: Option<i64>,
}

async fn gif(ctx: &CommandCtx, args: GameboyGif) -> Result<()> {
    admin(ctx)?;
    let seconds = args
        .seconds
        .map_or(RECENT_SECONDS, |seconds| seconds as u32)
        .min(RECENT_SECONDS);
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
    ctx.send(Draft::new().file("gameboy.gif", gif)).await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "look",
    desc = "What code reads from the game: where she is, the map so far, the words on screen"
)]
pub struct GameboyLook;

async fn look(ctx: &CommandCtx) -> Result<()> {
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

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "walk",
    desc = "Walk to a step on the map (code finds the way), or explore without one"
)]
pub struct GameboyWalk {
    /// The step's x, as `look` shows it (leave both out to explore)
    x: Option<i64>,
    /// The step's y, as `look` shows it
    y: Option<i64>,
}

async fn walk(ctx: &CommandCtx, args: GameboyWalk) -> Result<()> {
    let (x, y) = (args.x.map(|x| x as i32), args.y.map(|y| y as i32));
    changes(ctx)?;
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
    ctx.send(Draft::new().content(text).file("gameboy.gif", gif))
        .await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "task",
    desc = "Have her carry out a short task by herself, with a cheap model choosing each action"
)]
pub struct GameboyTask {
    /// What to do, ending with "until" and what the screen shows when it's done
    task: String,
    /// Actions she may take (30)
    #[command(min_value = 1, max_value = 60)]
    actions: Option<i64>,
}

async fn task(ctx: &CommandCtx, args: GameboyTask) -> Result<()> {
    let GameboyTask { task, actions } = args;
    let settings = changes(ctx)?;
    let provider = ctx
        .bot
        .ai
        .chat()
        .ok_or_else(|| user_error("Tasks need a model: the bot has no AI key."))?;
    ctx.defer().await?;
    let (game, mark, rom, known) = on_machine(|m| {
        game_look(m)?;
        Ok((
            game_name(&m.emulator.info().title),
            m.recorded(),
            m.rom.clone(),
            m.world.skills.clone(),
        ))
    })
    .await?;
    let most = actions.map_or(TASK_ACTIONS, |actions| actions as u32);
    let episode = run_task(provider.as_ref(), &mut OnConsole, &game, &task, None, most).await;
    // What she wrote down is kept even when the task fails partway.
    let now_known = on_machine(|m| Ok(m.world.skills.clone())).await?;
    if now_known != known {
        write_skills(&settings, &rom, &now_known).await?;
    }
    let episode = episode?;
    let (now, gif) = on_machine(move |m| {
        let look = game_look(m)?;
        let mut world = std::mem::take(&mut m.world);
        let view = look(&mut world, m);
        m.world = world;
        let gif = pictures::gif(&m.screens_since(mark), RECORD_EVERY, GIF_SCALE)?;
        Ok((describe(m, &view), gif))
    })
    .await?;
    let head = match &episode.finished {
        Some((true, summary)) => format!("✅ She says it's done: {summary}"),
        Some((false, summary)) => format!("❌ She stopped: {summary}"),
        None => format!("⏱️ She used all {most} actions."),
    };
    let skipped = episode.log.len().saturating_sub(TASK_LINES);
    let mut lines = vec![
        head,
        format!(
            "-# {} actions, {} model calls",
            episode.actions, episode.requests
        ),
    ];
    if skipped > 0 {
        lines.push(format!("… {skipped} earlier actions"));
    }
    lines.extend(episode.log[skipped..].iter().map(|line| {
        let line: String = line.chars().take(160).collect();
        format!("- {line}")
    }));
    let mut text = format!("{}\n\n{now}", lines.join("\n"));
    if text.chars().count() > 1990 {
        text = text.chars().take(1990).collect::<String>() + "…";
    }
    ctx.send(Draft::new().content(text).file("gameboy.gif", gif))
        .await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "skills",
    desc = "Show the skills she has written for this game"
)]
pub struct GameboySkills {
    /// A skill to forget, by its name
    forget: Option<String>,
}

async fn skills(ctx: &CommandCtx, args: GameboySkills) -> Result<()> {
    let GameboySkills { forget } = args;
    let settings = if forget.is_some() {
        changes(ctx)?
    } else {
        admin(ctx)?
    };
    let (rom, forgot, list) = on_machine(move |m| {
        let forgot = forget.map(|name| skills::forget(&mut m.world.skills, &name).ok_or(name));
        Ok((m.rom.clone(), forgot, m.world.skills.clone()))
    })
    .await?;
    let mut text = match forgot {
        Some(Ok(skill)) => {
            write_skills(&settings, &rom, &list).await?;
            format!("🗑️ Forgot **{}**.\n\n", skill.name)
        }
        Some(Err(name)) => return Err(user_error(format!("She has no skill called `{name}`."))),
        None => String::new(),
    };
    if list.is_empty() {
        text.push_str("She hasn't written any skills for this game yet.");
    } else {
        let written: Vec<String> = list.iter().map(skills::Skill::written).collect();
        text.push_str(&written.join("\n\n"));
    }
    if text.chars().count() > 1990 {
        text = text.chars().take(1990).collect::<String>() + "…";
    }
    ctx.say(text).await?;
    Ok(())
}

/// Her skills for `rom`, from their file (none when there isn't one yet).
async fn read_skills(settings: &Settings, rom: &str) -> Result<Vec<skills::Skill>> {
    let path = files::skills_path(&settings.saves_dir(), rom);
    match tokio::fs::read_to_string(&path).await {
        Ok(json) => Ok(skills::from_json(&json)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    }
}

async fn write_skills(settings: &Settings, rom: &str, list: &[skills::Skill]) -> Result<()> {
    let path = files::skills_path(&settings.saves_dir(), rom);
    write_file(&path, serde_json::to_string_pretty(list)?.as_bytes()).await
}

/// A ROM's title as a game's name: `POKEMON RED` is Pokémon Red.
pub(super) fn game_name(title: &str) -> String {
    let words: Vec<String> = title
        .split_whitespace()
        .map(|word| match word {
            "POKEMON" => "Pokémon".to_string(),
            _ => {
                let mut chars = word.chars();
                chars.next().map_or(String::new(), |first| {
                    first.to_string() + &chars.as_str().to_lowercase()
                })
            }
        })
        .collect();
    if words.is_empty() {
        "an unnamed game".to_string()
    } else {
        words.join(" ")
    }
}

/// How code looks at this game, or a friendly no.
pub(super) fn game_look(m: &Machine) -> Result<Look> {
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

#[derive(CommandModel, CreateCommand)]
#[command(name = "save", desc = "Save the game exactly as it is now")]
pub struct GameboySave {
    /// A name for it, like `before Brock`
    #[command(autocomplete = true)]
    name: String,
}

async fn save(ctx: &CommandCtx, args: GameboySave) -> Result<()> {
    let GameboySave { name } = args;
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

#[derive(CommandModel, CreateCommand)]
#[command(name = "restore", desc = "Go back to a saved game")]
pub struct GameboyRestore {
    /// One of this ROM's saves
    #[command(autocomplete = true)]
    name: String,
}

async fn restore(ctx: &CommandCtx, args: GameboyRestore) -> Result<()> {
    let GameboyRestore { name } = args;
    let settings = changes(ctx)?;
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
        Draft::new()
            .content(format!(
                "⏪ Back to `{}`.",
                files::save_name(&name).unwrap_or(&name)
            ))
            .file("gameboy.png", png),
    )
    .await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "off", desc = "Take the ROM out (anything not saved is lost)")]
pub struct GameboyOff;

async fn off(ctx: &CommandCtx) -> Result<()> {
    changes(ctx)?;
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
pub(super) fn admin(ctx: &CommandCtx) -> Result<Settings> {
    if !ctx.bot.is_admin(ctx.author_id()) {
        return Err(user_error("Only admins can use the Game Boy commands."));
    }
    Ok(settings(ctx))
}

pub(super) fn settings(ctx: &CommandCtx) -> Settings {
    ctx.bot
        .config
        .feature::<crate::features::games::Settings>("games")
        .map(|s| s.gameboy)
        .unwrap_or_default()
}

/// For commands that move the game or change what she knows: admins, while she isn't
/// playing by herself.
fn changes(ctx: &CommandCtx) -> Result<Settings> {
    let settings = admin(ctx)?;
    not_playing()?;
    Ok(settings)
}

/// Runs `job` on the machine, or says there's no ROM in.
pub(super) async fn on_machine<T, F>(job: F) -> Result<T>
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

async fn rom_names(ctx: &CommandCtx, partial: &str) -> Vec<Choice> {
    let roms = files::roms(&settings(ctx).roms_dir());
    choices(roms, partial)
}

async fn save_names(ctx: &CommandCtx, partial: &str) -> Vec<Choice> {
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
fn choices(names: Vec<String>, partial: &str) -> Vec<Choice> {
    let partial = partial.to_lowercase();
    names
        .into_iter()
        .filter(|name| name.to_lowercase().contains(&partial))
        .take(25)
        .map(|name| choice(name.clone(), name))
        .collect()
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
