//! "Check Snail": has this message's link or picture been posted in the server before?

use anyhow::Context as _;
use chrono::Utc;
use image::DynamicImage;
use poise::CreateReply;
use serenity::all::{ChannelId, CreateAllowedMentions, GuildId, Message, MessageId};
use tracing::debug;

use super::fingerprint::{self, Fingerprint};
use super::index::{self, LoadedPicture};
use super::store::{self, Candidate, Catch, Failure, Indexed, Posted};
use super::{backfill, collect};
use crate::core::{BotCtx, Context, Result};
use crate::util::count;

/// At most this many close pictures get the detail check (each needs a download).
const MAX_CANDIDATES: usize = 12;
/// At most this many snails are listed.
const MAX_LISTED: usize = 10;
/// Only messages posted this recently are counted as snails. Edits, late pictures and the
/// catch-up can reach much older messages (a picture from the backfill that loaded on retry,
/// an old message Discord sends again with refreshed previews); reposting is about now.
/// `count_within_days` in the settings, a week by default.
const DAY_SECS: i64 = 24 * 60 * 60;

/// Check whether this was posted here before
#[poise::command(context_menu_command = "Check Snail", guild_only, ephemeral)]
pub async fn check_snail(ctx: Context<'_>, msg: Message) -> Result<()> {
    let guild = ctx
        .guild_id()
        .context("guild_only command without a server")?;
    // Downloads and the detail check take longer than Discord's 3 seconds.
    ctx.defer_ephemeral().await?;
    let text = match find_snails(ctx.data(), guild, &msg).await? {
        None => "This message has no links or pictures to check.".to_string(),
        Some((snails, unchecked, failed)) => {
            let mut text = match (snails.is_empty(), failed) {
                (true, 0) => "No snails: this hasn't been posted here before.".to_string(),
                (true, _) => "No snails among what could be checked.".to_string(),
                (false, _) => describe(guild, &snails, unchecked),
            };
            if failed > 0 {
                text.push_str(&format!(
                    "\n-# {} couldn't be downloaded, so {} not checked. Try again in a few minutes.",
                    count(failed, "picture"),
                    if failed == 1 { "it was" } else { "they were" }
                ));
            }
            text
        }
    };
    ctx.send(
        CreateReply::default()
            .content(text)
            .allowed_mentions(CreateAllowedMentions::new()),
    )
    .await?;
    Ok(())
}

/// An earlier post of the same thing.
#[derive(Debug)]
struct Snail {
    posted: Posted,
    same_link: bool,
}

/// The earlier posts (oldest first, at most [`MAX_LISTED`]), how many more weren't checked,
/// and how many pictures failed to load. None when the message has nothing to check.
async fn find_snails(
    ctx: &BotCtx,
    guild: GuildId,
    msg: &Message,
) -> Result<Option<(Vec<Snail>, usize, usize)>> {
    let keys = index::link_keys(msg).await;
    let (pictures, failed) = index::load_pictures(msg).await;
    if keys.is_empty() && pictures.is_empty() && failed.is_empty() {
        return Ok(None);
    }
    let failed_count = failed.len();
    save_if_new(ctx, guild, msg, &keys, &pictures, failed).await?;
    let (snails, unchecked) = earlier_posts(ctx, guild, msg, &keys, &pictures, MAX_LISTED).await?;
    Ok(Some((snails, unchecked, failed_count)))
}

/// A new message, or one that changed (an edit, a late link preview, or a message read by
/// the startup catch-up): saves its links and pictures, and if what's new in it is a snail,
/// notes that for the control panel's count. A message counts once however often it
/// changes. Nothing is posted: snails are only pointed out when someone asks with Check
/// Snail.
pub async fn on_new_message(ctx: &BotCtx, guild: GuildId, msg: &Message) -> Result<()> {
    let synced = index::sync_message(ctx, guild, msg).await?;
    count_if_snail(ctx, guild, msg, &synced.new_links, &synced.new_pictures).await
}

/// A failed picture of `msg` downloaded on retry (and was saved): checked like a new
/// picture, so a snail whose picture was slow to load still counts.
pub async fn on_retried_picture(
    ctx: &BotCtx,
    guild: GuildId,
    msg: &Message,
    picture: LoadedPicture,
) -> Result<()> {
    count_if_snail(ctx, guild, msg, &[], &[picture]).await
}

