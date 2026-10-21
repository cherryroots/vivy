//! A second game for the tests: three rooms to walk around, joined by doors, so the look
//! with no memory map (`any/`) can be tried on a game that isn't Pokémon Red. Built byte
//! by byte here, like `test_rom.rs`, with a small assembler for the jumps.
//!
//! It plays like Red, but keeps everything somewhere else and in other ways:
//!
//! - She stands in the middle of the screen, and a step scrolls the screen 16 pixels, two
//!   a frame. Walls stop her; nothing else does.
//! - Her x is kept in pixels at `0xD010` (16 per step), her y in steps at `0xD011`.
//! - The room's number is at `0xC0A0`: rooms are `0x21`, `0x35` and `0x4A`. The room she
//!   came from is at `0xC0A1`, and the room's music at `0xC0A2` (the first two rooms
//!   share it), to catch an area finder that picks the wrong byte.
//! - Stepping onto a door fades the screen to black, draws the next room, and fades back.
//! - A frame counter (`0xC001`) and a step counter (`0xC006`) change all the time.

use std::collections::HashMap;

/// Where things are kept.
pub const X_PIXELS: u16 = 0xd010;
pub const Y: u16 = 0xd011;
pub const ROOM: u16 = 0xc0a0;
const FRAME: u16 = 0xc001;
const PAD: u16 = 0xc002;
const MOVING: u16 = 0xc003;
const DX: u16 = 0xc004;
const DY: u16 = 0xc005;
const STEPS: u16 = 0xc006;
const LAST_ROOM: u16 = 0xc0a1;
const MUSIC: u16 = 0xc0a2;

/// Hardware registers, as `ldh` offsets.
const P1: u8 = 0x00;
const LCDC: u8 = 0x40;
const SCY: u8 = 0x42;
const SCX: u8 = 0x43;
const LY: u8 = 0x44;
const BGP: u8 = 0x47;
const OBP0: u8 = 0x48;

/// Screen on: background and sprites on, tiles at 0x8000, map at 0x9800.
const SCREEN_ON: u8 = 0x93;

/// Where the data goes in the ROM. Each room's steps are at its number times 256, so its
/// number is the high byte of its address.
const TILES: u16 = 0x1000;
const PLAYER: u16 = 0x1200;
const DOORS: u16 = 0x5000;

/// The kinds of step, and their pictures: four tiles each, the kind times four on.
const WALL: u8 = 3;
const DOOR_KIND: u8 = 4;
const KINDS: usize = 5;
const PLAYER_TILE: u8 = (KINDS * 4) as u8;

/// The rooms: their numbers, floor kind and steps. `#` is a wall, `.` floor, and a digit
/// a door, by its number in `DOOR_TO`.
pub const ROOMS: [(u8, u8, [&str; 12]); 3] = [
    (
        0x21,
        0,
        [
            "################",
            "#......#.......#",
            "#......#.......#",
            "#..........##..#",
            "#..............0",
            "#......#.......#",
            "####.###.......#",
            "#..............#",
            "#.....##.......#",
            "#..............#",
            "################",
            "################",
        ],
    ),
    (
        0x35,
        1,
        [
            "################",
            "#..........#####",
            "#..........#####",
            "#..............#",
            "1..............#",
            "#.....##.......#",
            "#.....##.......#",
            "#..............#",
            "#..............#",
            "#..............#",
            "#..............#",
            "########2#######",
        ],
    ),
    (
        0x4a,
        2,
        [
            "#######3########",
            "#..............#",
            "#..#....#......#",
            "#..#....#......#",
            "#..............#",
            "#.......####...#",
            "#..............#",
            "################",
            "################",
            "################",
            "################",
            "################",
        ],
    ),
];

/// Where each door leads: the room, x and y, and the room's music.
const DOOR_TO: [[u8; 4]; 4] = [
    [0x35, 1, 4, 7],
    [0x21, 14, 4, 7],
    [0x4a, 7, 1, 3],
    [0x35, 8, 10, 7],
];

