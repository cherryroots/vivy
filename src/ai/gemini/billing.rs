//! What Google Cloud actually billed this month, read from the Cloud Billing export.
//!
//! Gemini API keys can't read the bill, and Google has no Admin key like Anthropic's or
//! OpenAI's. What it has is the Cloud Billing export: Google writes every cost of the
//! billing account into a BigQuery table, a few hours behind. The bot reads that table with
//! a service account (a JSON key file, `GEMINI_BILLING_KEY_FILE` in `.env`), and
//! [`crate::ai::Spend`] puts the month's total in place of its own estimate once an hour.
//!
//! The total is `cost`, the list price before credits, so it matches the estimate and can
//! be compared with the monthly Google Cloud credit (`monthly_budget`). It is the whole
//! billing account, like Anthropic's and OpenAI's bills are the whole organization.
//!
//! Each read is two requests:
//! 1. A token: the bot signs a short JWT with the key file's private key (RS256) and trades
//!    it at Google's token URL for an access token that can read BigQuery.
//! 2. The query: `SELECT SUM(cost)` over this month's rows (`invoice.month`, like `202611`).
//!
//! Setup: <https://cloud.google.com/billing/docs/how-to/export-data-bigquery>,
//! the table's columns: <https://cloud.google.com/billing/docs/how-to/export-data-bigquery-tables/standard-usage>,
//! service account tokens: <https://developers.google.com/identity/protocols/oauth2/service-account#httprest>,
//! queries: <https://cloud.google.com/bigquery/docs/reference/rest/v2/jobs/query>

use anyhow::Context as _;
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::ai::api::error_message;
use crate::ai::spend::Bill;

const BIGQUERY: &str = "https://bigquery.googleapis.com/bigquery/v2";
/// Lets the token read BigQuery and run queries, nothing else.
const SCOPE: &str = "https://www.googleapis.com/auth/bigquery.readonly";

/// The parts of a service account's JSON key file the bot uses.
#[derive(Deserialize)]
struct KeyFile {
    client_email: String,
    /// PEM, "-----BEGIN PRIVATE KEY-----" (PKCS#8).
    private_key: String,
    /// The project the query runs in (and is billed to: BigQuery's free tier covers it).
    project_id: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

pub struct Billing {
    http: reqwest::Client,
    email: String,
    key: RsaKeyPair,
    project: String,
    token_uri: String,
    /// `project.dataset.table` of the export.
    table: String,
}

impl Billing {
    /// Reads the key file at `path`. `table` is `billing_table` under `[ai.gemini]`.
    pub fn new(http: reqwest::Client, path: &str, table: &str) -> anyhow::Result<Billing> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("couldn't read {path}"))?;
        let file: KeyFile = serde_json::from_str(&text)
            .with_context(|| format!("{path} isn't a service account key file"))?;
        let key = private_key(&file.private_key)?;
        anyhow::ensure!(
            is_table_name(table),
            "billing_table under [ai.gemini] should look like project.dataset.table, not {table:?}"
        );
        Ok(Billing {
            http,
            email: file.client_email,
            key,
            project: file.project_id,
            token_uri: file.token_uri,
            table: table.to_string(),
        })
    }

    /// Trades a signed JWT for an access token (valid an hour, so one per read).
    async fn access_token(&self, now: DateTime<Utc>) -> anyhow::Result<String> {
        let jwt = self.signed_jwt(now)?;
        let response = self
            .http
            .post(&self.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", jwt.as_str()),
            ])
            .send()
            .await
            .context("couldn't reach Google's token URL")?;
        let body = answer(response, "Google's token URL").await?;
        body["access_token"]
            .as_str()
            .map(str::to_string)
            .context("Google's token URL sent no access_token")
    }

    /// The JWT that asks for a token: header, claims and signature, each base64url.
    fn signed_jwt(&self, now: DateTime<Utc>) -> anyhow::Result<String> {
        let header = json!({"alg": "RS256", "typ": "JWT"});
        let claims = json!({
            "iss": self.email,
            "scope": SCOPE,
            "aud": self.token_uri,
            "iat": now.timestamp(),
            "exp": now.timestamp() + 3600,
        });
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                message.as_bytes(),
                &mut signature,
            )
            .map_err(|_| anyhow::anyhow!("couldn't sign the token request"))?;
        Ok(format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }
}

