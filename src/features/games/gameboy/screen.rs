//! Reading the screen from video memory instead of pixels. Pure: it works on the
//! [`Video`] the emulator read as the frame's V-Blank started.
//!
//! A Game Boy screen is a grid of 20×18 tiles of 8×8 pixels, from two layers: the
//! background (a 32×32 tile map the game scrolls) and the window (a second map on top,
//! where most games draw menus and text boxes), plus up to 40 sprites. The game picks,
//! for each layer, which of the two maps it uses and how tile numbers find their pixels
//! (LCDC bits 3, 4 and 6). Reading all that gives each cell of the grid exactly, with no
//! guessing from pixels, which is how the emulator test of 19 October matched Pokémon Red's
//! screen pixel for pixel.
//!
//! A tile's number alone doesn't say what it looks like (games load different pictures into
//! the same numbers), so every tile also gets a [`TileKey`]: a fingerprint of its pixels.
//! Text, tile kinds and everything learned about tiles are keyed on that.

use super::emulator::{Registers, Video};

pub const COLS: usize = 20;
pub const ROWS: usize = 18;

/// A fingerprint of a tile's 8×8 pixels (and, on a Game Boy Color, its flips). The same
/// picture always gives the same key, in any game and after any update: it's FNV-1a, not
/// Rust's own hasher, which may change.
pub type TileKey = u64;

/// Which layer a cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    Background,
    Window,
    /// The background is switched off (LCDC bit 0 on the original Game Boy): blank.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub layer: Layer,
    /// The tile's number in its map.
    pub index: u8,
    pub key: TileKey,
}

/// A sprite on the screen: its top left corner in screen pixels (it may hang off the
/// edges), its tile and flags (OAM byte 3: flips, palette, behind the background).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sprite {
    pub x: i32,
    pub y: i32,
    pub tile: u8,
    pub flags: u8,
    /// Its picture's fingerprint (both tiles of a tall 8×16 sprite).
    pub key: TileKey,
    /// 8 or 16.
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// 20×18 cells, row by row.
    pub cells: Vec<Cell>,
    pub sprites: Vec<Sprite>,
    pub registers: Registers,
    /// Whether the screen was drawn at all (it's off between areas in many games).
    pub drawn: bool,
    /// Whether the background sits exactly on the 8-pixel grid. While it scrolls between
    /// tiles each cell straddles two, and the grid shows the one under its top left pixel.
    pub aligned: bool,
    /// Pixels right and down from each cell's top left to where its background tile
    /// starts: none, or with `read_steady` whatever the scroll leaves over.
    pub phase: (i32, i32),
}

impl Screen {
    pub fn read(video: &Video) -> Screen {
        Screen::read_with(video, (0, 0))
    }

    /// The screen with each background cell a whole tile of the map, wherever the scroll
    /// stops (games that scroll a pixel at a time and stop anywhere): a cell is the tile
    /// that starts in it. The window doesn't scroll, so its cells are read as usual.
    pub fn read_steady(video: &Video) -> Screen {
        let r = video.registers;
        let phase = (
            (8 - i32::from(r.scx) % 8) % 8,
            (8 - i32::from(r.scy) % 8) % 8,
        );
        let mut screen = Screen::read_with(video, phase);
        screen.aligned = true;
        screen
    }

    fn read_with(video: &Video, phase: (i32, i32)) -> Screen {
        let r = video.registers;
        let drawn = video.drawn && r.lcdc & 0x80 != 0 && !video.vram.is_empty();
        let mut cells = Vec::with_capacity(COLS * ROWS);
        for row in 0..ROWS {
            for col in 0..COLS {
                cells.push(if drawn {
                    let (x, y) = (col * 8, row * 8);
                    if in_window(&r, x, y) {
                        read_cell(video, x, y, true)
                    } else {
                        read_cell(video, x + phase.0 as usize, y + phase.1 as usize, false)
                    }
                } else {
                    Cell {
                        layer: Layer::Off,
                        index: 0,
                        key: 0,
                    }
                });
            }
        }
        Screen {
            cells,
            sprites: if drawn { sprites(video) } else { Vec::new() },
            registers: r,
            drawn,
            aligned: r.scx.is_multiple_of(8) && r.scy.is_multiple_of(8),
            phase,
        }
    }

    /// The cell at a column (0 to 19) and row (0 to 17).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cell(&self, col: usize, row: usize) -> Cell {
        self.cells[row * COLS + col]
    }

    /// How many cells show a different tile than in `other`. For the novelty score of a
    /// later stage.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cells_changed(&self, other: &Screen) -> usize {
        self.cells
            .iter()
            .zip(&other.cells)
            .filter(|(a, b)| a.key != b.key || a.layer != b.layer)
            .count()
    }
}

/// Whether the window covers this screen pixel.
fn in_window(r: &Registers, x: usize, y: usize) -> bool {
    r.lcdc & 0x20 != 0 && r.wx <= 166 && r.wy <= 143 && y >= r.wy as usize && x + 7 >= r.wx as usize
}

