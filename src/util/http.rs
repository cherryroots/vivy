//! The settings every HTTP client of the bot starts from.

use std::time::Duration;

/// A `reqwest` client builder with the bot's user agent and timeouts. main.rs builds the
/// shared client for API calls from it, and `media::safe_client` adds its address checks on
/// top for links from messages.
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(concat!("Vivy/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        // A response that sends nothing for this long fails instead of hanging forever.
        // Generous, since AI streams can go quiet while the model thinks (Claude sends a
        // ping now and then).
        .read_timeout(Duration::from_secs(5 * 60))
}
