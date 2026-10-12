//! Producing one answer: stream it from the model into Discord, run the tools it asks for,
//! and attach the files it makes.
//!
//! While the answer streams, the reply is edited once a second. Its last line is a small
//! status (`-# 💭 Thinking…`, `-# 🔧 Reading recent messages…`) that disappears when the
//! answer is done, or turns into `-# ⏹️ Stopped` or `-# ⚠️ Something went wrong: …`.
//! Above it, while the model works through tools, goes its latest progress note
//! (`-# 💬 Found the reminder, now moving it`), until it starts writing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use serenity::all::{CreateAttachment, MessageId};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::ai::{
    Activity, ChatEvent, ChatProvider, ChatRequest, Done, GeneratedFile, Input, NativeRound,
    ToolCall, ToolDef, ToolOutput, ToolReply, next_input,
};
use crate::core::errors::is_user_error;
use crate::core::{Asker, BotCtx, Feature};
use crate::util::reply::LiveReply;
use crate::util::shorten;
use crate::util::split::{DISCORD_LIMIT, close_code_block, split_message};

use super::store::Written;

/// The fixed instructions. Static, so every request starts the same and hits the cache.
pub(super) const SYSTEM_PROMPT: &str = include_str!("prompt.md");
/// Room kept in the last message for the progress note and the status line.
const STATUS_ROOM: usize = 300;
/// The most characters of the status line, and of the progress note.
const STATUS_LENGTH: usize = 95;
const NOTE_LENGTH: usize = 190;
/// Most rounds of tool calls in one answer, so a confused model can't loop forever.
const MAX_ROUNDS: usize = 8;
/// Discord allows 10 files per message.
const MAX_FILES: usize = 10;
/// Shown when the model answered with tools or files only.
const EMPTY_ANSWER: &str = "-# (nothing to say this time; 🔁 asks again)";

pub struct Job {
    pub asker: Asker,
    pub reply: LiveReply,
    pub input: Input,
    pub cancel: CancellationToken,
    /// Kept up to date with the reply's messages, so ❌ on any of them finds this answer.
    pub message_ids: Arc<Mutex<Vec<MessageId>>>,
}

pub enum End {
    Finished,
    Stopped,
    Failed(anyhow::Error),
}

pub struct Outcome {
    pub text: String,
    /// How to continue from this answer. `None` when it didn't finish.
    pub written: Option<Written>,
    pub message_ids: Vec<MessageId>,
    pub end: End,
}

/// The text of the reply so far and the status line under it.
struct Screen {
    reply: LiveReply,
    text: String,
    /// The model's latest progress note, shown above the status.
    note: Option<String>,
    status: Option<String>,
    /// What Discord shows now, to skip edits that change nothing.
    shown: Vec<String>,
    message_ids: Arc<Mutex<Vec<MessageId>>>,
}

impl Screen {
    async fn show(&mut self, ctx: &BotCtx) -> anyhow::Result<()> {
        let parts = render(&self.text, self.note.as_deref(), self.status.as_deref());
        if parts == self.shown {
            return Ok(());
        }
        self.reply.show(&ctx.http, &parts).await?;
        self.shown = parts;
        *self.message_ids.lock().unwrap() = self.reply.message_ids();
        Ok(())
    }
}

/// The reply's first look, before any slow work: no text, just the Thinking status.
pub(super) fn placeholder() -> Vec<String> {
    render("", None, Some(&activity_status(&Activity::Thinking)))
}

/// The messages to show: the text split to fit, with the note and the status as the last
/// lines.
fn render(text: &str, note: Option<&str>, status: Option<&str>) -> Vec<String> {
    let text = discord_links(text);
    // Every part but the last fills a whole message; the last keeps room for the lines below.
    let mut parts = split_message(&text, DISCORD_LIMIT);
    if let Some(last) = parts.pop() {
        parts.extend(split_message(&last, DISCORD_LIMIT - STATUS_ROOM));
    }
    // Each is cut to fit, so the last message stays under Discord's limit.
    let mut lines = Vec::new();
    if let Some(note) = note {
        // One line: a note's own line breaks would end the small `-#` text.
        let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
        lines.push(format!("-# 💬 {}", shorten(&note, NOTE_LENGTH)));
    }
    if let Some(status) = status {
        lines.push(format!("-# {}", shorten(status, STATUS_LENGTH)));
    }
    if !lines.is_empty() {
        let line = lines.join("\n");
        match parts.last_mut() {
            Some(last) => {
                // While code streams, the block is still open: close it, or the lines
                // below would show as code.
                *last = close_code_block(last);
                last.push('\n');
                last.push_str(&line);
            }
            None => parts.push(line),
        }
    }
    parts
}

