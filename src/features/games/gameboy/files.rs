//! Where ROMs and save states live on disk, and which names are allowed.
//!
//! ROMs are the owner's own files in `roms_dir` (`.gb` or `.gbc`). Save states go in
//! `saves_dir`, a folder per ROM: `gameboy/pokemon_red/start.state`. Names come from
//! slash command options, so they're checked to stay inside those folders.

use std::path::{Path, PathBuf};

/// The ROM files in `dir`, sorted by name. Nothing when the folder is missing.
pub fn roms(dir: &Path) -> Vec<String> {
    names_with(dir, &["gb", "gbc"])
}

/// A ROM's path, if `name` is one of the ROMs in `dir`.
pub fn rom_path(dir: &Path, name: &str) -> Option<PathBuf> {
    roms(dir)
        .into_iter()
        .find(|rom| rom == name)
        .map(|rom| dir.join(rom))
}

/// The folder holding a ROM's save states: its file name without the extension.
pub fn saves_folder(saves_dir: &Path, rom: &str) -> PathBuf {
    let stem = Path::new(rom)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(rom);
    saves_dir.join(stem)
}

/// Her skills for a ROM's game, next to its save states. Going back to a save keeps them:
/// they're what she knows, not where the game is.
pub fn skills_path(saves_dir: &Path, rom: &str) -> PathBuf {
    saves_folder(saves_dir, rom).join("skills.json")
}

/// Her journal for a ROM's game (`journal.rs`): the goal, notes and episode log.
pub fn journal_path(saves_dir: &Path, rom: &str) -> PathBuf {
    saves_folder(saves_dir, rom).join("journal.json")
}

/// The maps code has drawn of the game's areas. Kept across sessions and rollbacks: the
/// places stay where they are.
pub fn maps_path(saves_dir: &Path, rom: &str) -> PathBuf {
    saves_folder(saves_dir, rom).join("maps.json")
}

/// The session's checkpoints, `<n>.state` with `<n>.json` (the notes at the time). In a
/// folder of their own, so they stay out of the saves list.
pub fn checkpoints_folder(saves_dir: &Path, rom: &str) -> PathBuf {
    saves_folder(saves_dir, rom).join("checkpoints")
}

/// What playing used today, for every game (`budget.rs`).
pub fn today_path(saves_dir: &Path) -> PathBuf {
    saves_dir.join("today.json")
}

/// The save states made for a ROM, by name (without `.state`), sorted.
pub fn saves(saves_dir: &Path, rom: &str) -> Vec<String> {
    names_with(&saves_folder(saves_dir, rom), &["state"])
        .into_iter()
        .filter_map(|file| file.strip_suffix(".state").map(str::to_string))
        .collect()
}

/// Where the save state called `name` goes, after checking the name.
pub fn save_path(saves_dir: &Path, rom: &str, name: &str) -> Result<PathBuf, String> {
    let name = save_name(name)?;
    Ok(saves_folder(saves_dir, rom).join(format!("{name}.state")))
}

/// A save state's name, trimmed, if it's 1 to 40 letters, digits, spaces, `-` or `_`.
pub fn save_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.chars().count() <= 40
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'));
    if ok {
        Ok(name)
    } else {
        Err("A save's name is 1 to 40 letters, digits, spaces, `-` or `_`.".to_string())
    }
}

/// File names in `dir` ending in one of `extensions` (any case).
fn names_with(dir: &Path, extensions: &[&str]) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            Path::new(name)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_roms_and_only_roms() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["red.gb", "Crystal.GBC", "notes.txt", "red.sav"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        std::fs::create_dir(dir.path().join("folder.gb")).unwrap();
        assert_eq!(roms(dir.path()), vec!["Crystal.GBC", "red.gb"]);
        assert!(rom_path(dir.path(), "red.gb").is_some());
        // Only names that are listed, so `../` and friends never reach the disk.
        assert!(rom_path(dir.path(), "../red.gb").is_none());
        assert!(rom_path(dir.path(), "notes.txt").is_none());
        assert!(roms(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn save_states_live_in_a_folder_per_rom() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_path(dir.path(), "red.gb", " before Brock ").unwrap();
        assert_eq!(path, dir.path().join("red").join("before Brock.state"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();
        std::fs::write(path.with_file_name("old.state"), b"x").unwrap();
        assert_eq!(saves(dir.path(), "red.gb"), vec!["before Brock", "old"]);
        assert!(saves(dir.path(), "blue.gb").is_empty());
    }

    #[test]
    fn save_names_stay_in_their_folder() {
        assert!(save_name("start").is_ok());
        assert!(save_name("Route 1_b-2").is_ok());
        for bad in ["", "  ", "../x", "a/b", "a.state", &"x".repeat(41)] {
            assert!(save_name(bad).is_err(), "{bad:?} was allowed");
        }
    }
}
