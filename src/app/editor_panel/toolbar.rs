//! Code-editor toolbar (header row): Copy, the Errors / Types toggles and the
//! live status label. (Scan USB + Flash moved to the Flash tab; Serial /
//! Terminal / Activity / Clippy shortcuts were removed; the Build button moved
//! into the Cargo Check tab on 2026-07-10.)
//!
//! Also hosts the `pub(crate)` action helpers on AppIde fired from the
//! bottom-panel tabs: `scan_usb`, `flash_swd`, `flash_esp` (Flash tab) and
//! `start_build` (Cargo tab).

use crate::app::{AppIde, BuildPanelTab, ProjectFileId};
use crate::build::{self, BuildState};
use crate::dfu::{self, DfuState};
use crate::espflash::{self, EspFlashState};
use crate::openocd::{self, OpenOcdState};
use crate::panels::mcu_module::{fpga_bitstream, project_gen};
use eframe::egui;
use egui_phosphor::regular as ph;
use std::sync::Arc;

impl AppIde {
    /// Scan connected USB programmers (DFU / ST-Link / J-Link / CMSIS-DAP /
    /// USB-serial) and open the Flash tab. Fired from the Flash tab's Scan
    /// button (was a top-toolbar button before 2026-07-08).
    pub(crate) fn scan_usb(&mut self) {
        self.build_tab = BuildPanelTab::Dfu;
        // An explicit Scan means "show me what is there now", so the previous
        // pick is dropped. The AUTOMATIC scan must not do this — see
        // `scan_usb_keep_selection`.
        self.dfu_sel_programmer = String::new();
        self.scan_usb_keep_selection();
    }

    /// The same USB enumeration, without touching the current selection or the
    /// active tab — what the Flash tab runs by itself on entry. Losing the
    /// chosen programmer just because you looked at the tab would be worse than
    /// having a stale list.
    pub(crate) fn scan_usb_keep_selection(&mut self) {
        dfu::detect_dfu(
            Arc::clone(&self.dfu_state),
            Arc::clone(&self.dfu_log),
            Arc::clone(&self.dfu_programmers),
            self.egui_ctx.clone(),
        );
    }

