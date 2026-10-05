//! What the watcher's raw notify events mean for the project tree.
//!
//! notify 8.2 does not report a rename the same way on every OS. Measured on
//! Windows, read from the backends elsewhere:
//!
//! - Windows (`ReadDirectoryChangesW`): `Name(From)` with the old path, then
//!   `Name(To)` with the new one - two events, no tracker, never a `Both`. They
//!   reach the channel in two sends, so a drain can fall between them (89 of
//!   3000 renames with a reader draining as fast as it could). A rename that
//!   REPLACES a file - an atomic save, or a case-only rename, which replaces
//!   the file with itself - is preceded by a `Remove` of the destination. A
//!   move into another folder of the watch is `Remove` + `Create`, as is a move
//!   in or out of it; a folder moved in or out is one event for the folder and
//!   nothing for its contents.
//! - Linux (inotify): `From` and `To` carrying the same tracker (the move's
//!   cookie), then a `Both` with both paths - the same rename a second time.
//! - macOS (FSEvents): `Name(Any)` once per path, with nothing tying the old
//!   side to the new one.
//!
//! Without a tracker (Windows), a `From` pairs only with a `To` in the SAME
//! folder: Windows never reports a move between folders as a name pair, and
//! two renames in different folders can interleave (`From d1/a`, `From d2/c`,
//! `To d1/b`, `To d2/e` - measured, 33 in 24000 with two renaming threads),
//! which would otherwise hand one file's text to another file's name.
//!
//! [`FsEventPairer`] turns all of these into one ordered list of [`FsChange`]s,
//! and [`apply_fs_changes`] applies that list to the tree. A rename stays a
//! RENAME: the entry keeps its index in `user_src_files` (the editor names its
//! file by index) and its in-memory content, unsaved edits included.

use crate::project_tree::logic::{FsEventKind, ProjectTreeState};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long half of a rename waits for the rest before it counts as a removal.
///
/// The halves are sent microseconds apart, and the wait is counted from the
/// frame that drained the first one - after that frame drained everything that
/// was already queued - so this only has to cover the next frame or two.
pub(super) const RENAME_PAIR_WAIT: Duration = Duration::from_millis(250);

/// One change to the tree, with the absolute paths the watcher reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FsChange {
    /// Appeared: a file to read, or a folder to read with everything in it.
    Created(PathBuf),
    Removed(PathBuf),
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
}

/// The first half of something, waiting for the event that says what it was.
#[derive(Debug)]
enum Held {
    /// A `Remove` that may be the destination a rename is about to replace.
    Remove { path: PathBuf, since: Instant },
    /// A `Name(From)` waiting for its `Name(To)`, with the `Remove` that
    /// came right before it.
    From {
        path: PathBuf,
        tracker: Option<usize>,
        replaced: Option<PathBuf>,
        since: Instant,
    },
}

/// Pairs the halves of a rename, across frames when a drain splits them.
#[derive(Debug)]
pub(super) struct FsEventPairer {
    held: Option<Held>,
    /// The last rename made from a `From` + `To`: inotify follows the `To`
    /// with a `Both` for the same rename, which must not apply twice.
    last_pair: Option<(Option<usize>, PathBuf, PathBuf)>,
}

impl Default for FsEventPairer {
    fn default() -> Self {
        Self::new()
    }
}

impl FsEventPairer {
    pub(super) fn new() -> Self {
        Self {
            held: None,
            last_pair: None,
        }
    }

    /// Something is held: the caller has to come back within
    /// [`RENAME_PAIR_WAIT`] even when nothing else wakes it.
    pub(super) fn is_waiting(&self) -> bool {
        self.held.is_some()
    }

