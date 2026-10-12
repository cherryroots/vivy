//! `/snails`: the server's top snailers, this week and of all time. Counts the new messages
//! that repeated an earlier post (`snail_caught`), not what the backfill found in old history.

use anyhow::Context as _;
use chrono::Utc;
use poise::CreateReply;
use serenity::all::CreateAllowedMentions;

use super::store;
use crate::core::{Context, Result};

/// How many people each list shows.
const TOP: usize = 10;
const WEEK_SECS: i64 = 7 * 24 * 60 * 60;

/// Who reposts the most in this server
#[poise::command(slash_command, guild_only)]
pub async fn snails(ctx: Context<'_>) -> Result<()> {
    let guild = ctx
        .guild_id()
        .context("guild_only command without a server")?
        .get();
    let week_ago = Utc::now().timestamp() - WEEK_SECS;
    let (week, all_time) = ctx
        .data()
        .db
        .call(move |conn| {
            Ok((
                store::top_snailers(conn, guild, week_ago, TOP)?,
                store::top_snailers(conn, guild, 0, TOP)?,
            ))
        })
        .await?;
    // Mentions show names without pinging anyone.
    ctx.send(
        CreateReply::default()
            .content(board(&week, &all_time))
            .allowed_mentions(CreateAllowedMentions::new()),
    )
    .await?;
    Ok(())
}

/// The leaderboard text from two lists of (user, snails).
fn board(week: &[(u64, u64)], all_time: &[(u64, u64)]) -> String {
    if all_time.is_empty() {
        return "🐌 No snails caught here yet.".to_string();
    }
    let mut text = String::from("## 🐌 Snail leaderboard\n**This week**\n");
    text.push_str(&list(week));
    text.push_str("\n**All time**\n");
    text.push_str(&list(all_time));
    text.push_str(
        "\n-# A snail is a link or picture the server had already seen. \
         Counted since Vivy started watching.",
    );
    text
}

fn list(rows: &[(u64, u64)]) -> String {
    if rows.is_empty() {
        return "Nobody yet.\n".to_string();
    }
    let mut text = String::new();
    for (n, (user, count)) in rows.iter().enumerate() {
        // Medals for the top three, then plain numbers (a small square glyph is blank on some
        // phone fonts).
        let place = match n {
            0 => "🥇".to_string(),
            1 => "🥈".to_string(),
            2 => "🥉".to_string(),
            _ => format!("{}.", n + 1),
        };
        let snails = if *count == 1 { "snail" } else { "snails" };
        text.push_str(&format!("{place} <@{user}>: {count} {snails}\n"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_text() {
        assert_eq!(board(&[], &[]), "🐌 No snails caught here yet.");
        let text = board(&[], &[(7, 3), (8, 1)]);
        assert!(text.contains("**This week**\nNobody yet.\n"), "{text}");
        assert!(
            text.contains("🥇 <@7>: 3 snails\n🥈 <@8>: 1 snail\n"),
            "{text}"
        );
        assert!(text.chars().count() < 2000);
    }
}
