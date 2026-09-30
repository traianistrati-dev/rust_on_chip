//! What follows a `pins/configs/` sync that MOVED or dropped files - an I2C
//! device renamed or renumbered, a project from before buses were folders
//! opened for the first time.
//!
//! The tree already holds the files where they are now. These do not follow
//! on their own:
//!
//! - **The workspace copy.** The Save flush writes files but never deletes
//!   one, so the old `i2c1.rs` would stay beside the new `i2c1/mod.rs` - and
//!   the two at once do not compile. A workspace write prunes it.
//! - **rust-analyzer**, which keeps every file it was sent open as an overlay
//!   until it is told otherwise.
//! - **State keyed by path**: breakpoints, the Reference tab, the editor's
//!   undo history. A renumber hands a path to ANOTHER device, so following
//!   the path would put a breakpoint - or a Ctrl+Z - in the wrong device's
//!   code.
//! - **Code elsewhere naming the old module path** - only the user can fix it,
//!   so they are told where it is.

use super::AppIde;
use crate::project_tree::logic::ConfigSync;
use eframe::egui;
use std::collections::{BTreeMap, BTreeSet};

/// The follow-up still to do, carried across frames.
#[derive(Default)]
pub(super) struct ConfigMoves {
    /// Paths whose workspace copy and rust-analyzer state are behind the tree.
    pending: Vec<String>,
    /// Paths that are gone, to close in rust-analyzer AFTER the workspace
    /// write - closing first lets a flush open them again.
    close: Vec<String>,
    /// Closed by the write this frame asked for.
    close_after_write: Vec<String>,
    /// What the user has to hear about, until they close it.
    notice: Option<Vec<String>>,
}

/// `src/pins/configs/i2c1/device2_imu.rs` → `configs::i2c1::device2_imu`,
/// `…/i2c1/mod.rs` → `configs::i2c1`.
fn module_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("src/pins/")?.strip_suffix(".rs")?;
    let rest = rest.strip_suffix("/mod").unwrap_or(rest);
    Some(rest.replace('/', "::"))
}

/// Does `line` hold `word` as a whole name - not as part of a longer one?
fn names(line: &str, word: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(word).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(ident)
            && !line[at + word.len()..].chars().next().is_some_and(ident)
    })
}

/// The 1-based lines of `text` that are code, not a `//` comment - an
/// example the IDE wrote in a comment is not the user's code to fix.
fn code_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
        .map(|(i, l)| (i + 1, l))
}

/// Does `line` name the module `path` (`configs::i2c1::device2`): the path
/// itself, or its last name inside a `use …configs::i2c1::{…}` group?
fn names_module(line: &str, path: &str) -> bool {
    if names(line, path) {
        return true;
    }
    let Some((parent, last)) = path.rsplit_once("::") else {
        return false;
    };
    line.contains(&format!("{parent}::{{")) && names(line, last)
}