    /// Enumerate the connected debug probes via `probe-rs list` for the shared
    /// probe selector (RTT / Debug / Flash / Profile).
    ///
    /// Runs on a THREAD: `probe-rs list` shells out, and while it usually
    /// answers immediately, a probe held by another process or left wedged makes
    /// it take seconds — inline, that is the whole IDE frozen. The result lands
    /// in `probe_scan_inbox` and [`apply_probe_scan`] picks it up next frame.
    pub(crate) fn scan_probes(&mut self) {
        if self.probe_scanning {
            return; // one enumeration at a time
        }
        self.probe_scanning = true;
        let inbox = Arc::clone(&self.probe_scan_inbox);
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let result = crate::probe::list_probes();
            *inbox.lock().unwrap() = Some(result);
            ctx.request_repaint();
        });
    }

    /// Apply a finished probe scan. Keeps the current selection when that probe
    /// is still attached, otherwise falls back to auto-select. Call once a
    /// frame; a no-op while nothing has arrived.
    pub(crate) fn apply_probe_scan(&mut self) {
        let Some(result) = self.probe_scan_inbox.lock().unwrap().take() else {
            return;
        };
        self.probe_scanning = false;
        match result {
            Ok(list) => {
                if let Some(sel) = &self.selected_probe {
                    if !list.iter().any(|p| &p.selector == sel) {
                        self.selected_probe = None; // the chosen probe went away
                    }
                }
                self.probe_list = list;
                self.probe_scan_err = None;
            }
            Err(e) => {
                self.probe_list.clear();
                self.probe_scan_err = Some(e);
            }
        }
    }

    /// Get the Flash tab's device lists ready the moment it is opened, so the
    /// first thing you see is what is actually attached.
    ///
    /// Only on a real click on the tab (a flash or a Size run switches to it by
    /// itself — enumerating right as the probe is being claimed helps nobody),
    /// and at most once every [`FLASH_AUTOSCAN_EVERY`], so flipping between tabs
    /// doesn't spawn a scan per click.
    pub(crate) fn autoscan_flash_devices(&mut self, missing_tools: &[&'static str]) {
        const FLASH_AUTOSCAN_EVERY: std::time::Duration = std::time::Duration::from_secs(3);
        if self
            .last_flash_autoscan
            .is_some_and(|t| t.elapsed() < FLASH_AUTOSCAN_EVERY)
        {
            return;
        }
        self.last_flash_autoscan = Some(std::time::Instant::now());
        // USB programmers: both toolchains need this list — the ST-Link for
        // STM32, the USB-serial adapter for espflash.
        self.scan_usb_keep_selection();
        // Debug probes: skipped when probe-rs is known missing, or the scan
        // would just write the same error into the tab on every visit.
        if !crate::app::tabs::tool_missing(missing_tools, "probe-rs") {
            self.scan_probes();
        }
    }

    /// Build `--release` and flash over SWD via OpenOCD, using the selected
    /// programmer's interface/adapter. No-op without a buildable chip config.
    pub(crate) fn flash_swd(&mut self) {
        let Some((project, _tc)) = self.selected_build_cfg() else {
            return;
        };
        let (kind, vid_pid) = self
            .dfu_programmers
            .lock()
            .unwrap()
            .get(&self.dfu_sel_programmer)
            .map(|p| (p.kind.clone(), p.vid_pid.clone()))
            .unwrap_or_default();
        let interface_cfg = openocd::interface_cfg_for_kind(&kind).to_string();
        let adapter = openocd::adapter_select_cmd(&kind, &vid_pid);
        let files = self.current_project_files();
        // The bitstream, and an STM32 flash store's settings and memory.x.
        if let Some((why, phase)) = self.flash_preflight(&files) {
            *self.openocd_state.lock().unwrap() = OpenOcdState::Error(phase);
            self.refuse_flash(why);
            return;
        }
        let build_dir = crate::workspace::dir();
        if project_gen::write_project(
            &build_dir,
            &files,
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        )
        .is_ok()
        {
            self.build_tab = BuildPanelTab::Dfu;
            openocd::start_flash(
                build_dir,
                project.target.clone(),
                project.pkg_name.clone(),
                interface_cfg,
                adapter,
                self.openocd_target_cfg.clone(),
                Arc::clone(&self.openocd_state),
                Arc::clone(&self.dfu_log),
                Arc::clone(&self.swd_flash_child),
                self.egui_ctx.clone(),
                std::sync::Arc::clone(&self.activity),
            );
        }
    }

    /// Flash via probe-rs (`cargo flash`) over the SHARED debug probe
    /// (`selected_probe`) — the same one the Debug / RTT / Runtime tabs use. This
    /// is the Flash tab's probe-rs path; no-op without a buildable chip config.
    pub(crate) fn flash_probe_rs(&mut self) {
        let Some((project, _tc)) = self.selected_build_cfg() else {
            return;
        };
        let files = self.current_project_files();
        // The bitstream, and an STM32 flash store's settings and memory.x.
        if let Some((why, phase)) = self.flash_preflight(&files) {
            *self.probe_flash_state.lock().unwrap() =
                crate::probe_flash::ProbeFlashState::Error(phase);
            self.refuse_flash(why);
            return;
        }
        let build_dir = crate::workspace::dir();
        if project_gen::write_project(
            &build_dir,
            &files,
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        )
        .is_ok()
        {
            self.build_tab = BuildPanelTab::Dfu;
            crate::probe_flash::start_probe_flash(
                build_dir,
                project.target.clone(),
                project.probe_chip.clone(),
                self.selected_probe.clone(),
                Arc::clone(&self.probe_flash_state),
                Arc::clone(&self.dfu_log),
                Arc::clone(&self.probe_flash_child),
                self.egui_ctx.clone(),
                std::sync::Arc::clone(&self.activity),
            );
        }
    }

    /// Say in the Flash tab why a flash did not start. The button is normally
    /// red already (see `show_dfu_tab`); this catches a file that changed since
    /// the tab last looked.
    ///
    /// A fresh log, as every real start begins with: the phase row reads the
    /// log, and the last run's "Build OK" above this line would report a build
    /// that never ran.
    fn refuse_flash(&mut self, why: String) {
        let mut log = self.dfu_log.lock().unwrap();
        log.clear();
        log.push(format!("[error] {why}"));
        drop(log);
        self.build_tab = BuildPanelTab::Dfu;
    }

    /// Stop a running `cargo flash` (the Flash button's second state).
    pub(crate) fn stop_probe_flash(&mut self) {
        crate::probe_flash::stop_probe_flash(&self.probe_flash_child, &self.dfu_log);
    }

    /// Stop a running SWD flash — whichever of its two children is up: the
    /// `cargo build`, or openocd itself.
    pub(crate) fn stop_swd_flash(&mut self) {
        crate::flash_stop::request_stop(&self.swd_flash_child, &self.dfu_log, "openocd");
    }

    /// Stop a running ESP flash (or `espflash board-info`, which holds the same
    /// port) — again whichever child is up, the build or espflash.
    pub(crate) fn stop_esp_flash(&mut self) {
        crate::flash_stop::request_stop(&self.esp_flash_child, &self.dfu_log, "espflash");
    }

    /// Why the project's `partitions.csv` (or flash store) must not be
    /// flashed, or `None` - `flash_store::flash_block`, asked by espflash, RTT
    /// Run and Debug alike: all three pass the table on.
    ///
    /// On an STM32 it is the store's settings and `memory.x` instead
    /// (`flash_store::stm32_flash_block`): every STM32 flashing path writes
    /// the program memory.x laid out.
    pub(crate) fn partition_table_block(&self) -> Option<String> {
        let mcu = self.mcu.as_ref()?;
        crate::panels::mcu_module::flash_store::project_flash_block(
            &self.partitions_csv,
            &self.memory_x,
            mcu.flash_store.as_ref(),
            &mcu.family,
            crate::panels::mcu_module::flash_store::part_of(mcu),
            mcu.runtime,
        )
    }

    /// What the Flash tab's "Fix partitions.csv" would change, while the TABLE
    /// is what blocks flashing (not the store's settings, not a bitstream) and
    /// a subtype espflash panics on is in it - or `None`. Asked only while a
    /// block is shown, never per idle frame.
    pub(crate) fn partition_table_repair(&self) -> Option<String> {
        use crate::panels::mcu_module::flash_store;
        self.partition_table_block()
            .filter(|b| b.starts_with(flash_store::TABLE_BLOCK_PREFIX))?;
        let (_, notes) = flash_store::repair_data_subtypes(&self.partitions_csv)?;
        Some(notes.join("; "))
    }

    /// The Flash tab's "Fix partitions.csv": the subtypes espflash panics on
    /// become `undefined` in the editor's copy - nothing else in the file
    /// moves. Like any edit it reaches the project folder on Save; Flash
    /// already uses it, since the build copy is written from the editor.
    /// The file opens, so the change is seen, and the Flash log says what it
    /// was. `true` when the buffer changed - the caller must then refresh an
    /// editor showing it (see `show_editor_panel`'s `source_rewritten`).
    pub(crate) fn repair_partition_table(&mut self) -> bool {
        let Some((fixed, notes)) =
            crate::panels::mcu_module::flash_store::repair_data_subtypes(&self.partitions_csv)
        else {
            return false;
        };
        self.partitions_csv = fixed;
        self.invalidate_project_files_cache();
        self.selected_file = ProjectFileId::PartitionsCsv;
        let mut log = self.dfu_log.lock().unwrap();
        log.clear();
        log.push(format!(
            "[OK] partitions.csv fixed in the editor ({}) - Save to keep it on disk.",
            notes.join("; ")
        ));
        true
    }

    /// Build `--release` and flash an ESP32 via espflash, over the selected
    /// programmer's serial port. No-op without a buildable chip config.
    pub(crate) fn flash_esp(&mut self) {
        let Some((project, _tc)) = self.selected_build_cfg() else {
            return;
        };
        // A table espflash would crash on, or one that does not reserve the
        // flash store, is refused here rather than flashed - the button is red
        // already; this catches a file edited since the tab last looked.
        if let Some(why) = self.partition_table_block() {
            *self.espflash_state.lock().unwrap() = espflash::EspFlashState::Error(why.clone());
            self.refuse_flash(why);
            return;
        }
        let files = self.current_project_files();
        // From THIS project's state, never from a file in the build workspace:
        // that folder is shared by every project, and a table another one left
        // there would be flashed onto this board.
        let partition_table = !files.partitions_csv.trim().is_empty();
        let build_dir = crate::workspace::dir();
        if project_gen::write_project(
            &build_dir,
            &files,
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        )
        .is_ok()
        {
            self.build_tab = BuildPanelTab::Dfu;
            let port = self
                .dfu_programmers
                .lock()
                .unwrap()
                .get(&self.dfu_sel_programmer)
                .map(|p| p.port.clone())
                .unwrap_or_default();
            // The Monitor cannot attach while it (or the Serial tab) still holds
            // the port, and espflash needs it for the flash itself — so end any
            // live session first. It is restarted below if auto-open is on.
            self.esp_monitor.stop();
            let monitor_follows = self.esp_monitor_auto;
            espflash::start_flash(
                build_dir,
                project.target.clone(),
                project.probe_chip.clone(),
                port,
                Arc::clone(&self.espflash_used_port),
                monitor_follows,
                partition_table,
                Arc::clone(&self.espflash_state),
                Arc::clone(&self.dfu_log),
                Arc::clone(&self.esp_flash_child),
                self.egui_ctx.clone(),
                std::sync::Arc::clone(&self.activity),
            );
        }
    }

    /// Attach `espflash monitor` to the ESP board and stream its output into the
    /// Flash tab's right-hand pane. `after_flash` distinguishes the automatic
    /// run — which follows the port the flash just used, and must reset a chip
    /// the flash deliberately left held — from the Monitor button's manual one.
    /// No-op without an ESP chip config.
    pub(crate) fn start_esp_monitor(&mut self, after_flash: bool) {
        let Some((project, toolchain)) = self.selected_build_cfg() else {
            return;
        };
        use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
        if toolchain != ToolchainKind::EspRust {
            return;
        }
        // A port MUST be resolved here. `--non-interactive` (which the monitor
        // needs, or espflash stops on a prompt nobody can see) refuses to
        // auto-detect: "No serial port was provided … when using the
        // `--non-interactive` flag". So try, in order:
        //   1. the port the last flash actually used (auto-detected or not),
        //   2. the Flash tab's explicit override,
        //   3. the port of the programmer selected in the row above — the same
        //      source `flash_esp` uses, and what the user actually picked.
        let port = {
            let from_flash = self.espflash_used_port.lock().unwrap().clone();
            if after_flash && !from_flash.is_empty() {
                from_flash
            } else if !self.espflash_port.is_empty() {
                self.espflash_port.clone()
            } else if !from_flash.is_empty() {
                from_flash
            } else {
                self.dfu_programmers
                    .lock()
                    .unwrap()
                    .get(&self.dfu_sel_programmer)
                    .map(|p| p.port.clone())
                    .unwrap_or_default()
            }
        };
        if port.is_empty() {
            self.esp_monitor.state.lock().unwrap().push_plain(
                crate::terminal::LineKind::Notice,
                "[error] no serial port known — press Scan and pick the board in the \
                 programmer list above, then try again",
            );
            self.build_tab = BuildPanelTab::Dfu;
            return;
        }
        // The Serial tab opens ports exclusively; two readers on one port means
        // whichever loses gets an opaque OS error. Say so instead of racing.
        //
        // Matched on the PORT, which is why this sits after the resolution
        // above: an unrelated board in the Serial tab is not a conflict - the
        // Serial tab says so itself - and refusing on it was not harmless. This
        // runs after a flash deliberately given `--after no-reset`, so bailing
        // here left the chip sitting in the ROM bootloader with nobody to reset
        // it, and the flash log's cheerful "[OK] ESP32 flash complete!" above.
        if self.serial.is_connected() && self.serial.port == port {
            self.esp_monitor.state.lock().unwrap().push_plain(
                crate::terminal::LineKind::Notice,
                format!(
                    "[error] the Serial tab is connected on {port} - disconnect it there first"
                ),
            );
            self.build_tab = BuildPanelTab::Dfu;
            return;
        }
        // Symbols for backtrace decoding — same ELF path the flash used.
        let build_dir = crate::workspace::dir();
        let elf = build_dir
            .join("target")
            .join(&project.target)
            .join("release")
            .join(format!("{}-project", project.probe_chip));
        self.build_tab = BuildPanelTab::Dfu;
        self.esp_monitor.start(
            build_dir,
            project.probe_chip.clone(),
            port,
            Some(elf),
            // Only the post-flash session left the chip in the bootloader, so
            // only it has something to rescue.
            after_flash,
            self.egui_ctx.clone(),
        );
    }

    /// Run `cargo check` on the generated project: write it to the check
    /// workspace, snapshot the compiled text (for the unused-local fade), then
    /// start the background build. No-op without a buildable chip config.
    /// Fired from the Cargo Check tab's Build button (was a top-toolbar button
    /// before 2026-07-10).
    pub(crate) fn start_build(&mut self, release: bool) {
        let Some((project, _toolchain)) = self.selected_build_cfg() else {
            return;
        };
        let files = self.current_project_files();
        // Refused here too, not only at Flash: the firmware would build fine
        // and the fault would surface on the bench, as an FPGA that never
        // configures.
        if let Err(why) = fpga_bitstream::preflight(&files) {
            self.build_tab = BuildPanelTab::Cargo;
            *self.build_state.lock().unwrap() = BuildState::Failed(fpga_bitstream::refusal(&why));
            return;
        }
        let build_dir = crate::workspace::dir();
        match project_gen::write_project(
            &build_dir,
            &files,
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        ) {
            Ok(()) => {
                self.selected_diagnostic = None;
                self.build_tab = BuildPanelTab::Cargo;
                // Snapshot the compiled text so the "unused local variable"
                // fade can tell later whether this run's diagnostics still
                // match the live file.
                self.snapshot_build_text();
                build::start_build(
                    build_dir,
                    project.target.clone(),
                    Arc::clone(&self.build_state),
                    self.egui_ctx.clone(),
                    Arc::clone(&self.activity),
                    release,
                );
            }
            Err(e) => {
                *self.build_state.lock().unwrap() = BuildState::Failed(format!(
                    "Could not write project to the build workspace ({}): {e}",
                    crate::workspace::dir().display()
                ));
            }
        }
    }

    /// Fire the Flash/RAM measurement once, when a flash that was running has
    /// just finished successfully. Called every frame from `AppIde::ui`.
    ///
    /// It runs AFTER the flash rather than alongside it on purpose: the flash
    /// pipelines build `--release` into the same workspace, and a second cargo
    /// there would just block on the target-dir lock. Afterwards the build is
    /// warm, so the measurement is near-instant.
    pub(crate) fn poll_flash_finished_size(&mut self) {
        let dfu = {
            let s = self.dfu_state.lock().unwrap();
            FlashNow {
                busy: s.is_busy(),
                ok: matches!(*s, DfuState::Success),
            }
        };
        let openocd = {
            let s = self.openocd_state.lock().unwrap();
            FlashNow {
                busy: s.is_busy(),
                ok: matches!(*s, OpenOcdState::Success),
            }
        };
        let espflash = {
            let s = self.espflash_state.lock().unwrap();
            FlashNow {
                busy: s.is_busy(),
                ok: matches!(*s, EspFlashState::Success),
            }
        };
        let probe_rs = {
            let s = self.probe_flash_state.lock().unwrap();
            FlashNow {
                busy: s.is_busy(),
                ok: matches!(*s, crate::probe_flash::ProbeFlashState::Success),
            }
        };
        // A literal, so a pipeline added to FLASH_PIPELINES without a reading
        // here fails to compile. Its order is the index order, pinned by the
        // assertion so renumbering the indexes fails to compile too.
        const _: () = assert!(DFU == 0 && OPENOCD == 1 && ESPFLASH == 2 && PROBE_RS == 3);
        let now: [FlashNow; FLASH_PIPELINES] = [dfu, openocd, espflash, probe_rs];
        let Some(ok) = flash_finished(&mut self.flash_ran, &now) else {
            return;
        };
        // Only on success — a failed flash usually means the build failed, and
        // measuring would just repeat the same cargo error in a second place.
        if ok.iter().any(|&o| o) {
            self.start_size_measure_quiet();
        }
        // ESP only: attach the device console to the board just programmed. The
        // flash left the chip in reset for exactly this (`monitor_follows`), so
        // the monitor resets it and catches the output from the first line.
        if ok[ESPFLASH] && self.esp_monitor_auto {
            self.start_esp_monitor(true);
        }
    }

    /// Measure Flash/RAM usage from the Cargo tab's Size button — brings that
    /// tab to the front so the result is visible.
    pub(crate) fn start_size_measure(&mut self) {
        self.start_size_measure_inner(true);
    }

    /// Same measurement without switching tabs: the Flash tab's own Size button
    /// and the automatic run after each flash, both of which must leave the
    /// Flash tab in view (it renders its own copy of the usage row).
    pub(crate) fn start_size_measure_quiet(&mut self) {
        self.start_size_measure_inner(false);
    }

    /// Measure Flash/RAM usage: write the project, `cargo build --release`,
    /// then parse the ELF against the memory.x limits (see `crate::size`).
    /// No-op without a chip config.
    fn start_size_measure_inner(&mut self, focus_cargo_tab: bool) {
        let Some((project, _toolchain)) = self.selected_build_cfg() else {
            return;
        };
        let build_dir = crate::workspace::dir();
        match project_gen::write_project(
            &build_dir,
            &self.current_project_files(),
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        ) {
            Ok(()) => {
                if focus_cargo_tab {
                    self.build_tab = BuildPanelTab::Cargo;
                }
                crate::size::start_measure(
                    build_dir,
                    project.target.clone(),
                    self.memory_x.clone(),
                    Arc::clone(&self.size_state),
                    self.egui_ctx.clone(),
                    Arc::clone(&self.activity),
                );
            }
            Err(e) => {
                *self.size_state.lock().unwrap() =
                    crate::size::SizeState::Failed(format!("Could not write project: {e}"));
            }
        }
    }

    /// Run `cargo bloat` for the Profile tab: write the project, then analyze the
    /// release build's `.text` per function (or per crate). No-op without a chip.
    pub(crate) fn start_profile(&mut self) {
        let Some((project, _toolchain)) = self.selected_build_cfg() else {
            return;
        };
        let build_dir = crate::workspace::dir();
        match project_gen::write_project(
            &build_dir,
            &self.current_project_files(),
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        ) {
            Ok(()) => {
                self.build_tab = BuildPanelTab::Profile;
                crate::profile::start_profile(
                    build_dir,
                    project.target.clone(),
                    self.profile_by_crate,
                    Arc::clone(&self.profile_state),
                    self.egui_ctx.clone(),
                );
            }
            Err(e) => {
                *self.profile_state.lock().unwrap() =
                    crate::profile::ProfileState::Failed(format!("Could not write project: {e}"));
            }
        }
    }

    /// Runtime flamegraph: write the project, then halt-sample the RUNNING
    /// firmware's call stack via probe-rs (see `crate::flamegraph`). Attach only
    /// (no flash) — the firmware must already be running. No-op without a chip.
    pub(crate) fn start_flame(&mut self) {
        let Some((project, toolchain)) = self.selected_build_cfg() else {
            return;
        };
        let build_dir = crate::workspace::dir();
        match project_gen::write_project(
            &build_dir,
            &self.current_project_files(),
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        ) {
            Ok(()) => {
                self.build_tab = BuildPanelTab::Profile;
                crate::flamegraph::start_flame(
                    build_dir,
                    project.target.clone(),
                    project.probe_chip.clone(),
                    self.selected_probe.clone(),
                    toolchain,
                    // Sample count. Measured on an ESP32-C3 over its built-in
                    // USB-JTAG: ~0.44 s per sample, nearly all of it the
                    // pause/stackTrace/continue round trip and none of it the
                    // 8 ms spacing - so 400 was about THREE MINUTES of watching
                    // a counter, not the "few seconds" it used to claim here.
                    // 120 is ~55 s, and still leaves a branch worth 10% of the
                    // runtime around a dozen samples wide - enough to read off
                    // the graph. Three hardware runs at this count landed
                    // within 8-11% of a 10% branch.
                    120,
                    Arc::clone(&self.flame_state),
                    self.egui_ctx.clone(),
                );
            }
            Err(e) => {
                *self.flame_state.lock().unwrap() =
                    crate::flamegraph::FlameState::Failed(format!("Could not write project: {e}"));
            }
        }
    }

    /// What stops RTT Run and Debug from flashing, as (console line, phase
    /// text): the FPGA bitstream, then the partition table probe-rs would be
    /// handed (`--idf-partition-table`) - the same check the espflash path
    /// makes.
    fn flash_preflight(&self, files: &project_gen::ProjectFiles) -> Option<(String, String)> {
        match fpga_bitstream::preflight(files) {
            Err(why) => {
                let phase = fpga_bitstream::refusal(&why);
                Some((why, phase))
            }
            Ok(()) => self.partition_table_block().map(|why| (why.clone(), why)),
        }
    }

    /// Start an RTT session: write the project, then hand off to the
    /// [`crate::rtt::RttConsole`] pipeline (build --release → probe-rs
    /// run/attach). Fired from the RTT tab's buttons. No-op without a chip.
    pub(crate) fn start_rtt(&mut self, mode: crate::rtt::RttMode) {
        let Some((project, _toolchain)) = self.selected_build_cfg() else {
            return;
        };
        let files = self.current_project_files();
        // Run flashes the board; Attach only reads what already runs there.
        if mode == crate::rtt::RttMode::Run
            && let Some((why, phase)) = self.flash_preflight(&files)
        {
            self.build_tab = BuildPanelTab::Rtt;
            self.rtt
                .state
                .lock()
                .unwrap()
                .push_plain(crate::terminal::LineKind::Notice, format!("[error] {why}"));
            *self.rtt.phase.lock().unwrap() = crate::rtt::RttPhase::Error(phase);
            return;
        }
        let build_dir = crate::workspace::dir();
        match project_gen::write_project(
            &build_dir,
            &files,
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        ) {
            Ok(()) => {
                self.build_tab = BuildPanelTab::Rtt;
                self.rtt.start(
                    mode,
                    build_dir,
                    project.target.clone(),
                    project.probe_chip.clone(),
                    self.selected_probe.clone(),
                    self.egui_ctx.clone(),
                );
            }
            Err(e) => {
                *self.rtt.phase.lock().unwrap() =
                    crate::rtt::RttPhase::Error(format!("could not write project: {e}"));
            }
        }
    }

    /// Start a debug session: write the project, snapshot the breakpoints,
    /// then hand off to the [`crate::debugger::Debugger`] pipeline (build →
    /// probe-rs dap-server → flash + attach). No-op without a chip config.
    pub(crate) fn start_debug(&mut self) {
        let Some((project, _toolchain)) = self.selected_build_cfg() else {
            return;
        };
        let files = self.current_project_files();
        if let Some((why, phase)) = self.flash_preflight(&files) {
            self.build_tab = BuildPanelTab::Debug;
            self.debugger
                .console
                .lock()
                .unwrap()
                .push_plain(crate::terminal::LineKind::Notice, format!("[error] {why}"));
            self.debugger.state.lock().unwrap().phase = crate::debugger::DebugPhase::Error(phase);
            return;
        }
        let build_dir = crate::workspace::dir();
        match project_gen::write_project(
            &build_dir,
            &files,
            &self.project_tree.user_src_files,
            &self.mcu_config_text(),
            &self.structure_config_text(),
        ) {
            Ok(()) => {
                self.build_tab = BuildPanelTab::Debug;
                let bps: std::collections::BTreeMap<String, Vec<u32>> = self
                    .breakpoints
                    .iter()
                    .map(|(k, v)| (k.clone(), v.iter().copied().collect()))
                    .collect();
                self.debugger.start(
                    build_dir,
                    project.target.clone(),
                    project.probe_chip.clone(),
                    self.selected_probe.clone(),
                    bps,
                    self.egui_ctx.clone(),
                );
            }
            Err(e) => {
                self.debugger.state.lock().unwrap().phase =
                    crate::debugger::DebugPhase::Error(format!("could not write project: {e}"));
            }
        }
    }

    /// Render the editor header toolbar.  `display_code` is the text shown in
    /// the editor (copied verbatim by the Copy button).
    pub(super) fn show_editor_toolbar(&mut self, ui: &mut egui::Ui, display_code: &str) {
        ui.horizontal(|ui| {
            // The open file IS this panel's title. "Code Editor" named the panel
            // the user is already looking at; the path is the one thing here
            // that changes, and it used to sit in the middle of the right-hand
            // button group where nothing anchored it.
            let open_label = match self.selected_file {
                ProjectFileId::UserFile(i) => self
                    .project_tree
                    .user_src_files
                    .get(i)
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| "src/???".to_string()),
                other => other.label().to_string(),
            };
            // Heading size, resolved from the STYLE rather than hardcoded, so a
            // theme change moves the title with everything else.
            let heading_size = egui::TextStyle::Heading.resolve(ui.style()).size;
            ui.label(
                egui::RichText::new(elide_path_left(&open_label, PATH_MAX_CHARS))
                    .size(heading_size)
                    .color(egui::Color32::from_rgb(120, 160, 200)),
            )
            .on_hover_text(&open_label);

            // ── Analysis badge ────────────────────────────────────────────
            // "Clean" and "not analysed since you typed" used to be the same
            // number of pixels: zero. The inline overlay blanks whenever
            // rust-analyzer's copy is behind the buffer, and the floating error
            // list returns before drawing anything when it has no rows — so the
            // one state a reader most needs named was the one nothing named.
            //
            // Drawn ONLY when something is off, so a healthy file stays quiet.
            {
                let rel = crate::editor::gui::text_pos::selected_file_rel_path(
                    &self.selected_file,
                    &self.project_tree.user_src_files,
                );
                let state = match &rel {
                    Some(rel) => {
                        let lsp = self.lsp_state.lock().unwrap();
                        analysis_of(
                            matches!(lsp.status, crate::lsp::LspStatus::Ready),
                            lsp.is_file_open(rel),
                            lsp.last_sent_matches(rel, display_code),
                            lsp.diagnostics_fresh(rel),
                        )
                    }
                    None => Analysis::Live,
                };
                if let Some((label, hover)) = analysis_badge(state) {
                    ui.label(
                        egui::RichText::new(label)
                            .size(10.5)
                            .color(egui::Color32::from_rgb(128, 132, 142)),
                    )
                    .on_hover_text(hover);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // ── Collapse / expand the Project tree (far-right panel) ──
                // Deliberately a DIFFERENT glyph from the MCU toggle beside it:
                // two identical carets sitting next to each other are a coin
                // flip every time you reach for one.
                // Too narrow for both right-hand zones (see
                // `AppIde::enforce_narrow_layout`)? Then these two toggles are a
                // radio pair: showing one hides the other, and the hover text
                // says so rather than leaving the user to discover it.
                let narrow = self.layout_narrow;
                let tree_hidden = self.tree_collapsed;
                if ui
                    .selectable_label(
                        tree_hidden,
                        egui::RichText::new(ph::SIDEBAR_SIMPLE)
                            .size(11.0)
                            .color(if tree_hidden {
                                egui::Color32::from_rgb(120, 190, 240)
                            } else {
                                egui::Color32::GRAY
                            }),
                    )
                    .on_hover_text(match (tree_hidden, narrow) {
                        (true, true) => {
                            "Show the Project tree — the window is too narrow for \
                             both, so the MCU Configurator gives way"
                        }
                        (true, false) => "Show the Project tree again",
                        (false, _) => {
                            "Hide the Project tree so the editor widens — this button \
                             brings it back"
                        }
                    })
                    .clicked()
                {
                    self.tree_collapsed = !tree_hidden;
                    // Showing the tree on a narrow window: the MCU zone is what
                    // makes room for it.
                    if narrow && !self.tree_collapsed {
                        self.side_panels_collapsed = true;
                    }
                }

                ui.add_space(4.0);

                // ── Collapse / expand the middle (MCU Configurator) zone ──
                // Hides Pins / Clock / Structure / … so the editor widens; the
                // Project tree on the far right always stays.
                // NOTE: this layout is RIGHT-TO-LEFT, so the first widget added
                // sits furthest right — this must come BEFORE Copy to appear to
                // its right.
                let collapsed = self.side_panels_collapsed;
                if ui
                    .selectable_label(
                        collapsed,
                        egui::RichText::new(if collapsed {
                            // format!("{} Panels", ph::ARROWS_OUT_SIMPLE)
                            ph::CARET_DOUBLE_LEFT
                        } else {
                            // format!("{} Panels", ph::ARROWS_IN_SIMPLE)
                            ph::CARET_RIGHT
                        })
                        .size(11.0)
                        .color(if collapsed {
                            egui::Color32::from_rgb(120, 190, 240)
                        } else {
                            egui::Color32::GRAY
                        }),
                    )
                    .on_hover_text(match (collapsed, narrow) {
                        (true, true) => {
                            "Show the MCU Configurator (Pins / Clock / Structure …) — the \
                             window is too narrow for both, so the Project tree gives way"
                        }
                        (true, false) => "Show the MCU Configurator again (Pins / Clock / Structure …)",
                        (false, _) => {
                            "Hide the MCU Configurator (Pins / Clock / Structure …) so the editor widens — the Project tree stays"
                        }
                    })
                    .clicked()
                {
                    self.side_panels_collapsed = !collapsed;
                    // The other half of the radio pair — see the tree toggle.
                    if narrow && !self.side_panels_collapsed {
                        self.tree_collapsed = true;
                    }
                }

                ui.add_space(4.0);

                // Copy button — copies the currently displayed file
                let copy_ok = format!("{} Copied!", ph::CHECK);
                let copy_def = format!("{} Copy", ph::COPY);
                let copy_label: &str = if self.copy_flash > 0 {
                    &copy_ok
                } else {
                    &copy_def
                };
                let copy_btn = ui.add(egui::Button::new(
                    egui::RichText::new(copy_label).size(11.0),
                ));
                if copy_btn.clicked() {
                    ui.output_mut(|o| {
                        o.commands.push(egui::output::OutputCommand::CopyText(
                            display_code.to_owned(),
                        ));
                    });
                    self.copy_flash = 60;
                }

                ui.add_space(4.0);

                // (Serial / Terminal / Activity / Clippy shortcut buttons were
                // removed on 2026-07-08; the Build button moved into the Cargo
                // Check tab on 2026-07-10 — see `show_cargo_tab` / `start_build`.)

                // ── Inline-errors toggle ──────────────────────────────
                // Show/hide the in-editor RA/cargo diagnostic overlay
                // (squiggles + inline error text). The bottom-panel Cargo
                // Check / rust-analyzer tabs keep listing everything.
                let inline_btn = ui.selectable_label(
                    self.inline_errors_enabled,
                    egui::RichText::new(format!("{} Errors", ph::WARNING_OCTAGON))
                        .size(11.0)
                        .color(if self.inline_errors_enabled {
                            egui::Color32::from_rgb(230, 160, 60)
                        } else {
                            egui::Color32::GRAY
                        }),
                );
                if inline_btn.clicked() {
                    self.inline_errors_enabled = !self.inline_errors_enabled;
                }
                inline_btn.on_hover_text(if self.inline_errors_enabled {
                    "Inline errors: ON — squiggles and error messages are drawn in \
                     the editor.\nClick to hide them (they stay in the Cargo Check / \
                     rust-analyzer tabs)."
                } else {
                    "Inline errors: OFF — the editor overlay is hidden.\nClick to \
                     show squiggles and error messages inline again."
                });

                ui.add_space(4.0);

                // ── Inline warnings / info / hints toggle ─────────────
                // The other half of the same overlay. Its own button because
                // the two are wanted at different moments — errors while
                // getting a build to pass, the rest while tidying up.
                //
                // Covers every severity that is NOT an error, not just
                // `Info`: rust-analyzer publishes lints as `Warning` and weak
                // lints as `Hint`, and hardly ever uses `Info` at all, so a
                // button wired to `Info` alone would look broken.
                let info_btn = ui.selectable_label(
                    self.inline_info_enabled,
                    egui::RichText::new(format!("{} Info", ph::INFO))
                        .size(11.0)
                        .color(if self.inline_info_enabled {
                            egui::Color32::from_rgb(80, 140, 215)
                        } else {
                            egui::Color32::GRAY
                        }),
                );
                if info_btn.clicked() {
                    self.inline_info_enabled = !self.inline_info_enabled;
                }
                info_btn.on_hover_text(if self.inline_info_enabled {
                    "Inline info: ON — warnings, hints and informational messages                      are drawn in the editor alongside the errors.
Click to hide                      them (they stay in the Cargo Check / rust-analyzer tabs)."
                } else {
                    "Inline info: OFF — only errors are marked in the editor.
                     Click to show warnings and hints inline too."
                });

                ui.add_space(4.0);

                // ── Inferred-type hint toggle ─────────────────────────
                // Show/hide the ghost type on the cursor's untyped `let` line
                // (Tab inserts it). OFF also disables the Tab accept.
                let types_btn = ui.selectable_label(
                    self.inlay_types_enabled,
                    egui::RichText::new(format!("{} Types", ph::TEXT_T))
                        .size(11.0)
                        .color(if self.inlay_types_enabled {
                            egui::Color32::from_rgb(120, 170, 210)
                        } else {
                            egui::Color32::GRAY
                        }),
                );
                if types_btn.clicked() {
                    self.inlay_types_enabled = !self.inlay_types_enabled;
                }
                types_btn.on_hover_text(if self.inlay_types_enabled {
                    "Inferred types: ON — the type of the `let` on the cursor line \
                     shows as ghost text; press Tab to insert it.\nClick to hide."
                } else {
                    "Inferred types: OFF — no ghost type is shown.\nClick to show \
                     the inferred type on the cursor's `let` line (Tab inserts it)."
                });

                ui.add_space(4.0);

                // ── Git diff line-background toggle ────────────────────
                // The full-width yellow band on git-changed lines can distract /
                // mis-align while editing; OFF keeps just the gutter bars.
                let bg_btn = ui.selectable_label(
                    self.diff_line_bg,
                    egui::RichText::new(format!("{} Diff bg", ph::GIT_DIFF))
                        .size(11.0)
                        .color(if self.diff_line_bg {
                            egui::Color32::from_rgb(210, 190, 90)
                        } else {
                            egui::Color32::GRAY
                        }),
                );
                if bg_btn.clicked() {
                    self.diff_line_bg = !self.diff_line_bg;
                }
                bg_btn.on_hover_text(if self.diff_line_bg {
                    "Git change background: ON — changed lines get a full-width \
                     yellow band.\nClick to keep ONLY the gutter bars next to the \
                     line number."
                } else {
                    "Git change background: OFF — only the gutter bars (green / \
                     amber) mark changed lines.\nClick to highlight the whole row \
                     again."
                });

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── USB DFU + SWD section ─────────────────────────────
                let dfu_guard = self.dfu_state.lock().unwrap();
                let dfu_busy = dfu_guard.is_busy();
                let dfu_label = dfu_guard.status_label().to_string();
                let dfu_color = dfu_guard.status_color();
                let dfu_detail = dfu_guard.detail();
                _ = matches!(*dfu_guard, DfuState::DeviceFound(_));
                drop(dfu_guard);

                let ocd_busy = self.openocd_state.lock().unwrap().is_busy();
                let esp_busy = self.espflash_state.lock().unwrap().is_busy();
                let any_busy = dfu_busy || ocd_busy || esp_busy;

                // Keep UI refreshing while any flash operation is running.
                // (Scan USB + Flash SWD/ESP32 buttons moved to the Flash tab's
                // Programmer row on 2026-07-08 — see `dfu_tab.rs`.)
                if any_busy {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(120));
                }

                ui.add_space(4.0);

                // Status label — shows the most active state
                let (show_label, show_color, show_detail) = {
                    let ocd = self.openocd_state.lock().unwrap();
                    let esp = self.espflash_state.lock().unwrap();
                    if !matches!(*ocd, OpenOcdState::Idle) {
                        (ocd.status_label().to_string(), ocd.status_color(), None)
                    } else if !matches!(*esp, EspFlashState::Idle) {
                        (esp.status_label().to_string(), esp.status_color(), None)
                    } else {
                        (dfu_label.clone(), dfu_color, dfu_detail)
                    }
                };
                let status_widget = ui.label(
                    egui::RichText::new(&show_label)
                        .size(10.5)
                        .color(show_color),
                );
                if let Some(detail) = show_detail {
                    status_widget.on_hover_text(detail);
                }

            });
        });
    }
}

