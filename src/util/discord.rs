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
    ActionRow, Button, ButtonStyle, Component, Label, SelectMenu, SelectMenuOption, SelectMenuType,
    TextInput, TextInputStyle,
};
use twilight_model::channel::message::embed::EmbedField;
use twilight_model::channel::message::{AllowedMentions, Embed, EmojiReactionType, MessageFlags};
use twilight_model::http::attachment::Attachment as NewFile;
use twilight_model::id::Id;
use twilight_model::id::marker::{
    ApplicationMarker, AttachmentMarker, ChannelMarker, EmojiMarker, GuildMarker,
    InteractionMarker, MessageMarker, RoleMarker, UserMarker,
};
use twilight_model::user::User;
use twilight_model::util::Timestamp;
use twilight_util::builder::message::{ButtonBuilder, SelectMenuBuilder, SelectMenuOptionBuilder};

pub use twilight_model::channel::{Channel, ChannelType, Message};
pub use twilight_model::gateway::GatewayReaction;
pub use twilight_model::guild::{Member, PartialMember, Permissions, Role};

/// The REST API client.
pub type Http = twilight_http::Client;
pub use twilight_http::request::channel::reaction::RequestReactionType;
use twilight_http::request::{Form, Request};
use twilight_http::routing::Route;

pub type UserId = Id<UserMarker>;
pub type GuildId = Id<GuildMarker>;
pub type ChannelId = Id<ChannelMarker>;
pub type MessageId = Id<MessageMarker>;
pub type RoleId = Id<RoleMarker>;
pub type EmojiId = Id<EmojiMarker>;
pub type AttachmentId = Id<AttachmentMarker>;
pub type ApplicationId = Id<ApplicationMarker>;
pub type InteractionId = Id<InteractionMarker>;

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

/// Whether a message @-mentions this user (replies that ping count too).
pub fn mentions_user(msg: &Message, user: UserId) -> bool {
    msg.mentions.iter().any(|mention| mention.id == user)
}

// ---------------------------------------------------------------------------------------
// Buttons, menus and modals
// ---------------------------------------------------------------------------------------

/// A button that sends `custom_id` to the bot when pressed. Add `.emoji(..)` or
/// `.disabled(..)` to the builder, and put it in a row with [`buttons`].
pub fn button(
    custom_id: impl Into<String>,
    label: impl Into<String>,
    style: ButtonStyle,
) -> ButtonBuilder {
    ButtonBuilder::new(style).custom_id(custom_id).label(label)
}

/// An emoji like "🎯" for a button or menu option.
pub fn emoji(text: &str) -> EmojiReactionType {
    EmojiReactionType::Unicode {
        name: text.to_string(),
    }
}

/// A row of buttons. Discord fits 5 on a row.
pub fn buttons<B: Into<Button>>(buttons: impl IntoIterator<Item = B>) -> Component {
    Component::ActionRow(ActionRow {
        id: None,
        components: buttons
            .into_iter()
            .map(|button| Component::Button(button.into()))
            .collect(),
    })
}

/// A menu of text options (made with [`menu_option`], at most 25). Add a
/// `.placeholder(..)` to the builder and put it on a row of its own with [`menu_row`].
pub fn menu<O: Into<SelectMenuOption>>(
    custom_id: impl Into<String>,
    options: impl IntoIterator<Item = O>,
) -> SelectMenuBuilder {
    let mut menu = SelectMenuBuilder::new(custom_id, SelectMenuType::Text);
    for option in options {
        menu = menu.option(option);
    }
    menu
}

/// One option of a [`menu`].
pub fn menu_option(label: impl Into<String>, value: impl Into<String>) -> SelectMenuOptionBuilder {
    SelectMenuOptionBuilder::new(label, value)
}

