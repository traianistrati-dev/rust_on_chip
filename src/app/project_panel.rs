//! "Project" panel — docked on the FAR RIGHT (the [Editor][MCU][Project]
//! layout) — header toolbar (Tools dropdown: Save / Open / New / Rename) plus the file
//! tree body (delegated to [`crate::project_tree`]).
//!
//! Rendering is an inherent method on [`AppIde`] so it can read/write the
//! many `self` fields the tree needs.  Button presses are returned to the
//! caller as [`ProjectPanelSignals`]; the caller acts on them after the panel
//! closure ends (opening folders, showing modals, exporting).

use super::AppIde;
use crate::panels::mcu_module::project_gen::ProjectFiles;
use crate::project_tree::gui::show_project_tree as show_project_tree_panel;
use eframe::egui;
use egui_phosphor::regular as ph;

/// Toolbar button presses collected inside the panel, acted on by the caller.
pub(super) struct ProjectPanelSignals {
    pub open_clicked: bool,
    pub new_clicked: bool,
    pub save_clicked: bool,
    /// A project picked from "Open Recent" — the same destructive load as
    /// `open_clicked`, minus the folder picker, so it goes through the same
    /// unsaved-changes gate.
    pub open_recent: Option<std::path::PathBuf>,
    /// Folder (relative to `src/`) the user asked to extract into its own crate.
    pub extract_folder: Option<String>,
    /// The LIBRARIES "+" button was clicked — create an empty library crate.
    pub new_library: bool,
    /// The LIBRARIES "clone from git" button was clicked.
    pub clone_library: bool,
    /// The project-header "Clone project" button was clicked.
    pub clone_project: bool,
    /// `(crate dir, is_rename)` when a library's pen / trash icon was clicked.
    pub library_action: Option<(String, bool)>,
    /// A DETACHED library the user asked to promote into the workspace.
    pub add_to_workspace: Option<String>,
    /// A member library the user asked to remove from the workspace (keep files).
    pub detach_from_workspace: Option<String>,
    /// Project-root-relative path of the file to show in the Reference tab.
    ///
    /// A PATH, not a `user_src_files` index: fixed project files (Cargo.toml,
    /// memory.x, …) have no index at all, and an index would go stale the
    /// moment a file above it is deleted. `AppIde::reference_file` stores a
    /// path for the same reason.
    pub open_reference: Option<String>,
    /// Tree item to stage on the cross-instance clipboard.
    pub clip_copy: Option<crate::project_tree::clipboard::CopyRequest>,
    /// Where to paste a staged payload.
    pub clip_paste: Option<crate::project_tree::clipboard::PasteRequest>,
    /// A file row carrying the RED error badge was clicked - jump the editor to
    /// that file's FIRST error rather than leaving it at the top.
    pub goto_error: Option<crate::app::ProjectFileId>,
    /// A validated file rename for the app to perform. The tree deliberately
    /// does NOT move the file itself: the module-reference rewrite has to run
    /// while the old path still exists.
    pub rename_request: Option<crate::project_tree::gui::RenameRequest>,
    /// "Move to folder…" was picked on a file row; the app opens the dialog.
    pub move_to_folder: Option<String>,
    /// "Publish…" was picked on a library; the app opens the publish dialog.
    pub publish_lib: Option<String>,
}

