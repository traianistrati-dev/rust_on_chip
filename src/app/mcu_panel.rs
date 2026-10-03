//! Central "MCU Configurator" panel — chip label, tab bar (Pins / Peripherals
//! / Clock / System) and the active tab's content.
//!
//! Inherent method on [`AppIde`]; the Pins tab draws the chip and, on any pin
//! change, re-syncs the generated `pins/` files.

use super::tabs::show_peripherals_tab;
use super::{AppIde, McuTab, ProjectFileId, TabGroup};
use crate::panels::mcu_module::codegen::family;
use crate::panels::mcu_module::mcu::gui::module_docs;
use eframe::egui;
use egui_phosphor::regular as ph;

use crate::panels::mcu_module::modules::ModuleKind;

/// One documented entry per line: a strong label, then the sentence.
///
/// Shared by the two groups in the details pane. The body is handed to egui one
/// WORD at a time rather than as a string — `horizontal_wrapped` then wraps it
/// to whatever width the column has, and a run of spaces inside a literal cannot
/// survive `split_whitespace`. This file has been bitten by that twice.
fn field_entries(
    ui: &mut egui::Ui,
    fields: &[crate::panels::mcu_module::mcu::gui::module_docs::FieldDoc],
    label_color: egui::Color32,
) {
    for f in fields {
        // Label and body on ONE wrapped line: in a 220px column a label line
        // plus a body block costs a third more height and reads no better.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.label(
                egui::RichText::new(&f.label)
                    .size(10.5)
                    .strong()
                    .color(label_color),
            );
            for w in f.doc.split_whitespace() {
                ui.label(
                    egui::RichText::new(w)
                        .size(10.5)
                        .color(egui::Color32::from_gray(160)),
                );
            }
        });
        ui.add_space(4.0);
    }
}

/// The details pane: what one Virtual Module actually holds.
///
/// # What is in here, and what deliberately is not
///
/// Everything shown is read off the module and the chip — the peripheral it
/// took, the pins it holds, what is still unwired. Nothing is a second
/// description of the generated code kept in step by hand: the only link to it
/// is the jump button, which goes through `request_pin_goto`, the same path the
/// canvas already uses. A pane that re-stated the `init` signature would be one
/// more list to keep in step with `codegen`, and it would be the one that rots.
fn module_details_ui(
    ui: &mut egui::Ui,
    mcu: &crate::panels::mcu_module::mcu::model::Mcu,
    id: &str,
    pin_names: &std::collections::HashMap<usize, String>,
    // What this module's config had to say that was not a control — see the
    // `out` parameter on `module_config_ui`.
    said: Option<&crate::panels::mcu_module::mcu::gui::module_docs::ConfigOut>,
    goto_pin: &mut Option<usize>,
) {
    use crate::panels::mcu_module::mcu::gui::module_docs::ConfigOut;
    use crate::panels::mcu_module::mcu::gui::modules as mod_gui;

    let Some(m) = mcu.modules.iter().find(|m| m.id == id) else {
        return;
    };
    let color = mod_gui::module_color(m.kind, m.instance());
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(format!("{}  {}", ph::INFO, mod_gui::module_title(m)))
            .strong()
            .color(color),
    );
    ui.add_space(6.0);

    // What the module is for, in the words the palette already uses — one
    // sentence, and the same one, so the two cannot drift apart. Labelled as
    // the palette's own text because it is written in the imperative ("Add
    // a…"), which needs saying when it is read next to a module that exists.
    ui.label(
        egui::RichText::new("From the palette:")
            .size(10.0)
            .color(egui::Color32::from_gray(120)),
    );
    ui.label(
        egui::RichText::new(add_module_hint(m.kind))
            .size(11.0)
            .color(egui::Color32::from_gray(170)),
    );
    // ── Good to know ──
    // These came out of the config column, where each one was a grid row wide
    // enough that the column could not shrink below it.
    if let Some(notes) = said.map(ConfigOut::notes).filter(|n| !n.is_empty()) {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("Good to know")
                .size(11.0)
                .strong()
                .color(egui::Color32::from_gray(200)),
        );
        ui.add_space(2.0);
        for n in notes {
            ui.label(
                egui::RichText::new(n)
                    .size(10.5)
                    .color(egui::Color32::from_gray(150)),
            );
            ui.add_space(3.0);
        }
    }

    // ── Fields ──
    // Every row the config column just drew, in the order it drew them and
    // under the label it used. Closed by default: the pane's other sections are
    // what a reader glances at, and a fifteen-row list above them would bury
    // both. `id_salt` on the KIND, not the module, so opening it for one USART
    // opens it for the next — someone who wants field docs wants them for that
    // whole kind.
    //
    // `fields()` is None until the arm that drew them says it documents all of
    // them, so a kind that is half rolled out shows no section rather than a
    // roster that reads as the module's whole surface.
    if let Some(fields) = said.and_then(ConfigOut::fields).filter(|f| !f.is_empty()) {
        ui.add_space(10.0);
        egui::CollapsingHeader::new(
            egui::RichText::new(format!("Fields \u{2014} {}", fields.len()))
                .size(11.0)
                .strong()
                .color(egui::Color32::from_gray(200)),
        )
        .id_salt(("vmod_fields", m.kind))
        .default_open(false)
        .show(ui, |ui| {
            field_entries(ui, fields, color);
        });
    }

    // —— Set elsewhere ——
    // Settings this module carries that the config column never draws: which
    // peripheral it took, the Name row above the grid, and the two data models
    // that have no control at all. Without this group the pane is dishonest the
    // moment a reader compares it with what the project actually saves.
    //
    // NOT gated by the roster's completeness — see `ConfigOut::elsewhere`. It
    // is true for every kind today, so it shows for every kind today.
    let elsewhere = said.map(ConfigOut::elsewhere_fields).unwrap_or_default();
    if !elsewhere.is_empty() {
        ui.add_space(6.0);
        egui::CollapsingHeader::new(
            egui::RichText::new(format!("Set elsewhere — {}", elsewhere.len()))
                .size(11.0)
                .strong()
                .color(egui::Color32::from_gray(160)),
        )
        .id_salt(("vmod_elsewhere", m.kind))
        .default_open(false)
        .show(ui, |ui| {
            // Grey, not the module's colour: these are not rows you can go and
            // change up there.
            field_entries(ui, elsewhere, egui::Color32::from_gray(175));
        });
    }

    // —— Not offered here ——
    // Rows this chip or runtime does not draw, and WHY - but only where the
    // panel already had the reason written down. A gate is an `if` that did not
    // fire, and there are some forty of them here: inventing a sentence for each
    // would be forty claims nobody checked. So this group is short on purpose,
    // and everything else absent is simply absent.
    let skipped = said.map(ConfigOut::skipped_fields).unwrap_or_default();
    // …and the rest of this kind's rows that this chip did not draw. The roster
    // is the union of everything the panel CAN draw, generated from the test
    // matrix, so subtracting what it drew leaves exactly what is missing here.
    //
    // No reason is given for these: a gate is an `if` that did not fire, and
    // forty invented sentences would be forty claims nobody checked. The ones
    // above carry a reason because the panel already had it written down.
    let drawn: Vec<&str> = said
        .and_then(ConfigOut::fields)
        .into_iter()
        .flatten()
        .map(|f| f.label.as_str())
        .collect();
    let absent: Vec<crate::panels::mcu_module::mcu::gui::module_docs::FieldDoc> = said
        .map(|_| {
            module_docs::ROSTER
                .iter()
                .find(|(k, _)| *k == m.kind.short())
                .map_or(&[][..], |(_, rows)| *rows)
                .iter()
                .filter(|(label, _)| {
                    !drawn.contains(label) && !skipped.iter().any(|s| s.label == *label)
                })
                .map(|(label, doc)| {
                    crate::panels::mcu_module::mcu::gui::module_docs::FieldDoc {
                        label: (*label).to_owned(),
                        // A row whose meaning moves with the chip has no single
                        // sentence, so it gets the label alone rather than one
                        // of its forms picked at random.
                        doc: std::borrow::Cow::Borrowed(
                            doc.unwrap_or("Not offered on this chip or runtime."),
                        ),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    if !skipped.is_empty() || !absent.is_empty() {
        ui.add_space(6.0);
        egui::CollapsingHeader::new(
            egui::RichText::new(format!(
                "Not offered here — {}",
                skipped.len() + absent.len()
            ))
            .size(11.0)
            .strong()
            .color(egui::Color32::from_gray(160)),
        )
        .id_salt(("vmod_skipped", m.kind))
        .default_open(false)
        .show(ui, |ui| {
            // The explained ones first: they are the answers, and the rest is
            // the list of what else this module has.
            field_entries(ui, skipped, egui::Color32::from_rgb(190, 150, 110));
            field_entries(ui, &absent, egui::Color32::from_gray(150));
        });
    }

    ui.add_space(10.0);

    // —— Pins ——
    ui.label(
        egui::RichText::new("Pins")
            .size(11.0)
            .strong()
            .color(egui::Color32::from_gray(200)),
    );
    ui.add_space(2.0);
    if m.connections.is_empty() {
        ui.label(
            egui::RichText::new(
                "Nothing wired yet. Assign this peripheral's functions on the Pins canvas and \
                 they appear here.",
            )
            .size(10.5)
            .italics()
            .color(egui::Color32::from_gray(140)),
        );
    } else {
        egui::Grid::new(("vmod_info_pins", id))
            .num_columns(3)
            .spacing([8.0, 3.0])
            .show(ui, |ui| {
                for c in &m.connections {
                    ui.label(
                        egui::RichText::new(c.signal.label())
                            .size(10.5)
                            .monospace()
                            .color(color),
                    );
                    let name = pin_names
                        .get(&c.mcu_pin)
                        .cloned()
                        .unwrap_or_else(|| "?".to_owned());
                    // `ph::ARROW_RIGHT`, not a raw U+2192: the UI font has no
                    // glyph for it and it draws as tofu. The config rows below
                    // this pane already learned that.
                    ui.label(
                        egui::RichText::new(format!("{} {name}", ph::ARROW_RIGHT))
                            .size(10.5)
                            .monospace(),
                    );
                    // The pad NUMBER, which is what the datasheet and the board
                    // silkscreen agree on and the canvas does not always have
                    // room for — and a way into the code it generated.
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(format!("{}  {}", ph::CODE, c.mcu_pin))
                                    .size(10.0),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Pin number - click to jump to its line in the code")
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        *goto_pin = Some(c.mcu_pin);
                    }
                    ui.end_row();
                }
            });
    }

    ui.add_space(10.0);

    // ── This kind, on this chip ──
    ui.label(
        egui::RichText::new("On this chip")
            .size(11.0)
            .strong()
            .color(egui::Color32::from_gray(200)),
    );
    ui.add_space(2.0);
    let same = mcu.modules.iter().filter(|o| o.kind == m.kind).count();
    ui.label(
        egui::RichText::new(format!(
            "{same} {} module{} of this kind.",
            m.kind.short(),
            if same == 1 { "" } else { "s" }
        ))
        .size(10.5)
        .color(egui::Color32::from_gray(170)),
    );
    ui.label(
        egui::RichText::new(if mcu.can_add_module(m.kind) {
            "Another one can still be added."
        } else if mcu.has_free_instance(m.kind) {
            // `can_add_module` says no for TWO different reasons, and this used
            // to report both as exhausted instances - sending the reader to
            // remove a module when the thing in the way was a pad.
            //
            // Ahead of `is_single_instance` for the same reason
            // `add_module_block_reason` asks in this order: a spent pad is what
            // is in the way whether the chip has one of these or six.
            "The peripheral is free, but its pins are assigned elsewhere."
        } else if m.kind.is_single_instance() {
            "The chip has only this one."
        } else {
            "Every instance is taken."
        })
        .size(10.5)
        .color(egui::Color32::from_gray(140)),
    );
    ui.add_space(8.0);
}

/// Hover for the palette's "Choose pins..." entry.
///
/// Out here because it is longer than `max_width` and inline it takes the whole
/// panel function out of rustfmt's reach, the same reason as
/// [`add_module_hint`].
const CHOOSE_PINS_HINT: &str = "Pick the peripheral instance and every pad yourself. It opens on the wiring shown above, so confirming straight away is the same as choosing Auto.";

/// Why a palette entry is greyed out.
///
/// Out here for the same reason as [`add_module_hint`]: these sentences are
/// longer than `max_width`, and inline they take the whole panel function out
/// of rustfmt's reach.
/// Whether module `id`'s config should be open, one frame after a click on the
/// canvas hit `clicked`'s box.
///
/// A click on the CANVAS means "show me this one": every other config closes, so
/// the right column holds exactly the module whose box was hit. Several at once
/// is a deliberate act, and the list is where you do it.
///
/// The clicked one FOLLOWS `canvas_selected` rather than toggling itself. The
/// canvas has already decided what the click means - it toggles
/// `mcu.selected_module`, and a box is drawn white because its config is
/// showing, so a second, independent toggle here only agreed with it while the
/// two happened to start in step. Nothing guarantees that: `open_after_add` opens a
/// config without selecting anything, so adding a module and then clicking its
/// box to inspect it CLOSED the config that was already open and lit the box
/// instead.
fn config_open_after_click(id: &str, clicked: &str, canvas_selected: Option<&str>) -> bool {
    id == clicked && canvas_selected == Some(id)
}

/// Whether the Virtual-modules panel draws a BODY under its bar, or only the bar.
///
/// Three things are worth showing: virtual modules, devices, and pads that could
/// become either. With none of them the body would be an empty box under a bar
/// that already says what to press.
///
/// Pulled out of the panel because one line of it is load-bearing for a control
/// that lives somewhere else entirely. `+ Device` sits on the BAR and creates a
/// group there, 120-odd lines before this is evaluated — and the only reason the
/// click is not a no-op is that `has_groups` is then true, so the roster is
/// drawn on the same frame and can unfold the row it just made. Written inline,
/// that dependency was invisible from the button and untestable from anywhere.
fn panel_show_body(
    collapsed: bool,
    has_modules: bool,
    has_groups: bool,
    anything_to_group: bool,
    // Notes whose module is gone. The last module going is exactly when
    // the other three all turn false, so without this the list of them
    // disappeared the moment it had something to list.
    has_orphan_notes: bool,
) -> bool {
    !collapsed && (has_modules || has_groups || anything_to_group || has_orphan_notes)
}

fn add_module_block_reason(
    kind: ModuleKind,
    hw_only: Option<&'static str>,
    // The chip still hosts an instance no module holds — so what stands in the
    // way is the PADS, not the peripheral.
    free_instance: bool,
) -> &'static str {
    if let Some(why) = hw_only {
        // The silicon has it; no driver in this HAL can reach it.
        why
    } else if free_instance {
        // `can_add_module` says no for two different reasons and this said only
        // one of them, so a chip with a free instance whose pads were spent sent
        // the reader off to remove a module that was not in the way.
        //
        // Asked FIRST, ahead of the instance count. It used to sit last, behind
        // `is_single_instance`, which made it unreachable for exactly the seven
        // kinds that have one instance - and those are the ones where "remove a
        // module to free it" is worst advice, because on a chip with no USB
        // module at all it named a module that does not exist. What decides the
        // wording is what is IN THE WAY, and a spent pad is in the way whether
        // the chip has one of these or six.
        "the peripheral is free, but the pins it needs are assigned to other functions — free one \
         on the Pins canvas"
    } else if kind.is_single_instance() {
        "this chip has only one such peripheral and it's already used"
    } else {
        "every instance of this peripheral is already wired to a module — remove one to free it"
    }
}

/// What one palette entry promises, for its hover text.
///
/// A free function, and not a `match` inside the palette loop, for one
/// mechanical reason: every arm here is a sentence, so the arms run past
/// `max_width` and **rustfmt gives up on the whole enclosing function**. It
/// did, silently, and the palette's indentation had been hand-kept ever
/// since. Out here the long lines cost only this table.
fn add_module_hint(kind: ModuleKind) -> &'static str {
    match kind {
        ModuleKind::GenericInterfaceUsart => {
            "Add a virtual USART device and auto-wire it to a free USART TX/RX pin pair"
        }
        ModuleKind::GenericInterfaceLpuart => {
            "Add a virtual LPUART device and auto-wire it to a free LPUART TX/RX pin pair (a peripheral of its own, not a USART instance)"
        }
        ModuleKind::GenericInterfaceSpi => {
            "Add a virtual SPI device and auto-wire it to free SPI SCK/MOSI/MISO(/NSS) pins"
        }
        ModuleKind::GenericInterfaceI2c => {
            "Add a virtual I2C device and auto-wire it to a free I2C SCL/SDA pin pair"
        }
        ModuleKind::GenericInterfaceRmt => {
            "Add an RMT channel and auto-wire its single line: a pulse-train engine for IR, WS2812 strips and 1-Wire (Espressif only)"
        }
        ModuleKind::GenericInterfacePcnt => {
            "Add a PCNT unit and auto-wire its edge input: a hardware pulse counter for flow meters, tachometers and encoders (Espressif only)"
        }
        ModuleKind::GenericInterfaceMcpwm => {
            "Add an MCPWM unit and auto-wire its first output: motor-control PWM with complementary pairs, for bridges and inverters (Espressif only)"
        }
        ModuleKind::GenericInterfaceParlIoRx => {
            "Add the RECEIVING half of the parallel port: its own pads and its own settings, running alongside the sending half (Espressif only). The two share one DMA channel"
        }
        ModuleKind::GenericInterfaceParlIo => {
            "Add the parallel port and auto-wire its first data line and clock: a byte per clock over DMA, for LED matrices and fast ADCs (Espressif only). Assign the rest of the bus on the canvas"
        }
        ModuleKind::GenericInterfaceCamera => {
            "Add the DVP camera half of the video port: 8 or 16 bits in, on its own pads and its own DMA channel - it runs alongside a display on the other half (ESP32-S3 only)"
        }
        ModuleKind::GenericInterfaceTouch => {
            "Add the capacitive touch controller and auto-wire its first pad: ten channels, each welded to one GPIO, read as a 16-bit count (original ESP32 only). Assign the other pads on the canvas"
        }
        ModuleKind::GenericInterfaceLcdCam => {
            "Add the parallel video port: an i8080 or RGB display, or a DVP camera - 8 or 16 bits wide, always on DMA (ESP32-S3 only). Assign the control pads your mode needs on the canvas"
        }
        ModuleKind::GenericInterfaceHspi => {
            "Add external memory on the HSPI (high-end U5): it auto-wires the single-line width, and the octal one joins by assigning IO2-IO7 and DQS0, which that call requires"
        }
        ModuleKind::GenericInterfaceXspi => {
            "Add an external flash or RAM on an XSPI port (H7RS / N6): it auto-wires the two-line minimum on NCS1, and the wider modes join by assigning IO2-IO15 on the canvas"
        }
        ModuleKind::GenericInterfaceOspi => {
            "Add an external flash or RAM on an OCTOSPI port: it auto-wires the two-line minimum (CLK, NCS, IO0-IO1), and the quad and octal modes join by assigning IO2-IO7 on the canvas"
        }
        ModuleKind::GenericInterfaceQspi => {
            "Add an external-flash module on the QUADSPI: it auto-wires bank 1 whole (CLK, NCS, IO0-IO3), and bank 2 joins the same module when you assign its pads on the canvas"
        }
        ModuleKind::GenericInterfaceSdmmc => {
            "Add an SD card / eMMC module: it auto-wires CK, CMD and D0, and the wider buses join by assigning D1-D3 (4-bit) or D1-D7 (8-bit) on the canvas"
        }
        ModuleKind::GenericInterfaceSai => {
            "Add a SAI module for one audio unit: it auto-wires sub-block A (SCK/SD/FS), and sub-block B joins the same module when you assign its pads on the canvas"
        }
        ModuleKind::GenericInterfaceDac => {
            "Add a DAC module for one peripheral: it takes a free OUT pad now, and the other channel of the SAME DAC joins it when you assign it on the canvas"
        }
        ModuleKind::GenericInterfaceI2s => {
            "Add a virtual I2S audio device and auto-wire it to free I2S CK/WS/SD pins (MCK stays free unless you assign it). I2Sn runs on SPIn, so the two cannot both be built"
        }
        ModuleKind::GenericInterfaceTimer => {
            "Add a PWM module for one TIMER: it takes a free channel now, and any other channel of the SAME timer you assign on the canvas joins it (they share the frequency)"
        }
        ModuleKind::GenericInterfaceCan => {
            "Add a virtual CAN device and auto-wire it to the CAN RX/TX pins (needs the bxcan crate)"
        }
        ModuleKind::GenericInterfaceUsb => {
            "Add a virtual USB device and auto-wire it to the USB D-/D+ pins"
        }
        ModuleKind::Custom => {
            "Add a CUSTOM module — nothing is auto-wired: name it, add the pins you want, and a `configs/custom_<name>.rs` struct is generated for them"
        }
    }
}

/// How much of the panel the auto-fitted diagram fills.
///
/// Not 1.0. A fit that touches the panel's edges reads as "already zoomed in as
/// far as it goes": the outermost pin labels sit flush against the border, a
/// device mat's tab has nowhere to go, and there is no room to see that anything
/// continues past the edge. The remaining fifth becomes a tenth of the panel as
/// margin on each side.
const AUTO_FIT_FILL: f32 = 0.8;

/// The scene rect to feed the `Scene` so `content` lands at
/// [`AUTO_FIT_FILL`] of `avail`.
///
/// The `Scene` scales the rect it is given to fill the panel, so asking for a
/// SMALLER picture means handing it a LARGER rect — the content's own bounds
/// divided by the fill fraction.
///
/// Floored at the panel size, so content that already fits comfortably is shown
/// at 100 % and centred rather than blown up to reach the 80 % mark.
fn auto_fit_bounds(content: egui::Rect, avail: egui::Vec2) -> egui::Rect {
    if !content.is_finite() {
        return content;
    }
    let want = content.size() / AUTO_FIT_FILL;
    egui::Rect::from_center_size(
        content.center(),
        egui::vec2(want.x.max(avail.x), want.y.max(avail.y)),
    )
}

#[cfg(test)]
mod auto_fit_tests {
    use super::{AUTO_FIT_FILL, auto_fit_bounds};
    use eframe::egui;

    /// The Scene scales what it is given to fill the panel, so the fed rect is
    /// the content divided by the fill fraction — and the picture lands at 80 %.
    #[test]
    fn a_diagram_larger_than_the_panel_lands_at_eighty_percent() {
        let avail = egui::vec2(1000.0, 600.0);
        let content = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(2000.0, 1200.0));
        let fed = auto_fit_bounds(content, avail);
        // What the Scene will do with it.
        let scale = (avail / fed.size()).min_elem();
        let drawn = content.size() * scale;
        assert!(
            (drawn.x / avail.x - AUTO_FIT_FILL).abs() < 0.001,
            "{drawn:?}"
        );
        assert!(drawn.y / avail.y <= AUTO_FIT_FILL + 0.001);
    }

    /// A tenth of the panel on each side is the point of the exercise.
    #[test]
    fn the_margin_is_a_tenth_of_the_panel_on_each_side() {
        let avail = egui::vec2(800.0, 800.0);
        let content =
            egui::Rect::from_min_size(egui::pos2(-500.0, -500.0), egui::vec2(1000.0, 1000.0));
        let fed = auto_fit_bounds(content, avail);
        let scale = (avail / fed.size()).min_elem();
        let margin = (avail.x - content.width() * scale) / 2.0;
        assert!((margin / avail.x - 0.1).abs() < 0.001, "margin {margin}");
    }

    /// Content that already fits comfortably is shown at 100 % and centred, not
    /// blown up to reach the 80 % mark — a four-pin chip in a wide panel would
    /// otherwise be magnified until its labels were huge.
    #[test]
    fn a_small_diagram_is_centred_at_full_size_not_magnified() {
        let avail = egui::vec2(1000.0, 1000.0);
        let content = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        let fed = auto_fit_bounds(content, avail);
        assert_eq!(fed.size(), avail, "the panel itself is fed");
        assert_eq!(fed.center(), content.center(), "and the content is centred");
    }

    /// A frame before anything has been drawn feeds `NOTHING` straight through
    /// rather than turning it into a NaN rect the Scene would latch.
    #[test]
    fn an_empty_canvas_is_passed_through_untouched() {
        let fed = auto_fit_bounds(egui::Rect::NOTHING, egui::vec2(100.0, 100.0));
        assert_eq!(fed, egui::Rect::NOTHING);
    }
}

