//! Enter in the Replace bar runs Replace All once per keystroke.
//!
//! Enter ends a single-line `TextEdit` (egui drops its focus), and the bar
//! reads that as `lost_focus() && Enter`. Since egui 0.36 `lost_focus` stays
//! true for a SECOND frame - also after the focus moved to another widget - so
//! an Enter in the very next frame ran Replace All again, and a replacement
//! that contains the query (`value` -> `value2`) came out as `value22`. And a
//! held Enter's key repeats must not replay Replace All from the find field,
//! which takes the focus back, or that same replacement grows at the repeat
//! rate.

use crate::app::{AppIde, EditorSlot, ProjectFileId};
use eframe::egui;

const RUST: &str = "value\nlet total = value + 1;\n";
/// `RUST` after one Replace All of `value` with `value2`.
const ONCE: &str = "value2\nlet total = value2 + 1;\n";

struct Editor {
    ctx: egui::Context,
    app: AppIde,
    pass: u64,
}

impl Editor {
    fn open() -> Self {
        let ctx = egui::Context::default();
        let mut app = AppIde::new(
            &eframe::CreationContext::_new_kittest(ctx.clone()),
            None,
            None,
        );
        app.project_tree.user_src_files.clear();
        app.project_tree
            .user_src_files
            .push(("src/main.rs".into(), RUST.into()));
        app.selected_file = ProjectFileId::UserFile(0);
        let mut ed = Self { ctx, app, pass: 0 };
        ed.idle(4);
        ed
    }

    fn step(&mut self, events: Vec<egui::Event>) {
        self.pass += 1;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            time: Some(self.pass as f64 / 30.0),
            predicted_dt: 1.0 / 30.0,
            focused: true,
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        let _ = crate::headless::run_ui(&self.ctx, input, |ui| {
            let file = ProjectFileId::UserFile(0);
            let (path, code) = app.project_tree.user_src_files[0].clone();
            let syntax = file.syntax(&path);
            let manifest = file.is_cargo_manifest(&path);
            app.show_code_view(
                ui,
                EditorSlot::Main,
                file,
                code,
                &syntax,
                manifest,
                true,
                None,
            );
        });
    }

    fn idle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.step(vec![]);
        }
    }

    fn editor_id(&self) -> egui::Id {
        self.app.ed.editor_widget_id.expect("the editor was drawn")
    }

    fn focused(&self) -> Option<egui::Id> {
        self.ctx.memory(|m| m.focused())
    }

    fn text(&self) -> String {
        self.app.project_tree.user_src_files[0].1.clone()
    }

    fn enter(pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// One Enter keystroke, press and release in the same frame.
    fn press_enter(&mut self) {
        self.step(vec![Self::enter(true, false), Self::enter(false, false)]);
    }

    /// Enter held down: the press, then a key repeat in each following frame.
    fn hold_enter(&mut self, repeats: usize) {
        self.step(vec![Self::enter(true, false)]);
        for _ in 0..repeats {
            self.step(vec![Self::enter(true, true)]);
        }
        self.step(vec![Self::enter(false, false)]);
    }

    fn ctrl_h(&mut self) {
        let event = |pressed| egui::Event::Key {
            key: egui::Key::H,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        };
        self.step(vec![event(true), event(false)]);
        self.idle(3);
    }

    fn type_text(&mut self, text: &str) {
        self.step(vec![egui::Event::Text(text.into())]);
        self.idle(1);
    }

    /// Click into the editor on the first line, which is the word `value`.
    fn click_on_value(&mut self) {
        let r = self
            .ctx
            .read_response(self.editor_id())
            .expect("a response")
            .rect;
        let p = r.left_top() + egui::vec2(60.0, 8.0);
        let button = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.step(vec![egui::Event::PointerMoved(p), button(true)]);
        self.step(vec![button(false)]);
        self.idle(3);
        assert_eq!(
            self.focused(),
            Some(self.editor_id()),
            "the click focused the editor"
        );
    }

    /// Tap into the code - press and release in one frame, as a touchpad
    /// delivers them - and press Enter in the very next frame. That Enter
    /// belongs to the code, which keeps the keyboard.
    fn tap_into_the_code_then_enter(&mut self) {
        let at = self
            .ctx
            .read_response(self.editor_id())
            .expect("a response")
            .rect
            .left_top()
            + egui::vec2(60.0, 8.0);
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.step(vec![
            egui::Event::PointerMoved(at),
            button(true),
            button(false),
        ]);
        self.press_enter();
        self.idle(3);
        assert!(!self.text().contains("value2"), "{:?}", self.text());
        assert_eq!(
            self.focused(),
            Some(self.editor_id()),
            "the code keeps the keyboard"
        );
    }
}

