//! Keeps chat's prompt cache warm through quiet stretches.
//!
//! Chat answers and chime-ins cache their system prompt and tools (about 14k tokens) for
//! an hour. The hour counts from the start of the last request that read or wrote them, and
//! every read starts it over for free. After an hour with no chat, the next answer writes
//! them again at 2x the input price (about $0.11 on Opus 5.5).
//!
//! So shortly before the hour is up (`keep_cache_warm_minutes` after the last use, 58 by
//! default), a ping sends the same system prompt and tools with `max_tokens: 0`: Claude
//! reads the cache and stops, which costs only the read (about $0.003) and starts the hour
//! over. Every chat request with the same prompt and tools counts as a use, so while people
//! talk there are no pings at all. After `keep_cache_warm_hours` without chat it stops:
//! pinging longer than that costs more than writing the cache once.
//!
//! Counted per day in `claude_keep_warm`, for the status picture and the savings doc:
//! - pings, and what they cost (also in the spend, as the job "keep_warm");
//! - cold pings: a ping that had to write the cache, so it had expired already (the wait
//!   is too close to an hour);
//! - saves: a chat request more than an hour after the last chat, which found the cache
//!   warm thanks to a ping, and the write that saved.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use rusqlite::params;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::collect::Usage;
use super::{Sender, container_gone, messages, price, set_container};
use crate::ai::spend::{Spend, Used};
use crate::core::Timers;
use crate::core::db::Db;

pub const MIGRATIONS: &[&str] = &[
    // 1: per day (UTC, "2026-10-11"): pings and what they cost, pings that found the cache
    // cold, and cold starts the pings saved, with what those writes would have cost.
    "CREATE TABLE claude_keep_warm (
        day TEXT PRIMARY KEY,
        pings INTEGER NOT NULL DEFAULT 0,
        ping_usd REAL NOT NULL DEFAULT 0,
        cold_pings INTEGER NOT NULL DEFAULT 0,
        saves INTEGER NOT NULL DEFAULT 0,
        saved_usd REAL NOT NULL DEFAULT 0
    );",
];

/// How long Anthropic keeps the prompt cached after a use.
const CACHE_LIFETIME: TimeDelta = TimeDelta::hours(1);
/// Waits after a failed ping, doubling up to the last.
const RETRY_WAITS: [u64; 4] = [60, 120, 300, 900];
/// How long to sleep when there's nothing to keep warm, before looking again.
const IDLE_CHECK: Duration = Duration::from_secs(5 * 60);

/// Shared by the Claude provider, which reports each chat request, and the loop that
/// pings. Cheap to clone.
#[derive(Clone)]
pub struct KeepWarm {
    state: Arc<Mutex<State>>,
    sender: Sender,
    spend: Option<Spend>,
    /// Ping this long after the last use.
    after: TimeDelta,
    /// Stop pinging this long after the last chat.
    stop_after: TimeDelta,
}

/// What the cache holds and when it was used.
#[derive(Debug, Default)]
struct State {
    /// What a ping sends: the last chat request without its conversation.
    ping: Option<Value>,
    /// Which prompt and tools `ping` caches (see [`prefix_key`]).
    key: String,
    /// The start of the last request that used the cached prefix: chat or a ping.
    last_use: Option<DateTime<Utc>>,
    /// The start of the last chat request that used it.
    last_chat: Option<DateTime<Utc>>,
    /// The start of the last ping that worked.
    last_ping: Option<DateTime<Utc>>,
    /// The prefix's size in tokens, as the last ping reported it.
    prefix_tokens: u64,
}

/// What [`State::chat`] found: a cold start a ping prevented.
#[derive(Debug, PartialEq)]
struct Save {
    tokens: u64,
}

