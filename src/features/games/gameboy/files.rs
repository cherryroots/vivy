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

/// The values she named in a ROM's game's memory (`ram.rs`), kept like her skills.
pub fn watches_path(saves_dir: &Path, rom: &str) -> PathBuf {
    saves_folder(saves_dir, rom).join("watches.json")
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

/// What the look with no memory map has learned of the game (`any/`): where it keeps
/// things, and what each step's picture is.
pub fn sight_path(saves_dir: &Path, rom: &str) -> PathBuf {
    saves_folder(saves_dir, rom).join("sight.json")
}

/// The session's checkpoints, `<n>.state` with `<n>.json` (the notes at the time). In a
/// folder of their own, so they stay out of the saves list.
pub fn checkpoints_folder(saves_dir: &Path, rom: &str) -> PathBuf {
    saves_folder(saves_dir, rom).join("checkpoints")
}

/// The last episodes as she played them (`session::Replay`), `<n>.json` with `<n>.state`
/// (the game as the episode began), to play again with no model.
pub fn replays_folder(saves_dir: &Path, rom: &str) -> PathBuf {
    saves_folder(saves_dir, rom).join("replays")
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

/// The folder under `saves_dir` that games put aside by a reset go in.
pub const BACKUP: &str = "backup";

/// Starts a ROM's game afresh on disk: its folder (maps, journal, what code learned,
/// saves, replays, checkpoints) is moved aside into `backup` as `<folder> before reset
/// <when>`, so nothing is lost. With `keep_skills`, her skills and watches are copied into the new
/// folder. Says where the old folder went, or `None` when there wasn't one yet.
pub fn reset(
    saves_dir: &Path,
    rom: &str,
    when: &str,
    keep_skills: bool,
) -> std::io::Result<Option<PathBuf>> {
    let folder = saves_folder(saves_dir, rom);
    if !folder.is_dir() {
        return Ok(None);
    }
    let name = folder
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("game")
        .to_string();
    let backup = saves_dir.join(BACKUP);
    std::fs::create_dir_all(&backup)?;
    // Two resets in the same minute get a number.
    let aside = (1..)
        .map(|n| match n {
            1 => backup.join(format!("{name} before reset {when}")),
            n => backup.join(format!("{name} before reset {when} {n}")),
        })
        .find(|path| !path.exists())
        .expect("a free name");
    std::fs::rename(&folder, &aside)?;
    if keep_skills {
        std::fs::create_dir_all(&folder)?;
        for kept in [skills_path(saves_dir, rom), watches_path(saves_dir, rom)] {
            if let Some(file) = kept.file_name()
                && aside.join(file).is_file()
            {
                std::fs::copy(aside.join(file), &kept)?;
            }
        }
    }
    Ok(Some(aside))
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

    #[test]
    fn a_reset_puts_the_folder_aside_and_can_keep_skills() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing to put aside yet.
        assert_eq!(
            reset(dir.path(), "hm.gb", "2026-10-25 1100", false).unwrap(),
            None
        );
        let folder = saves_folder(dir.path(), "hm.gb");
        std::fs::create_dir_all(&folder).unwrap();
        for file in [
            "maps.json",
            "sight.json",
            "skills.json",
            "watches.json",
            "latest.state",
        ] {
            std::fs::write(folder.join(file), file).unwrap();
        }
        let aside = reset(dir.path(), "hm.gb", "2026-10-25 1100", true)
            .unwrap()
            .unwrap();
        let backup = dir.path().join(BACKUP);
        assert_eq!(aside, backup.join("hm before reset 2026-10-25 1100"));
        assert!(aside.join("maps.json").is_file());
        assert!(aside.join("latest.state").is_file());
        // Only her skills and watches come back.
        assert_eq!(
            names_with(&folder, &["json", "state"]),
            vec!["skills.json", "watches.json"]
        );
        // Again in the same minute, without skills: a numbered name, and no folder left.
        let again = reset(dir.path(), "hm.gb", "2026-10-25 1100", false)
            .unwrap()
            .unwrap();
        assert_eq!(again, backup.join("hm before reset 2026-10-25 1100 2"));
        assert!(!folder.exists());
    }
}
