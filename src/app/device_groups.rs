//! The device roster: the list at the top of the Virtual-modules panel where a
//! sensor's pads are gathered under one name.
//!
//! A board part is rarely one peripheral. A radar module is a UART pair and a
//! spare interrupt line; a display is a SPI bus, a D/C pin and a reset pin. The
//! diagram could draw each of those, and the generated file could bind each of
//! them, but neither could say the three belong to ONE thing — so the reader had
//! to hold the datasheet beside the picture to know which input line went with
//! which bus.
//!
//! A group claims nothing. It takes no pad away from a peripheral, changes no
//! binding and no init call; the only thing it reaches in the generated project
//! is one comment (`codegen::common::device_comment`). That is deliberate: the
//! moment a group renamed an identifier, renaming a device in this panel would
//! rewrite names in code the user had already written against.
//!
//! Membership is by PAD NUMBER, never by module id — see
//! [`PinGroup`](crate::panels::mcu_module::mcu_config::PinGroup) for why - plus
//! the devices of an I2C bus, which have no pad of their own, by their uid. A
//! device of a bus is in its bus's Device until it is put in another. This
//! panel is the only place a group is created, renamed, filled or dissolved.
//!
//! The roster draws the rows; the `Devices:` label and the `+ Device` button
//! that creates one live on the panel's TOP BAR, beside `+ Add module` — see
//! [`device_add_button`]. Both are in this file, and `apply_act` is still the
//! single place a device is created.

use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::mcu::gui::modules as mod_gui;
use egui_phosphor::regular as ph;

/// What the roster's controls asked for, applied after the UI is drawn.
///
/// Deferred rather than immediate for the usual reason: every row is drawn
/// while the group list is borrowed, and adding a pad can delete a group —
/// which would move the rows still to be drawn.
enum Act {
    /// Start a new, empty device.
    New,
    /// Take the whole device apart (its pads keep their functions).
    Dissolve(usize),
    /// Put every pad of this module in the device.
    AddModule(usize, String),
    /// Put one pad in the device.
    AddPin(usize, usize),
    /// Take one pad out of whatever device holds it.
    Drop(usize),
    /// Put one device of an I2C bus in the device: (row, bus instance, which).
    AddI2c(usize, u8, crate::panels::mcu_module::modules::I2cDeviceKey),
    /// Take a device of an I2C bus out of the device it was put in, back to
    /// its bus's: (bus instance, uid).
    DropI2c(u8, u32),
}

/// One device of an I2C bus, as the roster offers and lists it.
struct BusDevice {
    instance: u8,
    key: crate::panels::mcu_module::modules::I2cDeviceKey,
    /// `I2C1 · oled (0x3C)`.
    label: String,
    color: egui::Color32,
    /// The uid a Device holds it by, once it has one.
    uid: Option<u32>,
    /// Put in a Device by hand - else it is in its bus's.
    explicit: bool,
    /// The Device it is in, either way (trimmed).
    group: Option<String>,
}

/// Every device on every I2C bus of the chip.
fn bus_devices(mcu: &Mcu) -> Vec<BusDevice> {
    use crate::panels::mcu_module::modules::{I2cDeviceKey, ModuleConfig};
    let mut out = Vec::new();
    for m in &mcu.modules {
        let ModuleConfig::I2c(cfg) = &m.config else {
            continue;
        };
        let inst = m.instance();
        for (n, row) in cfg.rows().iter().enumerate() {
            let name = if row.name.trim().is_empty() {
                format!("device {}", n + 1)
            } else {
                row.name.trim().to_owned()
            };
            let uid = match row.key {
                I2cDeviceKey::Uid(u) => Some(u),
                _ => None,
            };
            out.push(BusDevice {
                instance: inst,
                key: row.key,
                label: format!(
                    "{} · {name} (0x{:02X})",
                    mod_gui::module_base_name(m),
                    row.address
                ),
                color: mod_gui::module_color(m.kind, inst),
                uid,
                explicit: uid.is_some_and(|u| {
                    mcu.groups
                        .iter()
                        .any(|g| g.is_live() && g.i2c.contains(&(inst, u)))
                }),
                group: mcu
                    .group_of_i2c_device(inst, row.key)
                    .map(|g| g.name.trim().to_owned()),
            });
        }
    }
    out
}

/// A pad that can be added to a device on its own — one the user configured by
/// hand, not one a virtual module already owns.
///
/// Module pads are offered through their MODULE instead: "add the UART" is the
/// gesture, and picking its two pads one at a time only invites half a bus in a
/// device.
fn loose_pins(mcu: &Mcu) -> Vec<(usize, String)> {
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
    let claimed: std::collections::HashSet<usize> = mcu
        .modules
        .iter()
        .flat_map(|m| m.connections.iter().map(|c| c.mcu_pin))
        .collect();
    mcu.iter_all_pins()
        .filter(|p| {
            !p.reserved && p.selected_function != PinFunction::Unset && !claimed.contains(&p.number)
        })
        .map(|p| {
            (
                p.number,
                format!("{} — {}", p.name, p.selected_function.short_label()),
            )
        })
        .collect()
}

/// How one pad reads inside a device's row: its name, and what it carries.
fn member_label(mcu: &Mcu, pin: usize) -> String {
    let Some(p) = mcu.find_pin(pin) else {
        return format!("#{pin}");
    };
    match mcu
        .modules
        .iter()
        .find(|m| m.connections.iter().any(|c| c.mcu_pin == pin))
    {
        Some(m) => format!("{} — {}", p.name, mod_gui::module_title(m)),
        None => format!("{} — {}", p.name, p.selected_function.short_label()),
    }
}

/// Whether a roster row names a device yet.
fn named(name: &String) -> bool {
    !name.trim().is_empty()
}

/// A device name nothing else on the board answers to.
fn fresh_name(mcu: &Mcu) -> String {
    (1..)
        .map(|n| format!("Device {n}"))
        .find(|c| !mcu.groups.iter().any(|g| g.name.trim() == c.as_str()))
        .unwrap_or_else(|| "Device".to_owned())
}

