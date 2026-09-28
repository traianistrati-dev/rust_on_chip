//! Project I/O â loading an existing Cargo project from disk and polling
//! the filesystem watcher for external file changes.
//!
//! Both are inherent methods on AppIde (child module of app), so they can
//! mutate the many self fields involved in project + tree state.

use super::AppIde;
use super::ProjectFileId;
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use crate::project_tree::ProjectTreeState;

impl AppIde {
    // ── Project load ──────────────────────────────────────────────────────────

    /// Loads user source files from an existing Cargo project at `root`.
    /// Only files in `root/src/` are imported; `main.rs` is always skipped
    /// (it is regenerated from MCU pin state).
    /// Any previous user files are replaced.
    pub(super) fn load_project_from_dir(&mut self, root: &std::path::Path) {
        // Cover the whole switch with the loading overlay — this call is the
        // single choke point for every project change (Open, clone, git branch
        // switch / discard-all, the startup restore), and what follows it
        // (workspace write, RA re-index, flycheck) takes far longer than the
        // read below while leaving a half-loaded project on screen.
        self.begin_project_loading(super::loading_overlay::LoadKind::Open);

        // Load files and folders via ProjectTreeState
        self.project_tree = ProjectTreeState::load_from_dir(root);

        // The RA workspace content is about to change wholesale — drop the
        // flush hash cache so the first flush re-writes every file.
        self.flushed_hashes.lock().unwrap().clear();
        // Arm the one-shot post-load re-verify: RA's first analysis of this
        // project can be stale (a document opened before the workspace was
        // complete), so once it is loaded, errors get one forced re-check.
        self.lsp_settle_recheck_done = false;
        // A new project gets its own automatic restart after a crash mid-load.
        self.lsp_auto_restarted = false;
        self.last_workspace_change = Some(std::time::Instant::now());

        self.selected_file = ProjectFileId::MainRs;
        // An in-flight file rename belongs to the project being LEFT. Its reply
        // carries edits addressed by project-root-relative path, so leaving it
        // armed would let rust-analyzer's answer about the old project rewrite
        // the new one's files at the old one's coordinates.
        self.pending_rename = None;
        // Same reason: the dialog holds a project-root-RELATIVE path, which
        // means a different file once another project is loaded.
        self.move_file_dialog = None;
        // Go-tos and the Definition tab's walk belong to the project being LEFT.
        self.drop_project_gotos();
        // Through the closer, not a bare `None`: a cargo run started from that
        // window would otherwise keep going against the OLD project's folder,
        // with its only kill path gone along with the window.
        self.close_publish_dialog();
        self.renaming_file = None;
        self.renaming_folder = None;
        self.new_src_name = None;
        self.new_src_folder_name = None;
        self.new_file_parent_folder = None;
        self.new_folder_parent_folder = None;
        self.new_file_in_folder = None;
        self.project_name = root.file_name().and_then(|n| n.to_str()).map(String::from);
        // Per PROJECT, not per process: the flag latched on the first Serial tab
        // open and never cleared, so a second project opened in the same window
        // kept the first one's baud rate.
        self.serial.baud_seeded = false;
        // The same relative image path names a different file in this project.
        self.module_note_images.clear();
        self.project_dir = Some(root.to_path_buf());
        // Take this folder for this window (or find out another one has it).
        self.claim_open_project();

        // ── Detect which chip this project targets ───────────────────────────
        // Switching the MCU type BEFORE restoring pin state ensures the correct
        // pin diagram + clock graph are active when parse_main_rs() is applied.
        //
        // Two signals, in priority order:
        //   1. The `// rust_on_chip:mcu=<id>` marker in src/main.rs (or its
        //      pre-rename `// embedded-ide:mcu=` spelling) — written by
        //      our own codegen. This pins the EXACT definition (incl. imported
        //      chips that share a HAL crate with a built-in, which step 2 can't
        //      disambiguate — e.g. "esp32c3-graph" vs "esp32c3").
        //   2. Fallback: match the Cargo.toml HAL crate (first token of each
        //      definition's `hal_dep`) for older projects without the marker.
        let main_rs_path = root.join("src").join("main.rs");
        // LF-normalized, like every user file (see `scan_src_dir`): the git
        // gutter compares this text against an LF baseline, and a CRLF body
        // from a Windows checkout showed as a permanent phantom diff.
        let main_rs_source = std::fs::read_to_string(&main_rs_path)
            .ok()
            .map(|s| s.replace("\r\n", "\n"));

        let cargo_source = std::fs::read_to_string(root.join("Cargo.toml")).ok();
        let detected_id: Option<String> = crate::panels::mcu_module::registry::detect_chip_id(
            &self.mcu_registry,
            main_rs_source.as_deref(),
            cargo_source.as_deref(),
        );

        if let Some(id) = detected_id {
            if id != self.selected_mcu_id {
                self.selected_mcu_id = id;
                self.mcu = Self::build_mcu_for(&self.mcu_registry, &self.selected_mcu_id);
                // The same HAL lookup the chip picker starts. Opening a project
                // is how you meet a chip someone ELSE chose — the case the
                // verdict exists for — and it does not pass through the picker,
                // so without this line the one gap that stops a project
                // compiling would be the one gap never reported here.
                self.start_hal_check();
                // Re-fit the Pins canvas to the new chip (drop any persisted view).
                self.mcu_view_adjusted = false;
                // Reset LSP — it was attached to the previous chip's workspace.
                self.lsp_state.lock().unwrap().reset();
                self.lsp_selected_diagnostic = None;
            }
        }
        // Opening a project replaces the workspace deps → the previous
        // project's lock is stale. Take THIS project's own `Cargo.lock` when it
        // has one, and drop the lock only when it has none (later saves keep it).
        seed_workspace_lock(root, &crate::workspace::dir());

        // Restore the Structure diagram's dragged positions from
        // `project_structure.config` — read independently of the MCU restore
        // below since the diagram is chip-agnostic. Missing file/section →
        // automatic layout. `load` falls back to `mcu.config`, where this state
        // lived before it got its own file.
        self.restore_view_state(root);

        // ── Virtual Module notes ─────────────────────────────────────────────
        // On EVERY open, outside the main.rs branch below: a project with no
        // main.rs or no mcu.config must still clear the previous project's
        // notes rather than inherit them. `restore_module_notes` assigns.
        if let Some(mcu) = &mut self.mcu {
            let cfg = std::fs::read_to_string(
                root.join(crate::panels::mcu_module::mcu_config::FILE_NAME),
            )
            .ok();
            mcu.restore_module_notes(cfg.as_deref());
        }

        // ── Restore pin state from mcu.config and src/main.rs ────────────────
        // Restore the MCU diagram: pin assignments from the `@pins` section of
        // `mcu.config` when it has one, otherwise from the recognized bindings
        // in the GEN_BEGIN…GEN_END block.  If neither yields any pins (e.g. a
        // hand-written main.rs) the diagram is reset.
        if let Some(source) = main_rs_source {
            use crate::panels::mcu_module::codegen;
            use crate::panels::mcu_module::mcu_config;

            // Restore virtual modules + clock-tree config from the project-root
            // `mcu.config` file (must happen before update_main_rs below, so the
            // restored clock drives the regenerated chain). Older projects
            // without that file fall back to the legacy `@modules` / `@clock`
            // comment markers that used to live in main.rs.
            // Kept, not just consumed: `@pins` is read further down, and
            // `@labels` has to be applied after the pin apply has done its
            // `reset_all_pins`.
            let cfg_text = std::fs::read_to_string(root.join(mcu_config::FILE_NAME)).ok();
            match &cfg_text {
                Some(cfg) => {
                    if let Some(mcu) = &mut self.mcu {
                        mcu.apply_mcu_config(cfg);
                    }
                }
                None => {
                    use crate::panels::mcu_module::clock::persist as clock_persist;
                    if let Some(clock) = clock_persist::parse_from_source(&source) {
                        if let Some(mcu) = &mut self.mcu {
                            mcu.apply_saved_clock(clock);
                        }
                    }
                    let restored =
                        crate::panels::mcu_module::modules::persist::parse_from_source(&source);
                    if !restored.is_empty() {
                        if let Some(mcu) = &mut self.mcu {
                            mcu.modules = restored;
                        }
                    }
                }
            }

            // The pins. `@pins` first, when the config has it: that is the nRF
            // store, whose generated block carries no label `parse_main_rs`
            // could read. Every STM32 and ESP project is recovered from
            // main.rs as it always was; an nRF project saved before the
            // section existed has nothing to recover, and resets below.
            if let Some(saved) = saved_pins(cfg_text.as_deref(), &source) {
                if let Some(mcu) = &mut self.mcu {
                    match &saved {
                        SavedPins::ByNumber(pins) => mcu.apply_saved_pins_by_number(pins),
                        SavedPins::ByName(pins) => mcu.apply_saved_pins(pins),
                    }
                    // Restore the per-pin user labels (the `_<label>` suffix on a
                    // binding) — after apply_saved_pins, which would clear them.
                    mcu.apply_saved_pin_labels(&codegen::parse_pin_labels(&source));
                    // `@labels` LAST, and it wins: it holds the free text the
                    // user typed, where the binding suffix above holds only what
                    // survived being turned into a Rust identifier. A project
                    // saved before the section existed has none, and keeps the
                    // recovered-from-main.rs labels it always had.
                    if let Some(cfg) = &cfg_text {
                        mcu.apply_config_pin_labels(cfg);
                    }
                    // Rebuild generated_code from the restored pin state while
                    // keeping the user's loop body from the existing file.
                    self.generated_code = mcu.update_main_rs(&source);
                }
            } else {
                // No saved pins: a blank project, a hand-written main.rs, or an
                // nRF or RP project saved before either wrote `@pins` (their
                // shape is one `parse_main_rs` cannot read).  Always reset the
                // MCU diagram so pins configured in the previously-open project
                // do not bleed into this one.
                if let Some(mcu) = &mut self.mcu {
                    mcu.reset_all_pins();
                }
                self.generated_code = source;
            }
        }

        // ── Restore editable config files from disk ──────────────────────────
        // Read each generated config file the project carries and refresh its
        // `<<< GENERATED >>>` block from the (now-selected) chip, preserving any
        // edits the user made outside the block. Missing files are generated
        // fresh; files a toolchain doesn't use (memory.x/build.rs on ESP) stay
        // empty.
        if let Some((cfg, tc)) = self.selected_build_cfg() {
            use crate::panels::mcu_module::project_gen::{ConfigFile, gen_config, splice_config};
            let load = |file: ConfigFile, path: std::path::PathBuf| -> String {
                match std::fs::read_to_string(&path) {
                    // LF-normalized like every buffer (phantom-gutter rule).
                    Ok(disk) => splice_config(file, &disk.replace("\r\n", "\n"), &cfg, &tc),
                    Err(_) => gen_config(file, &cfg, &tc),
                }
            };
            self.cargo_toml = load(ConfigFile::CargoToml, root.join("Cargo.toml"));
            self.cargo_config = load(
                ConfigFile::CargoConfig,
                root.join(".cargo").join("config.toml"),
            );
            self.memory_x = load(ConfigFile::MemoryX, root.join("memory.x"));
            self.build_rs = load(ConfigFile::BuildRs, root.join("build.rs"));
            self.gitignore = load(ConfigFile::GitIgnore, root.join(".gitignore"));
        }

        // Remember it for "Open Recent" and for starting another window on it.
        // Here, at the END of the load: `selected_mcu_id` is settled by now, so
        // the entry carries the chip that was actually detected — which is what
        // tells two projects apart in a one-line menu.
        // `None` rather than an empty id: a project whose chip could not be
        // detected has no chip, and the menu should show nothing, not a blank.
        crate::recent::record(
            root,
            Some(self.selected_mcu_id.as_str()).filter(|id| !id.is_empty()),
        );

        // Baseline the dependency fingerprint so the FIRST Save after the user
        // edits a library in Cargo.toml auto-builds (see `last_saved_deps`).
        self.last_saved_deps = Some(crate::panels::mcu_module::project_gen::deps_fingerprint(
            &self.cargo_toml,
        ));

        // Verify the project still loads for cargo (and therefore rust-analyzer)
        // — a workspace member left in a bad state (e.g. an incompatible library
        // added by hand) would otherwise fail silently as a stuck "Checking…".
        self.recheck_workspace_health();

        // A chip of a system brings its system to the Board tab.
        self.board_follow_project(root);
    }

    /// The per-project VIEW state - diagram positions, the Structure tab's
    /// options, the Flow tab's reading position - from `root`'s
    /// `project_structure.config`.
    ///
    /// Only what the file holds is applied, so whatever it leaves out still
    /// carries the previous project's value
    /// (`opening_a_project_forgets_the_last_ones_structure_view`).
    pub(super) fn restore_view_state(&mut self, root: &std::path::Path) {
        use crate::panels::mcu_module::structure_config;
        let (positions, view, clock, clock_view, flow) = structure_config::load(root);
        self.structure_overrides = positions;
        // Clock-diagram node positions — applied by the Clock tab over the
        // generated layout (unknown ids, e.g. after a chip change, are
        // simply ignored).
        self.clock_ui.positions = clock;
        self.clock_ui.fields = clock_view;
        // View options (Calls / depth / path style / externals) — absent
        // section (older projects) keeps the defaults.
        if let Some((show_calls, depth, style, externals)) = view {
            self.structure_view.show_calls = show_calls;
            self.structure_view.call_depth = depth;
            self.structure_view.path_style =
                crate::panels::structure_map::gui::PathStyle::from_u8(style);
            self.structure_view.show_externals = externals;
        }
        // Flow-tab reading position - restored only onto its own file (see
        // `FlowViewPersist`); an absent section leaves it empty and the tab
        // opens on the file's entry point. The mode ("All — whole file")
        // is per project too, and an absent section is the default.
        self.flow_selected = flow.selected;
        self.flow_view.set_mode_bits(flow.mode);
        // Force the next Structure-tab frame to rebuild + re-apply them
        // even when the content hash happens to match the cached graph.
        self.structure_cache = None;
        // Same for the Flow tab: a new project's files are different text
        // even when a content hash happens to collide.
        self.flow_cache = None;
    }

