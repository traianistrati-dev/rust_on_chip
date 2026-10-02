//! "New MCU definition" dialog — the visual editor for a [`McuDefinition`].
//!
//! Renders the pure [`McuForm`] model, shows live validation, and on Save
//! writes the `.ron` into the user `mcus/` folder + merges it into the live
//! registry (`registry::save_definition` + `merge_def`) — so a form-authored
//! chip appears in the New Project chip list immediately and survives restarts.

use super::AppIde;
use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
use crate::panels::mcu_module::mcu_form::{self, McuForm, PinRow, SIDE_DISPLAY_ORDER, SIDES};
use crate::panels::mcu_module::registry;
use eframe::egui;
use egui_phosphor::regular as ph;

impl AppIde {
    /// Open the form empty, or seeded from an existing definition (Clone/Edit).
    pub(crate) fn open_mcu_form(&mut self, seed: Option<McuForm>) {
        self.mcu_form = Some(seed.unwrap_or_else(McuForm::empty));
        self.mcu_form_clock_note = None;
        // Reopen at the normal size — reset both the maximize STATE and the
        // first-frame force (which overrides egui's persisted window rect).
        self.mcu_form_maximized = false;
        self.mcu_form_shown_once = false;
    }

    /// Render the form window. No-op while it is closed.
    pub(super) fn show_mcu_form_dialog(&mut self, ui: &egui::Ui) {
        let Some(mut form) = self.mcu_form.take() else {
            return;
        };
        let mut keep_open = true;
        let mut do_save = false;
        let mut want_open_import = false;
        let mut want_open_clock_import = false;
        // Clock Import/Export feedback — a local so the window closure never
        // borrows `self`; written back after `.show()`.
        let mut clock_note = self.mcu_form_clock_note.take();
        // Window maximize state, via a local (the closure can't borrow `self`).
        let mut maximized = self.mcu_form_maximized;
        let force_default =
            !self.mcu_form_shown_once || (self.mcu_form_prev_maximized && !maximized);
        self.mcu_form_prev_maximized = maximized;
        self.mcu_form_shown_once = true;

        let title = if form.editing {
            "Edit MCU definition"
        } else {
            "New MCU definition"
        };
        super::datasheet_import_dialog::window_frame(
            ui.ctx(), title, maximized, force_default, 680.0, 560.0, 0.0,
        )
        .show(ui.ctx(), |ui| {
            super::datasheet_import_dialog::maximize_button(ui, &mut maximized);
            let errors = form.errors();
            let warnings = form.warnings();

            egui::ScrollArea::vertical()
                .max_height(ui.ctx().content_rect().height() * 0.62)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                egui::RichText::new(format!(
                                    "{} Import from datasheet (AI)…",
                                    ph::SPARKLE
                                ))
                                .color(egui::Color32::from_rgb(150, 200, 255)),
                            )
                            .on_hover_text(
                                "Paste a datasheet pin table and let Claude fill this form",
                            )
                            .clicked()
                        {
                            want_open_import = true;
                        }
                        ui.label(
                            egui::RichText::new("fills the fields below for you to review")
                                .size(10.0)
                                .color(egui::Color32::from_gray(140)),
                        );
                    });
                    ui.add_space(4.0);

                    section(ui, "Identity");
                    egui::Grid::new("mcu_form_identity")
                        .num_columns(2)
                        .spacing([10.0, 5.0])
                        .show(ui, |ui| {
                            labeled(
                                ui,
                                "Id",
                                &mut form.id,
                                "lowercase a-z 0-9 _ — file name + registry key",
                            );
                            labeled(
                                ui,
                                "Display name",
                                &mut form.display_name,
                                "shown in the chip selector",
                            );
                            labeled(
                                ui,
                                "Family",
                                &mut form.family,
                                "codegen/clock backend key, e.g. stm32f1",
                            );
                            labeled(ui, "CPU", &mut form.cpu, "e.g. Cortex-M3 (label only)");
                            labeled(
                                ui,
                                "Package *",
                                &mut form.package,
                                "e.g. UFQFPN48 — set this before importing pins from a datasheet \
                                 (it selects the right pin-number column)",
                            );
                        });