impl AppIde {
    /// What an MCU tab shows when there is no [`Mcu`](crate::panels::mcu_module::mcu::model::Mcu)
    /// to draw.
    ///
    /// Two different situations hide behind that one `None`, and they need
    /// opposite answers:
    ///
    /// * **No chip chosen at all** — a fresh New Project. Offer the picker: it
    ///   is the only way forward, and this is where the user is already looking.
    /// * **A chip whose definition exists but has no runtime support yet.**
    ///   Nothing to pick; say so.
    ///
    /// The old text said "coming soon" for both, which named a chip that wasn't
    /// selected and offered no way out of an empty project.
    fn show_no_mcu_notice(&mut self, ui: &mut egui::Ui, what: &str) {
        let chip = self.selected_label();
        ui.add_space((ui.available_height() * 0.3).min(160.0));
        ui.vertical_centered(|ui| {
            if !chip.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  {chip}  —  {what} not supported yet",
                        ph::GEAR
                    ))
                    .size(16.0)
                    .color(egui::Color32::GRAY),
                );
                return;
            }
            ui.label(
                egui::RichText::new(format!("{}  No MCU selected", ph::CPU))
                    .size(17.0)
                    .color(egui::Color32::from_rgb(150, 158, 172)),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!(
                    "{what} needs a chip — the project's code is generated from it."
                ))
                .size(12.0)
                .color(egui::Color32::from_gray(120)),
            );
            ui.add_space(12.0);
            if ui
                .button(egui::RichText::new(format!("{}  Select a chip…", ph::CPU)).size(13.0))
                .on_hover_text("Choose the MCU this project targets")
                .clicked()
            {
                // The same dialog New Project uses — it IS the chip picker.
                // Safe to reach for here: the project is empty by definition,
                // so its "this clears everything" has nothing to clear.
                self.begin_new_project();
                // That dialog is rendered EARLIER in the frame than this panel,
                // so it first appears next frame — guarantee there is one.
                ui.ctx().request_repaint();
            }
        });
    }

    /// Render the central MCU configurator panel.
    ///
    /// Two-level navigation (2026-07-10): a GROUP row — "MCU" (chip config)
    /// and "Project" (chip-agnostic) — then the active group's own tab row:
    /// MCU → Pins / Peripherals / Clock / System; Project → Structure /
    /// Definition. Clicking a group returns to its last-used tab; the chip
    /// header row (Chip label + Reset pins) shows only for the MCU group.
    pub(super) fn show_mcu_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            // The Definition tab (F12 snippet) exists only while a snippet is
            // loaded — auto-leave it the moment the snippet clears.
            if self.definition_view.is_none() && self.active_tab == McuTab::Definition {
                self.active_tab = self.definition_return_tab;
            }
            // The second editor only renders while ITS tab is active. Without
            // this, both of its flags would keep their last value forever:
            // `reference_was_focused` would go on stealing the main editor's
            // shortcuts, and a completion it owns would keep swallowing
            // Enter/Escape for a popup nobody can see.
            if self.active_tab != McuTab::Reference || self.reference_file.is_none() {
                self.reference_was_focused = false;
                self.reference_ctrl_space = false;
                if self.completion_owner == crate::app::EditorSlot::Reference {
                    let ed = self.ed_of(crate::app::EditorSlot::Reference);
                    ed.completion_open = false;
                    ed.completion_note = None;
                }
            }
            // Track each group's last-used tab so group clicks restore it.
            // Reference belongs to NEITHER group — recording it would make a
            // later "MCU" click land on a file instead of the chip.
            let group = self.active_tab.group();
            match group {
                Some(TabGroup::Project) => self.project_group_last = self.active_tab,
                Some(TabGroup::Mcu) => self.mcu_group_last = self.active_tab,
                // Board has one tab; Reference belongs to no group.
                Some(TabGroup::Board) | None => {}
            }
            let reference_active = group.is_none();
            let project_active = group == Some(TabGroup::Project);
            let board_active = group == Some(TabGroup::Board);
            // The chip-only chrome (Reset pins, the chip label) must not come
            // back just because Reference or Board left the Project group.
            let mcu_active = group == Some(TabGroup::Mcu);

            // ── Level 1: group selector ────────────────────────────────────
            ui.horizontal(|ui| {
                for (label, g) in [
                    ("MCU", TabGroup::Mcu),
                    ("Project", TabGroup::Project),
                    ("Board", TabGroup::Board),
                ] {
                    let active = group == Some(g);
                    let text = egui::RichText::new(label).size(14.0).strong().color(
                        if active {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::from_rgb(150, 150, 160)
                        },
                    );
                    if ui.selectable_label(active, text).clicked() && !active {
                        self.active_tab = match g {
                            // A remembered Definition tab needs its snippet.
                            TabGroup::Project
                                if self.project_group_last == McuTab::Definition
                                    && self.definition_view.is_none() =>
                            {
                                McuTab::Structure
                            }
                            TabGroup::Project => self.project_group_last,
                            TabGroup::Mcu => self.mcu_group_last,
                            TabGroup::Board => McuTab::Board,
                        };
                    }
                }
                // The open reference file, as a THIRD top-level entry — right
                // of Project, level with the code editor it is read beside.
                if let Some(path) = self.reference_file.clone() {
                    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                    let text = egui::RichText::new(name).size(14.0).strong().color(
                        if reference_active {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::from_rgb(150, 200, 150)
                        },
                    );
                    if ui.selectable_label(reference_active, text).clicked() {
                        self.active_tab = McuTab::Reference;
                    }
                    if ui
                        .add(egui::Button::new(egui::RichText::new(ph::X).size(10.0)).frame(false))
                        .on_hover_text("Close the reference file")
                        .clicked()
                    {
                        self.reference_file = None;
                        if reference_active {
                            self.active_tab = self.project_group_last;
                        }
                    }
                }
                // Reset pins — a chip operation, shown with the MCU group only.
                //
                // It ASKS first, like removing a single module does, because it
                // is the widest destructive action in the app: every pin function
                // goes, and with the pins go the Virtual Modules that were wired
                // to them (`reconcile_modules` drops a peripheral module whose
                // pins are gone). The confirm names the count, so the question
                // carries the size of the loss instead of just repeating itself.
                if mcu_active {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let configured =
                            self.mcu.as_ref().map_or(0, |m| m.configured_pin_count());
                        if self.reset_pins_confirm {
                            // Right-to-left: Cancel is added first, so it lands
                            // on the OUTSIDE — the safe choice is the one nearest
                            // the edge, away from where the pointer already is.
                            if ui.button("Cancel").clicked() {
                                self.reset_pins_confirm = false;
                            }
                            if ui
                                .button(
                                    egui::RichText::new(format!(
                                        "{} Reset {configured} pin{}",
                                        ph::ARROW_COUNTER_CLOCKWISE,
                                        if configured == 1 { "" } else { "s" }
                                    ))
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(220, 100, 80)),
                                )
                                .clicked()
                            {
                                if let Some(mcu) = &mut self.mcu {
                                    // The most destructive button in the panel,
                                    // and the only one that took no undo point.
                                    // A confirm is not an undo: it asks once,
                                    // before the user can see what they lost.
                                    // `undo_modules` restores exactly what this
                                    // throws away - every pin's function and
                                    // label, and every module - so Ctrl+Z is one
                                    // snapshot away from working.
                                    mcu.push_module_undo("Reset pins".to_owned());
                                    mcu.reset_all_pins();
                                }
                                self.reset_pins_confirm = false;
                            }
                            ui.label(
                                egui::RichText::new("also removes the Virtual Modules")
                                    .size(10.5)
                                    .color(egui::Color32::from_rgb(220, 180, 90)),
                            );
                        } else {
                            // Nothing configured → nothing to ask about, and a
                            // live button would only invite a pointless confirm.
                            let btn = ui.add_enabled(
                                configured > 0,
                                egui::Button::new(
                                    egui::RichText::new(format!(
                                        "{} Reset pins",
                                        ph::ARROW_COUNTER_CLOCKWISE
                                    ))
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(220, 100, 80)),
                                ),
                            );
                            let btn = if configured > 0 {
                                btn.on_hover_text(
                                    "Clear every pin function on this chip (asks first)",
                                )
                            } else {
                                btn.on_disabled_hover_text("No pin is configured")
                            };
                            if btn.clicked() {
                                self.reset_pins_confirm = true;
                            }
                        }
                    });
                } else {
                    // Left the MCU group with the question open: disarm, so it
                    // is not still waiting when the user comes back much later.
                    self.reset_pins_confirm = false;
                }
            });

            // Chip label — MCU group only (read-only; selection happens in the
            // "New Project" popup). The Project group doesn't involve the chip.
            if mcu_active {
                ui.horizontal(|ui| {
                    ui.label("Chip:");
                    ui.label(
                        egui::RichText::new(self.selected_label())
                            .strong()
                            .color(egui::Color32::LIGHT_BLUE),
                    );
                    let facts = self.chip_facts();
                    if !facts.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("·  {}", facts.join("  ·  ")))
                                .color(egui::Color32::GRAY)
                                .size(11.0),
                        );
                    }
                });
                // In a system: every chip of it, one click from being the one
                // this tab shows.
                self.show_system_chip_row(ui);
            }

            ui.separator();

            // ── Level 2: the active group's tab row ────────────────────────
            // Skipped entirely for Reference: it is a top-level entry with no
            // sub-tabs, and falling through to the `else` below would show the
            // MCU row (Pins/Clock/…) over a file that has nothing to do with it.
            // Board too: its one tab draws its own toolbar there instead.
            if !reference_active && !board_active {
            ui.horizontal(|ui| {
                let mut tabs: Vec<McuTab> = if project_active {
                    McuTab::project_group_tabs(self.definition_view.is_some())
                } else {
                    vec![
                        McuTab::Pins,
                        McuTab::Peripherals,
                        McuTab::Configuration,
                        McuTab::Clock,
                        McuTab::System,
                    ]
                };
                for tab in tabs.drain(..) {
                    let is_active = self.active_tab == tab;
                    // The second editor is named after its FILE — "Reference"
                    // says nothing about which one is open.
                    let text = if tab == McuTab::Reference {
                        self.reference_file
                            .as_deref()
                            .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                            .unwrap_or_else(|| tab.label().to_string())
                    } else {
                        tab.label().to_string()
                    };
                    let label = egui::RichText::new(text)
                        .size(13.0)
                        .color(if is_active {
                            egui::Color32::WHITE
                        } else if tab == McuTab::Definition {
                            egui::Color32::from_rgb(120, 180, 240)
                        } else if tab == McuTab::Reference {
                            egui::Color32::from_rgb(150, 200, 150)
                        } else {
                            egui::Color32::from_rgb(160, 160, 170)
                        });
                    if ui.selectable_label(is_active, label).clicked() {
                        self.active_tab = tab;
                    }
                }
                // Close button for the transient Definition tab.
                if project_active
                    && self.definition_view.is_some()
                    && ui
                        .add(egui::Button::new(egui::RichText::new(ph::X).size(10.0)).frame(false))
                        .on_hover_text("Close the definition and its Back / Forward history")
                        .clicked()
                {
                    self.def_close();
                }
            });
            }

            ui.separator();

            // Set by a tab that has no MCU to draw, and rendered AFTER the
            // match: those arms sit inside `match &mut self.mcu`, whose borrow
            // is live across them, so the notice (which takes `&mut self`)
            // cannot be drawn from inside one. Nothing else is drawn there
            // either, so it lands in the same place on screen.
            let mut no_mcu: Option<&'static str> = None;

            // Tab content
            match self.active_tab {
                McuTab::Pins => {
                    // ── Virtual-module palette + list, in a scrollable strip
                    //    BELOW the chip. Add a module (auto-wires to compatible
                    //    pins), rename it, edit its config, or remove it. Add/
                    //    remove change pin functions, so re-sync pins/ after.
                    let mut modules_changed = false;
                    // Set when the Rotate toggle is clicked → re-fit the Pins
                    // canvas so the re-oriented chip isn't left off-screen.
                    let mut rotate_toggled = false;
                    // The panel keeps a real MINIMUM of one bar, so the border
                    // is always draggable and the diagram can always be given
                    // the space back. Making room for a freshly opened config is
                    // a one-shot RESIZE further down instead of a floor here: a
                    // floor cannot be dragged past, and this panel sits on top of
                    // the chip.
                    let mut needed_h = 0.0_f32;
                    // Height the module LIST needs to show every row.
                    let mut list_h = 0.0_f32;
                    // Set when the caret button (not the handle) expands the
                    // panel — that is the gesture that means "show me all of it".
                    let mut expand_clicked = false;
                    // A module added from the palette: its config is opened, and
                    // a collapsed panel opens with it.
                    let mut open_after_add: Option<String> = None;
                    // `+ Device` on the bar created one: the roster below unfolds
                    // its row so the name can be typed straight away.
                    let mut device_added = false;
                    // "Choose pins..." was clicked: the dialog is seeded AFTER
                    // this block, because seeding reads `mcu` and the panel
                    // still holds it borrowed here.
                    let mut pick_request: Option<
                        crate::panels::mcu_module::modules::ModuleKind,
                    > = None;
                    let mut open_sig = 0_u64;
                    // The panel hugs its content (an egui bottom panel stores
                    // the CONTENT's rect, not the size it was given), so the body
                    // height is ours to set — and ours to offer a drag handle for.
                    // Two ceilings, on purpose. The DRAG may go nearly to the
                    // top — it is a deliberate act, and someone configuring six
                    // modules wants the room. Growing BY ITSELF stops at 45 %:
                    // a panel that takes the whole zone the moment you open a
                    // config has stopped being a panel.
                    let body_cap = (ui.available_height() - 90.0).max(120.0);
                    let auto_cap = (ui.available_height() * 0.45).min(body_cap);
                    let mut body_h = self.vmod_body_h.min(body_cap);
                    let mut collapsed = self.vmod_collapsed;
                    // `self` is borrowed for the whole panel closure below (it
                    // holds `&mut self.mcu`), so this travels in and out as a
                    // local, exactly like `collapsed` and `body_h`.
                    let mut info_id = self.vmod_info_id.clone();
                    // The handle below IS this panel's top border, so egui's
                    // own separator line would be a second bar right above it —
                    // which is exactly what it looked like.
                    // Collapsed, it hugs its content on purpose. Forcing it to
                    // the OTHER panel's exact box lined the two outlines up but
                    // left dead space under these buttons, which pushed the
                    // buttons off the line — and the buttons are what you see.
                    // Both bars are bottom-anchored, so equal content is what
                    // puts equal rows on one line.
                    egui::Panel::bottom("vmodules_panel")
                        // NOT `resizable`: the handle below does the resizing,
                        // and egui's own can only cap a content-sized panel.
                        .resizable(false)
                        .show_separator_line(false)
                        .show(ui, |ui| {
                            // ── The panel's top border, as a real handle ──
                            // Drag it up to make room, down to give it back to
                            // the diagram. Drawn before anything else so it sits
                            // on the panel's top edge, where the pointer expects
                            // the boundary to be.
                            {
                                /// Smallest body worth showing, and how far past
                                /// it the handle must be pulled DOWN before the
                                /// panel collapses itself — same pair as the
                                /// panel under the editor, so both borders
                                /// behave identically.
                                const MIN_BODY_H: f32 = 60.0;
                                const COLLAPSE_SLACK: f32 = 14.0;
                                let (hid, hrect) =
                                    ui.allocate_space(egui::vec2(ui.available_width(), 6.0));
                                let h = ui.interact(hrect, hid, egui::Sense::drag());
                                if h.dragged() {
                                    let dy = h.drag_delta().y;
                                    if collapsed {
                                        // Only upward opens it: a collapsed bar
                                        // has nowhere to shrink to. The caret
                                        // button flips on its own — it renders
                                        // straight from this flag.
                                        if dy < 0.0 {
                                            collapsed = false;
                                            body_h =
                                                (MIN_BODY_H - dy).clamp(MIN_BODY_H, body_cap);
                                        }
                                    } else {
                                        let want = body_h - dy;
                                        // Pulled past the smallest useful body:
                                        // finish the gesture by collapsing
                                        // rather than jamming against the floor.
                                        // The slack keeps a drag that merely
                                        // BOTTOMS OUT from snapping it shut.
                                        if want < MIN_BODY_H - COLLAPSE_SLACK {
                                            collapsed = true;
                                        }
                                        body_h = want.clamp(MIN_BODY_H, body_cap);
                                    }
                                }
                                let live = h.hovered() || h.dragged();
                                if live {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                                }
                                ui.painter().hline(
                                    hrect.x_range(),
                                    hrect.center().y,
                                    egui::Stroke::new(
                                        if live { 2.0_f32 } else { 1.0_f32 },
                                        if live {
                                            egui::Color32::from_rgb(120, 160, 210)
                                        } else {
                                            egui::Color32::from_gray(70)
                                        },
                                    ),
                                );
                            }
                            let Some(mcu) = &mut self.mcu else { return };
                            use crate::panels::mcu_module::mcu::gui::i2c_devices as mod_gui_i2c;
                            use crate::panels::mcu_module::mcu::gui::modules as mod_gui;
                            use crate::panels::mcu_module::modules::ModuleKind;

                            // A per-module "Init API" change (Portable | Native)
                            // below STAGES a codegen choice exactly like the
                            // System-tab Runtime cards — so show the SAME Apply /
                            // Discard prompt HERE. Without it the staged change was
                            // only visible on the System tab, leaving the user
                            // unsure whether the Init-API switch took effect.
                            Self::runtime_apply_bar(ui, mcu);

                            if !collapsed {
                                ui.add_space(4.0);
                            }
                            // WRAPPED, not a plain `horizontal`. Laid out under
                            // the app's own fonts the row now ends at ~407 px
                            // (caret 19, "Virtual modules:" 114, "+ Add module"
                            // 217, "Devices:" 275, "+ Device" 337, "Undo" 407)
                            // and `MCU_MIN_W` is 420, which leaves ~388 px once
                            // the two 8 px margins are taken — so at the narrowest
                            // MCU zone the app allows, the tail of Undo fell
                            // outside the clip rect and was neither drawn nor
                            // clickable. It was ~303 px before the two device
                            // controls joined the row.
                            //
                            // Wrapping costs one extra line of bar ONLY when the
                            // zone is too narrow to hold the row, and hides
                            // nothing; clipping silently took a control away.
                            ui.horizontal_wrapped(|ui| {
                                // Disclosure for the WHOLE panel, left of its
                                // name. Collapsed it keeps only this bar, which
                                // is the difference between a panel that can be
                                // put away and one that permanently costs the
                                // diagram a third of its height.
                                // Same pair as the panel under the editor
                                // (`diag_panel`): double-up to reopen, plain
                                // caret-down to put away. One collapse control,
                                // one shape — a right-caret reads as "unfold
                                // sideways", which is not what this does.
                                let (icon, tip) = if collapsed {
                                    (
                                        ph::CARET_DOUBLE_UP,
                                        "Expand the Virtual-modules panel.",
                                    )
                                } else {
                                    (
                                        ph::CARET_DOWN,
                                        "Collapse the panel to this bar and give the height back \
                                         to the diagram. Drag the top border to resize it instead.",
                                    )
                                };
                                if ui
                                    .button(
                                        egui::RichText::new(icon)
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(160, 185, 215)),
                                    )
                                    .on_hover_text(tip)
                                    .clicked()
                                {
                                    collapsed = !collapsed;
                                    expand_clicked = !collapsed;
                                }
                                // Undo the last module add/remove (also Ctrl+Z).
                                // Beside the collapse caret rather than at the
                                // far end of the row: it undoes what the palette
                                // did, and the palette is now one button — with
                                // Undo left behind at the other end there was
                                // nothing between them to explain the distance.

                                ui.label(
                                    egui::RichText::new("Virtual modules:")
                                        .size(12.0)
                                        .color(egui::Color32::from_rgb(150, 150, 160)),
                                );
                                // Only the kinds THIS chip's pins can host are
                                // offered (derived from the pins, so a new chip
                                // needs no per-family list). A supported-but-
                                // exhausted kind stays visible but disabled with
                                // the reason — a button that silently vanishes
                                // is more confusing than one that explains.
                                // One dropdown instead of up to twenty-four
                                // buttons. The row used to be a wall of `+ …`
                                // that pushed everything else off the bar on a
                                // chip with many peripherals — and every one of
                                // them does the same thing.
                                ui.menu_button(
                                        egui::RichText::new(format!(
                                            "{} Add module {}",
                                            ph::PLUS,
                                            ph::CARET_DOWN
                                        ))
                                        .size(11.0),
                                        |ui| {
                                            // A floor, so the items do not stagger to each
                                            // label's own width.
                                            ui.set_min_width(190.0);
                                            // Collected INSIDE the closure, which
                                            // runs only while the menu is OPEN —
                                            // the same rule the pad preview one
                                            // level down already follows. Outside,
                                            // the chip was asked about all
                                            // twenty-four kinds on every repaint
                                            // of the MCU tab, with the menu shut
                                            // and even with this whole panel
                                            // collapsed to its bar.
                                            //
                                            // It used to be out there to keep an
                                            // `any_supported` gate from flickering.
                                            // That gate could never be false:
                                            // `ModuleKind::ALL` contains `Custom`,
                                            // and `supports_module` answers true
                                            // for it on every chip.
                                            let offered: Vec<ModuleKind> = ModuleKind::ALL
                                                .into_iter()
                                                // A peripheral the silicon HAS but
                                                // no driver can reach is kept and
                                                // shown disabled, with the reason —
                                                // same as an exhausted one. It used
                                                // to vanish, which reads as a bug to
                                                // anyone holding the chip's
                                                // datasheet.
                                                .filter(|k| {
                                                    mcu.supports_module(*k)
                                                        || mcu.hardware_only_reason(*k).is_some()
                                                })
                                                .collect();
                                            for kind in offered {
                                                let hw_only = mcu.hardware_only_reason(kind);
                                                // Feasibility only. The pad
                                                // preview is the expensive half
                                                // and it is computed INSIDE the
                                                // submenu below, so it runs for
                                                // the one kind whose submenu is
                                                // open rather than for all of
                                                // them on every frame.
                                                let can_add =
                                                    hw_only.is_none() && mcu.can_add_module(kind);
                                                let hover = add_module_hint(kind);
                                                // Colour the button's TEXT with the peripheral's
                                                // colour ONLY when a module of this kind is already
                                                // on the chip — a glance shows what's wired; the
                                                // rest stay neutral. No background fill (it would
                                                // hurt text clarity), matching the list names below.
                                                let added = mcu.modules.iter().any(|m| m.kind == kind);
                                                // No `+` per item: the menu that holds them
                                                // is already called "Add module".
                                                let label = kind.short().to_owned();
                                                // 11, like every other button on this bar.
                                                // Without an explicit size they took the
                                                // global `TextStyle::Button` (12.5) and were
                                                // visibly bigger than the rest of the row.
                                                let text = if added {
                                                    egui::RichText::new(label)
                                                        .size(11.0)
                                                        .color(mod_gui::module_color(kind, 1))
                                                } else {
                                                    egui::RichText::new(label).size(11.0)
                                                };
                                                // A SUBMENU rather than a button.
                                                // The automatic wiring is right
                                                // most of the time and wrong in a
                                                // way the IDE cannot detect: which
                                                // pads the BOARD needs free is
                                                // nowhere in the pin model. So the
                                                // one-click path keeps the first
                                                // slot - one extra hover - and the
                                                // override sits beside it.
                                                if !can_add {
                                                    ui.add_enabled(false, egui::Button::new(text))
                                                        .on_disabled_hover_text(
                                                            add_module_block_reason(
                                                                kind,
                                                                hw_only,
                                                                mcu.has_free_instance(kind),
                                                            ),
                                                        );
                                                    continue;
                                                }
                                                // Custom is a one-click kind: it
                                                // has no signals, so there is
                                                // nothing to choose and a submenu
                                                // would offer an entry that does
                                                // nothing. Its pads are added in
                                                // its own config panel.
                                                if kind.is_custom() {
                                                    if ui
                                                        .add(egui::Button::new(text))
                                                        .on_hover_text(hover)
                                                        .clicked()
                                                    {
                                                        mcu.push_module_undo(format!(
                                                            "Add {}",
                                                            kind.short()
                                                        ));
                                                        if mcu.add_module(kind) {
                                                            modules_changed = true;
                                                            open_after_add = mcu
                                                                .modules
                                                                .last()
                                                                .map(|m| m.id.clone());
                                                            collapsed = false;
                                                        } else {
                                                            mcu.discard_last_module_undo();
                                                        }
                                                        ui.close();
                                                    }
                                                    continue;
                                                }
                                                ui.menu_button(text, |ui| {
                                                    ui.set_min_width(190.0);
                                                    // Computed HERE, not beside
                                                    // `can_add`: this closure
                                                    // runs only while THIS
                                                    // submenu is open, and the
                                                    // exhaustive search behind
                                                    // the label is the costliest
                                                    // thing on the bar. Asking
                                                    // it for every entry, every
                                                    // frame, was most of the
                                                    // palette's cost.
                                                    let auto_pads =
                                                        mod_gui::auto_wiring_summary(mcu, kind);
                                                    let auto_label = match &auto_pads {
                                                        Some(p) => format!("Auto - {p}"),
                                                        None => "Auto (best fit)".to_owned(),
                                                    };
                                                    if ui
                                                        .button(
                                                            egui::RichText::new(auto_label)
                                                                .size(11.0),
                                                        )
                                                        .on_hover_text(hover)
                                                        .clicked()
                                                    {
                                                        // Snapshot for Ctrl+Z BEFORE the add; drop it
                                                        // again if the add found no free pins.
                                                        mcu.push_module_undo(format!(
                                                            "Add {}",
                                                            kind.short()
                                                        ));
                                                        if mcu.add_module(kind) {
                                                            modules_changed = true;
                                                            // Adding from a collapsed bar means
                                                            // "I want to set this up" — so the
                                                            // panel opens with the new module's
                                                            // config already unfolded, instead
                                                            // of leaving the click looking like
                                                            // it did nothing.
                                                            open_after_add = mcu
                                                                .modules
                                                                .last()
                                                                .map(|m| m.id.clone());
                                                            collapsed = false;
                                                        } else {
                                                            mcu.discard_last_module_undo();
                                                        }
                                                        ui.close();
                                                    }
                                                    ui.separator();
                                                    if ui
                                                        .button(
                                                            egui::RichText::new("Choose pins...")
                                                                .size(11.0),
                                                        )
                                                        .on_hover_text(CHOOSE_PINS_HINT)
                                                        .clicked()
                                                    {
                                                        // No undo snapshot yet - the dialog
                                                        // can be cancelled, and a phantom
                                                        // entry would make the next Ctrl+Z
                                                        // look like it skipped a step.
                                                        pick_request = Some(kind);
                                                        ui.close();
                                                    }
                                                });
                                            }
                                        },
                                    );

                                // The device roster's two controls, immediately
                                // right of `+ Add module`. They used to head the
                                // LIST below, which put the panel's two "make
                                // something" gestures on different rows and hid
                                // one of them whenever the panel was collapsed to
                                // its bar.
                                ui.add_space(6.0);
                                if super::device_groups::device_add_button(ui, mcu) {
                                    // Same rule the palette follows: creating
                                    // from a collapsed bar means "I want to set
                                    // this up", so the panel opens on the new
                                    // row. Without this the click would appear to
                                    // do nothing — and the roster, which is what
                                    // unfolds that row, is not drawn at all while
                                    // the panel is shut.
                                    collapsed = false;
                                    device_added = true;
                                }

                                if mcu.can_undo_modules() {
                                     ui.separator();
                                    // Says MODULES explicitly, because `+ Device`
                                    // now stands two widgets to its left and
                                    // position implies coverage. It has none:
                                    // `ModuleUndo` holds `modules` and `pins`
                                    // only, and no roster gesture pushes a
                                    // snapshot — so pressing this after creating
                                    // a Device silently reverts the last MODULE
                                    // action instead, taking its pin assignments
                                    // with it. Extending the stack to Devices is
                                    // a real change (every roster gesture would
                                    // have to snapshot, or an Undo would clobber
                                    // the edits made since), not a line here.
                                    // An I2C bus's devices ARE on it: they live
                                    // in the module's config, and every edit of
                                    // them snapshots first (`edit_i2c_device`).
                                    let hover = format!(
                                        "Undo: {}  (Ctrl+Z)\n\nModules and the devices on an I2C bus. Adding, renaming or dissolving a module group is not on this stack.",
                                        mcu.last_module_undo_label().unwrap_or("last change")
                                    );
                                    if ui
                                        .button(
                                            egui::RichText::new(format!(
                                                "{} Undo",
                                                ph::ARROW_COUNTER_CLOCKWISE
                                            ))
                                            .size(11.0),
                                        )
                                        .on_hover_text(hover)
                                        .clicked()
                                    {
                                        mcu.undo_modules();
                                        modules_changed = true;
                                    }

                                }
                            });

                            // Ctrl+Z reverts the last module add/remove — but only
                            // when NO text field has focus, so the editor's own undo
                            // still works while the user is typing.
                            let undo_z = ui.memory(|m| m.focused().is_none())
                                && ui.input_mut(|i| {
                                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z)
                                });
                            if undo_z && mcu.can_undo_modules() {
                                mcu.undo_modules();
                                modules_changed = true;
                            }

                            // Id of a module clicked on the canvas last frame →
                            // TOGGLE its list entry this frame (expand if closed,
                            // collapse if open), then it's user-controlled again.
                            let to_open = mcu.expand_module.take();
                            // What the CANVAS decided that click means. Clicking
                            // a box toggles its selection, and a box is white
                            // because its config is showing - so this, not a
                            // second toggle down in the loop, is what the config
                            // has to follow.
                            let canvas_selected = mcu.selected_module.clone();
                            // Clicking a module box on the canvas has to REACH
                            // something: with the panel put away, the request
                            // would be taken here and quietly dropped, and the
                            // click would look broken.
                            if to_open.is_some() {
                                collapsed = false;
                            }
                            // Empty-canvas click last frame → close EVERY entry
                            // (the canvas cleared its selection at the same time).
                            let collapse_all = std::mem::take(&mut mcu.collapse_modules);
                            // The confirm row and its Cancel live inside ONE open
                            // config, so folding that config or collapsing the
                            // panel takes Cancel away while leaving the question
                            // armed - and the canvas goes on pulsing the box red,
                            // repainting every frame, at a question nobody can
                            // answer. `reconcile_modules` retires a confirm whose
                            // MODULE died; this retires one whose ROW is no longer
                            // drawn, and it sits OUTSIDE the gate below because
                            // that gate is exactly what strands it.
                            if let Some(id) = mcu.module_remove_confirm.clone() {
                                let row_shows = !collapsed
                                    && !collapse_all
                                    && egui::collapsing_header::CollapsingState::load_with_default_open(
                                        ui.ctx(),
                                        egui::Id::new(("vmod_hdr", id.as_str())),
                                        false,
                                    )
                                    .is_open();
                                if !row_shows {
                                    mcu.module_remove_confirm = None;
                                }
                            }
                            // The module selectors follow the STAGED runtime (what
                            // the user is about to Apply), not the applied one.
                            //
                            // EMBASSY async only. Its rows — the transport,
                            // the channel names, the I2C timeout — all come
                            // from `embassy-stm32`.
                            //
                            // The ESP is excluded, but NOT because it has no
                            // DMA: it has its own, and none of its controls
                            // turn on the runtime. `with_dma` is on
                            // `impl Spi<'d, Blocking>`, so the panel decides
                            // those by FAMILY instead (see `esp` in
                            // `module_config_ui`). The exclusion here used to
                            // hide every DMA control on an ESP outright.
                            let is_async = mcu.pending_is_async()
                                && !crate::panels::mcu_module::codegen::family::async_is_esp(
                                    &mcu.family,
                                );
                            let is_native = mcu.pending_is_native();

                            // A DEVICE is made of pads, not of modules: `loose_pins`
                            // exists precisely to offer the pads no module owns, and
                            // this panel is the only place a device is created
                            // (the bar's `+ Device`), renamed or dissolved (its
                            // roster below). Gated on modules alone, a chip
                            // whose pads are all hand-configured could not group
                            // anything, and removing the last module stranded every
                            // device that already existed.
                            let anything_to_group = mcu.iter_all_pins().any(|p| {
                                !p.reserved
                                    && p.selected_function
                                        != crate::panels::mcu_module::pins::logic::pin_function::PinFunction::Unset
                            });
                            let show_body = panel_show_body(
                                collapsed,
                                !mcu.modules.is_empty(),
                                !mcu.groups.is_empty(),
                                anything_to_group,
                                !mcu.orphan_notes().is_empty(),
                            );
                            if show_body {
                                use crate::panels::mcu_module::mcu::logic::module_style;
                                use crate::panels::mcu_module::modules::{ApiStyle, AsyncBusMode};
                                let pin_names: std::collections::HashMap<usize, String> = mcu
                                    .iter_all_pins()
                                    .map(|p| (p.number, p.name.clone()))
                                    .collect();
                                // Per-pin fingerprint (function + label) so a
                                // Custom module's Update button also lights up
                                // when a pin is renamed or flipped In->Out —
                                // both change the generated field names.
                                // Pins a Custom module must NOT offer — only FREE
                                // pins can be claimed:
                                //  • reserved (power / NRST — no user code),
                                //  • already assigned a function (a peripheral
                                //    pin is moved into its own `init(…)`, and a
                                //    configured GPIO already belongs somewhere),
                                //  • already claimed by another virtual module.
                                // Add the pin first, then give it its function —
                                // its rename field lives inside the module box.
                                let pin_blocked: std::collections::HashSet<usize> = mcu
                                    .iter_all_pins()
                                    .filter(|p| {
                                        p.reserved
                                            || p.selected_function
                                                != crate::panels::mcu_module::pins::logic::pin_function::PinFunction::Unset
                                    })
                                    .map(|p| p.number)
                                    .chain(mcu.modules.iter().flat_map(|other| {
                                        other.connections.iter().map(|c| c.mcu_pin)
                                    }))
                                    .collect();
                                // Editable copy of the per-pin labels: a Custom
                                // module's pin rows edit them here, mirroring the
                                // field inside its box on the canvas. Written back
                                // after the loop (which borrows `mcu.modules`).
                                let mut pin_labels: std::collections::HashMap<usize, String> = mcu
                                    .iter_all_pins()
                                    .map(|p| (p.number, p.custom_label.clone()))
                                    .collect();
                                let pin_labels_before = pin_labels.clone();
                                // Selectable functions per pin — a Custom module's
                                // pin buttons open this list, mirroring the picker
                                // inside the chip.
                                let pin_funcs: std::collections::HashMap<
                                    usize,
                                    Vec<crate::panels::mcu_module::pins::logic::pin_function::PinFunction>,
                                > = mcu
                                    .iter_all_pins()
                                    .filter(|p| !p.reserved)
                                    .map(|p| (p.number, p.available_functions.clone()))
                                    .collect();
                                // Set when the user picks one; applied after the
                                // loop (which holds `mcu.modules` mutably).
                                let pin_funcs_current: std::collections::HashMap<
                                    usize,
                                    crate::panels::mcu_module::pins::logic::pin_function::PinFunction,
                                > = mcu
                                    .iter_all_pins()
                                    .map(|p| (p.number, p.selected_function.clone()))
                                    .collect();
                                let mut pin_fn_choice: Option<
                                    crate::panels::mcu_module::mcu::gui::modules::PinEdit,
                                > = None;
                                let pin_sigs: std::collections::HashMap<usize, String> = mcu
                                    .iter_all_pins()
                                    .map(|p| {
                                        (
                                            p.number,
                                            format!("{:?}|{}", p.selected_function, p.custom_label),
                                        )
                                    })
                                    .collect();
                                // Removal is confirmed inline (it resets the module's
                                // pins). `confirm_id` = the module currently showing
                                // the confirm; the loop can't touch `mcu` while it
                                // borrows `mcu.modules`, so it signals via locals
                                // applied after the loop.
                                let confirm_id = mcu.module_remove_confirm.clone();
                                let mut remove_id: Option<String> = None; // confirmed → remove
                                // An I2C bus's device rows: they only collect,
                                // applied after the loop like the signals above.
                                let mut i2c_io = mod_gui_i2c::I2cIo {
                                    confirm: mcu.i2c_remove_confirm,
                                    acts: Vec::new(),
                                };
                                // A pin the details pane wants shown in the
                                // editor. `request_pin_goto` is the existing,
                                // deferred path — `AppIde` consumes it after the
                                // frame, so the pane never touches the editor
                                // itself.
                                let mut goto_pin: Option<usize> = None;
                                // What each open config had to say beyond its
                                // controls, by module id: the standing remarks,
                                // and what every row it drew MEANS. Filled
                                // below, drawn by the details pane — which is
                                // further right in the same row, so it reads
                                // them the same frame.
                                let mut said: std::collections::HashMap<
                                    String,
                                    module_docs::ConfigOut,
                                > = std::collections::HashMap::new();
                                let mut arm_confirm: Option<String> = None; // show the confirm
                                let mut cancel_confirm = false;
                                // Pull the staged per-module styles into a LOCAL map so
                                // the config panels can edit them without borrowing
                                // `mcu` while `mcu.modules` is iterated. Seeded from the
                                // current staged value, else the module's applied style.
                                // Cloned once per frame: the picker needs the
                                // chip's channels while `mcu.modules` is
                                // borrowed mutably below.
                                let chip_dma = mcu.dma.clone();
                                // The clock, read now for the same reason: the
                                // baud row reports against it.
                                let baud_chip =
                                    crate::panels::mcu_module::uart_baud::Chip::of(mcu);
                                let family = mcu.family.clone();
                                // Read before `mcu.modules` is borrowed mutably.
                                let usart_line_extras =
                                    crate::panels::mcu_module::stm32_pin_data::usart_has_swap_invert(
                                        mcu.usart_ip.as_deref(),
                                    );
                                // Which nRF SPIM / TWIM modules share one
                                // block, by module id, on the STAGED runtime
                                // the rows below describe. Also read now: each
                                // config panel sees one module, and the pair
                                // is a fact about the pads of the others.
                                let block_partner: std::collections::HashMap<String, String> =
                                    mcu.modules
                                        .iter()
                                        .filter_map(|m| {
                                            crate::panels::mcu_module::codegen::nrf::shared_block_partner(
                                                mcu, m, is_async,
                                            )
                                            .map(|p| (m.id.clone(), p))
                                        })
                                        .collect();
                                let mut local_pending: std::collections::BTreeMap<
                                    String,
                                    (ApiStyle, AsyncBusMode),
                                > = mcu
                                    .modules
                                    .iter()
                                    .map(|m| {
                                        let cur = mcu
                                            .pending_module_styles
                                            .get(&m.id)
                                            .copied()
                                            .unwrap_or_else(|| module_style(&m.config));
                                        (m.id.clone(), cur)
                                    })
                                    .collect();
                                // ── Two columns: the list on the LEFT, the
                                //    open modules' configs on the RIGHT, one
                                //    under another.
                                //
                                // One column could not hold both: a USART's
                                // config is a dozen rows, so an expanded module
                                // pushed every other one off the bottom of the
                                // panel and the list stopped being a list.
                                //
                                // The open/closed bit still lives in a
                                // `CollapsingState`, but keyed off the MODULE ID
                                // rather than the Ui: the header and the body are
                                // in different Uis now, so a Ui-derived id would
                                // no longer find the state the other one stored.
                                let cs_id =
                                    |id: &str| egui::Id::new(("vmod_hdr", id));
                                let list_w = (ui.available_width() * 0.32).clamp(160.0, 260.0);
                                // Filled by the left column, read by the right —
                                // the two loops borrow `mcu.modules` in turn.
                                let mut open_ids: Vec<String> = Vec::new();
                                let mut clicked: Option<String> = None;
                                // Notes: an orphan deleted in the list is applied
                                // after it, like `clicked` - the list is drawn
                                // while `mcu.modules` is borrowed. The project
                                // folder and the image cache are other fields of
                                // `self`, so they can ride along beside `mcu`.
                                let mut orphan_delete: Option<
                                    crate::panels::mcu_module::modules::NotesKey,
                                > = None;
                                let notes_root = self.project_dir.clone();
                                let note_images = &mut self.module_note_images;
                                // Deferred, like `clicked`: the (i) button is
                                // drawn inside the loop that borrows
                                // `mcu.modules`, and flipping `info_id` there
                                // would also make the pane appear or vanish
                                // mid-column.
                                let mut info_toggle: Option<String> = None;
                                // Everything the panel spends ABOVE the columns
                                // — the Apply bar, the palette row, the spacing.
                                // Measured, not guessed: a new button in that
                                // toolbar would otherwise start clipping the
                                // bottom of every config.

                                // The body is allocated at OUR height, so the
                                // panel that hugs it ends up exactly as tall as
                                // the handle says.
                                let body = egui::vec2(ui.available_width(), body_h);
                                ui.allocate_ui(body, |ui| {
                                ui.horizontal_top(|ui| {
                                    // ── left: the list ──
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(list_w, ui.available_height()),
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            let list_out = egui::ScrollArea::vertical()
                                                .id_salt("vmod_list")
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    // ── the devices, above the
                                                    //    modules that make them
                                                    //    up. Drawn FIRST, and
                                                    //    with `mcu` still free:
                                                    //    the module loop below
                                                    //    borrows it for the rest
                                                    //    of the closure.
                                                    super::device_groups::device_roster(
                                                        ui,
                                                        mcu,
                                                        device_added,
                                                    );
                                                    ui.add_space(4.0);
                                                    ui.separator();
                                                    ui.add_space(2.0);
                                                    for m in &mcu.modules {
                                                        let mut st =
                                                            egui::collapsing_header::CollapsingState::load_with_default_open(
                                                                ui.ctx(),
                                                                cs_id(&m.id),
                                                                false,
                                                            );
                                                        // "Collapse everything"
                                                        // outranks the per-module
                                                        // toggle: both can't be
                                                        // true in one frame, but
                                                        // if they ever were,
                                                        // closing is the safe
                                                        // answer.
                                                        if collapse_all {
                                                            st.set_open(false);
                                                        } else if let Some(t) =
                                                            to_open.as_deref()
                                                        {
                                                            st.set_open(
                                                                config_open_after_click(
                                                                    &m.id,
                                                                    t,
                                                                    canvas_selected
                                                                        .as_deref(),
                                                                ),
                                                            );
                                                        }
                                                        let open = st.is_open();
                                                        st.store(ui.ctx());
                                                        if open {
                                                            open_ids.push(m.id.clone());
                                                        }

                                                        let title = mod_gui::module_title(m);
                                                        let c = mod_gui::module_color(
                                                            m.kind,
                                                            m.instance(),
                                                        );
                                                        // The same idea as the
                                                        // module's box on the
                                                        // canvas: the peripheral's
                                                        // colour, well diluted. Not
                                                        // the same NUMBER - the box
                                                        // lerps 16 % towards it over
                                                        // an opaque ground, this
                                                        // lays 10 % alpha over
                                                        // whatever is behind. The
                                                        // OPEN one is brighter, so
                                                        // the list says which config
                                                        // is on the right.
                                                        let bg = egui::Color32::from_rgba_unmultiplied(
                                                            c.r(),
                                                            c.g(),
                                                            c.b(),
                                                            if open { 56 } else { 26 },
                                                        );
                                                        let resp = egui::Frame::new()
                                                            .fill(bg)
                                                            .inner_margin(egui::Margin::symmetric(6, 3))
                                                            .corner_radius(egui::CornerRadius::same(4))
                                                            .show(ui, |ui| {
                                                                ui.set_width(ui.available_width());
                                                                ui.horizontal(|ui| {
                                                                    ui.label(
                                                                        egui::RichText::new(if open {
                                                                            ph::CARET_DOWN
                                                                        } else {
                                                                            ph::CARET_RIGHT
                                                                        })
                                                                        .size(11.0)
                                                                        .color(c),
                                                                    );
                                                                    ui.label(
                                                                        egui::RichText::new(title)
                                                                            .strong()
                                                                            .color(c),
                                                                    );
                                                                    // Says the module has notes
                                                                    // without opening it; the
                                                                    // hover is their first lines.
                                                                    if let Some(p) = mcu
                                                                        .notes_for(m)
                                                                        .and_then(|n| n.preview())
                                                                    {
                                                                        ui.label(
                                                                            egui::RichText::new(ph::NOTE)
                                                                                .size(11.0)
                                                                                .color(c),
                                                                        )
                                                                        .on_hover_text(p);
                                                                    }
                                                                });
                                                            })
                                                            .response
                                                            .interact(egui::Sense::click());
                                                        if resp
                                                            .on_hover_cursor(
                                                                egui::CursorIcon::PointingHand,
                                                            )
                                                            .clicked()
                                                        {
                                                            clicked = Some(m.id.clone());
                                                        }
                                                        ui.add_space(2.0);
                                                    }
                                                    super::module_notes_ui::orphan_rows(
                                                        ui,
                                                        mcu,
                                                        notes_root.as_deref(),
                                                        &mut orphan_delete,
                                                    );
                                                });
                                            // What "show me the whole list"
                                            // costs — the caret button opens the
                                            // panel to exactly this.
                                            list_h = list_out.content_size.y + 8.0;
                                        },
                                    );
                                    ui.separator();

                                    // ── right: one config block per open module ──
                                    //
                                    // The explicit `top_down` is load-bearing: a
                                    // `ScrollArea` inherits its parent's layout,
                                    // and the parent here is a HORIZONTAL one —
                                    // so without it the title and the Name row
                                    // laid out side by side and the config's grid
                                    // had nowhere left to go.
                                    // The pane is only ever shown for a module
                                    // that is actually OPEN — collapsing one (or
                                    // removing it) takes its details with it,
                                    // rather than leaving a column describing
                                    // something no longer on screen.
                                    // The config column's REAL floor. Its rows are
                                    // a label beside a ComboBox or a fixed-width
                                    // `TextEdit`, and neither shrinks below its
                                    // own desired width whatever rect it is handed
                                    // - a column asked for less does not get
                                    // smaller, it overflows and eats the pane's
                                    // share. The old 120 floor was a width nothing
                                    // in there could honour, so on a narrow zone
                                    // the pane was allocated whatever was left
                                    // after the overflow: a ~60 px shred of
                                    // one-word-per-line text.
                                    const CFG_MIN_W: f32 = 215.0;
                                    // Narrower than this a pane says less than the
                                    // space it costs, so it is not opened at all -
                                    // the (i) button stays unlit rather than
                                    // lighting up over a sliver.
                                    const INFO_MIN_W: f32 = 180.0;
                                    let room = ui.available_width();
                                    // Read by the (i) button as well as by the
                                    // pane. Gating only the pane made the button
                                    // a switch that lit up, changed its tooltip
                                    // to "Hide the details pane", and opened
                                    // nothing - and since the id it wrote is
                                    // persisted, it stayed lit across frames.
                                    let info_fits = room - CFG_MIN_W >= INFO_MIN_W;
                                    let info_for = info_id
                                        .clone()
                                        .filter(|id| open_ids.iter().any(|o| o == id))
                                        .filter(|_| info_fits);
                                    let info_w = if info_for.is_some() {
                                        (room * 0.40).clamp(INFO_MIN_W, 420.0)
                                    } else {
                                        0.0
                                    };
                                    let cfg_size = egui::vec2(
                                        (room - info_w).max(CFG_MIN_W),
                                        ui.available_height(),
                                    );
                                    let out = ui
                                        .allocate_ui_with_layout(
                                            cfg_size,
                                            egui::Layout::top_down(egui::Align::Min),
                                            |ui| {
                                                egui::ScrollArea::vertical()
                                                    .id_salt("vmod_cfg")
                                                    .auto_shrink([false, false])
                                                    .show(ui, |ui| {
                                            // A HARD cap, not just an allocation:
                                            // a child wider than the rect it was
                                            // given still overflows, and the
                                            // horizontal row then places the
                                            // details pane after the width
                                            // actually used. That is how one long
                                            // label squeezed the pane down to a
                                            // word. With a max width, labels wrap
                                            // instead.
                                            ui.set_max_width(cfg_size.x);
                                            if open_ids.is_empty() {
                                                ui.add_space(6.0);
                                                ui.label(
                                                    egui::RichText::new(
                                                        "Click a module on the left to configure it here.",
                                                    )
                                                    .size(11.0)
                                                    .color(egui::Color32::from_gray(130)),
                                                );
                                                return;
                                            }
                                            for m in &mut mcu.modules {
                                                if !open_ids.iter().any(|id| id == &m.id) {
                                                    continue;
                                                }
                                                let title = mod_gui::module_title(m);
                                                let mod_color =
                                                    mod_gui::module_color(m.kind, m.instance());
                                                let bg = egui::Color32::from_rgba_unmultiplied(
                                                    mod_color.r(),
                                                    mod_color.g(),
                                                    mod_color.b(),
                                                    26,
                                                );
                                                // The title rides along with the
                                                // config: with several open at
                                                // once, an unlabelled block would
                                                // not say whose it is.
                                                ui.horizontal(|ui| {
                                                    egui::Frame::new()
                                                        .fill(bg)
                                                        .inner_margin(egui::Margin::symmetric(
                                                            6, 2,
                                                        ))
                                                        .corner_radius(egui::CornerRadius::same(4))
                                                        .show(ui, |ui| {
                                                            ui.label(
                                                                egui::RichText::new(title)
                                                                    .strong()
                                                                    .color(mod_color),
                                                            );
                                                        });
                                                    // Pushed to the far right of
                                                    // the config column, which is
                                                    // the edge the details pane
                                                    // then opens along — so the
                                                    // button sits on the thing it
                                                    // summons.
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            let showing = info_fits
                                                                && info_id.as_deref()
                                                                    == Some(m.id.as_str());
                                                            ui.add_enabled_ui(
                                                                info_fits,
                                                                |ui| {
                                                                    let r = ui.selectable_label(
                                                                        showing,
                                                                        egui::RichText::new(
                                                                            ph::INFO,
                                                                        )
                                                                        .size(14.0)
                                                                        .color(mod_color),
                                                                    );
                                                                    let r = if info_fits {
                                                                        r.on_hover_text(
                                                                            if showing {
                                                                                "Hide the details \
                                                                                 pane"
                                                                            } else {
                                                                                "What this module \
                                                                                 holds and where \
                                                                                 its pins land in \
                                                                                 the generated \
                                                                                 code"
                                                                            },
                                                                        )
                                                                    } else {
                                                                        r.on_disabled_hover_text(
                                                                            "There is no room for \
                                                                             the details pane \
                                                                             here — widen the MCU \
                                                                             zone to open one",
                                                                        )
                                                                    };
                                                                    if r.clicked() {
                                                                        info_toggle =
                                                                            Some(m.id.clone());
                                                                    }
                                                                },
                                                            );
                                                        },
                                                    );
                                                });
                                                // Rename field — appended to the generated
                                                // variable name(s); also shown in the title.
                                                ui.horizontal(|ui| {
                                                    // Custom modules label their
                                                    // fields in BOLD (the user's
                                                    // ask) so the hand-authored
                                                    // panel reads apart from the
                                                    // peripheral ones.
                                                    // Fixed-width label there, so
                                                    // this field and the Struct
                                                    // one below it (a different
                                                    // container) line up.
                                                    if m.kind.is_custom() {
                                                        mod_gui::custom_field_label(ui, "Name:");
                                                    } else {
                                                        ui.label("Name:");
                                                    }
                                                    ui.add(
                                                        egui::TextEdit::singleline(
                                                            m.config.custom_label_mut(),
                                                        )
                                                        .hint_text("variable name")
                                                        .desired_width(mod_gui::CUSTOM_FIELD_W),
                                                    );
                                                });
                                                let pending = local_pending
                                                    .get_mut(&m.id)
                                                    .expect("pending entry seeded above");
                                                // One id namespace per module.
                                                // `module_config_ui` builds a
                                                // `Grid::new("module_cfg")` and
                                                // salt-less combo boxes, so two
                                                // configs drawn in the SAME frame
                                                // — which is what the right column
                                                // does — collide on every one of
                                                // them, and egui paints an
                                                // "ID clash" banner across the
                                                // panel.
                                                // Owned: the closure needs `m`
                                                // mutably, so the salt cannot
                                                // borrow out of it.
                                                let salt = m.id.clone();
                                                let mut my_out =
                                                    module_docs::ConfigOut::default();
                                                ui.push_id(salt, |ui| {
                                                    mod_gui::module_config_ui(
                                                        ui, m, &pin_names, &pin_sigs,
                                                        &pin_blocked, &mut pin_labels,
                                                        &pin_funcs_current, &pin_funcs,
                                                        &mut pin_fn_choice, is_async, is_native,
                                                        &family, pending, chip_dma.as_ref(),
                                                        usart_line_extras,
                                                        block_partner.get(&m.id).map(String::as_str),
                                                        &mut my_out, &baud_chip, &mut i2c_io,
                                                    );
                                                });
                                                if !my_out.is_empty() {
                                                    said.insert(m.id.clone(), my_out);
                                                }
                                                ui.add_space(4.0);
                                                // Notes, link, image. Outside the
                                                // settings grid on purpose: a
                                                // TextEdit in a grid column that is
                                                // not the last one collapses to
                                                // 40 px for good. Keyed by the
                                                // module's (kind, instance), not
                                                // by `m` - see `Mcu::module_notes`.
                                                super::module_notes_ui::notes_section(
                                                    ui,
                                                    &mut mcu.module_notes,
                                                    (m.kind, m.instance()),
                                                    notes_root.as_deref(),
                                                    note_images,
                                                );
                                                ui.add_space(4.0);
                                                if confirm_id.as_deref() == Some(m.id.as_str()) {
                                                    // Armed → inline confirm (removing
                                                    // resets this module's pins).
                                                    ui.horizontal(|ui| {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "{} Remove this module & free its pins?",
                                                                ph::WARNING
                                                            ))
                                                            .size(11.5)
                                                            .color(egui::Color32::from_rgb(220, 180, 90)),
                                                        );
                                                        if ui
                                                            .button(
                                                                egui::RichText::new(format!(
                                                                    "{} Remove",
                                                                    ph::TRASH
                                                                ))
                                                                .color(egui::Color32::from_rgb(220, 80, 80)),
                                                            )
                                                            .clicked()
                                                        {
                                                            remove_id = Some(m.id.clone());
                                                        }
                                                        if ui.button("Cancel").clicked() {
                                                            cancel_confirm = true;
                                                        }
                                                    });
                                                } else if ui
                                                    // Red TEXT signals the destructive
                                                    // action; the fill stays default.
                                                    .button(
                                                        egui::RichText::new(format!(
                                                            "{} Remove module",
                                                            ph::TRASH
                                                        ))
                                                        .color(egui::Color32::from_rgb(220, 80, 80)),
                                                    )
                                                    .clicked()
                                                {
                                                    arm_confirm = Some(m.id.clone());
                                                }
                                                // Where one module's config ends
                                                // and the next begins. With
                                                // several open the blocks ran
                                                // together and only the coloured
                                                // title said a new one had
                                                // started.
                                                ui.add_space(8.0);
                                                ui.separator();
                                                ui.add_space(6.0);
                                            }
                                                    })
                                            },
                                        )
                                        .inner;
                                    // + a row of slack so the last button is not
                                    // flush against the panel edge.
                                    needed_h = out.content_size.y + 16.0;

                                    // ── far right: the details pane ──
                                    if let Some(id) = info_for {
                                        ui.separator();
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(
                                                ui.available_width(),
                                                ui.available_height(),
                                            ),
                                            egui::Layout::top_down(egui::Align::Min),
                                            |ui| {
                                                egui::ScrollArea::vertical()
                                                    .id_salt("vmod_info")
                                                    .auto_shrink([false, false])
                                                    .show(ui, |ui| {
                                                        module_details_ui(
                                                            ui,
                                                            mcu,
                                                            &id,
                                                            &pin_names,
                                                            said.get(&id),
                                                            &mut goto_pin,
                                                        );
                                                    });
                                            },
                                        );
                                    }
                                });
                                });

                                // Which modules are open, as one number, so the
                                // panel can be resized ONCE when the set changes.
                                open_sig = open_ids.iter().fold(0u64, |h, id| {
                                    id.bytes().fold(h.wrapping_mul(31).wrapping_add(1), |h, b| {
                                        h.wrapping_mul(131).wrapping_add(u64::from(b))
                                    })
                                });
                                // Applied after both loops — each borrowed
                                // `mcu.modules`, and the toggle only touches
                                // egui's own state.
                                if let Some(n) = goto_pin {
                                    mcu.request_pin_goto(n);
                                }
                                // Toggling OFF is the same click again — the
                                // button is a `selectable_label`, so it reads as
                                // a switch and behaves like one.
                                if let Some(id) = info_toggle {
                                    info_id = if info_id.as_deref() == Some(id.as_str()) {
                                        None
                                    } else {
                                        Some(id)
                                    };
                                }
                                if let Some(key) = orphan_delete {
                                    mcu.delete_notes(key);
                                }
                                if let Some(id) = clicked {
                                    let mut st =
                                        egui::collapsing_header::CollapsingState::load_with_default_open(
                                            ui.ctx(),
                                            cs_id(&id),
                                            false,
                                        );
                                    let was_open = st.is_open();
                                    st.set_open(!was_open);
                                    st.store(ui.ctx());
                                    // Closing a config from the LIST also drops
                                    // the white border its box wears on the
                                    // canvas. To the user those are one state:
                                    // the box was called out BECAUSE its config
                                    // was showing, and the list is where that
                                    // showing ends. Left set, the diagram kept a
                                    // box picked out with nothing open to say
                                    // why.
                                    if was_open {
                                        mcu.config_collapsed(&id);
                                    }
                                }
                                // A module added this frame is not in the list
                                // yet (it was built after the loop ran), so its
                                // config is opened here and shows next frame —
                                // by which time `open_sig` has changed and the
                                // panel has already grown to fit it.
                                if let Some(id) = &open_after_add {
                                    let mut st =
                                        egui::collapsing_header::CollapsingState::load_with_default_open(
                                            ui.ctx(),
                                            cs_id(id),
                                            false,
                                        );
                                    st.set_open(true);
                                    st.store(ui.ctx());
                                    ui.ctx().request_repaint();
                                }
                                // Write the edited staged styles back onto the MCU.
                                mcu.pending_module_styles = local_pending;
                                // A function picked from a Custom module's pin
                                // button — same path as clicking the pin on the
                                // chip, so partners/labels stay consistent.
                                match pin_fn_choice {
                                    Some(mod_gui::PinEdit::Set(num, func)) => {
                                        mcu.apply_pin_function(num, func);
                                        modules_changed = true;
                                    }
                                    // A signal dragged to another pad. One
                                    // operation on purpose - the two-step form
                                    // either wipes the bus or doubles the wire.
                                    Some(mod_gui::PinEdit::Move { from, to }) => {
                                        modules_changed = mcu.move_pin_function(from, to);
                                    }
                                    None => {}
                                }
                                // Pin labels a Custom module's rows edited (the
                                // mirror of the field inside its canvas box) —
                                // applied now that `mcu.modules` is free again.
                                for (num, label) in &pin_labels {
                                    if pin_labels_before.get(num) != Some(label) {
                                        if let Some(p) = mcu.find_pin_mut(*num) {
                                            p.custom_label = label.clone();
                                        }
                                    }
                                }
                                // I2C device edits: handed to the canvas, which
                                // applies them with its own once it has drawn
                                // (see `Mcu::pending_i2c_acts`).
                                mcu.pending_i2c_acts.append(&mut i2c_io.acts);
                                // Apply the inline remove-confirm signals.
                                if let Some(id) = arm_confirm {
                                    mcu.module_remove_confirm = Some(id);
                                }
                                if cancel_confirm {
                                    mcu.module_remove_confirm = None;
                                }
                                if let Some(id) = remove_id {
                                    // Snapshot for Ctrl+Z, then remove + free pins.
                                    let title = mcu
                                        .modules
                                        .iter()
                                        .find(|m| m.id == id)
                                        .map(mod_gui::module_title)
                                        .unwrap_or_else(|| "module".to_owned());
                                    mcu.push_module_undo(format!("Remove {title}"));
                                    mcu.remove_module(&id);
                                    mcu.module_remove_confirm = None;
                                    // The id is freed and `free_module_id` hands
                                    // it straight to the next module, so anything
                                    // still keyed on it would greet a stranger:
                                    // the details pane open on its own, the row
                                    // already unfolded. `selected_module` and the
                                    // staged style are retired by
                                    // `reconcile_modules`; these two live on the
                                    // app, out of its reach.
                                    if info_id.as_deref() == Some(id.as_str()) {
                                        info_id = None;
                                    }
                                    let mut st =
                                        egui::collapsing_header::CollapsingState::load_with_default_open(
                                            ui.ctx(),
                                            cs_id(&id),
                                            false,
                                        );
                                    st.set_open(false);
                                    st.store(ui.ctx());
                                    modules_changed = true;
                                }
                            }
                        });

                    // One frame late by construction (the height of a config is
                    // only known once it has been laid out), so a change has to
                    // repaint — otherwise the panel settles on the next mouse
                    // move instead of now.
                    if (self.vmod_needed_h - needed_h).abs() > 0.5 {
                        self.vmod_needed_h = needed_h;
                        ui.ctx().request_repaint();
                    }
                    // Make room ONCE, when the set of open configs changes —
                    // never every frame, or the handle could not be dragged back
                    // down. Growing only: nobody wants the panel they just
                    // widened snapping shut because the next config is shorter.
                    if self.vmod_open_sig != open_sig && !collapsed {
                        body_h = body_h.max(self.vmod_needed_h.min(auto_cap));
                        ui.ctx().request_repaint();
                    }
                    if (self.vmod_list_h - list_h).abs() > 0.5 && list_h > 0.0 {
                        self.vmod_list_h = list_h;
                    }
                    // Created with `+ Device` from a bar the user had dragged
                    // shut: open tall enough to show the roster, or the row that
                    // was just made is below the fold and the click reads as a
                    // no-op.
                    //
                    // Dragging the top border past the floor stores
                    // `body_h = MIN_BODY_H` (60), so un-collapsing alone reopens
                    // a 60 px body — barely two folded device rows. The two
                    // neighbouring gestures are both already rescued from this
                    // and by different routes: `+ Add module` through the
                    // `vmod_open_sig` growth just above (it unfolds a config, so
                    // the signature changes), and the caret through the block
                    // below. `device_added` fed neither.
                    //
                    // Growing only, and capped like the caret's: nobody wants
                    // the panel they just sized snapping taller than the diagram
                    // can spare.
                    if device_added {
                        body_h = body_h.max(self.vmod_list_h.min(auto_cap));
                        ui.ctx().request_repaint();
                    }
                    // Expanded with the caret button: open tall enough for the
                    // whole module list, and close every config on the way —
                    // the gesture means "show me what modules there are", not
                    // "restore the six-row form I was last editing". Uses the
                    // height measured on a PREVIOUS frame (this one was laid out
                    // collapsed, so the list has no size yet); `body_cap` still
                    // has the last word, since the diagram above needs to live.
                    if expand_clicked {
                        body_h = self.vmod_list_h.clamp(60.0, body_cap);
                        if let Some(mcu) = &mut self.mcu {
                            mcu.all_configs_collapsed();
                        }
                        ui.ctx().request_repaint();
                    }
                    // Only a frame that actually DREW the configs knows which are
                    // open. A collapsed frame draws none, so storing 0 here made
                    // the next re-open look like "the set of open configs
                    // changed" and re-fit the panel to 45% of the zone - in the
                    // same frame the user was dragging its top border up from the
                    // bar, so a small panel could not be re-established by
                    // dragging at all.
                    //
                    // Guarding the STORE and not the test that reads it is what
                    // keeps "add a module from the collapsed bar" working: that
                    // path needs the signature to change from the preserved one
                    // to the preserved one plus the new module, which it still
                    // does.
                    if !collapsed {
                        self.vmod_open_sig = open_sig;
                    }
                    self.vmod_collapsed = collapsed;
                    self.vmod_body_h = body_h;
                    self.vmod_info_id = info_id;

                    // ── "Choose pins..." ─────────────────────────────────
                    // Seeded here, not in the click handler: the seed runs the
                    // same `pick_pins` the plain add would, which needs `&mcu`
                    // while the panel above still holds it borrowed.
                    if let Some(kind) = pick_request
                        && let Some(mcu) = &self.mcu
                    {
                        self.add_module_pick =
                            crate::app::add_module_dialog::AddModulePick::seed(mcu, kind);
                    }
                    // Seeded from a chip that is no longer loaded - a project
                    // switch while this was open. The pad numbers mean nothing
                    // now, so it goes rather than reappearing over the new chip.
                    if let (Some(pick), Some(mcu)) = (&self.add_module_pick, &self.mcu)
                        && !crate::app::add_module_dialog::belongs_to(pick, mcu)
                    {
                        self.add_module_pick = None;
                    }
                    if let Some(pick) = &mut self.add_module_pick {
                        use crate::app::add_module_dialog::{PickOutcome, show as show_pick};
                        let outcome = match &self.mcu {
                            Some(mcu) => show_pick(ui, mcu, pick),
                            None => PickOutcome::Cancelled,
                        };
                        match outcome {
                            PickOutcome::Pending => {}
                            PickOutcome::Cancelled => self.add_module_pick = None,
                            PickOutcome::Confirmed => {
                                let (kind, inst, wiring) =
                                    (pick.kind, pick.instance, pick.wiring());
                                if let Some(mcu) = &mut self.mcu {
                                    // The undo snapshot goes here rather than
                                    // where the dialog opened: a cancelled
                                    // dialog must leave the Ctrl+Z stack
                                    // exactly as it found it.
                                    mcu.push_module_undo(format!("Add {}", kind.short()));
                                    mcu.add_module_wired(inst, &wiring);
                                    modules_changed = true;
                                    // Same courtesy as the plain add: the new
                                    // module opens with its config unfolded.
                                    open_after_add = mcu.modules.last().map(|m| m.id.clone());
                                    self.vmod_collapsed = false;
                                }
                                self.add_module_pick = None;
                            }
                        }
                    }
                    // The dialog commits AFTER the panel body has run, so the
                    // module it created was never in this frame's list and the
                    // loop that unfolds a new module's config never saw it.
                    // Unfolding it here writes the same egui state that loop
                    // does, keyed the same way (`cs_id`, line 1315).
                    //
                    // `set_open` takes `&mut self` on a value loaded from the
                    // context - it changes a COPY, and without `store` nothing
                    // reaches egui at all. The config simply stayed folded on
                    // this path while the one-click path opened it.
                    //
                    // The details pane is deliberately NOT touched: opening it
                    // unasked squeezes the config column that was just
                    // unfolded, and the plain "Auto" add never did that.
                    if let Some(id) = open_after_add {
                        let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(
                            ui.ctx(),
                            egui::Id::new(("vmod_hdr", id.as_str())),
                            false,
                        );
                        st.set_open(true);
                        st.store(ui.ctx());
                        ui.ctx().request_repaint();
                    }

                    if modules_changed {
                        if let Some(mcu) = &self.mcu {
                            let all_pins = mcu.all_pin_functions();
                            self.project_tree.sync_pin_files(&all_pins);
                        }
                    }
                    // ── The diagram's own toolbar, at its top-left ────────
                    // Rotate and the pin search are VIEW controls: they change
                    // how the chip is DRAWN and nothing else. They used to sit
                    // at the far right of the Virtual-modules bar, whose every
                    // other control adds, removes or undoes a module — so the
                    // two things you do to the picture were the two furthest
                    // from it. Here they sit on the thing they act on.
                    if self.mcu.is_some() {
                        ui.horizontal(|ui| {
                            let Some(mcu) = &mut self.mcu else { return };
                            // Asked of the SAME predicate the rotation itself
                            // uses. Two hand-written package tests is how the
                            // button came to promise a 90° turn on a ball grid
                            // that the renderer would have made a diamond.
                            let rot_hint =
                                crate::panels::mcu_module::mcu::gui::rotate::RotMode::for_package(
                                    mcu,
                                )
                                .hint();
                            if ui
                                .selectable_label(
                                    mcu.rotated,
                                    egui::RichText::new(format!("{} Rotate", ph::ARROW_CLOCKWISE))
                                        .size(11.0),
                                )
                                .on_hover_text(rot_hint)
                                .clicked()
                            {
                                mcu.rotated = !mcu.rotated;
                                rotate_toggled = true;
                                // ANY orientation change → clean auto-layout:
                                // drop the manual drag positions (they don't
                                // transfer between 0° / 90° / diamond) so the
                                // modules + in/out fields snap beside their pins
                                // for the new orientation.
                                for m in &mut mcu.modules {
                                    m.pos = (0.0, 0.0);
                                }
                                mcu.io_pin_pos.clear();
                            }

                            // Find a pin on the diagram: matches stay bright,
                            // every other pin fades. Purely a view filter —
                            // nothing is selected or changed.
                            ui.separator();
                            ui.label(
                                egui::RichText::new(ph::MAGNIFYING_GLASS)
                                    .size(12.0)
                                    .color(egui::Color32::from_gray(160)),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut mcu.pin_search)
                                    .hint_text("find pin")
                                    .desired_width(96.0)
                                    .font(egui::FontId::proportional(11.0)),
                            )
                            .on_hover_text(
                                "Highlight pins by NAME (any part: pa5, osc, pb) or by \
                                 NUMBER (exact: 13). Everything else fades to 30%.",
                            );
                            if !mcu.pin_search.trim().is_empty() {
                                // Match count doubles as the "nothing found"
                                // signal — the diagram deliberately does NOT fade
                                // at all when a query matches nothing.
                                let n = mcu.pin_search_hits().len();
                                let (txt, col) = if n == 0 {
                                    (
                                        "no match".to_owned(),
                                        egui::Color32::from_rgb(220, 160, 70),
                                    )
                                } else {
                                    (
                                        format!("{n} pin{}", if n == 1 { "" } else { "s" }),
                                        egui::Color32::from_gray(150),
                                    )
                                };
                                ui.label(egui::RichText::new(txt).size(10.5).color(col));
                                if ui
                                    .button(egui::RichText::new(ph::X).size(11.0))
                                    .on_hover_text("Clear the search")
                                    .clicked()
                                {
                                    mcu.pin_search.clear();
                                }
                            }
                        });
                    }

                    // Rotating re-orients the whole chip, so drop any manual
                    // zoom/pan and let the canvas auto-fit the new layout.
                    //
                    // Read AFTER the toolbar above, which is what sets it now:
                    // when the toggle lived on the bottom panel this ran first,
                    // and moving the control without moving this check would
                    // have left the re-fit one frame late.
                    if rotate_toggled {
                        self.mcu_view_adjusted = false;
                    }

                    // Diagram fills the remaining (top) area.
                    let pin_changed = match &mut self.mcu {
                        Some(mcu) => {
                            // The chip + virtual modules are drawn at their
                            // natural fixed size inside an `egui::Scene` (egui's
                            // pan/zoom container). DEFAULT = auto-fit: the scene
                            // rect is refilled from last frame's content bounds,
                            // so the canvas rescales to fit the panel (window /
                            // panel resizes, added modules). The user can then
                            // take over the view exactly like the Structure tab —
                            // mouse wheel = zoom to the cursor, Ctrl+± = zoom,
                            // drag / middle-drag = pan, up to 4×; that latches
                            // `mcu_view_adjusted` and the view PERSISTS. Ctrl+0
                            // re-fits.
                            // `avail`/`outer` = the rect the Scene will fill —
                            // `outer` maps the cursor into scene space (for the
                            // wheel zoom), `avail` keeps AUTO-fit capped at 100%.
                            let avail = ui.available_size_before_wrap();
                            let outer = ui.available_rect_before_wrap();
                            let mut scene_rect = self.mcu_scene_bounds;
                            let mut content_bounds = egui::Rect::NOTHING;

                            // Structure-style plain mouse-wheel zoom, anchored at
                            // the pointer. egui's Scene PANS on plain scroll, so
                            // we intercept the wheel and turn it into a cursor
                            // zoom (Ctrl+scroll is left to the Scene, which also
                            // zooms to the pointer). We replicate the Scene's own
                            // fit transform (scene→screen) to map the cursor into
                            // scene space, zoom about it, then consume the scroll
                            // so the Scene does not also pan.
                            let fit = |scene: egui::Rect| -> egui::emath::TSTransform {
                                let scale =
                                    (outer.size() / scene.size()).min_elem().clamp(0.05, 4.0);
                                egui::emath::TSTransform::from_translation(
                                    outer.center().to_vec2() - scale * scene.center().to_vec2(),
                                ) * egui::emath::TSTransform::from_scaling(scale)
                            };
                            // Only when this panel is the TOPMOST thing under
                            // the pointer, which a bare `outer.contains` never
                            // asked. Whatever is drawn over the canvas - the New
                            // Project chip list, a window, a popup - scrolls its
                            // own content with that wheel, and the part it does
                            // not use would zoom the chip behind it.
                            let hover = ui.input(|i| i.pointer.hover_pos());
                            let ptr = hover.filter(|_| ui.rect_contains_pointer(outer));
                            // Refused, the wheel must not reach the Scene either:
                            // its hit test reaches a few points in under the edge
                            // of whatever covers the canvas, and it would PAN by
                            // the wheel there. Held back, then handed back after
                            // it for anything drawn later in the frame.
                            let held = if ptr.is_none() && hover.is_some_and(|p| outer.contains(p)) {
                                ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta))
                            } else {
                                egui::Vec2::ZERO
                            };
                            let (scroll_y, ctrl) =
                                ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.command));
                            // Over the pin-function list inside the chip the wheel
                            // means SCROLL THE LIST, not zoom. That list is painted
                            // inside the Scene and can't take the wheel itself (we
                            // intercept it here, before the Scene), so this is where
                            // it is fed — and consumed, so neither the zoom below
                            // nor the Scene's own pan sees it. The rect is last
                            // frame's, in screen coords, which is exact unless the
                            // view moved in between.
                            let on_fn_list = ptr.is_some_and(|p| self.mcu_fn_list_rect.contains(p));
                            if on_fn_list && scroll_y != 0.0 && !ctrl {
                                mcu.fn_scroll_offset -= scroll_y;
                                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                            }
                            if let Some(ptr) = ptr
                                && scroll_y != 0.0
                                && !ctrl
                                && !on_fn_list
                                && outer.contains(ptr)
                                && scene_rect.is_finite()
                                && scene_rect.size() != egui::Vec2::ZERO
                            {
                                let to_global = fit(scene_rect);
                                // Clamp so the effective scale stays in range — no
                                // scene-rect drift past the zoom limits.
                                let cur = to_global.scaling;
                                let z = ((scroll_y * 0.002).exp() * cur).clamp(0.05, 4.0) / cur;
                                let p = to_global.inverse() * ptr;
                                let new = to_global
                                    * egui::emath::TSTransform::from_translation(p.to_vec2())
                                    * egui::emath::TSTransform::from_scaling(z)
                                    * egui::emath::TSTransform::from_translation(-p.to_vec2());
                                let new_rect = new.inverse() * outer;
                                if new_rect.is_finite() && new_rect.size() != egui::Vec2::ZERO {
                                    scene_rect = new_rect;
                                    self.mcu_scene_bounds = new_rect;
                                    self.mcu_view_adjusted = true;
                                }
                                // Consume so the Scene doesn't pan with it too.
                                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                            }
                            let scene = crate::app::helpers::scene::show(
                                egui::Scene::new().zoom_range(0.05..=4.0).drag_pan_buttons(
                                    egui::DragPanButtons::PRIMARY | egui::DragPanButtons::MIDDLE,
                                ),
                                ui,
                                &mut scene_rect,
                                |ui| {
                                    let r = mcu.draw(ui);
                                    content_bounds = ui.min_rect();
                                    r
                                },
                            );
                            if held != egui::Vec2::ZERO {
                                ui.input_mut(|i| i.smooth_scroll_delta += held);
                            }
                            let (inner, fn_rect) = scene.inner;
                            // Screen rect of the function list — next frame's wheel
                            // uses it to tell "scroll the list" from "zoom".
                            self.mcu_fn_list_rect = fn_rect;

                            // Scene marks its response changed on the pan/zoom it
                            // still owns (drag-pan + Ctrl+scroll zoom) and writes
                            // the new view back into `scene_rect` — latch it.
                            if scene.response.changed() {
                                self.mcu_view_adjusted = true;
                                self.mcu_scene_bounds = scene_rect;
                            }

                            // Ctrl+± zoom (around the view centre) + Ctrl+0
                            // reset — consumed ONLY while the pointer is over the
                            // canvas, so the editor keeps its own Ctrl+±. egui's
                            // global keyboard-zoom is disabled, so the Scene
                            // never sees these keys itself.
                            if scene.response.contains_pointer() {
                                let (reset, zoom_f) = ui.input_mut(|i| {
                                    let cmd = egui::Modifiers::COMMAND;
                                    if i.consume_key(cmd, egui::Key::Num0) {
                                        (true, None)
                                    } else if i.consume_key(cmd, egui::Key::Plus)
                                        || i.consume_key(cmd, egui::Key::Equals)
                                    {
                                        (false, Some(1.0 / 1.2)) // smaller rect = zoom IN
                                    } else if i.consume_key(cmd, egui::Key::Minus) {
                                        (false, Some(1.2)) // larger rect = zoom OUT
                                    } else {
                                        (false, None)
                                    }
                                });
                                if reset {
                                    self.mcu_view_adjusted = false;
                                } else if let Some(f) = zoom_f {
                                    // `scene_rect` post-show is the actual current
                                    // view (Scene wrote it, or it equals what we
                                    // fed) — zoom relative to it, no jump.
                                    let base = scene_rect;
                                    if base.is_finite()
                                        && base.size() != egui::Vec2::ZERO
                                        && avail.x > 0.0
                                        && avail.y > 0.0
                                    {
                                        // `scene_rect` maps onto `avail`, so bound
                                        // the implied scale to ~0.2×..5× (the Scene
                                        // also hard-clamps the displayed zoom to
                                        // its own range) so repeated presses can't
                                        // drift the view out of reach.
                                        let min = avail * 0.2;
                                        let max = avail * 5.0;
                                        let s = base.size() * f;
                                        let s = egui::vec2(
                                            s.x.clamp(min.x, max.x),
                                            s.y.clamp(min.y, max.y),
                                        );
                                        self.mcu_scene_bounds =
                                            egui::Rect::from_center_size(base.center(), s);
                                        self.mcu_view_adjusted = true;
                                    }
                                }
                            }

            // Keep auto-fitting until the user takes over.
                            if !self.mcu_view_adjusted {
                                self.mcu_scene_bounds = auto_fit_bounds(content_bounds, avail);
                            }

                            inner
                        }
                        None => {
                            no_mcu = Some("Pin configuration");
                            None
                        }
                    };

                    // Any pin change (configure OR deselect) triggers a full
                    // sync: files for unconfigured pins are removed, files for
                    // configured pins are created/updated, and mod.rs is rebuilt.
                    if pin_changed.is_some() {
                        if let Some(mcu) = &self.mcu {
                            let all_pins = mcu.all_pin_functions();
                            self.project_tree.sync_pin_files(&all_pins);
                        }
                    }
                }
                McuTab::Peripherals if self.mcu.is_none() => {
                    // Checked here rather than letting `show_peripherals_tab`
                    // draw its own note, so every tab says the same thing and
                    // offers the same way out. Its guard stays as defence for
                    // any other caller.
                    no_mcu = Some("Peripheral configuration");
                }
                McuTab::Peripherals => {
                    // Assigning a function here mutates the MCU just like the
                    // Pins tab, so re-sync the generated pins/ files on change.
                    let changed = show_peripherals_tab(ui, &mut self.mcu, &mut self.peripheral_query);
                    if changed.is_some() {
                        if let Some(mcu) = &self.mcu {
                            let all_pins = mcu.all_pin_functions();
                            self.project_tree.sync_pin_files(&all_pins);
                        }
                    }
                }
                McuTab::Clock => match &mut self.mcu {
                    Some(mcu) => {
                        // The Clock tab owns its layout (fixed 3-zone footer +
                        // scrollable diagram), so no outer ScrollArea here.
                        // Mutating mcu.clock is enough — `init_frame`
                        // regenerates main.rs from MCU state each frame.
                        // `clock_ui` is a disjoint field, so it can be borrowed
                        // alongside `self.mcu`; the tab writes the dragged
                        // positions and its note straight into it.
                        let out = mcu.draw_clock_tab(ui, &mut self.clock_ui);
                        if out.save_to_definition {
                            self.save_clock_to_definition();
                        }
                    }
                    None => no_mcu = Some("Clock configuration"),
                },
                McuTab::Configuration if self.mcu.is_none() => {
                    no_mcu = Some("Peripheral configuration");
                }
                McuTab::Configuration => self.show_configuration_tab(ui),
                McuTab::System => {
                    // A runtime Apply or an `@runtime` restore may have moved the
                    // HAL line since the chip was picked (the F1 swaps crates).
                    self.refresh_hal_check();
                    // Read the shared slot BEFORE borrowing `self.mcu` mutably.
                    let hal = self.hal_verdict_now();
                    // A background thread finishing does not wake egui. Without
                    // this nudge the verdict would sit in the slot unseen until
                    // something else happened to cause a repaint — which, if the
                    // user picked a chip and then just looked at the screen,
                    // could be never.
                    if hal.is_none() && self.hal_check.is_some() {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(250));
                    }
                    match &mut self.mcu {
                        Some(mcu) => Self::show_system_tab(ui, mcu, hal),
                        None => no_mcu = Some("System configuration"),
                    }
                }
                // Module-relationship diagram — chip-agnostic (works with no
                // MCU selected), so it doesn't gate on `self.mcu`.
                McuTab::Structure => self.show_structure_tab(ui),
                McuTab::Flow => self.show_flow_tab(ui),
                McuTab::Definition => self.show_definition_tab(ui),
                // The second editor draws with ITS OWN view state installed, so
                // every `self.ed.*` inside means the Reference editor's popup,
                // caret and find bar rather than the main editor's.
                McuTab::Reference => {
                    self.with_editor(crate::app::EditorSlot::Reference, |s| {
                        s.show_reference_tab(ui);
                    });
                }
                // Works with no chip open: a system can be empty, or open
                // from a project that belongs to it not at all.
                McuTab::Board => self.show_board_tab(ui),
            }

            if let Some(what) = no_mcu {
                self.show_no_mcu_notice(ui, what);
            }
        });
    }

    /// The "System" tab — project-level settings that aren't tied to a single
    /// pin. Currently the **Runtime** selector (Blocking bare-metal vs. embassy
    /// Async), which re-targets code generation and the embassy deps. Takes the
    /// MCU by `&mut` (not `&mut self`) so the caller can hand it the already
    /// borrowed `self.mcu` without a second borrow of `self`.
    fn show_system_tab(
        ui: &mut egui::Ui,
        mcu: &mut crate::panels::mcu_module::mcu::Mcu,
        hal: Option<crate::app::dialogs::FeatureVerdict>,
    ) {
        use crate::panels::mcu_module::mcu::Runtime;

        egui::ScrollArea::vertical().show(ui, |ui| {
            // What this chip cannot do, said where the project actually starts.
            //
            // The import report already says it — but the import report is not
            // where most chips are met. One arrives from a shared `.ron`, from
            // the recent list, or inside a project someone else made, and then
            // nothing ever explained why the clock in `main.rs` is a commented
            // skeleton and the DMA a `TODO`. The verdict now travels with the
            // chip instead of with the moment it was imported.
            let clock_ok = crate::panels::mcu_module::codegen::rcc::generates_clock_code_for(
                &mcu.family,
                &mcu.clock,
            );
            let channels = crate::app::dialogs::uses_dma_def(&mcu.family)
                .then(|| mcu.dma.as_ref().map_or(0, |d| d.channels.len()));
            // All three once the index has answered; the two free ones until
            // then, and forever on a chip whose HAL line has no feature to look
            // up. Never a guess in the gap: `local_chip_gaps` says nothing at
            // all about the HAL rather than assuming it is fine.
            let mut gaps = match hal {
                Some(v) => crate::app::dialogs::chip_gaps(&v, clock_ok, channels),
                None => crate::app::dialogs::local_chip_gaps(clock_ok, channels),
            };
            if let Some(g) = crate::app::dialogs::backend_gap(&mcu.family) {
                gaps.insert(0, g);
            }
            if !gaps.is_empty() {
                ui.add_space(10.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  {} is missing {} of what codegen needs",
                            ph::WARNING,
                            mcu.name,
                            if gaps.len() == 1 { "one piece" } else { "pieces" }
                        ))
                        .size(12.0)
                        .color(egui::Color32::from_rgb(230, 190, 90)),
                    );
                    for g in &gaps {
                        ui.label(
                            egui::RichText::new(format!("    {}  {g}", ph::ARROW_ELBOW_DOWN_RIGHT))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(200, 170, 100)),
                        );
                    }
                    // Not a refusal. The rest of the chip works, and half a
                    // project is worth more than none — the point is that you
                    // find out now rather than from generated code.
                    ui.label(
                        egui::RichText::new(
                            if gaps
                                .iter()
                                .any(|g| g.starts_with(crate::app::dialogs::BACKEND_GAP_PREFIX))
                            {
                                "    main.rs is empty until a backend exists for this family."
                            } else {
                                "    Everything else still generates; these parts come out as comments or TODOs."
                            },
                        )
                        .size(10.5)
                        .color(egui::Color32::GRAY),
                    );
                });
            }

            ui.add_space(10.0);
            ui.heading(format!("{}  Runtime", ph::GEAR));
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "How the firmware executes. Changing this regenerates the \
                     entry point in main.rs and the runtime dependencies.",
                )
                .color(egui::Color32::GRAY),
            );
            ui.add_space(10.0);

            let async_ok = family::async_supported(&mcu.family);
            let native_ok = family::native_supported(&mcu.family);
            let rtic_ok = family::rtic_supported(&mcu.family);

            // The cards edit the STAGED choice (`pending_*`); nothing regenerates
            // until "Apply" commits it (see the Apply bar below).
            Self::runtime_apply_bar(ui, mcu);

            // ── Blocking ─────────────────────────────────────────────────────
            let blocking_sel = mcu.pending_runtime == Runtime::Blocking;
            let blocking_note = blocking_subtitle(&mcu.family);
            if runtime_card(
                ui,
                blocking_sel,
                true,
                "Blocking (bare-metal)",
                &blocking_note,
            )
            .clicked()
            {
                mcu.pending_runtime = Runtime::Blocking;
            }
            runtime_details(ui, "rt_details_blocking", BLOCKING_DETAILS, BLOCKING_EXAMPLE);
            ui.add_space(6.0);

            // ── Native (concrete HAL) ────────────────────────────────────────
            let native_sel = mcu.pending_runtime == Runtime::Native;
            let native_why = family::native_unavailable_reason(&mcu.family);
            let mut native_resp = runtime_card(
                ui,
                native_sel,
                native_ok,
                "Native (bare-metal, HAL types)",
                NATIVE_SUBTITLE,
            );
            if let Some(why) = &native_why {
                native_resp = native_resp.on_hover_text(why);
            }
            if native_resp.clicked() && native_ok {
                mcu.pending_runtime = Runtime::Native;
            }
            // The reason a card is greyed belongs BESIDE it, not three clicks
            // deep in "Details" — a disabled control with no explanation is the
            // question this answers.
            disabled_reason(ui, native_why.as_deref());
            runtime_details_when_available(
                ui,
                native_why.as_deref(),
                "rt_details_native",
                NATIVE_DETAILS,
                NATIVE_EXAMPLE,
            );
            ui.add_space(6.0);

            // ── Async (embassy) ──────────────────────────────────────────────
            // Four async stacks behind one card: embassy-stm32, embassy-rp and
            // embassy-nrf on ARM, esp-rtos (same embassy executor, ESP
            // scheduler) on the Espressif parts. The title and subtitle tell
            // the ESP apart here; the pane under them is per family in
            // `family::async_details`.
            let esp_async = family::async_is_esp(&mcu.family);
            let async_sel = mcu.pending_runtime == Runtime::Async;
            let async_why = family::async_unavailable_reason(&mcu.family);
            let async_note = async_subtitle(&mcu.family);
            let mut async_resp = runtime_card(
                ui,
                async_sel,
                async_ok,
                if esp_async {
                    "Async (embassy on esp-rtos)"
                } else {
                    "Async (embassy)"
                },
                &async_note,
            );
            if let Some(why) = &async_why {
                async_resp = async_resp.on_hover_text(why);
            }
            if async_resp.clicked() && async_ok {
                mcu.pending_runtime = Runtime::Async;
            }
            // Same rule as the Native and RTIC cards: the reason a card is
            // greyed belongs beside it, not in "Details" the user cannot open.
            disabled_reason(ui, async_why.as_deref());
            // Per family, like the subtitle: the pane used to be an ESP one and
            // an STM32 one shown to every other chip, Pico and micro:bit
            // included.
            let details = family::async_details(&mcu.family);
            runtime_details_when_available(
                ui,
                async_why.as_deref(),
                "rt_details_async",
                details.rows,
                details.example,
            );

            let rtic_sel = mcu.pending_runtime == Runtime::Rtic;
            let rtic_why = family::rtic_unavailable_reason(&mcu.family);
            let mut rtic_resp = runtime_card(
                ui,
                rtic_sel,
                rtic_ok,
                "RTIC 2",
                RTIC_SUBTITLE,
            );
            if let Some(why) = &rtic_why {
                rtic_resp = rtic_resp.on_hover_text(why);
            }
            if rtic_resp.clicked() && rtic_ok {
                mcu.pending_runtime = Runtime::Rtic;
            }
            disabled_reason(ui, rtic_why.as_deref());
            runtime_details_when_available(
                ui,
                rtic_why.as_deref(),
                "rt_details_rtic",
                RTIC_DETAILS,
                RTIC_EXAMPLE,
            );

            ui.add_space(10.0);
            // The old catch-all summary here ("only Blocking is supported…") is
            // gone: every unavailable card now carries its OWN reason, and that
            // sentence hardcoded a family list ("STM32F4/G0/G4/L4/H7/…") that
            // goes stale the moment a family is added.
            if native_sel {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  All USART/SPI/I2C peripherals now expose the concrete \
                         stm32f1xx-hal types (the per-module Portable/Native selector is \
                         subsumed). USART `init` returns the split (Tx, Rx).",
                        ph::INFO,
                    ))
                    .color(egui::Color32::from_rgb(120, 170, 220)),
                );
            } else if async_sel && async_ok {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  main.rs now runs on the embassy executor — add \
                         `embassy_time::Timer::after_millis(..).await` in the loop.",
                        ph::INFO,
                    ))
                    .color(egui::Color32::from_rgb(120, 170, 220)),
                );
            } else if async_sel {
                // Async SELECTED on a family that has no async backend. The card
                // itself cannot be clicked there, but `mcu.config` is a file the
                // user can edit, and a hand-written `@runtime Async` lands here.
                // (Every family this IDE ships has a backend now - the F1 was
                // the last to get one.) Codegen falls back to the blocking
                // backend (see `Mcu::is_async`), so promising the executor would
                // be a lie about code that does not exist.
                ui.label(
                    egui::RichText::new(format!(
                        "{}  Async is selected but INERT on this chip — main.rs is still \
                         the blocking project, and the dependencies with it. Pick a \
                         runtime above, or see the reason on the card.",
                        ph::WARNING,
                    ))
                    .color(egui::Color32::from_rgb(220, 160, 70)),
                );
            }

            // ── GPIO In/Out bridge (io.rs) ───────────────────────────────────
            // Shown ONLY where the choice exists. The `io.rs` bridge is an F1
            // feature; elsewhere GPIO binds straight to the HAL's own types, and
            // rendering the section greyed out on those chips was dead UI that
            // read like a misconfiguration — its own subtitle said "STM32F1".
            use crate::panels::mcu_module::modules::ApiStyle;
            if family::gpio_bridge_supported(&mcu.family) {
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(10.0);
            ui.heading(format!("{}  GPIO In/Out", ph::GEAR));
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "How GPIO In/Out pins bind in main.rs (STM32F1 blocking path).",
                )
                .color(egui::Color32::GRAY),
            );
            ui.add_space(8.0);

            // Only the STM32F1 backend has the io.rs bridge; on the (staged)
            // Native runtime GPIO is forced raw regardless, and on Async the F1
            // is on embassy-stm32, whose own `Output`/`Input` every pin binds
            // to. Edits the STAGED `pending_gpio_api`.
            let gpio_ok = native_ok
                && !matches!(mcu.pending_runtime, Runtime::Native | Runtime::Async);
            // What the two cards SHOW. While the Native runtime is staged they
            // are dead and GPIO binds raw whatever this field holds, so Native
            // is what they show - derived here rather than written into
            // `pending_gpio_api`.
            //
            // Writing it is what `normalize_gpio_api` used to do, and it staged
            // a second change nobody could have made: the bar read "2 staged
            // changes - Runtime: Blocking -> Native AND GPIO In/Out: Portable ->
            // Native", with the GPIO cards greyed out. Applying then made it
            // permanent, so going back to Blocking afterwards left GPIO on
            // Native - the bridge the user never chose to leave, gone for good.
            //
            // Nothing downstream needed the write: every emitter reads
            // `gpio_native()`, which is already `is_native() || gpio_api ==
            // Native`, and the `embedded-hal` dependency follows the io.rs that
            // comes out of it.
            let shown = if gpio_ok {
                mcu.pending_gpio_api
            } else {
                ApiStyle::Native
            };
            let portable_sel = shown == ApiStyle::Portable;
            if runtime_card(
                ui,
                portable_sel && native_ok,
                gpio_ok,
                "Portable (embedded-hal bridge)",
                "let pa0_out = &mut pins::configs::io::DigitalOut(...)  ·  \
                 generates io.rs (embedded-hal 1.0 OutputPin/InputPin/DelayNs)",
            )
            .clicked()
                && gpio_ok
            {
                mcu.pending_gpio_api = ApiStyle::Portable;
            }
            ui.add_space(6.0);
            let native_gpio_sel = shown == ApiStyle::Native;
            if runtime_card(
                ui,
                native_gpio_sel && native_ok,
                gpio_ok,
                "Native (raw HAL pin)",
                "let pa0_out = &mut gpioa.pa0.into_push_pull_output(...)  ·  \
                 no io.rs, no embedded-hal dep for GPIO",
            )
            .clicked()
                && gpio_ok
            {
                mcu.pending_gpio_api = ApiStyle::Native;
            }

            ui.add_space(8.0);
            // The "this is an F1 feature" warning is gone with the section it
            // explained — a chip that never shows the choice needs no excuse.
            if mcu.pending_runtime == Runtime::Native {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  The Native runtime already binds all GPIO raw — this choice \
                         applies on the Blocking runtime.",
                        ph::INFO,
                    ))
                    .color(egui::Color32::from_rgb(120, 170, 220)),
                );
            } else if mcu.pending_runtime == Runtime::Async {
                ui.label(
                    egui::RichText::new(format!(
                        concat!(
                            "{}  The Async runtime binds every GPIO to embassy-stm32's own ",
                            "Output / Input — this choice applies on Blocking and RTIC."
                        ),
                        ph::INFO,
                    ))
                    .color(egui::Color32::from_rgb(120, 170, 220)),
                );
            }
            } // end: only families with the io.rs bridge

            // ── Build on Save ────────────────────────────────────────────────
            // A workflow preference (NOT a codegen choice) — applied immediately,
            // no staged Apply.
            use crate::panels::mcu_module::mcu::AutoBuild;
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(10.0);
            ui.heading(format!("{}  Build on Save", ph::GEAR));
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "When a library changes in Cargo.toml (yours or codegen-added), Save can \
                     run a build automatically so the new deps resolve + compile.",
                )
                .color(egui::Color32::GRAY),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("On dependency change:");
                egui::ComboBox::from_id_salt("auto_build_on_save")
                    .selected_text(match mcu.auto_build {
                        AutoBuild::Off => "Off",
                        AutoBuild::Check => "cargo check",
                        AutoBuild::Release => "cargo build --release",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut mcu.auto_build, AutoBuild::Off, "Off")
                            .on_hover_text("Never auto-build — run Build / Check manually.");
                        ui.selectable_value(&mut mcu.auto_build, AutoBuild::Check, "cargo check")
                            .on_hover_text("Fast: resolves new deps + catches errors (default).");
                        ui.selectable_value(
                            &mut mcu.auto_build,
                            AutoBuild::Release,
                            "cargo build --release",
                        )
                        .on_hover_text("Full optimized build — slower, but exactly what gets flashed.");
                    });
            });

            // ── Strict lints (Clippy) ────────────────────────────────────────
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(10.0);
            ui.heading(format!("{}  Strict lints (Clippy)", ph::SHIELD_CHECK));
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "Add a strict `[lints.clippy]` deny profile (pedantic + nursery + no \
                     unwrap / panic / indexing / arithmetic / as-cast) to Cargo.toml. Only \
                     affects the Clippy tab — check / build / flash are unaffected.",
                )
                .color(egui::Color32::GRAY),
            );
            ui.add_space(8.0);
            if ui
                .checkbox(&mut mcu.strict_lints, "Deny panics & risky ops on my code")
                .on_hover_text(
                    "The generated code (main's init + src/pins/configs/*.rs) is auto-exempted \
                     with #[allow], so only YOUR modules are held to the strict profile. \
                     Toggling regenerates Cargo.toml + the exemptions.",
                )
                .changed()
            {
                // Codegen reads `strict_lints` (hashed) → regeneration next frame.
            }
        });
    }

    /// The staged-changes banner shown at the top of the System tab: nothing
    /// regenerates until the user **Applies** the pending Runtime / GPIO /
    /// per-module choices. Nothing is drawn when there's no staged change.
    fn runtime_apply_bar(ui: &mut egui::Ui, mcu: &mut crate::panels::mcu_module::mcu::Mcu) {
        if !mcu.apply_bar_visible() {
            return;
        }
        let diff = mcu.style_diff_summary();
        egui::Frame::new()
            .inner_margin(egui::Margin::same(10))
            .corner_radius(egui::CornerRadius::same(6))
            .fill(egui::Color32::from_rgb(48, 44, 30))
            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(150, 130, 70)))
            .show(ui, |ui| {
                if !mcu.pending_apply_confirm {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}  {} staged change{} — not applied yet.",
                                ph::WARNING,
                                diff.len(),
                                if diff.len() == 1 { "" } else { "s" },
                            ))
                            .color(egui::Color32::from_rgb(230, 200, 120)),
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(egui::RichText::new(format!("{}  Apply — regenerate", ph::CHECK)).strong())
                            .on_hover_text("Commit these choices and regenerate main.rs, the config files and Cargo.toml deps")
                            .clicked()
                        {
                            mcu.pending_apply_confirm = true;
                        }
                        if ui.button("Discard").on_hover_text("Revert the staged choices to what's applied").clicked() {
                            mcu.sync_pending_style();
                        }
                    });
                } else {
                    ui.label(
                        egui::RichText::new("Apply these changes? Everything below will be regenerated:")
                            .strong()
                            .color(egui::Color32::from_gray(225)),
                    );
                    ui.add_space(4.0);
                    // The FULL list: the staged choices + their concrete effects
                    // (entry point, config files added/removed/regenerated, deps).
                    let changes = mcu.apply_change_list();
                    egui::ScrollArea::vertical()
                        .max_height(190.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for line in &changes {
                                let (color, mono) = if line.starts_with('•') {
                                    (egui::Color32::from_gray(220), false)
                                } else if line.starts_with('+') {
                                    (egui::Color32::from_rgb(140, 200, 140), true)
                                } else if line.starts_with('−') {
                                    (egui::Color32::from_rgb(210, 150, 150), true)
                                } else {
                                    (egui::Color32::from_gray(185), true)
                                };
                                let mut txt = egui::RichText::new(format!("   {line}")).size(11.5).color(color);
                                if mono {
                                    txt = txt.monospace();
                                }
                                ui.label(txt);
                            }
                        });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  Edits inside a config file may be lost if its template changes (e.g. blocking -> async).",
                            ph::WARNING,
                        ))
                        .size(11.0)
                        .color(egui::Color32::from_rgb(210, 170, 90)),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(egui::RichText::new(format!("{}  Confirm & apply", ph::CHECK)).strong())
                            .clicked()
                        {
                            mcu.apply_pending_style();
                        }
                        if ui.button("Cancel").clicked() {
                            mcu.pending_apply_confirm = false;
                        }
                    });
                }
            });
        ui.add_space(12.0);
    }

    /// The F12 "Go to definition" snippet (external / crate / std files) —
    /// moved here from the bottom diagnostics panel on 2026-07-10. The whole
    /// file is shown (scrollable above and below the target); rows are
    /// virtualized and the target line is scrolled near the top once on open,
    /// drawn coloured so it stands out from the surrounding code.
    /// The second editor: another project file, EDITABLE, beside the main one.
    ///
    /// It runs the SAME [`show_code_view`] the main editor does, so the two
    /// share every shortcut, overlay and popup rather than drifting apart. What
    /// makes that affordable is [`AppIde::with_editor`]: the caller installs
    /// this view's state before calling in, so the ~420 `self.ed.*` reads inside
    /// mean this editor's popups, caret and find bar without knowing a second
    /// view exists.
    ///
    /// What it does NOT get is the main editor's CHROME — the toolbar and the
    /// bottom diagnostics panel belong to the application, not to a view — plus
    /// the handful of things keyed to the main editor itself: the breakpoint and
    /// git gutters, the debug hover, Ctrl+Tab file cycling, and the
    /// rust-analyzer features (rename, F12, code actions, squiggles, type
    /// hints), which arbitrate over a single LSP inbox and are wired next.
    ///
    /// [`show_code_view`]: AppIde::show_code_view
    fn show_reference_tab(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self.reference_file.clone() else {
            ui.label(egui::RichText::new("No file open.").color(egui::Color32::GRAY));
            return;
        };
        let Some((id, code)) = self.reference_content(&path) else {
            // Deleted or renamed while it was open here.
            ui.label(
                egui::RichText::new(format!("{path} is no longer in the project."))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(220, 170, 90)),
            );
            return;
        };

        // The same file in BOTH editors: the widget ids differ
        // (`reference_editor:…` vs `code_editor:…`), so egui keeps two
        // independent carets and undo stacks over ONE buffer. Typing in either
        // then makes the other's view and undo history stale, and both write
        // back to the same slot in the same frame. Editing is refused rather
        // than left to that race — "Open in editor" is the way to edit it.
        let clash = self.selected_file == id;

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(&path)
                    .size(11.0)
                    .monospace()
                    .color(egui::Color32::from_rgb(150, 200, 150)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Open in editor").size(10.5))
                            .frame(false),
                    )
                    .on_hover_text("Switch the MAIN editor to this file")
                    .clicked()
                {
                    self.selected_file = id;
                }
                if clash {
                    ui.label(
                        egui::RichText::new("read-only - already open in the main editor")
                            .size(10.0)
                            .color(egui::Color32::from_rgb(220, 180, 90)),
                    );
                }
            });
        });
        ui.separator();

        if clash {
            // Read-only: an IMMUTABLE buffer, never `interactive(false)` — a
            // non-interactive `TextEdit` is not focusable, surrenders focus
            // every frame and can never be clicked back into it (see the code
            // folding notes). An immutable buffer keeps focus, clicks, selection
            // and scrolling, and simply refuses edits.
            let frozen = code;
            let mut view: &str = &frozen;
            crate::editor::gui::code_editor::show_rust_editor_plain(
                ui,
                &mut view,
                self.editor_font_size,
                ((ui.available_height() / (self.editor_font_size * 1.3)).floor() as usize).max(6),
                &format!("reference_editor:{path}"),
            );
            // A read-only view must not go on claiming the main editor's keys.
            self.reference_was_focused = false;
            self.ed.editor_was_focused = false;
            return;
        }

        // Syntax by FILE, not hardcoded Rust: a library crate's `Cargo.toml` is
        // an ordinary user file here, and Rust highlighting rendered its `#`
        // comments as plain text.
        let syntax = id.syntax(&path);
        let is_manifest = id.is_cargo_manifest(&path);
        let respace = match id {
            ProjectFileId::MainRs | ProjectFileId::BuildRs => true,
            ProjectFileId::UserFile(_) => path.ends_with(".rs"),
            _ => false,
        };
        self.show_code_view(
            ui,
            crate::app::EditorSlot::Reference,
            id,
            code,
            &syntax,
            is_manifest,
            respace,
            // No diagnostics panel below this view, so nothing clips it early.
            None,
        );
    }

    /// Resolve the reference file's path to `(id, content)`, or `None` when it
    /// is no longer in the project (deleted or renamed since it was opened).
    fn reference_content(&self, path: &str) -> Option<(ProjectFileId, String)> {
        match path {
            "src/main.rs" => Some((ProjectFileId::MainRs, self.generated_code.clone())),
            "Cargo.toml" => Some((ProjectFileId::CargoToml, self.cargo_toml.clone())),
            ".cargo/config.toml" => Some((ProjectFileId::CargoConfig, self.cargo_config.clone())),
            "memory.x" => Some((ProjectFileId::MemoryX, self.memory_x.clone())),
            "build.rs" => Some((ProjectFileId::BuildRs, self.build_rs.clone())),
            ".gitignore" => Some((ProjectFileId::GitIgnore, self.gitignore.clone())),
            _ => self
                .project_tree
                .user_src_files
                .iter()
                .position(|(p, _)| p == path)
                .map(|i| {
                    (
                        ProjectFileId::UserFile(i),
                        self.project_tree.user_src_files[i].1.clone(),
                    )
                }),
        }
    }

    fn show_definition_tab(&mut self, ui: &mut egui::Ui) {
        use crate::app::definition_nav::{self, DefScroll, Step};
        self.def_drawn_frame = Some(ui.ctx().cumulative_frame_nr());

        // ── Keyboard ownership ─────────────────────────────────────────────
        // GIVEN by a press on the tab's text or its Back / Forward buttons
        // (below) — not by one on its scroll bars: dragging those beside the
        // code must leave the editor typing. TAKEN by a press on another panel.
        // A press on anything floating changes nothing: the chooser this tab
        // opened hangs past its edge when the zone is narrow, and a click on a
        // row out there must not hand F12 back to the main editor.
        let tab_rect = ui.available_rect_before_wrap();
        let press_pos = ui.input(|i| {
            i.pointer
                .any_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        if let Some(p) = press_pos
            && !tab_rect.contains(p)
            && ui
                .ctx()
                .layer_id_at(p)
                .is_none_or(|l| l.order == egui::Order::Background)
        {
            self.def_owns_kbd = false;
        }
        let mut owns_kbd = self.def_owns_kbd;
        let mut claim_kbd = false;

        // ── Back / Forward ─────────────────────────────────────────────────
        // Keys before the rows: a label holding a (hidden) selection takes
        // Alt+Arrow as a caret move, and on a wide row that scrolls sideways.
        let mut step: Option<Step> = None;
        if owns_kbd {
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft) {
                    step = Some(Step::Back);
                } else if i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight) {
                    step = Some(Step::Forward);
                }
            });
        }
        // The mouse's own Back / Forward buttons, wherever the keyboard is: the
        // pointer being over the tab says which page they mean.
        if ui.rect_contains_pointer(tab_rect) {
            ui.input(|i| {
                if i.pointer.button_pressed(egui::PointerButton::Extra1) {
                    step = Some(Step::Back);
                } else if i.pointer.button_pressed(egui::PointerButton::Extra2) {
                    step = Some(Step::Forward);
                }
            });
        }
        if let Some(header) = self.definition_view.as_ref().map(|d| d.header.clone()) {
            let (behind, ahead) = self.def_history.counts();
            let title = |s: Step| {
                self.def_history
                    .peek(s)
                    .map(|e| e.title.clone())
                    .unwrap_or_default()
            };
            let (back_to, fwd_to) = (title(Step::Back), title(Step::Forward));
            ui.horizontal(|ui| {
                let nav =
                    |icon: &str| egui::Button::new(egui::RichText::new(icon).size(13.0)).small();
                if ui
                    .add_enabled(behind > 0, nav(ph::ARROW_LEFT))
                    .on_hover_text(format!("Back to {back_to}  (Alt+Left)"))
                    .on_disabled_hover_text("No earlier definition")
                    .clicked()
                {
                    step = Some(Step::Back);
                    claim_kbd = true;
                }
                if ui
                    .add_enabled(ahead > 0, nav(ph::ARROW_RIGHT))
                    .on_hover_text(format!("Forward to {fwd_to}  (Alt+Right)"))
                    .on_disabled_hover_text("No later definition")
                    .clicked()
                {
                    step = Some(Step::Forward);
                    claim_kbd = true;
                }
                // Where this page sits in the walk.
                ui.label(
                    egui::RichText::new(format!("{}/{}", behind + 1, behind + ahead + 1))
                        .size(10.5)
                        .color(egui::Color32::from_rgb(140, 145, 155)),
                );
                // Truncated to the tab, never wider: a label in a horizontal row
                // does not wrap, and a long path widened the whole layout under
                // it — the code's scroll bar ended up past the visible edge.
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&header)
                            .size(11.0)
                            .monospace()
                            .color(egui::Color32::from_rgb(150, 190, 240)),
                    )
                    .truncate(),
                )
                .on_hover_text(&header);
            });
        }
        if claim_kbd {
            self.def_owns_kbd = true;
            owns_kbd = true;
        }
        if let Some(s) = step {
            self.def_step(s);
        }

        let Some(def) = &self.definition_view else {
            ui.label(egui::RichText::new("No definition.").color(egui::Color32::GRAY));
            return;
        };
        // What you opened, not just where it lives. The band below marks the
        // same item, but you should not have to find the band to know.
        if !def.signature.is_empty() {
            ui.label(
                egui::RichText::new(&def.signature)
                    .size(11.0)
                    .monospace()
                    .strong()
                    .color(DEF_ITEM_FG),
            );
        }
        ui.separator();
        let line_count = def.line_count();
        let highlight = def.highlight;
        let (ext_lo, ext_hi) = def.extent;
        // Height of one monospace-12 line (matches the rows below).
        let row_h = ui
            .painter()
            .layout_no_wrap(
                "X".to_owned(),
                egui::FontId::monospace(crate::app::DEF_FONT_SIZE),
                egui::Color32::WHITE,
            )
            .size()
            .y;
        // Match the spacing show_rows will use, so its offset math lines up
        // with the rendered rows.
        ui.spacing_mut().item_spacing.y = 1.0;
        let pitch = row_h + ui.spacing().item_spacing.y;
        // One id for every page: each navigation therefore SETS an offset, or
        // the new page would open wherever the last one was scrolled to.
        let mut area = egui::ScrollArea::both()
            .id_salt("definition_tab")
            .auto_shrink([false, false]);
        match self.def_scroll_to.take() {
            // A new page: the target near the top, 2 lines of context above.
            Some(DefScroll::Target) => {
                area =
                    area.scroll_offset(egui::vec2(0.0, highlight.saturating_sub(2) as f32 * pitch));
            }
            // A page revisited: where the user left it.
            Some(DefScroll::Restore(v)) => area = area.scroll_offset(v),
            None => {}
        }
        // The word whose every occurrence is banded, and the double-click that
        // picks a new one. Collected here and applied AFTER the closure: `def`
        // borrows `self.definition_view`, so the closure cannot write to it —
        // the same goes for the caret and a Ctrl+Click.
        let word = def.word.clone();
        let caret = def.caret;
        let ctrl_held = ui.input(|i| i.modifiers.command);
        let mut new_word: Option<String> = None;
        let mut clear_word = false;
        let mut new_caret: Option<(usize, usize)> = None;
        let mut ctrl_click: Option<(usize, usize, egui::Pos2)> = None;
        let mut caret_anchor: Option<egui::Pos2> = None;
        let pressed_now = press_pos.is_some();
        let mut text_pressed = false;
        let caret_bright = owns_kbd;

        let out = area.show_rows(ui, row_h, line_count, |ui, range| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            for i in range {
                let line = def.line(i);
                let shown = if line.is_empty() { " " } else { line };
                let inside = i >= ext_lo && i <= ext_hi;
                // The whole ITEM carries a band, not just its first line: F12
                // asks "where does this thing begin and end", and one coloured
                // row cannot answer that. The declaration keeps a stronger tint
                // so the head of the item is still the thing your eye lands on.
                let bg = if i == highlight {
                    DEF_HEAD_BG
                } else if inside {
                    DEF_BODY_BG
                } else {
                    egui::Color32::TRANSPARENT
                };
                // Pre-coloured rows exist only for the item (see
                // `DEF_HIGHLIGHT_MAX_LINES`); everything around it stays plain
                // and DIM, which is what makes the item stand out — colouring
                // the whole file would dilute exactly what should be loud.
                let job = match inside.then(|| def.rows.get(i - ext_lo)).flatten() {
                    Some(j) => {
                        let mut j = j.clone();
                        for sec in &mut j.sections {
                            sec.format.background = bg;
                        }
                        j
                    }
                    None => egui::text::LayoutJob::single_section(
                        shown.to_owned(),
                        egui::TextFormat {
                            font_id: egui::FontId::monospace(crate::app::DEF_FONT_SIZE),
                            color: if inside { DEF_ITEM_FG } else { DEF_CONTEXT_FG },
                            background: bg,
                            ..Default::default()
                        },
                    ),
                };

                // Laid out by hand rather than handed to `Label` as a job: the
                // galley is needed three more times — to place the occurrence
                // bands and the caret at real glyph positions, and to turn a
                // click into the character it landed on. `Label` would keep it
                // to itself.
                let galley = ui.fonts_mut(|f| f.layout_job(job));
                let resp = ui.add(egui::Label::new(galley.clone()).selectable(true));
                // The widget the press went DOWN on — not `contains_pointer`,
                // which is also true for a row lying under the scroll bar.
                if pressed_now && resp.is_pointer_button_down_on() {
                    text_pressed = true;
                }
                let x_of = |c: usize| {
                    resp.rect.min.x + galley.pos_from_cursor(egui::text::CCursor::new(c)).min.x
                };

                if !word.is_empty() {
                    let painter = ui.painter();
                    for (a, b) in crate::app::word_ranges(shown, &word) {
                        let (x0, x1) = (x_of(a), x_of(b));
                        if x1 > x0 {
                            painter.rect_filled(
                                egui::Rect::from_min_max(
                                    egui::pos2(x0, resp.rect.min.y),
                                    egui::pos2(x1, resp.rect.max.y),
                                ),
                                2.0,
                                DEF_WORD_BG,
                            );
                        }
                    }
                }

                // The tab's own caret: where F12 asks from. Full strength while
                // the tab holds the keyboard, dimmed when a keypress would go
                // elsewhere — so it shows where F12 will act, not just where
                // the last click was. Steady, not blinking: a blink needs a
                // repaint loop, and the idle cost of those was taken out.
                if let Some((cl, cc)) = caret
                    && cl == i
                {
                    let x = x_of(cc);
                    let color = if caret_bright {
                        ui.visuals().text_cursor.stroke.color
                    } else {
                        egui::Color32::from_rgb(110, 115, 125)
                    };
                    ui.painter()
                        .vline(x, resp.rect.y_range(), egui::Stroke::new(1.5_f32, color));
                    caret_anchor = Some(egui::pos2(x, resp.rect.max.y + 4.0));
                }

                // A click moves the caret; with Ctrl held it also asks.
                if resp.clicked()
                    && let Some(p) = resp.interact_pointer_pos()
                {
                    let idx = galley
                        .cursor_from_pos(p - resp.rect.min)
                        .index
                        .0
                        .min(line.chars().count());
                    new_caret = Some((i, idx));
                    if ctrl_held {
                        ctrl_click = Some((i, idx, egui::pos2(p.x, resp.rect.max.y + 4.0)));
                    }
                }
                // With Ctrl held, a name under the pointer is a link: underlined,
                // with the hand cursor — the promise that a click goes there.
                if ctrl_held
                    && let Some(p) = resp.hover_pos()
                    && let Some((a, b)) = definition_nav::ident_near(
                        line,
                        galley.cursor_from_pos(p - resp.rect.min).index.0,
                    )
                {
                    ui.painter().hline(
                        x_of(a)..=x_of(b),
                        resp.rect.max.y - 1.0,
                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(150, 190, 240)),
                    );
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }

                // Double-click, because that is the gesture that selects a word
                // in the editor too — egui's own label selection cannot be read
                // back (`LabelSelectionState` exposes only `has_selection`), so
                // the word is worked out from where the pointer landed.
                if resp.double_clicked()
                    && let Some(p) = resp.interact_pointer_pos()
                {
                    let cur = galley.cursor_from_pos(p - resp.rect.min);
                    let w = crate::app::word_at(shown, cur.index.0);
                    if w.is_empty() {
                        clear_word = true;
                    } else {
                        new_word = Some(w);
                    }
                }
            }
        });

        if text_pressed {
            self.def_owns_kbd = true;
            owns_kbd = true;
            // A drag that STARTED in the editor keeps it focused, and the keys
            // would go on typing into the file behind this tab.
            ui.memory_mut(|m| m.stop_text_input());
        }
        // Only while the tab holds the keyboard: an Escape in the editor —
        // closing its completion list — used to clear this tab's word too.
        if owns_kbd && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            clear_word = true;
        }
        // F12 / Ctrl+F12. The main editor left them in the queue: it does not
        // claim the keyboard while this tab owns it. Ctrl first — `consume_key`
        // would not confuse the two, but the editor checks in the same order.
        let (f12, ctrl_f12) = if owns_kbd {
            ui.input_mut(|i| {
                let c = i.consume_key(egui::Modifiers::CTRL, egui::Key::F12);
                let p = !c && i.consume_key(egui::Modifiers::NONE, egui::Key::F12);
                (p, c)
            })
        } else {
            (false, false)
        };

        let mut ask: Option<(usize, usize, bool, egui::Pos2)> = None;
        if let Some(def) = self.definition_view.as_mut() {
            def.scroll = out.state.offset;
            if let Some(w) = new_word {
                def.word = w;
            } else if clear_word {
                def.word.clear();
            }
            if let Some(c) = new_caret {
                def.caret = Some(c);
            }
            if let Some((l, c, at)) = ctrl_click {
                ask = Some((l, c, false, at));
            } else if (f12 || ctrl_f12)
                && let Some((l, c)) = def.caret
            {
                // Under the caret when its row is on screen, else just inside
                // the tab: a chooser must not open off-screen.
                let at = caret_anchor.unwrap_or(tab_rect.left_top() + egui::vec2(24.0, 48.0));
                ask = Some((l, c, ctrl_f12, at));
            }
        }
        if let Some((l, c, implementation, at)) = ask {
            let line_text = self
                .definition_view
                .as_ref()
                .map(|d| d.line(l).to_owned())
                .unwrap_or_default();
            if definition_nav::ident_near(&line_text, c).is_none() {
                // Nothing to ask about — say so, or the key looks broken.
                self.set_status_msg(format!(
                    "{} Click a name in the Definition tab first, then press F12",
                    ph::INFO
                ));
            } else {
                let col = definition_nav::utf16_col(&line_text, c);
                self.def_request_from_tab(l as u32, col, implementation, at, line_text);
            }
        }
    }
}

