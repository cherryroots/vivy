//! Reading a message's links and pictures and saving them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::util::discord::{GuildId, Message};
use anyhow::Context as _;
use chrono::Utc;
use futures::future::join_all;
use image::DynamicImage;
use tracing::debug;

use super::collect::{self, Picture};
use super::fingerprint::{self, Fingerprint};
use super::links;
use super::store::{self, Failure, Indexed, StoredPicture};
use crate::core::BotCtx;
use crate::util::media;

/// The small proxy pictures are a few KB; anything bigger than this is not what we asked for.
const MAX_SMALL_BYTES: usize = 5 * 1024 * 1024;
/// When the proxy can't serve a picture, the original is fetched up to this size.
const MAX_ORIGINAL_BYTES: usize = 20 * 1024 * 1024;

/// The link keys in a message: links to known sites (short links like vm.tiktok.com and t.co
/// are followed first), and articles from other sites that Discord previewed.
pub async fn link_keys(msg: &Message) -> Vec<String> {
    let mut keys = Vec::new();
    for link in collect::message_links(msg) {
        let link = if links::needs_resolving(&link) {
            resolve(&link).await.unwrap_or(link)
        } else {
            link
        };
        if let Some(key) = links::link_key(&link)
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    for link in collect::article_links(msg) {
        if let Some(key) = links::article_key(&link)
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    keys
}

/// Where a short link leads. Goes through the download client that refuses local and
/// private addresses, at every redirect too.
async fn resolve(link: &str) -> Option<String> {
    media::check_url(link)
        .map_err(|err| debug!("not following {link}: {err}"))
        .ok()?;
    let response = media::safe_client()
        .head(link)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|err| debug!("couldn't follow {link}: {err}"))
        .ok()?;
    Some(response.url().to_string())
}

/// Downloads a picture, small from the media proxy if it can, and decodes it.
pub async fn load_picture(picture: &Picture) -> anyhow::Result<DynamicImage> {
    let data = match media::download(&picture.url, MAX_SMALL_BYTES).await {
        Ok(data) => data,
        Err(err) => {
            debug!("the media proxy couldn't serve {}: {err}", picture.url);
            media::download(&picture.original, MAX_ORIGINAL_BYTES)
                .await
                .with_context(|| format!("downloading {}", picture.original))?
        }
    };
    let img = tokio::task::spawn_blocking(move || media::decode_bounded(&data))
        .await?
        .context("decoding the picture")?;
    // An empty picture has nothing to fingerprint (and would break the crops).
    if img.width() == 0 || img.height() == 0 {
        anyhow::bail!("the picture is empty");
    }
    Ok(img)
}

/// A picture that downloaded, with its fingerprint.
pub struct LoadedPicture {
    pub position: usize,
    pub source: String,
    pub image: DynamicImage,
    pub fp: Fingerprint,
}

impl LoadedPicture {
    pub fn stored(&self) -> StoredPicture {
        StoredPicture {
            position: self.position,
            source: self.source.clone(),
            fp: self.fp.clone(),
        }
    }
}

/// Downloads and fingerprints the picture at `position` of a message.
pub async fn load_one(position: usize, picture: &Picture) -> Result<LoadedPicture, Failure> {
    let failure = |err: anyhow::Error| Failure {
        position,
        source: picture.source(),
        host: picture.host(),
        error: format!("{err:#}"),
    };
    let img = load_picture(picture).await.map_err(failure)?;
    let (image, fp) = tokio::task::spawn_blocking(move || {
        let fp = fingerprint::fingerprint(&img);
        (img, fp)
    })
    .await
    .map_err(|err| failure(err.into()))?;
    Ok(LoadedPicture {
        position,
        source: picture.source(),
        image,
        fp,
    })
}

/// Loads several pictures at once, splitting them into the ones that loaded and the ones
/// that failed.
async fn load_all(
    msg: &Message,
    pictures: &[(usize, Picture)],
) -> (Vec<LoadedPicture>, Vec<Failure>) {
    let loads = pictures
        .iter()
        .map(|(position, picture)| load_one(*position, picture));
    let mut loaded = Vec::new();
    let mut failed = Vec::new();
    for result in join_all(loads).await {
        match result {
            Ok(picture) => loaded.push(picture),
            Err(failure) => {
                debug!(
                    "picture {} of message {} failed, will retry: {}",
                    failure.position, msg.id, failure.error
                );
                failed.push(failure);
            }
        }
    }
    (loaded, failed)
}

/// All of a message's pictures, fingerprinted, and the ones that failed to load.
pub async fn load_pictures(msg: &Message) -> (Vec<LoadedPicture>, Vec<Failure>) {
    let pictures: Vec<(usize, Picture)> = collect::pictures(msg).into_iter().enumerate().collect();
    load_all(msg, &pictures).await
}

/// How long an idle entry stays in [`MessageTracker`] before pruning.
const TRACK_TTL: Duration = Duration::from_secs(10 * 60);
/// Prune stale idle entries once the map reaches this size.
const PRUNE_AT: usize = 1_000;

struct MessageState {
    generation: u64,
    deleted: bool,
    in_flight: usize,
    touched: Instant,
}

/// Tracks the latest live revision of each message so a slow in-flight `sync_message`
/// (downloading pictures or following short links) never resurrects a deleted message or
/// overwrites a newer edit.
#[derive(Default)]
pub struct MessageTracker {
    states: Mutex<HashMap<u64, MessageState>>,
}

impl MessageTracker {
    /// Starts a live sync for `message`, bumping its generation. Returns `None` if the
    /// message was already deleted.
    pub fn begin_sync(self: &Arc<Self>, message: u64) -> Option<SyncGuard> {
        let mut states = self.states.lock().unwrap();
        let now = Instant::now();
        let state = states.entry(message).or_insert(MessageState {
            generation: 0,
            deleted: false,
            in_flight: 0,
            touched: now,
        });
        if state.deleted {
            state.touched = now;
            return None;
        }
        state.generation += 1;
        state.in_flight += 1;
        state.touched = now;
        let generation = state.generation;
        Some(SyncGuard {
            tracker: Arc::clone(self),
            message,
            generation,
        })
    }

    /// Marks `message` as deleted so any in-flight sync skips saving it.
    pub fn mark_deleted(&self, message: u64) {
        let mut states = self.states.lock().unwrap();
        let now = Instant::now();
        let state = states.entry(message).or_insert(MessageState {
            generation: 0,
            deleted: true,
            in_flight: 0,
            touched: now,
        });
        state.generation += 1;
        state.deleted = true;
        state.touched = now;
        prune_idle(&mut states);
    }

    /// Whether `generation` is still the latest live sync for `message` (neither superseded
    /// by a newer edit nor deleted).
    pub fn is_current(&self, message: u64, generation: u64) -> bool {
        let states = self.states.lock().unwrap();
        states
            .get(&message)
            .is_some_and(|s| !s.deleted && s.generation == generation)
    }

    /// Whether a live sync is currently running for `message`.
    pub fn is_in_flight(&self, message: u64) -> bool {
        let states = self.states.lock().unwrap();
        states.get(&message).is_some_and(|s| s.in_flight > 0)
    }
}

fn prune_idle(states: &mut HashMap<u64, MessageState>) {
    if states.len() >= PRUNE_AT {
        states.retain(|_, s| s.in_flight > 0 || s.touched.elapsed() < TRACK_TTL);
    }
}

/// Token held by one in-flight live sync. Decrements `in_flight` when dropped.
pub struct SyncGuard {
    tracker: Arc<MessageTracker>,
    message: u64,
    generation: u64,
}

impl SyncGuard {
    pub fn is_current(&self) -> bool {
        self.tracker.is_current(self.message, self.generation)
    }

    fn check_handle(&self) -> (Arc<MessageTracker>, u64) {
        (Arc::clone(&self.tracker), self.generation)
    }
}

impl Drop for SyncGuard {
    fn drop(&mut self) {
        let mut states = self.tracker.states.lock().unwrap();
        if let Some(state) = states.get_mut(&self.message) {
            state.in_flight = state.in_flight.saturating_sub(1);
            state.touched = Instant::now();
        }
        prune_idle(&mut states);
    }
}

/// Saves `found` only when `guard` (if any) is still the latest live revision of the
/// message. Returns `false` if a newer edit or a delete superseded this sync.
fn save_if_current(
    conn: &mut rusqlite::Connection,
    found: &Indexed,
    now: i64,
    guard: Option<&(Arc<MessageTracker>, u64)>,
) -> rusqlite::Result<bool> {
    if let Some((tracker, generation)) = guard
        && !tracker.is_current(found.message, *generation)
    {
        return Ok(false);
    }
    let tx = conn.transaction()?;
    store::save(&tx, found, now)?;
    tx.commit()?;
    Ok(true)
}

/// What [`sync_message`] found that wasn't saved before.
#[derive(Default)]
pub struct Synced {
    pub new_links: Vec<String>,
    pub new_pictures: Vec<LoadedPicture>,
    /// Pictures saved for the message now, old and new.
    pub pictures: usize,
}

/// Reads a message's links and pictures and makes the saved rows match: new ones are added
/// and ones an edit removed are dropped. Pictures already saved (the same file) keep their
/// fingerprint instead of being downloaded again, pictures that fail to load go on the
/// retry list, and pictures already on it are left to the retry loop. Safe to run again on
/// the same message: it only reports what is new. (Messages read from history don't say
/// which server they are in, so it's passed.)
///
/// When `guard` is passed (for live messages and edits), a message that was edited again or
/// deleted while downloads were running skips saving so it can't overwrite newer state or
/// resurrect a deleted message.
pub async fn sync_message(
    ctx: &BotCtx,
    guild: GuildId,
    msg: &Message,
    guard: Option<&SyncGuard>,
) -> anyhow::Result<Synced> {
    let id = msg.id.get();
    let (old_links, saved, retrying) = ctx
        .db
        .call(move |conn| {
            Ok((
                store::message_links(conn, id)?,
                store::message_pictures(conn, id)?,
                store::waiting_failures(conn, id)?,
            ))
        })
        .await?;
    let links = link_keys(msg).await;

    let mut kept = Vec::new();
    let mut to_load = Vec::new();
    let mut waiting = Vec::new();
    for (position, picture) in collect::pictures(msg).into_iter().enumerate() {
        let source = picture.source();
        // Pictures saved before sources were kept are matched by their place instead.
        let same = saved.iter().find(|s| match &s.source {
            Some(saved_source) => *saved_source == source,
            None => s.position == position,
        });
        match same {
            Some(s) => kept.push(StoredPicture {
                position,
                source,
                fp: s.fp.clone(),
            }),
            // It failed before and the retry loop is on it: downloading it again now (like
            // the re-read for link previews) would count as another try within seconds.
            None if retrying.contains(&source) => waiting.push(source),
            None => to_load.push((position, picture)),
        }
    }
    let (new_pictures, failed) = load_all(msg, &to_load).await;
    if guard.is_some_and(|g| !g.is_current()) {
        debug!(message = id, "skipping stale snail sync");
        return Ok(Synced::default());
    }

    let mut pictures = kept;
    pictures.extend(new_pictures.iter().map(LoadedPicture::stored));
    let count = pictures.len();
    let new_links = links
        .iter()
        .filter(|key| !old_links.contains(key))
        .cloned()
        .collect();
    let found = Indexed {
        guild: guild.get(),
        channel: msg.channel_id.get(),
        message: id,
        author: msg.author.id.get(),
        links,
        pictures,
        failed,
        waiting,
    };
    let now = Utc::now().timestamp();
    let guard_check = guard.map(SyncGuard::check_handle);
    let saved = ctx
        .db
        .call(move |conn| Ok(save_if_current(conn, &found, now, guard_check.as_ref())?))
        .await?;
    if !saved {
        debug!(message = id, "skipping stale snail save");
        return Ok(Synced::default());
    }
    Ok(Synced {
        new_links,
        new_pictures,
        pictures: count,
    })
}

/// Indexes one message from history unless it was read before. Returns how many pictures
/// were saved.
pub async fn index_message(ctx: &BotCtx, guild: GuildId, msg: &Message) -> anyhow::Result<usize> {
    let id = msg.id.get();
    if ctx
        .db
        .call(move |conn| Ok(store::is_indexed(conn, id)?))
        .await?
    {
        return Ok(0);
    }
    Ok(sync_message(ctx, guild, msg, None).await?.pictures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::test_connection;

    fn sample(message: u64, links: &[&str]) -> Indexed {
        Indexed {
            guild: 1,
            channel: 10,
            message,
            author: 100,
            links: links.iter().map(|s| s.to_string()).collect(),
            pictures: Vec::new(),
            failed: Vec::new(),
            waiting: Vec::new(),
        }
    }

    #[test]
    fn deleted_message_skips_stale_in_flight_save() {
        let mut conn = test_connection("snails", store::MIGRATIONS);
        let tracker = Arc::new(MessageTracker::default());
        let guard = tracker.begin_sync(42).unwrap();
        assert!(tracker.is_in_flight(42));
        assert!(guard.is_current());

        // The message is deleted while the sync is still downloading.
        tracker.mark_deleted(42);
        assert!(!guard.is_current());
        assert!(tracker.begin_sync(42).is_none());

        let saved = save_if_current(
            &mut conn,
            &sample(42, &["x:1"]),
            0,
            Some(&guard.check_handle()),
        )
        .unwrap();
        assert!(!saved);
        assert!(!store::is_indexed(&conn, 42).unwrap());

        drop(guard);
        assert!(!tracker.is_in_flight(42));
    }

    #[test]
    fn newer_edit_supersedes_older_in_flight_sync() {
        let mut conn = test_connection("snails", store::MIGRATIONS);
        let tracker = Arc::new(MessageTracker::default());
        let old_guard = tracker.begin_sync(50).unwrap();
        let new_guard = tracker.begin_sync(50).unwrap();

        assert!(!old_guard.is_current());
        assert!(new_guard.is_current());

        // The newer edit saves first.
        assert!(
            save_if_current(
                &mut conn,
                &sample(50, &["x:new"]),
                0,
                Some(&new_guard.check_handle()),
            )
            .unwrap()
        );
        // The slower older sync finishes afterwards and must not overwrite the edit.
        assert!(
            !save_if_current(
                &mut conn,
                &sample(50, &["x:old"]),
                0,
                Some(&old_guard.check_handle()),
            )
            .unwrap()
        );
        assert_eq!(store::message_links(&conn, 50).unwrap(), ["x:new"]);
    }
}