    // ── Project-folder claim ──────────────────────────────────────────────────

    /// Claim the open project's folder for this window, replacing any previous
    /// claim. Sets `project_lock_conflict` when another live instance holds it.
    ///
    /// A busy folder does NOT stop the project from opening: refusing would be
    /// the more damaging failure (a crashed sibling, a folder open in a window
    /// the user forgot about, and the project becomes unopenable), and the
    /// banner states the actual risk plainly enough to act on.
    pub(super) fn claim_open_project(&mut self) {
        // Release first — re-claiming the SAME folder would otherwise find our
        // own lock and report the project busy against itself.
        self.project_lock = None;
        self.project_lock_conflict = None;
        self.project_lock_retry = None;
        let Some(dir) = self.project_dir.clone() else {
            return; // an unsaved project has no folder to claim yet
        };
        match crate::workspace::claim_project(&dir) {
            crate::workspace::ProjectClaim::Acquired(lock) => self.project_lock = Some(lock),
            crate::workspace::ProjectClaim::Busy => {
                self.project_lock_conflict = Some(
                    dir.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| dir.display().to_string()),
                );
                self.project_lock_retry = Some(std::time::Instant::now());
            }
            // No lock file possible here — never warn on a guess.
            crate::workspace::ProjectClaim::Unavailable => {}
        }
    }

    /// While the project is claimed elsewhere, retry every couple of seconds so
    /// the banner disappears on its own when the other window closes. Cheap: one
    /// file open, and only while a conflict is actually up.
    pub(super) fn retry_project_claim(&mut self) {
        const RETRY: std::time::Duration = std::time::Duration::from_secs(2);
        if self.project_lock_conflict.is_none() {
            return;
        }
        if !self
            .project_lock_retry
            .is_some_and(|t| t.elapsed() >= RETRY)
        {
            return;
        }
        self.project_lock_retry = Some(std::time::Instant::now());
        let Some(dir) = self.project_dir.clone() else {
            self.project_lock_conflict = None;
            return;
        };
        if let crate::workspace::ProjectClaim::Acquired(lock) =
            crate::workspace::claim_project(&dir)
        {
            self.project_lock = Some(lock);
            self.project_lock_conflict = None;
            self.project_lock_retry = None;
        }
    }

    /// Keep the window title on the open project, so the taskbar peek and
    /// Alt+Tab name the window by what is in it. Sent only on change — see
    /// [`crate::app::helpers::window_title`] for the composition rules.
    pub(super) fn refresh_window_title(&mut self, ui: &egui::Ui) {
        // The instance marker, shown only when it disambiguates. Slot 0 is the
        // pid fallback (every slot taken), where the number IS the identity.
        let tag = match crate::workspace::slot() {
            1 => None,
            0 => Some(format!("#p{}", std::process::id())),
            s => Some(format!("#{s}")),
        };
        let title = crate::app::helpers::window_title::compose(
            self.project_name.as_deref(),
            tag.as_deref(),
            // Another window has this same project open: the names are equal,
            // so the marker is the only thing telling the two apart.
            self.project_lock_conflict.is_some(),
        );
        if title != self.window_title {
            self.window_title = title.clone();
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
    }

    // ── Project rename ────────────────────────────────────────────────────────

    /// Rename the saved project's FOLDER (leaf only — always the same drive)
    /// and update `project_dir` / `project_name`. The Cargo package name is
    /// deliberately untouched: it is per-chip (the flash pipeline looks the ELF
    /// up by it), not per-project. Everything else follows automatically — the
    /// Git tab reads `project_dir` per command, the fs watcher watches the temp
    /// RA workspace, and no project file stores the folder name.
    pub(super) fn rename_project(&mut self, new_name: &str) -> Result<(), String> {
        let old_dir = self
            .project_dir
            .clone()
            .ok_or("No saved project to rename")?;
        let new_dir = rename_project_dir(&old_dir, new_name)?;
        let name = new_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| new_name.trim().to_owned());
        // A chip of a system: its folder name is its identity there, in the
        // chip list and in every link, so the system follows the rename.
        self.board_chip_renamed(&old_dir, &new_dir);
        self.project_dir = Some(new_dir);
        self.project_name = Some(name);
        // The claim is keyed by path — the old one now points at a folder that
        // no longer exists, so re-take it under the new name.
        self.claim_open_project();
        Ok(())
    }

    // ── Filesystem watcher polling ────────────────────────────────────────────
    /// Drains the notify channel and applies the Create / Remove / Rename
    /// changes it carries to `project_tree`, in the order they happened.
    ///
    /// Rules:
    /// - Only files inside `workspace/src/` are tracked.
    /// - `src/main.rs` is always excluded (it is the generated file).
    /// - Create: add if not already present (avoids duplicates from our own writes).
    /// - Remove: drop from the list (IDE-initiated removes are already gone).
    /// - Rename: update the stored path in place. notify reports one in two
    ///   halves on Windows (and the halves can land in different frames), so
    ///   [`super::fs_events::FsEventPairer`] pairs them first.
    pub(super) fn poll_fs_events(&mut self) {
        use super::fs_events::{RENAME_PAIR_WAIT, apply_fs_changes};

        // Paths are made relative to the workspace ROOT (tree paths are
        // project-root-relative). The WATCH itself still covers only `src/`:
        // watching the root would pull in `target/`, which churns constantly
        // during a build and would flood the channel.
        let workspace_root = crate::workspace::dir();
        let workspace_src = workspace_root.join("src");

        self.sync_fs_watch(&workspace_src);

        let Some(rx) = self.fs_rx.as_ref() else {
            return;
        };

        let now = std::time::Instant::now();
        let mut changes = Vec::new();
        let mut drained = 0usize;

        // Bounded: a burst larger than this finishes on the next frames instead
        // of stalling one of them.
        for event in rx.try_iter().take(FS_EVENTS_PER_FRAME) {
            drained += 1;
            let Ok(event) = event else {
                continue;
            };
            self.fs_pairer
                .push(&event, now, |p| p.exists(), &mut changes);
        }
        self.fs_pairer.expire(now, &mut changes);
        if self.fs_pairer.is_waiting() {
            // Half a rename is held: come back for the rest, or to give up on
            // it, even if nothing else wakes the UI.
            self.egui_ctx.request_repaint_after(RENAME_PAIR_WAIT);
        }

        // The editor names its file by index, which a removal shifts.
        let mut selected = match self.selected_file {
            ProjectFileId::UserFile(i) => Some(i),
            _ => None,
        };
        let src_root_removed = apply_fs_changes(
            &mut self.project_tree,
            &workspace_root,
            changes,
            &mut selected,
        );
        if let ProjectFileId::UserFile(_) = self.selected_file {
            self.selected_file = selected.map_or(ProjectFileId::MainRs, ProjectFileId::UserFile);
        }

        if drained == FS_EVENTS_PER_FRAME {
            self.egui_ctx.request_repaint();
        }

        // The watched directory itself went away. Its handle is dead even if a
        // new `src/` appears under the same name, so drop it now and re-attach
        // on the next check. Only backends that report the root's own removal
        // (inotify's DELETE_SELF) get here: notify's Windows backend never names
        // the watched directory in an event, so there the throttled
        // `sync_fs_watch` check (`is_dir()` false -> Unwatch) does the recovery.
        if src_root_removed {
            self.release_fs_watch();
            self.request_fs_watch_check();
        }
    }

    /// Keeps exactly ONE live watch on `workspace/src`.
    ///
    /// notify's Windows backend, 6.1.1 through 8.2.0, is not idempotent (fixed
    /// in 9.0): every `watch()` on an already-watched path opens another
    /// directory handle and replaces the map entry WITHOUT stopping the old one. Calling it every frame, as this used
    /// to, left one live recursive watch per frame. A single write to `src/`
    /// then fired two callbacks per leaked watch, and the next frame's `watch()`
    /// blocked in its acknowledgement until the watcher thread had run all of
    /// them. Measured: 35 ms at 1 000 leaked watches, 2.6 s at 12 000. That was
    /// the freeze after Ctrl+S on a MODIFIED file (the RA flush writes it into
    /// `src/`); an unmodified save writes nothing, so it returned at once. The
    /// leaked handles cannot be reclaimed at runtime (neither `unwatch` nor
    /// dropping the watcher frees them), so the only fix is never to leak.
    ///
    /// Throttled to [`FS_WATCH_RECHECK`]; the steady state makes no notify call,
    /// only one `is_dir` stat per check. [`Self::request_fs_watch_check`] skips
    /// the wait after the IDE writes the workspace itself.
    pub(super) fn sync_fs_watch(&mut self, workspace_src: &std::path::Path) {
        let now = std::time::Instant::now();
        if now < self.fs_watch_next_check {
            return;
        }
        self.fs_watch_next_check = now + FS_WATCH_RECHECK;
        let Some(w) = self._fs_watcher.as_mut() else {
            return;
        };
        let exists = workspace_src.is_dir();
        sync_watch(w, &mut self.fs_watched, workspace_src, exists);
    }

    /// Stops the current watch, if any, closing its directory handle. That
    /// handle is what keeps a deleted `src/` pending deletion.
    fn release_fs_watch(&mut self) {
        if let Some(w) = self._fs_watcher.as_mut() {
            release_watch(w, &mut self.fs_watched);
        }
    }

    /// Re-check the watch on the next frame rather than after the throttle.
    /// Called after the IDE (re)writes the workspace, which may have just
    /// created `src/`.
    pub(super) fn request_fs_watch_check(&mut self) {
        self.fs_watch_next_check = std::time::Instant::now();
    }
}

/// Give the build `workspace` the `Cargo.lock` of the project being opened at
/// `project_root`, or none when it has none.
///
/// It used to be deleted on every open, so the first `cargo metadata` resolved
/// the whole graph again from the crates.io index: a network round-trip, a
/// lock that appeared mid-load and made rust-analyzer fetch the workspace 3-4
/// times over, and versions free to drift from the ones the user builds with,
/// each drift a cold rebuild of build scripts and proc-macros. The project's
/// own lock was sitting next to it the whole time. Cargo still adjusts it if
/// the manifest has moved on.
///
/// But the workspace lock is where the user's builds really resolve - a
/// dependency added and built, a `cargo update` in the Terminal tab - and
/// nothing copies it back to the project. So it is KEPT when it grew from this
/// very project lock for these very dependencies: [`LOCK_SEED_MARKER`] records
/// the project and the lock it was seeded from, and the workspace manifest -
/// still the last one written when this runs - must list the same dependencies
/// as the project's. A different project, a project lock changed from outside
/// (a branch switch, a pull) or dependencies changed without being saved (a
/// Discard all, a reopen that dropped unsaved edits) seed again. The IDE's own
/// rewrite of the project lock carries the marker over instead - see
/// [`restamp_lock_seed`].
///
/// An identical lock is not rewritten either, so a reopen does not touch its
/// mtime and set the analyzer off on another fetch. Best effort, like the
/// delete was: cargo makes a lock of its own when this one is missing.
pub(super) fn seed_workspace_lock(project_root: &std::path::Path, workspace: &std::path::Path) {
    let dest = workspace.join("Cargo.lock");
    let marker = workspace.join(LOCK_SEED_MARKER);
    let lock = std::fs::read(project_root.join("Cargo.lock")).ok();
    let stamp = lock_seed_stamp(project_root, lock.as_deref());
    if dest.is_file()
        && std::fs::read_to_string(&marker).is_ok_and(|m| m == stamp)
        && same_dependencies(workspace, project_root)
    {
        return;
    }
    let seeded = match &lock {
        Some(lock) => {
            std::fs::read(&dest).ok().as_deref() == Some(lock.as_slice())
                || std::fs::write(&dest, lock).is_ok()
        }
        None => {
            let _ = std::fs::remove_file(&dest);
            !dest.exists()
        }
    };
    if seeded {
        let _ = std::fs::write(&marker, stamp);
    } else {
        let _ = std::fs::remove_file(&marker);
    }
}

/// Carry the seed marker over a rewrite of the project lock `before` -> `after`
/// that the IDE made itself: its `cargo metadata` health check runs in the
/// project folder and resolves the manifest into the project's own lock. That
/// is no change from outside, so it must not make the next open throw away the
/// workspace lock the marker keeps. Only a marker that recorded exactly
/// `before` of this project moves; any other stays as it is.
pub(super) fn restamp_lock_seed(
    project_root: &std::path::Path,
    workspace: &std::path::Path,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
) {
    if before == after {
        return;
    }
    let marker = workspace.join(LOCK_SEED_MARKER);
    if std::fs::read_to_string(&marker).is_ok_and(|m| m == lock_seed_stamp(project_root, before)) {
        let _ = std::fs::write(&marker, lock_seed_stamp(project_root, after));
    }
}

/// Remove the build workspace's `Cargo.lock` and the note of where it came
/// from - a New Project, which has no lock of its own to take.
pub(super) fn clear_workspace_lock(workspace: &std::path::Path) {
    let _ = std::fs::remove_file(workspace.join("Cargo.lock"));
    let _ = std::fs::remove_file(workspace.join(LOCK_SEED_MARKER));
}

