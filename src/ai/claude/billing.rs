//! What Anthropic actually billed this month, read with an Admin API key.
//!
//! The Cost API reports the whole organization's spend (tokens, web search, code
//! execution), in daily buckets, a few minutes behind. It needs an Admin API key
//! (`ANTHROPIC_ADMIN_KEY` in `.env`), which only organizations have. [`crate::ai::Spend`]
//! reads it once an hour and puts it in place of its own estimate.
//!
//! API reference: <https://platform.claude.com/docs/en/build-with-claude/usage-cost-api>

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::{DateTime, Datelike, TimeDelta, TimeZone, Utc};
use serde_json::Value;

use super::{API, VERSION};
use crate::ai::spend::Bill;

#[derive(Clone)]
pub struct Billing {
    http: reqwest::Client,
    key: String,
}

impl Billing {
    pub fn new(http: reqwest::Client, key: String) -> Billing {
        Billing { http, key }
    }
}

#[async_trait]
impl Bill for Billing {
    fn name(&self) -> &'static str {
        "Anthropic bill"
    }

    async fn this_month(&self, now: DateTime<Utc>) -> anyhow::Result<f64> {
        let (start, end) = month_so_far(now);
        let mut cents = 0.0;
        let mut page: Option<String> = None;
        // One page holds up to 31 daily buckets, so this is usually one request.
        loop {
            let mut query = vec![
                ("starting_at", start.clone()),
                ("ending_at", end.clone()),
                ("bucket_width", "1d".to_string()),
                ("limit", "31".to_string()),
            ];
            if let Some(page) = page.take() {
                query.push(("page", page));
            }
            let response = self
                .http
                .get(format!("{API}/organizations/cost_report"))
                .header("x-api-key", &self.key)
                .header("anthropic-version", VERSION)
                .query(&query)
                .send()
                .await
                .context("couldn't reach Anthropic")?;
            let status = response.status();
            let body: Value = response.json().await.unwrap_or_default();
            if !status.is_success() {
                let message = body["error"]["message"]
                    .as_str()
                    .unwrap_or("no reason given");
                anyhow::bail!("Anthropic's cost report answered {status}: {message}");
            }
            cents += total_cents(&body);
            match body["next_page"].as_str() {
                Some(next) if body["has_more"] == true => page = Some(next.to_string()),
                _ => return Ok(cents / 100.0),
            }
        }
    }
}

/// The days to ask for: from the month's first midnight to the next midnight, both UTC,
/// so today's bucket is in. Without `ending_at` Anthropic ends at today's midnight, which
/// on the 1st (UTC) is the start itself, and it answers "Invalid date range".
fn month_so_far(now: DateTime<Utc>) -> (String, String) {
    let midnight = |day: DateTime<Utc>| {
        Utc.with_ymd_and_hms(day.year(), day.month(), day.day(), 0, 0, 0)
            .unwrap()
    };
    let start = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .unwrap();
    let end = midnight(now) + TimeDelta::days(1);
    let text = |time: DateTime<Utc>| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    (text(start), text(end))
}

/// Adds up every amount in one page of the cost report. Amounts are decimal strings, in
/// cents.
fn total_cents(page: &Value) -> f64 {
    let buckets = page["data"].as_array().into_iter().flatten();
    buckets
        .flat_map(|bucket| bucket["results"].as_array().into_iter().flatten())
        .filter_map(|result| result["amount"].as_str()?.parse::<f64>().ok())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adds_up_a_page() {
        let page = json!({
            "data": [
                {"results": [{"amount": "1234.5", "currency": "USD"}, {"amount": "65.5"}]},
                {"results": [{"amount": "100"}]},
                {"results": []},
            ],
            "has_more": false,
        });
        assert_eq!(total_cents(&page), 1400.0);
        assert_eq!(total_cents(&json!({})), 0.0);
    }

    #[test]
    fn asks_for_the_month_up_to_tomorrow() {
        let at = |text: &str| DateTime::parse_from_rfc3339(text).unwrap().to_utc();
        let range = |start: &str, end: &str| (start.to_string(), end.to_string());
        // Early on the 1st: still a whole day, not an empty range.
        assert_eq!(
            month_so_far(at("2026-11-01T00:50:00Z")),
            range("2026-11-01T00:00:00Z", "2026-11-02T00:00:00Z")
        );
        assert_eq!(
            month_so_far(at("2026-11-17T13:05:00Z")),
            range("2026-11-01T00:00:00Z", "2026-11-18T00:00:00Z")
        );
        // The last day ends at the next month's first midnight.
        assert_eq!(
            month_so_far(at("2026-12-31T23:59:00Z")),
            range("2026-12-01T00:00:00Z", "2027-01-01T00:00:00Z")
        );
    }
}
