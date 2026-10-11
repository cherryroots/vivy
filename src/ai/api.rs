//! What every provider's HTTP calls share: trying again when the API is busy, and reading
//! the message out of an error answer.

use std::time::Duration;

use reqwest::header::HeaderMap;
use serde_json::Value;
use tracing::warn;

use super::fallback;

/// The longest a `Retry-After` header can make us wait before one retry. An API asking for
/// longer is out for a while: the request fails instead, and the fallback can take over.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Sends a request built by `request`, trying again after each of `waits` while the answer's
/// status is one of `busy`. When the API says how long to wait (`Retry-After`) and that is
/// longer than the planned wait, its wait is used instead, up to [`MAX_RETRY_AFTER`].
///
/// Returns the first answer that isn't busy, or the last busy one once `waits` run out, so
/// the caller reads the status and turns a failure into its own error. Each try goes through
/// [`fallback::send`], which tries once more when there's no connection at all.
///
/// `name` is for the log line, like "Claude" or "OpenAI's Image API".
pub async fn send_retrying(
    name: &str,
    request: impl Fn() -> reqwest::RequestBuilder,
    busy: &[u16],
    waits: &[Duration],
) -> reqwest::Result<reqwest::Response> {
    let mut waits = waits.iter();
    loop {
        let response = fallback::send(request()).await?;
        let status = response.status();
        if status.is_success() || !busy.contains(&status.as_u16()) {
            return Ok(response);
        }
        let Some(&planned) = waits.next() else {
            return Ok(response);
        };
        let wait = match retry_after(response.headers()) {
            Some(asked) => planned.max(asked.min(MAX_RETRY_AFTER)),
            None => planned,
        };
        warn!(
            "{name} answered {status}, trying again in {} seconds",
            wait.as_secs()
        );
        tokio::time::sleep(wait).await;
    }
}

/// How long the API asked us to wait: `retry-after-ms` (OpenAI) or `retry-after` in
/// seconds (both). A date in `retry-after` isn't read; the planned wait is used then.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let number = |name: &str| -> Option<f64> {
        let value: f64 = headers.get(name)?.to_str().ok()?.trim().parse().ok()?;
        (value.is_finite() && value >= 0.0).then_some(value)
    };
    if let Some(ms) = number("retry-after-ms") {
        return Duration::try_from_secs_f64(ms / 1000.0).ok();
    }
    Duration::try_from_secs_f64(number("retry-after")?).ok()
}

/// The message of an API error answer (`{"error": {"message": ...}}`, which Claude and
/// OpenAI both send), or the start of the body when it isn't that.
pub fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| crate::util::shorten(body, 300))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn reads_retry_after() {
        assert_eq!(retry_after(&headers(&[])), None);
        assert_eq!(
            retry_after(&headers(&[("retry-after", "20")])),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            retry_after(&headers(&[("retry-after", "1.5")])),
            Some(Duration::from_millis(1500))
        );
        // The milliseconds one wins when both are there.
        assert_eq!(
            retry_after(&headers(&[("retry-after", "2"), ("retry-after-ms", "250")])),
            Some(Duration::from_millis(250))
        );
        // A date, or nonsense, is ignored.
        assert_eq!(
            retry_after(&headers(&[(
                "retry-after",
                "Wed, 21 Oct 2026 07:28:00 GMT"
            )])),
            None
        );
        assert_eq!(retry_after(&headers(&[("retry-after", "-3")])), None);
    }

    #[test]
    fn reads_error_messages() {
        let body = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(error_message(body), "Overloaded");
        assert_eq!(error_message("Bad Gateway"), "Bad Gateway");
    }
}
