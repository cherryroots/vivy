//! The `memory` chat tool, and the list of memory files chat adds to each question.
//!
//! The tool takes the same arguments as Anthropic's memory tool, which models know well. Claude
//! gets it as a normal tool too: Anthropic's own memory tool comes with an instruction to log
//! task progress, which filled people's files with one-off tasks.

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use tracing::info;

use super::folder::{self, Command, Folder, ROOT};
use super::{reflect, store};
use crate::ai::{ToolCall, ToolDef, ToolReply, ToolRunner};
use crate::core::{Asker, BotCtx, Result, user_error};

/// The most files listed next to a question. The model can view the folder for the rest.
const MAX_LISTED: usize = 50;
/// Vivy's notes about herself, shown at the start of each conversation.
pub const SELF_DIR: &str = "/memories/vivy";
/// Her notes on how she does recurring jobs. They're listed with the other files and
/// viewed when a job comes up, so they're left out of [`SELF_DIR`]'s notes shown up front.
pub const SKILLS_DIR: &str = "/memories/vivy/skills";
/// The most characters of those notes shown; she can view the rest.
const MAX_SELF: usize = 3000;
/// A built-in, read-only file with the rules for saving. They're here rather than in the
/// tool's description because the description goes with every request, and on Gemini
/// mostly uncached, while saves are rare: viewing this costs one tool round when she saves.
pub const RULES_PATH: &str = "/memories/how-to-save.md";
const RULES: &str = include_str!("how-to-save.md");

pub fn def() -> ToolDef {
    ToolDef {
        name: "memory",
        description: "Your long-term memory: text files under /memories that last between conversations, shared by everyone in a server and private in DMs. \
The newest message ends with <memory_files>: view the ones that matter before answering, especially the asker's own. \
People are in /memories/users/<user id>/ (about.md first), the server in /memories/server/, you in /memories/vivy/ (your how-to notes in /memories/vivy/skills/, viewed when their job comes up). \
Before your first save in a conversation, view /memories/how-to-save.md: what's worth saving, where, and who may change what. \
Commands: view (a file with line numbers, or a directory), create (a whole file), str_replace (text that appears once), insert (after insert_line; 0 is the top), delete, rename. Prefer str_replace and insert to rewriting. A file holds at most 8K.",
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["view", "create", "str_replace", "insert", "delete", "rename"]
                },
                "path": {"type": "string", "description": "A path under /memories. Every command but rename."},
                "view_range": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "view: [first, last] line, 1-based; last -1 means to the end."
                },
                "file_text": {"type": "string", "description": "create: the whole file."},
                "old_str": {"type": "string", "description": "str_replace: text that appears exactly once."},
                "new_str": {"type": "string", "description": "str_replace: the new text; leave out to delete old_str."},
                "insert_line": {"type": "integer", "description": "insert: the line to insert after."},
                "insert_text": {"type": "string", "description": "insert: the lines to add."},
                "old_path": {"type": "string", "description": "rename: what to move."},
                "new_path": {"type": "string", "description": "rename: where to move it."}
            },
            "required": ["command"]
        }),
    }
}

/// Runs one memory command for the asker. Mistakes in the command (a missing file, text
/// that appears twice) come back as text in the same words Claude's memory tool uses.
pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<String> {
    let scope = store::scope(asker.guild.map(|g| g.get()), asker.user.get());
    run_in(ctx, scope, asker.user.get(), args).await
}

/// Runs one memory command in the folder `scope`, for `user` (who the change log names).
pub async fn run_in(ctx: &BotCtx, scope: String, user: u64, args: &Value) -> Result<String> {
    // Models sometimes fill unused arguments with null or []; leave those out.
    let mut args = args.clone();
    if let Some(fields) = args.as_object_mut() {
        fields.retain(|_, v| !v.is_null() && v.as_array().is_none_or(|a| !a.is_empty()));
    }
    let command: Command = serde_json::from_value(args)
        .map_err(|err| user_error(format!("not a valid memory command: {err}")))?;
    if let Some(text) = rules_file(&command) {
        return Ok(text);
    }
    let writes = command.writes();
    let log_scope = scope.clone();

    let (text, changed) = ctx
        .db
        .call(move |conn| {
            // One transaction, so two answers editing the same file can't lose an edit.
            let tx = conn.transaction()?;
            let before = store::load(&tx, &scope)?;
            let mut after: Folder = before.clone();
            let result = folder::run(&mut after, command)
                .and_then(|text| folder::check_limits(&before, &after).map(|()| text));
            let text = match result {
                Ok(text) => text,
                Err(text) => return Ok((text, Vec::new())),
            };
            let changed: Vec<String> = folder::changes(&before, &after)
                .into_iter()
                .map(|(path, _)| path.to_string())
                .collect();
            if writes {
                store::save(&tx, &scope, &before, &after, user, Utc::now().timestamp())?;
                tx.commit()?;
            }
            Ok((text, changed))
        })
        .await?;
    for path in changed {
        info!(scope = log_scope, "memory: {path} changed for <@{user}>");
    }
    Ok(text)
}