/// Every place in the user's code that still names what the sync moved or
/// dropped, as lines for the notice. `sources` = `(path, text)` of everything
/// to search. A text search: it finds the usual spellings, and the compiler
/// reports whatever it misses.
pub(super) fn stale_references(report: &ConfigSync, sources: &[(&str, &str)]) -> Vec<String> {
    // (module path, hint, the file it names - which is not searched)
    let mut modules: Vec<(String, String, String)> = Vec::new();
    let taken: BTreeSet<&str> = report.moved.iter().map(|(_, n)| n.as_str()).collect();
    for (old, new) in &report.moved {
        if let (Some(o), Some(n)) = (module_path(old), module_path(new))
            && o != n
        {
            modules.push((o, format!("now `{n}`"), new.clone()));
        }
    }
    for gone in &report.removed {
        let Some(m) = module_path(gone) else {
            continue;
        };
        let hint = if taken.contains(gone.as_str()) {
            "that name is ANOTHER device's file now".to_owned()
        } else {
            "removed".to_owned()
        };
        modules.push((m, hint, gone.clone()));
    }
    // A bus from before the folders: its file carried the address. The bus
    // file itself (`i2c1.rs`), not one of its devices (`i2c1_oled.rs`).
    let buses: Vec<String> = report
        .migrated
        .iter()
        .filter_map(|(old, _)| {
            let bus = old.strip_prefix("src/pins/configs/")?.strip_suffix(".rs")?;
            (!bus.contains('_') && !bus.contains('/')).then(|| bus.to_owned())
        })
        .collect();
    let address_hint = |bus: &str| {
        format!("each device has its own now: `configs::{bus}::device<n>_<name>::DEVICE_ADDRESS`")
    };

    let mut out = Vec::new();
    for (path, text) in sources {
        for (line_no, line) in code_lines(text) {
            for (module, hint, file) in &modules {
                if *path != file && names_module(line, module) {
                    out.push(format!("{path}:{line_no}  `{module}` - {hint}"));
                }
            }
            for bus in &buses {
                let at = format!("configs::{bus}");
                let hit = names(line, &format!("{at}::DEVICE_ADDRESS"))
                    || line.contains(&format!("{at}::*"))
                    || (line.contains(&format!("{at}::{{")) && names(line, "DEVICE_ADDRESS"));
                if hit {
                    out.push(format!(
                        "{path}:{line_no}  `{at}::DEVICE_ADDRESS` - {}",
                        address_hint(bus)
                    ));
                }
            }
        }
    }
    // The bus's own init, moved in with the editable half it had: a bare
    // `DEVICE_ADDRESS` there named the const the bus no longer has.
    for bus in &buses {
        let file = format!("src/pins/configs/{bus}/mod.rs");
        let Some((_, text)) = sources.iter().find(|(p, _)| *p == file) else {
            continue;
        };
        let user_half = text
            .find("// <<< GENERATED END >>>")
            .map_or(0, |end| text[..end].lines().count());
        for (line_no, line) in code_lines(text) {
            if line_no > user_half && names(line, "DEVICE_ADDRESS") {
                out.push(format!(
                    "{file}:{line_no}  `DEVICE_ADDRESS` - {}",
                    address_hint(bus)
                ));
            }
        }
    }
    // Inside a device file moved one folder down, `super` is the bus now.
    let in_configs = |p: &str| p.strip_prefix("src/pins/configs/").unwrap_or(p).to_owned();
    for (old, new) in &report.moved {
        let deeper = !in_configs(old).contains('/')
            && in_configs(new).contains('/')
            && !new.ends_with("/mod.rs");
        if !deeper {
            continue;
        }
        if let Some((_, text)) = sources.iter().find(|(p, _)| p == new) {
            for (line_no, line) in code_lines(text) {
                if names(line, "super") {
                    out.push(format!(
                        "{new}:{line_no}  `super` - this file moved one folder down, `super` is its bus now"
                    ));
                }
            }
        }
    }
    out
}

/// Breakpoints after the moves: each follows its FILE, and one on a path
/// another file took over goes - it was in code that is not there any more.
pub(super) fn rekey_breakpoints(
    bps: &BTreeMap<String, BTreeSet<u32>>,
    report: &ConfigSync,
) -> BTreeMap<String, BTreeSet<u32>> {
    let from: BTreeMap<&str, &str> = report
        .moved
        .iter()
        .map(|(o, n)| (o.as_str(), n.as_str()))
        .collect();
    let taken: BTreeSet<&str> = report.moved.iter().map(|(_, n)| n.as_str()).collect();
    let mut out = BTreeMap::new();
    for (path, lines) in bps {
        if let Some(to) = from.get(path.as_str()) {
            out.insert((*to).to_owned(), lines.clone());
        } else if !taken.contains(path.as_str()) && !report.removed.contains(path) {
            out.insert(path.clone(), lines.clone());
        }
    }
    out
}

/// The Reference tab's file after the moves: its file's new path, or none
/// when its file went - or another file took its path.
pub(super) fn rekey_reference(current: Option<String>, report: &ConfigSync) -> Option<String> {
    let r = current?;
    if let Some((_, to)) = report.moved.iter().find(|(o, _)| *o == r) {
        return Some(to.clone());
    }
    let taken = report.moved.iter().any(|(_, n)| *n == r);
    (!taken && !report.removed.contains(&r)).then_some(r)
}

