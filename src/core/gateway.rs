//! The live connections to Discord ("shards"), as far as features need them: setting the
//! bot's status line, and how fast Discord answers.
//!
//! `main.rs` runs one task per shard that reads its events. Those tasks own the shards, so
//! this keeps what the rest of the bot needs: a sender per shard to send it commands, and
//! the latency each shard measured last. [`Reconnects`] helps those tasks decide which
//! drops are worth a line in the log channel.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::warn;
use twilight_gateway::{CloseFrame, MessageSender};
use twilight_model::gateway::CloseCode;
use twilight_model::gateway::payload::outgoing::UpdatePresence;
use twilight_model::gateway::presence::{Activity, ActivityType, Status};

pub struct Gateway {
    senders: Vec<MessageSender>,
    /// The average heartbeat round trip of each shard, `None` before the first one.
    latencies: Mutex<Vec<Option<Duration>>>,
}

impl Gateway {
    pub fn new(senders: Vec<MessageSender>) -> Gateway {
        let latencies = Mutex::new(vec![None; senders.len()]);
        Gateway { senders, latencies }
    }

    /// Called by the shard's task after each event.
    pub fn record_latency(&self, shard: usize, latency: Option<Duration>) {
        if let Some(slot) = self.latencies.lock().unwrap().get_mut(shard) {
            *slot = latency;
        }
    }

    /// Each shard's latency, `None` where none was measured yet.
    pub fn latencies(&self) -> Vec<Option<Duration>> {
        self.latencies.lock().unwrap().clone()
    }

    /// Sets the line under the bot's name (a custom status) on every shard.
    pub fn set_status(&self, text: &str) {
        let activity = Activity {
            application_id: None,
            assets: None,
            buttons: Vec::new(),
            created_at: None,
            details: None,
            emoji: None,
            flags: None,
            id: None,
            instance: None,
            kind: ActivityType::Custom,
            // Discord shows `state` for custom statuses; the name has to be there anyway.
            name: "Custom Status".to_string(),
            party: None,
            secrets: None,
            state: Some(text.to_string()),
            timestamps: None,
            url: None,
        };
        let presence = match UpdatePresence::new(vec![activity], false, None, Status::Online) {
            Ok(presence) => presence,
            Err(err) => {
                warn!("couldn't build the status {text:?}: {err}");
                return;
            }
        };
        for sender in &self.senders {
            if let Err(err) = sender.command(&presence) {
                warn!("couldn't set the status: {err}");
            }
        }
    }

