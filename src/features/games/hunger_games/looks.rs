//! How the Games look in Vivy's pictures, the same in every picture of a game: a top-down
//! map of the arena with a zone for each of its places, and each tribute as a sprite.
//!
//! When the Games start with pictures on, one model call gets every tribute's profile
//! picture and the arena's places, and describes a sprite for each tribute, inspired by
//! their picture, and the terrain of each zone. [`map`] lays the zones out around the
//! Cornucopia in a fixed order, and [`fill`] puts each tribute's sprite in place of their
//! tag (`{T3}`) in her picture prompt.

use std::collections::HashMap;
use std::sync::Arc;

use serenity::all::{GuildId, UserId};
use tracing::{debug, info, warn};

use super::game::{self, Game};
use super::writer::NoTools;
use crate::ai::{ChatProvider, ChatRequest, Input, Part, Role, Turn, complete};
use crate::core::BotCtx;
use crate::util::media::{self, ModelImage};

/// The size profile pictures are sent at: enough to see what's in them.
const FACE_SIZE: u32 = 256;
/// The longest description kept, in characters.
const MOST: usize = 300;
/// Where the zones go around the Cornucopia, in order; past eight, a ring further out.
const DIRECTIONS: [&str; 8] = [
    "north",
    "east",
    "south",
    "west",
    "northeast",
    "southeast",
    "southwest",
    "northwest",
];

const SYSTEM: &str = "You design the look of a Hunger Games-style battle royale played by members of a Discord server, drawn as a top-down 2D map of the arena with the tributes as small pixel-art sprites, like a retro strategy game. \
Each member's profile picture is shown with their tag (T1, T2...). For each tribute, describe their sprite, inspired by their profile picture: a person or character becomes a sprite of them; an animal or creature can stay one; a thing, logo or art becomes a sprite whose colors and costume clearly come from it. \
Each sprite is one line of 10 to 25 words that reads at a tiny size: bold main colors, hair or headgear, outfit, and one signature item. Make every tribute easy to tell apart from the others by color. Nothing about real identity, health or body size, no names and no words or letters. For a tribute with no picture, invent a fitting sprite from their name. \
The arena's zones are listed with keys (Z1, Z2...) and their names. For each zone, describe its terrain in 3 to 10 words, fitting its name when you can (a channel about games could be an arcade ruin, one about food an orchard), each zone clearly different. \
Answer with only one JSON object, no code fence: {\"T1\": \"sprite\", ..., \"Z1\": \"terrain\", ...}, with every tribute and every zone.";

/// How the tributes and the zones look, in the game's and the arena's order: `None` for
/// any the answer left out, or all when the call failed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Looks {
    pub tributes: Vec<Option<String>>,
    pub zones: Vec<Option<String>>,
}

/// Describes how the tributes and the arena's zones look.
pub async fn describe(
    bot: BotCtx,
    provider: Arc<dyn ChatProvider>,
    guild: GuildId,
    tributes: Vec<(u64, String)>,
    arena: Vec<String>,
) -> Looks {
    // The profile pictures download side by side.
    let downloads: Vec<_> = tributes
        .iter()
        .map(|&(id, _)| tokio::spawn(face(bot.clone(), guild, UserId::new(id))))
        .collect();
    let mut parts = Vec::new();
    for (i, ((_, name), download)) in tributes.iter().zip(downloads).enumerate() {
        let tag = Game::tag(i);
        match download.await {
            Ok(Ok(face)) => {
                parts.push(Part::Text(format!("{tag}, named {name}:")));
                parts.push(Part::Image(face));
            }
            Ok(Err(err)) => {
                debug!("couldn't load a tribute's profile picture: {err:#}");
                parts.push(Part::Text(format!("{tag}, named {name}: no picture.")));
            }
            Err(_) => parts.push(Part::Text(format!("{tag}, named {name}: no picture."))),
        }
    }
    let zones: Vec<String> = arena
        .iter()
        .enumerate()
        .map(|(i, place)| format!("Z{}: {place}", i + 1))
        .collect();
    parts.push(Part::Text(format!("The zones:\n{}", zones.join("\n"))));
    let request = ChatRequest {
        system: SYSTEM.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts,
        }]),
        tools: Vec::new(),
        cache_key: "hunger_games:looks".to_string(),
        job: "hunger_games",
    };
    match complete(provider.as_ref(), request, &NoTools, 1).await {
        Ok(done) => {
            let looks = parse(&done.text, tributes.len(), arena.len());
            info!(
                tributes = looks.tributes.iter().flatten().count(),
                zones = looks.zones.iter().flatten().count(),
                "described how the Hunger Games look"
            );
            looks
        }
        Err(err) => {
            warn!("couldn't describe how the Hunger Games look: {err:#}");
            parse("", tributes.len(), arena.len())
        }
    }
}

