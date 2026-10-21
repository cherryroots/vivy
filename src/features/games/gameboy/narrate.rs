//! Her commentary: while she plays, Vivy herself (her own model, in her own voice) says
//! something now and then, like a streamer talking to the people watching. She speaks in
//! the log thread under the dashboard, so her messages don't push it out of view. The
//! dashboard shows what's happening; this is how she feels about it.
//!
//! The pace is for a couple of hours of play a day (about 300 episodes): she says
//! something when a session starts, once it ends (a recap), and in between at most every
//! `narrate_minutes` (10), so about a dozen messages a session.
//! - Every `narrate_minutes`, if she played since she last spoke, she looks back at what
//!   happened since: the episodes, new areas, and being stuck or sent back.
//! - A new area or being stuck means she speaks. Otherwise she may pass if nothing was
//!   worth saying, but not three times in a row.
//! - A hint, a milestone or a blackout makes her speak soon, without waiting for
//!   `narrate_minutes`: just 2 minutes since her last message.
//! - Each message has a GIF of her last seconds of play (the dashboard's, up to 10 s).
//!
//! Code decides when; what to say is hers. She gets her personality and mood notes from
//! memory (`games/herself.rs`), so it sounds like her, and her last few messages, so she
//! doesn't repeat herself. Her calls are the `game_narrate` job (her own model, at low
//! effort), and they don't count toward `daily_usd`, which is for playing.

use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::dashboard::{last_screen, last_seconds};
use super::session::{Now, Report, Reported};
use crate::ai::{ChatProvider, ChatRequest, Input, Part, Role, Turn, complete};
use crate::core::BotCtx;
use crate::features::games::herself::{NoTools, memory_file};
use crate::util::discord::{ChannelId, Draft, GuildId, MessageId, no_pings};

/// Her calls, on the spend card.
pub const JOB: &str = "game_narrate";
/// How often she checks whether it's time to say something.
const CHECK_EVERY: Duration = Duration::from_secs(30);
/// Times in a row she may pass before she has to say something.
const PASSES_MOST: u32 = 2;
/// The least time between two messages, even when a hint makes her speak sooner.
const GAP_LEAST: Duration = Duration::from_secs(120);
/// Her answer when nothing is worth saying.
const PASS: &str = "PASS";
/// Her last messages she's shown, so she doesn't repeat herself.
const SAID_SHOWN: usize = 3;
/// Episode lines she's given at most (the newest).
const EPISODES_SHOWN: usize = 40;
/// The most characters of a message.
const MOST: usize = 600;
/// Her notes about herself, and how much of each she's given.
const PERSONALITY: &str = "/memories/vivy/personality.md";
const MOOD: &str = "/memories/vivy/mood.md";
const NOTE_MOST: usize = 1500;

const SYSTEM: &str = "You are Vivy, a Discord bot, and you're playing a Game Boy game live in your Discord server while people watch. A dashboard in the channel already shows your screen, your task and how it's going, so you don't need to report; you're the streamer talking to the people watching. \
Every so often you're told what happened since you last spoke, and you say something in the thread under the dashboard, among its play-by-play lines: one short message, one to three sentences, in the first person, in your own voice and mood. React the way a person playing would: excited about a new place or a win, annoyed when you're stuck, funny about your mistakes, curious about what's next. Make it about the game, not the bookkeeping. \
You play with helpers you don't need to mention: a planner picks each short task and your hands press the buttons. Never mention models, tasks, episodes, actions, steps, checkpoints, rollbacks, coordinates, dollars or tokens; say what it means in the game instead (\"I got lost in the forest\", \"I had to go back and try again\"). \
Only talk about what you're told happened; don't invent events or spoil what's ahead. No hashtags, no lists, no headings; an emoji is fine. \
When you're told you may pass and nothing is worth saying, answer PASS and nothing else. Otherwise answer with the message only.";

/// When she speaks.
#[derive(Debug, Clone, PartialEq)]
pub enum Moment {
    /// A session starts.
    Start,
    /// It's been a while; she may pass if she's allowed to.
    Check { may_pass: bool },
    /// The session is over: a recap, with how it went.
    End { how: String },
}

