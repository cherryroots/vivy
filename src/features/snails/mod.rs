//! Snails: spotting reposts. A "snail" is someone posting a link or picture the server has
//! already seen.
//!
//! Every new message's links and pictures are saved as it arrives, and a new message that
//! repeats an earlier post is counted (quietly, for the control panel and `/snails top`). "Check Snail" (right-click a
//! message → Apps) lists the earlier posts of the same thing. `/snails backfill` reads the
//! server's older history, but only when an admin starts it; after that, each start catches
//! up on what those channels got while the bot was offline.
//!
//! Edits are read again (links and pictures added count, removed ones stop counting), and
//! deleted messages are forgotten. A snail that gets deleted stays counted as caught: it was
//! posted. Pictures that fail to download are retried later and listed by host in
//! `/snails backfill status`.
//!
//! - `links.rs`: links to keys, so mirrors and tracking junk share one key (pure)
//! - `fingerprint.rs`: picture fingerprints and the detail check (pure)
//! - `collect.rs`: which links and pictures of a message count (pure)
//! - `store.rs`: the tables and queries
//! - `index.rs`: downloading and saving one message
//! - `check.rs`: the Check Snail command
//! - `board.rs`: `/snails top`, the leaderboard, and `/snails reset`
//! - `backfill.rs`: `/snails backfill`, the history crawl and the startup catch-up
//! - `retry.rs`: trying failed pictures again

mod backfill;
mod board;
mod check;
mod collect;
mod fingerprint;
mod index;
mod links;
mod retry;
mod store;

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::util::discord::{ChannelId, GuildId, Message, MessageId, get_message};
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::core::{BotCtx, Command, Feature, Result, Stat};
use crate::util::media;

/// How long after a new message it's read again for late link previews.
const PREVIEW_RECHECK: Duration = Duration::from_secs(12);
/// How long the link and picture totals on the status picture are reused before they're
/// counted again. Counting reads both whole tables; the graphs only sample every 15 minutes.
const TOTALS_FOR: Duration = Duration::from_secs(15 * 60);

/// `[features.snails]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// How old a message may be and still count as a snail, in days. The backfill and its
    /// catch-up reach old messages, and reposting is about now.
    pub count_within_days: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            count_within_days: 7,
        }
    }
}

/// The settings, read the first time they're needed.
fn settings(ctx: &BotCtx) -> &'static Settings {
    static SETTINGS: OnceLock<Settings> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        ctx.config.feature("snails").unwrap_or_else(|err| {
            warn!("snails use their default settings: {err:#}");
            Settings::default()
        })
    })
}

/// (links, pictures, crawl channels finished, crawl channels), from `store::stats`.
type Totals = (u64, u64, u64, u64);

#[derive(Default)]
pub struct Snails {
    /// New messages waiting for their late link preview recheck. An edit event that reads
    /// the message first takes it off, and the recheck is skipped.
    waiting: Mutex<HashSet<u64>>,
    /// The totals for the status picture, and when they were counted.
    totals: Mutex<Option<(Instant, Totals)>>,
}

