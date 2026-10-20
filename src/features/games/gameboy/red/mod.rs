//! Pokémon Red's memory map: where the game keeps the facts Vivy needs, so code reads them
//! instead of her remembering them. Pokémon Blue is the same game with other Pokémon, so it
//! works for both.
//!
//! The addresses come from the pret/pokered disassembly's symbol file
//! (github.com/pret/pokered, the `symbols` branch), and the story flags from its
//! `constants/event_constants.asm`. They're only read, never written.
//!
//! - `names.rs`: species, item and map names (made from the disassembly's constants)
//! - `story.rs`: the story flags worth knowing, in the order the game reaches them
//!
//! A RAM map can be wrong for another version (a Japanese cartridge, a hack), so
//! [`Trust`] checks it against the screen as she plays before anything relies on it.

mod look;
mod names;
mod story;

use super::emulator::{Emulator, RomInfo};
pub use look::look;
pub use story::STORY;

/// Each address, named as in the disassembly.
mod address {
    pub const PLAYER_FACING: u16 = 0xc109; // wSpritePlayerStateData1FacingDirection
    pub const TILE_MAP: u16 = 0xc3a0; // wTileMap, the screen's 20×18 tiles
    pub const WALK_COUNTER: u16 = 0xcfc5; // wWalkCounter
    pub const IN_BATTLE: u16 = 0xd057; // wIsInBattle
    pub const PLAYER_NAME: u16 = 0xd158; // wPlayerName
    pub const PARTY_COUNT: u16 = 0xd163; // wPartyCount
    pub const PARTY_MONS: u16 = 0xd16b; // wPartyMon1
    pub const BAG_COUNT: u16 = 0xd31d; // wNumBagItems
    pub const BAG_ITEMS: u16 = 0xd31e; // wBagItems
    pub const MONEY: u16 = 0xd347; // wPlayerMoney
    pub const RIVAL_NAME: u16 = 0xd34a; // wRivalName
    pub const BADGES: u16 = 0xd356; // wObtainedBadges
    pub const MAP: u16 = 0xd35e; // wCurMap
    pub const Y: u16 = 0xd361; // wYCoord
    pub const X: u16 = 0xd362; // wXCoord
    pub const TILESET: u16 = 0xd367; // wCurMapTileset
    pub const MAP_HEIGHT: u16 = 0xd368; // wCurMapHeight
    pub const MAP_WIDTH: u16 = 0xd369; // wCurMapWidth
    pub const WARP_COUNT: u16 = 0xd3ae; // wNumberOfWarps
    pub const WARPS: u16 = 0xd3af; // wWarpEntries
    pub const SIGN_COUNT: u16 = 0xd4b0; // wNumSigns
    pub const SIGNS: u16 = 0xd4b1; // wSignCoords
    pub const PASSABLE_TILES: u16 = 0xd530; // wTilesetCollisionPtr
    pub const GRASS_TILE: u16 = 0xd535; // wGrassTile
    pub const EVENT_FLAGS: u16 = 0xd747; // wEventFlags
    pub const JOY_IGNORE: u16 = 0xcd6b; // wJoyIgnore, buttons the game ignores
    pub const ENEMY_SPECIES: u16 = 0xcfe5; // wEnemyMonSpecies
    pub const ENEMY_HP: u16 = 0xcfe6; // wEnemyMonHP
    pub const ENEMY_STATUS: u16 = 0xcfe9; // wEnemyMonStatus
    pub const ENEMY_LEVEL: u16 = 0xcff3; // wEnemyMonLevel
    pub const ENEMY_MAX_HP: u16 = 0xcff4; // wEnemyMonMaxHP
    pub const BATTLE_SPECIES: u16 = 0xd014; // wBattleMonSpecies
    pub const BATTLE_HP: u16 = 0xd015; // wBattleMonHP
    pub const BATTLE_STATUS: u16 = 0xd018; // wBattleMonStatus
    pub const BATTLE_MOVES: u16 = 0xd01c; // wBattleMonMoves
    pub const BATTLE_LEVEL: u16 = 0xd022; // wBattleMonLevel
    pub const BATTLE_MAX_HP: u16 = 0xd023; // wBattleMonMaxHP
    pub const BATTLE_PP: u16 = 0xd02d; // wBattleMonPP
    pub const STATUS_FLAGS_5: u16 = 0xd730; // wStatusFlags5
}