/// Where she starts: the first room, at (3, 3).
const START: (u8, u8, u8) = (0x21, 3, 3);

/// The kind of a room's step, as the game keeps it: a door is 0x80 and its number.
pub fn step_byte(room: usize, (x, y): (usize, usize)) -> u8 {
    let (_, floor, rows) = ROOMS[room];
    match rows.get(y).and_then(|row| row.as_bytes().get(x)) {
        Some(b'.') => floor,
        Some(&c) if c.is_ascii_digit() => 0x80 + (c - b'0'),
        _ => WALL,
    }
}

/// A program with labels, for jumps and calls to code further on.
struct Asm {
    base: u16,
    code: Vec<u8>,
    labels: HashMap<&'static str, u16>,
    /// Where a label's address goes: two bytes, or one relative to the next instruction.
    wanted: Vec<(usize, &'static str, bool)>,
}

impl Asm {
    fn new(base: u16) -> Asm {
        Asm {
            base,
            code: Vec::new(),
            labels: HashMap::new(),
            wanted: Vec::new(),
        }
    }

    fn op(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    /// An instruction with an address after it: `ld a, (nn)`, `ld hl, nn` and the like.
    fn op16(&mut self, opcode: u8, address: u16) {
        self.code.push(opcode);
        self.code.extend_from_slice(&address.to_le_bytes());
    }

    fn label(&mut self, name: &'static str) {
        let at = self.base + self.code.len() as u16;
        assert!(self.labels.insert(name, at).is_none(), "{name} twice");
    }

    /// `jr` (0x18), `jr nz` (0x20), `jr z` (0x28) to a label.
    fn jr(&mut self, opcode: u8, name: &'static str) {
        self.code.push(opcode);
        self.wanted.push((self.code.len(), name, true));
        self.code.push(0);
    }

    /// `jp` (0xc3) or `call` (0xcd) to a label.
    fn jp(&mut self, opcode: u8, name: &'static str) {
        self.code.push(opcode);
        self.wanted.push((self.code.len(), name, false));
        self.code.extend_from_slice(&[0, 0]);
    }

    fn call(&mut self, name: &'static str) {
        self.jp(0xcd, name);
    }

    fn finish(mut self) -> Vec<u8> {
        for &(at, name, relative) in &self.wanted {
            let target = self.labels[name];
            if relative {
                let next = i32::from(self.base) + at as i32 + 1;
                let by = i32::from(target) - next;
                assert!((-128..128).contains(&by), "{name} is too far for jr");
                self.code[at] = by as i8 as u8;
            } else {
                self.code[at..at + 2].copy_from_slice(&target.to_le_bytes());
            }
        }
        self.code
    }
}

const NZ: u8 = 0x20;
const Z: u8 = 0x28;
const ALWAYS: u8 = 0x18;

fn program() -> Vec<u8> {
    let mut a = Asm::new(0x150);
    a.label("start");
    a.op(&[0xf3]); // di
    a.op16(0x31, 0xfffe); // ld sp, $fffe
    a.call("wait_vblank");
    a.op(&[0xaf, 0xe0, LCDC]); // screen off
    // The tiles, into 0x8000.
    a.op16(0x21, 0x8000); // ld hl, $8000
    a.op16(0x11, TILES); // ld de, TILES
    a.op16(0x01, ((KINDS * 4 + 1) * 16) as u16); // ld bc, length
    a.label("copy_tiles");
    a.op(&[0x1a, 0x22, 0x13, 0x0b, 0x78, 0xb1]); // ld a, (de); ld (hl+), a; inc de; dec bc; ld a, b; or c
    a.jr(NZ, "copy_tiles");
    // No sprites but hers: clear the sprite table, then copy her four in.
    a.op16(0x21, 0xfe00); // ld hl, $fe00
    a.op(&[0x06, 160, 0xaf]); // ld b, 160; xor a
    a.label("clear_sprites");
    a.op(&[0x22, 0x05]); // ld (hl+), a; dec b
    a.jr(NZ, "clear_sprites");
    a.op16(0x21, 0xfe00); // ld hl, $fe00
    a.op16(0x11, PLAYER); // ld de, PLAYER
    a.op(&[0x06, 16]); // ld b, 16
    a.label("copy_player");
    a.op(&[0x1a, 0x22, 0x13, 0x05]); // ld a, (de); ld (hl+), a; inc de; dec b
    a.jr(NZ, "copy_player");
    // The first room.
    a.op(&[0x3e, START.0]); // ld a, room
    a.op16(0xea, ROOM); // ld (ROOM), a
    a.op(&[0x3e, START.1 * 16]); // ld a, x in pixels
    a.op16(0xea, X_PIXELS);
    a.op(&[0x3e, START.2]); // ld a, y
    a.op16(0xea, Y);
    a.op(&[0x3e, 7]); // ld a, 7: the first room's music
    a.op16(0xea, MUSIC);
    a.op(&[0xaf]); // xor a
    for address in [LAST_ROOM, MOVING, DX, DY, STEPS, FRAME, PAD] {
        a.op16(0xea, address); // ld (address), a
    }
    a.call("draw_room");
    a.call("set_scroll");
    a.op(&[0x3e, 0xe4, 0xe0, BGP, 0xe0, OBP0]); // palettes: white, light, dark, black
    a.op(&[0x3e, SCREEN_ON, 0xe0, LCDC]);

    // Every frame, at the start of V-Blank.
    a.label("main");
    a.call("wait_vblank");
    a.op16(0x21, FRAME); // ld hl, FRAME
    a.op(&[0x34]); // inc (hl)
    a.call("read_pad");
    a.op16(0xfa, MOVING); // ld a, (MOVING)
    a.op(&[0xb7]); // or a
    a.jr(Z, "idle");
    // Mid-step: scroll two pixels the way she's going.
    a.op(&[0x3d]); // dec a
    a.op16(0xea, MOVING);
    a.op16(0xfa, DX);
    a.op(&[0x47, 0xf0, SCX, 0x80, 0xe0, SCX]); // ld b, a; ldh a, (SCX); add a, b; ldh (SCX), a
    a.op16(0xfa, DY);
    a.op(&[0x47, 0xf0, SCY, 0x80, 0xe0, SCY]);
    a.op16(0xfa, MOVING);
    a.op(&[0xb7]); // or a
    a.jr(NZ, "next");
    a.call("check_door");
    a.jr(ALWAYS, "next");
    // Standing: a direction held starts a step (b: x by, c: y by).
    a.label("idle");
    a.op16(0xfa, PAD);
    a.op16(0x01, 0x0100); // ld bc: right
    a.op(&[0xcb, 0x67]); // bit 4, a
    a.jr(NZ, "go");
    a.op16(0x01, 0xff00); // left
    a.op(&[0xcb, 0x6f]); // bit 5, a
    a.jr(NZ, "go");
    a.op16(0x01, 0x00ff); // up
    a.op(&[0xcb, 0x77]); // bit 6, a
    a.jr(NZ, "go");
    a.op16(0x01, 0x0001); // down
    a.op(&[0xcb, 0x7f]); // bit 7, a
    a.jr(NZ, "go");
    a.jr(ALWAYS, "next");
    a.label("go");
    a.call("try_step");
    a.label("next");
    a.call("wait_after_vblank");
    a.jp(0xc3, "main");

    // Waits for the start of V-Blank, then for it to pass.
    a.label("wait_vblank");
    a.op(&[0xf0, LY, 0xfe, 144]); // ldh a, (LY); cp 144
    a.jr(NZ, "wait_vblank");
    a.op(&[0xc9]); // ret
    a.label("wait_after_vblank");
    a.op(&[0xf0, LY, 0xfe, 144]);
    a.jr(Z, "wait_after_vblank");
    a.op(&[0xc9]);
    // Waits b frames.
    a.label("wait_frames");
    a.call("wait_vblank");
    a.call("wait_after_vblank");
    a.op(&[0x05]); // dec b
    a.jr(NZ, "wait_frames");
    a.op(&[0xc9]);

    // The joypad into PAD: Right, Left, Up, Down in bits 4 to 7, buttons in 0 to 3.
    a.label("read_pad");
    a.op(&[0x3e, 0x20, 0xe0, P1, 0xf0, P1, 0xf0, P1]); // the d-pad
    a.op(&[0x2f, 0xe6, 0x0f, 0xcb, 0x37, 0x47]); // cpl; and $0f; swap a; ld b, a
    a.op(&[0x3e, 0x10, 0xe0, P1, 0xf0, P1, 0xf0, P1]); // the buttons
    a.op(&[0x2f, 0xe6, 0x0f, 0xb0]); // cpl; and $0f; or b
    a.op16(0xea, PAD);
    a.op(&[0x3e, 0x30, 0xe0, P1, 0xc9]); // let go of both; ret

    // hl: the room's step at her x + b, y + c.
    a.label("step_at");
    a.op16(0xfa, X_PIXELS);
    a.op(&[0xcb, 0x37, 0xe6, 0x0f, 0x80, 0xe6, 0x0f, 0x5f]); // swap a; and $0f; add a, b; and $0f; ld e, a
    a.op16(0xfa, Y);
    a.op(&[0x81, 0xe6, 0x0f, 0xcb, 0x37, 0xb3, 0x6f]); // add a, c; and $0f; swap a; or e; ld l, a
    a.op16(0xfa, ROOM);
    a.op(&[0x67, 0xc9]); // ld h, a; ret

    // A step by b, c, unless it's into a wall.
    a.label("try_step");
    a.call("step_at");
    a.op(&[0x7e, 0xfe, WALL, 0xc8]); // ld a, (hl); cp WALL; ret z
    a.op(&[0x78, 0xcb, 0x37, 0xe6, 0xf0, 0x5f]); // ld a, b; swap a; and $f0; ld e, a
    a.op16(0xfa, X_PIXELS);
    a.op(&[0x83]); // add a, e
    a.op16(0xea, X_PIXELS);
    a.op16(0xfa, Y);
    a.op(&[0x81]); // add a, c
    a.op16(0xea, Y);
    a.op(&[0x78, 0x87]); // ld a, b; add a, a
    a.op16(0xea, DX);
    a.op(&[0x79, 0x87]); // ld a, c; add a, a
    a.op16(0xea, DY);
    a.op(&[0x3e, 8]); // eight frames of two pixels
    a.op16(0xea, MOVING);
    a.op16(0x21, STEPS);
    a.op(&[0x34, 0xc9]); // inc (hl); ret

    // At the end of a step: on a door, go where it leads.
    a.label("check_door");
    a.op(&[0x01, 0, 0]); // ld bc, 0
    a.call("step_at");
    a.op(&[0x7e, 0xcb, 0x7f, 0xc8]); // ld a, (hl); bit 7, a; ret z
    a.op(&[0xe6, 0x7f, 0x87, 0x87, 0x6f, 0x26, (DOORS >> 8) as u8]); // its line in DOORS
    a.op(&[0x3e, 0xff, 0xe0, BGP]); // all black
    a.op(&[0xe5, 0x06, 20]); // push hl; ld b, 20
    a.call("wait_frames");
    a.op(&[0xe1]); // pop hl
    a.op16(0xfa, ROOM);
    a.op16(0xea, LAST_ROOM);
    a.op(&[0x2a]); // ld a, (hl+)
    a.op16(0xea, ROOM);
    a.op(&[0x2a, 0xcb, 0x37]); // ld a, (hl+); swap a
    a.op16(0xea, X_PIXELS);
    a.op(&[0x2a]);
    a.op16(0xea, Y);
    a.op(&[0x2a]);
    a.op16(0xea, MUSIC);
    a.call("wait_vblank");
    a.op(&[0xaf, 0xe0, LCDC]); // screen off
    a.call("draw_room");
    a.call("set_scroll");
    a.op(&[0x3e, SCREEN_ON, 0xe0, LCDC]);
    a.op(&[0x06, 20]);
    a.call("wait_frames");
    a.op(&[0x3e, 0xe4, 0xe0, BGP, 0xc9]); // back to the picture; ret

    // The scroll that puts her step in the middle: (x * 16 - 64, y * 16 - 64).
    a.label("set_scroll");
    a.op16(0xfa, X_PIXELS);
    a.op(&[0xd6, 64, 0xe0, SCX]); // sub 64; ldh (SCX), a
    a.op16(0xfa, Y);
    a.op(&[0xcb, 0x37, 0xd6, 64, 0xe0, SCY, 0xc9]); // swap a; sub 64; ldh (SCY), a; ret

    // Draws the room's 16 by 16 steps into the background, four tiles each.
    a.label("draw_room");
    a.op16(0xfa, ROOM);
    a.op(&[0x67, 0x2e, 0x00]); // ld h, a; ld l, 0
    a.op16(0x11, 0x9800); // ld de, $9800
    a.op(&[0x0e, 16]); // ld c, 16 rows
    a.label("row");
    a.op(&[0x06, 16]); // ld b, 16 steps
    a.label("column");
    a.op(&[0x2a, 0xcb, 0x7f]); // ld a, (hl+); bit 7, a
    a.jr(Z, "kind");
    a.op(&[0x3e, DOOR_KIND]);
    a.label("kind");
    a.op(&[0x87, 0x87, 0xe5, 0x62, 0x6b]); // add a, a; add a, a; push hl; ld h, d; ld l, e
    a.op(&[0x22, 0x3c, 0x77, 0x3c, 0xf5]); // ld (hl+), a; inc a; ld (hl), a; inc a; push af
    a.op(&[0x7d, 0xc6, 31, 0x6f, 0x7c, 0xce, 0, 0x67]); // hl += 31: the row below
    a.op(&[0xf1, 0x22, 0x3c, 0x77, 0xe1]); // pop af; ld (hl+), a; inc a; ld (hl), a; pop hl
    a.op(&[0x13, 0x13, 0x05]); // inc de; inc de; dec b
    a.jr(NZ, "column");
    a.op(&[0x7b, 0xc6, 32, 0x5f, 0x7a, 0xce, 0, 0x57]); // de += 32: past the row below
    a.op(&[0x0d]); // dec c
    a.jr(NZ, "row");
    a.op(&[0xc9]);

    a.finish()
}

/// A tile's 16 bytes: a pattern of its own for each kind and corner, so every step kind
/// looks different. Walls are dark, doors darker, floors light.
fn tile(kind: usize, corner: usize) -> [u8; 16] {
    let mut bytes = [0; 16];
    for row in 0..8 {
        let seed = (kind * 4 + corner + 1) * 73 + row * 29;
        let pattern = (seed * seed % 251) as u8 | 0x81;
        let (low, high) = match kind as u8 {
            WALL => (0xff, pattern),
            DOOR_KIND => (pattern, 0xff),
            _ => (pattern, 0x00),
        };
        bytes[row * 2] = low;
        bytes[row * 2 + 1] = high;
    }
    bytes
}

/// The 32 KB ROM.
pub fn build() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xc3, 0x50, 0x01]); // nop; jp $0150
    rom[0x134..0x13d].copy_from_slice(b"VIVYROOMS");
    let mut sum: u8 = 0;
    for &byte in &rom[0x134..0x14d] {
        sum = sum.wrapping_sub(byte).wrapping_sub(1);
    }
    rom[0x14d] = sum;
    let code = program();
    assert!(
        0x150 + code.len() <= usize::from(TILES),
        "the program runs into the tiles"
    );
    rom[0x150..0x150 + code.len()].copy_from_slice(&code);

    let tiles = usize::from(TILES);
    for kind in 0..KINDS {
        for corner in 0..4 {
            let at = tiles + (kind * 4 + corner) * 16;
            rom[at..at + 16].copy_from_slice(&tile(kind, corner));
        }
    }
    // Her: a ring, the same tile four times.
    let her = tiles + usize::from(PLAYER_TILE) * 16;
    let ring = [0x3c, 0x7e, 0xe7, 0xc3, 0xc3, 0xe7, 0x7e, 0x3c];
    for (row, &bits) in ring.iter().enumerate() {
        rom[her + row * 2] = bits;
        rom[her + row * 2 + 1] = bits;
    }
    // Her four sprites in the middle of the screen: (y + 16, x + 8, tile, flags).
    let player = usize::from(PLAYER);
    for (i, (y, x)) in [(80, 72), (80, 80), (88, 72), (88, 80)]
        .into_iter()
        .enumerate()
    {
        rom[player + i * 4..player + i * 4 + 4].copy_from_slice(&[y, x, PLAYER_TILE, 0]);
    }

    for (room, &(number, _, _)) in ROOMS.iter().enumerate() {
        let at = usize::from(number) * 256;
        for y in 0..16 {
            for x in 0..16 {
                rom[at + y * 16 + x] = step_byte(room, (x, y));
            }
        }
    }
    let doors = usize::from(DOORS);
    for (i, line) in DOOR_TO.iter().enumerate() {
        rom[doors + i * 4..doors + i * 4 + 4].copy_from_slice(line);
    }
    rom
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::gameboy::console::Machine;
    use crate::features::games::gameboy::emulator::Button;
    use crate::features::games::gameboy::moves::{Outcome, step};

    fn started() -> Machine {
        let mut m = Machine::new("rooms.gb", &build()).unwrap();
        m.run(120);
        m
    }

    #[test]
    fn rooms_and_doors_fit_where_they_go() {
        let code = program();
        assert!(0x150 + code.len() < usize::from(TILES));
        for &(number, _, _) in &ROOMS {
            assert!(u16::from(number) * 256 > PLAYER + 16);
            assert!(u16::from(number) * 256 < DOORS);
        }
        // Every door leads next to a floor step of the room it names.
        for [to, x, y, _] in DOOR_TO {
            let room = ROOMS.iter().position(|r| r.0 == to).unwrap();
            assert_eq!(step_byte(room, (x.into(), y.into())), ROOMS[room].1);
        }
    }

    #[test]
    fn a_step_scrolls_the_screen_and_walls_stop_her() {
        let mut m = started();
        let read = |m: &mut Machine| {
            (
                m.emulator.read(X_PIXELS),
                m.emulator.read(Y),
                m.emulator.read(ROOM),
            )
        };
        assert_eq!(read(&mut m), (3 * 16, 3, 0x21));
        let right = step(&mut m, Button::Right);
        assert_eq!(right.outcome, Outcome::Moved(1));
        assert_eq!(right.scrolled, (16, 0));
        assert_eq!(read(&mut m), (4 * 16, 3, 0x21));
        // Up twice, to the top row; the wall above stops her.
        step(&mut m, Button::Up);
        step(&mut m, Button::Up);
        assert_eq!(read(&mut m), (4 * 16, 1, 0x21));
        let blocked = step(&mut m, Button::Up);
        assert_eq!(blocked.outcome, Outcome::Blocked);
        assert_eq!(read(&mut m), (4 * 16, 1, 0x21));
    }

    #[test]
    fn a_door_fades_into_the_next_room() {
        let mut m = started();
        // From (3, 3): down to row 4, then right along it to the door at (15, 4).
        step(&mut m, Button::Down);
        let mut last = None;
        for _ in 0..12 {
            last = Some(step(&mut m, Button::Right));
        }
        assert_eq!(last.unwrap().outcome, Outcome::NewArea);
        m.run(60);
        assert_eq!(m.emulator.read(ROOM), 0x35);
        assert_eq!(m.emulator.read(LAST_ROOM), 0x21);
        assert_eq!((m.emulator.read(X_PIXELS), m.emulator.read(Y)), (16, 4));
    }
}
