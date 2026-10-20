//! Discord's message search (`GET /guilds/{id}/messages/search`), the same search as the
//! search bar, with every filter the API has. The chat tool `search_messages` and the
//! Hunger Games' last words both use it.
//!
//! Twilight has no call for this endpoint yet, so it goes through `reqwest` with the bot
//! token. Discord answers 202 while it is still indexing a server, and 429 when searches
//! come too fast; both say how long to wait, and the search waits and tries again a few
//! times.
//!
//! The search covers every channel the bot can read. Whoever shows the results must keep
//! them to what the people seeing them may read.

use std::time::Duration;

use super::discord::{Channel, GuildId, Message};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde_json::Value;
use tracing::{debug, warn};

/// Most results one page has (Discord's limit).
pub const MAX_LIMIT: u32 = 25;
/// The furthest `offset` Discord allows.
pub const MAX_OFFSET: u32 = 9975;
/// How many channels, authors, mentions and so on one filter can list.
pub const MAX_IDS: usize = 100;
/// How many channels `channel_ids` can list.
pub const MAX_CHANNELS: usize = 500;
/// Who wrote a message. Any can be negated with a leading `-` ("-bot").
pub const AUTHOR_TYPES: &[&str] = &["user", "bot", "webhook"];
/// What a message has. Any can be negated with a leading `-` ("-link").
pub const HAS_TYPES: &[&str] = &[
    "image", "sound", "video", "file", "sticker", "embed", "link", "poll", "snapshot",
];
/// The kinds of embeds. Messages from before February 24, 2026 may not be found by `gif`.
pub const EMBED_TYPES: &[&str] = &["image", "video", "gif", "sound", "article"];
/// How many times a search waits for Discord (indexing, or too many searches) before it
/// gives up.
const TRIES: usize = 4;
/// The longest wait between tries.
const LONGEST_WAIT: f64 = 10.0;

/// One search: every filter of the API. Empty lists and `None` leave a filter out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Search {
    /// Words in the message text (at most 1024 characters).
    pub content: Option<String>,
    /// How many other words may sit between the words of `content` (at most 100,
    /// Discord's default 2).
    pub slop: Option<u32>,
    /// Only these channels (at most 500).
    pub channel_ids: Vec<u64>,
    /// "user", "bot", "webhook", or one of them negated ("-bot").
    pub author_types: Vec<String>,
    /// Only messages by these people.
    pub author_ids: Vec<u64>,
    /// Only messages mentioning these people.
    pub mentions: Vec<u64>,
    /// Only messages mentioning these roles.
    pub mentions_role_ids: Vec<u64>,
    /// Whether the message mentions @everyone.
    pub mention_everyone: Option<bool>,
    /// Only replies to these people.
    pub replied_to_user_ids: Vec<u64>,
    /// Only replies to these messages.
    pub replied_to_message_ids: Vec<u64>,
    /// Whether the message is pinned.
    pub pinned: Option<bool>,
    /// What the message has, see [`HAS_TYPES`] (negate with "-").
    pub has: Vec<String>,
    /// The kinds of embeds, see [`EMBED_TYPES`].
    pub embed_types: Vec<String>,
    /// Who made the embed, like "Tenor" (case matters).
    pub embed_providers: Vec<String>,
    /// The sites of links in the message, like "youtube.com".
    pub link_hostnames: Vec<String>,
    /// Attachment file names.
    pub attachment_filenames: Vec<String>,
    /// Attachment extensions, like "png".
    pub attachment_extensions: Vec<String>,
    /// Only messages after this message ID (see [`snowflake_at`] for a time).
    pub min_id: Option<u64>,
    /// Only messages before this message ID.
    pub max_id: Option<u64>,
    /// Newest first (the default), oldest first, or best match first.
    pub sort: Sort,
    /// How many results, 1 to 25 (Discord's default 25).
    pub limit: Option<u32>,
    /// How many results to skip, for the next pages (at most 9975).
    pub offset: Option<u32>,
    /// Also search age-restricted channels.
    pub include_nsfw: bool,
}

/// The order of the results.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Sort {
    #[default]
    Newest,
    Oldest,
    Relevance,
}

