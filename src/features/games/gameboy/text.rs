//! Reading the words on the screen exactly, from the font's tiles. Pure.
//!
//! Games draw text with tiles: one tile per letter (sometimes two letters, like Red's `'s`).
//! Once each letter's tile is labelled, every word on the screen reads exactly, with no
//! picture-reading model. Labels are keyed on the tile's pixels ([`TileKey`]), not its
//! number, since a game may load the font at different numbers in different places, and a
//! screen fading to black keeps the tiles the same while the colours change.
//!
//! Where labels come from: in Pokémon Red a font tile's number is its character code, so
//! [`Font::learn_from_numbers`] labels them from the code table. In other games a model
//! names them: a text box over her map is found by what changed since she last walked
//! ([`page_of`]), its letters are the tiles that aren't one colour all over, and the model
//! reads the box's picture row by row. Its reading is only taken where each word has as
//! many letters as the screen has tiles there ([`Page::votes`]), and each tile keeps the
//! name most readings gave it. The font is saved, so a game's letters are named once.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::screen::{COLS, Layer, ROWS, Screen, TileKey, is_blank};

/// What each font tile shows. A tile given two different labels is ambiguous and never
/// read as either.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Font {
    labels: HashMap<TileKey, String>,
    #[serde(default)]
    ambiguous: Vec<TileKey>,
    /// Names a model gave tiles, and how often, in the order first given: a tile reads as
    /// the name given most (the first, on a tie). `#` is a tile that isn't a letter.
    #[serde(default)]
    votes: HashMap<TileKey, Vec<(String, u32)>>,
}

impl Font {
    /// Labels a tile. Labelling it again the same way changes nothing; labelling it as
    /// something else makes it ambiguous.
    #[cfg_attr(not(test), allow(dead_code))]
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
        self.labels.get(&key).map(String::as_str).or_else(|| {
            let votes = self.votes.get(&key)?;
            let mut best: Option<&(String, u32)> = None;
            for vote in votes {
                if best.is_none_or(|b| vote.1 > b.1) {
                    best = Some(vote);
                }
            }
            best.map(|(name, _)| name.as_str())
        })
    }

    /// One reading's name for a tile (see [`Page::votes`]).
    pub fn vote(&mut self, key: TileKey, name: &str) {
        let votes = self.votes.entry(key).or_default();
        match votes.iter_mut().find(|(n, _)| n == name) {
            Some((_, count)) => *count += 1,
            None => votes.push((name.to_string(), 1)),
        }
    }

    /// The page's words, a line for each row with words, once every tile on it is named;
    /// tiles named `#` are left out. `None` while any isn't.
    pub fn read(&self, page: &Page) -> Option<String> {
        let mut lines = Vec::new();
        for line in &page.lines {
            let mut words = Vec::new();
            for word in line {
                let mut text = String::new();
                for &key in &word.keys {
                    text.push_str(self.get(key)?);
                }
                let text = text.replace('#', "");
                if !text.is_empty() {
                    words.push(text);
                }
            }
            if !words.is_empty() {
                lines.push(words.join(" "));
            }
        }
        Some(lines.join("\n"))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    /// Labels the screen's tiles from their numbers, for a game whose tile numbers are its
    /// character codes (`code` says what each shows). Only from a screen with a text box
    /// (`has_box`), since outside one the same numbers may hold other pictures.
    #[cfg_attr(not(test), allow(dead_code))]
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
    #[cfg_attr(not(test), allow(dead_code))]
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
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn words(&self, screen: &Screen) -> String {
        words_of(&self.rows(screen))
    }
}

/// A text box on the screen, to read with the font or have its letters named: its
/// letters as rows of words, a word being tiles with no blank one between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The box's first and last column and row on the screen.
    pub cols: (usize, usize),
    pub rows: (usize, usize),
    /// The rows with letters, top to bottom.
    pub lines: Vec<Vec<Word>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// Where its first tile is on the screen.
    pub col: usize,
    pub row: usize,
    pub keys: Vec<TileKey>,
}

/// Cells a text box covers at least.
const BOX_LEAST: usize = 8;

