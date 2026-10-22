//! Her own cheat finder: tools to search the game's memory for a number that matters (HP,
//! money, a counter) and to watch what she finds, the way cheat finders do it. Code only
//! keeps the candidates and reads the bytes; what to look for, and when, is hers (and the
//! strategist's). Nothing in it knows any game.
//!
//! A search starts with every byte of work RAM and high RAM read five ways (one byte, two
//! bytes either way round, and two or three bytes of decimal digits). Each `ram_search`
//! keeps the ones that did what she says: changed, stayed, went up, went down, or equal a
//! number she can see on screen. A few rounds leave one or two, which `ram_watch` names;
//! watched values are in the facts after every action from then on, saved for the game.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::console::Machine;
use crate::ai::ToolDef;

/// Work RAM and high RAM, where games keep what changes.
const RAM: [(u16, u16); 2] = [(0xC000, 0xDFFF), (0xFF80, 0xFFFE)];
/// Candidates an answer lists at most.
const LIST_MOST: usize = 8;
/// Values she can watch at once.
const WATCH_MOST: usize = 8;

/// How a number is kept in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// One byte, 0 to 255.
    Byte,
    /// Two bytes, low byte first.
    Le16,
    /// Two bytes, high byte first.
    Be16,
    /// Two bytes of decimal digits, 0 to 9999 (one digit in each half of a byte).
    Bcd2,
    /// Three bytes of decimal digits, 0 to 999999.
    Bcd3,
}

impl Format {
    const ALL: [Format; 5] = [
        Format::Byte,
        Format::Le16,
        Format::Be16,
        Format::Bcd2,
        Format::Bcd3,
    ];

    fn bytes(self) -> u16 {
        match self {
            Format::Byte => 1,
            Format::Le16 | Format::Be16 | Format::Bcd2 => 2,
            Format::Bcd3 => 3,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Format::Byte => "byte",
            Format::Le16 => "le16",
            Format::Be16 => "be16",
            Format::Bcd2 => "bcd2",
            Format::Bcd3 => "bcd3",
        }
    }

    fn from_name(name: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.name() == name)
    }
}

/// The number at `address` kept as `format`, if those bytes can hold one (decimal digits
/// only go up to 9 in each half of a byte).
fn value(read: &mut impl FnMut(u16) -> u8, address: u16, format: Format) -> Option<u32> {
    let end = address.checked_add(format.bytes() - 1)?;
    if !RAM.iter().any(|&(lo, hi)| address >= lo && end <= hi) {
        return None;
    }
    let bytes: Vec<u32> = (address..=end).map(|a| u32::from(read(a))).collect();
    match format {
        Format::Byte => Some(bytes[0]),
        Format::Le16 => Some(bytes[0] | bytes[1] << 8),
        Format::Be16 => Some(bytes[0] << 8 | bytes[1]),
        Format::Bcd2 | Format::Bcd3 => {
            let mut n = 0;
            for b in bytes {
                let (high, low) = (b >> 4, b & 0x0F);
                if high > 9 || low > 9 {
                    return None;
                }
                n = n * 100 + high * 10 + low;
            }
            Some(n)
        }
    }
}

/// A search under way: what's left, and each one's value at the last round.
#[derive(Debug, Clone, Default)]
pub struct Search {
    left: Vec<(u16, Format, u32)>,
}

/// A value she named, read into the facts after every action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    pub name: String,
    pub address: u16,
    pub format: Format,
}

/// What a round keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    Changed,
    Same,
    Up,
    Down,
    Equals(u32),
}

impl Keep {
    fn keeps(self, was: u32, now: u32) -> bool {
        match self {
            Keep::Changed => now != was,
            Keep::Same => now == was,
            Keep::Up => now > was,
            Keep::Down => now < was,
            Keep::Equals(n) => now == n,
        }
    }
}

