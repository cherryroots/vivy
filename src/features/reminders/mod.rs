//! Reminders: "@Vivy remind me in 2h to check the oven".
//!
//! This file only wires the feature into the bot. The pieces live next to it:
//!
//! - `parse.rs`: the time grammar (pure, no Discord or database)
//! - `store.rs`: the tables and queries
//! - `scheduler.rs`: the background task that delivers reminders
//! - `commands.rs`: `/reminders` and `/timezone`
//! - `ui.rs`: message texts, buttons and the delete menu
//! - `import.rs`: reminders from voltgpt's database
//! - `tools.rs`: create, list and cancel reminders from chat

mod commands;
mod import;
mod parse;
mod scheduler;
mod store;
mod tools;
mod ui;

use std::sync::Arc;

use anyhow::{Context as _, bail};
use async_trait::async_trait;
use chrono::Utc;
use chrono_tz::Tz;
use serenity::all::{
    ActionRowComponent, ComponentInteraction, ComponentInteractionDataKind, CreateAllowedMentions,
    CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, Message,
    ModalInteraction, PremiumTier, UserId,
};
use tokio::sync::Notify;
use tracing::{info, warn};

use self::parse::{Repeat, When};
use self::store::{Image, NewReminder};
use self::ui::Action;
use crate::ai::ToolDef;
use crate::core::{
    Asker, BotCtx, Command, Feature, LegacyImport, Mention, Result, Stat, settings, user_error,
};

#[derive(Default)]
pub struct Reminders {
    /// Wakes the scheduler when a reminder is added or deleted.
    wake: Arc<Notify>,
}