impl State {
    /// A request with `body` starts at `now`. `chat` is a chat answer, which may move the
    /// pings to a new prompt or tool list; a chime-in only counts when it reads the same
    /// cache entry. Returns a save when the cache would have gone cold without the pings.
    fn chat(&mut self, body: &Value, now: DateTime<Utc>, chat: bool) -> Option<Save> {
        let key = prefix_key(body);
        let same = self.ping.is_some() && key == self.key;
        if !same && !chat {
            return None;
        }
        let save = (same
            && self.prefix_tokens > 0
            && self
                .last_chat
                .is_some_and(|last| now - last >= CACHE_LIFETIME)
            && self.last_ping.is_some_and(|ping| {
                now - ping < CACHE_LIFETIME && self.last_chat.is_some_and(|last| ping > last)
            }))
        .then_some(Save {
            tokens: self.prefix_tokens,
        });
        if !same {
            // Another prompt or tool list (the bot was updated, say): another cache entry.
            self.prefix_tokens = 0;
            self.last_ping = None;
        }
        self.ping = Some(ping_body(body));
        self.key = key;
        self.last_use = Some(now);
        self.last_chat = Some(now);
        save
    }

    /// When the next ping is due, or `None` when there's nothing to keep warm (no chat
    /// since the start, or none for `stop_after`).
    fn due(&self, after: TimeDelta, stop_after: TimeDelta) -> Option<DateTime<Utc>> {
        self.ping.as_ref()?;
        let at = self.last_use? + after;
        (at <= self.last_chat? + stop_after).then_some(at)
    }
}

/// Which cache entry a request reads: its model, system prompt, tools, thinking and effort
/// settings and skills, but not the conversation or the container it runs in.
fn prefix_key(body: &Value) -> String {
    let parts = json!([
        body["model"],
        body["thinking"],
        body["output_config"],
        body["container"]["skills"],
    ]);
    messages::setup(&json!([body["system"], parts]), &body["tools"])
}

/// A ping for the prefix of `body`: the same model, prompt, tools and settings, one tiny
/// message, `max_tokens: 0` and no streaming (which `max_tokens: 0` doesn't allow). The
/// conversation's own cache marker is left out, so the ping writes nothing of its own.
fn ping_body(body: &Value) -> Value {
    let mut ping = json!({
        "model": body["model"],
        "max_tokens": 0,
        "system": body["system"],
        "tools": body["tools"],
        "thinking": body["thinking"],
        "output_config": body["output_config"],
        "messages": [{"role": "user", "content": "."}],
    });
    for key in ["fallbacks", "container"] {
        if let Some(value) = body.get(key) {
            ping[key] = value.clone();
        }
    }
    ping
}

/// The token counts of a non-streamed answer.
fn usage_of(answer: &Value) -> Usage {
    let usage = &answer["usage"];
    let count = |value: &Value| value.as_u64().unwrap_or(0);
    Usage {
        input: count(&usage["input_tokens"]),
        cache_read: count(&usage["cache_read_input_tokens"]),
        cache_write: count(&usage["cache_creation_input_tokens"]),
        cache_write_1h: count(&usage["cache_creation"]["ephemeral_1h_input_tokens"]),
        output: count(&usage["output_tokens"]),
        web_searches: 0,
    }
}

impl KeepWarm {
    pub(super) fn new(sender: Sender, spend: Option<Spend>, minutes: u32, hours: u32) -> KeepWarm {
        KeepWarm {
            state: Arc::default(),
            sender,
            spend,
            after: TimeDelta::minutes(i64::from(minutes)),
            stop_after: TimeDelta::hours(i64::from(hours)),
        }
    }

    /// A request with `body` is about to be sent. Only requests that cache their prompt
    /// for an hour (chat and chime-ins) count.
    pub(super) async fn used(&self, body: &Value, job: &str) {
        if body["system"][0]["cache_control"]["ttl"] != "1h" {
            return;
        }
        let now = Utc::now();
        let save = self.state.lock().unwrap().chat(body, now, job == "chat");
        let Some(save) = save else {
            return;
        };
        let model = body["model"].as_str().unwrap_or_default();
        let usd = price::hour_write_saved(save.tokens, model);
        info!(
            "the chat cache was still warm after a quiet hour thanks to the pings, saving ${usd:.3}"
        );
        self.count(now, move |conn, day| {
            conn.execute(
                "INSERT INTO claude_keep_warm (day, saves, saved_usd) VALUES (?1, 1, ?2)
                 ON CONFLICT(day) DO UPDATE SET saves = saves + 1, saved_usd = saved_usd + ?2",
                params![day, usd],
            )
        })
        .await;
    }