/// Every occurrence of the double-clicked identifier. The SAME cyan at the same
/// 20% opacity the editor's word highlight uses — it is the same idea, and two
/// different colours for it would just be two things to learn.
const DEF_WORD_BG: egui::Color32 = egui::Color32::from_rgba_unmultiplied_const(52, 232, 235, 51);

/// Definition tab: the declaration line's background — the strongest tint, so
/// the head of the item is where the eye lands first.
const DEF_HEAD_BG: egui::Color32 = egui::Color32::from_rgb(64, 58, 30);
/// …the rest of the item's body, the same hue much quieter: enough to read the
/// item's extent at a glance, not enough to fight the syntax colours on top.
const DEF_BODY_BG: egui::Color32 = egui::Color32::from_rgb(44, 41, 26);
/// Fallback foreground inside the item when it was too large to pre-colour.
const DEF_ITEM_FG: egui::Color32 = egui::Color32::from_rgb(255, 214, 90);
/// The surrounding file. Dimmed on purpose — the contrast has to come from BOTH
/// directions, or a coloured item in a bright white file is no clearer than the
/// single yellow line this replaced.
const DEF_CONTEXT_FG: egui::Color32 = egui::Color32::from_rgb(132, 136, 146);

/// A selectable "card" for the Runtime picker: a framed, clickable block with a
/// bold title over a dimmed monospace subtitle. `selected` tints it with the
/// accent; `enabled == false` greys the text and swallows clicks (the disabled
/// Async option on families without an embassy backend). Returns the click
/// [`egui::Response`] so the caller decides what a click does.
/// The one-line "why is this greyed out" note under a disabled runtime card.
/// `None` — the card is enabled — draws nothing at all.
///
/// Both cards stay VISIBLE when unavailable rather than being hidden: the
/// question they answer ("why can't I pick RTIC on this chip?") can only be
/// asked about something you can see.
fn disabled_reason(ui: &mut egui::Ui, why: Option<&str>) {
    let Some(why) = why else {
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(format!("{}  {why}", ph::INFO))
                .size(10.5)
                .color(egui::Color32::from_rgb(170, 150, 110)),
        );
    });
}