/// The search tool.
pub fn search_tool() -> ToolDef {
    ToolDef {
        name: "ram_search",
        description: "Search the game's memory for a number that matters and isn't in \
                the facts (HP, money, a counter), like a cheat finder. \"start\" begins a \
                search with every place it could be. Then do something in the game that \
                changes it (or doesn't), and say what it did since the last search: \
                \"changed\", \"same\", \"up\", \"down\", or \"equals\" with the value you can \
                see on screen (equals also starts a search). Each keeps only the places \
                that did that, and says how many are left; when one or two are, name it \
                with ram_watch.",
        parameters: json!({
            "type": "object",
            "properties": {
                "how": {
                    "type": "string",
                    "enum": ["start", "changed", "same", "up", "down", "equals"]
                },
                "value": {"type": "integer"}
            },
            "required": ["how"]
        }),
    }
}

/// The tool that names what a search found.
pub fn watch_tool() -> ToolDef {
    ToolDef {
        name: "ram_watch",
        description: "Name a value ram_search found, by the address and format it \
                listed, like name \"money\", address \"D347\", format \"bcd3\". From then on \
                its value is in the facts after every action, in this game for good. The \
                same name replaces it; an empty address stops watching it.",
        parameters: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "address": {"type": "string"},
                "format": {
                    "type": "string",
                    "enum": ["byte", "le16", "be16", "bcd2", "bcd3"]
                }
            },
            "required": ["name", "address"]
        }),
    }
}

/// One round of a search, from the tool's arguments. Says what's left.
pub fn search(search: &mut Option<Search>, machine: &mut Machine, args: &Value) -> String {
    let how = args["how"].as_str().unwrap_or("");
    let number = args["value"].as_u64().and_then(|n| u32::try_from(n).ok());
    let keep = match (how, number) {
        ("start", _) => None,
        ("changed", _) => Some(Keep::Changed),
        ("same", _) => Some(Keep::Same),
        ("up", _) => Some(Keep::Up),
        ("down", _) => Some(Keep::Down),
        ("equals", Some(n)) => Some(Keep::Equals(n)),
        ("equals", None) => return "equals needs the value, a whole number.".to_string(),
        _ => return format!("There's no search called \"{how}\"."),
    };
    let mut read = |a: u16| machine.emulator.read(a);
    let started = keep.is_none() || search.is_none();
    if started {
        if matches!(
            keep,
            Some(Keep::Changed | Keep::Same | Keep::Up | Keep::Down)
        ) {
            return "There's no search under way: start one first, or use equals.".to_string();
        }
        *search = Some(Search {
            left: everything(&mut read),
        });
    }
    let Some(found) = search.as_mut() else {
        unreachable!("a search was just started");
    };
    if let Some(keep) = keep {
        found.left = std::mem::take(&mut found.left)
            .into_iter()
            .filter_map(|(address, format, was)| {
                let now = value(&mut read, address, format)?;
                // A search started by equals has no last round to compare with.
                let was = if started { now } else { was };
                keep.keeps(was, now).then_some((address, format, now))
            })
            .collect();
    }
    answer(&found.left, started && keep.is_none())
}

/// Every place a number could be kept, with its value now.
fn everything(read: &mut impl FnMut(u16) -> u8) -> Vec<(u16, Format, u32)> {
    let mut all = Vec::new();
    for (lo, hi) in RAM {
        for address in lo..=hi {
            for format in Format::ALL {
                if let Some(n) = value(read, address, format) {
                    all.push((address, format, n));
                }
            }
        }
    }
    all
}

fn answer(left: &[(u16, Format, u32)], started: bool) -> String {
    if started {
        return format!(
            "Started a search: {} places. Now change the number in the game (or don't), \
             then say what it did.",
            left.len()
        );
    }
    if left.is_empty() {
        return "None left: it isn't kept in a way this can find, or a round said the wrong \
            thing. Start again."
            .to_string();
    }
    let listed: Vec<String> = left
        .iter()
        .take(LIST_MOST)
        .map(|&(address, format, n)| format!("{address:04X} {} = {n}", format.name()))
        .collect();
    let more = if left.len() > LIST_MOST {
        ", the first are"
    } else {
        ":"
    };
    format!("{} left{more} {}.", left.len(), listed.join(", "))
}

