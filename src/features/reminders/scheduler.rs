//! The one background task that delivers reminders.
//!
//! It sends whatever is due, then sleeps until the next reminder (at most an hour). Adding or
//! deleting a reminder wakes it early through `wake`, so it never sleeps past a new one.
//! A reminder is only marked sent after Discord accepted it. If Discord says the message with
//! images is too big, it's sent again without them. Any other failed send is retried later
//! with a growing delay.

use std::sync::Arc;
use std::time::Duration;

use crate::util::discord::{ChannelId, UserId, error_code, error_status};
use anyhow::Context as _;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use tokio::sync::Notify;
use tracing::{Instrument as _, error, info, info_span, warn};

use super::parse;
use super::store::{self, Reminder};
use super::ui;
use crate::core::{BotCtx, BotEvent, Result, settings};

/// Failed sends before a reminder is dropped (about 8 hours of retrying).
const MAX_ATTEMPTS: u32 = 10;
/// How long delivered reminders are kept, so their snooze buttons keep working.
const KEEP_SENT_SECS: i64 = 7 * 24 * 60 * 60;

pub fn spawn(ctx: &BotCtx, wake: Arc<Notify>) {
    let ctx = ctx.clone();
    let span = info_span!("scheduler", feature = "reminders");
    ctx.tasks.clone().spawn(run(ctx, wake).instrument(span));
}

async fn run(ctx: BotCtx, wake: Arc<Notify>) {
    let timer = ctx.timers.add("Reminders", "at the next reminder");
    loop {
        timer.running();
        if let Err(err) = send_due(&ctx).await {
            error!("sending due reminders: {err:#}");
        }
        let sleep = match ctx.db.call(|conn| Ok(store::next_try_at(conn)?)).await {
            Ok(next) => sleep_duration(next, Utc::now().timestamp()),
            Err(err) => {
                error!("finding the next reminder: {err:#}");
                Duration::from_secs(60)
            }
        };
        timer.sleeping(sleep);
        tokio::select! {
            () = tokio::time::sleep(sleep) => {}
            () = wake.notified() => {}
            () = ctx.shutdown.cancelled() => break,
        }
    }
}

async fn send_due(ctx: &BotCtx) -> Result<()> {
    let now = Utc::now().timestamp();
    let due = ctx
        .db
        .call(move |conn| {
            store::purge_sent(conn, now - KEEP_SENT_SECS)?;
            Ok(store::due(conn, now)?)
        })
        .await?;
    for reminder in due {
        let id = reminder.id;
        deliver(ctx, reminder)
            .await
            .with_context(|| format!("reminder {id}"))?;
    }
    Ok(())
}