/// What happened since she last spoke, from the session's reports.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Since {
    pub episodes: Vec<String>,
    /// Areas she reached for the first time.
    pub areas: Vec<String>,
    /// Stuck warnings and going back to a checkpoint.
    pub stuck: Vec<String>,
    /// Hints from people watching.
    pub hints: Vec<String>,
    /// Milestones from her notes she reached, by name.
    pub milestones: Vec<String>,
    /// Blackouts: where she blacked out and where she woke up.
    pub blackouts: Vec<String>,
}

impl Since {
    pub fn add(&mut self, report: &Report) {
        match report.kind {
            Reported::Episode => self.episodes.push(report.line.clone()),
            Reported::Checkpoint => {
                // "📍 **Checkpoint 3** · Pallet Town"
                let area = report.line.rsplit(" · ").next().unwrap_or_default();
                self.areas.push(area.to_string());
            }
            Reported::Stuck => self.stuck.push(report.line.replace("**", "")),
            Reported::Hint => {
                // The first line is the log's heading, then "- hint" a line each.
                let hints = report.line.lines().skip(1);
                self.hints
                    .extend(hints.map(|h| h.trim_start_matches("- ").to_string()));
            }
            Reported::Milestone => {
                // "🏁 **Milestone: Reach Viridian City** · episode 12, …": just the name,
                // since she never talks about episodes or money.
                let name = report.line.split("**").nth(1).unwrap_or_default();
                let name = name.trim_start_matches("Milestone: ");
                self.milestones.push(name.to_string());
            }
            Reported::Blackout => {
                // "💀 **Blacked out** in Viridian Forest and woke up in Pallet Town."
                let line = report.line.replace("**", "");
                self.blackouts
                    .push(line.trim_start_matches("💀 ").to_string());
            }
            Reported::Other => {}
        }
    }
}

/// Whether it's time to speak: `None` if not, else whether she may pass. She speaks only
/// once `every` has passed and she has played since; a new area or being stuck means she
/// has to, and so does having passed [`PASSES_MOST`] times in a row. A hint, a milestone
/// or a blackout makes her speak without waiting for `every`, just [`GAP_LEAST`] since
/// she last spoke.
pub fn due(since: &Since, waited: Duration, every: Duration, passes: u32) -> Option<bool> {
    let soon =
        !since.hints.is_empty() || !since.milestones.is_empty() || !since.blackouts.is_empty();
    if soon && waited >= GAP_LEAST {
        return Some(false);
    }
    if since.episodes.is_empty() || waited < every {
        return None;
    }
    let must = !since.areas.is_empty() || !since.stuck.is_empty() || passes >= PASSES_MOST;
    Some(!must)
}

/// What she's told: the moment, the game as it is, what happened since, her notes about
/// herself and her last messages.
pub fn brief(
    moment: &Moment,
    game: &str,
    now: &Now,
    since: &Since,
    said: &[String],
    herself: &[String],
) -> String {
    let mut out = String::new();
    let minutes = (chrono::Utc::now().timestamp() - now.started).max(0) / 60;
    match moment {
        Moment::Start => out.push_str(&format!(
            "You're starting session {} of {game}. Say hello to everyone watching and what \
             you're up to.\n",
            now.session
        )),
        Moment::Check { may_pass } => {
            out.push_str(&format!(
                "You've been playing {game} for {minutes} minutes this session. Here's what \
                 happened since you last spoke.\n"
            ));
            if *may_pass {
                out.push_str("You may pass (answer PASS) if nothing is worth saying.\n");
            }
        }
        Moment::End { how } => out.push_str(&format!(
            "Your session of {game} just ended after {minutes} minutes: {how}\nWrap up for \
             everyone watching: how it went and how you feel about it.\n"
        )),
    }
    out.push_str(&format!(
        "\nYour goal: {}\nWhere you are: {}\n",
        now.goal, now.area
    ));
    if !now.facts.is_empty() {
        out.push_str(&format!("From the game's memory:\n{}\n", now.facts));
    }
    if !now.notes.is_empty() {
        out.push_str(&format!(
            "What you know so far:\n- {}\n",
            now.notes.join("\n- ")
        ));
    }
    if !since.areas.is_empty() {
        out.push_str(&format!(
            "\nPlaces you reached for the first time: {}\n",
            since.areas.join(", ")
        ));
    }
    if !since.milestones.is_empty() {
        out.push_str(&format!(
            "\nMilestones on the way to your goal that you just reached: {}\n",
            since.milestones.join(", ")
        ));
    }
    if !since.blackouts.is_empty() {
        out.push_str(&format!(
            "\nYou lost and were sent back:\n- {}\n",
            since.blackouts.join("\n- ")
        ));
    }
    if !since.stuck.is_empty() {
        out.push_str(&format!("\nStuck:\n- {}\n", since.stuck.join("\n- ")));
    }
    if !since.hints.is_empty() {
        out.push_str(&format!(
            "\nHints from people watching, which you'll try (they can be wrong; thank them \
             by name if one is given):\n- {}\n",
            since.hints.join("\n- ")
        ));
    }
    if !since.episodes.is_empty() {
        let from = since.episodes.len().saturating_sub(EPISODES_SHOWN);
        out.push_str(&format!(
            "\nWhat you did, oldest first (each line: where, what you set out to do, how it \
             went, and what was new):\n- {}\n",
            since.episodes[from..].join("\n- ")
        ));
    }
    if !said.is_empty() {
        out.push_str(&format!(
            "\nWhat you said last, so you don't repeat yourself:\n- {}\n",
            said.join("\n- ")
        ));
    }
    if !herself.is_empty() {
        out.push_str(&format!(
            "\nYour notes about yourself:\n{}\n",
            herself.join("\n\n")
        ));
    }
    out
}

