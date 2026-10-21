//! The session runner: she plays by herself, an episode at a time, until a cap. Each
//! episode the strategist reads her journal and picks a short task, the supervisor carries
//! it out, and code counts what was new. Plain code, apart from those two models.
//!
//! - Planning: GPT-6 Astra (or whichever model `[ai.openai.jobs.game_plan]` names) on the
//!   free daily tokens, counted in `budget.rs`; past them, or without OpenAI, the cheap
//!   model plans (`strategist::plan_cheaply`).
//! - Caps: minutes, episodes, and the day's dollars for the paid models (`daily_usd`).
//! - Checkpoints: a save state each time she reaches an area for the first time, with the
//!   notes as they were. The newest few are kept.
//! - The stuck ladder (`journal::Rung`): after 3 episodes with nothing new she's told to
//!   try something different and explore first; at 6 the strategist reviews its notes at
//!   high effort; at 9 she goes back to a checkpoint, keeping her skills, each time one
//!   further back; 3 rollbacks without progress, or 15 episodes, and the session ends.
//! - After every episode the journal, the maps, her skills, the day's use and a `latest`
//!   save are written, so a restart loses one episode at most.
//! - For the channel: a [`Report`] after each episode, checkpoint and warning, and [`Now`],
//!   what she's doing, after every action (the dashboard in `dashboard.rs` shows it).

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::actions::Acted;
use super::budget::Today;
use super::files;
use super::journal::{CHANGE_AT, DEFAULT_GOAL, Journal, ROLLBACK_AT, Rung};
use super::map::AreaMap;
use super::skills::{self, Skill};
use super::strategist::{self, Ask, Calls, Care, Plan};
use super::supervisor::{Episode, Player, args_text, run_task};
use super::world::World;
use crate::ai::{ChatProvider, OpenAi, Spend};
use crate::core::timers::Timer;

/// Actions in one episode.
pub const EPISODE_ACTIONS: u32 = 30;
/// Checkpoints kept.
const CHECKPOINTS_KEPT: usize = 10;
/// Rollbacks in a row without progress before the session ends.
const ROLLBACKS_MOST: u32 = 3;
/// Model failures in a row before the session ends, and the wait after each (a model
/// that's overloaded often answers again after a short while).
const FAILURES_MOST: u32 = 3;
/// (No wait in tests.)
const FAILURE_WAIT: Duration = Duration::from_secs(if cfg!(test) { 0 } else { 30 });
/// Episode lines the strategist sees.
const RECENT_EPISODES: usize = 5;
/// Episode lines and actions [`Now`] keeps for the dashboard.
const EPISODES_SHOWN: usize = 3;
const ACTIONS_SHOWN: usize = 6;
/// The save made after every episode, for carrying on (`/games gameboy restore latest`).
pub const LATEST: &str = "latest";
/// What the strategist is told about the last task before there was one.
const NOTHING_YET: &str = "Nothing yet: this is the first task.";
/// The task when a plan comes back without one.
const EXPLORE: &str = "Explore somewhere you haven't seen yet, until the screen shows a new area.";

/// What the session plays: a game it can act in (`Player`), and save and load.
#[async_trait]
pub trait Game: Player {
    /// What code knows of the world now: the maps and her skills.
    async fn world(&mut self) -> anyhow::Result<World>;
    async fn set_skills(&mut self, skills: Vec<Skill>) -> anyhow::Result<()>;
    async fn set_areas(&mut self, areas: HashMap<String, AreaMap>) -> anyhow::Result<()>;
    async fn save_state(&mut self) -> anyhow::Result<Vec<u8>>;
    async fn load_state(&mut self, state: Vec<u8>) -> anyhow::Result<()>;
}

/// When a session stops.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub minutes: u32,
    pub episodes: u32,
    /// USD a day the paid models may cost while playing.
    pub daily_usd: f64,
    /// The free OpenAI tokens a day.
    pub free_tokens: u64,
}

/// Everything a session needs apart from the game.
pub struct Session<'a> {
    /// The supervisor's model, which also plans once the free tokens are used up.
    pub provider: &'a dyn ChatProvider,
    /// The strategist, if OpenAI is set up.
    pub openai: Option<&'a OpenAi>,
    /// What each provider cost this month, for the day's dollars.
    pub spends: &'a [Spend],
    /// The game's name, for the supervisor.
    pub game: String,
    pub saves_dir: PathBuf,
    pub rom: String,
    pub limits: Limits,
    /// Cancelled by `/games gameboy stop` or a shutdown.
    pub stop: CancellationToken,
    /// A line after each episode, checkpoint and warning, for the log.
    pub report: mpsc::UnboundedSender<Report>,
    /// What she's doing now, for the dashboard.
    pub now: Option<watch::Sender<Now>>,
    /// Shown as running on the status picture while she plays. The caller ends it.
    pub timer: Option<Timer>,
    /// Hints from people watching (the dashboard's Hint button), each like
    /// `Cherry: the Mart is north`. She reads them before planning the next task.
    pub hints: Hints,
}

/// Hints waiting for her next plan. Shared with whoever adds them.
pub type Hints = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// Why a session ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Ended {
    Time,
    Episodes,
    Money,
    Stuck,
    Stopped,
    Failing(String),
}

