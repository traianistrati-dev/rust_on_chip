//! The devices on an I2C bus, drawn on the Pins canvas as boxes beside the
//! bus's own box and wired to it by SCL and SDA.
//!
//! # Layout
//!
//! A bus's devices form a COLUMN on the side of the bus box that faces away
//! from the chip - never between the box and the pins, where the wire corridor
//! is clear only by construction (`wire.rs`).
//!
//! * A bus above or below the chip stacks its devices further out, inside its
//!   own 170 px band, so two buses on that side can never reach into each
//!   other's column.
//! * A bus left or right of the chip puts the column beside it, starting a
//!   little below its top edge. The packer is told how far the column runs past
//!   the box (`tail`), so the next bus on that side is pushed clear of it.
//!
//! # Wires
//!
//! A real bus, as a schematic draws one: an SCL rail down one side of the
//! column and an SDA rail down the other, with a tap into every device. SCL and
//! SDA are the same green on this canvas (the pads' colour), so the layout is
//! built to have NO crossing at all - a crossing of two lines of one colour
//! reads as a short, whatever dot is or is not drawn on it. Each device box says
//! which side is which in its top strip.
//!
//! The gaps are kept under `device_frame::JOIN`, so a device mat merges a bus
//! and its devices into one mat instead of drawing `name 1/2` and `name 2/2`.

use super::i2c_devices::{I2cAct, id_field, name_field, remove_question};
use super::modules::{Side, signal_color, text_scale, tint, wire_shapes};
use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::modules::{I2cDeviceEdit, I2cDeviceKey, ModuleConfig, ModuleSignal};
use eframe::egui;

/// A device box.
pub const CHILD_W: f32 = 150.0;
pub const CHILD_H: f32 = 60.0;
/// The strip at the top of a device box: `SCL  #n  SDA`, and where the taps
/// land. It is also what a click on the box lands on (the rest is fields).
const STRIP_H: f32 = 14.0;
/// Between a bus box above/below the chip and its first device, and between
/// two devices of one bus.
pub const GAP: f32 = 8.0;
/// Between a bus box left/right of the chip and its column - the SCL rail runs
/// down the middle of it.
pub const GAP_SIDE: f32 = 20.0;
/// How far below a left/right bus box's top its column starts: room for the
/// SDA wire to pass over the first device on its way to the far side - and far
/// enough that the SECOND device starts below the bus box. A device mat merging
/// the bus box with its first device covers the box's whole height; with a
/// device of another Device beside that span, the merge would be refused and
/// the bus's Device drawn in two pieces.
const SIDE_DROP: f32 = 32.0;
/// Where the SDA wire leaves a left/right bus box, below its top.
const SDA_Y: f32 = 8.0;
/// How far outside a column the rails of an above/below bus run - inside the
/// bus's 170 px band, which the 150 px column leaves 10 px of on each side.
const RAIL_OUT: f32 = 5.0;
/// How far past the column a left/right bus's SDA rail runs.
const SDA_OUT: f32 = 10.0;

/// One bus's devices, placed, with their wires.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Bus {
    pub side: Side,
    /// One per device, in list order; the first is the one nearest the bus box.
    pub children: Vec<egui::Rect>,
    /// SCL and SDA as polylines, each starting at the bus box end: one rail
    /// that ends in the farthest device's tap, and one short tap per other
    /// device.
    pub scl: Vec<Vec<egui::Pos2>>,
    pub sda: Vec<Vec<egui::Pos2>>,
    /// Where a tap leaves a rail that goes on past it - drawn as a dot, the
    /// schematic sign that two lines are joined.
    pub junctions: Vec<egui::Pos2>,
    /// The points on the bus box the two wires leave from: [SCL, SDA].
    pub terminals: [egui::Pos2; 2],
}

impl Bus {
    /// Everything the bus's devices and wires cover.
    pub fn bounds(&self) -> egui::Rect {
        let mut r = egui::Rect::NOTHING;
        for c in &self.children {
            r = r.union(*c);
        }
        for p in self.scl.iter().chain(&self.sda).flatten() {
            r.extend_with(*p);
        }
        r
    }
}

