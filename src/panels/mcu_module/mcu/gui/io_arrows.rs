//! In/out arrow graphics for the independently-bound pins on the Pins canvas.
//!
//! A lightweight cousin of the virtual-module schematic ([`super::modules`]):
//! every pin that becomes **its own variable** in the generated code gets an
//! arrow and a rename field here — GPIO In/Out, ADC, MCO and the generic
//! alternate functions. A BUS pin does not: it is consumed by its peripheral's
//! `init_*` and is drawn as a wire to that module's box instead. Neither does a
//! pin a virtual module already names: a Custom module carries its pins' fields
//! inside its own box, and a PWM channel claimed by a Timer module disappears
//! into that module's handle (see [`super::modules::module_owned_pins`]). The
//! arrow
//! points away from the chip for a driven signal, into it for one the MCU reads,
//! and carries no head when the function's direction is unknown (see
//! [`io_dir`]). Each arrow carries a small text field to rename the pin; the
//! typed text is appended to the generated binding name, e.g. a `pc13` output
//! labelled "led" becomes `let pc13_out_led = …`. No add/remove buttons — the
//! arrows simply mirror the pin functions.

use super::super::model::{Mcu, PIN_HEIGHT, PIN_SPACING, PIN_WIDTH};
use super::modules::{nearest_edge, pin_anchor_dir};
use super::rotate::Rot;

/// Line from `from` to `to` with a FIXED-size arrowhead at `to`. egui's
/// `Painter::arrow` scales the head with the vector length, so a long connector
/// (to a far / dragged field) grows a huge head — this keeps it constant.
fn connector(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    color: egui::Color32,
    // `false` for a signal whose direction we don't know (a generic alternate
    // function): a plain line says "wired here", an arrow would be a guess.
    head: bool,
    // The device this pad belongs to, when the canvas is pointing at it: the
    // shaft gains a halo in that device's colour, exactly as a module wire does.
    lit: Option<&str>,
    out: &mut (Vec<egui::Shape>, Vec<egui::Shape>),
) {
    let (halo, line) = super::modules::wire_shapes(&[from, to], color, 2.0, lit);
    out.0.extend(halo);
    out.1.push(line);
    let stroke = egui::Stroke::new(if lit.is_some() { 2.6_f32 } else { 2.0 }, color);
    let v = to - from;
    if !head || v.length() < 1.0 {
        return;
    }
    let dir = v.normalized();
    let head = 9.0;
    let rot = egui::emath::Rot2::from_angle(std::f32::consts::TAU / 12.0);
    painter.line_segment([to, to - head * (rot * dir)], stroke);
    painter.line_segment([to, to - head * (rot.inverse() * dir)], stroke);
}
use crate::panels::mcu_module::codegen::{pin_binding, sanitize_label};
use crate::panels::mcu_module::pins::logic::pin::{Edge, TaskPriority};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use eframe::egui;

/// Shared with the chip pins and the module boxes — one selection look.
use crate::panels::mcu_module::pins::gui::draw::SELECTED_TEXT_SCALE as SEL_SCALE;

const ARROW_LEN: f32 = 24.0;
const GAP: f32 = 5.0;
const FIELD_W: f32 = 88.0;
const FIELD_H: f32 = 18.0;

/// Canvas margin (beyond the pin tips) needed to fit an arrow + rename field.
pub const MARGIN_X: f32 = PIN_HEIGHT + ARROW_LEN + GAP + FIELD_W + 14.0;
pub const MARGIN_Y: f32 = PIN_HEIGHT + ARROW_LEN + GAP + FIELD_H + 14.0;

/// Field centres for ball-grid pads: a column just outside the body on the side
/// the pad leans to (`dir_x`), pads stacked top-down in their own row order.
///
/// `pads` is `(pin number, anchor y, outward x)`; `row` is the vertical pitch of
/// one field + its preview strip. Pure, so the rule that matters — the field
/// lands OUTSIDE `body`, and two pads never share a slot — is testable without
/// an egui context.
fn ball_columns(
    pads: &[(usize, f32, f32)],
    body: egui::Rect,
    row: f32,
) -> std::collections::HashMap<usize, egui::Pos2> {
    let mut out = std::collections::HashMap::new();
    for right in [true, false] {
        let mut group: Vec<&(usize, f32, f32)> = pads
            .iter()
            .filter(|(_, _, dx)| (*dx >= 0.0) == right)
            .collect();
        group.sort_by(|a, b| a.1.total_cmp(&b.1));
        let col_x = if right {
            body.right() + ARROW_LEN + GAP + FIELD_W / 2.0
        } else {
            body.left() - ARROW_LEN - GAP - FIELD_W / 2.0
        };
        for (i, (num, _, _)) in group.iter().enumerate() {
            out.insert(*num, egui::pos2(col_x, body.top() + i as f32 * row));
        }
    }
    out
}

