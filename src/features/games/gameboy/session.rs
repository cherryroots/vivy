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
use super::notes::{self, Note, Reached};
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
    pub kind: Reported,
    pub line: String,
    /// A PNG.
    pub picture: Option<Vec<u8>>,
}

/// What a [`Report`] is about, for her commentary (`narrate.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reported {
    /// An episode's line.
    Episode,
    /// A checkpoint: she reached an area for the first time.
    Checkpoint,
    /// Stuck: the ladder's warning, or going back to a checkpoint.
    Stuck,
    /// Hints from people watching, a line each after the first.
    Hint,
    /// A milestone from her notes reached: "🏁 **Milestone: Reach Viridian City** · …".
    Milestone,
    /// She blacked out: "💀 **Blacked out** in Viridian Forest and woke up in Pallet Town."
    Blackout,
    /// Anything else, like a planning failure.
    Other,
}

impl Report {
    fn line(kind: Reported, line: String) -> Report {
        Report {
            kind,
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
    /// Her notes as people see them, with a mark for what code found.
    pub notes: Vec<String>,
    /// How far through her milestones, like "2 of 5 · next: Reach Viridian City".
    pub milestone: String,
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
        self.notes = journal.notes.iter().map(Note::shown).collect();
        self.milestone = notes::progress(&journal.milestones);
        self.episodes = journal.recent(EPISODES_SHOWN).to_vec();
        self.stuck = journal.stuck;
        self.checkpoints = journal.checkpoints;
        self.skills = skills.len();
        self.newest_skill = skills.last().map(|s| s.name.clone());
        self.usd = today.usd;
        self.free_tokens = today.free_tokens;
    }
}

/// The game as the supervisor plays it: each action is also told to the dashboard, and a
/// blackout is noticed.
struct Watched<'a, G> {
    game: &'a mut G,
    now: Option<&'a watch::Sender<Now>>,
    /// The area she was in before the last action.
    area: String,
    /// The area she blacked out in, if she did.
    blacked_out: Option<String>,
}