/// The length of a column of `n` devices.
fn column_len(n: usize) -> f32 {
    n as f32 * CHILD_H + n.saturating_sub(1) as f32 * GAP
}

/// How far a bus box's devices run past its far end along its side of the chip:
/// what the packer adds before placing the next box on that side. Only a
/// left/right bus has any; above or below, the column goes outward instead.
pub(super) fn tail(side: Side, box_h: f32, n: usize) -> f32 {
    match side {
        Side::Left | Side::Right if n > 0 => (SIDE_DROP + column_len(n) - box_h).max(0.0),
        _ => 0.0,
    }
}

/// How many device boxes a module draws: its I2C devices, none for anything else.
pub(super) fn device_count(config: &ModuleConfig) -> usize {
    match config {
        ModuleConfig::I2c(c) => c.rows().len(),
        _ => 0,
    }
}

/// Place `n` devices beside bus box `parent`, which sits on `side` of the chip.
pub(super) fn layout(parent: egui::Rect, side: Side, n: usize) -> Bus {
    let mut bus = Bus {
        side,
        children: Vec::new(),
        scl: Vec::new(),
        sda: Vec::new(),
        junctions: Vec::new(),
        terminals: [parent.center(); 2],
    };
    if n == 0 {
        return bus;
    }
    let tap_y = |c: &egui::Rect| c.top() + STRIP_H / 2.0;
    match side {
        Side::Top | Side::Bottom => {
            let top = side == Side::Top;
            let edge_y = if top { parent.top() } else { parent.bottom() };
            let left = parent.center().x - CHILD_W / 2.0;
            for k in 0..n {
                let near = GAP + k as f32 * (CHILD_H + GAP);
                let y0 = if top {
                    edge_y - near - CHILD_H
                } else {
                    edge_y + near
                };
                bus.children.push(egui::Rect::from_min_size(
                    egui::pos2(left, y0),
                    egui::vec2(CHILD_W, CHILD_H),
                ));
            }
            let scl_x = left - RAIL_OUT;
            let sda_x = left + CHILD_W + RAIL_OUT;
            bus.terminals = [egui::pos2(scl_x, edge_y), egui::pos2(sda_x, edge_y)];
            let last = &bus.children[n - 1];
            bus.scl.push(vec![
                egui::pos2(scl_x, edge_y),
                egui::pos2(scl_x, tap_y(last)),
                egui::pos2(last.left(), tap_y(last)),
            ]);
            bus.sda.push(vec![
                egui::pos2(sda_x, edge_y),
                egui::pos2(sda_x, tap_y(last)),
                egui::pos2(last.right(), tap_y(last)),
            ]);
            for c in &bus.children[..n - 1] {
                let y = tap_y(c);
                bus.scl
                    .push(vec![egui::pos2(scl_x, y), egui::pos2(c.left(), y)]);
                bus.sda
                    .push(vec![egui::pos2(sda_x, y), egui::pos2(c.right(), y)]);
                bus.junctions.push(egui::pos2(scl_x, y));
                bus.junctions.push(egui::pos2(sda_x, y));
            }
        }
        Side::Left | Side::Right => {
            let right = side == Side::Right;
            let dir = if right { 1.0 } else { -1.0 };
            let edge_x = if right { parent.right() } else { parent.left() };
            let col_left = if right {
                edge_x + GAP_SIDE
            } else {
                edge_x - GAP_SIDE - CHILD_W
            };
            for k in 0..n {
                let y0 = parent.top() + SIDE_DROP + k as f32 * (CHILD_H + GAP);
                bus.children.push(egui::Rect::from_min_size(
                    egui::pos2(col_left, y0),
                    egui::vec2(CHILD_W, CHILD_H),
                ));
            }
            // SCL taps the device edge facing the bus, SDA the far one.
            let near = |c: &egui::Rect| if right { c.left() } else { c.right() };
            let far = |c: &egui::Rect| if right { c.right() } else { c.left() };
            let scl_x = edge_x + dir * GAP_SIDE / 2.0;
            let sda_x = far(&bus.children[0]) + dir * SDA_OUT;
            let scl_y = parent.top() + SIDE_DROP;
            let sda_y = parent.top() + SDA_Y;
            bus.terminals = [egui::pos2(edge_x, scl_y), egui::pos2(edge_x, sda_y)];
            let last = &bus.children[n - 1];
            bus.scl.push(vec![
                egui::pos2(edge_x, scl_y),
                egui::pos2(scl_x, scl_y),
                egui::pos2(scl_x, tap_y(last)),
                egui::pos2(near(last), tap_y(last)),
            ]);
            // Over the top of the column, clear of the SCL rail, which only
            // starts lower down.
            bus.sda.push(vec![
                egui::pos2(edge_x, sda_y),
                egui::pos2(sda_x, sda_y),
                egui::pos2(sda_x, tap_y(last)),
                egui::pos2(far(last), tap_y(last)),
            ]);
            for c in &bus.children[..n - 1] {
                let y = tap_y(c);
                bus.scl
                    .push(vec![egui::pos2(scl_x, y), egui::pos2(near(c), y)]);
                bus.sda
                    .push(vec![egui::pos2(sda_x, y), egui::pos2(far(c), y)]);
                bus.junctions.push(egui::pos2(scl_x, y));
                bus.junctions.push(egui::pos2(sda_x, y));
            }
        }
    }
    bus
}

