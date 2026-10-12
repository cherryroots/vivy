//! `look_back_at_answers`: Vivy reads her own earlier answers, to find out why she said
//! something or what went wrong. A list of recent answers, or one answer step by step:
//! what she thought, wrote, which tools she called with what, and what came back.
//!
//! Everything comes from `chat_turns`. The steps are read from `native_json`, the
//! provider's own output, which looks different for Claude and OpenAI; [`steps`] reads
//! both. Only answers in channels the asker can read are shown (in a DM, only that DM),
//! so a conversation never leaks into another place.

use std::collections::HashMap;

use chrono::DateTime;
use chrono_tz::Tz;
use serde_json::{Value, json};
use serenity::all::{ChannelId, MessageId};

use super::channels;
use super::store::{self, StoredPart, Turn};
use super::tools::{can_read, home_zone, message_link, parse_link};
use crate::ai::{NativeRound, ToolDef};
use crate::core::{Asker, BotCtx, Result, user_error};
use crate::util::shorten;

pub const TOOL: &str = "look_back_at_answers";

/// Most answers one list shows.
const MAX_LIST: usize = 20;
/// How many recent answers a list across the server looks through.
const SCAN: usize = 200;
/// Cuts for the detail view, so one answer can't fill the model's context.
const MAX_QUESTION: usize = 1200;
const MAX_CONTEXT: usize = 300;
const MAX_STEP: usize = 500;
const MAX_SENT: usize = 1500;
const MAX_DETAIL: usize = 9000;
/// Below this, a number is an answer number; above, a Discord message ID.
const SMALLEST_DISCORD_ID: u64 = 1_000_000_000_000_000;

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL,
        description: "Your own earlier answers, as saved: what you were asked, which tools you called with what, what they returned (errors too), what you sent, the model and how long it took. Use it when someone asks why you said or did something, when something went wrong, or to check your own past work. Without `answer`, lists recent answers in this channel with their numbers; with `answer`, shows that one step by step.",
        parameters: json!({
            "type": "object",
            "properties": {
                "answer": {"type": "string", "description": "The answer to show in detail: its number from the list, a link to or ID of one of your messages (or the message you answered), or \"replied\" for the answer the current message replies to."},
                "channel": {"type": "string", "description": "For the list: a channel mention or ID, or \"all\" for every channel of this server the asker can read. Default: this channel."},
                "before": {"type": "integer", "description": "For the list: only answers older than this answer number, to page back."},
                "count": {"type": "integer", "description": "For the list: how many, 1 to 20. Default 10."}
            },
        }),
    }
}

pub async fn run(ctx: &BotCtx, asker: &Asker, args: &Value) -> Result<String> {
    let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    let zone = home_zone(ctx, asker.user).await;
    if let Some(which) = text("answer") {
        let answer = find_answer(ctx, asker, which).await?;
        return detail(ctx, asker, &answer, zone).await;
    }
    let count = args["count"]
        .as_u64()
        .unwrap_or(10)
        .clamp(1, MAX_LIST as u64) as usize;
    let before = args["before"].as_i64();
    list(ctx, asker, text("channel"), before, count, zone).await
}

