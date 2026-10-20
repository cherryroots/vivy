//! A tiny Game Boy program for the tests, so they need no game. Built byte by byte here,
//! since a ROM file in the repository would be a binary nobody can read.
//!
//! What it does, as soon as it starts:
//!
//! 1. Turns the screen off, makes tile 1 solid black, clears the background map and puts
//!    tile 1 in its top left corner, then turns the screen back on.
//! 2. Every frame, at the start of V-Blank: reads the joypad into `0xC000` (the standard
//!    byte: A, B, Select, Start in bits 0 to 3, Right, Left, Up, Down in bits 4 to 7), adds
//!    one to the frame counter at `0xC001`, and scrolls the background one pixel right
//!    while Right is held.

/// The program, at 0x150 (just after the header).
const PROGRAM: &[u8] = &[
    0xf3, // di
    0x31, 0xfe, 0xff, // ld sp, $fffe
    // Wait for V-Blank: until LY >= 144.
    0xf0, 0x44, // ldh a, (LY)
    0xfe, 0x90, // cp 144
    0x38, 0xfa, // jr c, -6
    // Screen off.
    0xaf, // xor a
    0xe0, 0x40, // ldh (LCDC), a
    // Tile 0 (0x8000) all white.
    0x21, 0x00, 0x80, // ld hl, $8000
    0x06, 0x10, // ld b, 16
    0xaf, // xor a
    0x22, // ld (hl+), a
    0x05, // dec b
    0x20, 0xfc, // jr nz, -4
    // Tile 1 (0x8010) all black.
    0x06, 0x10, // ld b, 16
    0x3e, 0xff, // ld a, $ff
    0x22, // ld (hl+), a
    0x05, // dec b
    0x20, 0xfc, // jr nz, -4
    // Background map 0x9800-0x9BFF all tile 0.
    0x21, 0x00, 0x98, // ld hl, $9800
    0x01, 0x00, 0x04, // ld bc, $0400
    0xaf, // xor a
    0x22, // ld (hl+), a
    0x0b, // dec bc
    0x78, // ld a, b
    0xb1, // or c
    0x20, 0xf9, // jr nz, -7
    // Tile 1 in the top left corner.
    0x3e, 0x01, // ld a, 1
    0xea, 0x00, 0x98, // ld ($9800), a
    // Palette: white, light, dark, black.
    0x3e, 0xe4, // ld a, $e4
    0xe0, 0x47, // ldh (BGP), a
    // No scroll.
    0xaf, // xor a
    0xe0, 0x42, // ldh (SCY), a
    0xe0, 0x43, // ldh (SCX), a
    0xea, 0x00, 0xc0, // ld ($c000), a
    0xea, 0x01, 0xc0, // ld ($c001), a
    // Screen on: background on, tiles at 0x8000, map at 0x9800.
    0x3e, 0x91, // ld a, $91
    0xe0, 0x40, // ldh (LCDC), a
    // main: wait for the start of V-Blank (LY == 144).
    0xf0, 0x44, // ldh a, (LY)
    0xfe, 0x90, // cp 144
    0x20, 0xfa, // jr nz, -6
    // The d-pad, into the top four bits of b.
    0x3e, 0x20, // ld a, $20
    0xe0, 0x00, // ldh (P1), a
    0xf0, 0x00, // ldh a, (P1)
    0xf0, 0x00, // ldh a, (P1)
    0x2f, // cpl
    0xe6, 0x0f, // and $0f
    0xcb, 0x37, // swap a
    0x47, // ld b, a
    // The buttons, into the bottom four.
    0x3e, 0x10, // ld a, $10
    0xe0, 0x00, // ldh (P1), a
    0xf0, 0x00, // ldh a, (P1)
    0xf0, 0x00, // ldh a, (P1)
    0x2f, // cpl
    0xe6, 0x0f, // and $0f
    0xb0, // or b
    0xea, 0x00, 0xc0, // ld ($c000), a
    0x3e, 0x30, // ld a, $30
    0xe0, 0x00, // ldh (P1), a
    // One more frame.
    0x21, 0x01, 0xc0, // ld hl, $c001
    0x34, // inc (hl)
    // Right held: scroll one pixel.
    0xfa, 0x00, 0xc0, // ld a, ($c000)
    0xcb, 0x67, // bit 4, a
    0x28, 0x05, // jr z, +5
    0xf0, 0x43, // ldh a, (SCX)
    0x3c, // inc a
    0xe0, 0x43, // ldh (SCX), a
    // Wait for V-Blank's first line to pass, so a frame counts once.
    0xf0, 0x44, // ldh a, (LY)
    0xfe, 0x90, // cp 144
    0x28, 0xfa, // jr z, -6
    0xc3, 0x00, 0x00, // jp main (filled in by `build`)
];

/// Where `main` starts in the program, for the jump at its end.
const MAIN: usize = 0x150 + 68;

/// The 32 KB ROM, for the original Game Boy.
pub fn build() -> Vec<u8> {
    with_flag(0x00)
}

/// The same ROM, marked as made for the Game Boy Color too.
pub fn build_color() -> Vec<u8> {
    with_flag(0x80)
}

fn with_flag(color: u8) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x143] = color;
    // Entry point: nop, jp $0150.
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xc3, 0x50, 0x01]);
    // Title, then cartridge type 0 (ROM only), 32 KB of ROM, no RAM.
    rom[0x134..0x13c].copy_from_slice(b"VIVYTEST");
    // The header checksum the boot ROM checks.
    let mut sum: u8 = 0;
    for &byte in &rom[0x134..0x14d] {
        sum = sum.wrapping_sub(byte).wrapping_sub(1);
    }
    rom[0x14d] = sum;
    let start = 0x150;
    rom[start..start + PROGRAM.len()].copy_from_slice(PROGRAM);
    // Point the final jump at `main`.
    let jump = start + PROGRAM.len() - 2;
    rom[jump..jump + 2].copy_from_slice(&(MAIN as u16).to_le_bytes());
    rom
}

#[test]
fn main_is_where_the_frame_loop_starts() {
    // `main` begins with `ldh a, (LY); cp 144; jr nz`.
    let rom = build();
    assert_eq!(&rom[MAIN..MAIN + 6], &[0xf0, 0x44, 0xfe, 0x90, 0x20, 0xfa]);
}