/// Names a value from the tool's arguments, or stops watching one.
pub fn watch(watches: &mut Vec<Watch>, args: &Value) -> Result<String, String> {
    let name = args["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err("Give the value a name.".to_string());
    }
    let address = args["address"].as_str().unwrap_or("").trim();
    if address.is_empty() {
        let before = watches.len();
        watches.retain(|w| w.name != name);
        return Ok(if watches.len() < before {
            format!("Stopped watching {name}.")
        } else {
            format!("Nothing called {name} was watched.")
        });
    }
    let address = u16::from_str_radix(address.trim_start_matches("0x"), 16)
        .map_err(|_| format!("\"{address}\" isn't an address like D347."))?;
    let format = match args["format"].as_str() {
        None => Format::Byte,
        Some(f) => Format::from_name(f).ok_or_else(|| format!("There's no format \"{f}\"."))?,
    };
    let end = address.saturating_add(format.bytes() - 1);
    if !RAM.iter().any(|&(lo, hi)| address >= lo && end <= hi) {
        return Err(format!(
            "{address:04X} isn't in the game's memory that changes (C000 to DFFF, FF80 to FFFE)."
        ));
    }
    watches.retain(|w| w.name != name);
    if watches.len() >= WATCH_MOST {
        return Err(format!(
            "At most {WATCH_MOST} values are watched: stop watching one first."
        ));
    }
    watches.push(Watch {
        name: name.clone(),
        address,
        format,
    });
    Ok(format!("Watching {name}: it's in the facts from now on."))
}