impl Ended {
    pub fn line(&self) -> String {
        match self {
            Ended::Time => "Her time was up.".to_string(),
            Ended::Episodes => "She played all her episodes.".to_string(),
            Ended::Money => "Today's money for playing is spent.".to_string(),
            Ended::Stuck => "She was stuck, so she stopped rather than spend more.".to_string(),
            Ended::Stopped => "She was stopped.".to_string(),
            Ended::Failing(why) => format!("The models kept failing: {why}"),
        }
    }
}

/// How a session went.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub ended: Ended,
    pub episodes: u32,
    /// Areas she reached for the first time.
    pub new_areas: Vec<String>,
    pub checkpoints: u32,
    pub rollbacks: u32,
    /// What the paid models cost, at list prices.
    pub usd: f64,
    /// Free tokens the strategist used.
    pub free_tokens: u64,
    /// Plans made by the cheap model.
    pub cheap_plans: u32,
}

/// A line for the log, with a picture of the screen for a checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub line: String,
    /// A PNG.
    pub picture: Option<Vec<u8>>,
}

impl Report {
    fn line(line: String) -> Report {
        Report {
            line,
            picture: None,
        }
    }
}

/// What she's doing now, and what the journal and the day say: everything the dashboard
/// shows. The session sends it before each episode and after each action.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Now {
    /// When the session started, in seconds since 1970.
    pub started: i64,
    pub session: u32,
    /// The episode playing (0 before the first plan).
    pub episode: u32,
    pub task: String,
    pub area: String,
    /// Actions in this episode so far.
    pub actions: u32,
    /// The last few actions, oldest first, like `press "a": The text moved on.`.
    pub last_actions: VecDeque<String>,
    /// Facts from the game's memory, a line each (none for games without a memory map).
    pub facts: String,
    pub goal: String,
    pub notes: Vec<String>,
    /// The last few episode lines.
    pub episodes: Vec<String>,
    /// Episodes in a row with nothing new.
    pub stuck: u32,
    pub checkpoints: u32,
    pub skills: usize,
    /// The skill written last.
    pub newest_skill: Option<String>,
    /// USD the paid models cost today, and the free tokens used.
    pub usd: f64,
    pub free_tokens: u64,
}

impl Now {
    /// Takes in what the journal, her skills and the day say now.
    pub fn read(&mut self, journal: &Journal, skills: &[Skill], today: &Today) {
        self.session = journal.sessions;
        self.goal.clone_from(&journal.goal);
        self.notes.clone_from(&journal.notes);
        self.episodes = journal.recent(EPISODES_SHOWN).to_vec();
        self.stuck = journal.stuck;
        self.checkpoints = journal.checkpoints;
        self.skills = skills.len();
        self.newest_skill = skills.last().map(|s| s.name.clone());
        self.usd = today.usd;
        self.free_tokens = today.free_tokens;
    }
}

/// The game as the supervisor plays it: each action is also told to the dashboard.
struct Watched<'a, G> {
    game: &'a mut G,
    now: Option<&'a watch::Sender<Now>>,
}

#[async_trait]
impl<G: Game> Player for Watched<'_, G> {
    async fn act(&mut self, name: &str, args: &Value) -> anyhow::Result<Acted> {
        let acted = self.game.act(name, args).await?;
        if let Some(now) = self.now {
            let line = format!("{name} {}", args_text(args));
            let line = format!("{}: {}", line.trim(), short(&acted.outcome, 70));
            now.send_modify(|now| {
                now.actions += 1;
                now.area.clone_from(&acted.view.area);
                now.facts.clone_from(&acted.view.facts);
                if now.last_actions.len() == ACTIONS_SHOWN {
                    now.last_actions.pop_front();
                }
                now.last_actions.push_back(line);
            });
        }
        Ok(acted)
    }

    async fn look(&mut self) -> anyhow::Result<Acted> {
        self.game.look().await
    }

    async fn describe(&mut self, acted: &Acted) -> anyhow::Result<String> {
        self.game.describe(acted).await
    }

    async fn screen(&mut self) -> anyhow::Result<Vec<u8>> {
        self.game.screen().await
    }
}

/// The notes kept with a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Checkpoint {
    notes: Vec<String>,
    area: String,
    episode: u32,
}