#[async_trait]
impl Feature for Reminders {
    fn name(&self) -> &'static str {
        "reminders"
    }

    fn commands(&self) -> Vec<Command> {
        vec![commands::reminders(), commands::timezone()]
    }

    fn migrations(&self) -> &'static [&'static str] {
        store::MIGRATIONS
    }

    fn mention_prefixes(&self) -> &'static [&'static str] {
        &["remind me", "reminder", "remind"]
    }

    fn legacy_import(&self) -> Option<LegacyImport> {
        Some(LegacyImport {
            part: "reminders",
            run: import::import,
        })
    }

    fn tools(&self) -> Vec<ToolDef> {
        tools::defs()
    }

    fn tool_status(&self, tool: &str, _args: &serde_json::Value) -> Option<&'static str> {
        match tool {
            "create_reminder" => Some("⏰ Setting a reminder…"),
            "cancel_reminder" => Some("⏰ Cancelling a reminder…"),
            _ => Some("⏰ Checking reminders…"),
        }
    }

    async fn run_tool(
        &self,
        ctx: &BotCtx,
        asker: &Asker,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<String> {
        let (changed, text) = tools::run(ctx, asker, name, args).await?;
        if changed {
            self.wake.notify_one();
        }
        Ok(text)
    }

    async fn stats(&self, ctx: &BotCtx) -> Result<Vec<Stat>> {
        let stats = ctx.db.call(|conn| Ok(store::stats(conn)?)).await?;
        let mut lines = vec![Stat::new("Pending", stats.pending)];
        if let Some(next) = stats.next_fire_at {
            lines.push(Stat::new("Next", format!("<t:{next}:R>")));
        }
        lines.push(Stat::new("Sent this week", stats.sent_this_week));
        if stats.failing > 0 {
            lines.push(Stat::new("Failing to send", stats.failing));
        }
        Ok(lines)
    }

    async fn start(&self, ctx: &BotCtx) -> Result<()> {
        scheduler::spawn(ctx, self.wake.clone());
        Ok(())
    }

    /// "@Vivy remind me in 2h to check the oven": `rest` is "in 2h to check the oven".
    ///
    /// When no reminder can be made from it ("remind me what the movie was called", a time
    /// that has passed, no text), the message goes to chat instead, which can answer it or
    /// help set the reminder up with its reminder tools.
    async fn on_mention(&self, ctx: &BotCtx, msg: &Message, rest: &str) -> Result<Mention> {
        let parsed = match parse::parse_reminder(rest) {
            Ok(parsed) if !parsed.message.is_empty() => parsed,
            Ok(_) => {
                info!("no reminder text; passing the mention on");
                return Ok(Mention::PassOn);
            }
            Err(err) => {
                info!("not a reminder ({err}); passing the mention on");
                return Ok(Mention::PassOn);
            }
        };
        let saved_zone = settings::timezone(&ctx.db, msg.author.id).await?;
        let home = saved_zone.unwrap_or(Tz::UTC);
        let fire_at = match parse::resolve(&parsed.when, Utc::now(), home) {
            Ok(at) => at.timestamp(),
            Err(err) => {
                info!("can't set that reminder ({err}); passing the mention on");
                return Ok(Mention::PassOn);
            }
        };

        // Only keep what the bot can upload again in this server, which depends on its boosts.
        let tier = msg
            .guild_id
            .and_then(|id| ctx.cache.guild(id).map(|guild| guild.premium_tier));
        let (images, skipped) = download_images(msg, upload_limit(tier)).await;
        let new = NewReminder {
            user_id: msg.author.id.get(),
            channel_id: msg.channel_id.get(),
            guild_id: msg.guild_id.map(|g| g.get()),
            message: parsed.message.clone(),
            fire_at,
            created_at: Utc::now().timestamp(),
            source_message_id: Some(msg.id.get()),
            missing_images: skipped.len() as u32,
            images,
            repeat: match &parsed.when {
                When::Every(repeat) => Some(parse::describe_repeat(repeat, home)),
                _ => None,
            },
        };
        let repeat = new.repeat.clone();
        let image_count = new.images.len();
        let id = ctx
            .db
            .call(move |conn| Ok(store::add(conn, &new)?))
            .await
            .context("saving the reminder")?;
        self.wake.notify_one();
        info!(reminder = id, "reminder set for <t:{fire_at}:f>");

        // Mention /timezone when a clock time was read as UTC only because nothing was set.
        let zone_hint = saved_zone.is_none()
            && matches!(
                parsed.when,
                When::At { zone: None, .. } | When::Every(Repeat { zone: None, .. })
            );
        let reply = CreateMessage::new()
            .content(ui::confirmation(
                fire_at,
                &parsed.message,
                repeat.as_deref(),
                zone_hint,
                image_count,
                &skipped,
            ))
            .components(vec![ui::cancel_button(id)])
            .reference_message(msg)
            .allowed_mentions(CreateAllowedMentions::new());
        msg.channel_id.send_message(&ctx.http, reply).await?;
        Ok(Mention::Handled)
    }

    async fn on_component(
        &self,
        ctx: &BotCtx,
        i: &ComponentInteraction,
        action: &str,
    ) -> Result<()> {
        match Action::parse(action) {
            Some(Action::Delete) => self.delete(ctx, i).await,
            Some(Action::Cancel { id }) => self.cancel(ctx, i, id).await,
            Some(Action::Snooze { id, minutes }) => {
                let until = Utc::now().timestamp() + minutes * 60;
                self.snooze(ctx, i.user.id, id, until).await?;
                i.create_response(&ctx.http, snoozed_update(&i.message.content, until))
                    .await?;
                Ok(())
            }
            Some(Action::Tomorrow { id }) => {
                let zone = settings::timezone(&ctx.db, i.user.id).await?;
                let until = parse::tomorrow_morning(Utc::now(), zone.unwrap_or(Tz::UTC))
                    .map_err(user_error)?
                    .timestamp();
                self.snooze(ctx, i.user.id, id, until).await?;
                i.create_response(&ctx.http, snoozed_update(&i.message.content, until))
                    .await?;
                Ok(())
            }
            Some(Action::AskSnooze { id }) => {
                // Checked before the modal opens, so nobody types a time for nothing.
                self.snoozable(ctx, i.user.id, id).await?;
                i.create_response(
                    &ctx.http,
                    CreateInteractionResponse::Modal(ui::snooze_modal(id)),
                )
                .await?;
                Ok(())
            }
            Some(Action::SnoozeTo { .. }) | None => bail!("unknown action {action:?}"),
        }
    }

    async fn on_modal(&self, ctx: &BotCtx, i: &ModalInteraction, action: &str) -> Result<()> {
        let Some(Action::SnoozeTo { id }) = Action::parse(action) else {
            bail!("unknown modal {action:?}");
        };
        // The typed time, read like the time of a reminder, in the user's timezone.
        let typed = modal_value(i).unwrap_or_default();
        let when = parse::parse_when(&typed).map_err(user_error)?;
        if matches!(when, When::Every(_)) {
            return Err(user_error(
                "A snooze happens once. Set a new reminder for something that repeats.",
            ));
        }
        let zone = settings::timezone(&ctx.db, i.user.id).await?;
        let until = parse::resolve(&when, Utc::now(), zone.unwrap_or(Tz::UTC))
            .map_err(user_error)?
            .timestamp();
        self.snooze(ctx, i.user.id, id, until).await?;
        let original = i.message.as_ref().map_or("", |m| m.content.as_str());
        i.create_response(&ctx.http, snoozed_update(original, until))
            .await?;
        Ok(())
    }
}

