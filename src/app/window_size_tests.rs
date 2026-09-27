//! Every window sized in numbers gives those numbers to its CONTENT.
//!
//! egui 0.35 made `Window::default_size`, `fixed_size`, `default_width` and the
//! rest OUTER sizes, so after the upgrade each of these windows quietly lost
//! its frame and title bar out of the number - 18 px of width, 51 of height.
//! The datasheet import's paste and prompt boxes, the Publish dialog's
//! metadata boxes and the pin info text all fill the content width, so each
//! came out narrower than it was laid out for. These measure what the content
//! actually gets, through the code each window is built with.

use super::AppIde;
use eframe::egui;

const SCREEN: egui::Vec2 = egui::vec2(1366.0, 768.0);

fn input(pass: u64) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
        time: Some(pass as f64 / 60.0),
        ..Default::default()
    }
}

/// The app, and a context styled the way it styles one: the window frame and
/// the heading font the title bar is made of are the app's.
fn new_app() -> (egui::Context, AppIde) {
    let ctx = egui::Context::default();
    let mut app = AppIde::new(
        &eframe::CreationContext::_new_kittest(ctx.clone()),
        None,
        None,
    );
    app.startup_picker = None;
    (ctx, app)
}

/// The width the first text starting with `prefix` was wrapped to. A label
/// wraps to what its `Ui` has left, so a window's first line gives away its
/// content width.
fn wrap_width(shapes: &[egui::epaint::ClippedShape], prefix: &str) -> Option<f32> {
    fn walk(s: &egui::Shape, prefix: &str) -> Option<f32> {
        match s {
            egui::Shape::Text(t) if t.galley.text().starts_with(prefix) => {
                Some(t.galley.job.wrap.max_width)
            }
            egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, prefix)),
            _ => None,
        }
    }
    shapes.iter().find_map(|c| walk(&c.shape, prefix))
}

#[test]
fn a_framed_dialog_gives_its_content_the_size_it_asks_for() {
    // What the MCU form, the datasheet import and the clock-tree import ask
    // `window_frame` for, with their anchors.
    let dialogs = [
        (680.0, 560.0, 0.0),
        (560.0, 540.0, 30.0),
        (520.0, 440.0, 40.0),
    ];
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN);
    for (w, h, anchor_y) in dialogs {
        // Opened the way the dialogs open - one forced frame, then released -
        // and with no force at all.
        for forced in [true, false] {
            let (ctx, _app) = new_app();
            for pass in 0..6 {
                let mut content = egui::Rect::NOTHING;
                let mut outer = egui::Rect::NOTHING;
                let force = forced && pass == 0;
                let _ = crate::headless::run_ui(&ctx, input(pass), |ui| {
                    let frame = super::datasheet_import_dialog::window_frame(
                        ui.ctx(),
                        "Dialog",
                        false,
                        force,
                        w,
                        h,
                        anchor_y,
                    );
                    outer = frame
                        .show(ui.ctx(), |ui| {
                            content = ui.max_rect();
                            // Filled, so the window is as big as the size makes it.
                            ui.allocate_space(ui.available_size());
                        })
                        .expect("open")
                        .response
                        .rect;
                });
                // A new window spends its first frame measuring itself.
                if pass == 0 {
                    continue;
                }
                assert!(
                    (content.size() - egui::vec2(w, h)).length() < 0.5,
                    "{w}x{h} (forced {forced}, pass {pass}): the content got {:?}",
                    content.size()
                );
                assert!(
                    screen.contains_rect(outer),
                    "{w}x{h}: the window {outer:?} does not fit a {SCREEN:?} screen"
                );
            }
        }
    }
    // Maximized is the whole WINDOW on the screen, not its content.
    let (ctx, _app) = new_app();
    let mut outer = egui::Rect::NOTHING;
    for pass in 0..3 {
        let _ = crate::headless::run_ui(&ctx, input(pass), |ui| {
            outer = super::datasheet_import_dialog::window_frame(
                ui.ctx(),
                "Dialog",
                true,
                false,
                680.0,
                560.0,
                0.0,
            )
            .show(ui.ctx(), |ui| {
                ui.allocate_space(ui.available_size());
            })
            .expect("open")
            .response
            .rect;
        });
    }
    assert_eq!(outer, screen.shrink(12.0), "maximized");
}

#[test]
fn the_publish_dialog_content_is_as_wide_as_it_asks() {
    let (ctx, mut app) = new_app();
    let manifest = "[package]\nname = \"radar\"\nversion = \"0.1.0\"\n";
    app.project_tree
        .user_src_files
        .push(("radar/Cargo.toml".to_owned(), manifest.to_owned()));
    app.publish_dialog = Some(super::publish_dialog::PublishDialog::new(
        "radar".to_owned(),
        manifest,
        vec![crate::publish_target::Target::CratesIo],
    ));
    let mut shapes = Vec::new();
    for pass in 0..4 {
        shapes =
            crate::headless::run_ui(&ctx, input(pass), |ui| app.show_publish_dialog(ui)).shapes;
    }
    // Only the width: the dialog's content is taller than the 520 it asks for,
    // so its height is the content's either way.
    let w = wrap_width(&shapes, "Check, preview, rehearse").expect("the dialog is drawn");
    assert!(
        (w - 620.0).abs() < 0.5,
        "the Publish dialog asks for 620 px of content and got {w}"
    );
}

#[test]
fn the_pin_info_popup_content_is_as_wide_as_it_asks() {
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
    let (ctx, _app) = new_app();
    // One whose specs fit in 340: a longer row widens the window to itself.
    let func = PinFunction::I2cScl(1);
    let chip = egui::Rect::from_min_size(egui::pos2(500.0, 400.0), egui::vec2(10.0, 10.0));
    let mut shapes = Vec::new();
    for pass in 0..4 {
        shapes = crate::headless::run_ui(&ctx, input(pass), |ui| {
            crate::panels::mcu_module::mcu::gui::info::draw_info_popup(&func, chip, ui, None);
        })
        .shapes;
    }
    let w = wrap_width(&shapes, &func.info().description).expect("the description is drawn");
    assert!(
        (w - 340.0).abs() < 0.5,
        "the pin info popup asks for 340 px of content and got {w}"
    );
}