/// Plays until a cap, starting from where the game is now. `goal` replaces the journal's.
pub async fn play<G: Game>(
    session: &Session<'_>,
    game: &mut G,
    goal: Option<&str>,
) -> anyhow::Result<Summary> {
    let (dir, rom) = (session.saves_dir.as_path(), session.rom.as_str());
    let mut journal = read_json::<Journal>(&files::journal_path(dir, rom))
        .await?
        .unwrap_or_else(|| Journal::new(DEFAULT_GOAL));
    if let Some(goal) = goal.map(str::trim).filter(|g| !g.is_empty()) {
        journal.goal = goal.to_string();
    }
    if journal.goal.is_empty() {
        journal.goal = DEFAULT_GOAL.to_string();
    }
    journal.sessions += 1;
    if game.world().await?.areas.is_empty()
        && let Some(areas) = read_json(&files::maps_path(dir, rom)).await?
    {
        game.set_areas(areas).await?;
    }
    let mut today = Today::on(read_json(&files::today_path(dir)).await?, Utc::now());
    if let Some(now) = &session.now {
        let skills = game.world().await?.skills;
        now.send_modify(|now| {
            now.started = Utc::now().timestamp();
            now.read(&journal, &skills, &today);
        });
    }
    let started = Instant::now();
    let mut paid = paid_so_far(session.spends).await;
    let mut summary = Summary {
        ended: Ended::Stopped,
        episodes: 0,
        new_areas: Vec::new(),
        checkpoints: 0,
        rollbacks: 0,
        usd: 0.0,
        free_tokens: 0,
        cheap_plans: 0,
    };
    let mut failures = 0;

    summary.ended = loop {
        today = Today::on(Some(today), Utc::now());
        if session.stop.is_cancelled() {
            break Ended::Stopped;
        }
        if started.elapsed() >= Duration::from_secs(u64::from(session.limits.minutes) * 60) {
            break Ended::Time;
        }
        if summary.episodes >= session.limits.episodes {
            break Ended::Episodes;
        }
        if today.usd >= session.limits.daily_usd {
            break Ended::Money;
        }
        let mut rung = journal.rung();
        if rung == Rung::Rollback && journal.rollbacks >= ROLLBACKS_MOST {
            rung = Rung::GiveUp;
        }
        if rung == Rung::GiveUp {
            break Ended::Stuck;
        }
        if rung == Rung::Rollback && roll_back(session, game, &mut journal).await? {
            summary.rollbacks += 1;
            let _ = session.report.send(Report::line(
                journal.log.last().cloned().unwrap_or_default(),
            ));
            rung = journal.rung();
        }
        if let Some(timer) = &session.timer {
            timer.running();
        }
        if rung == Rung::Change && journal.stuck == CHANGE_AT {
            let _ = session.report.send(Report::line(format!(
                "⚠️ **Nothing new for {CHANGE_AT} episodes.** She's asked for a different kind \
                 of task, exploring first."
            )));
        }

        let hints = std::mem::take(&mut *session.hints.lock().unwrap());
        if !hints.is_empty() {
            let lines: Vec<String> = hints.iter().map(|h| format!("- {h}")).collect();
            let _ = session.report.send(Report::line(format!(
                "💡 **She reads the hints before planning:**\n{}",
                lines.join("\n")
            )));
        }
        let plan = match plan(session, game, &journal, &mut today, rung, &hints).await {
            Ok((plan, cheap)) => {
                summary.free_tokens += plan.tokens;
                summary.cheap_plans += u32::from(cheap);
                plan
            }
            Err(err) => {
                failures += 1;
                let line = format!("Planning failed: {err:#}");
                journal.log(line.clone());
                let _ = session.report.send(Report::line(line));
                if failures >= FAILURES_MOST {
                    break Ended::Failing(format!("{err:#}"));
                }
                wait_after_failure(&session.stop).await;
                continue;
            }
        };
        journal.notes = plan.notes.clone();
        let mut world = game.world().await?;
        let reviewed = skills::review(&mut world.skills, &plan.skill_changes);
        if !reviewed.is_empty() {
            game.set_skills(world.skills.clone()).await?;
        }
        let task = if plan.task.trim().is_empty() {
            EXPLORE.to_string()
        } else {
            plan.task.clone()
        };

        let look = game.look().await?;
        let area = look.view.area;
        if let Some(now) = &session.now {
            now.send_modify(|now| {
                now.read(&journal, &world.skills, &today);
                now.episode = journal.episodes + 1;
                now.task.clone_from(&task);
                now.area.clone_from(&area);
                now.facts = look.view.facts;
                now.actions = 0;
            });
        }
        let mut stopped = false;
        let mut failed = false;
        let mut watched = Watched {
            game: &mut *game,
            now: session.now.as_ref(),
        };
        let episode = tokio::select! {
            episode = run_task(session.provider, &mut watched, &session.game, &task, None, EPISODE_ACTIONS) => episode,
            () = session.stop.cancelled() => {
                stopped = true;
                Ok(Episode::default())
            }
        };
        let episode = match episode {
            Ok(episode) => {
                failures = 0;
                episode
            }
            Err(err) => {
                failures += 1;
                failed = true;
                Episode {
                    log: vec![format!("The model call failed: {err:#}")],
                    ..Episode::default()
                }
            }
        };
        journal.episodes += 1;
        summary.episodes += 1;
        let world = game.world().await?;
        let done = !stopped && !failed && matches!(episode.finished, Some((true, _)));
        let found = journal.found(&world, &episode.words, done);
        journal.digest = episode.digest();
        let outcome = match (&episode.finished, stopped) {
            (_, true) => "stopped partway".to_string(),
            _ if failed => format!("the model failed ({})", short(&episode.log[0], 120)),
            (Some((true, _)), _) => "done".to_string(),
            (Some((false, why)), _) => format!("not done ({})", short(why, 80)),
            (None, _) => "ran out of actions".to_string(),
        };
        let mut line = format!(
            "#{} {} {area}: {} → {outcome}, {} actions, {}",
            journal.episodes,
            Utc::now().format("%m-%d %H:%M"),
            short(&task, 120),
            episode.actions,
            found.line()
        );
        if journal.stuck > 0 {
            write!(line, " (stuck {})", journal.stuck).unwrap();
        }
        if !reviewed.is_empty() {
            write!(line, "; skills: {}", reviewed.join(", ")).unwrap();
        }
        journal.log(line.clone());
        for new in &found.areas {
            summary.new_areas.push(new.clone());
        }
        let mut pictured = None;
        if !found.areas.is_empty() {
            let at = game.look().await?.view.area;
            checkpoint(session, game, &mut journal, &at).await?;
            summary.checkpoints += 1;
            pictured = Some(Report {
                line: format!("📍 **Checkpoint {}** · {at}", journal.checkpoints),
                picture: game.screen().await.ok(),
            });
        }
        let now_paid = paid_so_far(session.spends).await;
        let usd = (now_paid - paid).max(0.0);
        (paid, today.usd, summary.usd) = (now_paid, today.usd + usd, summary.usd + usd);
        keep(session, game, &journal, &world, &today).await?;
        let _ = session.report.send(Report::line(line));
        if let Some(pictured) = pictured {
            let _ = session.report.send(pictured);
        }
        if let Some(now) = &session.now {
            now.send_modify(|now| now.read(&journal, &world.skills, &today));
        }
        if failures >= FAILURES_MOST {
            break Ended::Failing(
                episode
                    .log
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "the model calls failed".to_string()),
            );
        }
        if failures > 0 {
            wait_after_failure(&session.stop).await;
        }
    };
    Ok(summary)
}

