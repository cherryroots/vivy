//! Last words: a real message each tribute once sent, shown next to their name when the
//! cannons sound for them. They're found while the first rounds are played.
//!
//! For each tribute, Discord's message search (`util::search`) looks for their messages in
//! the public channels: one search to see how many there are, then a few pages at random
//! offsets until one has a message that makes good last words. Anyone the search finds
//! nothing for (it can fail while Discord indexes a server) gets random reads of channel
//! history with the who-said-it quote reader instead. Someone who hardly writes may still
//! fall without last words.

use std::collections::HashMap;

use chrono::{Duration, Utc};
use serenity::all::{GuildId, Message};
use serenity::futures::future::join_all;
use tracing::{debug, info, warn};

use crate::core::BotCtx;
use crate::features::games::who_said_it::game::{Person, Quote};
use crate::features::games::who_said_it::sources::Source;
use crate::features::games::who_said_it::{Settings, quotes};
use crate::util::search::{self, MAX_CHANNELS, MAX_LIMIT, MAX_OFFSET, Search};

/// How many random pages are searched per tribute, after the first search. Each is one
/// Discord call, no AI. It stops at the first page with good last words.
const PAGES_PER_TRIBUTE: usize = 3;
/// The most random reads (100 messages each) per tribute the search found nothing for.
const READS_PER_TRIBUTE: usize = 3;
/// How many tributes are searched, or reads made, at once.
const AT_ONCE: usize = 4;
/// The longest last words, in characters.
const LONGEST: usize = 200;

/// One tribute's last words, and where they said them.
#[derive(Debug, Clone, PartialEq)]
pub struct LastWords {
    pub text: String,
    pub link: String,
}

/// Finds last words for each of `tributes` in the history of `sources`. Fails quietly: a
/// search or read that doesn't work gives nothing.
pub async fn load(
    ctx: BotCtx,
    guild: GuildId,
    sources: Vec<Source>,
    settings: Settings,
    tributes: Vec<u64>,
) -> HashMap<u64, LastWords> {
    let mut picked = HashMap::new();
    for some in tributes.chunks(AT_ONCE) {
        let searches = some
            .iter()
            .map(|&tribute| search_for(&ctx, guild, &sources, &settings, tribute));
        for (tribute, words) in some.iter().zip(join_all(searches).await) {
            if let Some(words) = words {
                picked.insert(*tribute, words);
            }
        }
    }
    let searched = picked.len();
    let missing: Vec<u64> = tributes
        .iter()
        .copied()
        .filter(|t| !picked.contains_key(t) && !settings.exclude_users.contains(t))
        .collect();
    if !missing.is_empty() {
        read_for(&ctx, &sources, &settings, &missing, &mut picked).await;
    }
    info!(
        tributes = tributes.len(),
        searched,
        with_words = picked.len(),
        "found Hunger Games last words"
    );
    picked
}

/// One tribute's last words from Discord's search: a random message of theirs, from
/// `sources`, in the time who said it? reads (`lookback_days`, not the last hour).
async fn search_for(
    ctx: &BotCtx,
    guild: GuildId,
    sources: &[Source],
    settings: &Settings,
    tribute: u64,
) -> Option<LastWords> {
    if settings.exclude_users.contains(&tribute) {
        return None;
    }
    let now = Utc::now();
    let mut search = Search {
        channel_ids: sources
            .iter()
            .map(|source| source.channel.get())
            .take(MAX_CHANNELS)
            .collect(),
        author_ids: vec![tribute],
        author_types: vec!["user".into()],
        // Links are rarely funny without what they link to, and quotes leave them out.
        has: vec!["-link".into()],
        min_id: Some(search::snowflake_at(
            now - Duration::days(settings.lookback_days.max(1)),
        )),
        max_id: Some(search::snowflake_at(now - Duration::hours(1))),
        limit: Some(MAX_LIMIT),
        ..Default::default()
    };
    let run = |search: Search| async move {
        search::search(&ctx.web, ctx.http.token(), guild, &search)
            .await
            .inspect_err(|err| debug!("couldn't search {tribute}'s messages: {err:#}"))
            .ok()
    };
    // The newest page, and how many messages there are in all.
    let newest = run(search.clone()).await?;
    let furthest = newest.total.saturating_sub(1).min(MAX_OFFSET as u64) as u32;
    if furthest >= MAX_LIMIT {
        for _ in 0..PAGES_PER_TRIBUTE {
            search.offset = Some(fastrand::u32(0..=furthest));
            let Some(page) = run(search.clone()).await else {
                break;
            };
            if let Some(words) = choose(&page.messages, settings, tribute) {
                return Some(words);
            }
        }
    }
    choose(&newest.messages, settings, tribute)
}

