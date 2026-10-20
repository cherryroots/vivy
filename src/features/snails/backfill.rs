//! `/snails backfill`: reads the server's history so older posts count as snails too.
//!
//! Nothing starts on its own: an admin runs `/snails backfill start`. The crawl goes channel by
//! channel, 100 messages at a time from the newest back, and saves its place after every page,
//! so pausing, restarting the bot or a network failure never loses work. A crawl an admin
//! started (and didn't pause) carries on when the bot restarts.
//!
//! On every start, channels the backfill has read before are caught up: what was posted
//! there while the bot was offline is read (from the newest message read or saved, forward)
//! and checked like new messages. That's not a backfill start; channels never crawled stay
//! untouched until an admin starts one.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use futures::stream::{self, StreamExt};
use tracing::{debug, info, warn};
use twilight_interactions::command::{CommandModel, CommandOption, CreateCommand, CreateOption};

use super::index;
use super::store::{self, CrawlStatus, HostFailures};
use super::{check, collect};
use crate::core::{BotCtx, CommandCtx, Result, user_error};
use crate::util::discord::{
    Channel, ChannelId, ChannelType, GuildId, MessageId, Page, error_status, get_messages,
};

/// Messages indexed at the same time. Each downloads its pictures from Discord's media proxy.
const PARALLEL: usize = 8;
/// Discord's largest page of messages.
const PAGE: u16 = 100;
/// Wait after a failed page before trying again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Servers with a crawl running in this process, so a second start doesn't run two.
static WORKERS: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(Default::default);

#[derive(Debug, CommandOption, CreateOption)]
pub enum Action {
    #[option(name = "start", value = "start")]
    Start,
    #[option(name = "pause", value = "pause")]
    Pause,
    #[option(name = "status", value = "status")]
    Status,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "backfill",
    desc = "Read the server's older messages so Check Snail knows them (admins)"
)]
pub struct SnailsBackfill {
    /// start (or continue), pause, or show progress
    action: Action,
}

/// `/snails backfill`. Answers privately.
pub async fn snail_backfill(ctx: CommandCtx, args: SnailsBackfill) -> Result<()> {
    if !ctx.bot.is_admin(ctx.author_id()) {
        return Err(user_error("Only admins can use this command."));
    }
    let guild = ctx
        .guild_id()
        .context("server-only command without a server")?;
    let bot = &ctx.bot;
    let action = args.action;
    let g = guild.get();
    let text = match action {
        Action::Start => {
            // Listing every thread takes a few requests.
            ctx.defer().await?;
            let channels = list_channels(bot, guild).await?;
            let total = channels.len();
            let added = bot
                .db
                .call(move |conn| {
                    let tx = conn.transaction()?;
                    let added = store::add_crawl_channels(&tx, g, &channels)?;
                    store::set_running(&tx, g, true)?;
                    tx.commit()?;
                    Ok(added)
                })
                .await?;
            info!(guild = g, channels = total, added, "snail backfill started");
            spawn_worker(bot, guild);
            format!(
                "Started. Found {total} channels and threads ({added} new to the backfill). \
                 This can take hours and keeps going after restarts; \
                 `/snails backfill status` shows how far it is."
            )
        }
        Action::Pause => {
            bot.db
                .call(move |conn| Ok(store::set_running(conn, g, false)?))
                .await?;
            info!(guild = g, "snail backfill paused");
            "Paused after the current page. `/snails backfill start` continues where it stopped."
                .to_string()
        }
        Action::Status => {
            let (status, next, failures, dropped) = bot
                .db
                .call(move |conn| {
                    Ok((
                        store::crawl_status(conn, g)?,
                        store::next_crawl_channel(conn, g)?,
                        store::failures_by_host(conn, g)?,
                        store::dropped_by_host(conn, g)?,
                    ))
                })
                .await?;
            let mut text = describe(&status);
            if let (true, Some((channel, _))) = (status.running, next) {
                text.push_str(&format!("\nReading <#{channel}> now."));
            }
            text.push_str(&describe_failures(&failures));
            text.push_str(&describe_dropped(&dropped));
            text
        }
    };
    ctx.say(text).await?;
    Ok(())
}

fn describe(s: &CrawlStatus) -> String {
    let state = if s.channels == 0 {
        "not started"
    } else if s.running {
        "running"
    } else if s.finished == s.channels {
        "finished"
    } else {
        "paused"
    };
    let mut text = format!(
        "**Snail backfill: {state}**\nChannels and threads: {} of {} done\n\
         Messages read: {}, pictures hashed: {}",
        s.finished, s.channels, s.messages, s.pictures
    );
    if s.failed > 0 {
        text.push_str(&format!("\n{} couldn't be read (no access).", s.failed));
    }
    text
}

