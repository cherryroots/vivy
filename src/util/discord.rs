//! The Discord pieces every part of the bot uses, on top of the Twilight crates.
//!
//! Twilight splits Discord into small crates: `twilight-model` (the data: messages, users,
//! buttons), `twilight-http` (the REST API), `twilight-gateway` (the live connection) and
//! `twilight-cache-inmemory` (what the gateway told us). This file gives the bot one place
//! to get the common ones from, plus a few things Twilight leaves to the bot:
//!
//! - Short names for IDs (`UserId` instead of `Id<UserMarker>`).
//! - [`Draft`]: a message that is built in one place and sent (or used to edit) in another.
//!   Twilight's own request builders borrow everything and must be sent right away.
//! - Small builders for buttons, rows, menus and modals.
//! - Display names, profile picture links and times.

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use serde::Serialize;
use twilight_model::channel::message::component::{
    ActionRow, Button, ButtonStyle, Component, Label, SelectMenu, SelectMenuOption,
    SelectMenuType, TextInput, TextInputStyle,
};
use twilight_model::channel::message::{AllowedMentions, Embed, MessageFlags, MentionType};
use twilight_model::http::attachment::Attachment as NewFile;
use twilight_model::id::Id;
use twilight_model::id::marker::{
    ApplicationMarker, AttachmentMarker, ChannelMarker, EmojiMarker, GuildMarker,
    InteractionMarker, MessageMarker, RoleMarker, UserMarker,
};
use twilight_model::user::User;
use twilight_model::util::Timestamp;

pub use twilight_model::channel::message::component;
pub use twilight_model::channel::{Channel, ChannelType, Message};
pub use twilight_model::guild::{Member, PartialMember, Permissions};

/// The REST API client.
pub type Http = twilight_http::Client;

pub type UserId = Id<UserMarker>;
pub type GuildId = Id<GuildMarker>;
pub type ChannelId = Id<ChannelMarker>;
pub type MessageId = Id<MessageMarker>;
pub type RoleId = Id<RoleMarker>;
pub type EmojiId = Id<EmojiMarker>;
pub type AttachmentId = Id<AttachmentMarker>;
pub type ApplicationId = Id<ApplicationMarker>;
pub type InteractionId = Id<InteractionMarker>;

/// The biggest message Discord takes, in characters.
pub const MAX_MESSAGE: usize = 2000;

// ---------------------------------------------------------------------------------------
// Drafts: messages to send or edits to make
// ---------------------------------------------------------------------------------------

/// A message before it's sent, or the changes to make to one. Build it with the methods
/// below, then [`Draft::send`] it to a channel, [`Draft::edit`] a message with it, or hand it
/// to an interaction (see `core::interaction`).
///
/// When editing, a field left as `None` stays as it is on Discord.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Draft {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeds: Option<Vec<Embed>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<Component>>,
    /// New files to upload.
    #[serde(skip)]
    pub files: Vec<NewFile>,
    /// When editing: the files already on the message to keep. `Some(empty)` removes them
    /// all; `None` keeps them (unless new files are added, which replaces them).
    #[serde(skip)]
    pub keep_files: Option<Vec<AttachmentId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_mentions: Option<AllowedMentions>,
    /// The message this one replies to (only when sending to a channel).
    #[serde(skip)]
    pub reply_to: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<MessageFlags>,
}

impl Draft {
    pub fn new() -> Draft {
        Draft::default()
    }

    pub fn content(mut self, content: impl Into<String>) -> Draft {
        self.content = Some(content.into());
        self
    }

    pub fn embed(mut self, embed: Embed) -> Draft {
        self.embeds.get_or_insert_with(Vec::new).push(embed);
        self
    }

    pub fn embeds(mut self, embeds: Vec<Embed>) -> Draft {
        self.embeds = Some(embeds);
        self
    }

    /// The rows of buttons and menus. An empty list removes them when editing.
    pub fn components(mut self, components: Vec<Component>) -> Draft {
        self.components = Some(components);
        self
    }

