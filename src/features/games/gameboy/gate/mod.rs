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
//!
//! They only run with `VIVY_GB_GATE=1`, so `cargo test gameboy -- --ignored` (the ROM
//! tests) never spends money. `VIVY_GB_GATE_USD` caps what one run spends at list prices (15). Results go in
//! `VIVY_GB_RESULTS` (the project's shared folder).

mod player;
mod tasks;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

use super::strategist::{self, Calls};
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
    HashMap::from([
        ("game_play".to_string(), job.clone()),
        ("game_plan".to_string(), job),
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
                    match strategist::plan(openai, Calls::One, task.text, &[], NOTHING_YET, &now)
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
                    let plan =
                        match strategist::plan(&openai, calls, LONG_GOAL, &notes, &digest, &now)
                            .await
                        {
                            Ok(plan) => plan,
                            Err(err) => {
                                writeln!(log, "The strategist failed: {err:#}\n").unwrap();
                                break;
                            }
                        };
                    tokens += plan.tokens;
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
                        "## Episode {episodes}\n\nTask: {}\n\nNotes:\n{}\n\n{digest}",
                        plan.task,
                        plan.notes
                            .iter()
                            .map(|n| format!("- {n}"))
                            .collect::<Vec<_>>()
                            .join("\n")
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

    #[test]
    fn file_names_keep_letters_and_digits() {
        assert_eq!(file_name("claude-haiku-5.5"), "claude-haiku-5-5");
        assert_eq!(file_name("talk to Mom"), "talk-to-Mom");
    }
}
