//! Last words: a real message each tribute once sent, shown next to their name when the
//! cannons sound for them. They come from the same quote reader as who said it?, reading
//! the public channels' history at random moments while the first rounds are played.
//!
//! Someone who hardly writes may have no message in what was read: they fall without
//! last words.

use std::collections::HashMap;

use tracing::{info, warn};

use crate::core::BotCtx;
use crate::features::who_said_it::game::Quote;
use crate::features::who_said_it::sources::Source;
use crate::features::who_said_it::{Settings, quotes};

/// How many moments of history are read, 100 messages each: twice a whole-server who said
/// it?, since every tribute should get something.
const READS: usize = 20;
/// The longest last words, in characters.
const LONGEST: usize = 200;

/// One tribute's last words, and where they said them.
#[derive(Debug, Clone, PartialEq)]
pub struct LastWords {
    pub text: String,
    pub link: String,
}

/// Reads the history of `sources` and picks last words for each of `tributes` that wrote
/// something. Fails quietly: a read that doesn't work gives no last words.
pub async fn load(
    ctx: BotCtx,
    sources: Vec<Source>,
    settings: Settings,
    tributes: Vec<u64>,
) -> HashMap<u64, LastWords> {
    match quotes::load_reads(&ctx, &sources, &settings, READS).await {
        Ok((found, _)) => {
            let picked = pick(found, &tributes);
            info!(
                tributes = tributes.len(),
                with_words = picked.len(),
                "found Hunger Games last words"
            );
            picked
        }
        Err(err) => {
            warn!("couldn't read last words for the Hunger Games: {err:#}");
            HashMap::new()
        }
    }
}

/// The first short enough quote of each tribute (the quotes come shuffled), tidied to one
/// line that can't break out of a spoiler.
pub fn pick(quotes: Vec<Quote>, tributes: &[u64]) -> HashMap<u64, LastWords> {
    let mut picked = HashMap::new();
    for quote in quotes {
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
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(author: u64, text: &str) -> Quote {
        Quote {
            author,
            text: text.to_string(),
            posted: "March 2025".into(),
            place: None,
            link: format!("https://discord.com/channels/1/2/{author}"),
        }
    }

    #[test]
    fn each_tribute_gets_their_first_short_quote() {
        let quotes = vec![
            quote(1, &"long ".repeat(60)),
            quote(1, "see you\nall || later"),
            quote(1, "a later one"),
            quote(2, "not a tribute"),
        ];
        let picked = pick(quotes, &[1, 3]);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[&1].text, "see you all | later");
        assert!(picked[&1].link.ends_with("/1"));
    }
}
