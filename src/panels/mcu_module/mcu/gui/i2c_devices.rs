//! The name and ID (address) fields of an I2C bus's devices, shared by the two
//! editors that show them: the rows in the Virtual-modules panel and the boxes
//! on the Pins canvas.
//!
//! Both only COLLECT what the user did ([`I2cAct`]) and hand it back; the
//! caller applies it through `Mcu::edit_i2c_device` once its loop over
//! `mcu.modules` has let go of the borrow. One write path, so the two editors
//! cannot carry rules of their own.
//!
//! # Why the fields are staged
//!
//! A device's config file is named after the device
//! (`pins/configs/i2c0/device1_oled.rs`), so a rename renames the file. The
//! user's code moves with it now, but each move is a workspace write and a
//! round trip through rust-analyzer - and before the tree moved device files
//! it DROPPED them, the code below the markers included. A field bound
//! straight to the model renamed the file on every keystroke: typing "oled"
//! went through `o`, `ol` and `ole`. So the field edits a copy and commits it
//! once: on Enter, when it loses focus, or (if it vanished first, on a tab
//! switch or a folded panel) the next time it is drawn. Escape throws the copy
//! away.

use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::mcu::gui::module_docs as docs;
use crate::panels::mcu_module::modules::{
    AddressIssue, I2cDeviceEdit, I2cDeviceKey, I2cRow, format_i2c_address, parse_i2c_address,
};
use eframe::egui;

/// What the user did to a bus's devices this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum I2cAct {
    Edit(I2cDeviceEdit),
    /// Ask before removing: the device's config file goes with it.
    ArmRemove(I2cDeviceKey),
    CancelRemove,
    /// Put the device in the Device of this name - or, with an empty one, in
    /// none. The canvas's right-click "Put in Device"; the roster has its own.
    Group(I2cDeviceKey, String),
}

/// What the panel's device rows need from outside the one module they see, and
/// what they hand back - one parameter on a function that already has many.
#[derive(Default, Debug)]
pub struct I2cIo {
    /// `Mcu::i2c_remove_confirm`, read before `mcu.modules` is borrowed.
    pub confirm: Option<(u8, I2cDeviceKey)>,
    pub acts: Vec<(u8, I2cAct)>,
}

/// The question an armed removal asks, and what it warns about. `n` is the
/// device's number (1-based), `devices` how many the bus has.
///
/// Every device has a file now, the only one included, so every removal
/// takes one - and each device after it moves up a number, which renames
/// its file.
pub fn remove_question(name: &str, n: usize, devices: usize) -> String {
    let who = if name.trim().is_empty() {
        format!("device {n}")
    } else {
        name.trim().to_owned()
    };
    let file = "Its file under pins/configs/ goes too, with any code you wrote in it - Ctrl+Z brings both back until you close the IDE.";
    if n < devices {
        format!(
            "Remove {who}? {file} The devices after it move up a number; their files are renamed and keep their code."
        )
    } else {
        format!("Remove {who}? {file}")
    }
}

/// Apply what the editors collected. Returns whether a device list changed.
///
/// Every key is pinned to a uid FIRST, for the whole batch: an `Implicit` or
/// `Unminted` key names a position, and the first edit's mint renames every
/// device of its bus - so the second edit in the same frame (the panel's and
/// the canvas's arrive together) would otherwise name a key that no longer
/// exists, and be dropped with the text typed for it.
pub fn apply_acts(mcu: &mut Mcu, mut acts: Vec<(u8, I2cAct)>) -> bool {
    // Where each positional key points, read before anything is minted.
    let at: Vec<Option<usize>> = acts
        .iter()
        .map(|(inst, act)| {
            let key = act_key(act)?;
            let cfg = mcu.i2c_bus(*inst)?;
            match key {
                I2cDeviceKey::Uid(_) => None,
                I2cDeviceKey::Implicit => cfg.has(key).then_some(0),
                I2cDeviceKey::Unminted(_) => cfg.position(key),
            }
        })
        .collect();
    // Only an EDIT mints: arming a removal changes nothing in the project,
    // and must not rewrite its file. An armed key is re-pinned only when an
    // edit in the same batch minted its bus anyway.
    let mut minted: Vec<u8> = Vec::new();
    for ((inst, act), a) in acts.iter().zip(&at) {
        if a.is_some() && matches!(act, I2cAct::Edit(_)) && !minted.contains(inst) {
            mcu.mint_i2c_bus(*inst);
            minted.push(*inst);
        }
    }
    for ((inst, act), a) in acts.iter_mut().zip(&at) {
        if let Some(p) = a
            && minted.contains(inst)
            && let Some(uid) = mcu
                .i2c_bus(*inst)
                .and_then(|c| c.devices.get(*p))
                .map(|d| d.uid)
        {
            set_act_key(act, I2cDeviceKey::Uid(uid));
        }
    }
    let mut changed = false;
    for (instance, act) in acts {
        match act {
            I2cAct::Edit(e) => changed |= mcu.edit_i2c_device(instance, e),
            I2cAct::ArmRemove(k) => mcu.i2c_remove_confirm = Some((instance, k)),
            I2cAct::CancelRemove => mcu.i2c_remove_confirm = None,
            I2cAct::Group(k, name) => changed |= mcu.join_group_i2c(instance, k, &name),
        }
    }
    changed
}