/// Whether both manifests list the same dependencies (normalized, so CRLF and
/// reformatting do not count). A missing manifest matches nothing.
fn same_dependencies(a: &std::path::Path, b: &std::path::Path) -> bool {
    let deps = |dir: &std::path::Path| {
        std::fs::read_to_string(dir.join("Cargo.toml"))
            .ok()
            .map(|text| crate::panels::mcu_module::project_gen::deps_fingerprint(&text))
    };
    let a = deps(a);
    a.is_some() && a == deps(b)
}

/// Beside the build workspace's `Cargo.lock`: the project and the project
/// lock it was last seeded from (see [`seed_workspace_lock`]). A dotfile, which
/// neither the stale-`.rs` sweep nor the foreign-crate prune touches.
const LOCK_SEED_MARKER: &str = ".rust_on_chip_lock_seed";

/// The marker's content for `project_root` and its lock (`None`: it has none):
/// the folder, as canonical as the disk allows, and an FNV-1a hash of the
/// lock. FNV because it is stable across toolchains, unlike `DefaultHasher`,
/// so an IDE update does not re-seed every project.
fn lock_seed_stamp(project_root: &std::path::Path, lock: Option<&[u8]>) -> String {
    let root = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let Some(lock) = lock else {
        return format!("{}\nnone\n", root.display());
    };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in lock {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{}\n{hash:016x}\n", root.display())
}

/// How often [`AppIde::sync_fs_watch`] looks at the disk.
const FS_WATCH_RECHECK: std::time::Duration = std::time::Duration::from_millis(500);

/// Most watcher events handled in one frame.
const FS_EVENTS_PER_FRAME: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WatchAction {
    /// Nothing to do — the steady state.
    Keep,
    Watch,
    Unwatch,
    /// Watching a different path than the target.
    Rewatch,
}

/// What to do with the watch, given what is watched now and whether the target
/// directory exists. Never asks to watch a path that is already watched: that
/// is the call that leaks on Windows.
pub(super) fn watch_action(
    watched: Option<&std::path::Path>,
    target: &std::path::Path,
    target_exists: bool,
) -> WatchAction {
    match (watched, target_exists) {
        (None, true) => WatchAction::Watch,
        (None, false) => WatchAction::Keep,
        (Some(_), false) => WatchAction::Unwatch,
        (Some(w), true) if w == target => WatchAction::Keep,
        (Some(_), true) => WatchAction::Rewatch,
    }
}

/// Applies [`watch_action`] to a watcher and the path it is known to watch.
/// Generic over the watcher so a counting mock can prove the call count.
pub(super) fn sync_watch<W: notify::Watcher>(
    w: &mut W,
    watched: &mut Option<std::path::PathBuf>,
    target: &std::path::Path,
    target_exists: bool,
) {
    match watch_action(watched.as_deref(), target, target_exists) {
        WatchAction::Keep => {}
        WatchAction::Unwatch => release_watch(w, watched),
        action @ (WatchAction::Watch | WatchAction::Rewatch) => {
            if action == WatchAction::Rewatch {
                release_watch(w, watched);
            }
            // A failed `watch` returns before notify stores anything, so
            // retrying leaks nothing; the caller's throttle keeps a path that
            // keeps failing from paying the blocking round trip every frame.
            if w.watch(target, notify::RecursiveMode::Recursive).is_ok() {
                *watched = Some(target.to_path_buf());
            }
        }
    }
}

fn release_watch<W: notify::Watcher>(w: &mut W, watched: &mut Option<std::path::PathBuf>) {
    if let Some(old) = watched.take() {
        // Result ignored: on Windows `unwatch` only queues the request (Err
        // means the watcher thread is gone); elsewhere Err means the entry was
        // already removed. Nothing left to free either way.
        let _ = w.unwatch(&old);
    }
}

impl AppIde {
    /// The directory git commands run in: the project root, or a library's own
    /// repository when the Git tab is pointed at one.
    pub(super) fn git_dir(&self) -> Option<std::path::PathBuf> {
        let root = self.project_dir.as_ref()?;
        Some(self.git.target.dir(root))
    }

    /// A path as git reported it — relative to the ACTIVE repo root — turned
    /// into the project-root-relative key the IDE's buffers and tree use.
    ///
    /// These are the same string only while the target is the project. Inside a
    /// library repo git says `src/lib.rs` where the IDE stores
    /// `mw_radar/src/lib.rs`, and using the raw value would look up a file that
    /// does not exist (silently reverting nothing).
    pub(super) fn git_path_to_project(&self, git_path: &str) -> String {
        format!("{}{}", self.git.target.prefix(), git_path)
    }

    /// Start a git operation on a worker thread (signal handler for the Git
    /// tab's buttons and the tree's context menu). Guards: needs a saved
    /// project (`project_dir`), no overlap with a running op, and no save in
    /// flight (git would read a half-written tree).
    pub(super) fn run_git_op(&mut self, op: crate::git::GitOp) {
        let Some(dir) = self.git_dir() else {
            return; // the tab shows the "save first" hint instead
        };
        if self.git.is_busy() {
            return;
        }
        if self.save_in_progress.is_some() {
            self.git.state.lock().unwrap().lines.push((
                crate::git::GitLine::Notice,
                "[busy] save in progress — retry in a moment".into(),
            ));
            return;
        }
        // Same reason as the save gate, and worse for a branch switch: it
        // rewrites the working tree `cargo publish` is packaging, and on
        // success reloads the project — which closes the publish window and
        // KILLS the upload. A real upload is permanent and cannot be repeated,
        // so it is the one thing worth making a git op wait for. The switch
        // confirmation cannot stand in for this: it only appears when there are
        // unsaved changes, and an upload's own precondition is that there are
        // none.
        if self.publish_uploading() {
            self.git.state.lock().unwrap().lines.push((
                crate::git::GitLine::Notice,
                "[busy] a crate upload is running — cargo is reading the working tree".into(),
            ));
            return;
        }
        let msg = self.git.commit_msg.trim().to_owned();
        let remote = self.git.remote_url_draft.trim().to_owned();
        // A switch/delete takes its target branch from the header picker; a new
        // branch from the text field. Both flow through `run_op`'s single
        // `branch` argument.
        let branch = if matches!(
            op,
            crate::git::GitOp::SwitchBranch | crate::git::GitOp::DeleteBranch
        ) {
            self.git.switch_target.take().unwrap_or_default()
        } else {
            self.git.branch_draft.trim().to_owned()
        };
        // Checkbox selection: everything checked → plain `add -A`; otherwise
        // stage only the checked changed files. All-unchecked never spawns.
        let add_paths = if self.git.excluded.is_empty() {
            None
        } else {
            let picked: Vec<String> = self
                .git
                .state
                .lock()
                .unwrap()
                .status
                .changes
                .iter()
                .map(|c| c.path.clone())
                .filter(|p| !self.git.excluded.contains(p))
                .collect();
            if picked.is_empty()
                && matches!(
                    op,
                    crate::git::GitOp::Commit | crate::git::GitOp::CommitPush
                )
            {
                self.git.state.lock().unwrap().lines.push((
                    crate::git::GitLine::Notice,
                    "[info] no files checked — check what you want in the commit".into(),
                ));
                return;
            }
            Some(picked)
        };
        crate::git::run_op(
            op,
            msg,
            remote,
            branch,
            add_paths,
            dir,
            crate::git::snapshot_for_target(self.git_disk_snapshot(), &self.git.target),
            // Only meaningful at the project root: a library repo has no
            // generated config files of its own to go stale.
            match self.git.target {
                crate::git::RepoTarget::Project => {
                    generated_files_that_must_be_absent(&self.current_project_files())
                        .into_iter()
                        .map(str::to_owned)
                        .collect()
                }
                _ => Vec::new(),
            },
            // Mirror only when committing a LIBRARY and the option is on; the
            // project root is where the second commit runs.
            match (&self.git.target, self.git.mirror_to_project) {
                (crate::git::RepoTarget::Library(lib), true) => {
                    self.project_dir.clone().map(|root| (lib.clone(), root))
                }
                _ => None,
            },
            std::sync::Arc::clone(&self.git.state),
            std::sync::Arc::clone(&self.activity),
            self.egui_ctx.clone(),
        );
    }

    /// Start a "Clone from git" library import into `<project>/<dir>` — a plain
    /// clone (independent repo) or a submodule. The worker runs git + validates;
    /// `finish_clone_library` wires it in on completion.
    pub(super) fn start_clone_library(&mut self, url: String, dir: String, as_submodule: bool) {
        let Some(root) = self.project_dir.clone() else {
            return; // dialog shows the "save first" hint
        };
        if self.git.is_busy() {
            return;
        }
        crate::git::run_clone_library(
            url,
            dir,
            as_submodule,
            root,
            std::sync::Arc::clone(&self.git.state),
            self.egui_ctx.clone(),
        );
    }

    /// Wire a freshly-imported library into the project: scan its files into the
    /// tree, and (for an INDEPENDENT clone) gitignore it. It is deliberately NOT
    /// added to `[workspace] members` here — an external crate that can't resolve
    /// for the firmware's target would break `cargo metadata`, which rust-analyzer
    /// runs to load the WHOLE workspace, killing RA (no inline errors, no
    /// Structure edges, a stuck "Checking…"). The user promotes it explicitly via
    /// "Add to workspace" ([`add_detached_lib_to_workspace`]), which runs a
    /// `cargo metadata` pre-check first. Until then it shows as a DETACHED library.
    ///
    /// For an INDEPENDENT clone (`is_submodule == false`) it also GITIGNORES the
    /// folder — the clone is its own repo, and tracking its files would gitlink it
    /// and break a fresh checkout. A SUBMODULE is left tracked (git records it via
    /// `.gitmodules` + a pinned commit — that's the whole point).
    pub(super) fn finish_clone_library(&mut self, dir: String, is_submodule: bool) {
        let Some(root) = self.project_dir.clone() else {
            return;
        };
        if !is_submodule {
            let entry = format!("{dir}/");
            let already = self
                .gitignore
                .lines()
                .any(|l| matches!(l.trim(), t if t == dir || t == entry || t == format!("/{dir}")));
            if !already {
                if !self.gitignore.is_empty() && !self.gitignore.ends_with('\n') {
                    self.gitignore.push('\n');
                }
                self.gitignore.push_str(&format!("{entry}\n"));
            }
        }
        // Bring its files into the tree WITHOUT a full reload (preserves other
        // in-memory buffers); `load_from_dir` already treats members this way.
        self.project_tree.add_member_dir(&root, &dir);
        self.cached_project_files = None;
        self.request_save = true;
    }

    /// Start promoting the DETACHED library `dir` into the workspace as BOTH a
    /// `[workspace] member` AND a `[dependencies.<name>]` path dependency, so the
    /// firmware can actually `use` it. A cargo-metadata PRE-CHECK runs first
    /// (async), because an incompatible crate (its own workspace, conflicting
    /// deps, unresolvable path deps) would break `cargo metadata` for the WHOLE
    /// workspace and kill rust-analyzer. The change is applied only if that check
    /// passes ([`poll_workspace_add`]).
    pub(super) fn add_detached_lib_to_workspace(&mut self, dir: String) {
        let Some(root) = self.project_dir.clone() else {
            return;
        };
        if self.workspace_add.is_some() {
            return; // one check at a time
        }
        // The dependency table key is the crate's PACKAGE name (from its own
        // Cargo.toml), which can differ from the directory name — read it, and
        // fall back to the dir name if the manifest can't be parsed.
        let name = self
            .project_tree
            .user_src_files
            .iter()
            .find(|(p, _)| *p == format!("{dir}/Cargo.toml"))
            .and_then(|(_, c)| crate::git::parse_package_name(c))
            .unwrap_or_else(|| dir.clone());
        // The manifest we WOULD write. The worker trials it before we commit.
        let tentative =
            crate::project_tree::extract_crate::patch_root_manifest(&self.cargo_toml, &dir, &name);
        let state = std::sync::Arc::new(std::sync::Mutex::new(None));
        run_metadata_precheck(
            root,
            tentative.clone(),
            std::sync::Arc::clone(&state),
            self.egui_ctx.clone(),
        );
        self.workspace_add = Some(WorkspaceAdd {
            dir,
            tentative,
            state,
        });
    }

    /// Consume a finished "Add to workspace" pre-check. On success the tentative
    /// manifest becomes the live one and a Save is requested (RA reloads with the
    /// member). On failure the cargo error is stored for the modal. Idempotent —
    /// a no-op until the worker posts a result.
    pub(super) fn poll_workspace_add(&mut self) {
        let done = self
            .workspace_add
            .as_ref()
            .and_then(|w| w.state.lock().unwrap().take());
        let Some(result) = done else {
            return;
        };
        let w = self.workspace_add.take().unwrap();
        match result {
            Ok(()) => {
                self.cargo_toml = w.tentative;
                self.cached_project_files = None;
                self.request_save = true;
                // The workspace gained a member — restart RA so it re-runs
                // `cargo metadata` cleanly and analyzes the new crate (a wedged
                // RA won't pick it up from a didChange alone).
                self.restart_lsp();
            }
            Err(e) => self.workspace_add_error = Some((w.dir, e)),
        }
    }

    /// Remove library `dir` from `[workspace] members` (and any path dependency)
    /// WITHOUT deleting its files — the inverse of "Add to workspace". Used both
    /// as the LIBRARIES "Detach" action and as the one-click recovery when an
    /// incompatible member has broken the workspace load.
    pub(super) fn detach_lib_from_workspace(&mut self, dir: &str) {
        self.cargo_toml =
            crate::project_tree::extract_crate::remove_workspace_member(&self.cargo_toml, dir);
        // A detach can only FIX a broken workspace, so clear the stale banner
        // now, then re-verify — the recheck re-flags it if something else is
        // still wrong.
        self.workspace_load_error = None;
        self.cached_project_files = None;
        self.request_save = true;
        // Removing a member changes the workspace graph — and if an incompatible
        // member had wedged RA, only a restart recovers it (a manifest edit alone
        // won't un-stick a dead analyzer, the reported "stuck Checking…").
        self.restart_lsp();
        self.recheck_workspace_health();
    }

    /// Restart rust-analyzer: `reset()` kills the child + bumps the generation +
    /// marks it `Stopped`; the LSP lifecycle in `init_frame` then re-writes the
    /// workspace and spawns a fresh RA next frame. The single entry point for
    /// the Analyzer-tab "Restart" button and every workspace-structure change.
    pub(super) fn restart_lsp(&mut self) {
        self.lsp_state.lock().unwrap().reset();
        self.lsp_selected_diagnostic = None;
        // The relaunched session goes through Starting → Indexing again, and the
        // "open the document only once indexing is done" fallback clock is
        // per-session.
        self.lsp_indexing_since = None;
    }

    /// Restart rust-analyzer when the `linkedProjects` a launch would compute
    /// NOW differs from what the running one was started with.
    ///
    /// `linkedProjects` is fixed at launch, and rust-analyzer will not switch
    /// to a workspace fetch that partly fails while it already has one. So a
    /// linked detached library whose folder then leaves the build workspace -
    /// deleted, renamed, or another project opened on the same chip, all pruned
    /// by the next write - froze every later manifest edit out of the analyzer.
    /// The other direction is the welcome one: adding the `exclude` line and
    /// saving brings the library in without a manual restart.
    ///
    /// Throttled: it reads the workspace root and a few manifests from disk,
    /// and the answer only changes when a write lands.
    pub(super) fn recheck_linked_projects(&mut self) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(2);
        if self.linked_check_at.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.linked_check_at = Some(std::time::Instant::now());
        // `None`: the manifests cannot say right now - keep what is running.
        let Some(now) = crate::lsp::linked_projects_now(&crate::workspace::dir()) else {
            return;
        };
        if now != self.lsp_state.lock().unwrap().linked_projects {
            self.restart_lsp();
        }
    }

    /// Start a background `cargo metadata` HEALTH CHECK of the project exactly as
    /// it is on disk — the same load rust-analyzer performs. A failure means RA
    /// will not load either (no inline errors, no Structure edges, a stuck
    /// "Checking…"), so [`poll_workspace_health`] surfaces it as a banner rather
    /// than leaving the user with a silently-dead analyzer. Runs on open and
    /// after a detach; no-op without a saved project dir.
    pub(super) fn recheck_workspace_health(&mut self) {
        let Some(root) = self.project_dir.clone() else {
            self.workspace_load_error = None;
            return;
        };
        let state = std::sync::Arc::new(std::sync::Mutex::new(None));
        run_metadata_healthcheck(
            root,
            crate::workspace::dir(),
            std::sync::Arc::clone(&state),
            self.egui_ctx.clone(),
        );
        self.workspace_health = Some(state);
    }

    /// Consume a finished health check: store the error (banner) or clear it.
    pub(super) fn poll_workspace_health(&mut self) {
        let done = self
            .workspace_health
            .as_ref()
            .and_then(|s| s.lock().unwrap().take());
        let Some(result) = done else {
            return;
        };
        self.workspace_health = None;
        self.workspace_load_error = result.err();
    }
}

