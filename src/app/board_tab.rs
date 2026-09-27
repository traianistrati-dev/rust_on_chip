//! The Board tab's side of the app: which system is open, putting chips into
//! it, and the canvas.
//!
//! The system is the PARENT folder of a chip project, when that folder has a
//! `system.config` - so opening any chip of a system opens its Board too, and
//! nothing about a chip project changes by being in one. Opening a project
//! that belongs to no system leaves the Board as it was.
//!
//! A chip gets into a system two ways:
//! * **New chip** runs the ordinary New Project dialog; once a chip is picked
//!   there, the project is saved straight INTO the system (no folder dialog)
//!   and listed in `system.config`.
//! * **Add existing project** copies a saved project into the system folder
//!   (files only - no `target/`, no `.git`), or just lists one already there.
//!
//! `system.config` is shared state - another window of the same system, or a
//! hand edit, can change it at any time - so every write re-reads it and
//! applies only this window's own change ([`model::carry_positions`]).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use eframe::egui;
use egui_phosphor::regular as ph;

use super::AppIde;
use crate::panels::board::links::{self, Dir, Resolved};
use crate::panels::board::model::{self, Link, LinkEnd, SystemConfig, View};
use crate::panels::board::parts::{self, Part, PartBus, PartPin};
use crate::panels::board::snapshot::{self, ChipView};
use crate::panels::board::{gui, layout};
use crate::panels::mcu_module::mcu::gui::modules::module_color;

/// Everything the Board tab keeps between frames.
pub(crate) struct BoardState {
    /// The open system's folder, without a `\\?\` prefix.
    pub root: Option<PathBuf>,
    pub config: SystemConfig,
    /// One view per chip, in `config` order, read from disk. The chip open in
    /// this window is rebuilt from the live `Mcu` every frame instead.
    pub views: Vec<ChipView>,
    /// The project in this window is a "New chip" with its chip picked: its
    /// first Save goes into `root` and lists it there.
    pub adding_chip: bool,
    /// "New chip" passed the unsaved-changes gate and the New Project dialog
    /// is on its way; the dialog's OK turns it into `adding_chip`. Not
    /// before: until then the OLD project is still loaded, and a Save of it
    /// must not go into the system.
    pub new_chip_intent: bool,
    /// Clicked on the Board, acted on next frame at the same gates the
    /// toolbar's New / Open go through - the canvas draws after them.
    pub new_chip_request: bool,
    pub open_request: Option<PathBuf>,
    /// The last thing to tell the user: text, and whether it is an error.
    pub notice: Option<(String, bool)>,
    /// The canvas view, and whether the user has zoomed or panned it (until
    /// then it follows the frames).
    pub scene_rect: egui::Rect,
    pub view_adjusted: bool,
    /// Frame number the tab last drew on. A gap means it was hidden, and the
    /// system is read again: another window may have changed it meanwhile.
    pub last_frame: u64,
    /// Frames dragged here and not written yet (folder names, lower-cased).
    pub moved: BTreeSet<String>,
    /// A link being made: the module clicked first.
    pub arming: Option<LinkEnd>,
    /// The link picked in the list or on the canvas - by identity, so an
    /// edit from another window cannot move the pick onto another link.
    pub selected_link: Option<Link>,
    /// Windows started on a chip and maybe not up yet: a new window claims
    /// its folder only after it has started, and until then a second "Open
    /// in new window" would start a second window on the same chip.
    pub new_windows: Vec<(PathBuf, std::process::Child, std::time::Instant)>,
    /// The external part being edited, when its window is open.
    pub part_editor: Option<PartEditor>,
}

/// The External part window's state.
pub(crate) struct PartEditor {
    /// The name the part had when the window opened; `None` for a new part.
    pub original: Option<String>,
    /// The part as it was when the window opened - a Save is refused when the
    /// file's copy has changed since (another window edited it), rather than
    /// writing this older copy over that edit.
    pub opened: Option<Part>,
    pub part: Part,
    /// Why Save was refused.
    pub error: Option<String>,
}

impl Default for BoardState {
    fn default() -> Self {
        Self {
            root: None,
            config: SystemConfig::default(),
            views: Vec::new(),
            adding_chip: false,
            new_chip_intent: false,
            new_chip_request: false,
            open_request: None,
            notice: None,
            scene_rect: egui::Rect::NOTHING,
            view_adjusted: false,
            last_frame: 0,
            moved: BTreeSet::new(),
            arming: None,
            selected_link: None,
            new_windows: Vec::new(),
            part_editor: None,
        }
    }
}

/// How listing a folder as a chip went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listed {
    Added,
    Already,
    /// `system.config` could not be read or written (the notice says why).
    Failed,
    /// A name `system.config` cannot hold (see [`model::listable`]).
    BadName,
}

/// Why a folder cannot be added as a chip, or `None` when it can.
fn not_a_chip(src: &Path, root: &Path) -> Option<&'static str> {
    if root.starts_with(src) {
        Some("That folder holds the system itself.")
    } else if model::is_system_root(src) {
        Some("That folder is a system, not a chip project.")
    } else if !is_project(src) {
        Some("Not a chip project - it has no Cargo.toml or src/main.rs.")
    } else {
        None
    }
}

/// Each frame's size with no links - what placing a frame that has no spot
/// yet goes by.
fn plain_sizes(views: &[ChipView]) -> Vec<egui::Vec2> {
    views
        .iter()
        .map(|v| layout::frame_layout(v, &layout::Arrange::plain(v)).size)
        .collect()
}

/// Why a chip cannot be opened in a new window, or `None` when it can. The
/// new window takes its folder from the command line, which asks for a
/// `Cargo.toml`.
fn new_window_refusal(path: &Path, dir: &str, open_here: bool) -> Option<String> {
    if open_here {
        Some(format!("{dir} is the chip open in this window."))
    } else if !path.is_dir() {
        Some(format!("{dir} is not there any more - moved or deleted?"))
    } else if !path.join("Cargo.toml").is_file() {
        Some(format!(
            "{dir} has no Cargo.toml - a new window cannot open it."
        ))
    } else {
        None
    }
}

fn is_project(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file() || dir.join("src").join("main.rs").is_file()
}

/// How long a window just started is taken to be still starting: enough
/// for a debug build to come up and claim its folder.
const NEW_WINDOW_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

const BAD_NAME: &str = "system.config cannot list a folder whose name contains `=` or starts with \
                        `#` or `@` - rename the folder first.";

impl AppIde {
    fn board_notice(&mut self, text: impl Into<String>, error: bool) {
        self.board.notice = Some((text.into(), error));
    }

    /// Open the system at `root`: read its config and its chips.
    pub(super) fn board_open_system(&mut self, root: PathBuf) {
        self.board_flush_positions();
        let root = model::plain(&root);
        match model::load(&root) {
            Ok(cfg) => {
                self.board.root = Some(root);
                self.board.config = cfg;
                self.board.moved.clear();
                self.board.view_adjusted = false;
                // A half-made link, a picked row, a notice: all about the
                // system left.
                self.board.arming = None;
                self.board.selected_link = None;
                self.board.notice = None;
                self.board.part_editor = None;
                self.board_refresh();
            }
            Err(e) => self.board_notice(e, true),
        }
    }

    /// Stop showing the system. Nothing on disk changes.
    fn board_close_system(&mut self) {
        self.board_flush_positions();
        let scene_rect = self.board.scene_rect;
        self.board = BoardState {
            scene_rect,
            ..BoardState::default()
        };
    }

    /// Read every chip again from disk.
    fn board_refresh(&mut self) {
        let Some(root) = self.board.root.clone() else {
            self.board.views.clear();
            return;
        };
        self.board.views = self
            .board
            .config
            .chips
            .iter()
            .map(|c| snapshot::read_chip(&root, &c.dir, &self.mcu_registry))
            .collect();
        self.board_place_unplaced();
    }

    /// Give each frame without a spot the one it is drawn at - so it stays
    /// there while another frame is dragged.
    fn board_place_unplaced(&mut self) {
        let frames = self.board.config.frames();
        let spots: Vec<(Option<(f32, f32)>, egui::Vec2)> = frames
            .iter()
            .zip(plain_sizes(&self.board_frame_views()))
            .map(|((_, pos), s)| (*pos, s))
            .collect();
        let at = layout::frame_positions(&spots);
        for ((id, pos), p) in frames.iter().zip(at) {
            if pos.is_none() {
                self.board.config.set_pos(id, (p.x, p.y));
            }
        }
    }