/// The answer `which` points at, if the asker may see it.
async fn find_answer(ctx: &BotCtx, asker: &Asker, which: &str) -> Result<Turn> {
    let which = which.trim_start_matches('#');
    let found = if which.eq_ignore_ascii_case("replied") {
        let message = asker.message.get();
        ctx.db
            .call(move |conn| {
                let Some(question) = store::turn_for_message(conn, message)? else {
                    return Ok(None);
                };
                match question.parent_id {
                    Some(parent) => store::get_turn(conn, parent),
                    None => Ok(None),
                }
            })
            .await?
            .filter(is_answer)
            .ok_or_else(|| user_error("This message doesn't reply to one of your answers."))?
    } else {
        let message = match (parse_link(which), which.parse::<u64>()) {
            (Some((_, _, message)), _) => Some(message),
            (None, Ok(id)) if id >= SMALLEST_DISCORD_ID => Some(id),
            (None, Ok(_)) => None,
            (None, Err(_)) => {
                return Err(user_error(
                    "`answer` is an answer number, a message link or ID, or \"replied\".",
                ));
            }
        };
        let number: i64 = which.parse().unwrap_or_default();
        ctx.db
            .call(move |conn| {
                let Some(message) = message else {
                    return store::get_turn(conn, number);
                };
                // A message of hers is the answer; a question leads to its newest answer.
                match store::turn_for_message(conn, message)? {
                    Some(turn) if is_answer(&turn) => Ok(Some(turn)),
                    Some(turn) => store::answer_to(conn, turn.id),
                    None => Ok(None),
                }
            })
            .await?
            .filter(is_answer)
            .ok_or_else(|| user_error("I have no saved answer there."))?
    };
    if !may_see(ctx, asker, found.channel_id).await? {
        return Err(user_error(
            "That answer is in a place the asker can't read.",
        ));
    }
    Ok(found)
}

fn is_answer(turn: &Turn) -> bool {
    turn.role == crate::ai::Role::Assistant
}

/// Whether the asker can read `channel`: the same rule as reading messages.
async fn may_see(ctx: &BotCtx, asker: &Asker, channel: u64) -> Result<bool> {
    if channel == 0 {
        return Ok(false);
    }
    can_read(
        ctx,
        asker,
        asker.guild.map(|g| g.get()),
        ChannelId::new(channel),
    )
    .await
}

async fn list(
    ctx: &BotCtx,
    asker: &Asker,
    channel: Option<&str>,
    before: Option<i64>,
    count: usize,
    zone: Tz,
) -> Result<String> {
    let everywhere = channel.is_some_and(|c| c.eq_ignore_ascii_case("all"));
    let channel = match channel {
        None => asker.channel.get(),
        Some(_) if everywhere => 0,
        Some(text) => {
            let id = parse_channel(text)
                .ok_or_else(|| user_error("`channel` is a channel mention or ID, or \"all\"."))?;
            if !may_see(ctx, asker, id).await? {
                return Err(user_error("The asker can't read that channel."));
            }
            id
        }
    };

    let answers = if everywhere {
        // Look through the newest answers anywhere, and keep those the asker may see.
        let found = ctx
            .db
            .call(move |conn| store::recent_answers(conn, None, before, SCAN))
            .await?;
        let mut allowed: HashMap<u64, bool> = HashMap::new();
        let mut kept = Vec::new();
        for answer in found {
            let ok = match allowed.get(&answer.channel_id) {
                Some(ok) => *ok,
                None => {
                    let ok = may_see(ctx, asker, answer.channel_id).await?;
                    allowed.insert(answer.channel_id, ok);
                    ok
                }
            };
            if ok {
                kept.push(answer);
            }
            if kept.len() == count {
                break;
            }
        }
        kept
    } else {
        ctx.db
            .call(move |conn| store::recent_answers(conn, Some(channel), before, count))
            .await?
    };
    if answers.is_empty() {
        return Ok("No saved answers there.".to_string());
    }

    // Each answer's question, for the first words of it.
    let parents: Vec<i64> = answers.iter().filter_map(|a| a.parent_id).collect();
    let questions: HashMap<i64, Turn> = ctx
        .db
        .call(move |conn| {
            let mut found = HashMap::new();
            for id in parents {
                if let Some(turn) = store::get_turn(conn, id)? {
                    found.insert(id, turn);
                }
            }
            Ok(found)
        })
        .await?;

    let mut lines = vec![
        "Your answers, newest first. Pass a number as `answer` to see one in detail.".to_string(),
    ];
    for answer in &answers {
        let question = answer.parent_id.and_then(|id| questions.get(&id));
        let place = if everywhere {
            Some(channels::describe(ctx, ChannelId::new(answer.channel_id)).await)
        } else {
            None
        };
        lines.push(list_line(answer, question, ctx.bot_id.get(), place, zone));
    }
    Ok(lines.join("\n"))
}

