//! What each of the arena's places is like: a short description of each area, written once
//! as the Games start, like the tributes' looks (Cherry's call). Vivy gets the places'
//! names and her memory notes about what the server's channels are for
//! (`/memories/server/channels.md`, see `notes.rs`), so each area fits how people use its
//! channel. Her brief shows each place with its description every round, so events and the
//! weapons found there fit it, and the pictures use it as the zone's scenery.

use std::sync::Arc;

use tracing::{info, warn};

use super::looks;
use super::writer::NoTools;
use crate::ai::{ChatProvider, ChatRequest, Input, Part, Role, Turn, complete};

const SYSTEM: &str = "You design the arena of a Hunger Games-style battle royale played by members of a Discord server. Each of the arena's areas is one of the server's channels, listed with keys (Z1, Z2...) and their names, and with your notes about what the channels are for when you have some. \
For each area, write a general description of what it's like, in one or two sentences under 40 words: start with its terrain and landmarks, then its mood and what lies around to be found or used. Make it fit what the channel is for and how people use it, as a place you could fight in (a channel about games could be an arcade ruin, one about food an orchard or a market kitchen, one for memes a junkyard of old posters), and make each area clearly different from the others. \
Scenery only: places and things, never people, characters, creatures, robots or crowds, and no names of members. \
Answer with only one JSON object, no code fence: {\"Z1\": \"description\", ...}, with every area.";

/// The description of each place in `arena`, in its order: `None` for any the answer left
/// out, or all when the call failed. `channels` is her note about the server's channels.
pub async fn describe(
    provider: Arc<dyn ChatProvider>,
    arena: Vec<String>,
    channels: Option<String>,
) -> Vec<Option<String>> {
    if arena.is_empty() {
        return Vec::new();
    }
    let request = ChatRequest {
        system: SYSTEM.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts: vec![Part::Text(ask(&arena, channels.as_deref()))],
        }]),
        tools: Vec::new(),
        cache_key: "hunger_games:areas".to_string(),
        job: "hunger_games",
    };
    match complete(provider.as_ref(), request, &NoTools, 1).await {
        Ok(done) => {
            let areas = looks::parse(&done.text, 0, arena.len()).zones;
            info!(
                areas = areas.iter().flatten().count(),
                notes = channels.is_some(),
                "described the Hunger Games arena"
            );
            areas
        }
        Err(err) => {
            warn!("couldn't describe the Hunger Games arena: {err:#}");
            vec![None; arena.len()]
        }
    }
}

/// What she's asked: the areas, and her notes about the channels when there are some.
fn ask(arena: &[String], channels: Option<&str>) -> String {
    let mut text = String::new();
    if let Some(channels) = channels {
        text.push_str(&format!(
            "Your notes about the server's channels:\n<notes>\n{channels}\n</notes>\n\n"
        ));
    }
    text.push_str("The areas:\n");
    for (i, place) in arena.iter().enumerate() {
        text.push_str(&format!("Z{}: #{place}\n", i + 1));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_with_the_channel_notes() {
        let arena = vec!["general".to_string(), "memes".to_string()];
        assert_eq!(
            ask(&arena, Some("#memes: shitposts")),
            "Your notes about the server's channels:\n<notes>\n#memes: shitposts\n</notes>\n\nThe areas:\nZ1: #general\nZ2: #memes\n"
        );
        assert_eq!(ask(&arena[..1], None), "The areas:\nZ1: #general\n");
    }
}
