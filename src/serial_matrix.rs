//! Serial matrix view — the payload framed by Find start / Find end laid out
//! as a rows × cols grid of N-byte integers. Phase 3 of the Serial monitor.
//!
//! The motivating format: a radar frame of 4 B header + 1280 B data + 4 B
//! tail, where the data is 20 rows × 16 gate energies × u32 — the matrix
//! shows the 20×16 numbers live, one grid per received frame. Row count,
//! values per row and bytes per value are all user fields (min 1), so any
//! fixed-layout binary payload can be inspected the same way.
//!
//! The frame source is [`crate::serial::frame_ranges`] over the RX buffer —
//! the same delimiting the "Between" counter uses; the LAST complete payload
//! is decoded each frame (live), or a frozen snapshot while paused (the live
//! bytes may leave the RX ring while reading numbers).

use eframe::egui;
use egui_phosphor::regular as ph;

/// Values-cell cap actually laid out — a huge rows×cols setting must not
/// allocate hundreds of thousands of egui rects per frame.
const MAX_CELLS: usize = 4096;

/// State of the matrix view (owned by `SerialMonitor`).
pub struct MatrixView {
    /// `true` → the RX area shows the matrix (exclusive with the plotter).
    pub on: bool,
    /// Grid height, in value rows (min 1).
    pub rows: usize,
    /// Values per row (min 1).
    pub cols: usize,
    /// Bytes per value, 1..=8 (4 = u32).
    pub value_bytes: usize,
    /// Payload bytes IGNORED before the first value (min 0) — for frames
    /// whose data is preceded by status/length fields (e.g. 1 status byte +
    /// 2 distance bytes before the gate energies).
    pub skip_bytes: usize,
    /// Little-endian decode (big-endian when false).
    pub le: bool,
    /// Hex display (decimal when false).
    pub hex: bool,
    /// Heatmap cell tint (value vs the frame's max).
    pub heat: bool,
    /// Heat mode switch: 0 = tint by MAGNITUDE (value vs the frame's max, the
    /// original behaviour); N > 0 = tint ONLY cells that GREW ≥ N% since the
    /// previous serial frame — big-but-static values stop glowing, real jumps
    /// stand out.
    pub change_pct: u32,
    /// Freeze the current frame (new payloads are ignored while set).
    pub paused: bool,
    /// Fullscreen: the grid covers the whole window, auto-zoomed so ALL
    /// rows × cols fit the display (Esc or the Normal button returns).
    pub full: bool,
    /// The payload snapshot shown while paused.
    frozen: Option<Vec<u8>>,
    /// The newest SERIAL frame seen (raw payload bytes) — not an egui-repaint
    /// notion: it only advances when different bytes arrive.
    cur_frame: Option<Vec<u8>>,
    /// The frame before `cur_frame` — the `Change %` baseline. Raw bytes, so
    /// decode-config changes (width/skip/endianness) can't desync the compare.
    prev_payload: Option<Vec<u8>>,
}

impl Default for MatrixView {
    fn default() -> Self {
        Self {
            on: false,
            rows: 20,
            cols: 16,
            value_bytes: 4,
            skip_bytes: 0,
            le: true,
            hex: false,
            heat: true,
            change_pct: 0,
            paused: false,
            full: false,
            frozen: None,
            cur_frame: None,
            prev_payload: None,
        }
    }
}

impl MatrixView {
    /// Show THIS payload and stop following the stream.
    ///
    /// For the Frames list, where the user picks a row: the matrix normally
    /// decodes whatever arrived last, which is precisely not what you want
    /// after singling out one frame — the next burst would replace it under
    /// your eyes. Pausing is the same freeze the Pause button does, so the
    /// button is also the way back out.
    pub fn show_payload(&mut self, payload: &[u8]) {
        self.on = true;
        self.paused = true;
        self.frozen = Some(payload.to_vec());
        self.note_frame(payload);
    }

    /// Track serial frames (NOT egui repaints): when a payload with different
    /// bytes arrives, the current one becomes "previous" — the baseline the
    /// `Change %` heat compares against. The byte compare is a few KB.
    fn note_frame(&mut self, incoming: &[u8]) {
        if self.cur_frame.as_deref() != Some(incoming) {
            self.prev_payload = self.cur_frame.take();
            self.cur_frame = Some(incoming.to_vec());
        }
    }
}