/// The quick rename: caret on `value`, Ctrl+H opens Replace pre-filled with it
/// and focuses the replacement field, and `2` is typed there. Returns the
/// replacement field's id.
fn typing_in_the_replace_field() -> (Editor, egui::Id) {
    let mut ed = Editor::open();
    ed.click_on_value();
    ed.ctrl_h();
    let field = ed.focused().expect("the replacement field took the focus");
    assert_ne!(field, ed.editor_id());
    ed.type_text("2");
    assert_eq!(
        (
            ed.app.ed.find.query.as_str(),
            ed.app.ed.find.replace.as_str()
        ),
        ("value", "value2")
    );
    (ed, field)
}

#[test]
fn an_enter_in_the_next_frame_does_not_replay_replace_all() {
    let (mut ed, _) = typing_in_the_replace_field();
    ed.press_enter();
    ed.press_enter();
    ed.idle(3);
    assert_eq!(
        ed.text(),
        ONCE,
        "the second frame's Enter ran Replace All again"
    );
}

/// The replacement field lets the focus go on Enter, as it did on egui 0.34:
/// a later Enter is not a second Replace All.
#[test]
fn a_later_enter_does_not_replace_again() {
    let (mut ed, _) = typing_in_the_replace_field();
    ed.press_enter();
    ed.idle(3);
    ed.press_enter();
    ed.idle(3);
    assert_eq!(ed.text(), ONCE);
}

/// Focus moved from the replacement field into the code by a click, then
/// Enter in the very next frame: that Enter belongs to the code. On egui 0.36
/// the field still reported `lost_focus`, so it ran Replace All too.
#[test]
fn an_enter_right_after_clicking_into_the_code_is_not_a_replace_all() {
    let (mut ed, _) = typing_in_the_replace_field();
    ed.tap_into_the_code_then_enter();
}

/// The same from the find field, which would also have pulled the focus back
/// out of the code after its Replace All.
#[test]
fn an_enter_right_after_clicking_from_the_find_field_is_not_a_replace_all() {
    let mut ed = Editor::open();
    // Nothing clicked, so no word under the caret: Ctrl+H focuses the find
    // field.
    ed.ctrl_h();
    ed.type_text("value");
    ed.app.ed.find.replace = "value2".into();
    ed.tap_into_the_code_then_enter();
}

#[test]
fn a_held_enter_in_the_replace_field_replaces_once() {
    let (mut ed, _) = typing_in_the_replace_field();
    ed.hold_enter(6);
    ed.idle(3);
    assert_eq!(ed.text(), ONCE, "a key repeat ran Replace All again");
}

/// The find field kept the focus after Enter all along, so its repeats have
/// always reached it.
#[test]
fn a_held_enter_in_the_find_field_replaces_once() {
    let mut ed = Editor::open();
    // Nothing clicked, so no word under the caret: Ctrl+H focuses the find
    // field.
    ed.ctrl_h();
    ed.type_text("value");
    assert_eq!(ed.app.ed.find.query, "value");
    assert!(
        ed.app.ed.find.replace.is_empty(),
        "the find field was focused"
    );
    ed.app.ed.find.replace = "value2".into();
    ed.hold_enter(6);
    ed.idle(3);
    assert_eq!(ed.text(), ONCE, "a key repeat ran Replace All again");
}