    /// Translate one event, appending its changes to `out` in order.
    /// `exists` answers for `Name(Any)`, which does not say which side of a
    /// rename a path was.
    pub(super) fn push(
        &mut self,
        event: &notify::Event,
        now: Instant,
        exists: impl Fn(&Path) -> bool,
        out: &mut Vec<FsChange>,
    ) {
        use notify::EventKind::{Create, Modify, Remove};
        use notify::event::{ModifyKind::Name, RenameMode};

        let paths = &event.paths;
        match event.kind {
            Create(_) => {
                self.release(out);
                for p in paths {
                    emit(out, FsChange::Created(p.clone()));
                }
            }
            Remove(_) => {
                self.release(out);
                if let Some((last, rest)) = paths.split_last() {
                    for p in rest {
                        emit(out, FsChange::Removed(p.clone()));
                    }
                    self.held = Some(Held::Remove {
                        path: last.clone(),
                        since: now,
                    });
                }
            }
            Modify(Name(RenameMode::From)) => {
                self.last_pair = None;
                let replaced = match self.held.take() {
                    Some(Held::Remove { path, .. }) => Some(path),
                    other => {
                        flush(other, out);
                        None
                    }
                };
                match paths.split_last() {
                    Some((last, rest)) => {
                        for p in rest {
                            emit(out, FsChange::Removed(p.clone()));
                        }
                        self.held = Some(Held::From {
                            path: last.clone(),
                            tracker: event.tracker(),
                            replaced,
                            since: now,
                        });
                    }
                    None => flush(replaced.map(|path| Held::Remove { path, since: now }), out),
                }
            }
            Modify(Name(RenameMode::To)) => {
                self.last_pair = None;
                let held = self.held.take();
                let Some(to) = paths.first() else {
                    flush(held, out);
                    return;
                };
                match held {
                    Some(Held::From {
                        path: from,
                        tracker,
                        replaced,
                        ..
                    }) if tracker == event.tracker()
                        && (tracker.is_some() || from.parent() == to.parent()) =>
                    {
                        // The file the rename replaced is not a removal: the
                        // name is taken again by the file that moved there -
                        // the destination of an overwrite, or the source
                        // itself in a case-only rename. Only those EXACT
                        // names: `B.rs` removed before `a.rs` -> `b.rs` is a
                        // removal of its own, or a case-insensitive disk ends
                        // up with two tree entries for one file.
                        if let Some(r) = replaced.filter(|r| r != to && *r != from) {
                            emit(out, FsChange::Removed(r));
                        }
                        out.push(FsChange::Renamed {
                            from: from.clone(),
                            to: to.clone(),
                        });
                        self.last_pair = Some((tracker, from, to.clone()));
                    }
                    // Nothing it pairs with: moved in from outside the watch.
                    other => {
                        flush(other, out);
                        emit(out, FsChange::Created(to.clone()));
                    }
                }
            }
            Modify(Name(RenameMode::Both)) if paths.len() == 2 => {
                let pair = (event.tracker(), paths[0].clone(), paths[1].clone());
                if self.last_pair.take().is_some_and(|last| last == pair) {
                    return;
                }
                self.release(out);
                out.push(FsChange::Renamed {
                    from: pair.1,
                    to: pair.2,
                });
            }
            Modify(Name(RenameMode::Any | RenameMode::Other)) => {
                self.release(out);
                for p in paths {
                    emit(
                        out,
                        if exists(p) {
                            FsChange::Created(p.clone())
                        } else {
                            FsChange::Removed(p.clone())
                        },
                    );
                }
            }
            // Content, metadata and access say nothing about the tree's shape,
            // and do not break a pair either.
            _ => {}
        }
    }

    /// Give up on a first half held for [`RENAME_PAIR_WAIT`]: a `From` whose
    /// `To` never came left the watch.
    pub(super) fn expire(&mut self, now: Instant, out: &mut Vec<FsChange>) {
        let since = match &self.held {
            Some(Held::Remove { since, .. } | Held::From { since, .. }) => *since,
            None => return,
        };
        if now.saturating_duration_since(since) >= RENAME_PAIR_WAIT {
            flush(self.held.take(), out);
        }
    }

    /// Something else happened: whatever was held is what it looked like.
    fn release(&mut self, out: &mut Vec<FsChange>) {
        self.last_pair = None;
        flush(self.held.take(), out);
    }
}

/// A held half on its own: removals, in the order they happened.
fn flush(held: Option<Held>, out: &mut Vec<FsChange>) {
    match held {
        None => {}
        Some(Held::Remove { path, .. }) => emit(out, FsChange::Removed(path)),
        Some(Held::From { path, replaced, .. }) => {
            if let Some(r) = replaced {
                emit(out, FsChange::Removed(r));
            }
            emit(out, FsChange::Removed(path));
        }
    }
}

/// Windows reports one change as several records; the same removal twice in a
/// row is one removal.
fn emit(out: &mut Vec<FsChange>, change: FsChange) {
    if matches!(change, FsChange::Removed(_)) && out.last() == Some(&change) {
        return;
    }
    out.push(change);
}

