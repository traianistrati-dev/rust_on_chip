//! The devices on an I2C bus, drawn on the Pins canvas as boxes of their own,
//! each joined to the bus's box by one line.
//!
//! # Layout
//!
//! A device the user has not moved sits in a COLUMN on the side of the bus box
//! that faces away from the chip - never between the box and the pins, where
//! the wire corridor is clear only by construction (`wire.rs`).
//!
//! * A bus above or below the chip stacks its devices further out, inside its
//!   own 170 px band, so two buses on that side can never reach into each
//!   other's column.
//! * A bus left or right of the chip puts the column beside it, starting a
//!   little below its top edge. The packer is told how far the column runs past
//!   the box (`tail`), so the next bus on that side is pushed clear of it.
//!
//! A device the user DRAGGED sits where it was put (`Mcu::i2c_child_pos`), like
//! a dragged module box: it stays there when its bus moves, and moves with its
//! module group.
//!
//! # Wires
//!
//! ONE line per device, the bus as a schematic block diagram draws it - SCL and
//! SDA are always a pair, and two lines per device said nothing a single one
//! does not. The column shares a rail with a tap into each device; a moved
//! device gets a line of its own, straight or with two bends.
//!
//! The column's gaps are kept under `device_frame::JOIN`, so a module group
//! holding the bus and a device merges them into one mat.

use super::i2c_devices::{I2cAct, id_field, name_field, remove_question};
use super::modules::{Side, signal_color, text_scale, tint, wire_shapes};
use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::modules::{I2cDeviceEdit, I2cDeviceKey, ModuleConfig, ModuleSignal};
use eframe::egui;

/// A device box.
pub const CHILD_W: f32 = 150.0;
pub const CHILD_H: f32 = 60.0;
/// The strip at the top of a device box: its number, and where the tap lands.
const STRIP_H: f32 = 14.0;
/// Between a bus box above/below the chip and its first device, and between
/// two devices of one bus.
pub const GAP: f32 = 8.0;
/// Between a bus box left/right of the chip and its column - the rail runs
/// down the middle of it.
pub const GAP_SIDE: f32 = 20.0;
/// How far below a left/right bus box's top its column starts - far enough that
/// the SECOND device starts below the bus box. A mat merging the bus box with
/// its first device covers the box's whole height; with a device of another
/// group beside that span, the merge would be refused and the bus's group drawn
/// in two pieces.
const SIDE_DROP: f32 = 32.0;
/// How far outside a column the rail of an above/below bus runs - inside the
/// bus's 170 px band, which the 150 px column leaves 10 px of on each side.
const RAIL_OUT: f32 = 5.0;
/// How far a moved device's line keeps its ends from box corners.
const END_INSET: f32 = 10.0;

/// One line from the bus box to its device(s).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Wire {
    /// From the bus box end to the device end.
    pub path: Vec<egui::Pos2>,
    /// The device it leads to alone - `None` for the column's rail, which
    /// every device of the column hangs on.
    pub serves: Option<usize>,
}

/// One bus's devices, placed, with their wires.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Bus {
    pub side: Side,
    /// One per device: the column's first (nearest the bus box first), then the
    /// moved ones in the order given.
    pub children: Vec<egui::Rect>,
    pub wires: Vec<Wire>,
    /// Where a tap leaves a rail that goes on past it - drawn as a dot, the
    /// schematic sign that two lines are joined.
    pub junctions: Vec<egui::Pos2>,
    /// Where lines leave the bus box, and where they reach a device.
    pub terminals: Vec<egui::Pos2>,
    pub ends: Vec<egui::Pos2>,
}

impl Bus {
    /// Everything the bus's devices and wires cover.
    pub fn bounds(&self) -> egui::Rect {
        let mut r = egui::Rect::NOTHING;
        for c in &self.children {
            r = r.union(*c);
        }
        for p in self.wires.iter().flat_map(|w| w.path.iter()) {
            r.extend_with(*p);
        }
        r
    }
}

/// The length of a column of `n` devices.
fn column_len(n: usize) -> f32 {
    n as f32 * CHILD_H + n.saturating_sub(1) as f32 * GAP
}

