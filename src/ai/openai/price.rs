//! What an OpenAI response or picture costs, from its token counts, at OpenAI's list prices.
//!
//! Prices: <https://developers.openai.com/api/docs/pricing> (checked 2026-10-11).
//! Left out, so the estimate is a little low and the bill (`billing.rs`) is the real
//! number: long prompts on models that charge more for them, writing to the prompt cache
//! where a model charges for it, and code interpreter containers ($0.03 per session).
//! `service_tier = "fast"` costs twice the standard price on every model here.

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

/// Model ID prefixes and their standard prices. The first match wins, so a longer ID comes
/// before a shorter one it starts with.
const TABLE: &[(&str, Prices)] = &[
    ("gpt-6.1-sol", prices(2.0, 0.10, 10.0)),
    ("gpt-6-astra", prices(10.0, 1.0, 50.0)),
    ("gpt-6-luna", prices(0.10, 0.01, 0.50)),
    ("gpt-5.6-sol", prices(4.0, 0.40, 20.0)),
    ("gpt-5.3-codex", prices(1.75, 0.175, 14.0)),
];

/// A model the table doesn't know is priced like gpt-6.1-sol (the default model), so the
/// total is still close.
const UNKNOWN: Prices = prices(2.0, 0.10, 10.0);

/// Each web search costs this, on top of the tokens its results add.
const WEB_SEARCH: f64 = 10.0 / 1000.0;

/// GPT Image 2.5 (Sunburst and Flare cost the same), USD per million tokens. Text and
/// pictures in the prompt are priced apart; only pictures come out.
const IMAGE_TEXT_INPUT: f64 = 5.0;
const IMAGE_TEXT_CACHED: f64 = 1.25;
const IMAGE_IMAGE_INPUT: f64 = 8.0;
const IMAGE_IMAGE_CACHED: f64 = 2.0;
const IMAGE_OUTPUT: f64 = 30.0;

fn per_token(count: u64, price: f64) -> f64 {
    count as f64 * price / 1_000_000.0
}

/// What one finished chat response cost, from the `response` object of
/// `response.completed`. `tier` is the service tier the request asked for, used when the
/// response doesn't say which one it ran on.
pub fn chat_cost(response: &Value, tier: Option<&str>) -> Used {
    let usage = &response["usage"];
    let input = usage["input_tokens"].as_u64().unwrap_or(0);
    let cached = usage["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0)
        .min(input);
    let output = usage["output_tokens"].as_u64().unwrap_or(0);
    let searches = response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "web_search_call")
        .count();

    let model = response["model"].as_str().unwrap_or_default();
    let p = TABLE
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map_or(UNKNOWN, |(_, prices)| *prices);
    let tier = response["service_tier"].as_str().or(tier);
    let times = if matches!(tier, Some("fast" | "priority")) {
        2.0
    } else {
        1.0
    };
    let tokens = per_token(input - cached, p.input)
        + per_token(cached, p.cache_read)
        + per_token(output, p.output);
    Used {
        usd: tokens * times + searches as f64 * WEB_SEARCH,
        input: input - cached,
        cache_read: cached,
        cache_write: 0,
    }
}

/// What one picture request cost, from the `usage` of the Image API's answer.
pub fn image_cost(usage: &Value) -> Used {
    let details = &usage["input_tokens_details"];
    let get = |value: &Value| value.as_u64().unwrap_or(0);
    let (text, image) = (get(&details["text_tokens"]), get(&details["image_tokens"]));
    // Cached tokens aren't split into text and pictures; count them as text, the cheaper.
    let cached = get(&details["cached_tokens"]).min(text + image);
    let cached_text = cached.min(text);
    let cached_image = cached - cached_text;
    let output = get(&usage["output_tokens"]);
    let usd = per_token(text - cached_text, IMAGE_TEXT_INPUT)
        + per_token(cached_text, IMAGE_TEXT_CACHED)
        + per_token(image - cached_image, IMAGE_IMAGE_INPUT)
        + per_token(cached_image, IMAGE_IMAGE_CACHED)
        + per_token(output, IMAGE_OUTPUT);
    Used {
        usd,
        input: text + image - cached,
        cache_read: cached,
        cache_write: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn prices_a_chat_response() {
        let response = json!({
            "model": "gpt-6.1-sol-2026-08-01",
            "service_tier": "default",
            "usage": {
                "input_tokens": 101_000,
                "input_tokens_details": {"cached_tokens": 100_000},
                "output_tokens": 2_000,
            },
            "output": [{"type": "web_search_call"}, {"type": "message"}],
        });
        let used = chat_cost(&response, Some("fast"));
        // 0.002 + 0.01 + 0.02 + 0.01
        assert!(close(used.usd, 0.042), "{}", used.usd);
        assert_eq!((used.input, used.cache_read), (1_000, 100_000));

        // The fast tier doubles the tokens, not the searches.
        let mut fast = response.clone();
        fast["service_tier"] = json!("fast");
        assert!(close(chat_cost(&fast, None).usd, 0.074));
        // Without a tier in the response, the request's counts.
        fast.as_object_mut().unwrap().remove("service_tier");
        assert!(close(chat_cost(&fast, Some("fast")).usd, 0.074));
        assert!(close(chat_cost(&fast, None).usd, 0.042));

        assert_eq!(chat_cost(&json!({}), None), Used::default());
    }

    #[test]
    fn prices_a_picture() {
        let usage = json!({
            "input_tokens": 1_100,
            "input_tokens_details": {"text_tokens": 100, "image_tokens": 1_000},
            "output_tokens": 4_000,
        });
        let used = image_cost(&usage);
        // 0.0005 + 0.008 + 0.12
        assert!(close(used.usd, 0.1285), "{}", used.usd);
        assert_eq!(used.input, 1_100);
        assert_eq!(image_cost(&json!({})), Used::default());
    }
}
