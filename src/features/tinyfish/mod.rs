//! TinyFish: Vivy searches the web with TinyFish's Search API.
//!
//! Chat gets one tool, `tinyfish_search`: she gives a query, the bot sends one GET request to
//! `https://api.search.tinyfish.ai`, and she reads back a numbered list of results (title,
//! site, date, link, snippet). It works with every chat provider, next to their own built-in
//! web search. Search is free up to 12,000 requests a day per key (30 a minute).
//! <https://docs.tinyfish.ai/search-api/reference>
//!
//! The key goes in .env as `TINYFISH_API_KEY`. Without it the tool isn't offered.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Feature, Result, user_error};
use crate::util::{env_key, shorten};

const KEY_VAR: &str = "TINYFISH_API_KEY";
const TOOL: &str = "tinyfish_search";
const API: &str = "https://api.search.tinyfish.ai";
/// Search usually answers in 1 to 3 seconds; TinyFish suggests a 10 second limit.
const TIMEOUT: Duration = Duration::from_secs(15);
/// The longest snippet she reads per result.
const MAX_SNIPPET: usize = 400;

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
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_results: 8,
            location: None,
            language: None,
        }
    }
}

/// The TinyFish key from .env, if it's set.
fn key() -> Option<String> {
    env_key(KEY_VAR)
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
        vec![ToolDef {
            name: TOOL,
            description: "Searches the web with TinyFish and returns ranked results: title, site, date, link and a snippet. Use it to look things up, check facts, or find news and pages to point people at. Snippets are short: to read a whole page, open its link with your web fetch tool if you have one. Include the links when you point people at pages.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to search for, like you'd type it into a search engine."},
                    "purpose": {"type": "string", "description": "Optional: why you're searching, in a sentence. Helps rank the results."},
                    "kind": {"type": "string", "enum": ["web", "news", "research_paper"], "description": "Default web. news gives articles with publisher and date; research_paper gives papers with authors and citations."},
                    "recency_minutes": {"type": "integer", "description": "Only results from the last this many minutes (60 = last hour, 1440 = last day). Not for research_paper."},
                    "include_domains": {"type": "string", "description": "Only these sites, comma-separated, like \"reddit.com,github.com\"."},
                    "exclude_domains": {"type": "string", "description": "Never these sites, comma-separated."},
                    "location": {"type": "string", "description": "Country code for where results come from, like \"US\", \"GB\", \"JP\"."},
                    "language": {"type": "string", "description": "Language code for the results, like \"en\", \"ja\"."},
                    "page": {"type": "integer", "description": "Which page of results, from 0 (the default) to 10."}
                },
                "required": ["query"]
            }),
        }]
    }

    fn tool_status(&self, _tool: &str, _args: &Value) -> Option<&'static str> {
        Some("🐟 Searching the web…")
    }

    async fn run_tool(
        &self,
        ctx: &BotCtx,
        _asker: &Asker,
        _name: &str,
        args: &Value,
    ) -> Result<String> {
        let key = key().ok_or_else(|| user_error("TinyFish search isn't set up"))?;
        let settings: Settings = ctx.config.feature("tinyfish")?;
        let query = search_query(&settings, args)?;
        let response = ctx
            .web
            .get(API)
            .query(&query)
            .header("X-API-Key", key)
            // The client has no overall timeout, and a stuck request would hold up the answer.
            .timeout(TIMEOUT)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            // 400 is a bad argument she can fix; 402 and 429 mean the day's or minute's
            // searches are used up. Either way she reads why and can answer without it.
            return Err(user_error(format!(
                "TinyFish answered {status}: {}",
                shorten(text.trim(), 300)
            )));
        }
        let body: Value = serde_json::from_str(&text)?;
        let results = body["results"].as_array().map_or(0, Vec::len);
        info!(results, "searched the web with TinyFish");
        Ok(format_results(&body, settings.max_results.clamp(1, 10)))
    }
}

