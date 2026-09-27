//! Enter in a single-line text field.

use eframe::egui;

/// Whether Enter ended editing in `field` THIS frame.
///
/// A single-line field lets the focus go on Enter, so a form reads Enter as
/// `lost_focus() && Enter`. Since egui 0.36 `lost_focus` stays true for a
/// second frame, and after the focus moved to another widget too, so an Enter
/// in the next frame - a quick second one, or the first one typed into the
/// code just clicked into - acted on the field again: Replace All ran twice,
/// and the Terminal ran a half-typed command. Only a field that had the focus
/// in the frame before this one ended with THIS Enter, which is what
/// `lost_focus` meant in egui 0.34.
pub fn ended_with_enter(ui: &egui::Ui, field: &egui::Response) -> bool {
    field.lost_focus()
        && ui.memory(|m| m.had_focus_last_frame(field.id))
        && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-line field above a multi-line one, as the Terminal's command
    /// line sits beside the code editor.
    struct Form {
        ctx: egui::Context,
        pass: u64,
        line: String,
        code: String,
        line_rect: egui::Rect,
        code_rect: egui::Rect,
        /// Frames in which the one-line field reported an Enter.
        entered: Vec<u64>,
    }

    impl Form {
        fn new() -> Self {
            let mut f = Self {
                ctx: egui::Context::default(),
                pass: 0,
                line: String::new(),
                code: String::new(),
                line_rect: egui::Rect::NOTHING,
                code_rect: egui::Rect::NOTHING,
                entered: Vec::new(),
            };
            f.step(vec![]);
            f
        }

        fn step(&mut self, events: Vec<egui::Event>) {
            self.pass += 1;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                time: Some(self.pass as f64 / 30.0),
                predicted_dt: 1.0 / 30.0,
                focused: true,
                events,
                ..Default::default()
            };
            let (line, code) = (&mut self.line, &mut self.code);
            let (mut line_rect, mut code_rect, mut entered) =
                (egui::Rect::NOTHING, egui::Rect::NOTHING, false);
            let _ = crate::headless::run_ui(&self.ctx, input, |ui| {
                let r = ui.text_edit_singleline(line);
                entered = ended_with_enter(ui, &r);
                line_rect = r.rect;
                code_rect = ui.text_edit_multiline(code).rect;
            });
            (self.line_rect, self.code_rect) = (line_rect, code_rect);
            if entered {
                self.entered.push(self.pass);
            }
        }

        fn tap(&mut self, at: egui::Pos2) {
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
        }

        fn enter(&mut self) {
            let key = |pressed| egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            self.step(vec![key(true), key(false)]);
        }
    }

    #[test]
    fn enter_in_the_field_is_its_enter() {
        let mut f = Form::new();
        f.tap(f.line_rect.center());
        f.step(vec![egui::Event::Text("echo".into())]);
        f.enter();
        let at = f.pass;
        f.step(vec![]);
        f.step(vec![]);
        assert_eq!(
            f.entered,
            vec![at],
            "exactly once, in the frame of the Enter"
        );
    }

    /// The field lost the focus to a click elsewhere; the Enter that follows
    /// at once is typed there, not into the field.
    #[test]
    fn an_enter_right_after_clicking_elsewhere_is_not_the_fields() {
        let mut f = Form::new();
        f.tap(f.line_rect.center());
        f.step(vec![egui::Event::Text("echo".into())]);
        f.tap(f.code_rect.center());
        f.enter();
        f.step(vec![]);
        assert!(
            f.entered.is_empty(),
            "the field took an Enter: {:?}",
            f.entered
        );
    }

    /// A second Enter in the very next frame is not a second Enter of the
    /// field's: it let the focus go with the first.
    #[test]
    fn a_quick_second_enter_is_not_the_fields() {
        let mut f = Form::new();
        f.tap(f.line_rect.center());
        f.step(vec![egui::Event::Text("echo".into())]);
        f.enter();
        f.enter();
        f.step(vec![]);
        assert_eq!(f.entered.len(), 1, "{:?}", f.entered);
    }

    /// Every Enter a text field acts on goes through [`ended_with_enter`]: the
    /// bare `lost_focus() && Enter` also takes an Enter meant for whatever got
    /// the focus next.
    #[test]
    fn every_field_enter_goes_through_the_helper() {
        fn scan(dir: &std::path::Path, exempt: &std::path::Path, found: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    scan(&p, exempt, found);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs") || p == exempt {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                let code: Vec<&str> = text
                    .lines()
                    .map(|l| l.split("//").next().unwrap_or(""))
                    .collect();
                for (i, line) in code.iter().enumerate() {
                    if !line.contains("lost_focus()") {
                        continue;
                    }
                    // The statement it is in: this line and the few after it.
                    let near = code[i..(i + 4).min(code.len())].concat();
                    let enter_after = near.contains("Key::Enter");
                    // `enter |= a.lost_focus();` ... `if enter && Enter`.
                    let folded = line.contains("|=") && text.contains("Key::Enter");
                    if enter_after || folded {
                        found.push(format!("{}:{}", p.display(), i + 1));
                    }
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        scan(
            &src,
            &src.join("app").join("helpers").join("text_field.rs"),
            &mut found,
        );
        assert!(
            found.is_empty(),
            "use crate::app::helpers::text_field::ended_with_enter here:\n{}",
            found.join("\n")
        );
    }
}