    /// Adds a file to upload.
    pub fn file(mut self, filename: impl Into<String>, data: Vec<u8>) -> Draft {
        // The ID only has to be unique within the request.
        let id = self.files.len() as u64;
        self.files
            .push(NewFile::from_bytes(filename.into(), data, id));
        self
    }

    /// When editing: removes every file already on the message (new ones are still added).
    pub fn remove_all_files(mut self) -> Draft {
        self.keep_files = Some(Vec::new());
        self
    }

    /// When editing: keeps this file that's already on the message. Adding new files
    /// replaces the old ones, unless they are kept with this.
    pub fn keep_file(mut self, id: AttachmentId) -> Draft {
        self.keep_files.get_or_insert_with(Vec::new).push(id);
        self
    }

    pub fn allowed_mentions(mut self, mentions: AllowedMentions) -> Draft {
        self.allowed_mentions = Some(mentions);
        self
    }

    pub fn reply_to(mut self, message: MessageId) -> Draft {
        self.reply_to = Some(message);
        self
    }

    pub fn flags(mut self, flags: MessageFlags) -> Draft {
        self.flags = Some(self.flags.unwrap_or(MessageFlags::empty()) | flags);
        self
    }

    /// Only the person who used the button or command sees it (interactions only).
    pub fn ephemeral(self) -> Draft {
        self.flags(MessageFlags::EPHEMERAL)
    }

    /// Link previews are left off.
    pub fn suppress_embeds(self) -> Draft {
        self.flags(MessageFlags::SUPPRESS_EMBEDS)
    }

    pub fn is_ephemeral(&self) -> bool {
        self.flags
            .is_some_and(|flags| flags.contains(MessageFlags::EPHEMERAL))
    }

    /// Sends it to `channel`.
    pub async fn send(&self, http: &Http, channel: ChannelId) -> anyhow::Result<Message> {
        let mut request = http.create_message(channel);
        if let Some(content) = &self.content {
            request = request.content(content);
        }
        if let Some(embeds) = &self.embeds {
            request = request.embeds(embeds);
        }
        if let Some(components) = &self.components {
            request = request.components(components);
        }
        if !self.files.is_empty() {
            request = request.attachments(&self.files);
        }
        if let Some(mentions) = &self.allowed_mentions {
            request = request.allowed_mentions(Some(mentions));
        }
        if let Some(reply_to) = self.reply_to {
            // Still send it if the message it replies to was deleted meanwhile.
            request = request.reply(reply_to).fail_if_not_exists(false);
        }
        if let Some(flags) = self.flags {
            request = request.flags(flags);
        }
        Ok(request.await?.model().await?)
    }

    /// Changes message `id` in `channel` to match it.
    pub async fn edit(
        &self,
        http: &Http,
        channel: ChannelId,
        id: MessageId,
    ) -> anyhow::Result<Message> {
        let mut request = http.update_message(channel, id);
        if let Some(content) = &self.content {
            request = request.content(Some(content));
        }
        if let Some(embeds) = &self.embeds {
            request = request.embeds(Some(embeds));
        }
        if let Some(components) = &self.components {
            request = request.components(Some(components));
        }
        if !self.files.is_empty() {
            request = request.attachments(&self.files);
        }
        if let Some(keep) = &self.keep_files {
            request = request.keep_attachment_ids(keep);
        }
        if let Some(mentions) = &self.allowed_mentions {
            request = request.allowed_mentions(Some(mentions));
        }
        if let Some(flags) = self.flags {
            request = request.flags(flags);
        }
        Ok(request.await?.model().await?)
    }
}

// ---------------------------------------------------------------------------------------
// Mentions
// ---------------------------------------------------------------------------------------

/// Pings nobody, whatever the text says.
pub fn no_pings() -> AllowedMentions {
    AllowedMentions::default()
}

/// Pings only these people.
pub fn ping_users(users: impl IntoIterator<Item = UserId>) -> AllowedMentions {
    AllowedMentions {
        users: users.into_iter().collect(),
        ..AllowedMentions::default()
    }
}

/// Pings the person a reply replies to, and nobody else.
pub fn ping_replied() -> AllowedMentions {
    AllowedMentions {
        replied_user: true,
        ..AllowedMentions::default()
    }
}