/// The device an act names, if it names one.
fn act_key(act: &I2cAct) -> Option<I2cDeviceKey> {
    match act {
        I2cAct::Edit(I2cDeviceEdit::Remove(k))
        | I2cAct::Edit(I2cDeviceEdit::Name(k, _))
        | I2cAct::Edit(I2cDeviceEdit::Address(k, _))
        | I2cAct::ArmRemove(k)
        | I2cAct::Group(k, _) => Some(*k),
        I2cAct::Edit(I2cDeviceEdit::Add) | I2cAct::CancelRemove => None,
    }
}

fn set_act_key(act: &mut I2cAct, key: I2cDeviceKey) {
    match act {
        I2cAct::Edit(I2cDeviceEdit::Remove(k))
        | I2cAct::Edit(I2cDeviceEdit::Name(k, _))
        | I2cAct::Edit(I2cDeviceEdit::Address(k, _))
        | I2cAct::ArmRemove(k)
        | I2cAct::Group(k, _) => *k = key,
        I2cAct::Edit(I2cDeviceEdit::Add) | I2cAct::CancelRemove => {}
    }
}

/// A field's working copy: `base` is the model value it was seeded from (or
/// last committed), `buf` what is typed.
#[derive(Clone, Default)]
struct Staged {
    base: String,
    buf: String,
    /// Focused at the end of last frame - what makes an Escape this one's.
    had_focus: bool,
}

impl Staged {
    fn seeded(model: &str) -> Self {
        Self {
            base: model.to_owned(),
            buf: model.to_owned(),
            had_focus: false,
        }
    }
}

/// A single-line field over a COPY of `model`: returns the text once it is
/// committed, never while it is being typed (see the module docs). `place`
/// puts it at a fixed rect (the canvas) instead of in the layout (the panel).
pub fn staged_text(
    ui: &mut egui::Ui,
    id: egui::Id,
    model: &str,
    place: Option<egui::Rect>,
    style: impl FnOnce(egui::TextEdit<'_>) -> egui::TextEdit<'_>,
) -> (egui::Response, Option<String>) {
    let mut st: Staged = ui
        .data(|d| d.get_temp(id))
        .unwrap_or_else(|| Staged::seeded(model));
    let mut commit = None;
    // egui drops focus on Escape before any widget runs, so the field never
    // sees it as its own - it counts for this field if the field had focus
    // last frame. Read from the RAW events: anything drawn earlier in the
    // frame (the editor's find bar) may have consumed the key already, and a
    // consumed Escape would read as never pressed and commit the typed text.
    let escape = st.had_focus
        && ui.input(|i| {
            i.raw.events.iter().any(|e| {
                matches!(e, egui::Event::Key { key: egui::Key::Escape, pressed: true, modifiers, .. } if !modifiers.any())
            })
        });
    if escape {
        st.buf = st.base.clone();
    }
    let focused = ui.memory(|m| m.has_focus(id));
    if st.base != model && (!focused || st.buf == st.base) {
        // The model moved under a field nobody is editing - the other editor,
        // an undo, a rejected value. The model wins, also in a field that holds
        // the caret but was not typed into: it would otherwise commit its stale
        // copy back when it is left, undoing the other editor's change.
        st = Staged::seeded(model);
    } else if !focused && st.buf != st.base {
        // Typed into, then the field stopped being drawn before it could lose
        // focus. Late, but not lost.
        commit = Some(st.buf.clone());
        st.base = st.buf.clone();
    }
    let edit = style(egui::TextEdit::singleline(&mut st.buf).id(id));
    let resp = match place {
        Some(rect) => ui.put(rect, edit),
        None => ui.add(edit),
    };
    // Only what was TYPED here is committed - never a copy that merely went
    // out of date.
    if resp.lost_focus() && !escape && st.buf != st.base {
        commit = Some(st.buf.clone());
        st.base = st.buf.clone();
    }
    st.had_focus = resp.has_focus();
    ui.data_mut(|d| d.insert_temp(id, st));
    (resp, commit)
}

/// The colour an address issue is drawn in.
pub fn issue_color(issue: AddressIssue) -> egui::Color32 {
    if issue.is_error() {
        egui::Color32::from_rgb(235, 90, 80)
    } else {
        egui::Color32::from_rgb(225, 175, 70)
    }
}

const NAME_HOVER: &str = "What this device is called. Its file under pins/configs/<bus>/ is named after it and renamed with it, your code inside included - so the name is kept only when you press Enter or leave the field. Escape keeps the old one.";

/// A device's name field. `n` is its 1-based position, for the hint.
pub fn name_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    row: &I2cRow<'_>,
    n: usize,
    place: Option<egui::Rect>,
    width: f32,
    font: Option<egui::FontId>,
) -> Option<I2cAct> {
    let (resp, commit) = staged_text(ui, id, row.name, place, |te| {
        let te = te.desired_width(width).hint_text(format!("device {n}"));
        match font {
            Some(f) => te.font(f),
            None => te,
        }
    });
    resp.on_hover_text(NAME_HOVER);
    commit.map(|name| I2cAct::Edit(I2cDeviceEdit::Name(row.key, name)))
}