/// Split `payload` into `value_bytes`-wide unsigned integers. A trailing
/// partial value is dropped; the width is clamped to 1..=8 (u64).
pub fn decode_values(payload: &[u8], value_bytes: usize, le: bool) -> Vec<u64> {
    let vb = value_bytes.clamp(1, 8);
    payload
        .chunks_exact(vb)
        .map(|c| {
            let mut v: u64 = 0;
            if le {
                for (i, &byte) in c.iter().enumerate() {
                    v |= (byte as u64) << (8 * i);
                }
            } else {
                for &byte in c {
                    v = (v << 8) | byte as u64;
                }
            }
            v
        })
        .collect()
}

/// Characters the widest value of `value_bytes` bytes needs — hex is
/// zero-padded to 2·vb, decimal takes the digits of `2^(8·vb) − 1`. The cell
/// width derives from THIS (the config), never from the data on screen.
fn cell_chars(value_bytes: usize, hex: bool) -> usize {
    let vb = value_bytes.clamp(1, 8);
    if hex {
        vb * 2
    } else {
        [3, 5, 8, 10, 13, 15, 17, 20][vb - 1]
    }
}

/// The heat gradient at intensity `t` ∈ [0, 1]: cold dark blue → red.
fn heat_t(t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    egui::Color32::from_rgb(
        (30.0 + t * 175.0) as u8,
        (44.0 + t * 8.0) as u8,
        (92.0 - t * 52.0) as u8,
    )
}

/// Magnitude heat (Change % = 0): tint by value vs the frame's max.
/// Transparent when the whole frame is zeros (no information to colour).
fn heat_color(v: u64, max: u64) -> egui::Color32 {
    if max == 0 {
        return egui::Color32::TRANSPARENT;
    }
    heat_t(v as f32 / max as f32)
}

/// Percentage-growth heat (Change % = `thr` > 0): `Some(intensity)` when
/// `now` grew at least `thr`% over `prev` — faint right at the threshold,
/// saturated at 4× it. Growth out of zero is a full hit (the jump is
/// "infinite" percent). Decreases and sub-threshold growth stay untinted, so
/// big-but-static values no longer glow permanently.
fn change_heat(prev: u64, now: u64, thr: u32) -> Option<f32> {
    if now <= prev {
        return None;
    }
    if prev == 0 {
        return Some(1.0);
    }
    let growth = (now - prev) as f32 / prev as f32 * 100.0;
    let thr = thr.max(1) as f32;
    if growth < thr {
        return None;
    }
    Some((0.15 + 0.85 * (growth - thr) / (3.0 * thr)).min(1.0))
}