/// What the editor knows about rust-analyzer's view of the file on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Analysis {
    /// rust-analyzer holds this exact text and has published for it.
    Live,
    /// It holds this text; the answer has not come back yet.
    Analysing,
    /// Its copy predates these edits — everything the editor shows about this
    /// file is from before you typed.
    Stale,
    /// No analyzer for this file at all.
    Off,
}

/// Fold the four LSP facts into one state.
fn analysis_of(ready: bool, file_open: bool, in_sync: bool, diags_fresh: bool) -> Analysis {
    if !ready {
        return Analysis::Off;
    }
    // A file rust-analyzer has never opened has no analysis to be stale ABOUT;
    // saying "stale" there would blame the edits for a file it never read.
    if !file_open {
        return Analysis::Off;
    }
    match (in_sync, diags_fresh) {
        (false, _) => Analysis::Stale,
        (true, false) => Analysis::Analysing,
        (true, true) => Analysis::Live,
    }
}

/// `(badge, hover)` for a state worth naming; `None` while all is well.
///
/// `Ctrl+S` is in the text on purpose and is the load-bearing half. A Project
/// Save is the ONLY thing in this app that re-syncs rust-analyzer — there is no
/// idle debounce, whatever three older comments claimed — so a reader who does
/// not know that waits for a squiggle that is never coming.
fn analysis_badge(a: Analysis) -> Option<(&'static str, &'static str)> {
    match a {
        Analysis::Live => None,
        Analysis::Stale => Some((
            "· analysis stale · Ctrl+S",
            "rust-analyzer has not seen these edits. Everything the editor shows              about this file — squiggles, inline messages, the error list, types —              is from before you typed. A Project Save (Ctrl+S) is the only thing              that re-analyses.",
        )),
        Analysis::Analysing => Some((
            "· analysing…",
            "Sent to rust-analyzer; waiting for the result.",
        )),
        Analysis::Off => Some((
            "· no analyzer",
            "rust-analyzer is not running for this file, so nothing here is              analysed: no squiggles, no types, no go-to-definition.",
        )),
    }
}