/// What the roster's "remove?" question should hold after this frame.
///
/// Four ways it ends, and only the first adds one:
///
/// * a trash button ARMED a row — that wins outright, so arming a second row
///   replaces the first rather than leaving two rows asking;
/// * Cancel;
/// * an ACT that MOVED something — Remove took the row away, and every other
///   act the roster carries changed the thing the question was about. Creating
///   a device is deliberately NOT one of them: it pushes at the END, so it
///   renames nothing and shifts no row, and the question goes on pointing at
///   the same still-open row. (It could not reach here anyway — `+ Device`
///   lives on the panel's top bar and applies its own act, so the roster's
///   `act` is `None` on a creation frame. The rule and the plumbing agree, but
///   the rule is the reason.)
/// * the row STOPPED ASKING. A folded row draws no controls, so it may hold no
///   pending decision either, and a renamed one no longer answers to the name
///   the question was filed under.
///
/// `still_asking` is the names whose rows actually drew the question this frame.
fn next_confirm(
    armed: Option<&str>,
    arm: Option<&str>,
    disarm: bool,
    acted: bool,
    still_asking: &[String],
) -> Option<String> {
    if let Some(n) = arm {
        return Some(n.to_owned());
    }
    let n = armed?;
    if disarm || acted || !still_asking.iter().any(|a| a == n) {
        return None;
    }
    Some(n.to_owned())
}

/// Fold every row that answers to a name an earlier row already answers to.
///
/// A name typed onto another row's is MERGED when the field is left. If the
/// panel stops being drawn first — the user clicks another MCU tab — no
/// `lost_focus` ever arrives, and two rows keep one name for good: from then on
/// the second row's `+` fills the first, and the canvas draws one mat where the
/// roster shows two.
///
/// Called only when nothing is being typed, because a name being typed passes
/// through duplicates on the way somewhere else.
fn sweep_duplicate_names(mcu: &mut Mcu) {
    while let Some(dup) = mcu.groups.iter().enumerate().position(|(i, g)| {
        mcu.groups[..i]
            .iter()
            .any(|e| e.name.trim() == g.name.trim())
    }) {
        let name = mcu.groups[dup].name.clone();
        mcu.rename_group(dup, &name);
    }
}

/// Write the roster's edited names back to the devices.
///
/// Two things make this safe, and both were learned the hard way.
///
/// FOUND BY OLD NAME, not by row index. A merge ends in `groups.remove(idx)`, so
/// an index captured while the roster was drawn can name a different device by
/// the time it is used — and the deferred actions below can remove a row too.
/// Applying every row by index destroyed devices: the next row's unchanged name
/// was handed to whatever had shifted into its slot, and since that name still
/// existed it merged as well, swallowing the rest of the roster with no undo.
/// The row's OLD name is the one thing that still identifies it after anything
/// else has moved.
///
/// A row still being TYPED IN is set, never merged. Committing a merge on every
/// keystroke destroyed devices in passing: typing "disp" out to "display2"
/// passes through "display".
///
/// `focused[gi]` is that row's field holding the caret this frame; `lost[gi]` is
/// the frame it stops holding it — the one on which the name becomes a decision,
/// even though by then the text has long since stopped changing.
fn apply_renames(
    mcu: &mut Mcu,
    names: &[String],
    before: &[String],
    focused: &[bool],
    lost: &[bool],
) {
    for gi in 0..names.len().min(before.len()) {
        let leaving = lost.get(gi).copied().unwrap_or(false);
        // A row the user is leaving is revisited even when its text has not
        // changed this frame: the merge it may be owed was deferred on every
        // keystroke, so this is the frame that owes it.
        if names[gi] == before[gi] && !leaving {
            continue;
        }
        // The row is almost always still where it was drawn; the name search is
        // the fallback for a frame in which something before us moved it.
        let idx = if mcu.groups.get(gi).is_some_and(|g| g.name == before[gi]) {
            Some(gi)
        } else {
            mcu.groups.iter().position(|g| g.name == before[gi])
        };
        let Some(idx) = idx else { continue };
        if focused.get(gi).copied().unwrap_or(false) && !leaving {
            mcu.set_group_name(idx, &names[gi]);
        } else {
            mcu.rename_group(idx, &names[gi]);
        }
    }
}

/// The `Devices:` label and its `+ Device` button, for the panel's TOP BAR.
///
/// Split out of [`device_roster`] so the two things that CREATE something sit
/// together: `+ Add module` and `+ Device` are now one gesture apart on the same
/// bar, instead of one being on the bar and the other at the head of the list
/// below it.
///
/// It applies the act here rather than handing one back, because the bar is
/// drawn BEFORE the list and the list is drawn only while the panel is open —
/// a request parked for the roster would be dropped on the frame it is most
/// likely to be made, with the panel still collapsed. The caller opens the panel
/// and tells the roster to unfold the new row; both follow from the `true`.
///
/// Returns whether a device was created.
pub(super) fn device_add_button(ui: &mut egui::Ui, mcu: &mut Mcu) -> bool {
    ui.label(
        egui::RichText::new("Devices:")
            .size(12.0)
            .color(egui::Color32::from_rgb(150, 150, 160)),
    );
    let clicked = ui
        .button(egui::RichText::new(format!("{} Device", ph::PLUS)).size(11.0))
        .on_hover_text(
            "Gather the pads of one board part — a sensor's bus and its spare \
             interrupt line — under one name.\n\nA device claims nothing: it \
             renames no binding and moves no pin. It marks the pads on the \
             diagram and writes one comment into the generated file.",
        )
        .clicked();
    apply_act(mcu, clicked.then_some(Act::New))
}

