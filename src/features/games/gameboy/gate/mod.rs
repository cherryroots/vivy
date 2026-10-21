//! Stage 3's gate: does the cheap supervisor (Haiku) carry out short tasks in Pokémon Red
//! well enough to build on? Test-only: it plays the owner's ROM and calls the real models.
//!
//! - `tasks.rs`: plays Red's opening by script, saving a state at each point a task starts,
//!   and the tasks themselves, each with a success check code can make
//! - `player.rs`: a Game Boy of the gate's own for the supervisor to play
//! - here: the runs, which write a report and every episode's log to a results folder
//!
//! Run it as the docs say (`docs/features/gameboy.md`, "The gate"). Each run is an
//! ignored test, set up with environment variables:
//!
//! - `gate_keys_work`: one small call to Claude, OpenAI and Gemini each
//! - `gate_tasks`: each task `VIVY_GB_RUNS` times (3); `VIVY_GB_PLAYER` picks the model
//!   playing (`claude-haiku-5-5`; a `gemini-...` or `gpt-...` model works too),
//!   `VIVY_GB_ONLY` runs only the tasks whose names contain it, and `VIVY_GB_HINTS` names
//!   an OpenAI model that writes a hint before each task
//! - `gate_strategists`: the long goal with each strategist in `VIVY_GB_STRATEGISTS`
//!   (`gpt-6-astra,gpt-6.1-sol`), asked in one call and in two
//! - `gate_session`: one whole session as the bot plays it, with the real models
//! - `gate_learning`: stage 4's question, whether she gets better by writing skills. The
//!   tasks in `VIVY_GB_LEARN` (the rival battle, then up Route 1), `VIVY_GB_RUNS` times
//!   (5) in a row, with the skills she writes kept from one run to the next; with
//!   `VIVY_GB_REVIEWER` (an OpenAI model) reviewing her skills after each task
//!
//! They only run with `VIVY_GB_GATE=1`, so `cargo test gameboy -- --ignored` (the ROM
//! tests) never spends money. `VIVY_GB_GATE_USD` caps what one run spends at list prices (15). Results go in
//! `VIVY_GB_RESULTS` (the project's shared folder).

pub(super) mod player;
mod tasks;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

use super::strategist::{self, Ask, Calls, Care};
use super::supervisor::{Episode, Player, finish_stream, run_task};
use crate::ai::{
    ChatProvider, ChatRequest, Claude, ClaudeConfig, Gemini, GeminiConfig, Input, Job, OpenAi,
    OpenAiConfig, Part, Role, Spend, Turn,
};
use crate::core::db::{self, Db};
use player::Local;
use tasks::{Check, LONG_GOAL, LONG_START, TASKS, load, long_done};

const GAME: &str = "Pokémon Red";
/// Under this share of task runs done, the gate fails and the fallbacks get tried.
const PASS_SHARE: f64 = 0.5;
/// The long goal's episodes, and the actions in each.
const LONG_EPISODES: u32 = 15;
const LONG_ACTIONS: u32 = 30;
/// What the strategist reads before the first episode.
const NOTHING_YET: &str = "Nothing yet: this is the first task.";

fn setting(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Whether the runs that call models are on (`VIVY_GB_GATE=1`).
fn gate_on() -> bool {
    let on = std::env::var("VIVY_GB_GATE").is_ok_and(|v| v == "1");
    if !on {
        println!("Skipped: set VIVY_GB_GATE=1 to call the models.");
    }
    on
}

/// The first of these environment variables that's set. Keys are never printed.
fn key(names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| panic!("set one of {names:?}"))
}

/// The bot's HTTP client. Behind a proxy that re-signs TLS (a cloud session), its
/// certificate from `SSL_CERT_FILE` is trusted too.
fn http() -> reqwest::Client {
    let mut builder = crate::util::http::client_builder();
    if let Some(pem) = std::env::var("SSL_CERT_FILE")
        .ok()
        .and_then(|path| std::fs::read(path).ok())
    {
        for cert in reqwest::Certificate::from_pem_bundle(&pem).unwrap_or_default() {
            builder = builder.add_root_certificate(cert);
        }
    }
    builder.build().unwrap()
}

/// A database of its own, only for adding up what the runs spend.
async fn spend_db() -> Db {
    let db = Db::open_in_memory().await.unwrap();
    db.call(|conn| {
        db::migrate(conn, "core", db::CORE_MIGRATIONS)?;
        Ok(())
    })
    .await
    .unwrap();
    db
}

fn job(model: &str, effort: &str) -> HashMap<String, Job> {
    let job = Job {
        model: Some(model.to_string()),
        effort: Some(effort.to_string()),
        web: Some(false),
        code: Some(false),
    };
    let review = Job {
        effort: Some("high".to_string()),
        ..job.clone()
    };
    HashMap::from([
        ("game_play".to_string(), job.clone()),
        ("game_plan".to_string(), job),
        ("game_review".to_string(), review),
    ])
}

/// The model that plays, by its name: Claude, Gemini or OpenAI.
fn player_model(http: &reqwest::Client, db: &Db, model: &str) -> (Box<dyn ChatProvider>, Spend) {
    let jobs = job(model, &setting("VIVY_GB_EFFORT", "low"));
    if model.starts_with("gemini") {
        let spend = Spend::new(db.clone(), "gemini", 0.0);
        let config = GeminiConfig {
            jobs,
            ..GeminiConfig::default()
        };
        let key = key(&["GEMINI_API_KEY", "GOOGLE_API_KEY"]);
        let gemini = Gemini::new(http.clone(), key, config, Some(spend.clone()));
        (Box::new(gemini), spend)
    } else if model.starts_with("gpt") {
        (
            Box::new(openai(http, db, model, "low")),
            Spend::new(db.clone(), "openai", 0.0),
        )
    } else {
        let spend = Spend::new(db.clone(), "claude", 0.0);
        let config = ClaudeConfig {
            jobs,
            keep_cache_warm_minutes: 0,
            ..ClaudeConfig::default()
        };
        let key = key(&["VIVY_ANTHROPIC_API_KEY", "ANTHROPIC_API_KEY"]);
        let claude = Claude::new(http.clone(), key, config, Some(spend.clone()));
        (Box::new(claude), spend)
    }
}