/// The flash pipelines [`AppIde::poll_flash_finished_size`] watches, as indexes
/// into its [`FlashNow`] array and into `AppIde::flash_ran`.
const DFU: usize = 0;
const OPENOCD: usize = 1;
const ESPFLASH: usize = 2;
const PROBE_RS: usize = 3;
pub(crate) const FLASH_PIPELINES: usize = 4;

/// One flash pipeline, as read this frame.
#[derive(Clone, Copy)]
struct FlashNow {
    busy: bool,
    /// Its state is `Success`, from this run or from an earlier one.
    ok: bool,
}

/// One frame of the "a flash just finished" edge detector.
///
/// `ran` marks every pipeline seen busy since the last frame on which none was.
/// It returns `None` while any pipeline is still busy, and on idle frames with
/// no run behind them. When the last busy pipeline stops, it returns which of
/// the ones that ran ended in `Success`, and clears `ran`.
///
/// Only the pipelines that RAN count: a `Success` stays in a pipeline's state
/// until its next run, so a board flashed over OpenOCD earlier and then over
/// probe-rs with a build error would otherwise still read as a success.
fn flash_finished(
    ran: &mut [bool; FLASH_PIPELINES],
    now: &[FlashNow; FLASH_PIPELINES],
) -> Option<[bool; FLASH_PIPELINES]> {
    for (r, n) in ran.iter_mut().zip(now) {
        *r |= n.busy;
    }
    if now.iter().any(|n| n.busy) || !ran.iter().any(|&r| r) {
        return None;
    }
    let ok = std::array::from_fn(|i| ran[i] && now[i].ok);
    *ran = [false; FLASH_PIPELINES];
    Some(ok)
}

