//! MRU (most-recently-used) file switching — Ctrl+Tab / Ctrl+Shift+Tab.
//!
//! VS Code semantics: the first Ctrl+Tab jumps to the PREVIOUSLY used file;
//! while Ctrl stays held, further Tab presses walk deeper into the MRU list
//! (Shift+Tab walks back) WITHOUT reordering it; releasing Ctrl commits the
//! session — the chosen file is promoted to the front. A quick tap therefore
//! toggles between the two most recent files.
//!
//! History entries store user files by PATH (not `UserFile(idx)`) so deletes /
//! reorders can't make an entry point at the wrong file — stale paths simply
//! drop out at purge time.

use crate::app::ProjectFileId;

/// One history entry. User files are tracked by their `src/`-relative path;
/// fixed project files by their id.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum HistEntry {
    Fixed(ProjectFileId),
    User(String),
}

impl HistEntry {
    /// Build an entry from the currently-selected id (`None` for an id that
    /// can't be resolved, e.g. an out-of-range user index).
    pub(crate) fn from_id(id: ProjectFileId, user_files: &[(String, String)]) -> Option<Self> {
        match id {
            ProjectFileId::UserFile(idx) => {
                user_files.get(idx).map(|(p, _)| HistEntry::User(p.clone()))
            }
            fixed => Some(HistEntry::Fixed(fixed)),
        }
    }

    /// Resolve back to a `ProjectFileId` against the CURRENT file list.
    pub(crate) fn to_id(&self, user_files: &[(String, String)]) -> Option<ProjectFileId> {
        match self {
            HistEntry::Fixed(id) => Some(*id),
            HistEntry::User(path) => user_files
                .iter()
                .position(|(p, _)| p == path)
                .map(ProjectFileId::UserFile),
        }
    }

    /// Display label for the cycling overlay.
    pub(crate) fn label(&self) -> String {
        match self {
            HistEntry::User(path) => path.clone(),
            HistEntry::Fixed(id) => match id {
                ProjectFileId::MainRs => "main.rs".into(),
                ProjectFileId::CargoToml => "Cargo.toml".into(),
                ProjectFileId::CargoConfig => ".cargo/config.toml".into(),
                ProjectFileId::MemoryX => "memory.x".into(),
                ProjectFileId::BuildRs => "build.rs".into(),
                ProjectFileId::GitIgnore => ".gitignore".into(),
                ProjectFileId::PartitionsCsv => "partitions.csv".into(),
                ProjectFileId::UserFile(_) => unreachable!("user files use HistEntry::User"),
            },
        }
    }
}

const MAX_HISTORY: usize = 32;

/// MRU history + the active cycling session, if any.
#[derive(Default)]
pub(crate) struct FileCycle {
    /// Front = most recently used.
    history: Vec<HistEntry>,
    /// `Some(cursor)` while a Ctrl-held cycling session is active; the cursor
    /// indexes `history` (the file currently shown).
    cycling: Option<usize>,
}

