//! Which of the project tree's files differ from what is on disk - the dot
//! beside a file's name, so a change made in memory (an edit, a regeneration
//! of main.rs or Cargo.toml, a Custom file's swapped `use` line) shows where
//! the file is, not only in the Git tab.
//!
//! The same question `git::unsaved_changes` answers for the close prompt and
//! the commit warning, asked often enough to draw: each file's text is hashed
//! with its `\r`s left out (CRLF on disk is not a change, as there), and the
//! disk side is re-read only when the file's size or mtime moved - or after a
//! Save, when it is always re-read: on a FAT32 volume a same-size rewrite can
//! keep its 2 s mtime.
//!
//! The texts in memory are hashed at most every [`HASH_EVERY`]; the disk is
//! looked at again when they changed, after a Save ([`TreeUnsaved::saved`]),
//! and at most every [`RECHECK`] on frames drawn anyway. An idle IDE stays
//! idle: the only frame asked for is one [`HASH_EVERY`] after a burst of
//! frames, so the last edit of the burst is seen.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How often the texts in memory are hashed, at most.
const HASH_EVERY: Duration = Duration::from_millis(250);

/// How long a set stays trusted while nothing in memory changed - an outside
/// write to a file is seen within this, on the next frame drawn.
const RECHECK: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct TreeUnsaved {
    /// Project-relative paths whose text in memory differs from the disk's,
    /// or that are not on disk yet.
    set: HashSet<String>,
    /// The folder the set was worked out against.
    dir: Option<PathBuf>,
    /// The memory side the set was worked out for, and when it was hashed.
    memory: u64,
    hashed: Option<Instant>,
    /// When the disk was last looked at.
    checked: Option<Instant>,
    /// A Save ended: the disk may have moved under an unchanged memory.
    stale: bool,
    /// Per path, the file as last read: `(mtime, size, hash)`.
    disk: HashMap<String, (SystemTime, u64, u64)>,
}

impl TreeUnsaved {
    /// The unsaved files' project-relative paths.
    pub(super) fn unsaved(&self) -> impl Iterator<Item = &String> {
        self.set.iter()
    }

    /// A Save ended, written whole or in part: read every file again.
    pub(super) fn saved(&mut self) {
        self.stale = true;
        self.disk.clear();
    }

    /// Brings the set up to date with `files` (`(project-relative path,
    /// text)`) against `dir`, the saved project's folder, and with `absent`,
    /// the paths the next Save deletes - unsaved while they are still on
    /// disk. A project never saved has nothing to differ from: no file is
    /// marked.
    ///
    /// Returns how soon a frame is wanted, when this one was too close to the
    /// last hash to take a new one.
    pub(super) fn refresh(
        &mut self,
        dir: Option<&Path>,
        files: &[(&str, &str)],
        absent: &[&str],
        now: Instant,
    ) -> Option<Duration> {
        let Some(dir) = dir else {
            *self = Self::default();
            return None;
        };
        if self.dir.as_deref() != Some(dir) {
            *self = Self {
                dir: Some(dir.to_owned()),
                ..Self::default()
            };
        }
        let since = |at: Option<Instant>| at.map(|at| now.saturating_duration_since(at));
        if !self.stale
            && let Some(ago) = since(self.hashed)
            && ago < HASH_EVERY
        {
            return Some(HASH_EVERY - ago);
        }
        let texts: Vec<u64> = files.iter().map(|(_, t)| hash(t.as_bytes())).collect();
        let memory = files
            .iter()
            .zip(&texts)
            .fold(0, |acc, ((p, _), h)| mix(mix(acc, hash(p.as_bytes())), *h));
        let memory = absent
            .iter()
            .fold(mix(memory, 1), |acc, p| mix(acc, hash(p.as_bytes())));
        self.hashed = Some(now);
        let fresh = since(self.checked).is_some_and(|ago| ago < RECHECK);
        if memory == self.memory && fresh && !self.stale {
            return None;
        }
        self.memory = memory;
        self.checked = Some(now);
        self.stale = false;
        let mut disk = HashMap::with_capacity(files.len());
        self.set.clear();
        for ((path, _), mem) in files.iter().zip(texts) {
            let on_disk = disk_hash(&dir.join(path), self.disk.get(*path).copied());
            if let Some(entry) = on_disk {
                disk.insert((*path).to_owned(), entry);
            }
            if on_disk.map(|(.., h)| h) != Some(mem) {
                self.set.insert((*path).to_owned());
            }
        }
        self.disk = disk;
        self.set.extend(
            absent
                .iter()
                .filter(|p| dir.join(p).exists())
                .map(|p| (*p).to_owned()),
        );
        None
    }
}

/// The file's `(mtime, size, hash)`, re-read only when the first two moved
/// since `cached`; `None` when it cannot be read.
fn disk_hash(
    path: &Path,
    cached: Option<(SystemTime, u64, u64)>,
) -> Option<(SystemTime, u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let size = meta.len();
    if let Some(c @ (t, s, _)) = cached
        && t == mtime
        && s == size
    {
        return Some(c);
    }
    let bytes = std::fs::read(path).ok()?;
    Some((mtime, size, hash(&bytes)))
}