/// Pictures dropped after their last try, per host: "Dropped after 3 tries:
/// `pbs.twimg.com` (50x), `cdn.discordapp.com` (2x)". Empty when none were.
fn describe_dropped(hosts: &[(String, u64)]) -> String {
    if hosts.is_empty() {
        return String::new();
    }
    let mut list: Vec<String> = hosts
        .iter()
        .take(MAX_HOSTS)
        .map(|(host, count)| format!("`{host}` ({count}x)"))
        .collect();
    if hosts.len() > MAX_HOSTS {
        list.push(format!("{} more hosts", hosts.len() - MAX_HOSTS));
    }
    format!(
        "\n\n**Dropped after {} tries:** {}",
        store::MAX_ATTEMPTS,
        list.join(", ")
    )
}

/// Hosts shown in the failed pictures list.
const MAX_HOSTS: usize = 10;

/// The failed pictures grouped by host, so a provider with trouble stands out. Empty when
/// nothing failed.
fn describe_failures(hosts: &[HostFailures]) -> String {
    if hosts.is_empty() {
        return String::new();
    }
    let waiting: u64 = hosts.iter().map(|h| h.waiting).sum();
    let mut text = format!("\n\n**Pictures that failed to download:** {waiting} waiting to retry");
    for h in hosts.iter().take(MAX_HOSTS) {
        let mut error: String = h.error.chars().take(80).collect();
        if error.len() < h.error.len() {
            error.push('…');
        }
        text.push_str(&format!(
            "\n- `{}`: {} (last error: {error})",
            h.host, h.waiting
        ));
    }
    if hosts.len() > MAX_HOSTS {
        text.push_str(&format!("\n…and {} more hosts.", hosts.len() - MAX_HOSTS));
    }
    text
}

/// Every channel and thread with messages: text, news, voice and stage chats, plus active
/// and archived threads (forum posts are threads too).
async fn list_channels(ctx: &BotCtx, guild: GuildId) -> Result<Vec<(u64, String)>> {
    let channels = ctx
        .http
        .guild_channels(guild)
        .await
        .context("listing the server's channels")?
        .models()
        .await?;
    let mut found = Vec::new();
    for channel in &channels {
        if matches!(
            channel.kind,
            ChannelType::GuildText
                | ChannelType::GuildAnnouncement
                | ChannelType::GuildVoice
                | ChannelType::GuildStageVoice
        ) {
            found.push(thread_entry(channel));
        }
        if matches!(
            channel.kind,
            ChannelType::GuildText | ChannelType::GuildAnnouncement | ChannelType::GuildForum
        ) {
            for private in [false, true] {
                match archived_threads(ctx, channel.id, private).await {
                    Ok(threads) => found.extend(threads.iter().map(thread_entry)),
                    // Private archived threads need Manage Threads; without it they're skipped.
                    Err(err) => debug!(
                        "no archived threads for #{}: {err}",
                        channel.name.as_deref().unwrap_or_default()
                    ),
                }
            }
        }
    }
    let active = ctx
        .http
        .active_threads(guild)
        .await
        .context("listing active threads")?
        .model()
        .await?;
    found.extend(active.threads.iter().map(thread_entry));
    found.sort();
    found.dedup_by_key(|(id, _)| *id);
    Ok(found)
}

fn thread_entry(channel: &Channel) -> (u64, String) {
    (channel.id.get(), channel.name.clone().unwrap_or_default())
}

/// All archived threads of a channel. Discord pages them by archive time.
async fn archived_threads(
    ctx: &BotCtx,
    channel: ChannelId,
    private: bool,
) -> anyhow::Result<Vec<Channel>> {
    let mut threads: Vec<Channel> = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let page = if private {
            let request = ctx.http.private_archived_threads(channel).limit(100);
            match &before {
                Some(before) => request.before(before).await?,
                None => request.await?,
            }
        } else {
            let request = ctx.http.public_archived_threads(channel).limit(100);
            match &before {
                Some(before) => request.before(before).await?,
                None => request.await?,
            }
        };
        let page = page.model().await?;
        let oldest = page.threads.last().and_then(|t| {
            let time = t.thread_metadata.as_ref()?.archive_timestamp;
            Some(time.iso_8601().to_string())
        });
        let more = page.has_more.unwrap_or(false);
        threads.extend(page.threads);
        match oldest {
            Some(time) if more => before = Some(time),
            _ => return Ok(threads),
        }
    }
}