/// Bytes in one party Pokémon's record (`party_struct`).
const MON_SIZE: u16 = 44;

pub const BADGES: [&str; 8] = [
    "Boulder", "Cascade", "Thunder", "Rainbow", "Soul", "Marsh", "Volcano", "Earth",
];

/// Whether this is a game the map is for: Pokémon Red or Blue.
pub fn is_red(info: &RomInfo) -> bool {
    matches!(info.title.as_str(), "POKEMON RED" | "POKEMON BLUE")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Facing {
    Down,
    Up,
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mon {
    pub species: String,
    pub level: u8,
    pub hp: u16,
    pub max_hp: u16,
    /// Asleep, poisoned, burned, frozen or paralysed, if any.
    pub status: Option<&'static str>,
}

impl Mon {
    /// Like `Pidgey Lv3, 12/12 HP, asleep`.
    pub fn short(&self) -> String {
        let status = self.status.map(|s| format!(", {s}")).unwrap_or_default();
        format!(
            "{} Lv{}, {}/{} HP{status}",
            self.species, self.level, self.hp, self.max_hp
        )
    }
}

/// A door or staircase on this map: where it is, and the map it leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Warp {
    pub x: u8,
    pub y: u8,
    pub to_map: u8,
}

/// Everything read from memory at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub map: u8,
    /// The map's width and height in steps (blocks are 2×2 steps).
    pub map_size: (u8, u8),
    /// Where the player stands, in steps from the map's top left corner.
    pub x: u8,
    pub y: u8,
    pub facing: Option<Facing>,
    /// Still part-way through a step.
    pub walking: bool,
    /// The game is moving her itself or ignoring buttons: a scene, like Oak walking her
    /// to his lab.
    pub scripted: bool,
    /// 0 out of battle, 1 against a wild Pokémon, 2 against a trainer.
    pub battle: u8,
    pub player_name: String,
    pub rival_name: String,
    pub badges: u8,
    pub money: u32,
    pub party: Vec<Mon>,
    /// Items in the bag: name and how many.
    pub bag: Vec<(String, u8)>,
    pub warps: Vec<Warp>,
    /// Where signs stand: (x, y).
    pub signs: Vec<(u8, u8)>,
    pub tileset: u8,
    /// The tile numbers the player can walk on in this map's tileset.
    pub passable: Vec<u8>,
    /// The tile number of tall grass here (0xFF where there's none).
    pub grass_tile: u8,
    /// The story flags of [`STORY`] that are set, by name.
    pub story: Vec<&'static str>,
    /// The battle going on, if there is one.
    pub fight: Option<Fight>,
}

/// Both sides of a battle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fight {
    /// Her Pokémon out now.
    pub ours: Mon,
    /// Its moves and the PP left on each.
    pub moves: Vec<(String, u8)>,
    pub theirs: Mon,
}