impl AppIde {
    /// The current long-running activity for the bottom status bar:
    /// `(show_spinner, label, colour)`, or `None` when idle. Priority: save >
    /// build > flash > rust-analyzer; otherwise the last save result (✓ / ✗)
    /// while it's still flashing. Rendered in the bottom bar (see `app::ui`).
    pub(super) fn activity_status(&self) -> Option<(bool, String, egui::Color32)> {
        let amber = egui::Color32::from_rgb(220, 180, 70);
        let blue = egui::Color32::from_rgb(100, 170, 240);

        // The busy chain of one save, named by the step it is actually on.
        //
        // All three used to read "Saving…", which was wrong in two different
        // ways. Once the worker is done the project IS on disk - what remains is
        // refreshing the diagnostics - and the WAIT in the middle showed nothing
        // at all: `save_in_progress` is already cleared and the flush has not
        // started, so a save blocked on rust-analyzer looked finished while the
        // spinner sat there. It is the longest step of a slow save and it was
        // the one with no label.
        let flush_in_flight = self
            .lsp_flush_in_flight
            .load(std::sync::atomic::Ordering::Acquire);
        if self.save_in_progress.is_some() || flush_in_flight || self.lsp_flush_requested {
            let status = self.lsp_state.lock().unwrap().status.clone();
            if let Some(label) = save_step_label(
                self.save_in_progress.is_some(),
                flush_in_flight,
                self.lsp_flush_requested,
                &status,
            ) {
                return Some((true, label, amber));
            }
        }
        if matches!(
            *self.build_state.lock().unwrap(),
            crate::build::BuildState::Building
        ) {
            return Some((true, "Building…".to_owned(), blue));
        }
        if matches!(
            *self.clippy_state.lock().unwrap(),
            crate::build::BuildState::Building
        ) {
            return Some((true, "Running clippy…".to_owned(), amber));
        }
        let dfu_busy = self.dfu_state.lock().unwrap().is_busy();
        let ocd_busy = self.openocd_state.lock().unwrap().is_busy();
        let esp_busy = self.espflash_state.lock().unwrap().is_busy();
        if dfu_busy || ocd_busy || esp_busy {
            return Some((true, "Flashing…".to_owned(), blue));
        }
        if let Some(op) = self.git.state.lock().unwrap().busy {
            return Some((true, format!("Git: {op}…"), amber));
        }
        // Waiting on a cold analyzer for a Go-to-definition the user asked for.
        // Above the generic "Indexing…" because it says WHY the wait is
        // happening — and because it is the only feedback there is: F12 with the
        // analyzer down is otherwise silent, and `Stopped` / `Failed` fall
        // through the match below without a status of their own.
        if self.pending_goto.is_some() {
            return Some((
                true,
                "Go to definition: loading the analyzer…".to_owned(),
                amber,
            ));
        }
        {
            let lsp = self.lsp_state.lock().unwrap();
            match lsp.status {
                crate::lsp::LspStatus::Starting | crate::lsp::LspStatus::Indexing => {
                    return Some((true, "Indexing…".to_owned(), amber));
                }
                // Past the first load phase but not loaded yet - a save made
                // now is held until it is, so "Checking…" would be a promise.
                crate::lsp::LspStatus::Ready if !lsp.workspace_loaded() => {
                    return Some((true, "Loading workspace…".to_owned(), amber));
                }
                crate::lsp::LspStatus::Ready if lsp.checking || lsp.flycheck_pending() => {
                    // Live elapsed seconds — makes the post-save flycheck tail
                    // (the "save takes 20s" perception) visible and measurable.
                    // `flycheck_pending` covers the QUEUE phase (didSave →
                    // cargo start), previously a status GAP with no spinner —
                    // and therefore no scheduled repaint to keep frames coming.
                    let label = match lsp.checking_elapsed_secs() {
                        Some(s) if s >= 1 => format!("Checking… {s}s"),
                        _ => "Checking…".to_owned(),
                    };
                    return Some((true, label, amber));
                }
                _ => {}
            }
        }
        if self
            .export_status_until
            .is_some_and(|t| std::time::Instant::now() < t)
            && !self.export_msg.is_empty()
        {
            let ok = !self.export_msg.starts_with(ph::X_CIRCLE);
            let color = if ok {
                egui::Color32::from_rgb(90, 200, 120)
            } else {
                egui::Color32::from_rgb(220, 90, 80)
            };
            return Some((false, self.export_msg.clone(), color));
        }
        None
    }