/// Apply `changes`, in order, to the tree. Paths become relative to the
/// workspace ROOT (tree paths are project-root-relative), and the generated
/// `src/main.rs` is never tracked. `selected` is the editor's file index,
/// carried across every index shift (`None` once its file is gone).
///
/// Returns whether the watched `src/` itself went away.
pub(super) fn apply_fs_changes(
    tree: &mut ProjectTreeState,
    workspace_root: &Path,
    changes: Vec<FsChange>,
    selected: &mut Option<usize>,
) -> bool {
    let workspace_src = workspace_root.join("src");
    let rel = |abs: &Path| -> Option<String> {
        let rel = abs
            .strip_prefix(workspace_root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        (rel != "src/main.rs").then_some(rel)
    };
    let mut src_root_removed = false;
    for change in changes {
        match change {
            FsChange::Created(abs) => {
                if let Some(r) = rel(&abs) {
                    track_from_disk(tree, &abs, &r);
                }
            }
            FsChange::Removed(abs) => {
                // The watched directory itself. Its handle is dead, but the
                // files are still the project's - the workspace is only their
                // copy - so the tree keeps them.
                if abs == workspace_src {
                    src_root_removed = true;
                } else if let Some(r) = rel(&abs) {
                    tree.apply_fs_event(&r, FsEventKind::Remove, selected);
                }
            }
            FsChange::Renamed { from, to } => {
                if from == workspace_src {
                    src_root_removed = true;
                    continue;
                }
                match (rel(&from), rel(&to)) {
                    (Some(old_rel), Some(new_rel)) => {
                        let kind = FsEventKind::Rename {
                            old_rel: old_rel.clone(),
                            new_rel: new_rel.clone(),
                        };
                        // Nothing the tree knew moved - a temp file renamed
                        // into place, or the echo of a rename the IDE already
                        // made - so the destination simply appeared.
                        if !tree.apply_fs_event(&old_rel, kind, selected) {
                            track_from_disk(tree, &to, &new_rel);
                        }
                    }
                    (Some(old_rel), None) => {
                        tree.apply_fs_event(&old_rel, FsEventKind::Remove, selected);
                    }
                    (None, Some(new_rel)) => track_from_disk(tree, &to, &new_rel),
                    (None, None) => {}
                }
            }
        }
    }
    src_root_removed
}

/// Track a path that appeared, from what is on disk now: a file with its
/// content, a folder with everything in it. A folder moved or renamed in from
/// outside the watch arrives as ONE event on every backend, with nothing
/// reported for what it holds.
fn track_from_disk(tree: &mut ProjectTreeState, abs: &Path, rel: &str) {
    // Directories must be tracked as FOLDERS, and unreadable paths skipped -
    // see `apply_fs_create`.
    let is_dir = abs.is_dir();
    super::project_io::apply_fs_create(
        &mut tree.user_src_files,
        &mut tree.user_src_folders,
        rel,
        is_dir,
        || read_lf(abs),
    );
    if !is_dir {
        return;
    }
    let Ok(entries) = std::fs::read_dir(abs) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let child = format!("{rel}/{name}");
        // A build tree never enters the tree (as in `scan_src_dir`), and a
        // symlinked folder is not followed: a link to an ancestor would never end.
        if kind.is_dir() {
            if name != "target" && name != ".git" {
                track_from_disk(tree, &path, &child);
            }
        } else if path.is_file() && child != "src/main.rs" {
            super::project_io::apply_fs_create(
                &mut tree.user_src_files,
                &mut tree.user_src_folders,
                &child,
                false,
                || read_lf(&path),
            );
        }
    }
}

/// A file's text, LF-normalized (phantom-gutter rule).
fn read_lf(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.replace("\r\n", "\n"))
}