/// One bus to draw: which module it is, where its box went, and its devices.
pub(super) struct BusDraw {
    pub module: usize,
    pub module_id: String,
    pub instance: u8,
    /// The Device its box is in - the one its devices are in unless put in
    /// another.
    pub group: Option<String>,
    pub bus: Bus,
    /// The devices, in column order (`bus.children`'s): which device, its
    /// 1-based place in the bus's list (what its box and its panel row are
    /// numbered by), and the Device it is in.
    pub keys: Vec<I2cDeviceKey>,
    pub numbers: Vec<usize>,
    pub child_groups: Vec<Option<String>>,
}

/// What the user did on the devices this frame, applied by the caller once it
/// can take `mcu` mutably.
#[derive(Default)]
pub(super) struct BusOut {
    pub acts: Vec<(u8, I2cAct)>,
    /// A device box clicked: (its bus module's id, which device).
    pub picked: Option<(String, I2cDeviceKey)>,
}

/// Where the "+ device" button sits on a bus box: its bottom-left corner,
/// clear of the box's drag area (all but the bottom 30 px) and of the signal
/// legend (bottom-right).
pub(crate) fn add_button_rect(parent: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(parent.left() + 8.0, parent.bottom() - 24.0),
        egui::vec2(62.0, 17.0),
    )
}

/// Rects inside a device box: the name row, the ID field, the remove button.
pub(crate) fn name_rect(c: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(c.left() + 6.0, c.top() + STRIP_H + 3.0),
        egui::pos2(c.right() - 6.0, c.top() + STRIP_H + 21.0),
    )
}

pub(crate) fn id_rect(c: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(c.left() + 26.0, c.bottom() - 21.0),
        egui::pos2(c.left() + 78.0, c.bottom() - 4.0),
    )
}

pub(crate) fn remove_rect(c: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(c.right() - 24.0, c.bottom() - 21.0),
        egui::pos2(c.right() - 6.0, c.bottom() - 4.0),
    )
}

/// While a removal is armed: the [Remove] and [Cancel] buttons.
pub(crate) fn confirm_rects(c: egui::Rect) -> (egui::Rect, egui::Rect) {
    let at = |x: f32| {
        egui::Rect::from_min_size(
            egui::pos2(c.left() + x, c.bottom() - 21.0),
            egui::vec2(62.0, 17.0),
        )
    };
    (at(6.0), at(74.0))
}

/// A device's whole box, from the strip a click lands on.
#[cfg(test)]
pub(crate) fn child_of_strip(strip: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(strip.min, egui::vec2(CHILD_W, CHILD_H))
}

