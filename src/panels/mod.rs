pub mod board;
pub mod flow_map;
pub mod mcu_module;
pub mod structure_map;

use eframe::egui;

/// Whether the pointer is really dragging: it moved past the click distance,
/// or the button was held past the click time.
///
/// Every canvas item that a `Sense::click_and_drag` widget MOVES waits for
/// this. egui 0.36 starts that widget's drag the moment the pointer leaves it,
/// so a click on an item's edge with a pixel or two of hand movement moved
/// it, and an item that keeps a moved position as a manual placement stayed
/// pinned there. egui 0.34 waited for exactly this.
///
/// Not for telling a drag's END from a slipped click's: on the release frame
/// egui forgets the press time, so a slow drag held past the click time but
/// moved less than the click distance reads as undecided there. A slipped
/// click reports `clicked()` on release; a real drag never does.
pub fn drag_decided(ui: &egui::Ui) -> bool {
    ui.input(|i| i.pointer.is_decidedly_dragging())
}

/// `egui::DragValue::new(value)` that a slipped click cannot change.
///
/// A `DragValue` is a `Sense::click_and_drag` widget as well, so since egui
/// 0.36 a click on it whose hand drifts off it DRAGS it, and the value moves
/// by the drift times the speed without a word: a click on the Clock tab's
/// 8 MHz crystal left it at 8.3 MHz, and main.rs was generated from that.
/// Here a change made while a drag is not yet [decided](drag_decided) never
/// reaches `value`. A real drag, a typed value and the arrow keys change it as
/// before; the drag catches up with the pointer once it is decided, since the
/// widget keeps its own running total.
pub fn drag_value<'a, N: egui::emath::Numeric>(
    ui: &egui::Ui,
    value: &'a mut N,
) -> egui::DragValue<'a> {
    let ctx = ui.ctx().clone();
    let widget = egui::DragValue::from_get_set(move |new| {
        if let Some(v) = new {
            // The drag in progress is this field's own only if the dragged
            // widget is NOT the focused one. A text field takes the focus the
            // moment it is pressed, and the typed value of a field left for
            // it is written a frame later - by when the hand may have slipped
            // off that text field, starting ITS drag. A value field's button
            // takes no focus from a press.
            let slipping = ctx.dragged_id().is_some_and(|dragged| {
                ctx.memory(|m| m.focused()) != Some(dragged)
                    && !ctx.input(|i| i.pointer.is_decidedly_dragging())
            });
            if !slipping {
                *value = N::from_f64(v);
            }
        }
        value.to_f64()
    });
    // What `DragValue::new` sets up for an integer.
    if N::INTEGRAL {
        widget.max_decimals(0).range(N::MIN..=N::MAX).speed(0.25)
    } else {
        widget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value field beside a label that senses clicks - what a slip lands on
    /// in the real panels - drawn frame by frame.
    struct Field {
        ctx: egui::Context,
        pass: u64,
        value: f64,
        rect: egui::Rect,
    }

    impl Field {
        fn new() -> Self {
            let mut f = Self {
                ctx: egui::Context::default(),
                pass: 0,
                value: 8.0,
                rect: egui::Rect::NOTHING,
            };
            f.step(vec![]);
            f.step(vec![]);
            f
        }

        fn step(&mut self, events: Vec<egui::Event>) {
            self.pass += 1;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 200.0),
                )),
                time: Some(self.pass as f64 / 30.0),
                predicted_dt: 1.0 / 30.0,
                events,
                ..Default::default()
            };
            let (value, mut rect) = (&mut self.value, egui::Rect::NOTHING);
            let _ = crate::headless::run_ui(&self.ctx, input, |ui| {
                ui.add(egui::Label::new("crystal").sense(egui::Sense::click()));
                rect = ui.add(drag_value(ui, value).speed(0.1)).rect;
                ui.add(egui::Label::new("MHz").sense(egui::Sense::click()));
            });
            self.rect = rect;
        }

        /// Press 1 px inside the field's top edge, move by `by` in two frames,
        /// release there.
        fn press_move_release(&mut self, by: egui::Vec2) {
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let at = egui::pos2(self.rect.center().x, self.rect.top() + 1.0);
            self.step(vec![egui::Event::PointerMoved(at)]);
            self.step(vec![button(at, true)]);
            let mut p = at;
            for _ in 0..2 {
                p += by / 2.0;
                self.step(vec![egui::Event::PointerMoved(p)]);
            }
            self.step(vec![button(p, false)]);
            self.step(vec![]);
        }
    }

    #[test]
    fn a_click_that_slips_off_a_value_leaves_it_alone() {
        let mut f = Field::new();
        f.press_move_release(egui::vec2(0.0, -3.0));
        assert_eq!(f.value, 8.0, "a slipped click changed the value");
    }

    #[test]
    fn a_real_drag_still_changes_the_value() {
        let mut f = Field::new();
        f.press_move_release(egui::vec2(30.0, 0.0));
        assert!(f.value > 8.0, "a 30 px drag right raised it: {}", f.value);
    }

    /// A value typed into a field that only takes it when editing ends (the
    /// Custom baud), ended by a click into a text field drawn after it whose
    /// hand slips onto the label below: the typed value stays. That text field is being dragged,
    /// not the value field, so it is no slip of the value's.
    #[test]
    fn a_typed_value_survives_a_slipped_click_into_a_later_text_field() {
        struct Form {
            ctx: egui::Context,
            pass: u64,
            value: f64,
            note: String,
            field: egui::Rect,
            text: egui::Rect,
        }
        impl Form {
            fn step(&mut self, events: Vec<egui::Event>) {
                self.pass += 1;
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 200.0),
                    )),
                    time: Some(self.pass as f64 / 30.0),
                    predicted_dt: 1.0 / 30.0,
                    events,
                    ..Default::default()
                };
                let (value, note) = (&mut self.value, &mut self.note);
                let (mut field, mut text) = (egui::Rect::NOTHING, egui::Rect::NOTHING);
                let _ = crate::headless::run_ui(&self.ctx, input, |ui| {
                    field = ui
                        .add(drag_value(ui, value).update_while_editing(false))
                        .rect;
                    text = ui.text_edit_singleline(note).rect;
                    ui.add(egui::Label::new("parity").sense(egui::Sense::click()));
                });
                (self.field, self.text) = (field, text);
            }
        }
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut f = Form {
            ctx: egui::Context::default(),
            pass: 0,
            value: 8.0,
            note: String::new(),
            field: egui::Rect::NOTHING,
            text: egui::Rect::NOTHING,
        };
        f.step(vec![]);
        f.step(vec![]);
        // Click the field to type into it; the click selects its text.
        let at = f.field.center();
        f.step(vec![egui::Event::PointerMoved(at), button(at, true)]);
        f.step(vec![button(at, false)]);
        f.step(vec![]);
        f.step(vec![egui::Event::Text("12.5".into())]);
        f.step(vec![]);
        assert_eq!(f.value, 8.0, "not taken while still editing");
        // End the edit with a click on the text field whose hand slips 5 px
        // down onto the label under it in the next frame - the frame the value
        // field learns it lost the focus and takes the typed text.
        let at = egui::pos2(f.text.left() + 10.0, f.text.bottom() - 1.0);
        f.step(vec![egui::Event::PointerMoved(at), button(at, true)]);
        let p = at + egui::vec2(0.0, 5.0);
        f.step(vec![egui::Event::PointerMoved(p)]);
        f.step(vec![button(p, false)]);
        f.step(vec![]);
        assert_eq!(f.value, 12.5, "the typed value was dropped");
    }

    /// Integers keep `DragValue::new`'s whole-number steps and range.
    #[test]
    fn an_integer_field_keeps_whole_numbers() {
        let ctx = egui::Context::default();
        let mut n: u8 = 7;
        let _ = crate::headless::run_ui(&ctx, Default::default(), |ui| {
            ui.add(drag_value(ui, &mut n));
        });
        assert_eq!(n, 7);
    }

    /// Every value field goes through [`drag_value`]: one made with
    /// `DragValue::new` directly takes a slipped click as an edit.
    #[test]
    fn every_value_field_is_made_by_drag_value() {
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
                for (i, line) in text.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    if code.contains("DragValue::new(") || code.contains("DragValue::from_get_set(")
                    {
                        found.push(format!("{}:{}", p.display(), i + 1));
                    }
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        scan(&src, &src.join("panels").join("mod.rs"), &mut found);
        assert!(
            found.is_empty(),
            "make these with crate::panels::drag_value:\n{}",
            found.join("\n")
        );
    }
}