/// Pings every user mentioned in the text (not roles or @everyone).
pub fn ping_mentioned_users() -> AllowedMentions {
    AllowedMentions {
        parse: vec![MentionType::Users],
        ..AllowedMentions::default()
    }
}

/// Whether a message @-mentions this user (replies that ping count too).
pub fn mentions_user(msg: &Message, user: UserId) -> bool {
    msg.mentions.iter().any(|mention| mention.id == user)
}

// ---------------------------------------------------------------------------------------
// Buttons, menus and modals
// ---------------------------------------------------------------------------------------

/// A button that sends `custom_id` to the bot when pressed.
pub fn button(custom_id: impl Into<String>, label: impl Into<String>, style: ButtonStyle) -> Button {
    Button {
        id: None,
        custom_id: Some(custom_id.into()),
        disabled: false,
        emoji: None,
        label: Some(label.into()),
        style,
        url: None,
        sku_id: None,
    }
}

/// A button that opens a link.
pub fn link_button(url: impl Into<String>, label: impl Into<String>) -> Button {
    Button {
        id: None,
        custom_id: None,
        disabled: false,
        emoji: None,
        label: Some(label.into()),
        style: ButtonStyle::Link,
        url: Some(url.into()),
        sku_id: None,
    }
}

/// A row of buttons. Discord fits 5 on a row.
pub fn buttons(buttons: Vec<Button>) -> Component {
    Component::ActionRow(ActionRow {
        id: None,
        components: buttons.into_iter().map(Component::Button).collect(),
    })
}

/// A menu of text options, on a row of its own.
pub fn menu(custom_id: impl Into<String>, options: Vec<SelectMenuOption>) -> SelectMenu {
    SelectMenu {
        id: None,
        channel_types: None,
        custom_id: custom_id.into(),
        default_values: None,
        disabled: false,
        kind: SelectMenuType::Text,
        max_values: None,
        min_values: None,
        options: Some(options),
        placeholder: None,
        required: None,
    }
}

/// One option of a [`menu`].
pub fn menu_option(label: impl Into<String>, value: impl Into<String>) -> SelectMenuOption {
    SelectMenuOption {
        default: false,
        description: None,
        emoji: None,
        label: label.into(),
        value: value.into(),
    }
}

/// A row holding one menu.
pub fn menu_row(menu: SelectMenu) -> Component {
    Component::ActionRow(ActionRow {
        id: None,
        components: vec![Component::SelectMenu(menu)],
    })
}

/// A pop-up form with text boxes, opened by a button.
#[derive(Debug, Clone)]
pub struct Modal {
    pub custom_id: String,
    pub title: String,
    pub components: Vec<Component>,
}

impl Modal {
    pub fn new(custom_id: impl Into<String>, title: impl Into<String>) -> Modal {
        Modal {
            custom_id: custom_id.into(),
            title: title.into(),
            components: Vec::new(),
        }
    }

    /// Adds a text box (see [`text_input`]) under a label.
    pub fn input(mut self, label: impl Into<String>, input: TextInput) -> Modal {
        self.components.push(Component::Label(Label {
            id: None,
            label: label.into(),
            description: None,
            component: Box::new(Component::TextInput(input)),
        }));
        self
    }
}

/// A text box for a [`Modal`]. Its text comes back under `custom_id`.
#[allow(deprecated, reason = "the label goes on the Label around it instead")]
pub fn text_input(custom_id: impl Into<String>, style: TextInputStyle) -> TextInput {
    TextInput {
        id: None,
        custom_id: custom_id.into(),
        label: None,
        max_length: None,
        min_length: None,
        placeholder: None,
        required: None,
        style,
        value: None,
    }
}

// ---------------------------------------------------------------------------------------
// Users, pictures and times
// ---------------------------------------------------------------------------------------

/// The name Discord shows for a user: their display name, or their username.
pub fn display_name(user: &User) -> &str {
    user.global_name.as_deref().unwrap_or(&user.name)
}

/// The name a member goes by in a server: their nickname there, or [`display_name`].
pub fn member_name<'a>(nick: Option<&'a str>, user: &'a User) -> &'a str {
    nick.unwrap_or_else(|| display_name(user))
}

