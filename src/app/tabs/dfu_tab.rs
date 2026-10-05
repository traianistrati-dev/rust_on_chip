//! DFU and Flash programming tab.
use super::cargo_tab::render_size_bar;
use crate::dfu::{self, DfuState};
use crate::espflash::{self, EspFlashState};
use crate::openocd::{self, OpenOcdState};
use crate::panels::mcu_module::ToolchainKind;
use crate::size::SizeState;
use eframe::egui;
use egui_phosphor::regular as ph;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
pub fn show_dfu_tab(
    ui: &mut egui::Ui,
    dfu_state: &Arc<Mutex<DfuState>>,
    dfu_log: &Arc<Mutex<Vec<String>>>,
    dfu_programmers: &Arc<Mutex<HashMap<String, dfu::ProgrammerInfo>>>,
    dfu_sel_programmer: &mut String,
    // `None` when the chip has no USB DFU ROM bootloader (see
    // `dfu::has_usb_dfu_bootloader`). The DFU config is then left off the row
    // below: a flash address the part does not have, and the phases of a run
    // its ROM cannot start.
    dfu_flash_addr: Option<&mut String>,
    openocd_state: &Arc<Mutex<OpenOcdState>>,
    openocd_target_cfg: &mut String,
    espflash_state: &Arc<Mutex<EspFlashState>>,
    // The port espflash is on right now, written by BOTH espflash paths -
    // the flash and `read_board_info`. `diag_panel` reads it to name the
    // holder for the Serial tab.
    espflash_used_port: &Arc<Mutex<String>>,
    //espflash_port: &mut String,
    _: &mut String,
    toolchain: &ToolchainKind,
    // Set true when the user clicks Scan / Flash on the Programmer row; the
    // caller runs `AppIde::scan_usb` / `flash_swd` / `flash_esp`. `can_flash`
    // gates the Flash button (a buildable chip config exists).
    scan_out: &mut bool,
    flash_out: &mut bool,
    // Set when that same button - which reads "Stop Flash" while a flash runs -
    // is clicked to abort it. One flag for both paths: the toolchain decides
    // which one can be running, exactly as it does for `flash_out`.
    flash_stop: &mut bool,
    // The running SWD / ESP child, so `Read chip info` can be stopped too: it
    // runs espflash against the same port and blocks the same buttons.
    esp_flash_child: &crate::flash_stop::FlashHandle,
    can_flash: bool,
    // Flash/RAM usage: the row is rendered under the Programmer row and is
    // refreshed automatically after every flash. `size_out` = the manual Size
    // button (the caller runs `start_size_measure_quiet`, which keeps this tab
    // in front instead of jumping to Cargo).
    size_state: &Arc<Mutex<SizeState>>,
    size_out: &mut bool,
    // Shared probe-rs probe (same as Debug / RTT / Runtime) + its `cargo flash`
    // path: `probe_flash_out` fires the flash, `probe_scan` re-runs `probe-rs
    // list`, `probe_flash_state` is the status.
    probe_list: &[crate::probe::ProbeInfo],
    selected_probe: &mut Option<String>,
    probe_scan: &mut bool,
    probe_scan_err: Option<&str>,
    probe_flash_state: &Arc<Mutex<crate::probe_flash::ProbeFlashState>>,
    probe_flash_out: &mut bool,
    // Set when the Flash button — which becomes "Stop Flash" while one runs —
    // is clicked to abort it.
    probe_flash_stop: &mut bool,
    // Which probe-rs session currently OWNS the probe ("Debug", "RTT", …), if
    // any. Only one process can hold it, so every flasher is blocked while one
    // runs — and the button says so instead of failing deep inside the tool.
    probe_holder: Option<&'static str>,
    // ESP device console (`espflash monitor`) — lives in THIS tab, beside the
    // flash log, because "flash it and see what it prints" is one action to the
    // user. `monitor_go` fires a manual Start (the caller runs
    // `start_esp_monitor`); `monitor_auto` + its setter are the "open it after
    // every flash" preference.
    esp_monitor: &mut crate::esp_monitor::EspMonitor,
    monitor_go: &mut bool,
    monitor_auto: bool,
    monitor_auto_set: &mut Option<bool>,
    // Port the Serial tab holds, if connected — a serial port is exclusive, so
    // the monitor cannot have it at the same time.
    serial_port_held: Option<&str>,
    // Tools confirmed missing — buttons needing one are greyed out with a
    // "install it in Tools" hint (see `super::tool_missing`).
    missing_tools: &[&'static str],
    // Who inside the IDE holds the serial DEVICE, if anyone, as (port, holder).
    //
    // Distinct from `probe_holder` above, which is about a probe-rs session
    // owning the PROBE. This one is the espflash/Monitor pair owning the port -
    // and it is why `probe-rs list` can come back empty on a board that is
    // plugged in and working. See `tabs::no_probe_message`.
    holder: Option<(&str, &str)>,
    // Why the project must not be flashed as it stands, if it must not: an
    // FPGA bitstream the board would reject, or a partition table that is
    // wrong. Each source only arises on its own boards (FPGA loaders, ESP), so
    // every flash button can read the one value.
    flash_block: Option<&str>,
    // What "Fix partitions.csv" would change, when the block is a table
    // espflash panics on (`AppIde::partition_table_repair`). The click comes
    // back through `take_partition_fix_request`.
    table_fix: Option<&str>,
) {
    let state = dfu_state.lock().unwrap().clone();
    let ocd_state = openocd_state.lock().unwrap().clone();
    let esp_state = espflash_state.lock().unwrap().clone();
    let log = dfu_log.lock().unwrap().clone();
    let progs = dfu_programmers.lock().unwrap().clone();
    // Read once here: the ESP phase widgets moved up to the probe row, above
    // the config row where this used to be computed.
    let build_done = log.iter().any(|l| l.contains("[OK] Build OK"));

    // Sort programmers: compatible with selected toolchain first, then by name
    // progs.sort_by(|a, b| {
    //     let a_is_stm = matches!(
    //         a.kind,
    //         "DFU Bootloader" | "ST-Link" | "J-Link" | "CMSIS-DAP"
    //     );
    //     let a_is_esp = matches!(a.kind, "USB-Serial" | "ESP32");
    //     let b_is_stm = matches!(
    //         b.kind,
    //         "DFU Bootloader" | "ST-Link" | "J-Link" | "CMSIS-DAP"
    //     );
    //     let b_is_esp = matches!(b.kind, "USB-Serial" | "ESP32");

    //     let a_compatible = match toolchain {
    //         ToolchainKind::RustEmbedded => a_is_stm,
    //         ToolchainKind::EspRust => a_is_esp,
    //         ToolchainKind::SdccC => false,
    //     };
    //     let b_compatible = match toolchain {
    //         ToolchainKind::RustEmbedded => b_is_stm,
    //         ToolchainKind::EspRust => b_is_esp,
    //         ToolchainKind::SdccC => false,
    //     };

    //     // Compatible first, then by name
    //     b_compatible
    //         .cmp(&a_compatible)
    //         .then_with(|| a.name.cmp(&b.name))
    // });

    // Determine selected programmer kind for adaptive config UI
    let sel_kind = progs
        .get(dfu_sel_programmer)
        .map(|p| p.kind.clone())
        .unwrap_or("".to_string());
    let is_swd = matches!(sel_kind.as_str(), "ST-Link" | "J-Link" | "CMSIS-DAP");
    let interface_cfg = openocd::interface_cfg_for_kind(&sel_kind);

    let dfu_busy = state.is_busy();
    let any_busy = dfu_busy || ocd_state.is_busy() || esp_state.is_busy();
    let size_snapshot = size_state.lock().unwrap().clone();
    let size_busy = size_snapshot.is_busy();

    // ── What blocks each Flash button right now ──────────────────────────────
    // Computed before the rows: the same sentence colours the button red, fills
    // its disabled hover, and is listed as a notice below — a greyed button that
    // won't say why is the thing this replaces.
    let pf_state = probe_flash_state.lock().unwrap().clone();
    // One process at a time owns a probe. A live Debug / RTT / sampling session
    // makes every flasher fail deep inside the tool with "probe in use".
    let held = |needs: &str| {
        probe_holder.map(|h| {
            format!("the {h} session is holding the probe — stop it first, {needs} needs exclusive access")
        })
    };
    let no_cfg = || {
        (!can_flash)
            .then(|| "no buildable chip configuration yet — set the MCU up first".to_owned())
    };
    let busy_note = || any_busy.then(|| "another flash is already running".to_owned());
    let blocked = || flash_block.map(str::to_owned);
    let swd_reason: Option<String> = held("OpenOCD")
        .or_else(no_cfg)
        .or_else(blocked)
        .or_else(busy_note)
        .or_else(|| {
            (!is_swd).then(|| {
                "pick an ST-Link / J-Link / CMSIS-DAP in the Programmer list (Scan USB finds them)"
                    .to_owned()
            })
        });
    let esp_reason: Option<String> = super::tool_missing(missing_tools, "espflash")
        .then(|| super::needs_tool_hint("espflash"))
        .or_else(no_cfg)
        .or_else(blocked)
        .or_else(busy_note);
    let probe_reason: Option<String> = super::tool_missing(missing_tools, "probe-rs")
        .then(|| super::needs_tool_hint("probe-rs"))
        .or_else(|| held("cargo flash"))
        .or_else(no_cfg)
        .or_else(blocked)
        // No auto-select on this path: `cargo flash` with an ambiguous probe
        // doesn't error, it waits — so the choice is made here, up front.
        .or_else(|| {
            selected_probe.is_none().then(|| {
                "pick a probe in the list — flashing does not auto-select, because with \
                 several probes attached it would sit there waiting instead of failing"
                    .to_owned()
            })
        })
        .or_else(busy_note)
        .or_else(|| {
            pf_state
                .is_busy()
                .then(|| "a probe-rs flash is running".to_owned())
        });

    // ── Two rows: [Scan] <device list> [Flash …] | usage bar + button ────────
    // Both rows use the same fixed widths, so Scan, the list and the Flash
    // button line up in three columns.
    let panel_w = ui.available_width();
    let combo_w = combo_width(panel_w);
    // On a narrow panel the usage bars + Size/Info don't fit beside the rows;
    // they get a wrapped row of their own underneath instead of being clipped.
    let compact = right_block_w(panel_w) <= 0.0;

    // Row 1 — the USB programmer (OpenOCD SWD / DFU / espflash) + Flash usage.
    split_row(
        ui,
        |ui| {
            programmer_combo(ui, combo_w, &progs, dfu_sel_programmer, toolchain);

            // Scan USB (detect DFU / ST-Link / J-Link / CMSIS-DAP / USB-serial).
            // Placed between the list and Flash: those two are used together,
            // and starting the row with Scan meant dragging the pointer across
            // the whole list to reach Flash.
            if ui
                .add_enabled(
                    !dfu_busy,
                    egui::Button::new(
                        egui::RichText::new(format!("{} Scan", ph::MAGNIFYING_GLASS)).size(10.5),
                    )
                    .min_size(egui::vec2(SCAN_W, ROW_H)),
                )
                .on_hover_text(
                    "Scan for connected USB programmers:\n\
                     - DFU bootloader (STM32 with BOOT0 = 1)\n\
                     - ST-Link / J-Link / CMSIS-DAP\n\
                     - USB-Serial (ESP32-C3, ...)",
                )
                .clicked()
            {
                *scan_out = true;
            }

            // Toolchain-specific Flash button.
            match toolchain {
                ToolchainKind::RustEmbedded => {
                    // While it runs, the same button STOPS it - see
                    // `stop_flash_button`.
                    if ocd_state.is_busy() {
                        if stop_flash_button(
                            ui,
                            "Kill the running SWD flash - the `cargo build` or openocd itself,\n\
                             with its children, so the ST-Link is released.",
                        ) {
                            *flash_stop = true;
                        }
                    } else if flash_button(
                        ui,
                        "Flash SWD",
                        format!("{} Flash SWD", ph::LIGHTNING),
                        swd_reason.as_deref(),
                        "Build --release, then program via SWD (OpenOCD).\n\
                         Needs: OpenOCD in PATH, a selected ST-Link/J-Link/CMSIS-DAP,\n\
                         the target .cfg below, and SWDIO + SWCLK + GND wiring.",
                    ) {
                        *flash_out = true;
                    }
                }
                ToolchainKind::EspRust => {
                    if esp_state.is_busy() {
                        if stop_flash_button(
                            ui,
                            "Kill the running espflash - the `cargo build`, the flash itself\n\
                             or a chip-info read - so the serial port is released.",
                        ) {
                            *flash_stop = true;
                        }
                    } else if flash_button(
                        ui,
                        "Flash ESP32",
                        format!("{} Flash ESP32", ph::LIGHTNING),
                        esp_reason.as_deref(),
                        "Build --release, then flash via espflash.\n\
                         Needs: espflash in PATH, the ESP32 in download mode\n\
                         (hold BOOT -> press RESET -> release BOOT).",
                    ) {
                        *flash_out = true;
                    }
                }
                ToolchainKind::SdccC => {}
            }
        },
        // Right block is laid out RIGHT to left: the button first, so it lands
        // on the panel edge; the bar then fills what is left, left to right.
        // The right block holds ONLY the usage bar: its buttons moved down to
        // the config row (next to Clear), where they don't share a clip region
        // with a bar that grows with the numbers in it.
        |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                render_size_bar(ui, &size_snapshot, true);
            });
        },
    );

    // Row 2 — the probe-rs probe (shared with Debug / RTT) + RAM usage.
    split_row(
        ui,
        |ui| {
            if matches!(toolchain, ToolchainKind::RustEmbedded) {
                super::probe_selector_ui_with(
                    ui,
                    "",
                    probe_list,
                    selected_probe,
                    probe_scan,
                    probe_scan_err,
                    toolchain,
                    SCAN_W,
                    combo_w,
                    false, // no Auto here — see `allow_auto`
                    holder,
                    |ui| {
                        // While it runs, the same button STOPS it. A flash that
                        // hangs (an ambiguous probe, a target that won't halt)
                        // otherwise leaves killing the IDE as the only way out.
                        if pf_state.is_busy() {
                            if stop_flash_button(
                                ui,
                                "Kill the running `cargo flash` (and its children, so the \
                                 probe is released).",
                            ) {
                                *probe_flash_stop = true;
                            }
                        } else if flash_button(
                            ui,
                            "Flash (probe-rs)",
                            format!("{} Flash (probe-rs)", ph::LIGHTNING),
                            probe_reason.as_deref(),
                            "Build --release, then flash over the selected debug probe with \
                             probe-rs (`cargo flash`). Uses the SAME probe as the Debug / \
                             RTT / Runtime tabs. Needs probe-rs-tools in PATH.",
                        ) {
                            *probe_flash_out = true;
                        }
                        // Status of the last/ongoing probe-rs flash.
                        if !matches!(pf_state, crate::probe_flash::ProbeFlashState::Idle) {
                            ui.label(
                                egui::RichText::new(pf_state.label())
                                    .size(10.5)
                                    .color(pf_state.color()),
                            );
                            if pf_state.is_busy() {
                                ui.spinner();
                                ui.ctx()
                                    .request_repaint_after(std::time::Duration::from_millis(120));
                            }
                        }
                    },
                );
            } else if *toolchain == ToolchainKind::EspRust {
                // ESP has no probe-rs row, so this line is the natural home for
                // the espflash pipeline: which tool runs, and how far it got.
                // (It used to sit on the config row below, which then had no
                // space for the Read-Chip-Info and Monitor buttons.)
                ui.label(
                    egui::RichText::new("Tool: espflash")
                        .size(10.5)
                        .color(egui::Color32::from_rgb(220, 140, 60)),
                );
                ui.separator();
                esp_phase_widgets(ui, &esp_state, build_done);
                ui.label(
                    egui::RichText::new("(probe-rs path is for the ARM toolchain)")
                        .size(10.0)
                        .color(egui::Color32::from_gray(90))
                        .italics(),
                );
            } else {
                ui.label(
                    egui::RichText::new("probe-rs path is for the ARM toolchain")
                        .size(10.5)
                        .color(egui::Color32::from_gray(110)),
                );
            }
        },
        |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                render_size_bar(ui, &size_snapshot, false);
            });
        },
    );
    // Narrow panel: the usage bars get their own wrapped row rather than a
    // clipped column beside the rows above.
    if compact {
        ui.horizontal_wrapped(|ui| {
            render_size_bar(ui, &size_snapshot, true);
            ui.separator();
            render_size_bar(ui, &size_snapshot, false);
        });
    }
    if size_busy {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(120));
    }

    // A red Flash button explains itself on CLICK (see `flash_button`), so the
    // reason no longer takes a permanent line here.
    blocked_dialog(ui, flash_block.zip(table_fix));

    // A tagged probe-rs failure (a probe that won't open, a crash) gets its
    // explanation here — the log below only holds the raw cause.
    if let crate::probe_flash::ProbeFlashState::Error(e) = &pf_state {
        if crate::failure_hint::show_card(ui, e, |_| {}) {
            ui.add_space(4.0);
        }
    }

    // The old per-programmer guidance line lived here; it is part of the Info
    // panel now (it explained the DFU/SWD split, which the table covers).
    let _ = &sel_kind;

    ui.separator();
    // ── Config row — adaptive: ESP32 / SWD (OpenOCD) / DFU ───────────────────
    ui.horizontal(|ui| {
        // One height for the whole row. Every interactive widget takes its
        // minimum from here (a Button sizes to its text, then grows to at least
        // `interact_size`), so Size, Info, Clear, the presets and the address
        // field all come out the same — text fields need `add_sized`, they
        // measure themselves from the font instead.
        ui.spacing_mut().interact_size.y = ROW_H;

        // Helper: render a phase indicator icon + label
        let phase_widget = |ui: &mut egui::Ui, icon: &str, label: &str, color: egui::Color32| {
            ui.label(egui::RichText::new(icon).size(11.5).color(color));
            ui.label(egui::RichText::new(label).size(11.0).color(color));
        };

        if *toolchain == ToolchainKind::EspRust {
            // ── EspRust: one row for everything ──────────────────────────────
            // Reading order matches the panes below it: what acts on the flash
            // log first (left), then the device console (right), with Size/Info
            // pushed to the far edge. `Tool: espflash` and the Build -> Flash
            // phases live on the probe row above.
            esp_board_info_button(
                ui,
                &esp_state,
                espflash_state,
                dfu_log,
                esp_flash_child,
                dfu_sel_programmer,
                espflash_used_port,
            );
            // Disabled while a flash runs: it resets the very phase states the
            // Flash button reads to know it is showing "Stop Flash", so a Clear
            // mid-run would put Flash back and leave the run unstoppable.
            if ui
                .add_enabled(
                    !any_busy,
                    egui::Button::new(
                        egui::RichText::new(ph::BROOM)
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    ),
                )
                .on_hover_text(if any_busy {
                    "Wait for the flash to finish (or stop it) - clearing now would \
                     hide the Stop button"
                } else {
                    "Clear the flash log (left pane) and reset the phase icons"
                })
                .clicked()
            {
                dfu_log.lock().unwrap().clear();
                *dfu_state.lock().unwrap() = DfuState::Idle;
                *openocd_state.lock().unwrap() = OpenOcdState::Idle;
                *espflash_state.lock().unwrap() = EspFlashState::Idle;
            }
        } else if is_swd {
            // ── SWD / OpenOCD config ──────────────────────────────────────────
            ui.label(
                egui::RichText::new("Interface:")
                    .size(10.5)
                    .color(egui::Color32::GRAY),
            );
            ui.label(
                egui::RichText::new(interface_cfg)
                    .size(10.5)
                    .monospace()
                    .color(egui::Color32::from_rgb(120, 160, 200)),
            )
            .on_hover_text("OpenOCD interface config — auto-selected from programmer type");

            ui.separator();

            ui.label(
                egui::RichText::new("Target:")
                    .size(10.5)
                    .color(egui::Color32::GRAY),
            );
            ui.add_sized(
                egui::vec2(140.0, ROW_H),
                egui::TextEdit::singleline(openocd_target_cfg).font(egui::TextStyle::Monospace),
            )
            .on_hover_text(
                "OpenOCD target config file.\n\
                 Examples: target/stm32f1x.cfg, target/stm32f4x.cfg\n\
                 Full list in your OpenOCD install under scripts/target/",
            );

            // Quick-select presets for common STM32 families
            for (label, cfg, tip) in [
                ("F1", "target/stm32f1x.cfg", "STM32F1 (F103, F100, F105, …)"),
                ("F4", "target/stm32f4x.cfg", "STM32F4 (F407, F411, F401, …)"),
                ("H7", "target/stm32h7x.cfg", "STM32H7 (H743, H750, …)"),
                ("L4", "target/stm32l4x.cfg", "STM32L4 (L432, L476, L496, …)"),
            ] {
                if ui
                    .add(egui::Button::new(egui::RichText::new(label).size(10.0)))
                    .on_hover_text(tip)
                    .clicked()
                {
                    *openocd_target_cfg = cfg.to_string();
                }
            }

            ui.separator();

            // SWD phases: Build → Flash (no objcopy — OpenOCD programs ELF directly)
            let (b_icon, b_col) = match &ocd_state {
                OpenOcdState::Building => (ph::CIRCLE_NOTCH, egui::Color32::from_rgb(220, 180, 60)),
                _ if build_done => (ph::CHECK_CIRCLE, egui::Color32::from_rgb(80, 200, 100)),
                OpenOcdState::Error(_) if !build_done && !log.is_empty() => {
                    (ph::X_CIRCLE, egui::Color32::from_rgb(220, 80, 70))
                }
                _ => (ph::CIRCLE_NOTCH, egui::Color32::from_gray(70)),
            };
            phase_widget(ui, b_icon, "Build", b_col);
            ui.label(
                egui::RichText::new(ph::ARROW_RIGHT)
                    .size(11.0)
                    .color(egui::Color32::from_gray(70)),
            );

            let (f_icon, f_col) = match &ocd_state {
                OpenOcdState::Flashing => {
                    (ph::CIRCLE_NOTCH, egui::Color32::from_rgb(100, 180, 255))
                }
                OpenOcdState::Success => (ph::CHECK_CIRCLE, egui::Color32::from_rgb(80, 200, 100)),
                OpenOcdState::Error(_) if build_done => {
                    (ph::X_CIRCLE, egui::Color32::from_rgb(220, 80, 70))
                }
                _ => (ph::CIRCLE_NOTCH, egui::Color32::from_gray(70)),
            };
            phase_widget(ui, f_icon, "Flash SWD", f_col);
        } else if let Some(dfu_flash_addr) = dfu_flash_addr {
            // ── DFU config ────────────────────────────────────────────────────
            ui.label(
                egui::RichText::new("Flash addr:")
                    .size(10.5)
                    .color(egui::Color32::GRAY),
            );
            ui.add_sized(
                egui::vec2(90.0, ROW_H),
                egui::TextEdit::singleline(dfu_flash_addr).font(egui::TextStyle::Monospace),
            )
            .on_hover_text(
                "Start address passed to dfu-util.\n\
                 0x08000000 — standard (no bootloader)\n\
                 0x08002000 — with 8 KB bootloader offset",
            );

            if ui
                .add(egui::Button::new(egui::RichText::new("Std").size(10.0)))
                .on_hover_text("0x08000000 — standard flash start (no bootloader)")
                .clicked()
            {
                *dfu_flash_addr = "0x08000000".to_string();
            }
            if ui
                .add(egui::Button::new(egui::RichText::new("+BL").size(10.0)))
                .on_hover_text("0x08002000 — 8 KB bootloader offset")
                .clicked()
            {
                *dfu_flash_addr = "0x08002000".to_string();
            }

            ui.separator();

            // DFU phases: Build → Objcopy → Flash
            let objcopy_done = log.iter().any(|l| l.contains("[OK] firmware.bin ready"));

            let (b_icon, b_col) = match &state {
                DfuState::Building => (ph::CIRCLE_NOTCH, egui::Color32::from_rgb(220, 180, 60)),
                _ if build_done => (ph::CHECK_CIRCLE, egui::Color32::from_rgb(80, 200, 100)),
                DfuState::Error(_) if !build_done && !log.is_empty() => {
                    (ph::X_CIRCLE, egui::Color32::from_rgb(220, 80, 70))
                }
                _ => (ph::CIRCLE_NOTCH, egui::Color32::from_gray(70)),
            };
            phase_widget(ui, b_icon, "Build", b_col);
            ui.label(
                egui::RichText::new(ph::ARROW_RIGHT)
                    .size(11.0)
                    .color(egui::Color32::from_gray(70)),
            );

            let (o_icon, o_col) = match &state {
                _ if objcopy_done => (ph::CHECK_CIRCLE, egui::Color32::from_rgb(80, 200, 100)),
                DfuState::Flashing | DfuState::Success => {
                    (ph::CHECK_CIRCLE, egui::Color32::from_rgb(80, 200, 100))
                }
                DfuState::Error(_) if build_done && !objcopy_done => {
                    (ph::X_CIRCLE, egui::Color32::from_rgb(220, 80, 70))
                }
                DfuState::Building if build_done => {
                    (ph::CIRCLE_NOTCH, egui::Color32::from_rgb(220, 180, 60))
                }
                _ => (ph::CIRCLE_NOTCH, egui::Color32::from_gray(70)),
            };
            phase_widget(ui, o_icon, "Objcopy", o_col);
            ui.label(
                egui::RichText::new(ph::ARROW_RIGHT)
                    .size(11.0)
                    .color(egui::Color32::from_gray(70)),
            );

            let (f_icon, f_col) = match &state {
                DfuState::Flashing => (ph::CIRCLE_NOTCH, egui::Color32::from_rgb(100, 180, 255)),
                DfuState::Success => (ph::CHECK_CIRCLE, egui::Color32::from_rgb(80, 200, 100)),
                DfuState::Error(_) if objcopy_done => {
                    (ph::X_CIRCLE, egui::Color32::from_rgb(220, 80, 70))
                }
                _ => (ph::CIRCLE_NOTCH, egui::Color32::from_gray(70)),
            };
            phase_widget(ui, f_icon, "Flash", f_col);
        } else {
            // No ROM bootloader to hand a flash address to, and no DFU phases
            // to show: this chip is programmed over the probe instead.
            ui.label(
                egui::RichText::new("No USB DFU bootloader on this chip")
                    .size(10.5)
                    .color(egui::Color32::from_gray(140)),
            )
            .on_hover_text(
                "The DFU path programs an STM32's ROM bootloader with dfu-util, at a \
                 flash address this part does not have.\n\
                 Flash this chip with Flash (probe-rs), or pick an ST-Link / J-Link / \
                 CMSIS-DAP above for Flash SWD.",
            );
        }

        // Right-aligned tail of the config row: Size · Info · Clear. Laid out
        // right to left, so Clear stays on the edge and the other two sit just
        // before it — away from the usage bars they used to overlap.
        //
        // On ESP the text "Clear" is gone — the two broom icons (one per pane)
        // replace it, and a third Clear here would be a coin toss.
        if *toolchain == ToolchainKind::EspRust {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                crate::app::helpers::help_panel::toggle_button_with(
                    ui,
                    INFO_ID,
                    "Info",
                    "How flashing an ESP32 works, and what the selected programmer can do",
                );
                if size_button(ui, size_busy, !size_busy && !any_busy && can_flash) {
                    *size_out = true;
                }
                ui.separator();
                // The monitor group sits just left of Size/Info — it belongs to
                // the right-hand pane, so it reads next to it rather than tucked
                // against the flash-log buttons on the far left.
                esp_monitor_controls(
                    ui,
                    esp_monitor,
                    monitor_go,
                    monitor_auto,
                    monitor_auto_set,
                    can_flash,
                    serial_port_held,
                    missing_tools,
                    true, // this block is laid out right to left
                );
            });
            return;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    !any_busy,
                    egui::Button::new(
                        egui::RichText::new(format!("{} Clear", ph::X))
                            .size(10.0)
                            .color(egui::Color32::GRAY),
                    ),
                )
                .on_hover_text(if any_busy {
                    "Wait for the flash to finish (or stop it) - clearing now would \
                     hide the Stop button"
                } else {
                    "Clear the flash log and reset the phase icons"
                })
                .clicked()
            {
                dfu_log.lock().unwrap().clear();
                *dfu_state.lock().unwrap() = DfuState::Idle;
                *openocd_state.lock().unwrap() = OpenOcdState::Idle;
                *espflash_state.lock().unwrap() = EspFlashState::Idle;
            }
            crate::app::helpers::help_panel::toggle_button_with(
                ui,
                INFO_ID,
                "Info",
                "How the flashing paths differ, and what each programmer kind can do",
            );
            if size_button(ui, size_busy, !size_busy && !any_busy && can_flash) {
                *size_out = true;
            }
        });
    });

    // ── Info panel — right under the button that opens it ────────────────────
    flash_info_panel(ui, progs.get(dfu_sel_programmer), toolchain);
    /*
        // ── ESP32 port selector ───────────────────────────────────────────────────
        // Shown only for EspRust.  Lets the user type a COM port (e.g. "COM3") so
        // espflash targets the correct device when multiple serial ports are present.
        // Leave empty to let espflash auto-detect.
        if *toolchain == ToolchainKind::EspRust {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Port:")
                        .size(11.0)
                        .color(egui::Color32::from_gray(160)),
                );
                let resp = ui.add(
                    egui::TextEdit::singleline(espflash_port)
                        .hint_text("auto (e.g. COM3)")
                        .desired_width(120.0)
                        .font(egui::TextStyle::Monospace),
                );
                resp.on_hover_text(
                    "Serial port for espflash.\n\
                     Leave empty to auto-detect.\n\
                     Windows example: COM3\n\
                     Linux example:   /dev/ttyUSB0\n\
                     macOS example:   /dev/cu.usbserial-0001",
                );
                if !espflash_port.is_empty() {
                    if ui
                        .small_button(ph::X)
                        .on_hover_text("Clear — use auto-detect")
                        .clicked()
                    {
                        espflash_port.clear();
                    }
                }
                ui.label(
                    egui::RichText::new(format!("{} leave empty for auto-detect", ph::ARROW_LEFT))
                        .size(10.0)
                        .color(egui::Color32::from_gray(100))
                        .italics(),
                );
            });
            ui.add_space(2.0);
        }
    */
    // (The ESP32 "Read Chip Info" row was merged into the config row above, so
    // the ESP toolchain has ONE row of buttons instead of two half-empty ones.)

    ui.separator();

    // ── DFU Error / Success banners ───────────────────────────────────────────
    /*
        if let DfuState::Error(msg) = &state {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(ph::X_CIRCLE)
                        .size(13.0)
                        .color(egui::Color32::from_rgb(220, 80, 70)),
                );
                ui.label(
                    egui::RichText::new(format!("DFU: {}", msg.lines().next().unwrap_or("Error")))
                        .size(11.0)
                        .color(egui::Color32::from_rgb(220, 80, 70))
                        .strong(),
                );
            });
            ui.separator();
        }
        if matches!(state, DfuState::Success) {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(ph::CHECK_CIRCLE)
                        .size(13.0)
                        .color(egui::Color32::from_rgb(80, 200, 100)),
                );
                ui.label(
                    egui::RichText::new("Device programmed via DFU successfully!")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(80, 200, 100))
                        .strong(),
                );
            });
            ui.separator();
        }
    */
    // ── OpenOCD Error / Success banners ───────────────────────────────────────
    if let OpenOcdState::Error(msg) = &ocd_state {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(ph::X_CIRCLE)
                    .size(13.0)
                    .color(egui::Color32::from_rgb(220, 80, 70)),
            );
            ui.label(
                egui::RichText::new(format!("SWD: {}", msg.lines().next().unwrap_or("Error")))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(220, 80, 70))
                    .strong(),
            );
        });
        ui.separator();
    }
    if matches!(ocd_state, OpenOcdState::Success) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(ph::CHECK_CIRCLE)
                    .size(13.0)
                    .color(egui::Color32::from_rgb(80, 200, 100)),
            );
            ui.label(
                egui::RichText::new("Device programmed via SWD successfully!")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(80, 200, 100))
                    .strong(),
            );
        });
        ui.separator();
    }

    // ── ESP32 flash Error / Success banners ───────────────────────────────────
    if let EspFlashState::Error(msg) = &esp_state {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(ph::X_CIRCLE)
                    .size(13.0)
                    .color(egui::Color32::from_rgb(220, 80, 70)),
            );
            ui.label(
                egui::RichText::new(format!("ESP: {}", msg.lines().next().unwrap_or("Error")))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(220, 80, 70))
                    .strong(),
            );
        });
        ui.separator();
    }
    // The two verdicts of one action, on one row: the flash worked, and then
    // the monitor did or did not manage to attach. They are read together -
    // "programmed successfully" alone is misleading when the console that was
    // supposed to follow it never opened.
    //
    // Either can appear without the other: a monitor started by hand can fail
    // with no flash before it, so the row is not tied to Success.
    let monitor_error = match &*esp_monitor.phase.lock().unwrap() {
        crate::esp_monitor::MonitorPhase::Error(e) => Some(e.clone()),
        _ => None,
    };
    let flashed = matches!(esp_state, EspFlashState::Success);
    if flashed || monitor_error.is_some() {
        ui.horizontal_wrapped(|ui| {
            if flashed {
                ui.label(
                    egui::RichText::new(ph::CHECK_CIRCLE)
                        .size(13.0)
                        .color(egui::Color32::from_rgb(80, 200, 100)),
                );
                ui.label(
                    egui::RichText::new("ESP32 programmed successfully!")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(80, 200, 100))
                        .strong(),
                );
            }
            if let Some(e) = &monitor_error {
                ui.label(
                    egui::RichText::new(ph::X_CIRCLE)
                        .size(13.0)
                        .color(egui::Color32::from_rgb(230, 90, 80)),
                );
                ui.label(
                    egui::RichText::new(e.as_str())
                        .size(11.0)
                        .color(egui::Color32::from_rgb(230, 90, 80)),
                )
                .on_hover_text(e.as_str());
            }
        });
        ui.separator();
    }

    // ── Output area ──────────────────────────────────────────────────────────
    // On ESP it is split in two: the flasher's own log on the left, the device
    // console on the right. They are two different conversations — one with the
    // programmer, one with the firmware — and interleaving them in one
    // scrollback made it impossible to tell which side said what.
    if *toolchain == ToolchainKind::EspRust {
        // No per-pane toolbars: every button lives on the single row above (see
        // the config row), so both panes are pure output. Each takes the full
        // height of its column; which is which is stated by the empty-state text
        // and by the ordering — flasher left, device right, same as the buttons.
        let h = ui.available_height();
        ui.columns(2, |cols| {
            flash_log_view(&mut cols[0], &log, h, toolchain);
            monitor_view(&mut cols[1], esp_monitor, h);
        });
        return;
    }
    flash_log_view(ui, &log, ui.available_height(), toolchain);
}

