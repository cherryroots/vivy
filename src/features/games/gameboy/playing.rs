//! `/games gameboy play`, `stop`, `status` and `dashboard`: she plays by herself
//! (`session.rs`) in the background. The reply to `play` is the dashboard
//! (`dashboard.rs`), kept up to date while she plays. A thread under it is the log: a
//! message per episode, checkpoint (with a picture) and warning, and the summary with a GIF
//! at the end, so it reads in order afterwards. While she plays, the commands that move the
//! game are refused, so nobody presses buttons under her.

use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc::error::SendError;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::warn;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::channel::thread::AutoArchiveDuration;

use super::budget::Today;
use super::commands::{admin, game_look, game_name, on_machine};
use super::dashboard::{self, Board, Live, Parts, last_screen, last_seconds};
use super::journal::{DEFAULT_GOAL, Journal};
use super::narrate::{Narrator, Place};
use super::player::OnConsole;
use super::session::{self, Limits, Now, Report, Session, Summary};
use super::skills::Skill;
use super::{Settings, files};
use crate::core::interaction::{ComponentInteraction, Respond, Response};
use crate::core::{BotCtx, CommandCtx, Result, user_error};
use crate::util::discord::{ChannelId, Draft, Message, no_pings};

/// Episode lines `status` shows.
const LINES_SHOWN: usize = 8;
/// Episodes one session plays at most.
const EPISODES_MOST: u32 = 200;
/// What a dashboard says while she isn't playing.
const RESTING: &str = "⏸️ She isn't playing. `/games gameboy play` starts her.";

/// The session that's playing, if one is: how to stop it, and how to hand its dashboard a
/// new message.
static PLAYING: Mutex<Option<Running>> = Mutex::new(None);

struct Running {
    stop: CancellationToken,
    moves: mpsc::UnboundedSender<Message>,
}

/// Clears [`PLAYING`] when the session ends, however it ends.
struct Playing;

impl Drop for Playing {
    fn drop(&mut self) {
        *PLAYING.lock().unwrap() = None;
    }
}

/// Refuses while she's playing by herself.
pub fn not_playing() -> Result<()> {
    if PLAYING.lock().unwrap().is_some() {
        return Err(user_error(
            "She's playing by herself right now. `/games gameboy stop` stops her first.",
        ));
    }
    Ok(())
}

/// Stops her, if she's playing.
fn stop_playing() -> &'static str {
    let stopped = PLAYING.lock().unwrap().as_ref().map(|r| r.stop.cancel());
    match stopped {
        Some(()) => "⏹️ Stopping her. The summary follows in the log.",
        None => "She isn't playing by herself.",
    }
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "play",
    desc = "Let her play by herself for a while, choosing her own tasks"
)]
pub struct GameboyPlay {
    /// How many minutes (default 60)
    #[command(min_value = 5, max_value = 240)]
    minutes: Option<i64>,
    /// Her goal, kept for next time (default: play through to the credits)
    goal: Option<String>,
    /// Episodes at most, a short task each (200)
    #[command(min_value = 1, max_value = 200)]
    episodes: Option<i64>,
}

pub async fn play(ctx: &CommandCtx, args: GameboyPlay) -> Result<()> {
    let GameboyPlay {
        minutes,
        goal,
        episodes,
    } = args;
    let settings = admin(ctx)?;
    if ctx.bot.ai.chat().is_none() {
        return Err(user_error("Playing needs a model: the bot has no AI key."));
    }
    let guild = ctx
        .guild_id()
        .ok_or_else(|| user_error("Playing needs a server."))?;
    let (rom, game, skills) = on_machine(|m| {
        game_look(m)?;
        Ok((
            m.rom.clone(),
            game_name(&m.emulator.info().title),
            m.world.skills.clone(),
        ))
    })
    .await?;
    let (moves, moved) = mpsc::unbounded_channel();
    let stop = {
        let mut playing = PLAYING.lock().unwrap();
        if playing.is_some() {
            return Err(user_error(
                "She's already playing. `/games gameboy stop` stops her.",
            ));
        }
        let stop = ctx.bot.shutdown.child_token();
        *playing = Some(Running {
            stop: stop.clone(),
            moves,
        });
        stop
    };
    let guard = Playing;
    let limits = Limits {
        minutes: minutes.map_or(settings.session_minutes, |m| m as u32),
        episodes: episodes.map_or(EPISODES_MOST, |e| e as u32),
        daily_usd: settings.daily_usd,
        free_tokens: settings.free_tokens,
    };
    // The dashboard before the first episode, from the journal.
    let journal = read_journal(&settings, &rom).await.unwrap_or_default();
    let today = Today::on(
        read(&files::today_path(&settings.saves_dir())).await,
        chrono::Utc::now(),
    );
    let mut first = Now::default();
    first.read(&journal, &skills, &today);
    first.started = chrono::Utc::now().timestamp();
    first.session = journal.sessions + 1;
    first.goal = goal
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .or(Some(journal.goal.as_str()).filter(|g| !g.is_empty()))
        .unwrap_or(DEFAULT_GOAL)
        .to_string();
    let board = Board {
        game: &game,
        now: &first,
        minutes: limits.minutes,
        daily_usd: settings.daily_usd,
        free_tokens: settings.free_tokens,
        log: None,
        over: None,
    };
    let parts = dashboard::parts(&board, first.started);
    let message = ctx
        .send(dashboard::message(&parts, None, None, false))
        .await?
        .message(ctx)
        .await?;
    let live = Live {
        bot: ctx.bot.clone(),
        game,
        minutes: limits.minutes,
        daily_usd: settings.daily_usd,
        free_tokens: settings.free_tokens,
        guild,
        log: message.channel_id,
    };
    ctx.bot.tasks.spawn(async move {
        let _guard = guard;
        run(
            live, message, moved, first, settings, rom, limits, goal, stop,
        )
        .await;
    });
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "stop", desc = "Stop her playing by herself")]
pub struct GameboyStop;