fn activity_status(activity: &Activity) -> String {
    match activity {
        Activity::Thinking => "💭 Thinking…".to_string(),
        Activity::SearchingWeb => "🔎 Searching the web…".to_string(),
        Activity::Searching(query) => format!("🔎 Searching “{}”…", shorten(query, 60)),
        Activity::Reading(site) => format!("📖 Reading {site}…"),
        Activity::RunningCode => "🐍 Running code…".to_string(),
        Activity::Writing => "✍️ Writing…".to_string(),
    }
}

/// The status line when the answer failed: what happened in plain words and what to do,
/// not the provider's error (that goes to the log). The failed turn is saved, so 🔁 retries.
fn friendly_error(err: &anyhow::Error) -> String {
    let text = format!("{err:#}").to_lowercase();
    let what = if text.contains("declined to answer") || text.contains("refus") {
        "I can't answer that one."
    } else if text.contains("kept calling tools") || text.contains("kept pausing") {
        "I got stuck going round in circles. 🔁 to try again."
    } else if text.contains("out of credit")
        || text.contains("insufficient")
        || text.contains("billing")
        || text.contains("quota")
    {
        "The AI is out of credit this month. 🔁 later to try again."
    } else if text.contains("overloaded")
        || text.contains("timed out")
        || text.contains("stream ended")
        || text.contains("answered 5")
        || text.contains("answered 429")
        || text.contains("connect")
    {
        "The AI is overloaded right now. 🔁 in a minute to try again."
    } else {
        "Something went wrong (it's logged). 🔁 to try again."
    };
    format!("⚠️ {what}")
}

/// The status line while a tool runs: the label its feature gives it
/// ([`Feature::tool_status`]), or the tool's name as words.
fn tool_status(
    owners: &HashMap<&'static str, Arc<dyn Feature>>,
    name: &str,
    args: &serde_json::Value,
) -> String {
    match owners
        .get(name)
        .and_then(|feature| feature.tool_status(name, args))
    {
        Some(label) => label.to_string(),
        None => plain_tool_status(name),
    }
}

/// "read_recent_messages" → "🔧 Read recent messages…"
fn plain_tool_status(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut chars = words.chars();
    let label: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    };
    format!("🔧 {label}…")
}

/// Runs the whole answer. Never fails: problems end up in [`Outcome::end`] and on screen.
pub async fn run(ctx: &BotCtx, provider: &dyn ChatProvider, job: Job) -> Outcome {
    let mut screen = Screen {
        reply: job.reply,
        text: String::new(),
        note: None,
        status: Some(activity_status(&Activity::Thinking)),
        shown: Vec::new(),
        message_ids: job.message_ids,
    };
    let result = stream_answer(
        ctx,
        provider,
        &job.asker,
        job.input,
        &job.cancel,
        &mut screen,
    )
    .await;

    let (end, written) = match result {
        Ok(Some((done, rounds, files))) => {
            screen.note = None;
            screen.status = None;
            if let Err(err) = attach_files(ctx, provider, &mut screen, &files).await {
                warn!("couldn't attach the answer's files: {err:#}");
                screen.status = Some("⚠️ Some files couldn't be attached.".into());
            }
            // A tool's message (like a voice message) can be the whole answer: then the
            // text message goes away.
            if screen.text.trim().is_empty()
                && files.is_empty()
                && job.asker.posted.ids().is_empty()
            {
                screen.text = EMPTY_ANSWER.to_string();
            }
            let written = Written {
                provider: provider.name().to_string(),
                model: provider.model().to_string(),
                continuation_id: done.continuation,
                native_json: serde_json::to_string(&rounds).ok(),
            };
            (End::Finished, Some(written))
        }
        Ok(None) => {
            screen.status = Some("⏹️ Stopped · ❌ again removes this, 🔁 retries".into());
            (End::Stopped, None)
        }
        Err(err) => {
            screen.status = Some(friendly_error(&err));
            (End::Failed(err), None)
        }
    };
    let end = match (screen.show(ctx).await, end) {
        (Err(err), End::Finished) => End::Failed(err.context("showing the answer")),
        (_, end) => end,
    };
    let mut message_ids = screen.reply.message_ids();
    message_ids.extend(job.asker.posted.ids());
    Outcome {
        text: with_posted(&screen.text, &job.asker.posted.texts()),
        written,
        message_ids,
        end,
    }
}

/// The answer's text for the history, with what the tools' messages said after it.
pub(super) fn with_posted(text: &str, posted: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !text.trim().is_empty() {
        parts.push(text.trim_end().to_string());
    }
    parts.extend(posted.iter().cloned());
    parts.join("\n\n")
}