/// The flasher's own output (cargo, dfu-util, OpenOCD, espflash) — one line per
/// entry, coloured by content since the tools' ANSI escapes are stripped.
fn flash_log_view(ui: &mut egui::Ui, log: &[String], height: f32, toolchain: &ToolchainKind) {
    egui::ScrollArea::vertical()
        .id_salt("dfu_log_scroll")
        .stick_to_bottom(true)
        .max_height(height)
        // Full panel width: with the default auto-shrink the area is only as
        // wide as its longest line, which put the scrollbar in the middle of
        // the panel.
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if log.is_empty() {
                ui.add_space(8.0);
                // Only the paths this chip HAS: listing DFU and SWD on an ESP
                // board describes hardware it does not have (same reason the
                // Info panel is split per toolchain).
                let hint = if *toolchain == ToolchainKind::EspRust {
                    "No output yet.\n\
                     Flash ESP32 builds --release, then programs the board with \
                     espflash over the serial port.\n\
                     Read chip info connects without writing anything."
                } else {
                    "No output yet.\n\
                     • Flash USB    — DFU mode (STM32 with BOOT0 = 1)\n\
                     • Flash SWD    — ST-Link / J-Link via OpenOCD\n\
                     • Flash (probe-rs) — the shared debug probe"
                };
                ui.label(
                    egui::RichText::new(hint)
                        .size(11.0)
                        .color(egui::Color32::GRAY),
                );
                return;
            }

            for line in log {
                // A tool that colours its output (cargo through `cargo flash`,
                // espflash) sends ANSI escapes; this console renders plain
                // strings, so they would show up as literal `[1m[93m` noise.
                // The colour below comes from the line's CONTENT instead.
                let line = crate::terminal::strip_ansi(line);
                let line = line.as_str();
                // Colour-code lines by content prefix.
                //
                // IMPORTANT: cargo prints "   Compiling proc-macro-error-attr2"
                // and "   Compiling proc-macro-error2" which contain the word
                // "error" inside the crate name — NOT an actual build error.
                // Real cargo errors always start with "error" at column 0
                // (e.g. "error[E0308]: …" or "error: …").
                // Warnings start with "warning" at column 0.
                // Using starts_with() avoids false positives from crate names.
                let trimmed = line.trim_start();
                let color = if line.starts_with("[OK]") {
                    egui::Color32::from_rgb(80, 200, 100) // green  — success
                } else if line.starts_with(">") {
                    egui::Color32::from_rgb(100, 180, 255) // blue   — command header
                } else if trimmed.starts_with("error[") || trimmed.starts_with("error:") {
                    egui::Color32::from_rgb(220, 100, 80) // red    — real compile error
                } else if trimmed.starts_with("warning[") || trimmed.starts_with("warning:") {
                    egui::Color32::from_rgb(210, 170, 40) // yellow — compile warning
                } else if trimmed.starts_with("Compiling ")
                    || trimmed.starts_with("Finished ")
                    || trimmed.starts_with("Running ")
                {
                    egui::Color32::from_rgb(130, 170, 130) // muted green — progress
                } else {
                    egui::Color32::from_rgb(175, 180, 192) // grey   — normal output
                };
                ui.label(
                    egui::RichText::new(line)
                        .size(10.5)
                        .monospace()
                        .color(color),
                );
            }
        });
}