    /// Render the left Project panel and return which toolbar buttons were hit.
    ///
    /// `ctrl_s_pressed` seeds `save_clicked` so the Ctrl+S shortcut behaves
    /// exactly like clicking Save.  `save_project_needed` is set when the tree
    /// body mutates files/folders and the workspace must be rewritten.
    pub(super) fn show_project_panel(
        &mut self,
        ui: &mut egui::Ui,
        project_files: &Option<ProjectFiles>,
        ctrl_s_pressed: bool,
        save_project_needed: &mut bool,
    ) -> ProjectPanelSignals {
        let mut open_project_clicked = false;
        let mut open_recent: Option<std::path::PathBuf> = None;
        // Same one-slot arm as the startup picker's, taken out for the frame so
        // the menu closure can borrow it while `self` is borrowed for the tree.
        let mut armed = std::mem::take(&mut self.recent_forget_confirm);
        let mut new_project_clicked = false;
        let mut save_project_clicked = ctrl_s_pressed; // Ctrl+S triggers save
        let mut extract_folder: Option<String> = None;
        let mut new_library = false;
        let mut clone_library = false;
        let mut clone_project = false;
        let mut library_action: Option<(String, bool)> = None;
        let mut add_to_workspace: Option<String> = None;
        let mut detach_from_workspace: Option<String> = None;
        let mut open_reference: Option<String> = None;
        let mut clip_copy: Option<crate::project_tree::clipboard::CopyRequest> = None;
        let mut clip_paste: Option<crate::project_tree::clipboard::PasteRequest> = None;
        let mut goto_error: Option<crate::app::ProjectFileId> = None;
        let mut rename_request: Option<crate::project_tree::gui::RenameRequest> = None;
        let mut move_to_folder: Option<String> = None;
        let mut publish_lib: Option<String> = None;

        // Collapsed: the panel is not built at all, so the editor and the MCU
        // zone take the width back. This function still RUNS, because it is
        // what turns Ctrl+S into `save_clicked` above — and because the caller
        // acts on ~15 signal fields it expects to get back either way. Hiding
        // the tree must not quietly disable saving.
        //
        // No collapsed strip is drawn: the toolbar's sidebar button is always
        // on screen and is the way back, exactly as it is for the MCU zone.
        // Its width feeds the editor's cap next frame (see `AppIde::tree_width`)
        // — collapsed it costs nothing, so the editor gets that space back.
        let mut tree_width = 0.0_f32;
        if !self.tree_collapsed {
            let panel = egui::Panel::right("project_tree")
                .resizable(true)
                .default_size(crate::app::TREE_MIN_W)
                .show(ui, |ui| {
                    // ── Panel header row ──────────────────────────────────────────
                    ui.horizontal(|ui| {
                        ui.heading("Project");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // New / Open / Save grouped in one "Tools" dropdown
                            // (2026-07-10 refactor — the three separate buttons
                            // crowded the header).
                            ui.menu_button(
                                egui::RichText::new(format!("{} Tools", ph::WRENCH)).size(11.0),
                                |ui| {
                                    if ui
                                        .button(format!("{} New Project", ph::NOTE_PENCIL))
                                        .on_hover_text("Start a new empty project")
                                        .clicked()
                                    {
                                        new_project_clicked = true;
                                        ui.close();
                                    }
                                    if ui
                                        .button(format!("{} Open Project…", ph::FOLDER_OPEN))
                                        .on_hover_text("Open an existing project folder")
                                        .clicked()
                                    {
                                        open_project_clicked = true;
                                        ui.close();
                                    }
                                    // "Open Recent" — read from disk only while
                                    // the submenu is being built, so the shared
                                    // list (another window may have just added
                                    // to it) is never stale, and nothing is read
                                    // on a frame where the menu is closed.
                                    ui.menu_button(
                                        format!("{} Open Recent", ph::CLOCK_COUNTER_CLOCKWISE),
                                        |ui| {
                                            let list = crate::recent::load();
                                            if list.is_empty() {
                                                ui.label(
                                                    egui::RichText::new("No recent projects")
                                                        .size(10.5)
                                                        .italics()
                                                        .color(egui::Color32::from_gray(140)),
                                                );
                                                return;
                                            }
                                            for entry in list {
                                                // Skip the project already open
                                                // in THIS window — reopening it
                                                // would only throw work away.
                                                if self.project_dir.as_ref().is_some_and(|d| {
                                                    crate::recent::is_same_path(d, &entry.path)
                                                }) {
                                                    continue;
                                                }
                                                let label = match &entry.mcu_id {
                                                    Some(id) => format!("{}   ({id})", entry.name),
                                                    None => entry.name.clone(),
                                                };
                                                ui.horizontal(|ui| {
                                                    // Forget FIRST, so every one
                                                    // of them starts at the same
                                                    // x — see the module note in
                                                    // `helpers::forget_button`.
                                                    if crate::app::helpers::forget_button::forget_button(
                                                        ui,
                                                        &entry.path,
                                                        &mut armed,
                                                    ) {
                                                        crate::recent::forget(
                                                            std::path::Path::new(&entry.path),
                                                        );
                                                        // NOT `ui.close()`: the
                                                        // list is re-read every
                                                        // frame the menu is open,
                                                        // so the row vanishes on
                                                        // its own and several can
                                                        // be cleared in one go.
                                                    }
                                                    if ui
                                                        .add(
                                                            egui::Button::new(label).min_size(
                                                                egui::vec2(
                                                                    ui.available_width(),
                                                                    0.0,
                                                                ),
                                                            ),
                                                        )
                                                        .on_hover_text(&entry.path)
                                                        .clicked()
                                                    {
                                                        open_recent = Some(
                                                            std::path::PathBuf::from(&entry.path),
                                                        );
                                                        ui.close();
                                                    }
                                                });
                                            }
                                        },
                                    );
                                    let can_save = project_files.is_some();
                                    if ui
                                        .add_enabled(
                                            can_save,
                                            egui::Button::new(format!(
                                                "{} Save Project",
                                                ph::EXPORT
                                            )),
                                        )
                                        .on_hover_text("Export/Save project to disk (Ctrl+S)")
                                        .clicked()
                                    {
                                        save_project_clicked = true;
                                        ui.close();
                                    }
                                    // Rename needs a folder on disk (a project gets
                                    // its name at the first Save) and no save
                                    // worker writing into the old path meanwhile.
                                    let can_rename = self.project_dir.is_some()
                                        && self.save_in_progress.is_none();
                                    if ui
                                        .add_enabled(
                                            can_rename,
                                            egui::Button::new(format!(
                                                "{} Rename Project…",
                                                ph::PENCIL_SIMPLE
                                            )),
                                        )
                                        .on_hover_text(
                                            "Rename the project folder on disk \
                                         (the Cargo package name is unaffected)",
                                        )
                                        .clicked()
                                    {
                                        self.renaming_project =
                                            Some(self.project_name.clone().unwrap_or_default());
                                        self.renaming_project_focus = true;
                                        ui.close();
                                    }

                                    // ── Settings ──────────────────────────────
                                    // Last, and in its own submenu: preferences
                                    // are not project actions, and this is the
                                    // one place the IDE's own settings live (see
                                    // `settings_menu`).
                                    ui.separator();
                                    ui.menu_button(
                                        format!("{} Settings", ph::GEAR),
                                        crate::app::settings_menu::show,
                                    );
                                },
                            );
                        });
                    });

                    ui.separator();
                    // Show project name under the heading when one is loaded
                    if let Some(name) = &self.project_name {
                        ui.label(
                            egui::RichText::new(format!("  {}", name))
                                .size(10.5)
                                .color(egui::Color32::from_rgb(140, 160, 180))
                                .italics(),
                        );
                    }

                    // Owned (project params, toolchain) so no `self` borrow is held
                    // across the `&mut self` arguments below. The manifest is the
                    // authority on which directories are library crates — a stray
                    // top-level folder must not be presented as one.
                    let lib_crates =
                        crate::panels::mcu_module::project_gen::workspace_members(&self.cargo_toml);
                    // Every folder cargo loads with the firmware: the members, plus
                    // any the root reaches through a `path` dependency. Those are
                    // loaded too, so they are no "NOT IN WORKSPACE" library - but
                    // their dependency line is the user's, not the IDE's to edit.
                    let built = self.built_lib_dirs();
                    let path_dep_libs: Vec<String> = built
                        .iter()
                        .filter(|d| !lib_crates.contains(d))
                        .cloned()
                        .collect();
                    // Cloned libraries not (yet) promoted into the workspace — shown
                    // in their own LIBRARIES subsection with an "Add to workspace"
                    // action (guarded by a cargo-metadata pre-check).
                    let detached = crate::project_tree::extract_crate::detached_libs(
                        &self.project_tree.user_src_files,
                        &built,
                    );
                    // Which detached lib has a pre-check running (spinner in the row).
                    let ws_add_pending = self.workspace_add.as_ref().map(|w| w.dir.clone());
                    let build_cfg = self.selected_build_cfg();
                    match (project_files, build_cfg) {
                        (Some(_), Some((project, toolchain))) => {
                            // Only the badge flags leave the locks, taken one at a
                            // time: neither result is cloned, and the tree renders
                            // holding neither lock.
                            let mut badges = crate::project_tree::gui::DiagBadges::default();
                            if let Some(result) = self.build_state.lock().unwrap().result() {
                                badges.add_build(result);
                            }
                            badges.add_lsp(&self.lsp_state.lock().unwrap());
                            // Use actual project directory if available, otherwise use temp workspace
                            let workspace_dir = if let Some(project_dir) = &self.project_dir {
                                project_dir.clone()
                            } else {
                                crate::workspace::dir()
                            };
                            show_project_tree_panel(
                                ui,
                                &project.pkg_name,
                                &toolchain,
                                !self.partitions_csv.is_empty(),
                                &mut self.selected_file,
                                &badges,
                                &mut self.project_tree.user_src_files,
                                &mut self.project_tree.user_src_folders,
                                &mut self.new_src_name,
                                &mut self.new_src_folder_name,
                                &mut self.new_file_parent_folder,
                                &mut self.new_folder_parent_folder,
                                &mut self.new_file_in_folder,
                                &mut self.renaming_file,
                                &mut self.renaming_folder,
                                &workspace_dir,
                                self.project_dir.as_deref(),
                                save_project_needed,
                                &mut extract_folder,
                                &lib_crates,
                                &path_dep_libs,
                                &detached,
                                ws_add_pending.as_deref(),
                                &mut self.tree_split_ratio,
                                &mut new_library,
                                &mut clone_library,
                                &mut clone_project,
                                &mut library_action,
                                &mut publish_lib,
                                &mut add_to_workspace,
                                &mut detach_from_workspace,
                                &mut open_reference,
                                &mut clip_copy,
                                &mut clip_paste,
                                &mut goto_error,
                                &mut move_to_folder,
                                &mut rename_request,
                            );
                        }
                        _ => {
                            ui.add_space(12.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("Export not available\nfor this chip yet.")
                                        .size(11.0)
                                        .color(egui::Color32::GRAY),
                                );
                            });
                        }
                    }
                });
            tree_width = panel.response.rect.width();
        }
        self.tree_width = tree_width;

        self.recent_forget_confirm = armed;
        ProjectPanelSignals {
            open_clicked: open_project_clicked,
            open_recent,
            new_clicked: new_project_clicked,
            save_clicked: save_project_clicked,
            extract_folder,
            new_library,
            clone_library,
            clone_project,
            library_action,
            add_to_workspace,
            detach_from_workspace,
            open_reference,
            clip_copy,
            clip_paste,
            goto_error,
            rename_request,
            move_to_folder,
            publish_lib,
        }
    }
}