fn runtime_card(
    ui: &mut egui::Ui,
    selected: bool,
    enabled: bool,
    title: &str,
    subtitle: &str,
) -> egui::Response {
    let accent = egui::Color32::from_rgb(90, 140, 210);
    let fill = if selected {
        egui::Color32::from_rgb(40, 55, 78)
    } else {
        egui::Color32::from_rgb(38, 38, 46)
    };
    let border = if selected {
        egui::Stroke::new(1.5_f32, accent)
    } else {
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(70, 70, 82))
    };

    let inner = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 10))
        .corner_radius(egui::CornerRadius::same(6))
        .fill(fill)
        .stroke(border)
        .show(ui, |ui| {
            ui.set_width(ui.available_width().min(560.0));
            let title_col = if enabled {
                egui::Color32::from_gray(232)
            } else {
                egui::Color32::from_gray(120)
            };
            ui.label(
                egui::RichText::new(title)
                    .strong()
                    .size(14.0)
                    .color(title_col),
            );
            ui.label(
                egui::RichText::new(subtitle)
                    .monospace()
                    .size(11.0)
                    .color(egui::Color32::from_gray(if enabled { 155 } else { 100 })),
            );
        });

    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let resp = ui.interact(
        inner.response.rect,
        egui::Id::new(("runtime_card", title)),
        sense,
    );
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