pub async fn stop(ctx: &CommandCtx) -> Result<()> {
    admin(ctx)?;
    ctx.say(stop_playing()).await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "dashboard",
    desc = "Post her dashboard here: live while she plays, else how her last session ended"
)]
pub struct GameboyDashboard;

pub async fn dashboard(ctx: &CommandCtx) -> Result<()> {
    let settings = admin(ctx)?;
    let moves = PLAYING.lock().unwrap().as_ref().map(|r| r.moves.clone());
    if let Some(moves) = moves {
        let message = ctx
            .send(dashboard::note("➡️ Moving the dashboard here…"))
            .await?
            .message(ctx)
            .await?;
        // She stopped meanwhile: this one shows how she left it.
        if let Err(SendError(message)) = moves.send(message) {
            let (parts, gif) = resting(&settings).await?;
            dashboard::message(&parts, gif, None, true)
                .edit(&ctx.bot.http, message.channel_id, message.id)
                .await?;
        }
        return Ok(());
    }
    let (parts, gif) = resting(&settings).await?;
    ctx.send(dashboard::message(&parts, gif, None, true))
        .await?;
    Ok(())
}

/// The dashboard while she isn't playing, from her files, with her screen if the Game Boy
/// is on.
async fn resting(settings: &Settings) -> Result<(Parts, Option<Vec<u8>>)> {
    let (rom, game, skills) = on_machine(|m| {
        Ok((
            m.rom.clone(),
            game_name(&m.emulator.info().title),
            m.world.skills.clone(),
        ))
    })
    .await?;
    let journal = read_journal(settings, &rom).await.unwrap_or_default();
    let today = Today::on(
        read(&files::today_path(&settings.saves_dir())).await,
        chrono::Utc::now(),
    );
    let mut now = Now::default();
    now.read(&journal, &skills, &today);
    if now.goal.is_empty() {
        now.goal = DEFAULT_GOAL.to_string();
    }
    let board = Board {
        game: &game,
        now: &now,
        minutes: settings.session_minutes,
        daily_usd: settings.daily_usd,
        free_tokens: settings.free_tokens,
        log: None,
        over: Some(RESTING),
    };
    let parts = dashboard::parts(&board, chrono::Utc::now().timestamp());
    Ok((parts, last_screen().await))
}

/// The dashboard's buttons: Stop, and Skills (her skills, for the one who pressed).
pub async fn on_component(bot: &BotCtx, i: &ComponentInteraction, action: &str) -> Result<()> {
    let text = if !bot.is_admin(i.user.id) {
        "Only admins can use the Game Boy buttons.".to_string()
    } else if action == "stop" {
        stop_playing().to_string()
    } else if action == "skills" {
        match on_machine(|m| Ok(m.world.skills.clone())).await {
            Ok(list) => skills_text(&list),
            Err(err) => format!("{err:#}"),
        }
    } else {
        return Ok(());
    };
    let answer = Draft::new()
        .content(text)
        .allowed_mentions(no_pings())
        .ephemeral();
    i.create_response(bot, Response::Message(answer)).await
}

/// Her skills, a line each: the name, when she uses it, and how often she has.
fn skills_text(list: &[Skill]) -> String {
    if list.is_empty() {
        return "She hasn't written any skills for this game yet.".to_string();
    }
    let lines: Vec<String> = list
        .iter()
        .map(|s| {
            let kind = if s.rules.is_empty() {
                "know-how"
            } else {
                "routine"
            };
            format!(
                "- **{}** ({kind}, used {}): {}",
                s.name,
                s.uses,
                short(&s.use_when, 150)
            )
        })
        .collect();
    short(&lines.join("\n"), 1990)
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "status",
    desc = "Her goal, notes and last episodes for this game, and what playing used today"
)]
pub struct GameboyStatus;