/// The text box on a screen, from which cells changed since she last walked (`changed`,
/// row by row): where they are, if together they fill most of a box (a box covers what's
/// under it; a few tiles changing here and there are the map moving). A frame around the
/// box (an edge with no blank tile) isn't letters.
pub fn page_of(screen: &Screen, changed: &[bool]) -> Option<Page> {
    let at = |i: usize| (i % COLS, i / COLS);
    let cells: Vec<(usize, usize)> = (0..COLS * ROWS).filter(|&i| changed[i]).map(at).collect();
    if cells.len() < BOX_LEAST {
        return None;
    }
    let (mut c0, mut c1) = (COLS, 0);
    let (mut r0, mut r1) = (ROWS, 0);
    for &(c, r) in &cells {
        (c0, c1, r0, r1) = (c0.min(c), c1.max(c), r0.min(r), r1.max(r));
    }
    let area = (c1 - c0 + 1) * (r1 - r0 + 1);
    if cells.len() * 10 < area * 6 {
        return None;
    }
    let blank = |c: usize, r: usize| {
        let cell = screen.cell(c, r);
        cell.layer == Layer::Off || is_blank(cell.key)
    };
    let full_row = |r: usize| (c0..=c1).all(|c| !blank(c, r));
    let full_col = |c: usize| (r0..=r1).all(|r| !blank(c, r));
    let page_cols = (c0, c1);
    let page_rows = (r0, r1);
    let (top, bottom, left, right) = (full_row(r0), full_row(r1), full_col(c0), full_col(c1));
    if r1 > r0 + 1 {
        r0 += usize::from(top);
        r1 -= usize::from(bottom);
    }
    if c1 > c0 + 1 {
        c0 += usize::from(left);
        c1 -= usize::from(right);
    }
    let lines: Vec<Vec<Word>> = (r0..=r1)
        .map(|row| {
            let mut words: Vec<Word> = Vec::new();
            let mut last_blank = true;
            for col in c0..=c1 {
                if blank(col, row) {
                    last_blank = true;
                    continue;
                }
                let key = screen.cell(col, row).key;
                match words.last_mut() {
                    Some(word) if !last_blank => word.keys.push(key),
                    _ => words.push(Word {
                        col,
                        row,
                        keys: vec![key],
                    }),
                }
                last_blank = false;
            }
            words
        })
        .filter(|words| !words.is_empty())
        .collect();
    (!lines.is_empty()).then_some(Page {
        cols: page_cols,
        rows: page_rows,
        lines,
    })
}

impl Page {
    /// Whether every tile of `other` is on this page too, in the same place: the same box
    /// typed further.
    pub fn holds(&self, other: &Page) -> bool {
        let tiles = |page: &Page| -> Vec<(usize, usize, TileKey)> {
            page.lines
                .iter()
                .flatten()
                .flat_map(|w| {
                    w.keys
                        .iter()
                        .enumerate()
                        .map(move |(i, &k)| (w.col + i, w.row, k))
                })
                .collect()
        };
        let mine = tiles(self);
        tiles(other).iter().all(|t| mine.contains(t))
    }

    /// How many tiles each word has, row by row: what a reading must fit.
    pub fn shape(&self) -> Vec<Vec<usize>> {
        self.lines
            .iter()
            .map(|line| line.iter().map(|w| w.keys.len()).collect())
            .collect()
    }

    /// The names a reading of the page gives its tiles: one string per row with words,
    /// words split by spaces. A row is only taken when it has as many words as the screen
    /// shows there and each word as many letters as it has tiles, so a misread length or
    /// a skipped word names nothing.
    pub fn votes(&self, rows: &[String]) -> Vec<(TileKey, String)> {
        let mut votes = Vec::new();
        if rows.len() != self.lines.len() {
            return votes;
        }
        for (line, row) in self.lines.iter().zip(rows) {
            let read: Vec<&str> = row.split_whitespace().collect();
            let fits = read.len() == line.len()
                && read
                    .iter()
                    .zip(line)
                    .all(|(text, word)| text.chars().count() == word.keys.len());
            if !fits {
                continue;
            }
            for (text, word) in read.iter().zip(line) {
                for (c, &key) in text.chars().zip(&word.keys) {
                    votes.push((key, c.to_string()));
                }
            }
        }
        votes
    }
}

/// The words of the pages a text box showed, as one text: a page caught half typed
/// gives way to the rest of it, and where the box scrolls a line at a time, the lines a
/// page shares with the one before are only kept once.
pub fn joined(pages: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for page in pages {
        let new: Vec<&str> = page.lines().collect();
        // The most lines at the end so far that the page starts with, the last of them
        // perhaps typed further now.
        let shared = (1..=new.len().min(lines.len()))
            .rev()
            .find(|&k| {
                let tail = &lines[lines.len() - k..];
                tail.iter().zip(&new).enumerate().all(|(i, (old, now))| {
                    if i + 1 == k {
                        now.starts_with(old.as_str())
                    } else {
                        old == now
                    }
                })
            })
            .unwrap_or(0);
        lines.truncate(lines.len() - shared);
        lines.extend(new.iter().map(|l| l.to_string()));
    }
    lines.join("\n")
}

/// A page with tiles the font hasn't named, and its picture for the model.
#[derive(Debug, Clone)]
pub struct Unread {
    pub page: Page,
    pub picture: Vec<u8>,
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