async fn wait_after_failure(stop: &CancellationToken) {
    tokio::select! {
        () = tokio::time::sleep(FAILURE_WAIT) => {}
        () = stop.cancelled() => {}
    }
}

/// The next plan: the strategist while the free tokens last, else the cheap model. Says
/// whether the cheap model made it.
async fn plan<G: Game>(
    session: &Session<'_>,
    game: &mut G,
    journal: &Journal,
    today: &mut Today,
    rung: Rung,
    hints: &[String],
) -> anyhow::Result<(Plan, bool)> {
    let look = game.look().await?;
    let mut now = game.describe(&look).await?;
    let recent = journal.recent(RECENT_EPISODES);
    if !recent.is_empty() {
        write!(now, "\n\nThe last episodes:\n- {}", recent.join("\n- ")).unwrap();
    }
    if rung != Rung::Play {
        write!(
            now,
            "\n\nNothing new for {} episodes (no new area, steps or text). Back out of any \
             menu, then pick a different kind of task; exploring somewhere not seen yet \
             comes first.",
            journal.stuck
        )
        .unwrap();
    }
    if !hints.is_empty() {
        write!(
            now,
            "\n\nHints from people watching, ideas rather than orders: they can be wrong, so \
             check them against what you see. Keep a note of one that turns out right, saying \
             who gave it.\n- {}",
            hints.join("\n- ")
        )
        .unwrap();
    }
    let world = game.world().await?;
    let limit = session.limits.free_tokens;
    let mut care = if rung == Rung::Review {
        Care::Review
    } else {
        Care::Routine
    };
    if care == Care::Review && !today.free_call_fits(limit, true) {
        care = Care::Routine;
    }
    let digest = if journal.digest.is_empty() {
        NOTHING_YET
    } else {
        &journal.digest
    };
    let ask = Ask {
        goal: &journal.goal,
        notes: &journal.notes,
        skills: &world.skills,
        digest,
        now: &now,
        care,
    };
    if let Some(openai) = session.openai
        && today.free_call_fits(limit, care == Care::Review)
    {
        match strategist::plan(openai, Calls::One, &ask).await {
            Ok(plan) => {
                today.used_free(plan.tokens, care == Care::Review);
                return Ok((plan, false));
            }
            // OpenAI being down shouldn't stop play: the cheap model plans this once.
            Err(err) => tracing::warn!("the Game Boy strategist failed: {err:#}"),
        }
    }
    let ask = Ask {
        care: Care::Routine,
        ..ask
    };
    Ok((
        strategist::plan_cheaply(session.provider, &ask).await?,
        true,
    ))
}

/// Saves a checkpoint of where the game is now, with the notes, and drops old ones.
async fn checkpoint<G: Game>(
    session: &Session<'_>,
    game: &mut G,
    journal: &mut Journal,
    area: &str,
) -> anyhow::Result<()> {
    let folder = files::checkpoints_folder(&session.saves_dir, &session.rom);
    journal.checkpoints += 1;
    let n = journal.checkpoints;
    write_file(
        &folder.join(format!("{n}.state")),
        &game.save_state().await?,
    )
    .await?;
    let saved = Checkpoint {
        notes: journal.notes.clone(),
        area: area.to_string(),
        episode: journal.episodes,
    };
    write_json(&folder.join(format!("{n}.json")), &saved).await?;
    let numbers = checkpoints(&folder).await;
    for old in numbers
        .iter()
        .take(numbers.len().saturating_sub(CHECKPOINTS_KEPT))
    {
        let _ = tokio::fs::remove_file(folder.join(format!("{old}.state"))).await;
        let _ = tokio::fs::remove_file(folder.join(format!("{old}.json"))).await;
    }
    Ok(())
}