    /// One view per frame, in `config.frames()` order: each chip as last read
    /// from disk, then each external part from its description. Looked up by
    /// name, so a chip list re-read before its views were cannot shift one
    /// frame's picture onto another's spot.
    fn board_frame_views(&self) -> Vec<ChipView> {
        self.board
            .config
            .chips
            .iter()
            .map(|c| {
                self.board
                    .views
                    .iter()
                    .find(|v| v.dir.eq_ignore_ascii_case(&c.dir))
                    .cloned()
                    .unwrap_or_else(|| ChipView::broken(&c.dir, "Not read yet - press refresh"))
            })
            .chain(self.board.config.parts.iter().map(Part::view))
            .collect()
    }

    /// Re-read the system from disk (writing this window's moves first).
    fn board_sync(&mut self) {
        if !self.board.moved.is_empty() {
            self.board_edit_config(|_| false);
            return;
        }
        if let Some(root) = self.board.root.clone() {
            match model::load(&root) {
                Ok(cfg) => self.board.config = cfg,
                Err(e) => self.board_notice(e, true),
            }
        }
        self.board_refresh();
    }

    /// Change `system.config`: re-read it, carry this window's frame positions
    /// over, apply `edit`, write it back when anything changed.
    ///
    /// Returns what `edit` returned, or `None` when nothing could be written -
    /// the file could not be read (never overwritten from this window's older
    /// copy: a deleted file stays deleted, a hand-edited one is not clobbered)
    /// or the write failed. The error notice is up then, and memory holds what
    /// is on disk, so nothing shows as done that was not.
    fn board_edit_config(&mut self, edit: impl FnOnce(&mut SystemConfig) -> bool) -> Option<bool> {
        let root = self.board.root.clone()?;
        let mut cfg = match model::load(&root) {
            Ok(cfg) => cfg,
            Err(e) => {
                self.board_notice(format!("{e} - nothing was saved"), true);
                return None;
            }
        };
        let on_disk = cfg.clone();
        let carried = model::carry_positions(&mut cfg, &self.board.config, &self.board.moved);
        let changed = edit(&mut cfg);
        self.board.moved.clear();
        let dirs = |c: &SystemConfig| -> Vec<String> {
            c.chips.iter().map(|c| c.dir.to_ascii_lowercase()).collect()
        };
        let chips_changed = dirs(&cfg) != dirs(&self.board.config);
        let saved = if carried || changed {
            model::save(&root, &cfg)
        } else {
            Ok(())
        };
        let result = match saved {
            Ok(()) => {
                self.board.config = cfg;
                Some(changed)
            }
            Err(e) => {
                self.board_notice(e, true);
                self.board.config = on_disk;
                None
            }
        };
        // Reading every chip again costs a rebuild of each: only when the
        // list of chips is not the one the views were read for.
        if chips_changed || self.board.views.len() != self.board.config.chips.len() {
            self.board_refresh();
        } else {
            self.board_place_unplaced();
        }
        result
    }

    /// Write dragged frames down, if any were dragged.
    fn board_flush_positions(&mut self) {
        if !self.board.moved.is_empty() {
            self.board_edit_config(|_| false);
        }
    }

    /// The picked link's place in `config.links`, if it is still there.
    fn board_selected_index(&self) -> Option<usize> {
        let s = self.board.selected_link.as_ref()?;
        self.board.config.links.iter().position(|l| l.same_as(s))
    }

    /// A chip folder was renamed by Rename Project: follow it in the system.
    pub(super) fn board_chip_renamed(&mut self, old_dir: &Path, new_dir: &Path) {
        let (Some(root), Some(old), Some(new)) = (
            model::system_root_of(old_dir),
            old_dir.file_name().and_then(|n| n.to_str()),
            new_dir.file_name().and_then(|n| n.to_str()),
        ) else {
            return;
        };
        if !self
            .board
            .root
            .as_deref()
            .is_some_and(|r| model::same_dir(r, &root))
        {
            return;
        }
        let (old, new) = (old.to_owned(), new.to_owned());
        if self.board.config.part(&new).is_some() {
            self.board_notice(
                format!(
                    "{new} is the name of a part of the system, so the system still lists the chip \
                     as {old} - rename the part, then add the chip again."
                ),
                true,
            );
            return;
        }
        if self.board_edit_config(|cfg| model::rename_chip(cfg, &old, &new)) == Some(true) {
            if self
                .board
                .arming
                .as_ref()
                .is_some_and(|e| e.chip.eq_ignore_ascii_case(&old))
            {
                self.board.arming = None;
            }
            self.board.selected_link = None;
        }
    }

    /// List `dir` (a folder directly under the root) as a chip, to the right
    /// of the others.
    fn board_register(&mut self, dir: &str) -> Listed {
        if !model::listable(dir) {
            return Listed::BadName;
        }
        let views = self.board_frame_views();
        let sizes: HashMap<String, egui::Vec2> = views
            .iter()
            .zip(plain_sizes(&views))
            .map(|(v, s)| (v.dir.to_ascii_lowercase(), s))
            .collect();
        let added = self.board_edit_config(|cfg| {
            let mut spots: Vec<(Option<(f32, f32)>, egui::Vec2)> = cfg
                .frames()
                .into_iter()
                .map(|(id, pos)| {
                    let size = sizes.get(&id.to_ascii_lowercase()).copied();
                    (pos, size.unwrap_or(egui::vec2(layout::FRAME_W, 0.0)))
                })
                .collect();
            spots.push((None, egui::vec2(layout::FRAME_W, 0.0)));
            let at = layout::frame_positions(&spots)[spots.len() - 1];
            cfg.add(dir, Some((at.x, at.y)))
        });
        match added {
            Some(true) => Listed::Added,
            Some(false) => Listed::Already,
            None => Listed::Failed,
        }
    }

    /// `board_register` plus the notice that says how it went.
    fn board_register_and_say(&mut self, dir: &str) {
        match self.board_register(dir) {
            Listed::Added => self.board_notice(format!("Added {dir} to the system."), false),
            Listed::Already => self.board_notice(format!("{dir} is already in the system."), false),
            Listed::BadName => self.board_notice(BAD_NAME, true),
            Listed::Failed => {}
        }
    }

    /// The folder of the chip open in this window, when it sits in the system.
    fn board_open_chip_dir(&self) -> Option<String> {
        let root = self.board.root.as_deref()?;
        let dir = model::plain(self.project_dir.as_deref()?);
        model::same_dir(dir.parent()?, root)
            .then(|| dir.file_name()?.to_str().map(str::to_owned))
            .flatten()
    }

    /// A project was opened: show its system, if it is in one. Called at the
    /// end of `load_project_from_dir`.
    pub(super) fn board_follow_project(&mut self, project_dir: &Path) {
        self.board.adding_chip = false;
        self.board.new_chip_intent = false;
        // Whatever the last notice said was about the project just left.
        self.board.notice = None;
        if let Some(root) = model::system_root_of(project_dir) {
            if self
                .board
                .root
                .as_deref()
                .is_some_and(|r| model::same_dir(r, &root))
            {
                // Same system: re-read it, the open may be a branch switch.
                self.board_sync();
            } else {
                self.board_open_system(root);
            }
        }
    }

    /// Where a "New chip" project is created on its first Save: the system
    /// root, without asking. `None` for any other new project.
    pub(super) fn board_new_chip_parent(&self) -> Option<PathBuf> {
        if self.board.adding_chip && self.project_dir.is_none() {
            self.board.root.clone()
        } else {
            None
        }
    }

    /// A Save just gave a new project its folder. If it was a "New chip", list
    /// it in the system.
    pub(super) fn board_register_saved_chip(&mut self) {
        if !std::mem::take(&mut self.board.adding_chip) {
            return;
        }
        if let Some(dir) = self.board_open_chip_dir() {
            self.board_register_and_say(&dir);
        }
    }

