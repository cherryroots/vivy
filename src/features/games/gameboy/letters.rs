//! Naming a game's letters: the supervisor's model reads a text box's picture once, and
//! code keeps what each letter's picture is called (`text.rs`), so the game's words read
//! exactly from then on, with no model. For games read from the screen (`any`).
//!
//! The model is told how many words each row has and how many tiles each word is, and
//! only a row that fits is taken, so a misread word names nothing.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::json;

use super::screen::TileKey;
use super::strategist::strict;
use super::supervisor::{Player, finish_stream};
use super::text::Unread;
use crate::ai::{ChatProvider, ChatRequest, Input, Part, Role, ToolDef, Turn};
use crate::util::media::ModelImage;

const PROMPT: &str = "You read the words in text boxes from a Game Boy game, letter for \
letter. Each picture is one text box, enlarged. Copy every row exactly as written: the same \
capital and small letters, punctuation and symbols, with one space between words. A tile \
that isn't a letter or symbol (a frame piece, an icon, a picture) is written as #. Answer \
by calling read.";

#[derive(Debug, Deserialize)]
struct Answer {
    boxes: Vec<Read>,
}

#[derive(Debug, Deserialize)]
struct Read {
    rows: Vec<String>,
}

/// What the model is told about a box: its rows and how many tiles each word has.
fn shape_line(n: usize, unread: &Unread) -> String {
    let rows: Vec<String> = unread
        .page
        .shape()
        .iter()
        .enumerate()
        .map(|(r, words)| {
            let sizes: Vec<String> = words.iter().map(usize::to_string).collect();
            format!(
                "row {}: {} word{} of {} tiles",
                r + 1,
                words.len(),
                if words.len() == 1 { "" } else { "s" },
                sizes.join(", ")
            )
        })
        .collect();
    format!("Box {n}: {}.", rows.join("; "))
}

/// Has the model read the boxes the player kept, names their letters, and gives the
/// words of the boxes that read now and the words on screen now. `None` when there were
/// no boxes, or when the model couldn't be asked (that's logged; playing goes on without
/// words).
pub async fn name_letters(
    provider: &dyn ChatProvider,
    player: &mut dyn Player,
) -> Option<(Vec<String>, String)> {
    let kept = player.unread().await.ok().filter(|u| !u.is_empty())?;
    let unread = fewest(&kept);
    let reads = match read_boxes(provider, &unread).await {
        Ok(reads) => reads,
        Err(e) => {
            tracing::warn!("Game Boy: naming letters failed: {e:#}");
            return None;
        }
    };
    let votes: Vec<_> = unread
        .iter()
        .zip(&reads)
        .flat_map(|(u, rows)| u.page.votes(rows))
        .collect();
    let pages: Vec<_> = kept.into_iter().map(|u| u.page).collect();
    player.name_letters(&votes, &pages).await.ok()
}

/// The fewest boxes that show every tile the kept ones do, in the order they came: the
/// same lines come up again and again while a box types and scrolls.
pub fn fewest(kept: &[Unread]) -> Vec<Unread> {
    let tiles = |u: &Unread| -> HashSet<TileKey> {
        u.page
            .lines
            .iter()
            .flatten()
            .flat_map(|w| w.keys.iter().copied())
            .collect()
    };
    let all: Vec<HashSet<TileKey>> = kept.iter().map(tiles).collect();
    let mut shown: HashSet<TileKey> = HashSet::new();
    let mut picked = Vec::new();
    loop {
        let best = (0..kept.len())
            .filter(|i| !picked.contains(i))
            .max_by_key(|&i| (all[i].difference(&shown).count(), std::cmp::Reverse(i)));
        match best {
            Some(i) if all[i].difference(&shown).count() > 0 => {
                shown.extend(&all[i]);
                picked.push(i);
            }
            _ => break,
        }
    }
    picked.sort_unstable();
    picked.into_iter().map(|i| kept[i].clone()).collect()
}

/// Each box's rows, as the model read them.
pub async fn read_boxes(
    provider: &dyn ChatProvider,
    unread: &[Unread],
) -> anyhow::Result<Vec<Vec<String>>> {
    let mut parts = Vec::new();
    for (i, u) in unread.iter().enumerate() {
        parts.push(Part::Text(shape_line(i + 1, u)));
        parts.push(Part::Image(ModelImage {
            mime: "image/png",
            data: u.picture.clone(),
        }));
    }
    let request = ChatRequest {
        system: PROMPT.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts,
        }]),
        tools: vec![ToolDef {
            name: "read",
            description: "The boxes' rows, in order: for each box, one string per row.",
            parameters: strict(json!({
                "boxes": {"type": "array", "items": strict(json!({
                    "rows": {"type": "array", "items": {"type": "string"}}
                }))}
            })),
        }],
        cache_key: "vivy:gameboy-letters".to_string(),
        job: "game_play",
    };
    let done = finish_stream(provider, request).await?;
    let call = done
        .tool_calls
        .into_iter()
        .find(|call| call.name == "read")
        .ok_or_else(|| anyhow::anyhow!("the model answered without calling read"))?;
    let answer: Answer = serde_json::from_value(call.args)?;
    Ok(answer.boxes.into_iter().map(|b| b.rows).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::text::{Page, Word};

    fn kept(rows: &[&[u64]]) -> Unread {
        Unread {
            page: Page {
                cols: (0, 19),
                rows: (12, 17),
                lines: rows
                    .iter()
                    .map(|keys| {
                        vec![Word {
                            col: 1,
                            row: 13,
                            keys: keys.to_vec(),
                        }]
                    })
                    .collect(),
            },
            picture: Vec::new(),
        }
    }

    #[test]
    fn the_fewest_boxes_show_every_tile() {
        let all = [
            kept(&[&[1, 2]]),
            kept(&[&[1, 2], &[3]]),
            kept(&[&[3]]),
            kept(&[&[4]]),
        ];
        let picked: Vec<Vec<usize>> = fewest(&all)
            .iter()
            .map(|u| u.page.shape()[..].concat())
            .collect();
        assert_eq!(picked, vec![vec![2, 1], vec![1]]);
    }
}
