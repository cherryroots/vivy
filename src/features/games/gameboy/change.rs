//! Noticing when the picture goes dark between areas. Pure.
//!
//! Most games hide the switch to a new area: the screen fades to one colour, or the game
//! switches it off for a few frames while it loads the new map. In Pokémon Red, doors and
//! stairs fade to black and then switch the screen off; towns and routes join with no fade
//! at all (there the RAM map says the map changed). Code watches for any of the three:
//! a screen that's off, a frame of one colour (any shade), or a palette faded to one shade.

use super::emulator::Registers;

/// How a frame looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// A normal picture.
    Picture,
    /// The screen was off.
    Off,
    /// Every pixel the same colour.
    Blank,
}

/// How a frame looks, from whether it was drawn, its registers and its pixels (3 bytes
/// each).
pub fn look(drawn: bool, registers: &Registers, rgb: &[u8], color: bool) -> Look {
    if !drawn || registers.lcdc & 0x80 == 0 {
        return Look::Off;
    }
    // A palette that maps all four colour numbers to one shade draws one colour, whatever
    // the tiles hold. (Sprites have their own palettes, but a faded screen fades them too.)
    if !color
        && [1, 2, 3]
            .iter()
            .all(|i| (registers.bgp >> (i * 2)) & 3 == registers.bgp & 3)
    {
        return Look::Blank;
    }
    let first = &rgb[..3];
    if rgb.chunks_exact(3).all(|p| p == first) {
        Look::Blank
    } else {
        Look::Picture
    }
}

/// Follows frames and says when a dark spell ends: the game has probably moved somewhere
/// new. A blank screen in a game's normal play (a black cave, a white flash in a battle)
/// is shorter than a real change, so only spells of at least `min_frames` count.
#[derive(Debug, Clone, Default)]
pub struct Transitions {
    dark_frames: u32,
}

impl Transitions {
    /// Frames of dark that make an area change. Red's doors are dark for about 20.
    pub const MIN_FRAMES: u32 = 4;

    /// Feeds one frame. True on the first picture after a long enough dark spell.
    pub fn feed(&mut self, look: Look) -> bool {
        match look {
            Look::Picture => {
                let ended = self.dark_frames >= Self::MIN_FRAMES;
                self.dark_frames = 0;
                ended
            }
            Look::Off | Look::Blank => {
                self.dark_frames += 1;
                false
            }
        }
    }

    /// Whether it's dark right now.
    pub fn dark(&self) -> bool {
        self.dark_frames > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> Vec<u8> {
        let mut rgb = vec![255; 160 * 144 * 3];
        rgb[0..3].copy_from_slice(&[0, 0, 0]);
        rgb
    }

    fn on(bgp: u8) -> Registers {
        Registers {
            lcdc: 0x91,
            bgp,
            ..Registers::default()
        }
    }

    #[test]
    fn tells_pictures_from_dark_frames() {
        assert_eq!(look(true, &on(0xe4), &picture(), false), Look::Picture);
        assert_eq!(look(false, &on(0xe4), &picture(), false), Look::Off);
        // Every colour number white, or every one black: faded out either way.
        assert_eq!(look(true, &on(0x00), &picture(), false), Look::Blank);
        assert_eq!(look(true, &on(0xff), &picture(), false), Look::Blank);
        assert_eq!(
            look(true, &on(0xe4), &vec![96; 160 * 144 * 3], false),
            Look::Blank
        );
        // A Game Boy Color has no BGP: only the pixels count.
        assert_eq!(look(true, &on(0xff), &picture(), true), Look::Picture);
        let mut off = on(0xe4);
        off.lcdc = 0x11;
        assert_eq!(look(true, &off, &picture(), false), Look::Off);
    }

    #[test]
    fn a_long_dark_spell_is_a_change_and_a_flicker_is_not() {
        let mut t = Transitions::default();
        assert!(!t.feed(Look::Picture));
        for _ in 0..2 {
            assert!(!t.feed(Look::Blank));
        }
        assert!(!t.feed(Look::Picture), "a 2-frame flicker counted");
        for _ in 0..20 {
            t.feed(Look::Blank);
            t.feed(Look::Off);
        }
        assert!(t.dark());
        assert!(t.feed(Look::Picture));
        assert!(!t.feed(Look::Picture));
    }
}