/// OpenAI with `model` for the game jobs, at list prices (no "fast" tier).
fn openai(http: &reqwest::Client, db: &Db, model: &str, effort: &str) -> OpenAi {
    let config = OpenAiConfig {
        jobs: job(model, effort),
        service_tier: None,
        ..OpenAiConfig::default()
    };
    let spend = Spend::new(db.clone(), "openai", 0.0);
    OpenAi::new(
        http.clone(),
        key(&["OPENAI_API_KEY"]),
        None,
        config,
        Some(spend),
    )
}

/// USD spent so far by every provider, at list prices.
async fn spent(db: &Db) -> f64 {
    let mut usd = 0.0;
    for provider in ["claude", "openai", "gemini"] {
        usd += Spend::new(db.clone(), provider, 0.0)
            .this_month()
            .await
            .map(|m| m.usd)
            .unwrap_or(0.0);
    }
    usd
}

/// A new folder for this run's results.
fn results(what: &str) -> PathBuf {
    let folder = PathBuf::from(setting(
        "VIVY_GB_RESULTS",
        "/mnt/project-files/gameboy/gate",
    ))
    .join(format!(
        "{}-{what}",
        chrono::Utc::now().format("%Y-%m-%d-%H%M")
    ));
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn file_name(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Saves the screen as it is now, to see where a run ended.
fn save_screen(local: &mut Local, path: PathBuf) {
    let screen = local.machine.emulator.screen();
    if let Ok(png) = super::pictures::png_at(&screen, 2) {
        let _ = std::fs::write(path, png);
    }
}

/// One task's runs.
#[derive(Default)]
struct Row {
    name: &'static str,
    runs: u32,
    done: u32,
    /// Runs where the model said it was done and code agreed, or didn't.
    claimed_right: u32,
    claimed_wrong: u32,
    actions: u32,
    requests: u32,
    usd: f64,
    seconds: f64,
}

#[tokio::test]
#[ignore = "calls Claude, OpenAI and Gemini"]
async fn gate_keys_work() {
    if !gate_on() {
        return;
    }
    let http = http();
    let db = spend_db().await;
    let ask = |job: &'static str| ChatRequest {
        system: "Answer in one word.".to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts: vec![Part::Text("Say ok.".to_string())],
        }]),
        tools: Vec::new(),
        cache_key: "vivy:gate".to_string(),
        job,
    };
    let mut failed = Vec::new();
    for model in [
        setting("VIVY_GB_PLAYER", "claude-haiku-5-5"),
        setting("VIVY_GB_GEMINI", "gemini-3.8-flash"),
    ] {
        let (provider, _) = player_model(&http, &db, &model);
        match finish_stream(provider.as_ref(), ask("game_play")).await {
            Ok(_) => println!("{model}: works"),
            Err(err) => failed.push(format!("{model}: {err:#}")),
        }
    }
    let sol = openai(&http, &db, "gpt-6.1-sol", "low");
    let schema = json!({
        "type": "object",
        "properties": {"ok": {"type": "boolean"}},
        "required": ["ok"],
        "additionalProperties": false,
    });
    match sol
        .structured(
            "game_plan",
            "Answer as asked.",
            "Is this ok?",
            "check",
            &schema,
        )
        .await
    {
        Ok(answer) => println!("gpt-6.1-sol: works ({} tokens)", answer.tokens),
        Err(err) => failed.push(format!("gpt-6.1-sol: {err:#}")),
    }
    println!("spent ${:.4}", spent(&db).await);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[tokio::test]
#[ignore = "needs a ROM in VIVY_GB_ROM and the model keys; spends money"]
async fn gate_tasks() {
    if !gate_on() {
        return;
    }
    let http = http();
    let db = spend_db().await;
    let model = setting("VIVY_GB_PLAYER", "claude-haiku-5-5");
    let (provider, _) = player_model(&http, &db, &model);
    let hints = std::env::var("VIVY_GB_HINTS")
        .ok()
        .map(|m| openai(&http, &db, &m, "medium"));
    let runs: u32 = setting("VIVY_GB_RUNS", "3").parse().unwrap();
    let cap: f64 = setting("VIVY_GB_GATE_USD", "15").parse().unwrap();
    let only = std::env::var("VIVY_GB_ONLY").ok();
    let folder = results(&format!("tasks-{}", file_name(&model)));
    let mut log = String::new();
    let mut rows = Vec::new();
    let mut capped = false;

    'tasks: for task in TASKS
        .iter()
        .filter(|t| only.as_ref().is_none_or(|o| t.name.contains(o.as_str())))
    {
        let mut row = Row {
            name: task.name,
            ..Row::default()
        };
        for run in 1..=runs {
            if spent(&db).await >= cap {
                capped = true;
                rows.push(row);
                break 'tasks;
            }
            let (before, started) = (spent(&db).await, Instant::now());
            let mut local = Local::new(load(task.state));
            let hint = match &hints {
                Some(openai) => {
                    let look = local.look().await.unwrap();
                    let now = local.describe(&look).await.unwrap();
                    match strategist::plan(
                        openai,
                        Calls::One,
                        &Ask {
                            goal: task.text,
                            notes: &[],
                            milestones: &[],
                            skills: &[],
                            digest: NOTHING_YET,
                            now: &now,
                            care: Care::Routine,
                        },
                    )
                    .await
                    {
                        Ok(plan) => Some(plan.task),
                        Err(err) => {
                            writeln!(log, "(no hint: {err:#})").unwrap();
                            None
                        }
                    }
                }
                None => None,
            };
            let result = run_task(
                provider.as_ref(),
                &mut local,
                GAME,
                task.text,
                hint.as_deref(),
                task.most,
            )
            .await;
            let after = local.state();
            let episode = result.unwrap_or_else(|err| Episode {
                log: vec![format!("The model call failed: {err:#}")],
                ..Episode::default()
            });
            let done = (task.done)(&Check {
                after: &after,
                words: &episode.words,
            });
            let claimed = matches!(episode.finished, Some((true, _)));
            row.runs += 1;
            row.done += u32::from(done);
            row.claimed_right += u32::from(claimed && done);
            row.claimed_wrong += u32::from(claimed && !done);
            row.actions += episode.actions;
            row.requests += episode.requests;
            row.usd += spent(&db).await - before;
            row.seconds += started.elapsed().as_secs_f64();

            let mark = if done { "done" } else { "NOT done" };
            writeln!(log, "## {} #{run}: {mark}\n", task.name).unwrap();
            if let Some(hint) = &hint {
                writeln!(log, "Hint: {hint}\n").unwrap();
            }
            writeln!(
                log,
                "{}\nEnded at {} {:?}.\n",
                episode.digest(),
                after.map_name(),
                (after.x, after.y)
            )
            .unwrap();
            save_screen(
                &mut local,
                folder.join(format!("{}-{run}.png", file_name(task.name))),
            );
            println!("{} #{run}: {mark}, {} actions", task.name, episode.actions);
        }
        rows.push(row);
    }

    let report = task_report(&model, hints.is_some(), &rows, capped, spent(&db).await);
    std::fs::write(folder.join("report.md"), &report).unwrap();
    std::fs::write(folder.join("log.md"), &log).unwrap();
    println!("{report}\nResults in {}", folder.display());
}