/// Which step of a save the status bar should name, or `None` when no save is
/// in flight.
///
/// All three steps used to read "Saving…", which was wrong in two ways. Once
/// the worker is done the project IS on disk - what is left is refreshing the
/// diagnostics - and the WAIT in the middle showed NOTHING: `save_in_progress`
/// is already cleared and the flush has not started, so a save blocked on
/// rust-analyzer looked finished while the spinner sat there. It is the longest
/// step of a slow save and it was the one step with no label at all.
pub(super) fn save_step_label(
    worker_running: bool,
    flush_in_flight: bool,
    flush_requested: bool,
    ra: &crate::lsp::LspStatus,
) -> Option<String> {
    // Order is the order the steps happen in, so an overlapping pair names the
    // earlier one - the save is still "on" that step.
    if worker_running {
        return Some("Saving…".to_owned());
    }
    if flush_in_flight {
        return Some("Syncing to rust-analyzer…".to_owned());
    }
    if flush_requested {
        // The flush only runs while RA is Ready, so this waits on exactly that
        // - and says so, rather than implying the disk is slow.
        let why = match ra {
            crate::lsp::LspStatus::Indexing => " (indexing)",
            crate::lsp::LspStatus::Stopped => " (not running)",
            crate::lsp::LspStatus::Failed(_) => " (failed)",
            _ => "",
        };
        return Some(format!("Waiting for rust-analyzer{why}…"));
    }
    None
}

