//! Naming a game's letters: the supervisor's model reads a text box's picture once, and
//! code keeps what each letter's picture is called (`text.rs`), so the game's words read
//! exactly from then on, with no model. For games read from the screen (`any`).
//!
//! The model is told how many words each row has and how many tiles each word is, and
//! only a row that fits is taken, so a misread word names nothing.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::json;

use super::any::LETTERS_SCALE;
use super::screen::TileKey;
use super::strategist::strict;
use super::supervisor::{Player, finish_stream};
use super::text::{Unread, Word};
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
    let (bars, boxes): (Vec<Unread>, Vec<Unread>) = fewest(&kept).into_iter().partition(|u| u.bar);
    let mut votes = Vec::new();
    if !boxes.is_empty() {
        match read_boxes(provider, &boxes).await {
            Ok(reads) => votes.extend(
                boxes
                    .iter()
                    .zip(&reads)
                    .flat_map(|(u, rows)| u.page.votes(rows)),
            ),
            Err(e) => {
                tracing::warn!("Game Boy: naming letters failed: {e:#}");
                return None;
            }
        }
    }
    if !bars.is_empty() {
        match read_bars(provider, &bars).await {
            Ok(bar_votes) => votes.extend(bar_votes),
            Err(e) => tracing::warn!("Game Boy: naming a status bar's tiles failed: {e:#}"),
        }
    }
    // A status bar's words aren't words she read: its tiles are named, that's all.
    let pages: Vec<_> = kept
        .into_iter()
        .filter(|u| !u.bar)
        .map(|u| u.page)
        .collect();
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
        answer_now: false,
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

const BAR_PROMPT: &str = "You name the tiles of a Game Boy game's status bar (the strip \
at the edge of the screen with numbers, hearts, weapon names). Each picture is one group of \
tiles from it, enlarged, with how many tiles it has; the tiles are equal squares side by \
side. Write each group as exactly that many characters, one per tile, left to right: the \
letter, digit or symbol a tile shows whole, and # for anything else: a piece of a bracket \
or frame (even one that looks like a letter, like the corner of a bracket looking like L), \
a heart, an icon, part of a bar. Answer by calling read.";

/// Each word of each status bar on its own picture, for the model to name tile by tile:
/// a bar's tiles rarely line up as words of text (a tall bracket's halves sit on two
/// rows), so a reading of whole rows seldom fits.
pub async fn read_bars(
    provider: &dyn ChatProvider,
    bars: &[Unread],
) -> anyhow::Result<Vec<(TileKey, String)>> {
    let mut parts = Vec::new();
    let mut words = Vec::new();
    for bar in bars {
        let picture = image::load_from_memory(&bar.picture)?.to_rgb8();
        let tile = 8 * LETTERS_SCALE;
        for word in bar.page.lines.iter().flatten() {
            let x = (word.col - bar.page.cols.0) as u32 * tile;
            let y = (word.row - bar.page.rows.0) as u32 * tile;
            let w = word.keys.len() as u32 * tile;
            if x + w > picture.width() || y + tile > picture.height() {
                continue;
            }
            let part = image::imageops::crop_imm(&picture, x, y, w, tile).to_image();
            let mut png = std::io::Cursor::new(Vec::new());
            part.write_to(&mut png, image::ImageFormat::Png)?;
            words.push(word);
            parts.push(Part::Text(format!(
                "Group {} ({} tile{}):",
                words.len(),
                word.keys.len(),
                if word.keys.len() == 1 { "" } else { "s" }
            )));
            parts.push(Part::Image(ModelImage {
                mime: "image/png",
                data: png.into_inner(),
            }));
        }
    }
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let request = ChatRequest {
        system: BAR_PROMPT.to_string(),
        input: Input::Full(vec![Turn {
            role: Role::User,
            parts,
        }]),
        tools: vec![ToolDef {
            name: "read",
            description: "The groups' tiles, in order: one string per group, a character \
                per tile.",
            parameters: strict(json!({
                "groups": {"type": "array", "items": {"type": "string"}}
            })),
        }],
        cache_key: "vivy:gameboy-bar".to_string(),
        job: "game_play",
        answer_now: false,
    };
    let done = finish_stream(provider, request).await?;
    let call = done
        .tool_calls
        .into_iter()
        .find(|call| call.name == "read")
        .ok_or_else(|| anyhow::anyhow!("the model answered without calling read"))?;
    let groups: Vec<String> = serde_json::from_value(call.args["groups"].clone())?;
    Ok(bar_votes(&words, &groups))
}

/// The names a reading of a bar's groups gives its tiles: only a group read as exactly as
/// many characters as it has tiles.
fn bar_votes(words: &[&Word], groups: &[String]) -> Vec<(TileKey, String)> {
    words
        .iter()
        .zip(groups)
        .filter(|(word, read)| read.chars().count() == word.keys.len())
        .flat_map(|(word, read)| {
            word.keys
                .iter()
                .zip(read.chars())
                .map(|(&key, c)| (key, c.to_string()))
        })
        .collect()
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
            bar: false,
        }
    }

    #[test]
    fn a_bars_groups_name_their_tiles_only_when_the_count_fits() {
        let word = |keys: &[u64]| Word {
            col: 0,
            row: 0,
            keys: keys.to_vec(),
        };
        let (a, b) = (word(&[1, 2, 3]), word(&[4]));
        let votes = bar_votes(&[&a, &b], &["#00".to_string(), "P2".to_string()]);
        assert_eq!(
            votes,
            [
                (1, "#".to_string()),
                (2, "0".to_string()),
                (3, "0".to_string())
            ]
        );
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
