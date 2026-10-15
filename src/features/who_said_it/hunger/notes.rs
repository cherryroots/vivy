//! What Vivy knows about the tributes: her memory notes about each one, so a round can
//! bring in something fun about them (a favourite game, an in-joke).
//!
//! The memory feature keeps her notes about a person in `/memories/users/<user id>/`. This
//! file doesn't import that feature: it finds whichever feature offers the `memory` chat
//! tool and runs `view` with it, as Vivy herself, the same way a chat answer would. With
//! memory off in the server, or no notes, a tribute simply has none.

use serde_json::json;
use serenity::all::{ChannelId, GuildId, MessageId};
use tracing::debug;

use crate::core::{Asker, BotCtx};

/// The most of one tribute's notes she's given, in characters.
const MOST: usize = 800;
/// The most files read per tribute.
const MOST_FILES: usize = 6;

/// Her notes about each of `tributes`, in the same order: `None` for someone she has no
/// notes about.
pub async fn about(
    ctx: &BotCtx,
    guild: GuildId,
    channel: ChannelId,
    message: MessageId,
    tributes: &[u64],
) -> Vec<Option<String>> {
    let mut notes = vec![None; tributes.len()];
    if !ctx.allows("memory", Some(guild), channel) {
        return notes;
    }
    let Some(memory) = ctx
        .features
        .iter()
        .find(|f| f.tools().iter().any(|t| t.name == "memory"))
    else {
        return notes;
    };
    let herself = Asker {
        user: ctx.bot_id,
        guild: Some(guild),
        channel,
        message,
        posted: Default::default(),
    };
    for (i, id) in tributes.iter().enumerate() {
        let dir = format!("/memories/users/{id}");
        let view = |path: String| {
            let herself = &herself;
            async move {
                let args = json!({"command": "view", "path": path});
                memory.run_tool(ctx, herself, "memory", &args).await
            }
        };
        let listing = match view(dir.clone()).await {
            Ok(listing) => listing,
            Err(err) => {
                debug!("couldn't list the notes about {id}: {err:#}");
                continue;
            }
        };
        let mut text = String::new();
        for file in files_in(&listing, &dir).into_iter().take(MOST_FILES) {
            if let Ok(content) = view(file.clone()).await
                && let Some(body) = file_text(&content)
            {
                text.push_str(&format!("{}:\n{body}\n", file.trim_start_matches(&dir)));
            }
        }
        let text = text.trim();
        if !text.is_empty() {
            notes[i] = Some(crate::util::shorten_at_word(text, MOST));
        }
    }
    notes
}

/// The files a directory listing names under `dir` (not `dir` itself, nor anything that
/// isn't a file: a directory's view lists instead of showing text, see [`file_text`]).
fn files_in(listing: &str, dir: &str) -> Vec<String> {
    if !listing.starts_with("Here're the files") {
        return Vec::new();
    }
    listing
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once('\t').map(|(_, path)| path.trim()))
        .filter(|path| path.starts_with(&format!("{dir}/")))
        .map(str::to_string)
        .collect()
}

/// A file's text from what `view` answered, without the header and line numbers. `None`
/// when it isn't a file's content (a directory, a missing path).
fn file_text(view: &str) -> Option<String> {
    let mut lines = view.lines();
    if !lines.next()?.starts_with("Here's the content of") {
        return None;
    }
    let body: Vec<&str> = lines
        .map(|line| line.split_once('\t').map_or(line, |(_, text)| text))
        .collect();
    let body = body.join("\n");
    let body = body.trim();
    (!body.is_empty()).then(|| body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_gives_the_files_under_the_folder() {
        let listing = "Here're the files and directories up to 2 levels deep in /memories/users/7, excluding hidden items and node_modules:\n\
                       1.0K\t/memories/users/7\n\
                       0.5K\t/memories/users/7/games.md\n\
                       0.5K\t/memories/users/7/likes.md";
        assert_eq!(
            files_in(listing, "/memories/users/7"),
            ["/memories/users/7/games.md", "/memories/users/7/likes.md"]
        );
        assert!(
            files_in(
                "The path /memories/users/7 does not exist.",
                "/memories/users/7"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_file_view_loses_its_line_numbers() {
        let view = "Here's the content of /memories/users/7/games.md with line numbers:\n     1\tPlays chess\n     2\tHates losing";
        assert_eq!(
            file_text(view).as_deref(),
            Some("Plays chess\nHates losing")
        );
        assert_eq!(file_text("Here're the files ..."), None);
    }
}