/// Of the paths the sync left, the ones rust-analyzer can close: not one the
/// tree holds again, under another file's text.
pub(super) fn still_to_close(left: Vec<String>, tree: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in left {
        if !tree.iter().any(|(q, _)| *q == p) && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Where the copy of `name` goes in `dir`: `<name>.txt`, or the first free
/// `<name>.<n>.txt` - never over a copy that says something else. `None`
/// when a copy with exactly `body` is already there (it is `Some(existing)`
/// in the second slot then).
fn backup_path(
    dir: &std::path::Path,
    name: &str,
    body: &str,
) -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    for n in 1.. {
        let file = if n == 1 {
            dir.join(format!("{name}.txt"))
        } else {
            dir.join(format!("{name}.{n}.txt"))
        };
        match std::fs::read(&file) {
            Ok(bytes) if bytes == body.as_bytes() => return (None, Some(file)),
            Ok(_) => continue,
            Err(_) => return (Some(file), None),
        }
    }
    unreachable!("an unbounded counter always reaches a free name")
}

impl AppIde {
    /// Right after `sync_config_files`, with what it reported.
    pub(super) fn follow_config_sync(&mut self, report: ConfigSync) {
        if !report.left_a_path() && report.conflicts.is_empty() && report.unplaced.is_empty() {
            return;
        }
        self.breakpoints = rekey_breakpoints(&self.breakpoints, &report);
        self.reference_file = rekey_reference(self.reference_file.take(), &report);
        self.forget_undo(&report);

        let mut lines: Vec<String> = Vec::new();
        let backups = self.back_up_before_folders(&report);
        if !report.migrated.is_empty() {
            lines.push(
                "I2C buses are folders now: each bus's init is in pins/configs/<bus>/mod.rs, and every device has a file of its own there holding its DEVICE_ADDRESS. Your code moved with the files:"
                    .to_owned(),
            );
            for (old, new) in &report.moved {
                if report.migrated.iter().any(|(m, _)| m == old) {
                    lines.push(format!("    {old}  ->  {new}"));
                }
            }
        }
        for (path, _) in &report.conflicts {
            lines.push(format!(
                "{path} was dropped: its folder's mod.rs was already there, and both at once do not compile."
            ));
        }
        for (path, _) in &report.unplaced {
            lines.push(format!(
                "{path} was dropped: no device on its bus matches it, so it could not be moved into the bus folder."
            ));
        }
        if !backups.is_empty() {
            lines.push("A copy of each file as it was before is kept outside src/:".to_owned());
            lines.extend(backups.into_iter().map(|b| format!("    {b}")));
        }
        let sources: Vec<(&str, &str)> =
            std::iter::once(("src/main.rs", self.generated_code.as_str()))
                .chain(
                    self.project_tree
                        .user_src_files
                        .iter()
                        .filter(|(p, _)| p.ends_with(".rs"))
                        .map(|(p, c)| (p.as_str(), c.as_str())),
                )
                .collect();
        let stale = stale_references(&report, &sources);
        if !stale.is_empty() {
            lines.push(
                "Code that still names the old place - fix these by hand (a text search; the compiler reports any it missed):"
                    .to_owned(),
            );
            lines.extend(stale.into_iter().map(|s| format!("    {s}")));
        }
        if !lines.is_empty() {
            self.config_moves
                .notice
                .get_or_insert_with(Vec::new)
                .extend(lines);
        }

        let m = &mut self.config_moves;
        for (old, new) in &report.moved {
            m.pending.push(old.clone());
            m.pending.push(new.clone());
            m.close.push(old.clone());
        }
        for p in &report.removed {
            m.pending.push(p.clone());
            m.close.push(p.clone());
        }
    }

    /// The editor's undo history for every path that now holds ANOTHER file,
    /// or none: egui keeps it per widget, and a Ctrl+Z there would write the
    /// old file's text over the new one.
    fn forget_undo(&self, report: &ConfigSync) {
        let paths = report
            .removed
            .iter()
            .chain(report.moved.iter().map(|(_, n)| n));
        for path in paths {
            for view in ["code_editor", "reference_editor"] {
                let Some(id) = self.fold_ids.get(&format!("{view}:user:{path}")) else {
                    continue;
                };
                if let Some(mut state) = egui::TextEdit::load_state(&self.egui_ctx, *id) {
                    state.clear_undoer();
                    state.store(&self.egui_ctx, *id);
                }
            }
        }
    }

    /// Every file the sync moved out of the old layout or dropped for good,
    /// as it was, written as `.txt` under `<project>/.rustonchip/pre-folder/` -
    /// never as `.rs`: a Save deletes every `.rs` in the project the tree does
    /// not hold. A copy already there with the same text is listed, not
    /// written again; one with other text is kept, and this one gets the next
    /// free name. Returns the copies.
    fn back_up_before_folders(&self, report: &ConfigSync) -> Vec<String> {
        let Some(dir) = self.project_dir.as_ref() else {
            return Vec::new();
        };
        let to = dir.join(".rustonchip").join("pre-folder");
        let mut copies = Vec::new();
        let all = report
            .migrated
            .iter()
            .chain(&report.conflicts)
            .chain(&report.unplaced);
        for (path, body) in all {
            let Some(name) = path.rsplit('/').next() else {
                continue;
            };
            match backup_path(&to, name, body) {
                (_, Some(existing)) => copies.push(existing.display().to_string()),
                (Some(file), None) => {
                    if std::fs::create_dir_all(&to).is_ok() && std::fs::write(&file, body).is_ok() {
                        copies.push(file.display().to_string());
                    }
                }
                (None, None) => {}
            }
        }
        copies
    }

    /// Once per frame, before the workspace write: ask for that write when a
    /// sync left the workspace behind - unless a Save flush is still writing
    /// it, which could put an old path back after the prune.
    pub(super) fn config_moves_frame(&mut self) {
        if self.config_moves.pending.is_empty()
            || self
                .lsp_flush_in_flight
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let pending = std::mem::take(&mut self.config_moves.pending);
        if let Ok(mut hashes) = self.flushed_hashes.lock() {
            for p in &pending {
                hashes.remove(p);
            }
        }
        self.workspace_write_requested = true;
        let close = std::mem::take(&mut self.config_moves.close);
        self.config_moves.close_after_write.extend(close);
    }

    /// Right after the workspace write: the old paths are gone from disk, so
    /// rust-analyzer can let them go too.
    pub(super) fn config_moves_after_write(&mut self) {
        if self.config_moves.close_after_write.is_empty() {
            return;
        }
        let left = std::mem::take(&mut self.config_moves.close_after_write);
        let close = still_to_close(left, &self.project_tree.user_src_files);
        let mut lsp = self.lsp_state.lock().unwrap();
        for p in close {
            lsp.did_close(&p);
        }
    }

    pub(super) fn show_config_moves_notice(&mut self, ui: &egui::Ui) {
        let Some(lines) = self.config_moves.notice.as_ref() else {
            return;
        };
        let mut close = false;
        egui::Window::new("I2C device files moved")
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for line in lines {
                            if line.starts_with("    ") {
                                ui.label(
                                    egui::RichText::new(line.trim_start())
                                        .monospace()
                                        .size(11.0),
                                );
                            } else {
                                ui.add_space(4.0);
                                ui.label(line);
                            }
                        }
                    });
                ui.add_space(6.0);
                if ui.button("OK").clicked() {
                    close = true;
                }
            });
        if close {
            self.config_moves.notice = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(moved: &[(&str, &str)], removed: &[&str]) -> ConfigSync {
        ConfigSync {
            moved: moved
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            removed: removed.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn d(n: &str) -> String {
        format!("src/pins/configs/i2c1/{n}.rs")
    }

    /// Each breakpoint follows its FILE: one on a path another file took over
    /// goes (it was in code that moved away), and so does one on a path that
    /// was only removed - each rule on its own.
    #[test]
    fn a_breakpoint_follows_its_file_not_its_path() {
        let mut bps = BTreeMap::new();
        bps.insert(d("device2"), BTreeSet::from([5]));
        bps.insert(d("device3"), BTreeSet::from([9]));
        bps.insert("src/main.rs".to_owned(), BTreeSet::from([1]));
        // device3 moves onto device2's path; device2's own file is not
        // reported removed, so only the takeover rule can drop it.
        let r = report(&[(&d("device3"), &d("device2"))], &[]);
        let out = rekey_breakpoints(&bps, &r);
        assert_eq!(out.get(&d("device2")), Some(&BTreeSet::from([9])));
        assert_eq!(out.get(&d("device3")), None);
        assert_eq!(out.get("src/main.rs"), Some(&BTreeSet::from([1])));
        // device2 removed, nothing taking its path: only the removed rule.
        let r = report(&[], &[&d("device2")]);
        let out = rekey_breakpoints(&bps, &r);
        assert_eq!(out.get(&d("device2")), None);
        assert_eq!(out.get(&d("device3")), Some(&BTreeSet::from([9])));
        // The takeover rule, whatever order the paths sort in: here the file
        // that moves sorts BEFORE the path it takes.
        let r = report(&[(&d("device1_a"), &d("device1_b"))], &[]);
        let mut bps = BTreeMap::new();
        bps.insert(d("device1_a"), BTreeSet::from([3]));
        bps.insert(d("device1_b"), BTreeSet::from([7]));
        let out = rekey_breakpoints(&bps, &r);
        assert_eq!(out.get(&d("device1_b")), Some(&BTreeSet::from([3])));
        assert_eq!(out.len(), 1, "{out:?}");
    }

    /// The Reference tab follows its file, and lets go of a path that went or
    /// now holds another file.
    #[test]
    fn the_reference_file_follows_its_file() {
        let moved = report(&[(&d("device3"), &d("device2"))], &[]);
        assert_eq!(
            rekey_reference(Some(d("device3")), &moved),
            Some(d("device2"))
        );
        assert_eq!(
            rekey_reference(Some(d("device2")), &moved),
            None,
            "taken over"
        );
        let removed = report(&[], &[&d("device1")]);
        assert_eq!(rekey_reference(Some(d("device1")), &removed), None);
        assert_eq!(
            rekey_reference(Some("src/main.rs".into()), &removed),
            Some("src/main.rs".into())
        );
        assert_eq!(rekey_reference(None, &removed), None);
    }

    /// rust-analyzer keeps a path another file now stands on open - its text
    /// is that file's - and each path is closed once.
    #[test]
    fn only_a_path_nothing_holds_is_closed() {
        let tree = vec![(d("device1"), String::new())];
        let out = still_to_close(vec![d("device1"), d("device2"), d("device2")], &tree);
        assert_eq!(out, vec![d("device2")]);
    }

    /// A backup never overwrites one that says something else, and a second
    /// identical one is the first.
    #[test]
    fn a_backup_never_overwrites_another() {
        let dir = std::env::temp_dir().join(format!("eide_backup_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (free, _) = backup_path(&dir, "i2c1.rs", "one");
        let first = free.unwrap();
        assert!(first.ends_with("i2c1.rs.txt"));
        std::fs::write(&first, "one").unwrap();
        assert_eq!(
            backup_path(&dir, "i2c1.rs", "one"),
            (None, Some(first.clone()))
        );
        let (second, _) = backup_path(&dir, "i2c1.rs", "two");
        assert!(second.unwrap().ends_with("i2c1.rs.2.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A module path the move left behind is found as a whole path only; a
    /// bus from before the folders is found by its old `DEVICE_ADDRESS`, and
    /// `super` in a file that moved one folder down.
    #[test]
    fn stale_references_are_whole_paths() {
        let mut r = report(
            &[
                (
                    "src/pins/configs/i2c1_oled.rs",
                    "src/pins/configs/i2c1/device1_oled.rs",
                ),
                ("src/pins/configs/i2c1.rs", "src/pins/configs/i2c1/mod.rs"),
            ],
            &[],
        );
        r.migrated = vec![
            ("src/pins/configs/i2c1_oled.rs".into(), String::new()),
            ("src/pins/configs/i2c1.rs".into(), String::new()),
        ];
        let main = "use pins::configs::i2c1_oled::DEVICE_ADDRESS;\nlet a = pins::configs::i2c1_oledx::A;\nlet b = pins::configs::i2c1::DEVICE_ADDRESS;\n";
        let dev = "use super::i2c1::Handle;\n";
        let out = stale_references(
            &r,
            &[
                ("src/main.rs", main),
                ("src/pins/configs/i2c1/device1_oled.rs", dev),
            ],
        );
        assert!(
            out.iter().any(|l| l.starts_with("src/main.rs:1 ")),
            "{out:?}"
        );
        assert!(
            !out.iter().any(|l| l.starts_with("src/main.rs:2 ")),
            "{out:?}"
        );
        assert!(
            out.iter().any(|l| l.starts_with("src/main.rs:3 ")),
            "{out:?}"
        );
        assert!(
            out.iter()
                .any(|l| l.starts_with("src/pins/configs/i2c1/device1_oled.rs:1 ")),
            "{out:?}"
        );
        // The bus's own module path did not change: nothing to say about it.
        assert!(
            !out.iter().any(|l| l.contains("`configs::i2c1` ")),
            "{out:?}"
        );
    }

    /// The other spellings: a grouped or glob import, a bare
    /// `DEVICE_ADDRESS` in the bus's own moved-in init, a path a renumber
    /// gave to another device - and never the IDE's own example comment.
    #[test]
    fn stale_references_other_spellings() {
        let mut r = report(
            &[
                ("src/pins/configs/i2c1.rs", "src/pins/configs/i2c1/mod.rs"),
                (&d("device3"), &d("device2")),
            ],
            &[&d("device2")],
        );
        r.migrated = vec![("src/pins/configs/i2c1.rs".into(), String::new())];
        let main = "\
use pins::configs::i2c1::{init, DEVICE_ADDRESS};
use pins::configs::i2c1::*;
//     use pins::configs::i2c1::DEVICE_ADDRESS;
use pins::configs::i2c1::{device2, device3};
let x = pins::configs::i2c1::device2::DEVICE_ADDRESS;
";
        let bus = "\
// <<< GENERATED>>>
pub mod device1;
// <<< GENERATED END >>>
pub fn ping(bus: &mut B) { bus.write(DEVICE_ADDRESS, &[]); }
// {HANDLE}.write(DEVICE_ADDRESS, ..)
const OTHER: u8 = MY_DEVICE_ADDRESS + myconfigs::i2c1::device2::X;
";
        let out = stale_references(
            &r,
            &[("src/main.rs", main), ("src/pins/configs/i2c1/mod.rs", bus)],
        );
        let at = |p: &str| out.iter().filter(|l| l.starts_with(p)).count();
        assert_eq!(at("src/main.rs:1 "), 1, "{out:?}");
        assert_eq!(at("src/main.rs:2 "), 1, "{out:?}");
        assert_eq!(at("src/main.rs:3 "), 0, "a comment: {out:?}");
        assert!(at("src/main.rs:4 ") >= 1, "{out:?}");
        assert!(
            out.iter()
                .any(|l| l.starts_with("src/main.rs:5 ") && l.contains("ANOTHER device")),
            "{out:?}"
        );
        assert_eq!(at("src/pins/configs/i2c1/mod.rs:4 "), 1, "{out:?}");
        assert_eq!(at("src/pins/configs/i2c1/mod.rs:5 "), 0, "{out:?}");
        // Part of a longer name is not the name.
        assert_eq!(at("src/pins/configs/i2c1/mod.rs:6 "), 0, "{out:?}");
    }
}
