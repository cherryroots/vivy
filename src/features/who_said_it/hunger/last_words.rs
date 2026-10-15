//! Last words: a real message each tribute once sent, shown next to their name when the
//! cannons sound for them. They come from the same quote reader as who said it?, reading
//! the public channels' history at random moments while the first rounds are played.
//!
//! Reading goes on until every tribute has last words, up to [`READS_PER_TRIBUTE`] reads
//! for each tribute. Someone who hardly writes may still have no message in what was read:
//! they fall without last words.

use std::collections::HashMap;

use serenity::futures::future::join_all;
use tracing::{info, warn};

use crate::core::BotCtx;
use crate::features::who_said_it::game::{Person, Quote};
use crate::features::who_said_it::sources::Source;
use crate::features::who_said_it::{Settings, quotes};

/// The most reads (100 messages each, one Discord call, no AI) per tribute. Reading
/// stops sooner once everyone has last words.
const READS_PER_TRIBUTE: usize = 5;
/// How many reads go at once.
const AT_ONCE: usize = 4;
/// The longest last words, in characters.
const LONGEST: usize = 200;

/// One tribute's last words, and where they said them.
#[derive(Debug, Clone, PartialEq)]
pub struct LastWords {
    pub text: String,
    pub link: String,
}

/// Reads the history of `sources` at random moments until each of `tributes` has last
/// words, or the reads run out. Fails quietly: a read that doesn't work gives nothing.
pub async fn load(
    ctx: BotCtx,
    sources: Vec<Source>,
    settings: Settings,
    tributes: Vec<u64>,
) -> HashMap<u64, LastWords> {
    let most = tributes.len() * READS_PER_TRIBUTE;
    let mut picked = HashMap::new();
    let mut reads = 0;
    while reads < most && picked.len() < tributes.len() {
        let batch = AT_ONCE.min(most - reads);
        reads += batch;
        let results =
            join_all((0..batch).map(|_| quotes::read_once(&ctx, &sources, &settings))).await;
        let mut worked = false;
        for result in results {
            match result {
                Ok(found) => {
                    worked = true;
                    pick(&mut picked, found, &tributes);
                }
                Err(err) => warn!("couldn't read last words for the Hunger Games: {err}"),
            }
        }
        // Every read in the batch failed: the channels can't be read, so stop trying.
        if !worked {
            break;
        }
    }
    info!(
        tributes = tributes.len(),
        with_words = picked.len(),
        reads,
        "found Hunger Games last words"
    );
    picked
}

/// Adds last words for each of `tributes` who has none yet: a random one of their short
/// enough quotes in `found` (one read), tidied to one line that can't break out of a
/// spoiler.
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
        let text = quote
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace("||", "|");
        if text.is_empty() || text.chars().count() > LONGEST {
            continue;
        }
        picked.insert(
            quote.author,
            LastWords {
                text,
                link: quote.link,
            },
        );
    }
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