/// Counts `msg` as caught if any of `links` or `pictures` (what's new in it) was posted
/// before. Only what's new is checked: the rest was checked when it arrived. A message is
/// counted once, and only if it was posted in the last `count_within_days`.
async fn count_if_snail(
    ctx: &BotCtx,
    guild: GuildId,
    msg: &Message,
    links: &[String],
    pictures: &[LoadedPicture],
) -> Result<()> {
    if links.is_empty() && pictures.is_empty() {
        return Ok(());
    }
    let at = msg.timestamp.unix_timestamp();
    let within = super::settings(ctx).count_within_days.max(1) * DAY_SECS;
    if !recent(at, Utc::now().timestamp(), within) {
        return Ok(());
    }
    let id = msg.id.get();
    if ctx
        .db
        .call(move |conn| Ok(store::is_caught(conn, id)?))
        .await?
    {
        return Ok(());
    }
    let (earlier, _) = earlier_posts(ctx, guild, msg, links, pictures, 1).await?;
    let Some(original) = earlier.into_iter().next() else {
        return Ok(());
    };
    let catch = Catch {
        guild: guild.get(),
        message: id,
        channel: msg.channel_id.get(),
        author: msg.author.id.get(),
        at,
        original: original.posted,
    };
    ctx.db
        .call(move |conn| Ok(store::record_caught(conn, &catch)?))
        .await
}

/// Whether a message posted at `at` is new enough to count as a snail at `now`: no more
/// than `within` seconds old.
fn recent(at: i64, now: i64, within: i64) -> bool {
    now - at <= within
}

/// Earlier posts of the same links or pictures, oldest first. Each one is checked before it
/// counts (read again from Discord, and a picture downloaded for the detail check), so this
/// stops once `limit` are confirmed. Also returns how many earlier posts of the same link
/// were left unchecked then; close pictures aren't looked for at all once the limit is hit.
async fn earlier_posts(
    ctx: &BotCtx,
    guild: GuildId,
    msg: &Message,
    keys: &[String],
    pictures: &[LoadedPicture],
    limit: usize,
) -> Result<(Vec<Snail>, usize)> {
    let mut snails = Vec::new();
    let (g, before) = (guild.get(), msg.id.get());
    let keys = keys.to_vec();
    let same_links = ctx
        .db
        .call(move |conn| Ok(store::same_links(conn, g, &keys, before)?))
        .await?;
    let mut unchecked = 0;
    for posted in same_links {
        if snails.len() >= limit {
            unchecked += 1;
        } else if still_there(ctx, &posted).await?.is_some() {
            snails.push(Snail {
                posted,
                same_link: true,
            });
        }
    }
    if snails.len() >= limit {
        return Ok((snails, unchecked));
    }

    let checked: Vec<Fingerprint> = pictures.iter().map(|p| p.fp.clone()).collect();
    for candidate in close_pictures(ctx, g, checked, before).await? {
        if snails.iter().any(|s| s.posted == candidate.posted) {
            continue;
        }
        let image = &pictures[candidate.checked].image;
        if same_picture(ctx, &candidate, image).await? {
            snails.push(Snail {
                posted: candidate.posted,
                same_link: false,
            });
            if snails.len() >= limit {
                break;
            }
        }
    }
    snails.sort_by_key(|s| s.posted.message);
    Ok((snails, unchecked))
}

/// Saves the checked message too, so later checks find it. Pictures that failed go on the
/// retry list.
async fn save_if_new(
    ctx: &BotCtx,
    guild: GuildId,
    msg: &Message,
    keys: &[String],
    pictures: &[LoadedPicture],
    failed: Vec<Failure>,
) -> Result<()> {
    let found = Indexed {
        guild: guild.get(),
        channel: msg.channel_id.get(),
        message: msg.id.get(),
        author: msg.author.id.get(),
        links: keys.to_vec(),
        pictures: pictures.iter().map(LoadedPicture::stored).collect(),
        failed,
        // Only saved when it wasn't read before, so nothing failed before.
        waiting: Vec::new(),
    };
    let now = Utc::now().timestamp();
    ctx.db
        .call(move |conn| {
            if !store::is_indexed(conn, found.message)? {
                let tx = conn.transaction()?;
                store::save(&tx, &found, now)?;
                tx.commit()?;
            }
            Ok(())
        })
        .await
}