/// Starts the crawl for a server unless it's already running here.
pub fn spawn_worker(ctx: &BotCtx, guild: GuildId) {
    if !WORKERS.lock().unwrap().insert(guild.get()) {
        return;
    }
    let ctx = ctx.clone();
    ctx.tasks.clone().spawn(async move {
        if let Err(err) = crawl(&ctx, guild).await {
            warn!(guild = guild.get(), "snail backfill stopped: {err:#}");
            WORKERS.lock().unwrap().remove(&guild.get());
        }
    });
}

/// Why a page couldn't be read.
enum PageError {
    /// The bot can't read this channel; skip it.
    NoAccess(String),
    /// Anything else (network, Discord trouble): try again later.
    Retry(anyhow::Error),
}

async fn crawl(ctx: &BotCtx, guild: GuildId) -> Result<()> {
    let g = guild.get();
    loop {
        if ctx.shutdown.is_cancelled() {
            WORKERS.lock().unwrap().remove(&g);
            return Ok(());
        }
        let (running, next) = ctx
            .db
            .call(move |conn| {
                Ok((
                    store::is_running(conn, g)?,
                    store::next_crawl_channel(conn, g)?,
                ))
            })
            .await?;
        let next = if running { next } else { None };
        let Some((channel, before)) = next else {
            if running {
                ctx.db
                    .call(move |conn| Ok(store::set_running(conn, g, false)?))
                    .await?;
                info!(guild = g, "snail backfill finished");
            }
            WORKERS.lock().unwrap().remove(&g);
            // An admin may have pressed start again just before this; then keep going.
            let again = ctx
                .db
                .call(move |conn| Ok(store::is_running(conn, g)?))
                .await?;
            if again && WORKERS.lock().unwrap().insert(g) {
                continue;
            }
            return Ok(());
        };
        match read_page(ctx, guild, ChannelId::new(channel), before).await {
            Ok(()) => {}
            Err(PageError::NoAccess(error)) => {
                debug!(channel, "snail backfill can't read this channel: {error}");
                ctx.db
                    .call(move |conn| Ok(store::crawl_failed(conn, channel, &error)?))
                    .await?;
            }
            Err(PageError::Retry(err)) => {
                warn!(
                    channel,
                    "snail backfill page failed, retrying in a minute: {err:#}"
                );
                tokio::select! {
                    _ = ctx.shutdown.cancelled() => {}
                    _ = tokio::time::sleep(RETRY_AFTER) => {}
                }
            }
        }
    }
}

/// Reads and indexes one page of a channel, then saves the place.
async fn read_page(
    ctx: &BotCtx,
    guild: GuildId,
    channel: ChannelId,
    before: Option<u64>,
) -> std::result::Result<(), PageError> {
    let page = match before {
        Some(before) => Page::Before(MessageId::new(before)),
        None => Page::Newest,
    };
    let messages = match get_messages(&ctx.http, channel, page, PAGE).await {
        Ok(messages) => messages,
        Err(err) => {
            return Err(if is_no_access(&err) {
                PageError::NoAccess(format!("{err:#}"))
            } else {
                PageError::Retry(err)
            });
        }
    };
    // Newest first, so the last one is where the next page starts.
    let oldest = messages.last().map(|m| m.id.get());
    let newest = messages.first().map(|m| m.id.get());
    let finished = messages.len() < PAGE as usize;
    let jobs: Vec<_> = messages
        .iter()
        .filter(|m| !m.author.bot)
        .map(|msg| async move {
            match index::index_message(ctx, guild, msg).await {
                Ok(count) => count,
                Err(err) => {
                    debug!(message = msg.id.get(), "couldn't index: {err:#}");
                    0
                }
            }
        })
        .collect();
    let pictures: usize = stream::iter(jobs)
        .buffer_unordered(PARALLEL)
        .collect::<Vec<usize>>()
        .await
        .into_iter()
        .sum();
    let (id, count) = (channel.get(), messages.len());
    ctx.db
        .call(move |conn| {
            if let Some(newest) = newest {
                // Where the startup catch-up starts.
                store::crawl_newest(conn, id, newest)?;
            }
            Ok(store::crawl_progress(
                conn, id, oldest, count, pictures, finished,
            )?)
        })
        .await
        .map_err(PageError::Retry)?;
    Ok(())
}

// ---- Startup catch-up ----

