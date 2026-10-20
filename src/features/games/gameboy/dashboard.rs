//! The dashboard: one message that shows her playing by herself. It has the last seconds
//! of play as a GIF, her goal and task, her last actions and episodes, her notes and
//! skills, and what playing used today, with Stop, Skills and Open log buttons. While she
//! plays it's edited every `live_seconds` if anything changed. Everything in order goes in
//! the log thread instead (`playing.rs`). `/games gameboy dashboard` moves it to a new
//! message, and the old one then says where it went.
//!
//! It's a Components V2 message: a panel with a coloured edge, the GIF inside it, and
//! dividers between the parts, in Discord's normal font size (unlike an embed). Such a
//! message has no `content`; everything is in its components, 4000 characters in all.

use std::time::Duration;

use chrono::Utc;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::warn;
use twilight_model::channel::message::MessageFlags;
use twilight_model::channel::message::component::{
    Button, ButtonStyle, Component, Container, MediaGallery, MediaGalleryItem, Section, Separator,
    SeparatorSpacingSize, TextDisplay, Thumbnail, UnfurledMediaItem,
};
use twilight_util::builder::message::ButtonBuilder;

use super::commands::GIF_SCALE;
use super::console::{RECENT_SECONDS, RECORD_EVERY, console};
use super::emulator::FPS;
use super::pictures;
use super::session::{EPISODE_ACTIONS, Now};
use crate::core::BotCtx;
use crate::util::discord::{self, ChannelId, Draft, GuildId, Message, no_pings};

/// The buttons' IDs.
pub const STOP: &str = "games:gameboy:stop";
pub const SKILLS: &str = "games:gameboy:skills";
/// The dashboard is edited at most this often, in seconds.
const FASTEST: u64 = 5;
/// Under Discord's 4000 characters for a whole Components V2 message.
const MOST_CHARS: usize = 3800;
/// The pictures' file names on the message: the GIF while she plays, the still after.
const GIF_NAME: &str = "live.gif";
const STILL_NAME: &str = "last.png";
/// How long a live GIF stays on its last screen, in seconds: longer than the dashboard
/// takes to bring the next one, so it never starts over.
const HOLD_SECONDS: u32 = 60;
/// Seconds allowed for a GIF to upload and load before it plays: the next one waits until
/// the last has played this long after it.
const LOAD_SECONDS: f64 = 2.0;
/// The panel's edge: teal like her hair while she plays, grey once it's over.
const LIVE_COLOUR: u32 = 0x2BB3A3;
const OVER_COLOUR: u32 = 0x80848E;

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

/// The dashboard's text, in the parts the panel shows with dividers between them.
#[derive(Debug, PartialEq)]
pub struct Parts {
    /// The title, whether she's live, and how it went once it's over.
    pub top: String,
    /// Her goal, her task and what code reads from the game.
    pub about: String,
    /// Her last actions, recent episodes, notes and skills, those that fit.
    pub middle: Vec<String>,
    /// What playing used today.
    pub foot: String,
}

impl Parts {
    /// All of it as one text, as long as the panel's text is.
    pub fn text(&self) -> String {
        let mut parts = vec![self.top.clone(), self.about.clone()];
        parts.extend(self.middle.iter().cloned());
        parts.push(self.foot.clone());
        parts.join("\n\n")
    }
}

