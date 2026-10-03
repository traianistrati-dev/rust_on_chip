//! The selected pin's function list — painted INSIDE the chip body.
//!
//! It lives in the chip (over the body, under the pin stubs) and is driven by
//! `Mcu::fn_scroll_offset`: a hand-painted list rather than an egui `ScrollArea`,
//! because it is drawn inside the [`egui::Scene`] of the Pins canvas and must
//! scale with it.
//!
//! Two things the caller must do for it, both because of that Scene:
//! * the pin NUMBERS around the body are hidden while it is open (they are
//!   painted at the same place and would show through the rows) — see
//!   [`super::chip`];
//! * the wheel over it must scroll it instead of zooming the canvas. It cannot
//!   do that itself: `mcu_panel.rs` intercepts the wheel BEFORE this runs, so it
//!   is the one that feeds `fn_scroll_offset`. This function hands back its rect
//!   in SCREEN coordinates for exactly that test.

use super::info;
use crate::panels::mcu_module::mcu::model::Mcu;
use crate::panels::mcu_module::pins::logic::pin::GpioMode;
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use eframe::egui;

/// Width of the trailing ⓘ button.
const INFO_BTN_W: f32 = 22.0;
/// Gap between a function button and its ⓘ.
const GAP: f32 = 4.0;
/// Height of one function button, and the pitch between two rows.
const BTN_H: f32 = 28.0;
const ITEM_H: f32 = BTN_H + 6.0;
/// Scrollbar width + its gap from the buttons.
const SB_W: f32 = 4.0;
const SB_GAP: f32 = 3.0;
/// Height of a GPIO-mode chip (the row under the active function).
const MODE_H: f32 = 19.0;
/// Side of the square close button in the list's top-right corner.
const CLOSE_W: f32 = 18.0;
/// A board's note on a pad: size and colour.
const NOTE_PT: f32 = 11.0;
const NOTE_COLOR: egui::Color32 = egui::Color32::from_rgb(230, 205, 130);

/// Paint `text` wrapped to `wrap` at `pos`, cut to the rows that fit in
/// `max_h` and ended with "…" when cut, the full text then on hover. Returns
/// the painted height.
///
/// A board note can run to six lines (the nRF54L15 DK's P2.07), and the chip
/// body shrinks with the package and with rotation: uncapped, the note pushed
/// the function list down to a sliver.
fn draw_note(
    painter: &egui::Painter,
    ui: &mut egui::Ui,
    text: String,
    pos: egui::Pos2,
    wrap: f32,
    max_h: f32,
    id: egui::Id,
) -> f32 {
    let font = egui::FontId::proportional(NOTE_PT);
    let full = painter.layout(text.clone(), font.clone(), NOTE_COLOR, wrap);
    let galley = if full.size().y <= max_h || full.rows.is_empty() {
        full
    } else {
        let row_h = full.size().y / full.rows.len() as f32;
        let mut job = egui::text::LayoutJob::simple(text.clone(), font, NOTE_COLOR, wrap);
        // At least one row, so a cramped body still shows there IS a note.
        job.wrap.max_rows = ((max_h / row_h).floor() as usize).max(1);
        painter.layout_job(job)
    };
    painter.galley(pos, galley.clone(), egui::Color32::WHITE);
    if galley.elided {
        ui.interact(
            egui::Rect::from_min_size(pos, galley.size()),
            id,
            egui::Sense::hover(),
        )
        .on_hover_ui(|ui| {
            ui.set_max_width(360.0);
            ui.label(text);
        });
    }
    galley.size().y
}

/// `rect` from the canvas' Scene out to the screen, where the caller's wheel
/// test and the ⓘ window live. Scene coords ≠ screen coords as soon as the
/// user zooms or pans.
fn to_screen(ui: &egui::Ui, rect: egui::Rect) -> egui::Rect {
    ui.ctx()
        .layer_transform_to_global(ui.layer_id())
        .unwrap_or_default()
        * rect
}

