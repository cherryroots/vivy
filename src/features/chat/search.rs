//! `search_messages`: Discord's message search across the server, with every filter the
//! search API has (see `util::search`), limited to the channels chat may read (`tools::can_read`).
//!
//! The model names people, channels and roles the way people do (a mention, an ID or a
//! name); this file turns them into IDs, and dates in the asker's timezone into message
//! IDs.

use std::collections::HashMap;

use chrono::{NaiveDate, TimeZone as _};
use chrono_tz::Tz;
use serde_json::{Value, json};

use super::tools::{can_read, format_message, home_zone, message_link, parse_link};
use crate::ai::ToolDef;
use crate::core::{Asker, BotCtx, Result, user_error};
use crate::util::discord::{ChannelId, GuildId};
use crate::util::people::{find_member, parse_user_id};
use crate::util::search::{self, AUTHOR_TYPES, HAS_TYPES, MAX_OFFSET, Search, Sort};

/// How many results when the model doesn't say.
const DEFAULT_LIMIT: u32 = 10;

pub fn def() -> ToolDef {
    let negatable = |kinds: &[&str]| -> Vec<String> {
        kinds
            .iter()
            .flat_map(|k| [k.to_string(), format!("-{k}")])
            .collect()
    };
    let people = |what: &str| {
        json!({
            "type": "array",
            "items": {"type": "string"},
            "description": format!("{what}: mentions, IDs or names.")
        })
    };
    let texts =
        |what: &str| json!({"type": "array", "items": {"type": "string"}, "description": what});
    ToolDef {
        name: "search_messages",
        description: "Searches the whole server's messages, like Discord's search bar (only channels you may read). Give at least one filter. The answer starts with the match count; `offset` pages. Include its message links when pointing people at messages.",
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Words to find in the message text."},
                "from": people("By these people"),
                "author_type": {
                    "type": "array",
                    "items": {"type": "string", "enum": negatable(AUTHOR_TYPES)},
                    "description": "Author kinds; -bot leaves bots out."
                },
                "channel": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Channels or threads: mentions, IDs, names, or \"here\"."
                },
                "mentions": people("Mentioning these people"),
                "mentions_role": texts("Mentioning these roles: mentions, IDs or names."),
                "replied_to": people("Replies to these people"),
                "replied_to_message": texts("Replies to these messages: links or IDs."),
                "pinned": {"type": "boolean", "description": "Only pinned (true) or unpinned (false)."},
                "has": {
                    "type": "array",
                    "items": {"type": "string", "enum": negatable(HAS_TYPES)},
                    "description": "Has all of these (snapshot: forwarded); -link means without links."
                },
                "attachment_extension": texts("Attachment extensions, like \"pdf\" or \"csv\"."),
                "link_hostname": texts("Linking to these sites, like \"youtube.com\"."),
                "after": {"type": "string", "description": "On or after YYYY-MM-DD (asker's timezone)."},
                "before": {"type": "string", "description": "Before YYYY-MM-DD (asker's timezone)."},
                "sort": {"type": "string", "enum": ["newest", "oldest", "relevance"], "description": "Default newest."},
                "limit": {"type": "integer", "description": "1 to 25, default 10."},
                "offset": {"type": "integer", "description": "Matches to skip, at most 9975."}
            },
        }),
    }
}

pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<String> {
    let Some(guild) = asker.guild else {
        return Err(user_error("Search only works in a server."));
    };
    let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    let zone = home_zone(ctx, asker.user).await;

    let mut channel_ids = Vec::new();
    for name in list(args, "channel") {
        let id = find_channel(ctx, asker, guild, &name)
            .ok_or_else(|| user_error(format!("No channel matches \"{name}\".")))?;
        if !can_read(ctx, asker, Some(guild.get()), id).await? {
            return Err(user_error(format!("Chat is turned off in {name}.")));
        }
        channel_ids.push(id.get());
    }
    let mut replied_to_message_ids = Vec::new();
    for message in list(args, "replied_to_message") {
        let id = match parse_link(&message) {
            Some((_, _, id)) => Some(id),
            None => message.parse::<u64>().ok().filter(|id| *id > 0),
        };
        replied_to_message_ids.push(
            id.ok_or_else(|| user_error(format!("\"{message}\" isn't a message link or ID.")))?,
        );
    }
    let mut mentions_role_ids = Vec::new();
    for role in list(args, "mentions_role") {
        let id = find_role(ctx, guild, &role)
            .ok_or_else(|| user_error(format!("No role matches \"{role}\".")))?;
        mentions_role_ids.push(id);
    }
    let mut search = Search {
        content: text("query").map(str::to_string),
        channel_ids,
        author_types: list(args, "author_type"),
        author_ids: people(ctx, guild, args, "from").await?,
        mentions: people(ctx, guild, args, "mentions").await?,
        mentions_role_ids,
        replied_to_user_ids: people(ctx, guild, args, "replied_to").await?,
        replied_to_message_ids,
        pinned: args["pinned"].as_bool(),
        has: list(args, "has"),
        link_hostnames: list(args, "link_hostname"),
        attachment_extensions: list(args, "attachment_extension"),
        min_id: date(text("after"), zone)?,
        max_id: date(text("before"), zone)?,
        // Cherry's rule: age-restricted channels are searched too, from anywhere.
        include_nsfw: true,
        ..Default::default()
    };
    if !has_filter(&search) {
        return Err(user_error(
            "Give at least one filter, like `query`, `from`, `channel` or `has`.",
        ));
    }
    search.sort = match text("sort").unwrap_or("newest") {
        "newest" => Sort::Newest,
        "oldest" => Sort::Oldest,
        "relevance" => Sort::Relevance,
        other => return Err(user_error(format!("`sort` can't be {other}."))),
    };
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(DEFAULT_LIMIT as u64)
        .clamp(1, 25) as u32;
    let offset = args["offset"].as_u64().unwrap_or(0).min(MAX_OFFSET as u64) as u32;
    search.limit = Some(limit);
    search.offset = (offset > 0).then_some(offset);
    // A wrong value (an unknown `has`, too many names) goes back to the model to fix.
    search.query().map_err(user_error)?;

    let found = search::search(&ctx.web, ctx.token(), guild, &search).await?;

    // Discord searched everything the bot can see; keep the channels chat may read.
    let mut readable: HashMap<ChannelId, bool> = HashMap::new();
    let mut lines = Vec::new();
    for message in &found.messages {
        let channel = message.channel_id;
        let allowed = match readable.get(&channel) {
            Some(allowed) => *allowed,
            None => {
                let allowed = can_read(ctx, asker, Some(guild.get()), channel)
                    .await
                    .unwrap_or(false);
                readable.insert(channel, allowed);
                allowed
            }
        };
        if allowed {
            lines.push(format!(
                "#{} {} {}",
                ctx.channel_name(guild, channel)
                    // Archived threads aren't in the cache, but the search names them.
                    .or_else(|| found
                        .threads
                        .iter()
                        .find(|thread| thread.id == channel)
                        .and_then(|thread| thread.name.clone()))
                    .unwrap_or_else(|| "unknown".to_string()),
                format_message(ctx, message, zone).await,
                message_link(Some(guild), channel, message.id)
            ));
        }
    }
    Ok(answer(&found, offset, &lines))
}

/// The tool's answer: how many match, which page this is, then the messages.
fn answer(found: &search::Found, offset: u32, lines: &[String]) -> String {
    let mut out = format!("{} messages match.", found.total);
    let shown = found.messages.len() as u64;
    if shown > 0 {
        out.push_str(&format!(
            " These are {} to {}.",
            offset as u64 + 1,
            offset as u64 + shown
        ));
        if offset as u64 + shown < found.total {
            out.push_str(&format!(" Use offset {} for more.", offset as u64 + shown));
        }
    }
    let hidden = found.messages.len() - lines.len();
    if hidden > 0 {
        out.push_str(&format!(
            " {hidden} of them are in channels chat is turned off in, so they're left out."
        ));
    }
    if found.indexing {
        out.push_str(" Discord is still indexing this server, so older messages may be missing.");
    }
    if lines.is_empty() {
        if found.total == 0 {
            return "No messages found.".to_string();
        }
        return out;
    }
    format!("{out}\n{}", lines.join("\n"))
}

/// Whether the search filters anything (sort and paging alone don't).
fn has_filter(search: &Search) -> bool {
    let filters = Search {
        sort: Sort::Newest,
        limit: None,
        offset: None,
        include_nsfw: false,
        ..search.clone()
    };
    filters != Search::default()
}