/// The device console's scrollback, or a hint at what would fill it.
fn monitor_view(ui: &mut egui::Ui, monitor: &crate::esp_monitor::EspMonitor, height: f32) {
    // On screen, empty or not: the reader repaints only a view drawn lately.
    crate::terminal::mark_drawn(&monitor.state, ui.ctx());
    let empty = monitor.state.lock().unwrap().lines.is_empty();
    if empty {
        egui::ScrollArea::vertical()
            .id_salt("esp_monitor_empty")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Nothing from the device yet.\n\
                         What `esp_println::println!`, the `log` macros and panics \
                         print shows up here.\n\
                         \n\
                         Flash with Monitor on, or press Monitor to attach to the \
                         firmware already running.",
                    )
                    .size(11.0)
                    .color(egui::Color32::GRAY),
                );
            });
        return;
    }
    super::terminal_tab::render_scrollback(ui, &monitor.state, "esp_monitor_scroll", height);
}

/// `Build -> Flash ESP32` progress, shown on the probe row (ESP has no probe).
fn esp_phase_widgets(ui: &mut egui::Ui, esp_state: &EspFlashState, build_done: bool) {
    let phase = |ui: &mut egui::Ui, icon: &str, label: &str, color: egui::Color32| {
        ui.label(egui::RichText::new(icon).size(11.5).color(color));
        ui.label(egui::RichText::new(label).size(11.0).color(color));
    };
    let (b_icon, b_col) = match esp_state {
        EspFlashState::Building => (ph::CIRCLE_NOTCH, egui::Color32::from_rgb(220, 180, 60)),
        _ if build_done => (ph::CHECK_CIRCLE, egui::Color32::from_rgb(80, 200, 100)),
        EspFlashState::Error(_) if !build_done => {
            (ph::X_CIRCLE, egui::Color32::from_rgb(220, 80, 70))
        }
        _ => (ph::CIRCLE_NOTCH, egui::Color32::from_gray(70)),
    };
    phase(ui, b_icon, "Build", b_col);
    ui.label(
        egui::RichText::new(ph::ARROW_RIGHT)
            .size(11.0)
            .color(egui::Color32::from_gray(70)),
    );
    let (f_icon, f_col) = match esp_state {
        EspFlashState::Flashing => (ph::CIRCLE_NOTCH, egui::Color32::from_rgb(220, 140, 60)),
        EspFlashState::Success => (ph::CHECK_CIRCLE, egui::Color32::from_rgb(80, 200, 100)),
        EspFlashState::Error(_) if build_done => {
            (ph::X_CIRCLE, egui::Color32::from_rgb(220, 80, 70))
        }
        _ => (ph::CIRCLE_NOTCH, egui::Color32::from_gray(70)),
    };
    phase(ui, f_icon, "Flash ESP32", f_col);
}

