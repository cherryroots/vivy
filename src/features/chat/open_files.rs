//! `get_message`'s `open_files`: the files on the messages it opens (PDFs, Word, Excel and
//! PowerPoint files, videos, audio) handed to the model itself, whole or in part. Only for
//! a provider that opens files (`ChatProvider::opens_files`, Gemini): it reads them, and
//! bills only the part asked for, so she can look at an older message's file, or read a
//! big one a few pages at a time.

use futures::future::join_all;
use serde_json::Value;
use tracing::warn;

use crate::ai::{FilePart, ModelFile, ToolFile, document_mime};
use crate::util::discord::Message;
use crate::util::media;

/// Most files one call opens.
const MAX_FILES: usize = 3;
/// The biggest file opened, like Gemini's own limit.
const MAX_BYTES: usize = 50 * 1024 * 1024;

/// The parameters `get_message` gets when the model can open files.
pub fn parameters() -> Value {
    serde_json::json!({
        "open_files": {"type": "boolean", "description": "Also open the files on the messages: PDFs, Word, Excel, PowerPoint, video and audio."},
        "pages": {"type": "string", "description": "With open_files: only these pages or slides, like \"3-5\" or \"7\"."},
        "text_only": {"type": "boolean", "description": "With open_files: only the pages' text, without pictures of them. Much cheaper; for long documents, read the text first, then the pages with charts."},
        "sheet": {"type": "integer", "description": "With open_files: only this sheet of a spreadsheet, from 1."},
        "cells": {"type": "string", "description": "With open_files: only these cells, like \"A1:H50\"."},
        "from": {"type": "string", "description": "With open_files: where a video or audio starts, like \"1:30\"."},
        "to": {"type": "string", "description": "With open_files: where it ends, like \"3:00\"."}
    })
}

/// The files the call opens, and a note for the tool's text on what was opened.
pub async fn open(messages: &[&Message], args: &Value) -> (Vec<ToolFile>, String) {
    if args["open_files"] != true {
        return (Vec::new(), String::new());
    }
    let part = file_part(args);
    let attachments: Vec<_> = messages
        .iter()
        .flat_map(|msg| &msg.attachments)
        .filter(|a| opens(&a.filename, a.content_type.as_deref().unwrap_or_default()))
        .collect();
    if attachments.is_empty() {
        return (
            Vec::new(),
            "\n[No files to open on these messages.]".to_string(),
        );
    }
    let shown = &attachments[..attachments.len().min(MAX_FILES)];
    let downloads = shown.iter().map(|a| async move {
        if a.size as usize > MAX_BYTES {
            return Err(anyhow::anyhow!("it's over 50 MB"));
        }
        media::download(&a.url, MAX_BYTES).await
    });
    let mut files = Vec::new();
    let mut notes = Vec::new();
    for (attachment, result) in shown.iter().zip(join_all(downloads).await) {
        match result {
            Ok(data) => {
                notes.push(format!("{} is opened below", attachment.filename));
                files.push(ToolFile {
                    file: ModelFile {
                        name: attachment.filename.clone(),
                        mime: attachment
                            .content_type
                            .clone()
                            .unwrap_or_else(|| "application/octet-stream".to_string()),
                        data,
                    },
                    part: part.clone(),
                });
            }
            Err(err) => {
                warn!("couldn't open {}: {err:#}", attachment.filename);
                notes.push(format!("{} couldn't be opened", attachment.filename));
            }
        }
    }
    if attachments.len() > MAX_FILES {
        notes.push(format!(
            "{} more weren't opened (at most {MAX_FILES} a call)",
            attachments.len() - MAX_FILES
        ));
    }
    (files, format!("\n[{}.]", notes.join("; ")))
}

/// Whether a file named `name` of type `mime` is one the model opens.
fn opens(name: &str, mime: &str) -> bool {
    document_mime(name, mime).is_some() || mime.starts_with("video/") || mime.starts_with("audio/")
}

/// The part of the files the call asks for.
fn file_part(args: &Value) -> FilePart {
    let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    FilePart {
        pages: text("pages").and_then(pages),
        text_only: args["text_only"] == true,
        sheet: args["sheet"].as_u64().map(|n| n as u32).filter(|&n| n > 0),
        cells: text("cells").map(str::to_string),
        from: text("from").and_then(seconds),
        to: text("to").and_then(seconds),
    }
}

/// "3-5" as (3, 5), "7" as (7, 7). Pages count from 1.
fn pages(text: &str) -> Option<(u32, u32)> {
    let (first, last) = text.split_once('-').unwrap_or((text, text));
    let first: u32 = first.trim().parse().ok()?;
    let last: u32 = last.trim().parse().ok()?;
    (first > 0 && last >= first).then_some((first, last))
}

/// "90", "1:30" or "1:02:03" in seconds.
fn seconds(text: &str) -> Option<u64> {
    text.split(':').try_fold(0, |total, piece| {
        Some(total * 60 + piece.trim().parse::<u64>().ok()?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_part_asked_for() {
        let part = file_part(&json!({"pages": "3-5", "text_only": true, "sheet": 2,
            "cells": "A1:H50", "from": "1:30", "to": "1:02:03"}));
        assert_eq!(part.pages, Some((3, 5)));
        assert!(part.text_only);
        assert_eq!(part.sheet, Some(2));
        assert_eq!(part.cells.as_deref(), Some("A1:H50"));
        assert_eq!((part.from, part.to), (Some(90), Some(3723)));
        assert_eq!(pages("7"), Some((7, 7)));
        assert_eq!(pages("5-3"), None);
        assert_eq!(pages("0"), None);
        assert_eq!(file_part(&json!({})), FilePart::default());
    }

    #[test]
    fn opens_documents_video_and_audio() {
        assert!(opens("a.pdf", "application/pdf"));
        assert!(opens("plan.xlsx", "application/octet-stream"));
        assert!(opens("clip.mp4", "video/mp4"));
        assert!(opens("talk.mp3", "audio/mpeg"));
        assert!(!opens("notes.zip", "application/zip"));
    }
}