/// A row holding one menu.
pub fn menu_row(menu: impl Into<SelectMenu>) -> Component {
    Component::ActionRow(ActionRow {
        id: None,
        components: vec![Component::SelectMenu(menu.into())],
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

    /// Adds a text box with a label over it.
    pub fn input(mut self, label: impl Into<String>, input: TextBox) -> Modal {
        self.components.push(Component::Label(Label {
            id: None,
            label: label.into(),
            description: None,
            component: Box::new(Component::TextInput(input.0)),
        }));
        self
    }
}

/// A text box for a [`Modal`]. Its text comes back under `custom_id`.
#[derive(Debug, Clone)]
pub struct TextBox(TextInput);

impl TextBox {
    /// A one-line box.
    pub fn short(custom_id: impl Into<String>) -> TextBox {
        TextBox::new(custom_id, TextInputStyle::Short)
    }

    #[allow(deprecated, reason = "the label goes on the Label around it instead")]
    fn new(custom_id: impl Into<String>, style: TextInputStyle) -> TextBox {
        TextBox(TextInput {
            id: None,
            custom_id: custom_id.into(),
            label: None,
            max_length: None,
            min_length: None,
            placeholder: None,
            required: None,
            style,
            value: None,
        })
    }

    pub fn placeholder(mut self, text: impl Into<String>) -> TextBox {
        self.0.placeholder = Some(text.into());
        self
    }

    /// Text already in the box.
    pub fn value(mut self, text: impl Into<String>) -> TextBox {
        self.0.value = Some(text.into());
        self
    }

    pub fn max_length(mut self, length: u16) -> TextBox {
        self.0.max_length = Some(length);
        self
    }

    pub fn required(mut self, required: bool) -> TextBox {
        self.0.required = Some(required);
        self
    }
}

/// One field of an embed, for `EmbedBuilder::field`. `inline` fields sit side by side.
pub fn field(name: impl Into<String>, value: impl Into<String>, inline: bool) -> EmbedField {
    EmbedField {
        inline,
        name: name.into(),
        value: value.into(),
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
    let err = err
        .chain()
        .find_map(|e| e.downcast_ref::<twilight_http::Error>())?;
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
    let err = err
        .chain()
        .find_map(|e| e.downcast_ref::<twilight_http::Error>())?;
    match err.kind() {
        ErrorType::Response { status, .. } => Some(status.get()),
        _ => None,
    }
}

/// The name a member goes by in their server: their nickname, or [`display_name`].
pub fn member_display_name(member: &Member) -> &str {
    member_name(member.nick.as_deref(), &member.user)
}

/// Fetches a member of a server from Discord.
pub async fn get_member(http: &Http, guild: GuildId, user: UserId) -> anyhow::Result<Member> {
    Ok(http
        .guild_member(guild, user)
        .await
        .context("fetching a server member")?
        .model()
        .await?)
}

/// Fetches a user from Discord.
pub async fn get_user(http: &Http, user: UserId) -> anyhow::Result<User> {
    Ok(http
        .user(user)
        .await
        .context("fetching a user")?
        .model()
        .await?)
}

/// Fetches a channel or thread.
pub async fn get_channel(http: &Http, channel: ChannelId) -> anyhow::Result<Channel> {
    Ok(http
        .channel(channel)
        .await
        .context("fetching a channel")?
        .model()
        .await?)
}

/// Reacts to a message with `emoji`: a plain one ("👍") or a server's own, written
/// "<:name:123>", "<a:name:123>" or "name:123".
pub async fn react(
    http: &Http,
    channel: ChannelId,
    message: MessageId,
    emoji: &str,
) -> anyhow::Result<()> {
    let reaction = parse_reaction(emoji).with_context(|| format!("{emoji:?} isn't an emoji"))?;
    http.create_reaction(channel, message, &reaction)
        .await
        .context("adding a reaction")?;
    Ok(())
}

/// Takes a reaction someone added away again.
pub async fn remove_reaction(http: &Http, reaction: &GatewayReaction) -> anyhow::Result<()> {
    let emoji = match &reaction.emoji {
        EmojiReactionType::Custom { id, name, .. } => RequestReactionType::Custom {
            id: *id,
            name: name.as_deref(),
        },
        EmojiReactionType::Unicode { name } => RequestReactionType::Unicode { name },
    };
    http.delete_reaction(
        reaction.channel_id,
        reaction.message_id,
        &emoji,
        reaction.user_id,
    )
    .await
    .context("removing a reaction")?;
    Ok(())
}

/// The emoji in `text`, for [`react`].
fn parse_reaction(text: &str) -> Option<RequestReactionType<'_>> {
    let text = text.trim();
    let inner = text.trim_start_matches('<').trim_end_matches('>');
    if let Some((name, id)) = inner.rsplit_once(':')
        && let Ok(id) = id.parse::<u64>()
        && id > 0
    {
        let name = name.trim_start_matches("a:").trim_start_matches(':');
        return Some(RequestReactionType::Custom {
            id: EmojiId::new(id),
            name: (!name.is_empty()).then_some(name),
        });
    }
    (!text.is_empty()).then_some(RequestReactionType::Unicode { name: text })
}

/// Sends a message twilight has no builder for, like a voice message: `payload` is
/// Discord's JSON for the message, and `files` its attachments, in the order of the ids in
/// the payload's `attachments` (0, 1, ...).
pub async fn send_raw(
    http: &Http,
    channel: ChannelId,
    payload: &serde_json::Value,
    files: &[(String, Vec<u8>)],
) -> anyhow::Result<Message> {
    let mut form = Form::new().json_part(b"payload_json", &serde_json::to_vec(payload)?);
    for (i, (name, data)) in files.iter().enumerate() {
        form = form.file_part(format!("files[{i}]").as_bytes(), name.as_bytes(), data);
    }
    let route = Route::CreateMessage {
        channel_id: channel.get(),
    };
    let request = Request::builder(&route).form(form).build()?;
    Ok(http
        .request::<Message>(request)
        .await
        .context("sending a message")?
        .model()
        .await?)
}

/// Shows "Vivy is typing…" in a channel until it's dropped.
pub struct Typing(tokio::task::JoinHandle<()>);

impl Drop for Typing {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Starts [`Typing`]. Discord shows it for 10 seconds, so it's sent again every 8.
pub fn start_typing(http: &std::sync::Arc<Http>, channel: ChannelId) -> Typing {
    let http = http.clone();
    Typing(tokio::spawn(async move {
        loop {
            let _ = http.create_typing_trigger(channel).await;
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        }
    }))
}

/// Where a page of [`get_messages`] starts.
pub enum Page {
    /// The newest messages.
    Newest,
    /// The messages just before this one.
    Before(MessageId),
    /// The messages just after this one.
    After(MessageId),
}

/// A page of up to `limit` (at most 100) messages of a channel, newest first.
pub async fn get_messages(
    http: &Http,
    channel: ChannelId,
    page: Page,
    limit: u16,
) -> anyhow::Result<Vec<Message>> {
    let request = http.channel_messages(channel);
    let response = match page {
        Page::Newest => request.limit(limit).await,
        Page::Before(id) => request.before(id).limit(limit).await,
        Page::After(id) => request.after(id).limit(limit).await,
    };
    Ok(response.context("fetching messages")?.models().await?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions_read_plain_and_server_emoji() {
        assert_eq!(
            parse_reaction(" 👍 "),
            Some(RequestReactionType::Unicode { name: "👍" })
        );
        let pog = RequestReactionType::Custom {
            id: EmojiId::new(123),
            name: Some("pog"),
        };
        assert_eq!(parse_reaction("<:pog:123>"), Some(pog));
        assert_eq!(parse_reaction("<a:pog:123>"), Some(pog));
        assert_eq!(parse_reaction("pog:123"), Some(pog));
        assert_eq!(parse_reaction(""), None);
    }

    #[test]
    fn ids_tell_their_time() {
        // Discord's own example: 175928847299117063 was made on 2016-04-30.
        let id = UserId::new(175_928_847_299_117_063);
        assert_eq!(created_at(id).timestamp_millis(), 1_462_015_105_796);
    }
}