#[cfg(test)]
mod save_step_label_tests {
    use super::save_step_label;
    use crate::lsp::LspStatus;

    /// The step this whole change exists for.
    ///
    /// Between the worker finishing and the flush starting, NOTHING was shown:
    /// the save looked done while it was in fact blocked. On a slow save this
    /// is the longest step, and it was the invisible one.
    #[test]
    fn the_wait_is_no_longer_silent() {
        let l = save_step_label(false, false, true, &LspStatus::Indexing)
            .expect("a pending flush is not idle");
        assert!(l.contains("rust-analyzer"), "{l}");
        assert!(l.contains("indexing"), "it should say WHY: {l}");
    }

    /// It says the disk is busy only while the disk is actually busy.
    #[test]
    fn only_the_worker_step_says_saving() {
        assert_eq!(
            save_step_label(true, false, false, &LspStatus::Ready).as_deref(),
            Some("Saving…")
        );
        // Worker done, flush running: the project is already on disk.
        let l = save_step_label(false, true, false, &LspStatus::Ready).unwrap();
        assert!(!l.contains("Saving"), "the project is written by now: {l}");
        assert!(l.contains("rust-analyzer"), "{l}");
    }

    /// Overlapping flags name the EARLIER step - the save is still on it.
    #[test]
    fn overlap_names_the_earlier_step() {
        assert_eq!(
            save_step_label(true, true, true, &LspStatus::Indexing).as_deref(),
            Some("Saving…")
        );
    }

    /// Idle is idle - no spinner when nothing is in flight.
    #[test]
    fn nothing_in_flight_is_none() {
        assert!(save_step_label(false, false, false, &LspStatus::Ready).is_none());
        assert!(save_step_label(false, false, false, &LspStatus::Indexing).is_none());
    }

    /// A stopped or failed analyzer is named too - those waits never end on
    /// their own, and "Waiting…" with no reason is where a user gives up.
    #[test]
    fn a_dead_analyzer_is_named() {
        for (st, word) in [
            (LspStatus::Stopped, "not running"),
            (LspStatus::Failed("boom".into()), "failed"),
        ] {
            let l = save_step_label(false, false, true, &st).unwrap();
            assert!(l.contains(word), "{l}");
        }
    }
}