impl State {
    pub fn read(emu: &mut Emulator) -> State {
        use address::*;
        let r = |emu: &mut Emulator, at: u16| emu.read(at);
        let party_count = r(emu, PARTY_COUNT).min(6);
        let party = (0..u16::from(party_count))
            .map(|i| read_mon(emu, PARTY_MONS + i * MON_SIZE))
            .collect();
        let bag_count = r(emu, BAG_COUNT).min(20);
        let bag = (0..u16::from(bag_count))
            .filter_map(|i| {
                let id = r(emu, BAG_ITEMS + i * 2);
                let count = r(emu, BAG_ITEMS + i * 2 + 1);
                (id != 0xff).then(|| (item_name(id), count))
            })
            .collect();
        let warp_count = r(emu, WARP_COUNT).min(32);
        let warps = (0..u16::from(warp_count))
            .map(|i| {
                let at = WARPS + i * 4;
                Warp {
                    y: r(emu, at),
                    x: r(emu, at + 1),
                    to_map: r(emu, at + 3),
                }
            })
            .collect();
        let sign_count = r(emu, SIGN_COUNT).min(16);
        let signs = (0..u16::from(sign_count))
            .map(|i| (r(emu, SIGNS + i * 2 + 1), r(emu, SIGNS + i * 2)))
            .collect();
        let story = STORY
            .iter()
            .filter(|flag| event_set(emu, flag.index))
            .map(|flag| flag.name)
            .collect();
        let money = [r(emu, MONEY), r(emu, MONEY + 1), r(emu, MONEY + 2)];
        State {
            map: r(emu, MAP),
            map_size: (r(emu, MAP_WIDTH) * 2, r(emu, MAP_HEIGHT) * 2),
            x: r(emu, X),
            y: r(emu, Y),
            facing: match r(emu, PLAYER_FACING) {
                0x0 => Some(Facing::Down),
                0x4 => Some(Facing::Up),
                0x8 => Some(Facing::Left),
                0xc => Some(Facing::Right),
                _ => None,
            },
            walking: r(emu, WALK_COUNTER) != 0,
            // Bits 0, 5 and 7: people moved by a script, buttons off, her moved by a
            // script.
            scripted: r(emu, JOY_IGNORE) != 0 || r(emu, STATUS_FLAGS_5) & 0b1010_0001 != 0,
            battle: r(emu, IN_BATTLE),
            player_name: read_text(emu, PLAYER_NAME, 11),
            rival_name: read_text(emu, RIVAL_NAME, 11),
            badges: r(emu, BADGES),
            money: bcd(&money),
            party,
            bag,
            warps,
            signs,
            tileset: r(emu, TILESET),
            passable: passable_tiles(emu),
            grass_tile: r(emu, GRASS_TILE),
            story,
            // Until both Pokémon are out, the battle copies hold the last battle's.
            fight: (matches!(r(emu, IN_BATTLE), 1 | 2)
                && r(emu, BATTLE_SPECIES) != 0
                && r(emu, ENEMY_SPECIES) != 0)
                .then(|| read_fight(emu)),
        }
    }

    pub fn map_name(&self) -> String {
        map_name(self.map)
    }

    /// The badges won, by name.
    pub fn badge_names(&self) -> Vec<&'static str> {
        BADGES
            .iter()
            .enumerate()
            .filter(|(i, _)| self.badges & (1 << i) != 0)
            .map(|(_, name)| *name)
            .collect()
    }

    /// Whether this looks like real game data rather than a game that hasn't started (or
    /// a different game behind the same title): a known map whose size matches the
    /// disassembly's, the player inside it, a sensible party.
    pub fn plausible(&self) -> bool {
        let Some((_, width, height)) = names::MAPS[self.map as usize] else {
            return false;
        };
        self.map_size == (width * 2, height * 2)
            && self.x < self.map_size.0
            && self.y < self.map_size.1
            && self.party.len() <= 6
            && self
                .party
                .iter()
                .all(|m| m.level <= 100 && m.hp <= m.max_hp)
    }

    /// The facts as lines for a prompt, which beat anything in her notes.
    pub fn facts(&self) -> String {
        let mut lines = vec![
            format!(
                "Location: {} at x {}, y {}",
                self.map_name(),
                self.x,
                self.y
            ),
            format!(
                "Badges: {}",
                match self.badge_names() {
                    names if names.is_empty() => "none".to_string(),
                    names => names.join(", "),
                }
            ),
            format!("Money: ¥{}", self.money),
        ];
        if self.party.is_empty() {
            lines.push("Party: no Pokémon yet".to_string());
        } else {
            let party: Vec<String> = self
                .party
                .iter()
                .map(|m| {
                    let status = m.status.map(|s| format!(", {s}")).unwrap_or_default();
                    format!(
                        "{} Lv{} {}/{} HP{status}",
                        m.species, m.level, m.hp, m.max_hp
                    )
                })
                .collect();
            lines.push(format!("Party: {}", party.join("; ")));
        }
        if !self.bag.is_empty() {
            let bag: Vec<String> = self.bag.iter().map(|(n, c)| format!("{n} ×{c}")).collect();
            lines.push(format!("Bag: {}", bag.join(", ")));
        }
        if !self.story.is_empty() {
            lines.push(format!("Story so far: {}", self.story_lines().join("; ")));
        }
        if let Some(fight) = &self.fight {
            let against = if self.battle == 1 {
                "a wild"
            } else {
                "a trainer's"
            };
            let moves: Vec<String> = fight
                .moves
                .iter()
                .enumerate()
                .map(|(i, (name, pp))| format!("{} {name} (PP {pp})", i + 1))
                .collect();
            lines.push(format!(
                "In a battle with {against} {}. Out now: {}. Moves: {}",
                fight.theirs.short(),
                fight.ours.short(),
                moves.join(", ")
            ));
        } else if self.battle != 0 {
            lines.push(
                match self.battle {
                    1 => "In a battle with a wild Pokémon",
                    2 => "In a battle with a trainer",
                    _ => "In a battle",
                }
                .to_string(),
            );
        }
        lines.join("\n")
    }

    fn story_lines(&self) -> Vec<&'static str> {
        STORY
            .iter()
            .filter(|flag| self.story.contains(&flag.name))
            .map(|flag| flag.said)
            .collect()
    }
}

