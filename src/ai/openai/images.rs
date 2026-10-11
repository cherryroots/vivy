//! Drawing pictures with OpenAI's Image API (GPT Image 2.5).
//!
//! A new picture is `POST /v1/images/generations`; a picture made from others (an edit, or
//! "draw me in this style") is `POST /v1/images/edits` with those pictures as `data:` URLs.
//! Both answer with the picture as base64 and the tokens it used, which [`Spend`] adds
//! up under OpenAI as the job "drawing".
//!
//! Any feature can use this through `ctx.ai.images`; the drawing feature's chat tool does.
//!
//! API reference: <https://developers.openai.com/api/reference/resources/images>,
//! guide: <https://developers.openai.com/api/docs/guides/image-generation>

use std::time::Duration;

use anyhow::{Context as _, bail};
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use serde_json::{Value, json};

use super::price;
use crate::ai::Spend;
use crate::ai::api::{self, error_message};
use crate::core::user_error;
use crate::util::media::ModelImage;

const API: &str = "https://api.openai.com/v1";
/// Waits before retrying when OpenAI is busy or rate limited.
const RETRY_WAITS: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(15)];
/// The edits endpoint takes at most this many pictures.
pub const MAX_PICTURES: usize = 16;

/// One picture to draw.
#[derive(Debug, Clone)]
pub struct PictureRequest<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    /// Pictures to start from or follow. Empty draws something new.
    pub pictures: &'a [ModelImage],
    /// "1024x1024", "1536x1024", "1024x1536", "auto", or any WIDTHxHEIGHT in steps of 16.
    pub size: &'a str,
    /// "low", "medium", "high", "xhigh", "max" or "auto".
    pub quality: &'a str,
    /// "png", "jpeg" or "webp".
    pub format: &'a str,
    /// "auto" or "low" (less strict).
    pub moderation: &'a str,
}

/// A finished picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub data: Vec<u8>,
    /// The file extension: "png", "jpeg" or "webp".
    pub format: String,
    /// What it cost, estimated from its tokens.
    pub usd: f64,
}

#[derive(Clone)]
pub struct Images {
    http: reqwest::Client,
    token: String,
    /// OpenAI's spend, where each picture's cost is added; `None` in tests.
    spend: Option<Spend>,
}

impl Images {
    pub fn new(http: reqwest::Client, token: String, spend: Option<Spend>) -> Images {
        Images { http, token, spend }
    }

    /// Draws one picture. Takes a while: up to a couple of minutes for the best quality.
    pub async fn draw(&self, drawing: &PictureRequest<'_>) -> anyhow::Result<Picture> {
        let (path, body) = request_body(drawing);
        let answer = self.post(path, &body).await?;
        let picture = read_answer(&answer, drawing.format)?;
        if let Some(spend) = &self.spend {
            let used = price::image_cost(&answer["usage"]);
            spend.add("drawing", used).await;
        }
        Ok(picture)
    }

    /// Posts a request, retrying a couple of times when OpenAI is busy.
    async fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let request = || {
            self.http
                .post(format!("{API}{path}"))
                .bearer_auth(&self.token)
                .json(body)
        };
        let busy = [429, 500, 502, 503];
        let response = api::send_retrying("OpenAI's Image API", request, &busy, &RETRY_WAITS)
            .await
            .context("couldn't reach OpenAI")?;
        let status = response.status();
        if status.is_success() {
            return response.json().await.context("OpenAI sent invalid JSON");
        }
        let text = response.text().await.unwrap_or_default();
        let message = error_message(&text);
        // 400 is a request it won't draw (its safety system) or can't (a bad size): the
        // model reads why and can try again, and the log channel isn't bothered.
        if status == reqwest::StatusCode::BAD_REQUEST {
            return Err(user_error(format!("OpenAI won't draw that: {message}")));
        }
        bail!("OpenAI answered {status}: {message}");
    }
}

/// Which endpoint to call, and the JSON body for it.
fn request_body(drawing: &PictureRequest) -> (&'static str, Value) {
    let mut body = json!({
        "model": drawing.model,
        "prompt": drawing.prompt,
        "n": 1,
        "size": drawing.size,
        "quality": drawing.quality,
        "output_format": drawing.format,
        "moderation": drawing.moderation,
    });
    if drawing.pictures.is_empty() {
        return ("/images/generations", body);
    }
    let pictures: Vec<Value> = drawing
        .pictures
        .iter()
        .take(MAX_PICTURES)
        .map(|picture| json!({"image_url": picture.data_url()}))
        .collect();
    body["images"] = json!(pictures);
    ("/images/edits", body)
}

/// The picture in the API's answer.
fn read_answer(answer: &Value, asked_format: &str) -> anyhow::Result<Picture> {
    let encoded = answer["data"][0]["b64_json"]
        .as_str()
        .context("OpenAI's answer has no picture")?;
    let data = BASE64_STANDARD
        .decode(encoded)
        .context("OpenAI's picture isn't valid base64")?;
    let format = answer["output_format"].as_str().unwrap_or(asked_format);
    Ok(Picture {
        data,
        format: format.to_string(),
        usd: price::image_cost(&answer["usage"]).usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawing(pictures: &[ModelImage]) -> PictureRequest<'_> {
        PictureRequest {
            model: "gpt-image-2.5-flare",
            prompt: "a cat in a hat",
            pictures,
            size: "1024x1024",
            quality: "medium",
            format: "png",
            moderation: "auto",
        }
    }

    #[test]
    fn a_new_picture_is_a_generation() {
        let (path, body) = request_body(&drawing(&[]));
        assert_eq!(path, "/images/generations");
        assert_eq!(body["model"], "gpt-image-2.5-flare");
        assert_eq!(body["quality"], "medium");
        assert!(body.get("images").is_none());
    }

    #[test]
    fn pictures_make_it_an_edit() {
        let picture = ModelImage {
            mime: "image/png",
            data: vec![1],
        };
        let pictures = vec![picture; MAX_PICTURES + 2];
        let (path, body) = request_body(&drawing(&pictures));
        assert_eq!(path, "/images/edits");
        let images = body["images"].as_array().unwrap();
        assert_eq!(images.len(), MAX_PICTURES);
        assert_eq!(
            images[0],
            json!({"image_url": "data:image/png;base64,AQ=="})
        );
    }

    #[test]
    fn reads_the_picture() {
        let answer = json!({
            "data": [{"b64_json": "AQI="}],
            "output_format": "webp",
            "usage": {"input_tokens_details": {"text_tokens": 0, "image_tokens": 0}, "output_tokens": 1_000_000},
        });
        let picture = read_answer(&answer, "png").unwrap();
        assert_eq!(picture.data, vec![1, 2]);
        assert_eq!(picture.format, "webp");
        assert_eq!(picture.usd, 30.0);
        assert!(read_answer(&json!({"data": []}), "png").is_err());
    }
}