// ── Runtime card text ────────────────────────────────────────────────────────
//
// Every body below is ONE string literal (or a `\`-continuation, which strips
// the next line's indentation). A literal joined by hand from a continuation
// keeps the indentation as a run of spaces, and the card subtitles are
// monospace, so the run rendered as a gap the width of a word.
// `no_runtime_card_text_has_a_run_of_spaces` holds this; the Async pane's rows
// are held by `async_details_rows_have_no_double_space` in `family`.
//
// The two-space "  ·  " between a subtitle's parts is deliberate.

/// The Blocking card's subtitle names the HAL this CHIP will use — see
/// `family::blocking_hal_note`. A fixed sentence described the F1 path only,
/// which made embassy-stm32 appearing in a Blocking project on any other STM32
/// look like the choice had been ignored.
fn blocking_subtitle(family: &str) -> String {
    format!(
        "#[entry] fn main() -> !  ·  {}",
        family::blocking_hal_note(family)
    )
}

const BLOCKING_DETAILS: &[(&str, &str)] = &[
    (
        "On Apply:",
        "Regenerates main.rs with the #[entry] entry, rewrites every \
                   src/pins/configs/*.rs to the portable init, (re)writes src/pins/io.rs, \
                   and reconciles Cargo.toml — then triggers a build.",
    ),
    (
        "Entry:",
        "#[cortex_m_rt::entry] fn main() -> ! — classic bare-metal, runs forever.",
    ),
    (
        "Drivers:",
        "Each USART/SPI/I2C initializes in src/pins/configs/*.rs and is exposed \
                  through STANDARD portable traits — embedded-io (serial) + embedded-hal 1.0 \
                  (SpiBus / I2c / OutputPin). App code generic over those traits ports across \
                  chips/HALs unchanged.",
    ),
    (
        "Virtual modules:",
        "Each USART/SPI/I2C module keeps its Init API: Portable | Native (HAL \
                          type) selector on the Pins tab. Portable = the trait wrapper (its .0 \
                          still hands back the raw HAL object); Native = the concrete HAL type \
                          for that one module only. SPI/I2C are always blocking here.",
    ),
    (
        "GPIO / io.rs:",
        "GPIO API = Portable (System tab) -> src/pins/io.rs wraps the pins as \
                       DigitalOut / DigitalIn + a Delay over embedded-hal 1.0. GPIO API = Native \
                       -> raw HAL pins and io.rs is not emitted.",
    ),
    (
        "Cargo.toml:",
        "Adds embedded-io (serial) + embedded-hal 1.0 (SPI/I2C/GPIO) + the \
                     embedded-hal-0-2 bridge alias; nb is added only while a Native-mode module \
                     needs it. The embassy-* async crates are removed.",
    ),
    ("Applies to:", "Every chip — this is the default runtime."),
];

