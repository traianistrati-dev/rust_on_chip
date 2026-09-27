//! Serial plotter — real-time curves from numeric lines on the serial port
//! (Arduino Serial Plotter style). Phase 2 of the Serial monitor.
//!
//! Each received LINE is one sample tick. Supported formats, mixable on the
//! same line, separated by space/tab/comma/semicolon:
//!  - plain values:  `1.0 2.5 -3`         → channels ch1, ch2, ch3
//!  - labelled:      `temp:23.4, hum:56`  → channels temp, hum (also `name=v`)
//! Non-numeric tokens are skipped, lines with no numbers are ignored — so an
//! occasional log line between samples doesn't disturb the plot.
//!
//! Feeding is incremental: [`PlotState::feed`] gets the shared RX buffer plus
//! the monotonic `rx_total` counter and parses only the not-yet-seen tail, so
//! it costs nothing when no new bytes arrived. The plot keeps its own ring of
//! parsed points (`CAP` per channel) independent of the RX byte cap.

use eframe::egui;
use egui_phosphor::regular as ph;
use std::collections::VecDeque;

/// Retained points per channel (the view window slides inside this).
pub const CAP: usize = 4096;
/// A parse line longer than this is treated as binary garbage and discarded.
const MAX_LINE: usize = 512;

/// Distinct channel colours (index % 8 — matches the legend and the curves).
const PALETTE: [egui::Color32; 8] = [
    egui::Color32::from_rgb(120, 170, 240), // blue
    egui::Color32::from_rgb(230, 160, 80),  // orange
    egui::Color32::from_rgb(120, 200, 140), // green
    egui::Color32::from_rgb(230, 120, 120), // red
    egui::Color32::from_rgb(190, 130, 230), // purple
    egui::Color32::from_rgb(230, 210, 90),  // yellow
    egui::Color32::from_rgb(90, 210, 210),  // cyan
    egui::Color32::from_rgb(240, 140, 200), // pink
];

pub fn channel_color(i: usize) -> egui::Color32 {
    PALETTE[i % PALETTE.len()]
}

/// One plotted series.
pub struct Channel {
    pub name: String,
    /// Hidden channels keep collecting data but are not drawn.
    pub visible: bool,
    /// `(line index, value)` — line indices are shared across channels, so
    /// curves stay aligned even when a line carries only some channels.
    pub data: VecDeque<(u64, f32)>,
}

/// Parser + view state of the plotter (owned by `SerialMonitor`).
pub struct PlotState {
    pub channels: Vec<Channel>,
    /// Bytes of the RX stream already parsed (compared against `rx_total`).
    consumed: u64,
    /// The partial line being assembled between feeds.
    pending: String,
    /// `true` while discarding an overlong (binary) line up to its newline.
    discarding: bool,
    /// Next sample tick (one per parsed line with ≥ 1 value).
    line_no: u64,
    /// Visible window, in samples.
    pub window: usize,
    /// Freeze: incoming bytes are dropped (not queued) while paused.
    pub paused: bool,
}

impl Default for PlotState {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            consumed: 0,
            pending: String::new(),
            discarding: false,
            line_no: 0,
            window: 500,
            paused: false,
        }
    }
}

impl PlotState {
    /// Parse the tail of `rx` that arrived since the last call. `rx_total` is
    /// the monotonic received-bytes counter (`SerialState::rx_total`); bytes
    /// that fell out of the RX ring before we saw them are skipped (the
    /// partial line is dropped so a torn line can't parse wrong).
    pub fn feed(&mut self, rx: &[u8], rx_total: u64) {
        let new = rx_total.saturating_sub(self.consumed);
        self.consumed = rx_total;
        if new == 0 {
            return;
        }
        if self.paused {
            // The bytes arriving while paused are dropped, but WHERE they stop
            // decides what the next ones are. Ending on a newline means the
            // stream resumes at a line start and nothing is owed; ending
            // mid-line means the first bytes after Resume are that line's TAIL,
            // and parsing them as a whole line invents a sample. That is the
            // same hole the ring-loss branch below fixes, and it takes the same
            // answer - but only in the second case, which is why this looks at
            // the dropped bytes instead of always discarding.
            let take = (new as usize).min(rx.len());
            let resumes_at_a_line_start = take > 0 && rx[rx.len() - take..].last() == Some(&b'\n');
            self.pending.clear();
            self.discarding = !resumes_at_a_line_start;
            return;
        }
        if new as usize > rx.len() {
            // Lost bytes to the ring cap: `pending` has a hole AND the first
            // visible "line" is torn at its start — skip to the next newline.
            self.pending.clear();
            self.discarding = true;
        }
        let take = (new as usize).min(rx.len());
        let start = rx.len() - take;
        for &b in &rx[start..] {
            self.push_byte(b);
        }
    }