impl Reminders {
    /// Deletes a pending reminder of `user` and wakes the scheduler. Returns whether there
    /// was one to delete.
    async fn remove(&self, ctx: &BotCtx, id: i64, user: UserId) -> Result<bool> {
        let user = user.get();
        let deleted = ctx
            .db
            .call(move |conn| Ok(store::delete_pending(conn, id, user)?))
            .await?;
        self.wake.notify_one();
        Ok(deleted)
    }

    /// The `/reminders` delete menu. The list stays open, reloaded, for the next delete.
    async fn delete(&self, ctx: &BotCtx, i: &ComponentInteraction) -> Result<()> {
        let ComponentInteractionDataKind::StringSelect { values } = &i.data.kind else {
            bail!("the delete menu sent no values");
        };
        let id: i64 = values.first().context("no reminder selected")?.parse()?;
        let deleted = self.remove(ctx, id, i.user.id).await?;
        let note = if deleted {
            "✅ Reminder deleted."
        } else {
            "That reminder is already gone."
        };
        let user = i.user.id.get();
        let list = ctx
            .db
            .call(move |conn| Ok(store::list_pending(conn, user)?))
            .await?;
        let zone = settings::timezone(&ctx.db, i.user.id)
            .await?
            .unwrap_or(Tz::UTC);
        let (text, menu) = ui::list_message(&list, Some(note), zone);
        let response = CreateInteractionResponseMessage::new()
            .content(text)
            .components(menu);
        i.create_response(
            &ctx.http,
            CreateInteractionResponse::UpdateMessage(response),
        )
        .await?;
        Ok(())
    }

    /// The Cancel button under "I'll remind you": deletes that reminder, if it's still
    /// pending and the one pressing it set it.
    async fn cancel(&self, ctx: &BotCtx, i: &ComponentInteraction, id: i64) -> Result<()> {
        let reminder = ctx.db.call(move |conn| Ok(store::get(conn, id)?)).await?;
        if let Some(reminder) = &reminder
            && reminder.user_id != i.user.id.get()
        {
            return Err(user_error(format!(
                "Only <@{}> can cancel this reminder.",
                reminder.user_id
            )));
        }
        if !self.remove(ctx, id, i.user.id).await? {
            return Err(user_error(
                "That reminder has already gone out or been deleted. Anything still pending is in /reminders.",
            ));
        }
        info!(reminder = id, "reminder cancelled");
        let response = CreateInteractionResponseMessage::new()
            .content(ui::cancelled(&i.message.content))
            .components(Vec::new());
        i.create_response(
            &ctx.http,
            CreateInteractionResponse::UpdateMessage(response),
        )
        .await?;
        Ok(())
    }