#[cfg(test)]
mod pairer_tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};
    use notify::{Event, EventKind};

    fn p(name: &str) -> PathBuf {
        PathBuf::from("/ws/src").join(name)
    }
    fn ev(kind: EventKind, paths: &[&str]) -> Event {
        paths
            .iter()
            .fold(Event::new(kind), |e, name| e.add_path(p(name)))
    }
    fn from(name: &str) -> Event {
        ev(
            EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            &[name],
        )
    }
    fn to(name: &str) -> Event {
        ev(EventKind::Modify(ModifyKind::Name(RenameMode::To)), &[name])
    }
    fn both(a: &str, b: &str) -> Event {
        ev(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[a, b],
        )
    }
    fn any(name: &str) -> Event {
        ev(
            EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
            &[name],
        )
    }
    fn create(name: &str) -> Event {
        ev(EventKind::Create(CreateKind::Any), &[name])
    }
    fn remove(name: &str) -> Event {
        ev(EventKind::Remove(RemoveKind::Any), &[name])
    }
    fn modify(name: &str) -> Event {
        ev(EventKind::Modify(ModifyKind::Any), &[name])
    }
    fn renamed(a: &str, b: &str) -> FsChange {
        FsChange::Renamed {
            from: p(a),
            to: p(b),
        }
    }
    fn created(name: &str) -> FsChange {
        FsChange::Created(p(name))
    }
    fn removed(name: &str) -> FsChange {
        FsChange::Removed(p(name))
    }

    /// Every event in one drain, then the end-of-frame expiry check.
    fn map(pairer: &mut FsEventPairer, events: &[Event], now: Instant) -> Vec<FsChange> {
        let mut out = Vec::new();
        for e in events {
            pairer.push(e, now, |path| path.ends_with("exists.rs"), &mut out);
        }
        pairer.expire(now, &mut out);
        out
    }

    /// Measured: `fs::rename`, `cmd /c ren`, `move` in one folder and
    /// PowerShell's `Rename-Item` all arrive as exactly these two events.
    #[test]
    fn the_windows_pair_is_one_rename() {
        let mut m = FsEventPairer::new();
        let t = Instant::now();
        assert_eq!(
            map(&mut m, &[from("a.rs"), to("b.rs")], t),
            vec![renamed("a.rs", "b.rs")]
        );
        assert!(!m.is_waiting());
    }

    /// The two halves are two channel sends; a drain between them left the
    /// `To` for the next frame.
    #[test]
    fn a_pair_split_across_two_drains_is_still_one_rename() {
        let mut m = FsEventPairer::new();
        let t = Instant::now();
        assert_eq!(map(&mut m, &[from("a.rs")], t), vec![]);
        assert!(m.is_waiting(), "the From must survive to the next frame");
        let later = t + Duration::from_millis(16);
        assert_eq!(
            map(&mut m, &[to("b.rs")], later),
            vec![renamed("a.rs", "b.rs")]
        );
        assert!(!m.is_waiting());
    }

    /// A `From` that never gets its `To` left the watch.
    #[test]
    fn an_unpaired_from_is_a_removal_once_the_wait_is_over() {
        let mut m = FsEventPairer::new();
        let t = Instant::now();
        assert_eq!(map(&mut m, &[from("a.rs")], t), vec![]);
        assert_eq!(map(&mut m, &[], t + RENAME_PAIR_WAIT / 2), vec![]);
        assert_eq!(
            map(&mut m, &[], t + RENAME_PAIR_WAIT),
            vec![removed("a.rs")]
        );
        assert!(!m.is_waiting());
    }

    /// ...and one followed by anything else was not half of a rename.
    #[test]
    fn a_from_followed_by_something_else_is_a_removal_first() {
        let mut m = FsEventPairer::new();
        let t = Instant::now();
        assert_eq!(
            map(&mut m, &[from("a.rs"), create("c.rs")], t),
            vec![removed("a.rs"), created("c.rs")]
        );
    }

    /// A `To` with nothing to pair with was moved in from outside.
    #[test]
    fn an_unpaired_to_is_a_creation() {
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(&mut m, &[to("b.rs")], Instant::now()),
            vec![created("b.rs")]
        );
    }

    /// inotify: `From` and `To` share the cookie, then a `Both` repeats the
    /// same rename. Applied once, also when the `Both` comes a frame later.
    #[test]
    fn a_linux_rename_is_applied_once() {
        let t = Instant::now();
        let f = from("a.rs").set_tracker(7);
        let tt = to("b.rs").set_tracker(7);
        let b = both("a.rs", "b.rs").set_tracker(7);

        let mut m = FsEventPairer::new();
        assert_eq!(
            map(&mut m, &[f.clone(), tt.clone(), b.clone()], t),
            vec![renamed("a.rs", "b.rs")]
        );

        let mut m = FsEventPairer::new();
        assert_eq!(map(&mut m, &[f, tt], t), vec![renamed("a.rs", "b.rs")]);
        assert_eq!(map(&mut m, &[b], t), vec![]);
    }

    /// Different cookies are two different moves: one out, one in.
    #[test]
    fn a_linux_from_and_to_with_different_cookies_do_not_pair() {
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(
                &mut m,
                &[from("a.rs").set_tracker(7), to("b.rs").set_tracker(8)],
                Instant::now()
            ),
            vec![removed("a.rs"), created("b.rs")]
        );
    }

    /// A `Both` on its own keeps the old behaviour: a rename.
    #[test]
    fn a_lone_both_is_still_a_rename() {
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(&mut m, &[both("a.rs", "b.rs")], Instant::now()),
            vec![renamed("a.rs", "b.rs")]
        );
    }

    /// FSEvents: one `Name(Any)` per path, nothing linking the two - so each
    /// is what the disk says now.
    #[test]
    fn a_macos_rename_goes_by_what_exists() {
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(&mut m, &[any("gone.rs"), any("exists.rs")], Instant::now()),
            vec![removed("gone.rs"), created("exists.rs")]
        );
    }

    /// Measured: `Remove(c.rs)`, `From(c.rs)`, `To(C.rs)` - in three different
    /// drains with a fast reader. The Remove is the rename replacing the file
    /// with itself, not a deletion.
    #[test]
    fn a_case_only_rename_is_one_rename() {
        let t = Instant::now();
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(&mut m, &[remove("c.rs"), from("c.rs"), to("C.rs")], t),
            vec![renamed("c.rs", "C.rs")]
        );

        let mut m = FsEventPairer::new();
        assert_eq!(map(&mut m, &[remove("c.rs")], t), vec![]);
        assert_eq!(map(&mut m, &[from("c.rs")], t), vec![]);
        assert_eq!(map(&mut m, &[to("C.rs")], t), vec![renamed("c.rs", "C.rs")]);
    }

    /// Measured, `fs::rename` and `move /Y` onto an existing file: the temp
    /// file's own events, then `Remove(dest)`, `From(tmp)`, `To(dest)`.
    #[test]
    fn an_overwrite_rename_does_not_remove_its_destination() {
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(
                &mut m,
                &[
                    create("n.rs.tmp"),
                    modify("n.rs.tmp"),
                    remove("n.rs"),
                    from("n.rs.tmp"),
                    to("n.rs"),
                ],
                Instant::now()
            ),
            vec![created("n.rs.tmp"), renamed("n.rs.tmp", "n.rs")]
        );
    }

    /// A removal right before an unrelated rename is still a removal, in its
    /// place.
    #[test]
    fn a_remove_before_a_rename_elsewhere_stays_a_removal() {
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(
                &mut m,
                &[remove("x.rs"), from("a.rs"), to("b.rs")],
                Instant::now()
            ),
            vec![removed("x.rs"), renamed("a.rs", "b.rs")]
        );
    }

    /// A removal is held only until the next change, or the wait, says what it
    /// was - and a content change is neither.
    #[test]
    fn a_held_removal_is_released_by_the_next_change_or_the_wait() {
        let t = Instant::now();
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(&mut m, &[remove("a.rs"), create("b.rs")], t),
            vec![removed("a.rs"), created("b.rs")]
        );

        let mut m = FsEventPairer::new();
        let data = ev(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            &["z.rs"],
        );
        assert_eq!(map(&mut m, &[remove("a.rs"), data], t), vec![]);
        assert_eq!(
            map(&mut m, &[], t + RENAME_PAIR_WAIT),
            vec![removed("a.rs")]
        );
    }

    /// Measured: a quick `g -> h -> i`.
    #[test]
    fn a_rename_chain_is_applied_in_order() {
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(
                &mut m,
                &[from("g.rs"), to("h.rs"), from("h.rs"), to("i.rs")],
                Instant::now()
            ),
            vec![renamed("g.rs", "h.rs"), renamed("h.rs", "i.rs")]
        );
    }

    /// Measured: a move into another folder of the watch (`fs::rename` and
    /// `move`) is no rename on Windows, and stays what it is.
    #[test]
    fn a_move_into_a_subfolder_stays_a_removal_and_a_creation() {
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(
                &mut m,
                &[remove("d.rs"), create("sub/d.rs"), modify("sub")],
                Instant::now()
            ),
            vec![removed("d.rs"), created("sub/d.rs")]
        );
    }

    /// Windows reports one removal as several records.
    #[test]
    fn a_repeated_removal_is_one_removal() {
        let mut m = FsEventPairer::new();
        let t = Instant::now();
        assert_eq!(
            map(&mut m, &[remove("a.rs"), remove("a.rs"), create("b.rs")], t),
            vec![removed("a.rs"), created("b.rs")]
        );
    }

    /// Measured, `fs::rename` and `move /Y` of `a.rs` onto `b.rs` while `B.rs`
    /// exists: `Remove(B.rs)`, `From(a.rs)`, `To(b.rs)`. The removal names
    /// neither side of the rename exactly, so it stays one - the tree ends
    /// with one entry for the one file on disk, not `B.rs` beside `b.rs`.
    #[test]
    fn a_removal_of_another_case_of_the_destination_stays_a_removal() {
        let mut m = FsEventPairer::new();
        assert_eq!(
            map(
                &mut m,
                &[remove("B.rs"), from("a.rs"), to("b.rs")],
                Instant::now()
            ),
            vec![removed("B.rs"), renamed("a.rs", "b.rs")]
        );
    }

    /// Measured with two threads renaming in two folders at once: the halves
    /// interleave. Pairing across folders handed `d2/c`'s text to `d1/b`; now
    /// neither crosses, and both fall back to what they look like on disk.
    #[test]
    fn renames_interleaved_across_folders_never_cross() {
        let mut m = FsEventPairer::new();
        let out = map(
            &mut m,
            &[
                from("d1/a.rs"),
                from("d2/c.rs"),
                to("d1/b.rs"),
                to("d2/e.rs"),
            ],
            Instant::now(),
        );
        for change in &out {
            if let FsChange::Renamed { from, to } = change {
                assert_eq!(from.parent(), to.parent(), "a crossed rename: {out:?}");
            }
        }
        assert_eq!(
            out,
            vec![
                removed("d1/a.rs"),
                removed("d2/c.rs"),
                created("d1/b.rs"),
                created("d2/e.rs"),
            ]
        );
    }
}

