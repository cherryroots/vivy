//! How the Games look in Vivy's pictures, the same in every picture of a game: a top-down
//! map of the arena with a zone for each of its places, and each tribute as a sprite.
//!
//! When the Games start with pictures on, one model call gets every tribute's profile
//! picture and the arena's places, and describes a sprite for each tribute, inspired by
//! their picture, and the terrain of each zone. [`map`] lays the zones out around the
//! Octagon in a fixed order, and [`fill`] puts each tribute's sprite, with the weapon they
//! carry at that point in the game, in place of their tag (`{T3}`) in her picture prompt.

use std::collections::HashMap;
use std::sync::Arc;

use serenity::all::{GuildId, UserId};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
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
/// Where the zones go around the Octagon, in order; past eight, a ring further out.
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

/// The middle of the map, the same in every picture.
const OCTAGON: &str = "the Octagon in the center: an eight-sided fighting cage with a chain-link fence on a raised stone platform";

const SYSTEM: &str = "You design the look of a Hunger Games-style battle royale played by members of a Discord server, drawn as a top-down 2D map of the arena with the tributes as small pixel-art sprites, like a retro strategy game. \
Each member's profile picture is shown with their tag (T1, T2...). For each tribute, describe their sprite, inspired by their profile picture: a person or character becomes a sprite of them; an animal or creature can stay one; a thing, logo or art becomes a sprite whose colors and costume clearly come from it. \
Each sprite is one line of 10 to 25 words that reads at a tiny size: bold main colors, hair or headgear and outfit. No weapon or anything held: what they carry changes during the game and is added for each picture. Make every tribute easy to tell apart from the others by color. Nothing about real identity, health or body size, no names and no words or letters. For a tribute with no picture, invent a fitting sprite from their name. \
The arena's zones are listed with keys (Z1, Z2...) and their names. For each zone, describe its terrain in 3 to 10 words, fitting its name when you can (a channel about games could be an arcade ruin, one about food an orchard), each zone clearly different. Terrain is scenery only: places and things, never people, characters, creatures, robots or crowds. \
Answer with only one JSON object, no code fence: {\"T1\": \"sprite\", ..., \"Z1\": \"terrain\", ...}, with every tribute and every zone.";

/// How the tributes and the zones look, in the game's and the arena's order: `None` for
/// any the answer left out, or all when the call failed.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Looks {
    pub tributes: Vec<Option<String>>,
    pub zones: Vec<Option<String>>,
}

/// The looks while they're being described, shared by the rounds' drawings: the first to
/// need them waits for them, the rest find them ready.
#[derive(Clone)]
pub struct Shared {
    task: Arc<std::sync::Mutex<Option<JoinHandle<Looks>>>>,
    looks: Arc<OnceCell<Looks>>,
}

impl Shared {
    pub fn new(task: JoinHandle<Looks>) -> Shared {
        Shared {
            task: Arc::new(std::sync::Mutex::new(Some(task))),
            looks: Arc::default(),
        }
    }

    /// The looks, waiting for them the first time. A call that failed described nothing.
    pub async fn get(&self) -> &Looks {
        self.looks
            .get_or_init(|| async {
                let task = self.task.lock().unwrap().take();
                match task {
                    Some(task) => task.await.unwrap_or_default(),
                    None => Looks::default(),
                }
            })
            .await
    }
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

/// The arena as the image model is told it every time: the Octagon in the middle and
/// each place as a zone around it, labelled with its name, with its terrain. The last
/// `closed` places are closed off by fire as the arena shrinks.
pub fn map(arena: &[String], looks: &Looks, closed: usize) -> String {
    let first_closed = arena.len().saturating_sub(closed);
    let mut text = format!("The map, laid out the same in every picture: {OCTAGON}");
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
        let fire = if i >= first_closed {
            ", closed off: ringed by a wall of fire, scorched and empty"
        } else {
            ""
        };
        text.push_str(&format!("; {at}, {zone}{terrain}{fire}"));
    }
    text.push('.');
    text
}

