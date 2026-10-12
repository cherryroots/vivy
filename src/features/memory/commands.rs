//! `/memory show`, `/memory forget` and `/memory delete`. The answers are only visible to
//! the person who asked.

use chrono::Utc;
use poise::CreateReply;
use serenity::all::{
    AutocompleteChoice, ButtonStyle, ComponentInteraction, CreateActionRow, CreateAllowedMentions,
    CreateAttachment, CreateAutocompleteResponse, CreateButton, CreateInteractionResponse,
    CreateInteractionResponseMessage,
};
use tracing::info;

use super::folder::{self, Folder, ROOT};
use super::store;
use crate::core::{BotCtx, Context, Result, user_error};

/// Longest file shown in the message itself; longer ones come as an attachment.
const MAX_INLINE: usize = 1800;

/// See or wipe what Vivy remembers
#[poise::command(
    slash_command,
    subcommands("show", "forget", "delete"),
    subcommand_required
)]
pub async fn memory(_ctx: Context<'_>) -> Result<()> {
    Ok(())
}

/// Show a memory file or folder (default: what Vivy saved about you)
#[poise::command(slash_command, ephemeral)]
async fn show(
    ctx: Context<'_>,
    #[description = "A path like /memories/server/culture.md, or /memories for everything"]
    #[autocomplete = "any_file"]
    path: Option<String>,
) -> Result<()> {
    let path = match path {
        Some(path) => folder::clean_path(&path).map_err(user_error)?,
        // Like forget: your folder, or in DMs everything (it's all yours).
        None => own_area(ctx),
    };
    let folder = load(ctx).await?;
    let text = dump(&folder, &path);
    let reply = if text.is_empty() && path == own_area(ctx) {
        CreateReply::default().content("I haven't saved anything about you here.")
    } else if text.is_empty() {
        CreateReply::default().content(format!("There's nothing at {path}."))
    } else if text.len() <= MAX_INLINE {
        CreateReply::default().content(format!("```md\n{}\n```", fence_safe(&text)))
    } else {
        CreateReply::default()
            .content(format!("**{path}**"))
            .attachment(CreateAttachment::bytes(text.into_bytes(), file_name(&path)))
    };
    ctx.send(reply.allowed_mentions(CreateAllowedMentions::new()))
        .await?;
    Ok(())
}

/// Delete one of your memory files, or everything Vivy remembers about you here
#[poise::command(slash_command, ephemeral)]
async fn forget(
    ctx: Context<'_>,
    #[description = "The file to delete (default: all of them)"]
    #[autocomplete = "own_file"]
    file: Option<String>,
) -> Result<()> {
    let Some(file) = file else {
        // Nothing picked means everything: that's easy to do by accident on a phone, so it
        // asks first. The button does the deleting (see `on_component`).
        let area = own_area(ctx);
        let folder = load(ctx).await?;
        let prefix = format!("{area}/");
        let files = folder
            .keys()
            .filter(|p| **p == area || p.starts_with(&prefix) || area == ROOT)
            .count();
        if files == 0 {
            ctx.say("There was nothing to forget.").await?;
            return Ok(());
        }
        let user = ctx.author().id.get();
        let buttons = CreateActionRow::Buttons(vec![
            CreateButton::new(format!("memory:forget:{user}"))
                .label("Forget everything")
                .style(ButtonStyle::Danger),
            CreateButton::new(format!("memory:keep:{user}"))
                .label("Cancel")
                .style(ButtonStyle::Secondary),
        ]);
        ctx.send(
            CreateReply::default()
                .content(format!(
                    "This deletes all {} I saved about you here. Sure?",
                    crate::util::count(files, "file")
                ))
                .components(vec![buttons]),
        )
        .await?;
        return Ok(());
    };
    let path = folder::clean_path(&file).map_err(user_error)?;
    if !is_own(ctx, &path) {
        return Err(user_error(
            "You can only forget your own files. Pick one from the list.",
        ));
    }
    let text = match remove(ctx, &path).await? {
        0 => "There was nothing to forget.".to_string(),
        _ => format!("Done. I've forgotten {path}."),
    };
    ctx.say(text).await?;
    Ok(())
}

/// The reply after everything about a person was forgotten.
const FORGOTTEN: &str = "Done. I've forgotten what I saved about you here. \
    Notes other people or the server's own files may still mention you; \
    an admin can remove those with /memory delete.";

/// The "Forget everything" and "Cancel" buttons under `/memory forget`'s question.
/// `action` is `forget:<user>` or `keep:<user>`.
pub async fn on_component(bot: &BotCtx, i: &ComponentInteraction, action: &str) -> Result<()> {
    let (verb, user) = action.split_once(':').unwrap_or((action, ""));
    let user: u64 = user
        .parse()
        .map_err(|_| user_error("That button is broken."))?;
    if i.user.id.get() != user {
        return Err(user_error("Only the person who asked can press this."));
    }
    let text = match verb {
        "forget" => {
            let guild = i.guild_id.map(|g| g.get());
            let area = match guild {
                Some(_) => format!("{ROOT}/users/{user}"),
                None => ROOT.to_string(),
            };
            remove_in(bot, guild, user, &area).await?;
            FORGOTTEN
        }
        _ => "Kept everything.",
    };
    let response = CreateInteractionResponseMessage::new()
        .content(text)
        .components(Vec::new());
    i.create_response(
        &bot.http,
        CreateInteractionResponse::UpdateMessage(response),
    )
    .await?;
    Ok(())
}

