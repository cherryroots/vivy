//! Finding people by mention, user ID or name, and downloading their profile pictures.
//! Chat's `look_at_profile_picture` shows a picture to the model; drawing draws from them.
//!
//! In a server it uses the person's picture for that server when they have one, otherwise
//! their main one. Her own server picture is the face the mood check picked, so "me" shows
//! her as she looks right now.

use crate::core::{Result, user_error};
use crate::util::discord::{
    GuildId, Http, Member, UserId, avatar_url, display_name, get_member, get_user,
    member_avatar_url, member_display_name,
};
use crate::util::media::{ModelImage, download, png_link};

/// Profile pictures are small; anything bigger than this isn't one.
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// Someone's profile picture, downloaded.
pub struct ProfilePicture {
    pub user: UserId,
    /// Their name where the picture was looked up.
    pub name: String,
    /// Whether it's their picture for this server rather than their main one.
    pub for_server: bool,
    pub image: ModelImage,
}

/// Where a profile picture is looked up: who asked, where, and who the bot is.
pub struct Lookup {
    pub bot: UserId,
    pub guild: Option<GuildId>,
    pub asker: UserId,
}

/// The profile picture of the person `query` names (a mention, user ID, name, or `me` for
/// the bot), or the asker's without one. Discord makes it `size` pixels wide, as a PNG.
pub async fn profile_picture(
    http: &Http,
    lookup: &Lookup,
    query: Option<&str>,
    size: u32,
) -> Result<ProfilePicture> {
    let query = query.map(str::trim).filter(|s| !s.is_empty());
    let query = match query {
        Some(word) if is_herself(word) => Some(lookup.bot.to_string()),
        other => other.map(str::to_string),
    };

    // (who it is, their name, the picture's link, whether it's their picture for this server)
    let (user, name, url, for_server) = match lookup.guild {
        Some(guild) => {
            let member = match &query {
                None => get_member(http, guild, lookup.asker).await?,
                Some(query) => find_member(http, guild, query).await?,
            };
            let (url, for_server) = match member_avatar_url(guild, member.user.id, member.avatar) {
                Some(url) => (url, true),
                None => (avatar_url(&member.user), false),
            };
            (
                member.user.id,
                member_display_name(&member).to_string(),
                url,
                for_server,
            )
        }
        // A DM has no member list to search by name.
        None => {
            let id = match &query {
                None => lookup.asker,
                Some(query) => parse_user_id(query).ok_or_else(|| {
                    user_error("In a DM I can only find people by mention or user ID.")
                })?,
            };
            let user = get_user(http, id)
                .await
                .map_err(|_| user_error(format!("There's nobody with ID {id}.")))?;
            (
                user.id,
                display_name(&user).to_string(),
                avatar_url(&user),
                false,
            )
        }
    };

    let data = download(&png_link(&url, size), MAX_BYTES).await?;
    Ok(ProfilePicture {
        user,
        name,
        for_server,
        image: ModelImage {
            mime: "image/png",
            data,
        },
    })
}

/// The words a model might use for itself.
pub fn is_herself(query: &str) -> bool {
    matches!(
        query.to_lowercase().as_str(),
        "me" | "myself" | "you" | "yourself" | "self"
    )
}

/// A server member from a mention, user ID or name.
pub async fn find_member(http: &Http, guild: GuildId, query: &str) -> Result<Member> {
    match parse_user_id(query) {
        Some(id) => get_member(http, guild, id)
            .await
            .map_err(|_| user_error(format!("Nobody with ID {id} is in this server."))),
        None => http
            .search_guild_members(guild, query)
            .limit(5)
            .await?
            .models()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| user_error(format!("No member matches \"{query}\"."))),
    }
}

/// "<@123>", "<@!123>" or "123" → the ID.
pub fn parse_user_id(text: &str) -> Option<UserId> {
    let digits = text
        .trim_start_matches("<@")
        .trim_start_matches('!')
        .trim_end_matches('>');
    digits
        .parse::<u64>()
        .ok()
        .filter(|&id| id > 0)
        .map(UserId::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_when_she_means_herself() {
        assert!(is_herself("me"));
        assert!(is_herself("Yourself"));
        assert!(!is_herself("Cherry"));
    }

    #[test]
    fn user_ids() {
        assert_eq!(parse_user_id("<@123>"), Some(UserId::new(123)));
        assert_eq!(parse_user_id("<@!123>"), Some(UserId::new(123)));
        assert_eq!(parse_user_id("123"), Some(UserId::new(123)));
        assert_eq!(parse_user_id("cherry"), None);
    }
}