fn task_report(model: &str, hints: bool, rows: &[Row], capped: bool, usd: f64) -> String {
    let mut out = String::new();
    let with = if hints { ", with hints" } else { "" };
    writeln!(out, "# Stage 3 tasks: {model}{with}\n").unwrap();
    writeln!(
        out,
        "| Task | Done | Said done, wrongly | Actions | Requests | Cost | Seconds |\n|---|---|---|---|---|---|---|"
    )
    .unwrap();
    let (mut runs, mut done) = (0, 0);
    for row in rows {
        runs += row.runs;
        done += row.done;
        let per = |n: f64| n / f64::from(row.runs.max(1));
        writeln!(
            out,
            "| {} | {}/{} | {} | {:.0} | {:.0} | ${:.3} | {:.0} |",
            row.name,
            row.done,
            row.runs,
            row.claimed_wrong,
            per(f64::from(row.actions)),
            per(f64::from(row.requests)),
            per(row.usd),
            per(row.seconds),
        )
        .unwrap();
    }
    let share = f64::from(done) / f64::from(runs.max(1));
    let verdict = if share >= PASS_SHARE {
        "passes"
    } else {
        "fails"
    };
    writeln!(
        out,
        "\nActions, requests, cost and seconds are per run. {done} of {runs} runs done ({:.0}%): \
         the gate {verdict} (it needs {:.0}%). Spent ${usd:.2} at list prices.",
        share * 100.0,
        PASS_SHARE * 100.0
    )
    .unwrap();
    if capped {
        writeln!(out, "\nStopped early: the spending cap was reached.").unwrap();
    }
    out
}

#[tokio::test]
#[ignore = "needs a ROM in VIVY_GB_ROM and the model keys; spends money"]
async fn gate_strategists() {
    if !gate_on() {
        return;
    }
    let http = http();
    let db = spend_db().await;
    let model = setting("VIVY_GB_PLAYER", "claude-haiku-5-5");
    let (provider, _) = player_model(&http, &db, &model);
    let strategists = setting("VIVY_GB_STRATEGISTS", "gpt-6-astra,gpt-6.1-sol");
    let effort = setting("VIVY_GB_PLAN_EFFORT", "medium");
    let runs: u32 = setting("VIVY_GB_RUNS", "1").parse().unwrap();
    let cap: f64 = setting("VIVY_GB_GATE_USD", "15").parse().unwrap();
    let folder = results(&format!("strategists-{}", file_name(&model)));
    let mut log = String::new();
    let mut report = format!(
        "# Stage 3 strategists, {model} playing\n\nGoal: {LONG_GOAL}\n\n\
         | Strategist | Calls | Done | Episodes | Actions | Strategist tokens | Strategist cost | Player cost |\n\
         |---|---|---|---|---|---|---|---|\n"
    );

    for strategist_model in strategists.split(',').map(str::trim) {
        let openai = openai(&http, &db, strategist_model, &effort);
        for calls in [Calls::One, Calls::Split] {
            for run in 1..=runs {
                if spent(&db).await >= cap {
                    report.push_str("\nStopped early: the spending cap was reached.\n");
                    break;
                }
                let name = format!("{strategist_model} {calls:?} #{run}");
                writeln!(log, "# {name}\n").unwrap();
                let mut local = Local::new(load(LONG_START));
                let (mut notes, mut digest) = (Vec::new(), NOTHING_YET.to_string());
                let (mut episodes, mut actions, mut tokens) = (0, 0, 0);
                let (mut plan_usd, mut play_usd) = (0.0, 0.0);
                let mut done = false;
                while episodes < LONG_EPISODES && !done && spent(&db).await < cap {
                    episodes += 1;
                    let look = local.look().await.unwrap();
                    let now = local.describe(&look).await.unwrap();
                    let before = spent(&db).await;
                    let plan = match strategist::plan(
                        &openai,
                        calls,
                        &Ask {
                            goal: LONG_GOAL,
                            notes: &notes,
                            milestones: &[],
                            skills: &local.world.skills,
                            digest: &digest,
                            now: &now,
                            care: Care::Routine,
                        },
                    )
                    .await
                    {
                        Ok(plan) => plan,
                        Err(err) => {
                            writeln!(log, "The strategist failed: {err:#}\n").unwrap();
                            break;
                        }
                    };
                    tokens += plan.tokens;
                    let reviewed =
                        super::skills::review(&mut local.world.skills, &plan.skill_changes);
                    plan_usd += spent(&db).await - before;
                    let before = spent(&db).await;
                    let episode = run_task(
                        provider.as_ref(),
                        &mut local,
                        GAME,
                        &plan.task,
                        None,
                        LONG_ACTIONS,
                    )
                    .await
                    .unwrap_or_else(|err| Episode {
                        log: vec![format!("The model call failed: {err:#}")],
                        ..Episode::default()
                    });
                    play_usd += spent(&db).await - before;
                    actions += episode.actions;
                    done = long_done(&local.state());
                    digest = episode.digest();
                    writeln!(
                        log,
                        "## Episode {episodes}\n\nTask: {}\n\nNotes:\n{}\n\nSkill changes: {}\n\n{digest}",
                        plan.task,
                        plan.notes
                            .iter()
                            .map(|n| format!("- {}", n.for_plan()))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        if reviewed.is_empty() { "none".to_string() } else { reviewed.join("; ") }
                    )
                    .unwrap();
                    notes = plan.notes;
                }
                save_screen(&mut local, folder.join(format!("{}.png", file_name(&name))));
                let mark = if done { "yes" } else { "no" };
                writeln!(
                    report,
                    "| {strategist_model} | {calls:?} | {mark} | {episodes} | {actions} | {tokens} | ${plan_usd:.2} | ${play_usd:.2} |"
                )
                .unwrap();
                println!("{name}: done {mark} after {episodes} episodes");
            }
        }
    }
    writeln!(
        report,
        "\nAt most {LONG_EPISODES} episodes of {LONG_ACTIONS} actions. Costs are list prices; \
         the strategist's tokens are free on the daily data-sharing tokens. Spent ${:.2}.",
        spent(&db).await
    )
    .unwrap();
    std::fs::write(folder.join("report.md"), &report).unwrap();
    std::fs::write(folder.join("log.md"), &log).unwrap();
    println!("{report}\nResults in {}", folder.display());
}