/// `espflash board-info` — connect and identify the chip, writing nothing.
/// The Flash button's second state: while a flash runs, that same button stops
/// it. Red, and always ENABLED - a flash that hangs is precisely when this has
/// to be reachable, and a disabled button would be both faded and inert.
fn stop_flash_button(ui: &mut egui::Ui, hover: &str) -> bool {
    ui.add(
        egui::Button::new(
            egui::RichText::new(format!("{} Stop Flash", ph::STOP_CIRCLE))
                .size(10.5)
                .color(egui::Color32::from_rgb(235, 120, 110)),
        )
        .min_size(egui::vec2(FLASH_W, ROW_H)),
    )
    .on_hover_text(hover)
    .clicked()
}

fn esp_board_info_button(
    ui: &mut egui::Ui,
    esp_state: &EspFlashState,
    espflash_state: &Arc<Mutex<EspFlashState>>,
    dfu_log: &Arc<Mutex<Vec<String>>>,
    esp_flash_child: &crate::flash_stop::FlashHandle,
    dfu_sel_programmer: &str,
    espflash_used_port: &Arc<Mutex<String>>,
) {
    let busy = esp_state.is_busy();
    let reading = matches!(esp_state, EspFlashState::ReadingInfo);
    let label = if reading {
        format!("{} Reading…", ph::CIRCLE_NOTCH)
    } else {
        format!("{} Read chip info", ph::INFO)
    };
    if ui
        .add_enabled(
            !busy,
            egui::Button::new(
                egui::RichText::new(&label)
                    .size(10.5)
                    .color(egui::Color32::from_rgb(100, 180, 255)),
            ),
        )
        .on_hover_text(
            "Run `espflash board-info` — connects to the chip and reads:\n\
             chip type, silicon revision, flash size, MAC address.\n\
             \n\
             Nothing is written to flash — safe to run at any time.\n\
             Use this to verify the USB cable / COM port works before flashing.",
        )
        .clicked()
    {
        espflash::read_board_info(
            Arc::clone(espflash_state),
            Arc::clone(dfu_log),
            Arc::clone(esp_flash_child),
            ui.ctx().clone(),
            dfu_sel_programmer.to_owned(),
            Arc::clone(espflash_used_port),
        );
    }
}