type Finished = (Done, Vec<NativeRound>, Vec<GeneratedFile>);

/// Streams rounds of model output until the model stops asking for tools. Returns `None`
/// when ❌ stopped it.
async fn stream_answer(
    ctx: &BotCtx,
    provider: &dyn ChatProvider,
    asker: &Asker,
    mut input: Input,
    cancel: &CancellationToken,
    screen: &mut Screen,
) -> anyhow::Result<Option<Finished>> {
    let (tools, owners) = tools_for(ctx, asker);
    let mut rounds = Vec::new();
    let mut files = Vec::new();
    screen.show(ctx).await?;

    for _round in 0..MAX_ROUNDS {
        let request = ChatRequest {
            system: SYSTEM_PROMPT.to_string(),
            input: input.clone(),
            tools: tools.clone(),
            cache_key: format!("discord:{}", asker.channel),
            job: "chat",
        };
        let mut events = tokio::select! {
            events = provider.stream(request) => events?,
            () = cancel.cancelled() => return Ok(None),
        };

        // A new round of text starts a new paragraph.
        let mut first_delta = true;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let done = loop {
            tokio::select! {
                () = cancel.cancelled() => return Ok(None),
                _ = tick.tick() => screen.show(ctx).await?,
                event = events.recv() => match event {
                    None => return Err(anyhow!("the model's stream closed early")),
                    Some(Err(err)) => return Err(err),
                    Some(Ok(ChatEvent::TextDelta(delta))) => {
                        if first_delta && !screen.text.is_empty() && !screen.text.ends_with('\n') {
                            screen.text.push_str("\n\n");
                        }
                        first_delta = false;
                        screen.text.push_str(&delta);
                        // The text itself now says what's going on.
                        screen.note = None;
                        screen.status = Some(activity_status(&Activity::Writing));
                    }
                    Some(Ok(ChatEvent::Activity(activity))) => {
                        // Text after a search or code starts a new paragraph, like text
                        // after the bot's tools.
                        if activity != Activity::Writing {
                            first_delta = true;
                        }
                        screen.status = Some(activity_status(&activity));
                    }
                    Some(Ok(ChatEvent::Progress(note))) => screen.note = Some(note),
                    Some(Ok(ChatEvent::Done(done))) => break done,
                },
            }
        };
        files.extend(done.files.iter().cloned());
        if done.tool_calls.is_empty() {
            rounds.push(NativeRound {
                output: done.native.clone(),
                results: Vec::new(),
                setup: done.setup.clone(),
            });
            return Ok(Some((done, rounds, files)));
        }

        // Run the tools, then continue the answer with their results.
        let mut results = Vec::new();
        for call in &done.tool_calls {
            screen.status = Some(tool_status(&owners, &call.name, &call.args));
            screen.show(ctx).await?;
            let reply = tokio::select! {
                reply = run_tool(ctx, asker, &owners, call) => reply,
                () = cancel.cancelled() => return Ok(None),
            };
            results.push(ToolOutput {
                call_id: call.id.clone(),
                output: reply.text,
                images: reply.images,
            });
        }
        screen.status = Some(activity_status(&Activity::Thinking));
        input = next_input(provider, input, &done, &results);
        rounds.push(NativeRound {
            output: done.native,
            results,
            setup: done.setup,
        });
    }
    Err(anyhow!(
        "the model kept calling tools ({MAX_ROUNDS} rounds)"
    ))
}

/// The tools of every feature enabled where the asker is, and which feature owns each.
pub(super) fn tools_for(
    ctx: &BotCtx,
    asker: &Asker,
) -> (Vec<ToolDef>, HashMap<&'static str, Arc<dyn Feature>>) {
    let mut tools = Vec::new();
    let mut owners = HashMap::new();
    for feature in ctx.features.iter() {
        if !ctx.allows(feature.name(), asker.guild, asker.channel) {
            continue;
        }
        for tool in feature.tools() {
            owners.insert(tool.name, feature.clone());
            tools.push(tool);
        }
    }
    (tools, owners)
}

/// What the features enabled where the asker is add to the question (see
/// [`Feature::chat_context`]). A feature that fails is left out.
pub async fn context_for(ctx: &BotCtx, asker: &Asker, fresh: bool) -> Vec<String> {
    let mut texts = Vec::new();
    for feature in ctx.features.iter() {
        if !ctx.allows(feature.name(), asker.guild, asker.channel) {
            continue;
        }
        match feature.chat_context(ctx, asker, fresh).await {
            Ok(Some(text)) => texts.push(text),
            Ok(None) => {}
            Err(err) => warn!(
                feature = feature.name(),
                "couldn't add chat context: {err:#}"
            ),
        }
    }
    texts
}