/// The tasks `gate_learning` plays in turn: battles, where writing a skill should pay.
const LEARN_TASKS: &str = "rival battle,up Route 1";

#[tokio::test]
#[ignore = "needs a ROM in VIVY_GB_ROM and the model keys; spends money"]
async fn gate_learning() {
    if !gate_on() {
        return;
    }
    let http = http();
    let db = spend_db().await;
    let model = setting("VIVY_GB_PLAYER", "claude-haiku-5-5");
    let (provider, _) = player_model(&http, &db, &model);
    let runs: u32 = setting("VIVY_GB_RUNS", "5").parse().unwrap();
    let cap: f64 = setting("VIVY_GB_GATE_USD", "15").parse().unwrap();
    let names = setting("VIVY_GB_LEARN", LEARN_TASKS);
    let reviewer = std::env::var("VIVY_GB_REVIEWER")
        .ok()
        .filter(|m| !m.trim().is_empty());
    let openai = reviewer
        .as_ref()
        .map(|m| openai(&http, &db, m, &setting("VIVY_GB_PLAN_EFFORT", "medium")));
    let reviewed_by = reviewer
        .as_ref()
        .map(|m| format!("-reviewed-by-{}", file_name(m)))
        .unwrap_or_default();
    let folder = results(&format!("learning-{}{reviewed_by}", file_name(&model)));
    let mut log = String::new();
    let mut report = format!(
        "# Stage 4 learning: {model}{}\n\nEach run starts from the same state, with the skills \
         written in the runs before it.\n\n\
         | Run | Task | Done | Actions | Skills written | Skills used | Review | Cost |\n\
         |---|---|---|---|---|---|---|---|\n",
        reviewer
            .as_ref()
            .map(|m| format!(", skills reviewed by {m}"))
            .unwrap_or_default()
    );
    let mut known: Vec<super::skills::Skill> = Vec::new();
    let count = |episode: &Episode, action: &str| {
        episode
            .log
            .iter()
            .filter(|line| line.starts_with(action))
            .count()
    };

    'runs: for run in 1..=runs {
        for name in names.split(',').map(str::trim) {
            let task = TASKS.iter().find(|t| t.name == name).expect("a gate task");
            if spent(&db).await >= cap {
                report.push_str("\nStopped early: the spending cap was reached.\n");
                break 'runs;
            }
            let before = spent(&db).await;
            let mut local = Local::new(load(task.state));
            local.world.skills = known.clone();
            let mut result = run_task(
                provider.as_ref(),
                &mut local,
                GAME,
                task.text,
                None,
                task.most,
            )
            .await;
            // A busy model (Claude's 529, after the provider's own retries) says nothing
            // about her play: wait, then play the task again from the start, once.
            if let Err(err) = &result {
                writeln!(log, "(the model call failed, playing it again: {err:#})\n").unwrap();
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                local = Local::new(load(task.state));
                local.world.skills = known.clone();
                result = run_task(
                    provider.as_ref(),
                    &mut local,
                    GAME,
                    task.text,
                    None,
                    task.most,
                )
                .await;
            }
            let episode = result.unwrap_or_else(|err| Episode {
                log: vec![format!("The model call failed: {err:#}")],
                ..Episode::default()
            });
            let after = local.state();
            let done = (task.done)(&Check {
                after: &after,
                words: &episode.words,
            });
            // The reviewer sees what the task did, as the strategist will after each
            // episode, and edits her skills; the next task it plans is the gate's anyway.
            let mut reviewed = Vec::new();
            if let Some(openai) = &openai {
                let look = local.look().await.unwrap();
                let now = local.describe(&look).await.unwrap();
                match strategist::plan(
                    openai,
                    Calls::One,
                    &Ask {
                        goal: task.text,
                        notes: &[],
                        milestones: &[],
                        skills: &local.world.skills,
                        digest: &episode.digest(),
                        now: &now,
                        care: Care::Routine,
                    },
                )
                .await
                {
                    Ok(plan) => {
                        reviewed =
                            super::skills::review(&mut local.world.skills, &plan.skill_changes)
                    }
                    Err(err) => reviewed.push(format!("the review failed: {err:#}")),
                }
            }
            known = local.world.skills.clone();
            let mark = if done { "done" } else { "NOT done" };
            writeln!(
                report,
                "| {run} | {name} | {mark} | {} | {} | {} | {} | ${:.3} |",
                episode.actions,
                count(&episode, "write_skill"),
                count(&episode, "use_skill"),
                reviewed.len(),
                spent(&db).await - before
            )
            .unwrap();
            writeln!(log, "## Run {run}, {name}: {mark}\n\n{}", episode.digest()).unwrap();
            if !reviewed.is_empty() {
                writeln!(log, "Review: {}\n", reviewed.join("; ")).unwrap();
            }
            save_screen(
                &mut local,
                folder.join(format!("{run}-{}.png", file_name(name))),
            );
            println!("{run} {name}: {mark}, {} actions", episode.actions);
        }
    }
    let written: Vec<String> = known.iter().map(super::skills::Skill::written).collect();
    writeln!(
        report,
        "\nSpent ${:.2} at list prices.\n\n## Her skills at the end\n\n{}",
        spent(&db).await,
        if written.is_empty() {
            "None.".to_string()
        } else {
            written.join("\n\n")
        }
    )
    .unwrap();
    std::fs::write(folder.join("report.md"), &report).unwrap();
    std::fs::write(folder.join("log.md"), &log).unwrap();
    std::fs::write(
        folder.join("skills.json"),
        serde_json::to_string_pretty(&known).unwrap(),
    )
    .unwrap();
    println!("{report}\nResults in {}", folder.display());
}

