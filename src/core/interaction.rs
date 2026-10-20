//! Slash commands, buttons, menus and modals: what Discord calls interactions.
//!
//! - [`Command`] is one slash command (or right-click "Apps" command) a feature declares:
//!   what Discord is told about it, and the function that runs it. The options are a struct
//!   that derives `CreateCommand` and `CommandModel` from `twilight-interactions`, which
//!   both describes the command to Discord and reads the options back.
//! - [`CommandCtx`] is what a command's function gets: who used it, where, its options, and
//!   `say` / `send` / `defer` to answer.
//! - [`ComponentInteraction`] (a button or menu) and [`ModalInteraction`] (a submitted
//!   pop-up form) are what `Feature::on_component` and `Feature::on_modal` get. They answer
//!   through the [`Respond`] trait.
//!
//! Every interaction must get an answer within 3 seconds, or Discord shows "This
//! interaction failed". Slow work defers first (shows "thinking…"), then edits the answer in.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::{Context as _, anyhow};
use twilight_interactions::command::{CommandInputData, CommandModel, CreateCommand};
use twilight_model::application::command::{
    Command as CommandDefinition, CommandOptionChoice, CommandOptionChoiceValue, CommandType,
};
use twilight_model::application::interaction::application_command::{
    CommandData, CommandDataOption, CommandOptionValue,
};
use twilight_model::application::interaction::modal::ModalInteractionComponent;
use twilight_model::application::interaction::{
    Interaction, InteractionContextType, InteractionData, InteractionDataResolved,
};
use twilight_model::channel::message::MessageFlags;
use twilight_model::http::interaction::{
    InteractionResponse, InteractionResponseData, InteractionResponseType,
};
use twilight_model::user::User;

use super::{BotCtx, Result};
use crate::util::discord::{
    ChannelId, Draft, GuildId, InteractionId, Message, Modal, PartialMember, UserId,
};

/// A boxed future: what an `async fn` returns, in a form that fits in a struct field.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Runs a command. Written as `|ctx| Box::pin(my_command(ctx))`.
pub type CommandFn = fn(CommandCtx) -> BoxFuture<'static, Result<()>>;

/// Suggests values for the option being typed. Written as `|ctx, typed| Box::pin(f(ctx, typed))`.
pub type AutocompleteFn = fn(CommandCtx, Typed) -> BoxFuture<'static, Result<Vec<Choice>>>;

/// A suggestion for an option: the text shown and the value it fills in.
pub type Choice = CommandOptionChoice;

/// One command a feature declares in `Feature::commands`.
pub struct Command {
    /// What Discord is told: the name, description and options.
    pub definition: CommandDefinition,
    pub run: CommandFn,
    pub autocomplete: Option<AutocompleteFn>,
}

impl Command {
    /// A slash command whose name and options come from `T`, a struct (or an enum of
    /// subcommands) deriving `CreateCommand`.
    pub fn slash<T: CreateCommand>(run: CommandFn) -> Command {
        Command {
            definition: T::create_command().into(),
            run,
            autocomplete: None,
        }
    }

    /// A command in a message's right-click "Apps" menu.
    pub fn message(name: &str, run: CommandFn) -> Command {
        #[allow(
            deprecated,
            reason = "dm_permission has to be given; contexts replace it"
        )]
        let definition = CommandDefinition {
            application_id: None,
            contexts: None,
            default_member_permissions: None,
            dm_permission: None,
            description: String::new(),
            description_localizations: None,
            guild_id: None,
            id: None,
            integration_types: None,
            kind: CommandType::Message,
            name: name.to_string(),
            name_localizations: None,
            nsfw: None,
            options: Vec::new(),
            version: twilight_model::id::Id::new(1),
        };
        Command {
            definition,
            run,
            autocomplete: None,
        }
    }

    /// Suggestions while someone types an option marked `autocomplete = true`.
    pub fn autocomplete(mut self, suggest: AutocompleteFn) -> Command {
        self.autocomplete = Some(suggest);
        self
    }

    /// Only usable in servers, not in DMs.
    pub fn guild_only(mut self) -> Command {
        self.definition.contexts = Some(vec![InteractionContextType::Guild]);
        self
    }
}

/// A command and the feature it belongs to, so it only runs where that feature is allowed.
pub struct FeatureCommand {
    pub feature: &'static str,
    pub command: Command,
}

