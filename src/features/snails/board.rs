//! `/snails`, with the subcommands `top`, `recent` and `reset` (here) and `backfill` (in
//! `backfill.rs`).
//!
//! `/snails top`: the server's top snailers, this week and of all time. Counts the new messages
//! that repeated an earlier post (`snail_caught`), not what the backfill found in old history.
//! `/snails recent`: the last snails counted, with links to each one and to what it repeated.
//! `/snails reset` (admins): clears those counts for a fresh start.

use anyhow::Context as _;
use chrono::Utc;
use tracing::info;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_util::builder::embed::EmbedBuilder;

use super::backfill::{SnailsBackfill, snail_backfill};
use super::store::{self, RecentCatch};
use crate::core::{Command, CommandCtx, Result, user_error};
use crate::util::count;
use crate::util::discord::{Draft, no_pings};

/// How many people each list shows.
const TOP: usize = 10;
/// How many snails `/snails recent` shows.
const RECENT: usize = 10;
const WEEK_SECS: i64 = 7 * 24 * 60 * 60;

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "snails",
    desc = "Reposted links and pictures",
    contexts = "guild"
)]
pub enum SnailsCommand {
    #[command(name = "top")]
    Top(SnailsTop),
    #[command(name = "recent")]
    Recent(SnailsRecent),
    #[command(name = "reset")]
    Reset(SnailsReset),
    #[command(name = "backfill")]
    Backfill(SnailsBackfill),
}

pub fn snails() -> Command {
    Command::slash::<SnailsCommand>(|ctx| Box::pin(run(ctx)))
}

async fn run(ctx: CommandCtx) -> Result<()> {
    let guild = ctx
        .guild_id()
        .context("server-only command without a server")?
        .get();
    match ctx.parse::<SnailsCommand>()? {
        SnailsCommand::Top(_) => top(ctx, guild).await,
        SnailsCommand::Recent(_) => recent(ctx, guild).await,
        // The admin commands answer privately.
        SnailsCommand::Reset(_) => reset(ctx.ephemeral(), guild).await,
        SnailsCommand::Backfill(args) => snail_backfill(ctx.ephemeral(), args).await,
    }
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "top", desc = "Who reposts the most in this server")]
pub struct SnailsTop;

async fn top(ctx: CommandCtx, guild: u64) -> Result<()> {
    let week_ago = Utc::now().timestamp() - WEEK_SECS;
    let (week, all_time) = ctx
        .bot
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
        Draft::new()
            .content(board(&week, &all_time))
            .allowed_mentions(no_pings()),
    )
    .await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "recent",
    desc = "The last snails caught in this server, with links"
)]
pub struct SnailsRecent;

async fn recent(ctx: CommandCtx, guild: u64) -> Result<()> {
    let catches = ctx
        .bot
        .db
        .call(move |conn| Ok(store::recent_caught(conn, guild, RECENT)?))
        .await?;
    if catches.is_empty() {
        ctx.say("🐌 No snails caught here yet.").await?;
        return Ok(());
    }
    // An embed: ten lines of links are too long for a plain message (2000 characters), and
    // mentions in an embed never ping anyone.
    let embed = EmbedBuilder::new()
        .title(format!("🐌 Last {}", count(catches.len(), "snail")))
        .description(recent_list(guild, &catches))
        .build();
    ctx.send(Draft::new().embed(embed)).await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "reset", desc = "Start the leaderboard over (admins)")]
pub struct SnailsReset;

async fn reset(ctx: CommandCtx, guild: u64) -> Result<()> {
    if !ctx.bot.is_admin(ctx.author_id()) {
        return Err(user_error("Only admins can use this command."));
    }
    let cleared = ctx
        .bot
        .db
        .call(move |conn| Ok(store::reset_caught(conn, guild)?))
        .await?;
    info!(guild, cleared, "snail leaderboard reset");
    ctx.say(format!(
        "Reset: cleared {}. Everyone starts from zero. \
         Saved links and pictures stay, so reposts of older posts still count.",
        count(cleared, "snail")
    ))
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
         Counted since Vivy started watching, or since the last reset.",
    );
    text
}

/// The `/snails recent` list, one line per snail, newest first.
fn recent_list(guild: u64, catches: &[RecentCatch]) -> String {
    let mut text = String::new();
    for (n, c) in catches.iter().enumerate() {
        // Masked links keep each line short on a phone.
        let message = match c.channel {
            Some(channel) => format!(
                "[message](https://discord.com/channels/{guild}/{channel}/{}) in <#{channel}>",
                c.message
            ),
            None => "message deleted".to_string(),
        };
        let original = match c.original {
            Some((channel, id)) => {
                format!(", [first post](https://discord.com/channels/{guild}/{channel}/{id})")
            }
            None => String::new(),
        };
        text.push_str(&format!(
            "{}. <t:{}:R> <@{}>: {message}{original}\n",
            n + 1,
            c.at,
            c.author
        ));
    }
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
    fn subcommands_have_short_names() {
        let snails = snails().definition;
        let names: Vec<&str> = snails.options.iter().map(|c| c.name.as_ref()).collect();
        assert_eq!(snails.name, "snails");
        assert_eq!(names, ["top", "recent", "reset", "backfill"]);
    }

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

    #[test]
    fn recent_text() {
        let catches = [
            RecentCatch {
                message: 11,
                channel: Some(20),
                author: 7,
                at: 3000,
                original: Some((10, 1)),
            },
            RecentCatch {
                message: 5,
                channel: None,
                author: 8,
                at: 2000,
                original: None,
            },
        ];
        let text = recent_list(4, &catches);
        assert_eq!(
            text,
            "1. <t:3000:R> <@7>: [message](https://discord.com/channels/4/20/11) in <#20>, \
             [first post](https://discord.com/channels/4/10/1)\n\
             2. <t:2000:R> <@8>: message deleted\n"
        );
        // Ten of the longest lines (snowflake ids) fit in an embed's description.
        let big = RecentCatch {
            message: u64::MAX,
            channel: Some(u64::MAX),
            author: u64::MAX,
            at: 1_800_000_000,
            original: Some((u64::MAX, u64::MAX)),
        };
        let text = recent_list(u64::MAX, &vec![big; RECENT]);
        assert!(text.chars().count() < 4096, "{}", text.chars().count());
    }
}