#[cfg(test)]
mod apply_tests {
    use super::*;

    fn file(path: &str, content: &str) -> (String, String) {
        (path.to_owned(), content.to_owned())
    }

    fn tree(files: &[(&str, &str)], folders: &[&str]) -> ProjectTreeState {
        ProjectTreeState {
            user_src_files: files.iter().map(|(p, c)| file(p, c)).collect(),
            user_src_folders: folders.iter().map(|f| (*f).to_owned()).collect(),
            config_graveyard: Vec::new(),
            kept_config_files: Vec::new(),
        }
    }

    /// The editor names its file by index, so a rename must keep the entry
    /// where it is, with its in-memory (possibly unsaved) text.
    #[test]
    fn a_rename_keeps_the_entry_its_index_and_its_text() {
        let root = Path::new("/ws");
        let mut t = tree(&[("src/keep.rs", "k"), ("src/a.rs", "unsaved edit")], &[]);
        let mut sel = Some(1);
        let changes = vec![FsChange::Renamed {
            from: root.join("src/a.rs"),
            to: root.join("src/b.rs"),
        }];
        assert!(!apply_fs_changes(&mut t, root, changes, &mut sel));
        assert_eq!(
            t.user_src_files,
            vec![file("src/keep.rs", "k"), file("src/b.rs", "unsaved edit")]
        );
        assert_eq!(sel, Some(1));
    }