/// Longest path drawn in the title before it is shortened.
///
/// The title now shares one row with the whole right-hand button group, so an
/// unbounded path grows until the two overlap. At the heading's 14 px this is
/// roughly a third of a comfortably-sized editor panel.
const PATH_MAX_CHARS: usize = 46;

/// `path` shortened from the LEFT, never past `max` characters.
///
/// The head is what gives way, because the tail is what identifies the file:
/// `src/mw_radar/read_report.rs` becomes `…/mw_radar/read_report.rs`. Cutting
/// from the right instead would leave every file in a deep folder looking
/// identical, which is the opposite of what a title is for.
fn elide_path_left(path: &str, max: usize) -> String {
    let n = path.chars().count();
    if n <= max {
        return path.to_string();
    }
    // `max - 1` leaves room for the ellipsis, so the result is exactly `max`.
    let tail: String = path.chars().skip(n - (max - 1)).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::{PATH_MAX_CHARS, elide_path_left};

    use super::{Analysis, analysis_badge, analysis_of};

    /// A healthy file says nothing. The badge only earns its pixels when the
    /// editor would otherwise be showing something untrue by omission.
    #[test]
    fn an_analysed_file_gets_no_badge() {
        assert_eq!(analysis_of(true, true, true, true), Analysis::Live);
        assert!(analysis_badge(Analysis::Live).is_none());
    }

    /// The state this whole change exists for: rust-analyzer's copy is behind,
    /// so the overlay is blank and the error list draws nothing — previously
    /// indistinguishable from a clean file.
    #[test]
    fn edits_rust_analyzer_has_not_seen_are_named() {
        assert_eq!(analysis_of(true, true, false, true), Analysis::Stale);
        let (label, hover) = analysis_badge(Analysis::Stale).expect("a badge");
        assert!(label.contains("stale"));
        // Load-bearing: a Save is the ONLY re-sync in this app, so a reader who
        // is not told that waits for an update that never arrives.
        assert!(
            label.contains("Ctrl+S") && hover.contains("Ctrl+S"),
            "the remedy has to be on the badge itself: {label:?}"
        );
    }

    /// Saved, but the answer has not landed. Distinct from stale so that
    /// "I pressed Ctrl+S, why does it still say stale?" has an answer.
    #[test]
    fn a_sent_but_unanswered_file_says_so() {
        assert_eq!(analysis_of(true, true, true, false), Analysis::Analysing);
        assert!(
            analysis_badge(Analysis::Analysing)
                .expect("a badge")
                .0
                .contains("analysing")
        );
    }

    /// A file the analyzer never opened has no analysis to be stale ABOUT —
    /// calling that "stale" would blame the user's edits for a file it never
    /// read.
    #[test]
    fn a_file_the_analyzer_never_opened_is_not_called_stale() {
        assert_eq!(analysis_of(true, false, false, false), Analysis::Off);
        assert_eq!(analysis_of(false, true, true, true), Analysis::Off);
    }

    #[test]
    fn a_short_path_is_left_alone() {
        assert_eq!(
            elide_path_left("src/main.rs", PATH_MAX_CHARS),
            "src/main.rs"
        );
    }

    /// The FILE NAME is what a title has to keep. Cutting from the right would
    /// turn every file in one folder into the same title.
    #[test]
    fn a_long_path_loses_its_head_and_keeps_its_name() {
        let long = "src/mw_radar/protocol/frames/decoding/read_report.rs";
        let out = elide_path_left(long, 30);
        assert!(out.starts_with('…'), "cut from the left: {out}");
        assert!(out.ends_with("read_report.rs"), "the name survives: {out}");
        assert_eq!(out.chars().count(), 30, "and it fits exactly");
    }

    /// Counted in CHARACTERS: a byte cut would panic mid-`ă` on a path the user
    /// named in Romanian, and slicing a title is not worth a crash.
    #[test]
    fn a_non_ascii_path_survives_the_cut() {
        let long = "src/măsurători/înregistrări/frecvență_semnal_radar.rs";
        let out = elide_path_left(long, 24);
        assert_eq!(out.chars().count(), 24);
        assert!(out.ends_with(".rs"));
    }

    mod flash_finished {
        use super::super::{
            DFU, ESPFLASH, FLASH_PIPELINES, FlashNow, OPENOCD, PROBE_RS, flash_finished,
        };

        const IDLE: FlashNow = FlashNow {
            busy: false,
            ok: false,
        };
        const BUSY: FlashNow = FlashNow {
            busy: true,
            ok: false,
        };
        const OK: FlashNow = FlashNow {
            busy: false,
            ok: true,
        };

        /// Every pipeline idle except `i`, which is `state`.
        fn only(i: usize, state: FlashNow) -> [FlashNow; FLASH_PIPELINES] {
            let mut now = [IDLE; FLASH_PIPELINES];
            now[i] = state;
            now
        }

        /// The bug this exists for: a probe-rs flash that succeeds measures
        /// Flash/RAM, as every other flash path already did.
        #[test]
        fn a_probe_rs_flash_that_succeeds_is_measured() {
            let mut ran = [false; FLASH_PIPELINES];
            assert_eq!(flash_finished(&mut ran, &only(PROBE_RS, BUSY)), None);
            let ok = flash_finished(&mut ran, &only(PROBE_RS, OK)).expect("finished");
            assert!(ok[PROBE_RS]);
            // Fires once, not on every idle frame after it.
            assert_eq!(flash_finished(&mut ran, &only(PROBE_RS, OK)), None);
        }

        /// The existing paths still fire, each on its own index, so the ESP
        /// monitor still follows an espflash run and only that.
        #[test]
        fn every_pipeline_reports_on_its_own_index() {
            for i in [DFU, OPENOCD, ESPFLASH, PROBE_RS] {
                let mut ran = [false; FLASH_PIPELINES];
                flash_finished(&mut ran, &only(i, BUSY));
                let ok = flash_finished(&mut ran, &only(i, OK)).expect("finished");
                let mut want = [false; FLASH_PIPELINES];
                want[i] = true;
                assert_eq!(ok, want, "pipeline {i}");
            }
        }

        /// A failed flash finishes without a success, so nothing is measured.
        #[test]
        fn a_failed_flash_is_not_a_success() {
            let mut ran = [false; FLASH_PIPELINES];
            flash_finished(&mut ran, &only(PROBE_RS, BUSY));
            let ok = flash_finished(&mut ran, &only(PROBE_RS, IDLE)).expect("finished");
            assert!(!ok.iter().any(|&o| o));
        }

        /// A `Success` left over from an earlier flash on another path does not
        /// turn this run's failure into one.
        #[test]
        fn a_stale_success_on_another_path_does_not_count() {
            let mut ran = [false; FLASH_PIPELINES];
            let mut now = only(DFU, OK);
            now[PROBE_RS] = BUSY;
            flash_finished(&mut ran, &now);
            now[PROBE_RS] = IDLE;
            let ok = flash_finished(&mut ran, &now).expect("finished");
            assert!(!ok[DFU], "DFU did not run this time");
            assert!(!ok.iter().any(|&o| o));
        }

        /// Nothing fires while any pipeline is still busy, and nothing fires on
        /// idle frames that no run preceded.
        #[test]
        fn waits_for_the_last_busy_pipeline() {
            let mut ran = [false; FLASH_PIPELINES];
            assert_eq!(flash_finished(&mut ran, &only(DFU, OK)), None);

            let mut now = only(PROBE_RS, BUSY);
            now[OPENOCD] = BUSY;
            assert_eq!(flash_finished(&mut ran, &now), None);
            now[PROBE_RS] = OK;
            assert_eq!(flash_finished(&mut ran, &now), None, "OpenOCD still busy");
            now[OPENOCD] = OK;
            let ok = flash_finished(&mut ran, &now).expect("finished");
            assert!(ok[PROBE_RS] && ok[OPENOCD]);
        }
    }

    /// The boundary: exactly at the cap nothing is touched, one past it is cut.
    #[test]
    fn the_cap_is_inclusive() {
        let at = "a".repeat(PATH_MAX_CHARS);
        assert_eq!(elide_path_left(&at, PATH_MAX_CHARS), at);
        let over = "a".repeat(PATH_MAX_CHARS + 1);
        assert_eq!(
            elide_path_left(&over, PATH_MAX_CHARS).chars().count(),
            PATH_MAX_CHARS
        );
    }

    /// Every action here that writes the build copy either checks the FPGA
    /// bitstream first, or is on the list of those that never program a board
    /// with one. A new flash path lands in neither, and fails this test.
    #[test]
    fn every_path_that_programs_the_board_checks_the_bitstream() {
        const CHECKS: [&str; 5] = [
            "flash_swd",
            "flash_probe_rs",
            "start_build",
            "start_rtt",
            "start_debug",
        ];
        // The paths that hand an ESP's partitions.csv on (espflash
        // `--partition-table`, probe-rs `--idf-partition-table`, DAP), and
        // every path that writes an STM32 the memory.x of a flash store
        // (OpenOCD, cargo flash, RTT Run, Debug).
        const TABLE_CHECKS: [&str; 5] = [
            "flash_esp",
            "flash_swd",
            "flash_probe_rs",
            "start_rtt",
            "start_debug",
        ];
        // Measuring, profiling, sampling a running board, or an ESP (no FPGA).
        const EXEMPT: [&str; 4] = [
            "flash_esp",
            "start_size_measure_inner",
            "start_profile",
            "start_flame",
        ];
        let src = include_str!("toolbar.rs");
        let src = &src[..src.find("#[cfg(test)]").expect("test module")];
        let mut seen = Vec::new();
        for chunk in src.split("fn ").skip(1) {
            let name: String = chunk
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            // The body runs to the next item at this indentation.
            let body = &chunk[..chunk.find("\n    }\n").unwrap_or(chunk.len())];
            let Some(write) = body.find("project_gen::write_project(") else {
                continue;
            };
            if seen.contains(&name) {
                continue;
            }
            // `flash_preflight` is the bitstream check plus the partition table.
            let first = |a: &str, b: &str| match (body.find(a), body.find(b)) {
                (Some(x), Some(y)) => Some(x.min(y)),
                (x, y) => x.or(y),
            };
            if TABLE_CHECKS.contains(&name.as_str()) {
                let check = first("partition_table_block()", "self.flash_preflight(")
                    .unwrap_or_else(|| panic!("{name} flashes without checking partitions.csv"));
                assert!(check < write, "{name} checks the table only after writing");
            }
            if CHECKS.contains(&name.as_str()) {
                let check = first("fpga_bitstream::preflight(", "self.flash_preflight(")
                    .unwrap_or_else(|| panic!("{name} writes the project without the check"));
                assert!(
                    check < write,
                    "{name} checks only after writing the project"
                );
            } else {
                assert!(
                    EXEMPT.contains(&name.as_str()),
                    "{name} writes the build copy: add it to CHECKS (it programs the \
                     board) or to EXEMPT (it never does)"
                );
            }
            seen.push(name);
        }
        for name in CHECKS.iter().chain(&EXEMPT) {
            assert!(
                seen.iter().any(|s| s == name),
                "{name} is gone or no longer writes"
            );
        }
    }
}