/// The query string for one search: the tool's arguments, with `settings` filling in the
/// location and language she didn't give. Fails when there's no query.
fn search_query(settings: &Settings, args: &Value) -> Result<Vec<(&'static str, String)>> {
    let text = |key: &str| {
        args[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let query = text("query").ok_or_else(|| user_error("give a query to search for"))?;
    let mut params = vec![("query", query)];
    for (param, arg) in [
        ("purpose", "purpose"),
        ("domain_type", "kind"),
        ("include_domains", "include_domains"),
        ("exclude_domains", "exclude_domains"),
    ] {
        if let Some(value) = text(arg) {
            params.push((param, value));
        }
    }
    let location = text("location").or_else(|| settings.location.clone());
    let language = text("language").or_else(|| settings.language.clone());
    if let Some(location) = location {
        params.push(("location", location));
    }
    if let Some(language) = language {
        params.push(("language", language));
    }
    if let Some(minutes) = args["recency_minutes"].as_u64().filter(|m| *m > 0) {
        params.push(("recency_minutes", minutes.min(5_256_000).to_string()));
    }
    if let Some(page) = args["page"].as_u64() {
        params.push(("page", page.min(10).to_string()));
    }
    Ok(params)
}

/// TinyFish's answer as text for the model: the first `max` results, numbered, each with
/// its title, site and date, link, and snippet.
fn format_results(body: &Value, max: usize) -> String {
    let results = body["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        return "No results. Try other words, or fewer filters.".to_string();
    }
    let field = |result: &Value, key: &str| {
        result[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mut lines = Vec::new();
    for (i, result) in results.iter().take(max).enumerate() {
        let title = field(result, "title").unwrap_or_else(|| "(no title)".to_string());
        // Site, publisher (news) or venue (papers), and the date when there is one.
        let about: Vec<String> = ["site_name", "publisher", "venue", "date"]
            .into_iter()
            .filter_map(|key| field(result, key))
            .chain(
                result["citation_count"]
                    .as_u64()
                    .map(|n| format!("{n} citations")),
            )
            .collect();
        let mut entry = format!("{}. {title}", i + 1);
        if !about.is_empty() {
            entry.push_str(&format!(" ({})", about.join(", ")));
        }
        if let Some(url) = field(result, "url") {
            entry.push_str(&format!("\n{url}"));
        }
        if let Some(snippet) = field(result, "snippet") {
            entry.push_str(&format!("\n{}", shorten(&snippet, MAX_SNIPPET)));
        }
        lines.push(entry);
    }
    lines.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_needs_words() {
        let settings = Settings::default();
        assert!(search_query(&settings, &json!({"query": "  "})).is_err());
        assert!(search_query(&settings, &json!({})).is_err());
    }

    #[test]
    fn query_maps_arguments_and_fills_in_settings() {
        let settings = Settings {
            language: Some("en".into()),
            ..Settings::default()
        };
        let params = search_query(
            &settings,
            &json!({"query": "snail facts", "kind": "news", "recency_minutes": 60, "page": 30}),
        )
        .unwrap();
        assert_eq!(
            params,
            vec![
                ("query", "snail facts".to_string()),
                ("domain_type", "news".to_string()),
                ("language", "en".to_string()),
                ("recency_minutes", "60".to_string()),
                ("page", "10".to_string()),
            ]
        );
    }

    #[test]
    fn results_are_numbered_and_cut_to_max() {
        let body = json!({
            "query": "rust",
            "results": [
                {"position": 1, "site_name": "rust-lang.org", "title": "Rust", "snippet": "A language.", "url": "https://rust-lang.org"},
                {"position": 2, "site_name": "news.example", "title": "Rust 2.0", "date": "Oct 1, 2026", "url": "https://news.example/rust"},
                {"position": 3, "title": "Third", "url": "https://third.example"}
            ],
            "total_results": 3,
            "page": 0
        });
        let text = format_results(&body, 2);
        assert_eq!(
            text,
            "1. Rust (rust-lang.org)\nhttps://rust-lang.org\nA language.\n\n\
             2. Rust 2.0 (news.example, Oct 1, 2026)\nhttps://news.example/rust"
        );
    }

    #[test]
    fn no_results_says_so() {
        assert!(format_results(&json!({"results": []}), 8).starts_with("No results"));
    }
}