/// Draw the roster.
///
/// Nothing is returned: a device reaches the project only through
/// `calculate_mcu_state_hash`, which sees the edit on this same frame and
/// regenerates main.rs. There is no pin file and no config file to re-sync,
/// because a group owns neither.
///
/// `just_added`: the bar's `+ Device` created a device on THIS frame, so its
/// row is unfolded — the name field it needs is not drawn on a folded row.
pub(super) fn device_roster(ui: &mut egui::Ui, mcu: &mut Mcu, just_added: bool) {
    // Editable copies. A `TextEdit` needs a `&mut String` and the group list is
    // read all through the loop, so the names are edited here and written back
    // in one pass at the end.
    let mut names: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
    let names_before = names.clone();
    let members: Vec<Vec<usize>> = mcu
        .groups
        .iter()
        .map(|g| g.pins.iter().copied().collect())
        .collect();
    let loose = loose_pins(mcu);
    let bus_devs = bus_devices(mcu);
    let modules: Vec<(String, String, egui::Color32)> = mcu
        .modules
        .iter()
        .map(|m| {
            (
                m.id.clone(),
                mod_gui::module_title(m),
                mod_gui::module_color(m.kind, m.instance()),
            )
        })
        .collect();
    let mut act: Option<Act> = None;
    // Which row is asking "remove?", and the two things a row can ask for.
    // Deferred like everything else here: the row is drawn while the group list
    // is borrowed.
    let armed = mcu.device_remove_confirm.clone();
    let mut arm_name: Option<String> = None;
    let mut disarm = false;
    // The names whose rows actually DREW the question this frame.
    let mut still_asking: Vec<String> = Vec::new();
    // The fold bit each row ENDS this frame with, by NAME. Three ordinary
    // gestures shorten the roster and shift every row below - dissolving a
    // device, a rename that merges two, and dropping a device's last pad - and
    // the fold state is keyed by row, so without this the rows below inherit
    // somebody else's fold: a device the user shut springs open on its own.
    let mut folds: Vec<(String, bool)> = Vec::new();
    // Which row's name field holds the caret this frame, and which one is being
    // left. A name being typed is stored but never merged; the frame it is left
    // on is the frame the merge is owed — see `apply_renames`.
    let mut focused: Vec<bool> = vec![false; names.len()];
    let mut lost: Vec<bool> = vec![false; names.len()];

    if mcu.groups.is_empty() {
        ui.label(
            egui::RichText::new("No devices yet.")
                .size(10.0)
                .italics()
                .color(egui::Color32::from_gray(120)),
        );
    }

    // The last row's index, taken before the loop borrows `names` mutably.
    let last_row = names.len().saturating_sub(1);
    let focus = &mut focused;
    let left = &mut lost;
    let arm = &mut arm_name;
    let unarm = &mut disarm;
    for (gi, name) in names.iter_mut().enumerate() {
        let c = mod_gui::group_color(name);
        // Keyed by ROW, not by name. The name is what the user edits, and an id
        // built from it would change on every keystroke — the row would fold
        // itself shut in the middle of being renamed. An index survives a rename
        // untouched; it only shifts when a device is dissolved or merged away,
        // which is rare and is the user's own doing.
        let st_id = egui::Id::new(("device_row", gi));
        // The row the bar's `+ Device` just created, which is always the last.
        //
        // It has to be forced open HERE, before the row is drawn, not only in
        // the trailing block below. While `Act::New` was applied at the END of
        // this function the new group did not exist during this loop, so the
        // row simply was not drawn on its creation frame and the trailing
        // unfold was in time. Now the bar creates it first, so the loop DOES
        // draw it — with whatever `("device_row", gi)` bit the device that last
        // sat at this index left behind, since `load_with_default_open` returns
        // a STORED value in preference to the default and nothing ever clears
        // the tail indices. The row flashed folded (no name field, no `+`, no
        // trash) for one frame, or not, depending on unrelated history.
        let is_new_row = just_added && gi == last_row;
        let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            st_id,
            is_new_row,
        );
        if is_new_row {
            st.set_open(true);
        }
        let open = st.is_open();
        // Only an unfolded row can be asking: the trash that arms it is not
        // drawn on a folded one.
        let asking = open && armed.as_deref() == Some(names_before[gi].as_str());
        if asking {
            still_asking.push(names_before[gi].clone());
        }
        let mut toggle = false;
        let row = egui::Frame::new()
            .fill(egui::Color32::from_rgba_unmultiplied(
                c.r(),
                c.g(),
                c.b(),
                22,
            ))
            .inner_margin(egui::Margin::symmetric(6, 3))
            .corner_radius(egui::CornerRadius::same(4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    // The colour the diagram marks this device's pads with —
                    // shown here so the roster and the picture can be matched
                    // without reading either.
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(if open {
                                    ph::CARET_DOWN
                                } else {
                                    ph::CARET_RIGHT
                                })
                                .size(11.0)
                                .color(c),
                            )
                            .frame(false),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(if open {
                            "Fold the device away. Its name stays; its pads are hidden."
                        } else {
                            "Unfold the device to see its pads and rename it."
                        })
                        .clicked()
                    {
                        toggle = true;
                    }
                    let (r, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
                    ui.painter().rect_filled(r, 2.0, c);
                    // The name is EDITABLE only while the device is unfolded. A
                    // folded row is a summary, and a text field in one invites a
                    // rename the user cannot see the consequences of - the pads
                    // it applies to are not on screen.
                    if open {
                        let edit = ui.add(
                            egui::TextEdit::singleline(name)
                                .desired_width(ui.available_width() - 46.0)
                                .font(egui::TextStyle::Small)
                                .hint_text("name"),
                        );
                        // Still being typed in: `apply_renames` stores it without
                        // letting it merge onto a name it is only passing through.
                        focus[gi] = edit.has_focus();
                        left[gi] = edit.lost_focus();
                        edit.on_hover_text(
                            "The device's name. It appears on the generated comment and \
                             nowhere else in the code — renaming it is always safe.",
                        );
                    } else {
                        // The whole name is a second, larger target for the
                        // caret beside it — a 9 px glyph is a small thing to ask
                        // someone to hit.
                        if ui
                            .add(
                                egui::Label::new(
                                    egui::RichText::new(name.as_str())
                                        .text_style(egui::TextStyle::Small),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("Unfold the device to see its pads and rename it.")
                            .clicked()
                        {
                            toggle = true;
                        }
                    }
                    // A FOLDED row carries no controls. It is a summary — the
                    // pads the `+` would add to and the trash would take apart
                    // are not on screen, so neither button has anything visible
                    // to act on.
                    if !open {
                        return;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if asking {
                            // Cancel sits where the trash was, nearest the edge:
                            // the pointer is already there, and the safe choice
                            // is the one it lands on.
                            if ui.button("Cancel").clicked() {
                                *unarm = true;
                            }
                            if ui
                                .button(
                                    egui::RichText::new(format!("{} Remove", ph::TRASH))
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(220, 80, 80)),
                                )
                                .clicked()
                            {
                                act = Some(Act::Dissolve(gi));
                            }
                            return;
                        }
                        if ui
                            .button(egui::RichText::new(ph::TRASH).size(11.0))
                            .on_hover_text("Take the device apart. Its pads keep their functions.")
                            .clicked()
                        {
                            *arm = Some(name.clone());
                        }
                        ui.menu_button(egui::RichText::new(ph::PLUS).size(11.0), |ui| {
                            ui.set_min_width(180.0);
                            let mine: std::collections::HashSet<usize> =
                                members[gi].iter().copied().collect();
                            let mut any = false;
                            for (id, title, mc) in &modules {
                                if ui
                                    .button(egui::RichText::new(title).size(11.0).color(*mc))
                                    .clicked()
                                {
                                    act = Some(Act::AddModule(gi, id.clone()));
                                    ui.close();
                                }
                                any = true;
                            }
                            let free: Vec<&(usize, String)> =
                                loose.iter().filter(|(n, _)| !mine.contains(n)).collect();
                            if any && !free.is_empty() {
                                ui.separator();
                            }
                            for (n, label) in free {
                                if ui.button(egui::RichText::new(label).size(11.0)).clicked() {
                                    act = Some(Act::AddPin(gi, *n));
                                    ui.close();
                                }
                            }
                            // The devices of an I2C bus share its pads, so
                            // they are offered one by one: one may belong to a
                            // board part the rest of its bus does not. Not the
                            // ones already in THIS device, either way.
                            let here = name.trim();
                            let offer: Vec<&BusDevice> = bus_devs
                                .iter()
                                .filter(|d| d.group.as_deref() != Some(here))
                                .collect();
                            if (any || !loose.is_empty()) && !offer.is_empty() {
                                ui.separator();
                            }
                            for d in &offer {
                                if ui
                                    .button(egui::RichText::new(&d.label).size(11.0).color(d.color))
                                    .on_hover_text(
                                        "Put this device of the bus in the device. The rest of the bus stays where it is.",
                                    )
                                    .clicked()
                                {
                                    act = Some(Act::AddI2c(gi, d.instance, d.key));
                                    ui.close();
                                }
                            }
                            if !any && loose.is_empty() && offer.is_empty() {
                                ui.label(
                                    egui::RichText::new("Nothing configured to add yet.")
                                        .size(10.0)
                                        .color(egui::Color32::from_gray(130)),
                                );
                            }
                        })
                        .response
                        .on_hover_text("Add a module's pads, one configured pin, or one device of an I2C bus.");
                    });
                });
                if asking {
                    ui.label(
                        egui::RichText::new(
                            "Takes the device apart. Its pads keep their functions.",
                        )
                        .size(10.0)
                        .color(egui::Color32::from_rgb(220, 180, 90)),
                    );
                }
                for pin in members[gi].iter().filter(|_| open) {
                    ui.horizontal(|ui| {
                        ui.add_space(13.0);
                        ui.label(
                            egui::RichText::new(member_label(mcu, *pin))
                                .size(10.0)
                                .color(egui::Color32::from_gray(180)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button(egui::RichText::new(ph::X).size(9.0))
                                .on_hover_text("Take this pad out of the device.")
                                .clicked()
                            {
                                act = Some(Act::Drop(*pin));
                            }
                        });
                    });
                }
                // Its devices of I2C buses: the ones put here by hand, with an X
                // that sends them back to their bus's device; and, dimmer, the
                // ones here BECAUSE their bus is - no X, the bus decides those.
                let here = names_before[gi].trim();
                for d in bus_devs.iter().filter(|_| open) {
                    if d.group.as_deref() != Some(here) {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(13.0);
                        if d.explicit {
                            ui.label(egui::RichText::new(&d.label).size(10.0).color(d.color));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .small_button(egui::RichText::new(ph::X).size(9.0))
                                        .on_hover_text(
                                            "Take this device out - back to the device its bus is in.",
                                        )
                                        .clicked()
                                        && let Some(u) = d.uid
                                    {
                                        act = Some(Act::DropI2c(d.instance, u));
                                    }
                                },
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(format!("{}  - with its bus", d.label))
                                    .size(10.0)
                                    .color(egui::Color32::from_gray(130)),
                            )
                            .on_hover_text(
                                "In this device because its bus is. Put it in another device to move it on its own.",
                            );
                        }
                    });
                }
            });
        // The list keeps its scroll offset across a collapse, and the device
        // rows sit ABOVE the separator and the modules — so a user who had
        // scrolled down to a module got a new row appended off the top of the
        // viewport, and the only visible effect of the click was the module
        // list nudging down.
        if is_new_row {
            row.response.scroll_to_me(Some(egui::Align::Center));
        }
        folds.push((names_before[gi].clone(), if toggle { !open } else { open }));
        if toggle {
            st.set_open(!open);
        }
        // `set_open` mutates a COPY of the state, so it only reaches egui here.
        st.store(ui.ctx());
        ui.add_space(2.0);
    }

    // The deferred actions run FIRST, while their indices still mean what they
    // meant when the row was drawn. `apply_renames` then finds its row by NAME,
    // so it does not care what an action moved.
    mcu.device_remove_confirm = next_confirm(
        mcu.device_remove_confirm.as_deref(),
        arm_name.as_deref(),
        disarm,
        act.is_some(),
        &still_asking,
    );
    let rows_before = names_before.len();
    // `just_added` is NOT folded in here: the loop above already opened that
    // row, early enough to draw it open on its own first frame. This covers
    // only a creation through the roster's own act — which nothing does today,
    // the `+ Device` button having moved to the bar, but `apply_act` is the one
    // place a device is created and a second creator would be a second set of
    // rules.
    let was_new = apply_act(mcu, act);
    if names != names_before {
        // Asked about a name that is about to stop existing.
        mcu.device_remove_confirm = None;
    }
    apply_renames(mcu, &names, &names_before, &focused, &lost);
    // A name typed onto another row's is MERGED when the field is left. If the
    // panel stops being drawn first - the user clicks another MCU tab - no
    // `lost_focus` ever arrives, and two rows keep one name for good: from then
    // on the second row's `+` fills the first, and the canvas draws one mat
    // where the roster shows two. With nothing being typed anywhere, a duplicate
    // is simply a merge that was owed.
    if !focused.iter().any(|f| *f) {
        sweep_duplicate_names(mcu);
    }

    // The roster got shorter or longer, so every row below the change now holds
    // a fold bit that belonged to a different device: the fold state is keyed on
    // the row INDEX, and the rows moved. Re-seat them by NAME - the identity
    // everything else in this file uses.
    //
    // Placed here, at the end, and not beside `apply_act` where it started.
    // THREE things above can shorten the roster and only one of them was
    // covered: `apply_act` removes a device, `apply_renames` merges one onto a
    // taken name (`rename_group` -> `groups.remove(idx)`), and
    // `sweep_duplicate_names` loops that same merge. So typing an existing name
    // onto a row merged the two and then sprang the row below open, showing the
    // pads of a device the user had deliberately folded shut.
    //
    // A row whose name is not in `folds` cannot be identified - it was renamed
    // to something new this frame - and comes up folded, which is what a row
    // nobody asked about should be.
    if let Some(want) = reseated_folds(&folds, &mcu.groups, rows_before) {
        for (gi, open) in want.into_iter().enumerate() {
            let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                egui::Id::new(("device_row", gi)),
                open,
            );
            st.set_open(open);
            st.store(ui.ctx());
        }
    }

    // A device the user has just created is unfolded, so its name can be typed
    // straight away — folded, the field it needs is not there. AFTER the
    // re-seat, which would otherwise fold the brand-new row shut.
    if was_new {
        let gi = mcu.groups.len().saturating_sub(1);
        let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            egui::Id::new(("device_row", gi)),
            true,
        );
        st.set_open(true);
        st.store(ui.ctx());
    }
}