    /// Checks that `user` can snooze delivered reminder `id`.
    async fn snoozable(&self, ctx: &BotCtx, user: UserId, id: i64) -> Result<()> {
        let reminder = ctx
            .db
            .call(move |conn| Ok(store::get(conn, id)?))
            .await?
            .ok_or_else(|| user_error("This reminder is too old to snooze."))?;
        if reminder.user_id != user.get() {
            return Err(user_error(format!(
                "Only <@{}> can snooze this reminder.",
                reminder.user_id
            )));
        }
        Ok(())
    }

    /// A snooze: the same delivered reminder again at `until`. The caller swaps the
    /// buttons for a "snoozed until" line.
    async fn snooze(&self, ctx: &BotCtx, user: UserId, id: i64, until: i64) -> Result<()> {
        self.snoozable(ctx, user, id).await?;
        // Snoozing moves the delivered reminder (images and all) back into the queue. It only
        // works once, so a double press or a retry can't make two reminders.
        let user = user.get();
        let snoozed = ctx
            .db
            .call(move |conn| Ok(store::snooze(conn, id, user, until)?))
            .await?;
        if !snoozed {
            return Err(user_error("This reminder is already snoozed."));
        }
        self.wake.notify_one();
        Ok(())
    }
}

/// The delivered reminder after a snooze: its buttons swapped for a "snoozed until" line.
fn snoozed_update(original: &str, until: i64) -> CreateInteractionResponse {
    let response = CreateInteractionResponseMessage::new()
        .content(ui::snoozed(original, until))
        .components(Vec::new());
    CreateInteractionResponse::UpdateMessage(response)
}

/// The text typed into a modal (each reminder modal has one field).
fn modal_value(i: &ModalInteraction) -> Option<String> {
    i.data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|component| match component {
            ActionRowComponent::InputText(input) => input.value.clone(),
            _ => None,
        })
}

/// The biggest file the bot can upload in a server with this boost level, in bytes.
/// DMs (`None`) get the base limit.
fn upload_limit(tier: Option<PremiumTier>) -> u32 {
    const MIB: u32 = 1024 * 1024;
    match tier {
        Some(PremiumTier::Tier2) => 50 * MIB,
        Some(PremiumTier::Tier3) => 100 * MIB,
        // Raised from 10 MiB for bots in September 2026.
        _ => 20 * MIB,
    }
}

/// Downloads the images (and videos) attached to the message. Returns them and the names
/// of the ones that couldn't be kept: bigger than `max_bytes`, or the download failed.
async fn download_images(msg: &Message, max_bytes: u32) -> (Vec<Image>, Vec<String>) {
    let mut images = Vec::new();
    let mut skipped = Vec::new();
    // Discord only sets a width on images and videos.
    for attachment in msg.attachments.iter().filter(|a| a.width.is_some()) {
        if attachment.size > max_bytes {
            skipped.push(attachment.filename.clone());
            continue;
        }
        match attachment.download().await {
            Ok(data) => images.push(Image {
                filename: attachment.filename.clone(),
                data,
            }),
            Err(err) => {
                warn!("couldn't download {}: {err}", attachment.filename);
                skipped.push(attachment.filename.clone());
            }
        }
    }
    (images, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_limit_follows_boosts() {
        let mib = 1024 * 1024;
        assert_eq!(upload_limit(None), 20 * mib);
        assert_eq!(upload_limit(Some(PremiumTier::Tier1)), 20 * mib);
        assert_eq!(upload_limit(Some(PremiumTier::Tier2)), 50 * mib);
        assert_eq!(upload_limit(Some(PremiumTier::Tier3)), 100 * mib);
    }
}