/// Paint every bus's devices and wires, and collect what the user did on them.
///
/// Wires go into `halos`/`lines` - the canvas's one wire slot, under every box.
/// Boxes, dots and fields are painted now, after every module box, so a device
/// is never covered by a box drawn later.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint(
    painter: &egui::Painter,
    ui: &mut egui::Ui,
    mcu: &Mcu,
    buses: &[BusDraw],
    // Every I2C bus box, for its "+ device" button: (instance, box rect).
    add_buttons: &[(u8, egui::Rect)],
    active: Option<&str>,
    // The pulse of a pending removal, 0..1.
    blink: f32,
    halos: &mut Vec<egui::Shape>,
    lines: &mut Vec<egui::Shape>,
) -> BusOut {
    let mut out = BusOut::default();
    let picked = mcu.selected_i2c_child().map(|(id, k)| (id.to_owned(), k));
    for b in buses {
        let color = signal_color(ModuleSignal::Scl, b.instance);
        let is_active =
            |g: &Option<String>| active.filter(|a| g.as_deref().map(str::trim) == Some(a.trim()));
        // A device's tap lights with ITS Device; the rails, which every device
        // hangs on, with the bus's or with any of theirs.
        let taps_lit: Vec<Option<&str>> = b.child_groups.iter().map(is_active).collect();
        let lit = is_active(&b.group).or_else(|| taps_lit.iter().flatten().next().copied());
        for paths in [&b.bus.scl, &b.bus.sda] {
            for (k, path) in paths.iter().enumerate() {
                // Path 0 is the rail (ending in the farthest device's tap);
                // path k >= 1 is the tap of device k - 1.
                let on = if k == 0 {
                    lit
                } else {
                    taps_lit.get(k - 1).copied().flatten()
                };
                let (halo, line) = wire_shapes(
                    &crate::panels::structure_map::gui::rounded_path(path, super::wire::WIRE_R),
                    color,
                    1.6,
                    on,
                );
                halos.extend(halo);
                lines.push(line);
            }
        }
        let dot = if lit.is_some() { 4.5 } else { 3.5 };
        for t in b.bus.terminals {
            painter.circle_filled(t, dot, color);
        }
        for j in &b.bus.junctions {
            painter.circle_filled(*j, dot, color);
        }
        let Some(ModuleConfig::I2c(cfg)) = mcu.modules.get(b.module).map(|m| &m.config) else {
            continue;
        };
        let rows = cfg.rows();
        let issues = cfg.address_issues();
        for (n, (rect, key)) in b.bus.children.iter().zip(&b.keys).enumerate() {
            let Some(row) = rows.iter().find(|r| r.key == *key) else {
                continue;
            };
            let issue = rows
                .iter()
                .position(|r| r.key == *key)
                .and_then(|i| issues[i]);
            // Its place in the bus's list, which the column may not follow.
            let number = b.numbers.get(n).copied().unwrap_or(n + 1);
            let selected = picked.as_ref() == Some(&(b.module_id.clone(), *key));
            let removing = mcu.i2c_remove_confirm == Some((b.instance, *key));
            let scale = text_scale(selected);
            // The box, in the bus's colour - the same dilution as its parent.
            let fill = if removing {
                let lerp = |a: u8, c: u8| (a as f32 + (c as f32 - a as f32) * blink).round() as u8;
                egui::Color32::from_rgb(lerp(38, 190), lerp(42, 45), lerp(50, 45))
            } else {
                tint(egui::Color32::from_rgb(38, 42, 50), color, 0.16)
            };
            painter.rect_filled(*rect, 4.0, fill);
            let stroke = if removing {
                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(235, 70, 70))
            } else if selected {
                egui::Stroke::new(2.8_f32, egui::Color32::WHITE)
            } else {
                egui::Stroke::new(1.2_f32, color)
            };
            painter.rect_stroke(*rect, 4.0, stroke, egui::StrokeKind::Middle);
            // The strip: which edge is SCL, which SDA, and which device.
            let strip_y = rect.top() + STRIP_H / 2.0;
            let tag = egui::FontId::monospace(8.5 * scale);
            let dim = egui::Color32::from_rgb(170, 175, 190);
            let (scl_align, sda_align, scl_x, sda_x) = match b.bus.side {
                // SCL taps the edge facing the bus on a left/right bus.
                Side::Left => (
                    egui::Align2::RIGHT_CENTER,
                    egui::Align2::LEFT_CENTER,
                    rect.right() - 5.0,
                    rect.left() + 5.0,
                ),
                _ => (
                    egui::Align2::LEFT_CENTER,
                    egui::Align2::RIGHT_CENTER,
                    rect.left() + 5.0,
                    rect.right() - 5.0,
                ),
            };
            painter.text(
                egui::pos2(scl_x, strip_y),
                scl_align,
                "SCL",
                tag.clone(),
                dim,
            );
            painter.text(
                egui::pos2(sda_x, strip_y),
                sda_align,
                "SDA",
                tag.clone(),
                dim,
            );
            painter.text(
                egui::pos2(rect.center().x, strip_y),
                egui::Align2::CENTER_CENTER,
                format!("#{}", number),
                tag,
                if selected { egui::Color32::WHITE } else { dim },
            );
            // A click anywhere on the box that is not a field picks the device.
            // The WHOLE box, registered before its fields so they win where
            // they are: a click on its bare parts would otherwise reach the
            // canvas background, which clears the selection and folds every
            // config - the bus's included. Click only - a drag still pans.
            let resp = ui
                .interact(
                    *rect,
                    ui.id().with(("i2c_child", b.instance, *key)),
                    egui::Sense::click(),
                )
                .on_hover_text("Click to select this device");
            if resp.clicked() {
                out.picked = Some((b.module_id.clone(), *key));
            }
            resp.context_menu(|ui| {
                if ui.button("Remove device").clicked() {
                    out.acts.push((b.instance, I2cAct::ArmRemove(*key)));
                    ui.close();
                }
            });

            let font = egui::FontId::proportional(10.0 * scale);
            if removing {
                painter.text(
                    name_rect(*rect).left_center(),
                    egui::Align2::LEFT_CENTER,
                    "Remove this device?",
                    font.clone(),
                    egui::Color32::WHITE,
                );
                let (yes, no) = confirm_rects(*rect);
                let question = remove_question(row.name, number, rows.len());
                ui.push_id(("i2c_child_confirm", b.instance, *key), |ui| {
                    if ui
                        .put(
                            yes,
                            egui::Button::new(egui::RichText::new("Remove").size(10.0)),
                        )
                        .on_hover_text(&question)
                        .clicked()
                    {
                        out.acts
                            .push((b.instance, I2cAct::Edit(I2cDeviceEdit::Remove(*key))));
                    }
                    if ui
                        .put(
                            no,
                            egui::Button::new(egui::RichText::new("Cancel").size(10.0)),
                        )
                        .clicked()
                    {
                        out.acts.push((b.instance, I2cAct::CancelRemove));
                    }
                });
                continue;
            }
            let name_id = ui.id().with(("i2c_child_name", b.instance, *key));
            if let Some(act) = name_field(
                ui,
                name_id,
                row,
                number,
                Some(name_rect(*rect)),
                name_rect(*rect).width(),
                Some(font.clone()),
            ) {
                out.acts.push((b.instance, act));
            }
            painter.text(
                egui::pos2(rect.left() + 8.0, id_rect(*rect).center().y),
                egui::Align2::LEFT_CENTER,
                "ID",
                egui::FontId::monospace(9.5 * scale),
                dim,
            );
            let id_id = ui.id().with(("i2c_child_id", b.instance, *key));
            if let Some(act) = id_field(
                ui,
                id_id,
                row,
                issue,
                Some(id_rect(*rect)),
                id_rect(*rect).width(),
                Some(font.clone()),
            ) {
                out.acts.push((b.instance, act));
            }
            if let Some(i) = issue {
                painter.text(
                    egui::pos2(id_rect(*rect).right() + 5.0, id_rect(*rect).center().y),
                    egui::Align2::LEFT_CENTER,
                    egui_phosphor::regular::WARNING,
                    egui::FontId::proportional(11.0 * scale),
                    super::i2c_devices::issue_color(i),
                );
            }
            ui.push_id(("i2c_child_remove", b.instance, *key), |ui| {
                if ui
                    .put(
                        remove_rect(*rect),
                        egui::Button::new(egui::RichText::new("x").size(10.0)).small(),
                    )
                    .on_hover_text("Remove this device")
                    .clicked()
                {
                    out.acts.push((b.instance, I2cAct::ArmRemove(*key)));
                }
            });
        }
    }
    for (instance, parent) in add_buttons {
        ui.push_id(("i2c_add_device", *instance), |ui| {
            if ui
                .put(
                    add_button_rect(*parent),
                    egui::Button::new(egui::RichText::new("+ device").size(10.0)).small(),
                )
                .on_hover_text(
                    "Add a device to this bus - a box beside it, with a name and its 7-bit address",
                )
                .clicked()
            {
                out.acts.push((*instance, I2cAct::Edit(I2cDeviceEdit::Add)));
            }
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent(side: Side) -> egui::Rect {
        // A 170 x 98 bus box on `side` of a chip centred on the origin.
        let c = match side {
            Side::Top => egui::pos2(0.0, -300.0),
            Side::Bottom => egui::pos2(0.0, 300.0),
            Side::Left => egui::pos2(-300.0, 0.0),
            Side::Right => egui::pos2(300.0, 0.0),
        };
        egui::Rect::from_center_size(c, egui::vec2(170.0, 98.0))
    }

    const SIDES: [Side; 4] = [Side::Top, Side::Bottom, Side::Left, Side::Right];

    fn segments(paths: &[Vec<egui::Pos2>]) -> Vec<(egui::Pos2, egui::Pos2)> {
        paths
            .iter()
            .flat_map(|p| p.windows(2).map(|w| (w[0], w[1])))
            .collect()
    }

    /// Two axis-aligned segments touch or cross (bounding boxes overlap).
    fn touch(a: (egui::Pos2, egui::Pos2), b: (egui::Pos2, egui::Pos2)) -> bool {
        let ra = egui::Rect::from_two_pos(a.0, a.1).expand(0.01);
        let rb = egui::Rect::from_two_pos(b.0, b.1).expand(0.01);
        ra.intersects(rb)
    }

    /// A segment runs through the INSIDE of `r` (its edges do not count).
    fn enters(s: (egui::Pos2, egui::Pos2), r: egui::Rect) -> bool {
        egui::Rect::from_two_pos(s.0, s.1).intersects(r.shrink(0.5))
    }

    /// The whole point of the layout: SCL and SDA are one colour, so they may
    /// not cross or touch anywhere - on any side, for any number of devices.
    #[test]
    fn scl_and_sda_never_cross() {
        for side in SIDES {
            for n in 1..=6 {
                let bus = layout(parent(side), side, n);
                for a in segments(&bus.scl) {
                    for b in segments(&bus.sda) {
                        assert!(!touch(a, b), "{side:?} x{n}: {a:?} meets {b:?}");
                    }
                }
            }
        }
    }

    /// No wire runs through a device box or the bus box - only to their edges.
    #[test]
    fn wires_go_to_the_boxes_not_through_them() {
        for side in SIDES {
            for n in 1..=6 {
                let p = parent(side);
                let bus = layout(p, side, n);
                for s in segments(&bus.scl).into_iter().chain(segments(&bus.sda)) {
                    assert!(!enters(s, p), "{side:?} x{n}: {s:?} crosses the bus box");
                    for c in &bus.children {
                        assert!(!enters(s, *c), "{side:?} x{n}: {s:?} crosses {c:?}");
                    }
                }
            }
        }
    }

    /// Every device gets one SCL tap and one SDA tap, each ending on its edge,
    /// on opposite sides of it.
    #[test]
    fn every_device_is_on_both_lines() {
        for side in SIDES {
            for n in 1..=6 {
                let bus = layout(parent(side), side, n);
                for c in &bus.children {
                    let ends = |paths: &[Vec<egui::Pos2>]| {
                        paths
                            .iter()
                            .filter(|p| {
                                let e = *p.last().unwrap();
                                (e.x - c.left()).abs() < 0.01 || (e.x - c.right()).abs() < 0.01
                            })
                            .filter(|p| {
                                let e = *p.last().unwrap();
                                e.y > c.top() && e.y < c.bottom()
                            })
                            .map(|p| *p.last().unwrap())
                            .collect::<Vec<_>>()
                    };
                    let scl = ends(&bus.scl);
                    let sda = ends(&bus.sda);
                    assert_eq!(scl.len(), 1, "{side:?} x{n}: SCL taps {scl:?}");
                    assert_eq!(sda.len(), 1, "{side:?} x{n}: SDA taps {sda:?}");
                    assert!((scl[0].x - sda[0].x).abs() > 100.0, "{side:?}: same edge");
                }
            }
        }
    }

    /// Devices overlap neither each other nor the bus box, and sit on the side
    /// facing AWAY from the chip.
    #[test]
    fn devices_stand_clear_and_outward() {
        for side in SIDES {
            for n in 1..=6 {
                let p = parent(side);
                let bus = layout(p, side, n);
                assert_eq!(bus.children.len(), n);
                for (i, a) in bus.children.iter().enumerate() {
                    assert!(!a.intersects(p), "{side:?}: device over the bus box");
                    for b in &bus.children[i + 1..] {
                        assert!(!a.intersects(*b), "{side:?}: devices overlap");
                    }
                    let out = a.center() - p.center();
                    let away = match side {
                        Side::Top => out.y < 0.0,
                        Side::Bottom => out.y > 0.0,
                        Side::Left => out.x < 0.0,
                        Side::Right => out.x > 0.0,
                    };
                    assert!(away, "{side:?}: a device on the chip's side of its bus");
                }
            }
        }
    }

    /// The gaps stay inside the device mats' merge distance, so a Device holding
    /// the bus draws one mat around it and its devices, not two.
    #[test]
    fn the_gaps_stay_inside_a_device_mat() {
        const { assert!(GAP <= super::super::device_frame::JOIN) };
        const { assert!(GAP_SIDE <= super::super::device_frame::JOIN) };
        const { assert!(SIDE_DROP > SDA_Y + 4.0) };
        // The second device of a left/right column starts below a 98 px bus
        // box (see `SIDE_DROP`).
        const { assert!(SIDE_DROP + CHILD_H + GAP > 98.0) };
    }

    /// A left/right bus reports how far its column runs past its box, so the
    /// packer can push the next box on that side clear of it.
    #[test]
    fn the_tail_is_how_far_the_column_runs_past_the_box() {
        for side in [Side::Left, Side::Right] {
            for n in 0..=6 {
                let p = parent(side);
                let bus = layout(p, side, n);
                let bottom = bus
                    .children
                    .iter()
                    .map(|c| c.bottom())
                    .fold(p.bottom(), f32::max);
                assert!((bottom - p.bottom() - tail(side, p.height(), n)).abs() < 0.01);
            }
        }
        assert_eq!(
            tail(Side::Top, 98.0, 5),
            0.0,
            "above the chip the column goes outward"
        );
    }

    /// Junction dots sit only where a rail goes on past a tap: none for one
    /// device, two per device after the first.
    #[test]
    fn a_dot_marks_every_t_and_nothing_else() {
        for side in SIDES {
            for n in 1..=5 {
                assert_eq!(layout(parent(side), side, n).junctions.len(), 2 * (n - 1));
            }
        }
    }

    /// A bus in one Device with some devices in another: the bus and its own
    /// devices draw ONE mat, and so do the others - on every side, for the
    /// mixes a user makes. The layout puts the bus's own first; the second
    /// device of a left/right column starts below the bus box, or the mat of
    /// the bus and its first device would reach the foreign one and be refused.
    #[test]
    fn a_bus_and_its_own_devices_draw_one_mat_beside_another_device() {
        use super::super::device_frame::cluster;
        for side in SIDES {
            for (own, other) in [(1, 1), (2, 1), (1, 2), (3, 2)] {
                let p = parent(side);
                let bus = layout(p, side, own + other);
                let mut mine = vec![p];
                mine.extend(&bus.children[..own]);
                let theirs = &bus.children[own..];
                assert_eq!(
                    cluster(&mine, theirs).len(),
                    1,
                    "{side:?} {own}+{other}: the bus's Device split"
                );
                assert_eq!(
                    cluster(theirs, &mine).len(),
                    1,
                    "{side:?} {own}+{other}: the other Device split"
                );
            }
        }
    }
}