#[async_trait]
impl Feature for Snails {
    fn name(&self) -> &'static str {
        "snails"
    }

    fn commands(&self) -> Vec<Command> {
        vec![check::check_snail(), board::snails()]
    }

    fn migrations(&self) -> &'static [&'static str] {
        store::MIGRATIONS
    }

    /// Carries on with crawls an admin started before the restart, catches up channels
    /// crawled before, and starts retrying failed pictures. Never starts a new crawl.
    async fn start(&self, ctx: &BotCtx) -> Result<()> {
        let (running, crawled) = ctx
            .db
            .call(|conn| Ok((store::running_guilds(conn)?, store::crawled_guilds(conn)?)))
            .await?;
        let gate = ctx.gate(self.name());
        for guild in running {
            let guild = GuildId::new(guild);
            if gate.allows_guild(guild) {
                backfill::spawn_worker(ctx, guild);
            }
        }
        let crawled = crawled
            .into_iter()
            .map(GuildId::new)
            .filter(|&guild| gate.allows_guild(guild))
            .collect();
        backfill::spawn_catch_up(ctx, crawled);
        retry::spawn(ctx);
        Ok(())
    }

    /// Saves a new message's links and pictures right away, then reads it once more a bit
    /// later: Discord adds link previews (and their pictures) a few seconds after the
    /// message. That usually arrives as an edit too, so the recheck is skipped when the edit
    /// was read first, or when every link already has its preview. (Reading twice would be
    /// harmless, only wasted: only what's new is checked and a message counts once.)
    async fn on_message(&self, ctx: &BotCtx, msg: &Message) -> Result<()> {
        let Some(guild) = msg.guild_id else {
            return Ok(());
        };
        if !collect::has_content(msg) {
            return Ok(());
        }
        // Noted before saving, so an edit that arrives while this downloads still counts.
        let recheck = msg.embeds.len() < media::preview_links(msg).len();
        if recheck {
            self.waiting.lock().unwrap().insert(msg.id.get());
        }
        if let Err(err) = check::on_new_message(ctx, guild, msg).await {
            warn!("couldn't save the message's links and pictures: {err:#}");
        }
        if !recheck {
            return Ok(());
        }
        tokio::select! {
            _ = ctx.shutdown.cancelled() => {}
            _ = tokio::time::sleep(PREVIEW_RECHECK) => {}
        }
        // Still waiting unless an edit event read the message in the meantime.
        let still_waiting = self.waiting.lock().unwrap().remove(&msg.id.get());
        if !still_waiting || ctx.shutdown.is_cancelled() {
            return Ok(());
        }
        match get_message(&ctx.http, msg.channel_id, msg.id).await {
            Ok(fresh) => {
                if let Err(err) = check::on_new_message(ctx, guild, &fresh).await {
                    warn!("couldn't save the message's late link previews: {err:#}");
                }
            }
            // Deleted in the meantime: on_messages_deleted forgets it.
            Err(err) => debug!("couldn't read the message again for link previews: {err}"),
        }
        Ok(())
    }

    /// An edit (or a link preview Discord added later): reads the message again so added
    /// links and pictures count and removed ones stop counting.
    async fn on_message_edit(&self, ctx: &BotCtx, msg: &Message) -> Result<()> {
        let Some(guild) = msg.guild_id else {
            return Ok(());
        };
        // Skip the database when the message has nothing to index now and had nothing before.
        let may_have_content = !media::links(&msg.content).is_empty()
            || !msg.embeds.is_empty()
            || !msg.attachments.is_empty();
        let id = msg.id.get();
        if !may_have_content
            && !ctx
                .db
                .call(move |conn| Ok(store::is_indexed(conn, id)?))
                .await?
        {
            return Ok(());
        }
        if msg.author.bot {
            return Ok(());
        }
        // This read is newer than the recheck would be, so a waiting recheck is skipped.
        self.waiting.lock().unwrap().remove(&msg.id.get());
        if let Err(err) = check::on_new_message(ctx, guild, msg).await {
            warn!("couldn't save an edited message's links and pictures: {err:#}");
        }
        Ok(())
    }

    /// Deleted messages stop counting as earlier posts. Snails already counted stay counted.
    async fn on_messages_deleted(
        &self,
        ctx: &BotCtx,
        _channel: ChannelId,
        messages: &[MessageId],
    ) -> Result<()> {
        let ids: Vec<u64> = messages.iter().map(|m| m.get()).collect();
        ctx.db
            .call(move |conn| {
                for id in ids {
                    store::forget(conn, id)?;
                }
                Ok(())
            })
            .await
    }

    async fn stats(&self, ctx: &BotCtx) -> Result<Vec<Stat>> {
        let week_ago = Utc::now().timestamp() - 7 * 86_400;
        let caught = ctx
            .db
            .call(move |conn| Ok(store::caught_since(conn, week_ago)?))
            .await?;
        let (links, pictures, finished, channels) = self.totals(ctx).await?;
        let mut stats = vec![
            Stat::new("Caught this week", caught),
            Stat::new("Links", links),
            Stat::new("Pictures", pictures),
        ];
        if channels > 0 {
            stats.push(Stat::new(
                "Backfill",
                format!("{finished}/{channels} channels"),
            ));
        }
        Ok(stats)
    }
}

impl Snails {
    /// The totals, counted again at most every [`TOTALS_FOR`]. The status picture asks
    /// every minute.
    async fn totals(&self, ctx: &BotCtx) -> Result<Totals> {
        if let Some((at, totals)) = *self.totals.lock().unwrap()
            && at.elapsed() < TOTALS_FOR
        {
            return Ok(totals);
        }
        let totals = ctx.db.call(|conn| Ok(store::stats(conn)?)).await?;
        *self.totals.lock().unwrap() = Some((Instant::now(), totals));
        Ok(totals)
    }
}