    /// Pings whenever one is due, until `shutdown` is cancelled.
    pub async fn run(self, timers: Timers, shutdown: CancellationToken) {
        let timer = timers.add("Chat cache warmth", "before the cache expires");
        let mut failures = 0;
        loop {
            let now = Utc::now();
            let due = self.state.lock().unwrap().due(self.after, self.stop_after);
            let wait = match due {
                Some(at) if at <= now => Duration::ZERO,
                Some(at) => (at - now).to_std().unwrap_or_default(),
                None => IDLE_CHECK,
            };
            if !wait.is_zero() {
                timer.next_at(due.unwrap_or(now + TimeDelta::minutes(5)));
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(wait) => {}
                }
                // Chat may have used the cache meanwhile; look again.
                continue;
            }
            timer.running();
            let result = tokio::select! {
                _ = shutdown.cancelled() => return,
                result = self.ping() => result,
            };
            match result {
                Ok(()) => failures = 0,
                Err(err) => {
                    let wait = RETRY_WAITS[failures.min(RETRY_WAITS.len() - 1)];
                    if failures == 0 {
                        warn!("keeping the chat cache warm failed: {err:#}");
                    }
                    failures += 1;
                    timer.sleeping(Duration::from_secs(wait));
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                    }
                }
            }
        }
    }

    /// Sends one ping and counts it.
    async fn ping(&self) -> anyhow::Result<()> {
        let Some(mut body) = self.state.lock().unwrap().ping.clone() else {
            return Ok(());
        };
        let started = Utc::now();
        let mut result = self.sender.messages(&body).await;
        // The container of the last chat expired; the ping doesn't need one.
        if let Err(err) = &result
            && body["container"].get("id").is_some()
            && container_gone(err)
        {
            set_container(&mut body, None);
            result = self.sender.messages(&body).await;
        }
        let answer: Value = result?.json().await?;
        let usage = usage_of(&answer);
        let model = answer["model"]
            .as_str()
            .or(body["model"].as_str())
            .unwrap_or_default();
        let usd = price::cost(&usage, model);
        let cold = usage.cache_write > 0;
        {
            let mut state = self.state.lock().unwrap();
            // Chat may have used the cache while the ping was out; keep the later time.
            if state.last_use.is_none_or(|at| at < started) {
                state.last_use = Some(started);
            }
            state.last_ping = Some(started);
            state.prefix_tokens = usage.cache_read + usage.cache_write;
        }
        if cold {
            warn!(
                "the chat cache had expired before the keep-warm ping ({} tokens written again); \
                 a smaller `keep_cache_warm_minutes` keeps it",
                usage.cache_write
            );
        } else {
            debug!(
                "kept the chat cache warm: read {} tokens for ${usd:.4}",
                usage.cache_read
            );
        }
        if let Some(spend) = &self.spend {
            let used = Used {
                usd,
                input: usage.input,
                cache_read: usage.cache_read,
                cache_write: usage.cache_write,
            };
            spend.add("keep_warm", used).await;
        }
        let cold = i64::from(cold);
        self.count(started, move |conn, day| {
            conn.execute(
                "INSERT INTO claude_keep_warm (day, pings, ping_usd, cold_pings) VALUES (?1, 1, ?2, ?3)
                 ON CONFLICT(day) DO UPDATE SET pings = pings + 1, ping_usd = ping_usd + ?2,
                     cold_pings = cold_pings + ?3",
                params![day, usd, cold],
            )
        })
        .await;
        Ok(())
    }

    /// Updates the row of `at`'s day. Errors are only logged.
    async fn count(
        &self,
        at: DateTime<Utc>,
        change: impl FnOnce(&rusqlite::Connection, &str) -> rusqlite::Result<usize> + Send + 'static,
    ) {
        let Some(spend) = &self.spend else {
            return;
        };
        let day = at.format("%Y-%m-%d").to_string();
        let result = spend.db().call(move |conn| Ok(change(conn, &day)?)).await;
        if let Err(err) = result {
            warn!("couldn't count the keep-warm ping: {err:#}");
        }
    }
}

