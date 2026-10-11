//! What OpenAI actually billed this month, read with an Admin API key.
//!
//! The Costs API reports the whole organization's spend (tokens, pictures, web search,
//! code interpreter containers), in daily buckets, so it runs behind by up to a day. It
//! needs an Admin key (`OPENAI_ADMIN_KEY` in `.env`); an ordinary API key is refused.
//! [`crate::ai::Spend`] reads it once an hour and puts it in place of its own estimate.
//!
//! API reference:
//! <https://developers.openai.com/api/reference/resources/admin/subresources/organization/subresources/usage/methods/costs>

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde_json::Value;

use crate::ai::api::error_message;
use crate::ai::spend::Bill;

const API: &str = "https://api.openai.com/v1";

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
        "OpenAI bill"
    }

    async fn this_month(&self, now: DateTime<Utc>) -> anyhow::Result<f64> {
        let start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .unwrap()
            .timestamp();
        let mut usd = 0.0;
        let mut page: Option<String> = None;
        // One page holds up to 31 daily buckets, so this is usually one request.
        loop {
            let mut query = vec![
                ("start_time", start.to_string()),
                ("bucket_width", "1d".to_string()),
                ("limit", "31".to_string()),
            ];
            if let Some(page) = page.take() {
                query.push(("page", page));
            }
            let response = self
                .http
                .get(format!("{API}/organization/costs"))
                .bearer_auth(&self.key)
                .query(&query)
                .send()
                .await
                .context("couldn't reach OpenAI")?;
            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                anyhow::bail!(
                    "OpenAI's costs report answered {status}: {}",
                    error_message(&text)
                );
            }
            let body: Value = response.json().await.context("OpenAI sent invalid JSON")?;
            usd += total_usd(&body);
            match body["next_page"].as_str() {
                Some(next) if body["has_more"] == true => page = Some(next.to_string()),
                _ => return Ok(usd),
            }
        }
    }
}

/// Adds up every amount in one page of the costs report. Amounts are in USD, as numbers
/// (or, to be safe, as decimal strings).
fn total_usd(page: &Value) -> f64 {
    let buckets = page["data"].as_array().into_iter().flatten();
    buckets
        .flat_map(|bucket| bucket["results"].as_array().into_iter().flatten())
        .filter_map(|result| {
            let value = &result["amount"]["value"];
            value.as_f64().or_else(|| value.as_str()?.parse().ok())
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adds_up_a_page() {
        let page = json!({
            "object": "page",
            "data": [
                {"object": "bucket", "results": [
                    {"object": "organization.costs.result", "amount": {"value": 1.25, "currency": "usd"}},
                    {"amount": {"value": 0.5, "currency": "usd"}},
                ]},
                {"results": [{"amount": {"value": "3.0", "currency": "usd"}}]},
                {"results": []},
            ],
            "has_more": false,
            "next_page": null,
        });
        assert_eq!(total_usd(&page), 4.75);
        assert_eq!(total_usd(&json!({})), 0.0);
    }
}