/// In-flight state for the "Add to workspace" cargo-metadata pre-check.
pub(super) struct WorkspaceAdd {
    /// The library directory being promoted (project-root-relative).
    pub dir: String,
    /// The manifest to commit if the check passes.
    pub tentative: String,
    /// `None` while the worker runs; `Some(Ok/Err)` once it finishes.
    pub state: std::sync::Arc<std::sync::Mutex<Option<Result<(), String>>>>,
}

/// Run `cargo metadata` against a TRIAL manifest to see whether adding a member
/// keeps the workspace loadable — the same resolution rust-analyzer does on
/// load. Runs on a worker thread; posts `Ok(())` / `Err(cargo error)` into
/// `state`. RA-safe: RA watches the TEMP check workspace, never the project dir,
/// so briefly swapping the project's `Cargo.toml` here can't disturb it. The
/// original `Cargo.toml` (and `Cargo.lock`) are restored on every exit path.
fn run_metadata_precheck(
    project_dir: std::path::PathBuf,
    tentative: String,
    state: std::sync::Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    ctx: eframe::egui::Context,
) {
    use std::process::Command;
    std::thread::spawn(move || {
        let manifest = project_dir.join("Cargo.toml");
        let lock = project_dir.join("Cargo.lock");
        let orig_toml = std::fs::read(&manifest).ok();
        let orig_lock = std::fs::read(&lock).ok();

        let result = (|| -> Result<(), String> {
            std::fs::write(&manifest, tentative.as_bytes())
                .map_err(|e| format!("could not write trial Cargo.toml: {e}"))?;
            let mut cmd = Command::new("cargo");
            crate::build::no_window(&mut cmd)
                .args(["metadata", "--format-version", "1"])
                .current_dir(&project_dir)
                // A git path-dep could otherwise open a terminal credential
                // prompt that hangs a headless process forever.
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(std::process::Stdio::null());
            let out = cmd
                .output()
                .map_err(|e| format!("could not run cargo: {e}"))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(clean_cargo_error(&String::from_utf8_lossy(&out.stderr)))
            }
        })();

        // Restore the project's manifest + lock no matter what.
        match orig_toml {
            Some(o) => {
                let _ = std::fs::write(&manifest, o);
            }
            None => {
                let _ = std::fs::remove_file(&manifest);
            }
        }
        match orig_lock {
            Some(o) => {
                let _ = std::fs::write(&lock, o);
            }
            None => {
                let _ = std::fs::remove_file(&lock);
            }
        }

        *state.lock().unwrap() = Some(result);
        ctx.request_repaint();
    });
}

/// Run `cargo metadata` on the project AS-IS (no modification) to check whether
/// the workspace still loads — the resolution rust-analyzer does on start. Posts
/// `Ok(())` / `Err(cargo error)` into `state`. Read-only: unlike the pre-check it
/// never writes the manifest, so it is safe to fire on project open.
fn run_metadata_healthcheck(
    project_dir: std::path::PathBuf,
    workspace: std::path::PathBuf,
    state: std::sync::Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    ctx: eframe::egui::Context,
) {
    use std::process::Command;
    std::thread::spawn(move || {
        // No manifest → nothing to load (e.g. a chip with no export). Treat as
        // healthy so we never show a spurious banner.
        if !project_dir.join("Cargo.toml").is_file() {
            *state.lock().unwrap() = Some(Ok(()));
            ctx.request_repaint();
            return;
        }
        // Plain `cargo metadata` writes the project's lock when the manifest
        // has moved past it: carry the build workspace's seed marker over that
        // write, which is the IDE's own (see `restamp_lock_seed`).
        let lock_before = std::fs::read(project_dir.join("Cargo.lock")).ok();
        let mut cmd = Command::new("cargo");
        crate::build::no_window(&mut cmd)
            .args(["metadata", "--format-version", "1"])
            .current_dir(&project_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null());
        let output = cmd.output();
        let lock_after = std::fs::read(project_dir.join("Cargo.lock")).ok();
        restamp_lock_seed(
            &project_dir,
            &workspace,
            lock_before.as_deref(),
            lock_after.as_deref(),
        );
        let result = match output {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(clean_cargo_error(&String::from_utf8_lossy(&out.stderr))),
            // cargo missing → the health check is meaningless, not a load error.
            Err(_) => Ok(()),
        };
        *state.lock().unwrap() = Some(result);
        ctx.request_repaint();
    });
}

/// Trim cargo's `error:`/`Caused by:` stderr into a short, readable reason.
/// Keeps the first `error:` line and the most specific `Caused by:` tail, so the
/// modal shows "failed to select a version…" not the whole backtrace.
fn clean_cargo_error(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return "cargo metadata failed (no output)".to_owned();
    }
    let head = lines
        .iter()
        .find(|l| l.starts_with("error"))
        .copied()
        .unwrap_or(lines[0]);

    let mut out = vec![head];
    // The LAST `Caused by:` entry is usually the root cause.
    if let Some(cause) = lines
        .iter()
        .rposition(|l| l.starts_with("Caused by:"))
        .and_then(|i| lines.get(i + 1))
        .copied()
    {
        out.push(cause);
    }
    // Cargo's DEPENDENCY-RESOLUTION failures carry no `Caused by:` at all: the
    // head says only "failed to select a version for `x`" and the reason sits in
    // the middle of the report. Keeping just the head left the user with a
    // message that named the crate and nothing else — the reported case was a
    // chip feature that does not exist, and the line saying so was dropped.
    for l in &lines {
        if is_resolution_detail(l) && !out.contains(l) {
            out.push(l);
        }
    }
    out.join("\n")
}

/// Does this stderr line carry the REASON a dependency failed to resolve?
///
/// Matched by cargo's own phrasing rather than by position: these lines appear
/// between the head and the trailing summary, in an order that varies.
fn is_resolution_detail(line: &str) -> bool {
    const MARKERS: [&str; 5] = [
        // "…, with features: `stm32h5f4aj` but `embassy-stm32` does not have
        // these features." — the one that names the missing feature.
        "does not have these features",
        // "candidate versions found which didn't match: 0.4.0"
        "candidate versions found",
        // "versions that meet the requirements `^0.4` are: 0.4.0"
        "versions that meet the requirements",
        // "location searched: crates.io index" / "required by package `x`"
        "location searched",
        "no matching package named",
    ];
    MARKERS.iter().any(|m| line.contains(m))
}

/// The `ProjectFiles`-derived half of the git snapshot: every generated file
/// `write_project` puts on disk, keyed by project-relative path.
///
/// Split out of `git_disk_snapshot` so it can be checked against
/// `git_tab::is_ide_managed` directly — the two lists had drifted, and nothing
/// tied them together.
///
/// The conditional ones are conditional for real: `memory.x` and `build.rs` are
/// empty on EspRust, and `rust-toolchain.toml` exists ONLY on the three Xtensa
/// parts. That last one was missing here entirely, which made this function's
/// own promise false for esp32 / esp32s2 / esp32s3: switching a saved project
/// to one of them left the pin in memory and absent from disk, and the Git tab
/// reported the tree clean. The commit then carried an Xtensa `Cargo.toml` with
/// no toolchain to build it, and the failure surfaced on someone else's clone
/// as `'esp32s3' is not a recognized processor`.
///
/// A file that is empty is one `write_project` does not write, so it is left
/// out rather than tracked as an empty file that never matches disk.
fn generated_files_snapshot(
    files: crate::panels::mcu_module::project_gen::ProjectFiles,
) -> Vec<(String, String)> {
    let mut snap = vec![
        ("src/main.rs".to_owned(), files.main_rs),
        ("Cargo.toml".to_owned(), files.cargo_toml),
        (".cargo/config.toml".to_owned(), files.cargo_config),
        (".gitignore".to_owned(), files.gitignore),
    ];
    for (rel, content) in [
        ("memory.x", files.memory_x),
        ("build.rs", files.build_rs),
        ("rust-toolchain.toml", files.rust_toolchain),
    ] {
        if !content.is_empty() {
            snap.push((rel.to_owned(), content));
        }
    }
    snap
}

/// The conditional files this project must NOT have on disk.
///
/// The mirror of the empty-content skip above, and the half that was missing.
/// `write_project` DELETES each of these when its content is empty, but a
/// snapshot keyed on content cannot express "this should be gone", so the state
/// between a chip change and the next Save read as clean.
///
/// The case that makes it matter is an Espressif one: switch a saved project
/// from an esp32s3 to an esp32c6 and the Xtensa `rust-toolchain.toml` is stale
/// the moment the chip changes. Committing before saving pins a RISC-V project
/// to the `esp` toolchain — which still builds, so nothing complains, and the
/// repo quietly says something untrue about itself. `memory.x` and `build.rs`
/// have the same shape when moving between an STM32 and an ESP.
fn generated_files_that_must_be_absent(
    files: &crate::panels::mcu_module::project_gen::ProjectFiles,
) -> Vec<&'static str> {
    [
        ("memory.x", &files.memory_x),
        ("build.rs", &files.build_rs),
        ("rust-toolchain.toml", &files.rust_toolchain),
    ]
    .into_iter()
    .filter(|(_, c)| c.is_empty())
    .map(|(rel, _)| rel)
    .collect()
}

impl AppIde {
    /// The in-memory project content, keyed by project-relative path — the
    /// exact file set `write_project` persists. The git worker compares it
    /// against disk for the "unsaved changes" warning (commits are strictly
    /// what's ON DISK; this only powers the warning).
    fn git_disk_snapshot(&self) -> Vec<(String, String)> {
        let mut snap = generated_files_snapshot(self.current_project_files());
        let mcu_cfg = self.mcu_config_text();
        if !mcu_cfg.trim().is_empty() {
            snap.push((
                crate::panels::mcu_module::mcu_config::FILE_NAME.to_owned(),
                mcu_cfg,
            ));
        }
        for (rel, content) in &self.project_tree.user_src_files {
            snap.push((rel.clone(), content.clone()));
        }
        snap
    }