/// "<#123>", "123" or a channel link → the ID.
fn parse_channel(text: &str) -> Option<u64> {
    if let Some(url) = text.strip_prefix("https://")
        && let Some(id) = url.trim_end_matches('/').rsplit('/').next()
    {
        return id.parse().ok().filter(|id| *id > 0);
    }
    text.trim_start_matches("<#")
        .trim_end_matches('>')
        .parse()
        .ok()
        .filter(|id| *id > 0)
}

/// One answer in a list:
/// "#42 · 2026-10-12 14:02 · 12s · claude · tools: get_current_time · Cherry: what time… → It's two."
fn list_line(
    answer: &Turn,
    question: Option<&Turn>,
    bot_id: u64,
    place: Option<String>,
    zone: Tz,
) -> String {
    let mut bits = vec![
        format!("#{}", answer.id),
        local_time(answer.created_at, zone),
    ];
    bits.extend(place);
    if let Some(took) = took(answer) {
        bits.push(took);
    }
    if let Some(provider) = &answer.provider {
        bits.push(provider.clone());
    }
    let calls = answer
        .native_json
        .as_deref()
        .map(steps)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|step| match step {
            Step::Call { name, .. } => Some(name),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !calls.is_empty() {
        bits.push(format!("tools: {}", tool_counts(&calls)));
    }
    if let Some(error) = &answer.error {
        bits.push(format!("ENDED: {}", shorten(error, 120)));
    }
    let asked = match question {
        Some(q) if q.author_id == bot_id => "(chime-in)".to_string(),
        Some(q) => shorten(&one_line(&plain_question(&turn_text(q))), 80),
        None => "(no saved question)".to_string(),
    };
    let said = shorten(&one_line(&turn_text(answer)), 100);
    format!("{} · {asked} → {said}", bits.join(" · "))
}

/// "get_current_time, web_search ×2", in the order they were first called.
fn tool_counts(calls: &[String]) -> String {
    let mut counted: Vec<(&str, usize)> = Vec::new();
    for call in calls {
        match counted.iter_mut().find(|(name, _)| name == call) {
            Some((_, n)) => *n += 1,
            None => counted.push((call, 1)),
        }
    }
    counted
        .iter()
        .map(|(name, n)| {
            if *n == 1 {
                name.to_string()
            } else {
                format!("{name} ×{n}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

async fn detail(ctx: &BotCtx, asker: &Asker, answer: &Turn, zone: Tz) -> Result<String> {
    let (question, messages) = {
        let parent = answer.parent_id;
        let id = answer.id;
        ctx.db
            .call(move |conn| {
                let question = match parent {
                    Some(parent) => store::get_turn(conn, parent)?,
                    None => None,
                };
                Ok((question, store::messages_of(conn, id)?))
            })
            .await?
    };
    let place = channels::describe(ctx, ChannelId::new(answer.channel_id)).await;
    let link = messages.first().map(|&first| {
        message_link(
            asker.guild,
            ChannelId::new(answer.channel_id),
            MessageId::new(first),
        )
    });
    let mut out = render_detail(answer, question.as_ref(), ctx.bot_id.get(), &place, zone);
    if let Some(link) = link {
        out = out.replacen('\n', &format!("\nLink: {link}\n"), 1);
    }
    Ok(shorten(&out, MAX_DETAIL))
}

/// The detail view, without the link (which needs Discord).
fn render_detail(
    answer: &Turn,
    question: Option<&Turn>,
    bot_id: u64,
    place: &str,
    zone: Tz,
) -> String {
    let mut head = vec![
        format!("Answer #{}", answer.id),
        place.to_string(),
        local_time(answer.created_at, zone),
    ];
    if let Some(took) = took(answer) {
        head.push(format!("took {took}"));
    }
    match (&answer.provider, &answer.model) {
        (Some(provider), Some(model)) => head.push(format!("{provider} ({model})")),
        (Some(provider), None) => head.push(provider.clone()),
        _ => {}
    }
    let mut lines = vec![head.join(" · ")];

    match question {
        Some(q) if q.author_id == bot_id => {
            lines.push("A chime-in: nobody asked; you read along and spoke up.".to_string());
            lines.push(format!(
                "What you read: {}",
                shorten(&turn_text(q), MAX_QUESTION)
            ));
        }
        Some(q) => {
            lines.push(format!(
                "Question: {}",
                shorten(&plain_question(&turn_text(q)), MAX_QUESTION)
            ));
            let media = q
                .parts
                .iter()
                .filter(|p| matches!(p, StoredPart::Media { .. } | StoredPart::File { .. }))
                .count();
            if media > 0 {
                lines.push(format!("It came with {media} pictures, videos or files."));
            }
        }
        None => lines.push("No saved question (a message saved without one).".to_string()),
    }
    if let Some(q) = question {
        for part in &q.parts {
            if let StoredPart::Context { text } = part {
                lines.push(format!(
                    "Context added: {}",
                    shorten(&one_line(text), MAX_CONTEXT)
                ));
            }
        }
        if let Some(earlier) = q.parent_id {
            lines.push(format!(
                "It continues an earlier conversation: turn #{earlier} came before it."
            ));
        }
    }

    match answer.native_json.as_deref() {
        Some(json) => {
            let steps = steps(json);
            if steps.is_empty() {
                lines.push("No steps saved.".to_string());
            }
            lines.extend(steps.iter().map(render_step));
        }
        None if answer.provider.is_some() => lines.push(format!(
            "The steps are no longer kept (they go after {} days).",
            store::NATIVE_KEEP_DAYS
        )),
        None => lines.push("No steps saved.".to_string()),
    }

    lines.push(format!("Sent: {}", shorten(&turn_text(answer), MAX_SENT)));
    if let Some(error) = &answer.error {
        lines.push(format!("Ended: {error}"));
    }
    lines.join("\n")
}

/// One thing that happened in an answer.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// A new round of the model's output, after tools ran.
    Round(usize),
    Thought(String),
    Wrote(String),
    Call {
        id: String,
        name: String,
        input: String,
        result: Option<String>,
    },
}

fn render_step(step: &Step) -> String {
    match step {
        Step::Round(n) => format!("Round {n}:"),
        Step::Thought(text) => format!("- thought: {}", shorten(&one_line(text), MAX_STEP)),
        Step::Wrote(text) => format!("- wrote: {}", shorten(&one_line(text), MAX_STEP)),
        Step::Call {
            name,
            input,
            result,
            ..
        } => {
            let result = match result {
                Some(result) => shorten(&one_line(result), MAX_STEP),
                None => "(no result saved)".to_string(),
            };
            format!("- called {name} {} → {result}", shorten(input, 300))
        }
    }
}

/// The steps of an answer from its saved rounds. Claude's blocks and OpenAI's items are
/// told apart by their `type`, so this doesn't need to know who wrote it.
fn steps(native_json: &str) -> Vec<Step> {
    let Some(rounds) = NativeRound::parse_list(native_json) else {
        return Vec::new();
    };
    let mut steps = Vec::new();
    for (n, round) in rounds.iter().enumerate() {
        if rounds.len() > 1 {
            steps.push(Step::Round(n + 1));
        }
        let start = steps.len();
        for item in round.output.as_array().into_iter().flatten() {
            read_item(item, &mut steps);
        }
        // The bot's tools answered after the round.
        for result in &round.results {
            if let Some(Step::Call { result: slot, .. }) = steps[start..]
                .iter_mut()
                .find(|s| matches!(s, Step::Call { id, .. } if *id == result.call_id))
            {
                *slot = Some(result.output.clone());
            }
        }
    }
    steps
}

/// Adds what one block (Claude) or item (OpenAI) shows to `steps`.
fn read_item(item: &Value, steps: &mut Vec<Step>) {
    let kind = item["type"].as_str().unwrap_or_default();
    let compact = |value: &Value| match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    match kind {
        // Claude
        "thinking" => {
            let text = item["thinking"].as_str().unwrap_or_default();
            if !text.trim().is_empty() {
                steps.push(Step::Thought(text.to_string()));
            }
        }
        "text" => {
            let text = item["text"].as_str().unwrap_or_default();
            if !text.trim().is_empty() {
                steps.push(Step::Wrote(text.to_string()));
            }
        }
        "tool_use" | "server_tool_use" => steps.push(Step::Call {
            id: compact(&item["id"]),
            name: compact(&item["name"]),
            input: compact(&item["input"]),
            result: None,
        }),
        _ if kind.ends_with("_tool_result") => {
            let id = compact(&item["tool_use_id"]);
            let summary = claude_result(&item["content"]);
            if let Some(Step::Call { result, .. }) = steps
                .iter_mut()
                .rev()
                .find(|s| matches!(s, Step::Call { id: call, .. } if *call == id))
            {
                *result = Some(summary);
            }
        }
        // OpenAI
        "reasoning" => {
            let text: Vec<&str> = item["summary"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["text"].as_str())
                .collect();
            if !text.is_empty() {
                steps.push(Step::Thought(text.join(" ")));
            }
        }
        "message" => {
            let text: Vec<&str> = item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["text"].as_str())
                .collect();
            let text = text.join("");
            if !text.trim().is_empty() {
                steps.push(Step::Wrote(text));
            }
        }
        "function_call" => steps.push(Step::Call {
            id: compact(&item["call_id"]),
            name: compact(&item["name"]),
            input: compact(&item["arguments"]),
            result: None,
        }),
        "web_search_call" => steps.push(Step::Call {
            id: compact(&item["id"]),
            name: "web_search".to_string(),
            input: compact(&item["action"]),
            result: Some(compact(&item["status"])),
        }),
        "code_interpreter_call" => {
            let outputs: Vec<String> = item["outputs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| match o["logs"].as_str() {
                    Some(logs) => logs.to_string(),
                    None => format!("[{}]", compact(&o["type"])),
                })
                .collect();
            let result = if outputs.is_empty() {
                compact(&item["status"])
            } else {
                outputs.join("\n")
            };
            steps.push(Step::Call {
                id: compact(&item["id"]),
                name: "code_interpreter".to_string(),
                input: compact(&item["code"]),
                result: Some(result),
            });
        }
        _ => {}
    }
}

/// A short summary of a Claude built-in tool's result.
fn claude_result(content: &Value) -> String {
    let kind = content["type"].as_str().unwrap_or_default();
    if kind.ends_with("_error") {
        return format!("Error: {}", content["error_code"].as_str().unwrap_or(kind));
    }
    match content {
        // Web search: a list of results.
        Value::Array(results) => {
            let titles: Vec<&str> = results.iter().filter_map(|r| r["title"].as_str()).collect();
            format!("{} results: {}", results.len(), titles.join("; "))
        }
        Value::Null => "(result not kept)".to_string(),
        _ if kind == "web_fetch_result" => {
            format!("read {}", content["url"].as_str().unwrap_or("a page"))
        }
        _ if content.get("return_code").is_some() => {
            let mut text = format!("exit {}", content["return_code"]);
            for stream in ["stdout", "stderr"] {
                if let Some(out) = content[stream].as_str().filter(|s| !s.trim().is_empty()) {
                    text.push_str(&format!(", {stream}: {out}"));
                }
            }
            text
        }
        other => other.to_string(),
    }
}

/// The text parts of a turn (not the features' context).
fn turn_text(turn: &Turn) -> String {
    turn.parts
        .iter()
        .filter_map(|p| match p {
            StoredPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `<user name="Cherry" id="1">hi</user>` → `Cherry: hi`. Other text stays as it is.
fn plain_question(text: &str) -> String {
    let Some(rest) = text.strip_prefix("<user name=\"") else {
        return text.to_string();
    };
    let Some((name, rest)) = rest.split_once('"') else {
        return text.to_string();
    };
    let Some((_, inside)) = rest.split_once('>') else {
        return text.to_string();
    };
    let (inside, after) = inside.rsplit_once("</user>").unwrap_or((inside, ""));
    format!("{name}: {}{after}", inside.trim())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How long the answer took: from when work began to when it was saved.
fn took(answer: &Turn) -> Option<String> {
    let seconds = answer.created_at - answer.started_at?;
    Some(if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    })
}

fn local_time(timestamp: i64, zone: Tz) -> String {
    DateTime::from_timestamp(timestamp, 0)
        .unwrap_or_default()
        .with_timezone(&zone)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Role;

    fn turn(id: i64, role: Role, author_id: u64, text: &str) -> Turn {
        Turn {
            id,
            parent_id: None,
            role,
            author_id,
            parts: vec![StoredPart::Text { text: text.into() }],
            provider: None,
            model: None,
            continuation_id: None,
            native_json: None,
            channel_id: 5,
            started_at: None,
            error: None,
            // 2026-10-09 00:53 in Oslo.
            created_at: 1_791_500_000,
        }
    }

    #[test]
    fn claude_rounds() {
        let rounds = json!([
            {
                "output": [
                    {"type": "thinking", "thinking": "", "signature": "s"},
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "t1", "name": "get_current_time", "input": {"timezone": "UTC"}},
                    {"type": "server_tool_use", "id": "s1", "name": "web_search", "input": {"query": "dinos"}},
                    {"type": "web_search_tool_result", "tool_use_id": "s1", "content": [{"title": "Dinos"}, {"title": "More"}]},
                    {"type": "server_tool_use", "id": "s2", "name": "web_fetch", "input": {"url": "https://x.com"}},
                    {"type": "web_fetch_tool_result", "tool_use_id": "s2", "content": {"type": "web_fetch_tool_error", "error_code": "url_not_accessible"}}
                ],
                "results": [{"call_id": "t1", "output": "Error: Mars/Base isn't a timezone I know."}]
            },
            {"output": [{"type": "text", "text": "It's noon."}]}
        ]);
        let steps = steps(&rounds.to_string());
        let call = |id: &str, name: &str, input: &str, result: &str| Step::Call {
            id: id.into(),
            name: name.into(),
            input: input.into(),
            result: Some(result.into()),
        };
        assert_eq!(
            steps,
            [
                Step::Round(1),
                Step::Wrote("Let me check.".into()),
                call(
                    "t1",
                    "get_current_time",
                    r#"{"timezone":"UTC"}"#,
                    "Error: Mars/Base isn't a timezone I know."
                ),
                call(
                    "s1",
                    "web_search",
                    r#"{"query":"dinos"}"#,
                    "2 results: Dinos; More"
                ),
                call(
                    "s2",
                    "web_fetch",
                    r#"{"url":"https://x.com"}"#,
                    "Error: url_not_accessible"
                ),
                Step::Round(2),
                Step::Wrote("It's noon.".into()),
            ]
        );
    }

    #[test]
    fn openai_rounds() {
        let rounds = json!([{
            "output": [
                {"type": "reasoning", "id": "rs_1", "summary": [{"text": "Need the time."}]},
                {"type": "web_search_call", "id": "ws_1", "status": "completed", "action": {"type": "search", "query": "dinos"}},
                {"type": "code_interpreter_call", "id": "ci_1", "code": "print(1)", "outputs": [{"type": "logs", "logs": "1"}]},
                {"type": "function_call", "call_id": "call_1", "name": "get_current_time", "arguments": "{}"},
                {"type": "message", "content": [{"type": "output_text", "text": "Here"}]}
            ],
            "results": [{"call_id": "call_1", "output": "Friday"}]
        }]);
        let names: Vec<String> = steps(&rounds.to_string()).iter().map(render_step).collect();
        assert_eq!(
            names,
            [
                "- thought: Need the time.",
                r#"- called web_search {"query":"dinos","type":"search"} → completed"#,
                "- called code_interpreter print(1) → 1",
                "- called get_current_time {} → Friday",
                "- wrote: Here",
            ]
        );
    }

    #[test]
    fn old_or_missing_rounds() {
        assert!(steps("not json").is_empty());
        // Answers saved before rounds existed: a plain list of outputs.
        assert!(steps(r#"[[{"type": "text", "text": "hi"}]]"#).is_empty());
    }

    #[test]
    fn questions_in_plain_words() {
        assert_eq!(
            plain_question("<user name=\"Cherry\" id=\"1\">what time is it</user>"),
            "Cherry: what time is it"
        );
        assert_eq!(plain_question("no tags"), "no tags");
    }

    #[test]
    fn channels_from_text() {
        assert_eq!(parse_channel("<#12>"), Some(12));
        assert_eq!(parse_channel("12"), Some(12));
        assert_eq!(parse_channel("https://discord.com/channels/1/12"), Some(12));
        assert_eq!(parse_channel("general"), None);
        assert_eq!(parse_channel("0"), None);
    }

    #[test]
    fn tools_counted() {
        let calls: Vec<String> = ["a", "b", "a", "a"].iter().map(|s| s.to_string()).collect();
        assert_eq!(tool_counts(&calls), "a ×3, b");
    }

    #[test]
    fn a_line_per_answer() {
        let question = turn(
            1,
            Role::User,
            7,
            "<user name=\"Cherry\" id=\"7\">time?</user>",
        );
        let mut answer = turn(2, Role::Assistant, 9, "It's\nnoon.");
        answer.provider = Some("claude".into());
        answer.started_at = Some(answer.created_at - 75);
        answer.native_json = Some(
            json!([{"output": [{"type": "tool_use", "id": "t", "name": "get_current_time", "input": {}}]}])
                .to_string(),
        );
        answer.error = Some("stopped with ❌".into());
        assert_eq!(
            list_line(&answer, Some(&question), 9, None, Tz::Europe__Oslo),
            "#2 · 2026-10-09 00:53 · 1m15s · claude · tools: get_current_time · ENDED: stopped with ❌ · Cherry: time? → It's noon."
        );
        // Her own question means she chimed in.
        let read = turn(1, Role::User, 9, "chat log");
        assert!(
            list_line(&answer, Some(&read), 9, Some("#general".into()), Tz::UTC)
                .contains("#general · 1m15s")
        );
    }

    #[test]
    fn one_answer_in_detail() {
        let mut question = turn(
            1,
            Role::User,
            7,
            "<user name=\"Cherry\" id=\"7\">time?</user>",
        );
        question.parent_id = Some(0);
        question.parts.push(StoredPart::Context {
            text: "<vivy_mood>sleepy</vivy_mood>".into(),
        });
        let mut answer = turn(2, Role::Assistant, 9, "It's noon.");
        answer.provider = Some("claude".into());
        answer.model = Some("claude-opus-5-5".into());
        answer.started_at = Some(answer.created_at - 4);
        answer.native_json =
            Some(json!([{"output": [{"type": "text", "text": "It's noon."}]}]).to_string());
        assert_eq!(
            render_detail(&answer, Some(&question), 9, "#general", Tz::UTC),
            "Answer #2 · #general · 2026-10-08 22:53 · took 4s · claude (claude-opus-5-5)
Question: Cherry: time?
Context added: <vivy_mood>sleepy</vivy_mood>
It continues an earlier conversation: turn #0 came before it.
- wrote: It's noon.
Sent: It's noon."
        );

        // Old enough that the rounds were cleared.
        answer.native_json = None;
        assert!(
            render_detail(&answer, None, 9, "#general", Tz::UTC)
                .contains("The steps are no longer kept (they go after 60 days).")
        );
    }
}