/// Which rows should come back open, once the roster has changed shape.
///
/// `folds` is `(name as drawn, open)` per row from BEFORE the mutations, so a
/// device is followed by name through however many rows moved above it.
///
/// `None` when the roster is the same length it was: then no row moved, every
/// index still means what it meant, and rewriting the state would only be a
/// chance to get it wrong.
fn reseated_folds(
    folds: &[(String, bool)],
    groups: &[crate::panels::mcu_module::mcu_config::PinGroup],
    rows_before: usize,
) -> Option<Vec<bool>> {
    if groups.len() == rows_before {
        return None;
    }
    Some(
        groups
            .iter()
            .map(|g| {
                folds
                    .iter()
                    .find(|(n, _)| n.trim() == g.name.trim())
                    .map(|(_, o)| *o)
                    // Renamed to something new this frame, so it cannot be
                    // followed - and a row nobody asked about comes up folded.
                    .unwrap_or(false)
            })
            .collect(),
    )
}

/// Carry out what the roster's controls asked for.
///
/// Indices are into the roster AS DRAWN, so this runs before any rename can
/// move a row.
///
/// Returns whether a device was CREATED — the caller unfolds that row, and the
/// name field only exists while a row is unfolded.
fn apply_act(mcu: &mut Mcu, act: Option<Act>) -> bool {
    let was_new = matches!(act, Some(Act::New));
    match act {
        Some(Act::New) => {
            let name = fresh_name(mcu);
            mcu.new_group(name);
        }
        Some(Act::Dissolve(gi)) => {
            if gi < mcu.groups.len() {
                mcu.groups.remove(gi);
            }
        }
        // A row with no name yet is not a device to add anything TO. Without
        // this the gesture reached `join_group(pin, "")`, whose first act is to
        // take the pad out of every device - so "+" on a nameless row silently
        // un-grouped a pad from a different device and deleted it if it was the
        // last one.
        Some(Act::AddModule(gi, id)) => {
            if let (Some(name), Some(m)) = (
                mcu.groups.get(gi).map(|g| g.name.clone()).filter(named),
                mcu.modules.iter().find(|m| m.id == id).cloned(),
            ) {
                mcu.join_group_module(&m, &name);
            }
        }
        Some(Act::AddPin(gi, pin)) => {
            if let Some(name) = mcu.groups.get(gi).map(|g| g.name.clone()).filter(named) {
                mcu.join_group(pin, &name);
            }
        }
        Some(Act::Drop(pin)) => mcu.join_group(pin, ""),
        // The same nameless-row guard as a pad: joining "" is how a device of
        // a bus LEAVES its device, which from a row with no name would take it
        // out of somebody else's.
        Some(Act::AddI2c(gi, inst, key)) => {
            if let Some(name) = mcu.groups.get(gi).map(|g| g.name.clone()).filter(named) {
                mcu.join_group_i2c(inst, key, &name);
            }
        }
        Some(Act::DropI2c(inst, uid)) => {
            mcu.join_group_i2c(
                inst,
                crate::panels::mcu_module::modules::I2cDeviceKey::Uid(uid),
                "",
            );
        }
        None => {}
    }
    was_new
}