/// The close button in the header's top-right corner: paints it, returns whether
/// it was clicked.
///
/// The ✕ is drawn with two strokes rather than set as text — everything else in
/// this list is painted (it scales with the canvas' `Scene`), and a glyph would
/// also have to survive the font fallback the rest of the UI works around.
fn draw_close(painter: &egui::Painter, ui: &mut egui::Ui, rect: egui::Rect, num: usize) -> bool {
    let resp = ui.interact(rect, ui.id().with(("fn_close", num)), egui::Sense::click());
    let (bg, fg) = if resp.hovered() {
        (egui::Color32::from_rgb(150, 60, 60), egui::Color32::WHITE)
    } else {
        (
            egui::Color32::from_rgb(55, 55, 75),
            egui::Color32::from_rgb(200, 200, 215),
        )
    };
    painter.rect_filled(rect, 4.0, bg);
    let c = rect.center();
    let r = 4.5;
    let stroke = egui::Stroke::new(1.6_f32, fg);
    painter.line_segment(
        [egui::pos2(c.x - r, c.y - r), egui::pos2(c.x + r, c.y + r)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(c.x - r, c.y + r), egui::pos2(c.x + r, c.y - r)],
        stroke,
    );
    resp.clicked()
}

/// The functions offered for `num`: everything the pin can do, minus what is
/// already taken by another pin (GPIO In/Out are always offered — any pin can be
/// one).
fn selectable_functions(mcu: &Mcu, num: usize) -> Option<(String, Vec<PinFunction>, PinFunction)> {
    let used_elsewhere: Vec<PinFunction> = mcu
        .iter_all_pins()
        .filter(|p| p.number != num && p.selected_function != PinFunction::Unset)
        .map(|p| p.selected_function.clone())
        .collect();
    let pin = mcu.find_pin(num)?;
    let mut funcs: Vec<PinFunction> = pin
        .available_functions
        .iter()
        .filter(|f| {
            matches!(f, PinFunction::GpioInput | PinFunction::GpioOutput)
                || !used_elsewhere.contains(f)
        })
        .cloned()
        .collect();
    // The pin's CURRENT function always heads the list: it is the one the user
    // came to see (and to click again to clear), and on a long list it would
    // otherwise sit scrolled out of sight.
    if let Some(i) = funcs.iter().position(|f| *f == pin.selected_function) {
        let cur = funcs.remove(i);
        funcs.insert(0, cur);
    }
    Some((pin.name.clone(), funcs, pin.selected_function.clone()))
}

/// Draw the selected pin's header + function list inside `content_rect` (the
/// upright area of the chip body, scene coords).
///
/// Returns the `(number, name, function)` change when the user picks one, plus
/// the list's rect in SCREEN coordinates — [`egui::Rect::NOTHING`] when no pin is
/// selected, i.e. when nothing was drawn.
/// The panel body rect, for the wheel routing that the caller installs.
///
/// The reserved branch returns early, before the list geometry exists, but
/// the caller still needs a rect or the mouse wheel over the panel would zoom
/// the canvas underneath it.
fn list_rect_of(content_rect: egui::Rect, sep_y: f32) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(content_rect.left() + 8.0, sep_y + 12.0),
        egui::pos2(content_rect.right() - 8.0, content_rect.bottom() - 8.0),
    )
}

