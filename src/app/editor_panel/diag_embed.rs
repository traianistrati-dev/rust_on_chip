//! Bottom diagnostics panel embedded inside the code editor.
//!
//! Shows the resizable Cargo/RA/Flash/Tools panel when any of those have
//! activity.  Pure self-state; independent of the edited text.

use crate::app::AppIde;
use crate::app::BuildPanelTab;
use crate::app::diag_panel::show_diag_panel;
use crate::build::BuildState;
use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
use eframe::egui;

impl AppIde {
    /// Render the embedded bottom diagnostics panel (drag handle + content).
    /// Returns the panel's top Y (= the bottom edge of the editor region above
    /// it), or `None` when the panel isn't shown — used to clip the inline
    /// diagnostic overlay so it can't bleed into this panel.
    /// `source_rewritten` is set to `true` when a Clippy "Fix" / "Apply all"
    /// applied edits to a source buffer (`generated_code` or a user file), so the
    /// caller can refresh its editor working-copy and avoid reverting them.
    pub(super) fn show_editor_diag_panel(
        &mut self,
        ui: &mut egui::Ui,
        source_rewritten: &mut bool,
    ) -> Option<f32> {
        // First-open baud seeding for the Serial tab (was the toolbar Serial
        // button's job before it was removed): while the tab is selected and
        // idle, seed the baud from a USART virtual module — once per project.
        //
        // The LOWEST instance, not the first module in `mcu.modules`, which is
        // wiring order. It matters on Espressif parts: the port the Serial tab
        // is about to open carries the CONSOLE, which is UART0 there (the same
        // peripheral `esp-println` writes through), so a project whose first-
        // drawn module happened to be a UART1 sensor handed the tab that
        // sensor's rate. Instances are unique per module, so there is no tie.
        if self.build_tab == BuildPanelTab::Serial
            && !self.serial.baud_seeded
            && !self.serial.is_connected()
        {
            self.serial.baud_seeded = true;
            if let Some(baud) = self.mcu.as_ref().and_then(|mcu| {
                mcu.modules
                    .iter()
                    .filter_map(|m| match &m.config {
                        crate::panels::mcu_module::modules::ModuleConfig::Usart(c) => {
                            Some((c.instance, c.baud_rate))
                        }
                        _ => None,
                    })
                    .min_by_key(|(instance, _)| *instance)
                    .map(|(_, baud)| baud)
            }) {
                self.serial.baud = baud;
            }
        }

        // ── Tab just opened? ──────────────────────────────────────────────
        // The Git tab shows the working tree's state, which goes stale the
        // moment you edit anything — so re-read it every time the tab is
        // entered, not just on its first open. Detected as a TRANSITION so
        // re-clicking an already-open tab doesn't spawn a git process per
        // click; the tab's own Refresh button covers that. The switch happens
        // inside the panel closure below, so this sees it one frame later —
        // invisible in practice, and it keeps the check out of the render path.
        let entered_git =
            self.build_tab != self.last_build_tab && self.build_tab == BuildPanelTab::Git;
        // Same edge for the Flash tab: its device lists are only useful if they
        // describe what is plugged in RIGHT NOW.
        let entered_flash =
            self.build_tab != self.last_build_tab && self.build_tab == BuildPanelTab::Dfu;
        self.last_build_tab = self.build_tab;

        // The panel used to hide itself whenever no tab had activity. It no
        // longer does: the tab bar stays put until the user collapses it with
        // the caret button, and collapsing hides only the tab CONTENT.
        const HANDLE_H: f32 = 6.0;
        const MIN_H: f32 = 56.0;
        /// How far past `MIN_H` the handle must be dragged DOWN before the panel
        /// collapses itself. Without the slack, every drag that simply reaches
        /// the bottom limit would snap it shut.
        const COLLAPSE_SLACK: f32 = 14.0;
        /// The panel's top edge at rest — one colour for both states, so
        /// collapsing only removes the grip dots, never the boundary itself.
        const EDGE_IDLE: egui::Color32 = egui::Color32::from_gray(65);

        let collapsed = self.diag_collapsed;
        // The editor+panel region: `ui` hasn't given the panel its slice yet.
        let avail_h = ui.available_height();
        // Keep height in valid range for current window size.
        let max_h = (avail_h - 60.0).max(MIN_H);
        self.diag_panel_height = self.diag_panel_height.clamp(MIN_H, max_h);

        // Collapsed height = the tab-button row, the panel frame's vertical
        // inner margin and the handle. Both parts matter: `Panel` never shrinks
        // to fit its content (it takes the size it is given) and `exact_size`
        // INCLUDES the frame margins. Style-derived rather than a magic number
        // so it follows spacing — and shared with the Virtual-modules panel, so
        // the two collapsed bars sit on the same line.
        let collapsed_h = crate::app::collapsed_panel_height(ui, HANDLE_H);
        // The handle is part of BOTH states — collapsed, it is the only way to
        // pull the panel back open by dragging — so its strip is included here
        // too, or allocating it would clip the tab row `collapsed_h` measures.
        let panel_h = if collapsed {
            collapsed_h
        } else {
            self.diag_panel_height + HANDLE_H
        };

        // Panel::bottom takes space from the bottom of the remaining area
        // before the editor is laid out. exact_size gives us full control —
        // no egui-internal default that would reset on show/hide.
        // Diagnostic-row click navigation: (rel_path, 1-based line, band colour).
        let mut nav: Option<(String, usize, egui::Color32)> = None;
        // Set by the Clippy tab's "Run clippy" button.
        let mut clippy_run = false;
        // Set by the Clippy tab's per-row "Fix" / "Apply all" / "Rename" buttons.
        let mut clippy_apply_one: Option<usize> = None;
        let mut clippy_apply_all = false;
        let mut clippy_apply_rename: Option<usize> = None;
        // Byte ranges of main.rs's GENERATED block — the Clippy tab disables "Fix"
        // for suggestions whose edit lands inside (owned by the MCU Configurator).
        let clippy_gen_ranges = crate::app::generated_byte_ranges(&self.generated_code);
        // Set by the Git tab's buttons; the caller spawns the worker below.
        let mut git_op: Option<crate::git::GitOp> = None;
        // Set when the user clicks an added row in the Git diff view.
        let mut git_open: Option<(String, usize)> = None;
        // Set when the user clicks a hunk's revert button in the Git diff view.
        let mut git_revert_hunk: Option<(String, usize)> = None;
        // Git History (read-only): commit selected / commit file clicked.
        let mut git_commit_load: Option<String> = None;
        let mut git_commit_file_load: Option<(String, String)> = None;
        // History "Restore this file" → confirmed, then applied.
        let mut git_restore_from_commit: Option<(String, String)> = None;
        let mut git_restore_all_from_commit: Option<String> = None;
        // Set when the user clicks a file's discard button in the Git tab.
        let mut git_discard: Option<(String, bool)> = None;
        // Set when the user clicks "Discard all" in the Git tab.
        let mut git_discard_all = false;
        // Branch the header picker asked to switch to.
        let mut git_switch_branch: Option<String> = None;
        let mut git_delete_branch: Option<String> = None;
        // The Git tab's repository picker: each library folder can have its own
        // git repo rooted there — workspace members AND DETACHED libraries. A
        // detached clone is decoupled from the PROJECT's workspace but still its
        // own repo, so it must stay git-manageable (commit/push/pull on its own
        // remote) whether attached or not.
        let git_libraries = {
            let members =
                crate::panels::mcu_module::project_gen::workspace_members(&self.cargo_toml);
            let detached = crate::project_tree::extract_crate::detached_libs(
                &self.project_tree.user_src_files,
                &members,
            );
            let mut libs = members;
            libs.extend(detached);
            libs
        };
        // Flash-tab Programmer-row buttons (moved off the top toolbar).
        let mut flash_scan = false;
        let mut flash_go = false;
        let mut flash_stop = false;
        let mut probe_flash_go = false;
        let mut probe_flash_stop = false;
        // Snapshot of the tools proven missing by the startup self-check — the
        // tabs grey out the buttons that shell out to them. Cheap (a few &str)
        // and taken once per frame so no tab needs the mutex.
        let missing_tools: Vec<&'static str> = self.tools_state.lock().unwrap().unavailable();
        let can_flash = self.selected_build_cfg().is_some();
        // A bitstream the FPGA would reject turns the Flash buttons red. Looked
        // at only while the Flash tab shows, and only on a board that loads one.
        // So does a partition table that is wrong (ESP only, string checks).
        let flash_tab_open = self.build_tab == BuildPanelTab::Dfu && !self.diag_collapsed;
        let flash_block = (self.build_tab == BuildPanelTab::Dfu
            && !self.diag_collapsed
            && self
                .mcu
                .as_ref()
                .is_some_and(crate::panels::mcu_module::codegen::rp::fpga_loader))
        .then(|| {
            let verdict = self.fpga_watch.get(self.project_dir.as_deref());
            verdict
                .result
                .as_ref()
                .err()
                .map(crate::panels::mcu_module::fpga_bitstream::blocking_reason)
        })
        .flatten()
        .or_else(|| {
            flash_tab_open
                .then(|| self.partition_table_block())
                .flatten()
        });
        // Cargo-tab Build button (moved off the top toolbar on 2026-07-10).
        let mut build_go = false;
        // Cargo-tab Size button (Flash/RAM usage measurement).
        let mut size_go = false;
        // Flash-tab Size button — same measurement, stays on the Flash tab.
        let mut size_flash_go = false;
        // Set when ANY tab button is clicked (including the already-selected
        // one, which is why this can't be a `tab` change comparison) — while
        // collapsed, that reopens the panel.
        let mut tab_clicked = false;
        // RTT-tab Run/Attach buttons.
        let mut rtt_go: Option<crate::rtt::RttMode> = None;
        // Monitor-tab Start button + its "open after flash" checkbox.
        let mut esp_monitor_go = false;
        let mut esp_monitor_auto_set: Option<bool> = None;
        // Debug-tab Start button.
        let mut debug_go = false;
        // Debug-tab "Reset target" button.
        let mut reset_go = false;
        // Debug-tab breakpoint-list row click: `(rel path, 1-based line)`.
        let mut bp_jump: Option<(String, u32)> = None;
        // Same list's ✕ (one row) and "Remove all" buttons.
        let mut bp_remove: Option<(String, u32)> = None;
        let mut bp_clear = false;
        // Debug tab's "Debug-friendly build" checkbox.
        let debug_build = self.mcu.as_ref().is_some_and(|m| m.debug_build);
        let mut debug_build_set: Option<bool> = None;
        // Shared RTT/Debug probe-selector Scan button.
        let mut probe_scan = false;
        // Profile-tab "Analyze" (cargo bloat) + "Sample" (flamegraph) buttons.
        let mut profile_run = false;
        let mut profile_sample = false;
        let rtt_chip = self
            .selected_build_cfg()
            .map(|(p, _)| p.probe_chip)
            .unwrap_or_default();
        let project_dir = self.project_dir.clone();
        // Set when the HANDLE opened the panel, so the button's "reopen at 30 %"
        // below doesn't hijack a drag that is already sizing it by hand.
        let mut drag_opened = false;
        // Through `show_exact`: dragging the collapsed bar open flips
        // `diag_collapsed` inside this closure, so that frame lays the tab
        // content out in the bar's height - and a tab can want more than the
        // smallest panel anyway. egui would move the panel's top down by the
        // excess, and the editor laid out above it painted over the tab row.
        let panel = crate::app::helpers::panel::show_exact(
            egui::Panel::bottom("diag_panel"),
            panel_h,
            ui,
            |ui| {
                // ── Drag handle (top edge of panel) ───────
                // Drawn in BOTH states. Collapsed it is the panel's top border
                // (without it the tab bar bleeds into the editor and the tabs
                // lose their top edge) AND the grip that pulls the panel back
                // open — dragging it up expands and resizes in one gesture,
                // instead of forcing a trip to the caret button first.
                {
                    let (handle_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), HANDLE_H),
                        egui::Sense::hover(),
                    );
                    let drag_resp = ui.interact(
                        handle_rect,
                        egui::Id::new("diag_panel_resize"),
                        egui::Sense::drag(),
                    );

                    let mid_y = handle_rect.center().y;
                    let line_color = if drag_resp.hovered() || drag_resp.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                        egui::Color32::from_rgb(100, 140, 200)
                    } else {
                        EDGE_IDLE
                    };

                    // Line + three grip dots
                    ui.painter().hline(
                        handle_rect.x_range(),
                        mid_y,
                        egui::Stroke::new(1.5_f32, line_color),
                    );
                    for dx in [-6.0_f32, 0.0, 6.0] {
                        ui.painter().circle_filled(
                            egui::pos2(handle_rect.center().x + dx, mid_y),
                            1.5,
                            line_color,
                        );
                    }

                    if drag_resp.dragged() {
                        // Dragging up → negative delta.y → panel grows.
                        let dy = drag_resp.drag_delta().y;
                        if collapsed {
                            // Only upward opens it: a collapsed bar has nowhere
                            // to shrink to. The first pixel pops it to MIN_H,
                            // after which the drag tracks the pointer — and the
                            // caret button flips to "collapse" on its own, since
                            // it renders straight from this flag.
                            if dy < 0.0 {
                                self.diag_collapsed = false;
                                drag_opened = true;
                                self.diag_panel_height = (MIN_H - dy).clamp(MIN_H, max_h);
                            }
                        } else {
                            let want = self.diag_panel_height - dy;
                            // Pulled down past the smallest useful panel: finish
                            // the gesture by collapsing, instead of jamming
                            // against MIN_H and making the user go find the
                            // caret button. The slack keeps a drag that merely
                            // BOTTOMS OUT from collapsing — you have to keep
                            // pulling to mean it.
                            if want < MIN_H - COLLAPSE_SLACK {
                                self.diag_collapsed = true;
                            }
                            // Left at MIN_H either way, so re-opening (button,
                            // tab click or an upward drag) lands on a usable
                            // size rather than a sliver.
                            self.diag_panel_height = want.clamp(MIN_H, max_h);
                        }
                    }
                }