/// Stage 4b: one whole session as the bot plays it (`session::play`), with the real
/// models: Haiku acting, the strategist (`VIVY_GB_REVIEWER`'s model, `gpt-6-astra`)
/// planning, its checkpoints and stuck ladder. It starts from a new game in her bedroom,
/// or from the task state `VIVY_GB_START`, with the default goal or `VIVY_GB_GOAL`, for
/// `VIVY_GB_MINUTES` (30) or `VIVY_GB_EPISODES` (40). `VIVY_GB_GATE_USD` caps the paid
/// models (3). The report has every episode's line, her notes and skills at the end, and
/// the saves folder (journal, maps, skills, checkpoints).
#[tokio::test]
#[ignore = "needs a ROM in VIVY_GB_ROM and the model keys; spends money"]
async fn gate_session() {
    use super::session::{self, Game, Limits, Session};
    if !gate_on() {
        return;
    }
    let http = http();
    let db = spend_db().await;
    let model = setting("VIVY_GB_PLAYER", "claude-haiku-5-5");
    let (provider, _) = player_model(&http, &db, &model);
    let planner = setting("VIVY_GB_REVIEWER", "gpt-6-astra");
    let strategist = openai(
        &http,
        &db,
        &planner,
        &setting("VIVY_GB_PLAN_EFFORT", "medium"),
    );
    let start = setting("VIVY_GB_START", "");
    let goal = std::env::var("VIVY_GB_GOAL").ok();
    let folder = results(&format!("session-{}", file_name(&planner)));
    let saves = folder.join("saves");
    let mut local = if start.is_empty() {
        Local::new(super::red::tests::new_game())
    } else {
        Local::new(load(&start))
    };
    let spends = [
        Spend::new(db.clone(), "claude", 0.0),
        Spend::new(db.clone(), "openai", 0.0),
        Spend::new(db.clone(), "gemini", 0.0),
    ];
    let (report, mut lines) = tokio::sync::mpsc::unbounded_channel();
    // With VIVY_GB_WEB (like 127.0.0.1:8787) she plays on the bot's Game Boy, the way
    // `/games gameboy play` does, and the live web page shows it.
    let web = std::env::var("VIVY_GB_WEB").ok();
    let (now, watching) = tokio::sync::watch::channel(super::session::Now::default());
    let session = Session {
        provider: provider.as_ref(),
        openai: Some(&strategist),
        spends: &spends,
        game: GAME.to_string(),
        saves_dir: saves.clone(),
        rom: "red.gb".to_string(),
        limits: Limits {
            minutes: setting("VIVY_GB_MINUTES", "30").parse().unwrap(),
            episodes: setting("VIVY_GB_EPISODES", "40").parse().unwrap(),
            daily_usd: setting("VIVY_GB_GATE_USD", "3").parse().unwrap(),
            free_tokens: 1_000_000,
        },
        stop: tokio_util::sync::CancellationToken::new(),
        report,
        now: web.is_some().then_some(now),
        timer: None,
        hints: Default::default(),
    };
    // Each line as it comes, so a long run can be followed.
    let printer = tokio::spawn(async move {
        let mut all = Vec::new();
        while let Some(report) = lines.recv().await {
            println!("{}", report.line);
            all.push(report.line);
        }
        all
    });
    let started = Instant::now();
    let result = match &web {
        None => session::play(&session, &mut local, goal.as_deref()).await,
        Some(bind) => {
            use super::console::console;
            let Local { mut machine, world } = local;
            machine.world = world;
            console().with(move |m| *m = Some(machine)).await.unwrap();
            let stop = tokio_util::sync::CancellationToken::new();
            let (bind, serving) = (bind.clone(), stop.clone());
            tokio::spawn(async move { super::web::serve(&bind, serving).await.unwrap() });
            tokio::spawn(super::web::follow(GAME.to_string(), watching));
            let result =
                session::play(&session, &mut super::player::OnConsole, goal.as_deref()).await;
            let mut machine = console().with(|m| m.take()).await.unwrap().unwrap();
            let world = std::mem::take(&mut machine.world);
            local = Local { machine, world };
            stop.cancel();
            result
        }
    };
    drop(session);
    let all = printer.await.unwrap();
    save_screen(&mut local, folder.join("end.png"));
    let world = local.world().await.unwrap();
    let today: super::budget::Today = serde_json::from_str(
        &std::fs::read_to_string(saves.join("today.json")).unwrap_or_default(),
    )
    .unwrap_or_default();
    let journal: super::journal::Journal = serde_json::from_str(
        &std::fs::read_to_string(saves.join("red").join("journal.json")).unwrap_or_default(),
    )
    .unwrap_or_default();
    let mut out = format!(
        "# Stage 4b session: {model} playing, {planner} planning

Start: {}. Goal: {}.

",
        if start.is_empty() {
            "a new game, in her bedroom"
        } else {
            &start
        },
        journal.goal.trim_end_matches('.')
    );
    match &result {
        Ok(s) => writeln!(
            out,
            "Ended: {} {} episodes in {:.0} minutes. New areas: {}. {} checkpoints, {} \
             rollbacks, {} cheap plans. Paid models ${:.2}; {} free tokens; today's \
             counter {} tokens.\n",
            s.ended.line(),
            s.episodes,
            started.elapsed().as_secs_f64() / 60.0,
            if s.new_areas.is_empty() {
                "none".to_string()
            } else {
                s.new_areas.join(", ")
            },
            s.checkpoints,
            s.rollbacks,
            s.cheap_plans,
            s.usd,
            s.free_tokens,
            today.free_tokens
        )
        .unwrap(),
        Err(err) => writeln!(
            out,
            "The session broke off: {err:#}
"
        )
        .unwrap(),
    }
    writeln!(
        out,
        "Spent ${:.2} at list prices in all.

## Episodes
",
        spent(&db).await
    )
    .unwrap();
    for line in &all {
        writeln!(out, "- {line}").unwrap();
    }
    writeln!(
        out,
        "
## Her notes at the end
"
    )
    .unwrap();
    for note in &journal.notes {
        writeln!(out, "- {}", note.shown()).unwrap();
    }
    writeln!(
        out,
        "
## Her milestones at the end
"
    )
    .unwrap();
    if journal.milestones.is_empty() {
        writeln!(out, "(none)").unwrap();
    }
    for milestone in &journal.milestones {
        writeln!(out, "- {}", milestone.for_plan()).unwrap();
    }
    writeln!(
        out,
        "
## Her skills at the end
"
    )
    .unwrap();
    for skill in &world.skills {
        writeln!(
            out,
            "{}
",
            skill.written()
        )
        .unwrap();
    }
    writeln!(
        out,
        "## The last task's digest

{}",
        journal.digest
    )
    .unwrap();
    std::fs::write(folder.join("report.md"), &out).unwrap();
    println!(
        "{out}
Results in {}",
        folder.display()
    );
    result.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_says_whether_the_gate_passes() {
        let rows = [
            Row {
                name: "go downstairs",
                runs: 3,
                done: 2,
                claimed_wrong: 1,
                actions: 9,
                requests: 12,
                usd: 0.03,
                seconds: 30.0,
                ..Row::default()
            },
            Row {
                name: "talk to Mom",
                runs: 3,
                done: 0,
                ..Row::default()
            },
        ];
        let report = task_report("claude-haiku-5-5", false, &rows, false, 0.05);
        assert!(
            report.contains("| go downstairs | 2/3 | 1 | 3 | 4 | $0.010 | 10 |"),
            "{report}"
        );
        assert!(
            report.contains("2 of 6 runs done (33%): the gate fails"),
            "{report}"
        );
    }

    /// Plays some tasks with the actions a model would pick, to check the actions and the
    /// tasks' checks with no model.
    #[tokio::test]
    #[ignore = "needs a ROM in VIVY_GB_ROM and the task states"]
    async fn actions_carry_out_tasks_by_script() {
        async fn play(state: &str, actions: &[(&str, Value)]) -> (Local, Vec<String>) {
            let mut local = Local::new(load(state));
            let mut words = Vec::new();
            for (name, args) in actions {
                let acted = local.act(name, args).await.unwrap();
                println!("{name} {args}: {}", acted.outcome);
                words.extend(acted.words);
            }
            let view = local.look().await.unwrap();
            println!("{words:?}\n{}\n", local.describe(&view).await.unwrap());
            (local, words)
        }
        fn passes(name: &str, local: &mut Local, words: &[String]) -> bool {
            let task = TASKS.iter().find(|t| t.name == name).unwrap();
            let after = local.state();
            (task.done)(&Check {
                after: &after,
                words,
            })
        }
        let walk = |x: i32, y: i32| ("walk_to", json!({"x": x, "y": y}));
        let press = |buttons: &str| ("press", json!({"buttons": buttons}));
        let read = || ("read_text", json!({}));

        let (mut local, words) = play("bedroom", &[walk(7, 1)]).await;
        assert!(passes("go downstairs", &mut local, &words));
        assert!(!passes("potion from the PC", &mut local, &words));

        // The PC is at (0, 1): face it from below. WITHDRAW ITEM is the first line.
        let pc = [
            walk(0, 2),
            press("up a"),
            read(),
            press("a"),
            read(),
            press("a"),
            read(),
            press("a"),
            read(),
        ];
        let (mut local, words) = play("bedroom", &pc).await;
        assert!(
            passes("potion from the PC", &mut local, &words),
            "{words:?}"
        );

        // Mom sits at (5, 4): from her right, face left and talk.
        let (mut local, words) = play("downstairs", &[walk(6, 4), press("left a"), read()]).await;
        assert!(passes("talk to Mom", &mut local, &words), "{words:?}");

        // The town sign stands at (7, 9): read it from below.
        let (mut local, words) = play("pallet", &[walk(7, 10), press("up a"), read()]).await;
        assert!(
            passes("read the town sign", &mut local, &words),
            "{words:?}"
        );

        // The rival stops her on the way out. In battle, A picks FIGHT and the first move.
        let mut local = Local::new(load("starter"));
        let mut words = Vec::new();
        let mut acted = local
            .act("walk_to", &json!({"x": 5, "y": 11}))
            .await
            .unwrap();
        for _ in 0..40 {
            println!("{}", acted.outcome);
            words.extend(acted.words.clone());
            if passes("rival battle", &mut local, &words) && acted.view.can_walk {
                break;
            }
            let (name, args) = if acted.view.text.contains('▶') {
                press("a")
            } else {
                read()
            };
            acted = local.act(name, &args).await.unwrap();
        }
        println!("{:?}", &words[words.len().saturating_sub(12)..]);
        assert!(passes("rival battle", &mut local, &words));

        // Before the Pokédex, SAVE is the start menu's fourth line.
        let save = [
            press("start"),
            press("down down down a"),
            read(),
            press("a"),
            read(),
        ];
        let (mut local, words) = play("parcel", &save).await;
        assert!(passes("save the game", &mut local, &words), "{words:?}");
    }

    /// Skills as she might write them, run on the real game: one that presses A through the
    /// rival's battle, then on Route 1 one that runs from a wild Pokémon and one that fights.
    /// Code only follows the rules; nothing in it knows what a battle is.
    #[tokio::test]
    #[ignore = "needs a ROM in VIVY_GB_ROM and the task states"]
    async fn skills_she_writes_carry_out_battles() {
        async fn act(
            local: &mut Local,
            name: &str,
            args: Value,
        ) -> crate::features::games::gameboy::actions::Acted {
            let acted = local.act(name, &args).await.unwrap();
            println!("{name}: {}", acted.outcome);
            acted
        }
        let mash = json!({
            "name": "battle",
            "use_when": "a battle starts",
            "rules": [{"press": "a a"}],
            "most": 50
        });
        let run_away = json!({
            "name": "run away",
            "use_when": "a wild Pokémon I don't want to fight",
            "rules": [{"if_words": "FIGHT", "press": "down right a"}, {"press": "a"}],
            "stop_words": ["Can't escape"]
        });
        let fight = json!({
            "name": "fight",
            "use_when": "a wild Pokémon to beat",
            "rules": [{"if_words": "FIGHT", "press": "up left a"}, {"press": "a"}],
            "stop_words": ["learn", "which"],
            "most": 50
        });

        let mut local = Local::new(load("starter"));
        act(&mut local, "walk_to", json!({"x": 5, "y": 11})).await;
        act(&mut local, "write_skill", mash).await;
        let acted = act(&mut local, "use_skill", json!({"name": "Battle"})).await;
        assert!(local.state().story.contains(&"BATTLED_RIVAL_IN_OAKS_LAB"));
        assert!(acted.view.can_walk && acted.outcome.contains("you can walk"));
        assert!(
            acted.words.iter().any(|w| w.contains("defeated")),
            "{:?}",
            acted.words
        );
        assert_eq!(local.world.skills[0].uses, 1);

        // Pace back and forth on two steps of grass until a wild Pokémon shows up.
        async fn meet_one(local: &mut Local) {
            use crate::features::games::gameboy::map::Kind;
            let grass = |local: &Local, (x, y): (i32, i32)| {
                local.world.areas["Route 1"].kind((x, y)) == Some(Kind::Grass)
            };
            let mut patch = None;
            for _ in 0..3 {
                let at = local.look().await.unwrap().view.at;
                patch = (0..40)
                    .flat_map(|y| (0..20).map(move |x| (x, y)))
                    .filter(|&(x, y)| grass(local, (x, y)) && grass(local, (x + 1, y)))
                    .min_by_key(|&(x, y)| (x - at.0).abs() + (y - at.1).abs());
                if patch.is_some() {
                    break;
                }
                act(local, "explore", json!({})).await;
            }
            let (x, y) = patch.expect("no grass seen");
            act(local, "walk_to", json!({"x": x, "y": y})).await;
            for i in 0..200 {
                let way = if i % 2 == 0 { "right" } else { "left" };
                act(local, "press", json!({"buttons": way})).await;
                if local.state().battle == 1 {
                    return;
                }
            }
            panic!("never met one");
        }
        let mut local = Local::new(load("route1"));
        // A step past the area's edge is refused, rather than wandered toward.
        let acted = act(&mut local, "walk_to", json!({"x": 10, "y": -6})).await;
        assert!(
            acted.outcome.contains("is outside Route 1"),
            "{}",
            acted.outcome
        );
        act(&mut local, "write_skill", run_away).await;
        act(&mut local, "write_skill", fight).await;
        meet_one(&mut local).await;
        // What code reads of the battle is only facts for her prompt, once both are out.
        let facts = local.look().await.unwrap().view.facts;
        assert!(facts.contains("In a battle with a wild"), "{facts}");
        assert!(!facts.contains("#0"), "{facts}");
        let acted = act(&mut local, "use_skill", json!({"name": "run away"})).await;
        println!("{:?}", acted.words);
        assert!(acted.view.can_walk, "{:?}", acted.view);
        assert!(acted.words.iter().any(|w| w.contains("Got away")));

        meet_one(&mut local).await;
        let facts = act(&mut local, "read_text", json!({})).await.view.facts;
        assert!(facts.contains("Out now: Charmander Lv6"), "{facts}");
        assert!(facts.contains("Moves: 1 Scratch (PP "), "{facts}");
        let level = local.state().party[0].level;
        let acted = act(&mut local, "use_skill", json!({"name": "fight"})).await;
        println!("{:?}", acted.words);
        assert!(acted.view.can_walk, "{:?}", acted.view);
        assert!(
            acted
                .words
                .iter()
                .any(|w| w.contains("fainted") || w.contains("EXP"))
        );
        println!("level {level} -> {}", local.state().party[0].level);
        let listed = local.describe(&acted).await.unwrap();
        println!("{listed}");
        assert!(listed.contains("- fight: a wild Pokémon to beat (used once"));
    }

    /// At the top of Route 1 some unseen steps are past the map's edge: exploring has to
    /// end there, not aim at them forever, and walking past the edge is refused.
    #[tokio::test]
    #[ignore = "needs a ROM in VIVY_GB_ROM and the task states"]
    async fn exploring_ends_at_the_edge_of_a_route() {
        let mut local = Local::new(load("route1"));
        let acted = local
            .act("walk_to", &json!({"x": 10, "y": 0}))
            .await
            .unwrap();
        println!("{}", acted.outcome);
        assert_eq!(acted.view.at, (10, 0), "a wild Pokémon got in the way");
        for _ in 0..4 {
            let acted = local.act("explore", &json!({})).await.unwrap();
            println!("explore: {}", acted.outcome);
        }
        // Past the top, a tree (3, -1) and a step not seen (2, -5): neither is a way out.
        for (x, y) in [(3, -1), (2, -5)] {
            let acted = local
                .act("walk_to", &json!({"x": x, "y": y}))
                .await
                .unwrap();
            assert!(
                acted.outcome.contains("is outside Route 1"),
                "{}",
                acted.outcome
            );
        }
    }

    #[test]
    fn file_names_keep_letters_and_digits() {
        assert_eq!(file_name("claude-haiku-5.5"), "claude-haiku-5-5");
        assert_eq!(file_name("talk to Mom"), "talk-to-Mom");
    }
}