/// A battle's two Pokémon, from the game's battle copies of them (they can differ from the
/// party's, like a stat lowered by Growl).
fn read_fight(emu: &mut Emulator) -> Fight {
    use address::*;
    let word = |emu: &mut Emulator, at: u16| u16::from_be_bytes([emu.read(at), emu.read(at + 1)]);
    let ours = Mon {
        species: species_name(emu.read(BATTLE_SPECIES)),
        level: emu.read(BATTLE_LEVEL),
        hp: word(emu, BATTLE_HP),
        max_hp: word(emu, BATTLE_MAX_HP),
        status: status_name(emu.read(BATTLE_STATUS)),
    };
    let theirs = Mon {
        species: species_name(emu.read(ENEMY_SPECIES)),
        level: emu.read(ENEMY_LEVEL),
        hp: word(emu, ENEMY_HP),
        max_hp: word(emu, ENEMY_MAX_HP),
        status: status_name(emu.read(ENEMY_STATUS)),
    };
    let moves = (0..4)
        .filter_map(|i| {
            let id = emu.read(BATTLE_MOVES + i);
            // The low 6 bits are the PP left; the top 2 count PP Ups.
            let pp = emu.read(BATTLE_PP + i) & 0x3f;
            (id != 0).then(|| (move_name(id), pp))
        })
        .collect();
    Fight {
        ours,
        moves,
        theirs,
    }
}

fn read_mon(emu: &mut Emulator, at: u16) -> Mon {
    let species = emu.read(at);
    let hp = u16::from_be_bytes([emu.read(at + 1), emu.read(at + 2)]);
    let status = emu.read(at + 4);
    let level = emu.read(at + 0x21);
    let max_hp = u16::from_be_bytes([emu.read(at + 0x22), emu.read(at + 0x23)]);
    Mon {
        species: species_name(species),
        level,
        hp,
        max_hp,
        status: status_name(status),
    }
}

fn species_name(species: u8) -> String {
    names::SPECIES[species as usize]
        .map(str::to_string)
        .unwrap_or_else(|| format!("Pokémon #{species}"))
}

fn move_name(id: u8) -> String {
    names::MOVES[id as usize]
        .map(str::to_string)
        .unwrap_or_else(|| format!("move #{id}"))
}

fn status_name(status: u8) -> Option<&'static str> {
    match status {
        0 => None,
        s if s & 0x07 != 0 => Some("asleep"),
        s if s & 0x08 != 0 => Some("poisoned"),
        s if s & 0x10 != 0 => Some("burned"),
        s if s & 0x20 != 0 => Some("frozen"),
        s if s & 0x40 != 0 => Some("paralysed"),
        _ => None,
    }
}