    /// Project-relative paths whose in-memory content differs from disk — what
    /// closing the app right now would lose. Empty = everything is saved.
    ///
    /// For a project that was NEVER saved there is nothing to diff against, so
    /// it counts as unsaved only when the user actually built something
    /// (configured a pin, added a module or a source file). Otherwise a
    /// pristine start-up state would nag on every exit.
    pub(super) fn unsaved_files(&self) -> Vec<String> {
        let snapshot = self.git_disk_snapshot();
        match &self.project_dir {
            Some(dir) => crate::git::unsaved_changes(
                dir,
                &snapshot,
                &generated_files_that_must_be_absent(&self.current_project_files()),
            ),
            None => {
                let has_content = !self.project_tree.user_src_files.is_empty()
                    || self.mcu.as_ref().is_some_and(|m| {
                        !m.modules.is_empty()
                            || m.iter_all_pins()
                                .any(|p| p.selected_function != PinFunction::Unset)
                    });
                if has_content {
                    snapshot.into_iter().map(|(p, _)| p).collect()
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Reverse ONE hunk of a changed file — the Git diff view's per-hunk revert
    /// (Phase B). Reconstructs that hunk's patch from the open diff and applies
    /// it in reverse on disk, then refreshes the file's in-memory buffer so the
    /// next Save doesn't clobber it. Returns true when a buffer changed (the
    /// caller then refreshes the editor's `display_code`). IDE-managed files are
    /// refused (main.rs is better reverted hunk-by-hunk in the editor gutter).
    pub(super) fn apply_hunk_revert(&mut self, path: &str, hunk_row: usize) -> bool {
        let Some(dir) = self.git_dir() else {
            return false;
        };
        // `path` came from git and is relative to the ACTIVE repo; the tree and
        // the IDE-managed rules are keyed from the project root.
        let key = self.git_path_to_project(path);
        if crate::app::tabs::git_tab::is_ide_managed(&key) {
            self.git.state.lock().unwrap().lines.push((
                crate::git::GitLine::Notice,
                format!("[skip] {path} is IDE-managed — change it via the UI (for main.rs, use the editor gutter to revert hunks)"),
            ));
            return false;
        }
        // Build the single-hunk patch from the currently-open diff.
        let patch = {
            let st = self.git.state.lock().unwrap();
            match &st.diff {
                Some(d) if d.path == path => crate::git::hunk_patch(path, &d.rows, hunk_row),
                _ => None,
            }
        };
        let Some(patch) = patch else {
            self.git.state.lock().unwrap().lines.push((
                crate::git::GitLine::Notice,
                format!("[error] couldn't build the revert patch for {path}"),
            ));
            return false;
        };

        match crate::git::apply_reverse_patch(&dir, &patch) {
            Ok(()) => {
                // Refresh the in-memory buffer from the now-reverted disk file.
                // Only user files reach here (managed ones are refused), and
                // `path` is already project-root-relative — the same key the
                // tree uses.
                let mut changed = false;
                if let Ok(disk) = std::fs::read_to_string(dir.join(path)) {
                    let disk = disk.replace("\r\n", "\n");
                    if let Some(entry) = self
                        .project_tree
                        .user_src_files
                        .iter_mut()
                        .find(|(p, _)| *p == key)
                    {
                        entry.1 = disk;
                        changed = true;
                    }
                }
                {
                    let mut st = self.git.state.lock().unwrap();
                    st.op_gen += 1; // refresh the editor gutter's HEAD baseline marks
                    st.lines.push((
                        crate::git::GitLine::Notice,
                        format!("reverted a hunk in {path}"),
                    ));
                }
                // Re-open the diff so the remaining hunks show (or it closes if
                // the file now matches HEAD).
                crate::git::run_diff(
                    path.to_owned(),
                    false,
                    dir,
                    std::sync::Arc::clone(&self.git.state),
                    self.egui_ctx.clone(),
                );
                changed
            }
            Err(e) => {
                self.git.state.lock().unwrap().lines.push((
                    crate::git::GitLine::Notice,
                    format!("[error] git apply --reverse failed: {e}"),
                ));
                false
            }
        }
    }

    /// Discard a whole file's changes — Phase A. A TRACKED file is restored to
    /// its HEAD version (disk + in-memory buffer); an UNTRACKED file is deleted
    /// (disk + buffer + tree selection remap). Returns true when the open file's
    /// buffer was updated in place (the caller then refreshes `display_code`).
    /// IDE-managed files are refused; the caller confirms via a dialog first.
    /// Restore ONE file to its content at `sha` (History view).
    ///
    /// Scoped on purpose: only this file's buffer is refreshed, so unsaved
    /// edits elsewhere survive and no project reload is needed. HEAD does not
    /// move — the result is an ordinary uncommitted change, visible in the
    /// Changes view and undoable with Discard.
    ///
    /// Returns `true` when a buffer changed, so the caller can refresh the
    /// editor's working copy (otherwise the end-of-frame write-back would put
    /// the old text straight back).
    pub(super) fn apply_restore_from_commit(&mut self, sha: &str, path: &str) -> bool {
        let Some(dir) = self.git_dir() else {
            return false;
        };
        // git reports paths from the ACTIVE repo root; buffers are keyed from
        // the project root.
        let key = self.git_path_to_project(path);
        if crate::app::tabs::git_tab::is_ide_managed(&key) {
            self.git_note(format!(
                "[skip] {path} is IDE-managed — it is regenerated from the MCU configuration"
            ));
            return false;
        }
        let content = match crate::git::restore_file_at(&dir, sha, path) {
            Ok(c) => c,
            Err(e) => {
                self.git_note(format!("[error] restore {path}: {e}"));
                return false;
            }
        };

        // Update the in-memory buffer — and ADD the file when it isn't tracked
        // any more (it was deleted after `sha`). Writing it to disk WITHOUT
        // registering it would let `write_project`'s stale-file prune delete it
        // again at the next save.
        if let Some(e) = self
            .project_tree
            .user_src_files
            .iter_mut()
            .find(|(p, _)| *p == key)
        {
            e.1 = content;
        } else {
            self.project_tree
                .user_src_files
                .push((key.clone(), content));
        }
        self.cached_project_files = None;
        {
            let mut st = self.git.state.lock().unwrap();
            st.op_gen += 1; // refresh the editor gutter's HEAD baseline marks
            st.lines.push((
                crate::git::GitLine::Notice,
                format!("[ok] restored {path} from {}", &sha[..sha.len().min(7)]),
            ));
        }
        self.request_save = true;
        true
    }

    pub(super) fn apply_discard_file(&mut self, path: &str) -> bool {
        let Some(dir) = self.git_dir() else {
            return false;
        };
        let key = self.git_path_to_project(path);
        if crate::app::tabs::git_tab::is_ide_managed(&key) {
            self.git_note(format!(
                "[skip] {path} is IDE-managed — change it via the UI (for main.rs, use the editor gutter)"
            ));
            return false;
        }
        let untracked = self
            .git
            .state
            .lock()
            .unwrap()
            .status
            .changes
            .iter()
            .any(|c| c.path == path && c.code == "??");

        let changed = if untracked {
            // Untracked → delete the file (irreversible) + drop its buffer/tree.
            match std::fs::remove_file(dir.join(path)) {
                Ok(()) => {
                    {
                        if let Some(idx) = self
                            .project_tree
                            .user_src_files
                            .iter()
                            .position(|(p, _)| *p == key)
                        {
                            self.project_tree.user_src_files.remove(idx);
                            // Remap the index-based selection across the removal.
                            if let ProjectFileId::UserFile(sel) = self.selected_file {
                                self.selected_file = if sel == idx {
                                    ProjectFileId::MainRs
                                } else if sel > idx {
                                    ProjectFileId::UserFile(sel - 1)
                                } else {
                                    ProjectFileId::UserFile(sel)
                                };
                            }
                        }
                    }
                    self.git_note(format!("Deleted untracked {path}"));
                    false
                }
                Err(e) => {
                    self.git_note(format!("[error] couldn't delete {path}: {e}"));
                    false
                }
            }
        } else {
            // Tracked → restore its HEAD version on disk + refresh the buffer.
            match crate::git::restore_file_to_head(&dir, path) {
                Ok(content) => {
                    let mut c = false;
                    {
                        if let Some(entry) = self
                            .project_tree
                            .user_src_files
                            .iter_mut()
                            .find(|(p, _)| *p == key)
                        {
                            entry.1 = content;
                            c = true;
                        }
                    }
                    self.git_note(format!("Restored {path} to HEAD"));
                    c
                }
                Err(e) => {
                    self.git_note(format!("[error] couldn't discard {path}: {e}"));
                    false
                }
            }
        };

        self.git.state.lock().unwrap().op_gen += 1; // refresh gutter baseline
        self.run_git_op(crate::git::GitOp::Refresh); // status + clears the diff
        changed
    }

    /// Discard ALL uncommitted changes back to HEAD — Phase C. `git reset
    /// --hard` + `git clean -fd`, then RELOAD the whole project from disk
    /// (`load_project_from_dir` re-reads main.rs/pins, config files, user files,
    /// mcu.config) so every in-memory buffer matches the reset tree. The caller
    /// confirms first and gates on `has_commits`.
    pub(super) fn apply_discard_all(&mut self) {
        let Some(dir) = self.git_dir() else {
            return;
        };
        match crate::git::discard_all_to_head(&dir) {
            Ok(()) => {
                // Rebuild every buffer from the now-reset disk. Reload the
                // PROJECT, never `dir`: when a LIBRARY repo is the selected
                // target, `dir` is the library's folder — loading THAT as the
                // project would repoint `project_dir` at the library and
                // re-detect the chip from its manifest (the reported "Discard
                // all doesn't work for the selected repo"). The project reload
                // rescans src/ + members + detached libs, so a discarded
                // library's files refresh either way.
                let reload = self.project_dir.clone().unwrap_or(dir);
                self.load_project_from_dir(&reload);
                self.git_note("Discarded all changes (reset --hard + clean)".into());
            }
            Err(e) => self.git_note(format!("[error] discard-all failed: {e}")),
        }
        self.git.state.lock().unwrap().op_gen += 1; // refresh gutter baseline
        self.run_git_op(crate::git::GitOp::Refresh);
    }

    /// Push a one-off notice line into the Git tab's output scrollback.
    fn git_note(&self, msg: String) {
        self.git
            .state
            .lock()
            .unwrap()
            .lines
            .push((crate::git::GitLine::Notice, msg));
    }
}

/// A folder name derived from a chip's display name.
///
/// Display names carry things a folder should not (`STM32C011D6Yx (WLCSP12)`),
/// so the characters Windows refuses become `_`, runs collapse, and the trailing
/// dot/space Windows silently strips is trimmed. Empty input — a chip with no
/// name — falls back to `project`, because a folder must be called something.
pub(super) fn folder_name_for_chip(chip: &str) -> String {
    let mut out = String::with_capacity(chip.len());
    for c in chip.trim().chars() {
        let ok = c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        let ch = if ok { c } else { '_' };
        // Collapse runs of the replacement so "(WLCSP12)" doesn't become "__…__".
        if ch == '_' && out.ends_with('_') {
            continue;
        }
        out.push(ch);
    }
    let out = out
        .trim_matches(|c| c == '_' || c == '.' || c == ' ')
        .to_owned();
    if valid_project_name(&out).is_ok() {
        out
    } else {
        "project".to_owned()
    }
}

/// Where a NEW project's folder goes: `<parent>/<chip>`, or `<chip>_1`, `_2`, …
/// when that name is taken.
///
/// The user picks a PARENT in the file dialog and the project gets its own
/// folder underneath — saving straight into the picked folder used to scatter
/// `Cargo.toml`, `src/`, `memory.x` into whatever directory happened to be
/// selected, and a second project into the same place would have overwritten the
/// first.
///
/// `exists` is injected so the numbering rule is testable without a filesystem.
pub(super) fn new_project_dir(
    parent: &std::path::Path,
    chip: &str,
    exists: impl Fn(&std::path::Path) -> bool,
) -> std::path::PathBuf {
    let base = folder_name_for_chip(chip);
    let first = parent.join(&base);
    if !exists(&first) {
        return first;
    }
    // Bounded: a parent with 10 000 same-named projects is not a real case, and
    // an unbounded loop on a lying `exists` would hang the UI thread.
    for n in 1..10_000 {
        let candidate = parent.join(format!("{base}_{n}"));
        if !exists(&candidate) {
            return candidate;
        }
    }
    parent.join(format!("{base}_{}", std::process::id()))
}

/// Validate a project FOLDER name for Windows: non-empty, no reserved
/// characters, no trailing dot/space, not a reserved device name.
pub(super) fn valid_project_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("Name cannot be empty".into());
    }
    if n.chars().any(|c| {
        matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20
    }) {
        return Err(r#"Name cannot contain < > : " / \ | ? *"#.into());
    }
    if n.ends_with('.') || n.ends_with(' ') {
        return Err("Name cannot end with a dot or a space".into());
    }
    let stem = n.split('.').next().unwrap_or(n).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit()));
    if reserved {
        return Err(format!("\"{n}\" is a reserved Windows name"));
    }
    Ok(())
}

/// Rename `old_dir`'s LEAF to `new_name` (same parent → same drive, so
/// `fs::rename` always applies). Returns the new path. A case-only change is
/// allowed (the `exists()` collision check would false-positive on Windows'
/// case-insensitive filesystem); renaming to the identical name is a no-op.
pub(super) fn rename_project_dir(
    old_dir: &std::path::Path,
    new_name: &str,
) -> Result<std::path::PathBuf, String> {
    let new_name = new_name.trim();
    valid_project_name(new_name)?;
    let parent = old_dir
        .parent()
        .ok_or("Project folder has no parent directory")?;
    let new_dir = parent.join(new_name);
    let old_leaf = old_dir
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    if old_leaf == new_name {
        return Ok(old_dir.to_path_buf()); // unchanged
    }
    let case_only = old_leaf.eq_ignore_ascii_case(new_name);
    if !case_only && new_dir.exists() {
        return Err(format!("\"{new_name}\" already exists here"));
    }
    std::fs::rename(old_dir, &new_dir).map_err(|e| format!("Rename failed: {e}"))?;
    Ok(new_dir)
}

/// Apply one watcher CREATE event to the tree state. A directory is tracked as
/// a FOLDER (never a file); a file needs `read` to return its content (`None` —
/// deleted meanwhile, or unreadable — is skipped, NOT pushed as an empty phantom).
/// Duplicates of already-tracked entries are ignored. Pure, so the
/// directory-pushed-as-file regression stays covered by tests.
///
/// `read` runs only for a file the tree does not know yet: Windows reports one
/// creation as several records, and the IDE's own writes echo back, so reading
/// before the check read the same file once per duplicate on the UI thread.
pub(super) fn apply_fs_create(
    user_src_files: &mut Vec<(String, String)>,
    user_src_folders: &mut Vec<String>,
    rel: &str,
    is_dir: bool,
    read: impl FnOnce() -> Option<String>,
) {
    if is_dir {
        if !user_src_folders.iter().any(|f| f == rel) {
            user_src_folders.push(rel.to_owned());
        }
        return;
    }
    if user_src_files.iter().any(|(p, _)| p == rel) {
        return;
    }
    if let Some(content) = read() {
        user_src_files.push((rel.to_owned(), content));
    }
}

// `SavedPins` / `saved_pins` live in `mcu_config` now: the Board tab reads the
// pins of a chip it has not opened, and has to read them the way opening does.
use crate::panels::mcu_module::mcu_config::{SavedPins, saved_pins};

#[cfg(test)]
mod saved_pins_tests {
    use super::{SavedPins, saved_pins};
    use crate::panels::mcu_module::codegen::common::{GEN_BEGIN, GEN_END};
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    /// An STM32 file, in the shape `parse_main_rs` has always read.
    fn stm32_main_rs() -> String {
        format!(
            "{GEN_BEGIN}\nlet pc13 = &mut gpioc.pc13.into_push_pull_output(&mut gpioc.crh); // GPIO Output\n{GEN_END}\n"
        )
    }

