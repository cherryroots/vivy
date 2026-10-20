//! `/games gameboy play`, `stop` and `status`: she plays by herself (`session.rs`) in the
//! background. The reply to `play` is the live message: a GIF of the last seconds of play,
//! replaced every `live_seconds`, with what she's doing now. A thread under it gets a
//! message per episode and the summary at the end, so it reads in order afterwards. While she plays, the commands that move the game are refused, so nobody
//! presses buttons under her.

use std::sync::Mutex;
use std::time::Duration;

use poise::CreateReply;
use serenity::all::{
    AutoArchiveDuration, ChannelId, CreateAllowedMentions, CreateAttachment, CreateMessage,
    CreateThread, EditAttachments, EditMessage, Message,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::budget::Today;
use super::commands::{GIF_SCALE, admin, game_look, game_name, on_machine};
use super::console::{RECENT_SECONDS, RECORD_EVERY, console};
use super::journal::{DEFAULT_GOAL, Journal};
use super::player::OnConsole;
use super::session::{self, Limits, Session, Summary};
use super::{Settings, files, pictures};
use crate::core::{BotCtx, Context, Result, user_error};

/// Episode lines `status` shows.
const LINES_SHOWN: usize = 8;
/// The live GIF is replaced at most this often, in seconds.
const LIVE_FASTEST: u64 = 5;
/// Episodes one session plays at most.
const EPISODES_MOST: u32 = 200;

/// Cancels the session that's playing, if one is.
static PLAYING: Mutex<Option<CancellationToken>> = Mutex::new(None);

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

/// Let her play by herself for a while, choosing her own tasks
#[poise::command(slash_command)]
pub async fn play(
    ctx: Context<'_>,
    #[description = "How many minutes (default 60)"]
    #[min = 5]
    #[max = 240]
    minutes: Option<u32>,
    #[description = "Her goal, kept for next time (default: play through to the credits)"]
    goal: Option<String>,
    #[description = "Episodes at most, a short task each (200)"]
    #[min = 1]
    #[max = 200]
    episodes: Option<u32>,
) -> Result<()> {
    let settings = admin(ctx)?;
    if ctx.data().ai.chat().is_none() {
        return Err(user_error("Playing needs a model: the bot has no AI key."));
    }
    let (rom, game) = on_machine(|m| {
        game_look(m)?;
        Ok((m.rom.clone(), game_name(&m.emulator.info().title)))
    })
    .await?;
    let stop = {
        let mut playing = PLAYING.lock().unwrap();
        if playing.is_some() {
            return Err(user_error(
                "She's already playing. `/games gameboy stop` stops her.",
            ));
        }
        let stop = ctx.data().shutdown.child_token();
        *playing = Some(stop.clone());
        stop
    };
    let guard = Playing;
    let limits = Limits {
        minutes: minutes.unwrap_or(settings.session_minutes),
        episodes: episodes.unwrap_or(EPISODES_MOST),
        daily_usd: settings.daily_usd,
        free_tokens: settings.free_tokens,
    };
    let journal = read_journal(&settings, &rom).await;
    let shown_goal = goal
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .or(journal
            .as_ref()
            .map(|j| j.goal.as_str())
            .filter(|g| !g.is_empty()))
        .unwrap_or(DEFAULT_GOAL)
        .to_string();
    let planner = if ctx.data().ai.openai.is_some() {
        "the strategist plans on the free OpenAI tokens"
    } else {
        "the cheap model plans (no OpenAI key)"
    };
    let live = ctx
        .send(
            CreateReply::default()
                .content(format!(
                    "▶️ She's playing **{game}** by herself for up to {} minutes; {planner}.\n\
                     Goal: {shown_goal}\n-# Each episode goes in the thread below. \
                     `/games gameboy stop` stops her, `/games gameboy status` shows her notes.",
                    limits.minutes
                ))
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await?
        .into_message()
        .await?;
    let bot = ctx.data().clone();
    ctx.data().tasks.spawn(async move {
        let _guard = guard;
        run(bot, live, settings, rom, game, limits, goal, stop).await;
    });
    Ok(())
}

/// Stop her playing by herself
#[poise::command(slash_command)]
pub async fn stop(ctx: Context<'_>) -> Result<()> {
    admin(ctx)?;
    let stopped = PLAYING
        .lock()
        .unwrap()
        .as_ref()
        .map(CancellationToken::cancel);
    ctx.say(match stopped {
        Some(()) => "⏹️ Stopping her. The summary follows.",
        None => "She isn't playing by herself.",
    })
    .await?;
    Ok(())
}

/// Her goal, notes and last episodes for this game, and what playing used today
#[poise::command(slash_command)]
pub async fn status(ctx: Context<'_>) -> Result<()> {
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
    ctx.send(
        CreateReply::default()
            .content(text)
            .allowed_mentions(CreateAllowedMentions::new()),
    )
    .await?;
    Ok(())
}

/// Plays the session: the live GIF in `live` (the reply to `play`), a line per episode
/// and the summary in a thread under it.
#[allow(clippy::too_many_arguments)]
async fn run(
    bot: BotCtx,
    live: Message,
    settings: Settings,
    rom: String,
    game: String,
    limits: Limits,
    goal: Option<String>,
    stop: CancellationToken,
) {
    let Some(provider) = bot.ai.chat() else {
        return;
    };
    let name: String = format!("{game} · {}", chrono::Utc::now().format("%-d %b %H:%M"))
        .chars()
        .take(100)
        .collect();
    // Run inside a thread already, or without the permission: the channel itself.
    let log = match live
        .channel_id
        .create_thread_from_message(
            &bot.http,
            live.id,
            CreateThread::new(name).auto_archive_duration(AutoArchiveDuration::OneDay),
        )
        .await
    {
        Ok(thread) => thread.id,
        Err(err) => {
            warn!("couldn't start a thread for the Game Boy session: {err}");
            live.channel_id
        }
    };
    let (report, lines) = mpsc::unbounded_channel();
    let forward = tokio::spawn(post_lines(bot.clone(), log, lines));
    let (doing, now) = watch::channel(String::new());
    let ended = CancellationToken::new();
    let watching = tokio::spawn(live_gif(
        bot.clone(),
        live,
        now,
        ended.clone(),
        settings.live_seconds,
    ));
    let timer = bot
        .timers
        .add("Game Boy", "an episode at a time while she plays");
    let session = Session {
        provider: provider.as_ref(),
        openai: bot.ai.openai.as_deref(),
        spends: &bot.ai.spends,
        game,
        saves_dir: settings.saves_dir(),
        rom,
        limits,
        stop,
        report,
        doing: Some(doing),
        timer: Some(timer.clone()),
    };
    let result = session::play(&session, &mut OnConsole, goal.as_deref()).await;
    timer.ended();
    drop(session);
    ended.cancel();
    let _ = forward.await;
    let _ = watching.await;
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
    if let Err(err) = log.send_message(&bot.http, plain(&text)).await {
        warn!("couldn't post the Game Boy session's summary: {err}");
    }
}

/// Keeps the live message's GIF to the last seconds of play, every `every` seconds while
/// the game has moved, with what she's doing now; at the end, says where the summary is.
async fn live_gif(
    bot: BotCtx,
    mut live: Message,
    now: watch::Receiver<String>,
    ended: CancellationToken,
    every: u64,
) {
    let head = live.content.clone();
    let mut shown = None;
    loop {
        let over = if every == 0 {
            ended.cancelled().await;
            true
        } else {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(every.max(LIVE_FASTEST))) => false,
                () = ended.cancelled() => true,
            }
        };
        let Some((mark, gif)) = last_seconds().await else {
            if over {
                break;
            }
            continue;
        };
        // Nothing moved since the last one (she's thinking): leave it.
        if !over && shown == Some(mark) {
            continue;
        }
        shown = Some(mark);
        let line = if over {
            "⏹️ Session over: the summary is in the thread.".to_string()
        } else {
            format!("🔴 Live · {}", now.borrow().as_str())
        };
        let edit = EditMessage::new()
            .content(short(&format!("{head}\n{line}"), 1990))
            .allowed_mentions(CreateAllowedMentions::new())
            .attachments(EditAttachments::new().add(CreateAttachment::bytes(gif, "live.gif")));
        if let Err(err) = live.edit(&bot.http, edit).await {
            warn!("couldn't update the live Game Boy GIF: {err}");
        }
        if over {
            break;
        }
    }
}