/// The reported case end to end on the app: the example project's own table
/// blocks Flash ESP32, the repair unblocks it in the editor's copy - the
/// copy every flash path writes the build from.
#[cfg(test)]
mod partition_repair_tests {
    use crate::app::{AppIde, BuildPanelTab, ProjectFileId};
    use eframe::egui;

    /// The example project's table, as the editor holds it (LF).
    const THEIRS: &str = concat!(
        "# Name,   Type, SubType, Offset,   Size\n",
        "nvs,      data, nvs,     0x9000,   0x6000\n",
        "phy_init, data, phy,     0xf000,   0x1000\n",
        "factory,  app,  factory, 0x10000,  0x3E0000\n",
        "cfg,      data, 0x99,    0x3F0000, 0x4000",
    );
    const FIXED_ROW: &str = "cfg,      data, undefined, 0x3F0000, 0x4000";

    fn esp_app(ctx: egui::Context) -> AppIde {
        let mut app = AppIde::new(&eframe::CreationContext::_new_kittest(ctx), None, None);
        app._fs_watcher = None;
        app.fs_watched = None;
        app.startup_picker = None;
        app.selected_mcu_id = "esp32c3".to_owned();
        app.mcu = AppIde::build_mcu_for(&app.mcu_registry, "esp32c3");
        app.partitions_csv = THEIRS.to_owned();
        app
    }