    /// An nRF file: the pad name in a comment, no function label anywhere.
    fn nrf_main_rs() -> String {
        format!(
            "{GEN_BEGIN}\n// P0.21\nlet mut p0_21_out = Output::new(p.P0_21, Level::Low, OutputDrive::Standard);\n{GEN_END}\n"
        )
    }

    const WITH_PINS: &str = "@runtime\nAsync\n\n@pins\n7=GPIO Output\n";
    const WITHOUT_PINS: &str = "@runtime\nAsync\n";

    /// A project with no `@pins` - no config at all, or one written before
    /// the section existed - still restores from main.rs, so STM32 and ESP
    /// projects open exactly as before.
    #[test]
    fn a_project_without_the_section_still_restores_from_main_rs() {
        for cfg in [None, Some(WITHOUT_PINS)] {
            match saved_pins(cfg, &stm32_main_rs()) {
                Some(SavedPins::ByName(pins)) => {
                    assert_eq!(pins, vec![("PC13".to_owned(), PinFunction::GpioOutput)])
                }
                other => panic!("{cfg:?}: {other:?}"),
            }
        }
    }

    /// The section is the diagram's own record, so it wins over the parse -
    /// and against an nRF file it is the only record there is.
    #[test]
    fn the_section_wins_when_present() {
        for source in [stm32_main_rs(), nrf_main_rs()] {
            match saved_pins(Some(WITH_PINS), &source) {
                Some(SavedPins::ByNumber(pins)) => {
                    assert_eq!(pins, vec![(7, PinFunction::GpioOutput)])
                }
                other => panic!("{source}: {other:?}"),
            }
        }
    }

    /// The bug this exists for: an nRF file without the section has nothing
    /// to restore, and the open path resets the diagram rather than guessing.
    /// An nRF project saved before `@pins` opens this way once.
    #[test]
    fn an_nrf_file_without_the_section_has_nothing_to_restore() {
        assert!(saved_pins(None, &nrf_main_rs()).is_none());
        assert!(saved_pins(Some(WITHOUT_PINS), &nrf_main_rs()).is_none());
    }
}

#[cfg(test)]
mod cargo_error_tests {
    use super::clean_cargo_error;

    /// The reported case: an imported chip whose `embassy-stm32` feature does
    /// not exist. Cargo emits NO `Caused by:` here, so the old cleaner kept only
    /// the head — the user saw the crate name and nothing that could be acted on.
    #[test]
    fn a_missing_feature_is_explained_not_just_named() {
        const STDERR: &str = "\
error: failed to select a version for `embassy-stm32`.
    ... required by package `blink v0.1.0 (/tmp/x)`
versions that meet the requirements `^0.4` are: 0.4.0

the package `blink` depends on `embassy-stm32`, with features: `stm32h5f4aj` but `embassy-stm32` does not have these features.

failed to select a version for `embassy-stm32` which could resolve this conflict";
        let msg = clean_cargo_error(STDERR);
        assert!(msg.starts_with("error: failed to select a version"));
        assert!(
            msg.contains("does not have these features"),
            "the actionable line must survive: {msg}"
        );
        assert!(
            msg.contains("stm32h5f4aj"),
            "and it must still name the offending feature: {msg}"
        );
    }

    /// A missing crate names what was searched for and where.
    #[test]
    fn a_missing_crate_keeps_its_reason() {
        const STDERR: &str = "\
error: no matching package named `embassy-stm32-nope` found
location searched: registry `crates.io`
required by package `blink v0.1.0`";
        let msg = clean_cargo_error(STDERR);
        assert!(msg.contains("location searched"));
    }

    /// The `Caused by:` path (a malformed manifest) is unchanged.
    #[test]
    fn a_caused_by_error_still_reports_its_root_cause() {
        const STDERR: &str = "\
error: failed to parse manifest at `/tmp/x/Cargo.toml`

Caused by:
  invalid type: string, expected a table";
        let msg = clean_cargo_error(STDERR);
        assert!(msg.contains("failed to parse manifest"));
        assert!(msg.contains("invalid type: string"));
    }

    #[test]
    fn empty_output_says_so_instead_of_being_blank() {
        assert!(clean_cargo_error("   \n\n").contains("no output"));
    }
}

#[cfg(test)]
mod new_project_dir_tests {
    use super::{folder_name_for_chip, new_project_dir};
    use std::path::{Path, PathBuf};

    /// The requested behaviour: a new project lands in its own folder named
    /// after the chip, and repeats get `_1`, `_2`, …
    #[test]
    fn the_folder_is_the_chip_name_then_numbered() {
        let parent = Path::new("/projects");
        let taken: Vec<PathBuf> =
            vec![parent.join("STM32F217ZGTx"), parent.join("STM32F217ZGTx_1")];
        let exists = |p: &Path| taken.iter().any(|t| t == p);

        // Free name: no suffix at all.
        assert_eq!(
            new_project_dir(parent, "STM32F411RETx", exists),
            parent.join("STM32F411RETx")
        );
        // Taken, and `_1` too → the first free number.
        assert_eq!(
            new_project_dir(parent, "STM32F217ZGTx", exists),
            parent.join("STM32F217ZGTx_2")
        );
    }

    /// A display name is not a folder name.
    #[test]
    fn the_chip_name_is_made_safe_for_a_folder() {
        assert_eq!(folder_name_for_chip("STM32F217ZGTx"), "STM32F217ZGTx");
        // Spaces and brackets collapse into single underscores, none trailing.
        assert_eq!(
            folder_name_for_chip("STM32C011D6Yx (WLCSP12)"),
            "STM32C011D6Yx_WLCSP12"
        );
        // Path separators must never survive — they would escape the parent.
        assert_eq!(folder_name_for_chip("a/b\\c"), "a_b_c");
        // Windows strips a trailing dot or space, so we must not create one.
        assert_eq!(folder_name_for_chip("chip. "), "chip");
        // Nothing usable, and a reserved device name, both fall back.
        assert_eq!(folder_name_for_chip("   "), "project");
        assert_eq!(folder_name_for_chip("CON"), "project");
    }

    /// Whatever the chip is called, the result stays INSIDE the chosen parent.
    #[test]
    fn the_result_never_escapes_the_parent() {
        let parent = Path::new("/projects");
        for chip in ["../../etc", "C:\\Windows", "..", "a/../b"] {
            let dir = new_project_dir(parent, chip, |_| false);
            assert_eq!(
                dir.parent(),
                Some(parent),
                "{chip} produced {}",
                dir.display()
            );
        }
    }
}

#[cfg(test)]
mod rename_project_tests {
    use super::{rename_project_dir, valid_project_name};

    #[test]
    fn name_validation() {
        assert!(valid_project_name("my_project-2").is_ok());
        assert!(valid_project_name("  ").is_err(), "empty");
        assert!(valid_project_name("a/b").is_err(), "path separator");
        assert!(valid_project_name("a:b").is_err(), "colon");
        assert!(valid_project_name("done.").is_err(), "trailing dot");
        assert!(valid_project_name("CON").is_err(), "reserved device");
        assert!(valid_project_name("com3").is_err(), "reserved device");
        assert!(
            valid_project_name("COMET").is_ok(),
            "COM prefix but not COMn"
        );
    }

    #[test]
    fn renames_leaf_and_reports_collisions() {
        let base = std::env::temp_dir().join(format!("eide_rename_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let old = base.join("proj_a");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("marker.txt"), "x").unwrap();

        // Plain rename moves the folder and its contents.
        let newp = rename_project_dir(&old, "proj_b").unwrap();
        assert_eq!(newp, base.join("proj_b"));
        assert!(newp.join("marker.txt").exists());
        assert!(!old.exists());

        // Renaming to an existing sibling is refused.
        std::fs::create_dir_all(base.join("proj_c")).unwrap();
        assert!(rename_project_dir(&newp, "proj_c").is_err());

        // Same name → no-op; case-only change is allowed on Windows.
        assert_eq!(rename_project_dir(&newp, "proj_b").unwrap(), newp);
        let cased = rename_project_dir(&newp, "Proj_B").unwrap();
        assert!(cased.join("marker.txt").exists());

        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod fs_create_tests {
    use super::apply_fs_create;

    /// The reported bug: creating `folder1` fired a watcher CREATE that was
    /// pushed into `user_src_files` — the phantom file then shadowed the
    /// folder node in the tree (same map key), showing an extension-less
    /// "file" instead of the folder until the project was reopened.
    #[test]
    fn directory_create_is_tracked_as_folder_not_file() {
        let mut files = Vec::new();
        let mut folders = Vec::new();
        apply_fs_create(&mut files, &mut folders, "folder1", true, || None);
        assert!(
            files.is_empty(),
            "a directory must never become a file entry"
        );
        assert_eq!(folders, vec!["folder1".to_owned()]);
        // Re-delivered event (or our own create + the watcher's) → no dupe.
        apply_fs_create(&mut files, &mut folders, "folder1", true, || None);
        assert_eq!(folders.len(), 1);
    }

    #[test]
    fn file_create_adds_once_with_content() {
        let mut files = Vec::new();
        let mut folders = Vec::new();
        apply_fs_create(&mut files, &mut folders, "folder1/file1.rs", false, || {
            Some("// x\n".into())
        });
        assert_eq!(
            files,
            vec![("folder1/file1.rs".to_owned(), "// x\n".to_owned())]
        );
        // The IDE's own inline-create already tracked it → the watcher's echo
        // must not duplicate (or overwrite newer in-memory content).
        apply_fs_create(&mut files, &mut folders, "folder1/file1.rs", false, || {
            Some("stale".into())
        });
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, "// x\n");
        assert!(folders.is_empty());
    }

    #[test]
    fn unreadable_file_is_skipped_not_pushed_empty() {
        let mut files = Vec::new();
        let mut folders = Vec::new();
        apply_fs_create(&mut files, &mut folders, "ghost.rs", false, || None);
        assert!(files.is_empty(), "no phantom (\"ghost.rs\", \"\") entries");
        assert!(folders.is_empty());
    }

    /// Windows delivers one creation as several records. Each used to read the
    /// file on the UI thread before the duplicate check threw the result away.
    #[test]
    fn duplicate_creates_read_the_file_once() {
        let mut files = Vec::new();
        let mut folders = Vec::new();
        let mut reads = 0;
        for _ in 0..50 {
            apply_fs_create(&mut files, &mut folders, "new.rs", false, || {
                reads += 1;
                Some("fn f() {}\n".into())
            });
        }
        assert_eq!(reads, 1);
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn a_file_the_tree_already_has_is_never_read() {
        let mut files = vec![("known.rs".to_owned(), "in memory\n".to_owned())];
        let mut folders = Vec::new();
        apply_fs_create(&mut files, &mut folders, "known.rs", false, || {
            panic!("an echo of a tracked file must not touch the disk")
        });
        assert_eq!(files[0].1, "in memory\n");
    }
}

#[cfg(test)]
mod fs_watch_tests {
    use super::{WatchAction, sync_watch, watch_action};
    use std::path::{Path, PathBuf};

    /// Counts what the real backend would be asked to do.
    #[derive(Default)]
    struct CountingWatcher {
        watches: Vec<PathBuf>,
        unwatches: Vec<PathBuf>,
    }

    impl notify::Watcher for CountingWatcher {
        fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
            Ok(Self::default())
        }
        fn watch(&mut self, path: &Path, _: notify::RecursiveMode) -> notify::Result<()> {
            self.watches.push(path.to_path_buf());
            Ok(())
        }
        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            self.unwatches.push(path.to_path_buf());
            Ok(())
        }
        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::NullWatcher
        }
    }

    /// Every frame used to call `watch()`; each call leaked a live watch on
    /// Windows. However often the check runs, the backend sees ONE call.
    #[test]
    fn five_hundred_checks_watch_once() {
        let mut w = CountingWatcher::default();
        let mut watched = None;
        let src = Path::new("ws/src");
        for _ in 0..500 {
            sync_watch(&mut w, &mut watched, src, true);
        }
        assert_eq!(w.watches.len(), 1);
        assert!(w.unwatches.is_empty());
        assert_eq!(watched.as_deref(), Some(src));
    }

    #[test]
    fn a_vanished_dir_is_released_once_and_rewatched_when_it_returns() {
        let mut w = CountingWatcher::default();
        let mut watched = None;
        let src = Path::new("ws/src");
        sync_watch(&mut w, &mut watched, src, true);
        for _ in 0..10 {
            sync_watch(&mut w, &mut watched, src, false);
        }
        assert_eq!(w.unwatches, vec![src.to_path_buf()]);
        assert_eq!(watched, None);
        sync_watch(&mut w, &mut watched, src, true);
        assert_eq!(w.watches.len(), 2);
    }

    #[test]
    fn a_new_target_releases_the_old_watch_first() {
        let mut w = CountingWatcher::default();
        let mut watched = None;
        sync_watch(&mut w, &mut watched, Path::new("old/src"), true);
        sync_watch(&mut w, &mut watched, Path::new("new/src"), true);
        assert_eq!(w.unwatches, vec![PathBuf::from("old/src")]);
        assert_eq!(w.watches.len(), 2);
        assert_eq!(watched.as_deref(), Some(Path::new("new/src")));
    }

    /// A failed `watch` must leave nothing recorded, so the next check retries
    /// instead of believing the directory is covered.
    #[test]
    fn a_failed_watch_is_not_recorded() {
        struct Failing;
        impl notify::Watcher for Failing {
            fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
                Ok(Self)
            }
            fn watch(&mut self, _: &Path, _: notify::RecursiveMode) -> notify::Result<()> {
                Err(notify::Error::path_not_found())
            }
            fn unwatch(&mut self, _: &Path) -> notify::Result<()> {
                Ok(())
            }
            fn kind() -> notify::WatcherKind {
                notify::WatcherKind::NullWatcher
            }
        }
        let mut watched = None;
        sync_watch(&mut Failing, &mut watched, Path::new("ws/src"), true);
        assert_eq!(watched, None);
    }