/// Someone's profile picture in this server (or their main one) as a PNG.
async fn face(bot: BotCtx, guild: GuildId, user: UserId) -> anyhow::Result<ModelImage> {
    let member = guild.member(&bot.http, user).await?;
    let url = member.avatar_url().unwrap_or_else(|| member.user.face());
    let data = media::download(&media::png_link(&url, FACE_SIZE), 4 * 1024 * 1024).await?;
    Ok(ModelImage {
        mime: "image/png",
        data,
    })
}

/// Reads the looks from her answer: a JSON object from `T1`-style and `Z1`-style keys to
/// descriptions.
pub fn parse(answer: &str, tributes: usize, zones: usize) -> Looks {
    let mut looks = Looks {
        tributes: vec![None; tributes],
        zones: vec![None; zones],
    };
    let (Some(start), Some(end)) = (answer.find('{'), answer.rfind('}')) else {
        return looks;
    };
    let Ok(found) =
        serde_json::from_str::<HashMap<String, String>>(answer.get(start..=end).unwrap_or(""))
    else {
        return looks;
    };
    for (key, text) in found {
        let key = key.trim();
        let (list, number) = match key.split_at_checked(1) {
            Some(("T", n)) => (&mut looks.tributes, n),
            Some(("Z", n)) => (&mut looks.zones, n),
            _ => continue,
        };
        // Braces would read as tags when it's filled into a prompt.
        let text: String = text
            .replace(['{', '}'], "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(slot) = number
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| list.get_mut(i))
            && !text.is_empty()
        {
            *slot = Some(text.chars().take(MOST).collect());
        }
    }
    looks
}

/// The arena as the image model is told it every time: the Cornucopia in the middle and
/// each place as a zone around it, labelled with its name, with its terrain.
pub fn map(arena: &[String], looks: &Looks) -> String {
    let mut text =
        "The arena, laid out the same in every picture: the golden Cornucopia in the center"
            .to_string();
    for (i, place) in arena.iter().enumerate() {
        let direction = DIRECTIONS[i % DIRECTIONS.len()];
        let at = match i / DIRECTIONS.len() {
            0 => format!("to the {direction}"),
            1 => format!("further out to the {direction}"),
            _ => format!("at the far {direction} edge"),
        };
        let zone = match label(place) {
            Some(label) => format!("a zone labelled \"{label}\""),
            None => "a zone".to_string(),
        };
        let terrain = match looks.zones.get(i) {
            Some(Some(terrain)) => format!(": {terrain}"),
            _ => String::new(),
        };
        text.push_str(&format!("; {at}, {zone}{terrain}"));
    }
    text.push('.');
    text
}

/// A place's name as a label on the map: letters, digits, spaces, `-` and `_` only, since
/// an image model draws plain text best. `None` when nothing is left.
fn label(place: &str) -> Option<String> {
    let kept: String = place
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .collect();
    let kept = kept.trim_matches(|c: char| matches!(c, ' ' | '-' | '_'));
    (!kept.is_empty()).then(|| kept.chars().take(24).collect())
}

/// Her picture prompt with each `{T3}`-style tag replaced by that tribute's sprite, or "a
/// tribute" for someone without one.
pub fn fill(prompt: &str, looks: &Looks) -> String {
    game::fill(prompt, |i| match looks.tributes.get(i) {
        Some(Some(look)) => format!("({look})"),
        _ => "a tribute".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_looks() {
        let answer = "Here:\n{\"T1\": \"a teal-haired  girl\\nwith a {kite}\", \"T3\": \"a frog knight\", \"T9\": \"nobody\", \"T2\": \" \", \"Z2\": \"an arcade ruin\", \"Z0\": \"no\", \"X1\": \"no\"}";
        let looks = parse(answer, 3, 2);
        assert_eq!(
            looks.tributes,
            vec![
                Some("a teal-haired girl with a kite".to_string()),
                None,
                Some("a frog knight".to_string())
            ]
        );
        assert_eq!(looks.zones, vec![None, Some("an arcade ruin".to_string())]);
        assert_eq!(parse("no json", 2, 1).tributes, vec![None, None]);
    }

    #[test]
    fn lays_out_the_map() {
        let looks = Looks {
            tributes: Vec::new(),
            zones: vec![Some("an arcade ruin".into()), None],
        };
        let arena: Vec<String> = ["🎮・gaming", "💬"]
            .iter()
            .map(|s| s.to_string())
            .chain((0..8).map(|i| format!("room-{i}")))
            .collect();
        let map = map(&arena, &looks);
        assert!(map.starts_with("The arena, laid out the same in every picture: the golden Cornucopia in the center; to the north, a zone labelled \"gaming\": an arcade ruin; to the east, a zone; "));
        assert!(map.contains("further out to the east, a zone labelled \"room-7\"."));
    }

    #[test]
    fn fills_the_looks_in() {
        let looks = Looks {
            tributes: vec![Some("a frog knight".to_string()), None],
            zones: Vec::new(),
        };
        assert_eq!(
            fill("{T1} chases {T2} up a tree", &looks),
            "(a frog knight) chases a tribute up a tree"
        );
    }
}
