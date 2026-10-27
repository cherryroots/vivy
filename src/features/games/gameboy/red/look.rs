//! Looking at Pokémon Red: what's on screen, read from the game's memory and the video
//! memory, marked on the area's map.
//!
//! Red keeps its own copy of the screen's tiles (`wTileMap`), and before each step it
//! checks one of them against the tileset's list of tiles that can be walked on. So code
//! can tell floor from wall exactly, the way the game does, with no model looking at tiles.
//! The step at screen step (i, j) (the screen is 10×9 steps, she stands at (4, 4)) is
//! checked on the tile at column 2i, row 2j + 1: its bottom left tile.

use super::super::console::Machine;
use super::super::map::{AreaMap, Dir, Kind, Pos};
use super::super::screen::Screen;
use super::super::text::words_of;
use super::super::world::{Grid, View, World};
use super::{State, char_for, tile_map};

/// Screen steps across and down.
const STEPS_ACROSS: i32 = 10;
const STEPS_DOWN: i32 = 9;
/// Map tiles are numbered below this; the font and borders are above.
const MAP_TILES: u8 = 0x60;
/// Where she stands on the screen, in steps.
const PLAYER_STEP: (i32, i32) = (4, 4);
/// The top left pixel of her sprite. Red draws people 4 pixels above the step grid.
const PLAYER_PIXEL: (i32, i32) = (64, 60);

/// Looks at the game and marks what it sees on the area's map.
pub fn look(world: &mut World, machine: &mut Machine) -> View {
    let emu = &mut machine.emulator;
    let state = State::read(emu);
    let tiles = tile_map(emu);
    let screen = Screen::read(emu.video());
    let text_box = has_box(&tiles);
    let area = state.map_name();
    let at = (i32::from(state.x), i32::from(state.y));
    let can_walk = state.plausible()
        && screen.drawn
        && on_map(&state, &tiles, &screen)
        && !text_box
        && state.battle == 0
        && !state.walking
        && !state.scripted;
    if can_walk {
        see(world.area(&area), &state, &tiles, &screen);
    }
    // One disagreement between memory and the screen and memory's facts are left out.
    let facts = if world.trust.disagreed == 0 && state.plausible() {
        state.facts()
    } else {
        String::new()
    };
    View {
        area,
        at,
        can_walk,
        // Words are only in boxes; anything else is tiles that happen to share the font's
        // numbers (the swirl into a battle).
        text: if text_box {
            words_of(&text_rows(&tiles))
        } else {
            String::new()
        },
        facts,
        grid: can_walk.then_some(Grid {
            pixels: 16,
            across: STEPS_ACROSS,
            down: STEPS_DOWN,
            player: PLAYER_STEP,
        }),
    }
}

/// Whether a text box or menu is open: its corners are on screen. Map tiles never use
/// the border tiles' numbers.
pub(super) fn has_box(tiles: &[u8]) -> bool {
    tiles.contains(&0x79) && tiles.contains(&0x7e)
}

/// Whether the screen shows the map with her on it, not a page like the Pokédex or the
/// swirl into a battle: every tile is a map tile (Red's tilesets have 0x60), her sprite is
/// in the middle, and the tile she stands on can be walked on.
fn on_map(state: &State, tiles: &[u8], screen: &Screen) -> bool {
    if tiles.iter().any(|&t| t >= MAP_TILES) {
        return false;
    }
    let standing = tiles[(2 * PLAYER_STEP.1 as usize + 1) * 20 + 2 * PLAYER_STEP.0 as usize];
    let sprite = screen.sprites.iter().any(|s| (s.x, s.y) == PLAYER_PIXEL);
    sprite && state.passable.contains(&standing)
}

/// The screen's 18 rows of text, from the game's copy of the screen.
fn text_rows(tiles: &[u8]) -> Vec<String> {
    tiles
        .chunks(20)
        .map(|row| row.iter().map(|&t| char_for(t).unwrap_or(" ")).collect())
        .collect()
}

/// Marks every step on screen on the map, and where people stand.
fn see(map: &mut AreaMap, state: &State, tiles: &[u8], screen: &Screen) {
    let (width, height) = (i32::from(state.map_size.0), i32::from(state.map_size.1));
    map.size = Some((width, height));
    for j in 0..STEPS_DOWN {
        for i in 0..STEPS_ACROSS {
            let pos = (
                i32::from(state.x) + i - PLAYER_STEP.0,
                i32::from(state.y) + j - PLAYER_STEP.1,
            );
            let tile = tiles[((2 * j + 1) * 20 + 2 * i) as usize];
            let inside = (0..width).contains(&pos.0) && (0..height).contains(&pos.1);
            map.see(pos, kind(state, pos, tile, inside));
        }
    }
    for (pos, dir) in pair_walls(
        (i32::from(state.x), i32::from(state.y)),
        &state.pair_walls,
        tiles,
    ) {
        map.blocked(pos, dir);
        map.blocked(dir.step(pos), dir.back());
    }
    map.see_people(people(state, screen));
}