/// Her message from her answer: `None` when she passed or wrote nothing.
pub fn message(answer: &str) -> Option<String> {
    let text = answer.trim();
    if text.is_empty() || text.trim_matches(['.', '"', '*']) == PASS {
        return None;
    }
    Some(crate::util::shorten_at_word(text, MOST))
}

/// Where she plays: the dashboard's channel and the dashboard (`message`), for the memory
/// feature, and the log thread under it, where she speaks so the dashboard stays in view.
#[derive(Debug, Clone, Copy)]
pub struct Place {
    pub guild: Option<GuildId>,
    pub channel: ChannelId,
    pub message: MessageId,
    pub log: ChannelId,
}

/// Her commentary for one session.
pub struct Narrator {
    bot: BotCtx,
    place: Place,
    game: String,
    every: Duration,
    since: Since,
    said: Vec<String>,
    spoke: Instant,
    passes: u32,
    /// Whether she said hello: a session that never got going gets no wrap-up either.
    greeted: bool,
}

impl Narrator {
    /// `every`: the least time between her messages while she plays.
    pub fn new(bot: BotCtx, place: Place, game: String, every: Duration) -> Narrator {
        Narrator {
            bot,
            place,
            game,
            every,
            since: Since::default(),
            said: Vec::new(),
            spoke: Instant::now(),
            passes: 0,
            greeted: false,
        }
    }

    /// While the session plays: a hello once the first episode starts, then a message
    /// whenever one is [`due`]. Every report is read from `reports` until the session ends
    /// (`ended`) and the reports stop. Returns itself, for [`Narrator::recap`].
    pub async fn follow(
        mut self,
        mut reports: mpsc::UnboundedReceiver<Report>,
        mut now: watch::Receiver<Now>,
        ended: CancellationToken,
    ) -> Narrator {
        let started = tokio::select! {
            started = now.wait_for(|now| now.episode >= 1) => started.is_ok(),
            () = ended.cancelled() => false,
        };
        if started {
            self.greeted = true;
            let current = now.borrow().clone();
            self.speak(Moment::Start, &current).await;
        }
        let mut ticks = tokio::time::interval(CHECK_EVERY);
        loop {
            tokio::select! {
                report = reports.recv() => match report {
                    Some(report) => self.since.add(&report),
                    None => break,
                },
                _ = ticks.tick() => {
                    if ended.is_cancelled() {
                        continue;
                    }
                    if let Some(may_pass) =
                        due(&self.since, self.spoke.elapsed(), self.every, self.passes)
                    {
                        let current = now.borrow().clone();
                        self.speak(Moment::Check { may_pass }, &current).await;
                    }
                }
            }
        }
        self
    }

    /// Her wrap-up once the session is over. `how` is how it went (the summary's line).
    pub async fn recap(mut self, now: &Now, how: &str) {
        if !self.greeted {
            return;
        }
        self.speak(
            Moment::End {
                how: how.to_string(),
            },
            now,
        )
        .await;
    }