/// Delete a memory file or folder (admins)
#[poise::command(slash_command, ephemeral)]
async fn delete(
    ctx: Context<'_>,
    #[description = "A path like /memories/users/123/games.md, or a folder"]
    #[autocomplete = "any_file"]
    path: String,
) -> Result<()> {
    if !ctx.data().is_admin(ctx.author().id) {
        return Err(user_error(
            "Only admins can use /memory delete. To remove your own notes, use /memory forget.",
        ));
    }
    let path = folder::clean_path(&path).map_err(user_error)?;
    let count = remove(ctx, &path).await?;
    let text = match count {
        0 => format!("There's nothing at {path}."),
        1 => format!("Deleted {path}."),
        n => format!("Deleted {path} ({n} files)."),
    };
    ctx.say(text).await?;
    Ok(())
}

/// `/memories/users/<your id>`
fn own_folder(ctx: Context<'_>) -> String {
    format!("{ROOT}/users/{}", ctx.author().id)
}

/// What `/memory forget` may delete: your folder, or in DMs the whole private folder.
fn own_area(ctx: Context<'_>) -> String {
    if ctx.guild_id().is_some() {
        own_folder(ctx)
    } else {
        ROOT.to_string()
    }
}

fn is_own(ctx: Context<'_>, path: &str) -> bool {
    let area = own_area(ctx);
    path == area || path.starts_with(&format!("{area}/"))
}

/// Your own files that contain what you typed.
async fn own_file(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let area = own_area(ctx);
    suggest(ctx, partial, |path| path.starts_with(&format!("{area}/"))).await
}

/// Every file here that contains what you typed.
async fn any_file(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    suggest(ctx, partial, |_| true).await
}

async fn suggest(
    ctx: Context<'_>,
    partial: &str,
    keep: impl Fn(&str) -> bool,
) -> CreateAutocompleteResponse {
    let scope = scope(ctx);
    let files = ctx
        .data()
        .db
        .call(move |conn| Ok(store::list(conn, &scope)?))
        .await
        .unwrap_or_default();
    let partial = partial.to_lowercase();
    let choices = files
        .into_iter()
        .map(|(path, _)| path)
        // Discord allows 100 characters per choice.
        .filter(|path| path.len() <= 100 && keep(path) && path.to_lowercase().contains(&partial))
        .take(25)
        .map(|path| AutocompleteChoice::new(path.clone(), path))
        .collect();
    CreateAutocompleteResponse::new().set_choices(choices)
}

fn scope(ctx: Context<'_>) -> String {
    store::scope(ctx.guild_id().map(|g| g.get()), ctx.author().id.get())
}

async fn load(ctx: Context<'_>) -> Result<Folder> {
    let scope = scope(ctx);
    ctx.data()
        .db
        .call(move |conn| Ok(store::load(conn, &scope)?))
        .await
}

/// Deletes a file, or everything under a directory, and logs it. Returns how many files.
async fn remove(ctx: Context<'_>, path: &str) -> Result<usize> {
    let guild = ctx.guild_id().map(|g| g.get());
    remove_in(ctx.data(), guild, ctx.author().id.get(), path).await
}

/// Deletes `path` (a file, or a folder with everything in it) from the memory of `guild`
/// (or the DM folder of `user`), as `user`. Returns how many files went.
async fn remove_in(bot: &BotCtx, guild: Option<u64>, user: u64, path: &str) -> Result<usize> {
    let scope = store::scope(guild, user);
    let target = path.to_string();
    let log_scope = scope.clone();
    let count = bot
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let before = store::load(&tx, &scope)?;
            let prefix = format!("{target}/");
            let after: Folder = before
                .iter()
                .filter(|(p, _)| **p != target && !p.starts_with(&prefix))
                .map(|(p, c)| (p.clone(), c.clone()))
                .collect();
            let count = store::save(&tx, &scope, &before, &after, user, Utc::now().timestamp())?;
            tx.commit()?;
            Ok(count)
        })
        .await?;
    if count > 0 {
        info!(scope = log_scope, "memory: {path} deleted with /memory");
    }
    Ok(count)
}

/// A file, or every file in a directory, each under its path as a heading.
fn dump(folder: &Folder, path: &str) -> String {
    let prefix = format!("{path}/");
    folder
        .iter()
        .filter(|(p, _)| *p == path || p.starts_with(&prefix) || path == ROOT)
        .map(|(p, content)| format!("# {p}\n{}", content.trim_end()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Keeps a file's own ``` from ending the code block early.
fn fence_safe(text: &str) -> String {
    text.replace("```", "`\u{200b}``")
}

/// "/memories/users/1" → "1.md", "/memories/users/1/games.md" → "games.md"
fn file_name(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or("memory");
    if name.contains('.') {
        name.to_string()
    } else {
        format!("{name}.md")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_and_fences() {
        assert_eq!(file_name("/memories/users/1/games.md"), "games.md");
        assert_eq!(file_name("/memories/users/1"), "1.md");
        assert_eq!(fence_safe("a ``` b"), "a `\u{200b}`` b");
    }

    #[test]
    fn dumps_a_folder() {
        let folder: Folder = [
            ("/memories/users/1/about.md", "Cherry\n"),
            ("/memories/users/1/games.md", "likes chess"),
            ("/memories/users/10/about.md", "Bob"),
        ]
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect();
        assert_eq!(
            dump(&folder, "/memories/users/1"),
            "# /memories/users/1/about.md\nCherry\n\n# /memories/users/1/games.md\nlikes chess"
        );
        assert_eq!(
            dump(&folder, "/memories/users/1/games.md"),
            "# /memories/users/1/games.md\nlikes chess"
        );
        assert_eq!(dump(&folder, "/memories").matches("# ").count(), 3);
        assert_eq!(dump(&folder, "/memories/users/2"), "");
    }
}