/// What a search found.
#[derive(Debug, Default)]
pub struct Found {
    /// How many messages match in all (may be a little off while messages come and go).
    pub total: u64,
    /// This page of them. Can be a few short of the limit for old messages, so don't page
    /// by its length.
    pub messages: Vec<Message>,
    /// The threads the messages are in.
    pub threads: Vec<Channel>,
    /// Whether Discord is still indexing the server's older messages, so older results
    /// may be missing.
    pub indexing: bool,
}

impl Search {
    /// The query string, or what's wrong with the search, in words for the model.
    pub fn query(&self) -> Result<Vec<(&'static str, String)>, String> {
        let mut pairs: Vec<(&'static str, String)> = Vec::new();
        let ids = |pairs: &mut Vec<_>, key, ids: &[u64], most: usize| {
            if ids.len() > most {
                return Err(format!("`{key}` can list at most {most}."));
            }
            pairs.extend(ids.iter().map(|id| (key, id.to_string())));
            Ok(())
        };
        let texts = |pairs: &mut Vec<_>, key, texts: &[String], longest: usize| {
            if texts.len() > MAX_IDS {
                return Err(format!("`{key}` can list at most {MAX_IDS}."));
            }
            for text in texts {
                if text.is_empty() || text.chars().count() > longest {
                    return Err(format!("A `{key}` must be 1 to {longest} characters."));
                }
                pairs.push((key, text.clone()));
            }
            Ok(())
        };
        let kinds = |pairs: &mut Vec<_>, key, kinds: &[String], known: &[&str], negate| {
            for kind in kinds {
                let bare = if negate {
                    kind.strip_prefix('-').unwrap_or(kind)
                } else {
                    kind
                };
                if !known.contains(&bare) {
                    return Err(format!("`{key}` can't be {kind}."));
                }
                pairs.push((key, kind.clone()));
            }
            Ok(())
        };

        if let Some(content) = &self.content {
            if content.chars().count() > 1024 {
                return Err("`content` can be at most 1024 characters.".into());
            }
            pairs.push(("content", content.clone()));
        }
        if let Some(slop) = self.slop {
            if slop > 100 {
                return Err("`slop` can be at most 100.".into());
            }
            pairs.push(("slop", slop.to_string()));
        }
        ids(&mut pairs, "channel_id", &self.channel_ids, MAX_CHANNELS)?;
        kinds(
            &mut pairs,
            "author_type",
            &self.author_types,
            AUTHOR_TYPES,
            true,
        )?;
        ids(&mut pairs, "author_id", &self.author_ids, MAX_IDS)?;
        ids(&mut pairs, "mentions", &self.mentions, MAX_IDS)?;
        ids(
            &mut pairs,
            "mentions_role_id",
            &self.mentions_role_ids,
            MAX_IDS,
        )?;
        if let Some(everyone) = self.mention_everyone {
            pairs.push(("mention_everyone", everyone.to_string()));
        }
        ids(
            &mut pairs,
            "replied_to_user_id",
            &self.replied_to_user_ids,
            MAX_IDS,
        )?;
        ids(
            &mut pairs,
            "replied_to_message_id",
            &self.replied_to_message_ids,
            MAX_IDS,
        )?;
        if let Some(pinned) = self.pinned {
            pairs.push(("pinned", pinned.to_string()));
        }
        kinds(&mut pairs, "has", &self.has, HAS_TYPES, true)?;
        kinds(
            &mut pairs,
            "embed_type",
            &self.embed_types,
            EMBED_TYPES,
            false,
        )?;
        texts(&mut pairs, "embed_provider", &self.embed_providers, 256)?;
        texts(&mut pairs, "link_hostname", &self.link_hostnames, 256)?;
        texts(
            &mut pairs,
            "attachment_filename",
            &self.attachment_filenames,
            1024,
        )?;
        texts(
            &mut pairs,
            "attachment_extension",
            &self.attachment_extensions,
            256,
        )?;
        if let Some(min) = self.min_id {
            pairs.push(("min_id", min.to_string()));
        }
        if let Some(max) = self.max_id {
            pairs.push(("max_id", max.to_string()));
        }
        match self.sort {
            Sort::Newest => {}
            Sort::Oldest => pairs.push(("sort_order", "asc".into())),
            Sort::Relevance => pairs.push(("sort_by", "relevance".into())),
        }
        if let Some(limit) = self.limit {
            pairs.push(("limit", limit.clamp(1, MAX_LIMIT).to_string()));
        }
        if let Some(offset) = self.offset {
            if offset > MAX_OFFSET {
                return Err(format!("`offset` can be at most {MAX_OFFSET}."));
            }
            pairs.push(("offset", offset.to_string()));
        }
        if self.include_nsfw {
            pairs.push(("include_nsfw", "true".into()));
        }
        Ok(pairs)
    }
}

/// Runs `search` in `guild`, with the bot's `token` (`ctx.http.token()`). A search that
/// isn't valid fails with the reason from [`Search::query`].
pub async fn search(
    web: &reqwest::Client,
    token: &str,
    guild: GuildId,
    search: &Search,
) -> anyhow::Result<Found> {
    let pairs = search.query().map_err(anyhow::Error::msg)?;
    let url = format!("https://discord.com/api/v10/guilds/{guild}/messages/search");
    for _ in 0..TRIES {
        let response = web
            .get(&url)
            .header("Authorization", token)
            .query(&pairs)
            .send()
            .await?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or_default();
        // 202: still indexing the server. 429: too many searches. Both say how long to wait.
        if status == StatusCode::ACCEPTED || status == StatusCode::TOO_MANY_REQUESTS {
            let seconds = body["retry_after"].as_f64().unwrap_or(2.0);
            let seconds = if seconds <= 0.0 { 1.0 } else { seconds };
            debug!("message search waits {seconds:.1} s ({status})");
            tokio::time::sleep(Duration::from_secs_f64(seconds.min(LONGEST_WAIT))).await;
            continue;
        }
        if !status.is_success() {
            let reason = body["message"].as_str().unwrap_or("no reason given");
            anyhow::bail!("Discord search failed ({status}): {reason}");
        }
        return Ok(read(&body));
    }
    anyhow::bail!(
        "Discord is still indexing this server for search, or searches came too fast. Try again in a minute."
    )
}

/// The results from Discord's answer.
fn read(body: &Value) -> Found {
    let messages = body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        // Each hit is a list holding the message (it used to have context around it).
        .filter_map(|hit| match hit {
            Value::Array(list) => list.first().cloned(),
            message => Some(message.clone()),
        })
        .filter_map(|message| match serde_json::from_value(message) {
            Ok(message) => Some(message),
            Err(err) => {
                warn!("couldn't read a search result: {err}");
                None
            }
        })
        .collect();
    let threads = body["threads"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|thread| serde_json::from_value(thread.clone()).ok())
        .collect();
    Found {
        total: body["total_results"].as_u64().unwrap_or(0),
        messages,
        threads,
        indexing: body["doing_deep_historical_index"]
            .as_bool()
            .unwrap_or(false),
    }
}

/// The smallest message ID a message sent at `at` could have, for `min_id` and `max_id`.
pub fn snowflake_at(at: DateTime<Utc>) -> u64 {
    // Discord IDs count milliseconds since 2015 in their top bits.
    const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;
    let ms = (at.timestamp_millis() - DISCORD_EPOCH_MS).max(0);
    (ms as u64) << 22
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pairs(search: &Search) -> Vec<(&'static str, String)> {
        search.query().unwrap()
    }

    #[test]
    fn every_filter_goes_in_the_query() {
        let search = Search {
            content: Some("movie night".into()),
            slop: Some(3),
            channel_ids: vec![1, 2],
            author_types: vec!["-bot".into()],
            author_ids: vec![7],
            mentions: vec![8],
            mentions_role_ids: vec![9],
            mention_everyone: Some(false),
            replied_to_user_ids: vec![10],
            replied_to_message_ids: vec![11],
            pinned: Some(true),
            has: vec!["image".into(), "-link".into()],
            embed_types: vec!["gif".into()],
            embed_providers: vec!["Tenor".into()],
            link_hostnames: vec!["youtube.com".into()],
            attachment_filenames: vec!["notes.txt".into()],
            attachment_extensions: vec!["png".into()],
            min_id: Some(100),
            max_id: Some(200),
            sort: Sort::Oldest,
            limit: Some(50),
            offset: Some(25),
            include_nsfw: true,
        };
        let pairs = pairs(&search);
        let get = |key| -> Vec<&str> {
            pairs
                .iter()
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| v.as_str())
                .collect()
        };
        assert_eq!(get("content"), ["movie night"]);
        assert_eq!(get("slop"), ["3"]);
        assert_eq!(get("channel_id"), ["1", "2"]);
        assert_eq!(get("author_type"), ["-bot"]);
        assert_eq!(get("author_id"), ["7"]);
        assert_eq!(get("mentions"), ["8"]);
        assert_eq!(get("mentions_role_id"), ["9"]);
        assert_eq!(get("mention_everyone"), ["false"]);
        assert_eq!(get("replied_to_user_id"), ["10"]);
        assert_eq!(get("replied_to_message_id"), ["11"]);
        assert_eq!(get("pinned"), ["true"]);
        assert_eq!(get("has"), ["image", "-link"]);
        assert_eq!(get("embed_type"), ["gif"]);
        assert_eq!(get("embed_provider"), ["Tenor"]);
        assert_eq!(get("link_hostname"), ["youtube.com"]);
        assert_eq!(get("attachment_filename"), ["notes.txt"]);
        assert_eq!(get("attachment_extension"), ["png"]);
        assert_eq!(get("min_id"), ["100"]);
        assert_eq!(get("max_id"), ["200"]);
        assert_eq!(get("sort_order"), ["asc"]);
        // Clamped to Discord's 25.
        assert_eq!(get("limit"), ["25"]);
        assert_eq!(get("offset"), ["25"]);
        assert_eq!(get("include_nsfw"), ["true"]);
    }