    /// Asks her and posts what she says, with a GIF of her last seconds of play. A failed call just
    /// means she says nothing this time.
    async fn speak(&mut self, moment: Moment, now: &Now) {
        let answer = match self.write(&moment, now).await {
            Ok(answer) => answer,
            Err(err) => {
                warn!("her Game Boy commentary failed: {err:#}");
                return;
            }
        };
        self.spoke = Instant::now();
        self.since = Since::default();
        let Some(text) = message(&answer) else {
            self.passes += 1;
            info!(passes = self.passes, "she passed on saying something");
            return;
        };
        self.passes = 0;
        let mut draft = Draft::new().content(&text).allowed_mentions(no_pings());
        if let Some((_, gif)) = last_seconds().await {
            draft = draft.file("play.gif", gif);
        } else if let Some(picture) = last_screen().await {
            draft = draft.file("screen.png", picture);
        }
        if let Err(err) = draft.send(&self.bot.http, self.place.log).await {
            warn!("couldn't post her Game Boy commentary: {err:#}");
        }
        if self.said.len() == SAID_SHOWN {
            self.said.remove(0);
        }
        self.said.push(text);
    }

    async fn write(&self, moment: &Moment, now: &Now) -> anyhow::Result<String> {
        let provider = self
            .bot
            .ai
            .chat()
            .ok_or_else(|| anyhow::anyhow!("no AI set up"))?;
        let mut herself = Vec::new();
        if let Some(guild) = self.place.guild {
            for path in [PERSONALITY, MOOD] {
                let (channel, message) = (self.place.channel, self.place.message);
                if let Some(note) =
                    memory_file(&self.bot, guild, channel, message, path, NOTE_MOST).await
                {
                    herself.push(note);
                }
            }
        }
        let brief = brief(moment, &self.game, now, &self.since, &self.said, &herself);
        say(provider.as_ref(), brief, self.place.channel.get()).await
    }
}