/// How far a bus box's column runs past its far end along its side of the
/// chip: what the packer adds before placing the next box on that side. Only a
/// left/right bus has any; above or below, the column goes outward instead.
/// `n` counts the devices IN the column - a moved one is not.
pub(super) fn tail(side: Side, box_h: f32, n: usize) -> f32 {
    match side {
        Side::Left | Side::Right if n > 0 => (SIDE_DROP + column_len(n) - box_h).max(0.0),
        _ => 0.0,
    }
}

/// How many of a module's devices sit in its column: its I2C devices the user
/// has not moved, none for anything else.
pub(super) fn column_count(
    mcu: &Mcu,
    m: &crate::panels::mcu_module::modules::VirtualModule,
) -> usize {
    match &m.config {
        ModuleConfig::I2c(c) => c
            .rows()
            .iter()
            .filter(|r| moved_to(mcu, m.instance(), r.key).is_none())
            .count(),
        _ => 0,
    }
}

/// Where the user put device `key` of bus `instance`, as the offset of its
/// box's top-left from the chip centre - `None` while it sits in the column.
pub(super) fn moved_to(mcu: &Mcu, instance: u8, key: I2cDeviceKey) -> Option<(f32, f32)> {
    match key {
        I2cDeviceKey::Uid(u) => mcu.i2c_child_pos.get(&(instance, u)).copied(),
        _ => None,
    }
}

/// Place `n` devices in the column beside bus box `parent`, which sits on
/// `side` of the chip, then the `moved` ones where they were put - and a line
/// to each.
pub(super) fn layout(parent: egui::Rect, side: Side, n: usize, moved: &[egui::Rect]) -> Bus {
    let mut bus = Bus {
        side,
        children: Vec::new(),
        wires: Vec::new(),
        junctions: Vec::new(),
        terminals: Vec::new(),
        ends: Vec::new(),
    };
    let tap_y = |c: &egui::Rect| c.top() + STRIP_H / 2.0;
    if n > 0 {
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
                let rail_x = left - RAIL_OUT;
                let last = bus.children[n - 1];
                bus.terminals.push(egui::pos2(rail_x, edge_y));
                bus.wires.push(Wire {
                    path: vec![
                        egui::pos2(rail_x, edge_y),
                        egui::pos2(rail_x, tap_y(&last)),
                        egui::pos2(last.left(), tap_y(&last)),
                    ],
                    serves: None,
                });
                bus.ends.push(egui::pos2(last.left(), tap_y(&last)));
                for (k, c) in bus.children[..n - 1].iter().enumerate() {
                    let y = tap_y(c);
                    bus.wires.push(Wire {
                        path: vec![egui::pos2(rail_x, y), egui::pos2(c.left(), y)],
                        serves: Some(k),
                    });
                    bus.junctions.push(egui::pos2(rail_x, y));
                    bus.ends.push(egui::pos2(c.left(), y));
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
                // The tap reaches the device edge facing the bus.
                let near = |c: &egui::Rect| if right { c.left() } else { c.right() };
                let rail_x = edge_x + dir * GAP_SIDE / 2.0;
                let feed_y = parent.top() + SIDE_DROP;
                let last = bus.children[n - 1];
                bus.terminals.push(egui::pos2(edge_x, feed_y));
                bus.wires.push(Wire {
                    path: vec![
                        egui::pos2(edge_x, feed_y),
                        egui::pos2(rail_x, feed_y),
                        egui::pos2(rail_x, tap_y(&last)),
                        egui::pos2(near(&last), tap_y(&last)),
                    ],
                    serves: None,
                });
                bus.ends.push(egui::pos2(near(&last), tap_y(&last)));
                for (k, c) in bus.children[..n - 1].iter().enumerate() {
                    let y = tap_y(c);
                    bus.wires.push(Wire {
                        path: vec![egui::pos2(rail_x, y), egui::pos2(near(c), y)],
                        serves: Some(k),
                    });
                    bus.junctions.push(egui::pos2(rail_x, y));
                    bus.ends.push(egui::pos2(near(c), y));
                }
            }
        }
    }
    for (k, c) in moved.iter().enumerate() {
        bus.children.push(*c);
        let path = link(parent, *c);
        if let (Some(a), Some(b)) = (path.first(), path.last()) {
            bus.terminals.push(*a);
            bus.ends.push(*b);
        }
        bus.wires.push(Wire {
            path,
            serves: Some(n + k),
        });
    }
    bus
}