async fn deliver(ctx: &BotCtx, mut reminder: Reminder) -> Result<()> {
    let now = Utc::now().timestamp();
    let id = reminder.id;
    // Loaded now rather than with the list of due reminders, so only one reminder's
    // pictures are in memory at a time.
    reminder.images = ctx
        .db
        .call(move |conn| Ok(store::images(conn, id)?))
        .await?;
    let channel = ChannelId::new(reminder.channel_id);
    // The owner's timezone, so a repeat reads without the zone's name for them.
    let zone = settings::timezone(&ctx.db, UserId::new(reminder.user_id))
        .await?
        .unwrap_or(Tz::UTC);

    let mut sent = ui::fired_message(&reminder, now, true, zone)
        .send(&ctx.http, channel)
        .await;
    // If the images were the problem (too big for the server now, say), send it without
    // them. The reminder links to the original message, which still has them.
    if let Err(err) = &sent
        && too_big(err)
        && !reminder.images.is_empty()
    {
        warn!(
            reminder = id,
            "couldn't send a reminder with its images, sending it without: {err}"
        );
        sent = ui::fired_message(&reminder, now, false, zone)
            .send(&ctx.http, channel)
            .await;
    }

    // A recurring reminder queues its next time either way, so one failed delivery doesn't
    // end the series.
    let next = next_time(&reminder, now);
    match sent {
        Ok(_) => {
            let fired = reminder.clone();
            ctx.db
                .call(move |conn| {
                    let tx = conn.transaction()?;
                    store::mark_sent(&tx, id, now)?;
                    if let Some(next) = next {
                        store::repeat(&tx, &fired, next)?;
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await?;
            info!(reminder = id, "reminder delivered");
            ctx.publish(BotEvent::ReminderFired {
                reminder_id: id,
                user_id: UserId::new(reminder.user_id),
                channel_id: channel,
            });
        }
        Err(err) => {
            let attempts = reminder.attempts + 1;
            if attempts >= MAX_ATTEMPTS {
                error!(
                    reminder = id,
                    user = reminder.user_id,
                    channel = reminder.channel_id,
                    "gave up on a reminder after {attempts} failed sends ({err}). It said: {}",
                    reminder.message
                );
                let fired = reminder.clone();
                ctx.db
                    .call(move |conn| {
                        let tx = conn.transaction()?;
                        store::delete(&tx, id)?;
                        if let Some(next) = next {
                            store::repeat(&tx, &fired, next)?;
                        }
                        tx.commit()?;
                        Ok(())
                    })
                    .await?;
            } else {
                let next = now + retry_delay(attempts);
                warn!(
                    reminder = id,
                    channel = reminder.channel_id,
                    "couldn't send a reminder (attempt {attempts}), retrying <t:{next}:R>: {err}"
                );
                ctx.db
                    .call(move |conn| Ok(store::mark_failed(conn, id, next)?))
                    .await?;
            }
        }
    }
    Ok(())
}

/// When a recurring reminder comes up next: after the time it was due, or after `now` if
/// the bot was offline past that. `None` for a one-off reminder (or a repeat that can't be
/// read, which is logged).
fn next_time(reminder: &Reminder, now: i64) -> Option<i64> {
    let text = reminder.repeat.as_deref()?;
    let Some(repeat) = parse::parse_repeat(text) else {
        warn!(
            reminder = reminder.id,
            "can't read the repeat {text:?}; not repeating"
        );
        return None;
    };
    // The saved text names its zone wherever it matters, so the home zone is never used.
    let after = DateTime::from_timestamp(reminder.fire_at.max(now), 0)?;
    match parse::next_repeat(&repeat, after, Tz::UTC) {
        Ok(next) => Some(next.timestamp()),
        Err(err) => {
            warn!(
                reminder = reminder.id,
                "can't find the next time of {text:?}: {err}"
            );
            None
        }
    }
}

/// Whether Discord refused the message for being too big: HTTP 413, or Discord's error
/// 40005 ("Request entity too large"). Other errors, like a timeout, aren't the images' fault.
fn too_big(err: &anyhow::Error) -> bool {
    error_status(err) == Some(413) || error_code(err) == Some(40005)
}

/// How long to sleep when the next reminder is at `next`. At most an hour, so delivered
/// reminders get purged and a changed system clock is noticed.
fn sleep_duration(next: Option<i64>, now: i64) -> Duration {
    let secs = next.map_or(3600, |next| (next - now).clamp(1, 3600));
    Duration::from_secs(secs.unsigned_abs())
}

/// Seconds to wait after the `attempts`-th failed send: 1 min, 2, 4, 8, ... up to 6 hours.
fn retry_delay(attempts: u32) -> i64 {
    let doubled = 60_i64 << attempts.saturating_sub(1).min(16);
    doubled.min(6 * 60 * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleeps() {
        assert_eq!(sleep_duration(None, 0), Duration::from_secs(3600));
        assert_eq!(sleep_duration(Some(90), 30), Duration::from_secs(60));
        assert_eq!(sleep_duration(Some(10), 30), Duration::from_secs(1));
        assert_eq!(sleep_duration(Some(99_999), 0), Duration::from_secs(3600));
    }

    #[test]
    fn recurring_reminders_come_back() {
        let mut reminder = Reminder {
            id: 1,
            user_id: 2,
            channel_id: 3,
            guild_id: None,
            message: "trash".into(),
            // Monday 2026-02-23 09:00 UTC.
            fire_at: 1_771_837_200,
            created_at: 0,
            attempts: 0,
            source_message_id: None,
            missing_images: 0,
            images: Vec::new(),
            repeat: None,
        };
        assert_eq!(next_time(&reminder, reminder.fire_at), None);
        reminder.repeat = Some("every monday at 9:00 UTC".into());
        let week = 7 * 24 * 60 * 60;
        assert_eq!(
            next_time(&reminder, reminder.fire_at),
            Some(reminder.fire_at + week)
        );
        // Sent two weeks late: the next one is still ahead, not in the past.
        let late = reminder.fire_at + 2 * week + 60;
        assert_eq!(
            next_time(&reminder, late),
            Some(reminder.fire_at + 3 * week)
        );
        reminder.repeat = Some("gibberish".into());
        assert_eq!(next_time(&reminder, reminder.fire_at), None);
    }

    #[test]
    fn retry_delays_grow() {
        assert_eq!(retry_delay(1), 60);
        assert_eq!(retry_delay(2), 120);
        assert_eq!(retry_delay(5), 960);
        assert_eq!(retry_delay(30), 6 * 60 * 60);
        let total: i64 = (1..MAX_ATTEMPTS).map(retry_delay).sum();
        assert!(
            total > 6 * 60 * 60,
            "retries should cover several hours, got {total}s"
        );
    }
}