    fn board_new_system(&mut self) {
        let Some(folder) = rfd::FileDialog::new()
            .set_title("New system - choose or create an empty folder for it")
            .pick_folder()
        else {
            return;
        };
        let folder = model::plain(&folder);
        if model::is_system_root(&folder) {
            self.board_open_system(folder);
            self.board_notice("That folder already is a system - opened it.", false);
            return;
        }
        if folder.join("Cargo.toml").is_file() {
            self.board_notice(
                "That folder is a chip project. A system keeps its chips in subfolders - pick \
                 or create an empty folder.",
                true,
            );
            return;
        }
        if let Err(e) = model::save(&folder, &SystemConfig::default()) {
            self.board_notice(e, true);
            return;
        }
        self.board_open_system(folder.clone());
        let name = folder
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("system");
        self.board_notice(format!("Created the system {name}."), false);
        // The chip open right now already lives in that folder: it is the
        // system's first chip, not something to add by hand.
        if let Some(dir) = self.board_open_chip_dir() {
            self.board_register_and_say(&dir);
        }
    }

    fn board_open_system_dialog(&mut self) {
        let Some(folder) = rfd::FileDialog::new()
            .set_title("Open system - pick the folder that holds system.config")
            .pick_folder()
        else {
            return;
        };
        if model::is_system_root(&folder) {
            self.board_open_system(folder);
        } else if let Some(root) = model::system_root_of(&folder) {
            // A chip folder: its system is the one meant.
            self.board_open_system(root);
        } else {
            self.board_notice(
                format!(
                    "No {} in that folder, or in the one above it.",
                    model::FILE_NAME
                ),
                true,
            );
        }
    }