/// Which way a pin's signal flows, for the arrowhead.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IoDir {
    /// Driven by the MCU — arrow points away from the chip.
    Out,
    /// Read by the MCU — arrow points into the chip.
    In,
    /// Unknown: a generic alternate function carries only the datasheet's signal
    /// name, which says nothing about direction. Drawn as a plain line.
    Plain,
}

/// The arrow direction for a pin that gets its OWN binding on the diagram, or
/// `None` for a pin that gets no field at all.
///
/// The rule is "does this pin become a variable the user can rename": every
/// non-bus function does (`let pa1_adc = …`, `let pa8_mco = …`), while a BUS pin
/// is consumed by its peripheral's `init_*` and is drawn as a wire to that
/// module's box instead — and SWD generates a comment, not a binding, so there
/// is nothing to name.
fn io_dir(func: &PinFunction) -> Option<IoDir> {
    if func.is_bus() {
        return None;
    }
    match func {
        PinFunction::GpioOutput | PinFunction::TimerPwm { .. } | PinFunction::Mco => {
            Some(IoDir::Out)
        }
        PinFunction::GpioInput | PinFunction::AdcChannel { .. } => Some(IoDir::In),
        // Analog mode binds its own variable but has no direction — the pad is
        // handed to an analog block. Same shape as a generic alternate function.
        PinFunction::GpioAnalog | PinFunction::Other(_) => Some(IoDir::Plain),
        // Unset has no binding; SWD/JTAG generate a comment, not a variable.
        PinFunction::Unset | PinFunction::SwdIo | PinFunction::SwdClk => None,
        // A complementary PWM pad is never its own variable either: it is
        // handed straight to its timer's `ComplementaryPwm`, which owns the
        // name. Spelled out rather than left to the catch-all, because it is a
        // decision and not an oversight.
        PinFunction::TimerPwmN { .. } => None,
        // A break pad is an INPUT the timer reads by itself; the generated
        // `init` only puts it in alternate-function mode and holds it.
        PinFunction::TimerBreak { .. } => None,
        // Every remaining variant is a bus pin, already returned above.
        _ => None,
    }
}

/// Whether any non-reserved pin gets a floating field (so the canvas should
/// reserve a margin for arrows).
///
/// Module-owned pins are excluded for the same reason `draw_io_arrows` skips
/// them: a chip whose only such pin is a PWM channel draws no field, and would
/// otherwise reserve a gutter for arrows nobody paints.
pub fn has_io_pins(mcu: &Mcu) -> bool {
    let owned = super::modules::module_owned_pins(mcu);
    mcu.iter_all_pins().any(|p| {
        !p.reserved && !owned.contains(&p.number) && io_dir(&p.selected_function).is_some()
    })
}