    #[test]
    fn an_empty_search_has_an_empty_query() {
        assert!(pairs(&Search::default()).is_empty());
        let relevance = Search {
            sort: Sort::Relevance,
            ..Default::default()
        };
        assert_eq!(pairs(&relevance), [("sort_by", "relevance".to_string())]);
    }

    #[test]
    fn wrong_values_are_explained() {
        let has = Search {
            has: vec!["gif".into()],
            ..Default::default()
        };
        assert!(has.query().unwrap_err().contains("has"));
        // Embed types can't be negated.
        let embed = Search {
            embed_types: vec!["-gif".into()],
            ..Default::default()
        };
        assert!(embed.query().is_err());
        let offset = Search {
            offset: Some(MAX_OFFSET + 1),
            ..Default::default()
        };
        assert!(offset.query().is_err());
        let authors = Search {
            author_ids: vec![1; MAX_IDS + 1],
            ..Default::default()
        };
        assert!(authors.query().is_err());
        let slop = Search {
            slop: Some(101),
            ..Default::default()
        };
        assert!(slop.query().is_err());
    }

    #[test]
    fn reads_the_answer() {
        let message = json!({
            "id": "5", "channel_id": "6", "content": "hi there",
            "author": {"id": "7", "username": "moss", "discriminator": "0", "avatar": null},
            "timestamp": "2026-10-15T20:00:00+00:00", "edited_timestamp": null,
            "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [],
            "attachments": [], "embeds": [], "pinned": false, "type": 0
        });
        let found = read(&json!({
            "total_results": 42,
            "messages": [[message]],
            "doing_deep_historical_index": true
        }));
        assert_eq!(found.total, 42);
        assert_eq!(found.messages.len(), 1);
        assert_eq!(found.messages[0].content, "hi there");
        assert!(found.indexing);
        assert!(found.threads.is_empty());
    }

    #[test]
    fn times_become_message_ids() {
        let epoch = DateTime::from_timestamp_millis(1_420_070_400_000).unwrap();
        assert_eq!(snowflake_at(epoch), 0);
        let day_later = DateTime::from_timestamp_millis(1_420_070_400_000 + 86_400_000).unwrap();
        assert_eq!(snowflake_at(day_later), 86_400_000 << 22);
    }
}