    /// The regression this guards: the same path, still there, must produce
    /// no notify call at all. Asking again is what leaked a watch per frame.
    #[test]
    fn an_already_watched_existing_dir_is_left_alone() {
        let src = Path::new("ws/src");
        assert_eq!(watch_action(Some(src), src, true), WatchAction::Keep);
    }

    #[test]
    fn a_dir_that_appears_is_watched_once() {
        let src = Path::new("ws/src");
        assert_eq!(watch_action(None, src, true), WatchAction::Watch);
        assert_eq!(watch_action(None, src, false), WatchAction::Keep);
    }

    #[test]
    fn a_watched_dir_that_vanished_is_released() {
        let src = Path::new("ws/src");
        assert_eq!(watch_action(Some(src), src, false), WatchAction::Unwatch);
    }

    #[test]
    fn a_different_target_replaces_the_old_watch() {
        assert_eq!(
            watch_action(Some(Path::new("old/src")), Path::new("new/src"), true),
            WatchAction::Rewatch
        );
    }

    /// End to end against the real Windows backend, the one that leaked: many
    /// checks on one directory, then one write, must deliver a handful of
    /// events, not two per check. (inotify reuses the watch for a duplicate
    /// path, so elsewhere this could not tell the two apart.)
    #[cfg(windows)]
    #[test]
    fn repeated_checks_do_not_multiply_events() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut w = notify::recommended_watcher(move |ev| {
            let _ = tx.send(ev);
        })
        .unwrap();
        let mut watched = None;
        for _ in 0..500 {
            sync_watch(&mut w, &mut watched, &src, src.is_dir());
        }
        std::fs::write(src.join("a.rs"), "fn a() {}\n").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the single watch must still deliver"
        );
        let mut events = 1;
        while rx.recv_timeout(Duration::from_millis(300)).is_ok() {
            events += 1;
        }
        assert!(
            events <= 10,
            "{events} events from one write: the watch was re-added per check"
        );
    }
}

/// External renames through `poll_fs_events`, the way the frame loop drives it.
#[cfg(test)]
mod fs_rename_poll_tests {
    use super::super::{AppIde, ProjectFileId};
    use eframe::egui;
    use notify::event::{ModifyKind, RemoveKind, RenameMode};
    use notify::{Event, EventKind};
    use std::path::PathBuf;
    use std::sync::mpsc::Sender;

    fn file(path: &str, content: &str) -> (String, String) {
        (path.to_owned(), content.to_owned())
    }

    /// An app whose watcher channel the test feeds itself. The real watch on
    /// the IDE's workspace is dropped; nothing here reads or writes that disk.
    fn app(
        files: &[(&str, &str)],
        folders: &[&str],
        selected: usize,
    ) -> (AppIde, Sender<notify::Result<Event>>) {
        let ctx = egui::Context::default();
        let mut app = AppIde::new(&eframe::CreationContext::_new_kittest(ctx), None, None);
        app._fs_watcher = None;
        app.fs_watched = None;
        let (tx, rx) = std::sync::mpsc::channel();
        app.fs_rx = Some(rx);
        app.project_tree.user_src_files = files.iter().map(|(p, c)| file(p, c)).collect();
        app.project_tree.user_src_folders = folders.iter().map(|f| (*f).to_owned()).collect();
        app.selected_file = ProjectFileId::UserFile(selected);
        (app, tx)
    }

    fn ws(rel: &str) -> PathBuf {
        crate::workspace::dir().join(rel)
    }

    fn name(mode: RenameMode, rel: &str) -> notify::Result<Event> {
        Ok(Event::new(EventKind::Modify(ModifyKind::Name(mode))).add_path(ws(rel)))
    }

    /// The reported bug: Windows says `From` + `To`, and only `Both` was
    /// handled - the tree kept the old name and never showed the new one.
    #[test]
    fn a_windows_rename_reaches_the_tree() {
        let (mut app, tx) = app(
            &[("src/keep.rs", "k"), ("src/a.rs", "unsaved edit")],
            &[],
            1,
        );
        tx.send(name(RenameMode::From, "src/a.rs")).unwrap();
        tx.send(name(RenameMode::To, "src/b.rs")).unwrap();
        app.poll_fs_events();
        assert_eq!(
            app.project_tree.user_src_files,
            vec![file("src/keep.rs", "k"), file("src/b.rs", "unsaved edit")]
        );
        assert_eq!(app.selected_file, ProjectFileId::UserFile(1));
    }

    /// The halves are two channel sends, so one frame can drain the `From`
    /// and the next the `To`.
    #[test]
    fn a_rename_split_across_two_frames_is_one_rename() {
        let (mut app, tx) = app(&[("src/a.rs", "unsaved edit"), ("src/z.rs", "z")], &[], 0);
        tx.send(name(RenameMode::From, "src/a.rs")).unwrap();
        app.poll_fs_events();
        assert_eq!(app.project_tree.user_src_files[0].0, "src/a.rs");
        tx.send(name(RenameMode::To, "src/b.rs")).unwrap();
        app.poll_fs_events();
        assert_eq!(
            app.project_tree.user_src_files,
            vec![file("src/b.rs", "unsaved edit"), file("src/z.rs", "z")]
        );
        assert_eq!(app.selected_file, ProjectFileId::UserFile(0));
    }

    /// A file deleted ABOVE the open one shifts its index; the editor must stay
    /// on its own file rather than slide onto the next.
    #[test]
    fn a_removal_above_the_open_file_keeps_it_open() {
        let (mut app, tx) = app(
            &[("src/a.rs", "a"), ("src/b.rs", "b"), ("src/c.rs", "c")],
            &[],
            1,
        );
        tx.send(Ok(
            Event::new(EventKind::Remove(RemoveKind::Any)).add_path(ws("src/a.rs"))
        ))
        .unwrap();
        // A removal can be the destination of a rename that follows, so the
        // last one of a drain waits for the next event or the pairing window.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        app.poll_fs_events();
        while app.project_tree.user_src_files.len() == 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            app.poll_fs_events();
        }
        assert_eq!(
            app.project_tree.user_src_files,
            vec![file("src/b.rs", "b"), file("src/c.rs", "c")]
        );
        assert_eq!(app.selected_file, ProjectFileId::UserFile(0));
    }

    /// The real Windows backend: a file, a case-only and a folder rename in a
    /// watched temp dir. The recorded events are re-rooted onto the IDE's
    /// workspace path for `poll_fs_events`, which never touches that disk for
    /// a rename of a file the tree already has.
    #[cfg(windows)]
    #[test]
    fn real_windows_renames_move_the_tree() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("fold/inner")).unwrap();
        for f in ["a.rs", "c.rs", "fold/x.rs", "fold/inner/y.rs"] {
            std::fs::write(src.join(f), "on disk").unwrap();
        }
        let (wtx, wrx) = std::sync::mpsc::channel();
        let mut w = notify::recommended_watcher(move |ev| {
            let _ = wtx.send(ev);
        })
        .unwrap();
        notify::Watcher::watch(&mut w, &src, notify::RecursiveMode::Recursive).unwrap();

        std::fs::rename(src.join("a.rs"), src.join("b.rs")).unwrap();
        std::fs::rename(src.join("c.rs"), src.join("C.rs")).unwrap();
        std::fs::rename(src.join("fold"), src.join("fold2")).unwrap();

        let mut events = vec![
            wrx.recv_timeout(Duration::from_secs(5))
                .expect("the watch delivers"),
        ];
        while let Ok(ev) = wrx.recv_timeout(Duration::from_millis(300)) {
            events.push(ev);
        }
        drop(w);

        let (mut app, tx) = app(
            &[
                ("src/a.rs", "a edited"),
                ("src/c.rs", "c edited"),
                ("src/fold/x.rs", "x"),
                ("src/fold/inner/y.rs", "y"),
            ],
            &["src/fold", "src/fold/inner"],
            1,
        );
        let root = crate::workspace::dir();
        for ev in events {
            let ev = ev.unwrap();
            let paths = ev
                .paths
                .iter()
                .map(|p| root.join(p.strip_prefix(dir.path()).unwrap()))
                .collect();
            tx.send(Ok(Event { paths, ..ev })).unwrap();
        }
        app.poll_fs_events();

        assert_eq!(
            app.project_tree.user_src_files,
            vec![
                file("src/b.rs", "a edited"),
                file("src/C.rs", "c edited"),
                file("src/fold2/x.rs", "x"),
                file("src/fold2/inner/y.rs", "y"),
            ]
        );
        assert_eq!(
            app.project_tree.user_src_folders,
            vec!["src/fold2", "src/fold2/inner"]
        );
        assert_eq!(app.selected_file, ProjectFileId::UserFile(1));
    }
}

#[cfg(test)]
mod git_snapshot_tests {
    use super::*;
    use crate::app::tabs::git_tab::is_ide_managed;
    use crate::panels::mcu_module::project_gen::ProjectFiles;

    /// Every generated file carries content, so nothing is dropped for being
    /// empty and the two conditional families are both present.
    fn xtensa_files() -> ProjectFiles {
        ProjectFiles {
            main_rs: "fn main() {}".into(),
            cargo_toml: "[package]".into(),
            cargo_config: "[build]".into(),
            gitignore: "/target".into(),
            // An ESP has neither, which is what makes them conditional.
            memory_x: String::new(),
            build_rs: String::new(),
            rust_toolchain: "[toolchain]\nchannel = \"esp\"\n".into(),
            blob_source: None,
        }
    }

    /// The defect this guards: the pin lived in memory and never in the
    /// snapshot, so the Git tab called an Xtensa project clean while its
    /// toolchain file was missing from disk.

    /// Which committed files the editor cannot open, kept to exactly two.
    ///
    /// The Git tab lists every changed file and lets the user click a line to
    /// jump there. `resolve_diag_file` answers `None` for a path with no editor
    /// view, and that click used to be swallowed in silence - which reads as a
    /// dead button rather than a deliberate refusal.
    ///
    /// Two files legitimately have no view, and both are committed on purpose:
    /// `mcu.config` on every chip, and `rust-toolchain.toml` on the three Xtensa
    /// ESP32s. A THIRD appearing here means a generated file lost its editor
    /// entry, which is a real regression - the user would see it change in git
    /// and have no way to look at it.
    #[test]
    fn only_the_two_ide_owned_files_have_no_editor_view() {
        use crate::panels::mcu_module::builtins::builtin_definitions;
        use crate::panels::mcu_module::mcu_config::FILE_NAME as MCU_CONFIG;

        for d in builtin_definitions() {
            // Only the paths matter here, so a ProjectFiles with the right
            // EMPTINESS is enough - the snapshot drops empty entries, which is
            // exactly the rule under test.
            let pin = crate::panels::mcu_module::project_gen::rust_toolchain_for(&d.project.target);
            let files =
                generated_files_snapshot(crate::panels::mcu_module::project_gen::ProjectFiles {
                    main_rs: "x".into(),
                    cargo_toml: "x".into(),
                    cargo_config: "x".into(),
                    gitignore: "x".into(),
                    memory_x: String::new(),
                    build_rs: String::new(),
                    rust_toolchain: pin.clone(),
                    blob_source: None,
                });
            let mut blind: Vec<String> = files
                .into_iter()
                .map(|(p, _)| p)
                .chain(std::iter::once(MCU_CONFIG.to_owned()))
                .filter(|p| crate::app::resolve_diag_file(p, &[]).is_none())
                .collect();
            blind.sort();
            blind.dedup();

            let mut want = vec![MCU_CONFIG.to_owned()];
            if !pin.is_empty() {
                want.push("rust-toolchain.toml".to_owned());
            }
            want.sort();
            assert_eq!(
                blind, want,
                "{}: the set of unopenable committed files changed",
                d.id
            );
        }
    }