/// What a command on the built-in rules file gets, or `None` for any other path: viewing
/// it shows the rules, and changing it is refused.
fn rules_file(command: &Command) -> Option<String> {
    let path = match command {
        Command::View { path, .. }
        | Command::Create { path, .. }
        | Command::StrReplace { path, .. }
        | Command::Insert { path, .. }
        | Command::Delete { path } => path,
        Command::Rename { old_path, new_path } if old_path.trim() != RULES_PATH => new_path,
        Command::Rename { old_path, .. } => old_path,
    };
    if path.trim() != RULES_PATH {
        return None;
    }
    Some(match command {
        Command::View { .. } => RULES.to_string(),
        _ => format!("{RULES_PATH} is built in and can't be changed."),
    })
}

/// Runs memory commands in one folder as Vivy herself, for the reflection and the diary,
/// where nobody is asking.
pub struct FolderRunner {
    pub ctx: BotCtx,
    pub scope: String,
}

#[async_trait]
impl ToolRunner for FolderRunner {
    async fn run(&self, call: &ToolCall) -> ToolReply {
        if call.name != "memory" {
            return format!("Error: there is no tool named {}.", call.name).into();
        }
        let user = self.ctx.bot_id.get();
        match run_in(&self.ctx, self.scope.clone(), user, &call.args).await {
            Ok(text) => text.into(),
            Err(err) => format!("Error: {err:#}").into(),
        }
    }
}

/// Vivy's own notes about herself in this server, the files under [`SELF_DIR`], or `None`
/// when she hasn't written any yet.
pub async fn self_notes(ctx: &BotCtx, asker: &Asker) -> Result<Option<String>> {
    let scope = store::scope(asker.guild.map(|g| g.get()), asker.user.get());
    self_notes_in(ctx, scope).await
}

pub async fn self_notes_in(ctx: &BotCtx, scope: String) -> Result<Option<String>> {
    let files = ctx
        .db
        .call(move |conn| Ok(store::files_under(conn, &scope, SELF_DIR)?))
        .await?;
    Ok(render_self(&files))
}

fn render_self(files: &[(String, String)]) -> Option<String> {
    let skills = format!("{SKILLS_DIR}/");
    let notes: Vec<_> = files
        .iter()
        .filter(|(path, _)| !path.starts_with(&skills))
        .collect();
    if notes.is_empty() {
        return None;
    }
    let all = notes
        .iter()
        .map(|(path, content)| format!("# {path}\n{}", content.trim_end()))
        .collect::<Vec<_>>()
        .join("\n\n");
    let shown = if all.chars().count() > MAX_SELF {
        let cut: String = all.chars().take(MAX_SELF).collect();
        format!("{cut}\n[cut off; view {SELF_DIR} for the rest, and make it shorter]")
    } else {
        all
    };
    Some(format!("<vivy_self>\n{shown}\n</vivy_self>"))
}

/// Her mood line in the asker's server, for every question, so it can shape how she talks.
/// `None` in DMs or without a mood.
pub async fn mood_note(ctx: &BotCtx, asker: &Asker) -> Result<Option<String>> {
    let Some(guild) = asker.guild else {
        return Ok(None);
    };
    let scope = store::scope(Some(guild.get()), asker.user.get());
    let files = ctx
        .db
        .call(move |conn| Ok(store::files_under(conn, &scope, SELF_DIR)?))
        .await?;
    let mood = files
        .iter()
        .find(|(path, _)| path == reflect::MOOD_FILE)
        .and_then(|(_, file)| reflect::mood_line(file, "mood"));
    Ok(mood.map(|mood| format!("<vivy_mood>{mood}</vivy_mood>")))
}

/// The list of memory files that chat adds to the question, so the model knows what it
/// remembers without the files being in every request.
pub async fn file_list(ctx: &BotCtx, asker: &Asker) -> Result<String> {
    let scope = store::scope(asker.guild.map(|g| g.get()), asker.user.get());
    let files = ctx
        .db
        .call(move |conn| Ok(store::list(conn, &scope)?))
        .await?;
    Ok(render_list(&files, asker.user.get()))
}

