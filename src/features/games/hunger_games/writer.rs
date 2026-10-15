//! Vivy writing a round: one model call, answered in JSON, checked by the rules in
//! `game.rs`. A round that doesn't pass goes back to her with what's wrong, at most
//! [`TRIES`] times in all.
//!
//! She plays the Gamemaker in her own voice, but her mood isn't in the prompt: it never
//! changes what happens in the arena.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, warn};

use super::game::{Game, Kind, Round, Written, fill};
use crate::ai::{
    ChatProvider, ChatRequest, Input, Part, Role, ToolCall, ToolRunner, Turn, complete,
};

/// How many times she may write a round before the game gives up.
const TRIES: usize = 3;

const SYSTEM: &str = "You are Vivy, a Discord bot, and right now you're the Gamemaker of a Hunger Games-style battle royale played in your Discord server. The tributes are real members of the server, and everyone is watching the arena in the channel, so it's a show: dramatic, funny, a little dark, and full of personality. You decide everything that happens in the arena. \
Each round you get the tributes, the arena, the story so far and what kind of round it is, and you write what happens to every living tribute this round. \
Write events like a battle royale simulator: one or two short sentences each, in the past tense, about one to six tributes. Mix it up: kills, alliances, betrayals, traps, accidents, silly moments, near misses, someone just trying to survive. Let the story build: grudges, alliances and kill streaks from earlier rounds can come back. The places in the arena are the server's channels; use them when it's fun. \
Keep it playful: cartoon violence, no gore, nothing sexual, nothing hateful, and no jokes about anyone's real body, health, identity or private life. Fair play too: don't pick favourites or settle scores, and anyone can win. \
Write each tribute as their tag in braces, like {T3}, never their name; the bot fills in the names. Write in English.";

