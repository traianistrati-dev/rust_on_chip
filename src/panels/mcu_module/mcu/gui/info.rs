//! Information popup window — displays detailed specifications for pin functions.

use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use crate::panels::mcu_module::pins::logic::pin_function::info::MAX_BAUD_KEY;
use eframe::egui;

/// Render the info popup window for a pin function.
/// Returns `true` if the window should stay open, `false` if closed.
///
/// `max_baud` replaces the value of the [`MAX_BAUD_KEY`] row. `PinFunction::info`
/// takes no chip, so on its own that row can only say what the number depends
/// on; the caller, which has the `Mcu`, supplies the number itself.
pub fn draw_info_popup(
    func: &PinFunction,
    chip_rect: egui::Rect,
    ui: &mut egui::Ui,
    max_baud: Option<String>,
) -> bool {
    let mut info = func.info();
    if let Some(v) = max_baud {
        for (key, value) in &mut info.specs {
            if key == MAX_BAUD_KEY {
                *value = v.clone();
            }
        }
    }
    let mut open = true;

    let popup_pos = egui::pos2(chip_rect.center().x - 170.0, chip_rect.center().y - 100.0);

    egui::Window::new(format!("{}", func.label()))
        .open(&mut open)
        .resizable(true)
        // 340 is the content's width; egui takes the window's.
        .default_width(340.0 + crate::app::helpers::window_size::chrome(ui.ctx()).x)
        .default_pos(popup_pos)
        .show(ui.ctx(), |ui| {
            // Description
            ui.label(
                egui::RichText::new(&info.description)
                    .size(14.0)
                    .color(egui::Color32::from_rgb(255, 255, 255)),
            );

            if !info.specs.is_empty() {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Specs grid
                egui::Grid::new("info_specs_grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for (key, value) in &info.specs {
                            ui.label(
                                egui::RichText::new(key)
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(150, 150, 250)),
                            );
                            ui.label(
                                egui::RichText::new(value)
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(155, 155, 155)),
                            );
                            ui.end_row();
                        }
                    });
            }
        });

    open
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every string the popup paints for `func`, after a few frames - a new
    /// egui window spends its first frame measuring itself.
    fn painted(func: &PinFunction, max_baud: Option<String>) -> Vec<String> {
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 900.0),
                )),
                ..Default::default()
            };
            let rect = egui::Rect::from_min_size(egui::pos2(500.0, 400.0), egui::vec2(10.0, 10.0));
            shapes = crate::headless::run_ui(&ctx, input, |ui| {
                draw_info_popup(func, rect, ui, max_baud.clone());
            })
            .shapes;
        }
        let mut out = Vec::new();
        for s in &shapes {
            walk(&s.shape, &mut out);
        }
        out
    }

    /// The chip's own figure replaces the generic line; without one, the
    /// generic line stays and says what the number depends on.
    #[test]
    fn the_max_baud_row_takes_the_chip_figure() {
        let f = PinFunction::UsartTx(1);
        let with = painted(&f, Some("4 500 000 baud at PCLK2 72 MHz".to_owned()));
        assert!(
            with.iter().any(|t| t == "4 500 000 baud at PCLK2 72 MHz"),
            "{with:?}"
        );
        assert!(
            !with.iter().any(|t| t.starts_with("Set by the clock")),
            "{with:?}"
        );

        let without = painted(&f, None);
        assert!(
            without.iter().any(|t| t.starts_with("Set by the clock")),
            "{without:?}"
        );
    }
}