/// A device's ID field: its 7-bit address, in hex, coloured by what is wrong
/// with it. A value that does not parse is dropped and the field shows the
/// address again.
pub fn id_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    row: &I2cRow<'_>,
    issue: Option<AddressIssue>,
    place: Option<egui::Rect>,
    width: f32,
    font: Option<egui::FontId>,
) -> Option<I2cAct> {
    let shown = format_i2c_address(row.address);
    let (resp, commit) = staged_text(ui, id, &shown, place, |te| {
        let mut te = te.desired_width(width).char_limit(4);
        if let Some(i) = issue {
            te = te.text_color(issue_color(i));
        }
        match font {
            Some(f) => te.font(f),
            None => te,
        }
    });
    let hover = match issue {
        Some(i) => format!("{}\n\n{}", i.text(), docs::I2C_ADDRESS),
        None => docs::I2C_ADDRESS.to_owned(),
    };
    resp.on_hover_text(hover);
    commit
        .and_then(|text| parse_i2c_address(&text))
        .map(|a| I2cAct::Edit(I2cDeviceEdit::Address(row.key, a)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame of a staged field, with `events`, returning what it committed.
    fn frame(
        ctx: &egui::Context,
        model: &str,
        events: Vec<egui::Event>,
        draw: bool,
    ) -> Option<String> {
        let mut out = None;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 200.0),
            )),
            events,
            ..Default::default()
        };
        crate::headless::run_ui(ctx, input, |ui| {
            if draw {
                out = staged_text(ui, egui::Id::new("f"), model, None, |te| te).1;
            }
        });
        out
    }

    fn click_field() -> Vec<egui::Event> {
        let p = egui::pos2(30.0, 10.0);
        vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn key(k: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    /// Typing commits nothing; Enter commits the whole word once.
    #[test]
    fn typing_commits_only_on_enter() {
        let ctx = egui::Context::default();
        frame(&ctx, "", Vec::new(), true);
        frame(&ctx, "", click_field(), true);
        for c in ["o", "l", "e", "d"] {
            assert_eq!(
                frame(&ctx, "", vec![egui::Event::Text(c.into())], true),
                None,
                "committed mid-word at {c:?}"
            );
        }
        assert_eq!(
            frame(&ctx, "", key(egui::Key::Enter), true).as_deref(),
            Some("oled")
        );
        // Applied by the caller: the model now says "oled", nothing more comes.
        assert_eq!(frame(&ctx, "oled", Vec::new(), true), None);
    }

    /// Escape throws the copy away.
    #[test]
    fn escape_keeps_the_old_name() {
        let ctx = egui::Context::default();
        frame(&ctx, "imu", Vec::new(), true);
        frame(&ctx, "imu", click_field(), true);
        frame(&ctx, "imu", vec![egui::Event::Text("x".into())], true);
        assert_eq!(frame(&ctx, "imu", key(egui::Key::Escape), true), None);
        assert_eq!(frame(&ctx, "imu", Vec::new(), true), None);
    }

    /// A field that vanished while typed into commits when it is drawn again.
    #[test]
    fn a_field_that_vanished_commits_when_it_comes_back() {
        let ctx = egui::Context::default();
        frame(&ctx, "", Vec::new(), true);
        frame(&ctx, "", click_field(), true);
        frame(&ctx, "", vec![egui::Event::Text("rtc".into())], true);
        // Not drawn for a while: focus goes with it.
        frame(&ctx, "", Vec::new(), false);
        frame(&ctx, "", Vec::new(), false);
        assert_eq!(frame(&ctx, "", Vec::new(), true).as_deref(), Some("rtc"));
    }

    /// The model moved under an unfocused field (the other editor, an undo):
    /// the field follows it and commits nothing.
    #[test]
    fn the_model_wins_over_a_field_nobody_is_typing_in() {
        let ctx = egui::Context::default();
        frame(&ctx, "imu", Vec::new(), true);
        assert_eq!(frame(&ctx, "gyro", Vec::new(), true), None);
        assert_eq!(frame(&ctx, "gyro", Vec::new(), true), None);
    }

    /// Leaving the field without changing it commits nothing.
    #[test]
    fn leaving_an_unchanged_field_commits_nothing() {
        let ctx = egui::Context::default();
        frame(&ctx, "imu", Vec::new(), true);
        frame(&ctx, "imu", click_field(), true);
        assert_eq!(frame(&ctx, "imu", key(egui::Key::Enter), true), None);
    }

    /// Escape discards the typed text even when something drawn earlier in the
    /// frame (the editor's find bar) consumed the key first.
    #[test]
    fn escape_discards_even_when_already_consumed() {
        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, events: Vec<egui::Event>| {
            let mut out = None;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            crate::headless::run_ui(ctx, input, |ui| {
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                out = staged_text(ui, egui::Id::new("f"), "imu", None, |te| te).1;
            });
            out
        };
        run(&ctx, Vec::new());
        run(&ctx, click_field());
        run(&ctx, vec![egui::Event::Text("x".into())]);
        assert_eq!(
            run(&ctx, key(egui::Key::Escape)),
            None,
            "the typed text was kept"
        );
        assert_eq!(run(&ctx, Vec::new()), None);
    }

    /// Two editors over one value: B holds the caret, untouched, while A's
    /// text is committed. B follows the new value and, when left, does not
    /// write its stale copy back.
    #[test]
    fn a_field_left_untouched_never_writes_back_a_stale_value() {
        let ctx = egui::Context::default();
        // B is focused and untouched; the model moves from "" to "oled".
        let b = egui::Id::new("b");
        let run = |ctx: &egui::Context, model: &str, events: Vec<egui::Event>| {
            let mut out = None;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            crate::headless::run_ui(ctx, input, |ui| {
                out = staged_text(ui, b, model, None, |te| te).1;
            });
            out
        };
        run(&ctx, "", Vec::new());
        run(&ctx, "", click_field());
        run(&ctx, "oled", Vec::new());
        assert_eq!(
            run(&ctx, "oled", key(egui::Key::Enter)),
            None,
            "wrote back the stale copy"
        );
    }

    /// A field that holds the caret but was not typed into SHOWS the new value
    /// the moment the other editor changes it - not the stale copy it was
    /// seeded with.
    #[test]
    fn a_focused_untouched_field_shows_the_new_value() {
        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, model: &str, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            let out = crate::headless::run_ui(ctx, input, |ui| {
                staged_text(ui, egui::Id::new("f"), model, None, |te| te);
            });
            let mut texts = Vec::new();
            for s in &out.shapes {
                if let egui::Shape::Text(t) = &s.shape {
                    texts.push(t.galley.text().to_owned());
                }
            }
            texts
        };
        run(&ctx, "", Vec::new());
        run(&ctx, "", click_field());
        run(&ctx, "oled", Vec::new());
        let texts = run(&ctx, "oled", Vec::new());
        assert!(
            texts.iter().any(|t| t == "oled"),
            "still shows the stale copy: {texts:?}"
        );
    }
}