/// Draw an in/out arrow + rename field for every independently-bound pin. The
/// text field edits the pin's `custom_label` in place (regenerated into the
/// binding name every frame by `update_main_rs`).
pub fn draw_io_arrows(
    mcu: &mut Mcu,
    painter: &egui::Painter,
    local_chip: egui::Rect,
    rot: Rot,
    ui: &mut egui::Ui,
    // Out-param: each field's full painted footprint, for the device mats. It is
    // filled from INSIDE the draw loop, which is legal because the mats go into
    // a slot reserved before the chip body and filled after this returns - so
    // `name_rect`, the one rect that cannot exist before painting, costs nothing.
    members: &mut Vec<super::device_frame::Member>,
) {
    // Snapshot geometry first (immutable borrow), then edit labels (mutable).
    struct Item {
        num: usize,
        anchor: egui::Pos2,
        dir: egui::Vec2,
        flow: IoDir,
        color: egui::Color32,
        preview_base: String,
        /// The device holding this pad, if any. The NAME and not the colour:
        /// `group_color` maps six hues over unbounded names, so two devices can
        /// collide and a mat keyed on colour would merge them into one.
        /// Snapshotted here because the draw loop below borrows `mcu` mutably.
        group: Option<String>,
        /// Whether this field draws an IRQ strip under it - known only from the
        /// pin, which the loop reads again only behind a mutable borrow.
        has_irq: bool,
    }
    // A pin whose name belongs to a virtual module gets no field here: a Custom
    // module draws it inside its own box, and a Timer module owns the PWM
    // handle the channel pad disappears into. See `module_owned_pins`.
    let owned = super::modules::module_owned_pins(mcu);
    let mut items: Vec<Item> = Vec::new();
    for p in mcu.iter_all_pins() {
        if p.reserved || owned.contains(&p.number) {
            continue;
        }
        let Some(flow) = io_dir(&p.selected_function) else {
            continue;
        };
        let Some((anchor, dir)) = pin_anchor_dir(mcu, local_chip, rot, p.number) else {
            continue;
        };
        items.push(Item {
            num: p.number,
            anchor,
            dir,
            flow,
            color: p.selected_function.color(),
            // Base binding the label is appended to, e.g. "pc13_out" / "gpio2_out".
            preview_base: pin_binding(&p.name.to_ascii_lowercase(), &p.selected_function, ""),
            group: mcu.group_of_pin(p.number).map(|g| g.name.clone()),
            has_irq: p.selected_function == PinFunction::GpioInput,
        });
    }

    let chip_center = local_chip.center();
    // In the diamond the pins point DIAGONALLY, so auto-placing each field along
    // its pin pushes it far up the diagonal AND lets neighbours overlap. Group
    // the auto (non-dragged) io pins into CONTIGUOUS runs along each edge: a run
    // of ≥2 adjacent pins becomes a vertical column beside the chip (fields at a
    // fixed x → their labels line up), while a lone io pin keeps the simple
    // beside-the-pin placement.
    let pitch = PIN_WIDTH + PIN_SPACING;
    let row = FIELD_H + 16.0 + 8.0; // field + preview strip + gap
    let mut packed: std::collections::HashMap<usize, egui::Pos2> = std::collections::HashMap::new();

    // ── Ball-grid pads ───────────────────────────────────────────────────────
    // A ball sits INSIDE the body, so "a short arrow along the pin's outward
    // direction" lands its field on top of the neighbouring balls (and their
    // labels). Those pads get their fields stacked in a column OUTSIDE the body
    // instead — left or right, whichever half of the grid the pad is on — which
    // is both out of the way and non-overlapping. Edge pins are untouched: their
    // stubs already stick out, so the simple placement below is right for them.
    let ball_pins: std::collections::HashSet<usize> = mcu
        .grid
        .iter()
        .flat_map(|g| g.cells.iter().map(|c| c.pin.number))
        .collect();
    let body = egui::Rect::from_points(&rot.quad(local_chip));
    if !ball_pins.is_empty() {
        let pads: Vec<(usize, f32, f32)> = items
            .iter()
            .filter(|it| !mcu.io_pin_pos.contains_key(&it.num) && ball_pins.contains(&it.num))
            .map(|it| (it.num, it.anchor.y, it.dir.x))
            .collect();
        packed.extend(ball_columns(&pads, body, row));
    }
    {
        // Bucket diagonal, non-dragged io pins by edge (sign of the outward dir).
        let mut by_side: std::collections::BTreeMap<(i32, i32), Vec<&Item>> =
            std::collections::BTreeMap::new();
        for it in &items {
            if mcu.io_pin_pos.contains_key(&it.num) || ball_pins.contains(&it.num) {
                continue;
            }
            if it.dir.x.abs() > 0.3 && it.dir.y.abs() > 0.3 {
                let key = (it.dir.x.signum() as i32, it.dir.y.signum() as i32);
                by_side.entry(key).or_default().push(it);
            }
        }
        for ((sx, _), mut group) in by_side {
            // Sort along the edge (tangent ⟂ outward), then split into runs where
            // a gap > 1.5·pitch means a non-io pin sits between two io pins.
            let tangent = egui::vec2(-group[0].dir.y, group[0].dir.x);
            let proj = |it: &&Item| it.anchor.to_vec2().dot(tangent);
            group.sort_by(|a, b| proj(a).total_cmp(&proj(b)));
            let mut runs: Vec<Vec<&Item>> = Vec::new();
            let mut cur: Vec<&Item> = Vec::new();
            let mut prev = f32::NAN;
            for it in group {
                let t = proj(&it);
                if prev.is_finite() && (t - prev).abs() > 1.5 * pitch {
                    runs.push(std::mem::take(&mut cur));
                }
                cur.push(it);
                prev = t;
            }
            if !cur.is_empty() {
                runs.push(cur);
            }
            for mut run in runs {
                if run.len() < 2 {
                    continue; // a lone io pin stays with the simple placement
                }
                // Column just outside the run (outward side = sign of dir.x).
                let col_x = if sx >= 0 {
                    run.iter().map(|it| it.anchor.x).fold(f32::MIN, f32::max)
                        + ARROW_LEN
                        + GAP
                        + FIELD_W / 2.0
                } else {
                    run.iter().map(|it| it.anchor.x).fold(f32::MAX, f32::min)
                        - ARROW_LEN
                        - GAP
                        - FIELD_W / 2.0
                };
                run.sort_by(|a, b| a.anchor.y.total_cmp(&b.anchor.y));
                let base_y = run[0].anchor.y;
                for (i, it) in run.iter().enumerate() {
                    packed.insert(it.num, egui::pos2(col_x, base_y + i as f32 * row));
                }
            }
        }
    }

    // Drag / right-click-reset are collected and applied AFTER the loop (it
    // borrows `mcu.find_pin_mut`).
    let mut drag_updates: Vec<(usize, (f32, f32))> = Vec::new();
    let mut reset_updates: Vec<usize> = Vec::new();
    let mut click_request: Option<usize> = None;
    // Same idiom as the module wires: one slot for every connector shaft, so a
    // field drawn later cannot paint over an earlier field's line. Halos in `.0`,
    // lines in `.1`, concatenated in that order when the slot is filled.
    let conn_slot = painter.add(egui::Shape::Noop);
    let mut conn_shapes: (Vec<egui::Shape>, Vec<egui::Shape>) = (Vec::new(), Vec::new());
    // The device the canvas is pointing at. Read before the loop takes `mcu`.
    let active = mcu.active_device().map(str::to_owned);
    // …and the one being dragged, for the same reason.
    let dev_drag = mcu.device_drag.clone();
    // The pin the user has selected on the chip: its field group out here is
    // called out the same way as the pin itself and as a selected module box.
    let selected_pin = mcu.selected_pin;
    // Armed inputs that hit an erratum together (the classic ESP32's interrupt
    // groups): `(gpio, text)`, worked out once before the loop borrows `mcu`.
    let irq_clashes: Vec<(String, String)> = crate::panels::mcu_module::errata::clashes(mcu)
        .into_iter()
        .flat_map(|c| {
            c.pads
                .into_iter()
                .map(move |g| (g, c.text.clone()))
                .collect::<Vec<_>>()
        })
        .collect();

    for it in items {
        // Field centre: the user's dragged offset, else the packed diamond
        // column, else (axis-aligned default / 90°) straight beyond the arrow.
        let manual = mcu.io_pin_pos.get(&it.num).copied();
        let packed_center = packed.get(&it.num).copied();
        let field_center = if let Some((ox, oy)) = manual {
            chip_center + egui::vec2(ox, oy)
        } else if let Some(c) = packed_center {
            c
        } else {
            let along =
                ARROW_LEN + GAP + (it.dir.x.abs() * FIELD_W + it.dir.y.abs() * FIELD_H) / 2.0;
            it.anchor + it.dir * along
        };
        let field_rect = egui::Rect::from_center_size(field_center, egui::vec2(FIELD_W, FIELD_H));

        // The device this field belongs to is being dragged: move with it, from
        // wherever the auto placement just put it. Pushed BEFORE this field's own
        // drag handler below, so dragging one field out of a device still wins.
        if let Some((dev, (dx, dy))) = &dev_drag
            && it.group.as_deref().map(str::trim) == Some(dev.trim())
        {
            let off = field_center + egui::vec2(*dx, *dy) - chip_center;
            drag_updates.push((it.num, (off.x, off.y)));
        }

        // Connector pin ↔ field, with a FIXED-size arrowhead (see `connector`).
        // Straight axis-aligned auto = a short arrow along the pin direction;
        // dragged OR packed-column = a line to the nearest field edge (re-routes
        // as it moves), arrowhead pointing the data direction (out of / into pin).
        let routed = manual.is_some() || packed_center.is_some();
        painter.circle_filled(it.anchor, 2.5, it.color);
        let outward = it.anchor + it.dir * ARROW_LEN;
        let far = if routed {
            nearest_edge(field_rect, it.anchor)
        } else {
            outward
        };
        // The head points the way the data flows; an unknown direction gets a
        // plain line, drawn outward like an output so the geometry is the same.
        let (from, to) = match it.flow {
            IoDir::In => (far, it.anchor),
            IoDir::Out | IoDir::Plain => (it.anchor, far),
        };
        let lit = active
            .as_deref()
            .filter(|d| it.group.as_deref().map(str::trim) == Some(*d));
        connector(
            painter,
            from,
            to,
            it.color,
            it.flow != IoDir::Plain,
            lit,
            &mut conn_shapes,
        );

        // Preview of the resulting binding name — faint above the field, and the
        // DRAG HANDLE (the field itself is a text box, so drag from its label).
        let preview = {
            let label = mcu
                .find_pin(it.num)
                .map(|p| p.custom_label.clone())
                .unwrap_or_default();
            let suffix = sanitize_label(&label);
            if suffix.is_empty() {
                it.preview_base.clone()
            } else {
                format!("{}_{}", it.preview_base, suffix)
            }
        };
        let handle_rect = egui::Rect::from_min_max(
            egui::pos2(field_rect.left(), field_rect.top() - 16.0),
            egui::pos2(field_rect.right(), field_rect.top()),
        );
        let resp = ui
            .interact(
                handle_rect,
                ui.id().with(("io_drag", it.num)),
                egui::Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab)
            .on_hover_text(
                "Click to select the pin and jump to this variable in the code - \
                 drag to move the field",
            );
        // The strip SHOWS the generated variable name, so clicking it does what
        // clicking the pin on the chip does: select it (click again to deselect)
        // and jump to the line that binds the variable.
        if resp.clicked() {
            click_request = Some(it.num);
        }
        if resp.dragged() && crate::panels::drag_decided(ui) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            let off = (field_center + resp.drag_delta()) - chip_center;
            drag_updates.push((it.num, (off.x, off.y)));
        }
        if manual.is_some() {
            resp.context_menu(|ui| {
                if ui.button("Reset field position").clicked() {
                    reset_updates.push(it.num);
                    ui.close();
                }
            });
        }
        // A selected pin's group (variable name + rename field) is drawn WHITE
        // and `SELECTED_TEXT_SCALE` larger, exactly like the pin on the chip and
        // like a selected module box.
        let selected = selected_pin == Some(it.num);
        let scale = if selected { SEL_SCALE } else { 1.0 };
        let pcol = if selected {
            egui::Color32::WHITE
        } else if resp.hovered() {
            egui::Color32::from_rgb(205, 205, 215)
        } else {
            egui::Color32::from_rgb(140, 140, 150)
        };
        // Left-aligned with the field's left edge, so a column's labels line up.
        // `Painter::text` hands back the rect it painted — the variable name is
        // free text and routinely WIDER than the fixed-width field below it
        // (`pc14_out_power_led_light`), so the selection border is sized from
        // this rect, not from the field alone.
        let name_rect = painter.text(
            field_rect.left_top() - egui::vec2(0.0, 2.0),
            egui::Align2::LEFT_BOTTOM,
            preview,
            egui::FontId::proportional(9.0 * scale),
            pcol,
        );

        // The device this pad belongs to: the same bar a grouped module's box
        // wears, laid over the variable name rather than under a title. The pad
        // itself is ticked on the chip; this says the field out here is the same
        // device, without making the reader follow the wire back.
        if let Some(g) = &it.group {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(name_rect.left(), name_rect.top() - 4.0),
                    egui::vec2(name_rect.width().max(12.0), super::modules::GROUP_BAR_H),
                ),
                1.0,
                super::modules::group_color(g),
            );
        }

        // The field's full painted footprint, for the device mat: the text box,
        // the drag handle, the variable name above it (which is routinely WIDER
        // than the box), the group bar over that, and the IRQ strip below when
        // there is one. The connector is left out on purpose - it belongs to the
        // pin as much as to the field, and including it would drag every mat
        // back onto the chip.
        let mut foot = field_rect.union(handle_rect).union(name_rect);
        foot.min.y -= 4.0;
        if it.has_irq {
            foot.max.y += 18.0 * scale;
        }
        members.push(super::device_frame::Member {
            group: it.group.clone(),
            rect: foot,
            covers: vec![it.num],
        });

        if let Some(pin) = mcu.find_pin_mut(it.num) {
            ui.push_id(("io_label", it.num), |ui| {
                ui.put(
                    field_rect,
                    egui::TextEdit::singleline(&mut pin.custom_label)
                        .hint_text("name")
                        .font(egui::FontId::proportional(10.0 * scale)),
                );
            });
            // Interrupt edge — INPUTS only, and opt-in: an input with no edge is
            // one you poll, which is most of them. Only the RTIC runtime acts on
            // it (each armed pin becomes a hardware task), so the button is
            // shown but explains itself elsewhere.
            if pin.selected_function == PinFunction::GpioInput {
                let irq_rect = egui::Rect::from_min_max(
                    egui::pos2(field_rect.left(), field_rect.bottom() + 2.0),
                    egui::pos2(field_rect.right(), field_rect.bottom() + 18.0 * scale),
                );
                let clash = pin
                    .irq
                    .and_then(|_| irq_clashes.iter().find(|(g, _)| g == pin.gpio()))
                    .map(|(_, text)| text.clone());
                let (label, col) = match (pin.irq, &clash) {
                    (None, _) => ("no IRQ".to_string(), egui::Color32::from_gray(130)),
                    (Some(e), Some(_)) => (
                        format!("IRQ {} !", e.label().to_ascii_lowercase()),
                        egui::Color32::from_rgb(240, 130, 110),
                    ),
                    (Some(e), None) => (
                        format!("IRQ {}", e.label().to_ascii_lowercase()),
                        egui::Color32::from_rgb(235, 180, 90),
                    ),
                };
                ui.push_id(("io_irq", it.num), |ui| {
                    ui.put(irq_rect, |ui: &mut egui::Ui| {
                        ui.menu_button(
                            egui::RichText::new(label).size(9.5 * scale).color(col),
                            |ui| {
                                ui.set_min_width(140.0);
                                if let Some(text) = &clash {
                                    ui.set_max_width(280.0);
                                    ui.label(
                                        egui::RichText::new(text)
                                            .size(9.5)
                                            .color(egui::Color32::from_rgb(240, 130, 110)),
                                    );
                                    ui.separator();
                                }
                                ui.label(
                                    egui::RichText::new("Interrupt on")
                                        .size(10.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.separator();
                                let mut pick = |ui: &mut egui::Ui, v: Option<Edge>, t: &str| {
                                    if ui.selectable_label(pin.irq == v, t).clicked() {
                                        pin.irq = v;
                                        ui.close();
                                    }
                                };
                                pick(ui, None, "No interrupt (polled)");
                                pick(ui, Some(Edge::Rising), "Rising edge");
                                pick(ui, Some(Edge::Falling), "Falling edge");
                                pick(ui, Some(Edge::Both), "Both edges");

                                // Priority is only a question once there IS a
                                // task: an unarmed pin has nothing to schedule.
                                if pin.irq.is_some() {
                                    ui.separator();
                                    ui.label(
                                        egui::RichText::new("Task priority")
                                            .size(10.0)
                                            .color(egui::Color32::GRAY),
                                    );
                                    for p in TaskPriority::all() {
                                        if ui
                                            .selectable_label(pin.irq_priority == p, p.label())
                                            .clicked()
                                        {
                                            pin.irq_priority = p;
                                            ui.close();
                                        }
                                    }
                                    ui.label(
                                        egui::RichText::new(
                                            "Above Normal runs on its own interrupt \
                                             executor and preempts the rest. Tasks sharing \
                                             one level stay cooperative. ESP async only.",
                                        )
                                        .size(9.0)
                                        .color(egui::Color32::from_gray(140)),
                                    );
                                }
                            },
                        )
                        .response
                    })
                    // One string per line: a raw newline inside the literal
                    // carries its indentation into the tooltip, which is how
                    // this one used to render with a gap through the middle.
                    .on_hover_text({
                        let how = concat!(
                            "Raise an interrupt on this input.\n",
                            "ESP + Async: the pin becomes its own task, awaiting the edge.\n",
                            "ESP + Blocking: it is parked in a static, and one shared handler asks it.\n",
                            "STM32 + Async: it becomes an ExtiInput, and a task awaits the edge.\n",
                            "STM32F1 + RTIC: it becomes a #[task(binds = EXTIn)].\n",
                            "STM32F1 Blocking/Native: a #[interrupt] over a static holding the pin.\n",
                            "RP (Pico) + Async: the pin becomes its own task, awaiting the edge.\n",
                            "nRF (micro:bit) + Async: the pin becomes its own task, awaiting the edge.\n",
                            "nRF (micro:bit) + Blocking: nothing is generated for it; poll the pin.",
                        );
                        match &clash {
                            Some(text) => format!("{text}\n\n{how}"),
                            None => how.to_owned(),
                        }
                    });
                });
            }
        }

        // One border around the WHOLE group — the variable-name strip on top and
        // the rename field under it — so the selection reads as one item (the
        // module box's white border, applied to a lone pin). It takes the WIDER
        // of the two (the name usually), so a long binding is never clipped.
        if selected {
            painter.rect_stroke(
                field_rect.union(handle_rect).union(name_rect).expand(3.0),
                4.0,
                // Same 2.8 px as a selected module box's border.
                egui::Stroke::new(2.8_f32, egui::Color32::WHITE),
                egui::StrokeKind::Middle,
            );
        }
    }
    // Apply drag / reset now the per-item `mcu` borrows have ended.
    let (mut halos, mut lines) = conn_shapes;
    halos.append(&mut lines);
    painter.set(conn_slot, egui::Shape::Vec(halos));

    for (num, off) in drag_updates {
        mcu.io_pin_pos.insert(num, off);
    }
    for num in reset_updates {
        mcu.io_pin_pos.remove(&num);
    }
    // Same toggle as clicking the pin on the chip (see `Mcu::draw`): select it,
    // click again to deselect, and jump to the code only on the click that
    // selects. Applied after the loop, so the border it turns on shows on the
    // next frame — hence the repaint request.
    if let Some(num) = click_request {
        mcu.selected_pin = if mcu.selected_pin == Some(num) {
            None
        } else {
            Some(num)
        };
        if mcu.selected_pin == Some(num) {
            mcu.fn_scroll_offset = 0.0;
            mcu.request_pin_goto(num);
        }
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::modules::ModuleKind;

    /// Every pin that becomes its OWN variable gets a field; a bus pin (drawn as
    /// a wire to its module's box) and a pin that generates only a comment do
    /// not. This is the list that decides what appears beside the chip.
    #[test]
    fn every_independently_bound_function_gets_a_field() {
        use PinFunction::*;
        for (func, want) in [
            (GpioOutput, Some(IoDir::Out)),
            (
                TimerPwm {
                    timer: 3,
                    channel: 1,
                },
                Some(IoDir::Out),
            ),
            (Mco, Some(IoDir::Out)),
            (GpioInput, Some(IoDir::In)),
            (GpioAnalog, Some(IoDir::Plain)),
            (AdcChannel { adc: 1, channel: 4 }, Some(IoDir::In)),
            (Other("SAI1_SD_A".into()), Some(IoDir::Plain)),
            // No binding to name: SWD is a comment, Unset is nothing.
            (SwdIo, None),
            (SwdClk, None),
            (Unset, None),
            // Bus pins belong to a Virtual Module's box, not to a floating field.
            (UsartTx(1), None),
            (SpiSck(1), None),
            (I2cSda(1), None),
            (CanRx, None),
            (UsbDp, None),
        ] {
            assert!(
                io_dir(&func) == want,
                "{func:?} should {} a field",
                if want.is_some() { "get" } else { "not get" }
            );
        }
    }

    /// Build an F103 with the given functions applied, then let the virtual
    /// modules reconcile against the wiring.
    fn f103_with(pins: &[(&str, PinFunction)]) -> Mcu {
        use crate::panels::mcu_module::builtins::builtin_for;
        let mut mcu = builtin_for("stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        for (name, func) in pins {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == *name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = func.clone();
            }
        }
        mcu.reconcile_modules();
        mcu
    }

    fn pin_num(mcu: &Mcu, name: &str) -> usize {
        mcu.iter_all_pins()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no pin {name}"))
            .number
    }

    /// A PWM channel belongs to its Timer module, which already carries the
    /// name that reaches the generated code (`_pwm2`). Floating a second
    /// rename field beside the chip put two "name" boxes on one pin, and on
    /// embassy the pin's one is dead - the pad is passed straight into
    /// `init`, never bound.
    #[test]
    fn a_pwm_channel_owned_by_a_timer_gets_no_floating_field() {
        let mcu = f103_with(&[
            (
                "PA0",
                PinFunction::TimerPwm {
                    timer: 2,
                    channel: 1,
                },
            ),
            ("PC13", PinFunction::GpioOutput),
        ]);
        // The module really did claim the channel.
        assert!(
            mcu.modules
                .iter()
                .any(|m| m.kind == ModuleKind::GenericInterfaceTimer),
            "reconcile should have created the timer module"
        );

        let owned = super::super::modules::module_owned_pins(&mcu);
        assert!(
            owned.contains(&pin_num(&mcu, "PA0")),
            "the PWM channel is the timer module's"
        );
        assert!(
            !owned.contains(&pin_num(&mcu, "PC13")),
            "a plain GPIO output still names itself"
        );
        // The function itself is unchanged - it is the OWNERSHIP that decides,
        // so a PWM pin with no module (an older project) keeps its field.
        assert!(
            io_dir(&PinFunction::TimerPwm {
                timer: 2,
                channel: 1
            }) == Some(IoDir::Out)
        );
    }

    /// …and with nothing but that channel wired, the canvas reserves no gutter
    /// for arrows it will not paint.
    #[test]
    fn a_timer_only_chip_reserves_no_arrow_margin() {
        let pwm_only = f103_with(&[(
            "PA0",
            PinFunction::TimerPwm {
                timer: 2,
                channel: 1,
            },
        )]);
        assert!(!has_io_pins(&pwm_only), "no floating field, no margin");

        let with_gpio = f103_with(&[
            (
                "PA0",
                PinFunction::TimerPwm {
                    timer: 2,
                    channel: 1,
                },
            ),
            ("PC13", PinFunction::GpioOutput),
        ]);
        assert!(has_io_pins(&with_gpio), "PC13 still needs its field");
    }

    /// A ball's field must leave the body — that is the whole point: a pad in
    /// the middle of the grid used to get its field dropped on its neighbours.
    #[test]
    fn ball_fields_land_outside_the_body_and_never_share_a_slot() {
        let body = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0));
        let row = 42.0;
        // Two pads leaning right, one leaning left — all deep inside the body.
        let pads = [
            (1usize, 300.0_f32, 0.7_f32),
            (2, 120.0, 0.4),
            (3, 200.0, -0.5),
        ];
        let out = ball_columns(&pads, body, row);

        assert_eq!(out.len(), 3);
        for (num, c) in &out {
            let field = egui::Rect::from_center_size(*c, egui::vec2(FIELD_W, FIELD_H));
            assert!(
                field.left() > body.right() || field.right() < body.left(),
                "pin {num} field {field:?} still overlaps the body {body:?}"
            );
        }
        // Right column: the upper pad (2) takes the first slot, the lower (1) the
        // second — order follows the grid, and they are a full `row` apart.
        assert_eq!(out[&2].x, out[&1].x);
        assert_eq!(out[&2].y, body.top());
        assert_eq!(out[&1].y, body.top() + row);
        // The left column starts at the top again, on the other side.
        assert!(out[&3].x < body.left());
        assert_eq!(out[&3].y, body.top());
    }
}

/// Half-extents (from the chip centre) needed to keep every DRAGGED in/out field
/// on-canvas — combined with the module extent by `Mcu::draw` so the Scene's
/// auto-fit doesn't clip a field pulled far from the chip.
pub fn dragged_half_extent(mcu: &Mcu) -> egui::Vec2 {
    let mut hx = 0.0_f32;
    let mut hy = 0.0_f32;
    for (x, y) in mcu.io_pin_pos.values() {
        hx = hx.max(x.abs() + FIELD_W / 2.0);
        hy = hy.max(y.abs() + FIELD_H / 2.0 + 16.0);
    }
    egui::vec2(hx, hy)
}