const FORMAT: &str = "Answer with only one JSON object, no code fence and nothing before or after it:
{\"gamemaker\": string or null, \"events\": [{\"text\": string, \"tributes\": [tags], \"deaths\": [{\"tribute\": tag, \"by\": tag or null}]}], \"victory\": string or null}
- Every living tribute is in exactly one event's `tributes`, and every tag in an event's text is in its `tributes` (and the other way round).
- `deaths` lists who dies in that event, with `by` the tribute who killed them, or null for an accident, the arena or themselves. A killer must be another tribute in the same event.
- `gamemaker`: your announcement as the Gamemaker before this round, one to three sentences, spoken out loud to everyone watching (no tags, use names there). Only when the round asks for one; otherwise null.
- `victory`: only in the final showdown: what you say to the winner, one to three sentences, by name. Otherwise null.
- `gamemaker` and `victory` are also spoken out loud, as voice messages in your Gamemaker voice. Steer how they're said with audio tags in square brackets before the words they're for, in plain English: [booming], [dramatic pause], [chuckles], [whispers], [menacing]. Tags are spoken, never shown. Keep each line under 300 characters.";

/// The arena, the tributes, her notes about them, the story so far, and what this round
/// is: what she's told. `notes` are in the same order as the tributes.
pub fn brief(game: &Game, kind: Kind, arena: &[String], notes: &[Option<String>]) -> String {
    let alive = game.alive();
    let (min, max) = game.deaths_range(kind);
    let mut text = String::new();
    if !arena.is_empty() {
        text.push_str(
            "The arena's places for this game (some of the server's channels, picked at random; spread the tributes around, and don't keep sending people to the same place): ",
        );
        let names: Vec<String> = arena.iter().map(|n| format!("#{n}")).collect();
        text.push_str(&names.join(", "));
        text.push_str("\n\n");
    }
    text.push_str("The tributes:\n");
    for (i, t) in game.tributes.iter().enumerate() {
        let state = match t.died_in {
            Some(_) => "dead".to_string(),
            None if t.kills == 1 => "alive, 1 kill".to_string(),
            None => format!("alive, {} kills", t.kills),
        };
        text.push_str(&format!("- {}: {} ({state})\n", Game::tag(i), t.name));
    }
    let known: Vec<String> = game
        .tributes
        .iter()
        .zip(notes)
        .enumerate()
        .filter(|(_, (t, _))| t.alive())
        .filter_map(|(i, (t, note))| {
            let note = note.as_deref()?;
            Some(format!(
                "<notes tribute=\"{}\" name=\"{}\">\n{note}\n</notes>",
                Game::tag(i),
                t.name
            ))
        })
        .collect();
    if !known.is_empty() {
        text.push_str(
            "\nWhat your memory notes say about some of the living tributes. Bring a detail in only where it makes a moment funnier or more personal (a hobby, a favourite game, a running joke), a few per round at most. Never use anything private, sensitive or that could hurt or embarrass them (health, relationships, family, where they live, work or money troubles, anything told in confidence). When nothing fits, leave the notes out:\n",
        );
        text.push_str(&known.join("\n"));
        text.push('\n');
    }
    if !game.rounds.is_empty() {
        text.push_str("\nThe story so far:\n");
        for round in &game.rounds {
            text.push_str(&format!("{}:\n", title(round.kind)));
            for event in &round.events {
                let line = fill(&event.text, |i| {
                    game.tributes
                        .get(i)
                        .map_or_else(|| "someone".to_string(), |t| t.name.clone())
                });
                text.push_str(&format!("- {line}\n"));
            }
        }
    }
    let what = match kind {
        Kind::Bloodbath => {
            "The Bloodbath: the Games begin, and everyone starts at the Cornucopia full of supplies in the middle of the arena. Open the Games with your `gamemaker` announcement.".to_string()
        }
        Kind::Day(day) => format!("Day {day}."),
        Kind::Night(day) => format!("Night {day}."),
        Kind::Feast(day) => format!(
            "Day {day}: the Feast. You've laid out a feast of supplies at the Cornucopia, worth risking everything for. Announce it with your `gamemaker` line."
        ),
        Kind::Finale => {
            "The final showdown: only two are left. Announce the final two with your `gamemaker` line, then exactly one of them dies and the other wins. Write your `victory` line to the winner.".to_string()
        }
    };
    let deaths = if min == max {
        format!("Exactly {min} tribute{} must die this round.", plural(min))
    } else if min == 0 {
        format!(
            "At most {max} tribute{} may die this round, and it's fine if nobody does.",
            plural(max)
        )
    } else {
        format!("Between {min} and {max} tributes must die this round.")
    };
    text.push_str(&format!(
        "\nThis round: {what} {alive} tributes are alive. {deaths}"
    ));
    text
}

/// A round's name, as shown in Discord and in the story so far.
pub fn title(kind: Kind) -> String {
    match kind {
        Kind::Bloodbath => "The Bloodbath".to_string(),
        Kind::Day(day) => format!("Day {day}"),
        Kind::Night(day) => format!("Night {day}"),
        Kind::Feast(day) => format!("Day {day}: the Feast"),
        Kind::Finale => "The final showdown".to_string(),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Reads her answer: the JSON object in it, even with a code fence or a line around it.
pub fn parse(answer: &str) -> Result<Written, String> {
    let start = answer.find('{');
    let end = answer.rfind('}');
    let (Some(start), Some(end)) = (start, end) else {
        return Err("The answer has no JSON object.".to_string());
    };
    if end < start {
        return Err("The answer has no JSON object.".to_string());
    }
    serde_json::from_str(&answer[start..=end])
        .map_err(|err| format!("The JSON doesn't match the format: {err}."))
}

/// Has her write the round of `kind`, checked. Fails when every try was wrong or the
/// model couldn't be reached.
pub async fn write(
    provider: Arc<dyn ChatProvider>,
    game: Game,
    kind: Kind,
    arena: Vec<String>,
    notes: Arc<Vec<Option<String>>>,
    channel: u64,
) -> anyhow::Result<Round> {
    let mut turns = vec![Turn {
        role: Role::User,
        parts: vec![Part::Text(brief(&game, kind, &arena, &notes))],
    }];
    for attempt in 1..=TRIES {
        let request = ChatRequest {
            system: format!("{SYSTEM}\n\n{FORMAT}"),
            input: Input::Full(turns.clone()),
            tools: Vec::new(),
            cache_key: format!("hunger_games:{channel}"),
            job: "hunger_games",
        };
        let done = complete(provider.as_ref(), request, &NoTools, 1).await?;
        let problem = match parse(&done.text) {
            Ok(written) => match game.check(kind, written) {
                Ok(round) => {
                    info!(attempt, round = title(kind), "wrote a Hunger Games round");
                    return Ok(round);
                }
                Err(problem) => problem,
            },
            Err(problem) => problem,
        };
        warn!(attempt, problem, "a Hunger Games round didn't pass");
        turns.push(Turn {
            role: Role::Assistant,
            parts: vec![Part::Text(done.text)],
        });
        turns.push(Turn {
            role: Role::User,
            parts: vec![Part::Text(format!(
                "That round doesn't work: {problem} Write the whole round again, as JSON only."
            ))],
        });
    }
    anyhow::bail!("Vivy's round didn't pass the rules {TRIES} times")
}

struct NoTools;

#[async_trait]
impl ToolRunner for NoTools {
    async fn run(&self, call: &ToolCall) -> String {
        format!("Error: there is no tool named {}.", call.name)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::sync::mpsc;

    use super::*;
    use crate::ai::{ChatEvent, Done, GeneratedFile};

    /// Answers with each of its texts in turn, and keeps what it was asked.
    struct Scripted {
        answers: Mutex<Vec<&'static str>>,
        asked: Mutex<Vec<Input>>,
    }

    #[async_trait]
    impl ChatProvider for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn model(&self) -> &str {
            "m1"
        }
        async fn stream(
            &self,
            request: ChatRequest,
        ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<ChatEvent>>> {
            self.asked.lock().unwrap().push(request.input);
            let text = self.answers.lock().unwrap().remove(0);
            let (tx, rx) = mpsc::channel(4);
            tx.send(Ok(ChatEvent::TextDelta(text.to_string()))).await?;
            tx.send(Ok(ChatEvent::Done(Done::default()))).await?;
            Ok(rx)
        }
        async fn download_file(&self, _file: &GeneratedFile) -> anyhow::Result<Vec<u8>> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn a_wrong_round_goes_back_with_the_problem() {
        let provider = Arc::new(Scripted {
            answers: Mutex::new(vec![
                r#"{"events": [{"text": "{T1} waves.", "tributes": ["T1"]}]}"#,
                r#"{"gamemaker": "Final two!", "events": [{"text": "{T1} beats {T2}.", "tributes": ["T1", "T2"], "deaths": [{"tribute": "T2", "by": "T1"}]}], "victory": "Well done, Moss."}"#,
            ]),
            asked: Mutex::new(Vec::new()),
        });
        let game = Game::new(vec![(1, "Moss".into()), (2, "Ash".into())]);
        let round = write(
            provider.clone(),
            game,
            Kind::Finale,
            Vec::new(),
            Arc::new(Vec::new()),
            5,
        )
        .await
        .unwrap();
        assert_eq!(round.victory.as_deref(), Some("Well done, Moss."));
        let asked = provider.asked.lock().unwrap();
        assert_eq!(asked.len(), 2);
        let Input::Full(turns) = &asked[1] else {
            panic!("expected the whole conversation");
        };
        assert_eq!(turns.len(), 3);
        let Part::Text(retry) = &turns[2].parts[0] else {
            panic!("expected text");
        };
        assert!(retry.contains("T2 isn't in any event"), "{retry}");
    }

    #[test]
    fn the_brief_has_tags_story_and_pacing() {
        let mut game = Game::new(vec![
            (1, "Moss".into()),
            (2, "Cherry".into()),
            (3, "Ash".into()),
        ]);
        game.tributes[0].kills = 1;
        game.tributes[1].died_in = Some(0);
        game.rounds.push(Round {
            kind: Kind::Bloodbath,
            gamemaker: None,
            events: vec![super::super::game::Event {
                text: "{T1} bonks {T2}.".into(),
                deaths: Vec::new(),
            }],
            victory: None,
        });
        let notes = vec![
            Some("Plays chess".into()),
            Some("dead, unused".into()),
            None,
        ];
        let text = brief(
            &game,
            Kind::Finale,
            &["general".into(), "memes".into()],
            &notes,
        );
        assert!(text.contains("<notes tribute=\"T1\" name=\"Moss\">\nPlays chess\n</notes>"));
        assert!(!text.contains("dead, unused"));
        assert!(text.contains("#general, #memes"));
        assert!(text.contains("- T1: Moss (alive, 1 kill)"));
        assert!(text.contains("- T2: Cherry (dead)"));
        assert!(text.contains("- Moss bonks Cherry."));
        assert!(text.contains("Exactly 1 tribute must die"));
    }

    #[test]
    fn json_is_found_inside_a_fence() {
        let answer = "```json\n{\"gamemaker\": null, \"events\": [{\"text\": \"{T1} naps.\", \"tributes\": [\"T1\"]}]}\n```";
        let written = parse(answer).unwrap();
        assert_eq!(written.events[0].tributes, ["T1"]);
        assert!(written.events[0].deaths.is_empty());
        assert!(parse("no json here").is_err());
        assert!(
            parse("{\"events\": 3}")
                .unwrap_err()
                .contains("doesn't match")
        );
    }
}