/// The cell whose top left pixel is at screen pixel `(x, y)`, in the window or the
/// background.
fn read_cell(video: &Video, x: usize, y: usize, window: bool) -> Cell {
    let r = &video.registers;
    // The original Game Boy blanks both layers when bit 0 is off. (On a Game Boy Color it
    // only gives sprites priority, so the layers still show.)
    if r.lcdc & 0x01 == 0 && !video.color {
        return Cell {
            layer: Layer::Off,
            index: 0,
            key: 0,
        };
    }
    let (map, mx, my, layer) = if window {
        let map = if r.lcdc & 0x40 != 0 { 0x1c00 } else { 0x1800 };
        (map, x + 7 - r.wx as usize, y - r.wy as usize, Layer::Window)
    } else {
        let map = if r.lcdc & 0x08 != 0 { 0x1c00 } else { 0x1800 };
        // The background map is 256×256 pixels and wraps around.
        let mx = (x + r.scx as usize) & 255;
        let my = (y + r.scy as usize) & 255;
        (map, mx, my, Layer::Background)
    };
    let at = map + (my / 8) * 32 + mx / 8;
    let index = video.vram[at];
    // On a Game Boy Color, the map's second bank says which bank the tile is in and how
    // it's flipped.
    let attributes = if video.color {
        video.vram[0x2000 + at]
    } else {
        0
    };
    let bank = if attributes & 0x08 != 0 { 0x2000 } else { 0 };
    let offset = bank + tile_offset(r.lcdc, index);
    let pixels = &video.vram[offset..offset + 16];
    Cell {
        layer,
        index,
        key: fingerprint(pixels, attributes & 0x60),
    }
}

/// Where tile `index`'s 16 bytes start in video memory. With LCDC bit 4 set, tiles 0 to
/// 255 run from 0x8000; without it, 0 to 127 run from 0x9000 and 128 to 255 (as -128 to -1)
/// from 0x8800.
pub fn tile_offset(lcdc: u8, index: u8) -> usize {
    if lcdc & 0x10 != 0 {
        index as usize * 16
    } else {
        (0x1000 + (index as i8 as i32) * 16) as usize
    }
}

/// The colour number (0 to 3) of a pixel of a tile's 16 bytes: two bits from two bytes.
#[cfg_attr(not(test), allow(dead_code))]
pub fn tile_pixel(tile: &[u8], x: usize, y: usize) -> u8 {
    let bit = 7 - x;
    ((tile[y * 2] >> bit) & 1) | (((tile[y * 2 + 1] >> bit) & 1) << 1)
}

/// The visible sprites, in OAM order.
fn sprites(video: &Video) -> Vec<Sprite> {
    let r = &video.registers;
    if r.lcdc & 0x02 == 0 {
        return Vec::new();
    }
    let height = if r.lcdc & 0x04 != 0 { 16 } else { 8 };
    video
        .oam
        .chunks_exact(4)
        .filter_map(|s| {
            let (y, x) = (s[0] as i32 - 16, s[1] as i32 - 8);
            if y <= -height || y >= 144 || x <= -8 || x >= 160 {
                return None;
            }
            let mut tile = s[2];
            if height == 16 {
                tile &= 0xfe;
            }
            let bank = if video.color && s[3] & 0x08 != 0 {
                0x2000
            } else {
                0
            };
            let start = bank + tile as usize * 16;
            let end = start + height as usize * 2;
            Some(Sprite {
                x,
                y,
                tile,
                flags: s[3],
                key: fingerprint(&video.vram[start..end], s[3] & 0x60),
                height,
            })
        })
        .collect()
}