const BLOCKING_EXAMPLE: &str = "let mut _serial1 = pins::configs::usart1::init(...);\n\
    fn app<S: embedded_io::Read + embedded_io::Write>(s: &mut S) { /* portable */ }";

const NATIVE_SUBTITLE: &str = "#[entry] fn main() -> !  ·  concrete HAL types everywhere \
    (Serial / Spi / BlockingI2c) — no portable bridges";

const NATIVE_DETAILS: &[(&str, &str)] = &[
    (
        "On Apply:",
        "Regenerates main.rs, rewrites every src/pins/configs/*.rs to the concrete \
                   init, drops src/pins/io.rs (raw GPIO), and reconciles Cargo.toml — then builds.",
    ),
    (
        "Entry:",
        "#[entry] fn main() -> ! — the same bare-metal entry as Blocking.",
    ),
    (
        "Drivers:",
        "init returns the CONCRETE stm32f1xx-hal types: Serial split into (Tx, Rx), \
                  Spi<…>, BlockingI2c<…>. No portable bridges, no extra trait crates, full HAL \
                  features.",
    ),
    (
        "Virtual modules:",
        "Init API is FORCED to Native (HAL type) for every USART/SPI/I2C and \
                          shown locked on the Pins tab — project-wide, nothing to pick per module.",
    ),
    (
        "GPIO / io.rs:",
        "GPIO is raw concrete HAL pins — src/pins/io.rs (the DigitalOut / DigitalIn \
                       / Delay wrappers) is NOT generated. The System-tab GPIO selector is subsumed.",
    ),
    (
        "Cargo.toml:",
        "Adds nb (the concrete Tx / Rx are nb-based) and keeps the concrete \
                     stm32f1xx-hal; the embassy-* async crates are removed.",
    ),
    (
        "Applies to:",
        "STM32F1 only (the family with concrete-HAL templates). Grayed on other \
                     families, whose blocking HAL types are already concrete.",
    ),
];