    fn board_add_existing(&mut self) {
        let Some(root) = self.board.root.clone() else {
            return;
        };
        let Some(src) = rfd::FileDialog::new()
            .set_title("Add existing project - pick a chip project folder")
            .pick_folder()
        else {
            return;
        };
        let src = model::plain(&src);
        if let Some(why) = not_a_chip(&src, &root) {
            self.board_notice(why, true);
            return;
        }
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("chip")
            .to_owned();
        // Already in the system folder: list it, copy nothing.
        if src.parent().is_some_and(|p| model::same_dir(p, &root)) {
            self.board_register_and_say(&name);
            return;
        }
        // A folder of our own making: `new_project_dir` sanitises the name
        // and never returns one that exists.
        // Not a name a part already has: chips and parts share names.
        let dest = super::project_io::new_project_dir(&root, &name, |p| {
            p.exists()
                || p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| self.board.config.has_id(n))
        });
        match super::clone_project_dialog::copy_tree(&src, &dest) {
            Ok(n) => {
                let dir = dest
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_owned();
                if self.board_register(&dir) == Listed::Added {
                    self.board_notice(
                        format!(
                            "Copied {n} files from {} into {dir} - the saved files; its target/ \
                             and .git were left behind. The original is unchanged.",
                            src.display()
                        ),
                        false,
                    );
                }
            }
            Err(e) => {
                // Half a project would sit in the system folder unlisted, and
                // push the next attempt to `<name>_1`. It did not exist before
                // this copy, so nothing of the user's goes with it.
                let _ = std::fs::remove_dir_all(&dest);
                self.board_notice(format!("Copy into {} failed: {e}", dest.display()), true);
            }
        }
    }

    fn board_remove(&mut self, dir: &str) {
        let is_part = self.board.config.part(dir).is_some();
        if self.board_edit_config(|cfg| cfg.remove(dir)) == Some(true) {
            if self
                .board
                .arming
                .as_ref()
                .is_some_and(|e| e.chip.eq_ignore_ascii_case(dir))
            {
                self.board.arming = None;
            }
            if is_part {
                self.board_notice(format!("Removed the part {dir} and its links."), false);
                if self
                    .board
                    .part_editor
                    .as_ref()
                    .and_then(|e| e.original.as_deref())
                    .is_some_and(|o| o.eq_ignore_ascii_case(dir))
                {
                    self.board.part_editor = None;
                }
            } else {
                self.board_notice(
                    format!("Removed {dir} from the system. Its folder and files are still there."),
                    false,
                );
            }
        }
    }

    /// Open a chip of the system in this window - through the same gate as
    /// Open Recent - unless its folder is gone or holds no project: opening
    /// that would keep the previous chip's pins under the missing folder's
    /// name, and the next Save would write them there.
    pub(super) fn board_open_chip(&mut self, root: &Path, dir: &str, ctx: &egui::Context) {
        let path = root.join(dir);
        if !path.is_dir() {
            self.board_notice(
                format!("{dir} is not there any more - moved or deleted?"),
                true,
            );
            // Renamed or taken out from another window, maybe: read the
            // system again, not only the chips.
            self.board_sync();
        } else if !is_project(&path) {
            self.board_notice(
                format!("{dir} is not a chip project - it has no Cargo.toml or src/main.rs."),
                true,
            );
        } else {
            // A double-click on a module arms a link on its way to opening.
            self.board.arming = None;
            self.board.open_request = Some(path);
            ctx.request_repaint();
        }
    }

    /// Open a chip of the system in a window of its own: another instance of
    /// the IDE, started on the chip's folder. Every window has its own build
    /// workspace and rust-analyzer, so two chips are worked on side by side.
    fn board_open_in_new_window(&mut self, root: &Path, dir: &str) {
        let path = root.join(dir);
        let open_here = self
            .board_open_chip_dir()
            .is_some_and(|d| d.eq_ignore_ascii_case(dir));
        if let Some(why) = new_window_refusal(&path, dir, open_here) {
            self.board_notice(why, true);
            return;
        }
        // A window still starting has not claimed its folder yet, so the
        // probe below cannot see it. Past the grace period (or once it has
        // exited) it has, and the probe takes over.
        self.board.new_windows.retain_mut(|(_, child, at)| {
            at.elapsed() < NEW_WINDOW_GRACE && matches!(child.try_wait(), Ok(None))
        });
        if self
            .board
            .new_windows
            .iter()
            .any(|(p, _, _)| model::same_dir(p, &path))
        {
            self.board_notice(format!("{dir} is still opening in a new window…"), false);
            return;
        }
        // Probing is claiming: the claim is dropped at once, the new window
        // takes it. A window that grabs it in between gets the busy banner.
        if matches!(
            crate::workspace::claim_project(&path),
            crate::workspace::ProjectClaim::Busy
        ) {
            self.board_notice(format!("{dir} is already open in another window."), true);
            return;
        }
        let started = std::env::current_exe().and_then(|exe| {
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("--project")
                .arg(&path)
                // Its own window, not this one's console (see
                // `attach_parent_console`).
                .env(crate::build::SPAWNED_WINDOW_ENV, "1")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const DETACHED_PROCESS: u32 = 0x0000_0008;
                const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
                cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            }
            cmd.spawn()
        });
        match started {
            Ok(child) => {
                self.board
                    .new_windows
                    .push((path, child, std::time::Instant::now()));
                self.board_notice(format!("Opening {dir} in a new window…"), false);
            }
            Err(e) => self.board_notice(format!("Couldn't start a new window: {e}"), true),
        }
    }

    /// The system's chips, as a row of buttons under the MCU tab's chip name:
    /// the open one lit, a click on another opens it here (the unsaved-changes
    /// prompt first, like any open), its menu opens it in a window of its own.
    pub(super) fn show_system_chip_row(&mut self, ui: &mut egui::Ui) {
        let Some(root) = self.board.root.clone() else {
            return;
        };
        if self.board.config.chips.is_empty() {
            return;
        }
        let open = self.board_open_chip_dir();
        let name = root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("system")
            .to_owned();
        // The open chip says what it is NOW - the Board's disk copy of it is
        // as old as the last read.
        let live = self
            .mcu
            .as_ref()
            .map(|m| format!("{} · {}", self.selected_label(), m.runtime.as_token()));
        let chips: Vec<(String, String, bool)> = self
            .board
            .config
            .chips
            .iter()
            .map(|c| {
                let view = self
                    .board
                    .views
                    .iter()
                    .find(|v| v.dir.eq_ignore_ascii_case(&c.dir));
                let is_open = open
                    .as_deref()
                    .is_some_and(|d| d.eq_ignore_ascii_case(&c.dir));
                let hint = match (view, &live) {
                    (_, Some(live)) if is_open => live.clone(),
                    (Some(v), _) => match &v.problem {
                        Some(p) => p.clone(),
                        None if v.chip.is_empty() => "Unknown chip".to_owned(),
                        None => format!("{} · {}", v.chip, v.runtime.map_or("", |r| r.as_token())),
                    },
                    (None, _) => String::new(),
                };
                // Drawn faded, not disabled: the folder may be back by now,
                // and a click re-reads the system either way.
                let gone = view.is_some_and(|v| {
                    v.problem.as_deref() == Some("Folder not found")
                        || v.problem
                            .as_deref()
                            .is_some_and(|p| p.starts_with("Not a project"))
                });
                (c.dir.clone(), hint, gone)
            })
            .collect();
        // Like the Board's New chip: an open taken while a save runs would be
        // dropped at the gate without a word.
        let saving = self.save_in_progress.is_some();
        let mut open_here: Option<String> = None;
        let mut open_new: Option<String> = None;
        let mut to_board = false;
        let mut reread = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!("{}  {name}:", ph::CIRCUITRY))
                    .size(12.0)
                    .color(egui::Color32::from_gray(150)),
            )
            .on_hover_text(root.display().to_string());
            for (dir, hint, gone) in &chips {
                let active = open.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(dir));
                let mut text = egui::RichText::new(dir).size(12.0);
                if active {
                    text = text.strong();
                }
                if *gone {
                    text = text.color(egui::Color32::from_rgb(200, 120, 100)).italics();
                }
                let resp = ui.add_enabled(!saving, egui::Button::selectable(active, text));
                let resp = if saving {
                    resp.on_disabled_hover_text("Saving - wait for it to finish")
                } else if active {
                    resp.on_hover_text(format!("{hint}\nThe chip open in this window"))
                } else {
                    resp.on_hover_text(format!(
                        "{hint}\nClick to open it here - right-click for a new window"
                    ))
                };
                if resp.clicked() && !active {
                    open_here = Some(dir.clone());
                }
                resp.context_menu(|ui| {
                    if ui
                        .add_enabled(
                            !active,
                            egui::Button::new(format!("{}  Open in this window", ph::FOLDER_OPEN)),
                        )
                        .clicked()
                    {
                        open_here = Some(dir.clone());
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            !active,
                            egui::Button::new(format!(
                                "{}  Open in new window",
                                ph::ARROW_SQUARE_OUT
                            )),
                        )
                        .on_disabled_hover_text("It is the chip open here")
                        .clicked()
                    {
                        open_new = Some(dir.clone());
                        ui.close();
                    }
                });
            }
            if ui
                .small_button(ph::ARROW_CLOCKWISE)
                .on_hover_text("Read the system again - another window may have changed it")
                .clicked()
            {
                reread = true;
            }
            if ui
                .small_button(format!("{}  Board", ph::GRAPH))
                .on_hover_text("Show how the chips are linked")
                .clicked()
            {
                to_board = true;
            }
        });
        if reread {
            self.board_sync();
        }
        if let Some(dir) = open_here {
            self.board_open_chip(&root, &dir, ui.ctx());
        }
        if let Some(dir) = open_new {
            self.board_open_in_new_window(&root, &dir);
        }
        if to_board {
            self.active_tab = super::McuTab::Board;
        }
        // What those did, where the user is looking - the Board's own notice
        // line is on another tab.
        self.board_notice_row(ui);
    }

    /// The last notice, with a button to dismiss it. The button comes FIRST
    /// and the text wraps after it: a long line would otherwise push it off
    /// the panel.
    fn board_notice_row(&mut self, ui: &mut egui::Ui) {
        let Some((text, error)) = self.board.notice.clone() else {
            return;
        };
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(egui::RichText::new(ph::X).size(10.0)).frame(false))
                .on_hover_text("Dismiss")
                .clicked()
            {
                self.board.notice = None;
            }
            let (icon, color) = if error {
                (ph::WARNING, egui::Color32::from_rgb(230, 120, 90))
            } else {
                (ph::INFO, egui::Color32::from_gray(170))
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("{icon}  {text}"))
                        .size(11.5)
                        .color(color),
                )
                .wrap(),
            );
        });
    }

    /// Whether the open part window holds edits a Save has not written.
    fn board_part_dirty(&self) -> bool {
        self.board
            .part_editor
            .as_ref()
            .is_some_and(|ed| match &ed.opened {
                Some(opened) => *opened != ed.part,
                None => true,
            })
    }

    /// Start describing a new external part, under a name no chip or part
    /// has yet.
    fn board_new_part(&mut self) {
        if self.board_part_dirty() {
            self.board_notice("Save or cancel the part being edited first.", true);
            return;
        }
        let id = (1..)
            .map(|n| format!("part{n}"))
            .find(|id| !self.board.config.has_id(id))
            .unwrap_or_default();
        self.board.part_editor = Some(PartEditor {
            original: None,
            opened: None,
            part: Part::new(&id),
            error: None,
        });
    }

    /// Open the window on an existing part.
    fn board_edit_part(&mut self, id: &str) {
        // A double-click on one of its interfaces armed a link on the way.
        self.board.arming = None;
        if self
            .board
            .part_editor
            .as_ref()
            .and_then(|e| e.original.as_deref())
            .is_some_and(|o| o.eq_ignore_ascii_case(id))
        {
            return;
        }
        if self.board_part_dirty() {
            self.board_notice("Save or cancel the part being edited first.", true);
            return;
        }
        if let Some(p) = self.board.config.part(id) {
            self.board.part_editor = Some(PartEditor {
                original: Some(p.id.clone()),
                opened: Some(p.clone()),
                part: p.clone(),
                error: None,
            });
        }
    }

    /// The External part window: its name, what it is, its I/O voltage, and
    /// its interfaces with their pins. Nothing is written until Save.
    fn board_part_window(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.board.part_editor.take() else {
            return;
        };
        let mut open = true;
        let (mut save, mut cancel, mut delete) = (false, false, false);
        let title = if ed.original.is_some() {
            "External part"
        } else {
            "New external part"
        };
        egui::Window::new(title)
            .id(egui::Id::new("board_part_editor"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("board_part_fields")
                    .num_columns(2)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Name");
                        ui.add(
                            egui::TextEdit::singleline(&mut ed.part.id)
                                .desired_width(180.0)
                                .hint_text("fpga"),
                        )
                        .on_hover_text("Its name in the system - the links use it");
                        ui.end_row();
                        ui.label("What it is");
                        ui.add(
                            egui::TextEdit::singleline(&mut ed.part.label)
                                .desired_width(180.0)
                                .hint_text("iCE40UP5K"),
                        );
                        ui.end_row();
                        ui.label("I/O voltage");
                        egui::ComboBox::from_id_salt("board_part_io")
                            .selected_text(parts::volts(ed.part.io_mv))
                            .show_ui(ui, |ui| {
                                for mv in parts::VOLTAGES {
                                    ui.selectable_value(&mut ed.part.io_mv, mv, parts::volts(mv));
                                }
                            })
                            .response
                            .on_hover_text(
                                "Its pins' level. A chip project counts as 3.3 V; a link between \
                                 two levels is flagged.",
                            );
                        ui.end_row();
                    });
                ui.separator();
                ui.label(egui::RichText::new("Interfaces").strong());
                if ed.part.interfaces.is_empty() {
                    ui.label(
                        egui::RichText::new("None yet - a chip links to a part through them.")
                            .size(11.5)
                            .color(egui::Color32::from_gray(130)),
                    );
                }
                let mut remove_iface = None;
                // Scrolls, so Save stays on screen however many there are.
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (k, iface) in ed.part.interfaces.iter_mut().enumerate() {
                            ui.push_id(("board_part_iface", k), |ui| {
                                interface_rows(ui, iface, || remove_iface = Some(k));
                            });
                        }
                    });
                if let Some(k) = remove_iface {
                    ed.part.interfaces.remove(k);
                }
                ui.menu_button(format!("{}  Add interface", ph::PLUS), |ui| {
                    for bus in PartBus::ALL {
                        if ui.button(bus.label()).clicked() {
                            ed.part.add_interface(bus);
                            ui.close();
                        }
                    }
                });
                if let Some(e) = &ed.error {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("{}  {e}", ph::WARNING))
                                .color(egui::Color32::from_rgb(230, 120, 90)),
                        )
                        .wrap(),
                    );
                }
                ui.separator();
                ui.horizontal(|ui| {
                    save = ui.button("Save").clicked();
                    cancel = ui.button("Cancel").clicked();
                    if ed.original.is_some() {
                        delete = ui
                            .button(format!("{}  Remove from system", ph::MINUS_CIRCLE))
                            .on_hover_text("The part and its links go")
                            .clicked();
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "Removing an interface removes its links. Nothing is generated for a part.",
                    )
                    .size(11.0)
                    .color(egui::Color32::from_gray(120)),
                );
            });
        if save {
            let original = ed.original.clone();
            let opened = ed.opened.clone();
            let part = ed.part.clone();
            let mut refused = None;
            // A folder of the system by that name would be a chip the next
            // time someone lists it: the two would share every link end.
            if let Some(root) = &self.board.root
                && root.join(part.id.trim()).exists()
                && !original
                    .as_deref()
                    .is_some_and(|o| o.eq_ignore_ascii_case(part.id.trim()))
            {
                refused = Some(format!(
                    "{} is the name of a folder in the system - pick another.",
                    part.id.trim()
                ));
            }
            // The file's copy must still be the one this window opened, or
            // this older copy would be written over another window's edit.
            let unchanged = |cfg: &SystemConfig| {
                let without_pos = |p: &Part| Part {
                    pos: None,
                    ..p.clone()
                };
                match (&original, &opened) {
                    (Some(o), Some(opened)) => cfg
                        .part(o)
                        .is_some_and(|now| without_pos(now) == without_pos(opened)),
                    _ => true,
                }
            };
            let saved = if refused.is_some() {
                None
            } else {
                self.board_edit_config(|cfg| {
                    if !unchanged(cfg) {
                        refused = Some(
                            "This part was changed or removed in another window since this one \
                             opened - Cancel, then open it again."
                                .to_owned(),
                        );
                        return false;
                    }
                    match cfg.put_part(original.as_deref(), part) {
                        Ok(()) => true,
                        Err(e) => {
                            refused = Some(e);
                            false
                        }
                    }
                })
            };
            match (saved, refused) {
                (_, Some(e)) => {
                    ed.error = Some(e);
                    self.board.part_editor = Some(ed);
                }
                (Some(_), None) => {
                    self.board.selected_link = None;
                    self.board_notice(format!("Saved the part {}.", ed.part.id.trim()), false);
                }
                // Not written: the notice says why, the window stays.
                (None, None) => self.board.part_editor = Some(ed),
            }
            return;
        }
        if delete {
            if let Some(id) = ed.original.clone() {
                self.board_remove(&id);
            }
            return;
        }
        if cancel || !open {
            return;
        }
        self.board.part_editor = Some(ed);
    }

    /// The Board tab.
    pub(super) fn show_board_tab(&mut self, ui: &mut egui::Ui) {
        // Hidden until now: read the system again - another window, a git
        // checkout or a hand edit may have changed it. A repeated number is
        // not a gap: egui runs a frame twice when a widget asks it to.
        let frame = ui.ctx().cumulative_frame_nr();
        let shown_again = frame != self.board.last_frame && frame != self.board.last_frame + 1;
        if shown_again && self.board.root.is_some() {
            self.board_sync();
        }
        self.board.last_frame = frame;
        if self.board.arming.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.board.arming = None;
        }
        if self.board.selected_link.is_some() && self.board_selected_index().is_none() {
            self.board.selected_link = None;
        }
        // The open chip is drawn from the live `Mcu`, which only the Pins tab
        // reconciles as it draws: right after an open its modules may not yet
        // follow its pins, where every other chip's rebuilt ones do.
        if self.board_open_chip_dir().is_some()
            && let Some(mcu) = &mut self.mcu
        {
            mcu.reconcile_modules();
        }

        self.board_toolbar(ui);
        ui.separator();

        let Some(root) = self.board.root.clone() else {
            empty_note(
                ui,
                "No system open",
                "A system is a folder of chip projects shown together here. New system… makes \
                 one; opening a chip project that sits in a system shows that system.",
            );
            return;
        };
        // Before the empty-system return: a part can be a system's first frame.
        self.board_part_window(ui.ctx());
        if self.board.config.chips.is_empty() && self.board.config.parts.is_empty() {
            empty_note(
                ui,
                "No chips yet",
                "New chip creates a project inside this system. Add existing project… copies a \
                 saved project in.",
            );
            return;
        }
        let scene = self.board_scene();
        self.board_links_panel(ui, &scene);
        self.board_canvas(ui, &root, scene);
    }

    fn board_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| match self.board.root.clone() {
            Some(root) => {
                let name = root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("system");
                ui.label(
                    egui::RichText::new(format!("{}  {name}", ph::CIRCUITRY))
                        .strong()
                        .color(egui::Color32::LIGHT_BLUE),
                )
                .on_hover_text(root.display().to_string());
                ui.add_space(8.0);
                let busy = self.save_in_progress.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new(format!("{}  New chip", ph::PLUS)))
                    .on_hover_text("Pick a chip in New Project - it is created inside this system")
                    .clicked()
                {
                    self.board.new_chip_request = true;
                    ui.ctx().request_repaint();
                }
                if ui
                    .button(format!("{}  Add existing project…", ph::FOLDER_PLUS))
                    .on_hover_text("Copy a saved chip project into this system")
                    .clicked()
                {
                    self.board_add_existing();
                }
                if ui
                    .button(format!("{}  External part", ph::CPU))
                    .on_hover_text(
                        "Something on the board that is not a chip project - an FPGA, a sensor: \
                         its interfaces and pins, to link the chips to",
                    )
                    .clicked()
                {
                    self.board_new_part();
                }
                if ui
                    .button(ph::ARROW_CLOCKWISE)
                    .on_hover_text("Read the system and its chips again from disk")
                    .clicked()
                {
                    self.board_sync();
                }
                if ui
                    .button(ph::CORNERS_OUT)
                    .on_hover_text("Fit the view to the chips")
                    .clicked()
                {
                    self.board.view_adjusted = false;
                }
                ui.separator();
                for (view, label, hint) in [
                    (
                        View::Abstract,
                        "Abstract",
                        "One line per link, between the modules",
                    ),
                    (
                        View::Detailed,
                        "Detailed",
                        "One wire per pin, with the pads named",
                    ),
                ] {
                    let on = self.board.config.view == view;
                    if ui.selectable_label(on, label).on_hover_text(hint).clicked() && !on {
                        self.board_edit_config(|cfg| {
                            let changed = cfg.view != view;
                            cfg.view = view;
                            changed
                        });
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(format!("{}  Close system", ph::X))
                        .on_hover_text("Stop showing this system. Nothing on disk changes.")
                        .clicked()
                    {
                        self.board_close_system();
                    }
                });
            }
            None => {
                ui.label(egui::RichText::new("No system open").color(egui::Color32::GRAY));
                ui.add_space(8.0);
                if ui.button(format!("{}  New system…", ph::PLUS)).clicked() {
                    self.board_new_system();
                }
                if ui
                    .button(format!("{}  Open system…", ph::FOLDER_OPEN))
                    .clicked()
                {
                    self.board_open_system_dialog();
                }
            }
        });

        // Each row puts its button FIRST and lets its text wrap after it: a
        // long line would otherwise push the button off the panel.
        if let Some(dir) = self.board_open_chip_dir()
            && !self.board.config.contains(&dir)
        {
            ui.horizontal(|ui| {
                if ui.small_button("Add it").clicked() {
                    self.board_register_and_say(&dir);
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "{}  The open project {dir} is in this folder but not in the system.",
                            ph::INFO
                        ))
                        .size(11.5)
                        .color(egui::Color32::from_rgb(220, 180, 90)),
                    )
                    .wrap(),
                );
            });
        }
        let picking = self.confirm_new_project && self.board.new_chip_intent;
        if picking || (self.board.adding_chip && self.project_dir.is_none()) {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{}  New chip: the project is saved inside this system as soon as a chip \
                         is picked.",
                        ph::INFO
                    ))
                    .size(11.5)
                    .color(egui::Color32::from_rgb(150, 200, 240)),
                )
                .wrap(),
            );
        }
        if let Some(from) = self.board.arming.clone() {
            ui.horizontal(|ui| {
                if ui.small_button("Cancel").clicked() {
                    self.board.arming = None;
                }
                let name = self
                    .board_frame_views()
                    .iter()
                    .find(|v| v.dir.eq_ignore_ascii_case(&from.chip))
                    .and_then(|v| {
                        v.module(from.kind, from.instance)
                            .map(|m| v.modules[m].name.clone())
                    })
                    .unwrap_or_else(|| links::end_name(&from));
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "{}  Linking {} {name}: click a module on another chip (Esc cancels).",
                            ph::LINK,
                            from.chip
                        ))
                        .size(11.5)
                        .color(egui::Color32::WHITE),
                    )
                    .wrap(),
                );
            });
        }
        self.board_notice_row(ui);
    }

    /// Everything the canvas and the link list are drawn from, worked out
    /// once per frame.
    fn board_scene(&self) -> BoardScene {
        // The open chip shows what it is NOW, saved or not.
        let open = self.board_open_chip_dir();
        let mut views = self.board_frame_views();
        if let (Some(dir), Some(mcu)) = (&open, &self.mcu) {
            let chip = self.selected_label();
            // Never onto a part: a part can share a folder's name only on
            // paper, and it is not the chip open here.
            for v in views
                .iter_mut()
                .filter(|v| v.external_mv.is_none() && v.dir.eq_ignore_ascii_case(dir))
            {
                *v = ChipView::from_mcu(&v.dir, &chip, mcu);
            }
        }
        let all = &self.board.config.links;
        let resolved: Vec<Resolved> = all.iter().map(|l| links::resolve(l, &views, all)).collect();
        let plain = plain_sizes(&views);
        let spots: Vec<(Option<(f32, f32)>, egui::Vec2)> = self
            .board
            .config
            .frames()
            .into_iter()
            .zip(&plain)
            .map(|((_, pos), s)| (pos, *s))
            .collect();
        let positions = layout::frame_positions(&spots);
        let geometry: Vec<(egui::Pos2, egui::Vec2)> = positions
            .iter()
            .copied()
            .zip(plain.iter().copied())
            .collect();
        let detailed = self.board.config.view == View::Detailed;
        let layouts: Vec<layout::FrameLayout> = views
            .iter()
            .zip(links::arrange(&views, &geometry, &resolved, detailed))
            .map(|(v, a)| layout::frame_layout(v, &a))
            .collect();
        // A frame that grew for its links must not reach into its neighbour.
        let sizes: Vec<egui::Vec2> = layouts.iter().map(|l| l.size).collect();
        let positions = layout::separate(&positions, &sizes);
        BoardScene {
            views,
            resolved,
            positions,
            layouts,
            open,
        }
    }

    /// The list of links under the canvas: what each joins, and whether it
    /// works.
    fn board_links_panel(&mut self, ui: &mut egui::Ui, scene: &BoardScene) {
        let detailed = self.board.config.view == View::Detailed;
        let n = self.board.config.links.len();
        let selected = self.board_selected_index();
        let extra = selected.and_then(|k| scene.resolved.get(k)).map_or(0, |r| {
            r.warnings.len() + r.notes.len() + usize::from(r.broken.is_some())
        });
        let height = (46.0 + n.max(1) as f32 * 24.0 + extra as f32 * 18.0).min(260.0);
        let mut pick: Option<Option<usize>> = None;
        let mut remove: Option<usize> = None;
        crate::app::helpers::panel::show_exact(
            egui::Panel::bottom("board_links"),
            height,
            ui,
            |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("Links ({n})")).strong());
                    ui.label(
                        egui::RichText::new(
                            "Click a module, then a module on another chip, to link them.",
                        )
                        .size(11.0)
                        .color(egui::Color32::from_gray(130)),
                    );
                });
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (k, (link, r)) in self
                            .board
                            .config
                            .links
                            .iter()
                            .zip(&scene.resolved)
                            .enumerate()
                        {
                            let name = |end: &LinkEnd, at: Option<(usize, usize)>| {
                                let module = at
                                    .and_then(|(c, m)| scene.views.get(c)?.modules.get(m))
                                    .map(|m| m.name.clone())
                                    .unwrap_or_else(|| links::end_name(end));
                                format!("{} · {module}", end.chip)
                            };
                            let color = r
                                .a
                                .and_then(|(c, m)| scene.views.get(c)?.modules.get(m))
                                .map_or(egui::Color32::GRAY, |m| module_color(m.kind, m.instance));
                            let is_selected = selected == Some(k);
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        egui::Button::new(egui::RichText::new(ph::X).size(10.0))
                                            .frame(false),
                                    )
                                    .on_hover_text("Remove this link (the modules stay)")
                                    .clicked()
                                {
                                    remove = Some(k);
                                }
                                ui.label(egui::RichText::new("•").size(16.0).color(color));
                                // The verdict before the names: a long row is cut
                                // at the panel edge, and the verdict must not be.
                                let (status, status_color) = if r.broken.is_some() {
                                    ("broken".to_owned(), egui::Color32::from_rgb(230, 100, 90))
                                } else if !r.warnings.is_empty() {
                                    (
                                        format!(
                                            "{} warning{}",
                                            r.warnings.len(),
                                            if r.warnings.len() == 1 { "" } else { "s" }
                                        ),
                                        egui::Color32::from_rgb(220, 170, 70),
                                    )
                                } else {
                                    ("ok".to_owned(), egui::Color32::from_rgb(110, 190, 120))
                                };
                                let tip: Vec<String> = r
                                    .broken
                                    .iter()
                                    .chain(&r.warnings)
                                    .chain(&r.notes)
                                    .cloned()
                                    .collect();
                                let badge = ui.label(
                                    egui::RichText::new(status).size(11.0).color(status_color),
                                );
                                if !tip.is_empty() {
                                    badge.on_hover_text(tip.join("\n"));
                                }
                                let mut text =
                                    format!("{}  <->  {}", name(&link.a, r.a), name(&link.b, r.b));
                                if detailed && !r.wires.is_empty() {
                                    let pads: Vec<String> = r
                                        .wires
                                        .iter()
                                        .map(|w| {
                                            let arrow = match w.dir {
                                                Dir::AtoB => "->",
                                                Dir::BtoA => "<-",
                                                Dir::Both => "–",
                                            };
                                            format!("{}{arrow}{}", w.a.pad, w.b.pad)
                                        })
                                        .collect();
                                    text.push_str(&format!("   ({})", pads.join(", ")));
                                }
                                if ui.selectable_label(is_selected, text).clicked() {
                                    pick = Some((!is_selected).then_some(k));
                                }
                            });
                            if is_selected {
                                let lines = r
                                    .broken
                                    .iter()
                                    .map(|t| (t, egui::Color32::from_rgb(230, 100, 90)))
                                    .chain(
                                        r.warnings
                                            .iter()
                                            .map(|t| (t, egui::Color32::from_rgb(220, 170, 70))),
                                    )
                                    .chain(
                                        r.notes.iter().map(|t| (t, egui::Color32::from_gray(150))),
                                    );
                                for (t, c) in lines {
                                    ui.horizontal(|ui| {
                                        ui.add_space(34.0);
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(t).size(11.0).color(c),
                                            )
                                            .wrap(),
                                        );
                                    });
                                }
                            }
                        }
                        if n == 0 {
                            ui.label(
                                egui::RichText::new("No links yet.")
                                    .size(11.5)
                                    .color(egui::Color32::from_gray(120)),
                            );
                        }
                    });
            },
        );
        if let Some(p) = pick {
            self.board.selected_link = p.and_then(|k| self.board.config.links.get(k).cloned());
        }
        if let Some(k) = remove
            && let Some(link) = self.board.config.links.get(k).cloned()
        {
            self.board_edit_config(|cfg| cfg.remove_link(&link));
            self.board.selected_link = None;
        }
    }

    /// A module was clicked on the canvas: the first click picks where a link
    /// starts, the second - on another chip - makes it.
    fn board_module_clicked(&mut self, views: &[ChipView], dir: &str, module: usize) {
        let Some(m) = views
            .iter()
            .find(|v| v.dir == dir)
            .and_then(|v| v.modules.get(module))
        else {
            return;
        };
        let here = links::end_for(dir, m);
        // An armed end on a chip that has left the system starts afresh.
        let armed = self
            .board
            .arming
            .take()
            .filter(|e| self.board.config.has_id(&e.chip));
        let from = match armed {
            Some(from) if !from.chip.eq_ignore_ascii_case(dir) => from,
            // The same module again: that was a cancel.
            Some(from) if from.same_as(&here) => return,
            // Nothing armed, or another module of the same chip: start here.
            _ => {
                if links::bus_of(m.kind).is_some() {
                    self.board.arming = Some(here);
                } else {
                    self.board_notice(
                        format!(
                            "{} cannot be linked to another chip - only UART, SPI, I2C, CAN, I2S, \
                             SAI, USB and custom (GPIO) modules can.",
                            m.name
                        ),
                        true,
                    );
                }
                return;
            }
        };
        let link = Link { a: from, b: here };
        let wanted = link.clone();
        match self.board_edit_config(|cfg| cfg.add_link(wanted)) {
            Some(true) => {
                self.board.selected_link = Some(link);
                self.board.notice = None;
            }
            Some(false) => self.board_notice("Those two modules are already linked.", false),
            // Not saved: the error notice stays up.
            None => {}
        }
    }

    fn board_canvas(&mut self, ui: &mut egui::Ui, root: &Path, mut scene: BoardScene) {
        let (shapes, markers) = link_shapes(
            &scene,
            self.board.config.view == View::Detailed,
            self.board_selected_index(),
        );
        let arming = self.board.arming.clone();
        let layouts = std::mem::take(&mut scene.layouts);
        let frames: Vec<gui::Frame<'_>> = scene
            .views
            .iter()
            .zip(layouts)
            .enumerate()
            .map(|(c, (v, layout))| {
                let same_chip = arming
                    .as_ref()
                    .is_some_and(|e| e.chip.eq_ignore_ascii_case(&v.dir));
                gui::Frame {
                    external: v.external_mv.is_some(),
                    view: v,
                    pos: scene.positions[c],
                    active: scene
                        .open
                        .as_deref()
                        .is_some_and(|d| v.dir.eq_ignore_ascii_case(d)),
                    layout,
                    armed: arming
                        .as_ref()
                        .filter(|_| same_chip)
                        .and_then(|e| v.module(e.kind, e.instance)),
                    // While linking, a module on ANOTHER chip that cannot take
                    // the link is greyed out; on the same chip every module
                    // stays live - clicking one restarts from it.
                    dimmed: match &arming {
                        Some(e) if !same_chip => v
                            .modules
                            .iter()
                            .map(|m| !links::can_link(e.kind, m.kind))
                            .collect(),
                        _ => Vec::new(),
                    },
                }
            })
            .collect();

        let outer = ui.available_rect_before_wrap();
        let avail = ui.available_size_before_wrap();
        let mut scene_rect = self.board.scene_rect;
        // Plain wheel zooms at the pointer, as on the Pins and Structure tabs;
        // the Scene itself would pan with it. Only when this canvas is the
        // topmost thing under the pointer: a window or popup over it scrolls
        // its own content, and the wheel is held back from the Scene (which
        // would pan by it) and handed back afterwards.
        let hover = ui.input(|i| i.pointer.hover_pos());
        let ptr = hover.filter(|_| ui.rect_contains_pointer(outer));
        let held = if ptr.is_none() && hover.is_some_and(|p| outer.contains(p)) {
            ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta))
        } else {
            egui::Vec2::ZERO
        };
        let (scroll_y, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.command));
        if let Some(ptr) = ptr
            && scroll_y != 0.0
            && !ctrl
            && scene_rect.is_finite()
            && scene_rect.size() != egui::Vec2::ZERO
        {
            let scale = (outer.size() / scene_rect.size())
                .min_elem()
                .clamp(0.1, 3.0);
            let to_global = egui::emath::TSTransform::from_translation(
                outer.center().to_vec2() - scale * scene_rect.center().to_vec2(),
            ) * egui::emath::TSTransform::from_scaling(scale);
            let z = ((scroll_y * 0.002).exp() * scale).clamp(0.1, 3.0) / scale;
            let p = to_global.inverse() * ptr;
            let new = to_global
                * egui::emath::TSTransform::from_translation(p.to_vec2())
                * egui::emath::TSTransform::from_scaling(z)
                * egui::emath::TSTransform::from_translation(-p.to_vec2());
            let new_rect = new.inverse() * outer;
            if new_rect.is_finite() && new_rect.size() != egui::Vec2::ZERO {
                scene_rect = new_rect;
                self.board.view_adjusted = true;
            }
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
        let shown = crate::app::helpers::scene::show(
            egui::Scene::new()
                .zoom_range(0.1..=3.0)
                .drag_pan_buttons(egui::DragPanButtons::PRIMARY | egui::DragPanButtons::MIDDLE),
            ui,
            &mut scene_rect,
            |ui| gui::draw(ui, &frames, &shapes, &markers),
        );
        if held != egui::Vec2::ZERO {
            ui.input_mut(|i| i.smooth_scroll_delta += held);
        }
        if shown.response.changed() {
            self.board.view_adjusted = true;
        }
        // A click on empty canvas drops a half-made link.
        if shown.response.clicked() {
            self.board.arming = None;
        }
        let (events, bounds) = shown.inner;
        self.board.scene_rect = if self.board.view_adjusted || !bounds.is_finite() {
            scene_rect
        } else {
            let want = bounds.expand(24.0);
            egui::Rect::from_center_size(
                want.center(),
                egui::vec2(want.width().max(avail.x), want.height().max(avail.y)),
            )
        };
        drop(frames);

        for ev in events {
            match ev {
                gui::Event::Moved { dir, pos } => {
                    self.board.config.set_pos(&dir, pos);
                    self.board.moved.insert(dir.to_ascii_lowercase());
                    // Hold the view still: still fitting to the frames, it
                    // would slide away under the frame being dragged.
                    self.board.view_adjusted = true;
                }
                gui::Event::DragEnded => self.board_flush_positions(),
                gui::Event::Open(dir) => self.board_open_chip(root, &dir, ui.ctx()),
                gui::Event::OpenNewWindow(dir) => self.board_open_in_new_window(root, &dir),
                gui::Event::EditPart(id) => self.board_edit_part(&id),
                gui::Event::Remove(dir) => {
                    self.board_remove(&dir);
                    self.board.selected_link = None;
                }
                gui::Event::ModuleClicked { dir, module } => {
                    self.board_module_clicked(&scene.views, &dir, module);
                }
                gui::Event::LinkClicked(k) => {
                    self.board.selected_link = self.board.config.links.get(k).cloned();
                }
            }
        }
    }
}

