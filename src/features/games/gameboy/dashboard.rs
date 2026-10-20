//! The dashboard: one message that shows her playing by herself. It has the last seconds
//! of play as a GIF, her goal and task, her last actions and episodes, her notes and
//! skills, and what playing used today, with Stop, Skills and Open log buttons. While she
//! plays it's edited every `live_seconds` if anything changed. Everything in order goes in
//! the log thread instead (`playing.rs`). `/games gameboy dashboard` moves it to a new
//! message, and the old one then says where it went.
//!
//! It's a plain message, which keeps Discord's normal font size, unlike an embed. Discord's
//! newer layout (Components V2) would look closer to a panel, but serenity 0.12 can't read
//! those messages back, so every edit would log an error.

use std::time::Duration;

use chrono::Utc;
use serenity::all::{
    ButtonStyle, ChannelId, CreateActionRow, CreateAllowedMentions, CreateAttachment, CreateButton,
    EditAttachments, EditMessage, GuildId, Message,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::commands::GIF_SCALE;
use super::console::{RECENT_SECONDS, RECORD_EVERY, console};
use super::pictures;
use super::session::{EPISODE_ACTIONS, Now};
use crate::core::BotCtx;

/// The buttons' IDs.
pub const STOP: &str = "games:gameboy:stop";
pub const SKILLS: &str = "games:gameboy:skills";
/// The dashboard is edited at most this often, in seconds.
const FASTEST: u64 = 5;
/// Under Discord's 2000 characters.
const MOST_CHARS: usize = 1990;

/// What the dashboard shows besides [`Now`].
pub struct Board<'a> {
    pub game: &'a str,
    pub now: &'a Now,
    /// The session's minutes at most.
    pub minutes: u32,
    pub daily_usd: f64,
    pub free_tokens: u64,
    /// The log thread, once there is one.
    pub log: Option<ChannelId>,
    /// How it went, once it's over. `None` while she plays.
    pub over: Option<&'a str>,
}

/// The dashboard's text at `at` (seconds since 1970). When it's too long for one message,
/// sections are left out: her notes first, then skills, episodes and actions.
pub fn text(board: &Board<'_>, at: i64) -> String {
    let now = board.now;
    let place = if now.area.is_empty() {
        String::new()
    } else {
        format!(" · {}", now.area)
    };
    let mut top = vec![format!("## Vivy plays {}", board.game)];
    match board.over {
        None => {
            let minutes = (at - now.started).max(0) / 60;
            let episode = match now.episode {
                0 => "planning the first episode".to_string(),
                n => format!("Episode {n}"),
            };
            top.push(format!(
                "-# 🔴 Live · Session {} · {minutes} of {} min · {episode}{place}",
                now.session, board.minutes
            ));
        }
        Some(how) => {
            top.push(format!("-# ⏹️ Ended · Session {}{place}", now.session));
            top.push(how.to_string());
        }
    }
    let mut about = vec![format!("**Goal:** {}", short(&now.goal, 300))];
    if board.over.is_none() && !now.task.is_empty() {
        let stuck = match now.stuck {
            0 => String::new(),
            n => format!(" · nothing new for {n} episodes"),
        };
        about.push(format!(
            "**Now:** {}\n-# Action {} of {EPISODE_ACTIONS}{stuck}",
            short(&now.task, 300),
            now.actions
        ));
    }
    let facts: Vec<&str> = now.facts.lines().filter(|l| !l.trim().is_empty()).collect();
    if !facts.is_empty() {
        about.push(format!("-# {}", short(&facts.join(" · "), 300)));
    }

    let actions = (board.over.is_none() && !now.last_actions.is_empty()).then(|| {
        // Nothing she did can close the code block.
        let lines: Vec<String> = now
            .last_actions
            .iter()
            .map(|a| a.replace('`', "'"))
            .collect();
        format!("**Last actions**\n```\n{}\n```", lines.join("\n"))
    });
    let episodes = (!now.episodes.is_empty()).then(|| list("Recent episodes", &now.episodes));
    let notes = (!now.notes.is_empty()).then(|| list("Her notes", &now.notes));
    let skills = (now.skills > 0).then(|| {
        let newest = now
            .newest_skill
            .as_deref()
            .map(|name| format!(" · newest: `{}`", name.replace('`', "'")))
            .unwrap_or_default();
        format!("**Skills:** {}{newest}", now.skills)
    });
    let log = board
        .log
        .map(|id| format!(" · log: <#{id}>"))
        .unwrap_or_default();
    let foot = format!(
        "-# Today ${:.2} of ${:.2} · {}k of {}k free tokens · {} checkpoints{log}",
        now.usd,
        board.daily_usd,
        now.free_tokens / 1000,
        board.free_tokens / 1000,
        now.checkpoints
    );

    let mut middle = [actions, episodes, notes, skills];
    let joined = |middle: &[Option<String>]| {
        let mut parts = vec![top.join("\n"), about.join("\n")];
        parts.extend(middle.iter().flatten().cloned());
        parts.push(foot.clone());
        parts.join("\n\n")
    };
    let mut text = joined(&middle);
    for leave_out in [2, 3, 1, 0] {
        if text.chars().count() <= MOST_CHARS {
            break;
        }
        middle[leave_out] = None;
        text = joined(&middle);
    }
    short(&text, MOST_CHARS)
}