/// Goes back to a checkpoint: the newest the first time she's stuck, one further back each
/// time after, until something new turns up. Her skills and the maps stay. Says whether
/// there was one.
async fn roll_back<G: Game>(
    session: &Session<'_>,
    game: &mut G,
    journal: &mut Journal,
) -> anyhow::Result<bool> {
    let folder = files::checkpoints_folder(&session.saves_dir, &session.rom);
    let numbers = checkpoints(&folder).await;
    let back = journal.rollbacks as usize;
    let Some(&n) = numbers.iter().rev().nth(back).or(numbers.first()) else {
        journal.log("Stuck, with no checkpoint to go back to.".to_string());
        return Ok(false);
    };
    let state = tokio::fs::read(folder.join(format!("{n}.state")))
        .await
        .with_context(|| format!("reading checkpoint {n}"))?;
    let saved: Checkpoint = read_json(&folder.join(format!("{n}.json")))
        .await?
        .with_context(|| format!("checkpoint {n} has no notes"))?;
    game.load_state(state).await?;
    journal.notes = saved.notes;
    journal.stuck = 0;
    journal.rollbacks += 1;
    journal.digest = format!(
        "Went back to a checkpoint in {} (from episode {}) after {ROLLBACK_AT} episodes \
         with nothing new. The notes are as they were then; the skills are kept. Try \
         another way.",
        saved.area, saved.episode
    );
    journal.log(format!(
        "Rolled back to checkpoint {n} in {} (episode {}).",
        saved.area, saved.episode
    ));
    Ok(true)
}

/// The checkpoint numbers in `folder`, oldest first.
async fn checkpoints(folder: &Path) -> Vec<u32> {
    let mut numbers = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(folder).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            if let Some(n) = name
                .to_str()
                .and_then(|n| n.strip_suffix(".state"))
                .and_then(|n| n.parse().ok())
            {
                numbers.push(n);
            }
        }
    }
    numbers.sort_unstable();
    numbers
}

/// Writes everything a restart needs.
async fn keep<G: Game>(
    session: &Session<'_>,
    game: &mut G,
    journal: &Journal,
    world: &World,
    today: &Today,
) -> anyhow::Result<()> {
    let (dir, rom) = (session.saves_dir.as_path(), session.rom.as_str());
    write_json(&files::journal_path(dir, rom), journal).await?;
    write_json(&files::maps_path(dir, rom), &world.areas).await?;
    write_json(&files::skills_path(dir, rom), &world.skills).await?;
    write_json(&files::today_path(dir), today).await?;
    let latest = files::save_path(dir, rom, LATEST).map_err(anyhow::Error::msg)?;
    write_file(&latest, &game.save_state().await?).await
}

/// USD this month for playing on paid models: every `game_play` call, and planning on
/// anything but OpenAI's free tokens.
async fn paid_so_far(spends: &[Spend]) -> f64 {
    let mut usd = 0.0;
    for spend in spends {
        let Ok(month) = spend.this_month().await else {
            continue;
        };
        for (job, cost) in month.jobs {
            let free =
                spend.provider() == "openai" && matches!(job.as_str(), "game_plan" | "game_review");
            if job == "game_play" || (job.starts_with("game_") && job != "game_art" && !free) {
                usd += cost;
            }
        }
    }
    usd
}

fn short(text: &str, most: usize) -> String {
    let text = text.replace('\n', " ");
    if text.chars().count() <= most {
        return text;
    }
    text.chars().take(most).collect::<String>() + "…"
}

async fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    match tokio::fs::read_to_string(path).await {
        Ok(json) => Ok(serde_json::from_str(&json).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    }
}

async fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> anyhow::Result<()> {
    write_file(path, serde_json::to_string_pretty(value)?.as_bytes()).await
}