/// What one frame of the Board is drawn from.
struct BoardScene {
    /// Per chip, in `config.chips` order; the open one live.
    views: Vec<ChipView>,
    /// Per link, in `config.links` order.
    resolved: Vec<Resolved>,
    /// Per chip, the frame's top-left.
    positions: Vec<egui::Pos2>,
    /// Per chip, arranged for its links.
    layouts: Vec<layout::FrameLayout>,
    /// The chip open in this window.
    open: Option<String>,
}

/// The lines of every link that can be drawn, and a badge on each that has a
/// warning. Abstract: one thick line between the two modules. Detailed: one
/// wire per pin pair, with an arrowhead where the signal arrives; a link with
/// no pin wires (CAN) is still drawn module to module.
fn link_shapes(
    scene: &BoardScene,
    detailed: bool,
    selected: Option<usize>,
) -> (Vec<gui::LinkShape>, Vec<gui::Marker>) {
    let mut shapes = Vec::new();
    let mut markers = Vec::new();
    let rects: Vec<egui::Rect> = scene
        .positions
        .iter()
        .zip(&scene.layouts)
        .map(|(p, l)| egui::Rect::from_min_size(*p, l.size))
        .collect();
    for (k, r) in scene.resolved.iter().enumerate() {
        let (None, Some((ca, ma)), Some((cb, mb))) = (&r.broken, r.a, r.b) else {
            continue;
        };
        let (Some(la), Some(lb)) = (scene.layouts.get(ca), scene.layouts.get(cb)) else {
            continue;
        };
        let (oa, ob) = (scene.positions[ca].to_vec2(), scene.positions[cb].to_vec2());
        // The two end frames first - a path that would cross a frame goes
        // around those two.
        let mut frames = vec![rects[ca], rects[cb]];
        frames.extend(
            rects
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != ca && *i != cb)
                .map(|(_, r)| *r),
        );
        let (sa, sb) = (la.module_ports[ma].1, lb.module_ports[mb].1);
        let m = &scene.views[ca].modules[ma];
        let color = module_color(m.kind, m.instance);
        // Parallel links between the same frames must not lie on each other.
        let lane0 = (k % 4) as f32 * 5.0;
        let mut first: Option<Vec<egui::Pos2>> = None;
        if detailed && !r.wires.is_empty() {
            let port = |l: &layout::FrameLayout, m: usize, pin: usize| {
                l.pins[m].iter().find(|p| p.pin == pin).map(|p| p.port)
            };
            let n = r.wires.len() as f32;
            for (j, w) in r.wires.iter().enumerate() {
                let (Some(a), Some(b)) = (port(la, ma, w.a.pin), port(lb, mb, w.b.pin)) else {
                    continue;
                };
                let lane = (j as f32 - (n - 1.0) / 2.0) * 7.0 + lane0;
                let points = layout::route(a + oa, sa.out(), b + ob, sb.out(), lane, &frames);
                first.get_or_insert_with(|| points.clone());
                shapes.push(gui::LinkShape {
                    link: k,
                    points,
                    color,
                    width: 1.5,
                    arrow_end: w.dir == Dir::AtoB,
                    arrow_start: w.dir == Dir::BtoA,
                    selected: selected == Some(k),
                });
            }
        } else {
            let a = la.module_ports[ma].0 + oa;
            let b = lb.module_ports[mb].0 + ob;
            // Routed from the frame EDGES, level with each module: the stretch
            // from a module to its own edge is inside its frame, and would
            // read to the router as crossing it.
            let edge = |r: egui::Rect, s: layout::Side, y: f32| match s {
                layout::Side::Left => egui::pos2(r.left(), y),
                layout::Side::Right => egui::pos2(r.right(), y),
            };
            let (ea, eb) = (edge(rects[ca], sa, a.y), edge(rects[cb], sb, b.y));
            let mut points = vec![a];
            points.extend(layout::route(ea, sa.out(), eb, sb.out(), lane0, &frames));
            points.push(b);
            let points = layout::tidy(points);
            first = Some(points.clone());
            shapes.push(gui::LinkShape {
                link: k,
                points,
                color,
                width: 3.0,
                arrow_end: false,
                arrow_start: false,
                selected: selected == Some(k),
            });
        }
        if !r.warnings.is_empty()
            && let Some(points) = first
        {
            markers.push(gui::Marker {
                link: k,
                at: middle(&points),
            });
        }
    }
    (shapes, markers)
}