/// A list argument: an array of strings, or one string.
fn list(args: &Value, key: &str) -> Vec<String> {
    let items: Vec<&Value> = match &args[key] {
        Value::Array(items) => items.iter().collect(),
        Value::Null => Vec::new(),
        one => vec![one],
    };
    items
        .into_iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The user IDs of the people `key` names. A mention or an ID is taken as it is, so people
/// who left the server can still be found; a name is looked up among the members.
async fn people(ctx: &BotCtx, guild: GuildId, args: &Value, key: &str) -> Result<Vec<u64>> {
    let mut ids = Vec::new();
    for who in list(args, key) {
        let id = match parse_user_id(&who) {
            Some(id) => id,
            None => find_member(&ctx.http, guild, &who).await?.user.id,
        };
        ids.push(id.get());
    }
    Ok(ids)
}

/// A role from a mention, ID or name.
fn find_role(ctx: &BotCtx, guild: GuildId, text: &str) -> Option<u64> {
    let digits = text.trim_start_matches("<@&").trim_end_matches('>');
    if let Ok(id) = digits.parse::<u64>()
        && id > 0
    {
        return Some(id);
    }
    let name = text.trim_start_matches('@').to_lowercase();
    ctx.guild_roles(guild)
        .into_iter()
        .find(|role| role.name.to_lowercase() == name)
        .map(|role| role.id.get())
}

/// A YYYY-MM-DD date as the first message ID of that day in `zone`.
fn date(date: Option<&str>, zone: Tz) -> Result<Option<u64>> {
    match date {
        Some(date) => Ok(Some(snowflake_at_date(date, zone).map_err(user_error)?)),
        None => Ok(None),
    }
}

/// The smallest message ID Discord could give a message sent at midnight on `date`.
fn snowflake_at_date(date: &str, zone: Tz) -> std::result::Result<u64, String> {
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| format!("{date} isn't a date like 2026-10-09."))?;
    let midnight = day.and_hms_opt(0, 0, 0).unwrap_or_default();
    let time = zone
        .from_local_datetime(&midnight)
        .earliest()
        .ok_or_else(|| format!("{date} has no midnight in {}.", zone.name()))?;
    Ok(search::snowflake_at(time.with_timezone(&chrono::Utc)))
}

/// A channel from a mention, ID, name or "here".
pub(super) fn find_channel(
    ctx: &BotCtx,
    asker: &Asker,
    guild: GuildId,
    text: &str,
) -> Option<ChannelId> {
    if matches!(text, "here" | "this" | "this channel") {
        return Some(asker.channel);
    }
    let digits = text.trim_start_matches("<#").trim_end_matches('>');
    if let Ok(id) = digits.parse::<u64>()
        && id > 0
    {
        return Some(ChannelId::new(id));
    }
    let name = text.trim_start_matches('#').to_lowercase();
    ctx.guild_channels(guild)
        .into_iter()
        .find(|c| c.name.as_deref().is_some_and(|n| n.to_lowercase() == name))
        .map(|c| c.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_take_one_or_many() {
        let args = json!({"has": ["image", " "], "channel": "here", "from": null});
        assert_eq!(list(&args, "has"), ["image"]);
        assert_eq!(list(&args, "channel"), ["here"]);
        assert!(list(&args, "from").is_empty());
        assert!(list(&args, "missing").is_empty());
    }

    #[test]
    fn sort_and_paging_alone_arent_a_filter() {
        let paged = Search {
            sort: Sort::Oldest,
            limit: Some(5),
            offset: Some(10),
            ..Default::default()
        };
        assert!(!has_filter(&paged));
        let pinned = Search {
            pinned: Some(true),
            ..Default::default()
        };
        assert!(has_filter(&pinned));
    }

    #[test]
    fn the_answer_says_how_many_and_where_to_go_next() {
        let found = search::Found {
            total: 30,
            ..Default::default()
        };
        assert_eq!(answer(&found, 0, &[]), "30 messages match.");
        let empty = search::Found::default();
        assert_eq!(answer(&empty, 0, &[]), "No messages found.");
    }

    #[test]
    fn dates_become_message_ids() {
        // 2015-01-01 UTC is Discord's epoch, so the ID is 0.
        assert_eq!(snowflake_at_date("2015-01-01", Tz::UTC), Ok(0));
        // A day later: 86 400 000 ms in the top bits.
        assert_eq!(
            snowflake_at_date("2015-01-02", Tz::UTC),
            Ok(86_400_000 << 22)
        );
        // Midnight in Oslo is an hour earlier than in UTC in winter.
        assert_eq!(
            snowflake_at_date("2015-01-02", Tz::Europe__Oslo),
            Ok((86_400_000 - 3_600_000) << 22)
        );
        assert!(snowflake_at_date("yesterday", Tz::UTC).is_err());
    }
}