/// Older pictures within the match distance, closest first, at most one per stored picture.
async fn close_pictures(
    ctx: &BotCtx,
    guild: u64,
    checked: Vec<Fingerprint>,
    before: u64,
) -> Result<Vec<Candidate>> {
    if checked.is_empty() {
        return Ok(Vec::new());
    }
    let checked = std::sync::Arc::new(checked);
    let mut found: Vec<Candidate> = Vec::new();
    let mut after = 0;
    loop {
        // The database thread only reads; comparing a chunk takes a moment of CPU, so it
        // runs on a blocking thread and other features' queries don't wait for it.
        let (rows, next) = ctx
            .db
            .call(move |conn| Ok(store::pictures_after(conn, guild, before, after)?))
            .await?;
        let checked = checked.clone();
        let chunk = tokio::task::spawn_blocking(move || store::close_to(&checked, &rows)).await?;
        found.extend(chunk);
        match next {
            Some(rowid) => after = rowid,
            None => break,
        }
    }
    found.sort_by_key(|c| c.distance);
    let mut unique: Vec<Candidate> = Vec::new();
    for c in found {
        if !unique
            .iter()
            .any(|u| u.posted == c.posted && u.position == c.position)
        {
            unique.push(c);
        }
    }
    unique.truncate(MAX_CANDIDATES);
    Ok(unique)
}

/// Reads a stored message again. None (and forgotten) if it was deleted; None (but kept) if
/// the bot can't read it right now, like a channel it lost access to.
async fn still_there(ctx: &BotCtx, posted: &Posted) -> Result<Option<Message>> {
    let channel = ChannelId::new(posted.channel);
    match channel
        .message(&ctx.http, MessageId::new(posted.message))
        .await
    {
        Ok(msg) => Ok(Some(msg)),
        Err(err) if is_not_found(&err) => {
            let id = posted.message;
            ctx.db
                .call(move |conn| Ok(store::forget(conn, id)?))
                .await?;
            Ok(None)
        }
        Err(err) if backfill::is_no_access(&err) => {
            debug!(
                message = posted.message,
                "can't read an earlier message: {err}"
            );
            Ok(None)
        }
        Err(err) => Err(err).context("reading an earlier message"),
    }
}

/// The detail check against a stored picture, downloaded again from its message.
async fn same_picture(ctx: &BotCtx, candidate: &Candidate, image: &DynamicImage) -> Result<bool> {
    let Some(msg) = still_there(ctx, &candidate.posted).await? else {
        return Ok(false);
    };
    let Some(picture) = collect::pictures(&msg).into_iter().nth(candidate.position) else {
        return Ok(false);
    };
    let earlier = match index::load_picture(&picture).await {
        Ok(img) => img,
        Err(err) => {
            debug!("couldn't load the earlier picture for the detail check: {err:#}");
            return Ok(false);
        }
    };
    let image = image.clone();
    let difference =
        tokio::task::spawn_blocking(move || fingerprint::detail_difference(&image, &earlier))
            .await?;
    debug!(
        message = candidate.posted.message,
        distance = candidate.distance,
        difference,
        "detail check"
    );
    Ok(difference <= fingerprint::DETAIL_LIMIT)
}

fn is_not_found(err: &serenity::Error) -> bool {
    backfill::http_status(err) == Some(404)
}

/// The reply listing `snails`, plus a line for the `unchecked` ones not listed.
fn describe(guild: GuildId, snails: &[Snail], unchecked: usize) -> String {
    let mut text = String::from("🐌 **Snail!** This was posted before:");
    for snail in snails {
        let p = &snail.posted;
        let when = MessageId::new(p.message).created_at().unix_timestamp();
        let what = if snail.same_link {
            "same link"
        } else {
            "same picture"
        };
        // A masked link and a channel chip keep each line short on a phone.
        text.push_str(&format!(
            "\n- <t:{when}:d> <@{}> in <#{}>, {what}: [jump](https://discord.com/channels/{guild}/{}/{})",
            p.author, p.channel, p.channel, p.message
        ));
    }
    if unchecked > 0 {
        text.push_str(&format!(
            "\n…and {} of the same link, not checked one by one.",
            count(unchecked, "more post")
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_recent_messages_count() {
        let (now, week) = (1_800_000_000, 7 * DAY_SECS);
        assert!(recent(now, now, week));
        assert!(recent(now - 3 * 86_400, now, week));
        assert!(!recent(now - 8 * 86_400, now, week));
        assert!(recent(now - 8 * 86_400, now, 30 * DAY_SECS));
        // A message from 2023 that an edit or a retry brings back.
        assert!(!recent(1_685_302_717, now, week));
    }
}
