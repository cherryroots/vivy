//! What a Gemini interaction costs, from its token counts, at Google's list prices.
//!
//! Prices: <https://ai.google.dev/gemini-api/docs/pricing> (checked 2026-10-14).
//! Gemini 4 Argon has a launch price until the end of November 2026 and costs twice as much
//! from December. Thinking tokens cost as much as output. Google Search costs $14 per 1000
//! searches, but the first 5000 a month are free; the estimate counts every search, so it
//! runs a little high when there are few. Google keeps no Admin API for the bill, so the
//! estimate is all there is.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

use crate::ai::spend::Used;

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Prices {
    input: f64,
    cache_read: f64,
    output: f64,
}

const fn prices(input: f64, cache_read: f64, output: f64) -> Prices {
    Prices {
        input,
        cache_read,
        output,
    }
}

/// Gemini 4 Argon until 2026-11-30, and from [`ARGON_PRICE_RISE`].
const ARGON_LAUNCH: Prices = prices(2.0, 0.10, 10.0);
const ARGON: Prices = prices(4.0, 0.20, 20.0);
const ARGON_PRICE_RISE: NaiveDate = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();

/// Each Google Search costs this, on top of the tokens its results add.
const GOOGLE_SEARCH: f64 = 14.0 / 1000.0;

fn per_token(count: u64, price: f64) -> f64 {
    count as f64 * price / 1_000_000.0
}

/// The prices of `model` on `now`'s date. A model this doesn't know is priced like Argon.
fn prices_for(_model: &str, now: DateTime<Utc>) -> Prices {
    if now.date_naive() < ARGON_PRICE_RISE {
        ARGON_LAUNCH
    } else {
        ARGON
    }
}

/// What one finished interaction cost, from its `usage` and the steps it took (for the
/// searches).
pub fn cost(model: &str, usage: &Value, steps: &[Value], now: DateTime<Utc>) -> Used {
    let get = |key: &str| usage[key].as_u64().unwrap_or(0);
    // Pages read with URL context and the like count as input too.
    let input = get("total_input_tokens") + get("total_tool_use_tokens");
    let cached = get("total_cached_tokens").min(input);
    let output = get("total_output_tokens") + get("total_thought_tokens");
    let searches = steps
        .iter()
        .filter(|step| step["type"] == "google_search_call")
        .count();
    let p = prices_for(model, now);
    let usd = per_token(input - cached, p.input)
        + per_token(cached, p.cache_read)
        + per_token(output, p.output)
        + searches as f64 * GOOGLE_SEARCH;
    Used {
        usd,
        input: input - cached,
        cache_read: cached,
        cache_write: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prices_an_interaction() {
        let usage = json!({
            "total_input_tokens": 1_000_000,
            "total_cached_tokens": 400_000,
            "total_tool_use_tokens": 0,
            "total_output_tokens": 50_000,
            "total_thought_tokens": 50_000,
        });
        let steps = [
            json!({"type": "google_search_call"}),
            json!({"type": "thought"}),
        ];
        let october = "2026-10-14T12:00:00Z".parse().unwrap();
        let used = cost("gemini-4-argon", &usage, &steps, october);
        // 600k × $2 + 400k × $0.10 + 100k × $10, plus one search.
        assert!((used.usd - (1.2 + 0.04 + 1.0 + 0.014)).abs() < 1e-9);
        assert_eq!((used.input, used.cache_read), (600_000, 400_000));

        // Twice as much from December.
        let december = "2026-12-01T00:00:00Z".parse().unwrap();
        let used = cost("gemini-4-argon", &usage, &[], december);
        assert!((used.usd - (2.4 + 0.08 + 2.0)).abs() < 1e-9);
    }
}
