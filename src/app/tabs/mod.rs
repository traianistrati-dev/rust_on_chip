//! UI tabs for diagnostics and MCU configuration panels.

pub mod activity_tab;
pub mod cargo_tab;
pub mod clippy_tab;
pub mod configuration_tab;
pub mod debug_tab;
pub mod dfu_tab;
pub mod git_tab;
pub mod iot_tab;
pub mod mcu_tab;
pub mod profile_tab;
pub mod ra_tab;
pub mod rtt_tab;
pub mod serial_info;
pub mod serial_tab;
pub mod terminal_tab;
pub mod tools_tab;

// ── Feature gating on missing tools ──────────────────────────────────────────

/// `true` when `tool` is in the CONFIRMED-unavailable list (see
/// [`crate::required_tools::ToolsState::unavailable`]) — i.e. a button that
/// shells out to it should be greyed out. An empty list (the normal case, and
/// also the state before the startup check finishes) never gates anything.
pub(crate) fn tool_missing(missing_tools: &[&'static str], tool: &str) -> bool {
    missing_tools.iter().any(|m| *m == tool)
}

/// Standard hover text for a button disabled because its tool is absent — one
/// wording everywhere, always naming the fix.
pub(crate) fn needs_tool_hint(tool: &str) -> String {
    format!("Needs `{tool}` — it is missing. Install it from the Tools tab.")
}

// Re-export all tab functions for convenience
pub use activity_tab::show_activity_tab;
pub use cargo_tab::show_cargo_tab;
pub use clippy_tab::show_clippy_tab;
pub use debug_tab::show_debug_tab;
pub use dfu_tab::show_dfu_tab;
pub use git_tab::show_git_tab;
pub use mcu_tab::show_peripherals_tab;
pub use profile_tab::show_profile_tab;
pub use ra_tab::show_ra_tab;
pub use rtt_tab::show_rtt_tab;
pub use serial_tab::show_serial_tab;
pub use terminal_tab::show_terminal_tab;
pub use tools_tab::show_tools_tab;

use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
use eframe::egui;
use egui_phosphor::regular as ph;

/// What to do when `probe-rs list` comes back empty. One wording, shown by the
/// probe picker itself (so every tab that drives a probe says it) and quoted in
/// the Debug tab's help.
pub(crate) const NO_PROBE_HINT: &str = "No debug probe detected.\n\n\
     Press Scan to re-run `probe-rs list`. If it still doesn't appear, unplug the probe and \
     plug it back in — one left mid-session (a killed debugger, a target that hung) keeps \
     answering nothing until the USB stack re-enumerates it.\n\
     Check too that nothing else holds it: another IDE, STM32CubeProgrammer, OpenOCD, or a \
     Debug/RTT session in this one.";

/// What to show when the probe list comes back empty: a short line for the
/// toolbar and the long form for its hover.
///
/// When the IDE itself is holding the device, SAY SO. The generic advice tells
/// the user to replug the board, which does nothing about an `espflash monitor`
/// this IDE started on its own after the last flash - and on an Espressif
/// project that is the default path, so the wrong advice is also the common
/// one. The Serial tab has named its holder for a while; this is the same fact
/// reaching the other tab that needed it.
pub(crate) fn no_probe_message(
    holder: Option<(&str, &str)>,
    toolchain: &ToolchainKind,
) -> (String, String, bool) {
    // An ESP board that vanishes from the bus is NOT the same failure as an
    // absent ST-Link, and the generic advice does not cover it.
    //
    // The ESP32-C3's USB-Serial/JTAG is a peripheral OF THE CHIP, clocked and
    // powered by whatever firmware is running - unlike an ST-Link, which is a
    // separate device that enumerates no matter what the target does. Flash a
    // program that sleeps deeply, gates the USB clock, or panics before it
    // reaches USB init, and the whole device drops off USB: no port, no probe,
    // nothing for `probe-rs list` or the programmer scan to find.
    //
    // The ROM bootloader always enumerates, so download mode is the way back in
    // - and it is also the test that tells a dead firmware apart from a dead
    // cable, which is why the steps are here rather than in a wiki nobody opens.
    let esp_recovery = matches!(toolchain, ToolchainKind::EspRust);
    match holder {
        Some((port, who)) if !port.is_empty() => (
            format!("{who} holds {port}"),
            format!(
                "No probe listed, but this IDE is holding the device: {who} is on {port}.

                 A serial port has one owner, so `probe-rs list` cannot see the board while                  that session runs. Stop it in the Flash tab - or turn off the Monitor's                  auto-start after flashing - and Scan again.

                 Replugging will not help while the holder is still running."
            ),
            true,
        ),
        _ if esp_recovery => (
            "no board — try download mode".to_owned(),
            concat!(
                "No ESP board detected.\n\n",
                "Put the board in download mode: hold BOOT, press and release RESET, ",
                "then release BOOT. Then press Scan again.\n\n",
                "Why this works: the ESP32's USB-Serial/JTAG belongs to the chip, not to a ",
                "separate programmer, so the firmware you flashed can take it off the USB ",
                "bus entirely — by sleeping, by gating the USB clock, or by panicking ",
                "before it initialises USB. The ROM bootloader always enumerates, so if the ",
                "board appears in download mode the board is fine and the firmware is the ",
                "cause.\n\n",
                "If it does NOT appear in download mode either, it is physical: a ",
                "charge-only USB cable (very common, and impossible to spot by eye), the ",
                "board's other USB socket (many boards have one wired to the chip and one ",
                "to a UART bridge or to power alone), or a hub — try a different cable, ",
                "straight into the computer."
            )
            .to_owned(),
            true,
        ),
        _ => (
            "no probe — Scan, or replug it".to_owned(),
            NO_PROBE_HINT.to_owned(),
            false,
        ),
    }
}

pub(crate) use crate::probe::probe_compatible;

/// The shared probe picker rendered on both the RTT and Debug tabs (both drive
/// probe-rs). `Scan` re-runs `probe-rs list`; the ComboBox pins the session to
/// one probe via `--probe VID:PID[:Serial]`, or "Auto" to let probe-rs choose.
/// Probes incompatible with the project chip's toolchain are shown greyed and
/// can't be selected (like the Flash tab's programmer list). Meant to sit
/// inside a `horizontal_wrapped` toolbar row.
pub(crate) fn probe_selector_ui(
    ui: &mut egui::Ui,
    probes: &[crate::probe::ProbeInfo],
    selected: &mut Option<String>,
    // Set true when the user clicks Scan; the caller runs `scan_probes`.
    scan_go: &mut bool,
    scan_err: Option<&str>,
    toolchain: &ToolchainKind,
    // Who inside the IDE holds the device, if anyone - see [`no_probe_message`].
    holder: Option<(&str, &str)>,
) {
    probe_selector_ui_with(
        ui,
        "Probe:",
        probes,
        selected,
        scan_go,
        scan_err,
        toolchain,
        0.0,
        0.0,
        true, // Auto is a fine default for a session you can stop
        holder,
        |_| {},
    );
}

/// [`probe_selector_ui`] with a caller-chosen label, fixed Scan/ComboBox widths
/// (`0.0` = size to content, what the RTT and Debug toolbars use) and room for a
/// trailing widget AFTER the list — the Flash tab puts its "Flash (probe-rs)"
/// there, so its Probe row reads `[Scan] <list> [Flash]` like the Programmer row
/// above it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn probe_selector_ui_with(
    ui: &mut egui::Ui,
    label: &str,
    probes: &[crate::probe::ProbeInfo],
    selected: &mut Option<String>,
    scan_go: &mut bool,
    scan_err: Option<&str>,
    toolchain: &ToolchainKind,
    scan_w: f32,
    combo_w: f32,
    // Offer "Auto (first found)"? The Flash tab says NO: `cargo flash` with an
    // ambiguous probe doesn't fail, it SITS there — and a flash that hangs
    // forever is worse than one that refuses to start. Debug/RTT keep Auto,
    // which is the right default with a single probe attached.
    allow_auto: bool,
    // Who inside the IDE holds the device, if anyone - see [`no_probe_message`].
    holder: Option<(&str, &str)>,
    trailing: impl FnOnce(&mut egui::Ui),
) {
    if !label.is_empty() {
        ui.label(
            egui::RichText::new(label)
                .size(10.5)
                .color(egui::Color32::GRAY),
        );
    }

    // The label for the currently selected probe (or Auto).
    let current = selected
        .as_ref()
        .and_then(|sel| probes.iter().find(|p| &p.selector == sel))
        .map(|p| p.combo_label())
        .or_else(|| {
            // Selected but not in the list (not scanned yet / unplugged): show
            // the raw selector so the choice is still visible.
            selected.as_ref().map(|s| format!("· {s}"))
        })
        .unwrap_or_else(|| {
            if allow_auto {
                "Auto (first found)".to_owned()
            } else {
                "— select a probe —".to_owned()
            }
        });

    // Never wider than what is left: a widget that out-demands its region
    // re-widens the Code Editor side panel every frame (see `debug_tab`'s pane
    // note). A caller-given `combo_w` wins, so two rows can share one column.
    let width = if combo_w > 0.0 {
        combo_w
    } else {
        ui.available_width().min(320.0).max(0.0)
    };
    egui::ComboBox::from_id_salt("probe_selector")
        .selected_text(
            egui::RichText::new(crate::app::tabs::dfu_tab::ellipsize(&current, width))
                .size(10.5)
                .monospace(),
        )
        .width(width)
        .show_ui(ui, |ui| {
            if allow_auto
                && ui
                    .selectable_label(selected.is_none(), "Auto (first found)")
                    .clicked()
            {
                *selected = None;
            }
            for p in probes {
                let is_sel = selected.as_deref() == Some(p.selector.as_str());
                let compatible = probe_compatible(&p.kind, toolchain);
                // Incompatible probes are greyed + disabled, like the Flash
                // tab's programmer list.
                let color = if compatible {
                    egui::Color32::from_gray(210)
                } else {
                    egui::Color32::from_gray(90)
                };
                let resp = ui.add_enabled(
                    compatible,
                    egui::Button::selectable(
                        is_sel,
                        egui::RichText::new(p.combo_label()).size(10.5).color(color),
                    ),
                );
                if resp.clicked() && compatible {
                    *selected = Some(p.selector.clone());
                }
                if !compatible {
                    resp.on_hover_text("Not compatible with this chip's toolchain.");
                }
            }
            if probes.is_empty() {
                let (short, hover, actionable) = no_probe_message(holder, toolchain);
                ui.label(
                    egui::RichText::new(if actionable {
                        short
                    } else {
                        "No probes — click Scan.".to_owned()
                    })
                    .size(10.5)
                    .italics()
                    .color(egui::Color32::from_gray(140)),
                )
                .on_hover_text(hover);
            }
        })
        .response
        .on_hover_text(format!(
            "{current}\n\nWhich debug probe to use when several are attached. Auto lets \
             probe-rs pick the only one — ambiguous with more than one."
        ));

    // Scan sits AFTER the list, next to whatever the caller puts behind it —
    // on the Flash tab that is the Flash button, and the two actions you use
    // together should not be a row apart.
    let mut scan =
        egui::Button::new(egui::RichText::new(format!("{} Scan", ph::MAGNIFYING_GLASS)).size(10.5));
    if scan_w > 0.0 {
        scan = scan.min_size(egui::vec2(scan_w, ui.spacing().interact_size.y));
    }
    if ui
        .add(scan)
        .on_hover_text("Enumerate connected debug probes (`probe-rs list`).")
        .clicked()
    {
        *scan_go = true;
    }

    // Nothing on the list: say what to do, compactly. The full advice is on the
    // hover — this sits in three different toolbars, one of which is a
    // fixed-width row, so it cannot be a paragraph.
    if probes.is_empty() {
        let (short, hover, actionable) = no_probe_message(holder, toolchain);
        // Amber when there is something to DO — the IDE holds the port, or an
        // ESP board needs download mode — rather than the usual blue "nothing
        // attached", which carries no next step.
        let colour = if actionable {
            egui::Color32::from_rgb(220, 180, 90)
        } else {
            egui::Color32::from_rgb(150, 175, 205)
        };
        ui.label(
            egui::RichText::new(format!("{} {short}", ph::INFO))
                .size(10.5)
                .color(colour),
        )
        .on_hover_text(hover);
    }

    // Whatever the caller wants after the list (the Flash tab's Flash button).
    trailing(ui);

    if let Some(err) = scan_err {
        // A tagged failure (a probe-rs crash) carries a whole explanation — the
        // toolbar shows its first line, the full text sits on the hover.
        let plain = crate::failure_hint::strip(err);
        ui.label(
            egui::RichText::new(format!(
                "{} {}",
                ph::WARNING,
                plain.lines().next().unwrap_or(plain)
            ))
            .size(10.5)
            .color(egui::Color32::from_rgb(210, 150, 90)),
        )
        .on_hover_text(plain);
    }
}

