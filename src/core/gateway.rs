//! The live connections to Discord ("shards"), as far as features need them: setting the
//! bot's status line, and how fast Discord answers.
//!
//! `main.rs` runs one task per shard that reads its events. Those tasks own the shards, so
//! this keeps what the rest of the bot needs: a sender per shard to send it commands, and
//! the latency each shard measured last.

use std::sync::Mutex;
use std::time::Duration;

use twilight_gateway::{CloseFrame, MessageSender};
use twilight_model::gateway::payload::outgoing::UpdatePresence;
use twilight_model::gateway::presence::{Activity, ActivityType, Status};
use tracing::warn;

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

    pub fn shard_count(&self) -> usize {
        self.senders.len()
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