    /// `B.rs` removed, then `a.rs` renamed onto `b.rs`: one entry, with the
    /// moved file's text - not `B.rs` beside `b.rs` for the one file on disk.
    #[test]
    fn a_removal_then_a_rename_onto_another_case_leaves_one_entry() {
        let root = Path::new("/ws");
        let mut t = tree(&[("src/B.rs", "mem B"), ("src/a.rs", "mem a")], &[]);
        let mut sel = None;
        let changes = vec![
            FsChange::Removed(root.join("src/B.rs")),
            FsChange::Renamed {
                from: root.join("src/a.rs"),
                to: root.join("src/b.rs"),
            },
        ];
        apply_fs_changes(&mut t, root, changes, &mut sel);
        assert_eq!(t.user_src_files, vec![file("src/b.rs", "mem a")]);
    }

    /// Measured: renaming a folder is one pair for the folder and nothing for
    /// its contents - every folder and file under it moves with it.
    #[test]
    fn a_folder_rename_moves_everything_under_it() {
        let root = Path::new("/ws");
        let mut t = tree(
            &[
                ("src/fold/x.rs", "x"),
                ("src/fold/inner/y.rs", "y"),
                ("src/folder.rs", "f"),
            ],
            &["src/fold", "src/fold/inner"],
        );
        let mut sel = Some(1);
        let changes = vec![FsChange::Renamed {
            from: root.join("src/fold"),
            to: root.join("src/fold2"),
        }];
        apply_fs_changes(&mut t, root, changes, &mut sel);
        assert_eq!(t.user_src_folders, vec!["src/fold2", "src/fold2/inner"]);
        assert_eq!(
            t.user_src_files,
            vec![
                file("src/fold2/x.rs", "x"),
                file("src/fold2/inner/y.rs", "y"),
                // A sibling that merely starts with the same letters stays.
                file("src/folder.rs", "f"),
            ]
        );
        assert_eq!(sel, Some(1));
    }