#[cfg(test)]
mod no_probe_message_tests {
    use super::no_probe_message;

    /// An empty bench keeps the old advice: replug, check for other tools.
    #[test]
    fn nothing_held_gives_the_generic_advice() {
        let (short, hover, _) = no_probe_message(
            None,
            &crate::panels::mcu_module::mcu_catalog::ToolchainKind::RustEmbedded,
        );
        assert!(short.contains("Scan"), "{short}");
        assert!(hover.contains("unplug"), "{hover}");
    }

    /// When the IDE holds the device, the message says WHO and WHERE - and
    /// stops telling the user to replug, which cannot help.
    ///
    /// This is the whole point: after an ESP flash the Monitor starts itself on
    /// the same port, `probe-rs list` then sees nothing, and the old text sent
    /// the user to the cable while the fix was a button in the Flash tab.
    #[test]
    fn a_holder_is_named_and_replugging_is_not_suggested() {
        for who in ["espflash", "The ESP Monitor"] {
            let (short, hover, _) = no_probe_message(
                Some(("COM7", who)),
                &crate::panels::mcu_module::mcu_catalog::ToolchainKind::RustEmbedded,
            );
            assert!(short.contains(who), "{short}");
            assert!(short.contains("COM7"), "{short}");
            assert!(hover.contains(who) && hover.contains("COM7"), "{hover}");
            assert!(
                hover.contains("Replugging will not help"),
                "the wrong advice must be contradicted, not just omitted: {hover}"
            );
        }
    }