pub async fn status(ctx: &CommandCtx) -> Result<()> {
    let settings = admin(ctx)?;
    let rom = on_machine(|m| Ok(m.rom.clone())).await?;
    let journal = read_journal(&settings, &rom).await.unwrap_or_default();
    let today = Today::on(
        read(&files::today_path(&settings.saves_dir())).await,
        chrono::Utc::now(),
    );
    let playing = if not_playing().is_err() {
        "▶️ She's playing now."
    } else {
        "⏸️ She isn't playing."
    };
    let mut parts = vec![format!(
        "{playing} {} episodes of `{rom}` so far.\n-# Today: ${:.2} of ${:.2} on paid models, \
         {}k of {}k free tokens.",
        journal.episodes,
        today.usd,
        settings.daily_usd,
        today.free_tokens / 1000,
        settings.free_tokens / 1000
    )];
    let goal = if journal.goal.is_empty() {
        DEFAULT_GOAL
    } else {
        &journal.goal
    };
    parts.push(format!("**Goal:** {goal}"));
    if !journal.notes.is_empty() {
        let notes: Vec<String> = journal.notes.iter().map(|n| format!("- {n}")).collect();
        parts.push(format!("**Notes:**\n{}", notes.join("\n")));
    }
    if !journal.log.is_empty() {
        let lines: Vec<String> = journal
            .recent(LINES_SHOWN)
            .iter()
            .map(|l| format!("- {}", short(l, 200)))
            .collect();
        parts.push(format!("**Last episodes:**\n{}", lines.join("\n")));
    }
    if journal.stuck > 0 {
        parts.push(format!(
            "-# Nothing new for {} episodes; {} rollbacks since the last progress.",
            journal.stuck, journal.rollbacks
        ));
    }
    let text = short(&parts.join("\n\n"), 1990);
    ctx.send(plain(&text)).await?;
    Ok(())
}

/// Plays the session, with the dashboard in `message` (the reply to `play`) and the log in
/// a thread under it.
#[allow(clippy::too_many_arguments)]
async fn run(
    live: Live,
    mut message: Message,
    moved: mpsc::UnboundedReceiver<Message>,
    first: Now,
    settings: Settings,
    rom: String,
    limits: Limits,
    goal: Option<String>,
    stop: CancellationToken,
) {
    let bot = live.bot.clone();
    let Some(provider) = bot.ai.chat() else {
        return;
    };
    let name: String = format!("{} · Session {}", live.game, first.session)
        .chars()
        .take(100)
        .collect();
    // Run inside a thread already, or without the permission: the channel itself.
    let log = match start_thread(&bot, &message, &name).await {
        Ok(thread) => thread,
        Err(err) => {
            warn!("couldn't start a thread for the Game Boy session: {err:#}");
            message.channel_id
        }
    };
    let live = Live { log, ..live };
    live.show(&mut message, &first, None, None).await;
    let (report, reports) = mpsc::unbounded_channel();
    let (now, watching) = watch::channel(first);
    let ended = CancellationToken::new();
    // Her commentary hears every report the log gets, and speaks in the dashboard's channel.
    let mut heard = None;
    let narrator = (settings.narrate_minutes > 0).then(|| {
        let (tell, told) = mpsc::unbounded_channel();
        heard = Some(tell);
        let place = Place {
            guild: message.guild_id,
            channel: message.channel_id,
            message: message.id,
        };
        let every = Duration::from_secs(settings.narrate_minutes * 60);
        let narrator = Narrator::new(bot.clone(), place, live.game.clone(), every);
        tokio::spawn(narrator.follow(told, watching.clone(), ended.clone()))
    });
    let forward = tokio::spawn(post_reports(bot.clone(), log, reports, heard));
    let timer = bot
        .timers
        .add("Game Boy", "an episode at a time while she plays");
    let session = Session {
        provider: provider.as_ref(),
        openai: bot.ai.openai.as_deref(),
        spends: &bot.ai.spends,
        game: live.game.clone(),
        saves_dir: settings.saves_dir(),
        rom,
        limits,
        stop,
        report,
        now: Some(now),
        timer: Some(timer.clone()),
    };
    let playing = async {
        let result = session::play(&session, &mut OnConsole, goal.as_deref()).await;
        ended.cancel();
        result
    };
    let following = live.follow(
        message,
        watching,
        moved,
        ended.clone(),
        settings.live_seconds,
    );
    let (result, mut message) = tokio::join!(playing, following);
    timer.ended();
    let last = session
        .now
        .as_ref()
        .map(|now| now.borrow().clone())
        .unwrap_or_default();
    // The log's last episode lines go out before the summary.
    drop(session);
    let _ = forward.await;
    let today = Today::on(
        read(&files::today_path(&settings.saves_dir())).await,
        chrono::Utc::now(),
    );
    let text = match result {
        Ok(summary) => summary_text(&summary, &today, settings.daily_usd),
        Err(err) => {
            warn!("the Game Boy session broke off: {err:#}");
            format!("⚠️ Her session broke off: {err:#}")
        }
    };
    let gif = last_seconds().await.map(|(_, gif)| gif);
    let mut summary = plain(&text);
    if let Some(gif) = &gif {
        summary = summary.file("last.gif", gif.clone());
    }
    if let Err(err) = summary.send(&bot.http, log).await {
        warn!("couldn't post the Game Boy session's summary: {err:#}");
    }
    let over = text.trim_start_matches("⏹️ ");
    live.show(&mut message, &last, Some(over), last_screen().await)
        .await;
    if let Some(narrator) = narrator
        && let Ok(narrator) = narrator.await
    {
        narrator.recap(&last, over).await;
    }
}

