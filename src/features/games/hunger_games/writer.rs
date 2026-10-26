//! Vivy writing a round: one model call, answered in JSON, checked by the rules in
//! `game.rs`. A round that doesn't pass goes back to her with what's wrong, at most
//! [`TRIES`] times in all.
//!
//! She plays the Gamemaker in her own voice, but her mood isn't in the prompt: it never
//! changes what happens in the arena.

use std::sync::Arc;

use tracing::{info, warn};

use super::game::{Endgame, Game, Kind, Reward, Round, Written, fill};
use super::ui;
use crate::ai::{ChatProvider, ChatRequest, Input, Part, Role, Turn, complete};

/// How many times she may write a round before the game gives up.
const TRIES: usize = 3;

const SYSTEM: &str = "You are Vivy, a Discord bot, and right now you're the Gamemaker of a Hunger Games-style battle royale played in your Discord server. The tributes are real members of the server, and everyone is watching the arena in the channel, so it's a show: dark, brutal and dramatic, and the violence is where the comedy is: over-the-top, creative, absurd kills told deadpan, with plenty of personality. You decide everything that happens in the arena. \
Each round you get the tributes, the arena, the story so far and what kind of round it is, and you write what happens to every living tribute this round. \
Write events like a battle royale simulator: one or two short sentences each, in the past tense, about one to six tributes. Mix it up: kills, ambushes, traps, accidents, desperate last stands. About half the events each round are quiet ones with no deaths: camping, foraging, hunting, building traps, sharing food, forming alliances or breaking them, arguing, napping, talking to the cameras, someone doing something stupid. Say how each kill happens, plainly. Let the story build: grudges, alliances and kill streaks from earlier rounds can come back, and an alliance can end in betrayal. \
Everyone starts unarmed. Weapons are found in the arena's places, which are the server's channels, and each weapon is themed to the place it's found in: be creative (a channel about music might hide a razor-edged vinyl record or a mic-stand spear, one about cooking a cast-iron skillet or a cleaver, one about games a controller swung on its cable), but every weapon must be able to kill. Each tribute carries one weapon at a time, and keeps it until they find, steal or loot a better one. The unarmed fight with fists, rocks, sticks and traps, and usually lose to the armed. A legendary weapon won in the Octagon is the deadliest in the arena: whoever carries one wins nearly every fight, and only an ambush, a trap, being outnumbered or the arena itself can take them down; whoever kills them can loot it. \
Death and blood are fine, but skip drawn-out torture and lingering gore. Nothing sexual, nothing hateful, and no jokes about anyone's real body, health, identity or private life. Fair play too: don't pick favourites or settle scores, and anyone can win. \
Write each tribute as their tag in braces, like {T3}, never their name; the bot fills in the names. Write each place as its name with a # in front, exactly as it's listed, like #memes; the bot turns it into a link. Write in English.";