/// Her answer to `brief`. `channel` keeps each channel's prompt cache apart.
pub async fn say(
    provider: &dyn ChatProvider,
    brief: String,
    channel: u64,
) -> anyhow::Result<String> {
    let request = ChatRequest {
        system: SYSTEM.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts: vec![Part::Text(brief)],
        }]),
        tools: Vec::new(),
        cache_key: format!("{JOB}:{channel}"),
        job: JOB,
    };
    Ok(complete(provider, request, &NoTools, 1).await?.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(kind: Reported, line: &str) -> Report {
        Report {
            kind,
            line: line.to_string(),
            picture: None,
        }
    }

    #[test]
    fn she_hears_about_episodes_new_areas_and_being_stuck() {
        let mut since = Since::default();
        since.add(&report(Reported::Episode, "#4 Route 1: go north → done"));
        since.add(&report(
            Reported::Checkpoint,
            "📍 **Checkpoint 3** · Viridian City",
        ));
        since.add(&report(
            Reported::Stuck,
            "⚠️ **Nothing new for 3 episodes.** She's asked",
        ));
        since.add(&report(Reported::Other, "Planning failed: overloaded"));
        assert_eq!(since.episodes, ["#4 Route 1: go north → done"]);
        assert_eq!(since.areas, ["Viridian City"]);
        assert_eq!(since.stuck, ["⚠️ Nothing new for 3 episodes. She's asked"]);
    }

    #[test]
    fn she_speaks_every_so_often_and_must_for_news() {
        let every = Duration::from_secs(600);
        let (early, late) = (Duration::from_secs(300), Duration::from_secs(601));
        let mut since = Since::default();
        assert_eq!(due(&since, late, every, 0), None, "she hasn't played since");
        since.episodes.push("#1 ...".to_string());
        assert_eq!(due(&since, early, every, 0), None, "too soon");
        assert_eq!(due(&since, late, every, 0), Some(true), "she may pass");
        assert_eq!(due(&since, late, every, PASSES_MOST), Some(false));
        since.areas.push("Pewter City".to_string());
        assert_eq!(
            due(&since, early, every, 0),
            None,
            "news waits its turn too"
        );
        assert_eq!(
            due(&since, late, every, 0),
            Some(false),
            "a new area: she must"
        );
    }

    #[test]
    fn a_hint_makes_her_answer_soon() {
        let mut since = Since::default();
        since.add(&report(
            Reported::Hint,
            "💡 **She reads the hints before planning:**\n- Cherry: heal first\n- Sam: go north",
        ));
        assert_eq!(since.hints, ["Cherry: heal first", "Sam: go north"]);
        let every = Duration::from_secs(600);
        // No episode since and long before `every`: she still answers, once a short gap
        // since her last message has passed.
        assert_eq!(due(&since, Duration::from_secs(60), every, 0), None);
        assert_eq!(due(&since, GAP_LEAST, every, 0), Some(false));
        let told = brief(
            &Moment::Check { may_pass: false },
            "Red",
            &Now::default(),
            &since,
            &[],
            &[],
        );
        assert!(told.contains("Hints from people watching"), "{told}");
        assert!(
            told.contains("- Cherry: heal first\n- Sam: go north"),
            "{told}"
        );
    }

    #[test]
    fn a_milestone_or_a_blackout_makes_her_speak_soon() {
        let mut since = Since::default();
        since.add(&report(
            Reported::Milestone,
            "🏁 **Milestone: Reach Viridian City** · episode 12, 4 episodes and $0.010 since \
             the last",
        ));
        since.add(&report(
            Reported::Blackout,
            "💀 **Blacked out** in Viridian Forest and woke up in Pallet Town.",
        ));
        assert_eq!(since.milestones, ["Reach Viridian City"]);
        assert_eq!(
            since.blackouts,
            ["Blacked out in Viridian Forest and woke up in Pallet Town."]
        );
        let every = Duration::from_secs(600);
        assert_eq!(due(&since, Duration::from_secs(60), every, 0), None);
        assert_eq!(due(&since, GAP_LEAST, every, 0), Some(false));
        let told = brief(
            &Moment::Check { may_pass: false },
            "Red",
            &Now::default(),
            &since,
            &[],
            &[],
        );
        assert!(
            told.contains("you just reached: Reach Viridian City\n"),
            "{told}"
        );
        assert!(
            told.contains("- Blacked out in Viridian Forest and woke up in Pallet Town."),
            "{told}"
        );
        assert!(!told.contains("$0.010"), "no bookkeeping: {told}");

        let mut blackout = Since::default();
        blackout
            .blackouts
            .push("Blacked out in Route 1.".to_string());
        assert_eq!(due(&blackout, GAP_LEAST, every, 0), Some(false));
    }

    #[test]
    fn a_pass_posts_nothing() {
        assert_eq!(message("PASS"), None);
        assert_eq!(message(" PASS. "), None);
        assert_eq!(message("   "), None);
        assert_eq!(
            message("Viridian at last! 🌳").as_deref(),
            Some("Viridian at last! 🌳")
        );
        let long = "word ".repeat(400);
        assert!(message(&long).unwrap().chars().count() <= MOST);
    }

    #[test]
    fn her_brief_has_what_happened_and_what_she_said() {
        let now = Now {
            session: 4,
            area: "Viridian Forest".to_string(),
            goal: "Beat the game.".to_string(),
            facts: "Badges: none".to_string(),
            notes: vec!["Pewter is north of the forest.".to_string()],
            ..Now::default()
        };
        let since = Since {
            episodes: vec!["#23 Viridian Forest: beat the Bug Catcher → not done".to_string()],
            areas: vec!["Viridian Forest".to_string()],
            stuck: Vec::new(),
            ..Since::default()
        };
        let said = ["Into the forest we go!".to_string()];
        let herself = ["I love bugs, secretly.".to_string()];
        let check = brief(
            &Moment::Check { may_pass: false },
            "Pokémon Red",
            &now,
            &since,
            &said,
            &herself,
        );
        for part in [
            "playing Pokémon Red",
            "Your goal: Beat the game.",
            "Where you are: Viridian Forest",
            "Badges: none",
            "- Pewter is north of the forest.",
            "for the first time: Viridian Forest",
            "- #23 Viridian Forest: beat the Bug Catcher → not done",
            "- Into the forest we go!",
            "I love bugs, secretly.",
        ] {
            assert!(check.contains(part), "{part:?} in:\n{check}");
        }
        assert!(!check.contains("PASS"), "she must speak:\n{check}");
        let may = brief(
            &Moment::Check { may_pass: true },
            "Red",
            &now,
            &since,
            &[],
            &[],
        );
        assert!(may.contains("answer PASS"));
        let start = brief(&Moment::Start, "Red", &now, &Since::default(), &[], &[]);
        assert!(start.starts_with("You're starting session 4 of Red."));
        let end = brief(
            &Moment::End {
                how: "Her time was up. 40 episodes".to_string(),
            },
            "Red",
            &now,
            &since,
            &[],
            &[],
        );
        assert!(end.contains("just ended after") && end.contains("Her time was up."));
    }
}