/// The picture of a fight in the Octagon (an Octagon round, or the final showdown): zoomed
/// in on the cage in the middle of the map, so the `fighters` are drawn big.
pub fn cage(looks: &Looks, weapons: &[Option<String>], picture: &str, fighters: usize) -> String {
    let people = match fighters {
        1 => "Exactly 1 player is in the picture, the one described above.".to_string(),
        n => format!(
            "Exactly {n} players are in the picture, each one described above, each drawn once."
        ),
    };
    format!(
        "A close, zoomed-in view of just the middle of the battlefield map: {OCTAGON}. The fight inside it fills the picture.\n\nThe fight: {}\n\n{people} Nobody else is anywhere: no crowds, audience, referees, guards, monsters or robots.",
        fill(picture, looks, weapons)
    )
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

/// What the image model gets for a round: the map (with `closed` places closed off), then
/// her prompt with the sprites in, and how many people there are (`alive` when the round
/// began): image models fill an empty place with crowds of their own otherwise.
pub fn prompt(
    arena: &[String],
    looks: &Looks,
    weapons: &[Option<String>],
    picture: &str,
    alive: usize,
    closed: usize,
) -> String {
    let people = match alive {
        1 => "Exactly 1 player is on the map, the one described above.".to_string(),
        n => format!(
            "Exactly {n} players are on the map, each one described above, each drawn once."
        ),
    };
    format!(
        "{}\n\nThis round: {}\n\n{people} Nobody else is anywhere on the map: no crowds, townsfolk, guards, soldiers, enemies, monsters or robots. The zones are empty scenery.",
        map(arena, looks, closed),
        fill(picture, looks, weapons)
    )
}

/// The victor's picture: the winner alone (or a winning team's survivors, side by side),
/// up close, with everyone they killed lying dead around them.
pub fn victor(
    looks: &Looks,
    weapons: &[Option<String>],
    winners: &[usize],
    victims: &[usize],
) -> String {
    let look = |i: usize| match looks.tributes.get(i) {
        Some(Some(look)) => format!("({look})"),
        _ => "a player".to_string(),
    };
    let mut text = match winners {
        [winner] => format!(
            "A full-body picture of the winner of the battle, large and filling most of the frame: {} standing victorious, the only one standing, arms raised high, on a ruined battlefield at sunset.",
            sprite(looks, weapons, *winner)
        ),
        _ => {
            let team: Vec<String> = winners.iter().map(|&i| sprite(looks, weapons, i)).collect();
            format!(
                "A full-body picture of the winning team of the battle, large and filling most of the frame: {} standing victorious side by side, the only ones standing, arms raised high, on a ruined battlefield at sunset.",
                team.join(" and ")
            )
        }
    };
    if victims.is_empty() {
        text.push_str(" Nobody lies around them: they won without defeating anyone, so only scattered gear lies about.");
    } else {
        let defeated: Vec<String> = victims.iter().map(|&i| look(i)).collect();
        text.push_str(&format!(
            " Around them lie the bodies of the {} they killed, dead where they fell, with some blood, and nobody else: {}.",
            match defeated.len() {
                1 => "1 player".to_string(),
                n => format!("{n} players"),
            },
            defeated.join("; ")
        ));
    }
    text.push_str(" No other people or creatures anywhere in the picture.");
    text
}

/// Her picture prompt with each `{T3}`-style tag replaced by that tribute's sprite, or "a
/// player" for someone without one (plain words: the image model doesn't know the Games).
pub fn fill(prompt: &str, looks: &Looks, weapons: &[Option<String>]) -> String {
    game::fill(prompt, |i| sprite(looks, weapons, i))
}

/// One tribute in a picture: their sprite and what they carry.
fn sprite(looks: &Looks, weapons: &[Option<String>], index: usize) -> String {
    let held = match weapons.get(index) {
        Some(Some(weapon)) => format!("holding {weapon}"),
        _ => "empty-handed".to_string(),
    };
    match looks.tributes.get(index) {
        Some(Some(look)) => format!("({look}, {held})"),
        _ => format!("a player {held}"),
    }
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
        let map = map(&arena, &looks, 1);
        assert!(map.starts_with("The map, laid out the same in every picture: the Octagon in the center: an eight-sided fighting cage with a chain-link fence on a raised stone platform; to the north, a zone labelled \"gaming\": an arcade ruin; to the east, a zone; "));
        assert!(map.contains("further out to the east, a zone labelled \"room-7\", closed off: ringed by a wall of fire, scorched and empty."));
        assert!(!map.contains("\"room-6\", closed"));
    }

    #[test]
    fn draws_the_victor_with_their_victims() {
        let looks = Looks {
            tributes: vec![Some("a frog knight".into()), None, Some("a cat".into())],
            zones: Vec::new(),
        };
        let weapons = vec![Some("a cleaver".to_string())];
        let text = victor(&looks, &weapons, &[0], &[2, 1]);
        assert!(text.contains("(a frog knight, holding a cleaver) standing victorious"));
        assert!(
            text.contains("lie the bodies of the 2 players they killed, dead where they fell, with some blood, and nobody else: (a cat); a player.")
        );
        assert!(victor(&looks, &weapons, &[2], &[]).contains("Nobody lies around them"));
        assert!(victor(&looks, &weapons, &[0, 2], &[1]).contains(
            "the winning team of the battle, large and filling most of the frame: (a frog knight, holding a cleaver) and (a cat, empty-handed) standing victorious side by side"
        ));
    }

    #[test]
    fn says_how_many_are_on_the_map() {
        let text = prompt(
            &["general".into()],
            &Looks::default(),
            &[],
            "{T1} naps.",
            3,
            0,
        );
        assert!(text.contains("This round: a player empty-handed naps."));
        assert!(text.contains("Exactly 3 players are on the map"));
        assert!(text.contains("Nobody else is anywhere on the map"));
    }

    #[test]
    fn zooms_in_on_the_cage() {
        let looks = Looks {
            tributes: vec![Some("a frog knight".into()), None],
            zones: vec![Some("an arcade ruin".into()), None],
        };
        let text = cage(
            &looks,
            &[None, Some("a mic-stand spear".into())],
            "{T1} duels {T2}.",
            2,
        );
        assert!(text.contains("just the middle of the battlefield map: the Octagon in the center: an eight-sided fighting cage"));
        assert!(text.contains(
            "The fight: (a frog knight, empty-handed) duels a player holding a mic-stand spear."
        ));
        assert!(text.contains("Exactly 2 players are in the picture"));
        assert!(!text.contains("arcade ruin"));
        assert!(cage(&looks, &[], "{T1} wins.", 1).contains("Exactly 1 player is in the picture"));
    }

    #[test]
    fn fills_the_looks_in() {
        let looks = Looks {
            tributes: vec![Some("a frog knight".to_string()), None],
            zones: Vec::new(),
        };
        let weapons = vec![Some("a cast-iron skillet".to_string())];
        assert_eq!(
            fill("{T1} chases {T2} up a tree", &looks, &weapons),
            "(a frog knight, holding a cast-iron skillet) chases a player empty-handed up a tree"
        );
    }
}