const NATIVE_EXAMPLE: &str = "let (mut _tx1, mut _rx1) = pins::configs::usart1::init(...);\n\
    // use _tx1 with writeln!(), _rx1 with .read()";

/// The Async card's subtitle names the stack this family runs on, the ESP's
/// own scheduler included — see `family::async_stack_name`.
fn async_subtitle(family: &str) -> String {
    if family::async_is_esp(family) {
        "#[esp_rtos::main] async fn main(Spawner)  ·  embassy executor scheduled by esp-rtos"
            .to_owned()
    } else {
        format!(
            "#[embassy_executor::main] async fn main(Spawner)  ·  .await-able drivers on {}",
            family::async_stack_name(family)
        )
    }
}

const RTIC_SUBTITLE: &str =
    "#[rtic::app] with Shared / Local / init / idle  ·  one hardware task per interrupt pin";

const RTIC_DETAILS: &[(&str, &str)] = &[
    (
        "On Apply:",
        "Regenerates main.rs as an #[rtic::app] module. The init sequence is UNCHANGED - same clocks, same pin bindings, same pins/configs/*::init calls - it just moves into #[init]. The config files themselves are untouched.",
    ),
    (
        "Entry:",
        "RTIC owns main. #[init] runs once and returns (Shared, Local); #[idle] is the background loop (wfi).",
    ),
    (
        "Interrupts:",
        "A GPIO input with an Edge set on the Pins canvas becomes a #[task(binds = EXTIn)]. RTIC enables the vector itself, so the generated code never calls NVIC::unmask.",
    ),
    (
        "Shared EXTI vectors:",
        "STM32 gives lines 5-9 and 10-15 ONE vector each, so pins on them share a single task that branches on check_interrupt(). Two tasks on one vector would not compile.",
    ),
    (
        "Cargo.toml:",
        "Adds rtic (backend feature picked from the chip's Rust target: thumbv6 / thumbv7 / thumbv8) and rtic-monotonics with cortex-m-systick. cortex-m already carries critical-section-single-core. Leaving RTIC removes them.",
    ),
    (
        "Applies to:",
        "STM32F1 only for now - the interrupt tasks are written against stm32f1xx-hal's ExtiPin trait.",
    ),
];