                    // Fill the boring identity fields from the chip name.
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                egui::RichText::new(format!(
                                    "{} Auto-fill from name",
                                    ph::MAGIC_WAND
                                ))
                                .size(11.0),
                            )
                            .on_hover_text(
                                "Set Family / CPU / Toolchain / Target / HAL line from the chip \
                                 name, and the clock tree and memory origins when it names \
                                 another family \
                                 (e.g. STM32WBA55CG -> stm32wba · Cortex-M33 · thumbv8m.main-none-eabihf)",
                            )
                            .clicked()
                        {
                            form.auto_fill_identity();
                        }
                        ui.label(
                            egui::RichText::new(
                                "STM32 names only: fills Family, CPU, Toolchain, Target, \
                                 HAL line, clock tree and memory origins",
                            )
                            .size(10.0)
                            .color(egui::Color32::from_gray(140)),
                        );
                    });

                    ui.add_space(6.0);
                    section(ui, "Toolchain & target");
                    ui.horizontal(|ui| {
                        ui.label("Toolchain:");
                        egui::ComboBox::from_id_salt("mcu_form_toolchain")
                            .selected_text(toolchain_label(&form.toolchain))
                            .show_ui(ui, |ui| {
                                for tc in [
                                    ToolchainKind::RustEmbedded,
                                    ToolchainKind::EspRust,
                                    ToolchainKind::SdccC,
                                ] {
                                    ui.selectable_value(
                                        &mut form.toolchain,
                                        tc.clone(),
                                        toolchain_label(&tc),
                                    );
                                }
                            });
                        ui.add_space(8.0);
                        ui.label("Target:");
                        ui.add(egui::TextEdit::singleline(&mut form.target).desired_width(220.0))
                            .on_hover_text("Rust target triple, e.g. thumbv7m-none-eabi");
                    });

                    let arm = form.toolchain == ToolchainKind::RustEmbedded;
                    ui.add_space(6.0);
                    section(ui, "Memory & probe");
                    ui.add_enabled_ui(arm, |ui| {
                        egui::Grid::new("mcu_form_mem")
                            .num_columns(4)
                            .spacing([8.0, 5.0])
                            .show(ui, |ui| {
                                // Sized, not `desired_width`: a Grid offers a cell
                                // that is not in the last column only the width
                                // the column had last frame, which starts at the
                                // minimum. The edit never got to ask for more, and
                                // `0x20000000` showed as `0x200`.
                                let origin_size = [110.0, ui.spacing().interact_size.y];
                                ui.label("Flash origin");
                                ui.add_sized(
                                    origin_size,
                                    egui::TextEdit::singleline(&mut form.flash_origin),
                                );
                                ui.label("Flash size");
                                ui.add(
                                    egui::TextEdit::singleline(&mut form.flash_size)
                                        .desired_width(80.0),
                                );
                                ui.end_row();
                                ui.label("RAM origin");
                                ui.add_sized(
                                    origin_size,
                                    egui::TextEdit::singleline(&mut form.ram_origin),
                                );
                                ui.label("RAM size");
                                ui.add(
                                    egui::TextEdit::singleline(&mut form.ram_size)
                                        .desired_width(80.0),
                                );
                                ui.end_row();
                            });
                        ui.horizontal(|ui| {
                            ui.label("Probe chip:");
                            ui.add(
                                egui::TextEdit::singleline(&mut form.probe_chip)
                                    .desired_width(180.0),
                            )
                            .on_hover_text("probe-rs chip name — check with `probe-rs chip list`");
                        });
                    });
                    if !arm {
                        ui.label(
                            egui::RichText::new(
                                "ESP toolchain: memory + probe are managed by esp-hal / espflash.",
                            )
                            .size(10.5)
                            .color(egui::Color32::from_gray(140))
                            .italics(),
                        );
                    }

                    ui.add_space(6.0);
                    section(ui, "Dependency & clock");
                    ui.label(
                        egui::RichText::new("HAL / crate dependency line (Cargo.toml):").size(11.0),
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut form.hal_dep)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Async projects use this line instead (leave empty for one crate):",
                        )
                        .size(11.0),
                    )
                    .on_hover_text(
                        "For a family whose Async runtime needs a different HAL crate, such as \
                         embassy-rp or embassy-nrf. It replaces the line above in an Async \
                         project's Cargo.toml.",
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut form.hal_dep_async)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Clock model:");
                        egui::ComboBox::from_id_salt("mcu_form_clock")
                            .selected_text(form.clock.label())
                            .show_ui(ui, |ui| {
                                for c in mcu_form::ClockChoice::ALL {
                                    ui.selectable_value(&mut form.clock, c, c.label());
                                }
                            });
                        // A graph attached from a .ron overrides the dropdown
                        // (the dropdown shows None while it is in effect).
                        if form.imported_clock.is_some()
                            && form.clock == mcu_form::ClockChoice::None
                        {
                            ui.label(
                                egui::RichText::new(format!("{} imported .ron", ph::FILE))
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(150, 200, 150)),
                            );
                        }
                    });
                    // Import / export the clock tree as a standalone .ron — the
                    // data form a new family (or an AI extraction) is authored
                    // in, without a recompile.
                    ui.horizontal(|ui| {
                        use crate::panels::mcu_module::clock::graph as cg;
                        if ui
                            .button(format!("{} Import clock .ron…", ph::DOWNLOAD_SIMPLE))
                            .on_hover_text(
                                "Load a clock tree (GraphClock or bare ClockGraph) from a .ron file and attach it to this chip. It is validated before import.",
                            )
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("RON", &["ron"])
                                .pick_file()
                            {
                                clock_note = Some(match std::fs::read_to_string(&path) {
                                    Ok(text) => match cg::parse_clock_ron(&text) {
                                        Ok(gc) => {
                                            let n = gc.graph.nodes.len();
                                            form.set_imported_clock(gc);
                                            Ok(format!("Imported clock ({n} nodes)."))
                                        }
                                        Err(e) => Err(e),
                                    },
                                    Err(e) => Err(format!("Could not read the file: {e}")),
                                });
                            }
                        }
                        // The best path for an ST part: the chip's own pin-data
                        // XML names which CubeMX files describe ITS clock tree
                        // and which conditional branches it has, so this is a
                        // per-chip lookup with nothing guessed.
                        if ui
                            .button(format!("{} STM32 chip XML (+ CubeMX)…", ph::CPU))
                            .on_hover_text(
                                "Pick a chip from STM32_open_pin_data (mcu/STM32….xml). Its \
                                 ClockTree / RCC version / peripheral list select the exact \
                                 CubeMX files, which must be installed.",
                            )
                            .clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("STM32 chip XML", &["xml"])
                                .pick_file()
                        {
                            clock_note = Some(import_chip_clock(&path, &mut form));
                        }
                        // Fallback when there is no pin-data repo: pick the
                        // family's clock file directly.
                        if ui
                            .button(format!("{} CubeMX clock XML…", ph::FILE_CODE))
                            .on_hover_text(
                                "Import a clock tree from an STM32CubeMX installation: pick \
                                 db/plugins/clock/STM32<FAMILY>.xml — the matching RCC parameter \
                                 file is found next to it.",
                            )
                            .clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("CubeMX clock XML", &["xml"])
                                .pick_file()
                        {
                            clock_note = Some(import_cubemx_clock(&path, &mut form));
                        }
                        if ui
                            .button(format!("{} Extract from datasheet (AI)…", ph::SPARKLE))
                            .on_hover_text(
                                "Extract the clock tree from a datasheet PDF or pasted text with an AI, verified against the datasheet's own default SYSCLK.",
                            )
                            .clicked()
                        {
                            want_open_clock_import = true;
                        }
                        let effective = form.effective_clock();
                        let is_graph =
                            matches!(effective, crate::panels::mcu_module::mcu_def::ClockDef::Graph(_));
                        if ui
                            .add_enabled(
                                is_graph,
                                egui::Button::new(format!("{} Export clock .ron…", ph::UPLOAD_SIMPLE)),
                            )
                            .on_disabled_hover_text(
                                "Pick a clock tree (or import one) first — only graph clocks can be exported.",
                            )
                            .on_hover_text("Save this chip's clock tree as a .ron template.")
                            .clicked()
                        {
                            if let crate::panels::mcu_module::mcu_def::ClockDef::Graph(gc) = &effective
                            {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("RON", &["ron"])
                                    .set_file_name("clock.ron")
                                    .save_file()
                                {
                                    let text = cg::export_clock_ron(gc);
                                    clock_note = Some(match std::fs::write(&path, text) {
                                        Ok(()) => Ok(format!(
                                            "Exported to {}",
                                            path.file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_default()
                                        )),
                                        Err(e) => Err(format!("Could not write the file: {e}")),
                                    });
                                }
                            }
                        }
                        if form.imported_clock.is_some() {
                            if ui
                                .button(format!("{} Clear import", ph::X))
                                .on_hover_text("Drop the imported clock and use the dropdown again")
                                .clicked()
                            {
                                form.imported_clock = None;
                                clock_note = None;
                            }
                        }
                    });
                    if let Some(note) = &clock_note {
                        let (msg, col) = match note {
                            Ok(m) => (m.as_str(), egui::Color32::from_rgb(150, 200, 150)),
                            Err(m) => (m.as_str(), egui::Color32::from_rgb(220, 120, 100)),
                        };
                        ui.label(egui::RichText::new(msg).size(10.5).color(col));
                    }

                    ui.add_space(6.0);
                    section(ui, "Pins");
                    ui.label(
                        egui::RichText::new(format!(
                            "Function tokens:  {}",
                            mcu_form::FUNCTION_TOKEN_HELP
                        ))
                        .size(10.0)
                        .color(egui::Color32::from_gray(140)),
                    );
                    // A move touches two sides, so it's collected here and
                    // applied after the loop, once no side is borrowed.
                    let mut pending_move: Option<(usize, usize, usize)> = None;
                    let mut pending_reorder: Option<(usize, usize, isize)> = None;
                    // Presented Left → Bottom → Right → Top so the editors read
                    // in QFP pin-number order (see `SIDE_DISPLAY_ORDER`).
                    for &si in &SIDE_DISPLAY_ORDER {
                        let (mut mv, mut ro) = (None, None);
                        pin_side_editor(ui, si, SIDES[si], &mut form.pins[si], &mut mv, &mut ro);
                        if let Some((idx, to)) = mv {
                            pending_move = Some((si, idx, to));
                        }
                        if let Some((idx, delta)) = ro {
                            pending_reorder = Some((si, idx, delta));
                        }
                    }
                    if let Some((from, idx, to)) = pending_move {
                        form.move_pin(from, idx, to);
                    }
                    if let Some((si, idx, delta)) = pending_reorder {
                        form.reorder_pin(si, idx, delta);
                    }
                });

            // ── Validation feedback ────────────────────────────────────────
            ui.separator();
            // In a scroll area of its own, with a ceiling. The list has no
            // bound - one line per bad token per pin - and unbounded it grew
            // the window past the screen: a chip with several hundred of them
            // put Save, Cancel and the title bar out of reach, and a modal
            // that cannot be closed takes the whole app with it. With the
            // form's 62% above, this leaves the buttons on screen.
            if errors.len() > 1 {
                ui.label(
                    egui::RichText::new(format!("{} errors", errors.len()))
                        .size(10.5)
                        .color(egui::Color32::from_rgb(230, 110, 90)),
                );
            }
            egui::ScrollArea::vertical()
                .id_salt("mcu_form_feedback")
                .max_height(ui.ctx().content_rect().height() * 0.18)
                .show(ui, |ui| {
                    for w in &warnings {
                        ui.label(
                            egui::RichText::new(format!("{} {w}", ph::WARNING))
                                .size(10.5)
                                .color(egui::Color32::from_rgb(220, 170, 70)),
                        );
                    }
                    for e in &errors {
                        ui.label(
                            egui::RichText::new(format!("{} {e}", ph::X_CIRCLE))
                                .size(10.5)
                                .color(egui::Color32::from_rgb(230, 110, 90)),
                        );
                    }
                });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let can_save = errors.is_empty();
                if ui
                    .add_enabled(
                        can_save,
                        egui::Button::new(
                            egui::RichText::new(format!("{} Save definition", ph::FLOPPY_DISK))
                                .color(if can_save {
                                    egui::Color32::from_rgb(120, 210, 120)
                                } else {
                                    egui::Color32::GRAY
                                }),
                        ),
                    )
                    .on_hover_text(
                        "Write <id>.ron to the user mcus/ folder and add it to the chip list",
                    )
                    .clicked()
                {
                    do_save = true;
                }
                if ui.button("Cancel").clicked() {
                    keep_open = false;
                }
                if !errors.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("{} error(s) to fix", errors.len()))
                            .size(10.5)
                            .color(egui::Color32::from_gray(150)),
                    );
                }
            });
        });

        // ── Save handling ──────────────────────────────────────────────────
        if do_save {
            let def = form.to_definition();
            match registry::save_definition(&def) {
                Ok(path) => {
                    registry::merge_def(&mut self.mcu_registry, def.clone());
                    self.export_msg = format!(
                        "{}  MCU '{}' saved to {}",
                        ph::CHECK_CIRCLE,
                        def.display_name,
                        path.display()
                    );
                    keep_open = false;
                }
                Err(e) => {
                    self.export_msg = format!("{}  Could not save MCU: {e}", ph::X_CIRCLE);
                }
            }
            self.export_status_until =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
        }

        // The clock Import/Export note survives across frames while the form
        // is open (a save/close clears it via `open_mcu_form`).
        self.mcu_form_clock_note = clock_note;
        self.mcu_form_maximized = maximized;

        // Keep the (mutated) form unless it was closed / saved.
        if keep_open {
            // Open / render the AI import sub-dialog while we still hold the
            // form mutably (a finished extraction patches it in place).
            if want_open_import && self.datasheet_import.is_none() {
                self.open_datasheet_import(&form);
            }
            self.show_datasheet_import(ui, &mut form);
            if want_open_clock_import && self.clock_import.is_none() {
                self.open_clock_import();
            }
            self.show_clock_import(ui, &mut form);
            self.mcu_form = Some(form);
        } else {
            // The form closed — close its import sub-dialogs too.
            self.datasheet_import = None;
            self.clock_import = None;
        }
    }
}