#[cfg(test)]
mod tests {
    use crate::panels::mcu_module::mcu_config::PinGroup;

    fn group(name: &str, pins: &[usize]) -> PinGroup {
        PinGroup {
            name: name.to_owned(),
            pins: pins.iter().copied().collect(),
            ..Default::default()
        }
    }

    /// No field holds the caret — the roster's committed state, which is what
    /// every test but the typing one is about.
    fn blurred(n: usize) -> Vec<bool> {
        vec![false; n]
    }

    fn named(mcu: &crate::panels::mcu_module::mcu::Mcu, name: &str) -> Option<Vec<usize>> {
        mcu.groups
            .iter()
            .find(|g| g.name == name)
            .map(|g| g.pins.iter().copied().collect())
    }

    fn bare_mcu() -> crate::panels::mcu_module::mcu::Mcu {
        crate::panels::mcu_module::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu()
    }

    /// A merge moves every row below it up, and the fold bits are keyed on the
    /// row INDEX - so they have to follow the devices, not stay where they are.
    ///
    /// The re-seat used to run beside `apply_act` only, and `apply_renames` and
    /// `sweep_duplicate_names` merge rows AFTER that. Typing an existing name
    /// onto a row therefore merged the two and sprang the row below open,
    /// showing the pads of a device the user had deliberately folded shut.
    #[test]
    fn a_merge_carries_each_fold_bit_to_the_row_its_device_landed_on() {
        let folds = vec![
            ("radar".to_owned(), true),
            ("imu".to_owned(), true),
            ("leds".to_owned(), false),
        ];
        // `imu` was renamed onto `radar`, so the two merged and `leds` moved up.
        let after = vec![group("radar", &[1, 2]), group("leds", &[3])];
        let want = super::reseated_folds(&folds, &after, 3).expect("the roster got shorter");
        assert_eq!(want, vec![true, false], "leds stays shut where it landed");
    }

