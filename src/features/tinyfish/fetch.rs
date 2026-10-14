//! `tinyfish_fetch`: reads whole pages with TinyFish's Fetch API, which opens them in a real
//! browser when they need one (Reddit, pages built with JavaScript) and returns their text
//! as markdown. Free up to 1,000 pages a day per key (150 a minute).
//! <https://docs.tinyfish.ai/fetch-api/reference>

use std::time::Duration;

use serde_json::{Value, json};
use tracing::info;

use super::{Settings, read_answer};
use crate::ai::ToolDef;
use crate::core::{BotCtx, Result, user_error};
use crate::util::shorten;

pub const TOOL: &str = "tinyfish_fetch";
const API: &str = "https://api.fetch.tinyfish.ai";
/// Most pages read in one call.
const MAX_URLS: usize = 3;
/// TinyFish gives up on a page after 110 seconds and on the whole request after 120;
/// waiting a little longer lets its own timeout error arrive.
const TIMEOUT: Duration = Duration::from_secs(150);

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL,
        description: "Reads whole web pages with TinyFish and returns their text as markdown: articles, Reddit threads with their comments, docs, PDFs. It opens pages in a real browser when they need one, so it reads sites a plain fetch can't. Use it on links from tinyfish_search or links people post. Long pages are cut off.",
        parameters: json!({
            "type": "object",
            "properties": {
                "urls": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "The pages to read, 1 to 3 http or https links."
                },
                "purpose": {"type": "string", "description": "Optional: what you're looking for on the pages, in a sentence. Helps pick out the right text."}
            },
            "required": ["urls"]
        }),
    }
}

pub async fn run(ctx: &BotCtx, key: &str, settings: &Settings, args: &Value) -> Result<String> {
    let body = fetch_body(args)?;
    let request = ctx
        .web
        .post(API)
        .header("X-API-Key", key)
        .timeout(TIMEOUT)
        .json(&body);
    let answer = read_answer(request).await?;
    let pages = answer["results"].as_array().map_or(0, Vec::len);
    let failed = answer["errors"].as_array().map_or(0, Vec::len);
    info!(pages, failed, "read pages with TinyFish");
    Ok(format_pages(&answer, settings.max_page_characters.max(500)))
}

/// The request body for one call: the links (at most [`MAX_URLS`]) and the purpose. Fails
/// when there's no link.
fn fetch_body(args: &Value) -> Result<Value> {
    // A single link given as a string works too.
    let urls: Vec<&str> = match &args["urls"] {
        Value::String(url) => vec![url.as_str()],
        urls => urls
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect(),
    };
    let urls: Vec<&str> = urls
        .into_iter()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .collect();
    if urls.is_empty() {
        return Err(user_error("give at least one link to read"));
    }
    if urls.len() > MAX_URLS {
        return Err(user_error(format!(
            "that's {} links; read at most {MAX_URLS} at a time",
            urls.len()
        )));
    }
    let mut body = json!({"urls": urls, "format": "markdown"});
    if let Some(purpose) = args["purpose"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["purpose"] = purpose.into();
    }
    Ok(body)
}

/// TinyFish's answer as text for the model: each page with its title, link, date and text
/// (cut at `max_characters`), then a line for each page that couldn't be read.
fn format_pages(answer: &Value, max_characters: usize) -> String {
    let field = |value: &Value, key: &str| {
        value[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mut parts = Vec::new();
    for page in answer["results"].as_array().into_iter().flatten() {
        let url = field(page, "final_url")
            .or_else(|| field(page, "url"))
            .unwrap_or_default();
        let title = field(page, "title").unwrap_or_else(|| "(no title)".to_string());
        let mut part = format!("# {title}\n{url}");
        if let Some(date) = field(page, "published_date") {
            part.push_str(&format!("\nPublished {date}"));
        }
        let text = field(page, "text").unwrap_or_else(|| "(no text)".to_string());
        part.push_str(&format!("\n\n{}", shorten(&text, max_characters)));
        if text.chars().count() > max_characters {
            part.push_str("\n(The page goes on; this is the start.)");
        }
        parts.push(part);
    }
    for error in answer["errors"].as_array().into_iter().flatten() {
        let url = field(error, "url").unwrap_or_default();
        let code = field(error, "error").unwrap_or_else(|| "unknown error".to_string());
        let status = error["status"]
            .as_u64()
            .map(|s| format!(" ({s})"))
            .unwrap_or_default();
        parts.push(format!("Couldn't read {url}: {code}{status}"));
    }
    if parts.is_empty() {
        return "TinyFish returned nothing for those links.".to_string();
    }
    parts.join("\n\n---\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_takes_one_to_three_links() {
        assert!(fetch_body(&json!({"urls": []})).is_err());
        assert!(fetch_body(&json!({"urls": ["a", "b", "c", "d"]})).is_err());
        assert_eq!(
            fetch_body(&json!({"urls": "https://a.example", "purpose": " prices "})).unwrap(),
            json!({"urls": ["https://a.example"], "format": "markdown", "purpose": "prices"})
        );
    }

    #[test]
    fn pages_are_cut_and_errors_listed() {
        let answer = json!({
            "results": [{
                "url": "https://a.example",
                "final_url": "https://a.example/home",
                "title": "A",
                "published_date": null,
                "text": "abcdefghij"
            }],
            "errors": [{"url": "https://b.example", "error": "page_not_found", "status": 404}]
        });
        assert_eq!(
            format_pages(&answer, 5),
            "# A\nhttps://a.example/home\n\nabcd…\n(The page goes on; this is the start.)\
             \n\n---\n\nCouldn't read https://b.example: page_not_found (404)"
        );
    }
}