/// Runs one tool call. Whatever happens, the model gets text back: the result or the error.
pub(super) async fn run_tool(
    ctx: &BotCtx,
    asker: &Asker,
    owners: &HashMap<&'static str, Arc<dyn Feature>>,
    call: &ToolCall,
) -> ToolReply {
    let Some(feature) = owners.get(call.name.as_str()) else {
        return format!("Error: there is no tool named {}.", call.name).into();
    };
    match feature
        .run_tool_with_pictures(ctx, asker, &call.name, &call.args)
        .await
    {
        Ok(reply) => {
            info!(tool = call.name, "ran a chat tool");
            reply
        }
        Err(err) => {
            if !is_user_error(&err) {
                warn!(tool = call.name, "chat tool failed: {err:#}");
            }
            format!("Error: {err:#}").into()
        }
    }
}

/// Downloads the files the model made, attaches them to the last message, and points the
/// answer's `sandbox:` links at them.
async fn attach_files(
    ctx: &BotCtx,
    provider: &dyn ChatProvider,
    screen: &mut Screen,
    files: &[GeneratedFile],
) -> anyhow::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    // Make sure the final text is on screen first: attaching edits the last message.
    screen.show(ctx).await?;
    let mut attachments = Vec::new();
    for file in files.iter().take(MAX_FILES) {
        let data = provider.download_file(file).await?;
        attachments.push(CreateAttachment::bytes(data, file_name(&file.filename)));
    }
    let message = screen.reply.attach(&ctx.http, attachments).await?;
    let links: Vec<(String, String)> = message
        .attachments
        .iter()
        .map(|a| (a.filename.clone(), a.url.clone()))
        .collect();
    screen.text = link_files(&screen.text, &links);
    if screen.text.trim().is_empty() {
        screen.text = "Files attached.".to_string();
    }
    Ok(())
}

/// The last part of a path, never empty: "/mnt/data/plot.png" → "plot.png".
fn file_name(path: &str) -> String {
    let name = path.replace('\\', "/");
    match name.rsplit('/').next() {
        Some(last) if !last.is_empty() => last.to_string(),
        _ => "file".to_string(),
    }
}