    fn push_byte(&mut self, b: u8) {
        match b {
            b'\n' => {
                if self.discarding {
                    self.discarding = false;
                } else {
                    let line = std::mem::take(&mut self.pending);
                    self.apply_line(&line);
                }
                self.pending.clear();
            }
            b'\r' => {}
            _ => {
                if self.discarding {
                    return;
                }
                if self.pending.len() >= MAX_LINE {
                    self.pending.clear();
                    self.discarding = true;
                    return;
                }
                self.pending.push(b as char);
            }
        }
    }

    /// Parse one complete line and append its samples (no-op when it holds no
    /// numeric values — log lines between samples are fine).
    fn apply_line(&mut self, line: &str) {
        // A BINARY stream has no lines: `0x0A` occurs inside payloads by chance,
        // so the assembled "line" is a slice of frame bytes. Parsing it invents
        // channels out of noise — that is where the legend's unreadable names
        // came from. The plotter is for TEXT protocols; anything else is not a
        // sample line and is dropped whole.
        if !is_text_line(line) {
            return;
        }
        let samples = parse_line(line);
        if samples.is_empty() {
            return;
        }
        let x = self.line_no;
        self.line_no += 1;
        let mut unlabeled = 0usize;
        for (label, v) in samples {
            let idx = match label {
                Some(name) => self.channel_named(&name),
                None => {
                    unlabeled += 1;
                    self.channel_named(&format!("ch{unlabeled}"))
                }
            };
            let ch = &mut self.channels[idx];
            ch.data.push_back((x, v));
            if ch.data.len() > CAP {
                ch.data.pop_front();
            }
        }
    }

    /// Index of the channel called `name`, creating it on first sight.
    fn channel_named(&mut self, name: &str) -> usize {
        if let Some(i) = self.channels.iter().position(|c| c.name == name) {
            return i;
        }
        self.channels.push(Channel {
            name: name.to_string(),
            visible: true,
            data: VecDeque::new(),
        });
        self.channels.len() - 1
    }

    /// Drop every parsed point and channel (the byte cursor stays in place, so
    /// plotting resumes from *new* data only).
    pub fn clear_data(&mut self) {
        self.channels.clear();
        self.pending.clear();
        self.discarding = false;
        self.line_no = 0;
    }
}

/// The value tokens of one line: `(label, value)` per token; `label` is `None`
/// for bare numbers. See the module docs for the accepted formats.
/// Is this a plausible line of a TEXT protocol? Printable ASCII plus spacing,
/// nothing else. The one check that keeps a binary stream out of the plotter:
/// its bytes contain `0x0A` at random, so without it every frame boundary looks
/// like a sample line.
fn is_text_line(line: &str) -> bool {
    !line.trim().is_empty()
        && line
            .chars()
            .all(|c| c == ' ' || c == '\t' || c.is_ascii_graphic())
}

/// Is this a plausible CHANNEL NAME? A label comes from a human writing
/// `temp:23.4`, so it looks like an identifier — a run of bytes that merely
/// contains a `:` does not become one.
fn is_label(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 24
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '[' | ']'))
}

