//! Slash commands: `/reminders` and `/timezone`.

use chrono::Utc;
use poise::CreateReply;
use serenity::all::{AutocompleteChoice, CreateAutocompleteResponse};

use super::{parse, store, ui};
use crate::core::{Context, Result, settings, user_error};

/// List your pending reminders and delete them
#[poise::command(slash_command, ephemeral)]
pub async fn reminders(ctx: Context<'_>) -> Result<()> {
    let user = ctx.author().id.get();
    let list = ctx
        .data()
        .db
        .call(move |conn| Ok(store::list_pending(conn, user)?))
        .await?;
    let zone = settings::timezone(&ctx.data().db, ctx.author().id)
        .await?
        .unwrap_or(chrono_tz::Tz::UTC);
    let (text, menu) = ui::list_message(&list, None, zone);
    ctx.send(CreateReply::default().content(text).components(menu))
        .await?;
    Ok(())
}

/// Set your timezone, used for reminders and when Vivy talks about times
#[poise::command(slash_command, ephemeral)]
pub async fn timezone(
    ctx: Context<'_>,
    #[description = "A zone like Europe/Oslo or an abbreviation like CET. Leave empty to see yours."]
    #[autocomplete = "autocomplete_zone"]
    zone: Option<String>,
) -> Result<()> {
    let db = &ctx.data().db;
    let user = ctx.author().id;

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
async fn autocomplete_zone(_ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let partial = partial.to_lowercase();
    let short = parse::ZONE_ABBREVIATIONS
        .iter()
        .copied()
        .filter(|name| name.to_lowercase().starts_with(&partial));
    let long = chrono_tz::TZ_VARIANTS
        .iter()
        .map(|zone| zone.name())
        .filter(|name| name.contains('/') && name.to_lowercase().contains(&partial));
    let choices = short
        .chain(long)
        .take(25)
        .map(AutocompleteChoice::from)
        .collect();
    CreateAutocompleteResponse::new().set_choices(choices)
}
