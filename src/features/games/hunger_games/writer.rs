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

const SYSTEM: &str = "You are Vivy, a Discord bot, and right now you're the Gamemaker of a Hunger Games-style battle royale played in your Discord server. The tributes are real members of the server, and everyone is watching the arena in the channel, so it's a show: dark, brutal and dramatic, and the violence is where the comedy is: over-the-top, creative, absurd kills told deadpan, with plenty of personality. You decide everything that happens in the arena. \
Each round you get the tributes, the arena, the story so far and what kind of round it is, and you write what happens to every living tribute this round. \
Write events like a battle royale simulator: one or two short sentences each, in the past tense, about one to six tributes. Mix it up: kills, ambushes, traps, accidents, desperate last stands. About half the events each round are quiet ones with no deaths: camping, foraging, hunting, building traps, sharing food, forming alliances or breaking them, arguing, napping, talking to the cameras, someone doing something stupid. Say how each kill happens, plainly. Let the story build: grudges, alliances and kill streaks from earlier rounds can come back, and an alliance can end in betrayal. \
Everyone starts unarmed. Weapons are found in the arena's places, which are the server's channels, and each weapon is themed to the place it's found in: be creative (a channel about music might hide a razor-edged vinyl record or a mic-stand spear, one about cooking a cast-iron skillet or a cleaver, one about games a controller swung on its cable), but every weapon must be able to kill. Each tribute carries one weapon at a time, and keeps it until they find, steal or loot a better one. The unarmed fight with fists, rocks, sticks and traps, and usually lose to the armed. \
Death and blood are fine, but skip drawn-out torture and lingering gore. Nothing sexual, nothing hateful, and no jokes about anyone's real body, health, identity or private life. Fair play too: don't pick favourites or settle scores, and anyone can win. \
Write each tribute as their tag in braces, like {T3}, never their name; the bot fills in the names. Write each place as its name with a # in front, exactly as it's listed, like #memes; the bot turns it into a link. Write in English.";