    /// A holder with an EMPTY port is nobody - the generic advice, not
    /// "`The ESP Monitor` holds ".
    #[test]
    fn an_empty_port_is_not_a_holder() {
        let (short, _, _) = no_probe_message(
            Some(("", "The ESP Monitor")),
            &crate::panels::mcu_module::mcu_catalog::ToolchainKind::RustEmbedded,
        );
        assert!(!short.contains("ESP Monitor"), "{short}");
        assert_eq!(
            short,
            no_probe_message(
                None,
                &crate::panels::mcu_module::mcu_catalog::ToolchainKind::RustEmbedded
            )
            .0
        );
    }
}

#[cfg(test)]
mod esp_download_mode_tests {
    use super::no_probe_message;
    use crate::panels::mcu_module::mcu_catalog::ToolchainKind;

    /// An empty ESP bench gets the download-mode steps, in order.
    ///
    /// The exact sequence matters - BOOT down, RESET pressed AND released, BOOT
    /// up - because doing it in any other order just resets the board back into
    /// the firmware that made it vanish.
    #[test]
    fn an_esp_board_is_told_how_to_reach_download_mode() {
        let (short, hover, actionable) = no_probe_message(None, &ToolchainKind::EspRust);
        assert!(
            actionable,
            "there is a next step, so it must not read as idle"
        );
        assert!(short.contains("download mode"), "{short}");
        let boot = hover.find("hold BOOT").expect("holds BOOT first");
        let reset = hover.find("press and release RESET").expect("then RESET");
        let release = hover.find("release BOOT").expect("then lets BOOT go");
        assert!(
            boot < reset && reset < release,
            "the order is the instruction"
        );
    }