/// A user's profile picture, or Discord's default one when they have none.
pub fn avatar_url(user: &User) -> String {
    match &user.avatar {
        Some(hash) => {
            let ext = if hash.is_animated() { "gif" } else { "webp" };
            format!(
                "https://cdn.discordapp.com/avatars/{}/{hash}.{ext}?size=1024",
                user.id
            )
        }
        None => {
            // New usernames pick by ID, old ones with a #1234 by the number.
            let index = match user.discriminator {
                0 => (user.id.get() >> 22) % 6,
                n => u64::from(n) % 5,
            };
            format!("https://cdn.discordapp.com/embed/avatars/{index}.png")
        }
    }
}

/// A member's profile picture for one server, when they set one there.
pub fn member_avatar_url(
    guild: GuildId,
    user: UserId,
    avatar: Option<twilight_model::util::ImageHash>,
) -> Option<String> {
    let hash = avatar?;
    let ext = if hash.is_animated() { "gif" } else { "webp" };
    Some(format!(
        "https://cdn.discordapp.com/guilds/{guild}/users/{user}/avatars/{hash}.{ext}?size=1024"
    ))
}

/// A link to a message.
pub fn message_link(guild: Option<GuildId>, channel: ChannelId, message: MessageId) -> String {
    match guild {
        Some(guild) => format!("https://discord.com/channels/{guild}/{channel}/{message}"),
        None => format!("https://discord.com/channels/@me/{channel}/{message}"),
    }
}

/// A Discord time as a chrono time.
pub fn to_chrono(time: Timestamp) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(time.as_micros()).unwrap_or_default()
}

/// When an ID (a message, a user...) was made: Discord's IDs start with the time.
pub fn created_at<T>(id: Id<T>) -> DateTime<Utc> {
    // Milliseconds since the start of 2015, in the top bits.
    let ms = (id.get() >> 22) as i64 + 1_420_070_400_000;
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

/// The error code Discord answered with, like 10008 (Unknown Message) or 50013 (Missing
/// Permissions). `None` for other errors (no connection, a timeout).
pub fn error_code(err: &anyhow::Error) -> Option<u64> {
    use twilight_http::api_error::ApiError;
    use twilight_http::error::ErrorType;
    let err = err.chain().find_map(|e| e.downcast_ref::<twilight_http::Error>())?;
    match err.kind() {
        ErrorType::Response {
            error: ApiError::General(general),
            ..
        } => Some(general.code),
        _ => None,
    }
}

/// The HTTP status Discord answered with, like 404 or 403.
pub fn error_status(err: &anyhow::Error) -> Option<u16> {
    use twilight_http::error::ErrorType;
    let err = err.chain().find_map(|e| e.downcast_ref::<twilight_http::Error>())?;
    match err.kind() {
        ErrorType::Response { status, .. } => Some(status.get()),
        _ => None,
    }
}

/// Fetches one message.
pub async fn get_message(
    http: &Http,
    channel: ChannelId,
    id: MessageId,
) -> anyhow::Result<Message> {
    Ok(http
        .message(channel, id)
        .await
        .context("fetching a message")?
        .model()
        .await?)
}

/// Builds a message from Discord's JSON, for tests. Fields Discord always sends get
/// harmless defaults, so a test only gives the ones it's about.
#[cfg(test)]
pub fn test_message(fields: serde_json::Value) -> Message {
    let mut json = serde_json::json!({
        "id": "1",
        "channel_id": "2",
        "author": {"id": "3", "username": "someone", "discriminator": "0", "avatar": null},
        "content": "",
        "timestamp": "2026-01-01T00:00:00+00:00",
        "edited_timestamp": null,
        "tts": false,
        "mention_everyone": false,
        "mentions": [],
        "mention_roles": [],
        "attachments": [],
        "embeds": [],
        "pinned": false,
        "type": 0,
    });
    for (key, value) in fields.as_object().expect("an object") {
        json[key] = value.clone();
    }
    serde_json::from_value(json).expect("a valid message")
}