impl FileCycle {
    pub(crate) fn is_cycling(&self) -> bool {
        self.cycling.is_some()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// The history + cursor, for the overlay.
    pub(crate) fn view(&self) -> (&[HistEntry], Option<usize>) {
        (&self.history, self.cycling)
    }

    /// Record that `entry` became the active file OUTSIDE a cycling session
    /// (tree click, F12, diagnostics nav, …): promote it to the front.
    pub(crate) fn note_open(&mut self, entry: HistEntry) {
        self.history.retain(|e| *e != entry);
        self.history.insert(0, entry);
        self.history.truncate(MAX_HISTORY);
    }

    /// Drop entries that no longer resolve (deleted files, toolchain-hidden
    /// fixed files). Call before starting a session — never during one.
    pub(crate) fn purge(&mut self, mut valid: impl FnMut(&HistEntry) -> bool) {
        if self.cycling.is_none() {
            self.history.retain(|e| valid(e));
        }
    }

    /// First press starts a session at the previously-used file; further
    /// presses (Ctrl still held) walk the list — `forward` = deeper into
    /// history (Ctrl+Tab), else back toward the front (Ctrl+Shift+Tab). Wraps.
    /// Returns the entry to display, cloned (keeps borrows simple).
    pub(crate) fn begin_or_step(&mut self, forward: bool) -> Option<HistEntry> {
        let len = self.history.len();
        if len < 2 {
            return None;
        }
        let cursor = match self.cycling {
            None => 1, // previously-used file
            Some(c) => {
                if forward {
                    (c + 1) % len
                } else {
                    (c + len - 1) % len
                }
            }
        };
        self.cycling = Some(cursor);
        self.history.get(cursor).cloned()
    }

    /// End the session (Ctrl released): promote the chosen entry to the front.
    pub(crate) fn commit(&mut self) {
        if let Some(cursor) = self.cycling.take() {
            if cursor < self.history.len() {
                let entry = self.history.remove(cursor);
                self.history.insert(0, entry);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(p: &str) -> HistEntry {
        HistEntry::User(p.into())
    }

    #[test]
    fn note_open_promotes_dedupes_and_caps() {
        let mut fc = FileCycle::default();
        fc.note_open(user("a.rs"));
        fc.note_open(user("b.rs"));
        fc.note_open(user("a.rs")); // re-open → promoted, not duplicated
        let (h, _) = fc.view();
        assert_eq!(h, &[user("a.rs"), user("b.rs")]);

        for i in 0..(MAX_HISTORY + 10) {
            fc.note_open(user(&format!("f{i}.rs")));
        }
        assert_eq!(fc.view().0.len(), MAX_HISTORY);
    }

    #[test]
    fn quick_tap_toggles_between_two_most_recent() {
        let mut fc = FileCycle::default();
        fc.note_open(user("old.rs"));
        fc.note_open(user("cur.rs")); // current file at front
        // Ctrl+Tab → previously-used file.
        assert_eq!(fc.begin_or_step(true), Some(user("old.rs")));
        fc.commit(); // Ctrl released
        let (h, c) = fc.view();
        assert_eq!(h[0], user("old.rs")); // promoted
        assert_eq!(c, None);
        // Tap again → back to cur.rs.
        assert_eq!(fc.begin_or_step(true), Some(user("cur.rs")));
    }

    #[test]
    fn held_session_walks_and_wraps_both_ways() {
        let mut fc = FileCycle::default();
        fc.note_open(user("c.rs"));
        fc.note_open(user("b.rs"));
        fc.note_open(user("a.rs")); // history: a b c (a = current)
        assert_eq!(fc.begin_or_step(true), Some(user("b.rs")));
        assert_eq!(fc.begin_or_step(true), Some(user("c.rs")));
        assert_eq!(fc.begin_or_step(true), Some(user("a.rs"))); // wrap fwd
        assert_eq!(fc.begin_or_step(false), Some(user("c.rs"))); // back
        fc.commit();
        assert_eq!(fc.view().0[0], user("c.rs"));
    }

    #[test]
    fn single_entry_never_cycles() {
        let mut fc = FileCycle::default();
        fc.note_open(user("only.rs"));
        assert_eq!(fc.begin_or_step(true), None);
        assert!(!fc.is_cycling());
    }

    #[test]
    fn purge_drops_stale_entries_only_outside_session() {
        let mut fc = FileCycle::default();
        fc.note_open(user("dead.rs"));
        fc.note_open(user("live.rs"));
        fc.purge(|e| *e != user("dead.rs"));
        assert_eq!(fc.view().0, &[user("live.rs")]);
        // During a session purge is a no-op (indices must stay stable).
        fc.note_open(user("x.rs"));
        fc.begin_or_step(true);
        fc.purge(|_| false);
        assert_eq!(fc.view().0.len(), 2);
    }

    #[test]
    fn entry_resolution_survives_delete_and_reorder() {
        let mut files = vec![
            ("a.rs".to_string(), String::new()),
            ("b.rs".to_string(), String::new()),
        ];
        let e = HistEntry::from_id(crate::app::ProjectFileId::UserFile(1), &files).unwrap();
        assert_eq!(e, user("b.rs"));
        files.remove(0); // b.rs shifts to index 0
        assert_eq!(
            e.to_id(&files),
            Some(crate::app::ProjectFileId::UserFile(0))
        );
        files.clear();
        assert_eq!(e.to_id(&files), None); // deleted → unresolvable
    }
}