/// Monitor controls: Start / Stop / Clear, the auto-open checkbox and the phase
/// badge. Sits between Read-chip-info and the right-aligned Size/Info/Clear.
#[allow(clippy::too_many_arguments)]
fn esp_monitor_controls(
    ui: &mut egui::Ui,
    monitor: &mut crate::esp_monitor::EspMonitor,
    monitor_go: &mut bool,
    monitor_auto: bool,
    monitor_auto_set: &mut Option<bool>,
    can_flash: bool,
    serial_port_held: Option<&str>,
    missing_tools: &[&'static str],
    // The group lives inside the row's RIGHT-ALIGNED block, which lays widgets
    // out right to left — so to READ as `Monitor Stop 🧹 auto ⦿port` they have to
    // be ADDED in the opposite order. Set `false` for a normal left-to-right row.
    right_to_left: bool,
) {
    use crate::esp_monitor::MonitorPhase;
    let phase = monitor.phase();
    let busy = monitor.is_busy();
    let no_espflash = super::tool_missing(missing_tools, "espflash");
    // A serial port is exclusive: while the Serial tab holds one, espflash
    // cannot open it, and its error ("Access is denied") names nothing useful.
    let blocked_by_serial = serial_port_held.is_some();
    let can_start = can_flash && !no_espflash && !blocked_by_serial;

    // Which widget, in READING order. The row's right-aligned block lays out
    // right to left, so there the same list is walked backwards — one order,
    // declared once, instead of two copies of the buttons.
    #[derive(Clone, Copy)]
    enum W {
        Start,
        Stop,
        Clear,
        Auto,
        Status,
    }
    const ORDER: [W; 5] = [W::Start, W::Stop, W::Clear, W::Auto, W::Status];
    let walk: Box<dyn Iterator<Item = &W>> = if right_to_left {
        Box::new(ORDER.iter().rev())
    } else {
        Box::new(ORDER.iter())
    };

    for w in walk {
        match w {
            W::Start => {
                if ui
                    .add_enabled(
                        !busy && can_start,
                        egui::Button::new(
                            egui::RichText::new(format!("{} Monitor", ph::PLAY))
                                .size(10.5)
                                .color(if !busy && can_start {
                                    egui::Color32::from_rgb(100, 220, 100)
                                } else {
                                    egui::Color32::GRAY
                                }),
                        ),
                    )
                    .on_hover_text(
                        "`espflash monitor`: attach to the board and stream what the firmware \
                     prints (esp-println / log / panics).\nIt resets the target once \
                     attached, so the output starts at boot.",
                    )
                    .on_disabled_hover_text(if no_espflash {
                        super::needs_tool_hint("espflash")
                    } else if let Some(p) = serial_port_held {
                        format!("The Serial tab is connected on {p} — disconnect it there first.")
                    } else if busy {
                        "The monitor is already attached.".to_owned()
                    } else {
                        "No ESP chip config exists yet.".to_owned()
                    })
                    .clicked()
                {
                    *monitor_go = true;
                }
            }
            W::Stop => {
                ui.add_enabled_ui(busy, |ui| {
                    if ui
                        .button(
                            egui::RichText::new(format!("{} Stop", ph::STOP_CIRCLE))
                                .size(10.5)
                                .color(egui::Color32::from_rgb(230, 120, 110)),
                        )
                        .on_hover_text("Kill espflash and release the serial port.")
                        .clicked()
                    {
                        monitor.stop();
                    }
                });
            }
            W::Clear => {
                // Icon-only: the row already ends in a "Clear" (the flash log's), and two
                // buttons with the same word would be a coin toss.
                if ui
                    .button(
                        egui::RichText::new(ph::BROOM)
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    )
                    .on_hover_text("Clear the device output (the right-hand pane)")
                    .clicked()
                {
                    monitor.clear();
                }
            }
            W::Auto => {
                let mut auto = monitor_auto;
                if ui
                    .checkbox(&mut auto, egui::RichText::new("auto").size(10.5))
                    .on_hover_text(
                        "Open the monitor automatically after a successful flash.\n\
                     The flash then leaves the chip in reset and the monitor resets it \
                     once attached, so the first println! of main is not missed.\n\
                     Off = press Monitor yourself.",
                    )
                    .changed()
                {
                    *monitor_auto_set = Some(auto);
                }
            }
            W::Status => {
                // The port rides along with the status: which board is being watched matters
                // most exactly while one IS being watched, and it costs no extra widget.
                let port = monitor.port.lock().unwrap().clone();
                let (text, color) = match &phase {
                    MonitorPhase::Idle => (String::new(), egui::Color32::GRAY),
                    MonitorPhase::Starting => (
                        format!("attaching {port}…"),
                        egui::Color32::from_rgb(220, 180, 60),
                    ),
                    MonitorPhase::Streaming => (
                        format!("{} {port}", ph::BROADCAST),
                        egui::Color32::from_rgb(80, 200, 100),
                    ),
                    // Drawn on the VERDICT row below, next to the flash's own
                    // result - not here. It is a sentence long, and inline it
                    // pushed this row wide enough to crowd the buttons it sits
                    // among. The live statuses above stay: they are the
                    // monitor's current state, which belongs beside its
                    // controls.
                    MonitorPhase::Error(_) => (String::new(), egui::Color32::GRAY),
                };
                if !text.is_empty() {
                    ui.label(egui::RichText::new(text).size(10.5).color(color));
                }
            }
        }
    }
    if blocked_by_serial {
        ui.label(
            egui::RichText::new(ph::WARNING)
                .size(11.0)
                .color(egui::Color32::from_rgb(210, 170, 90)),
        )
        .on_hover_text("The Serial tab holds the port. Only one of the two can read it at a time.");
    }
}

/// Memory key of this tab's Info panel.
const INFO_ID: &str = "flash";

/// Height of everything on the two rows — buttons AND the ComboBoxes, which take
/// theirs from `spacing().interact_size.y`. Without pinning it, the buttons come
/// out visibly thinner than the dropdowns next to them.
const ROW_H: f32 = 22.0;
/// Fixed widths so the two rows line up column for column.
const SCAN_W: f32 = 76.0;
const FLASH_W: f32 = 146.0;
/// Right-hand block: the Flash/RAM bar plus the Size / Info button.
const RIGHT_W: f32 = 300.0;
/// Below this panel width the right-hand block would leave the device list
/// nothing, so it moves to a row of its own instead (see [`right_block_w`]).
const COMPACT_BELOW: f32 = 700.0;

/// Width of the right-hand block for a panel `total` px wide — `0.0` when the
/// panel is too narrow to carry it beside the rows. Splitting the row is only
/// worth it while BOTH halves stay usable; the alternative is what the user
/// saw before this: the Size button pushed out of the clipped region.
fn right_block_w(total: f32) -> f32 {
    if total < COMPACT_BELOW { 0.0 } else { RIGHT_W }
}

/// Width left for the device ComboBox once Scan, the Flash button, the gaps and
/// the right-hand block have taken theirs — capped at [`COMBO_MAX_FRACTION`] of
/// the panel, since a list that eats half the tab is no easier to read than a
/// truncated one. Shared by both rows so their lists are the same size.
fn combo_width(total: f32) -> f32 {
    let left = (total - right_block_w(total) - 8.0 - SCAN_W - FLASH_W - 24.0).max(40.0);
    left.min((total * COMBO_MAX_FRACTION).max(40.0))
}

/// The device lists never take more than this share of the panel width.
const COMBO_MAX_FRACTION: f32 = 0.40;

/// A Flash button that says WHY it can't run. `reason` is `None` when it can;
/// otherwise the text goes red and a click OPENS THE EXPLANATION instead of
/// flashing — the button stays enabled on purpose. A disabled button is drawn
/// faded (so the red barely reads as red), it can't be hovered for its own
/// tooltip on some platforms, and clicking it does nothing at all — which is
/// exactly when a user most wants to be told why. Returns true only when the
/// caller should actually flash.
fn flash_button(
    ui: &mut egui::Ui,
    what: &str,
    label: String,
    reason: Option<&str>,
    hover: &str,
) -> bool {
    let color = match reason {
        Some(_) => egui::Color32::from_rgb(235, 85, 75),
        None => egui::Color32::from_rgb(255, 165, 50),
    };
    let resp = ui.add(
        egui::Button::new(egui::RichText::new(label).size(10.5).color(color))
            .min_size(egui::vec2(FLASH_W, ROW_H)),
    );
    match reason {
        Some(r) => {
            resp.on_hover_text(format!(
                "{what} can't run right now — click for details.\n\n{r}"
            ))
            .clicked()
            .then(|| open_blocked_dialog(ui, what, r));
            false
        }
        None => resp.on_hover_text(hover).clicked(),
    }
}

fn blocked_dialog_id() -> egui::Id {
    egui::Id::new("flash_blocked_dialog")
}

/// Remember what to explain; [`blocked_dialog`] draws it at the end of the tab.
/// Kept in egui memory rather than an out-param so the button helper stays a
/// plain function — the tab render fn already carries enough signals.
fn open_blocked_dialog(ui: &egui::Ui, what: &str, reason: &str) {
    ui.data_mut(|d| d.insert_temp(blocked_dialog_id(), (what.to_owned(), reason.to_owned())));
}

fn partition_fix_request_id() -> egui::Id {
    egui::Id::new("flash_partition_fix_request")
}

/// "Fix partitions.csv" was clicked. Signalled through egui temp data, as
/// `failure_hint`'s "Open Tools" is, so the click needs no out-parameter
/// threaded through every tab.
pub(crate) fn request_partition_fix(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(partition_fix_request_id(), true));
}