/// The middle of a path's longest segment - where a badge reads as belonging
/// to the line and not to either end.
fn middle(points: &[egui::Pos2]) -> egui::Pos2 {
    points
        .windows(2)
        .max_by(|x, y| (x[1] - x[0]).length().total_cmp(&(y[1] - y[0]).length()))
        .map_or(points[0], |w| w[0] + (w[1] - w[0]) / 2.0)
}

/// One interface in the part window: its bus, name and rate, then a row per
/// pin (its name on the part, its role) and a button for another pin.
fn interface_rows(ui: &mut egui::Ui, iface: &mut parts::Interface, mut remove: impl FnMut()) {
    ui.horizontal(|ui| {
        if ui
            .small_button(ph::X)
            .on_hover_text("Remove this interface, and its links")
            .clicked()
        {
            remove();
        }
        ui.label(egui::RichText::new(iface.bus.label()).strong());
        let hint = {
            let mut probe = iface.clone();
            probe.name.clear();
            probe.display_name()
        };
        ui.add(
            egui::TextEdit::singleline(&mut iface.name)
                .desired_width(110.0)
                .hint_text(hint),
        );
        match iface.bus {
            PartBus::Uart | PartBus::Can => {
                ui.label(if iface.bus == PartBus::Uart {
                    "baud"
                } else {
                    "bit rate"
                });
                // Not clamped as it is drawn: a rate of 0 (none stated)
                // would become 1 and be saved.
                ui.add(
                    crate::panels::drag_value(ui, &mut iface.rate)
                        .range(0..=100_000_000)
                        .clamp_existing_to_range(false),
                );
            }
            PartBus::SpiSlave | PartBus::SpiMaster => {
                ui.label("mode");
                egui::ComboBox::from_id_salt("spi_mode")
                    .width(40.0)
                    .selected_text(iface.spi_mode.to_string())
                    .show_ui(ui, |ui| {
                        for m in 0..=3u8 {
                            ui.selectable_value(&mut iface.spi_mode, m, m.to_string());
                        }
                    });
            }
            PartBus::I2c | PartBus::Gpio => {}
        }
    });
    let roles = iface.bus.roles();
    let mut remove_pin = None;
    for (j, pin) in iface.pins.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            ui.add(
                egui::TextEdit::singleline(&mut pin.name)
                    .desired_width(110.0)
                    .hint_text("pin name"),
            );
            egui::ComboBox::from_id_salt(("pin_role", j))
                .width(110.0)
                .selected_text(pin.role.label())
                .show_ui(ui, |ui| {
                    for r in roles {
                        ui.selectable_value(&mut pin.role, *r, r.label());
                    }
                });
            if ui
                .small_button(ph::X)
                .on_hover_text("Remove this pin")
                .clicked()
            {
                remove_pin = Some(j);
            }
        });
    }
    if let Some(j) = remove_pin {
        iface.pins.remove(j);
    }
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        if ui.small_button(format!("{}  pin", ph::PLUS)).clicked() {
            iface.pins.push(PartPin {
                name: String::new(),
                role: roles[0],
            });
        }
    });
    ui.add_space(4.0);
}