fn parse_line(line: &str) -> Vec<(Option<String>, f32)> {
    let mut out = Vec::new();
    for tok in line.split([' ', '\t', ',', ';']) {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        if let Some((name, val)) = tok.split_once([':', '=']) {
            let name = name.trim();
            if let (true, Ok(v)) = (is_label(name), val.trim().parse::<f32>()) {
                if v.is_finite() {
                    out.push((Some(name.to_string()), v));
                }
            }
            continue;
        }
        if let Ok(v) = tok.parse::<f32>() {
            if v.is_finite() {
                out.push((None, v));
            }
        }
    }
    out
}

// ── Drawing ───────────────────────────────────────────────────────────────────

/// Controls row + plot canvas, `height` px tall in total. Replaces the RX
/// text/hex view while the Serial tab's Plot toggle is on.
pub fn show_plot(ui: &mut egui::Ui, plot: &mut PlotState, height: f32) {
    // ── Controls: Pause · Clear · Window · legend (click = show/hide) ────────
    ui.horizontal_wrapped(|ui| {
        let pause_label = if plot.paused {
            format!("{} Resume", ph::PLAY)
        } else {
            format!("{} Pause", ph::PAUSE)
        };
        if ui
            .selectable_label(plot.paused, egui::RichText::new(pause_label).size(10.5))
            .on_hover_text("Freeze the plot — bytes arriving while paused are not plotted")
            .clicked()
        {
            plot.paused = !plot.paused;
        }
        if ui
            .button(egui::RichText::new(format!("{} Clear plot", ph::TRASH)).size(10.5))
            .clicked()
        {
            plot.clear_data();
        }
        ui.label(
            egui::RichText::new("Window:")
                .size(10.5)
                .color(egui::Color32::GRAY),
        );
        ui.add(
            crate::panels::drag_value(ui, &mut plot.window)
                .range(50..=CAP)
                .speed(5.0)
                .suffix(" pts"),
        )
        .on_hover_text("How many samples are visible (newest at the right edge)");
        ui.separator();
        for (i, ch) in plot.channels.iter_mut().enumerate() {
            let color = channel_color(i);
            let last = ch
                .data
                .back()
                .map(|&(_, v)| format!(" {}", fmt_val(v)))
                .unwrap_or_default();
            let text = egui::RichText::new(format!("{}{last}", ch.name))
                .size(10.5)
                .monospace()
                .color(if ch.visible {
                    color
                } else {
                    egui::Color32::from_gray(90)
                });
            if ui
                .selectable_label(ch.visible, text)
                .on_hover_text("Click to show/hide this channel")
                .clicked()
            {
                ch.visible = !ch.visible;
            }
        }
    });

    // ── Canvas ────────────────────────────────────────────────────────────────
    let canvas_h = (height - 26.0).max(60.0);
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), canvas_h),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(22));
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(60)),
        egui::StrokeKind::Inside,
    );

    if plot.channels.iter().all(|c| c.data.is_empty()) {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "waiting for numeric lines…   e.g.  temp:23.4 hum:56   or   1.0 2.5",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(110),
        );
        return;
    }

    // Visible x range: the last `window` sample ticks.
    let x_max = plot.line_no.saturating_sub(1);
    let x_min = x_max.saturating_sub(plot.window as u64);

    // Y range over the visible points of the visible channels.
    let (mut y_min, mut y_max) = (f32::INFINITY, f32::NEG_INFINITY);
    for ch in plot.channels.iter().filter(|c| c.visible) {
        for &(x, v) in ch.data.iter().rev() {
            if x < x_min {
                break; // data is x-sorted — nothing older can be visible
            }
            y_min = y_min.min(v);
            y_max = y_max.max(v);
        }
    }
    if !y_min.is_finite() || !y_max.is_finite() {
        return; // every channel hidden
    }
    // Pad 5%; a flat line still needs a non-zero span.
    let span = (y_max - y_min).max(f32::EPSILON);
    let (y_min, y_max) = if y_max == y_min {
        (y_min - 1.0, y_max + 1.0)
    } else {
        (y_min - span * 0.05, y_max + span * 0.05)
    };

    // Screen mapping (labels live in the left gutter).
    const GUTTER_L: f32 = 46.0;
    const PAD: f32 = 4.0;
    let plot_l = rect.left() + GUTTER_L;
    let plot_r = rect.right() - PAD;
    let plot_t = rect.top() + PAD;
    let plot_b = rect.bottom() - 14.0;
    let x_span = (x_max - x_min).max(1) as f32;
    let to_screen = |x: u64, v: f32| -> egui::Pos2 {
        let fx = (x - x_min) as f32 / x_span;
        let fy = (v - y_min) / (y_max - y_min);
        egui::pos2(
            plot_l + fx * (plot_r - plot_l),
            plot_b - fy * (plot_b - plot_t),
        )
    };

    // ── Grid + labels ─────────────────────────────────────────────────────────
    let grid_col = egui::Color32::from_gray(45);
    let label_col = egui::Color32::from_gray(130);
    let step = nice_step((y_max - y_min) as f64, 4.0) as f32;
    let mut gy = (y_min / step).ceil() * step;
    while gy <= y_max {
        let y = to_screen(x_min, gy).y;
        painter.hline(
            egui::Rangef::new(plot_l, plot_r),
            y,
            egui::Stroke::new(1.0_f32, grid_col),
        );
        painter.text(
            egui::pos2(rect.left() + GUTTER_L - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            fmt_val(gy),
            egui::FontId::monospace(9.0),
            label_col,
        );
        gy += step;
    }
    // Vertical grid: 4 sample-tick marks.
    for i in 1..4u64 {
        let gx = x_min + (x_max - x_min) * i / 4;
        let x = to_screen(gx, y_min).x;
        painter.vline(
            x,
            egui::Rangef::new(plot_t, plot_b),
            egui::Stroke::new(1.0_f32, grid_col),
        );
        painter.text(
            egui::pos2(x, rect.bottom() - 2.0),
            egui::Align2::CENTER_BOTTOM,
            format!("{gx}"),
            egui::FontId::monospace(9.0),
            label_col,
        );
    }

    // ── Curves ────────────────────────────────────────────────────────────────
    for (i, ch) in plot.channels.iter().enumerate() {
        if !ch.visible {
            continue;
        }
        let mut pts: Vec<egui::Pos2> = Vec::new();
        for &(x, v) in ch.data.iter() {
            if x < x_min {
                continue;
            }
            pts.push(to_screen(x, v));
        }
        if pts.len() == 1 {
            painter.circle_filled(pts[0], 2.0, channel_color(i));
        } else if pts.len() > 1 {
            painter.add(egui::Shape::line(
                pts,
                egui::Stroke::new(1.5_f32, channel_color(i)),
            ));
        }
    }

    // ── Hover readout: vline + per-channel values at the nearest tick ────────
    if let Some(pos) = resp.hover_pos() {
        if pos.x >= plot_l && pos.x <= plot_r {
            let fx = ((pos.x - plot_l) / (plot_r - plot_l)).clamp(0.0, 1.0);
            let hx = x_min + (fx * x_span).round() as u64;
            painter.vline(
                to_screen(hx, y_min).x,
                egui::Rangef::new(plot_t, plot_b),
                egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
            );
            let mut ty = plot_t + 2.0;
            painter.text(
                egui::pos2(plot_r - 4.0, ty),
                egui::Align2::RIGHT_TOP,
                format!("sample {hx}"),
                egui::FontId::monospace(9.5),
                label_col,
            );
            ty += 12.0;
            for (i, ch) in plot.channels.iter().enumerate() {
                if !ch.visible {
                    continue;
                }
                // Nearest stored point at or before the hovered tick.
                let v = ch
                    .data
                    .iter()
                    .rev()
                    .find(|&&(x, _)| x <= hx)
                    .map(|&(_, v)| v);
                if let Some(v) = v {
                    painter.text(
                        egui::pos2(plot_r - 4.0, ty),
                        egui::Align2::RIGHT_TOP,
                        format!("{} {}", ch.name, fmt_val(v)),
                        egui::FontId::monospace(9.5),
                        channel_color(i),
                    );
                    ty += 12.0;
                }
            }
        }
    }
}