/// A bold title over a line per item.
fn list(title: &str, items: &[String]) -> String {
    let lines: Vec<String> = items
        .iter()
        .map(|i| format!("- {}", short(i, 160)))
        .collect();
    format!("**{title}**\n{}", lines.join("\n"))
}

/// What an old dashboard says once it has moved to `link`.
pub fn moved(link: &str) -> String {
    format!("➡️ The dashboard moved: {link}")
}

/// Stop and Skills while she plays, and a link to the log.
pub fn buttons(log: Option<String>, over: bool) -> Vec<CreateActionRow> {
    let mut row = Vec::new();
    if !over {
        row.push(
            CreateButton::new(STOP)
                .label("Stop")
                .style(ButtonStyle::Danger),
        );
        row.push(
            CreateButton::new(SKILLS)
                .label("Skills")
                .style(ButtonStyle::Secondary),
        );
    }
    if let Some(log) = log {
        row.push(CreateButton::new_link(log).label("Open log"));
    }
    if row.is_empty() {
        Vec::new()
    } else {
        vec![CreateActionRow::Buttons(row)]
    }
}

/// A link to a channel, or to a message in it.
pub fn link(guild: GuildId, channel: ChannelId, message: Option<&Message>) -> String {
    match message {
        Some(m) => format!("https://discord.com/channels/{guild}/{channel}/{}", m.id),
        None => format!("https://discord.com/channels/{guild}/{channel}"),
    }
}

/// One session's dashboard: what it says and where its log is.
pub struct Live {
    pub bot: BotCtx,
    pub game: String,
    pub minutes: u32,
    pub daily_usd: f64,
    pub free_tokens: u64,
    pub guild: GuildId,
    pub log: ChannelId,
}

impl Live {
    /// Edits `message` into the dashboard, with a new GIF if there is one, else keeping
    /// the one it has.
    pub async fn show(
        &self,
        message: &mut Message,
        now: &Now,
        over: Option<&str>,
        gif: Option<Vec<u8>>,
    ) {
        let board = Board {
            game: &self.game,
            now,
            minutes: self.minutes,
            daily_usd: self.daily_usd,
            free_tokens: self.free_tokens,
            log: Some(self.log),
            over,
        };
        let attachments = match gif {
            Some(gif) => EditAttachments::new().add(CreateAttachment::bytes(gif, "live.gif")),
            None => EditAttachments::keep_all(message),
        };
        let edit = EditMessage::new()
            .content(text(&board, Utc::now().timestamp()))
            .components(buttons(
                Some(link(self.guild, self.log, None)),
                over.is_some(),
            ))
            .allowed_mentions(CreateAllowedMentions::new())
            .attachments(attachments);
        if let Err(err) = message.edit(&self.bot.http, edit).await {
            warn!("couldn't update the Game Boy dashboard: {err}");
        }
    }

    /// Keeps `message` up to date while she plays, every `every` seconds (0: never, until
    /// it moves), moving to each message `moves` brings. Gives back the dashboard's message
    /// once `ended`.
    pub async fn follow(
        &self,
        mut message: Message,
        now: watch::Receiver<Now>,
        mut moves: mpsc::UnboundedReceiver<Message>,
        ended: CancellationToken,
        every: u64,
    ) -> Message {
        let mut shown_mark = None;
        let mut shown_now = None;
        loop {
            let tick = async {
                if every == 0 {
                    std::future::pending::<()>().await;
                } else {
                    tokio::time::sleep(Duration::from_secs(every.max(FASTEST))).await;
                }
            };
            tokio::select! {
                () = tick => {}
                Some(new) = moves.recv() => {
                    let edit = EditMessage::new()
                        .content(moved(&link(self.guild, new.channel_id, Some(&new))))
                        .components(Vec::new())
                        .attachments(EditAttachments::new());
                    if let Err(err) = message.edit(&self.bot.http, edit).await {
                        warn!("couldn't point the old Game Boy dashboard to the new one: {err}");
                    }
                    message = new;
                    (shown_mark, shown_now) = (None, None);
                }
                () = ended.cancelled() => return message,
            }
            let current = now.borrow().clone();
            let gif = last_seconds().await;
            let mark = gif.as_ref().map(|(mark, _)| *mark);
            let new_gif = mark.is_some() && mark != shown_mark;
            // Nothing moved and nothing changed (she's thinking): leave it.
            if !new_gif && shown_now.as_ref() == Some(&current) {
                continue;
            }
            let gif = gif.filter(|_| new_gif).map(|(_, gif)| gif);
            self.show(&mut message, &current, None, gif).await;
            shown_mark = mark.or(shown_mark);
            shown_now = Some(current);
        }
    }
}