/// The list the game checks before a step: tile numbers that can be walked on, ending at
/// 0xFF. The pointer is into bank 0 of the ROM, which can always be read.
fn passable_tiles(emu: &mut Emulator) -> Vec<u8> {
    let at = u16::from_le_bytes([
        emu.read(address::PASSABLE_TILES),
        emu.read(address::PASSABLE_TILES + 1),
    ]);
    if at >= 0x4000 {
        return Vec::new();
    }
    (0..32)
        .map(|i| emu.read(at + i))
        .take_while(|&tile| tile != 0xff)
        .collect()
}

fn event_set(emu: &mut Emulator, index: u16) -> bool {
    emu.read(address::EVENT_FLAGS + index / 8) & (1 << (index % 8)) != 0
}

/// The screen's tiles as the game keeps them (`wTileMap`), 20×18, row by row. In the
/// overworld the game checks these for walls, so they're what [`passable`] applies to.
pub fn tile_map(emu: &mut Emulator) -> Vec<u8> {
    (0..20 * 18)
        .map(|i| emu.read(address::TILE_MAP + i))
        .collect()
}

/// Reads a name in the game's own text encoding, up to the end mark (0x50).
fn read_text(emu: &mut Emulator, at: u16, most: u16) -> String {
    (0..most)
        .map(|i| emu.read(at + i))
        .take_while(|&b| b != 0x50)
        .filter_map(char_for)
        .collect()
}

pub fn map_name(map: u8) -> String {
    names::MAPS[map as usize]
        .map(|(name, _, _)| name.to_string())
        .unwrap_or_else(|| format!("map {map}"))
}

fn item_name(id: u8) -> String {
    names::ITEMS[id as usize]
        .map(str::to_string)
        .unwrap_or_else(|| format!("item {id}"))
}

/// Money is kept as 6 decimal digits, two per byte.
fn bcd(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |n, &b| {
        n * 100 + u32::from(b >> 4) * 10 + u32::from(b & 0x0f)
    })
}

/// What a character code (or a font tile's number, which in Red is the same) shows.
pub fn char_for(code: u8) -> Option<&'static str> {
    const UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
    const DIGITS: &str = "0123456789";
    let one = |set: &'static str, i: u8| Some(&set[i as usize..i as usize + 1]);
    match code {
        0x80..=0x99 => one(UPPER, code - 0x80),
        0xa0..=0xb9 => one(LOWER, code - 0xa0),
        0xf6..=0xff => one(DIGITS, code - 0xf6),
        _ => Some(match code {
            0x7f => " ",
            0x9a => "(",
            0x9b => ")",
            0x9c => ":",
            0x9d => ";",
            0x9e => "[",
            0x9f => "]",
            0xba => "é",
            0xbb => "'d",
            0xbc => "'l",
            0xbd => "'s",
            0xbe => "'t",
            0xbf => "'v",
            0xe0 => "'",
            0xe1 => "PK",
            0xe2 => "MN",
            0xe3 => "-",
            0xe4 => "'r",
            0xe5 => "'m",
            0xe6 => "?",
            0xe7 => "!",
            0xe8 => ".",
            0xec => "▷",
            0xed => "▶",
            0xee => "▼",
            0xef => "♂",
            0xf0 => "¥",
            0xf1 => "×",
            0xf3 => "/",
            0xf4 => ",",
            0xf5 => "♀",
            0x79 => "┌",
            0x7a => "─",
            0x7b => "┐",
            0x7c => "│",
            0x7d => "└",
            0x7e => "┘",
            _ => return None,
        }),
    }
}

/// Whether the RAM map can be believed for this cartridge. It starts unproven. Each step
/// the player takes is a test: the screen scrolled one way, and the coordinates in memory
/// must have moved the same way, on the same map. A few agreements make it trusted; one
/// disagreement makes it distrusted for good, and her facts then come from the screen only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Trust {
    pub agreed: u32,
    pub disagreed: u32,
}