/// Grid step ≈ `range / target` rounded to a 1/2/5 × 10ⁿ value.
fn nice_step(range: f64, target: f64) -> f64 {
    let raw = (range / target).max(f64::MIN_POSITIVE);
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let step = if norm < 1.5 {
        1.0
    } else if norm < 3.5 {
        2.0
    } else if norm < 7.5 {
        5.0
    } else {
        10.0
    };
    step * mag
}

/// Compact value text: `1234`, `23.45`, `0.0042`.
fn fmt_val(v: f32) -> String {
    let a = v.abs();
    if a >= 100.0 || (v == v.trunc() && a >= 1.0) {
        format!("{v:.0}")
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else if v == 0.0 {
        "0".to_string()
    } else {
        format!("{v:.4}")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_handles_all_formats() {
        // Bare values.
        assert_eq!(
            parse_line("1.0 2.5 -3"),
            vec![(None, 1.0), (None, 2.5), (None, -3.0)]
        );
        // Labelled, comma-separated, `=` too.
        assert_eq!(
            parse_line("temp:23.4, hum=56"),
            vec![(Some("temp".into()), 23.4), (Some("hum".into()), 56.0)]
        );
        // Junk tokens skipped; a pure log line yields nothing.
        assert_eq!(parse_line("Temp 23.5 C"), vec![(None, 23.5)]);
        assert!(parse_line("boot ok").is_empty());
        assert!(parse_line("").is_empty());
        // NaN / inf are rejected.
        assert!(parse_line("NaN inf x:nan").is_empty());
        // A "label" that is really binary noise is not a channel name — this is
        // what filled the legend with unreadable buttons on a binary stream.
        assert!(parse_line("\u{0}\u{f8}\u{d7}\u{a4}:10").is_empty());
        assert!(parse_line("way_too_long_a_name_to_be_real_here:1").is_empty());
    }

    /// A binary stream contains `0x0A` by chance, so the assembled "lines" are
    /// frame fragments. They must not reach the parser at all: the plotter is
    /// for text protocols, and inventing channels from noise is worse than
    /// plotting nothing.
    #[test]
    fn binary_noise_never_becomes_a_channel() {
        assert!(!is_text_line("\u{0}\u{f8}\u{d7}#\u{a4}"));
        assert!(!is_text_line("temp:23\u{7}")); // one control byte is enough
        assert!(!is_text_line("   "));
        assert!(is_text_line("temp:23.4, hum=56"));
        assert!(is_text_line("1.0 2.5 -3"));

        let mut p = PlotState::default();
        let junk = b"\x00\xf8\xd7\x23\x3a\x31\x30\n\xaa\xbb\n";
        p.feed(junk, junk.len() as u64);
        assert!(
            p.channels.is_empty(),
            "binary noise created channels: {:?}",
            p.channels.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    /// Feeding across arbitrary chunk boundaries assembles the same lines.
    #[test]
    fn feed_assembles_split_lines() {
        let mut p = PlotState::default();
        let stream = b"a:1\r\na:2\na:3\n";
        // Simulate the ring: feed byte by byte with growing totals.
        let mut buf: Vec<u8> = Vec::new();
        for (i, &b) in stream.iter().enumerate() {
            buf.push(b);
            p.feed(&buf, (i + 1) as u64);
        }
        let ch = &p.channels[0];
        assert_eq!(ch.name, "a");
        let vals: Vec<f32> = ch.data.iter().map(|&(_, v)| v).collect();
        assert_eq!(vals, vec![1.0, 2.0, 3.0]);
        // Sample ticks are consecutive.
        let xs: Vec<u64> = ch.data.iter().map(|&(x, _)| x).collect();
        assert_eq!(xs, vec![0, 1, 2]);
    }

    /// Unlabeled values get positional channels; labels map by name, so the
    /// same label always lands in the same channel.
    #[test]
    fn channels_map_by_label_and_position() {
        let mut p = PlotState::default();
        p.feed(b"1 2\nx:9\n3 4\n", 12);
        let names: Vec<&str> = p.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["ch1", "ch2", "x"]);
        assert_eq!(p.channels[0].data.len(), 2); // ch1 got 1 and 3
        assert_eq!(p.channels[2].data.len(), 1); // x got 9
    }

    /// Bytes lost to the RX ring cap drop the torn line instead of splicing
    /// two unrelated halves (or a beheaded tail) into a bogus sample.
    #[test]
    fn ring_loss_drops_partial_line() {
        let mut p = PlotState::default();
        p.feed(b"a:1\na:", 6); // partial "a:" pending
        // 100 bytes claimed new, but only 8 remain in the ring → hole. The
        // "999\n" fragment is the tail of a torn line — discarded whole.
        p.feed(b"999\na:3\n", 106);
        assert_eq!(p.channels.len(), 1);
        let vals: Vec<f32> = p.channels[0].data.iter().map(|&(_, v)| v).collect();
        assert_eq!(vals, vec![1.0, 3.0]);
    }

    #[test]
    fn paused_drops_data_and_overlong_lines_are_discarded() {
        let mut p = PlotState::default();
        p.feed(b"a:1\n", 4);
        p.paused = true;
        p.feed(b"a:2\n", 8);
        p.paused = false;
        p.feed(b"a:3\n", 12);
        let vals: Vec<f32> = p.channels[0].data.iter().map(|&(_, v)| v).collect();
        assert_eq!(vals, vec![1.0, 3.0]);

        // A line longer than MAX_LINE is discarded whole (binary guard) —
        // including its tail after the cap was hit.
        let mut q = PlotState::default();
        let mut long = vec![b'7'; MAX_LINE + 10];
        long.push(b'\n');
        long.extend_from_slice(b"b:5\n");
        q.feed(&long, long.len() as u64);
        assert_eq!(q.channels.len(), 1);
        assert_eq!(q.channels[0].name, "b");
    }

    /// Pausing mid-LINE and resuming must not parse the tail as a line. The
    /// existing test above pauses on a newline boundary, where nothing is owed;
    /// this is the other half, and it used to invent a sample per resume.
    #[test]
    fn resuming_in_the_middle_of_a_line_drops_its_tail() {
        let mut p = PlotState::default();
        p.feed(b"a:1\n", 4);
        p.paused = true;
        // Stops MID-LINE: "a:2" has no newline yet, so the stream does not
        // resume at a line start.
        p.feed(b"a:2", 7);
        p.paused = false;
        // ":999" is the tail of the line whose head was dropped. Read as a
        // whole line it would invent a sample; it must go to the newline.
        p.feed(b":999\na:3\n", 16);
        let vals: Vec<f32> = p.channels[0].data.iter().map(|&(_, v)| v).collect();
        assert_eq!(vals, vec![1.0, 3.0], "the torn tail became a sample");
        assert_eq!(p.channels.len(), 1);
    }

    #[test]
    fn cap_evicts_oldest_points() {
        let mut p = PlotState::default();
        let mut stream = String::new();
        for i in 0..(CAP + 10) {
            stream.push_str(&format!("{i}\n"));
        }
        p.feed(stream.as_bytes(), stream.len() as u64);
        let ch = &p.channels[0];
        assert_eq!(ch.data.len(), CAP);
        assert_eq!(ch.data.front().unwrap().1, 10.0); // oldest 10 evicted
    }

    #[test]
    fn nice_step_picks_125() {
        let approx = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(approx(nice_step(10.0, 4.0), 2.0));
        assert!(approx(nice_step(100.0, 4.0), 20.0));
        assert!(approx(nice_step(1.0, 4.0), 0.2)); // raw 0.25 → norm 2.5 → 2×0.1
        assert!(approx(nice_step(0.04, 4.0), 0.01));
    }
}
