//! Gemini's Files API: uploading videos and documents so the model can watch or read them.
//!
//! Pictures go inline in the request, but a video is too big for that, so it's uploaded
//! first and the request points at its `uri`. An upload takes two requests (a "resumable"
//! upload: one to start it, one with the bytes), and a video then needs a moment of
//! processing on Google's side before the model can use it, so [`Files::upload`] waits
//! until its `state` is `ACTIVE`.
//!
//! Google deletes uploads after 48 hours. The links uploaded in the last 47 are kept in
//! memory, so the same video in a later request isn't uploaded again.
//!
//! Reference: <https://ai.google.dev/gemini-api/docs/files>

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

use super::BASE_URL;
use crate::ai::api::error_message;
use crate::ai::claude::ContentKey;
use crate::ai::fallback;

/// How long an upload is reused: an hour less than Google keeps it (48 hours), so it can't
/// disappear halfway through an answer.
const REUSE_FOR: Duration = Duration::from_secs(47 * 60 * 60);
/// How often, and how many times, to check whether a video is ready.
const CHECK_EVERY: Duration = Duration::from_secs(2);
const MAX_CHECKS: usize = 90;

/// Uploads, and the links of the ones made recently.
#[derive(Clone)]
pub struct Files {
    http: reqwest::Client,
    key: String,
    /// The file's `uri`, and when it was uploaded.
    recent: Arc<Mutex<HashMap<ContentKey, (String, Instant)>>>,
}

impl Files {
    pub fn new(http: reqwest::Client, key: String) -> Files {
        Files {
            http,
            key,
            recent: Arc::default(),
        }
    }

    /// Uploads `data` (or finds it uploaded recently) and returns its `uri`, once the model
    /// can use it.
    pub async fn upload(&self, name: &str, mime: &str, data: &[u8]) -> anyhow::Result<String> {
        let key = ContentKey::of(data);
        if let Some((uri, at)) = self.recent.lock().unwrap().get(&key)
            && at.elapsed() < REUSE_FOR
        {
            return Ok(uri.clone());
        }
        let file = self.send(name, mime, data).await?;
        let file = self.wait_until_active(file).await?;
        let uri = file["uri"]
            .as_str()
            .context("Gemini's upload answer has no uri")?
            .to_string();
        self.recent
            .lock()
            .unwrap()
            .insert(key, (uri.clone(), Instant::now()));
        Ok(uri)
    }

    /// The upload itself: start it, then send the bytes. Returns the `file` object.
    async fn send(&self, name: &str, mime: &str, data: &[u8]) -> anyhow::Result<Value> {
        let start = self
            .http
            .post(format!("{BASE_URL}/upload/v1beta/files"))
            .header("x-goog-api-key", &self.key)
            .header("X-Goog-Upload-Protocol", "resumable")
            .header("X-Goog-Upload-Command", "start")
            .header(
                "X-Goog-Upload-Header-Content-Length",
                data.len().to_string(),
            )
            .header("X-Goog-Upload-Header-Content-Type", mime)
            .json(&json!({"file": {"display_name": name}}));
        let response = fallback::send(start)
            .await
            .context("couldn't reach Gemini")?;
        let response = check(response).await?;
        let upload_url = response
            .headers()
            .get("x-goog-upload-url")
            .and_then(|value| value.to_str().ok())
            .context("Gemini didn't say where to upload")?
            .to_string();

        let bytes = self
            .http
            .post(upload_url)
            .header("X-Goog-Upload-Offset", "0")
            .header("X-Goog-Upload-Command", "upload, finalize")
            .body(data.to_vec());
        let response = fallback::send(bytes)
            .await
            .context("couldn't reach Gemini")?;
        let body: Value = check(response).await?.json().await?;
        Ok(body["file"].clone())
    }

    /// Checks the file every couple of seconds until Google has processed it.
    async fn wait_until_active(&self, mut file: Value) -> anyhow::Result<Value> {
        for _ in 0..MAX_CHECKS {
            match file["state"].as_str() {
                // Documents and pictures are ready at once; some answers leave the state out.
                Some("ACTIVE") | None => return Ok(file),
                Some("FAILED") => bail!("Gemini couldn't process the file"),
                _ => {}
            }
            tokio::time::sleep(CHECK_EVERY).await;
            let name = file["name"].as_str().context("the upload has no name")?;
            let request = self
                .http
                .get(format!("{BASE_URL}/v1beta/{name}"))
                .header("x-goog-api-key", &self.key);
            let response = fallback::send(request)
                .await
                .context("couldn't reach Gemini")?;
            file = check(response).await?.json().await?;
        }
        bail!("Gemini was still processing the file after 3 minutes")
    }
}

/// The response, or an error with Gemini's message when it isn't a success.
async fn check(response: reqwest::Response) -> anyhow::Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    bail!("Gemini answered {status}: {}", error_message(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recent_uploads_are_reused() {
        let files = Files::new(reqwest::Client::new(), "key".into());
        let data = b"video";
        files.recent.lock().unwrap().insert(
            ContentKey::of(data),
            ("https://files/abc".into(), Instant::now()),
        );
        // Found without sending anything (there's no server to send to).
        assert_eq!(
            files.upload("clip.mp4", "video/mp4", data).await.unwrap(),
            "https://files/abc"
        );
    }
}