impl Trust {
    /// Agreements needed before memory is trusted.
    pub const NEEDED: u32 = 3;

    /// One step seen two ways: what memory says moved (in steps) and what the screen
    /// scrolled (in steps), each as (x, y).
    pub fn check(&mut self, memory: (i32, i32), screen: (i32, i32)) {
        if memory == screen {
            self.agreed += 1;
        } else {
            self.disagreed += 1;
        }
    }

    pub fn trusted(&self) -> bool {
        self.disagreed == 0 && self.agreed >= Self::NEEDED
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn money_is_decimal_digits() {
        assert_eq!(bcd(&[0x00, 0x30, 0x00]), 3000);
        assert_eq!(bcd(&[0x99, 0x99, 0x99]), 999_999);
    }

    #[test]
    fn the_name_tables_line_up_with_the_game() {
        assert_eq!(names::SPECIES[0x99], Some("Bulbasaur"));
        assert_eq!(names::SPECIES[0xb0], Some("Charmander"));
        assert_eq!(names::SPECIES[0xb1], Some("Squirtle"));
        assert_eq!(names::SPECIES[0x54], Some("Pikachu"));
        assert_eq!(names::ITEMS[0x04], Some("Poké Ball"));
        assert_eq!(names::ITEMS[0x46], Some("Oak's Parcel"));
        assert_eq!(names::ITEMS[0xc4], Some("HM01 Cut"));
        assert_eq!(names::ITEMS[0xc9], Some("TM01 Mega Punch"));
        assert_eq!(names::MAPS[0x00], Some(("Pallet Town", 10, 9)));
        assert_eq!(names::MAPS[0x26].map(|m| m.0), Some("Red's House 2F"));
        assert_eq!(names::MAPS[0x28].map(|m| m.0), Some("Oak's Lab"));
    }

    #[test]
    fn reads_the_font() {
        let text: String = [
            0x87, 0xa4, 0xab, 0xab, 0xae, 0x7f, 0xb3, 0xa7, 0xa4, 0xb1, 0xa4, 0xe7,
        ]
        .into_iter()
        .filter_map(char_for)
        .collect();
        assert_eq!(text, "Hello there!");
        assert_eq!(char_for(0x00), None);
    }

    #[test]
    fn trust_needs_agreement_and_never_survives_a_disagreement() {
        let mut trust = Trust::default();
        assert!(!trust.trusted());
        for _ in 0..3 {
            trust.check((1, 0), (1, 0));
        }
        assert!(trust.trusted());
        trust.check((0, 1), (0, 0));
        assert!(!trust.trusted());
        for _ in 0..10 {
            trust.check((1, 0), (1, 0));
        }
        assert!(!trust.trusted());
    }

    /// Starts a new game on the owner's ROM and plays the opening (names RED and BLUE)
    /// until she can walk in her bedroom.
    pub fn new_game() -> crate::features::games::gameboy::console::Machine {
        use crate::features::games::gameboy::console::Machine;
        use crate::features::games::gameboy::emulator::{Button, tests::real_rom};
        let mut m = Machine::new("red.gb", &real_rom()).unwrap();
        assert!(
            is_red(m.emulator.info()),
            "VIVY_GB_ROM isn't Pokémon Red or Blue"
        );
        m.run(60 * 8);
        let tap = |m: &mut Machine, button: Button, after: u32| {
            m.emulator.hold(&[button]);
            m.run(4);
            m.emulator.release();
            m.run(after);
        };
        for _ in 0..600 {
            let tiles = tile_map(&mut m.emulator);
            let words: String = tiles.iter().filter_map(|&t| char_for(t)).collect();
            if words.contains("NEW NAME") {
                // The second choice is a ready-made name.
                tap(&mut m, Button::Down, 10);
                tap(&mut m, Button::A, 30);
                continue;
            }
            let state = State::read(&mut m.emulator);
            if state.map == 0x26 && state.plausible() && !look::has_box(&tiles) {
                m.run(120);
                return m;
            }
            tap(&mut m, Button::A, 20);
        }
        panic!("the opening never reached the bedroom");
    }
}