/// This month's keep-warm numbers, for the status picture.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Month {
    pub pings: u64,
    pub ping_usd: f64,
    pub cold_pings: u64,
    pub saves: u64,
    pub saved_usd: f64,
}

impl Month {
    /// What the pings saved after paying for themselves.
    pub fn net_usd(&self) -> f64 {
        self.saved_usd - self.ping_usd
    }
}

/// The numbers from the start of `now`'s month (UTC).
pub async fn this_month(db: &Db, now: DateTime<Utc>) -> anyhow::Result<Month> {
    let from = now.format("%Y-%m-01").to_string();
    let month = db
        .call(move |conn| {
            Ok(conn.query_row(
                "SELECT coalesce(sum(pings), 0), coalesce(sum(ping_usd), 0),
                        coalesce(sum(cold_pings), 0), coalesce(sum(saves), 0),
                        coalesce(sum(saved_usd), 0)
                 FROM claude_keep_warm WHERE day >= ?1",
                [from],
                |row| {
                    Ok(Month {
                        pings: row.get(0)?,
                        ping_usd: row.get(1)?,
                        cold_pings: row.get(2)?,
                        saves: row.get(3)?,
                        saved_usd: row.get(4)?,
                    })
                },
            )?)
        })
        .await?;
    Ok(month)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_body(prompt: &str) -> Value {
        json!({
            "model": "claude-opus-5-5",
            "max_tokens": 32000,
            "stream": true,
            "system": [{"type": "text", "text": prompt, "cache_control": {"type": "ephemeral", "ttl": "1h"}}],
            "tools": [{"name": "memory"}],
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "medium"},
            "messages": [{"role": "user", "content": "hi"}],
            "cache_control": {"type": "ephemeral"},
            "fallbacks": "default",
            "container": {"id": "c1", "skills": [{"skill_id": "pdf"}]},
        })
    }

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap() + TimeDelta::minutes(minutes)
    }

    const AFTER: TimeDelta = TimeDelta::minutes(58);
    const STOP: TimeDelta = TimeDelta::hours(36);

    #[test]
    fn the_ping_is_the_chat_request_without_its_conversation() {
        let ping = ping_body(&chat_body("Be nice."));
        assert_eq!(ping["max_tokens"], 0);
        assert!(ping.get("stream").is_none());
        assert!(ping.get("cache_control").is_none());
        assert_eq!(ping["messages"], json!([{"role": "user", "content": "."}]));
        assert_eq!(ping["system"], chat_body("Be nice.")["system"]);
        assert_eq!(ping["fallbacks"], "default");
        assert_eq!(ping["container"]["id"], "c1");
        // The ping reads the same cache entry as chat.
        assert_eq!(prefix_key(&ping), prefix_key(&chat_body("Be nice.")));
        assert_ne!(prefix_key(&ping), prefix_key(&chat_body("Be kind.")));
        // Another container doesn't make another entry.
        let mut other = chat_body("Be nice.");
        other["container"]["id"] = json!("c2");
        assert_eq!(prefix_key(&other), prefix_key(&ping));
    }

    #[test]
    fn pings_are_due_58_minutes_after_the_last_use() {
        let mut state = State::default();
        assert_eq!(state.due(AFTER, STOP), None);
        state.chat(&chat_body("Be nice."), at(0), true);
        assert_eq!(state.due(AFTER, STOP), Some(at(58)));
        // Chat at minute 30 pushes the ping back.
        state.chat(&chat_body("Be nice."), at(30), true);
        assert_eq!(state.due(AFTER, STOP), Some(at(88)));
        // A ping counts as a use too.
        state.last_use = Some(at(88));
        assert_eq!(state.due(AFTER, STOP), Some(at(146)));
        // It stops 36 hours after the last chat (minute 30): a ping due at minute 2158
        // still goes, one due at 2218 doesn't.
        state.last_use = Some(at(2100));
        assert_eq!(state.due(AFTER, STOP), Some(at(2158)));
        state.last_use = Some(at(2160));
        assert_eq!(state.due(AFTER, STOP), None);
    }

    #[test]
    fn a_chat_after_a_quiet_hour_counts_as_saved() {
        let mut state = State::default();
        assert_eq!(state.chat(&chat_body("Be nice."), at(0), true), None);
        // Within the hour, the cache was warm anyway.
        assert_eq!(state.chat(&chat_body("Be nice."), at(40), true), None);
        // Pinged at 98 and 156; chat at 170 found it warm.
        state.last_ping = Some(at(156));
        state.last_use = Some(at(156));
        state.prefix_tokens = 14_000;
        assert_eq!(
            state.chat(&chat_body("Be nice."), at(170), true),
            Some(Save { tokens: 14_000 })
        );
        // The next one, 10 minutes later, is ordinary.
        assert_eq!(state.chat(&chat_body("Be nice."), at(180), true), None);
    }

    #[test]
    fn no_save_without_a_ping_or_with_another_prompt() {
        let mut state = State::default();
        state.chat(&chat_body("Be nice."), at(0), true);
        state.prefix_tokens = 14_000;
        // Two hours later with no ping: it was cold.
        assert_eq!(state.chat(&chat_body("Be nice."), at(120), true), None);
        // A ping older than an hour didn't keep it.
        state.last_ping = Some(at(150));
        assert_eq!(state.chat(&chat_body("Be nice."), at(240), true), None);
        // A new prompt (the bot was updated) is another cache entry: no save, and the
        // pings follow the new one.
        state.last_ping = Some(at(298));
        assert_eq!(state.chat(&chat_body("Be kind."), at(310), true), None);
        assert_eq!(state.prefix_tokens, 0);
        assert_eq!(
            state.ping.as_ref().unwrap()["system"][0]["text"],
            "Be kind."
        );
    }

    #[test]
    fn chime_ins_count_only_on_the_same_prompt() {
        let mut state = State::default();
        // Before any chat there's nothing to match.
        assert_eq!(state.chat(&chat_body("Be nice."), at(0), false), None);
        assert_eq!(state.due(AFTER, STOP), None);
        state.chat(&chat_body("Be nice."), at(0), true);
        // The same prompt and tools: it pushes the ping back.
        state.chat(&chat_body("Be nice."), at(20), false);
        assert_eq!(state.due(AFTER, STOP), Some(at(78)));
        // Another prompt: another cache entry, so it neither counts nor takes over.
        state.chat(&chat_body("Chime in."), at(40), false);
        assert_eq!(state.due(AFTER, STOP), Some(at(78)));
        assert_eq!(
            state.ping.as_ref().unwrap()["system"][0]["text"],
            "Be nice."
        );
    }

    #[test]
    fn reads_the_usage_of_a_ping() {
        let answer = json!({
            "model": "claude-opus-5-5",
            "content": [],
            "stop_reason": "max_tokens",
            "usage": {
                "input_tokens": 3,
                "cache_read_input_tokens": 14000,
                "cache_creation_input_tokens": 0,
                "output_tokens": 0,
            },
        });
        let usage = usage_of(&answer);
        assert_eq!(usage.cache_read, 14_000);
        assert_eq!(usage.cache_write, 0);
        // 14k read at $0.20 and 3 at $4.
        let usd = price::cost(&usage, "claude-opus-5-5");
        assert!((usd - 0.002_812).abs() < 1e-9, "{usd}");
    }

    #[tokio::test]
    async fn counts_pings_and_saves_per_month() {
        let db = Db::open_in_memory().await.unwrap();
        db.call(|conn| {
            crate::core::db::migrate(conn, "claude_keep_warm", MIGRATIONS)?;
            conn.execute_batch(
                "INSERT INTO claude_keep_warm VALUES ('2026-09-30', 9, 0.025, 0, 0, 0);
                 INSERT INTO claude_keep_warm VALUES ('2026-10-01', 20, 0.056, 1, 2, 0.218);
                 INSERT INTO claude_keep_warm VALUES ('2026-10-02', 10, 0.028, 0, 1, 0.109);",
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-11T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let month = this_month(&db, now).await.unwrap();
        assert_eq!(month.pings, 30);
        assert_eq!(month.cold_pings, 1);
        assert_eq!(month.saves, 3);
        assert!((month.net_usd() - (0.327 - 0.084)).abs() < 1e-9);
    }
}