/// Each episode's line as a message of its own, so the thread reads in order.
async fn post_lines(bot: BotCtx, log: ChannelId, mut lines: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = lines.recv().await {
        if let Err(err) = log.send_message(&bot.http, plain(&line)).await {
            warn!("couldn't post a Game Boy episode: {err}");
        }
    }
}

/// A message that pings nobody.
fn plain(text: &str) -> CreateMessage {
    CreateMessage::new()
        .content(short(text, 1990))
        .allowed_mentions(CreateAllowedMentions::new())
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

/// The last seconds of play as a GIF, if the Game Boy is still on, with how many screens
/// it had recorded (to tell whether it moved since). The screens are copied on the Game
/// Boy's thread and made into a GIF off it, so play isn't held up.
async fn last_seconds() -> Option<(u64, Vec<u8>)> {
    let (mark, screens) = console()
        .with(|machine| {
            let m = machine.as_ref()?;
            let screens = u64::from(RECENT_SECONDS) * 60 / u64::from(RECORD_EVERY);
            let shown = m.screens_since(m.recorded().saturating_sub(screens));
            (!shown.is_empty()).then(|| (m.recorded(), shown))
        })
        .await
        .ok()
        .flatten()?;
    let gif = tokio::task::spawn_blocking(move || pictures::gif(&screens, RECORD_EVERY, GIF_SCALE))
        .await
        .ok()?
        .ok()?;
    Some((mark, gif))
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