    /// And it says what a NEGATIVE result means - the cable, not the firmware.
    #[test]
    fn it_covers_the_case_where_download_mode_also_fails() {
        let (_, hover, _) = no_probe_message(None, &ToolchainKind::EspRust);
        assert!(
            hover.contains("charge-only"),
            "the commonest physical cause: {hover}"
        );
    }

    /// An ARM board must NOT get ESP advice: it has no BOOT/RESET dance, and a
    /// probe that is genuinely absent is a different problem.
    #[test]
    fn an_arm_board_keeps_the_generic_advice() {
        let (short, hover, actionable) = no_probe_message(None, &ToolchainKind::RustEmbedded);
        assert!(!actionable);
        assert!(!short.contains("download mode"), "{short}");
        assert!(!hover.contains("BOOT"), "{hover}");
    }

    /// A holder outranks the ESP advice: when the IDE itself has the port, the
    /// board never left the bus and download mode would be the wrong move.
    #[test]
    fn a_holder_outranks_the_download_mode_advice() {
        let (short, hover, _) =
            no_probe_message(Some(("COM7", "The ESP Monitor")), &ToolchainKind::EspRust);
        assert!(short.contains("ESP Monitor"), "{short}");
        assert!(!hover.contains("BOOT"), "{hover}");
    }
}
