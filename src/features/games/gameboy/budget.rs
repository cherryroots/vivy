//! What playing may use today: the free daily OpenAI tokens the strategist plans on, and
//! the dollars the paid models cost. Pure; the session keeps it in `saves_dir/today.json`.
//!
//! The free tokens (OpenAI's data-sharing tokens, 1M a day, reset at 00:00 UTC) don't show
//! on OpenAI's usage page until they run out, so code counts them itself from what each
//! answer reports. A request that crosses the limit is billed whole, so the strategist
//! stops when its next call could cross it, and Haiku plans from then on. High-effort calls
//! (reviews) stop earlier, at 90%, since one that crosses could cost about a dollar.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The tokens of a call before any call today has said how big they are.
const FIRST_GUESS: u64 = 20_000;
/// High-effort calls think more: guessed at this many times a routine one.
const HIGH_EFFORT_TIMES: u64 = 2;

/// One day's use, in UTC.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Today {
    /// "2026-10-20".
    pub day: String,
    /// Free tokens used, input and output.
    pub free_tokens: u64,
    /// The biggest routine strategist call today, in tokens: the guess for the next one.
    pub biggest_call: u64,
    /// USD the paid models cost while playing, at list prices.
    pub usd: f64,
}

impl Today {
    /// `saved` if it's from `now`'s day, or a fresh day.
    pub fn on(saved: Option<Today>, now: DateTime<Utc>) -> Today {
        let day = now.format("%Y-%m-%d").to_string();
        match saved {
            Some(today) if today.day == day => today,
            _ => Today {
                day,
                ..Today::default()
            },
        }
    }

    /// Whether the next strategist call fits in `limit` free tokens. `high` is a
    /// high-effort call, which stops at 90% of the limit.
    pub fn free_call_fits(&self, limit: u64, high: bool) -> bool {
        let guess = if self.biggest_call > 0 {
            self.biggest_call
        } else {
            FIRST_GUESS
        };
        let (guess, stop) = if high {
            (guess * HIGH_EFFORT_TIMES, limit - limit / 10)
        } else {
            (guess, limit)
        };
        self.free_tokens + guess <= stop
    }

    /// Counts a strategist call's tokens.
    pub fn used_free(&mut self, tokens: u64, high: bool) {
        self.free_tokens += tokens;
        if !high {
            self.biggest_call = self.biggest_call.max(tokens);
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, hour, 0, 0).unwrap()
    }

    #[test]
    fn a_new_day_starts_from_nothing() {
        let mut today = Today::on(None, at(20, 23));
        assert_eq!(today.day, "2026-10-20");
        today.used_free(5_000, false);
        today.usd = 0.4;
        let same = Today::on(Some(today.clone()), at(20, 1));
        assert_eq!(same, today);
        let next = Today::on(Some(today), at(21, 0));
        assert_eq!(
            (next.day.as_str(), next.free_tokens, next.usd),
            ("2026-10-21", 0, 0.0)
        );
    }

    #[test]
    fn calls_stop_before_they_could_cross_the_limit() {
        let limit = 1_000_000;
        let mut today = Today::on(None, at(20, 12));
        // Nothing known yet: the first guess.
        today.free_tokens = limit - FIRST_GUESS;
        assert!(today.free_call_fits(limit, false));
        today.free_tokens += 1;
        assert!(!today.free_call_fits(limit, false));

        // The biggest routine call so far is the guess.
        let mut today = Today::on(None, at(20, 12));
        today.used_free(30_000, false);
        today.used_free(10_000, false);
        assert_eq!(today.biggest_call, 30_000);
        today.free_tokens = limit - 30_000;
        assert!(today.free_call_fits(limit, false));
        // High effort stops at 900k and guesses twice as big.
        today.free_tokens = 840_000;
        assert!(today.free_call_fits(limit, true));
        today.free_tokens = 840_001;
        assert!(!today.free_call_fits(limit, true));
        // A review's size doesn't change the routine guess.
        today.used_free(80_000, true);
        assert_eq!(today.biggest_call, 30_000);
    }
}
