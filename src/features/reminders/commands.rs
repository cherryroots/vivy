//! Slash commands: `/reminders` and `/timezone`.

use chrono::Utc;
use twilight_interactions::command::{CommandModel, CreateCommand};

use super::{parse, store, ui};
use crate::core::interaction::{Choice, Typed, choice};
use crate::core::{Command, CommandCtx, Result, settings, user_error};
use crate::util::discord::Draft;

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "reminders",
    desc = "List your pending reminders and delete them"
)]
pub struct RemindersCommand;

pub fn reminders() -> Command {
    Command::slash::<RemindersCommand>(|ctx| Box::pin(list(ctx)))
}

async fn list(ctx: CommandCtx) -> Result<()> {
    let ctx = ctx.ephemeral();
    let user = ctx.author_id().get();
    let list = ctx
        .bot
        .db
        .call(move |conn| Ok(store::list_pending(conn, user)?))
        .await?;
    let zone = settings::timezone(&ctx.bot.db, ctx.author_id())
        .await?
        .unwrap_or(chrono_tz::Tz::UTC);
    let (text, menu) = ui::list_message(&list, None, zone);
    ctx.send(Draft::new().content(text).components(menu))
        .await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "timezone",
    desc = "Set your timezone, used for reminders and when Vivy talks about times"
)]
pub struct TimezoneCommand {
    /// A zone like Europe/Oslo or an abbreviation like CET. Leave empty to see yours.
    #[command(autocomplete = true)]
    zone: Option<String>,
}

pub fn timezone() -> Command {
    Command::slash::<TimezoneCommand>(|ctx| Box::pin(set_timezone(ctx)))
        .autocomplete(|_, typed| Box::pin(autocomplete_zone(typed)))
}

async fn set_timezone(ctx: CommandCtx) -> Result<()> {
    let ctx = ctx.ephemeral();
    let TimezoneCommand { zone } = ctx.parse()?;
    let db = &ctx.bot.db;
    let user = ctx.author_id();

    let Some(name) = zone else {
        let text = match settings::timezone(db, user).await? {
            Some(zone) => format!("Your timezone is **{zone}**. {}", local_time(zone)),
            None => "You haven't set a timezone, so reminders use UTC.".to_string(),
        };
        ctx.say(text).await?;
        return Ok(());
    };

    let zone = parse::lookup_zone(name.trim()).ok_or_else(|| {
        user_error(format!(
            "I don't know the timezone \"{name}\". Pick one from the list, like Europe/Oslo."
        ))
    })?;
    settings::set_timezone(db, user, zone).await?;
    // Echo what was typed when it was an abbreviation, so "CET" doesn't just come back as
    // "Europe/Paris" with no explanation.
    let typed = name.trim().to_ascii_uppercase();
    let shown = if typed == zone.name().to_ascii_uppercase() {
        format!("**{zone}**")
    } else {
        format!("**{typed}** ({zone})")
    };
    ctx.say(format!(
        "Your timezone is now {shown}. {}",
        local_time(zone)
    ))
    .await?;
    Ok(())
}

fn local_time(zone: chrono_tz::Tz) -> String {
    format!(
        "It's {} there now.",
        Utc::now().with_timezone(&zone).format("%H:%M")
    )
}

/// Suggests zone names containing what the user typed so far: the abbreviations Vivy knows
/// ("CET") first, then the long names ("Europe/Oslo").
async fn autocomplete_zone(typed: Typed) -> Result<Vec<Choice>> {
    let partial = typed.value.to_lowercase();
    let short = parse::ZONE_ABBREVIATIONS
        .iter()
        .copied()
        .filter(|name| name.to_lowercase().starts_with(&partial));
    let long = chrono_tz::TZ_VARIANTS
        .iter()
        .map(|zone| zone.name())
        .filter(|name| name.contains('/') && name.to_lowercase().contains(&partial));
    Ok(short
        .chain(long)
        .take(25)
        .map(|name| choice(name, name))
        .collect())
}