/// The last seconds of play as a GIF, if the Game Boy is on, with how many screens it had
/// recorded (to tell whether it moved since). The screens are copied on the Game Boy's
/// thread and made into a GIF off it, so play isn't held up.
pub async fn last_seconds() -> Option<(u64, Vec<u8>)> {
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

fn short(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    text.chars().take(most).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Now {
        Now {
            started: 1_000,
            session: 3,
            episode: 14,
            task: "Win the wild battle, then keep north to Viridian City.".to_string(),
            area: "Route 1".to_string(),
            actions: 18,
            last_actions: [
                "press \"a\": The text moved on.",
                "use_skill \"win a `wild` battle\": Done.",
            ]
            .map(String::from)
            .into(),
            facts: "Location: Route 1 at x 5, y 20\nParty: CHARMANDER Lv6 17/21 HP".to_string(),
            goal: "Bring Oak his parcel.".to_string(),
            notes: vec!["The Mart is in Viridian City.".to_string()],
            episodes: vec!["#13 10-20 14:36 Route 1: keep north → done".to_string()],
            stuck: 0,
            checkpoints: 5,
            skills: 7,
            newest_skill: Some("win a wild battle".to_string()),
            usd: 0.384,
            free_tokens: 212_400,
        }
    }

    fn board<'a>(now: &'a Now, over: Option<&'a str>) -> Board<'a> {
        Board {
            game: "Pokémon Red",
            now,
            minutes: 60,
            daily_usd: 1.5,
            free_tokens: 1_000_000,
            log: Some(ChannelId::new(42)),
            over,
        }
    }

    #[test]
    fn the_live_dashboard_shows_what_she_is_doing() {
        let now = now();
        assert_eq!(
            text(&board(&now, None), 1_000 + 24 * 60 + 30),
            "## Vivy plays Pokémon Red\n\
             -# 🔴 Live · Session 3 · 24 of 60 min · Episode 14 · Route 1\n\n\
             **Goal:** Bring Oak his parcel.\n\
             **Now:** Win the wild battle, then keep north to Viridian City.\n\
             -# Action 18 of 30\n\
             -# Location: Route 1 at x 5, y 20 · Party: CHARMANDER Lv6 17/21 HP\n\n\
             **Last actions**\n```\npress \"a\": The text moved on.\n\
             use_skill \"win a 'wild' battle\": Done.\n```\n\n\
             **Recent episodes**\n- #13 10-20 14:36 Route 1: keep north → done\n\n\
             **Her notes**\n- The Mart is in Viridian City.\n\n\
             **Skills:** 7 · newest: `win a wild battle`\n\n\
             -# Today $0.38 of $1.50 · 212k of 1000k free tokens · 5 checkpoints · log: <#42>"
        );
    }

    #[test]
    fn once_over_it_says_how_it_went_without_the_task() {
        let mut now = now();
        now.stuck = 4;
        let text = text(&board(&now, Some("**Session over.** Her time was up.")), 0);
        assert!(
            text.starts_with(
                "## Vivy plays Pokémon Red\n-# ⏹️ Ended · Session 3 · Route 1\n\
                 **Session over.** Her time was up.\n\n**Goal:**"
            ),
            "{text}"
        );
        assert!(!text.contains("**Now:**") && !text.contains("Last actions"));
        assert!(text.contains("Recent episodes"));
    }

    #[test]
    fn a_long_dashboard_leaves_out_her_notes_first() {
        let mut now = now();
        now.notes = vec!["n".repeat(150); 12];
        now.stuck = 3;
        let text = text(&board(&now, None), 0);
        assert!(text.chars().count() <= MOST_CHARS);
        assert!(!text.contains("Her notes"), "{text}");
        assert!(text.contains("Last actions") && text.contains("Skills"));
        assert!(text.contains("· nothing new for 3 episodes"));
        assert!(text.ends_with("log: <#42>"), "the footer stays");
    }

    #[test]
    fn buttons_stop_and_open_the_log() {
        let live = buttons(Some("https://discord.com/channels/1/2".to_string()), false);
        let json = serde_json::to_string(&live).unwrap();
        assert!(json.contains(STOP) && json.contains(SKILLS) && json.contains("Open log"));
        let over = serde_json::to_string(&buttons(None, true)).unwrap();
        assert_eq!(over, "[]");
    }
}