    /// The pinned fingerprint of an all-white tile (`screen.rs`).
    const BLANK: TileKey = 0x4dfa_4cff_d1f7_979f;

    /// A map of tiles (each its own picture) with a framed box over columns 1 to 8 and
    /// rows 2 to 5, "Hi yo" on its first row inside and nothing on the second.
    fn boxed() -> (Screen, Vec<bool>) {
        let mut screen = screen_of(&[]);
        for (i, cell) in screen.cells.iter_mut().enumerate() {
            cell.key = 5000 + i as u64;
        }
        let mut changed = vec![false; COLS * ROWS];
        for row in 2..=5 {
            for col in 1..=8 {
                let edge = row == 2 || row == 5 || col == 1 || col == 8;
                screen.cells[row * COLS + col].key = if edge { 900 } else { BLANK };
                screen.cells[row * COLS + col].index = 0x7f;
                changed[row * COLS + col] = true;
            }
        }
        for (col, key) in [(2, 1), (3, 2), (5, 3), (6, 4)] {
            screen.cells[3 * COLS + col].key = key;
        }
        (screen, changed)
    }

    #[test]
    fn a_text_box_is_found_without_its_frame() {
        let (screen, changed) = boxed();
        let page = page_of(&screen, &changed).unwrap();
        assert_eq!((page.cols, page.rows), ((1, 8), (2, 5)));
        assert_eq!(page.shape(), vec![vec![2, 2]]);
        assert_eq!((page.lines[0][1].col, page.lines[0][1].row), (5, 3));
        // A few tiles changing here and there aren't a box.
        let mut scattered = vec![false; COLS * ROWS];
        for i in [0, 19, 40, 100, 200, 300, 340, 359] {
            scattered[i] = true;
        }
        assert_eq!(page_of(&screen, &scattered), None);
    }

    #[test]
    fn a_reading_names_letters_only_where_it_fits() {
        let (screen, changed) = boxed();
        let page = page_of(&screen, &changed).unwrap();
        let mut font = Font::default();
        assert_eq!(font.read(&page), None);
        // Wrong lengths, a missing word, or a missing row name nothing.
        for wrong in [vec!["Hi you"], vec!["Hi"], vec![], vec!["Hi yo", "and"]] {
            let rows: Vec<String> = wrong.iter().map(|s| s.to_string()).collect();
            assert!(page.votes(&rows).is_empty(), "{wrong:?}");
        }
        for (key, name) in page.votes(&["Hi yo".to_string()]) {
            font.vote(key, &name);
        }
        assert_eq!(font.read(&page).as_deref(), Some("Hi yo"));
        // The name given most wins; a tie keeps the first.
        font.vote(4, "0");
        assert_eq!(font.get(4), Some("o"));
        font.vote(4, "0");
        assert_eq!(font.get(4), Some("0"));
        // Tiles that aren't letters are left out.
        font.vote(3, "#");
        font.vote(3, "#");
        assert_eq!(font.read(&page).as_deref(), Some("Hi 0"));
    }

    #[test]
    fn a_box_typed_further_holds_the_earlier_one() {
        let (mut screen, changed) = boxed();
        let early = page_of(&screen, &changed).unwrap();
        screen.cells[3 * COLS + 7].key = 6;
        let later = page_of(&screen, &changed).unwrap();
        assert!(later.holds(&early));
        assert!(!early.holds(&later));
    }

    #[test]
    fn pages_join_into_one_text() {
        // Link's Awakening scrolls a line at a time, and types a letter at a time.
        let pages = [
            "Follow the lane\nsouth to reach",
            "the beach where\nI found you.",
            "I found you.\nSince you washe",
            "Since you washed",
            "Since you washed\nashore, lots of",
            "ashore, lots of\nnasty monsters",
            "area, so be",
            "area, so be\ncareful, okay?",
        ];
        let pages: Vec<String> = pages.iter().map(|p| p.to_string()).collect();
        assert_eq!(
            joined(&pages),
            "Follow the lane\nsouth to reach\nthe beach where\nI found you.\n\
             Since you washed\nashore, lots of\nnasty monsters\narea, so be\n\
             careful, okay?"
        );
        // A box that shows a page at a time.
        let pages = ["Hello!".to_string(), "Bye.".to_string()];
        assert_eq!(joined(&pages), "Hello!\nBye.");
    }

    #[test]
    fn a_font_saves_and_loads() {
        let mut font = Font::default();
        font.label(u64::MAX, "é");
        font.label(3, "x");
        font.label(3, "y");
        font.vote(9, "q");
        let json = serde_json::to_string(&font).unwrap();
        let back: Font = serde_json::from_str(&json).unwrap();
        assert_eq!(back, font);
    }
}