// The body line's four spaces are the example's own indentation; the source
// indentation is stripped by the continuations.
const RTIC_EXAMPLE: &str = "#[task(binds = EXTI0, local = [pa0_in_button])]\n\
    fn exti0(cx: exti0::Context) {\n    \
    cx.local.pa0_in_button.clear_interrupt_pending_bit();\n\
    }";

#[cfg(test)]
mod no_runtime_card_text_has_a_run_of_spaces {
    use super::{
        BLOCKING_DETAILS, BLOCKING_EXAMPLE, NATIVE_DETAILS, NATIVE_EXAMPLE, NATIVE_SUBTITLE,
        RTIC_DETAILS, RTIC_EXAMPLE, RTIC_SUBTITLE, async_subtitle, blocking_subtitle,
    };
    use crate::panels::mcu_module::codegen::family::{
        async_unavailable_reason, native_unavailable_reason, rtic_unavailable_reason,
    };

    const FAMILIES: &[&str] = &[
        "stm32f1", "stm32f4", "rp2040", "nrf52833", "nrf5340", "nrf54l15", "esp32c3",
    ];

    /// Prose and subtitles: never three spaces in a row. Two is the "  ·  "
    /// separator; three is a continuation joined by hand.
    fn prose(what: &str, text: &str) {
        assert!(!text.contains("   "), "{what}: {text:?}");
    }

    /// Code: the same, after each line's own indentation, which must be the
    /// example's (at most four spaces), not the source file's.
    fn code(what: &str, text: &str) {
        for line in text.lines() {
            let indent = line.len() - line.trim_start().len();
            assert!(indent <= 4, "{what}: source indentation in {line:?}");
            prose(what, line.trim_start());
        }
    }

    /// The RTIC subtitle rendered a word-wide gap in monospace, from a
    /// `\`-continuation joined into one literal with its indentation kept.
    #[test]
    fn subtitles_rows_examples_and_reasons() {
        prose("native subtitle", NATIVE_SUBTITLE);
        prose("rtic subtitle", RTIC_SUBTITLE);
        for (name, rows) in [
            ("blocking", BLOCKING_DETAILS),
            ("native", NATIVE_DETAILS),
            ("rtic", RTIC_DETAILS),
        ] {
            for (label, body) in rows {
                prose(&format!("{name} {label}"), body);
            }
        }
        code("blocking example", BLOCKING_EXAMPLE);
        code("native example", NATIVE_EXAMPLE);
        code("rtic example", RTIC_EXAMPLE);
        for family in FAMILIES {
            prose(
                &format!("{family} blocking subtitle"),
                &blocking_subtitle(family),
            );
            prose(&format!("{family} async subtitle"), &async_subtitle(family));
            for (card, why) in [
                ("native", native_unavailable_reason(family)),
                ("async", async_unavailable_reason(family)),
                ("rtic", rtic_unavailable_reason(family)),
            ] {
                if let Some(why) = why {
                    prose(&format!("{family} {card} reason"), &why);
                }
            }
        }
    }
}

/// The crate names a Runtime-details body colors as crates. A crate a pane
/// names and this list lacks renders plain, which is how the Pico's and the
/// micro:bit's async panes looked unfinished next to the STM32 one: every
/// async stack `family::async_stack_name` can answer with is here.
const DETAIL_LIBS: &[&str] = &[
    "embedded-hal-0-2",
    "embedded-hal",
    "embedded-io",
    "embedded-io-async",
    "embedded-hal-async",
    "nb",
    "embassy-executor",
    "embassy-time",
    "embassy-stm32",
    "embassy-rp",
    "embassy-nrf",
    "embassy-usb",
    "esp-rtos",
    "cyw43",
    "cyw43-pio",
    "static_cell",
    "stm32f1xx-hal",
    "cortex-m-rt",
    "bxcan",
    "usb-device",
    "usbd-serial",
];

/// Style one word inside a Runtime-details body: crate/library names → yellow
/// bold, source-file names → orange bold, async/runtime keywords → orange-red
/// bold, peripheral names → bold (default colour); everything else stays plain.
/// Matching strips surrounding sentence punctuation but the returned text keeps
/// the original token verbatim.
fn detail_word(word: &str) -> egui::RichText {
    const SIZE: f32 = 11.5;
    let plain = egui::Color32::from_gray(165);
    // Trim only clear sentence punctuation for matching (keep '.', '/', '-', '_'
    // so file names / crate names / `.await` survive).
    let clean = word
        .trim_start_matches('(')
        .trim_end_matches([',', ';', ':', '.', ')']);

    // 1. Source-file names → orange bold (Cargo.toml, io.rs, main.rs, *.rs …).
    if clean.ends_with(".rs") || clean.ends_with(".toml") {
        return egui::RichText::new(word)
            .size(SIZE)
            .strong()
            .color(egui::Color32::from_rgb(240, 150, 40));
    }
    // 2. Crate / library names → yellow bold.
    if DETAIL_LIBS.contains(&clean) {
        return egui::RichText::new(word)
            .size(SIZE)
            .strong()
            .color(egui::Color32::from_rgb(235, 215, 70));
    }
    // 3. async / runtime keywords → orange-red bold.
    const KEYWORDS: &[&str] = &[
        "async",
        "await",
        ".await",
        "Async",
        "Async-DMA",
        "Spawner",
        "Blocking",
        "Native",
        "Portable",
        "#[entry]",
        "#[embassy_executor::main]",
        "#[cortex_m_rt::entry]",
    ];
    if KEYWORDS.contains(&clean) {
        return egui::RichText::new(word)
            .size(SIZE)
            .strong()
            .color(egui::Color32::from_rgb(255, 85, 50));
    }
    // 4. Peripheral names → bold (default colour). Slash-joined groups like
    // "USART/SPI/I2C" match via `contains`; case-sensitive so `BlockingI2c`
    // (lower-case c) or English "can" don't trip it.
    const PERIPH: &[&str] = &["USART", "SPI", "I2C", "CAN", "USB", "GPIO", "Tx", "Rx"];
    if PERIPH.iter().any(|p| clean.contains(p)) {
        return egui::RichText::new(word).size(SIZE).strong().color(plain);
    }

    egui::RichText::new(word).size(SIZE).color(plain)
}

/// A collapsible "ⓘ Details" section under a Runtime card, explaining what that
/// runtime generates and how it applies. `points` are `(label, body)` rows; a
/// non-empty `example` is rendered as a monospace code block. The open/closed
/// state persists per `salt` via egui's own widget memory.
/// Whether a runtime card draws its Details pane: only when the card can be
/// chosen, which is `why.is_none()`.
///
/// The Native and RTIC panes are written for the F1 (stm32f1xx-hal types, EXTI
/// vectors) and the Async pane for whichever family it is on, so under a
/// greyed card the pane describes another chip's runtime: a Pico read
/// "Applies to: STM32F1 only" under a Native card it could not click. The
/// reason the card is greyed is already on the card (`disabled_reason`), and
/// that is all there is to say about this chip.
fn shows_runtime_details(why: Option<&str>) -> bool {
    why.is_none()
}

/// [`runtime_details`] for a card that may be greyed: drawn only when
/// [`shows_runtime_details`] says so.
fn runtime_details_when_available(
    ui: &mut egui::Ui,
    why: Option<&str>,
    salt: &str,
    points: &[(&str, &str)],
    example: &str,
) {
    if shows_runtime_details(why) {
        runtime_details(ui, salt, points, example);
    }
}

fn runtime_details(ui: &mut egui::Ui, salt: &str, points: &[(&str, &str)], example: &str) {
    egui::CollapsingHeader::new(
        egui::RichText::new(format!("{}  Details — how it works & applies", ph::INFO))
            .size(11.5)
            .color(egui::Color32::from_gray(160)),
    )
    .id_salt(salt)
    .show(ui, |ui| {
        for (label, body) in points {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    egui::RichText::new(*label)
                        .strong()
                        .size(11.5)
                        .color(egui::Color32::from_gray(215)),
                );
                // Emphasise keywords inline: crate names, file names, runtime/
                // async keywords and peripheral names each get their own colour.
                for word in body.split_whitespace() {
                    ui.label(detail_word(word));
                }
            });
            ui.add_space(4.0);
        }
        if !example.is_empty() {
            ui.add_space(2.0);
            egui::Frame::new()
                .inner_margin(egui::Margin::same(7))
                .corner_radius(egui::CornerRadius::same(4))
                .fill(egui::Color32::from_rgb(30, 30, 36))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(example)
                            .monospace()
                            .size(10.5)
                            .color(egui::Color32::from_rgb(180, 200, 180)),
                    );
                });
        }
    });
}

/// The Runtime-details highlighter knows every crate the Async panes name.
#[cfg(test)]
mod a_greyed_runtime_card_has_no_details_pane {
    use super::shows_runtime_details;
    use crate::panels::mcu_module::codegen::family::{
        async_unavailable_reason, native_unavailable_reason, rtic_unavailable_reason,
    };

    /// Per family, which of the three guarded cards draw a pane: (native,
    /// async, rtic). Blocking is not here because it is never greyed.
    const EXPECTED: &[(&str, (bool, bool, bool))] = &[
        // Every card: Blocking/Native/RTIC on stm32f1xx-hal, Async on embassy.
        ("stm32f1", (true, true, true)),
        ("stm32f4", (false, true, false)),
        ("rp2040", (false, true, false)),
        ("rp235x", (false, true, false)),
        ("nrf52833", (false, true, false)),
        ("nrf5340", (false, true, false)),
        ("nrf54l15", (false, true, false)),
        ("esp32c3", (false, true, false)),
    ];

    /// The bug this exists for: a Pico with an F1 pane under its greyed
    /// Native card. A pane is drawn exactly when the card can be chosen.
    #[test]
    fn the_pane_follows_the_cards_availability() {
        for (family, (native, r#async, rtic)) in EXPECTED {
            let got = (
                shows_runtime_details(native_unavailable_reason(family).as_deref()),
                shows_runtime_details(async_unavailable_reason(family).as_deref()),
                shows_runtime_details(rtic_unavailable_reason(family).as_deref()),
            );
            assert_eq!(got, (*native, *r#async, *rtic), "{family}");
        }
    }
}

#[cfg(test)]
mod the_details_pane_colors_every_async_crate {
    use super::DETAIL_LIBS;
    use crate::panels::mcu_module::codegen::family;

    /// Each family's stack, as the pane names it, is on the crate list; so is
    /// the Pico W radio pair its Cargo.toml row names.
    #[test]
    fn every_async_stack_is_a_known_crate() {
        for family in ["stm32f4", "rp2040", "nrf52833", "esp32c3"] {
            let stack = family::async_stack_name(family);
            assert!(
                DETAIL_LIBS.contains(&stack),
                "{family}: {stack} renders plain"
            );
        }
        for radio in ["cyw43", "cyw43-pio"] {
            assert!(DETAIL_LIBS.contains(&radio), "{radio} renders plain");
        }
    }
}

/// What the palette SAYS when it greys an entry out.
///
/// The wording is the whole value of a disabled row: it is the only thing that
/// tells the reader where to go and undo the blockage.
#[cfg(test)]
mod the_greyed_entry_names_what_is_in_the_way {
    use super::add_module_block_reason;
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::modules::ModuleKind;
    use crate::panels::mcu_module::pins::PinFunction;

    fn f103() -> Mcu {
        builtin_definitions()
            .into_iter()
            .find(|d| d.id == "stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu()
    }

    /// A chip with one USB, no USB module, and its two pads spent on GPIO.
    ///
    /// Nothing is wired to USB, so "remove one to free it" names a module that
    /// does not exist and sends the reader looking for it. The pads are what is
    /// in the way, and the pads are on the Pins canvas.
    #[test]
    fn spent_pads_are_named_even_on_a_chip_with_one_of_the_peripheral() {
        let mut mcu = f103();
        let kind = ModuleKind::GenericInterfaceUsb;
        assert!(kind.is_single_instance(), "the F103 has one USB");
        assert!(mcu.can_add_module(kind), "and it is free to start with");

        // Every pad USB could use goes to plain GPIO.
        let (required, _) = kind.signals();
        for &sig in required {
            let want = sig.pin_function(1);
            let pads: Vec<usize> = mcu
                .iter_all_pins()
                .filter(|p| p.available_functions.contains(&want))
                .map(|p| p.number)
                .collect();
            for n in pads {
                if let Some(p) = mcu.find_pin_mut(n) {
                    p.selected_function = PinFunction::GpioOutput;
                }
            }
        }

        assert!(!mcu.can_add_module(kind), "so it can no longer be added");
        assert!(
            mcu.has_free_instance(kind),
            "but the peripheral itself is untouched"
        );
        assert!(
            mcu.modules.is_empty(),
            "and there is no module anywhere to remove"
        );

        let why = add_module_block_reason(kind, None, mcu.has_free_instance(kind));
        assert!(
            why.contains("Pins canvas"),
            "it points at the pads: {why:?}"
        );
        assert!(
            !why.contains("already used") && !why.contains("remove one"),
            "and does not send the reader after a module: {why:?}"
        );
    }

    /// With the peripheral genuinely held, the wording goes back to naming it.
    #[test]
    fn a_held_single_instance_peripheral_is_still_reported_as_held() {
        let mut mcu = f103();
        let kind = ModuleKind::GenericInterfaceUsb;
        assert!(mcu.add_module(kind), "wire the chip's only USB");
        assert!(!mcu.has_free_instance(kind));
        let why = add_module_block_reason(kind, None, mcu.has_free_instance(kind));
        assert!(
            why.contains("only one such peripheral"),
            "the chip has one and it is taken: {why:?}"
        );
    }

    /// A peripheral no driver can reach beats both - the reader cannot do
    /// anything about pads or modules there.
    #[test]
    fn a_hardware_only_peripheral_says_so_first() {
        let why = add_module_block_reason(
            ModuleKind::GenericInterfaceUsb,
            Some("the HAL has no driver for it"),
            true,
        );
        assert_eq!(why, "the HAL has no driver for it");
    }
}

/// One click on a module box, two things that must agree about it.
#[cfg(test)]
mod the_canvas_and_the_list_agree_about_one_click {
    use super::config_open_after_click;

    /// The config the click SELECTED opens — and stays open however it came to
    /// be open before, which is the case the old blind toggle got wrong: a
    /// config `open_after_add` had just unfolded read as "open, so close it".
    #[test]
    fn the_clicked_box_shows_its_config() {
        assert!(config_open_after_click(
            "usart_1",
            "usart_1",
            Some("usart_1")
        ));
    }

    /// Clicking a selected box again deselects it on the canvas, so its config
    /// closes with it - the two halves of one switch.
    #[test]
    fn clicking_it_again_closes_the_config() {
        assert!(!config_open_after_click("usart_1", "usart_1", None));
    }

    /// Every other config closes, whatever the canvas selected.
    #[test]
    fn every_other_config_closes() {
        assert!(!config_open_after_click(
            "spi_2",
            "usart_1",
            Some("usart_1")
        ));
        assert!(!config_open_after_click("spi_2", "usart_1", None));
        assert!(
            !config_open_after_click("spi_2", "usart_1", Some("spi_2")),
            "a selection the click did not make does not keep a config open"
        );
    }
}

/// The gate that decides whether the panel has a body at all.
#[cfg(test)]
mod the_panel_body_appears_when_there_is_something_in_it {
    use super::panel_show_body;

    /// The invariant `+ Device` rests on, and the one nothing else checks.
    ///
    /// The button is on the BAR and creates the group there; the roster that
    /// unfolds the new row is only drawn when this says so. A bare chip — no
    /// modules, no configured pins — therefore has to flip to `true` on the
    /// strength of the new group alone, on the very frame it was made.
    #[test]
    fn a_bare_chip_shows_its_body_the_moment_a_device_exists() {
        assert!(
            !panel_show_body(false, false, false, false, false),
            "nothing to show yet"
        );
        assert!(
            panel_show_body(false, false, true, false, false),
            "one device is enough - this is what makes `+ Device` not a no-op"
        );
    }

    /// Each of the three reasons stands on its own.
    #[test]
    fn any_one_of_the_three_is_enough() {
        assert!(
            panel_show_body(false, true, false, false, false),
            "a module"
        );
        assert!(
            panel_show_body(false, false, true, false, false),
            "a device"
        );
        assert!(
            panel_show_body(false, false, false, true, false),
            "a pad to group"
        );
    }

    /// Orphaned notes keep the body open on their own. The state that makes
    /// them - the last module gone, every pad clear, as after Reset pins - is
    /// exactly the state in which none of the other three reasons holds, so
    /// without this the "Notes without a module" list vanished the moment it
    /// had something to show, and the notes could be neither seen nor deleted.
    #[test]
    fn orphaned_notes_alone_keep_the_body_open() {
        assert!(panel_show_body(false, false, false, false, true));
        assert!(
            !panel_show_body(true, false, false, false, true),
            "collapsed still wins"
        );
    }

    /// Collapsed outranks all three: the user asked for the bar alone.
    #[test]
    fn collapsed_wins_over_everything() {
        assert!(!panel_show_body(true, true, true, true, true));
    }
}