/// Replaces `sandbox:/mnt/data/plot.png` links with the Discord link of the attached file
/// of the same name. A name that matches no file (it wasn't attached) becomes the plain
/// file name, so nobody taps a link that goes nowhere; `[label](sandbox:…)` keeps its label.
fn link_files(text: &str, files: &[(String, String)]) -> String {
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("sandbox:") {
        result.push_str(&rest[..start]);
        let link = &rest[start..];
        let end = link
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"'))
            .unwrap_or(link.len());
        let (link, after) = link.split_at(end);
        let name = file_name(link.trim_start_matches("sandbox:"));
        let matches: Vec<&(String, String)> = files.iter().filter(|(f, _)| *f == name).collect();
        match matches.as_slice() {
            [(_, url)] => {
                result.push_str(url);
                rest = after;
            }
            _ if result.ends_with("](") && after.starts_with(')') => {
                // "[label](sandbox:x.png)" → "label (x.png)"
                result.truncate(result.len() - 2);
                if let Some(open) = result.rfind('[') {
                    result.remove(open);
                }
                result.push_str(&format!(" ({name})"));
                rest = &after[1..];
            }
            _ => {
                result.push_str(&name);
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

/// Turns Slack-style links, `<https://example.com|the text>`, into Discord's masked links,
/// `[the text](<https://example.com>)`. Models sometimes write them (or copy them from wiki
/// markup), and Discord would show the `|text` as part of a broken link.
pub(super) fn discord_links(text: &str) -> String {
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        result.push_str(&rest[..start]);
        let link = &rest[start + 1..];
        let slack = link
            .find(['>', '<', '\n'])
            .filter(|&end| link.as_bytes()[end] == b'>')
            .and_then(|end| {
                let (url, label) = link[..end].split_once('|')?;
                let label = label.trim();
                let is_web = url.starts_with("https://") || url.starts_with("http://");
                (is_web && !url.contains(' ') && !label.is_empty()).then_some((url, label, end))
            });
        match slack {
            Some((url, label, end)) => {
                result.push_str(&format!("[{label}](<{url}>)"));
                rest = &link[end + 1..];
            }
            None => {
                result.push('<');
                rest = link;
            }
        }
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_tools_posted() {
        let voice = vec!["[Voice message] hi".to_string()];
        assert_eq!(with_posted("", &voice), "[Voice message] hi");
        assert_eq!(with_posted("Hey!\n", &voice), "Hey!\n\n[Voice message] hi");
        assert_eq!(with_posted("Hey!", &[]), "Hey!");
    }

    #[test]
    fn status_goes_under_the_text() {
        assert_eq!(render("", None, Some("💭 Thinking…")), ["-# 💭 Thinking…"]);
        assert_eq!(
            render("Hi", None, Some("✍️ Writing…")),
            ["Hi\n-# ✍️ Writing…"]
        );
        assert_eq!(render("Hi", None, None), ["Hi"]);

        let long = "word ".repeat(1000);
        let parts = render(&long, Some(&long), Some("✍️ Writing…"));
        assert!(parts.len() > 2);
        assert!(parts.iter().all(|p| p.chars().count() <= DISCORD_LIMIT));
        // Only the last part has the note and the status.
        assert_eq!(parts.iter().filter(|p| p.contains("-# ")).count(), 1);
        assert!(parts.last().unwrap().ends_with("-# ✍️ Writing…"));
        // The parts before the last one use the whole message.
        assert!(parts[0].chars().count() > DISCORD_LIMIT - STATUS_ROOM);
    }

    #[test]
    fn status_stays_out_of_an_open_code_block() {
        assert_eq!(
            render("```py\nx = 1", None, Some("✍️ Writing…")),
            ["```py\nx = 1\n```\n-# ✍️ Writing…"]
        );
        // A finished block is left alone, and so is the text when the status is gone.
        assert_eq!(
            render("```py\nx = 1\n```", None, Some("✍️ Writing…")),
            ["```py\nx = 1\n```\n-# ✍️ Writing…"]
        );
        assert_eq!(render("```py\nx = 1", None, None), ["```py\nx = 1"]);
    }

    #[test]
    fn placeholder_matches_the_first_render() {
        assert_eq!(placeholder(), ["-# 💭 Thinking…"]);
    }

    #[test]
    fn progress_note_goes_above_the_status() {
        assert_eq!(
            render(
                "",
                Some("Found it.\n\nNow checking"),
                Some("🔧 Read recent messages…")
            ),
            ["-# 💬 Found it. Now checking\n-# 🔧 Read recent messages…"]
        );
    }

    #[test]
    fn web_status_names_what_she_looks_at() {
        assert_eq!(
            activity_status(&Activity::Searching("dino decks 2017".into())),
            "🔎 Searching “dino decks 2017”…"
        );
        assert_eq!(
            activity_status(&Activity::Reading("ygoprodeck.com".into())),
            "📖 Reading ygoprodeck.com…"
        );
    }

    #[test]
    fn tool_labels() {
        assert_eq!(
            plain_tool_status("read_recent_messages"),
            "🔧 Read recent messages…"
        );
    }

    #[test]
    fn slack_links_become_masked_links() {
        assert_eq!(
            discord_links(
                "From the <https://wowpedia.fandom.com/wiki/Summon_Dreadsteed|Dreadsteed of Xoroth> quest."
            ),
            "From the [Dreadsteed of Xoroth](<https://wowpedia.fandom.com/wiki/Summon_Dreadsteed>) quest."
        );
        // Plain and unfinished links, mentions and comparisons stay as they are.
        for text in [
            "<https://example.com> and <@123>",
            "a <b and c> d",
            "<https://example.com|Exam",
            "<https://example.com|>",
            "x < y | z > w",
        ] {
            assert_eq!(discord_links(text), text);
        }
        assert_eq!(
            render("<https://a.com|A>", None, None),
            ["[A](<https://a.com>)"]
        );
    }

    #[test]
    fn sandbox_links_point_at_attachments() {
        let files = vec![
            ("plot.png".to_string(), "https://cdn/plot.png".to_string()),
            ("a.csv".to_string(), "https://cdn/1/a.csv".to_string()),
            ("a.csv".to_string(), "https://cdn/2/a.csv".to_string()),
        ];
        assert_eq!(
            link_files(
                "See [plot](sandbox:/mnt/data/plot.png) and (sandbox:/mnt/data/a.csv).",
                &files
            ),
            "See [plot](https://cdn/plot.png) and (a.csv)."
        );
        // A file that wasn't attached: no dead link, the label and name stay.
        assert_eq!(
            link_files("Here: [the chart](sandbox:/mnt/data/missing.png).", &files),
            "Here: the chart (missing.png)."
        );
        assert_eq!(link_files("no links", &files), "no links");
        assert_eq!(file_name("sandbox:/mnt/data/"), "file");
    }
}