/// A random one of `tribute`'s `messages` that makes good last words.
fn choose(messages: &[Message], settings: &Settings, tribute: u64) -> Option<LastWords> {
    let good: Vec<LastWords> = messages
        .iter()
        .filter(|msg| msg.author.id.get() == tribute)
        .filter_map(|msg| {
            let text = tidy(&quotes::quote_text(msg, settings)?)?;
            Some(LastWords {
                text,
                link: msg.link(),
            })
        })
        .collect();
    fastrand::choice(good)
}

/// Random reads of channel history, until each of `missing` has last words or the reads
/// run out.
async fn read_for(
    ctx: &BotCtx,
    sources: &[Source],
    settings: &Settings,
    missing: &[u64],
    picked: &mut HashMap<u64, LastWords>,
) {
    let most = missing.len() * READS_PER_TRIBUTE;
    let mut reads = 0;
    while reads < most && missing.iter().any(|t| !picked.contains_key(t)) {
        let batch = AT_ONCE.min(most - reads);
        reads += batch;
        let results = join_all((0..batch).map(|_| quotes::read_once(ctx, sources, settings))).await;
        let mut worked = false;
        for result in results {
            match result {
                Ok(found) => {
                    worked = true;
                    pick(picked, found, missing);
                }
                Err(err) => warn!("couldn't read last words for the Hunger Games: {err}"),
            }
        }
        // Every read in the batch failed: the channels can't be read, so stop trying.
        if !worked {
            break;
        }
    }
}

/// Adds last words for each of `tributes` who has none yet: a random one of their short
/// enough quotes in `found` (one read).
pub fn pick(
    picked: &mut HashMap<u64, LastWords>,
    mut found: Vec<(Quote, Person)>,
    tributes: &[u64],
) {
    fastrand::shuffle(&mut found);
    for (quote, _) in found {
        if !tributes.contains(&quote.author) || picked.contains_key(&quote.author) {
            continue;
        }
        if let Some(text) = tidy(&quote.text) {
            picked.insert(
                quote.author,
                LastWords {
                    text,
                    link: quote.link,
                },
            );
        }
    }
}

/// A quote as last words: on one line, unable to hide the rest of the line behind a spoiler,
/// and short enough.
fn tidy(text: &str) -> Option<String> {
    let text = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("||", "|");
    (!text.is_empty() && text.chars().count() <= LONGEST).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(author: u64, text: &str) -> (Quote, Person) {
        let quote = Quote {
            author,
            text: text.to_string(),
            posted: "March 2025".into(),
            place: None,
            link: format!("https://discord.com/channels/1/2/{author}"),
        };
        let person = Person {
            id: author,
            name: format!("person {author}"),
        };
        (quote, person)
    }

    #[test]
    fn each_tribute_gets_one_short_quote() {
        let found = vec![
            quote(1, &"long ".repeat(60)),
            quote(1, "see you\nall || later"),
            quote(2, "not a tribute"),
        ];
        let mut picked = HashMap::new();
        pick(&mut picked, found, &[1, 3]);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[&1].text, "see you all | later");
        assert!(picked[&1].link.ends_with("/1"));
    }

    #[test]
    fn last_words_are_one_short_line() {
        assert_eq!(tidy("a\n b  || c").as_deref(), Some("a b | c"));
        assert_eq!(tidy("   "), None);
        assert_eq!(tidy(&"x".repeat(LONGEST + 1)), None);
    }

    #[test]
    fn later_reads_only_fill_in_whoever_has_none() {
        let mut picked = HashMap::new();
        pick(&mut picked, vec![quote(1, "first read")], &[1, 3]);
        pick(
            &mut picked,
            vec![quote(1, "second read"), quote(3, "finally here")],
            &[1, 3],
        );
        assert_eq!(picked[&1].text, "first read");
        assert_eq!(picked[&3].text, "finally here");
    }
}