#[async_trait]
impl Bill for Billing {
    fn name(&self) -> &'static str {
        "Google Cloud bill"
    }

    async fn this_month(&self, now: DateTime<Utc>) -> anyhow::Result<f64> {
        let token = self.access_token(now).await?;
        let response = self
            .http
            .post(format!("{BIGQUERY}/projects/{}/queries", self.project))
            .bearer_auth(token)
            .json(&query(&self.table, now))
            .send()
            .await
            .context("couldn't reach BigQuery")?;
        let body = answer(response, "BigQuery").await?;
        total_usd(&body)
    }
}

/// The body of a successful answer, or what went wrong.
async fn answer(response: reqwest::Response, who: &str) -> anyhow::Result<Value> {
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("{who} answered {status}: {}", error_message(&text));
    }
    response
        .json()
        .await
        .with_context(|| format!("{who} sent invalid JSON"))
}

/// The key file's PEM private key as a key the bot can sign with.
fn private_key(pem: &str) -> anyhow::Result<RsaKeyPair> {
    let base64: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let der = STANDARD
        .decode(base64.trim())
        .context("the key file's private_key isn't valid PEM")?;
    RsaKeyPair::from_pkcs8(&der)
        .map_err(|why| anyhow::anyhow!("the key file's private_key isn't an RSA key: {why}"))
}

/// `project.dataset.table`, with only the characters those names can have, since the name
/// goes into the query as it is (a table name can't be a query parameter).
fn is_table_name(table: &str) -> bool {
    table.split('.').count() == 3
        && table.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

/// The request for `jobs.query`: this month's costs, added up.
fn query(table: &str, now: DateTime<Utc>) -> Value {
    json!({
        "query": format!("SELECT SUM(cost) FROM `{table}` WHERE invoice.month = @month"),
        "useLegacySql": false,
        "parameterMode": "NAMED",
        "queryParameters": [{
            "name": "month",
            "parameterType": {"type": "STRING"},
            "parameterValue": {"value": now.format("%Y%m").to_string()},
        }],
        "timeoutMs": 30_000,
    })
}

/// The one number the query answers. BigQuery sends it as a string, or null when the month
/// has no rows yet.
fn total_usd(body: &Value) -> anyhow::Result<f64> {
    anyhow::ensure!(
        body["jobComplete"] == true,
        "BigQuery didn't finish the query in time"
    );
    let value = &body["rows"][0]["f"][0]["v"];
    if value.is_null() {
        return Ok(0.0);
    }
    value
        .as_str()
        .and_then(|text| text.parse().ok())
        .with_context(|| format!("BigQuery sent {value} as the total"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn reads_the_total() {
        let body = json!({
            "kind": "bigquery#queryResponse",
            "schema": {"fields": [{"name": "f0_", "type": "FLOAT"}]},
            "rows": [{"f": [{"v": "12.5"}]}],
            "totalRows": "1",
            "jobComplete": true,
        });
        assert_eq!(total_usd(&body).unwrap(), 12.5);
        // A month with no rows yet adds up to null.
        let empty = json!({"rows": [{"f": [{"v": null}]}], "jobComplete": true});
        assert_eq!(total_usd(&empty).unwrap(), 0.0);
        assert!(total_usd(&json!({"jobComplete": false})).is_err());
    }

    #[test]
    fn checks_the_table_name() {
        assert!(is_table_name(
            "my-project.billing.gcp_billing_export_v1_0123AB_CDEF01_234567"
        ));
        assert!(!is_table_name("billing.gcp_billing_export_v1"));
        assert!(!is_table_name("p.d.t` WHERE true --"));
        assert!(!is_table_name("p..t"));
    }

    #[test]
    fn asks_for_this_month() {
        let now = Utc.with_ymd_and_hms(2026, 12, 1, 0, 30, 0).unwrap();
        let body = query("p.d.t", now);
        assert_eq!(
            body["queryParameters"][0]["parameterValue"]["value"],
            "202612"
        );
        assert!(body["query"].as_str().unwrap().contains("FROM `p.d.t`"));
    }

    #[test]
    fn refuses_a_bad_key() {
        assert!(
            private_key("-----BEGIN PRIVATE KEY-----\nbm9wZQ==\n-----END PRIVATE KEY-----")
                .is_err()
        );
        assert!(private_key("not pem at all!").is_err());
    }
}