/// The watched values now, as a line of the facts (empty when nothing is watched).
pub fn watched(watches: &[Watch], machine: &mut Machine) -> String {
    let mut read = |a: u16| machine.emulator.read(a);
    let values: Vec<String> = watches
        .iter()
        .map(|w| match value(&mut read, w.address, w.format) {
            Some(n) => format!("{} {n}", w.name),
            None => format!("{} (not a number now)", w.name),
        })
        .collect();
    if values.is_empty() {
        String::new()
    } else {
        format!("Watched: {}", values.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Memory as a list of bytes from C000, zeros elsewhere.
    fn reader(bytes: &[u8]) -> impl FnMut(u16) -> u8 + '_ {
        move |a| {
            a.checked_sub(0xC000)
                .and_then(|i| bytes.get(usize::from(i)))
                .copied()
                .unwrap_or(0)
        }
    }

    #[test]
    fn numbers_are_read_each_way() {
        let mut read = reader(&[0x12, 0x34, 0x56]);
        assert_eq!(value(&mut read, 0xC000, Format::Byte), Some(0x12));
        assert_eq!(value(&mut read, 0xC000, Format::Le16), Some(0x3412));
        assert_eq!(value(&mut read, 0xC000, Format::Be16), Some(0x1234));
        assert_eq!(value(&mut read, 0xC000, Format::Bcd2), Some(1234));
        assert_eq!(value(&mut read, 0xC000, Format::Bcd3), Some(123_456));
        let mut read = reader(&[0x1A]);
        assert_eq!(
            value(&mut read, 0xC000, Format::Bcd2),
            None,
            "A isn't a digit"
        );
        // Not past the end of work RAM, nor outside it.
        assert_eq!(value(&mut read, 0xDFFF, Format::Le16), None);
        assert_eq!(value(&mut read, 0x8000, Format::Byte), None);
        assert_eq!(value(&mut read, 0xFF80, Format::Byte), Some(0));
    }

    #[test]
    fn each_round_keeps_what_did_as_she_said() {
        assert!(Keep::Changed.keeps(1, 2) && !Keep::Changed.keeps(2, 2));
        assert!(Keep::Same.keeps(2, 2));
        assert!(Keep::Up.keeps(1, 2) && !Keep::Up.keeps(2, 1));
        assert!(Keep::Down.keeps(2, 1));
        assert!(Keep::Equals(7).keeps(0, 7) && !Keep::Equals(7).keeps(7, 6));
    }

    #[test]
    fn answers_say_how_many_are_left() {
        assert!(answer(&[(0xC000, Format::Byte, 1)], true).starts_with("Started a search: 1"));
        assert_eq!(
            answer(&[(0xD347, Format::Bcd3, 3175)], false),
            "1 left: D347 bcd3 = 3175."
        );
        assert!(answer(&[], false).starts_with("None left"));
        let many: Vec<_> = (0..20).map(|i| (0xC000 + i, Format::Byte, 5)).collect();
        assert!(answer(&many, false).starts_with("20 left, the first are C000 byte = 5"));
    }

    #[test]
    fn watches_are_named_replaced_and_stopped() {
        let mut watches = Vec::new();
        let ok = watch(
            &mut watches,
            &json!({"name": "money", "address": "D347", "format": "bcd3"}),
        );
        assert!(ok.is_ok(), "{ok:?}");
        watch(&mut watches, &json!({"name": "money", "address": "0xD348"})).unwrap();
        assert_eq!(
            watches,
            vec![Watch {
                name: "money".to_string(),
                address: 0xD348,
                format: Format::Byte
            }]
        );
        assert!(watch(&mut watches, &json!({"name": "hp", "address": "8000"})).is_err());
        assert!(watch(&mut watches, &json!({"name": "hp", "address": "zz"})).is_err());
        assert_eq!(
            watch(&mut watches, &json!({"name": "money", "address": ""})),
            Ok("Stopped watching money.".to_string())
        );
        assert!(watches.is_empty());
    }

    /// On Red, the way she'd do it: money by the number on screen, and her x by walking
    /// left and right. Both find Red's own addresses, which nothing here was told.
    #[test]
    #[ignore = "needs a ROM in VIVY_GB_ROM and the task states"]
    fn a_search_finds_reds_money_and_her_x() {
        use super::super::actions;
        use super::super::walk::look_for;
        use super::super::world::World;
        let mut m = super::super::gate::tasks::load("viridian");
        let look = look_for(m.emulator.info());
        let mut world = World::default();
        let act = |world: &mut World, m: &mut Machine, name: &str, args: Value| {
            actions::run(world, m, look, name, &args)
        };
        let facts = act(&mut world, &mut m, "ram_search", json!({"how": "start"}))
            .view
            .facts;
        let money: u32 = facts
            .split("Money: ¥")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap();
        let found = act(
            &mut world,
            &mut m,
            "ram_search",
            json!({"how": "equals", "value": money}),
        );
        assert!(
            found.outcome.contains(&format!("D347 bcd3 = {money}")),
            "{}",
            found.outcome
        );

        // Her x, as the screen shows it: a press may only turn her, which is "same".
        let mut x = act(&mut world, &mut m, "ram_search", json!({"how": "start"}))
            .view
            .at
            .0;
        let mut left = String::new();
        for dir in ["left", "left", "left", "right", "right"] {
            let now = act(&mut world, &mut m, "press", json!({"buttons": dir}))
                .view
                .at
                .0;
            let how = match now.cmp(&x) {
                std::cmp::Ordering::Greater => "up",
                std::cmp::Ordering::Less => "down",
                std::cmp::Ordering::Equal => "same",
            };
            x = now;
            left = act(&mut world, &mut m, "ram_search", json!({"how": how})).outcome;
        }
        // Many copies of her x move with it (her sprite, the screen's tiles), but Red's
        // own is among them.
        let kept = &world.search.as_ref().unwrap().left;
        assert!(
            kept.iter()
                .any(|&(a, f, _)| (a, f) == (0xD362, Format::Byte)),
            "{left}"
        );
        let watched = act(
            &mut world,
            &mut m,
            "ram_watch",
            json!({"name": "x", "address": "D362"}),
        );
        let x = m.emulator.read(0xD362);
        assert!(
            watched.view.facts.contains(&format!("Watched: x {x}")),
            "{}",
            watched.view.facts
        );
    }
}
