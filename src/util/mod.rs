//! Helpers shared by features: splitting and sending long messages, finding and loading
//! media, drawing pictures, and pulling text out of messages. Nothing here knows about a
//! specific feature.

pub mod frames;
pub mod http;
pub mod media;
pub mod reply;
pub mod search;
pub mod split;
pub mod svg;
pub mod text;

use chrono::{DateTime, Datelike as _, Utc};

/// A key from `.env`, trimmed. `None` when it's missing or empty.
pub fn env_key(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

/// The start of this month in UTC, as a Unix timestamp. Voice and drawing count their
/// monthly limits from here.
pub fn month_start(now: DateTime<Utc>) -> i64 {
    now.date_naive()
        .with_day(1)
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .map(|start| start.and_utc().timestamp())
        .unwrap_or_else(|| now.timestamp())
}

/// Cuts `text` to at most `max` characters (not bytes), ending in "…" when cut.
pub fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// Like [`shorten`], but cuts at the last space before the limit, so no word is split.
pub fn shorten_at_word(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let short: String = text.chars().take(max.saturating_sub(1)).collect();
    let cut = short.rfind(char::is_whitespace).unwrap_or(short.len());
    let mut short = short[..cut].trim_end().to_string();
    short.push('…');
    short
}

/// "1 bet", "3 bets": a number with its noun, for messages people read.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_counts_characters() {
        assert_eq!(shorten("héllo", 5), "héllo");
        assert_eq!(shorten("héllo wörld", 5), "héll…");
    }

    #[test]
    fn month_starts_on_the_first() {
        let now = DateTime::parse_from_rfc3339("2026-10-10T21:40:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let start = DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(month_start(now), start);
    }
}