    /// The editor on the file ABOVE which one disappears must stay on its own
    /// file, not slide onto the next one.
    #[test]
    fn a_removal_carries_the_selection_across_the_shift() {
        let root = Path::new("/ws");
        let mut t = tree(
            &[("src/a.rs", "a"), ("src/b.rs", "b"), ("src/c.rs", "c")],
            &[],
        );
        let mut sel = Some(2);
        let changes = vec![FsChange::Removed(root.join("src/a.rs"))];
        apply_fs_changes(&mut t, root, changes, &mut sel);
        assert_eq!(sel, Some(1));
        assert_eq!(t.user_src_files[1].0, "src/c.rs");

        let changes = vec![FsChange::Removed(root.join("src/c.rs"))];
        apply_fs_changes(&mut t, root, changes, &mut sel);
        assert_eq!(sel, None, "its own file is gone");
    }

    /// Measured: a folder moved out of the watch is ONE Remove.
    #[test]
    fn a_folder_removal_takes_its_contents() {
        let root = Path::new("/ws");
        let mut t = tree(
            &[
                ("src/out/v.rs", "v"),
                ("src/out/deep/w.rs", "w"),
                ("src/stay.rs", "s"),
            ],
            &["src/out", "src/out/deep"],
        );
        let mut sel = Some(2);
        apply_fs_changes(
            &mut t,
            root,
            vec![FsChange::Removed(root.join("src/out"))],
            &mut sel,
        );
        assert_eq!(t.user_src_files, vec![file("src/stay.rs", "s")]);
        assert!(t.user_src_folders.is_empty());
        assert_eq!(sel, Some(0));
    }

    /// Onto a file the tree has: the destination keeps its slot and takes the
    /// moved file's text; an editor on the moved file follows it there.
    #[test]
    fn a_rename_onto_a_tracked_file_replaces_it_in_its_slot() {
        let root = Path::new("/ws");
        let mut t = tree(
            &[
                ("src/n.rs", "old"),
                ("src/x.rs", "x"),
                ("src/new.rs", "new"),
            ],
            &[],
        );
        let mut sel = Some(2);
        let changes = vec![FsChange::Renamed {
            from: root.join("src/new.rs"),
            to: root.join("src/n.rs"),
        }];
        apply_fs_changes(&mut t, root, changes, &mut sel);
        assert_eq!(
            t.user_src_files,
            vec![file("src/n.rs", "new"), file("src/x.rs", "x")]
        );
        assert_eq!(sel, Some(0));

        // The editor on a file after the dropped slot keeps its file too.
        let mut t = tree(
            &[
                ("src/new.rs", "new"),
                ("src/n.rs", "old"),
                ("src/x.rs", "x"),
            ],
            &[],
        );
        let mut sel = Some(2);
        let changes = vec![FsChange::Renamed {
            from: root.join("src/new.rs"),
            to: root.join("src/n.rs"),
        }];
        apply_fs_changes(&mut t, root, changes, &mut sel);
        assert_eq!(
            t.user_src_files,
            vec![file("src/n.rs", "new"), file("src/x.rs", "x")]
        );
        assert_eq!(sel, Some(1));
    }

    /// The deletion of the watched `src/` itself says the watch is dead - the
    /// files are still the project's.
    #[test]
    fn the_watched_root_going_away_empties_nothing() {
        let root = Path::new("/ws");
        let mut t = tree(&[("src/a.rs", "a")], &["src/sub"]);
        let mut sel = Some(0);
        assert!(apply_fs_changes(
            &mut t,
            root,
            vec![FsChange::Removed(root.join("src"))],
            &mut sel
        ));
        assert_eq!(t.user_src_files.len(), 1);
        assert_eq!(t.user_src_folders.len(), 1);
        assert_eq!(sel, Some(0));
    }