/// Where she can't step between two steps on screen, though both can be walked on: their
/// tiles are one of the tileset's wall pairs (raised ground and the floor beside it). Each
/// as the step on the left or above, and the way to the other. `at` is where she stands.
fn pair_walls(at: Pos, pairs: &[(u8, u8)], tiles: &[u8]) -> Vec<(Pos, Dir)> {
    let tile = |i: i32, j: i32| tiles[((2 * j + 1) * 20 + 2 * i) as usize];
    let walls = |a: u8, b: u8| pairs.iter().any(|&pair| pair == (a, b) || pair == (b, a));
    let mut found = Vec::new();
    for j in 0..STEPS_DOWN {
        for i in 0..STEPS_ACROSS {
            for (dir, (di, dj)) in [(Dir::Right, (1, 0)), (Dir::Down, (0, 1))] {
                if i + di < STEPS_ACROSS
                    && j + dj < STEPS_DOWN
                    && walls(tile(i, j), tile(i + di, j + dj))
                {
                    found.push(((at.0 + i - PLAYER_STEP.0, at.1 + j - PLAYER_STEP.1), dir));
                }
            }
        }
    }
    found
}

fn kind(state: &State, (x, y): Pos, tile: u8, inside: bool) -> Kind {
    let passable = state.passable.contains(&tile);
    if !inside {
        // Past the edge the screen shows the next area (where one joins) or the border.
        return if passable { Kind::Edge } else { Kind::Wall };
    }
    let here = |px: u8, py: u8| i32::from(px) == x && i32::from(py) == y;
    if state.warps.iter().any(|w| here(w.x, w.y)) {
        Kind::Door
    } else if state.signs.iter().any(|&(sx, sy)| here(sx, sy)) {
        Kind::Sign
    } else if tile == state.grass_tile {
        Kind::Grass
    } else if passable {
        Kind::Floor
    } else {
        Kind::Wall
    }
}