const FORMAT: &str = "Answer with only one JSON object, no code fence and nothing before or after it:
{\"gamemaker\": string or null, \"events\": [{\"text\": string, \"tributes\": [tags], \"deaths\": [{\"tribute\": tag, \"by\": tag or null}], \"finds\": [{\"tribute\": tag, \"weapon\": string}], \"allies\": [tags], \"leaves\": [tags], \"picture\": string or null}], \"champion\": tag or null, \"victory\": string or null, \"picture\": string or null, \"theme\": {\"name\": string, \"twist\": string, \"surprise\": string} or null}
- Every living tribute is in exactly one event's `tributes`, and every tribute in an event's `tributes` is tagged in its text. A living tribute is tagged only in the text of their own event, never in another one (not even a teammate thinking of them). The dead are not in any `tributes`, but their tags may appear in any text: looting their body, avenging them, mourning them.
- `deaths` lists who dies in that event, with `by` the tribute who killed them, or null for an accident, the arena or themselves. A killer must be another tribute in the same event.
- `finds` lists every weapon a tribute in that event finds, steals or loots from the dead, with a short name (under 60 characters, like `razor-edged vinyl record`). It replaces what they carried. Someone who dies in the event finds nothing. Leave it out or empty when nobody gets a weapon.
- `allies`: two or more tributes in that event who team up (joining someone's alliance brings you into it). `leaves`: tributes in that event who walk out on their alliance. Killing an ally ends the killer's alliance by itself. Leave both out or empty otherwise.
- `champion`: only in an Octagon round: the tag of the one who wins it, alive at the end. Otherwise null.
- `gamemaker`: your announcement as the Gamemaker before this round, one to three sentences, spoken out loud to everyone watching (no tags, use names there). Only when the round asks for one; otherwise null.
- `victory`: only when this round ends the Games (one side is left): what you say to the winner, one to three sentences, by name. Otherwise null.
- `picture`: only when the round asks for one: what to show on the map of the arena this round, for an image model, in English. Go through every event: which zone it happens in (a place's name without the #, or the Octagon in the middle) and what its tributes are doing there, like `In memes, {T1} swings a sword at {T4} while {T2} hides in a bush.` Show someone who dies this round lying dead where they fell, with a little blood. Write each tribute as their tag; the bot adds the map, the zones, how each tribute looks and what they carry, so don't describe those, and use no names. The image model doesn't know the Hunger Games, so use plain words and no Hunger Games terms (Cornucopia, tribute, Bloodbath, Feast, Gamemaker, district). Otherwise null.
- An event's `picture`: only for the events the round asks for, the most interesting ones that people will most want to see: a close-up of that event alone, for the same image model, one or two sentences: what its tributes are doing at that moment and the scenery right around them, like `{T1} swings a sword at {T4} beside a toppled arcade cabinet while {T2} hides behind it.` Tag everyone in it, and only them; the dead from earlier rounds only when the event is about their body. The bot takes the zone from the place in the event's text and adds how each tribute looks and what they carry. Same rules as the round's `picture`: tags, no names, plain words, no Hunger Games terms, the dying lying dead with a little blood. Otherwise null.
- `theme`: only in the Scatter: the theme you pick for these Games. `name` is the season's name (a few words, like `The Static Season`), `twist` one quirk of this arena everyone knows about (a storm, a sponsor, a strange rule), and `surprise` something you mean to spring on them once, later on, that they don't know yet. Make it fit the tributes and the places. It's a mood, not a plot: the deaths and the Octagon aren't yours to plan. Otherwise null.
- `gamemaker` and `victory` are also spoken out loud, as voice messages in your Gamemaker voice. Steer how they're said with audio tags in square brackets before the words they're for, in plain English: [booming], [dramatic pause], [chuckles], [whispers], [menacing]. Tags are spoken, never shown. Keep each line under 300 characters.";

/// The arena, the tributes, her notes about them, the story so far, and what this round
/// is: what she's told. `areas` describe the arena's places, in its order (see
/// `areas.rs`); `notes` are in the same order as the tributes. With `pictures`, every
/// round asks for a `picture` too.
pub fn brief(
    game: &Game,
    kind: Kind,
    arena: &[String],
    areas: &[Option<String>],
    notes: &[Option<String>],
    pictures: bool,
) -> String {
    let alive = game.alive();
    let (min, max) = game.deaths_range(kind);
    let at = game.rounds.len();
    // The places whose index is (or isn't) in a range, as `#name`.
    let places = |range: std::ops::Range<usize>, inside: bool| -> Vec<String> {
        arena
            .iter()
            .enumerate()
            .filter(|(i, _)| range.contains(i) == inside)
            .map(|(_, n)| format!("#{n}"))
            .collect()
    };
    let closed = game.closed(at);
    let closing = places(game.closing(at), true);
    let open = places(closed.clone(), false);
    let mut text = String::new();
    if !arena.is_empty() {
        text.push_str(
            "The arena's places for this game (some of the server's channels, picked at random; spread the tributes around, and don't keep sending people to the same place): ",
        );
        text.push_str(&places(closed.clone(), false).join(", "));
        text.push('\n');
        // What each open place is like, so events and the weapons found there fit it.
        let described: Vec<String> = arena
            .iter()
            .enumerate()
            .filter(|(i, _)| !closed.contains(i))
            .filter_map(|(i, name)| {
                let area = areas.get(i)?.as_deref()?;
                Some(format!("- #{name}: {area}\n"))
            })
            .collect();
        if !described.is_empty() {
            text.push_str(
                "What each place is like (keep events and the weapons found there true to it):\n",
            );
            text.push_str(&described.concat());
        }
        let shut = places(closed, true);
        if !shut.is_empty() {
            text.push_str(&format!(
                "The arena is shrinking to drive the survivors together. Closed off by walls of fire, so nobody can be there anymore: {}\n",
                shut.join(", ")
            ));
        }
        text.push('\n');
    }
    if game.teams() {
        text.push_str(
            "This is a team Games: the tributes fight in pairs, and each one's teammate is in their state below. Teammates look out for each other and never hurt each other, but they can get split up, and one can fall while the other fights on for both. A team wins together: the Games end when only one team has anyone left alive, and a fallen teammate shares the win. Anyone without a teammate fights alone. Alliances between teams can still form and break.\n\n",
        );
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
    let grudges: Vec<String> = game
        .open_grudges()
        .iter()
        .map(|g| {
            // How the last time happened, so revenge can call back to it. Its place is
            // left out: it may not be in this arena.
            let last = match &g.last {
                Some(blow) => format!(
                    ", the last time {}{}",
                    ui::with_weapon(blow.weapon.as_deref().unwrap_or_default()),
                    if blow.octagon { " in the Octagon" } else { "" }
                ),
                None => String::new(),
            };
            format!(
                "- {} killed {} {}{last}",
                Game::tag(g.killer),
                Game::tag(g.victim),
                g.how_often()
            )
        })
        .collect();
    if !grudges.is_empty() {
        text.push_str(
            "\nGrudges from earlier Games in this server, among the living (who killed whom before). Bring one up now and then: a taunt, a hunt for revenge, a rematch, someone fleeing their old killer. It's a story hook, not a script: revenge can fail, and it doesn't decide who wins:\n",
        );
        text.push_str(&grudges.join("\n"));
        text.push('\n');
    }
    if let Some(theme) = &game.theme {
        text.push_str(&format!(
            "\nThis season's theme, which you picked: {}. The arena's twist: {} Your surprise, which the tributes don't know about yet: {} Let the theme colour your Gamemaker lines and some events, and spring the surprise once, when it fits (an Octagon or the last two are good moments; the story so far shows whether you already did). Don't force it into every event.\n",
            theme.name, theme.twist, theme.surprise
        ));
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
            "The Scatter: the Games begin. Every tribute stands on a metal starting plate in a ring around the Octagon, an empty eight-sided fighting cage in the middle of the arena. The gong sounds and everyone runs for the arena's places, unarmed; some find their first weapon there, and most just try to get away. Pick this season's `theme`, and open the Games with your `gamemaker` announcement, which names it.".to_string()
        }
        Kind::Day(_) | Kind::Night(_) => {
            let name = title(kind);
            match game.endgame(at) {
                Some(Endgame::Wander) => format!("{name}.{}{}", shrink(&closing), wander(game)),
                Some(Endgame::Showdown) => {
                    format!("{name}.{}{}", shrink(&closing), showdown(game, &open))
                }
                None => format!("{name}.{}{}", shrink(&closing), hunt(game)),
            }
        }
        Kind::Octagon(day) => {
            let entrants: Vec<String> = game.entrants().into_iter().map(Game::tag).collect();
            let prize = match game.prize.unwrap_or(Reward::Weapon) {
                Reward::Weapon => "a legendary weapon, the deadliest in the arena, which goes in the champion's `finds` (name it so it sounds legendary)",
                Reward::Armour => "a suit of golden armour: whoever wins it can't die in the next two rounds",
                Reward::Hunt => "the hunt: whoever wins it tracks someone down and kills them in the next round",
            };
            format!(
                "Day {day}: the Octagon. The cage in the middle of the arena opens, and the crowd voted for the prize the winner takes: {prize}. Announce the Octagon and its prize with your `gamemaker` line. The bold step in: {}, and nobody else. They fight one-on-one bouts, one after another, until one champion is left; put each bout in its own event. A loser either dies or yields and limps out of the cage to the crowd's boos, alive. Write the winner's tag as `champion`. Everyone else sits it out: watching from the shadows, using the distraction, or too scared to come near, and nobody else dies.",
                entrants.join(", ")
            )
        }
    };
    let deaths = if max == 0 {
        "Nobody dies this round.".to_string()
    } else if min == max {
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
            _ if game.endgame(at) == Some(Endgame::Showdown) => "Also write a `picture`: the showdown between everyone left, in the zone where they meet, with the fire closing in. The winners get a picture of their own after it.",
            Kind::Scatter => "Also write a `picture`: everyone running out from their starting plates in a ring around the Octagon toward the zones, and where each event happens.",
            Kind::Night(_) => "Also write a `picture`: the map at night, with each event where it happens (campfires, hiding spots, ambushes in the dark).",
            Kind::Octagon(_) => "Also write a `picture`: a close view inside the Octagon with only those who stepped in: the champion standing with the prize after the last bout, and each loser where their bout ended, dead or limping out of the cage. Leave out everyone who sat it out.",
            Kind::Day(_) => "Also write a `picture`: each event where it happens.",
        });
        // Close-ups of the most interesting events, as many as the server allows.
        let most = game.rules.close_ups;
        let (best, all) = match most {
            1 => (
                "the most interesting one gets".to_string(),
                "when there's only one".to_string(),
            ),
            n => (
                format!("the {n} most interesting get"),
                format!("when there are {n} or fewer"),
            ),
        };
        if most == 0 {
            text.push_str(" Events get no `picture` of their own.");
        } else if let Kind::Octagon(_) = kind {
            text.push_str(&format!(
                " The Octagon's `picture` already shows the fights in the cage, so give close-ups only to events outside it: of those, {best} their own `picture` (all of them, {all})."
            ));
        } else {
            text.push_str(&format!(
                " And give events their own `picture`, a close-up of each: {best} one (every event, {all})."
            ));
        }
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
    }
}

/// For a day or night while the last two sides wander (see [`Game::endgame`]).
fn wander(game: &Game) -> String {
    let (who, end) = if game.teams() {
        (
            "Only two teams are left",
            "everyone left on one team dies and the other team wins: write your `victory` line to the winning team, both of them by name, a fallen teammate too",
        )
    } else {
        (
            "Only two are left",
            "one of them dies and the other wins: write your `victory` line to the winner",
        )
    };
    format!(
        " {who}, somewhere in the shrinking arena, and the fire keeps burning closer. Nothing is forced: they may find each other this round, or not yet. One can be hunting the other, following tracks, setting a trap, or they hide, just miss each other, or have a moment that isn't a fight at all. A kill isn't needed every round. If they do meet and settle it, {end}."
    )
}

/// For the showdown: the last two sides meet in a place still `open`.
fn showdown(game: &Game, open: &[String]) -> String {
    let place = match open {
        [] => "the last corner of the arena the fire hasn't reached".to_string(),
        [place] => format!("{place}, the last place the fire hasn't reached"),
        _ => format!("one of the places still open ({})", open.join(", ")),
    };
    let ending = if game.teams() {
        "Announce the final teams with your `gamemaker` line, then everyone left on one team dies and the other team wins. Write your `victory` line to the winning team, both of them by name, a fallen teammate too."
    } else {
        "Announce the final two with your `gamemaker` line, then exactly one of them dies and the other wins. Write your `victory` line to the winner."
    };
    format!(
        " The showdown: the fire has driven the last ones together, and they finally find each other in {place}. {ending}"
    )
}

/// For a day or night when places close: which ones, and what that means.
fn shrink(closing: &[String]) -> String {
    if closing.is_empty() {
        return String::new();
    }
    format!(
        " The arena shrinks as this round begins: walls of fire close off {}. Anyone there flees to the places still open, or the fire gets them (a death by the arena).",
        closing.join(", ")
    )
}

/// For a day or night after an Octagon won with the hunt: who must make a kill.
fn hunt(game: &Game) -> String {
    match game.hunter(game.rounds.len()) {
        Some(hunter) => format!(
            " {} won the hunt in the Octagon: this round they track someone down and kill them.",
            Game::tag(hunter)
        ),
        None => String::new(),
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
        Some(weapon) if game.holds_prize(index) => {
            format!("armed with {weapon}, an Octagon prize (wins nearly every fight)")
        }
        Some(weapon) => format!("armed with {weapon}"),
        None => "unarmed".to_string(),
    });
    let at = game.rounds.len();
    if game.armoured(index, at) && game.endgame(at) != Some(Endgame::Showdown) {
        parts.push("wearing golden armour from the Octagon (can't die this round)".to_string());
    }
    if let Some(partner) = t.partner {
        parts.push(if game.tributes[partner].alive() {
            format!("teammate {}", Game::tag(partner))
        } else {
            format!("teammate {} has fallen", Game::tag(partner))
        });
    }
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
#[allow(clippy::too_many_arguments)]
pub async fn write(
    provider: Arc<dyn ChatProvider>,
    game: Game,
    kind: Kind,
    arena: Vec<String>,
    areas: Arc<Vec<Option<String>>>,
    notes: Arc<Vec<Option<String>>>,
    pictures: bool,
    channel: u64,
) -> anyhow::Result<Round> {
    let mut turns = vec![Turn {
        role: Role::User,
        parts: vec![Part::Text(brief(
            &game, kind, &arena, &areas, &notes, pictures,
        ))],
    }];
    for attempt in 1..=TRIES {
        let request = ChatRequest {
            system: format!("{SYSTEM}\n\n{FORMAT}"),
            input: Input::Full(turns.clone()),
            tools: Vec::new(),
            cache_key: format!("hunger_games:{channel}"),
            job: "hunger_games",
            answer_now: false,
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

pub(super) use crate::features::games::herself::NoTools;

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use tokio::sync::mpsc;

    use super::*;
    use crate::ai::{ChatEvent, Done, GeneratedFile};
    use crate::features::games::hunger_games::game::Blow;

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
        let mut game = Game::new(vec![(1, "Moss".into()), (2, "Ash".into())]);
        game.rounds.push(scatter());
        let round = write(
            provider.clone(),
            game,
            Kind::Day(1),
            Vec::new(),
            Arc::new(Vec::new()),
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

    fn scatter() -> Round {
        Round {
            kind: Kind::Scatter,
            gamemaker: None,
            events: Vec::new(),
            victory: None,
            picture: None,
            entrants: Vec::new(),
            champion: None,
            reward: None,
            theme: None,
        }
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
            reward: None,
            theme: None,
        });
        let notes = vec![
            Some("Plays chess".into()),
            Some("dead, unused".into()),
            None,
        ];
        game.theme = Some(super::super::game::Theme {
            name: "The Static Season".into(),
            twist: "Acid rain at noon.".into(),
            surprise: "The Octagon floods.".into(),
        });
        // Two are left, and with no places to wander it's the showdown at once.
        let text = brief(
            &game,
            Kind::Day(1),
            &["general".into(), "memes".into()],
            &[Some("A sunny plaza with a broken fountain.".into()), None],
            &notes,
            false,
        );
        assert!(!text.contains("`picture`"));
        assert!(text.contains("What each place is like (keep events and the weapons found there true to it):\n- #general: A sunny plaza with a broken fountain.\n"));
        assert!(!text.contains("- #memes"));
        assert!(text.contains("they finally find each other in one of the places still open (#general, #memes). Announce the final two"));
        let drawn = brief(&game, Kind::Day(1), &[], &[], &notes, true);
        assert!(drawn.contains("Also write a `picture`: the showdown between everyone left"));
        assert!(drawn.contains("And give events their own `picture`, a close-up of each: the 3 most interesting get one (every event, when there are 3 or fewer)."));
        let mut fewer = game.clone();
        fewer.rules.close_ups = 1;
        let drawn = brief(&fewer, Kind::Day(1), &[], &[], &notes, true);
        assert!(
            drawn
                .contains("the most interesting one gets one (every event, when there's only one)")
        );
        fewer.rules.close_ups = 0;
        let drawn = brief(&fewer, Kind::Day(1), &[], &[], &notes, true);
        assert!(drawn.contains("Events get no `picture` of their own."));
        assert!(text.contains("<notes tribute=\"T1\" name=\"Moss\">\nPlays chess\n</notes>"));
        assert!(!text.contains("dead, unused"));
        assert!(text.contains("#general, #memes"));
        assert!(text.contains("- T1: Moss (alive, 1 kill, unarmed, allied with T3)"));
        assert!(text.contains("- T2: Cherry (dead, their cleaver lies where they fell)"));
        assert!(text.contains("- Moss bonks Cherry."));
        assert!(text.contains("Exactly 1 tribute must die"));
        assert!(text.contains("This season's theme, which you picked: The Static Season. The arena's twist: Acid rain at noon. Your surprise, which the tributes don't know about yet: The Octagon floods."));
    }

    #[test]
    fn the_brief_has_the_shrinking_arena_and_grudges() {
        let mut game = Game::new((1..=8).map(|id| (id, format!("P{id}"))).collect());
        let arena: Vec<String> = (0..8).map(|i| format!("room-{i}")).collect();
        game.places = arena.len();
        let axe = Blow {
            weapon: Some("axe".into()),
            place: Some(5),
            octagon: true,
        };
        game.remember(&[(7, 8, 3, Some(axe)), (1, 8, 1, None)]);
        let text = brief(&game, Kind::Scatter, &arena, &[], &[], false);
        assert!(text.contains(
            "- T7 killed T8 3 times, the last time with an axe in the Octagon\n- T1 killed T8 once\n"
        ));
        assert!(!text.contains("walls of fire"));
        // Five fall in the first night, so day 2 closes five places.
        for t in &mut game.tributes[..5] {
            t.died_in = Some(0);
        }
        game.rounds.push(Round {
            kind: Kind::Night(1),
            gamemaker: None,
            events: Vec::new(),
            victory: None,
            picture: None,
            entrants: Vec::new(),
            champion: None,
            reward: None,
            theme: None,
        });
        let text = brief(&game, Kind::Day(2), &arena, &[], &[], false);
        assert!(text.contains("spread the tributes around, and don't keep sending people to the same place): #room-0, #room-1, #room-2\n"));
        assert!(text.contains("Closed off by walls of fire, so nobody can be there anymore: #room-3, #room-4, #room-5, #room-6, #room-7\n"));
        assert!(text.contains("This round: Day 2. The arena shrinks as this round begins: walls of fire close off #room-3, #room-4, #room-5, #room-6, #room-7."));
        // P1's victim, P8, is still alive, but P1 isn't: only living grudges are told.
        assert!(
            text.contains("- T7 killed T8 3 times, the last time with an axe in the Octagon\n\n")
        );
        assert!(!text.contains("T1 killed"));
    }

    #[test]
    fn a_team_brief_names_teammates() {
        let mut game = Game::new((1..=6).map(|id| (id, format!("P{id}"))).collect());
        for (a, b) in [(0, 1), (2, 3), (4, 5)] {
            game.tributes[a].partner = Some(b);
            game.tributes[b].partner = Some(a);
        }
        game.tributes[1].died_in = Some(0);
        let text = brief(&game, Kind::Day(1), &[], &[], &[], false);
        assert!(text.contains("This is a team Games"));
        assert!(text.contains("- T1: P1 (alive, unarmed, teammate T2 has fallen)"));
        assert!(text.contains("- T3: P3 (alive, unarmed, teammate T4)"));
        // P3 and P4 fall too: two teams are left, and they wander while places are open.
        game.tributes[2].died_in = Some(0);
        game.tributes[3].died_in = Some(0);
        game.rounds.push(scatter());
        game.places = 3;
        let arena: Vec<String> = (0..3).map(|i| format!("room-{i}")).collect();
        let text = brief(&game, Kind::Day(1), &arena, &[], &[], false);
        assert!(text.contains("Only two teams are left, somewhere in the shrinking arena"));
        assert!(text.contains("Nothing is forced"));
        assert!(
            text.contains("At most 2 tributes may die this round, and it's fine if nobody does.")
        );
        game.places = 1;
        let text = brief(&game, Kind::Day(1), &arena[..1], &[], &[], false);
        assert!(text.contains(
            "in #room-0, the last place the fire hasn't reached. Announce the final teams"
        ));
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
