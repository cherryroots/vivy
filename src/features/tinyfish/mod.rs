//! TinyFish: Vivy searches the web and reads pages with TinyFish's APIs.
//!
//! Chat gets two tools, which work with every chat provider, next to their own built-in web
//! tools. TinyFish reaches sites the providers' tools often can't, like Reddit.
//!
//! - `search.rs`: `tinyfish_search`, ranked results for a query
//! - `fetch.rs`: `tinyfish_fetch`, whole pages as markdown
//!
//! The key goes in .env as `TINYFISH_API_KEY`. Without it the tools aren't offered.

mod fetch;
mod search;

use anyhow::Context as _;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Feature, Result, user_error};
use crate::util::{env_key, shorten};

const KEY_VAR: &str = "TINYFISH_API_KEY";
/// Most bytes read from a single TinyFish response (2 MiB).
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// `[features.tinyfish]` settings.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// How many results she reads per search, 1 to 10 (a page has 10).
    pub max_results: usize,
    /// A country code like "US" for where results come from, when she doesn't name one.
    /// Left out, TinyFish picks it from the language (or "US").
    pub location: Option<String>,
    /// A language code like "en", when she doesn't name one. Left out, TinyFish picks it
    /// from the location (or "en").
    pub language: Option<String>,
    /// The most characters she reads of one fetched page; the rest is cut off.
    pub max_page_characters: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_results: 8,
            location: None,
            language: None,
            max_page_characters: 15_000,
        }
    }
}

/// The TinyFish key from .env, if it's set.
fn key() -> Option<String> {
    env_key(KEY_VAR)
}

/// Fails when a response advertises more bytes than `max_bytes`.
fn check_content_length(len: Option<u64>, max_bytes: usize) -> Result<()> {
    if let Some(len) = len
        && len > max_bytes as u64
    {
        anyhow::bail!("TinyFish response is too large ({len} bytes, limit is {max_bytes})");
    }
    Ok(())
}

/// Appends `chunk` to `body`, failing if that would exceed `max_bytes`.
fn append_bounded(body: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> Result<()> {
    if body.len().saturating_add(chunk.len()) > max_bytes {
        anyhow::bail!("TinyFish response exceeded {max_bytes} bytes");
    }
    body.extend_from_slice(chunk);
    Ok(())
}

/// Reads a response body up to `max_bytes`, propagating any transport or UTF-8 errors.
async fn read_bounded(mut response: reqwest::Response, max_bytes: usize) -> Result<String> {
    check_content_length(response.content_length(), max_bytes)?;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("reading TinyFish response")?
    {
        append_bounded(&mut body, &chunk, max_bytes)?;
    }
    String::from_utf8(body).context("TinyFish response was not valid UTF-8")
}

/// Sends a request to a TinyFish API and reads its JSON answer. An error answer becomes a
/// message for the model: 400 is a bad argument she can fix; 402 and 429 mean the day's or
/// minute's requests are used up. Either way she reads why and can answer without it.
async fn read_answer(request: reqwest::RequestBuilder) -> Result<Value> {
    let response = request
        .send()
        .await
        .context("sending request to TinyFish")?;
    let status = response.status();
    let text = read_bounded(response, MAX_RESPONSE_BYTES).await?;
    if !status.is_success() {
        return Err(user_error(format!(
            "TinyFish answered {status}: {}",
            shorten(text.trim(), 300)
        )));
    }
    serde_json::from_str(&text).context("reading TinyFish JSON")
}

pub struct TinyFish;

#[async_trait]
impl Feature for TinyFish {
    fn name(&self) -> &'static str {
        "tinyfish"
    }

    fn tools(&self) -> Vec<ToolDef> {
        if key().is_none() {
            return Vec::new();
        }
        vec![search::def(), fetch::def()]
    }

    fn tool_status(&self, tool: &str, _args: &Value) -> Option<&'static str> {
        Some(match tool {
            fetch::TOOL => "🐟 Reading the page…",
            _ => "🐟 Searching the web…",
        })
    }

    async fn run_tool(
        &self,
        ctx: &BotCtx,
        _asker: &Asker,
        name: &str,
        args: &Value,
    ) -> Result<String> {
        let key = key().ok_or_else(|| user_error("TinyFish isn't set up"))?;
        let settings: Settings = ctx.config.feature("tinyfish")?;
        match name {
            search::TOOL => search::run(ctx, &key, &settings, args).await,
            fetch::TOOL => fetch::run(ctx, &key, &settings, args).await,
            _ => anyhow::bail!("tinyfish has no tool named {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_cap_rejects_oversized_responses() {
        assert!(check_content_length(None, 16).is_ok());
        assert!(check_content_length(Some(16), 16).is_ok());
        let err = check_content_length(Some(17), 16).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn append_bounded_caps_streamed_response_size() {
        let mut body = Vec::new();
        append_bounded(&mut body, b"hello ", 11).unwrap();
        append_bounded(&mut body, b"world", 11).unwrap();
        assert_eq!(body, b"hello world");

        let err = append_bounded(&mut body, b"!", 11).unwrap_err();
        assert!(err.to_string().contains("exceeded 11 bytes"));
        assert_eq!(body, b"hello world");
    }
}