/// FNV-1a over `bytes` with every `\r` left out.
fn hash(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .filter(|&&b| b != b'\r')
        .fold(0xcbf2_9ce4_8422_2325, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn mix(a: u64, b: u64) -> u64 {
    (a ^ b.rotate_left(17)).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

#[cfg(test)]
mod tests {
    use super::*;

    impl TreeUnsaved {
        fn contains(&self, path: &str) -> bool {
            self.set.contains(path)
        }
    }

    /// Edits, CRLF, a file not written yet, a Save, an outside write, and the
    /// hash throttle with the frame it asks for.
    #[test]
    fn marks_what_differs_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\r\n").unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "a\n").unwrap();
        let t0 = Instant::now();
        let at = |n: u32| t0 + HASH_EVERY * n;
        let mut u = TreeUnsaved::default();

        // CRLF on disk, LF in memory: the same text.
        let mut a = "a\n";
        let f = |a| [("src/main.rs", "fn main() {}\n"), ("src/a.rs", a)];
        assert_eq!(u.refresh(Some(dir.path()), &f(a), &[], at(0)), None);
        assert!(!u.contains("src/main.rs") && !u.contains("src/a.rs"));

        // An edit right after: not hashed yet, and a frame is asked for to
        // see it; at that frame it is marked.
        a = "a2\n";
        assert_eq!(
            u.refresh(Some(dir.path()), &f(a), &[], t0 + HASH_EVERY / 5),
            Some(HASH_EVERY * 4 / 5)
        );
        assert!(!u.contains("src/a.rs"));
        assert_eq!(u.refresh(Some(dir.path()), &f(a), &[], at(1)), None);
        assert!(u.contains("src/a.rs") && !u.contains("src/main.rs"));

        // A file not on disk yet.
        let g = [("src/a.rs", a), ("src/b.rs", "b\n")];
        u.refresh(Some(dir.path()), &g, &[], at(2));
        assert!(u.contains("src/a.rs") && u.contains("src/b.rs"));

        // Saved: memory unchanged, the disk caught up - seen once told.
        std::fs::write(dir.path().join("src/a.rs"), "a2\n").unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "b\n").unwrap();
        u.refresh(Some(dir.path()), &g, &[], at(3));
        assert!(u.contains("src/a.rs"), "trusted until told or RECHECK");
        u.saved();
        u.refresh(Some(dir.path()), &g, &[], at(3));
        assert!(!u.contains("src/a.rs") && !u.contains("src/b.rs"));

        // Written from outside, nothing changed in memory: seen once RECHECK
        // has passed since the disk was last looked at.
        std::fs::write(dir.path().join("src/b.rs"), "b, changed\n").unwrap();
        u.refresh(Some(dir.path()), &g, &[], at(4));
        assert!(!u.contains("src/b.rs"));
        u.refresh(Some(dir.path()), &g, &[], at(3) + RECHECK);
        assert!(u.contains("src/b.rs"));

        // Never saved: nothing to differ from.
        u.refresh(None, &g, &[], at(3) + RECHECK * 2);
        assert!(!u.contains("src/a.rs") && !u.contains("src/b.rs"));
    }

    /// A same-size rewrite that keeps the mtime - a FAT32 volume's 2 s tick -
    /// is invisible to the (mtime, size) cache, until a Save says the disk
    /// moved: then every file is read again.
    #[test]
    fn a_save_rereads_a_rewrite_the_cache_cannot_see() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.rs");
        std::fs::write(&p, "one\n").unwrap();
        let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
        let t0 = Instant::now();
        let mut u = TreeUnsaved::default();
        let f = |t| [("x.rs", t)];
        u.refresh(Some(dir.path()), &f("two\n"), &[], t0);
        assert!(u.contains("x.rs"));

        // The Save writes "two\n": same size, and the mtime of before.
        std::fs::write(&p, "two\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        u.refresh(Some(dir.path()), &f("two\n"), &[], t0 + RECHECK);
        assert!(u.contains("x.rs"), "the cache answered, as designed");
        u.saved();
        u.refresh(Some(dir.path()), &f("two\n"), &[], t0 + RECHECK);
        assert!(!u.contains("x.rs"), "a Save reads the file again");
    }

    /// Another folder is another disk: nothing read from the old one is kept.
    #[test]
    fn a_new_folder_starts_over() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        std::fs::write(old.path().join("x.rs"), "x\n").unwrap();
        let t0 = Instant::now();
        let mut u = TreeUnsaved::default();
        u.refresh(Some(old.path()), &[("x.rs", "x\n")], &[], t0);
        assert!(!u.contains("x.rs"));
        u.refresh(Some(new.path()), &[("x.rs", "x\n")], &[], t0);
        assert!(u.contains("x.rs"), "not written to the new folder yet");
    }

    /// A file the next Save deletes is unsaved while it is on disk, and only
    /// then; emptying it is seen as a change of its own.
    #[test]
    fn a_file_to_delete_is_unsaved_while_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.x"), "MEMORY {}\n").unwrap();
        let t0 = Instant::now();
        let mut u = TreeUnsaved::default();
        u.refresh(Some(dir.path()), &[("memory.x", "MEMORY {}\n")], &[], t0);
        assert!(!u.contains("memory.x"));
        // Emptied: no text to write, so the Save deletes it.
        u.refresh(
            Some(dir.path()),
            &[],
            &["memory.x", "build.rs"],
            t0 + HASH_EVERY,
        );
        assert!(u.contains("memory.x"));
        assert!(!u.contains("build.rs"), "already gone");
        std::fs::remove_file(dir.path().join("memory.x")).unwrap();
        u.saved();
        u.refresh(
            Some(dir.path()),
            &[],
            &["memory.x", "build.rs"],
            t0 + HASH_EVERY,
        );
        assert!(!u.contains("memory.x"));
    }
}