/// Was "Fix partitions.csv" clicked since the last call? The other half of
/// [`request_partition_fix`].
pub(crate) fn take_partition_fix_request(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(partition_fix_request_id()))
        .unwrap_or(false)
}

/// The "why can't this run" dialog — a real window on the context, so it floats
/// above the panel that opened it.
///
/// `fix` = (the block as it stands now, what the repair would change). The
/// "Fix partitions.csv" button is offered only while the dialog explains THAT
/// block - not when it was opened for a missing tool, say, or the table has
/// changed since.
fn blocked_dialog(ui: &mut egui::Ui, fix: Option<(&str, &str)>) {
    let Some((what, reason)) = ui.data(|d| d.get_temp::<(String, String)>(blocked_dialog_id()))
    else {
        return;
    };
    let fix = fix.filter(|(block, _)| *block == reason).map(|(_, f)| f);
    let mut open = true;
    let mut dismissed = false;
    egui::Window::new(format!("{} {what}", ph::WARNING))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            ui.set_max_width(440.0);
            ui.add(
                egui::Label::new(
                    // The reasons are sentences of their own, often with a
                    // full stop already.
                    egui::RichText::new(format!(
                        "{what} can't run: {}.",
                        reason.trim_end_matches('.')
                    ))
                    .size(11.5)
                    .color(egui::Color32::from_rgb(230, 200, 190)),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if let Some(change) = fix {
                    let fix_clicked = ui
                        .button(format!("{} Fix partitions.csv", ph::WRENCH))
                        .on_hover_text(format!(
                            concat!(
                                "Change only what espflash cannot read: {}. Names, ",
                                "offsets and sizes stay. Firmware that looks the ",
                                "partition up by its old subtype number must use 0x06 ",
                                "(undefined) too; one that uses a fixed range is ",
                                "unaffected. Save keeps it on disk."
                            ),
                            change
                        ))
                        .clicked();
                    if fix_clicked {
                        request_partition_fix(ui.ctx());
                        dismissed = true;
                    }
                }
                if ui.button("OK").clicked() {
                    dismissed = true;
                }
                ui.label(
                    egui::RichText::new("Info explains the flashing paths.")
                        .size(10.0)
                        .color(egui::Color32::from_gray(130)),
                );
            });
        });
    if !open || dismissed {
        ui.data_mut(|d| d.remove_temp::<(String, String)>(blocked_dialog_id()));
    }
}

/// One row split in two: the left block takes what is left, the right block a
/// fixed width. Both rows use the same split, so their contents line up column
/// for column.
///
/// The row is allocated at EXACTLY the visible width and both halves are child
/// uis at fixed rects (`new_child` + `set_clip_rect`) — the pattern from
/// `debug_tab`'s pane row. A child's `min_rect` does not feed back into the
/// parent, which is what stops this tab from re-widening the Code Editor side
/// panel every frame (egui side panels adopt their content's width, so a row
/// that asks for one pixel too many grows forever). The right block is laid out
/// RIGHT to left, so its button sits on the panel edge and stays visible.
fn split_row(
    ui: &mut egui::Ui,
    left: impl FnOnce(&mut egui::Ui),
    right: impl FnOnce(&mut egui::Ui),
) {
    let total_w = ui.available_width();
    let gap = 8.0;
    let right_w = right_block_w(total_w);
    let left_w = (total_w - right_w - if right_w > 0.0 { gap } else { 0.0 }).max(0.0);
    let (row, _) = ui.allocate_exact_size(egui::vec2(total_w, ROW_H), egui::Sense::hover());

    let child = |ui: &mut egui::Ui, rect: egui::Rect, layout: egui::Layout| {
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout));
        c.set_clip_rect(rect.intersect(ui.clip_rect()));
        c.spacing_mut().interact_size.y = ROW_H;
        c
    };

    let left_rect = egui::Rect::from_min_size(row.min, egui::vec2(left_w, ROW_H));
    {
        let ui = &mut child(
            ui,
            left_rect,
            egui::Layout::left_to_right(egui::Align::Center),
        );
        left(ui);
    }
    if right_w > 0.0 {
        let x = row.min.x + left_w + gap * 0.5;
        ui.painter().vline(
            x,
            row.y_range(),
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        let right_rect = egui::Rect::from_min_size(
            egui::pos2(row.min.x + left_w + gap, row.min.y),
            egui::vec2(right_w, ROW_H),
        );
        let ui = &mut child(
            ui,
            right_rect,
            egui::Layout::right_to_left(egui::Align::Center),
        );
        right(ui);
    }
}

/// The Size button — the same measurement as the Cargo tab's, which also runs
/// on its own after every flash; this is for re-checking without programming the
/// board. Lives in a function because it has two homes: the right-hand block on
/// a wide panel, the wrapped row below on a narrow one.
fn size_button(ui: &mut egui::Ui, busy: bool, enabled: bool) -> bool {
    ui.add_enabled(
        enabled,
        egui::Button::new(
            egui::RichText::new(if busy {
                "Measuring…".to_owned()
            } else {
                format!("{} Size", ph::RULER)
            })
            .size(10.5)
            .color(if enabled {
                egui::Color32::from_rgb(120, 170, 210)
            } else {
                egui::Color32::GRAY
            }),
        )
        // Height from the row, not a constant of its own — that is what made it
        // taller than Clear and Info next to it.
        .min_size(egui::vec2(72.0, ui.spacing().interact_size.y)),
    )
    .on_hover_text(
        "Measure Flash/RAM usage: `cargo build --release`, then read the ELF \
         section sizes against the memory.x limits.\nRuns automatically after \
         each flash started with a Flash button, but not after a Debug or RTT \
         session flashes the board.",
    )
    .clicked()
}