    /// Closes every connection. Each shard's task then ends.
    pub fn close(&self) {
        for sender in &self.senders {
            let _ = sender.close(CloseFrame::NORMAL);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Drops and reconnects
// ---------------------------------------------------------------------------------------

/// This many drops within an hour is more than Discord's routine ones.
const STORM_DROPS: usize = 5;
/// Away longer than this is worth a warning when the bot is back.
const LONG_AWAY: Duration = Duration::from_secs(2 * 60);
const HOUR: Duration = Duration::from_secs(60 * 60);

/// One shard's drops and reconnects.
///
/// Discord drops every bot's connection now and then (usually code 4000, or a "please
/// reconnect"), and Twilight resumes a second later on its own. Those only get a line on
/// stdout. The log channel hears about the connection when:
/// - Discord drops it [`STORM_DROPS`] times within an hour (at most one warning an hour);
/// - the bot was away longer than [`LONG_AWAY`] before it got back;
/// - Discord closes it for good (see [`is_fatal`]).
#[derive(Debug, Default)]
pub struct Reconnects {
    /// When each drop in the last hour happened, oldest first.
    drops: VecDeque<Instant>,
    /// When the connection dropped, while it's down.
    away_since: Option<Instant>,
    /// When the last "drops a lot" warning went out.
    warned_at: Option<Instant>,
}

impl Reconnects {
    /// Discord closed the connection. Returns how many times it did in the last hour when
    /// that's enough for a warning.
    pub fn dropped(&mut self, now: Instant) -> Option<usize> {
        self.away_since.get_or_insert(now);
        while self
            .drops
            .front()
            .is_some_and(|&t| now.duration_since(t) > HOUR)
        {
            self.drops.pop_front();
        }
        self.drops.push_back(now);

        let warned_lately = self.warned_at.is_some_and(|t| now.duration_since(t) < HOUR);
        if self.drops.len() >= STORM_DROPS && !warned_lately {
            self.warned_at = Some(now);
            return Some(self.drops.len());
        }
        None
    }

    /// The bot is connected again (Ready or Resumed). Returns how long it was away, or `None`
    /// for the first connect.
    pub fn back(&mut self, now: Instant) -> Option<Duration> {
        self.away_since.take().map(|t| now.duration_since(t))
    }
}

/// Whether the bot was away long enough to say so.
pub fn long_away(away: Duration) -> bool {
    away > LONG_AWAY
}

/// Whether Discord closed the connection for good, so Twilight won't reconnect. Codes
/// Discord doesn't document (like 1006 when the network drops) get a reconnect.
pub fn is_fatal(frame: Option<&CloseFrame>) -> bool {
    frame
        .and_then(|frame| CloseCode::try_from(frame.code).ok())
        .is_some_and(|code| !code.can_reconnect())
}

/// A readable reason for a close: "4000 Unknown Error", "1006", "4014 Disallowed Intents:
/// ...". The ones that need something done get a hint.
pub fn close_reason(frame: Option<&CloseFrame>) -> String {
    let Some(frame) = frame else {
        return "no close code".to_string();
    };
    let mut text = match CloseCode::try_from(frame.code) {
        Ok(code) => format!("{} {code}", frame.code),
        Err(_) => frame.code.to_string(),
    };
    if !frame.reason.is_empty() {
        text = format!("{text} ({})", frame.reason);
    }
    match frame.code {
        4004 => text.push_str(": check DISCORD_TOKEN"),
        4014 => text
            .push_str(": turn on the Message Content and Presence intents in the Developer Portal"),
        _ => {}
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(code: u16, reason: &'static str) -> CloseFrame<'static> {
        CloseFrame::new(code, reason)
    }

    #[test]
    fn reasons_read_well() {
        assert_eq!(close_reason(Some(&frame(4000, ""))), "4000 Unknown Error");
        assert_eq!(close_reason(Some(&frame(1006, ""))), "1006");
        assert_eq!(
            close_reason(Some(&frame(4000, "resuming connection"))),
            "4000 Unknown Error (resuming connection)"
        );
        assert!(close_reason(Some(&frame(4014, ""))).contains("Developer Portal"));
        assert_eq!(close_reason(None), "no close code");
    }

    #[test]
    fn only_some_codes_are_fatal() {
        assert!(!is_fatal(Some(&frame(4000, ""))));
        assert!(!is_fatal(Some(&frame(4009, ""))));
        assert!(!is_fatal(Some(&frame(1000, ""))));
        assert!(!is_fatal(Some(&frame(1006, ""))));
        assert!(!is_fatal(None));
        assert!(is_fatal(Some(&frame(4004, ""))));
        assert!(is_fatal(Some(&frame(4014, ""))));
    }

    #[test]
    fn routine_drops_stay_quiet() {
        let start = Instant::now();
        let mut reconnects = Reconnects::default();
        assert_eq!(reconnects.back(start), None, "the first connect");
        // One drop every 90 minutes, back a second later: never a warning.
        for i in 0..10 {
            let t = start + Duration::from_secs(i * 90 * 60);
            assert_eq!(reconnects.dropped(t), None);
            let away = reconnects.back(t + Duration::from_secs(1)).unwrap();
            assert!(!long_away(away));
        }
    }

    #[test]
    fn many_drops_warn_once_an_hour() {
        let start = Instant::now();
        let mut reconnects = Reconnects::default();
        let minute = |m: u64| start + Duration::from_secs(m * 60);
        for m in 0..4 {
            assert_eq!(reconnects.dropped(minute(m)), None);
        }
        assert_eq!(reconnects.dropped(minute(4)), Some(5));
        // Still dropping, but it already said so.
        assert_eq!(reconnects.dropped(minute(5)), None);
        assert_eq!(reconnects.dropped(minute(30)), None);
        // Quiet for over an hour, then a new burst: say it again.
        for m in 100..104 {
            assert_eq!(reconnects.dropped(minute(m)), None);
        }
        assert_eq!(reconnects.dropped(minute(104)), Some(5));
    }

    #[test]
    fn a_long_time_away_is_noticed() {
        let start = Instant::now();
        let mut reconnects = Reconnects::default();
        reconnects.dropped(start);
        // Another drop before it's back doesn't restart the clock.
        reconnects.dropped(start + Duration::from_secs(60));
        let away = reconnects
            .back(start + Duration::from_secs(5 * 60))
            .unwrap();
        assert_eq!(away, Duration::from_secs(5 * 60));
        assert!(long_away(away));
        assert_eq!(reconnects.back(start + Duration::from_secs(6 * 60)), None);
    }
}