                // ── Content ────────────────────────────────
                let toolchain = self.selected_toolchain().unwrap_or(ToolchainKind::SdccC);
                // The DFU config row belongs to the STM32 ROM bootloader, and
                // its flash address means nothing on any other part.
                let dfu_addr = self
                    .mcu
                    .as_ref()
                    .is_some_and(|m| crate::dfu::has_usb_dfu_bootloader(&m.family))
                    .then_some(&mut self.dfu_flash_addr);
                show_diag_panel(
                    ui,
                    &self.egui_ctx,
                    &self.build_state,
                    &self.lsp_state,
                    &self.dfu_state,
                    &self.dfu_log,
                    &self.dfu_programmers,
                    &mut self.dfu_sel_programmer,
                    dfu_addr,
                    &self.openocd_state,
                    &mut self.openocd_target_cfg,
                    &self.espflash_state,
                    &mut self.espflash_port,
                    &self.espflash_used_port,
                    &self.tools_state,
                    &mut self.serial,
                    &mut self.terminal,
                    &self.activity,
                    &self.clippy_state,
                    &mut self.clippy_sel,
                    &mut clippy_run,
                    &mut clippy_apply_one,
                    &mut clippy_apply_all,
                    &mut clippy_apply_rename,
                    &clippy_gen_ranges,
                    &toolchain,
                    &mut self.build_tab,
                    &mut self.diag_collapsed,
                    &mut tab_clicked,
                    &mut self.selected_diagnostic,
                    &mut self.lsp_selected_diagnostic,
                    &mut nav,
                    &mut self.git,
                    project_dir.as_deref(),
                    &mut git_op,
                    &mut git_open,
                    &mut git_revert_hunk,
                    &mut git_commit_load,
                    &mut git_commit_file_load,
                    &mut git_restore_from_commit,
                    &mut git_restore_all_from_commit,
                    &mut git_discard,
                    &mut git_discard_all,
                    &mut git_switch_branch,
                    &mut git_delete_branch,
                    &git_libraries,
                    &mut flash_scan,
                    &mut flash_go,
                    &mut flash_stop,
                    &self.esp_flash_child,
                    can_flash,
                    &mut build_go,
                    &self.size_state,
                    &mut size_go,
                    &mut size_flash_go,
                    &mut self.rtt,
                    &mut rtt_go,
                    &rtt_chip,
                    &mut self.esp_monitor,
                    &mut esp_monitor_go,
                    self.esp_monitor_auto,
                    &mut esp_monitor_auto_set,
                    &mut self.debugger,
                    &mut debug_go,
                    &mut reset_go,
                    &self.breakpoints,
                    &mut bp_jump,
                    &mut bp_remove,
                    &mut bp_clear,
                    debug_build,
                    &mut debug_build_set,
                    &self.probe_list,
                    &mut self.selected_probe,
                    &mut probe_scan,
                    self.probe_scan_err.as_deref(),
                    &mut self.profile_mode,
                    &self.profile_state,
                    &mut self.profile_by_crate,
                    &mut profile_run,
                    &self.flame_state,
                    &mut profile_sample,
                    &self.probe_flash_state,
                    &mut probe_flash_go,
                    &mut probe_flash_stop,
                    &missing_tools,
                    flash_block.as_deref(),
                );
            },
        );
        // Clicking a tab on the collapsed bar reopens the panel at 20% of the
        // editor region, so the tab's content is actually visible. Takes effect
        // next frame — this frame was already laid out collapsed.
        if tab_clicked && collapsed {
            self.diag_collapsed = false;
            self.diag_panel_height = (avail_h * 0.2).clamp(MIN_H, max_h);
        }
        // Expanded with the caret button: open at 30 % of the WINDOW, not at
        // whatever sliver the panel was last left at. Measured on the window
        // rather than the editor region, so the panel comes back the same size
        // whichever side panels happen to be open. A drag that opened it is
        // excluded — there the pointer is already choosing the height — and so
        // is a tab click, which keeps its smaller "just show me this tab" size.
        if collapsed && !self.diag_collapsed && !drag_opened && !tab_clicked {
            let window_h = ui.ctx().content_rect().height();
            self.diag_panel_height = (window_h * 0.30).clamp(MIN_H, max_h);
        }
        // "Restore this file" → open the confirm; the write happens once the
        // user agrees (queued like the discard confirm, applied next frame so
        // the editor's working copy refreshes with it).
        if let Some((sha, path)) = git_restore_from_commit {
            self.git_restore_confirm = Some((sha, path));
        }
        if let Some(sha) = git_restore_all_from_commit {
            self.git_restore_all_confirm = Some(sha);
        }
        // A branch switch reloads the project from disk (via `reload_project`),
        // which discards unsaved editor edits — confirm first if any exist,
        // otherwise switch straight away.
        if let Some(b) = git_switch_branch {
            let has_unsaved = !self.git.state.lock().unwrap().unsaved.is_empty();
            if has_unsaved {
                self.git_switch_confirm = Some(b);
            } else {
                self.git.switch_target = Some(b);
                self.run_git_op(crate::git::GitOp::SwitchBranch);
            }
        }
        // Deleting a branch is destructive (`-D` drops unmerged commits) — always
        // confirm.
        if let Some(b) = git_delete_branch {
            self.git_delete_branch_confirm = Some(b);
        }
        // A whole-tree restore rewrote the files on disk; the in-memory buffers
        // are now stale and WOULD overwrite them at the next save, so reload.
        let reload = {
            let mut st = self.git.state.lock().unwrap();
            std::mem::take(&mut st.reload_project)
        };
        if reload {
            if let Some(dir) = project_dir.clone() {
                self.load_project_from_dir(&dir);
                *source_rewritten = true;
            }
        }
        // A "Clone from git" library worker finished: wire it into the workspace
        // (success) or surface the error in the dialog.
        let clone_result = self.git.state.lock().unwrap().clone_result.take();
        if let Some(res) = clone_result {
            match res {
                Ok(lib) => {
                    self.finish_clone_library(lib.dir, lib.is_submodule);
                    self.clone_library_dialog = None;
                }
                Err(e) => {
                    if let Some(d) = &mut self.clone_library_dialog {
                        d.error = Some(e);
                    }
                }
            }
        }
        // History view: load a commit's file list / one file's diff. Both are
        // read-only git reads, guarded against a concurrent op inside.
        if let (Some(sha), Some(dir)) = (git_commit_load, self.git_dir()) {
            crate::git::run_commit_files(
                sha,
                dir,
                std::sync::Arc::clone(&self.git.state),
                self.egui_ctx.clone(),
            );
        }
        if let (Some((sha, path)), Some(dir)) = (git_commit_file_load, self.git_dir()) {
            crate::git::run_commit_file_diff(
                sha,
                path,
                dir,
                std::sync::Arc::clone(&self.git.state),
                self.egui_ctx.clone(),
            );
        }
        // A Git tab button was clicked: spawn the worker (guards inside).
        // An automatic refresh on entering the tab loses to an explicit button
        // press — `run_git_op` is a no-op while an op is already running.
        if let Some(op) = git_op {
            self.run_git_op(op);
        } else if entered_git {
            self.run_git_op(crate::git::GitOp::Refresh);
        }
        // Cargo-tab Build button — always a fast `cargo check`.
        if build_go {
            self.start_build(false);
        }
        // Cargo-tab Size button (Flash/RAM measurement).
        if size_go {
            self.start_size_measure();
        }
        // Flash-tab Size button — must not jump to the Cargo tab.
        if size_flash_go {
            self.start_size_measure_quiet();
        }
        // RTT-tab Run/Attach buttons.
        if let Some(mode) = rtt_go {
            self.start_rtt(mode);
        }
        // Monitor-tab Start button (standalone — no flash preceding it).
        if esp_monitor_go {
            self.start_esp_monitor(false);
        }
        if let Some(v) = esp_monitor_auto_set {
            self.esp_monitor_auto = v;
        }
        // Debug-tab Start button.
        if debug_go {
            self.start_debug();
        }
        // Debug-tab "Reset target": restart the chip through the probe, no
        // reflash. The result lands in the Debug console.
        if reset_go {
            if let Some((project, _)) = self.selected_build_cfg() {
                crate::probe::start_reset(
                    project.probe_chip.clone(),
                    self.selected_probe.clone(),
                    std::sync::Arc::clone(&self.debugger.console),
                    self.egui_ctx.clone(),
                );
            }
        }
        // ── Debugger keys (the set every debugger uses) ──────────────────────
        // F5 continue · F10 over · F11 in · Shift+F11 out · Shift+F5 stop.
        // Handled on the CONTEXT, not inside the Debug tab: the halted line is
        // in the editor, which is where the user is looking (and where the
        // focus is) when they reach for these. The stepping keys only fire
        // while halted, so they can never be sent into a running target.
        {
            let halted = matches!(
                self.debugger.phase(),
                crate::debugger::DebugPhase::Stopped(_)
            );
            let busy = self.debugger.is_busy();
            let key =
                |m: egui::Modifiers, k: egui::Key| ui.ctx().input_mut(|i| i.consume_key(m, k));
            // Shift variants first, so a plain-key branch can't shadow one.
            if busy && key(egui::Modifiers::SHIFT, egui::Key::F5) {
                self.debugger.stop(ui.ctx());
            } else if halted && key(egui::Modifiers::SHIFT, egui::Key::F11) {
                self.debugger.step_out();
            } else if halted && key(egui::Modifiers::NONE, egui::Key::F5) {
                self.debugger.continue_run();
            } else if halted && key(egui::Modifiers::NONE, egui::Key::F10) {
                self.debugger.step_over();
            } else if halted && key(egui::Modifiers::NONE, egui::Key::F11) {
                self.debugger.step_in();
            }
        }
        // Shared RTT/Debug probe-selector Scan button.
        if probe_scan {
            self.scan_probes();
        }
        // Opening the Flash tab BY CLICKING it gets both device lists refreshed,
        // so Flash is ready to press. `tab_clicked` is the difference between
        // the user asking for the tab and a flash/Size run switching to it —
        // enumerating while the probe is being claimed helps nobody.
        if entered_flash && tab_clicked {
            self.autoscan_flash_devices(&missing_tools);
        }
        // Pick up a finished probe enumeration (it runs on a thread).
        self.apply_probe_scan();
        // Profile-tab "Analyze" button → cargo bloat.
        if profile_run {
            self.start_profile();
        }
        // Profile-tab "Sample" button → on-target flamegraph.
        if profile_sample {
            self.start_flame();
        }
        // "Debug-friendly build" flipped: the Mcu owns the preference, and the
        // codegen pass hashes it → `[profile.release]` is rewritten next frame,
        // exactly like the System tab's Strict-lints checkbox.
        if let Some(on) = debug_build_set {
            if let Some(mcu) = &mut self.mcu {
                mcu.debug_build = on;
            }
        }
        // Breakpoint-list removals — same bookkeeping as a gutter toggle: drop
        // the line (and the file's entry once empty), then push that file's NEW
        // set into a live session (`sync_breakpoints` no-ops without one).
        if let Some((rel, line)) = bp_remove {
            if let Some(set) = self.breakpoints.get_mut(&rel) {
                set.remove(&line);
                if set.is_empty() {
                    self.breakpoints.remove(&rel);
                }
            }
            let lines: Vec<u32> = self
                .breakpoints
                .get(&rel)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            self.debugger.sync_breakpoints(&rel, &lines);
        }
        // "Remove all": every FILE that had one must be told it now has none —
        // clearing the map alone would leave the running session's breakpoints
        // armed on the target.
        if bp_clear {
            let files: Vec<String> = self.breakpoints.keys().cloned().collect();
            self.breakpoints.clear();
            for rel in files {
                self.debugger.sync_breakpoints(&rel, &[]);
            }
        }
        // Debug-tab breakpoint-list row click: open that file at the line, with
        // the same band the halt navigation below paints (no session needed —
        // this is plain navigation).
        if let Some((rel, line)) = bp_jump {
            if let Some(id) = crate::app::resolve_diag_file(&rel, &self.project_tree.user_src_files)
            {
                self.selected_file = id;
                self.ed.pending_scroll_to_line = Some((id, line as usize));
                self.ed.highlighted_error_line = Some((
                    id,
                    line as usize,
                    egui::Color32::from_rgba_unmultiplied(200, 50, 50, 40),
                ));
            }
        }
        // Debugger halt location (breakpoint / step landed): jump the editor
        // there with a translucent GREEN band — same path as a diagnostic-row
        // click. Alpha 51 (~80% transparent) keeps the halted line's code
        // readable through the tint.
        if let Some((rel, line)) = self.debugger.take_nav() {
            if let Some(id) = crate::app::resolve_diag_file(&rel, &self.project_tree.user_src_files)
            {
                self.selected_file = id;
                self.ed.pending_scroll_to_line = Some((id, line as usize));
                self.ed.highlighted_error_line = Some((
                    id,
                    line as usize,
                    egui::Color32::from_rgba_unmultiplied(200, 50, 50, 40),
                ));
            }
        }
        // Flash-tab Programmer-row buttons.
        if flash_scan {
            self.scan_usb();
        }
        if flash_go {
            match self.selected_toolchain() {
                Some(crate::panels::mcu_module::mcu_catalog::ToolchainKind::EspRust) => {
                    self.flash_esp()
                }
                _ => self.flash_swd(),
            }
        }
        // Same button, second state - and the same toolchain decides which of
        // the two paths is the one running.
        if flash_stop {
            match self.selected_toolchain() {
                Some(crate::panels::mcu_module::mcu_catalog::ToolchainKind::EspRust) => {
                    self.stop_esp_flash()
                }
                _ => self.stop_swd_flash(),
            }
        }
        // Flash tab's probe-rs path (shared probe). `probe_scan` is consumed above.
        if probe_flash_stop {
            self.stop_probe_flash();
        }
        if probe_flash_go {
            self.flash_probe_rs();
        }
        // "Open Tools" on a failure-hint card (crate::failure_hint) — signalled
        // through egui temp data so it needn't be threaded through every tab.
        if crate::failure_hint::take_open_tools_request(ui.ctx()) {
            self.build_tab = BuildPanelTab::RequiredTools;
        }
        // An added diff row was clicked in the Git tab → open its file in the
        // editor and scroll to that line (jump straight to the changed code).
        // Config files map too; paths with no editor equivalent (e.g.
        // mcu.config) are silently ignored.
        if let Some((path, line)) = git_open {
            // `path` is relative to the ACTIVE git repo — inside a library repo
            // git reports `src/lib.rs` where the IDE keys the file
            // `mw_radar/src/lib.rs`. Re-root it (like every other git handler
            // does via `git_path_to_project`); without this a modified library
            // line never resolves and the click silently opens nothing.
            let key = self.git_path_to_project(&path);
            if let Some(id) = crate::app::resolve_diag_file(&key, &self.project_tree.user_src_files)
            {
                self.selected_file = id;
                self.ed.pending_scroll_to_line = Some((id, line));
            } else {
                // Some files the Git tab lists have no editor view at all:
                // `mcu.config` is the Configurator's own state on every chip,
                // and `rust-toolchain.toml` is derived from the target on the
                // Xtensa ESP32s. Both are committed on purpose, so they show up
                // in the diff - and the click used to be swallowed with nothing
                // said, which reads as the IDE having missed it.
                let why = if key == "rust-toolchain.toml" {
                    " - it is derived from the chip's target, so there is nothing to edit"
                } else if key == crate::panels::mcu_module::mcu_config::FILE_NAME {
                    " - it is the MCU Configurator's own state; change it through the tabs"
                } else {
                    ""
                };
                self.git.state.lock().unwrap().lines.push((
                    crate::git::GitLine::Notice,
                    format!("[skip] {path} has no editor view{why}"),
                ));
            }
        }
        // A hunk's revert button was clicked in the Git diff view (Phase B):
        // reverse just that hunk on disk + refresh its in-memory buffer. Like
        // the Clippy "Fix", set `source_rewritten` so the editor refreshes
        // `display_code` and the end-of-frame write-back keeps the change.
        if let Some((path, hunk_row)) = git_revert_hunk {
            if self.apply_hunk_revert(&path, hunk_row) {
                *source_rewritten = true;
            }
        }
        // A file's discard button was clicked (Phase A) → open the confirm
        // dialog; the actual restore/delete runs once the user confirms.
        if let Some((path, untracked)) = git_discard {
            self.git_discard_confirm = Some((path, untracked));
        }
        // "Discard all" was clicked (Phase C) → open the strong confirm dialog.
        if git_discard_all {
            self.git_discard_all_confirm = true;
        }
        // A confirmed whole-file discard, queued by the dialog on the previous
        // frame — apply it here (same `source_rewritten` refresh as the hunk
        // revert, so the open editor keeps the restored content).
        if let Some(path) = self.pending_discard_file.take() {
            if self.apply_discard_file(&path) {
                *source_rewritten = true;
            }
        }
        // Same for a confirmed restore-from-history.
        if let Some((sha, path)) = self.pending_restore.take() {
            if self.apply_restore_from_commit(&sha, &path) {
                *source_rewritten = true;
            }
        }
        // "Run clippy" was clicked: write the project to the workspace and start
        // `cargo clippy` on a worker thread (serialized with Build).
        if clippy_run {
            self.start_clippy_run();
        }

        // Single "Fix" — splice this diagnostic's machine-applicable edits.
        if let Some(idx) = clippy_apply_one {
            let edits: Vec<crate::build::SpanEdit> = {
                let cs = self.clippy_state.lock().unwrap();
                match &*cs {
                    BuildState::Done(r) => r
                        .diagnostics
                        .get(idx)
                        .map(|d| d.fixes.clone())
                        .unwrap_or_default(),
                    _ => Vec::new(),
                }
            };
            if !edits.is_empty() && self.apply_source_edits(&edits) > 0 {
                // Refresh the editor's working copy so the write-back doesn't revert
                // it, then re-run clippy so the (now-stale) offsets refresh.
                *source_rewritten = true;
                if !self.start_clippy_run() {
                    *self.clippy_state.lock().unwrap() = BuildState::Idle;
                    self.clippy_sel = None;
                }
            }
        }

        // Single "Rename" — enqueue one project-wide rename (RA `textDocument/
        // rename`, same path as Ctrl+R) and start it.
        if let Some(idx) = clippy_apply_rename {
            let rename = {
                let cs = self.clippy_state.lock().unwrap();
                match &*cs {
                    BuildState::Done(r) => r.diagnostics.get(idx).and_then(|d| d.rename.clone()),
                    _ => None,
                }
            };
            if let Some(rn) = rename {
                self.clippy_rename_queue.push_back(rn);
                self.clippy_rename_pending = true; // re-run clippy when the queue drains
                *self.clippy_state.lock().unwrap() = BuildState::Idle;
                self.clippy_sel = None;
                self.start_next_queued_rename();
            }
        }

        // "Apply all" — apply every unlocked machine-applicable splice in one shot,
        // then queue every unlocked rename to run one-by-one. A final clippy re-run
        // (clippy_rename_pending) refreshes the list once the whole batch is in.
        if clippy_apply_all {
            let (edits, renames): (Vec<crate::build::SpanEdit>, Vec<crate::build::RenameFix>) = {
                let cs = self.clippy_state.lock().unwrap();
                match &*cs {
                    BuildState::Done(r) => (
                        r.diagnostics.iter().flat_map(|d| d.fixes.clone()).collect(),
                        r.diagnostics
                            .iter()
                            .filter_map(|d| d.rename.clone())
                            .filter(|rn| {
                                // Skip renames inside main.rs's GENERATED block.
                                !(rn.file == "src/main.rs"
                                    && clippy_gen_ranges
                                        .iter()
                                        .any(|&(b, e)| rn.byte < e && rn.byte + 1 > b))
                            })
                            .collect(),
                    ),
                    _ => (Vec::new(), Vec::new()),
                }
            };
            // Splices first (locked ones are skipped inside apply_source_edits).
            let spliced = !edits.is_empty() && self.apply_source_edits(&edits) > 0;
            if spliced {
                *source_rewritten = true;
            }
            for rn in renames {
                self.clippy_rename_queue.push_back(rn);
            }
            if spliced || !self.clippy_rename_queue.is_empty() {
                self.clippy_rename_pending = true;
                *self.clippy_state.lock().unwrap() = BuildState::Idle;
                self.clippy_sel = None;
                // Drive the rename batch; with no renames, just re-run clippy now.
                if !self.start_next_queued_rename() {
                    self.clippy_rename_pending = false;
                    self.start_clippy_run();
                }
            }
        }

        // A diagnostic row was clicked: open its file (incl. user `src/` files)
        // and queue the scroll-to-line, applied once the editor shows that file.
        if let Some((path, line, color)) = nav {
            if let Some(id) =
                crate::app::resolve_diag_file(&path, &self.project_tree.user_src_files)
            {
                self.selected_file = id;
                self.ed.pending_scroll_to_line = Some((id, line));
                self.ed.highlighted_error_line = Some((id, line, color));
            }
        }
        Some(panel.response.rect.top())
    }

    /// Write the current project to the temp workspace and start `cargo clippy`
    /// on a worker thread. Returns `false` when there's no buildable chip config
    /// or the workspace write failed (so the caller can fall back). Clears the
    /// selected-row index since fresh results invalidate it.
    pub(crate) fn start_clippy_run(&mut self) -> bool {
        let Some((project, _tc)) = self.selected_build_cfg() else {
            return false;
        };
        let build_dir = crate::workspace::dir();
        if crate::panels::mcu_module::project_gen::write_project(
            &build_dir,
            &self.current_project_files(),
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        )
        .is_err()
        {
            return false;
        }
        self.clippy_sel = None;
        // Snapshot the compiled text so the "unused local variable" fade can
        // tell later whether this run's diagnostics still match the live file.
        self.snapshot_build_text();
        crate::build::start_clippy(
            build_dir,
            project.target.clone(),
            std::sync::Arc::clone(&self.clippy_state),
            self.egui_ctx.clone(),
            std::sync::Arc::clone(&self.activity),
        );
        true
    }

    /// Fire the next queued rename whose recorded position still matches its
    /// `old_name` (a prior splice/rename in the same batch may have shifted it).
    /// Stale entries are skipped — they resurface on the batch's final clippy
    /// re-run. Returns `true` when a rename was actually started (the caller then
    /// waits for its edits before continuing the batch).
    pub(crate) fn start_next_queued_rename(&mut self) -> bool {
        while let Some(rn) = self.clippy_rename_queue.pop_front() {
            let content = self.file_content_for(&rn.file);
            if ident_at_position(&content, rn.line, rn.col) != rn.old_name {
                continue; // moved since clippy ran — re-run will resurface it
            }
            {
                let mut lsp = self.lsp_state.lock().unwrap();
                lsp.did_change(&rn.file, &content, false);
                // rustc coords are 1-based; LSP wants 0-based.
                lsp.request_rename(
                    &rn.file,
                    rn.line.saturating_sub(1),
                    rn.col.saturating_sub(1),
                    &rn.new_name,
                );
            }
            self.ed.rename_in_flight = true;
            self.egui_ctx.request_repaint();
            return true;
        }
        false
    }
}

/// The identifier starting at 1-based (`line`, `col`) in `content`, or `""` when
/// the position isn't on an identifier. Used to verify a queued rename is still
/// pointing at the symbol it was computed for.
fn ident_at_position(content: &str, line: u32, col: u32) -> String {
    let Some(text) = content.lines().nth(line.saturating_sub(1) as usize) else {
        return String::new();
    };
    let chars: Vec<char> = text.chars().collect();
    let start = col.saturating_sub(1) as usize;
    let is_id = |c: char| c.is_alphanumeric() || c == '_';
    if start >= chars.len() || !is_id(chars[start]) {
        return String::new();
    }
    let mut end = start;
    while end < chars.len() && is_id(chars[end]) {
        end += 1;
    }
    chars[start..end].iter().collect()
}
