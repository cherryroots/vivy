//! Which channels a game may take quotes from.
//!
//! The game is seen by everyone in the channel it's played in, so quotes from another
//! channel must not show them anything they couldn't read already. The channel the game is
//! played in is always fine. Any other channel must be readable by everyone in the server
//! (the @everyone role can view it and read its history), and not age-restricted.

use serenity::all::{
    ChannelId, ChannelType, Guild, GuildChannel, PermissionOverwrite, PermissionOverwriteType,
    Permissions, RoleId,
};

use crate::core::{BotCtx, user_error};

/// Where `/games whosaidit` takes its quotes from.
pub enum From {
    /// The channel the game is played in.
    Here,
    /// One other channel or thread.
    Channel(Box<GuildChannel>),
    /// Every channel everyone in the server can read, and this one.
    Server,
}

/// A channel to read, with its name for the question ("Said in #general").
pub struct Source {
    pub channel: ChannelId,
    /// `None` for the channel the game is played in, which needs no name.
    pub name: Option<String>,
}

/// The channels to read for `from`, or a reason a person can read when there are none.
pub fn pick(
    bot: &BotCtx,
    guild: &Guild,
    here: ChannelId,
    from: From,
) -> anyhow::Result<Vec<Source>> {
    let this = Source {
        channel: here,
        name: None,
    };
    match from {
        From::Here => Ok(vec![this]),
        From::Channel(channel) if channel.id == here => Ok(vec![this]),
        From::Channel(channel) => {
            if !bot.allows("games", Some(guild.id), channel.id) {
                return Err(user_error(format!(
                    "Games are turned off in <#{}>.",
                    channel.id
                )));
            }
            if !public(guild, &channel) {
                return Err(user_error(
                    "I only take quotes from another channel when everyone in the server can \
                     read it, so nobody sees messages they couldn't already. Play it in that \
                     channel instead.",
                ));
            }
            Ok(vec![Source {
                channel: channel.id,
                name: Some(channel.name.clone()),
            }])
        }
        From::Server => {
            let mut sources = vec![this];
            for channel in guild.channels.values() {
                let readable = matches!(channel.kind, ChannelType::Text | ChannelType::News);
                if channel.id == here
                    || !readable
                    || !public(guild, channel)
                    || !bot.allows("games", Some(guild.id), channel.id)
                {
                    continue;
                }
                sources.push(Source {
                    channel: channel.id,
                    name: Some(channel.name.clone()),
                });
            }
            Ok(sources)
        }
    }
}

/// Whether everyone in the server can read `channel`'s history, and it isn't
/// age-restricted. A thread counts as its parent channel.
fn public(guild: &Guild, channel: &GuildChannel) -> bool {
    let channel = match channel.kind {
        ChannelType::PublicThread | ChannelType::NewsThread => {
            match channel.parent_id.and_then(|p| guild.channels.get(&p)) {
                Some(parent) => parent,
                None => return false,
            }
        }
        ChannelType::Text | ChannelType::News => channel,
        _ => return false,
    };
    if channel.nsfw {
        return false;
    }
    // The @everyone role's ID is the server's ID.
    let everyone = RoleId::new(guild.id.get());
    let Some(role) = guild.roles.get(&everyone) else {
        return false;
    };
    everyone_can_read(role.permissions, &channel.permission_overwrites, everyone)
}

/// Whether the @everyone role can view a channel and read its history: its server-wide
/// permissions, with the channel's overwrite for @everyone applied (denies first, then
/// allows, the way Discord does it).
fn everyone_can_read(
    server_wide: Permissions,
    overwrites: &[PermissionOverwrite],
    everyone: RoleId,
) -> bool {
    if server_wide.administrator() {
        return true;
    }
    let mut permissions = server_wide;
    for overwrite in overwrites {
        if overwrite.kind == PermissionOverwriteType::Role(everyone) {
            permissions = (permissions & !overwrite.deny) | overwrite.allow;
        }
    }
    permissions.contains(Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERYONE: RoleId = RoleId::new(1);
    const READ: Permissions = Permissions::VIEW_CHANNEL.union(Permissions::READ_MESSAGE_HISTORY);

    fn overwrite(role: u64, allow: Permissions, deny: Permissions) -> PermissionOverwrite {
        PermissionOverwrite {
            allow,
            deny,
            kind: PermissionOverwriteType::Role(RoleId::new(role)),
        }
    }

    #[test]
    fn a_plain_channel_is_public() {
        assert!(everyone_can_read(READ, &[], EVERYONE));
    }

    #[test]
    fn hiding_it_from_everyone_makes_it_private() {
        let hidden = [overwrite(
            1,
            Permissions::empty(),
            Permissions::VIEW_CHANNEL,
        )];
        assert!(!everyone_can_read(READ, &hidden, EVERYONE));
        // Another role's overwrite doesn't count: not everyone has that role.
        let other = [overwrite(2, READ, Permissions::empty())];
        assert!(!everyone_can_read(Permissions::empty(), &other, EVERYONE));
    }

    #[test]
    fn opening_it_to_everyone_makes_it_public() {
        let opened = [overwrite(1, READ, Permissions::empty())];
        assert!(everyone_can_read(Permissions::empty(), &opened, EVERYONE));
    }

    #[test]
    fn history_counts_too() {
        let no_history = [overwrite(
            1,
            Permissions::empty(),
            Permissions::READ_MESSAGE_HISTORY,
        )];
        assert!(!everyone_can_read(READ, &no_history, EVERYONE));
    }
}
