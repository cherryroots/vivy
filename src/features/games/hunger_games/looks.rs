//! How each tribute looks in Vivy's pictures, so they look the same in every picture of a
//! game. When the Games start with pictures on, one model call gets every tribute's
//! profile picture and describes a realistic character for each, inspired by it. Her picture
//! prompts name the tributes by tag (`{T3}`), and [`fill`] puts each one's look in its
//! place before the prompt goes to the drawing tool.

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
/// The longest look kept, in characters.
const MOST: usize = 300;

const SYSTEM: &str = "You design the characters for the pictures of a Hunger Games-style battle royale played by members of a Discord server, drawn as realistic film stills. Each member's profile picture is shown with their tag. For each tribute, describe one real-looking person inspired by their profile picture: if it shows a person or a drawn character, a realistic version of them; if it shows an animal, a thing, a logo or art, a person whose look clearly comes from it (its colors, its creature as a costume or emblem, its mood). \
Each description is one line of 15 to 30 words that an image model can draw the same way every time: rough age, hair, face, clothes and their colors, and one thing they carry or wear that stands out. Make every tribute easy to tell apart from the others. Nothing about real identity, health or body size, no names and no words or letters. \
For a tribute with no picture, invent a fitting person from their name. \
Answer with only one JSON object, no code fence: {\"T1\": \"description\", \"T2\": \"description\", ...}, with every tribute.";

/// Describes how each tribute looks, in the game's order: `None` for anyone the answer
/// left out, or everyone when the call failed.
pub async fn describe(
    bot: BotCtx,
    provider: Arc<dyn ChatProvider>,
    guild: GuildId,
    tributes: Vec<(u64, String)>,
) -> Vec<Option<String>> {
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
            let looks = parse(&done.text, tributes.len());
            let described = looks.iter().flatten().count();
            info!(
                described,
                tributes = tributes.len(),
                "described the Hunger Games tributes"
            );
            looks
        }
        Err(err) => {
            warn!("couldn't describe the Hunger Games tributes: {err:#}");
            vec![None; tributes.len()]
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

/// Reads the looks from her answer: a JSON object from tag to description.
pub fn parse(answer: &str, count: usize) -> Vec<Option<String>> {
    let mut looks = vec![None; count];
    let (Some(start), Some(end)) = (answer.find('{'), answer.rfind('}')) else {
        return looks;
    };
    let Ok(found) =
        serde_json::from_str::<HashMap<String, String>>(answer.get(start..=end).unwrap_or(""))
    else {
        return looks;
    };
    for (tag, look) in found {
        let index = tag
            .trim()
            .strip_prefix('T')
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| (1..=count).contains(n));
        // Braces would read as tags when the look is filled into a prompt.
        let look: String = look
            .replace(['{', '}'], "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(n) = index
            && !look.is_empty()
        {
            looks[n - 1] = Some(look.chars().take(MOST).collect());
        }
    }
    looks
}

/// Her picture prompt with each `{T3}`-style tag replaced by that tribute's look, or "a
/// tribute" for someone without one.
pub fn fill(prompt: &str, looks: &[Option<String>]) -> String {
    game::fill(prompt, |i| match looks.get(i) {
        Some(Some(look)) => format!("({look})"),
        _ => "a tribute".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_looks() {
        let answer = "Here:\n{\"T1\": \"a teal-haired  girl\\nwith a {kite}\", \"T3\": \"a frog knight\", \"T9\": \"nobody\", \"T2\": \" \"}";
        let looks = parse(answer, 3);
        assert_eq!(looks[0].as_deref(), Some("a teal-haired girl with a kite"));
        assert_eq!(looks[1], None);
        assert_eq!(looks[2].as_deref(), Some("a frog knight"));
        assert_eq!(parse("no json", 2), vec![None, None]);
    }

    #[test]
    fn fills_the_looks_in() {
        let looks = vec![Some("a frog knight".to_string()), None];
        assert_eq!(
            fill("{T1} chases {T2} up a tree", &looks),
            "(a frog knight) chases a tribute up a tree"
        );
    }
}