    /// `src/main.rs` is generated and never a user file, whichever side of a
    /// rename it is on.
    #[test]
    fn main_rs_is_never_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/y.rs"), "y").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();

        let mut t = tree(&[("src/x.rs", "x")], &[]);
        let mut sel = None;
        let changes = vec![
            FsChange::Renamed {
                from: root.join("src/x.rs"),
                to: root.join("src/main.rs"),
            },
            FsChange::Renamed {
                from: root.join("src/main.rs"),
                to: root.join("src/y.rs"),
            },
            FsChange::Created(root.join("src/main.rs")),
        ];
        apply_fs_changes(&mut t, root, changes, &mut sel);
        assert_eq!(t.user_src_files, vec![file("src/y.rs", "y")]);
    }

    /// A rename the tree cannot place - a temp file renamed into place, or the
    /// watcher's echo of a rename the IDE already made - is the destination
    /// appearing: read from disk when new, left alone when already tracked.
    #[test]
    fn a_rename_of_an_unknown_source_is_its_destination_appearing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/fresh.rs"), "on disk\r\n").unwrap();
        std::fs::write(root.join("src/echo.rs"), "on disk").unwrap();

        let mut t = tree(&[("src/echo.rs", "in memory")], &[]);
        let mut sel = Some(0);
        let changes = vec![
            FsChange::Renamed {
                from: root.join("src/fresh.rs.tmp"),
                to: root.join("src/fresh.rs"),
            },
            FsChange::Renamed {
                from: root.join("src/was.rs"),
                to: root.join("src/echo.rs"),
            },
        ];
        apply_fs_changes(&mut t, root, changes, &mut sel);
        assert_eq!(
            t.user_src_files,
            vec![
                file("src/echo.rs", "in memory"),
                file("src/fresh.rs", "on disk\n")
            ]
        );
        assert_eq!(sel, Some(0));
    }

    /// Measured: a folder moved in from outside the watch is one Create (and a
    /// Linux move-in one unpaired To): what it holds must be read with it.
    #[test]
    fn a_folder_that_appears_brings_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let infold = root.join("src/infold");
        std::fs::create_dir_all(infold.join("deep")).unwrap();
        std::fs::create_dir_all(infold.join("target/debug")).unwrap();
        std::fs::write(infold.join("z.rs"), "z").unwrap();
        std::fs::write(infold.join("deep/w.rs"), "w").unwrap();
        std::fs::write(infold.join("target/debug/junk.rs"), "junk").unwrap();

        let mut t = tree(&[], &[]);
        let mut sel = None;
        apply_fs_changes(
            &mut t,
            root,
            vec![FsChange::Created(infold.clone())],
            &mut sel,
        );
        let mut files = t.user_src_files.clone();
        files.sort();
        assert_eq!(
            files,
            vec![
                file("src/infold/deep/w.rs", "w"),
                file("src/infold/z.rs", "z")
            ]
        );
        let mut folders = t.user_src_folders.clone();
        folders.sort();
        assert_eq!(folders, vec!["src/infold", "src/infold/deep"]);
    }

    /// The whole path, pairer to tree, on the sequence measured for a
    /// case-only rename: the entry is renamed where it is.
    #[test]
    fn a_case_only_rename_keeps_its_slot_end_to_end() {
        use notify::event::{ModifyKind, RemoveKind, RenameMode};
        use notify::{Event, EventKind};
        let root = Path::new("/ws");
        let at = |n: &str| root.join("src").join(n);
        let events = [
            Event::new(EventKind::Remove(RemoveKind::Any)).add_path(at("c.rs")),
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From))).add_path(at("c.rs")),
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To))).add_path(at("C.rs")),
        ];
        let mut m = FsEventPairer::new();
        let mut t = tree(&[("src/c.rs", "unsaved"), ("src/d.rs", "d")], &[]);
        let mut sel = Some(0);
        let now = Instant::now();
        for e in &events {
            let mut changes = Vec::new();
            m.push(e, now, |_| true, &mut changes);
            m.expire(now, &mut changes);
            apply_fs_changes(&mut t, root, changes, &mut sel);
        }
        assert_eq!(
            t.user_src_files,
            vec![file("src/C.rs", "unsaved"), file("src/d.rs", "d")]
        );
        assert_eq!(sel, Some(0));
    }
}