const FORMAT: &str = "Answer with only one JSON object, no code fence and nothing before or after it:
{\"gamemaker\": string or null, \"events\": [{\"text\": string, \"tributes\": [tags], \"deaths\": [{\"tribute\": tag, \"by\": tag or null}], \"finds\": [{\"tribute\": tag, \"weapon\": string}], \"allies\": [tags], \"leaves\": [tags]}], \"champion\": tag or null, \"victory\": string or null, \"picture\": string or null}
- Every living tribute is in exactly one event's `tributes`, and every tag in an event's text is in its `tributes` (and the other way round).
- `deaths` lists who dies in that event, with `by` the tribute who killed them, or null for an accident, the arena or themselves. A killer must be another tribute in the same event.
- `finds` lists every weapon a tribute in that event finds, steals or loots from the dead, with a short name (under 60 characters, like `razor-edged vinyl record`). It replaces what they carried. Someone who dies in the event finds nothing. Leave it out or empty when nobody gets a weapon.
- `allies`: two or more tributes in that event who team up (joining someone's alliance brings you into it). `leaves`: tributes in that event who walk out on their alliance. Killing an ally ends the killer's alliance by itself. Leave both out or empty otherwise.
- `champion`: only in an Octagon round: the tag of the one who wins it, alive at the end. Otherwise null.
- `gamemaker`: your announcement as the Gamemaker before this round, one to three sentences, spoken out loud to everyone watching (no tags, use names there). Only when the round asks for one; otherwise null.
- `victory`: only in the final showdown: what you say to the winner, one to three sentences, by name. Otherwise null.
- `picture`: only when the round asks for one: what to show on the map of the arena this round, for an image model, in English. Go through every event: which zone it happens in (a place's name without the #, or the Octagon in the middle) and what its tributes are doing there, like `In memes, {T1} swings a sword at {T4} while {T2} hides in a bush.` Show someone who dies this round lying dead where they fell, with a little blood. Write each tribute as their tag; the bot adds the map, the zones, how each tribute looks and what they carry, so don't describe those, and use no names. The image model doesn't know the Hunger Games, so use plain words and no Hunger Games terms (Cornucopia, tribute, Bloodbath, Feast, Gamemaker, district). Otherwise null.
- `gamemaker` and `victory` are also spoken out loud, as voice messages in your Gamemaker voice. Steer how they're said with audio tags in square brackets before the words they're for, in plain English: [booming], [dramatic pause], [chuckles], [whispers], [menacing]. Tags are spoken, never shown. Keep each line under 300 characters.";

/// The arena, the tributes, her notes about them, the story so far, and what this round
/// is: what she's told. `notes` are in the same order as the tributes. With `pictures`,
/// every round asks for a `picture` too.
pub fn brief(
    game: &Game,
    kind: Kind,
    arena: &[String],
    notes: &[Option<String>],
    pictures: bool,
) -> String {
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
        text.push_str(&format!(
            "- {}: {} ({})\n",
            Game::tag(i),
            t.name,
            state(game, i)
        ));
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
        Kind::Scatter => {
            "The Scatter: the Games begin. Every tribute stands on a metal starting plate in a ring around the Octagon, an empty eight-sided fighting cage in the middle of the arena. The gong sounds and everyone runs for the arena's places, unarmed; some find their first weapon there, and most just try to get away. Open the Games with your `gamemaker` announcement.".to_string()
        }
        Kind::Day(day) => format!("Day {day}."),
        Kind::Night(day) => format!("Night {day}."),
        Kind::Octagon(day) => {
            let entrants: Vec<String> = game.entrants().into_iter().map(Game::tag).collect();
            format!(
                "Day {day}: the Octagon. The cage in the middle of the arena opens, and you put up a prize for whoever wins it: usually a legendary weapon, which goes in the champion's `finds`, or else a sponsor's gift. Announce the Octagon and its prize with your `gamemaker` line. The bold step in: {}, and nobody else. They fight one-on-one bouts, one after another, until one champion is left; put each bout in its own event. A loser either dies or yields and limps out of the cage to the crowd's boos, alive. Write the winner's tag as `champion`. Everyone else sits it out: watching from the shadows, using the distraction, or too scared to come near, and nobody else dies.",
                entrants.join(", ")
            )
        }
        Kind::Finale => {
            "The final showdown: only two are left, and you force them into the Octagon in the middle of the arena to settle it in the cage. Announce the final two with your `gamemaker` line, then exactly one of them dies and the other wins. Write your `victory` line to the winner.".to_string()
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
    if pictures {
        text.push(' ');
        text.push_str(match kind {
            Kind::Scatter => "Also write a `picture`: everyone running out from their starting plates in a ring around the Octagon toward the zones, and where each event happens.",
            Kind::Night(_) => "Also write a `picture`: the map at night, with each event where it happens (campfires, hiding spots, ambushes in the dark).",
            Kind::Octagon(_) => "Also write a `picture`: the fighters in a bout inside the Octagon, the others watching from the zones, and each event where it happens.",
            Kind::Day(_) => "Also write a `picture`: each event where it happens.",
            Kind::Finale => "Also write a `picture`: the final showdown between the last two inside the Octagon. The winner gets a picture of their own after it.",
        });
    }
    text
}

/// A round's name, as shown in Discord and in the story so far.
pub fn title(kind: Kind) -> String {
    match kind {
        Kind::Scatter => "The Scatter".to_string(),
        Kind::Day(day) => format!("Day {day}"),
        Kind::Night(day) => format!("Night {day}"),
        Kind::Octagon(day) => format!("Day {day}: the Octagon"),
        Kind::Finale => "The final showdown".to_string(),
    }
}

/// How a tribute stands, for her: alive or dead, kills, weapon and allies.
fn state(game: &Game, index: usize) -> String {
    let t = &game.tributes[index];
    if !t.alive() {
        return match &t.weapon {
            Some(weapon) => format!("dead, their {weapon} lies where they fell"),
            None => "dead".to_string(),
        };
    }
    let mut parts = vec!["alive".to_string()];
    match t.kills {
        0 => {}
        1 => parts.push("1 kill".to_string()),
        n => parts.push(format!("{n} kills")),
    }
    parts.push(match &t.weapon {
        Some(weapon) => format!("armed with {weapon}"),
        None => "unarmed".to_string(),
    });
    let allies: Vec<String> = game.allies_of(index).into_iter().map(Game::tag).collect();
    if !allies.is_empty() {
        parts.push(format!("allied with {}", allies.join(", ")));
    }
    parts.join(", ")
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
    pictures: bool,
    channel: u64,
) -> anyhow::Result<Round> {
    let mut turns = vec![Turn {
        role: Role::User,
        parts: vec![Part::Text(brief(&game, kind, &arena, &notes, pictures))],
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

/// For a call with no tools: any call is answered with an error.
pub(super) struct NoTools;

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
            false,
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
        game.tributes[1].weapon = Some("cleaver".into());
        game.tributes[0].team = Some(1);
        game.tributes[2].team = Some(1);
        game.rounds.push(Round {
            kind: Kind::Scatter,
            gamemaker: None,
            events: vec![super::super::game::Event {
                text: "{T1} bonks {T2}.".into(),
                deaths: Vec::new(),
                ..Default::default()
            }],
            victory: None,
            picture: None,
            entrants: Vec::new(),
            champion: None,
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
            false,
        );
        assert!(!text.contains("`picture`"));
        let drawn = brief(&game, Kind::Day(1), &[], &notes, true);
        assert!(drawn.contains("Also write a `picture`: each event where it happens."));
        assert!(
            brief(&game, Kind::Finale, &[], &notes, true).contains("`picture`: the final showdown")
        );
        assert!(text.contains("<notes tribute=\"T1\" name=\"Moss\">\nPlays chess\n</notes>"));
        assert!(!text.contains("dead, unused"));
        assert!(text.contains("#general, #memes"));
        assert!(text.contains("- T1: Moss (alive, 1 kill, unarmed, allied with T3)"));
        assert!(text.contains("- T2: Cherry (dead, their cleaver lies where they fell)"));
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