/// Controls row + grid, `height` px tall in total. `live` is the newest
/// complete payload between Find start / Find end (`None` = no complete frame
/// yet, or the Find fields are empty); `frames_total` is how many complete
/// frames the RX buffer currently holds (shown so a live stream is obvious).
pub fn show_matrix(
    ui: &mut egui::Ui,
    m: &mut MatrixView,
    live: Option<&[u8]>,
    frames_total: usize,
    height: f32,
) {
    // ── Controls row ──────────────────────────────────────────────────────────
    ui.horizontal_wrapped(|ui| {
        let pause_label = if m.paused {
            format!("{} Resume", ph::PLAY)
        } else {
            format!("{} Pause", ph::PAUSE)
        };
        if ui
            .selectable_label(m.paused, egui::RichText::new(pause_label).size(10.5))
            .on_hover_text("Freeze the shown frame — new payloads are ignored while paused")
            .clicked()
        {
            m.paused = !m.paused;
            m.frozen = if m.paused { live.map(|p| p.to_vec()) } else { None };
        }
        ui.separator();
        ui.label(egui::RichText::new("Rows:").size(10.5).color(egui::Color32::GRAY));
        ui.add(crate::panels::drag_value(ui, &mut m.rows).range(1..=512).speed(0.2))
            .on_hover_text("Matrix height, in value rows (min 1)");
        ui.label(egui::RichText::new("Row values:").size(10.5).color(egui::Color32::GRAY));
        ui.add(crate::panels::drag_value(ui, &mut m.cols).range(1..=128).speed(0.2))
            .on_hover_text("Values per row (min 1)");
        ui.label(egui::RichText::new("Value bytes:").size(10.5).color(egui::Color32::GRAY));
        ui.add(crate::panels::drag_value(ui, &mut m.value_bytes).range(1..=8).speed(0.1))
            .on_hover_text("Bytes per value: 1 = u8, 2 = u16, 4 = u32, 8 = u64");
        ui.label(egui::RichText::new("Ignore first:").size(10.5).color(egui::Color32::GRAY));
        ui.add(crate::panels::drag_value(ui, &mut m.skip_bytes).range(0..=65535).speed(0.2))
            .on_hover_text(
                "Payload bytes skipped before the first matrix value (min 0) — \
                 for status/length fields that precede the data.",
            );
        ui.separator();
        if ui
            .selectable_label(m.le, egui::RichText::new(if m.le { "LE" } else { "BE" }).size(10.5))
            .on_hover_text("Byte order of each value: LE = little-endian (STM32/ESP32), BE = big-endian. Click to flip.")
            .clicked()
        {
            m.le = !m.le;
        }
        ui.checkbox(&mut m.hex, egui::RichText::new("Hex").size(10.5))
            .on_hover_text("Show values in hexadecimal");
        ui.checkbox(&mut m.heat, egui::RichText::new("Heat").size(10.5))
            .on_hover_text("Tint each cell by its value (blue = 0 … red = the frame's max)");
        ui.add_enabled_ui(m.heat, |ui| {
            ui.label(
                egui::RichText::new("Change %:")
                    .size(10.5)
                    .color(egui::Color32::GRAY),
            );
            ui.add(crate::panels::drag_value(ui, &mut m.change_pct).range(0..=1000000).speed(0.5))
                .on_hover_text(
                    "0 = colour by MAGNITUDE (value vs the frame's max — big \
                     values always glow).\n\
                     N = tint ONLY cells that GREW ≥ N% since the previous \
                     frame: faint at the threshold, red at 4× it. Growth from \
                     0 counts as a full hit; decreases stay dark. max value = 1000000",
                );
        });

        // Status, right-aligned: which frame is shown + its size. The Max
        // button sits rightmost (fullscreen, auto-zoomed to fit the display).
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .selectable_label(
                    m.full,
                    egui::RichText::new(format!("{} Max", ph::CORNERS_OUT)).size(10.5),
                )
                .on_hover_text(
                    "Show the matrix over the whole window, zoomed so the \
                     entire grid fits the display. Esc or `Normal` returns.",
                )
                .clicked()
            {
                m.full = !m.full;
            }
            let (icon, text, color) = match (m.paused, live) {
                (true, _) => (
                    Some(ph::PAUSE),
                    "frozen".to_owned(),
                    egui::Color32::from_rgb(120, 180, 240),
                ),
                (false, Some(p)) => (
                    None,
                    format!("frame #{frames_total} · {} B", p.len()),
                    egui::Color32::from_rgb(120, 210, 140),
                ),
                (false, None) => (None, "—".to_owned(), egui::Color32::GRAY),
            };
            // Icon and text are SEPARATE labels: phosphor is registered only in
            // the Proportional family (`add_to_fonts` in app.rs), so a glyph
            // inside a `.monospace()` RichText draws as a tofu square — which is
            // what showed up before "frozen". The numbers keep monospace; the
            // icon gets the proportional font. Right-to-left layout, so the text
            // goes in FIRST to end up on the right of the icon.
            ui.label(egui::RichText::new(text).size(10.5).monospace().color(color));
            if let Some(i) = icon {
                ui.label(egui::RichText::new(i).size(10.5).color(color));
            }
        });
    });

    // ── Serial-frame tracking for the Change % baseline ───────────────────────
    // Advances only on DIFFERENT bytes (egui repaints re-see the same frame)
    // and not while paused — pause freezes the change tints along with the
    // values, keeping the comparison coherent with what is displayed.
    if !m.paused {
        if let Some(p) = live {
            m.note_frame(p);
        }
    }

    // ── Payload resolution (frozen wins while paused) ─────────────────────────
    // Owned copy (a frame is a few KB): the fullscreen toggle below mutates
    // `m`, which a live borrow of `m.frozen` would forbid.
    let payload: Option<Vec<u8>> = if m.paused {
        // A pause hit before any frame arrived freezes the first one to come.
        if m.frozen.is_none() {
            m.frozen = live.map(|p| p.to_vec());
        }
        m.frozen.clone()
    } else {
        live.map(|p| p.to_vec())
    };
    let Some(payload) = payload else {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "No complete frame yet.\n\
                 The matrix shows the newest payload BETWEEN `Find start` and \
                 `Find end` (both markers excluded) — fill both fields (hex) \
                 and wait for one full frame.\n\
                 Example: start FD FC FB FA · end 04 03 02 01 · 1280 B payload \
                 = 20 rows × 16 values × 4 bytes.",
            )
            .size(11.0)
            .color(egui::Color32::GRAY),
        );
        return;
    };

    // ── Decode ────────────────────────────────────────────────────────────────
    // "Ignore first" trims the leading status/length fields; hover offsets
    // stay ABSOLUTE in the payload (skip included) so they match the Hex view.
    let skip = m.skip_bytes.min(payload.len());
    let frame_len = payload.len();
    let data = &payload[skip..];
    let values = decode_values(data, m.value_bytes, m.le);
    let vb = m.value_bytes.clamp(1, 8);
    let want = m.rows * m.cols;
    let shown = want.min(values.len()).min(MAX_CELLS);
    let max = values[..shown].iter().copied().max().unwrap_or(0);
    // Cell metrics from the CONFIG, not the data: sizing by the widest value
    // currently on screen made every column jump left/right as values came
    // in, so it was impossible to spot WHICH cell changed. The cell is as
    // wide as the biggest value `value_bytes` can hold, and stays put.
    let widest = cell_chars(vb, m.hex);
    // The Change % baseline, decoded with the CURRENT config (raw bytes are
    // stored, so width/skip/endianness edits can't desync the comparison).
    let prev_values: Option<Vec<u64>> = if m.heat && m.change_pct > 0 {
        m.prev_payload.as_deref().map(|p| {
            let s = m.skip_bytes.min(p.len());
            decode_values(&p[s..], m.value_bytes, m.le)
        })
    } else {
        None
    };

    // ── Fullscreen: the whole window, auto-zoomed so ALL data fits ───────────
    if m.full {
        let mut close = ui.input(|i| i.key_pressed(egui::Key::Escape));
        egui::Area::new(egui::Id::new("serial_matrix_full"))
            .fixed_pos(egui::Pos2::ZERO)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                let screen = ui.ctx().viewport_rect();
                // Claim the whole screen (blocks the panels underneath) and
                // paint an opaque backdrop.
                let _ = ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
                ui.painter()
                    .rect_filled(screen, 0.0, egui::Color32::from_rgb(14, 16, 20));

                const TOP_H: f32 = 30.0;
                // MAX ZOOM that still fits: scale the base (font-11) metrics
                // by min(width ratio, height ratio) — the whole rows × cols
                // grid is on screen by construction, no scrolling.
                let base_font = egui::FontId::monospace(11.0);
                let char_w = ui.fonts_mut(|f| f.glyph_width(&base_font, '0'));
                let base_cell = egui::vec2(char_w * widest as f32 + 10.0, 17.0);
                let base_idx = char_w * 5.0;
                // Exact model — with `draw_grid` zeroing egui's item spacing,
                // the rendered grid is EXACTLY this many pixels (the earlier
                // ×1.06 fudge compensated for the unmodelled 8px spacing and
                // would now overshoot the other way).
                let grid_base = egui::vec2(
                    base_idx + base_cell.x * m.cols as f32,
                    // rows + the column-numbering header + the notes line
                    base_cell.y * (m.rows + 1) as f32 + 18.0,
                );
                let avail = screen.size() - egui::vec2(24.0, TOP_H + 20.0);
                let s = (avail.x / grid_base.x)
                    .min(avail.y / grid_base.y)
                    .clamp(0.2, 8.0);
                let font = egui::FontId::monospace(11.0 * s);
                let cell = base_cell * s;
                let idx_w = base_idx * s;

                // Top bar: restore button + frame status + Esc hint.
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(12.0, 4.0),
                    egui::vec2(screen.width() - 24.0, TOP_H),
                );
                let mut bar = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(bar_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                if bar
                    .selectable_label(
                        true,
                        egui::RichText::new(format!("{} Normal", ph::CORNERS_IN)).size(11.0),
                    )
                    .on_hover_text("Back to the panel view (Esc)")
                    .clicked()
                {
                    close = true;
                }
                bar.label(
                    egui::RichText::new(format!(
                        "Serial Matrix · frame #{frames_total} · {frame_len} B · {}×{}×{vb}",
                        m.rows, m.cols
                    ))
                    .size(11.0)
                    .monospace()
                    .color(egui::Color32::from_gray(180)),
                );
                bar.label(
                    egui::RichText::new("Esc = exit")
                        .size(10.5)
                        .color(egui::Color32::from_gray(110)),
                );

                // The grid, centered in the space under the bar.
                let grid_size = egui::vec2(
                    idx_w + cell.x * m.cols as f32,
                    cell.y * (m.rows + 1) as f32 + 18.0 * s.min(1.5),
                );
                let origin = egui::pos2(
                    (screen.width() - grid_size.x).max(0.0) / 2.0,
                    TOP_H + 8.0 + ((screen.height() - TOP_H - 16.0) - grid_size.y).max(0.0) / 2.0,
                );
                let mut grid_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(egui::Rect::from_min_size(origin, grid_size))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                draw_grid(
                    &mut grid_ui,
                    m,
                    data,
                    &values,
                    prev_values.as_deref(),
                    shown,
                    max,
                    skip,
                    vb,
                    cell,
                    idx_w,
                    &font,
                );
            });
        if close {
            m.full = false;
        }
        // Placeholder in the panel while the overlay is up.
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Matrix shown fullscreen — Esc or `Normal` returns.")
                .size(11.0)
                .color(egui::Color32::GRAY),
        );
        return;
    }

    // ── Inline grid (fixed font, scrollable) ──────────────────────────────────
    let font = egui::FontId::monospace(11.0);
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let cell = egui::vec2(char_w * widest as f32 + 10.0, 17.0);
    let idx_w = char_w * 5.0;
    egui::ScrollArea::both()
        .id_salt("serial_matrix")
        .auto_shrink([false, false])
        .max_height(height - 26.0)
        .show(ui, |ui| {
            draw_grid(
                ui,
                m,
                data,
                &values,
                prev_values.as_deref(),
                shown,
                max,
                skip,
                vb,
                cell,
                idx_w,
                &font,
            );
        });
}