/// A thread under the dashboard, for the log.
async fn start_thread(bot: &BotCtx, message: &Message, name: &str) -> Result<ChannelId> {
    let thread = bot
        .http
        .create_thread_from_message(message.channel_id, message.id, name)
        .auto_archive_duration(AutoArchiveDuration::Day)
        .await?
        .model()
        .await?;
    Ok(thread.id)
}

/// Each report as a message of its own, so the log reads in order. Each also goes to
/// `heard`, her commentary, if she talks.
async fn post_reports(
    bot: BotCtx,
    log: ChannelId,
    mut reports: mpsc::UnboundedReceiver<Report>,
    heard: Option<mpsc::UnboundedSender<Report>>,
) {
    while let Some(report) = reports.recv().await {
        if let Some(heard) = &heard {
            let _ = heard.send(report.clone());
        }
        let mut message = plain(&report.line);
        if let Some(picture) = report.picture {
            message = message.file("checkpoint.png", picture);
        }
        if let Err(err) = message.send(&bot.http, log).await {
            warn!("couldn't post to the Game Boy log: {err:#}");
        }
    }
}

/// A message that pings nobody.
fn plain(text: &str) -> Draft {
    Draft::new()
        .content(short(text, 1990))
        .allowed_mentions(no_pings())
}

fn summary_text(summary: &Summary, today: &Today, daily_usd: f64) -> String {
    let areas = if summary.new_areas.is_empty() {
        "no new areas".to_string()
    } else {
        format!("new areas: {}", summary.new_areas.join(", "))
    };
    let mut extra = Vec::new();
    if summary.checkpoints > 0 {
        extra.push(format!("{} checkpoints", summary.checkpoints));
    }
    if summary.rollbacks > 0 {
        extra.push(format!("{} rollbacks", summary.rollbacks));
    }
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!(" · {}", extra.join(" · "))
    };
    let cheap = match summary.cheap_plans {
        0 => String::new(),
        n => format!(" · {n} plans by the cheap model"),
    };
    format!(
        "⏹️ **Session over.** {}\n{} episodes · {areas}{extra}\n\
         -# ${:.2} on paid models (today ${:.2} of ${daily_usd:.2}) · {}k free tokens{cheap}",
        summary.ended.line(),
        summary.episodes,
        summary.usd,
        today.usd,
        summary.free_tokens / 1000
    )
}

async fn read_journal(settings: &Settings, rom: &str) -> Option<Journal> {
    read(&files::journal_path(&settings.saves_dir(), rom)).await
}

async fn read<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    let json = tokio::fs::read_to_string(path).await.ok()?;
    serde_json::from_str(&json).ok()
}

fn short(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    text.chars().take(most).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::session::Ended;

    #[test]
    fn the_summary_says_why_it_ended_and_what_it_cost() {
        let summary = Summary {
            ended: Ended::Time,
            episodes: 12,
            new_areas: vec!["Route 1".to_string(), "Viridian City".to_string()],
            checkpoints: 2,
            rollbacks: 0,
            usd: 0.214,
            free_tokens: 182_400,
            cheap_plans: 0,
        };
        let today = Today {
            usd: 0.5,
            ..Today::default()
        };
        assert_eq!(
            summary_text(&summary, &today, 1.5),
            "⏹️ **Session over.** Her time was up.\n12 episodes · new areas: Route 1, \
             Viridian City · 2 checkpoints\n-# $0.21 on paid models (today $0.50 of $1.50) · \
             182k free tokens"
        );
    }
}