/// A suggestion for an autocomplete option.
pub fn choice(name: impl Into<String>, value: impl Into<String>) -> Choice {
    Choice {
        name: name.into(),
        name_localizations: None,
        value: CommandOptionChoiceValue::String(value.into()),
    }
}

/// The option someone is typing, for [`AutocompleteFn`].
#[derive(Debug, Clone)]
pub struct Typed {
    /// The subcommand it's in, like `"show"` for `/memory show`, or `"gameboy save"`.
    /// Empty for a command without subcommands.
    pub subcommand: String,
    /// The option's name.
    pub option: String,
    /// What they typed so far.
    pub value: String,
}

/// Finds the option being typed, walking into subcommands.
fn find_typed(options: &[CommandDataOption], path: &mut Vec<String>) -> Option<Typed> {
    for option in options {
        match &option.value {
            CommandOptionValue::Focused(value, _) => {
                return Some(Typed {
                    subcommand: path.join(" "),
                    option: option.name.clone(),
                    value: value.clone(),
                });
            }
            CommandOptionValue::SubCommand(inner) | CommandOptionValue::SubCommandGroup(inner) => {
                path.push(option.name.clone());
                if let Some(typed) = find_typed(inner, path) {
                    return Some(typed);
                }
                path.pop();
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------------------
// CommandCtx: what a command's function gets
// ---------------------------------------------------------------------------------------

/// How far the answer to a command has got.
const NOT_ANSWERED: u8 = 0;
const DEFERRED: u8 = 1;
const ANSWERED: u8 = 2;

/// A slash command being run. Cheap to clone.
#[derive(Clone)]
pub struct CommandCtx {
    pub bot: BotCtx,
    pub interaction: Arc<Interaction>,
    pub data: Arc<CommandData>,
    /// Whether answers are private (only the person who used the command sees them).
    ephemeral: bool,
    /// [`NOT_ANSWERED`], [`DEFERRED`] or [`ANSWERED`], shared by the clones.
    state: Arc<AtomicU8>,
}

impl CommandCtx {
    pub fn new(bot: BotCtx, interaction: Arc<Interaction>, data: Arc<CommandData>) -> CommandCtx {
        CommandCtx {
            bot,
            interaction,
            data,
            ephemeral: false,
            state: Arc::new(AtomicU8::new(NOT_ANSWERED)),
        }
    }

    /// Makes every answer private: only the person who used the command sees it.
    pub fn ephemeral(mut self) -> CommandCtx {
        self.ephemeral = true;
        self
    }

    /// Reads the options into `T`, the same type the command was declared with.
    pub fn parse<T: CommandModel>(&self) -> Result<T> {
        let data = CommandInputData::from((*self.data).clone());
        T::from_interaction(data).map_err(|err| anyhow!("reading /{}: {err}", self.data.name))
    }

    pub fn author(&self) -> &User {
        // One of the two is always there: `member.user` in servers, `user` in DMs.
        self.interaction
            .member
            .as_ref()
            .and_then(|member| member.user.as_ref())
            .or(self.interaction.user.as_ref())
            .expect("an interaction has a user")
    }

    pub fn author_id(&self) -> UserId {
        self.author().id
    }

    pub fn guild_id(&self) -> Option<GuildId> {
        self.interaction.guild_id
    }

    pub fn channel_id(&self) -> ChannelId {
        channel_of(&self.interaction)
    }

    /// For a right-click "Apps" command on a message: that message.
    pub fn target_message(&self) -> Option<&Message> {
        let target = self.data.target_id?.cast();
        self.data.resolved.as_ref()?.messages.get(&target)
    }

    /// Shows "thinking…" so the answer can take longer than 3 seconds. The next
    /// `say` or `send` replaces it.
    pub async fn defer(&self) -> Result<()> {
        if self.state.swap(DEFERRED, Ordering::SeqCst) != NOT_ANSWERED {
            return Ok(());
        }
        let data = InteractionResponseData {
            flags: self.ephemeral.then_some(MessageFlags::EPHEMERAL),
            ..Default::default()
        };
        let response = InteractionResponse {
            kind: InteractionResponseType::DeferredChannelMessageWithSource,
            data: Some(data),
        };
        create_response(
            &self.bot,
            self.interaction.id,
            &self.interaction.token,
            &response,
        )
        .await
    }

    /// Answers with a line of text.
    pub async fn say(&self, text: impl Into<String>) -> Result<Reply> {
        self.send(Draft::new().content(text)).await
    }

    /// Answers with a message. The first answer is the reply to the command (replacing
    /// "thinking…" after a defer); later ones are follow-up messages.
    pub async fn send(&self, mut draft: Draft) -> Result<Reply> {
        if self.ephemeral {
            draft = draft.ephemeral();
        }
        let interaction = self.bot.http.interaction(self.bot.application_id);
        let token = &self.interaction.token;
        match self.state.swap(ANSWERED, Ordering::SeqCst) {
            NOT_ANSWERED => {
                let response = InteractionResponse {
                    kind: InteractionResponseType::ChannelMessageWithSource,
                    data: Some(response_data(draft)),
                };
                create_response(&self.bot, self.interaction.id, token, &response).await?;
                Ok(Reply {
                    token: token.clone(),
                    followup: None,
                })
            }
            DEFERRED => {
                edit_original(&self.bot, token, &draft).await?;
                Ok(Reply {
                    token: token.clone(),
                    followup: None,
                })
            }
            _ => {
                let message = followup(&interaction, token, &draft).await?;
                Ok(Reply {
                    token: token.clone(),
                    followup: Some(message),
                })
            }
        }
    }
}

/// An answer [`CommandCtx::send`] sent, to read or change later.
pub struct Reply {
    token: String,
    /// `None` for the first answer, which Discord keeps as the interaction's response.
    followup: Option<Message>,
}

impl Reply {
    /// The message itself.
    pub async fn message(&self, ctx: &CommandCtx) -> Result<Message> {
        if let Some(message) = &self.followup {
            return Ok(message.clone());
        }
        let http = ctx.bot.http.interaction(ctx.bot.application_id);
        Ok(http.response(&self.token).await?.model().await?)
    }

    /// Changes what the answer says.
    pub async fn edit(&self, ctx: &CommandCtx, draft: Draft) -> Result<Message> {
        match &self.followup {
            Some(message) => {
                let http = ctx.bot.http.interaction(ctx.bot.application_id);
                let mut request = http.update_followup(&self.token, message.id);
                if let Some(content) = &draft.content {
                    request = request.content(Some(content));
                }
                if let Some(components) = &draft.components {
                    request = request.components(Some(components));
                }
                if let Some(embeds) = &draft.embeds {
                    request = request.embeds(Some(embeds));
                }
                Ok(request.await?.model().await?)
            }
            None => edit_original(&ctx.bot, &self.token, &draft).await,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Buttons, menus and modals
// ---------------------------------------------------------------------------------------

/// A button press or a menu choice.
#[derive(Debug, Clone)]
pub struct ComponentInteraction {
    pub id: InteractionId,
    pub token: String,
    pub guild_id: Option<GuildId>,
    pub channel_id: ChannelId,
    /// Who pressed it.
    pub user: User,
    /// Their member info in this server. `None` in DMs.
    pub member: Option<PartialMember>,
    /// The message the button or menu is on.
    pub message: Message,
    /// The button's or menu's ID, like `reminders:snooze:12:10`.
    pub custom_id: String,
    /// What was picked in a menu. Empty for buttons.
    pub values: Vec<String>,
    /// The users, members, roles or channels picked in such a menu, by ID.
    pub resolved: Option<InteractionDataResolved>,
}

impl ComponentInteraction {
    /// Reads Discord's interaction. `None` when it isn't a button or menu.
    pub fn from_interaction(interaction: &Interaction) -> Option<ComponentInteraction> {
        let Some(InteractionData::MessageComponent(data)) = &interaction.data else {
            return None;
        };
        Some(ComponentInteraction {
            id: interaction.id,
            token: interaction.token.clone(),
            guild_id: interaction.guild_id,
            channel_id: channel_of(interaction),
            user: user_of(interaction)?,
            member: interaction.member.clone(),
            message: interaction.message.clone()?,
            custom_id: data.custom_id.clone(),
            values: data.values.clone(),
            resolved: data.resolved.clone(),
        })
    }
}

/// A submitted pop-up form.
#[derive(Debug, Clone)]
pub struct ModalInteraction {
    pub id: InteractionId,
    pub token: String,
    pub guild_id: Option<GuildId>,
    pub channel_id: ChannelId,
    pub user: User,
    /// The message whose button opened the form, if one did.
    pub message: Option<Message>,
    pub custom_id: String,
    /// What was typed in each text box: (the box's ID, the text).
    pub inputs: Vec<(String, String)>,
}

impl ModalInteraction {
    pub fn from_interaction(interaction: &Interaction) -> Option<ModalInteraction> {
        let Some(InteractionData::ModalSubmit(data)) = &interaction.data else {
            return None;
        };
        let mut inputs = Vec::new();
        collect_inputs(&data.components, &mut inputs);
        Some(ModalInteraction {
            id: interaction.id,
            token: interaction.token.clone(),
            guild_id: interaction.guild_id,
            channel_id: channel_of(interaction),
            user: user_of(interaction)?,
            message: interaction.message.clone(),
            custom_id: data.custom_id.clone(),
            inputs,
        })
    }

    /// The text typed in the first box, for forms with one.
    pub fn first_value(&self) -> Option<&str> {
        self.inputs.first().map(|(_, text)| text.as_str())
    }
}

/// Text boxes sit inside labels (or action rows, in older forms).
fn collect_inputs(components: &[ModalInteractionComponent], inputs: &mut Vec<(String, String)>) {
    for component in components {
        match component {
            ModalInteractionComponent::TextInput(input) => {
                inputs.push((input.custom_id.clone(), input.value.clone()));
            }
            ModalInteractionComponent::Label(label) => {
                collect_inputs(std::slice::from_ref(&label.component), inputs);
            }
            ModalInteractionComponent::ActionRow(row) => collect_inputs(&row.components, inputs),
            _ => {}
        }
    }
}

/// How to answer a button, menu or modal.
pub enum Response {
    /// A new message (private when the draft is [`Draft::ephemeral`]).
    Message(Draft),
    /// Changes the message the button is on.
    UpdateMessage(Draft),
    /// Opens a pop-up form.
    Modal(Modal),
    /// Answers without changing anything, so the button stops showing it's loading.
    Acknowledge,
}

/// Answering a button, menu or modal. Each interaction gets exactly one response (or a
/// defer), then any number of follow-ups and edits.
// The futures are only awaited on known types, where Rust can see they are `Send`.
#[allow(async_fn_in_trait)]
pub trait Respond {
    /// The interaction's ID and token, all a response needs.
    fn handle(&self) -> (InteractionId, &str);

    async fn create_response(&self, ctx: &BotCtx, response: Response) -> Result<()> {
        let (id, token) = self.handle();
        let response = match response {
            Response::Message(draft) => InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(response_data(draft)),
            },
            Response::UpdateMessage(draft) => InteractionResponse {
                kind: InteractionResponseType::UpdateMessage,
                data: Some(response_data(draft)),
            },
            Response::Modal(modal) => InteractionResponse {
                kind: InteractionResponseType::Modal,
                data: Some(InteractionResponseData {
                    custom_id: Some(modal.custom_id),
                    title: Some(modal.title),
                    components: Some(modal.components),
                    ..Default::default()
                }),
            },
            Response::Acknowledge => InteractionResponse {
                kind: InteractionResponseType::DeferredUpdateMessage,
                data: None,
            },
        };
        create_response(ctx, id, token, &response).await
    }

    /// Answers without changing anything yet; [`Respond::edit_response`] then changes the
    /// message the button is on.
    async fn defer(&self, ctx: &BotCtx) -> Result<()> {
        self.create_response(ctx, Response::Acknowledge).await
    }

    /// Shows a private "thinking…"; [`Respond::edit_response`] then replaces it.
    async fn defer_ephemeral(&self, ctx: &BotCtx) -> Result<()> {
        let (id, token) = self.handle();
        let response = InteractionResponse {
            kind: InteractionResponseType::DeferredChannelMessageWithSource,
            data: Some(InteractionResponseData {
                flags: Some(MessageFlags::EPHEMERAL),
                ..Default::default()
            }),
        };
        create_response(ctx, id, token, &response).await
    }

    /// Edits the response: the message the button is on after [`Respond::defer`], or the
    /// new message after [`Respond::defer_ephemeral`] or a [`Response::Message`].
    async fn edit_response(&self, ctx: &BotCtx, draft: Draft) -> Result<Message> {
        let (_, token) = self.handle();
        edit_original(ctx, token, &draft).await
    }

    /// Sends another message after the response.
    async fn create_followup(&self, ctx: &BotCtx, draft: Draft) -> Result<Message> {
        let (_, token) = self.handle();
        followup(&ctx.http.interaction(ctx.application_id), token, &draft).await
    }
}

impl Respond for ComponentInteraction {
    fn handle(&self) -> (InteractionId, &str) {
        (self.id, &self.token)
    }
}

impl Respond for ModalInteraction {
    fn handle(&self) -> (InteractionId, &str) {
        (self.id, &self.token)
    }
}

// ---------------------------------------------------------------------------------------
// The calls to Discord, shared by all of the above
// ---------------------------------------------------------------------------------------

/// Where an interaction happened.
fn channel_of(interaction: &Interaction) -> ChannelId {
    #[allow(deprecated, reason = "older interactions only have channel_id")]
    interaction
        .channel
        .as_ref()
        .map(|channel| channel.id)
        .or(interaction.channel_id)
        .unwrap_or_else(|| twilight_model::id::Id::new(1))
}

fn user_of(interaction: &Interaction) -> Option<User> {
    interaction
        .member
        .as_ref()
        .and_then(|member| member.user.clone())
        .or_else(|| interaction.user.clone())
}

/// A draft as the data of an interaction response.
pub fn response_data(draft: Draft) -> InteractionResponseData {
    let attachments = match (draft.files.is_empty(), &draft.keep_files) {
        (false, _) => Some(draft.files),
        // An empty list removes the message's files.
        (true, Some(_)) => Some(Vec::new()),
        (true, None) => None,
    };
    InteractionResponseData {
        allowed_mentions: draft.allowed_mentions,
        attachments,
        components: draft.components,
        content: draft.content,
        embeds: draft.embeds,
        flags: draft.flags,
        ..Default::default()
    }
}

pub async fn create_response(
    bot: &BotCtx,
    id: InteractionId,
    token: &str,
    response: &InteractionResponse,
) -> Result<()> {
    bot.http
        .interaction(bot.application_id)
        .create_response(id, token, response)
        .await
        .context("answering the interaction")?;
    Ok(())
}

/// Edits an interaction's response (the "thinking…" or first message).
async fn edit_original(bot: &BotCtx, token: &str, draft: &Draft) -> Result<Message> {
    let interaction = bot.http.interaction(bot.application_id);
    let mut request = interaction.update_response(token);
    if let Some(content) = &draft.content {
        request = request.content(Some(content));
    }
    if let Some(components) = &draft.components {
        request = request.components(Some(components));
    }
    if let Some(embeds) = &draft.embeds {
        request = request.embeds(Some(embeds));
    }
    if !draft.files.is_empty() {
        request = request.attachments(&draft.files);
    }
    if let Some(keep) = &draft.keep_files {
        request = request.keep_attachment_ids(keep);
    }
    if let Some(mentions) = &draft.allowed_mentions {
        request = request.allowed_mentions(Some(mentions));
    }
    if let Some(flags) = draft.flags {
        // Only flags like "components v2" can be set by an edit; ephemeral is fixed.
        request = request.flags(flags - MessageFlags::EPHEMERAL);
    }
    Ok(request.await?.model().await?)
}

async fn followup(
    interaction: &twilight_http::client::InteractionClient<'_>,
    token: &str,
    draft: &Draft,
) -> Result<Message> {
    let mut request = interaction.create_followup(token);
    if let Some(content) = &draft.content {
        request = request.content(content);
    }
    if let Some(components) = &draft.components {
        request = request.components(components);
    }
    if let Some(embeds) = &draft.embeds {
        request = request.embeds(embeds);
    }
    if !draft.files.is_empty() {
        request = request.attachments(&draft.files);
    }
    if let Some(mentions) = &draft.allowed_mentions {
        request = request.allowed_mentions(Some(mentions));
    }
    if let Some(flags) = draft.flags {
        request = request.flags(flags);
    }
    Ok(request.await?.model().await?)
}

/// Answers an autocomplete request with suggestions (at most 25).
pub async fn suggest(
    bot: &BotCtx,
    interaction: &Interaction,
    mut choices: Vec<Choice>,
) -> Result<()> {
    choices.truncate(25);
    let response = InteractionResponse {
        kind: InteractionResponseType::ApplicationCommandAutocompleteResult,
        data: Some(InteractionResponseData {
            choices: Some(choices),
            ..Default::default()
        }),
    };
    create_response(bot, interaction.id, &interaction.token, &response).await
}

/// The option being typed in an autocomplete request.
pub fn typed(data: &CommandData) -> Option<Typed> {
    find_typed(&data.options, &mut Vec::new())
}
