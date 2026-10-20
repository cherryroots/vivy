//! Control panel: a status message in a Discord channel that the bot keeps up to date.
//!
//! Its owner mode button turns on whether Vivy follows her owner's messages (see chat).
//!
//! The other half of the control panel, the log channel, is part of the core logging setup
//! (`core::logging`), so it also works when this feature is turned off.

mod history;
mod render;
mod status;

use crate::util::discord::{ChannelId, Draft};
use async_trait::async_trait;
use serde::Deserialize;
use tracing::{Instrument as _, info, info_span};

use crate::core::{
    BotCtx, ComponentInteraction, Feature, Respond, Response, Result, settings, user_error,
};

#[derive(Debug, Deserialize)]
#[serde(default)]
struct Settings {
    /// Channel for the status message. Without one this feature does nothing.
    status_channel: Option<u64>,
    /// Seconds between refreshes. The old key name `status_interval_secs` still works.
    #[serde(alias = "status_interval_secs")]
    status_interval_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            status_channel: None,
            status_interval_seconds: 60,
        }
    }
}

#[derive(Default)]
pub struct ControlPanel;

#[async_trait]
impl Feature for ControlPanel {
    fn name(&self) -> &'static str {
        "control_panel"
    }

    fn migrations(&self) -> &'static [&'static str] {
        status::MIGRATIONS
    }

    async fn start(&self, ctx: &BotCtx) -> Result<()> {
        let settings: Settings = ctx.config.feature(self.name())?;
        let Some(channel) = settings.status_channel else {
            return Ok(());
        };
        // Discord allows about 5 edits per 5 seconds; stay far below that.
        let interval = settings.status_interval_seconds.max(10);
        let span = info_span!("status", feature = "control_panel");
        let task = status::run(ctx.clone(), ChannelId::new(channel), interval).instrument(span);
        ctx.tasks.spawn(task);
        Ok(())
    }

    /// The owner mode button: turns owner mode on or off, for her owner only.
    async fn on_component(
        &self,
        ctx: &BotCtx,
        i: &ComponentInteraction,
        action: &str,
    ) -> Result<()> {
        if action != status::OWNER_MODE_ACTION {
            return Ok(());
        }
        if !ctx.is_owner(i.user.id) {
            return Err(user_error("Only Vivy's owner can use this button."));
        }
        let on = !settings::owner_mode(&ctx.db).await?;
        settings::set_owner_mode(&ctx.db, on).await?;
        info!(on, "owner mode turned {}", if on { "on" } else { "off" });
        // Only the button changes: a draft without files leaves the picture as it is.
        let buttons = Draft::new().components(status::owner_mode_buttons(on));
        i.create_response(ctx, Response::UpdateMessage(buttons))
            .await?;
        Ok(())
    }
}