/// Catches up every server with backfill history, one after another, in the background.
pub fn spawn_catch_up(ctx: &BotCtx, guilds: Vec<GuildId>) {
    let ctx = ctx.clone();
    ctx.tasks.clone().spawn(async move {
        for guild in guilds {
            if let Err(err) = catch_up(&ctx, guild).await {
                warn!(guild = guild.get(), "snail catch-up stopped: {err:#}");
            }
        }
    });
}

/// Reads what was posted while the bot was offline in a server's crawled channels.
async fn catch_up(ctx: &BotCtx, guild: GuildId) -> Result<()> {
    let g = guild.get();
    let channels = ctx
        .db
        .call(move |conn| Ok(store::catch_up_channels(conn, g)?))
        .await?;
    let mut total = 0;
    for (channel, after) in channels {
        if ctx.shutdown.is_cancelled() {
            return Ok(());
        }
        let channel = ChannelId::new(channel);
        if !ctx.allows("snails", Some(guild), channel) {
            continue;
        }
        match catch_up_channel(ctx, guild, channel, after).await {
            Ok(read) => total += read,
            // Deleted, or the bot lost access: nothing to catch up.
            Err(err) if is_no_access(&err) => {
                debug!(channel = channel.get(), "snail catch-up skipped: {err}")
            }
            Err(err) => warn!(channel = channel.get(), "snail catch-up failed: {err:#}"),
        }
    }
    if total > 0 {
        info!(
            guild = g,
            messages = total,
            "snail catch-up read the missed messages"
        );
    }
    Ok(())
}

/// Reads one channel forward from `after`, oldest first so a repost inside the missed
/// stretch still finds its original. Saves its place after every page. Returns how many
/// messages it read.
async fn catch_up_channel(
    ctx: &BotCtx,
    guild: GuildId,
    channel: ChannelId,
    mut after: u64,
) -> anyhow::Result<usize> {
    let mut read = 0;
    loop {
        let mut page =
            get_messages(&ctx.http, channel, Page::After(MessageId::new(after)), PAGE).await?;
        // Discord sends the page newest first.
        page.sort_by_key(|m| m.id);
        let Some(last) = page.last().map(|m| m.id.get()) else {
            return Ok(read);
        };
        for msg in &page {
            if ctx.shutdown.is_cancelled() {
                return Ok(read);
            }
            if msg.author.bot || !collect::has_content(msg) {
                continue;
            }
            // Missed messages are new messages, so they're checked for snails too.
            if let Err(err) = check::on_new_message(ctx, guild, msg).await {
                debug!(message = msg.id.get(), "couldn't index: {err:#}");
            }
        }
        read += page.len();
        after = last;
        let id = channel.get();
        ctx.db
            .call(move |conn| Ok(store::crawl_newest(conn, id, last)?))
            .await?;
        if page.len() < PAGE as usize {
            return Ok(read);
        }
    }
}

/// Whether Discord refused the request for good: any 4xx except 429 (slow down). Trying
/// again won't help, unlike a network error or a 5xx.
pub fn is_no_access(err: &anyhow::Error) -> bool {
    error_status(err).is_some_and(|s| (400..500).contains(&s) && s != 429)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text() {
        let mut s = CrawlStatus::default();
        assert!(describe(&s).contains("not started"));
        s.channels = 4;
        s.finished = 1;
        s.running = true;
        assert!(describe(&s).contains("running"));
        s.running = false;
        assert!(describe(&s).contains("paused"));
        s.finished = 4;
        s.failed = 1;
        let text = describe(&s);
        assert!(text.contains("finished") && text.contains("1 couldn't be read"));
    }

    #[test]
    fn dropped_text() {
        assert_eq!(describe_dropped(&[]), "");
        let text = describe_dropped(&[("pbs.twimg.com".into(), 50), ("i.imgur.com".into(), 2)]);
        assert!(
            text.contains("tries:** `pbs.twimg.com` (50x), `i.imgur.com` (2x)"),
            "{text}"
        );
    }

    #[test]
    fn failures_text() {
        assert_eq!(describe_failures(&[]), "");
        let hosts = vec![
            HostFailures {
                host: "pbs.twimg.com".into(),
                waiting: 3,
                error: "HTTP status client error (403 Forbidden)".into(),
            },
            HostFailures {
                host: "cdn.discordapp.com".into(),
                waiting: 1,
                error: "x".repeat(100),
            },
        ];
        let text = describe_failures(&hosts);
        assert!(text.contains("4 waiting to retry"), "{text}");
        assert!(text.contains("`pbs.twimg.com`: 3 (last error: HTTP"));
        assert!(text.contains("`cdn.discordapp.com`: 1 (last error: "));
        assert!(text.contains('…'));
    }
}