/// Writes through a temporary file, so a crash never leaves half a file.
async fn write_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(folder) = path.parent() {
        tokio::fs::create_dir_all(folder)
            .await
            .with_context(|| format!("making {}", folder.display()))?;
    }
    let temporary = path.with_extension("partial");
    tokio::fs::write(&temporary, bytes)
        .await
        .with_context(|| format!("writing {}", temporary.display()))?;
    tokio::fs::rename(&temporary, path)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use serde_json::{Value, json};

    use super::*;
    use crate::ai::{ChatEvent, ChatRequest, Done, GeneratedFile, ToolCall};
    use crate::features::games::gameboy::actions::Acted;
    use crate::features::games::gameboy::map::Kind;
    use crate::features::games::gameboy::world::View;

    /// Rooms in a row: `walk_to` goes on to the next room (a new area), anything else
    /// changes nothing. The save state is the room's number.
    struct Rooms {
        room: u32,
        world: World,
    }

    impl Rooms {
        fn new() -> Rooms {
            let mut rooms = Rooms {
                room: 0,
                world: World::default(),
            };
            rooms.see();
            rooms
        }

        fn see(&mut self) {
            let name = format!("Room {}", self.room);
            for x in 0..10 {
                self.world.area(&name).see((x, 0), Kind::Floor);
            }
        }

        fn acted(&self, outcome: &str) -> Acted {
            Acted {
                outcome: outcome.to_string(),
                view: View {
                    area: format!("Room {}", self.room),
                    at: (0, 0),
                    can_walk: true,
                    text: String::new(),
                    facts: String::new(),
                },
                words: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl Player for Rooms {
        async fn act(&mut self, name: &str, _args: &Value) -> anyhow::Result<Acted> {
            if name == "walk_to" {
                self.room += 1;
                self.see();
                return Ok(self.acted("Walked into the next room."));
            }
            Ok(self.acted("Nothing changed."))
        }
        async fn look(&mut self) -> anyhow::Result<Acted> {
            Ok(self.acted(""))
        }
        async fn describe(&mut self, acted: &Acted) -> anyhow::Result<String> {
            Ok(format!("Place: {}.", acted.view.area))
        }
        async fn screen(&mut self) -> anyhow::Result<Vec<u8>> {
            Ok(vec![1, 2, 3])
        }
    }

    #[async_trait]
    impl Game for Rooms {
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
            Ok(self.room.to_string().into_bytes())
        }
        async fn load_state(&mut self, state: Vec<u8>) -> anyhow::Result<()> {
            self.room = String::from_utf8(state)?.parse()?;
            Ok(())
        }
    }

    /// Plans with `plan` (counting them, and keeping what each was told), then plays each
    /// episode as one action, `walk_to` when the next of `walks` says so, and `finish`.
    struct Scripted {
        walks: Mutex<VecDeque<bool>>,
        /// Where `walk_to` goes.
        to: Value,
        acted: Mutex<bool>,
        plans: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new(walks: &[bool]) -> Scripted {
            Scripted {
                walks: Mutex::new(walks.iter().copied().collect()),
                to: json!({"x": 1, "y": 0}),
                acted: Mutex::new(false),
                plans: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ChatProvider for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn model(&self) -> &str {
            "m1"
        }
        async fn stream(
            &self,
            request: ChatRequest,
        ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>> {
            let (name, args) = if request.tools.iter().any(|t| t.name == "plan") {
                let told = match &request.input {
                    crate::ai::Input::Full(turns) => format!("{:?}", turns[0].parts),
                    _ => String::new(),
                };
                let mut plans = self.plans.lock().unwrap();
                plans.push(told);
                let n = plans.len();
                *self.acted.lock().unwrap() = false;
                let changes = if n == 1 {
                    json!([{"name": "talk", "change": "rewrite", "use_when": "text is up",
                        "notes": "", "rules": [{"if_words": "", "press": "a"}],
                        "stop_words": [], "stop_when_free": true, "most": 10}])
                } else {
                    json!([])
                };
                (
                    "plan",
                    json!({"reasoning": "", "notes": [format!("note {n}")],
                        "skill_changes": changes, "next_task": format!("task {n}")}),
                )
            } else if std::mem::replace(&mut *self.acted.lock().unwrap(), true) {
                ("finish", json!({"done": true, "summary": "ok"}))
            } else if self.walks.lock().unwrap().pop_front().unwrap_or(false) {
                ("walk_to", self.to.clone())
            } else {
                ("press", json!({"buttons": "a"}))
            };
            let (tx, rx) = mpsc::channel(4);
            tx.send(Ok(ChatEvent::Done(Done {
                tool_calls: vec![ToolCall {
                    id: "call".to_string(),
                    name: name.to_string(),
                    args,
                }],
                ..Done::default()
            })))
            .await?;
            Ok(rx)
        }
        async fn download_file(&self, _file: &GeneratedFile) -> anyhow::Result<Vec<u8>> {
            unimplemented!()
        }
    }

    fn session<'a>(
        provider: &'a Scripted,
        dir: &Path,
        episodes: u32,
        report: mpsc::UnboundedSender<Report>,
    ) -> Session<'a> {
        Session {
            provider,
            openai: None,
            spends: &[],
            game: "Test Game".to_string(),
            saves_dir: dir.to_path_buf(),
            rom: "test.gb".to_string(),
            limits: Limits {
                minutes: 60,
                episodes,
                daily_usd: 1.5,
                free_tokens: 1_000_000,
            },
            stop: CancellationToken::new(),
            report,
            now: None,
            timer: None,
            hints: Hints::default(),
        }
    }

    #[tokio::test]
    async fn she_plays_until_a_cap_with_checkpoints_and_a_rollback() {
        let dir = tempfile::tempdir().unwrap();
        // Two new rooms, then nothing new: stuck from episode 3 on.
        let provider = Scripted::new(&[true, true]);
        let (report, mut lines) = mpsc::unbounded_channel();
        let mut session = session(&provider, dir.path(), 13, report);
        let (now, watching) = watch::channel(Now::default());
        session.now = Some(now);
        let mut game = Rooms::new();
        let summary = play(&session, &mut game, Some("reach the end"))
            .await
            .unwrap();
        assert_eq!(summary.ended, Ended::Episodes);
        assert_eq!(summary.episodes, 13);
        assert_eq!(summary.new_areas, vec!["Room 0", "Room 1", "Room 2"]);
        assert_eq!((summary.checkpoints, summary.rollbacks), (2, 1));
        assert_eq!(summary.cheap_plans, 13, "no OpenAI: the cheap model plans");

        // The episode lines went to the log, with pictures of the checkpoints, the warning
        // at 3 episodes without progress, and the rollback.
        let mut sent = Vec::new();
        while let Ok(report) = lines.try_recv() {
            sent.push(report);
        }
        let line = |n: usize| sent[n].line.as_str();
        assert_eq!(sent.len(), 17, "{sent:#?}");
        assert!(line(0).starts_with("#1 ") && line(0).contains("Room 0: task 1 → done"));
        assert!(line(0).contains("new: Room 0, Room 1"), "{}", line(0));
        assert!(line(0).contains("skills: rewrote talk"), "{}", line(0));
        assert_eq!(line(1), "📍 **Checkpoint 1** · Room 1");
        assert_eq!(sent[1].picture.as_deref(), Some(&[1, 2, 3][..]));
        // (Episode 3, done with nothing new right after progress, is let off.)
        assert!(line(4).contains("nothing new") && !line(4).contains("stuck"));
        assert!(
            line(8).contains("Nothing new for 3 episodes"),
            "{}",
            line(8)
        );
        assert!(line(15).starts_with("Rolled back to checkpoint 2 in Room 2"));
        assert!(sent.iter().filter(|r| r.picture.is_some()).count() == 2);

        // The dashboard's last view of it.
        let now = watching.borrow().clone();
        assert_eq!(
            (now.session, now.episode, now.task.as_str()),
            (1, 13, "task 13")
        );
        assert_eq!((now.area.as_str(), now.actions), ("Room 2", 1));
        assert_eq!(now.last_actions.len(), ACTIONS_SHOWN);
        assert_eq!(now.last_actions[5], "press \"a\": Nothing changed.");
        assert_eq!(now.goal, "reach the end");
        assert_eq!(now.notes, vec!["note 13"]);
        assert_eq!(now.episodes.len(), EPISODES_SHOWN);
        assert!(now.episodes[2].starts_with("#13 "));
        assert_eq!((now.stuck, now.checkpoints, now.skills), (1, 2, 1));
        assert_eq!(now.newest_skill.as_deref(), Some("talk"));
        assert!(now.started > 0);

        // The ladder shows in the plans: a warning from 3 episodes without progress, and
        // after the rollback, what happened instead of the last task.
        {
            let plans = provider.plans.lock().unwrap();
            assert!(!plans[5].contains("Nothing new for"));
            assert!(plans[6].contains("Nothing new for 3 episodes"));
            assert!(plans[12].contains("Went back to a checkpoint in Room 2"));
            assert!(!plans[12].contains("Nothing new for"));
        }

        // Everything a restart needs is on disk.
        let journal: Journal = read_json(&files::journal_path(dir.path(), "test.gb"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(journal.goal, "reach the end");
        assert_eq!(journal.episodes, 13);
        assert_eq!(journal.notes, vec!["note 13"]);
        assert_eq!((journal.stuck, journal.rollbacks), (1, 1));
        let skills: Vec<Skill> = read_json(&files::skills_path(dir.path(), "test.gb"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(skills[0].name, "talk");
        let maps: HashMap<String, AreaMap> = read_json(&files::maps_path(dir.path(), "test.gb"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(maps.len(), 3);
        let folder = files::checkpoints_folder(dir.path(), "test.gb");
        assert_eq!(checkpoints(&folder).await, vec![1, 2]);
        let latest = files::save_path(dir.path(), "test.gb", LATEST).unwrap();
        assert_eq!(std::fs::read(latest).unwrap(), b"2");
        assert!(files::today_path(dir.path()).exists());

        // The next session carries on from the journal: the goal stays.
        let (report, _lines) = mpsc::unbounded_channel();
        let session = super::tests::session(&provider, dir.path(), 1, report);
        play(&session, &mut game, None).await.unwrap();
        let journal: Journal = read_json(&files::journal_path(dir.path(), "test.gb"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (journal.goal.as_str(), journal.episodes, journal.sessions),
            ("reach the end", 14, 2)
        );
    }

    #[tokio::test]
    async fn she_reads_hints_before_her_next_plan_once() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Scripted::new(&[true, true]);
        let (report, mut lines) = mpsc::unbounded_channel();
        let session = session(&provider, dir.path(), 2, report);
        session
            .hints
            .lock()
            .unwrap()
            .push("Cherry: the stairs are top right".to_string());
        play(&session, &mut Rooms::new(), None).await.unwrap();
        let first = lines.try_recv().unwrap().line;
        assert_eq!(
            first,
            "💡 **She reads the hints before planning:**\n- Cherry: the stairs are top right"
        );
        let plans = provider.plans.lock().unwrap();
        assert!(plans[0].contains("Hints from people watching"));
        assert!(plans[0].contains("- Cherry: the stairs are top right"));
        assert!(!plans[1].contains("Hints from"), "read once");
        assert!(session.hints.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stuck_for_good_she_goes_further_back_then_gives_up() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Scripted::new(&[true, true, true]);
        let (report, _lines) = mpsc::unbounded_channel();
        let session = session(&provider, dir.path(), 100, report);
        let mut game = Rooms::new();
        let summary = play(&session, &mut game, None).await.unwrap();
        assert_eq!(summary.ended, Ended::Stuck);
        assert_eq!((summary.checkpoints, summary.rollbacks), (3, 3));
        // 3 that found something, one let off, then 4 times 9 without.
        assert_eq!(summary.episodes, 3 + 1 + 4 * 9);
        let journal: Journal = read_json(&files::journal_path(dir.path(), "test.gb"))
            .await
            .unwrap()
            .unwrap();
        let rollbacks: Vec<&String> = journal
            .log
            .iter()
            .filter(|l| l.starts_with("Rolled back"))
            .collect();
        assert!(rollbacks[0].contains("checkpoint 3"), "{rollbacks:?}");
        assert!(rollbacks[1].contains("checkpoint 2"), "{rollbacks:?}");
        assert!(rollbacks[2].contains("checkpoint 1"), "{rollbacks:?}");
        assert_eq!(game.room, 1, "back at the first checkpoint, in Room 1");
    }

    #[tokio::test]
    async fn caps_stop_her_before_an_episode() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Scripted::new(&[]);
        let (report, _lines) = mpsc::unbounded_channel();
        let stopped = session(&provider, dir.path(), 10, report.clone());
        stopped.stop.cancel();
        let summary = play(&stopped, &mut Rooms::new(), None).await.unwrap();
        assert_eq!((summary.ended, summary.episodes), (Ended::Stopped, 0));

        let mut broke = session(&provider, dir.path(), 10, report);
        broke.limits.daily_usd = 0.0;
        let summary = play(&broke, &mut Rooms::new(), None).await.unwrap();
        assert_eq!((summary.ended, summary.episodes), (Ended::Money, 0));
        assert!(provider.plans.lock().unwrap().is_empty());
    }

    #[tokio::test]
    #[ignore = "needs a ROM in VIVY_GB_ROM"]
    async fn a_session_plays_red_and_its_saves_load() {
        use crate::features::games::gameboy::gate::player::Local;
        use crate::features::games::gameboy::red;

        let dir = tempfile::tempdir().unwrap();
        // Downstairs, then a press.
        let mut provider = Scripted::new(&[true, false]);
        provider.to = json!({"x": 7, "y": 1});
        let (report, mut lines) = mpsc::unbounded_channel();
        let session = session(&provider, dir.path(), 2, report);
        let mut game = Local::new(red::tests::new_game());
        let summary = play(&session, &mut game, None).await.unwrap();
        while let Ok(report) = lines.try_recv() {
            println!("{}", report.line);
        }
        assert_eq!(summary.episodes, 2);
        assert_eq!(summary.new_areas, vec!["Red's House 1F", "Red's House 2F"]);
        assert_eq!(summary.checkpoints, 1);

        // The checkpoint and the latest save load into a Game Boy of their own.
        let area_after = |state: Vec<u8>| async move {
            let mut other = Local::new(red::tests::new_game());
            other.load_state(state).await.unwrap();
            other.look().await.unwrap().view.area
        };
        let folder = files::checkpoints_folder(dir.path(), "test.gb");
        let checkpoint = std::fs::read(folder.join("1.state")).unwrap();
        assert_eq!(area_after(checkpoint).await, "Red's House 1F");
        let latest = std::fs::read(files::save_path(dir.path(), "test.gb", LATEST).unwrap());
        assert_eq!(area_after(latest.unwrap()).await, "Red's House 1F");

        // A new session on a Game Boy just switched on gets the maps back.
        let (report, _lines) = mpsc::unbounded_channel();
        let session = super::tests::session(&provider, dir.path(), 0, report);
        let mut fresh = Local::new(red::tests::new_game());
        let summary = play(&session, &mut fresh, None).await.unwrap();
        assert_eq!(summary.ended, Ended::Episodes);
        assert_eq!(fresh.world.areas.len(), 2);
    }

    /// A model that's always overloaded.
    struct Overloaded;

    #[async_trait]
    impl ChatProvider for Overloaded {
        fn name(&self) -> &'static str {
            "overloaded"
        }
        fn model(&self) -> &str {
            "m1"
        }
        async fn stream(
            &self,
            _request: ChatRequest,
        ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>> {
            anyhow::bail!("529 overloaded")
        }
        async fn download_file(&self, _file: &GeneratedFile) -> anyhow::Result<Vec<u8>> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn a_model_that_keeps_failing_ends_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let (report, _lines) = mpsc::unbounded_channel();
        let unused = Scripted::new(&[]);
        let mut session = session(&unused, dir.path(), 10, report);
        session.provider = &Overloaded;
        let summary = play(&session, &mut Rooms::new(), None).await.unwrap();
        assert_eq!(summary.episodes, 0);
        match summary.ended {
            Ended::Failing(why) => assert!(why.contains("529 overloaded"), "{why}"),
            other => panic!("ended with {other:?}"),
        }
    }
}