/// The dashboard's parts at `at` (seconds since 1970). When it's too long for one message,
/// parts are left out: her notes first, then skills, episodes and actions.
pub fn parts(board: &Board<'_>, at: i64) -> Parts {
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
            top.push(short(how, 1000));
        }
    }
    let mut about = vec![format!("**Goal:** {}", short(&now.goal, 300))];
    if !now.milestone.is_empty() {
        about.push(format!("-# 🏁 Milestones {}", short(&now.milestone, 200)));
    }
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
    // Once it's over the panel is short: how it went, her goal and today's use. The rest
    // is in the log and `/games gameboy status`.
    let live = board.over.is_none();
    if live {
        about.extend(now.facts.lines().filter_map(fact));
    }

    let actions = (live && !now.last_actions.is_empty()).then(|| {
        // Nothing she did can close the code block.
        let lines: Vec<String> = now
            .last_actions
            .iter()
            .map(|a| a.replace('`', "'"))
            .collect();
        format!("**Last actions**\n```\n{}\n```", lines.join("\n"))
    });
    let episodes = (live && !now.episodes.is_empty()).then(|| {
        let lines: Vec<String> = now.episodes.iter().map(|e| episode_line(e)).collect();
        format!("**Recent episodes**\n{}", lines.join("\n"))
    });
    let notes = (live && !now.notes.is_empty()).then(|| list("Her notes", &now.notes));
    let skills = (live && now.skills > 0).then(|| {
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
    let fitted = |middle: &[Option<String>]| Parts {
        top: top.join("\n"),
        about: about.join("\n"),
        middle: middle.iter().flatten().cloned().collect(),
        foot: foot.clone(),
    };
    let mut parts = fitted(&middle);
    for leave_out in [2, 3, 1, 0] {
        if parts.text().chars().count() <= MOST_CHARS {
            break;
        }
        middle[leave_out] = None;
        parts = fitted(&middle);
    }
    parts
}

/// A fact from the game's memory, like `Money: ¥3175`, as a line of its own with the
/// name in bold.
fn fact(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    Some(match line.split_once(": ") {
        Some((name, value)) => format!("**{name}:** {}", short(value, 200)),
        None => short(line, 200),
    })
}

/// An episode's journal line, like `#7 10-20 21:10 Oak's Lab: task → done, 8 actions, …`,
/// with how it went in bold. Only the task is shortened, so how it went always shows.
fn episode_line(line: &str) -> String {
    let Some((task, after)) = line.rsplit_once(" → ") else {
        return format!("- {}", short(line, 160));
    };
    // How it went is up to the first comma outside brackets: `not done (why, …)`.
    let mut depth = 0;
    let mut end = after.len();
    for (i, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    let (how, rest) = after.split_at(end);
    format!("- {} → **{how}**{rest}", short(task, 110))
}

/// A bold title over a line per item.
fn list(title: &str, items: &[String]) -> String {
    let lines: Vec<String> = items
        .iter()
        .map(|i| format!("- {}", short(i, 160)))
        .collect();
    format!("**{title}**\n{}", lines.join("\n"))
}

/// The dashboard as a panel: the top, the GIF (when there is one), her goal and task, then
/// the middle and the foot between dividers, and the buttons. `log` is a link to the log.
///
/// Once it's `over`, the picture is a small still of her last screen beside the text.
pub fn panel(parts: &Parts, picture: bool, log: Option<String>, over: bool) -> Vec<Component> {
    let mut inside = Vec::new();
    match (over, picture) {
        (true, true) => inside.push(Component::Section(Section {
            id: None,
            components: vec![text(&parts.top), text(&parts.about)],
            accessory: Box::new(Component::Thumbnail(Thumbnail {
                id: None,
                media: attached(STILL_NAME),
                description: Some(Some("Her last screen".to_string())),
                spoiler: None,
            })),
        })),
        (true, false) => inside.extend([text(&parts.top), text(&parts.about)]),
        (false, _) => {
            inside.push(text(&parts.top));
            if picture {
                inside.push(Component::MediaGallery(MediaGallery {
                    id: None,
                    items: vec![MediaGalleryItem {
                        media: attached(GIF_NAME),
                        description: Some("Her play since the last update".to_string()),
                        spoiler: None,
                    }],
                }));
            }
            inside.push(text(&parts.about));
        }
    }
    if !parts.middle.is_empty() {
        inside.push(divider());
        inside.push(text(&parts.middle.join("\n\n")));
    }
    inside.push(divider());
    inside.push(text(&parts.foot));
    if let Some(row) = buttons(log, over) {
        inside.push(row);
    }
    let colour = if over { OVER_COLOUR } else { LIVE_COLOUR };
    vec![Component::Container(Container {
        id: None,
        accent_color: Some(Some(colour)),
        spoiler: None,
        components: inside,
    })]
}

/// A file on the message, for a gallery or thumbnail.
fn attached(name: &str) -> UnfurledMediaItem {
    UnfurledMediaItem {
        url: format!("attachment://{name}"),
        proxy_url: None,
        height: None,
        width: None,
        content_type: None,
    }
}

/// A message that's a panel, with its picture if there is one. Used to edit, it replaces
/// the message's files (or removes them, without a picture).
fn draft(components: Vec<Component>, picture: Option<(&str, Vec<u8>)>) -> Draft {
    let draft = Draft::new()
        .components(components)
        .flags(MessageFlags::IS_COMPONENTS_V2)
        .allowed_mentions(no_pings());
    match picture {
        Some((name, bytes)) => draft.file(name, bytes),
        None => draft.remove_all_files(),
    }
}

/// The dashboard as a message to send or edit with. `picture` is a GIF ([`clip`]) while
/// she plays, and a PNG ([`last_screen`]) once it's `over`.
pub fn message(parts: &Parts, picture: Option<Vec<u8>>, log: Option<String>, over: bool) -> Draft {
    let name = if over { STILL_NAME } else { GIF_NAME };
    let components = panel(parts, picture.is_some(), log, over);
    draft(components, picture.map(|bytes| (name, bytes)))
}

/// A short Components V2 message, like what an old dashboard says once it has moved.
pub fn note(line: &str) -> Draft {
    draft(vec![text(line)], None)
}

/// What an old dashboard says once it has moved to `link`.
pub fn moved(link: &str) -> String {
    format!("➡️ The dashboard moved: {link}")
}

fn text(content: &str) -> Component {
    Component::TextDisplay(TextDisplay {
        id: None,
        content: content.to_string(),
    })
}

fn divider() -> Component {
    Component::Separator(Separator {
        id: None,
        divider: Some(true),
        spacing: Some(SeparatorSpacingSize::Small),
    })
}

/// Stop and Skills while she plays, and a link to the log, as a row (if there's any).
fn buttons(log: Option<String>, over: bool) -> Option<Component> {
    let mut row: Vec<Button> = Vec::new();
    if !over {
        row.push(discord::button(STOP, "Stop", ButtonStyle::Danger).build());
        row.push(discord::button(SKILLS, "Skills", ButtonStyle::Secondary).build());
    }
    if let Some(log) = log {
        row.push(
            ButtonBuilder::new(ButtonStyle::Link)
                .label("Open log")
                .url(log)
                .build(),
        );
    }
    (!row.is_empty()).then(|| discord::buttons(row))
}

/// A link to a channel.
pub fn channel_link(guild: GuildId, channel: ChannelId) -> String {
    format!("https://discord.com/channels/{guild}/{channel}")
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
    /// Edits `message` into the dashboard, with `picture` in it (see [`message`]).
    /// `message` becomes the edited message.
    pub async fn show(
        &self,
        message: &mut Message,
        now: &Now,
        over: Option<&str>,
        picture: Option<Vec<u8>>,
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
        let parts = parts(&board, Utc::now().timestamp());
        let log = Some(channel_link(self.guild, self.log));
        let edit = self::message(&parts, picture, log, over.is_some());
        match edit
            .edit(&self.bot.http, message.channel_id, message.id)
            .await
        {
            Ok(edited) => *message = edited,
            Err(err) => warn!("couldn't update the Game Boy dashboard: {err:#}"),
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
        // Seconds the last GIF plays for, so the next doesn't cut it off.
        let mut playing = 0.0;
        loop {
            let wait = (every.max(FASTEST) as f64).max(playing + LOAD_SECONDS);
            let tick = async {
                if every == 0 {
                    std::future::pending::<()>().await;
                } else {
                    tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                }
            };
            tokio::select! {
                () = tick => {}
                Some(new) = moves.recv() => {
                    let link = discord::message_link(Some(self.guild), new.channel_id, new.id);
                    let edit = note(&moved(&link));
                    if let Err(err) = edit.edit(&self.bot.http, message.channel_id, message.id).await {
                        warn!("couldn't point the old Game Boy dashboard to the new one: {err:#}");
                    }
                    message = new;
                    (shown_mark, shown_now) = (None, None);
                }
                () = ended.cancelled() => return message,
            }
            let current = now.borrow().clone();
            let moved = recorded()
                .await
                .is_some_and(|mark| Some(mark) != shown_mark);
            // Nothing moved and nothing changed (she's thinking): leave it.
            if !moved && shown_now.as_ref() == Some(&current) {
                continue;
            }
            // Each edit sends a GIF again, since a message's files can't be kept otherwise:
            // what she played since the last one, or her screen as it is if nothing moved.
            let clip = clip(shown_mark).await;
            playing = clip.as_ref().map_or(0.0, |c| c.seconds);
            let mark = clip.as_ref().map(|c| c.mark);
            self.show(&mut message, &current, None, clip.map(|c| c.gif))
                .await;
            shown_mark = mark.or(shown_mark);
            shown_now = Some(current);
        }
    }
}

/// How many screens the Game Boy has recorded, if it's on.
async fn recorded() -> Option<u64> {
    console()
        .with(|machine| machine.as_ref().map(|m| m.recorded()))
        .await
        .ok()
        .flatten()
}

/// A GIF for the live dashboard.
pub struct Clip {
    /// How many screens had been recorded, to make the next one from.
    pub mark: u64,
    pub gif: Vec<u8>,
    /// How long it plays before it stays on its last screen.
    pub seconds: f64,
}

/// What she played since `mark` (from [`recorded`]; `None`: the last seconds), at the
/// game's speed, so each one carries on from the last. Then it stays on her screen as it
/// is now for a minute, so it doesn't start over before the next one comes. When nothing
/// moved, it's that screen alone. At most the last [`RECENT_SECONDS`].
pub async fn clip(mark: Option<u64>) -> Option<Clip> {
    let (recorded, screens) = console()
        .with(move |machine| {
            let m = machine.as_ref()?;
            let since = mark.unwrap_or_else(|| m.recorded().saturating_sub(recent_screens()));
            let mut screens = m.screens_since(since);
            if screens.is_empty() {
                screens = m.screens_since(m.recorded().saturating_sub(1));
            }
            (!screens.is_empty()).then(|| (m.recorded(), screens))
        })
        .await
        .ok()
        .flatten()?;
    let seconds = (screens.len().saturating_sub(1) as u64 * u64::from(RECORD_EVERY)) as f64 / FPS;
    let gif = tokio::task::spawn_blocking(move || {
        pictures::gif_held(&screens, RECORD_EVERY, GIF_SCALE, HOLD_SECONDS * 60)
    })
    .await
    .ok()?
    .ok()?;
    Some(Clip {
        mark: recorded,
        gif,
        seconds,
    })
}

/// Her screen as it is, as a PNG, if the Game Boy is on: the ended dashboard's still.
pub async fn last_screen() -> Option<Vec<u8>> {
    let screen = console()
        .with(|machine| {
            let m = machine.as_ref()?;
            m.screens_since(m.recorded().saturating_sub(1)).pop()
        })
        .await
        .ok()
        .flatten()?;
    tokio::task::spawn_blocking(move || pictures::png(&screen))
        .await
        .ok()?
        .ok()
}

/// Screens recorded in [`RECENT_SECONDS`].
fn recent_screens() -> u64 {
    u64::from(RECENT_SECONDS) * 60 / u64::from(RECORD_EVERY)
}

/// The last seconds of play as a GIF, if the Game Boy is on, with how many screens it had
/// recorded (to tell whether it moved since). The screens are copied on the Game Boy's
/// thread and made into a GIF off it, so play isn't held up.
pub async fn last_seconds() -> Option<(u64, Vec<u8>)> {
    let (mark, screens) = console()
        .with(|machine| {
            let m = machine.as_ref()?;
            let shown = m.screens_since(m.recorded().saturating_sub(recent_screens()));
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
            milestone: "1 of 3 · next: Reach Viridian City".to_string(),
            episodes: vec![
                "#13 10-20 14:36 Route 1: keep north → done, 12 actions, a new area".to_string(),
            ],
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
            parts(&board(&now, None), 1_000 + 24 * 60 + 30).text(),
            "## Vivy plays Pokémon Red\n\
             -# 🔴 Live · Session 3 · 24 of 60 min · Episode 14 · Route 1\n\n\
             **Goal:** Bring Oak his parcel.\n\
             -# 🏁 Milestones 1 of 3 · next: Reach Viridian City\n\
             **Now:** Win the wild battle, then keep north to Viridian City.\n\
             -# Action 18 of 30\n\
             **Location:** Route 1 at x 5, y 20\n\
             **Party:** CHARMANDER Lv6 17/21 HP\n\n\
             **Last actions**\n```\npress \"a\": The text moved on.\n\
             use_skill \"win a 'wild' battle\": Done.\n```\n\n\
             **Recent episodes**\n\
             - #13 10-20 14:36 Route 1: keep north → **done**, 12 actions, a new area\n\n\
             **Her notes**\n- The Mart is in Viridian City.\n\n\
             **Skills:** 7 · newest: `win a wild battle`\n\n\
             -# Today $0.38 of $1.50 · 212k of 1000k free tokens · 5 checkpoints · log: <#42>"
        );
    }

    #[test]
    fn once_over_it_says_how_it_went_without_the_task() {
        let mut now = now();
        now.stuck = 4;
        let text = parts(&board(&now, Some("**Session over.** Her time was up.")), 0).text();
        assert!(
            text.starts_with(
                "## Vivy plays Pokémon Red\n-# ⏹️ Ended · Session 3 · Route 1\n\
                 **Session over.** Her time was up.\n\n**Goal:**"
            ),
            "{text}"
        );
        // Short: the rest is in the log and in `status`.
        for gone in [
            "**Now:**",
            "Last actions",
            "Recent episodes",
            "Her notes",
            "Location",
        ] {
            assert!(!text.contains(gone), "{gone}: {text}");
        }
        assert!(text.ends_with("log: <#42>"));
    }

    #[test]
    fn a_long_dashboard_leaves_out_her_notes_first() {
        let mut now = now();
        now.notes = vec!["n".repeat(150); 30];
        now.stuck = 3;
        let text = parts(&board(&now, None), 0).text();
        assert!(text.chars().count() <= MOST_CHARS);
        assert!(!text.contains("Her notes"), "{text}");
        assert!(text.contains("Last actions") && text.contains("Skills"));
        assert!(text.contains("· nothing new for 3 episodes"));
        assert!(text.ends_with("log: <#42>"), "the footer stays");
    }

    #[test]
    fn how_an_episode_went_is_bold_and_never_cut() {
        let long = "x".repeat(300);
        let line = episode_line(&format!(
            "#7 10-20 21:10 Oak's Lab: {long} → not done (Blue won, again), 30 actions, nothing new"
        ));
        assert!(
            line.ends_with("… → **not done (Blue won, again)**, 30 actions, nothing new"),
            "{line}"
        );
        assert_eq!(episode_line("no arrow"), "- no arrow");
        assert_eq!(fact("Money: ¥3175").as_deref(), Some("**Money:** ¥3175"));
        assert_eq!(fact("  "), None);
    }

    #[test]
    fn the_panel_has_the_gif_and_the_buttons() {
        let now = now();
        let parts = parts(&board(&now, None), 0);
        let log = Some("https://discord.com/channels/1/2".to_string());
        let live = message(&parts, Some(vec![1, 2, 3]), log.clone(), false);
        assert_eq!(live.flags, Some(MessageFlags::IS_COMPONENTS_V2));
        assert!(
            live.content.is_none(),
            "a Components V2 message has no content"
        );
        assert_eq!(live.files.len(), 1);
        let json = serde_json::to_string(&live.components).unwrap();
        assert!(json.starts_with(r#"[{"type":17,"#), "one container: {json}");
        assert!(json.contains("attachment://live.gif"));
        assert!(json.contains(STOP) && json.contains(SKILLS) && json.contains("Open log"));

        let over = message(&parts, Some(vec![4, 5]), log.clone(), true);
        assert_eq!(over.files.len(), 1);
        let json = serde_json::to_string(&over.components).unwrap();
        assert!(!json.contains(STOP) && json.contains("Open log"));
        assert!(json.contains(r#""type":11"#), "a thumbnail: {json}");
        assert!(json.contains("attachment://last.png") && !json.contains("live.gif"));

        let bare = message(&parts, None, log, true);
        assert!(bare.files.is_empty() && bare.keep_files == Some(Vec::new()));
        let json = serde_json::to_string(&bare.components).unwrap();
        assert!(!json.contains("attachment://"));
    }
}