/// FNV-1a over the tile's bytes and its flips.
fn fingerprint(bytes: &[u8], flips: u8) -> TileKey {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes.iter().chain(std::iter::once(&flips)) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::features::games::gameboy::emulator::{Button, Emulator, HEIGHT, WIDTH};
    use crate::features::games::gameboy::test_rom;

    /// Draws the background and window back from the cells, as shades 0 (white) to 3
    /// (black), wherever no sprite is drawn on top. `None` marks pixels a sprite covers.
    /// Only for an aligned screen on the original Game Boy.
    pub fn redraw(video: &Video, screen: &Screen) -> Vec<Option<u8>> {
        let r = &video.registers;
        let mut covered = vec![false; WIDTH * HEIGHT];
        for s in &screen.sprites {
            for y in s.y.max(0)..(s.y + s.height).min(HEIGHT as i32) {
                for x in s.x.max(0)..(s.x + 8).min(WIDTH as i32) {
                    covered[y as usize * WIDTH + x as usize] = true;
                }
            }
        }
        let mut out = Vec::with_capacity(WIDTH * HEIGHT);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if covered[y * WIDTH + x] {
                    out.push(None);
                    continue;
                }
                let cell = screen.cell(x / 8, y / 8);
                let shade = match cell.layer {
                    Layer::Off => 0,
                    _ => {
                        let at = tile_offset(r.lcdc, cell.index);
                        let colour = tile_pixel(&video.vram[at..at + 16], x % 8, y % 8);
                        (r.bgp >> (colour * 2)) & 3
                    }
                };
                out.push(Some(shade));
            }
        }
        out
    }

    /// The emulator's screen as shades 0 to 3 (it draws them as 255, 192, 96 and 0).
    pub fn shades(rgb: &[u8]) -> Vec<u8> {
        rgb.chunks_exact(3)
            .map(|p| match p[0] {
                255 => 0,
                192 => 1,
                96 => 2,
                _ => 3,
            })
            .collect()
    }

    /// Pixels where the redrawn screen and the real one differ.
    pub fn mismatches(emu: &mut Emulator) -> usize {
        let video = emu.video().clone();
        let screen = Screen::read(&video);
        let real = shades(&emu.screen());
        redraw(&video, &screen)
            .iter()
            .zip(&real)
            .filter(|(drawn, real)| drawn.is_some_and(|d| d != **real))
            .count()
    }

    #[test]
    fn reads_the_test_roms_tiles() {
        let mut emu = Emulator::new(&test_rom::build()).unwrap();
        emu.run(300);
        let screen = Screen::read(emu.video());
        assert!(screen.drawn && screen.aligned);
        // Tile 1 (black) in the corner, tile 0 (white) everywhere else.
        assert_eq!(screen.cell(0, 0).index, 1);
        assert_eq!(screen.cell(1, 0).index, 0);
        assert_eq!(screen.cell(19, 17).index, 0);
        assert_ne!(screen.cell(0, 0).key, screen.cell(1, 0).key);
        assert_eq!(screen.cell(1, 0).key, screen.cell(5, 5).key);
        assert_eq!(mismatches(&mut emu), 0);
    }

    #[test]
    fn follows_the_scroll() {
        let mut emu = Emulator::new(&test_rom::build()).unwrap();
        emu.run(300);
        let still = Screen::read(emu.video());
        emu.hold(&[Button::Right]);
        // The ROM scrolls a pixel a frame: after a few frames the screen is between tiles.
        emu.run(3);
        let moving = Screen::read(emu.video());
        assert!(!moving.aligned);
        // Eight pixels in all: the black tile has gone off the left edge.
        emu.run(5);
        emu.release();
        emu.run(1);
        let moved = Screen::read(emu.video());
        assert!(moved.aligned);
        assert_eq!(moved.registers.scx % 8, 0);
        assert_eq!(moved.cell(0, 0).index, 0);
        assert_eq!(moved.cells_changed(&still), 1);
        assert_eq!(mismatches(&mut emu), 0);
    }

    #[test]
    fn signed_tile_numbers_and_the_window() {
        let mut vram = vec![0u8; 0x2000];
        // Tile -1 (0x8FF0 in signed mode) is solid colour 3; tile 0 (0x9000) is colour 0.
        vram[0x0ff0..0x1000].fill(0xff);
        // Background map 0x9800: tile 0xff at (0, 0). Window map 0x9C00: tile 0xff at (0, 0).
        vram[0x1800] = 0xff;
        vram[0x1c00] = 0xff;
        let video = Video {
            vram,
            oam: vec![0; 160],
            registers: Registers {
                // On, window on with map 0x9C00, signed tiles, background map 0x9800.
                lcdc: 0x80 | 0x40 | 0x20 | 0x01,
                wx: 7 + 80,
                wy: 72,
                bgp: 0xe4,
                ..Registers::default()
            },
            drawn: true,
            color: false,
        };
        let screen = Screen::read(&video);
        assert_eq!(screen.cell(0, 0).index, 0xff);
        assert_eq!(screen.cell(0, 0).layer, Layer::Background);
        // The window starts at screen pixel (80, 72): cell (10, 9) is its top left.
        assert_eq!(screen.cell(10, 9).layer, Layer::Window);
        assert_eq!(screen.cell(10, 9).index, 0xff);
        assert_eq!(screen.cell(9, 9).layer, Layer::Background);
        assert_eq!(screen.cell(11, 9).index, 0);
        assert_eq!(screen.cell(0, 0).key, screen.cell(10, 9).key);
    }

    #[test]
    fn a_screen_that_is_off_has_no_tiles() {
        let mut emu = Emulator::new(&test_rom::build()).unwrap();
        emu.run(300);
        let mut video = emu.video().clone();
        video.drawn = false;
        let screen = Screen::read(&video);
        assert!(!screen.drawn);
        assert!(screen.cells.iter().all(|c| c.layer == Layer::Off));
    }

    #[test]
    fn fingerprints_are_stable() {
        // Pinned, since saved fonts and tile notes are keyed on it.
        assert_eq!(fingerprint(&[0; 16], 0), 0x4dfa_4cff_d1f7_979f);
        assert_ne!(fingerprint(&[0; 16], 0x20), fingerprint(&[0; 16], 0));
    }
}
