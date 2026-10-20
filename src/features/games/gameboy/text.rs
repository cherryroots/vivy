//! Reading the words on the screen exactly, from the font's tiles. Pure.
//!
//! Games draw text with tiles: one tile per letter (sometimes two letters, like Red's `'s`).
//! Once each letter's tile is labelled, every word on the screen reads exactly, with no
//! picture-reading model. Labels are keyed on the tile's pixels ([`TileKey`]), not its
//! number, since a game may load the font at different numbers in different places, and a
//! screen fading to black keeps the tiles the same while the colours change.
//!
//! Where labels come from: in Pokémon Red a font tile's number is its character code, so
//! [`Font::learn_from_numbers`] labels them from the code table. Other games will have a
//! model label each new tile once (a later stage); the font is saved, so that happens once
//! per game.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::screen::{COLS, Layer, ROWS, Screen, TileKey};

/// What each font tile shows. A tile given two different labels is ambiguous and never
/// read as either.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Font {
    labels: HashMap<TileKey, String>,
    #[serde(default)]
    ambiguous: Vec<TileKey>,
}

impl Font {
    /// Labels a tile. Labelling it again the same way changes nothing; labelling it as
    /// something else makes it ambiguous.
    pub fn label(&mut self, key: TileKey, text: &str) {
        if self.ambiguous.contains(&key) {
            return;
        }
        match self.labels.get(&key) {
            Some(old) if old != text => {
                self.labels.remove(&key);
                self.ambiguous.push(key);
            }
            Some(_) => {}
            None => {
                self.labels.insert(key, text.to_string());
            }
        }
    }

    pub fn get(&self, key: TileKey) -> Option<&str> {
        self.labels.get(&key).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }

    /// Labels the screen's tiles from their numbers, for a game whose tile numbers are its
    /// character codes (`code` says what each shows). Only from a screen with a text box
    /// (`has_box`), since outside one the same numbers may hold other pictures.
    pub fn learn_from_numbers(
        &mut self,
        screen: &Screen,
        code: impl Fn(u8) -> Option<&'static str>,
        has_box: bool,
    ) {
        if !has_box {
            return;
        }
        for cell in &screen.cells {
            if cell.layer == Layer::Off {
                continue;
            }
            if let Some(text) = code(cell.index) {
                self.label(cell.key, text);
            }
        }
    }

    /// The screen's 18 rows as text: labelled tiles as their letters, anything else as a
    /// space, with spaces at the ends trimmed.
    pub fn rows(&self, screen: &Screen) -> Vec<String> {
        (0..ROWS)
            .map(|row| {
                let line: String = (0..COLS)
                    .map(|col| {
                        let cell = screen.cell(col, row);
                        match cell.layer {
                            Layer::Off => " ",
                            _ => self.get(cell.key).unwrap_or(" "),
                        }
                    })
                    .collect();
                line.trim_end().to_string()
            })
            .collect()
    }

    /// The words on the screen (see [`words_of`]).
    pub fn words(&self, screen: &Screen) -> String {
        words_of(&self.rows(screen))
    }
}

/// The words in rows of screen text: rows with at least two letters or digits, with box
/// borders left out and runs of spaces closed up. Empty when there's no text.
pub fn words_of(rows: &[String]) -> String {
    rows.iter()
        .map(|row| {
            let plain: String = row
                .chars()
                .map(|c| {
                    if "┌─┐│└┘".contains(c) {
                        ' '
                    } else {
                        c
                    }
                })
                .collect();
            plain.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .filter(|row| row.chars().filter(|c| c.is_alphanumeric()).count() >= 2)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::emulator::{Registers, Video};
    use crate::features::games::gameboy::screen::Cell;

    /// A screen whose cells are the given tile numbers, each number its own picture.
    fn screen_of(rows: &[&[u8]]) -> Screen {
        let mut cells = vec![
            Cell {
                layer: Layer::Background,
                index: 0,
                key: 0
            };
            COLS * ROWS
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, &index) in row.iter().enumerate() {
                cells[r * COLS + c] = Cell {
                    layer: Layer::Window,
                    index,
                    key: 1000 + u64::from(index),
                };
            }
        }
        let video = Video {
            vram: Vec::new(),
            oam: Vec::new(),
            registers: Registers::default(),
            drawn: true,
            color: false,
        };
        let mut screen = Screen::read(&video);
        screen.cells = cells;
        screen
    }

    fn code(n: u8) -> Option<&'static str> {
        match n {
            1 => Some("H"),
            2 => Some("i"),
            3 => Some("!"),
            4 => Some("'s"),
            5 => Some("┌"),
            _ => None,
        }
    }

    #[test]
    fn learns_letters_from_a_text_box_and_reads_them() {
        let screen = screen_of(&[&[5, 1, 2, 3, 0, 1, 4]]);
        let mut font = Font::default();
        font.learn_from_numbers(&screen, code, false);
        assert_eq!(font.len(), 0, "learned outside a text box");
        font.learn_from_numbers(&screen, code, true);
        assert_eq!(font.len(), 5);
        assert_eq!(font.rows(&screen)[0], "┌Hi! H's");
        assert_eq!(font.words(&screen), "Hi! H's");
        // Rows that are only borders or empty aren't words.
        assert_eq!(font.words(&screen_of(&[&[5, 5, 5], &[0]])), "");
    }

    #[test]
    fn a_tile_with_two_labels_reads_as_neither() {
        let mut font = Font::default();
        font.label(7, "a");
        font.label(7, "a");
        assert_eq!(font.get(7), Some("a"));
        font.label(7, "o");
        assert_eq!(font.get(7), None);
        font.label(7, "a");
        assert_eq!(font.get(7), None);
    }

    #[test]
    fn a_font_saves_and_loads() {
        let mut font = Font::default();
        font.label(u64::MAX, "é");
        font.label(3, "x");
        font.label(3, "y");
        let json = serde_json::to_string(&font).unwrap();
        let back: Font = serde_json::from_str(&json).unwrap();
        assert_eq!(back, font);
    }
}