#[async_trait]
impl<G: Game> Player for Watched<'_, G> {
    async fn act(&mut self, name: &str, args: &Value) -> anyhow::Result<Acted> {
        let acted = self.game.act(name, args).await?;
        if blacked_out(&acted.words) {
            self.blacked_out = Some(self.area.clone());
        }
        self.area.clone_from(&acted.view.area);
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

/// What Pokémon games say when all her Pokémon faint and she's sent back to heal: "blacked
/// out" in Red and Blue, "whited out" in the later ones. Other games never say it, so for
/// them nothing happens.
const BLACKED_OUT: [&str; 2] = ["blacked out", "whited out"];

/// Whether the words on screen said she blacked out, ignoring case and line breaks.
fn blacked_out(words: &[String]) -> bool {
    words.iter().any(|words| {
        let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
        let words = words.to_lowercase();
        BLACKED_OUT.iter().any(|said| words.contains(said))
    })
}

/// The log line for a blackout: where it happened and where she woke up.
fn blackout_line(from: &str, to: &str) -> String {
    if from == to {
        format!("💀 **Blacked out** in {from}.")
    } else {
        format!("💀 **Blacked out** in {from} and woke up in {to}.")
    }
}

/// Marks the milestones perception says she has reached since the last episode, and
/// gives a line for the log for each: how many episodes and dollars it took since the one
/// before (the checkpoint ladder).
async fn reach_milestones<G: Game>(
    game: &mut G,
    journal: &mut Journal,
    entered: &[String],
) -> anyhow::Result<Vec<String>> {
    if journal.milestones.iter().all(|m| m.reached.is_some()) {
        return Ok(Vec::new());
    }
    let facts = game.look().await?.view.facts;
    let before = journal
        .milestones
        .iter()
        .filter_map(|m| m.reached)
        .max_by_key(|r| r.episode)
        .unwrap_or(Reached {
            episode: 0,
            usd: 0.0,
        });
    let at = Reached {
        episode: journal.episodes,
        usd: journal.usd,
    };
    let names = notes::reach(&mut journal.milestones, entered, &facts, at);
    let episodes = at.episode - before.episode;
    let s = if episodes == 1 { "" } else { "s" };
    let lines: Vec<String> = names
        .iter()
        .map(|name| {
            format!(
                "🏁 **Milestone: {name}** · episode {}, {episodes} episode{s} and ${:.3} since \
                 the last",
                at.episode,
                (at.usd - before.usd).max(0.0)
            )
        })
        .collect();
    for line in &lines {
        journal.log(line.replace("**", ""));
    }
    Ok(lines)
}

/// The notes kept with a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Checkpoint {
    notes: Vec<Note>,
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
                Reported::Stuck,
                journal.log.last().cloned().unwrap_or_default(),
            ));
            rung = journal.rung();
        }
        if let Some(timer) = &session.timer {
            timer.running();
        }
        if rung == Rung::Change && journal.stuck == CHANGE_AT {
            let _ = session.report.send(Report::line(
                Reported::Stuck,
                format!(
                    "⚠️ **Nothing new for {CHANGE_AT} episodes.** She's asked for a different kind \
                 of task, exploring first."
                ),
            ));
        }

        let hints = std::mem::take(&mut *session.hints.lock().unwrap());
        if !hints.is_empty() {
            let lines: Vec<String> = hints.iter().map(|h| format!("- {h}")).collect();
            let _ = session.report.send(Report::line(
                Reported::Hint,
                format!(
                    "💡 **She reads the hints before planning:**\n{}",
                    lines.join("\n")
                ),
            ));
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
                let _ = session.report.send(Report::line(Reported::Other, line));
                if failures >= FAILURES_MOST {
                    break Ended::Failing(format!("{err:#}"));
                }
                wait_after_failure(&session.stop).await;
                continue;
            }
        };
        journal.notes = plan.notes.clone();
        notes::replace_ahead(&mut journal.milestones, plan.milestones.clone());
        let mut world = game.world().await?;
        notes::check_all(&mut journal.notes, &world);
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
            area: area.clone(),
            blacked_out: None,
        };
        let episode = tokio::select! {
            episode = run_task(session.provider, &mut watched, &session.game, &task, None, EPISODE_ACTIONS) => episode,
            () = session.stop.cancelled() => {
                stopped = true;
                Ok(Episode::default())
            }
        };
        let blacked_out = watched.blacked_out.take();
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
            "#{} {} {area}: {} → {outcome}, {} action{}, {}",
            journal.episodes,
            Utc::now().format("%m-%d %H:%M"),
            short(&task, 120),
            episode.actions,
            if episode.actions == 1 { "" } else { "s" },
            found.line()
        );
        if journal.stuck > 0 {
            write!(line, " (stuck {})", journal.stuck).unwrap();
        }
        if !reviewed.is_empty() {
            write!(line, "; skills: {}", reviewed.join(", ")).unwrap();
        }
        journal.log(line.clone());
        let at = game.look().await?.view.area;
        let blackout = blacked_out.map(|from| blackout_line(&from, &at));
        if let Some(blackout) = &blackout {
            journal.log(blackout.clone());
        }
        for new in &found.areas {
            summary.new_areas.push(new.clone());
        }
        let mut pictured = None;
        if !found.areas.is_empty() {
            checkpoint(session, game, &mut journal, &at).await?;
            summary.checkpoints += 1;
            pictured = Some(Report {
                kind: Reported::Checkpoint,
                line: format!("📍 **Checkpoint {}** · {at}", journal.checkpoints),
                picture: game.screen().await.ok(),
            });
        }
        let now_paid = paid_so_far(session.spends).await;
        let usd = (now_paid - paid).max(0.0);
        (paid, today.usd, summary.usd) = (now_paid, today.usd + usd, summary.usd + usd);
        journal.usd += usd;
        notes::check_all(&mut journal.notes, &world);
        let reached = reach_milestones(game, &mut journal, &episode.entered).await?;
        keep(session, game, &journal, &world, &today).await?;
        let _ = session.report.send(Report::line(Reported::Episode, line));
        if let Some(blackout) = blackout {
            let _ = session
                .report
                .send(Report::line(Reported::Blackout, blackout));
        }
        if let Some(pictured) = pictured {
            let _ = session.report.send(pictured);
        }
        for line in reached {
            let _ = session.report.send(Report::line(Reported::Milestone, line));
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
             check them against what you see. Keep a note of one that turns out right, with \
             the source \"hint from\" and who gave it.\n- {}",
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
        milestones: &journal.milestones,
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
            // Her commentary isn't playing: it's her voice, like a chat answer.
            let talk = matches!(job.as_str(), "game_art" | super::narrate::JOB);
            if job == "game_play" || (job.starts_with("game_") && !talk && !free) {
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
        /// Whether a press blacks her out, back to the first room.
        faints: bool,
    }

    impl Rooms {
        fn new() -> Rooms {
            let mut rooms = Rooms {
                room: 0,
                world: World::default(),
                faints: false,
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
            if self.faints {
                self.room = 0;
                let mut acted = self.acted("Pressed A.");
                acted.words = vec![
                    "BLUE is out of useable POKéMON!".to_string(),
                    "BLUE blacked\nout!".to_string(),
                ];
                return Ok(acted);
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
        /// The notes and milestones each plan answers with (by default "note <n>" and none).
        notes: Option<Value>,
        milestones: Value,
    }

    impl Scripted {
        fn new(walks: &[bool]) -> Scripted {
            Scripted {
                walks: Mutex::new(walks.iter().copied().collect()),
                to: json!({"x": 1, "y": 0}),
                acted: Mutex::new(false),
                plans: Mutex::new(Vec::new()),
                notes: None,
                milestones: json!([]),
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
                let notes = self.notes.clone().unwrap_or(json!([format!("note {n}")]));
                (
                    "plan",
                    json!({"reasoning": "", "notes": notes, "milestones": self.milestones,
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

    #[test]
    fn blackouts_are_read_from_the_words() {
        let words = |w: &[&str]| w.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert!(blacked_out(&words(&[
            "BLUE is out of",
            "BLUE blacked\nout!"
        ])));
        assert!(blacked_out(&words(&["GOLD whited out!"])));
        assert!(!blacked_out(&words(&["The wild PIDGEY fainted!"])));
        assert_eq!(
            blackout_line("Viridian Forest", "Pallet Town"),
            "💀 **Blacked out** in Viridian Forest and woke up in Pallet Town."
        );
        assert_eq!(
            blackout_line("Route 1", "Route 1"),
            "💀 **Blacked out** in Route 1."
        );
    }

    #[tokio::test]
    async fn a_blackout_is_its_own_line_in_the_log() {
        let dir = tempfile::tempdir().unwrap();
        // She walks into Room 1, then a press blacks her out back to Room 0.
        let provider = Scripted::new(&[true, false]);
        let (report, mut lines) = mpsc::unbounded_channel();
        let session = session(&provider, dir.path(), 2, report);
        let mut game = Rooms::new();
        game.faints = true;
        play(&session, &mut game, None).await.unwrap();
        let mut sent = Vec::new();
        while let Ok(report) = lines.try_recv() {
            sent.push(report.line);
        }
        let blackouts: Vec<_> = sent.iter().filter(|l| l.starts_with("💀")).collect();
        assert_eq!(
            blackouts,
            ["💀 **Blacked out** in Room 1 and woke up in Room 0."],
            "{sent:#?}"
        );
        // It comes right after its episode's line.
        let at = sent.iter().position(|l| l.starts_with("💀")).unwrap();
        assert!(sent[at - 1].starts_with("#2 "), "{sent:#?}");
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
        let notes: Vec<&str> = journal.notes.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(notes, vec!["note 13"]);
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
    async fn code_checks_her_notes_and_milestones_against_the_maps() {
        let dir = tempfile::tempdir().unwrap();
        let mut provider = Scripted::new(&[true, false, true]);
        provider.notes = Some(json!([
            {"text": "Room 1 has floor at (3, 0).", "source": "tried"},
            {"text": "The sign in Room 1 is at (12, 0).", "source": "hint from Cherry"},
            {"text": "The exit of Room 1 is at (5, 0).", "source": "guess"},
            {"text": "She is standing at (0, 0) in Room 1.", "source": "seen"}
        ]));
        provider.milestones = json!([
            {"name": "Reach Room 1", "area": "Room 1", "fact": "", "has": ""},
            {"name": "Reach Room 2", "area": "room 2", "fact": "", "has": ""},
            {"name": "Win", "area": "", "fact": "", "has": ""},
            {"name": "Reach Room 9", "area": "Room 9", "fact": "", "has": ""}
        ]);
        let (report, mut lines) = mpsc::unbounded_channel();
        let mut session = session(&provider, dir.path(), 3, report);
        let (now, watching) = watch::channel(Now::default());
        session.now = Some(now);
        play(&session, &mut Rooms::new(), Some("reach the end"))
            .await
            .unwrap();

        let mut sent = Vec::new();
        while let Ok(report) = lines.try_recv() {
            sent.push(report.line);
        }
        let reached: Vec<&String> = sent.iter().filter(|l| l.starts_with("🏁")).collect();
        assert_eq!(
            reached,
            [
                "🏁 **Milestone: Reach Room 1** · episode 1, 1 episode and $0.000 since the last",
                "🏁 **Milestone: Reach Room 2** · episode 3, 2 episodes and $0.000 since the last"
            ],
            "{sent:#?}"
        );

        let journal: Journal = read_json(&files::journal_path(dir.path(), "test.gb"))
            .await
            .unwrap()
            .unwrap();
        // The one code can't check is dropped; the reached ones are kept with when.
        let names: Vec<(&str, Option<u32>)> = journal
            .milestones
            .iter()
            .map(|m| (m.name.as_str(), m.reached.map(|r| r.episode)))
            .collect();
        assert_eq!(
            names,
            [
                ("Reach Room 1", Some(1)),
                ("Reach Room 2", Some(3)),
                ("Reach Room 9", None)
            ]
        );
        assert!(
            journal
                .log
                .iter()
                .any(|l| l.starts_with("🏁 Milestone: Reach Room 2"))
        );
        let shown: Vec<String> = journal.notes.iter().map(Note::shown).collect();
        assert_eq!(
            shown,
            [
                "✅ Room 1 has floor at (3, 0).",
                "The sign in Room 1 is at (12, 0). (hint from Cherry)",
                "❔ The exit of Room 1 is at (5, 0).",
                "🔄 She is standing at (0, 0) in Room 1."
            ]
        );

        // She is told how far she is, and what code found.
        let now = watching.borrow().clone();
        assert_eq!(now.milestone, "2 of 3 · next: Reach Room 9");
        assert_eq!(now.notes, shown);
        let plans = provider.plans.lock().unwrap();
        assert!(plans[2].contains("Reach Room 1 (in area"), "{}", plans[2]);
        assert!(plans[2].contains("): reached in episode 1"), "{}", plans[2]);
        assert!(
            plans[2].contains("[tried; code checked: (3, 0) in Room 1 is on the map]"),
            "{}",
            plans[2]
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
