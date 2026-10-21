//! What a game needs to have Vivy speak as herself: a model call with no tools, and her
//! memory notes, read without importing the memory feature (it finds whichever feature
//! offers the `memory` chat tool and runs `view` with it, as Vivy, the way a chat answer
//! would). With memory off in the server, or no such file, a note is simply `None`.

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use crate::ai::{ToolCall, ToolRunner};
use crate::core::{Asker, BotCtx};
use crate::util::discord::{ChannelId, GuildId, MessageId};

/// For a call with no tools: any call is answered with an error.
pub struct NoTools;

#[async_trait]
impl ToolRunner for NoTools {
    async fn run(&self, call: &ToolCall) -> String {
        format!("Error: there is no tool named {}.", call.name)
    }
}

/// The memory file at `path`, at most `most` characters (cut at a word), or `None` without
/// it. `message` is any message of the game, for the memory feature's records.
pub async fn memory_file(
    ctx: &BotCtx,
    guild: GuildId,
    channel: ChannelId,
    message: MessageId,
    path: &str,
    most: usize,
) -> Option<String> {
    if !ctx.allows("memory", Some(guild), channel) {
        return None;
    }
    let memory = ctx
        .features
        .iter()
        .find(|f| f.tools().iter().any(|t| t.name == "memory"))?;
    let herself = Asker {
        user: ctx.bot_id,
        guild: Some(guild),
        channel,
        message,
        posted: Default::default(),
    };
    let args = json!({"command": "view", "path": path});
    match memory.run_tool(ctx, &herself, "memory", &args).await {
        Ok(view) => file_text(&view).map(|text| crate::util::shorten_at_word(&text, most)),
        Err(err) => {
            debug!("couldn't read {path}: {err:#}");
            None
        }
    }
}

/// A file's text from what `view` answered, without the header and line numbers. `None`
/// when it isn't a file's content (a directory, a missing path).
pub fn file_text(view: &str) -> Option<String> {
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
    fn a_file_view_loses_its_line_numbers() {
        let view = "Here's the content of /memories/users/7/games.md with line numbers:\n     1\tPlays chess\n     2\tHates losing";
        assert_eq!(
            file_text(view).as_deref(),
            Some("Plays chess\nHates losing")
        );
        assert_eq!(file_text("Here're the files ..."), None);
    }
}