pub fn draw_pin_functions(
    mcu: &mut Mcu,
    painter: &egui::Painter,
    ui: &mut egui::Ui,
    content_rect: egui::Rect,
) -> (Option<(usize, String, PinFunction)>, egui::Rect) {
    let Some(num) = mcu.selected_pin else {
        return (None, egui::Rect::NOTHING);
    };
    let Some((pin_name, funcs, selected_func)) = selectable_functions(mcu, num) else {
        return (None, egui::Rect::NOTHING);
    };

    // ── Header ───────────────────────────────────────────────────────────────
    let header_pos = content_rect.center_top() + egui::vec2(0.0, 14.0);
    painter.text(
        header_pos,
        egui::Align2::CENTER_CENTER,
        format!("Pin {num}  ·  {pin_name}"),
        egui::FontId::proportional(13.0),
        egui::Color32::WHITE,
    );
    // Close, top-right of the header — the same "focus nothing" as clicking the
    // pin again or clicking empty canvas. Applied at the end of the function so
    // this frame still draws a complete list.
    let close_rect = egui::Rect::from_min_size(
        egui::pos2(
            content_rect.right() - 8.0 - CLOSE_W,
            content_rect.top() + 6.0,
        ),
        egui::vec2(CLOSE_W, CLOSE_W),
    );
    let close_clicked = draw_close(painter, ui, close_rect, num);
    let sep_y = header_pos.y + 14.0;
    painter.line_segment(
        [
            egui::pos2(content_rect.left() + 8.0, sep_y),
            egui::pos2(content_rect.right() - 8.0, sep_y),
        ],
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(100, 100, 120)),
    );

    // ── Reserved pins: a description, not a list ─────────────────────────
    // They have no selectable functions, so the list machinery below would
    // draw an empty box under the header. Say what the pin IS instead - which
    // is the only question a power rail can answer.
    if mcu.find_pin(num).is_some_and(|p| p.reserved) {
        let role = crate::panels::mcu_module::pins::logic::pin::colors::reserved_role(&pin_name);
        let mut y = sep_y + 18.0;
        let left = content_rect.left() + 12.0;
        let wrap = content_rect.width() - 24.0;
        let galley = painter.layout(
            role.to_owned(),
            egui::FontId::proportional(11.5),
            egui::Color32::from_rgb(210, 214, 226),
            wrap,
        );
        painter.galley(egui::pos2(left, y), galley.clone(), egui::Color32::WHITE);
        y += galley.size().y + 10.0;
        // What the BOARD says about it - a solder bridge that frees the pad,
        // say. Data from the definition, so it is right for this kit. It
        // leaves room for the line under it.
        if let Some(note) = mcu.find_pin(num).map(|p| p.note.clone()).filter(|n| !n.is_empty()) {
            let max_h = content_rect.bottom() - 8.0 - 24.0 - y;
            let h = draw_note(
                painter,
                ui,
                note,
                egui::pos2(left, y),
                wrap,
                max_h,
                ui.id().with(("pad_note", num)),
            );
            y += h + 10.0;
        }
        painter.text(
            egui::pos2(left, y),
            egui::Align2::LEFT_TOP,
            "Fixed by the package - nothing to configure.",
            egui::FontId::proportional(10.5),
            egui::Color32::from_rgb(140, 144, 158),
        );
        if close_clicked {
            mcu.selected_pin = None;
            mcu.show_info = None;
        }
        return (None, to_screen(ui, list_rect_of(content_rect, sep_y)));
    }

    // The drive / pull modes this backend can generate for the pin's CURRENT
    // function — the row of chips under it. Empty for a peripheral pin (its mode
    // is dictated by the peripheral) or a backend that offers no choice.
    let modes: &[GpioMode] =
        crate::panels::mcu_module::codegen::family::gpio_modes_for(mcu, &selected_func);
    let current_mode = mcu
        .find_pin(num)
        .and_then(|p| p.io_mode)
        .or_else(|| modes.first().copied());
    let mode_row_h = if modes.is_empty() { 0.0 } else { MODE_H + 6.0 };

    // ── A board pad whose function switches something on ────────────────────
    // Its list reads "GPIO Output" and nothing else, which says nothing about
    // what picking it DOES on this board - so the panel says it first.
    // The board's own note on the pad joins it: a solder bridge, a resistor, a
    // pin the debugger drives - things no function in the list can say.
    let mut note_h = 0.0;
    let notes: Vec<&str> = mcu
        .find_pin(num)
        .map(|p| {
            [
                crate::panels::mcu_module::pins::logic::pin::colors::switch_role(&p.name),
                Some(p.note.as_str()).filter(|n| !n.is_empty()),
            ]
            .into_iter()
            .flatten()
            .collect()
        })
        .unwrap_or_default();
    if !notes.is_empty() {
        // The list keeps its first two rows (and the mode chips under the
        // first) in view; the note gets what is left.
        let room = content_rect.bottom() - 8.0 - (sep_y + 12.0);
        let min_list = funcs.len().min(2) as f32 * ITEM_H + mode_row_h;
        let h = draw_note(
            painter,
            ui,
            notes.join("\n\n"),
            egui::pos2(content_rect.left() + 12.0, sep_y + 10.0),
            content_rect.width() - 24.0,
            room - min_list - 10.0,
            ui.id().with(("pad_note", num)),
        );
        note_h = h + 10.0;
    }

    // ── Geometry ─────────────────────────────────────────────────────────────
    let btn_x = content_rect.left() + 12.0;
    let content_top = sep_y + 12.0 + note_h;
    let content_bottom = content_rect.bottom() - 8.0;
    let available_h = (content_bottom - content_top).max(0.0);
    let total_h = funcs.len() as f32 * ITEM_H + mode_row_h;
    let max_scroll = (total_h - available_h).max(0.0);
    mcu.fn_scroll_offset = mcu.fn_scroll_offset.clamp(0.0, max_scroll);
    let btn_w = content_rect.width() - 24.0 - INFO_BTN_W - GAP - SB_W - SB_GAP;

    let list_rect = egui::Rect::from_min_max(
        egui::pos2(btn_x - 4.0, content_top),
        egui::pos2(content_rect.right() - SB_W - SB_GAP - 1.0, content_bottom),
    );

    // ── Scrollbar thumb ──────────────────────────────────────────────────────
    if max_scroll > 0.0 {
        let sb_x = content_rect.right() - SB_W - 2.0;
        let thumb_h = ((available_h / total_h) * available_h).max(16.0);
        let thumb_top = content_top + (mcu.fn_scroll_offset / max_scroll) * (available_h - thumb_h);
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(sb_x, thumb_top), egui::vec2(SB_W, thumb_h)),
            SB_W / 2.0,
            egui::Color32::from_rgba_premultiplied(180, 180, 210, 140),
        );
    }

    // ── Rows ─────────────────────────────────────────────────────────────────
    let list_painter = painter.with_clip_rect(list_rect);
    let mut btn_y = content_top - mcu.fn_scroll_offset;
    let mut new_function: Option<(usize, PinFunction)> = None;
    let mut new_mode: Option<GpioMode> = None;
    let mut toggle_info: Option<PinFunction> = None;
    let show_info = mcu.show_info.clone();

    for (i, func) in funcs.iter().enumerate() {
        let btn_rect =
            egui::Rect::from_min_size(egui::pos2(btn_x, btn_y), egui::vec2(btn_w, BTN_H));
        let info_rect = egui::Rect::from_min_size(
            egui::pos2(btn_x + btn_w + GAP, btn_y),
            egui::vec2(INFO_BTN_W, BTN_H),
        );
        let visible = btn_rect.bottom() > content_top && btn_rect.top() < content_bottom;

        let is_sel = func == &selected_func;
        let bg = if is_sel {
            func.color()
        } else {
            egui::Color32::from_rgb(65, 65, 80)
        };
        list_painter.rect_filled(btn_rect, 5.0, bg);
        list_painter.text(
            btn_rect.center(),
            egui::Align2::CENTER_CENTER,
            func.list_label(),
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );

        // ⓘ button — hand-drawn so it matches the painted list.
        let info_bg = if show_info.as_ref() == Some(func) {
            egui::Color32::from_rgb(80, 120, 200)
        } else {
            egui::Color32::from_rgb(55, 55, 75)
        };
        list_painter.rect_filled(info_rect, 5.0, info_bg);
        let ic = info_rect.center();
        list_painter.circle_stroke(ic, 7.5, egui::Stroke::new(1.5_f32, egui::Color32::WHITE));
        list_painter.circle_filled(egui::pos2(ic.x, ic.y - 2.5), 1.3, egui::Color32::WHITE);
        list_painter.line_segment(
            [egui::pos2(ic.x, ic.y - 0.5), egui::pos2(ic.x, ic.y + 4.0)],
            egui::Stroke::new(1.8_f32, egui::Color32::WHITE),
        );

        // Only rows actually on screen take clicks — a scrolled-away button must
        // not keep a hit area over the chip, and a half-scrolled one only where
        // it is painted: above the list sits the pad's note.
        if visible {
            let btn_response = ui.interact(
                btn_rect.intersect(list_rect),
                ui.id().with(("fn_btn", num, i)),
                egui::Sense::click(),
            );
            if btn_response.hovered() {
                list_painter.rect_stroke(
                    btn_rect,
                    5.0,
                    egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
                    egui::StrokeKind::Middle,
                );
            }
            if btn_response.clicked() {
                // Clicking the ACTIVE function clears the pin — it is a toggle.
                let next = if is_sel {
                    PinFunction::Unset
                } else {
                    func.clone()
                };
                new_function = Some((num, next));
            }

            let info_response = ui.interact(
                info_rect.intersect(list_rect),
                ui.id().with(("info_btn", num, i)),
                egui::Sense::click(),
            );
            if info_response.hovered() {
                list_painter.rect_stroke(
                    info_rect,
                    5.0,
                    egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
                    egui::StrokeKind::Middle,
                );
            }
            if info_response.clicked() {
                toggle_info = Some(func.clone());
            }
        }

        btn_y += ITEM_H;

        // ── Mode chips, directly under the ACTIVE function ───────────────────
        // The active function is always the first row (see `selectable_functions`),
        // so this row sits at the top of the list where it is reachable without
        // scrolling. One chip per mode the backend can actually generate.
        if is_sel && !modes.is_empty() {
            let chip_w = (btn_w - GAP * (modes.len() as f32 - 1.0)) / modes.len() as f32;
            for (j, m) in modes.iter().enumerate() {
                let r = egui::Rect::from_min_size(
                    egui::pos2(btn_x + j as f32 * (chip_w + GAP), btn_y),
                    egui::vec2(chip_w, MODE_H),
                );
                let on = current_mode == Some(*m);
                list_painter.rect_filled(
                    r,
                    4.0,
                    if on {
                        egui::Color32::from_rgb(70, 100, 150)
                    } else {
                        egui::Color32::from_rgb(52, 52, 64)
                    },
                );
                list_painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    m.label(),
                    egui::FontId::proportional(10.0),
                    if on {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::from_rgb(185, 190, 205)
                    },
                );
                if r.bottom() > content_top && r.top() < content_bottom {
                    let resp = ui.interact(
                        r.intersect(list_rect),
                        ui.id().with(("fn_mode", num, j)),
                        egui::Sense::click(),
                    );
                    if resp.hovered() {
                        list_painter.rect_stroke(
                            r,
                            4.0,
                            egui::Stroke::new(1.2_f32, egui::Color32::WHITE),
                            egui::StrokeKind::Middle,
                        );
                    }
                    if resp.clicked() {
                        new_mode = Some(*m);
                    }
                }
            }
            btn_y += mode_row_h;
        }
    }

    // ── Apply ────────────────────────────────────────────────────────────────
    if close_clicked {
        mcu.selected_pin = None;
        mcu.show_info = None;
        mcu.fn_scroll_offset = 0.0;
    }
    // Applying also clears `show_info`, so it runs before the toggle below.
    let mut changed = new_function.and_then(|(n, f)| mcu.apply_pin_function(n, f));
    // A mode change rewrites the pin's `let` line (it is in the state hash), so
    // it is reported as a pin change too — same downstream sync as a function
    // change. Clicking the ACTIVE mode clears it back to the backend default.
    if let Some(m) = new_mode
        && let Some(pin) = mcu.find_pin_mut(num)
    {
        pin.io_mode = if pin.io_mode == Some(m) {
            None
        } else {
            Some(m)
        };
        changed = Some((pin.number, pin.name.clone(), pin.selected_function.clone()));
    }
    if let Some(func) = toggle_info {
        mcu.show_info = if mcu.show_info.as_ref() == Some(&func) {
            None
        } else {
            Some(func)
        };
    }

    // The list rect in screen space, for the caller's wheel test — and the ⓘ
    // window anchors there too, instead of on the scene-space chip rect (which
    // would place it wherever the diagram is).
    let list_screen = to_screen(ui, list_rect);

    if let Some(func) = mcu.show_info.clone()
        && !info::draw_info_popup(
            &func,
            list_screen,
            ui,
            crate::panels::mcu_module::uart_baud::max_baud_text(
                &crate::panels::mcu_module::uart_baud::Chip::of(mcu),
                &func,
            ),
        )
    {
        mcu.show_info = None;
    }

    (changed, list_screen)
}