/// The line from bus box `bus` to a device box the user moved: out of the bus
/// box's edge that faces the device, into the device's edge that faces the
/// bus - straight where the two overlap across that gap, else with two bends
/// half way. Empty when the boxes overlap and no edge faces the other.
pub(super) fn link(bus: egui::Rect, dev: egui::Rect) -> Vec<egui::Pos2> {
    let d = dev.center() - bus.center();
    let gap_x = if d.x >= 0.0 {
        dev.left() - bus.right()
    } else {
        bus.left() - dev.right()
    };
    let gap_y = if d.y >= 0.0 {
        dev.top() - bus.bottom()
    } else {
        bus.top() - dev.bottom()
    };
    // Along a range, kept off its ends.
    let inside =
        |v: f32, lo: f32, hi: f32| v.clamp(lo + END_INSET, (hi - END_INSET).max(lo + END_INSET));
    let horizontal = gap_x > 0.0 && (gap_y <= 0.0 || d.x.abs() >= d.y.abs());
    if horizontal {
        let (x0, x1) = if d.x >= 0.0 {
            (bus.right(), dev.left())
        } else {
            (bus.left(), dev.right())
        };
        // Straight across where both boxes span the same heights.
        let lo = bus.top().max(dev.top()) + END_INSET;
        let hi = bus.bottom().min(dev.bottom()) - END_INSET;
        if lo <= hi {
            let y = (lo + hi) / 2.0;
            return vec![egui::pos2(x0, y), egui::pos2(x1, y)];
        }
        let y0 = inside(dev.center().y, bus.top(), bus.bottom());
        let y1 = inside(bus.center().y, dev.top(), dev.bottom());
        let mid = (x0 + x1) / 2.0;
        vec![
            egui::pos2(x0, y0),
            egui::pos2(mid, y0),
            egui::pos2(mid, y1),
            egui::pos2(x1, y1),
        ]
    } else if gap_y > 0.0 {
        let (y0, y1) = if d.y >= 0.0 {
            (bus.bottom(), dev.top())
        } else {
            (bus.top(), dev.bottom())
        };
        let lo = bus.left().max(dev.left()) + END_INSET;
        let hi = bus.right().min(dev.right()) - END_INSET;
        if lo <= hi {
            let x = (lo + hi) / 2.0;
            return vec![egui::pos2(x, y0), egui::pos2(x, y1)];
        }
        let x0 = inside(dev.center().x, bus.left(), bus.right());
        let x1 = inside(bus.center().x, dev.left(), dev.right());
        let mid = (y0 + y1) / 2.0;
        vec![
            egui::pos2(x0, y0),
            egui::pos2(x0, mid),
            egui::pos2(x1, mid),
            egui::pos2(x1, y1),
        ]
    } else {
        Vec::new()
    }
}

/// One bus to draw: which module it is, where its box went, and its devices.
pub(super) struct BusDraw {
    pub module: usize,
    pub module_id: String,
    pub instance: u8,
    /// The module group its box is in.
    pub group: Option<String>,
    pub bus: Bus,
    /// The devices, in `bus.children`'s order: which device, its 1-based place
    /// in the bus's list (what its box and its panel row are numbered by), the
    /// module group it is in, and whether the user moved it out of the column.
    pub keys: Vec<I2cDeviceKey>,
    pub numbers: Vec<usize>,
    pub child_groups: Vec<Option<String>>,
    pub moved: Vec<bool>,
}