/// A section heading + underline.
fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .size(12.0)
            .strong()
            .color(egui::Color32::from_rgb(150, 180, 220)),
    );
    ui.separator();
}

/// A `Grid` row: label + full-width single-line edit + hover hint.
fn labeled(ui: &mut egui::Ui, label: &str, buf: &mut String, hint: &str) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(buf).desired_width(360.0))
        .on_hover_text(hint);
    ui.end_row();
}

fn toolchain_label(tc: &ToolchainKind) -> &'static str {
    match tc {
        ToolchainKind::RustEmbedded => "RustEmbedded (ARM · probe-rs/OpenOCD/DFU)",
        ToolchainKind::EspRust => "EspRust (ESP32 · espflash)",
        ToolchainKind::SdccC => "SdccC (STM8 · not implemented)",
    }
}

/// One collapsible side editor: a scrollable list of pin rows + add/fill.
///
/// Moving a pin to another side touches TWO sides at once, which this function
/// can't do (it only borrows its own). So it reports the intent through
/// `move_out` / `reorder_out` and the caller — which owns all four sides —
/// applies it via `McuForm::move_pin` / `reorder_pin`. Same deferred pattern
/// the row-remove already uses.
fn pin_side_editor(
    ui: &mut egui::Ui,
    side: usize,
    name: &str,
    rows: &mut Vec<PinRow>,
    // `(row index, target side)`
    move_out: &mut Option<(usize, usize)>,
    // `(row index, -1 = earlier | +1 = later)`
    reorder_out: &mut Option<(usize, isize)>,
) {
    egui::CollapsingHeader::new(format!("{name}  ({} pins)", rows.len()))
        .id_salt(("mcu_form_side", side))
        .show(ui, |ui| {
            let mut remove: Option<usize> = None;
            let count = rows.len();
            for (i, row) in rows.iter_mut().enumerate() {
                let note_open_id = egui::Id::new(("mcu_form_pin_note", side, i));
                let mut note_open = ui
                    .data(|d| d.get_temp::<bool>(note_open_id))
                    .unwrap_or(false);
                ui.horizontal(|ui| {
                    if row.imported {
                        ui.label(
                            egui::RichText::new(ph::SPARKLE)
                                .size(11.0)
                                .color(egui::Color32::from_rgb(150, 200, 255)),
                        )
                        .on_hover_text("Imported by AI — review this pin");
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut row.number)
                            .desired_width(34.0)
                            .hint_text("#"),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut row.name)
                            .desired_width(70.0)
                            .hint_text("name"),
                    );
                    ui.checkbox(&mut row.reserved, "rsv")
                        .on_hover_text("Reserved (VDD/VSS/…): no selectable functions");
                    ui.add_enabled(
                        !row.reserved,
                        egui::TextEdit::singleline(&mut row.functions)
                            .desired_width(280.0)
                            .hint_text("in out usart1_tx …"),
                    );
                    // ── Position along THIS side (list order = physical order)
                    if ui
                        .add_enabled(
                            i > 0,
                            egui::Button::new(egui::RichText::new(ph::ARROW_UP).size(10.0)).small(),
                        )
                        .on_hover_text("Move earlier along this side")
                        .clicked()
                    {
                        *reorder_out = Some((i, -1));
                    }
                    if ui
                        .add_enabled(
                            i + 1 < count,
                            egui::Button::new(egui::RichText::new(ph::ARROW_DOWN).size(10.0))
                                .small(),
                        )
                        .on_hover_text("Move later along this side")
                        .clicked()
                    {
                        *reorder_out = Some((i, 1));
                    }
                    // ── Move to another side (keeps the pin + its number) ────
                    egui::ComboBox::from_id_salt(("mcu_form_pin_side", side, i))
                        .selected_text(&SIDES[side][..1]) // T / B / L / R
                        .width(42.0)
                        .show_ui(ui, |ui| {
                            // Same Left → Bottom → Right → Top order as the
                            // editors, so the picker doesn't contradict them.
                            for &target in &SIDE_DISPLAY_ORDER {
                                if ui.selectable_label(target == side, SIDES[target]).clicked()
                                    && target != side
                                {
                                    *move_out = Some((i, target));
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "Move this pin to another side — the pin and its number are kept",
                        );
                    // ── The board's note on this pad, shown in the pin panel.
                    // A kit's notes name its own solder bridges, so a copy
                    // saved as another board must be able to see and drop them.
                    let has_note = !row.note.trim().is_empty();
                    let icon = egui::RichText::new(ph::NOTE_PENCIL).size(11.0);
                    let icon = if has_note {
                        icon.color(egui::Color32::from_rgb(230, 205, 130))
                    } else {
                        icon
                    };
                    let hover = if has_note {
                        format!("Board note, shown in the pin panel:\n\n{}", row.note)
                    } else {
                        "Add a board note: what the BOARD does to this pad (a solder bridge, \
                         a resistor, a pin the debugger drives). Shown in the pin panel."
                            .to_owned()
                    };
                    if ui.selectable_label(note_open, icon).on_hover_text(hover).clicked() {
                        note_open = !note_open;
                    }
                    if ui
                        .button(egui::RichText::new(ph::TRASH).size(11.0))
                        .on_hover_text("Remove this pin")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
                if note_open {
                    ui.add(
                        egui::TextEdit::multiline(&mut row.note)
                            .desired_rows(2)
                            .desired_width(560.0)
                            .hint_text("What the board does to this pad…"),
                    );
                }
                ui.data_mut(|d| d.insert_temp(note_open_id, note_open));
            }
            if let Some(i) = remove {
                rows.remove(i);
            }
            ui.horizontal(|ui| {
                if ui.button(format!("{} Add pin", ph::PLUS)).clicked() {
                    // Continue the numbering from the highest number so far.
                    let next = rows
                        .iter()
                        .filter_map(|r| r.number.trim().parse::<usize>().ok())
                        .max()
                        .unwrap_or(0)
                        + 1;
                    rows.push(PinRow {
                        number: next.to_string(),
                        ..Default::default()
                    });
                }
                if ui
                    .button(format!("{} Fill GPIO bank…", ph::LIST_PLUS))
                    .on_hover_text("Append 16 sequential GPIO pins (PX0…PX15)")
                    .clicked()
                {
                    let start = rows
                        .iter()
                        .filter_map(|r| r.number.trim().parse::<usize>().ok())
                        .max()
                        .unwrap_or(0)
                        + 1;
                    let prefix = ["PA", "PB", "PC", "PD"][side];
                    rows.extend(mcu_form::gpio_bank(prefix, start, 16));
                }
                if !rows.is_empty()
                    && ui
                        .button(format!("{} Clear", ph::X))
                        .on_hover_text("Remove every pin on this side")
                        .clicked()
                {
                    rows.clear();
                }
            });
        });
}

/// Import a clock tree from a CubeMX `db/plugins/clock/STM32<FAM>.xml` the user
/// picked, and attach it to `form`.
///
/// The path locates everything: its stem is the family, and its grandparent's
/// parent is the `db` directory holding the RCC parameter file. So the user
/// picks ONE file, not two — and picking the wrong one says so instead of
/// importing a half tree.
///
/// Variant tokens (`STM32WBAx5`, `SAI1_Exist`, …) are what CubeMX uses to fit
/// one family file to several parts. We do not know them from a bare file pick,
/// so nothing is assumed: conditional branches stay out, and the user adds what
/// their part has in the clock editor.
fn import_cubemx_clock(path: &std::path::Path, form: &mut McuForm) -> Result<String, String> {
    use crate::panels::mcu_module::clock::graph::cubemx;

    let family = cubemx::family_of(path).ok_or("that file has no name to take a family from")?;
    // …/db/plugins/clock/STM32WBA.xml -> …/db
    let db = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or("expected the file to sit in <CubeMX>/db/plugins/clock/")?;

    let (graph, boxes) = cubemx::import_from_db(db, &family, &cubemx::Variant::default())?;
    let (nodes, missing) = attach_imported_clock(form, graph, boxes);
    Ok(format!(
        "Imported {family} from CubeMX: {nodes} nodes, with ST's own diagram layout.{}",
        unbound_note(&missing)
    ))
}

/// Import the clock tree of ONE chip: its `STM32_open_pin_data` `mcu/*.xml`
/// names the CubeMX files, a CubeMX installation supplies them.
///
/// The pin-data file has no clock tree in it — but it carries `ClockTree=`
/// (which of a family's several topologies this part uses), the RCC IP
/// `Version=` (which of several parameter files), and the peripheral instance
/// list (which conditional branches exist). So nothing is guessed, and the
/// result is that part's tree rather than its family's.
fn import_chip_clock(path: &std::path::Path, form: &mut McuForm) -> Result<String, String> {
    use crate::panels::mcu_module::clock::graph::cubemx;

    let xml = std::fs::read_to_string(path).map_err(|e| format!("Could not read the file: {e}"))?;
    let key = cubemx::clock_key_from_mcu_xml(&xml)?;
    let db = cubemx::default_db_dir().ok_or(
        "No STM32CubeMX installation found. Install it, or use \"CubeMX clock XML…\" and \
         point at db/plugins/clock/ yourself.",
    )?;

    let (graph, boxes) = cubemx::import_for_chip(&db, &key)?;
    let (nodes, missing) = attach_imported_clock(form, graph, boxes);
    Ok(format!(
        "Imported {} nodes from {} (RCC {}), with ST's own diagram layout.{}",
        nodes,
        key.clock_tree,
        key.rcc_version,
        unbound_note(&missing)
    ))
}

/// Attach an imported tree to the form, with its codegen bindings proposed.
///
/// An imported tree uses the vendor's node names, which code generation does not
/// know; `bind::propose` matches them against the ids this family's generator
/// reads. The result is a filled-in mapping the user confirms in the Clock tab —
/// and the ids that found no plausible node are named in the message, because a
/// missing binding means that value silently falls back to a default.
fn attach_imported_clock(
    form: &mut McuForm,
    graph: crate::panels::mcu_module::clock::graph::ClockGraph,
    boxes: Vec<crate::panels::mcu_module::clock::graph::NodeBox>,
) -> (usize, Vec<String>) {
    use crate::panels::mcu_module::clock::graph::{GraphClock, bind, derive};
    use crate::panels::mcu_module::codegen::rcc::codegen_node_ids;

    let ids = codegen_node_ids(&form.family);
    let bindings = bind::propose(&ids, &graph);
    let missing = bind::unbound(&ids, &bindings);
    let nodes = graph.nodes.len();
    let layout = derive(&graph, boxes);
    form.set_imported_clock(GraphClock {
        graph,
        layout,
        bindings,
    });
    (nodes, missing)
}

/// " · N codegen id(s) still unbound: a, b" — appended to an import message.
fn unbound_note(missing: &[String]) -> String {
    if missing.is_empty() {
        return String::new();
    }
    format!(
        " · {} codegen id(s) unbound ({}) — bind them in the Clock tab or those values fall back to defaults.",
        missing.len(),
        missing.join(", ")
    )
}