/// Fit `text` into `width` px of the UI's monospace font, ending in `…` when it
/// has to be cut. A ComboBox does not truncate its selected text on its own, and
/// these labels carry a name + VID:PID + port + details — so without this the
/// row's width demand would follow whatever USB device happens to be plugged in.
pub(crate) fn ellipsize(text: &str, width: f32) -> String {
    // Monospace at size 10.5; the arrow + frame padding eat ~26 px.
    const CHAR_W: f32 = 6.3;
    let max = (((width - 26.0) / CHAR_W).floor() as i64).max(4) as usize;
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let keep: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{keep}…")
}

/// The programmer ComboBox (USB scan results), split out of the row so the row
/// itself stays readable.
fn programmer_combo(
    ui: &mut egui::Ui,
    width: f32,
    progs: &HashMap<String, dfu::ProgrammerInfo>,
    dfu_sel_programmer: &mut String,
    toolchain: &ToolchainKind,
) {
    {
        let combo_label = if progs.is_empty() {
            "— none detected —".to_string()
        } else {
            progs
                .get(dfu_sel_programmer)
                .map(|p| p.combo_label())
                .unwrap_or_else(|| "— select —".to_string())
        };

        egui::ComboBox::from_id_salt("dfu_programmer_selector")
            // Truncated to the column: programmer labels are long, and a
            // ComboBox that sizes to its text would re-widen the editor side
            // panel every frame (the panel-growth rule). Full text on hover.
            .selected_text(
                egui::RichText::new(ellipsize(&combo_label, width))
                    .size(10.5)
                    .monospace(),
            )
            .width(width)
            .height(progs.len() as f32 * 30.0)
            .show_ui(ui, |ui| {
                if progs.is_empty() {
                    ui.label(
                        egui::RichText::new("No programmer detected. Click 'Scan USB'.")
                            .size(10.5)
                            .color(egui::Color32::GRAY),
                    );
                }
                for (key, p) in progs.iter() {
                    // Determine if this programmer is compatible with the selected toolchain
                    let is_stm_programmer = matches!(
                        p.kind.as_str(),
                        "DFU Bootloader" | "ST-Link" | "STLink" | "STM" | "J-Link" | "CMSIS-DAP"
                    );
                    let is_esp_programmer = crate::dfu::is_espressif_target(&p.kind, &p.vid_pid);

                    let is_compatible = match toolchain {
                        ToolchainKind::RustEmbedded => is_stm_programmer,
                        ToolchainKind::EspRust => is_esp_programmer,
                        ToolchainKind::SdccC => false,
                    };

                    let kind_color = match p.kind.as_str() {
                        "DFU Bootloader" => egui::Color32::from_rgb(100, 200, 255),
                        "ST-Link" => egui::Color32::from_rgb(100, 220, 120),
                        "J-Link" => egui::Color32::from_rgb(220, 180, 60),
                        "CMSIS-DAP" => egui::Color32::from_rgb(180, 140, 220),
                        "USB-Serial" => egui::Color32::from_rgb(200, 160, 100),
                        "ESP32" => egui::Color32::from_rgb(220, 120, 60),
                        _ => egui::Color32::GRAY,
                    };

                    // Disable color for incompatible programmers
                    let display_color = if is_compatible {
                        kind_color
                    } else {
                        egui::Color32::from_gray(80)
                    };

                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("[{}] {}", p.kind, p.port))
                                .size(10.0)
                                .monospace()
                                .color(display_color),
                        );
                        ui.add_enabled(
                            is_compatible,
                            egui::Button::selectable(
                                dfu_sel_programmer == key,
                                egui::RichText::new(format!(
                                    "{}  [{}] {}; {}",
                                    p.name, p.vid_pid, p.port, p.extra_details
                                ))
                                .size(10.5)
                                .monospace(),
                            ),
                        )
                        .clicked()
                        .then(|| {
                            if is_compatible {
                                *dfu_sel_programmer = key.clone();
                            }
                        });
                    });
                }
            });
    }
}

/// The Info panel: how the flashing paths differ, side by side, plus what the
/// SELECTED programmer kind can actually do (the old per-programmer guidance
/// line, which used to sit under the row claiming SWD needed an external
/// OpenOCD — this tab has had a button for it for a while).
fn flash_info_panel(
    ui: &mut egui::Ui,
    selected: Option<&dfu::ProgrammerInfo>,
    toolchain: &ToolchainKind,
) {
    crate::app::helpers::help_panel::custom_panel(ui, INFO_ID, |ui| {
        // Explicit column widths. An `egui::Grid` sizes its columns from the
        // content, and a WRAPPED label reports a tiny minimum — which is what
        // squeezed the "Flash SWD" column into one word per line. Fixed widths
        // that add up to the available space fix the shape AND keep the table
        // from out-demanding the panel.
        let total = ui.available_width().max(240.0);
        let w_aspect = (total * 0.17).clamp(80.0, 190.0);
        let w_col = ((total - w_aspect - 28.0) * 0.5).max(90.0);
        let cell =
            |ui: &mut egui::Ui, w: f32, text: &str, rich: fn(egui::RichText) -> egui::RichText| {
                ui.allocate_ui_with_layout(
                    egui::vec2(w, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(w);
                        ui.add(egui::Label::new(rich(egui::RichText::new(text).size(10.5))).wrap());
                    },
                );
            };
        let head = |t: egui::RichText| t.strong().color(egui::Color32::from_rgb(200, 210, 230));
        let body = |t: egui::RichText| t.color(egui::Color32::from_gray(195));
        let aspect = |t: egui::RichText| t.color(egui::Color32::from_gray(150));

        // The ESP toolchain has exactly ONE flashing path, so the SWD vs
        // probe-rs comparison is noise there — it describes hardware the board
        // does not have. Show what espflash actually does instead.
        if *toolchain == ToolchainKind::EspRust {
            let w_val = (total - w_aspect - 20.0).max(120.0);
            for (i, (asp, val)) in ESP_INFO.iter().enumerate() {
                let bg = if i % 2 == 0 {
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6)
                } else {
                    egui::Color32::TRANSPARENT
                };
                egui::Frame::new()
                    .fill(bg)
                    .inner_margin(egui::Margin::symmetric(0, 2))
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            cell(ui, w_aspect, asp, aspect);
                            cell(ui, w_val, val, body);
                        });
                    });
            }
            ui.add_space(6.0);
            for note in ESP_NOTES {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{} {note}", ph::DOT))
                            .size(10.5)
                            .color(egui::Color32::from_gray(140)),
                    )
                    .wrap(),
                );
            }
            esp_selected_programmer_note(ui, selected);
            return;
        }

        ui.horizontal_top(|ui| {
            cell(ui, w_aspect, "", head);
            cell(ui, w_col, "Flash SWD", head);
            cell(ui, w_col, "Flash (probe-rs)", head);
        });
        for (i, (asp, swd, prs)) in FLASH_COMPARISON.iter().enumerate() {
            // Zebra striping, painted behind the row.
            let bg = if i % 2 == 0 {
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6)
            } else {
                egui::Color32::TRANSPARENT
            };
            egui::Frame::new()
                .fill(bg)
                .inner_margin(egui::Margin::symmetric(0, 2))
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        cell(ui, w_aspect, asp, aspect);
                        cell(ui, w_col, swd, body);
                        cell(ui, w_col, prs, body);
                    });
                });
        }
        ui.add_space(6.0);
        for note in FLASH_NOTES {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("{} {note}", ph::DOT))
                        .size(10.5)
                        .color(egui::Color32::from_gray(140)),
                )
                .wrap(),
            );
        }
        // What the programmer picked in the row above is good for.
        if let Some(p) = selected {
            let guidance = p.guidance();
            if !guidance.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!("Selected programmer — {} ({})", p.name, p.kind))
                        .size(11.0)
                        .strong()
                        .color(egui::Color32::from_rgb(200, 210, 230)),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(guidance)
                            .size(10.5)
                            .color(egui::Color32::from_gray(180)),
                    )
                    .wrap(),
                );
            }
        }
    });
}

/// The "Selected programmer — …" footer, shared by both Info variants.
fn esp_selected_programmer_note(ui: &mut egui::Ui, selected: Option<&dfu::ProgrammerInfo>) {
    let Some(p) = selected else { return };
    let guidance = p.guidance();
    if guidance.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(format!("Selected programmer — {} ({})", p.name, p.kind))
            .size(11.0)
            .strong()
            .color(egui::Color32::from_rgb(200, 210, 230)),
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(guidance)
                .size(10.5)
                .color(egui::Color32::from_gray(180)),
        )
        .wrap(),
    );
}

/// `(aspect, what it means)` — the Info panel on the ESP toolchain. There is
/// only one flashing path here, so this describes IT rather than comparing two
/// SWD tools the board has no pins for.
const ESP_INFO: &[(&str, &str)] = &[
    (
        "Tool",
        "espflash — over the serial port, no debug probe involved",
    ),
    (
        "Flash command",
        "espflash flash --chip <chip> --port <port> --ignore-app-descriptor <elf>",
    ),
    (
        "Port comes from",
        "the Programmer row above — pick the board there after a Scan. The monitor \
         reuses whatever port the flash actually used.",
    ),
    (
        "Chip identity comes from",
        "the project's chip name (e.g. esp32c3), passed as --chip",
    ),
    (
        "Needs installed",
        "espflash in PATH  (cargo install espflash)",
    ),
    (
        "Download mode",
        "Boards with an auto-reset circuit enter it themselves. If flashing fails to \
         connect: hold BOOT -> press RESET -> release BOOT, then flash again.",
    ),
    (
        "Monitor",
        "espflash monitor on the same port — it attaches FIRST and resets the chip \
         itself, so the output starts at boot. With `auto` on it opens after every \
         successful flash.",
    ),
];

