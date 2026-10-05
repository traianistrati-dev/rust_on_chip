//! Bottom diagnostics panel — tabbed: Cargo Check | rust-analyzer | Flash | Tools.
//!
//! Orchestrator only: renders the tab-header buttons (with status badges) and
//! dispatches to the per-tab render functions in `super::tabs`.

use super::BuildPanelTab;
use super::tabs::{
    show_activity_tab, show_cargo_tab, show_clippy_tab, show_debug_tab, show_dfu_tab, show_git_tab,
    show_profile_tab, show_ra_tab, show_rtt_tab, show_serial_tab, show_terminal_tab,
    show_tools_tab,
};
use crate::activity::ActivityLog;
use crate::build::BuildState;
use crate::dfu::{self, DfuState};
use crate::espflash::EspFlashState;
use crate::lsp::{self, LspStatus};
use crate::openocd::OpenOcdState;
use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
use crate::probe_flash::ProbeFlashState;
use crate::required_tools;
use crate::serial::SerialMonitor;
use crate::terminal::TerminalConsole;
use eframe::egui;
use egui_phosphor::regular as ph;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(super) fn show_diag_panel(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    build_state: &Arc<Mutex<BuildState>>,
    lsp_state: &Arc<Mutex<lsp::LspState>>,
    dfu_state: &Arc<Mutex<DfuState>>,
    dfu_log: &Arc<Mutex<Vec<String>>>,
    dfu_programmers: &Arc<Mutex<HashMap<String, dfu::ProgrammerInfo>>>,
    dfu_sel_programmer: &mut String,
    // `None` on a chip with no USB DFU ROM bootloader - see `show_dfu_tab`.
    dfu_flash_addr: Option<&mut String>,
    openocd_state: &Arc<Mutex<OpenOcdState>>,
    openocd_target_cfg: &mut String,
    espflash_state: &Arc<Mutex<EspFlashState>>,
    espflash_port: &mut String,
    // The port the running `espflash` actually took (its own override, or the
    // one it auto-detected). Needed by the Serial tab: a serial port has one
    // owner, and espflash is the SECOND in-IDE holder after the Monitor.
    espflash_used_port: &Arc<Mutex<String>>,
    tools_state: &Arc<Mutex<required_tools::ToolsState>>,
    serial: &mut SerialMonitor,
    terminal: &mut TerminalConsole,
    activity: &Arc<Mutex<ActivityLog>>,
    clippy_state: &Arc<Mutex<BuildState>>,
    clippy_sel: &mut Option<usize>,
    // Set true when the user presses "Run clippy" (the caller starts the run).
    clippy_run: &mut bool,
    // Set to `Some(i)` to apply diagnostic `i`'s fix; `clippy_apply_all` applies
    // every machine-applicable suggestion. The caller performs the edits.
    clippy_apply_one: &mut Option<usize>,
    clippy_apply_all: &mut bool,
    // Set to `Some(i)` to project-wide-rename diagnostic `i`'s symbol (RA rename).
    clippy_apply_rename: &mut Option<usize>,
    // Byte ranges of main.rs's GENERATED block — "Fix" is disabled for fixes there.
    clippy_gen_ranges: &[(usize, usize)],
    toolchain: &ToolchainKind,
    tab: &mut BuildPanelTab,
    // Panel reduced to this tab bar: the header still renders, the content
    // below it doesn't. Toggled by the caret button right of "More".
    collapsed: &mut bool,
    // Set when ANY tab button is clicked — including one that was already
    // selected, so the caller can reopen a collapsed panel on any click.
    tab_clicked: &mut bool,
    cargo_sel: &mut Option<usize>,
    lsp_sel: &mut Option<usize>,
    // Diagnostic-row click target: `(rel_path, 1-based line, band colour)`; the
    // editor opens the file, scrolls to the line, and tints it.
    nav: &mut Option<(String, usize, egui::Color32)>,
    // Git tab: console state + the saved project dir; buttons set `git_op`
    // (the `clippy_run` signal pattern — the caller spawns the worker).
    git: &mut crate::git::GitConsole,
    project_dir: Option<&std::path::Path>,
    git_op: &mut Option<crate::git::GitOp>,
    // `(git path, 1-based line)` of an added diff row the user clicked to open
    // in the editor; the caller maps the path to a `ProjectFileId`, selects it
    // and scrolls to the line.
    git_open: &mut Option<(String, usize)>,
    // `(git path, hunk row index)` when the user clicks a hunk's revert button
    // in the diff view; the caller reverses just that hunk (Phase B).
    git_revert_hunk: &mut Option<(String, usize)>,
    // Git History view (read-only): a selected commit / one of its files.
    git_commit_load: &mut Option<String>,
    git_commit_file_load: &mut Option<(String, String)>,
    // `(sha, path)` when History's "Restore this file" is clicked.
    git_restore_from_commit: &mut Option<(String, String)>,
    // Sha when History's "Restore ALL files" is clicked.
    git_restore_all_from_commit: &mut Option<String>,
    // `(git path, is_untracked)` when the user clicks a file's discard button;
    // the caller confirms then restores/deletes the whole file (Phase A).
    git_discard: &mut Option<(String, bool)>,
    // True when the user clicks "Discard all"; the caller confirms then resets
    // the whole tree to HEAD (Phase C).
    git_discard_all: &mut bool,
    // Branch name the header picker asked to switch to.
    git_switch_branch: &mut Option<String>,
    // Branch name the picker's trash icon asked to delete.
    git_delete_branch: &mut Option<String>,
    // Workspace-member crate names — the Git tab's repository picker.
    git_libraries: &[String],
    // Flash-tab Programmer-row buttons: set `flash_scan`/`flash_go` on click;
    // `can_flash` = a buildable chip config exists (gates the Flash button).
    flash_scan: &mut bool,
    flash_go: &mut bool,
    // Set when the same button - reading "Stop Flash" while one runs - aborts
    // it; the caller stops whichever path the toolchain uses.
    flash_stop: &mut bool,
    // The running SWD / ESP child, so the Flash tab can stop `Read chip info`
    // too (same tool, same port, same blocked buttons).
    esp_flash_child: &crate::flash_stop::FlashHandle,
    can_flash: bool,
    // Cargo-tab Build button (moved off the top toolbar): set on click; the
    // caller runs `start_build`. Gated like Flash, on the same chip config.
    build_go: &mut bool,
    // Cargo-tab Size button: Flash/RAM usage measurement (state + signal; the
    // caller runs `start_size_measure`).
    size_state: &Arc<Mutex<crate::size::SizeState>>,
    size_go: &mut bool,
    // Flash-tab Size button — same measurement, but the caller keeps the Flash
    // tab in front instead of switching to Cargo.
    size_flash_go: &mut bool,
    // RTT tab: console + Run/Attach signal (caller runs `start_rtt`) + the
    // probe-rs chip name shown in the tab.
    rtt: &mut crate::rtt::RttConsole,
    rtt_go: &mut Option<crate::rtt::RttMode>,
    rtt_chip: &str,
    // ESP device console — rendered INSIDE the Flash tab (see `show_dfu_tab`),
    // beside the flash log. Start signal (caller runs `start_esp_monitor`) plus
    // the "open after flash" preference and its toggle.
    esp_monitor: &mut crate::esp_monitor::EspMonitor,
    esp_monitor_go: &mut bool,
    esp_monitor_auto: bool,
    esp_monitor_auto_set: &mut Option<bool>,

    // Debug tab: session + Start signal (caller runs `start_debug`).
    debugger: &mut crate::debugger::Debugger,
    debug_go: &mut bool,
    // Debug tab's "Reset target" button (caller runs `probe::start_reset`).
    reset_go: &mut bool,
    // Debug tab's breakpoint list: every breakpoint (rel path → 1-based lines)
    // and the row the user clicked — the caller opens it in the editor. The
    // row's ✕ raises `bp_remove`, "Remove all" raises `bp_clear`; the caller
    // owns the map and re-syncs a live session.
    breakpoints: &std::collections::BTreeMap<String, std::collections::BTreeSet<u32>>,
    bp_jump: &mut Option<(String, u32)>,
    bp_remove: &mut Option<(String, u32)>,
    bp_clear: &mut bool,
    // Debug tab's "Debug-friendly build" toggle: the project's current setting
    // and the value the user picked (the caller applies it to the Mcu).
    debug_build: bool,
    debug_build_set: &mut Option<bool>,
    // Shared probe selector (RTT + Debug): scanned probe list, chosen `--probe`
    // selector, a "scan" click signal (caller runs `scan_probes`), last error.
    probe_list: &[crate::probe::ProbeInfo],
    selected_probe: &mut Option<String>,
    probe_scan: &mut bool,
    probe_scan_err: Option<&str>,
    // Profile tab: static-vs-runtime mode; cargo-bloat state + per-crate toggle +
    // "Analyze" signal; the flamegraph state + a "Sample" signal (caller runs
    // `start_profile` / `start_flame`).
    profile_mode: &mut crate::profile::ProfileMode,
    profile_state: &Arc<Mutex<crate::profile::ProfileState>>,
    profile_by_crate: &mut bool,
    profile_run: &mut bool,
    flame_state: &Arc<Mutex<crate::flamegraph::FlameState>>,
    profile_sample: &mut bool,
    // Flash tab's probe-rs path (shared probe): status + "Flash (probe-rs)" signal.
    probe_flash_state: &Arc<Mutex<crate::probe_flash::ProbeFlashState>>,
    probe_flash_go: &mut bool,
    // Set when the Flash button (in its "Stop Flash" state) aborts a run.
    probe_flash_stop: &mut bool,
    // Tools confirmed missing (startup self-check) — buttons that shell out to
    // one of them are greyed out with a "install it in Tools" hint. Empty while
    // the check hasn't proven a problem, so the UI stays permissive.
    missing_tools: &[&'static str],
    // Why the project must not be flashed as it stands: the FPGA bitstream it
    // would embed (`fpga_bitstream::preflight`), or a partition table espflash
    // would crash on or that does not reserve the flash store
    // (`AppIde::partition_table_block`). `None` when nothing is wrong.
    flash_block: Option<&str>,
) {
    // ── Tab header ────────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        // Cargo tab button
        {
            let st = build_state.lock().unwrap();
            let (badge, col) = match &*st {
                BuildState::Done(r) if r.error_count() > 0 => (
                    format!(" {} {}", r.error_count(), ph::X_CIRCLE),
                    egui::Color32::from_rgb(220, 80, 70),
                ),
                BuildState::Done(r) if r.warning_count() > 0 => (
                    format!(" {} {}", r.warning_count(), ph::WARNING),
                    egui::Color32::from_rgb(210, 170, 40),
                ),
                BuildState::Done(r) if r.success => (
                    format!(" {}", ph::CHECK_CIRCLE),
                    egui::Color32::from_rgb(80, 200, 100),
                ),
                BuildState::Building => (" …".to_owned(), egui::Color32::GRAY),
                _ => (String::new(), egui::Color32::GRAY),
            };
            let label = format!("{} Cargo Check{badge}", ph::HAMMER);
            let active = *tab == BuildPanelTab::Cargo;
            let btn = ui.add(
                egui::Button::new(egui::RichText::new(&label).size(11.0).color(if active {
                    egui::Color32::WHITE
                } else {
                    col
                }))
                .frame(active),
            );
            if btn.clicked() {
                *tab = BuildPanelTab::Cargo;
                *tab_clicked = true;
            }
        }

        ui.separator();

        // RA tab button
        {
            let lsp = lsp_state.lock().unwrap();
            let (badge, col) = match &lsp.status {
                LspStatus::Starting | LspStatus::Indexing => {
                    (" …".to_owned(), egui::Color32::from_rgb(180, 180, 80))
                }
                LspStatus::Ready if lsp.total_errors() > 0 => (
                    format!(" {} {}", lsp.total_errors(), ph::X_CIRCLE),
                    egui::Color32::from_rgb(220, 80, 70),
                ),
                LspStatus::Ready if lsp.total_warnings() > 0 => (
                    format!(" {} {}", lsp.total_warnings(), ph::WARNING),
                    egui::Color32::from_rgb(210, 170, 40),
                ),
                LspStatus::Ready => (
                    format!(" {}", ph::CHECK_CIRCLE),
                    egui::Color32::from_rgb(80, 200, 100),
                ),
                LspStatus::Failed(_) => (
                    format!(" {}", ph::X_CIRCLE),
                    egui::Color32::from_rgb(220, 80, 70),
                ),
                _ => (String::new(), egui::Color32::DARK_GRAY),
            };
            let label = format!("Analyzer{badge}");
            let active = *tab == BuildPanelTab::RustAnalyzer;
            let btn = ui.add(
                egui::Button::new(egui::RichText::new(&label).size(11.0).color(if active {
                    egui::Color32::WHITE
                } else {
                    col
                }))
                .frame(active),
            );
            if btn.clicked() {
                *tab = BuildPanelTab::RustAnalyzer;
                *tab_clicked = true;
            }
        }

        ui.separator();

        // Clippy tab button
        {
            let active = *tab == BuildPanelTab::Clippy;
            let cs = clippy_state.lock().unwrap();
            let (badge, col) = match &*cs {
                BuildState::Building => (" …".to_owned(), egui::Color32::GRAY),
                BuildState::Done(r) if !r.diagnostics.is_empty() => (
                    format!(" {} {}", r.diagnostics.len(), ph::LIGHTBULB),
                    egui::Color32::from_rgb(210, 170, 40),
                ),
                BuildState::Done(_) => (
                    format!(" {}", ph::CHECK_CIRCLE),
                    egui::Color32::from_rgb(80, 200, 100),
                ),
                BuildState::Failed(_) => (
                    format!(" {}", ph::X_CIRCLE),
                    egui::Color32::from_rgb(220, 80, 70),
                ),
                BuildState::Idle => (String::new(), egui::Color32::DARK_GRAY),
            };
            drop(cs);
            let label = format!("{} Clippy{badge}", ph::SPARKLE);
            let btn = ui.add(
                egui::Button::new(egui::RichText::new(&label).size(11.0).color(if active {
                    egui::Color32::WHITE
                } else {
                    col
                }))
                .frame(active),
            );
            if btn.clicked() {
                *tab = BuildPanelTab::Clippy;
                *tab_clicked = true;
            }
        }

        ui.separator();

        // Profile tab button (cargo bloat code-size breakdown).
        {
            let active = *tab == BuildPanelTab::Profile;
            let (badge, col) = match &*profile_state.lock().unwrap() {
                crate::profile::ProfileState::Running => (" …".to_owned(), egui::Color32::GRAY),
                crate::profile::ProfileState::Done(_) => (
                    format!(" {}", ph::CHECK_CIRCLE),
                    egui::Color32::from_rgb(80, 200, 100),
                ),
                crate::profile::ProfileState::Failed(_) => (
                    format!(" {}", ph::X_CIRCLE),
                    egui::Color32::from_rgb(220, 80, 70),
                ),
                crate::profile::ProfileState::Idle => (String::new(), egui::Color32::DARK_GRAY),
            };
            let label = format!("{} Profile{badge}", ph::CHART_BAR);
            let btn = ui.add(
                egui::Button::new(egui::RichText::new(&label).size(11.0).color(if active {
                    egui::Color32::WHITE
                } else {
                    col
                }))
                .frame(active),
            );
            if btn.clicked() {
                *tab = BuildPanelTab::Profile;
                *tab_clicked = true;
            }
        }

        ui.separator();

        // Git tab button (commit/push/pull in the project directory).
        {
            let active = *tab == BuildPanelTab::Git;
            let (busy, n) = {
                let st = git.state.lock().unwrap();
                (st.busy.is_some(), st.status.changes.len())
            };
            let badge = if busy {
                " …".to_owned()
            } else if n > 0 {
                format!(" {n}")
            } else {
                String::new()
            };
            let col = if busy {
                egui::Color32::from_rgb(220, 180, 70)
            } else {
                egui::Color32::GRAY
            };
            let btn = ui.add(
                egui::Button::new(
                    egui::RichText::new(format!("{} Git{badge}", ph::GIT_BRANCH))
                        .size(11.5)
                        .color(col),
                )
                .frame(active),
            );
            if btn.clicked() {
                *tab = BuildPanelTab::Git;
                *tab_clicked = true;
            }
        }

        ui.separator();

        // Flash tab button — badge reflects whichever flash operation is active
        {
            let dfu = dfu_state.lock().unwrap();
            let ocd = openocd_state.lock().unwrap();
            let esp = espflash_state.lock().unwrap();
            // probe-rs too: `cargo flash` builds and flashes in one `Flashing`
            // state, so a probe-rs run shows the flashing color throughout.
            let probe = probe_flash_state.lock().unwrap();
            let any_busy = dfu.is_busy() || ocd.is_busy() || esp.is_busy() || probe.is_busy();
            let any_success = matches!(*dfu, DfuState::Success)
                || matches!(*ocd, OpenOcdState::Success)
                || matches!(*esp, EspFlashState::Success)
                || matches!(*probe, ProbeFlashState::Success);
            let any_error = matches!(*dfu, DfuState::Error(_))
                || matches!(*ocd, OpenOcdState::Error(_))
                || matches!(*esp, EspFlashState::Error(_))
                || matches!(*probe, ProbeFlashState::Error(_));
            let (badge, col) = if any_busy {
                if matches!(*dfu, DfuState::Flashing)
                    || matches!(*ocd, OpenOcdState::Flashing)
                    || matches!(*esp, EspFlashState::Flashing)
                    || matches!(*probe, ProbeFlashState::Flashing)
                {
                    (" …".to_owned(), egui::Color32::from_rgb(100, 180, 255))
                } else {
                    (" …".to_owned(), egui::Color32::from_rgb(220, 180, 60))
                }
            } else if any_success {
                (
                    format!(" {}", ph::CHECK_CIRCLE),
                    egui::Color32::from_rgb(80, 200, 100),
                )
            } else if any_error {
                (
                    format!(" {}", ph::X_CIRCLE),
                    egui::Color32::from_rgb(220, 80, 70),
                )
            } else {
                (String::new(), egui::Color32::DARK_GRAY)
            };
            drop(probe);
            drop(esp);
            drop(ocd);
            drop(dfu);
            let label = format!("{} Flash{badge}", ph::LIGHTNING);
            let active = *tab == BuildPanelTab::Dfu;
            let btn = ui.add(
                egui::Button::new(egui::RichText::new(&label).size(11.0).color(if active {
                    egui::Color32::WHITE
                } else {
                    col
                }))
                .frame(active),
            );
            if btn.clicked() {
                *tab = BuildPanelTab::Dfu;
                *tab_clicked = true;
            }
        }

        // RTT / defmt tab button (streaming badge while a session is live).
        {
            let active = *tab == BuildPanelTab::Rtt;
            let (badge, col) = match rtt.phase() {
                crate::rtt::RttPhase::Streaming => (
                    format!(" {}", ph::BROADCAST),
                    egui::Color32::from_rgb(80, 200, 100),
                ),
                crate::rtt::RttPhase::Building => (" …".to_owned(), egui::Color32::GRAY),
                crate::rtt::RttPhase::Error(_) => (
                    format!(" {}", ph::X_CIRCLE),
                    egui::Color32::from_rgb(220, 80, 70),
                ),
                crate::rtt::RttPhase::Idle => (String::new(), egui::Color32::DARK_GRAY),
            };
            let label = format!("{} RTT{badge}", ph::BROADCAST);
            let btn = ui.add(
                egui::Button::new(egui::RichText::new(&label).size(11.0).color(if active {
                    egui::Color32::WHITE
                } else {
                    col
                }))
                .frame(active),
            );
            if btn.clicked() {
                *tab = BuildPanelTab::Rtt;
                *tab_clicked = true;
            }
        }

        // Debug tab button (state badge while a session is live).
        {
            let active = *tab == BuildPanelTab::Debug;
            use crate::debugger::DebugPhase;
            let (badge, col) = match debugger.phase() {
                DebugPhase::Stopped(_) => (
                    format!(" {}", ph::PAUSE),
                    egui::Color32::from_rgb(230, 180, 60),
                ),
                DebugPhase::Running => (
                    format!(" {}", ph::PLAY),
                    egui::Color32::from_rgb(80, 200, 100),
                ),
                DebugPhase::Building | DebugPhase::Launching | DebugPhase::Stopping => {
                    (" …".to_owned(), egui::Color32::GRAY)
                }
                DebugPhase::Error(_) => (
                    format!(" {}", ph::X_CIRCLE),
                    egui::Color32::from_rgb(220, 80, 70),
                ),
                DebugPhase::Idle => (String::new(), egui::Color32::DARK_GRAY),
            };
            let label = format!("{} Debug{badge}", ph::BUG);
            let btn = ui.add(
                egui::Button::new(egui::RichText::new(&label).size(11.0).color(if active {
                    egui::Color32::WHITE
                } else {
                    col
                }))
                .frame(active),
            );
            if btn.clicked() {
                *tab = BuildPanelTab::Debug;
                *tab_clicked = true;
            }
        }

        ui.separator();

        // Serial monitor tab button
        {
            let active = *tab == BuildPanelTab::Serial;
            let (badge, col) = if serial.is_connected() {
                (
                    format!(" {}", ph::PLUGS_CONNECTED),
                    egui::Color32::from_rgb(80, 200, 100),
                )
            } else {
                (String::new(), egui::Color32::DARK_GRAY)
            };
            let label = format!("{} Serial{badge}", ph::TERMINAL);
            let btn = ui.add(
                egui::Button::new(egui::RichText::new(&label).size(11.0).color(if active {
                    egui::Color32::WHITE
                } else {
                    col
                }))
                .frame(active),
            );
            if btn.clicked() {
                *tab = BuildPanelTab::Serial;
                *tab_clicked = true;
            }
        }

        // (The F12 "Definition" tab moved to the MCU Configurator on
        // 2026-07-10 — see `AppIde::show_definition_tab`.)

        // ── "More" dropdown (right-aligned) — groups the auxiliary panels
        //    Terminal / Activity / Tools so the main tab bar stays compact. ──
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Collapse / expand toggle. Added FIRST because this layout is
            // right-to-left — the first widget sits furthest right, i.e. to the
            // right of the "More" dropdown below.
            let (icon, tip) = if *collapsed {
                (
                    ph::CARET_DOUBLE_UP,
                    "Expand the panel — show the selected tab's content again.\n\
                     Clicking any tab above expands it too.",
                )
            } else {
                (
                    ph::CARET_DOWN,
                    "Collapse the panel — keep only this tab bar and give the \
                     space back to the editor.\n\
                     The bar stays visible; click any tab to reopen.",
                )
            };
            // Same text size as the "More" button beside it: a button is as
            // tall as its content once that content passes the standard row
            // height, so a 12 pt icon next to an 11 pt label left the two boxes
            // visibly different heights on the collapsed bar.
            if ui
                .button(
                    egui::RichText::new(icon)
                        .size(11.0)
                        .color(egui::Color32::from_rgb(160, 185, 215)),
                )
                .on_hover_text(tip)
                .clicked()
            {
                *collapsed = !*collapsed;
            }

            let grouped = matches!(
                *tab,
                BuildPanelTab::Terminal | BuildPanelTab::Activity | BuildPanelTab::RequiredTools
            );
            let running = terminal.is_running();
            let acts = activity.lock().unwrap().actions.len();
            let (missing, tools_busy) = {
                let ts = tools_state.lock().unwrap();
                (ts.missing_installable_count(), ts.any_busy())
            };
            // Label shows the active grouped tab (so it's obvious which is
            // selected), else "More"; a caret hints at the dropdown.
            let name = match *tab {
                BuildPanelTab::Terminal => "Terminal",
                BuildPanelTab::Activity => "Activity",
                BuildPanelTab::RequiredTools => "Tools",
                _ => "More",
            };
            // A badge when a grouped panel wants attention while NOT selected.
            let attention = (running && *tab != BuildPanelTab::Terminal)
                || (missing > 0 && *tab != BuildPanelTab::RequiredTools);
            let col = if grouped {
                egui::Color32::WHITE
            } else if attention {
                egui::Color32::from_rgb(230, 160, 50)
            } else {
                egui::Color32::from_rgb(160, 185, 215)
            };
            let hint = if running {
                " …".to_owned()
            } else if missing > 0 {
                format!(" {missing} {}", ph::WARNING)
            } else {
                String::new()
            };
            ui.menu_button(
                egui::RichText::new(format!("{name}{hint} {}", ph::CARET_DOWN))
                    .size(11.0)
                    .color(col),
                |ui| {
                    let term_badge = if running { " …" } else { "" };
                    if ui
                        .selectable_label(
                            *tab == BuildPanelTab::Terminal,
                            format!("{} Terminal{term_badge}", ph::TERMINAL_WINDOW),
                        )
                        .clicked()
                    {
                        *tab = BuildPanelTab::Terminal;
                        *tab_clicked = true;
                        ui.close();
                    }
                    let act_badge = if acts > 0 {
                        format!(" {acts}")
                    } else {
                        String::new()
                    };
                    if ui
                        .selectable_label(
                            *tab == BuildPanelTab::Activity,
                            format!("{} Activity{act_badge}", ph::TIMER),
                        )
                        .clicked()
                    {
                        *tab = BuildPanelTab::Activity;
                        *tab_clicked = true;
                        ui.close();
                    }
                    let tool_badge = if tools_busy {
                        " …".to_owned()
                    } else if missing > 0 {
                        format!(" {missing} {}", ph::WARNING)
                    } else {
                        String::new()
                    };
                    if ui
                        .selectable_label(
                            *tab == BuildPanelTab::RequiredTools,
                            format!("{} Tools{tool_badge}", ph::WRENCH),
                        )
                        .clicked()
                    {
                        *tab = BuildPanelTab::RequiredTools;
                        *tab_clicked = true;
                        ui.close();
                    }
                },
            );
        });
    });

    // Collapsed: the tab bar above is the whole panel — stop before the
    // content. (The caller sizes the panel to just this row.)
    if *collapsed {
        return;
    }

    ui.separator();

    // ── Tab content ───────────────────────────────────────────────────────────
    // Who inside the IDE holds the device, answered ONCE for every tab that
    // needs it. This used to live inside the Serial branch alone, so the Flash
    // tab's Scan reported an empty bench while the IDE itself was the reason -
    // see `esp_monitor::port_holder`.
    let flashing = matches!(
        *espflash_state.lock().unwrap(),
        EspFlashState::Flashing | EspFlashState::ReadingInfo
    );
    let esp_port = if flashing {
        espflash_used_port.lock().unwrap().clone()
    } else {
        String::new()
    };
    let monitor_port = esp_monitor.active_port();
    let holder = crate::esp_monitor::port_holder(&esp_port, &monitor_port);

    match tab {
        BuildPanelTab::Cargo => {
            let clippy_running = clippy_state.lock().unwrap().is_building();
            show_cargo_tab(
                ui,
                ctx,
                build_state,
                cargo_sel,
                nav,
                build_go,
                can_flash, // same gate: a buildable chip config exists
                clippy_running,
                size_state,
                size_go,
            );
        }
        BuildPanelTab::RustAnalyzer => {
            show_ra_tab(ui, lsp_state, lsp_sel, nav);
        }
        BuildPanelTab::Dfu => {
            show_dfu_tab(
                ui,
                dfu_state,
                dfu_log,
                dfu_programmers,
                dfu_sel_programmer,
                dfu_flash_addr,
                openocd_state,
                openocd_target_cfg,
                espflash_state,
                espflash_used_port,
                espflash_port,
                toolchain,
                flash_scan,
                flash_go,
                flash_stop,
                esp_flash_child,
                can_flash,
                size_state,
                size_flash_go,
                probe_list,
                selected_probe,
                probe_scan,
                probe_scan_err,
                probe_flash_state,
                probe_flash_go,
                probe_flash_stop,
                // A live probe-rs session owns the probe exclusively — the Flash
                // buttons go red rather than failing with "probe in use".
                if debugger.is_busy() {
                    Some("Debug")
                } else if rtt.is_busy() {
                    Some("RTT")
                } else if flame_state.lock().unwrap().is_busy() {
                    Some("Profile sampling")
                } else {
                    None
                },
                esp_monitor,
                esp_monitor_go,
                esp_monitor_auto,
                esp_monitor_auto_set,
                serial.is_connected().then(|| serial.port.as_str()),
                missing_tools,
                holder,
                flash_block,
            );
        }
        BuildPanelTab::Rtt => {
            show_rtt_tab(
                ui,
                rtt,
                rtt_go,
                can_flash,
                rtt_chip,
                probe_list,
                selected_probe,
                probe_scan,
                probe_scan_err,
                toolchain,
                missing_tools,
                holder,
            );
        }
        BuildPanelTab::Debug => {
            show_debug_tab(
                ui,
                debugger,
                debug_go,
                reset_go,
                breakpoints,
                bp_jump,
                bp_remove,
                bp_clear,
                debug_build,
                debug_build_set,
                can_flash,
                rtt_chip,
                probe_list,
                selected_probe,
                probe_scan,
                probe_scan_err,
                toolchain,
                missing_tools,
                holder,
            );
        }
        BuildPanelTab::Serial => {
            show_serial_tab(ui, serial, ctx, holder);
        }
        BuildPanelTab::Terminal => {
            show_terminal_tab(ui, terminal, ctx);
        }
        BuildPanelTab::Activity => {
            show_activity_tab(ui, activity);
        }
        BuildPanelTab::Git => {
            show_git_tab(
                ui,
                git,
                project_dir,
                git_op,
                git_open,
                git_revert_hunk,
                git_commit_load,
                git_commit_file_load,
                git_restore_from_commit,
                git_restore_all_from_commit,
                git_discard,
                git_discard_all,
                git_switch_branch,
                git_delete_branch,
                git_libraries,
            );
        }
        BuildPanelTab::Clippy => {
            let build_busy = build_state.lock().unwrap().is_building();
            show_clippy_tab(
                ui,
                clippy_state,
                build_busy,
                clippy_sel,
                nav,
                clippy_run,
                clippy_apply_one,
                clippy_apply_all,
                clippy_apply_rename,
                clippy_gen_ranges,
            );
        }
        BuildPanelTab::Profile => {
            show_profile_tab(
                ui,
                profile_mode,
                profile_state,
                profile_by_crate,
                profile_run,
                flame_state,
                profile_sample,
                rtt_chip,
                can_flash,
                probe_list,
                selected_probe,
                probe_scan,
                probe_scan_err,
                toolchain,
                missing_tools,
                holder,
            );
        }
        BuildPanelTab::RequiredTools => {
            show_tools_tab(ui, tools_state, ctx);
        }
    }
}