/// The rows × cols grid + the leftover/short notes — shared by the inline
/// (scrollable, font 11) and fullscreen (fit-scaled) views; `cell`/`idx_w`/
/// `font` carry the caller's metrics.
#[allow(clippy::too_many_arguments)]
fn draw_grid(
    ui: &mut egui::Ui,
    m: &MatrixView,
    payload: &[u8],
    values: &[u64],
    // Previous frame's values (same decode config) — `Some` only in the
    // Change % heat mode; cells without a counterpart stay untinted.
    prev: Option<&[u64]>,
    shown: usize,
    max: u64,
    skip: usize,
    vb: usize,
    cell: egui::Vec2,
    idx_w: f32,
    font: &egui::FontId,
) {
    let fmt = |v: u64| -> String {
        if m.hex {
            format!("{v:0width$X}", width = vb * 2)
        } else {
            v.to_string()
        }
    };
    let want = m.rows * m.cols;
    // Leftover info: payload bytes beyond the grid / grid cells beyond payload.
    let leftover = payload.len() as isize - (want * vb) as isize;

    // Geometry must equal the fit math EXACTLY: egui's default item spacing
    // (8 px, unscaled) between cells grew the REAL grid by ~8·cols horizontally
    // and ~4·rows vertically over the computed size — the fullscreen fit then
    // missed by more the more columns there were (the reported growing gap).
    // Cells carry their own padding, so spacing is zeroed outright.
    ui.spacing_mut().item_spacing = egui::Vec2::ZERO;

    // Column numbering across the top — right-aligned like the values, so the
    // header digit sits over the units digit of its column. (The fullscreen
    // fit budgets this as one extra row.)
    ui.horizontal(|ui| {
        let _ = ui.allocate_exact_size(egui::vec2(idx_w, cell.y), egui::Sense::hover());
        for col in 0..m.cols {
            let (rect, _) = ui.allocate_exact_size(cell, egui::Sense::hover());
            if !ui.is_rect_visible(rect) {
                continue;
            }
            ui.painter().text(
                rect.right_center() - egui::vec2(5.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                col.to_string(),
                font.clone(),
                egui::Color32::from_gray(110),
            );
        }
    });

    for row in 0..m.rows {
        ui.horizontal(|ui| {
            // Row index, dim, with the row's byte offset on hover.
            let (r, resp) = ui.allocate_exact_size(egui::vec2(idx_w, cell.y), egui::Sense::hover());
            ui.painter().text(
                r.right_center(),
                egui::Align2::RIGHT_CENTER,
                format!("{row:>3} "),
                font.clone(),
                egui::Color32::from_gray(110),
            );
            let row_off = skip + row * m.cols * vb;
            resp.on_hover_text(format!(
                "row {row} — payload offset {row_off} (0x{row_off:X})"
            ));
            for col in 0..m.cols {
                let i = row * m.cols + col;
                let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::hover());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                if i >= shown {
                    // Beyond the payload (or the cell cap) — greyed.
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "—",
                        font.clone(),
                        egui::Color32::from_gray(70),
                    );
                    continue;
                }
                let v = values[i];
                if m.heat {
                    // Change % = 0 → magnitude tint; > 0 → tint only the
                    // cells that grew enough since the previous frame.
                    let color = if m.change_pct == 0 {
                        Some(heat_color(v, max))
                    } else {
                        prev.and_then(|pv| pv.get(i))
                            .and_then(|&p| change_heat(p, v, m.change_pct))
                            .map(heat_t)
                    };
                    if let Some(c) = color {
                        ui.painter().rect_filled(rect.shrink(1.0), 2.0, c);
                    }
                }
                // Right-aligned: with the fixed cell width, the units
                // digit stays put — a changed value is spottable.
                ui.painter().text(
                    rect.right_center() - egui::vec2(5.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    fmt(v),
                    font.clone(),
                    egui::Color32::from_gray(215),
                );
                let off = i * vb;
                let raw: String = payload[off..off + vb]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let abs = skip + off;
                resp.on_hover_text(format!(
                    "[{row}][{col}] = {v}  (0x{v:X})\nbytes {raw} @ offset {abs} (0x{abs:X})"
                ));
            }
        });
    }
    ui.add_space(4.0); // zero item-spacing above — keep the notes off the grid
    if leftover > 0 {
        ui.label(
            egui::RichText::new(format!(
                "+{leftover} payload byte(s) beyond the {}×{} grid",
                m.rows, m.cols
            ))
            .size(10.0)
            .color(egui::Color32::from_rgb(200, 160, 80)),
        );
    } else if leftover < 0 {
        ui.label(
            egui::RichText::new(format!(
                "payload {} B short of the {}×{}×{} grid",
                -leftover, m.rows, m.cols, vb
            ))
            .size(10.0)
            .color(egui::Color32::from_gray(120)),
        );
    }
    if want > MAX_CELLS {
        ui.label(
            egui::RichText::new(format!("showing the first {MAX_CELLS} cells"))
                .size(10.0)
                .color(egui::Color32::from_rgb(200, 160, 80)),
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The motivating example: 1280 payload bytes as u32 LE → 320 values,
    /// i.e. exactly a 20 × 16 grid.
    #[test]
    fn radar_payload_decodes_to_20_by_16_u32() {
        let payload: Vec<u8> = (0..1280u32).map(|i| (i % 256) as u8).collect();
        let values = decode_values(&payload, 4, true);
        assert_eq!(values.len(), 320); // = 20 rows × 16 values
        // First value: bytes 00 01 02 03 LE → 0x03020100.
        assert_eq!(values[0], 0x0302_0100);
        // Same bytes BE → 0x00010203.
        assert_eq!(decode_values(&payload, 4, false)[0], 0x0001_0203);
    }

    #[test]
    fn decode_widths_and_partial_tail() {
        // 1-byte values are the bytes themselves.
        assert_eq!(decode_values(&[7, 8, 9], 1, true), vec![7, 8, 9]);
        // u16 LE / BE.
        assert_eq!(decode_values(&[0x34, 0x12], 2, true), vec![0x1234]);
        assert_eq!(decode_values(&[0x34, 0x12], 2, false), vec![0x3412]);
        // A trailing partial value is dropped (5 bytes at width 2 → 2 values).
        assert_eq!(decode_values(&[1, 0, 2, 0, 3], 2, true).len(), 2);
        // u64 max width; wider requests clamp to 8 instead of panicking.
        assert_eq!(decode_values(&[0xFF; 8], 8, true), vec![u64::MAX]);
        assert_eq!(decode_values(&[0xFF; 16], 99, true), vec![u64::MAX; 2]);
        // Zero width clamps to 1.
        assert_eq!(decode_values(&[5], 0, true), vec![5]);
    }

    /// The fixed cell width: derived from the config only. The decimal table
    /// must match the real digit counts of the widest representable values.
    #[test]
    fn cell_width_is_config_derived_and_exact() {
        for vb in 1..=8usize {
            let max_val = (1u128 << (8 * vb)) - 1;
            assert_eq!(
                cell_chars(vb, false),
                max_val.to_string().len(),
                "decimal digits for {vb}-byte values"
            );
            assert_eq!(
                cell_chars(vb, true),
                vb * 2,
                "hex chars for {vb}-byte values"
            );
        }
        // Out-of-range widths clamp like the decoder does.
        assert_eq!(cell_chars(0, false), 3);
        assert_eq!(cell_chars(99, true), 16);
    }

    /// The Change % gate: decreases and sub-threshold growth stay dark, the
    /// intensity rises with the growth, zero-baseline jumps are a full hit.
    #[test]
    fn change_heat_gates_on_growth_percent() {
        assert_eq!(change_heat(100, 100, 10), None); // unchanged
        assert_eq!(change_heat(100, 90, 10), None); // decrease
        assert_eq!(change_heat(100, 105, 10), None); // +5% < 10%
        let at = change_heat(100, 110, 10).unwrap(); // exactly the threshold
        assert!((0.14..0.2).contains(&at), "faint at the threshold: {at}");
        let mid = change_heat(100, 125, 10).unwrap(); // +25%
        assert!(mid > at);
        assert_eq!(change_heat(100, 500, 10), Some(1.0)); // way past 4× thr
        // Growth out of zero = "infinite" percent → full hit; 0→0 is nothing.
        assert_eq!(change_heat(0, 3, 10), Some(1.0));
        assert_eq!(change_heat(0, 0, 10), None);
    }

    /// The baseline advances per SERIAL frame, not per egui repaint: re-seeing
    /// the same bytes must not rotate the previous frame away.
    #[test]
    fn note_frame_tracks_serial_frames_not_repaints() {
        let mut m = MatrixView::default();
        m.note_frame(&[1, 2, 3]);
        assert_eq!(m.prev_payload, None); // first frame has no baseline
        m.note_frame(&[1, 2, 3]); // same frame re-seen (repaint)
        assert_eq!(m.prev_payload, None);
        m.note_frame(&[9, 9]);
        assert_eq!(m.prev_payload.as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(m.cur_frame.as_deref(), Some(&[9u8, 9][..]));
    }

    /// Heat: zero-max frames stay untinted; the gradient's ends are stable.
    #[test]
    fn heat_gradient_endpoints() {
        assert_eq!(heat_color(3, 0), eframe::egui::Color32::TRANSPARENT);
        let cold = heat_color(0, 100);
        let hot = heat_color(100, 100);
        assert!(cold.b() > cold.r(), "cold end is blue");
        assert!(hot.r() > hot.b(), "hot end is red");
    }
}