/// Notes under the ESP table.
const ESP_NOTES: &[&str] = &[
    "Flash builds `cargo build --release` first, and refreshes the Flash/RAM \
     measurement on the right afterwards.",
    "One process at a time owns the serial port: the monitor refuses to start while \
     the Serial tab is connected, and stops itself before a flash.",
    "esp-println picks its output at RUNTIME on the C3/C6/S3: USB Serial/JTAG when a \
     USB host is attached, otherwise UART0. On a board reached through a UART bridge, \
     giving UART0's pins another function on the Pins canvas silences the prints.",
    "SWD / OpenOCD / DFU do not apply here — those are the ARM toolchain's paths.",
];

/// `(aspect, Flash SWD, Flash (probe-rs))` — the comparison table in the Info
/// panel. Both write over the same SWD wires; what differs is the tool driving
/// them and where each gets its configuration.
const FLASH_COMPARISON: &[(&str, &str, &str)] = &[
    ("Tool", "OpenOCD", "probe-rs (`cargo flash`)"),
    (
        "Command",
        "openocd -f <interface.cfg> -f <target.cfg> -c \"program <elf> verify reset exit\"",
        "cargo flash --release --chip <chip> --target <triplet> [--probe VID:PID:Serial]",
    ),
    (
        "Adapter comes from",
        "the Programmer row above — the USB scan (ST-Link / J-Link / CMSIS-DAP)",
        "the Probe row — the SAME probe as the Debug, RTT and Profile tabs",
    ),
    (
        "Chip identity comes from",
        "the target .cfg you type below (e.g. target/stm32f1x.cfg)",
        "the project's probe-rs chip name (e.g. STM32F103C8)",
    ),
    (
        "Needs installed",
        "OpenOCD in PATH",
        "probe-rs-tools in PATH",
    ),
    (
        "Use it when",
        "probe-rs doesn't know your chip, or you need OpenOCD-specific adapter \
         settings (speed, reset sequence, exotic interfaces)",
        "normally — one probe identity across flashing, debugging and logging",
    ),
];

/// Notes under the comparison table.
const FLASH_NOTES: &[&str] = &[
    "Both build `cargo build --release` first, and both refresh the Flash/RAM \
     measurement on the right afterwards.",
    "Only ONE process can hold a probe: stop a Debug or RTT session before \
     flashing, or the flash fails with \"probe in use\". A Flash button turns RED \
     with the reason when that is the case.",
    "DFU is a third path and needs no probe at all: an STM32's own USB bootloader \
     (BOOT0 = 1 + reset) writes the .bin at a flash address. Those fields sit \
     on the row below, for those chips only.",
    "Flash ESP32 (espflash) replaces Flash SWD on the ESP toolchain: it goes over \
     the serial port with the board in download mode, not over SWD.",
];

#[cfg(test)]
mod tests {
    use super::{
        COMBO_MAX_FRACTION, FLASH_W, RIGHT_W, SCAN_W, combo_width, ellipsize, right_block_w,
    };

    /// A ComboBox does not truncate its own text, so a long programmer label
    /// would size the widget past its column and re-widen the editor side panel
    /// every frame. The cut keeps the HEAD (kind + name identify the device) and
    /// must never exceed what the column can show.
    #[test]
    fn combo_labels_are_cut_to_their_column() {
        let long = "[ST-Link] 3 ST-Link v2  [0483:3748] 'STMicroelectronics, 19, STM32 STLink'";
        let cut = ellipsize(long, 200.0);
        assert!(cut.ends_with('…'), "{cut}");
        assert!(cut.chars().count() < long.chars().count());
        assert!(cut.starts_with("[ST-Link]"), "{cut}");
        // Wider column → more text survives; the same text always fits at some
        // width and is then returned untouched.
        assert!(ellipsize(long, 400.0).chars().count() > cut.chars().count());
        assert_eq!(ellipsize(long, 2000.0), long);
        assert_eq!(ellipsize("short", 200.0), "short");
        // A degenerate column doesn't panic or slice mid-char.
        assert!(!ellipsize("ăăăăăăăăăă", 0.0).is_empty());
    }

    /// The columns must FIT at every panel width — content wider than the row is
    /// what pushed the Size button out of the clipped region (and, before the
    /// exact-rect rows, re-widened the editor panel forever). Below
    /// `COMPACT_BELOW` the right-hand block steps aside instead of squeezing the
    /// device list to nothing.
    #[test]
    fn the_row_columns_fit_every_panel_width() {
        for total in [240.0_f32, 360.0, 500.0, 699.0, 700.0, 900.0, 1600.0] {
            let right = right_block_w(total);
            let combo = combo_width(total);
            let used = right + combo + SCAN_W + FLASH_W + 32.0;
            assert!(
                used <= total.max(SCAN_W + FLASH_W + 72.0) + 0.5,
                "columns demand {used} of {total} (right={right}, combo={combo})"
            );
            assert!(combo >= 40.0, "the device list vanished at {total}");
        }
        // The split only happens when both halves stay usable.
        assert_eq!(right_block_w(699.0), 0.0);
        assert_eq!(right_block_w(700.0), RIGHT_W);
        // …and then the list still has room for a real label.
        assert!(combo_width(700.0) >= 140.0, "{}", combo_width(700.0));
    }

    /// The device list never takes more than its share of the panel, however
    /// much room the row has left — a list that eats half the tab is no easier
    /// to read than a truncated one.
    #[test]
    fn the_device_list_is_capped_at_its_share() {
        for total in [700.0_f32, 900.0, 1200.0, 1600.0, 2400.0] {
            let w = combo_width(total);
            assert!(
                w <= total * COMBO_MAX_FRACTION + 0.5,
                "list took {w} of {total}"
            );
        }
        // On a wide panel the cap is what binds, not the leftover space.
        assert!((combo_width(2400.0) - 2400.0 * COMBO_MAX_FRACTION).abs() < 0.5);
    }
}

/// The reported dead end: a red Flash ESP32 whose dialog only said "OK" about
/// the example project's `cfg, data, 0x99` row - and said it with "..".
#[cfg(test)]
mod blocked_dialog_tests {
    use super::{blocked_dialog, blocked_dialog_id, take_partition_fix_request};
    use eframe::egui;

    const WHAT: &str = "Flash ESP32";
    const WHY: &str = "partitions.csv: Line 5: espflash cannot read the data subtype '0x99' (it stops with a panic); use 'undefined'.";
    const FIX: &str = "line 5: '0x99' -> 'undefined'";

    fn frame(
        ctx: &egui::Context,
        pass: u64,
        events: Vec<egui::Event>,
        fix: Option<(&str, &str)>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            time: Some(pass as f64 / 30.0),
            predicted_dt: 1.0 / 30.0,
            events,
            ..Default::default()
        };
        crate::headless::run_ui(ctx, input, |ui| blocked_dialog(ui, fix))
    }

    fn painted(out: &egui::FullOutput, needle: &str) -> Option<egui::Rect> {
        fn walk(s: &egui::Shape, needle: &str) -> Option<egui::Rect> {
            match s {
                egui::Shape::Text(t) if t.galley.text().contains(needle) => {
                    Some(t.visual_bounding_rect())
                }
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                _ => None,
            }
        }
        out.shapes.iter().find_map(|c| walk(&c.shape, needle))
    }

    fn click(at: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            },
        ]
    }

    fn open(ctx: &egui::Context, reason: &str) {
        ctx.data_mut(|d| d.insert_temp(blocked_dialog_id(), (WHAT.to_owned(), reason.to_owned())));
    }

    #[test]
    fn the_dialog_offers_the_repair_for_its_own_block_and_reports_the_click() {
        let ctx = egui::Context::default();
        open(&ctx, WHY);
        let fix = Some((WHY, FIX));
        let mut out = frame(&ctx, 0, vec![], fix);
        for pass in 1..4 {
            out = frame(&ctx, pass, vec![], fix);
        }
        assert!(
            painted(&out, "'undefined'.").is_some(),
            "the reason is shown"
        );
        assert!(painted(&out, "'undefined'..").is_none(), "one full stop");
        let at = painted(&out, "Fix partitions.csv")
            .expect("the repair is offered")
            .center();
        frame(&ctx, 4, click(at, true), fix);
        frame(&ctx, 5, click(at, false), fix);
        assert!(
            take_partition_fix_request(&ctx),
            "the click reaches the app"
        );
        assert!(!take_partition_fix_request(&ctx), "and is taken once");
        assert!(
            ctx.data(|d| d.get_temp::<(String, String)>(blocked_dialog_id()))
                .is_none(),
            "the dialog closes"
        );
    }

    /// Opened for another reason (a missing tool), or for a table that has
    /// changed since: no repair button.
    #[test]
    fn no_repair_is_offered_for_another_reason() {
        for (reason, fix) in [("espflash is not installed", Some((WHY, FIX))), (WHY, None)] {
            let ctx = egui::Context::default();
            open(&ctx, reason);
            let mut out = frame(&ctx, 0, vec![], fix);
            for pass in 1..4 {
                out = frame(&ctx, pass, vec![], fix);
            }
            assert!(painted(&out, "can't run").is_some(), "{reason}");
            assert!(painted(&out, "Fix partitions.csv").is_none(), "{reason}");
        }
    }
}