    /// A roster that did not change shape is left entirely alone.
    #[test]
    fn a_plain_rename_does_not_touch_the_fold_bits() {
        let folds = vec![("radar".to_owned(), true), ("imu".to_owned(), false)];
        let after = vec![group("radar2", &[1]), group("imu", &[2])];
        assert!(
            super::reseated_folds(&folds, &after, 2).is_none(),
            "same length, so no row moved"
        );
    }

    /// A device added at the end does not disturb the ones above it.
    #[test]
    fn a_new_row_leaves_the_others_where_they_were() {
        let folds = vec![("radar".to_owned(), true), ("leds".to_owned(), false)];
        let after = vec![
            group("radar", &[1]),
            group("leds", &[2]),
            group("dev3", &[]),
        ];
        let want = super::reseated_folds(&folds, &after, 2).expect("the roster grew");
        assert_eq!(want, vec![true, false, false]);
    }

    /// Names are matched the way every other comparison in this file matches
    /// them - trimmed - so a device typed with padding is still followed.
    #[test]
    fn padding_does_not_lose_a_devices_fold_bit() {
        let folds = vec![("  radar ".to_owned(), true), ("imu".to_owned(), false)];
        let after = vec![group("radar", &[1])];
        let want = super::reseated_folds(&folds, &after, 2).expect("shorter");
        assert_eq!(want, vec![true]);
    }

    /// Renaming one device onto another's name merges the two — and touches
    /// NOTHING else. Re-applying every row by index used to walk indices the
    /// merge had already shifted, so one rename ate the whole rest of the
    /// roster: three devices became one holding all three pads, with no undo.
    #[test]
    fn a_rename_that_merges_leaves_the_rest_of_the_roster_alone() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![
            group("radar", &[1]),
            group("display", &[2]),
            group("imu", &[3]),
        ];
        let before: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
        // The user retypes the FIRST row as "display".
        let mut names = before.clone();
        names[0] = "display".into();

        super::apply_renames(
            &mut mcu,
            &names,
            &before,
            &blurred(names.len()),
            &blurred(names.len()),
        );