    #[test]
    fn the_xtensa_toolchain_pin_is_tracked() {
        let snap = generated_files_snapshot(xtensa_files());
        let paths: Vec<&str> = snap.iter().map(|(p, _)| p.as_str()).collect();
        assert!(
            paths.contains(&"rust-toolchain.toml"),
            "the unsaved-changes warning cannot see it: {paths:?}"
        );
    }

    /// A RISC-V ESP or an STM32 must not gain a phantom entry: an empty file is
    /// one `write_project` does not write, and tracking it would report the
    /// project permanently unsaved.
    #[test]
    fn a_project_without_one_does_not_track_it() {
        let mut files = xtensa_files();
        files.rust_toolchain = String::new();
        let paths: Vec<String> = generated_files_snapshot(files)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert!(
            !paths.iter().any(|p| p == "rust-toolchain.toml"),
            "{paths:?}"
        );
        // The always-present four survive.
        for p in [
            "src/main.rs",
            "Cargo.toml",
            ".cargo/config.toml",
            ".gitignore",
        ] {
            assert!(paths.iter().any(|q| q == p), "lost {p}: {paths:?}");
        }
    }

    /// The invariant that ties the two lists together. They were maintained by
    /// hand, in different files, and drifted — `rust-toolchain.toml` was in
    /// neither. Anything the IDE regenerates on Save must also be refused for a
    /// hunk-revert, or the user reverts a change that silently comes back.
    #[test]
    fn everything_the_ide_rewrites_is_also_refused_for_hunk_revert() {
        let mut files = xtensa_files();
        // Fill the conditional ones too, so every path this can emit is checked
        // in one pass rather than one family at a time.
        files.memory_x = "MEMORY {}".into();
        files.build_rs = "fn main() {}".into();
        for (path, _) in generated_files_snapshot(files) {
            assert!(
                is_ide_managed(&path),
                "`{path}` is rewritten by Save but hunk-revert would accept it"
            );
        }
    }
}

#[cfg(test)]
mod seed_lock_tests {
    use super::seed_workspace_lock;

    const MANIFEST: &str = "[package]\nname = \"p\"\n\n[dependencies]\nheapless = \"0.8\"\n";

    /// A project and the build workspace, both with the same manifest - the
    /// state after the project was written into the workspace.
    fn dirs() -> (tempfile::TempDir, tempfile::TempDir) {
        let (project, workspace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(project.path().join("Cargo.toml"), MANIFEST).unwrap();
        std::fs::write(workspace.path().join("Cargo.toml"), MANIFEST).unwrap();
        (project, workspace)
    }

    /// The point: the opened project's own lock, not a fresh resolve.
    #[test]
    fn the_projects_lock_replaces_the_previous_one() {
        let (project, workspace) = dirs();
        std::fs::write(project.path().join("Cargo.lock"), "# this project").unwrap();
        std::fs::write(workspace.path().join("Cargo.lock"), "# the last project").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("Cargo.lock")).unwrap(),
            "# this project"
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("Cargo.lock")).unwrap(),
            "# this project",
            "the user's own lock is only read"
        );
    }

    /// No lock of its own: the previous project's must not stand in for it.
    #[test]
    fn a_project_without_a_lock_leaves_none() {
        let (project, workspace) = dirs();
        std::fs::write(workspace.path().join("Cargo.lock"), "# the last project").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        assert!(!workspace.path().join("Cargo.lock").exists());
    }

    /// The regression the review found: a `cargo update` in the Terminal tab
    /// (or a dependency added and built) moves only the WORKSPACE lock. The
    /// next reopen of the same project, whose own lock has not changed, must
    /// keep it rather than roll it back.
    #[test]
    fn a_lock_the_user_moved_on_survives_a_reopen() {
        let (project, workspace) = dirs();
        std::fs::write(project.path().join("Cargo.lock"), "# as committed").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        let dest = workspace.path().join("Cargo.lock");
        std::fs::write(&dest, "# after cargo update").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "# after cargo update"
        );
    }

    /// ...but a project lock that changed since (a branch switch) seeds again,
    /// and so does another project in the slot.
    #[test]
    fn a_changed_project_lock_or_another_project_seeds_again() {
        let (project, workspace) = dirs();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("Cargo.toml"), MANIFEST).unwrap();
        let dest = workspace.path().join("Cargo.lock");
        std::fs::write(project.path().join("Cargo.lock"), "# branch a").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        std::fs::write(&dest, "# built on a").unwrap();

        std::fs::write(project.path().join("Cargo.lock"), "# branch b").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# branch b");

        std::fs::write(other.path().join("Cargo.lock"), "# other project").unwrap();
        seed_workspace_lock(other.path(), workspace.path());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# other project");
    }

    /// Round 2 of the review: the open-time health check's `cargo metadata`
    /// rewrites the PROJECT lock itself. That is the IDE's own write, so the
    /// next reopen must still keep the lock the user moved on.
    #[test]
    fn the_ides_own_project_lock_write_keeps_the_workspace_lock() {
        let (project, workspace) = dirs();
        let lock = project.path().join("Cargo.lock");
        let dest = workspace.path().join("Cargo.lock");
        std::fs::write(&lock, "# L0").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        std::fs::write(&dest, "# after cargo update").unwrap();

        std::fs::write(&lock, "# L1, resolved by the health check").unwrap();
        super::restamp_lock_seed(
            project.path(),
            workspace.path(),
            Some(b"# L0"),
            Some(b"# L1, resolved by the health check"),
        );
        seed_workspace_lock(project.path(), workspace.path());
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "# after cargo update"
        );
    }

    /// A project with no lock: its workspace lock grows from nothing, survives
    /// a reopen, and survives the health check creating the project's lock.
    #[test]
    fn a_project_without_a_lock_keeps_what_it_resolved() {
        let (project, workspace) = dirs();
        let dest = workspace.path().join("Cargo.lock");
        seed_workspace_lock(project.path(), workspace.path());
        std::fs::write(&dest, "# resolved by the first build").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "# resolved by the first build"
        );

        std::fs::write(
            project.path().join("Cargo.lock"),
            "# made by the health check",
        )
        .unwrap();
        super::restamp_lock_seed(
            project.path(),
            workspace.path(),
            None,
            Some(b"# made by the health check"),
        );
        seed_workspace_lock(project.path(), workspace.path());
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "# resolved by the first build"
        );
    }

    /// A marker for another project, or for another lock, is left alone.
    #[test]
    fn a_restamp_moves_only_its_own_marker() {
        let (project, workspace) = dirs();
        std::fs::write(project.path().join("Cargo.lock"), "# L0").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        let marker = workspace.path().join(super::LOCK_SEED_MARKER);
        let before = std::fs::read_to_string(&marker).unwrap();
        super::restamp_lock_seed(
            project.path(),
            workspace.path(),
            Some(b"# not L0"),
            Some(b"# L1"),
        );
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), before);
    }

    /// Round 2 again: dependencies built into the workspace but then
    /// discarded (Discard all, or a reopen that dropped unsaved edits) leave a
    /// workspace manifest unlike the project's - the committed lock returns.
    #[test]
    fn a_discarded_dependency_change_seeds_again() {
        let (project, workspace) = dirs();
        let dest = workspace.path().join("Cargo.lock");
        std::fs::write(project.path().join("Cargo.lock"), "# L0").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        std::fs::write(
            workspace.path().join("Cargo.toml"),
            format!("{MANIFEST}embassy-time = \"0.5\"\n"),
        )
        .unwrap();
        std::fs::write(&dest, "# with embassy-time").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "# L0");
    }

    /// A New Project clears both, so a later reopen seeds from scratch.
    #[test]
    fn clearing_forgets_the_seed() {
        let (project, workspace) = dirs();
        std::fs::write(project.path().join("Cargo.lock"), "# p").unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        super::clear_workspace_lock(workspace.path());
        assert!(!workspace.path().join("Cargo.lock").exists());
        assert!(!workspace.path().join(super::LOCK_SEED_MARKER).exists());
        seed_workspace_lock(project.path(), workspace.path());
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("Cargo.lock")).unwrap(),
            "# p"
        );
    }

    /// A reopen writes nothing, so the analyzer sees no change to refetch on.
    #[test]
    fn an_identical_lock_is_not_rewritten() {
        let (project, workspace) = dirs();
        std::fs::write(project.path().join("Cargo.lock"), "# same").unwrap();
        let dest = workspace.path().join("Cargo.lock");
        std::fs::write(&dest, "# same").unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&dest)
            .unwrap()
            .set_modified(old)
            .unwrap();
        seed_workspace_lock(project.path(), workspace.path());
        assert_eq!(std::fs::metadata(&dest).unwrap().modified().unwrap(), old);
    }
}

/// Found by review: opening a project applied only what its
/// `project_structure.config` held, and the Structure and Flow views kept the
/// rest from the project open before.
#[cfg(test)]
mod view_state_tests {
    use super::super::AppIde;
    use crate::panels::mcu_module::structure_config::{self, FlowPersist};
    use eframe::egui;

    fn app() -> AppIde {
        let ctx = egui::Context::default();
        let mut app = AppIde::new(&eframe::CreationContext::_new_kittest(ctx), None, None);
        app._fs_watcher = None;
        app.fs_watched = None;
        app
    }

    /// A project folder whose `project_structure.config` holds this view.
    fn project(
        view: structure_config::StructureViewPersist,
        flow: FlowPersist,
    ) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let text = structure_config::serialize(
            &Default::default(),
            &view,
            &Default::default(),
            &structure_config::CLOCK_VIEW_DEFAULT,
            &flow,
        );
        std::fs::write(dir.path().join(structure_config::FILE_NAME), text).unwrap();
        dir
    }

    fn default_view() -> structure_config::StructureViewPersist {
        let d = crate::panels::structure_map::gui::StructureView::default();
        (
            d.show_calls,
            d.call_depth,
            d.path_style.to_u8(),
            d.show_externals,
        )
    }

    /// The last project's open chart is a KEY both files can have (`main`),
    /// and the Flow tab keeps an open key over the saved one - so B opened on
    /// A's `main`, not its own `blink`, and its next Save lost `blink`.
    #[test]
    fn opening_a_project_forgets_the_last_ones_flow_position() {
        let mut app = app();
        app.flow_view.selected = "main".to_owned();
        app.flow_view.all = true;
        let b = project(
            default_view(),
            FlowPersist {
                selected: ("src/main.rs".to_owned(), "blink".to_owned()),
                mode: 0,
            },
        );
        app.restore_view_state(b.path());
        assert_eq!(
            app.flow_selected,
            ("src/main.rs".to_owned(), "blink".to_owned())
        );
        assert!(!app.flow_view.all, "B saved no whole-file mode");
        let Ok(model) = crate::panels::flow_map::parse::parse_file(
            "fn blink() {}\n\nfn main() {\n    blink();\n}\n",
        ) else {
            panic!("the file parses");
        };
        assert_eq!(
            crate::panels::flow_map::choose_selection(
                &model,
                &app.flow_view.selected,
                Some("blink")
            ),
            "blink",
            "the Flow tab opens B on its own saved chart"
        );
    }

    /// A default Structure view writes no section, so every project that uses
    /// it inherited the last one's options - and wrote them into its own file
    /// on the next Save - plus its open nodes (indices into ANOTHER graph),
    /// its search, zoom and pan.
    #[test]
    fn opening_a_project_forgets_the_last_ones_structure_view() {
        let mut app = app();
        app.structure_view.show_calls = false;
        app.structure_view.call_depth = Some(3);
        app.structure_view.show_externals = true;
        app.structure_view.expanded.insert(0);
        app.structure_view.search = "uart".to_owned();
        app.structure_view.zoom = 2.0;
        let b = project(default_view(), FlowPersist::default());
        app.restore_view_state(b.path());
        let v = &app.structure_view;
        let (calls, depth, _, externals) = default_view();
        assert_eq!(
            (v.show_calls, v.call_depth, v.show_externals),
            (calls, depth, externals)
        );
        assert!(v.expanded.is_empty(), "{:?}", v.expanded);
        assert!(v.search.is_empty(), "{:?}", v.search);
        assert_eq!(v.zoom, 1.0);
    }

    /// What a project DOES save still wins over the defaults.
    #[test]
    fn a_saved_view_is_restored() {
        let mut app = app();
        let b = project(
            (false, Some(2), 0, true),
            FlowPersist {
                selected: ("src/main.rs".to_owned(), "blink".to_owned()),
                mode: 1,
            },
        );
        app.restore_view_state(b.path());
        let v = &app.structure_view;
        assert_eq!(
            (v.show_calls, v.call_depth, v.show_externals),
            (false, Some(2), true)
        );
        assert_eq!(app.flow_selected.1, "blink");
        assert!(app.flow_view.all);
    }
}