/// Her commentary over a real session, from its report (`VIVY_GB_REPORT`, a `report.md`
/// that `gate_session` wrote): she's told what happened every `VIVY_GB_NARRATE_MINUTES`
/// (3, since a test session is short) of the episodes' clock times, the way the bot would,
/// and what she says goes in `narrate.md` next to the report. Her own model
/// (`VIVY_GB_NARRATOR`, Opus) at the job's effort.
#[tokio::test]
#[ignore = "needs the Claude key and a session's report; spends money"]
async fn gate_narrate() {
    use super::narrate::{Moment, Since, brief, due, message, say};
    use super::session::{Now, Report, Reported};
    use std::time::Duration;
    if !gate_on() {
        return;
    }
    let path = PathBuf::from(setting("VIVY_GB_REPORT", ""));
    let report = std::fs::read_to_string(&path).expect("VIVY_GB_REPORT: a session's report.md");
    let every: u64 = setting("VIVY_GB_NARRATE_MINUTES", "3").parse().unwrap();
    let http = http();
    let db = spend_db().await;
    let spend = Spend::new(db.clone(), "claude", 0.0);
    let model = setting("VIVY_GB_NARRATOR", "claude-opus-5-5");
    let config = ClaudeConfig {
        model,
        keep_cache_warm_minutes: 0,
        ..ClaudeConfig::default()
    };
    let key = key(&["VIVY_ANTHROPIC_API_KEY", "ANTHROPIC_API_KEY"]);
    let claude = Claude::new(http.clone(), key, config, Some(spend.clone()));

    let goal = report
        .lines()
        .find_map(|l| l.split_once("Goal: ").map(|(_, g)| g.trim().to_string()))
        .unwrap_or_default();
    let notes: Vec<String> = report
        .split("## Her notes at the end")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .map(|notes| {
            notes
                .lines()
                .filter_map(|l| l.strip_prefix("- ").map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let ended = report
        .lines()
        .find_map(|l| l.strip_prefix("Ended: ").map(str::to_string))
        .unwrap_or_default();
    // "- #12 10-20 22:16 Viridian City: ..." and "- 📍 **Checkpoint 5** · Viridian City"
    let mut said: Vec<String> = Vec::new();
    let mut out = String::from("# Her commentary\n\n");
    let mut now = Now {
        session: 1,
        goal,
        notes,
        ..Now::default()
    };
    let mut since = Since::default();
    let (mut spoke, mut passes, mut first) = (None::<u32>, 0, true);
    let minute = |line: &str| -> Option<u32> {
        let time = line.split_whitespace().nth(2)?;
        let (h, m) = time.split_once(':')?;
        Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
    };
    let speak = async |moment: Moment, now: &Now, since: &Since, said: &mut Vec<String>| {
        let text = say(
            &claude,
            brief(&moment, "Pokémon Red", now, since, said, &[]),
            0,
        )
        .await
        .unwrap();
        let posted = message(&text);
        println!("{moment:?}: {text}");
        if let Some(posted) = &posted {
            said.push(posted.clone());
            if said.len() > 3 {
                said.remove(0);
            }
        }
        posted
    };
    for line in report.lines() {
        let Some(line) = line.strip_prefix("- ") else {
            continue;
        };
        let kind = if line.starts_with("📍") {
            Reported::Checkpoint
        } else if line.starts_with('#') {
            Reported::Episode
        } else {
            continue;
        };
        if kind == Reported::Episode
            && let Some(at) = minute(line)
        {
            now.area = line
                .splitn(4, ' ')
                .nth(3)
                .and_then(|rest| rest.split(':').next())
                .unwrap_or_default()
                .to_string();
            if first {
                first = false;
                now.started = chrono::Utc::now().timestamp();
                let text = speak(Moment::Start, &now, &since, &mut said).await;
                out.push_str(&format!(
                    "**Start** ({})\n> {}\n\n",
                    now.area,
                    text.unwrap_or_default()
                ));
                spoke = Some(at);
            }
            let waited = Duration::from_secs(u64::from(at - spoke.unwrap_or(at)) * 60);
            if let Some(may_pass) = due(&since, waited, Duration::from_secs(every * 60), passes) {
                let moment = Moment::Check { may_pass };
                let text = speak(moment.clone(), &now, &since, &mut said).await;
                out.push_str(&format!(
                    "**{moment:?}** after {} episodes, new: {:?}\n> {}\n\n",
                    since.episodes.len(),
                    since.areas,
                    text.as_deref().unwrap_or("(passed)")
                ));
                passes = if text.is_some() { 0 } else { passes + 1 };
                since = Since::default();
                spoke = Some(at);
            }
        }
        since.add(&Report {
            kind,
            line: line.to_string(),
            picture: None,
        });
    }
    let text = speak(Moment::End { how: ended.clone() }, &now, &since, &mut said).await;
    out.push_str(&format!(
        "**End** ({ended})\n> {}\n",
        text.unwrap_or_default()
    ));
    let usd = spend.this_month().await.map(|m| m.usd).unwrap_or(0.0);
    out.push_str(&format!("\nSpent ${usd:.3} at list prices.\n"));
    let written = path.with_file_name("narrate.md");
    std::fs::write(&written, &out).unwrap();
    println!("{out}\nWritten to {}", written.display());
}