/// The asker's own files and the server's files one by one, and the other people's folders
/// as one line each, so the list stays short in a big server.
fn render_list(files: &[(String, usize)], asker: u64) -> String {
    if files.is_empty() {
        return format!("<memory_files>\n{RULES_PATH} (built in)\n</memory_files>");
    }
    let own = format!("{ROOT}/users/{asker}/");
    let users = format!("{ROOT}/users/");
    let mut lines = vec![format!("{RULES_PATH} (built in)")];
    // Other people's folders: path → (files, bytes), in path order.
    let mut folders: Vec<(String, usize, usize)> = Vec::new();
    for (path, size) in files {
        let other = path
            .strip_prefix(&users)
            .filter(|_| !path.starts_with(&own))
            .and_then(|rest| rest.split_once('/'))
            .map(|(id, _)| format!("{users}{id}/"));
        match other {
            Some(dir) => match folders.last_mut() {
                Some((last, count, bytes)) if *last == dir => {
                    *count += 1;
                    *bytes += size;
                }
                _ => folders.push((dir, 1, *size)),
            },
            None => lines.push(format!("{path} ({})", folder::human_size(*size))),
        }
    }
    for (dir, count, bytes) in folders {
        let files = if count == 1 { "file" } else { "files" };
        lines.push(format!(
            "{dir} ({count} {files}, {})",
            folder::human_size(bytes)
        ));
    }
    let extra = lines.len().saturating_sub(MAX_LISTED);
    lines.truncate(MAX_LISTED);
    if extra > 0 {
        lines.push(format!("…and {extra} more; view {ROOT} for the rest"));
    }
    format!("<memory_files>\n{}\n</memory_files>", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_own_files_and_other_folders() {
        assert_eq!(
            render_list(&[], 1),
            "<memory_files>\n/memories/how-to-save.md (built in)\n</memory_files>"
        );
        let files: Vec<(String, usize)> = [
            ("/memories/server/events.md", 1229),
            ("/memories/users/1/about.md", 300),
            ("/memories/users/1/games.md", 100),
            ("/memories/users/2/about.md", 512),
            ("/memories/users/2/movies.md", 512),
            ("/memories/users/3/about.md", 50),
        ]
        .iter()
        .map(|(p, s)| (p.to_string(), *s))
        .collect();
        assert_eq!(
            render_list(&files, 1),
            "<memory_files>
/memories/how-to-save.md (built in)
/memories/server/events.md (1.2K)
/memories/users/1/about.md (0.3K)
/memories/users/1/games.md (0.1K)
/memories/users/2/ (2 files, 1.0K)
/memories/users/3/ (1 file, 0.0K)
</memory_files>"
        );
        let many: Vec<(String, usize)> = (0..60)
            .map(|n| (format!("/memories/users/{n}/about.md"), 1))
            .collect();
        let text = render_list(&many, 1000);
        assert_eq!(text.lines().count(), 53);
        assert!(text.contains("…and 11 more"));
    }

    #[test]
    fn the_saving_rules_are_built_in() {
        let view = Command::View {
            path: RULES_PATH.to_string(),
            view_range: None,
        };
        assert!(rules_file(&view).unwrap().contains("What to save"));
        let create = Command::Create {
            path: RULES_PATH.to_string(),
            file_text: "x".to_string(),
        };
        assert!(rules_file(&create).unwrap().contains("can't be changed"));
        let rename = Command::Rename {
            old_path: "/memories/a.md".to_string(),
            new_path: RULES_PATH.to_string(),
        };
        assert!(rules_file(&rename).is_some());
        let other = Command::View {
            path: "/memories/a.md".to_string(),
            view_range: None,
        };
        assert!(rules_file(&other).is_none());
    }

    #[test]
    fn shows_vivys_own_notes() {
        assert_eq!(render_self(&[]), None);
        let files = vec![
            (
                "/memories/vivy/interests.md".to_string(),
                "horror films\n".to_string(),
            ),
            (
                "/memories/vivy/personality.md".to_string(),
                "dry humor".to_string(),
            ),
            (
                "/memories/vivy/skills/diary.md".to_string(),
                "Three short paragraphs.".to_string(),
            ),
        ];
        assert_eq!(
            render_self(&files).unwrap(),
            "<vivy_self>\n# /memories/vivy/interests.md\nhorror films\n\n# /memories/vivy/personality.md\ndry humor\n</vivy_self>"
        );
        let only_skills = vec![files[2].clone()];
        assert_eq!(render_self(&only_skills), None);
        let long = vec![("/memories/vivy/a.md".to_string(), "é".repeat(4000))];
        let text = render_self(&long).unwrap();
        assert!(text.contains("[cut off; view /memories/vivy"));
        assert!(text.chars().count() < 3200);
    }

    #[test]
    fn the_tool_takes_every_command() {
        let def = def();
        let commands = def.parameters["properties"]["command"]["enum"].clone();
        for name in commands.as_array().unwrap() {
            let mut args = json!({"command": name, "path": "/memories/a", "file_text": "",
                "old_str": "x", "insert_line": 0, "insert_text": "", "old_path": "/memories/a",
                "new_path": "/memories/b"});
            if name == "view" {
                args["view_range"] = json!([1, -1]);
            }
            assert!(serde_json::from_value::<Command>(args).is_ok(), "{name}");
        }
    }
}