    #[test]
    fn the_repair_unblocks_flashing_opens_the_file_and_says_to_save() {
        let mut app = esp_app(egui::Context::default());
        let block = app.partition_table_block().expect("0x99 blocks");
        assert!(block.contains("'0x99'"), "{block}");
        assert_eq!(
            app.partition_table_repair().as_deref(),
            Some("line 5: '0x99' -> 'undefined'")
        );

        assert!(app.repair_partition_table());
        assert!(!app.repair_partition_table(), "nothing left to change");
        assert_eq!(app.partition_table_block(), None, "flashable now");
        assert_eq!(app.partition_table_repair(), None);
        assert!(
            app.current_project_files()
                .partitions_csv
                .ends_with(FIXED_ROW),
            "the build copy follows the editor's"
        );
        assert_eq!(app.selected_file, ProjectFileId::PartitionsCsv);
        assert!(
            app.dfu_log
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("Save to keep it on disk")),
        );
    }

    /// Found by review: with partitions.csv open in the editor - the natural
    /// order, the dialog names line 5 - the editor's end-of-frame write-back
    /// put the text it read at the frame's start back over the repair.
    #[test]
    fn the_repair_survives_an_editor_already_showing_the_file() {
        let ctx = egui::Context::default();
        let mut app = esp_app(ctx.clone());
        app.selected_file = ProjectFileId::PartitionsCsv;
        app.build_tab = BuildPanelTab::Dfu;
        app.diag_collapsed = false;
        let frame = |app: &mut AppIde, pass: u64| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                time: Some(pass as f64 / 30.0),
                predicted_dt: 1.0 / 30.0,
                ..Default::default()
            };
            let _ = crate::headless::run_ui(&ctx, input, |ui| {
                let files = Some(app.current_project_files());
                app.show_editor_panel(ui, &files);
            });
        };
        for pass in 0..3 {
            frame(&mut app, pass);
        }
        assert!(
            app.partitions_csv.contains("0x99"),
            "the editor kept it as it was"
        );
        // The click, as the dialog sends it, lands in the next frame.
        crate::app::tabs::dfu_tab::request_partition_fix(&ctx);
        for pass in 3..6 {
            frame(&mut app, pass);
        }
        assert!(
            app.partitions_csv.ends_with(FIXED_ROW),
            "the write-back undid the repair:\n{}",
            app.partitions_csv
        );
        assert_eq!(app.partition_table_block(), None);
    }

    /// A block that is not the table's - the store's own settings here - gets
    /// no table repair, even with a 0x99 row in the table.
    #[test]
    fn no_table_repair_is_offered_for_a_block_that_is_not_the_table() {
        use crate::panels::mcu_module::flash_store::FlashStoreConfig;
        let mut app = esp_app(egui::Context::default());
        let bad = FlashStoreConfig {
            offset: 0,
            ..FlashStoreConfig::default_for("esp32c3")
        };
        if let Some(m) = &mut app.mcu {
            m.flash_store = Some(bad);
        }
        let block = app.partition_table_block().expect("blocked");
        assert!(block.starts_with("Flash store:"), "{block}");
        assert_eq!(app.partition_table_repair(), None);
    }
}