/// Where the people on screen stand, from their sprites. Each person is four 8×8 sprites;
/// all four fall in the same step.
fn people(state: &State, screen: &Screen) -> Vec<Pos> {
    let mut found: Vec<Pos> = screen
        .sprites
        .iter()
        .map(|s| {
            (
                (s.x - PLAYER_PIXEL.0).div_euclid(16),
                (s.y - PLAYER_PIXEL.1).div_euclid(16),
            )
        })
        .filter(|&step| step != (0, 0))
        .map(|(i, j)| (i32::from(state.x) + i, i32::from(state.y) + j))
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raised_ground_beside_the_floor_is_a_wall_between_them() {
        // Cave floor (0x05) with raised ground (0x20) in the column left of her and the
        // step above her; 0x05 to 0x05 and 0x20 to 0x20 are fine.
        let mut tiles = vec![0x05; 20 * 18];
        let mut put = |i: usize, j: usize, tile: u8| tiles[(2 * j + 1) * 20 + 2 * i] = tile;
        for j in 0..9 {
            put(3, j, 0x20);
        }
        put(4, 3, 0x20);
        let walls = pair_walls((10, 10), &[(0x20, 0x05)], &tiles);
        // She stands at screen step (4, 4), so screen step (i, j) is (6 + i, 6 + j).
        assert!(walls.contains(&((9, 10), Dir::Right)), "{walls:?}");
        assert!(walls.contains(&((10, 9), Dir::Down)), "{walls:?}");
        assert!(walls.contains(&((8, 6), Dir::Right)), "{walls:?}");
        assert!(!walls.contains(&((9, 9), Dir::Right)), "{walls:?}");
        assert!(!walls.contains(&((9, 9), Dir::Down)), "{walls:?}");
        // 9 down the column's left, 8 down its right (not where the raised step above her
        // joins it), and that step's top, bottom and right.
        assert_eq!(walls.len(), 9 + 8 + 3, "{walls:?}");
        assert!(pair_walls((10, 10), &[], &tiles).is_empty());
    }

    #[test]
    fn a_box_needs_both_corners() {
        let mut tiles = vec![0x01; 360];
        assert!(!has_box(&tiles));
        tiles[240] = 0x79;
        assert!(!has_box(&tiles));
        tiles[359] = 0x7e;
        assert!(has_box(&tiles));
    }

    #[test]
    fn text_rows_read_the_font() {
        let mut tiles = vec![0x7f; 360];
        tiles[20..25].copy_from_slice(&[0x87, 0xa8, 0xe7, 0x7f, 0xbd]);
        let rows = text_rows(&tiles);
        assert_eq!(rows[1].trim_end(), "Hi! 's");
        assert_eq!(words_of(&rows), "Hi! 's");
    }

    #[test]
    #[ignore = "needs a ROM in VIVY_GB_ROM"]
    fn walks_out_of_the_house_and_into_oak() {
        use crate::features::games::gameboy::map::Kind;
        use crate::features::games::gameboy::red::tests::new_game;
        use crate::features::games::gameboy::walk::{Walked, walk_to};

        let mut m = new_game();
        let mut world = World::default();
        let view = look(&mut world, &mut m);
        println!(
            "{}\n{}",
            view.facts,
            world.areas[&view.area].draw(view.at, 9)
        );
        assert_eq!(view.area, "Red's House 2F");
        assert!(view.can_walk && view.text.is_empty());
        let bedroom = &world.areas["Red's House 2F"];
        // The screen shows 4 steps above her: the top two rows aren't seen yet.
        assert_eq!(bedroom.kind((3, 1)), None);
        assert_eq!(bedroom.kind((8, 6)), Some(Kind::Wall), "past the edge");

        // Up to the stairs, through steps she hasn't seen yet.
        let walk = walk_to(&mut world, &mut m, look, (7, 1), 30);
        assert_eq!(walk.result, Walked::NewArea("Red's House 1F".to_string()));
        assert_eq!(world.areas["Red's House 2F"].kind((7, 1)), Some(Kind::Door));
        assert!(world.trust.trusted(), "{:?}", world.trust);
        println!("{}", world.recent(20).join("\n"));

        // Out of the front door. Its mat is two steps; stepping onto one may not leave yet.
        let view = walk.view;
        let house = &world.areas[&view.area];
        println!("{}", house.draw(view.at, 9));
        // The mat's warps are in memory before the screen has shown them.
        let mat = super::State::read(&mut m.emulator)
            .warps
            .iter()
            .map(|w| (i32::from(w.x), i32::from(w.y)))
            .max_by_key(|p| p.1)
            .expect("the front door");
        let walk = walk_to(&mut world, &mut m, look, mat, 40);
        println!(
            "{:?} after {} steps\n{}",
            walk.result,
            walk.steps,
            world.recent(5).join("\n")
        );
        let mut view = walk.view;
        if walk.result == Walked::Arrived {
            let (next, _) = crate::features::games::gameboy::walk::take_step(
                &mut world,
                &mut m,
                look,
                &view,
                crate::features::games::gameboy::map::Dir::Down,
            );
            view = next;
        }
        assert_eq!(view.area, "Pallet Town");

        // North out of town, Oak stops her: walking ends with his words on screen.
        let walk = walk_to(&mut world, &mut m, look, (10, 0), 60);
        println!(
            "{:?} after {} steps\n{}",
            walk.result,
            walk.steps,
            world.areas["Pallet Town"].draw(walk.view.at, 9)
        );
        match &walk.result {
            Walked::Interrupted(words) => {
                assert!(words.contains("OAK") || words.contains("Wait"), "{words}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(world.trust.disagreed, 0);

        // The font learned from Oak's box reads the same words from video memory.
        use crate::features::games::gameboy::text::Font;
        let screen = Screen::read(m.emulator.video());
        let mut font = Font::default();
        font.learn_from_numbers(&screen, char_for, true);
        assert!(font.len() > 5);
        assert_eq!(font.words(&screen), walk.view.text);
    }

    #[test]
    #[ignore = "needs a ROM in VIVY_GB_ROM"]
    fn exploring_fills_in_the_bedroom() {
        use crate::features::games::gameboy::walk::{Walked, explore};

        let mut m = crate::features::games::gameboy::red::tests::new_game();
        let mut world = World::default();
        let mut walks = 0;
        loop {
            let walk = explore(&mut world, &mut m, look, 30);
            println!(
                "{:?} after {} steps, at {:?}",
                walk.result, walk.steps, walk.view.at
            );
            walks += 1;
            assert!(walks < 20, "{:?}", walk.result);
            match walk.result {
                Walked::NoWay => break,
                Walked::Arrived | Walked::Stuck => {}
                // The stairs are a door: exploring never walks through one.
                other => panic!("{other:?}"),
            }
        }
        let view = look(&mut world, &mut m);
        let drawn = world.areas["Red's House 2F"].draw(view.at, 9);
        println!("{drawn}");
        // Every step of the 8×8 room has been seen (past its edge needn't be).
        let room = &world.areas["Red's House 2F"];
        for y in 0..8 {
            for x in 0..8 {
                assert!(room.kind((x, y)).is_some(), "({x}, {y})\n{drawn}");
            }
        }
        assert!(world.trust.trusted());
    }
}
