//! The sweep: each daily reflection also rereads a slice of the notes nobody changed lately,
//! so old duplicates and stale facts in quiet files get looked at too. Without it, a note
//! that nobody touches is never read by a reflection again.
//!
//! The slice goes through the folder in path order and starts where the last one stopped
//! (`memory_sweeps`), so over a week or two every file comes up once, and each day's
//! reflection stays about the same size. Each file comes with where it came from: when it
//! last changed and who wrote it, from `memory_files`.
//!
//! Pure: no Discord, no database. `reflect.rs` loads the files and saves the new spot.

use serde::Deserialize;

use super::reflect::MOOD_FILE;
use super::tool::{SELF_DIR, SKILLS_DIR};

/// `[features.memory]` settings for the sweep.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub(super) struct Settings {
    /// How many characters of older notes each reflection rereads. 0 turns the sweep off.
    pub sweep_chars: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sweep_chars: 12_000,
        }
    }
}

/// One file of a folder, with when it last changed and who asked for that change.
#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub path: String,
    pub content: String,
    pub updated_at: i64,
    pub updated_by: u64,
}

/// The files this sweep rereads, and the path to start after next time.
#[derive(Debug, PartialEq)]
pub struct Slice<'a> {
    pub files: Vec<&'a File>,
    pub next: String,
}

/// Picks the files after `after` (in path order, starting again from the top when the
/// end is reached) until `budget` characters, leaving out `skip` (the files the
/// reflection already shows as changed), her mood file and her self notes (shown up
/// front anyway; her skills are swept). The first file is always taken, even when it's
/// longer than `budget`, so a big file can't stop the sweep. `files` must be sorted by
/// path, as [`store::sweep_files`](super::store::sweep_files) returns them.
pub fn pick<'a>(files: &'a [File], after: &str, skip: &[&str], budget: usize) -> Slice<'a> {
    let self_dir = format!("{SELF_DIR}/");
    let skills = format!("{SKILLS_DIR}/");
    let sweepable = |file: &&File| {
        file.path != MOOD_FILE
            && !skip.contains(&file.path.as_str())
            && (!file.path.starts_with(&self_dir) || file.path.starts_with(&skills))
    };
    // The files after the spot, then the ones from the top up to it.
    let start = files.partition_point(|file| file.path.as_str() <= after);
    let order = files[start..].iter().chain(&files[..start]);

    let mut picked = Vec::new();
    let mut used = 0;
    for file in order.filter(sweepable) {
        let size = file.content.chars().count();
        if !picked.is_empty() && used + size > budget {
            break;
        }
        used += size;
        picked.push(file);
    }
    let next = picked
        .last()
        .map(|file| file.path.clone())
        .unwrap_or_else(|| after.to_string());
    Slice {
        files: picked,
        next,
    }
}

/// The slice for the reflection: each file's source line, then its text.
pub fn render(slice: &Slice, now: i64, bot: u64) -> String {
    let mut text = String::new();
    for file in &slice.files {
        text.push_str(&format!(
            "# {} ({})\n{}\n\n",
            file.path,
            source(file, now, bot),
            file.content.trim()
        ));
    }
    text
}

/// Where a note came from: "last changed 40 days ago by user 123, about themselves".
fn source(file: &File, now: i64, bot: u64) -> String {
    let days = (now - file.updated_at).max(0) / (24 * 60 * 60);
    let when = match days {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        days => format!("{days} days ago"),
    };
    let who = if file.updated_by == bot {
        "you".to_string()
    } else {
        let own = format!("/memories/users/{}/", file.updated_by);
        if file.path.starts_with(&own) {
            format!("user {}, about themselves", file.updated_by)
        } else {
            format!("user {}", file.updated_by)
        }
    };
    format!("last changed {when} by {who}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: usize) -> File {
        File {
            path: path.to_string(),
            content: "x".repeat(size),
            updated_at: 0,
            updated_by: 1,
        }
    }

    fn paths<'a>(slice: &Slice<'a>) -> Vec<&'a str> {
        slice.files.iter().map(|file| file.path.as_str()).collect()
    }

    #[test]
    fn goes_on_from_where_it_stopped_and_wraps() {
        let files = vec![
            file("/memories/a.md", 10),
            file("/memories/b.md", 10),
            file("/memories/c.md", 10),
        ];
        let slice = pick(&files, "", &[], 20);
        assert_eq!(paths(&slice), ["/memories/a.md", "/memories/b.md"]);
        assert_eq!(slice.next, "/memories/b.md");

        let slice = pick(&files, &slice.next, &[], 20);
        assert_eq!(paths(&slice), ["/memories/c.md", "/memories/a.md"]);
        assert_eq!(slice.next, "/memories/a.md");
    }

    #[test]
    fn a_deleted_spot_still_works() {
        let files = vec![file("/memories/a.md", 10), file("/memories/c.md", 10)];
        let slice = pick(&files, "/memories/b.md", &[], 10);
        assert_eq!(paths(&slice), ["/memories/c.md"]);
    }

    #[test]
    fn never_takes_a_file_twice() {
        let files = vec![file("/memories/a.md", 10), file("/memories/b.md", 10)];
        let slice = pick(&files, "/memories/a.md", &[], 1000);
        assert_eq!(paths(&slice), ["/memories/b.md", "/memories/a.md"]);
    }

    #[test]
    fn a_big_file_alone_is_still_taken() {
        let files = vec![file("/memories/a.md", 9000), file("/memories/b.md", 10)];
        let slice = pick(&files, "", &[], 100);
        assert_eq!(paths(&slice), ["/memories/a.md"]);
    }

    #[test]
    fn leaves_out_changed_files_mood_and_self_notes() {
        let files = vec![
            file("/memories/server/culture.md", 10),
            file("/memories/users/5/about.md", 10),
            file("/memories/vivy/mood.md", 10),
            file("/memories/vivy/personality.md", 10),
            file("/memories/vivy/skills/diary.md", 10),
        ];
        let slice = pick(&files, "", &["/memories/users/5/about.md"], 1000);
        assert_eq!(
            paths(&slice),
            [
                "/memories/server/culture.md",
                "/memories/vivy/skills/diary.md"
            ]
        );
    }

    #[test]
    fn nothing_to_sweep_keeps_the_spot() {
        let files = vec![file("/memories/vivy/mood.md", 10)];
        let slice = pick(&files, "/memories/x", &[], 1000);
        assert!(slice.files.is_empty());
        assert_eq!(slice.next, "/memories/x");
    }

    #[test]
    fn says_where_a_note_came_from() {
        let day = 24 * 60 * 60;
        let mut own = file("/memories/users/7/about.md", 1);
        own.updated_by = 7;
        own.updated_at = 0;
        assert_eq!(
            source(&own, 40 * day, 99),
            "last changed 40 days ago by user 7, about themselves"
        );
        let mut other = file("/memories/users/7/games.md", 1);
        other.updated_by = 8;
        assert_eq!(source(&other, day, 99), "last changed yesterday by user 8");
        let mut hers = file("/memories/server/culture.md", 1);
        hers.updated_by = 99;
        assert_eq!(source(&hers, 10, 99), "last changed today by you");
    }
}