        assert_eq!(mcu.groups.len(), 2, "one merge, one row gone");
        assert_eq!(named(&mcu, "display"), Some(vec![1, 2]));
        assert_eq!(named(&mcu, "imu"), Some(vec![3]), "imu is untouched");
    }

    /// The same, one row further along, and with four devices — the arrangement
    /// where the old loop destroyed a device that was not even adjacent to the
    /// edit.
    #[test]
    fn a_rename_in_the_middle_of_the_roster_takes_only_its_own_row() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![
            group("a", &[1]),
            group("b", &[2]),
            group("c", &[3]),
            group("d", &[4]),
        ];
        let before: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
        let mut names = before.clone();
        names[1] = "a".into();

        super::apply_renames(
            &mut mcu,
            &names,
            &before,
            &blurred(names.len()),
            &blurred(names.len()),
        );

        assert_eq!(named(&mcu, "a"), Some(vec![1, 2]));
        assert_eq!(named(&mcu, "c"), Some(vec![3]));
        assert_eq!(named(&mcu, "d"), Some(vec![4]));
        assert_eq!(mcu.groups.len(), 3);
    }

    /// Two rows edited in one frame — a merge AND a rename — each land on the
    /// device that was in THAT row.
    ///
    /// Row 0 is retyped as "b", merging it into row 1; row 1 is retyped as "x".
    /// The merged device is the one that was in row 1, so it is the one that ends
    /// up called "x". Keyed by row INDEX instead, the second edit was written to
    /// whatever had shifted into slot 1 — device "c", which was not being edited
    /// at all.
    #[test]
    fn two_rows_edited_in_one_frame_each_land_on_their_own_device() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("a", &[1]), group("b", &[2]), group("c", &[3])];
        let before: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
        let names = vec!["b".to_owned(), "x".to_owned(), "c".to_owned()];

        super::apply_renames(
            &mut mcu,
            &names,
            &before,
            &blurred(names.len()),
            &blurred(names.len()),
        );

        assert_eq!(named(&mcu, "c"), Some(vec![3]), "c was never edited");
        assert_eq!(
            named(&mcu, "x"),
            Some(vec![1, 2]),
            "the merged device took row 1's new name"
        );
        assert_eq!(mcu.groups.len(), 2);
    }

    /// A name is committed on every keystroke, so it passes THROUGH values the
    /// user never meant — and one of them can be another device's name. Merging
    /// there destroyed that device mid-word, and the rest of the word then landed
    /// on whatever row had shifted into the slot.
    #[test]
    fn a_name_typed_through_another_devices_name_does_not_merge() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("display", &[1]), group("disp", &[2])];
        // Row 1's field holds the caret while the user types "lay2" onto "disp".
        for ch in "lay2".chars() {
            let before: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
            let mut names = before.clone();
            names[1] = format!("{}{ch}", before[1]);
            super::apply_renames(&mut mcu, &names, &before, &[false, true], &[false, false]);
        }
        assert_eq!(mcu.groups.len(), 2, "nothing was merged in passing");
        assert_eq!(named(&mcu, "display"), Some(vec![1]));
        assert_eq!(named(&mcu, "display2"), Some(vec![2]));
    }

    /// …and leaving the field DOES commit the merge, which is the whole point of
    /// waiting.
    #[test]
    fn leaving_the_field_on_a_taken_name_commits_the_merge() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("display", &[1]), group("disp", &[2])];
        let before: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
        let names = vec!["display".to_owned(), "display".to_owned()];
        // Typed with the caret in row 1: stored, not merged.
        super::apply_renames(&mut mcu, &names, &before, &[false, true], &[false, false]);
        assert_eq!(mcu.groups.len(), 2);
        // The user clicks away.
        let before: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
        super::apply_renames(&mut mcu, &names, &before, &blurred(2), &[false, true]);
        assert_eq!(mcu.groups.len(), 1, "now they are one device");
        assert_eq!(named(&mcu, "display"), Some(vec![1, 2]));
    }

    /// A plain rename onto a free name renames exactly that device.
    #[test]
    fn a_plain_rename_renames_one_device() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("radar", &[1]), group("display", &[2])];
        let before: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
        let mut names = before.clone();
        names[0] = "mw radar".into();
        super::apply_renames(
            &mut mcu,
            &names,
            &before,
            &blurred(names.len()),
            &blurred(names.len()),
        );
        assert_eq!(named(&mcu, "mw radar"), Some(vec![1]));
        assert_eq!(named(&mcu, "display"), Some(vec![2]));
    }

    /// A device name is typed one character at a time, and the roster re-seeds
    /// its text field from the stored name every frame — so a name that is
    /// trimmed on the way in can never grow a space. "mw radar" came back as
    /// "mwradar", which is the very example this feature was built for.
    #[test]
    fn a_device_name_can_be_typed_with_a_space_in_it() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("", &[1])];
        // One keystroke per frame, re-reading the field from the model each time.
        for ch in "mw radar".chars() {
            let before: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
            let names = vec![format!("{}{ch}", before[0])];
            super::apply_renames(
                &mut mcu,
                &names,
                &before,
                &blurred(names.len()),
                &blurred(names.len()),
            );
        }
        assert_eq!(mcu.groups[0].name, "mw radar");
        // …and the pad gesture still finds the group by that name.
        mcu.join_group(2, "mw radar");
        assert_eq!(named(&mcu, "mw radar"), Some(vec![1, 2]));
    }

    /// Two rows answering to one name is a merge that never got its
    /// `lost_focus` frame — the user typed the duplicate and then left the panel
    /// by another route. With nothing being typed, it is simply owed.
    #[test]
    fn a_duplicate_name_left_behind_is_merged_when_nothing_is_being_typed() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![
            group("radar", &[1]),
            group("radar", &[2]),
            group("imu", &[3]),
        ];

        super::sweep_duplicate_names(&mut mcu);

        assert_eq!(mcu.groups.len(), 2);
        assert_eq!(
            named(&mcu, "radar"),
            Some(vec![1, 2]),
            "the pads came together"
        );
        assert_eq!(named(&mcu, "imu"), Some(vec![3]), "and nothing else moved");
    }

    /// Padding is not a difference here either.
    #[test]
    fn a_padded_duplicate_is_swept_too() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("radar", &[1]), group(" radar ", &[2])];
        super::sweep_duplicate_names(&mut mcu);
        assert_eq!(mcu.groups.len(), 1);
        assert_eq!(named(&mcu, "radar"), Some(vec![1, 2]));
    }

    /// A roster with no duplicates is left exactly as it is.
    #[test]
    fn a_roster_without_duplicates_is_untouched() {
        let mut mcu = bare_mcu();
        let before = vec![group("radar", &[1]), group("imu", &[2])];
        mcu.groups = before.clone();
        super::sweep_duplicate_names(&mut mcu);
        assert_eq!(mcu.groups, before);
    }

    /// The trash on a device asks before it takes anything apart, and the
    /// question survives exactly as long as the row keeps asking it.
    #[test]
    fn the_remove_question_lasts_while_its_row_keeps_asking() {
        let asking = ["radar".to_owned()];
        // Armed by the trash.
        assert_eq!(
            super::next_confirm(None, Some("radar"), false, false, &[]).as_deref(),
            Some("radar")
        );
        // Still on screen, still asking.
        assert_eq!(
            super::next_confirm(Some("radar"), None, false, false, &asking).as_deref(),
            Some("radar")
        );
        // Cancel.
        assert!(super::next_confirm(Some("radar"), None, true, false, &asking).is_none());
        // Remove, or any other act that MOVED a row. Creating a device is not
        // one of them and cannot reach here as one — see
        // `creating_a_device_leaves_an_armed_question_alone`.
        assert!(super::next_confirm(Some("radar"), None, false, true, &asking).is_none());
        // The row folded, or was renamed away: it no longer draws the question.
        assert!(super::next_confirm(Some("radar"), None, false, false, &[]).is_none());
    }

    /// Creating a device leaves another row's armed "remove?" question standing.
    ///
    /// `+ Device` used to produce an `Act::New` inside the roster, so
    /// `next_confirm` saw `acted == true` and disarmed whatever was asking. That
    /// was always over-broad: the rule is that an act which MOVED the row the
    /// question is about ends it, and a creation appends — it renames nothing
    /// and shifts no index. Now that the button lives on the panel's top bar and
    /// applies its own act, the roster reaches `next_confirm` with
    /// `acted == false`, and this is the behaviour that has to hold for that to
    /// be right rather than merely different.
    #[test]
    fn creating_a_device_leaves_an_armed_question_alone() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("radar", &[7])];
        mcu.device_remove_confirm = Some("radar".to_owned());

        assert!(super::apply_act(&mut mcu, Some(super::Act::New)));

        assert_eq!(
            mcu.device_remove_confirm.as_deref(),
            Some("radar"),
            "the question is untouched"
        );
        assert_eq!(mcu.groups.len(), 2, "and a device was created");
        assert_eq!(
            mcu.groups[0].name, "radar",
            "APPENDED, so the armed row keeps its index and its name — which is \
             the whole reason the question is still answerable"
        );
        assert_eq!(
            mcu.groups[0].pins.iter().copied().collect::<Vec<_>>(),
            vec![7],
            "and its pads"
        );
    }

    /// Arming a second row replaces the first — a single question at a time,
    /// because two rows both asking is two answers the user did not give.
    #[test]
    fn arming_one_row_takes_the_question_off_another() {
        let asking = ["radar".to_owned()];
        assert_eq!(
            super::next_confirm(Some("radar"), Some("display"), false, false, &asking).as_deref(),
            Some("display")
        );
    }

    /// Only creating a device asks the caller to unfold its row — that is the
    /// one act after which the user needs the name field that folding hides.
    #[test]
    fn only_a_new_device_asks_to_be_unfolded() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("radar", &[7])];
        assert!(super::apply_act(&mut mcu, Some(super::Act::New)));
        assert!(!super::apply_act(&mut mcu, None));
        assert!(!super::apply_act(&mut mcu, Some(super::Act::Drop(7))));
        assert!(!super::apply_act(&mut mcu, Some(super::Act::Dissolve(0))));
    }

    /// "+" on a row the user has not named yet must do NOTHING.
    ///
    /// The handler used to read the row's name straight out of the model and
    /// hand it to `join_group`, whose first act is to take the pad out of every
    /// device — so the gesture silently un-grouped a pad from a DIFFERENT device
    /// and deleted that device if it was its last one.
    #[test]
    fn adding_a_pad_through_a_nameless_row_takes_it_from_nobody() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("display", &[15]), group("", &[])];

        super::apply_act(&mut mcu, Some(super::Act::AddPin(1, 15)));

        assert_eq!(
            named(&mcu, "display"),
            Some(vec![15]),
            "the pad never left the device that held it"
        );
    }

    /// A device of an I2C bus through the roster: a named row takes it, a
    /// nameless row takes it from nobody, and its X sends it back to its bus's
    /// device.
    #[test]
    fn an_i2c_device_joins_a_named_row_and_its_x_sends_it_back() {
        use crate::panels::mcu_module::modules::{I2cDeviceEdit, I2cDeviceKey, ModuleKind};
        let mut mcu = crate::panels::mcu_module::mock_mcu::create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let inst = mcu.modules[0].instance();
        mcu.edit_i2c_device(inst, I2cDeviceEdit::Add);
        let uid = match &mcu.modules[0].config {
            crate::panels::mcu_module::modules::ModuleConfig::I2c(c) => c.devices[0].uid,
            _ => unreachable!(),
        };
        let key = I2cDeviceKey::Uid(uid);
        mcu.groups = vec![group("display", &[]), group("", &[])];
        mcu.groups[0].i2c = [(inst, uid)].into();

        super::apply_act(&mut mcu, Some(super::Act::AddI2c(1, inst, key)));
        assert!(
            mcu.groups[0].i2c.contains(&(inst, uid)),
            "a nameless row took it"
        );

        mcu.groups[1].name = "oled".into();
        super::apply_act(&mut mcu, Some(super::Act::AddI2c(1, inst, key)));
        assert_eq!(
            mcu.group_of_i2c_device(inst, key).map(|g| g.name.as_str()),
            Some("oled")
        );
        assert!(named(&mcu, "display").is_none(), "it held only that device");

        super::apply_act(&mut mcu, Some(super::Act::DropI2c(inst, uid)));
        assert_eq!(
            mcu.group_of_i2c_device(inst, key),
            None,
            "back with its bus, in none"
        );
    }

    /// The same gesture on a NAMED row does move the pad, which is what makes the
    /// test above about the name and not about the gesture.
    #[test]
    fn adding_a_pad_through_a_named_row_moves_it() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("display", &[15]), group("radar", &[16])];

        super::apply_act(&mut mcu, Some(super::Act::AddPin(1, 15)));

        assert_eq!(named(&mcu, "radar"), Some(vec![15, 16]));
        assert!(named(&mcu, "display").is_none(), "it had only that pad");
    }

    /// Two names that differ only in padding are the SAME device.
    ///
    /// Storing the name verbatim is what lets a space be typed, but identity had
    /// to stay trimmed: `mcu.config` trims on the way out and the colour is
    /// hashed from the trimmed name, so "Device 1 " and "Device 1" drew one
    /// colour, saved as one line, and came back after a reload as two devices
    /// with byte-identical names — where every "+ pad" gesture on the second row
    /// silently filled the first.
    #[test]
    fn two_names_that_differ_only_in_padding_are_one_device() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("Device 1", &[1]), group("radar", &[2])];
        let before: Vec<String> = mcu.groups.iter().map(|g| g.name.clone()).collect();
        let mut names = before.clone();
        names[1] = "Device 1 ".into();

        super::apply_renames(
            &mut mcu,
            &names,
            &before,
            &blurred(names.len()),
            &blurred(names.len()),
        );

        assert_eq!(mcu.groups.len(), 1, "they merged");
        assert_eq!(named(&mcu, "Device 1"), Some(vec![1, 2]));
    }

    /// …and "+ Device" will not hand out a name a padded one already answers to.
    #[test]
    fn a_new_device_skips_a_name_a_padded_one_already_answers_to() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("Device 1 ", &[1])];
        assert_eq!(super::fresh_name(&mcu), "Device 2");
    }

    /// The roster's "+ Device" must not hand out a name already on the board —
    /// two groups with one name would answer to the same colour and the same
    /// `join_group` lookup.
    #[test]
    fn a_new_device_gets_a_name_nothing_else_answers_to() {
        let mut mcu = bare_mcu();
        mcu.groups = vec![group("Device 1", &[1]), group("Device 3", &[2])];
        assert_eq!(super::fresh_name(&mcu), "Device 2");
        mcu.groups.push(group("Device 2", &[3]));
        assert_eq!(super::fresh_name(&mcu), "Device 4");
    }
}