/// The centred note an empty Board shows.
fn empty_note(ui: &mut egui::Ui, title: &str, body: &str) {
    ui.add_space((ui.available_height() * 0.3).min(160.0));
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(format!("{}  {title}", ph::CIRCUITRY))
                .size(17.0)
                .color(egui::Color32::from_rgb(150, 158, 172)),
        );
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(body)
                .size(12.0)
                .color(egui::Color32::from_gray(120)),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("roc_board_tab_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A new window opens a chip only when the command line would accept it,
    /// and never the chip already open here.
    #[test]
    fn a_new_window_needs_a_project_that_is_not_open_here() {
        let base = scratch("new_window");
        let chip = base.join("radio");
        std::fs::create_dir_all(&chip).unwrap();
        assert!(
            new_window_refusal(&base.join("gone"), "gone", false)
                .unwrap()
                .contains("not there")
        );
        assert!(
            new_window_refusal(&chip, "radio", false)
                .unwrap()
                .contains("no Cargo.toml")
        );
        std::fs::write(chip.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(new_window_refusal(&chip, "radio", false), None);
        assert!(
            new_window_refusal(&chip, "radio", true)
                .unwrap()
                .contains("open in this window")
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Only a chip project may become a chip, and never the folder holding
    /// the system (the copy would recurse into itself).
    #[test]
    fn only_a_chip_project_can_be_added() {
        let base = scratch("add");
        let root = base.join("sys");
        std::fs::create_dir_all(&root).unwrap();
        model::save(&root, &SystemConfig::default()).unwrap();
        let chip = base.join("blinky");
        std::fs::create_dir_all(chip.join("src")).unwrap();
        std::fs::write(chip.join("src/main.rs"), "fn main() {}").unwrap();
        let notes = base.join("notes");
        std::fs::create_dir_all(&notes).unwrap();

        assert_eq!(not_a_chip(&chip, &root), None);
        assert!(
            not_a_chip(&notes, &root)
                .unwrap()
                .starts_with("Not a chip project")
        );
        assert!(
            not_a_chip(&base, &root)
                .unwrap()
                .contains("holds the system")
        );
        assert!(
            not_a_chip(&root, &root)
                .unwrap()
                .contains("holds the system")
        );
        let other = base.join("other_sys");
        std::fs::create_dir_all(&other).unwrap();
        model::save(&other, &SystemConfig::default()).unwrap();
        assert!(not_a_chip(&other, &root).unwrap().contains("is a system"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