/// What the user did on the devices this frame, applied by the caller once it
/// can take `mcu` mutably.
#[derive(Default)]
pub(super) struct BusOut {
    pub acts: Vec<(u8, I2cAct)>,
    /// A device box clicked: (its bus module's id, which device).
    pub picked: Option<(String, I2cDeviceKey)>,
    /// Device boxes dragged: (bus instance, which device, the new offset of
    /// its top-left from the chip centre).
    pub moves: Vec<(u8, I2cDeviceKey, (f32, f32))>,
    /// "Reset to auto position": back into the column.
    pub resets: Vec<(u8, I2cDeviceKey)>,
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
    // What a dragged box's position is stored relative to.
    chip_center: egui::Pos2,
    halos: &mut Vec<egui::Shape>,
    lines: &mut Vec<egui::Shape>,
) -> BusOut {
    let mut out = BusOut::default();
    let picked = mcu.selected_i2c_child().map(|(id, k)| (id.to_owned(), k));
    // What "Put in Module Group" offers: every group the roster holds by a
    // name, live or just created and still empty.
    let mut devices: Vec<String> = Vec::new();
    for g in &mcu.groups {
        let name = g.name.trim();
        if !name.is_empty() && !devices.iter().any(|d| d == name) {
            devices.push(name.to_owned());
        }
    }
    for b in buses {
        let color = signal_color(ModuleSignal::Scl, b.instance);
        let is_active =
            |g: &Option<String>| active.filter(|a| g.as_deref().map(str::trim) == Some(a.trim()));
        // A device's own line lights with ITS group; the column's rail, which
        // every device of the column hangs on, with the bus's or any of theirs.
        let taps_lit: Vec<Option<&str>> = b.child_groups.iter().map(is_active).collect();
        let column_lit = b
            .moved
            .iter()
            .zip(&taps_lit)
            .filter(|(moved, _)| !**moved)
            .find_map(|(_, lit)| *lit);
        let lit = is_active(&b.group).or(column_lit);
        for w in &b.bus.wires {
            let on = match w.serves {
                None => lit,
                Some(k) => taps_lit.get(k).copied().flatten(),
            };
            let (halo, line) = wire_shapes(
                &crate::panels::structure_map::gui::rounded_path(&w.path, super::wire::WIRE_R),
                color,
                1.6,
                on,
            );
            halos.extend(halo);
            lines.push(line);
        }
        let dot = if lit.is_some() { 4.5 } else { 3.5 };
        for p in b
            .bus
            .terminals
            .iter()
            .chain(&b.bus.junctions)
            .chain(&b.bus.ends)
        {
            painter.circle_filled(*p, dot, color);
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
            // The strip: which device it is.
            let strip_y = rect.top() + STRIP_H / 2.0;
            let tag = egui::FontId::monospace(8.5 * scale);
            let dim = egui::Color32::from_rgb(170, 175, 190);
            painter.text(
                egui::pos2(rect.center().x, strip_y),
                egui::Align2::CENTER_CENTER,
                format!("#{}", number),
                tag,
                if selected { egui::Color32::WHITE } else { dim },
            );
            // A click anywhere on the box that is not a field picks the device,
            // and a drag from there moves it. The WHOLE box, registered before
            // its fields so they win where they are: a click on its bare parts
            // would otherwise reach the canvas background, which clears the
            // selection and folds every config - the bus's included.
            let moved = b.moved.get(n).copied().unwrap_or(false);
            let resp = ui
                .interact(
                    *rect,
                    ui.id().with(("i2c_child", b.instance, *key)),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::Grab)
                .on_hover_text("Click to select this device - drag to move it");
            if resp.clicked() {
                out.picked = Some((b.module_id.clone(), *key));
            }
            // egui 0.36 starts a drag the moment the pointer leaves the widget,
            // so a click whose hand slipped would pin the box where it stood.
            if resp.dragged() && crate::panels::drag_decided(ui) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                // Already in canvas coordinates - the Scene's transform is
                // applied by egui - so right at any zoom.
                let off = rect.min + resp.drag_delta() - chip_center;
                out.moves.push((b.instance, *key, (off.x, off.y)));
            }
            let mine = b.child_groups.get(n).cloned().flatten();
            resp.context_menu(|ui| {
                // Every device of a bus is grouped on its own, straight from
                // its box: the groups the roster holds, or none.
                ui.menu_button("Put in Module Group", |ui| {
                    for name in &devices {
                        let on = mine.as_deref() == Some(name.as_str());
                        if ui.selectable_label(on, name).clicked() {
                            if !on {
                                out.acts
                                    .push((b.instance, I2cAct::Group(*key, name.clone())));
                            }
                            ui.close();
                        }
                    }
                    if devices.is_empty() {
                        ui.label(
                            egui::RichText::new(
                                "No module groups yet - make one with + Module Group",
                            )
                            .size(10.0)
                            .color(egui::Color32::from_gray(130)),
                        );
                    }
                    ui.separator();
                    if ui
                        .selectable_label(mine.is_none(), "No Module Group")
                        .clicked()
                    {
                        if mine.is_some() {
                            out.acts
                                .push((b.instance, I2cAct::Group(*key, String::new())));
                        }
                        ui.close();
                    }
                });
                if moved && ui.button("Reset to auto position").clicked() {
                    out.resets.push((b.instance, *key));
                    ui.close();
                }
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

    /// A segment runs through the INSIDE of `r` (its edges do not count).
    fn enters(s: (egui::Pos2, egui::Pos2), r: egui::Rect) -> bool {
        egui::Rect::from_two_pos(s.0, s.1).intersects(r.shrink(0.5))
    }

    fn on_edge(p: egui::Pos2, r: egui::Rect) -> bool {
        let x_edge = (p.x - r.left()).abs() < 0.01 || (p.x - r.right()).abs() < 0.01;
        let y_edge = (p.y - r.top()).abs() < 0.01 || (p.y - r.bottom()).abs() < 0.01;
        (x_edge && p.y >= r.top() && p.y <= r.bottom())
            || (y_edge && p.x >= r.left() && p.x <= r.right())
    }

    /// Moved devices, somewhere around the bus box.
    fn moved(p: egui::Rect) -> Vec<egui::Rect> {
        [(420.0, -40.0), (-380.0, 260.0), (10.0, -330.0)]
            .iter()
            .map(|(dx, dy)| {
                egui::Rect::from_min_size(
                    p.center() + egui::vec2(*dx, *dy),
                    egui::vec2(CHILD_W, CHILD_H),
                )
            })
            .collect()
    }

    /// No line of the column runs through a device box or the bus box - only to
    /// their edges. A moved device's line keeps out of the bus box and its own
    /// box; where the user put the device, it may have to pass others.
    #[test]
    fn wires_go_to_the_boxes_not_through_them() {
        for side in SIDES {
            for n in 0..=5 {
                let p = parent(side);
                let bus = layout(p, side, n, &moved(p));
                for w in &bus.wires {
                    let column = w.serves.is_none_or(|k| k < n);
                    for s in w.path.windows(2).map(|q| (q[0], q[1])) {
                        assert!(!enters(s, p), "{side:?} x{n}: {s:?} crosses the bus box");
                        for (k, c) in bus.children.iter().enumerate() {
                            if column || w.serves == Some(k) {
                                assert!(!enters(s, *c), "{side:?} x{n}: {s:?} crosses {c:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// ONE line reaches every device - column or moved - and it ends on the
    /// device's edge; every line starts at the bus box or on the rail.
    #[test]
    fn every_device_is_reached_by_one_line() {
        for side in SIDES {
            for n in 0..=5 {
                let p = parent(side);
                let away = moved(p);
                let bus = layout(p, side, n, &away);
                assert_eq!(bus.children.len(), n + away.len());
                for (k, c) in bus.children.iter().enumerate() {
                    let reaching: Vec<&Wire> = bus
                        .wires
                        .iter()
                        .filter(|w| w.path.last().is_some_and(|e| on_edge(*e, *c)))
                        .collect();
                    assert_eq!(
                        reaching.len(),
                        1,
                        "{side:?} x{n}: device {k} has {reaching:?}"
                    );
                    let serves = reaching[0].serves;
                    // The column's last device hangs on the rail itself.
                    assert!(
                        serves == Some(k) || (serves.is_none() && k + 1 == n),
                        "{side:?} x{n}: device {k} served by {serves:?}"
                    );
                }
                for w in bus
                    .wires
                    .iter()
                    .filter(|w| w.serves.is_none() || w.serves >= Some(n))
                {
                    assert!(
                        on_edge(w.path[0], p),
                        "{side:?}: {w:?} does not leave the bus box"
                    );
                }
                assert_eq!(
                    bus.ends.len(),
                    n + away.len(),
                    "a dot where each line arrives"
                );
            }
        }
    }

    /// The column stands clear of the bus box and faces AWAY from the chip.
    #[test]
    fn the_column_stands_clear_and_outward() {
        for side in SIDES {
            for n in 1..=6 {
                let p = parent(side);
                let bus = layout(p, side, n, &[]);
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

    /// A moved device's line: out of the bus box's edge facing it, into its
    /// own edge facing the bus - straight where they overlap, square bends
    /// otherwise; none for boxes that overlap.
    #[test]
    fn a_moved_devices_line_runs_edge_to_edge_at_right_angles() {
        let bus = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(170.0, 98.0));
        let at = |x: f32, y: f32| {
            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(CHILD_W, CHILD_H))
        };
        let straight = link(bus, at(300.0, 20.0));
        assert_eq!(straight.len(), 2, "{straight:?}");
        for dev in [
            at(300.0, 200.0),
            at(-400.0, -150.0),
            at(20.0, 300.0),
            at(-200.0, 250.0),
        ] {
            let path = link(bus, dev);
            assert!(path.len() >= 2, "{dev:?}: {path:?}");
            assert!(on_edge(path[0], bus), "{dev:?}: {path:?}");
            assert!(on_edge(*path.last().unwrap(), dev), "{dev:?}: {path:?}");
            for w in path.windows(2) {
                assert!(
                    w[0].x == w[1].x || w[0].y == w[1].y,
                    "a slanted leg: {path:?}"
                );
                assert!(
                    !enters((w[0], w[1]), bus) && !enters((w[0], w[1]), dev),
                    "{path:?}"
                );
            }
        }
        assert!(
            link(bus, at(50.0, 30.0)).is_empty(),
            "overlapping boxes get no line"
        );
    }

    /// The gaps stay inside the mats' merge distance, so a module group holding
    /// the bus and a device of its column draws one mat around them, not two.
    #[test]
    fn the_gaps_stay_inside_a_group_mat() {
        const { assert!(GAP <= super::super::device_frame::JOIN) };
        const { assert!(GAP_SIDE <= super::super::device_frame::JOIN) };
        // The second device of a left/right column starts below a 98 px bus
        // box (see `SIDE_DROP`).
        const { assert!(SIDE_DROP + CHILD_H + GAP > 98.0) };
    }

    /// A left/right bus reports how far its column runs past its box, so the
    /// packer can push the next box on that side clear of it - the column only:
    /// a moved device is no part of it.
    #[test]
    fn the_tail_is_how_far_the_column_runs_past_the_box() {
        for side in [Side::Left, Side::Right] {
            for n in 0..=6 {
                let p = parent(side);
                let bus = layout(p, side, n, &[]);
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

    /// Junction dots sit only where the rail goes on past a tap: none for one
    /// device, one per device after the first. A moved device's line joins
    /// nothing.
    #[test]
    fn a_dot_marks_every_t_and_nothing_else() {
        for side in SIDES {
            for n in 1..=5 {
                let p = parent(side);
                assert_eq!(layout(p, side, n, &moved(p)).junctions.len(), n - 1);
            }
        }
    }

    /// A bus in one group with some devices in another: the bus and its own
    /// devices draw ONE mat, and so do the others - on every side, for the
    /// mixes a user makes.
    #[test]
    fn a_bus_and_its_own_devices_draw_one_mat_beside_another_group() {
        use super::super::device_frame::cluster;
        for side in SIDES {
            for (own, other) in [(1, 1), (2, 1), (1, 2), (3, 2)] {
                let p = parent(side);
                let bus = layout(p, side, own + other, &[]);
                let mut mine = vec![p];
                mine.extend(&bus.children[..own]);
                let theirs = &bus.children[own..];
                assert_eq!(
                    cluster(&mine, theirs).len(),
                    1,
                    "{side:?} {own}+{other}: the bus's group split"
                );
                assert_eq!(
                    cluster(theirs, &mine).len(),
                    1,
                    "{side:?} {own}+{other}: the other group split"
                );
            }
        }
    }
}
