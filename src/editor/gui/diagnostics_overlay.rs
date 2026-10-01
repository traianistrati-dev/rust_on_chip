//! Inline diagnostic visualization — wavy underlines, error messages, tooltips.

use crate::editor::gui::text_pos::{GalleyRows, LineIndex, draw_wavy_underline};
use crate::lsp::LspDiagnostic;
use eframe::egui;
use egui_phosphor::regular as ph;

/// Draw inline diagnostics (wavy underlines, inline messages, hover tooltips)
/// for the currently visible code in the editor.
///
/// Called after rendering the code editor but before closing the UI panel.
/// The rustc error-index URL for a compiler error code like `E0599`, or `None`
/// for lint names (e.g. `unused_variables`) which have no such page.
fn rustc_error_doc_url(code: &str) -> Option<String> {
    let digits = code.strip_prefix('E')?;
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        Some(format!("https://doc.rust-lang.org/error_codes/{code}.html"))
    } else {
        None
    }
}

/// Paint one full-width translucent band over `line` (1-based) of the editor.
///
/// The band lands OVER the text, so `color` must stay translucent — an opaque
/// fill hides the very line it is pointing at.
///
/// Standalone (rather than only inside [`show_diagnostics_overlay`]) so a
/// highlight can be drawn on a file rust-analyzer doesn't track, or while the
/// inline-errors toggle is off — neither has anything to do with wanting to see
/// where a jump landed.
pub fn show_line_band(
    ui: &egui::Ui,
    galley_pos: egui::Pos2,
    text_clip_rect: egui::Rect,
    galley: &egui::text::Galley,
    // Of the text the galley shows (`display_code`).
    line_index: &LineIndex,
    line: u32,
    color: egui::Color32,
) {
    let total_chars = line_index.total_chars();
    let ci = line_index.pos_to_char_idx(line, 1).min(total_chars);
    let loc = galley.pos_from_cursor(egui::text::CCursor::new(ci));
    let y_top = galley_pos.y + loc.min.y;
    let y_bot = galley_pos.y + loc.max.y;
    if y_bot < text_clip_rect.top() || y_top > text_clip_rect.bottom() {
        return; // scrolled out of view
    }
    ui.painter().with_clip_rect(text_clip_rect).rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(text_clip_rect.left(), y_top),
            egui::pos2(text_clip_rect.right(), y_bot),
        ),
        0.0,
        color,
    );
}

/// The inline message's font. Monospace, which is what lets one glyph's advance
/// size any message (see `char_w` in the overlay).
fn msg_font() -> egui::FontId {
    egui::FontId::monospace(10.5)
}

/// Deepest `<…>` nesting in a type label.
fn max_depth(label: &str) -> usize {
    let (mut d, mut best, mut prev) = (0usize, 0usize, ' ');
    for c in label.chars() {
        match c {
            '<' => {
                d += 1;
                best = best.max(d);
            }
            // `->` in `impl Fn(A) -> B` is not a closing bracket.
            '>' if prev != '-' => d = d.saturating_sub(1),
            _ => {}
        }
        prev = c;
    }
    best
}

/// `label` with the contents of every `<…>` deeper than `depth` replaced by an
/// ellipsis. `depth 0` collapses the outermost argument list itself.
fn collapse_to_depth(label: &str, depth: usize) -> String {
    let mut out = String::with_capacity(label.len());
    let (mut d, mut skipping, mut prev) = (0usize, 0usize, ' ');
    for c in label.chars() {
        match c {
            '<' => {
                d += 1;
                if skipping > 0 {
                    skipping += 1;
                } else if d > depth {
                    // Enter the first group past the budget: emit the marker
                    // once and swallow everything up to its match.
                    out.push('<');
                    out.push('…');
                    skipping = 1;
                } else {
                    out.push('<');
                }
            }
            '>' if prev != '-' => {
                d = d.saturating_sub(1);
                if skipping > 0 {
                    skipping -= 1;
                    if skipping == 0 {
                        out.push('>');
                    }
                } else {
                    out.push('>');
                }
            }
            other => {
                if skipping == 0 {
                    out.push(other);
                }
            }
        }
        prev = c;
    }
    out
}

/// The outermost `<…>` split into its top-level arguments, as
/// `(head, args, tail)`. `None` when the label has no generic list.
fn split_top_args(label: &str) -> Option<(String, Vec<String>, String)> {
    let chars: Vec<char> = label.chars().collect();
    let open = chars.iter().position(|&c| c == '<')?;
    let (mut d, mut close, mut prev) = (0usize, None, ' ');
    for (i, &c) in chars.iter().enumerate().skip(open) {
        match c {
            '<' => d += 1,
            '>' if prev != '-' => {
                d -= 1;
                if d == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
        prev = c;
    }
    let close = close?;
    let inner: String = chars[open + 1..close].iter().collect();
    let mut args = Vec::new();
    let (mut d, mut start, mut prev) = (0usize, 0usize, ' ');
    let ic: Vec<char> = inner.chars().collect();
    for (i, &c) in ic.iter().enumerate() {
        match c {
            '<' | '(' | '[' => d += 1,
            '>' if prev != '-' => d = d.saturating_sub(1),
            ')' | ']' => d = d.saturating_sub(1),
            ',' if d == 0 => {
                args.push(ic[start..i].iter().collect::<String>().trim().to_owned());
                start = i + 1;
            }
            _ => {}
        }
        prev = c;
    }
    args.push(ic[start..].iter().collect::<String>().trim().to_owned());
    Some((
        chars[..=open].iter().collect(),
        args,
        chars[close..].iter().collect(),
    ))
}

/// Give up whole top-level arguments, LONGEST first, until it fits.
///
/// The rung between "collapse the nesting" and "collapse everything". Without
/// it the ladder fell off a cliff — a 78-character form, then straight to a
/// 15-character one, so every budget in between showed far less than it had
/// room for.
fn collapse_longest_args(label: &str, max_chars: usize) -> Option<String> {
    let (head, mut args, tail) = split_top_args(label)?;
    if args.len() < 2 {
        return None; // nothing to choose between; the depth ladder covers it
    }
    let render = |a: &[String]| format!("{head}{}{tail}", a.join(", "));
    loop {
        let out = render(&args);
        if out.chars().count() <= max_chars {
            return Some(out);
        }
        // The longest argument that has not already been given up.
        let victim = args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a != "…")
            .max_by_key(|(_, a)| a.chars().count())
            .map(|(i, _)| i)?;
        args[victim] = "…".to_owned();
    }
}

/// A type label shortened to at most `max_chars`, by collapsing its generic
/// arguments from the INSIDE out.
///
/// A blind character cut turns
/// `Ssd1306Async<I2CInterface<I2c<'_, Async>>, DisplaySize128x32, …>` into
/// `Ssd1306Async<I2CInterface<I2c<'_, Asy…`, which loses even the fact that the
/// type has three parameters. Collapsing keeps the structure and gives up the
/// detail, which is the right way round: the head is what identifies the type.
///
/// The most collapsed form (`Name<…>`) is short by construction, so it fits
/// almost any budget — which is what lets the caller stop sliding the hint
/// leftwards over the code to make room.
pub(crate) fn shorten_type(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_owned();
    }
    // Least aggressive first: keep as much nesting as still fits.
    for d in (1..max_depth(label)).rev() {
        let s = collapse_to_depth(label, d);
        if s.chars().count() <= max_chars {
            return s;
        }
    }
    // Then give up whole arguments rather than all of them at once.
    let flattened = collapse_to_depth(label, 1);
    if let Some(s) = collapse_longest_args(&flattened, max_chars) {
        return s;
    }
    // Everything inside the outermost list.
    let bare = collapse_to_depth(label, 0);
    if bare.chars().count() <= max_chars {
        return bare;
    }
    // Not even the bare head fits (a very long type NAME, or almost no room).
    let mut out: String = label.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Gap kept between a line's "N refs" pill and the inline message after it.
const PILL_GAP: f32 = 10.0;

/// Where the inline message starts: after this line's pill when there is one,
/// and never before the position it would have taken on its own.
///
/// `max`, not "pill_right + gap", because a pill can be narrower than the
/// message's own 16 px indent — moving the message LEFT to hug a short pill
/// would be a second bug wearing the first one's clothes.
pub(crate) fn inline_message_x(base_x: f32, pill_right: Option<f32>) -> f32 {
    match pill_right {
        Some(right) => base_x.max(right + PILL_GAP),
        None => base_x,
    }
}

/// `text` cut to what fits in `avail` pixels at `char_w` per character, or
/// `None` when so little room is left that a stub would say nothing.
///
/// Monospace, so the character count is exact rather than a guess. The cut keeps
/// a trailing `…`; the caller already uses that character to mean "there is
/// more", so a width-elided message reads as truncated and not as broken.
pub(crate) fn fit_to_width(text: &str, avail: f32, char_w: f32) -> Option<String> {
    if char_w <= 0.0 {
        return Some(text.to_owned());
    }
    // Below this the message is a couple of letters and an ellipsis, which is
    // noise on top of the code; the hover tooltip and the error list still carry
    // the full text.
    const MIN_CHARS: usize = 8;
    let fits = (avail / char_w).floor().max(0.0) as usize;
    if text.chars().count() <= fits {
        return Some(text.to_owned());
    }
    if fits < MIN_CHARS {
        return None;
    }
    let mut out: String = text.chars().take(fits - 1).collect();
    out.push('…');
    Some(out)
}

/// How long the tooltip's button reads "Copied!" after a click.
const COPIED_FLASH_SECS: f64 = 1.5;

/// The label of the tooltip's copy button: `shown` messages, and whether one
/// was copied a moment ago.
fn copy_button_label(shown: usize, copied: bool) -> String {
    if copied {
        format!("{} Copied!", ph::CHECK)
    } else if shown > 1 {
        format!("{} Copy all ({shown})", ph::COPY)
    } else {
        format!("{} Copy", ph::COPY)
    }
}

/// The 1-based CHARACTER column of `d`, the one rustc prints.
///
/// `d.col` is rust-analyzer's, counted in UTF-16 units (the client negotiates
/// no position encoding): one too many per emoji or other astral character
/// earlier on the line.
fn char_column(line_index: &LineIndex, d: &LspDiagnostic) -> u32 {
    let at = line_index.pos_to_char_idx(d.line, d.col);
    let start = line_index.pos_to_char_idx(d.line, 1);
    (at.saturating_sub(start) + 1) as u32
}

/// Whether this frame's click landed in the inline-diagnostic tooltip drawn
/// at `at` = `(cumulative_frame_nr, rect)`.
///
/// Last frame's tooltip counts as well as this one's: each editor view asks
/// right after its own text box, and the tooltip may belong to the OTHER view,
/// which draws it later in the frame.
pub(crate) fn click_in_tooltip(
    at: Option<(u64, egui::Rect)>,
    frame: u64,
    click: Option<egui::Pos2>,
) -> bool {
    at.is_some_and(|(drawn, rect)| drawn + 1 >= frame && click.is_some_and(|p| rect.contains(p)))
}

/// What the copy button puts on the clipboard: one diagnostic per entry, in
/// the tooltip's order, each led by where it is —
/// `src/main.rs:304:12: error[E0425]: cannot find value …` — so the paste
/// says which file and line it is about. The first line is rustc's own
/// `path:line:col` form, with `col_of` giving the character column (see
/// [`char_column`]); any further lines of the message (rustc's `note:` and
/// friends) follow indented, so every entry still starts at column 0.
fn diagnostic_copy_text(
    diags: &[LspDiagnostic],
    shown: &[usize],
    file: Option<&str>,
    col_of: impl Fn(&LspDiagnostic) -> u32,
) -> String {
    shown
        .iter()
        .map(|&i| {
            let d = &diags[i];
            let place = match file {
                Some(f) => format!("{f}:{}:{}", d.line, col_of(d)),
                None => format!("{}:{}", d.line, col_of(d)),
            };
            let kind = match d.severity {
                crate::lsp::DiagSeverity::Error => "error",
                crate::lsp::DiagSeverity::Warning => "warning",
                crate::lsp::DiagSeverity::Info => "info",
                crate::lsp::DiagSeverity::Hint => "hint",
            };
            let kind = match &d.code {
                Some(c) => format!("{kind}[{c}]"),
                None => kind.to_owned(),
            };
            let mut out = format!("{place}: {kind}: {}", d.headline());
            for line in d.message.lines().skip(1) {
                let line = line.trim_end();
                out.push('\n');
                if !line.is_empty() {
                    out.push_str("  ");
                    out.push_str(line);
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Returns where the hover tooltip was drawn this frame, if it was: a click
/// in it takes keyboard focus from the editor, and the editor pass gives it
/// back (see [`click_in_tooltip`]).
pub fn show_diagnostics_overlay(
    ui: &mut egui::Ui,
    galley_pos: egui::Pos2,
    text_clip_rect: egui::Rect,
    galley: &egui::text::Galley,
    diags: &[LspDiagnostic],
    // Of `display_code`, the text every diagnostic position refers to.
    line_index: &LineIndex,
    // The file `diags` belong to (`src/main.rs`, …), named in the copied text.
    file: Option<&str>,
    // `highlight`: (1-based line, band colour) of the diagnostic the user clicked
    // in the bottom panel — drawn as a translucent band (colour keyed by
    // severity: error red / warning yellow / info blue).
    highlight: Option<(u32, egui::Color32)>,
    // `def_line`: 1-based line of the F12 go-to-definition target (when it's in
    // this project file) — drawn with a translucent yellow band, like the
    // Definition tab.
    def_line: Option<u32>,
    // `pill_edges`: (1-BASED line, right edge in screen x) of every "N refs"
    // pill the usages overlay painted earlier in this same frame. The inline
    // message steps around them; see [`inline_message_x`].
    pill_edges: &[(u32, f32)],
    // A 1-based line whose inline message is left out: the call-signature
    // ghost is drawn there instead (see `editor_panel::signature_hint`).
    quiet_line: Option<u32>,
) -> Option<egui::Rect> {
    let total_chars = line_index.total_chars();
    // Every position below is `galley.pos_from_cursor`, looked up by binary
    // search: one row table for the pass instead of a row walk per lookup.
    let rows = GalleyRows::new(galley);

    // Painter clipped to editor bounds.
    let gp = galley_pos;
    let clip = text_clip_rect;
    let painter = ui.painter().with_clip_rect(clip);

    // ── Full-width line-highlight bands ───────────────────────────────────
    // Drawn before the diagnostics (so squiggles/messages render on top) AND
    // before the empty-diags return below (the def target may be a clean file).
    let band = |line: u32, color: egui::Color32| {
        let ci = line_index.pos_to_char_idx(line, 1).min(total_chars);
        let loc = rows.pos(ci);
        let y_top = gp.y + loc.min.y;
        let y_bot = gp.y + loc.max.y;
        if y_bot >= clip.top() && y_top <= clip.bottom() {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(clip.left(), y_top),
                    egui::pos2(clip.right(), y_bot),
                ),
                0.0,
                color,
            );
        }
    };
    // F12 definition line — translucent yellow (matches the Definition tab).
    if let Some(line) = def_line {
        band(
            line,
            egui::Color32::from_rgba_unmultiplied(255, 214, 90, 32),
        );
    }
    // Clicked-diagnostic line — translucent band, colour keyed by severity.
    if let Some((line, color)) = highlight {
        band(line, color);
    }

    if diags.is_empty() {
        return None;
    }

    // Lines that already drew an inline message — a line can carry several
    // diagnostics, but a second message would overlap the first, so show one.
    let mut msg_lines: Vec<u32> = quiet_line.into_iter().collect();
    // `(index into diags, hover rect)` of every visible diagnostic, grouped into
    // tooltips after the drawing pass.
    let mut hover_spans: Vec<(usize, egui::Rect)> = Vec::new();

    // One measurement for the whole pass. The message font is monospace, so a
    // single glyph's advance sizes every message; measuring inside the loop laid
    // out an "M" once per diagnostic per frame for the same answer.
    let char_w = painter
        .layout_no_wrap("M".to_owned(), msg_font(), egui::Color32::WHITE)
        .size()
        .x;

    // ── Per-diagnostic: underline + inline message + tooltip ──────────────
    for (di, diag) in diags.iter().enumerate() {
        let start_ci = line_index
            .pos_to_char_idx(diag.line, diag.col)
            .min(total_chars);

        // Galley-local position of the start
        let loc_s = rows.pos(start_ci);

        // Screen coordinates
        let sx = gp.x + loc_s.min.x;
        let sy_top = gp.y + loc_s.min.y;
        let sy_bot = gp.y + loc_s.max.y;
        let line_h = loc_s.height().max(1.0);
        let sy_mid = (sy_top + sy_bot) * 0.5;

        // Skip lines scrolled out of the visible editor — otherwise the squiggle,
        // inline message, and hover region would land below the editor in the
        // bottom diagnostics panel (the painter clip hides the drawing, but the
        // hover interaction must be skipped too).
        //
        // Decided from the START alone, so it runs before the end and
        // end-of-line lookups: most of a file's diagnostics are off screen.
        if sy_bot < clip.top() || sy_top > clip.bottom() {
            continue;
        }

        let end_ci_raw = line_index
            .pos_to_char_idx(diag.end_line, diag.end_col)
            .min(total_chars);
        let end_ci = if end_ci_raw <= start_ci {
            (start_ci + 1).min(total_chars)
        } else {
            end_ci_raw
        };
        let loc_e = rows.pos(end_ci);

        // Same-line check
        let same_line = (loc_s.min.y - loc_e.min.y).abs() < line_h * 0.5;
        let ex = if same_line {
            gp.x + loc_e.min.x
        } else {
            gp.x + galley.rect.width()
        };
        if ex <= sx + 1.0 {
            continue;
        }

        // Severity colours
        let (ul_color, bg_color, msg_color) = match diag.severity {
            crate::lsp::DiagSeverity::Error => (
                egui::Color32::from_rgb(220, 65, 55),
                egui::Color32::from_rgba_unmultiplied(210, 55, 45, 22),
                egui::Color32::from_rgb(200, 80, 70),
            ),
            crate::lsp::DiagSeverity::Warning => (
                egui::Color32::from_rgb(210, 165, 35),
                egui::Color32::from_rgba_unmultiplied(200, 160, 30, 14),
                egui::Color32::from_rgb(190, 150, 40),
            ),
            crate::lsp::DiagSeverity::Info => (
                egui::Color32::from_rgb(80, 140, 215),
                egui::Color32::TRANSPARENT,
                egui::Color32::from_rgb(100, 150, 210),
            ),
            crate::lsp::DiagSeverity::Hint => (
                egui::Color32::from_rgb(100, 160, 110),
                egui::Color32::TRANSPARENT,
                egui::Color32::from_rgb(110, 150, 110),
            ),
        };

        // Background tint
        if bg_color.a() > 0 {
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(sx, sy_top), egui::pos2(ex, sy_bot)),
                0.0,
                bg_color,
            );
        }

        // Wavy underline
        draw_wavy_underline(&painter, sx, ex, sy_bot, ul_color);

        // ── Inline message at end of line ─────────────────────────────────
        let eol_ci = line_index.line_end_char_idx(diag.line).min(total_chars);
        let loc_eol = rows.pos(eol_ci);
        let same_row_eol = (loc_s.min.y - loc_eol.min.y).abs() < line_h * 0.5;
        // Only one inline message per line (a second would overlap the first).
        if same_row_eol && !msg_lines.contains(&diag.line) {
            msg_lines.push(diag.line);
            // Start after this line's "N refs" pill when it has one. The pill
            // begins at end-of-line + 14 and the message at + 16, so before this
            // the message was painted straight through it.
            let pill_right = pill_edges
                .iter()
                .find(|(line, _)| *line == diag.line)
                .map(|(_, right)| *right);
            let msg_x = inline_message_x(gp.x + loc_eol.min.x + 16.0, pill_right);
            // First line only, then cap length — a multi-line message rendered
            // raw would draw extra rows and overlap the code below it.
            let headline = diag.headline();
            let short_msg: String = headline.chars().take(72).collect();
            let short_msg = if headline.chars().count() > 72 || diag.has_more_lines() {
                format!("{short_msg}…")
            } else {
                short_msg
            };
            // Fit what is left of the row, rather than running off the edge.
            //
            // The message never had a right-edge rule and was already cut mid-word
            // by the clip; stepping around the pill spends more of the same room,
            // so it now elides deliberately and keeps the ellipsis the 72-char cap
            // above already uses as the "there is more" signal.
            //
            // Deliberately NOT the clamp `show_inlay_hint` uses below: that one
            // right-ALIGNS its text against the clip edge, which is right for a
            // short ghost type and wrong here — it would drag a 450 px message
            // hundreds of pixels left, over the code of the line it annotates.
            if let Some(fitted) = fit_to_width(&short_msg, clip.right() - 4.0 - msg_x, char_w) {
                painter.text(
                    egui::pos2(msg_x, sy_mid),
                    egui::Align2::LEFT_CENTER,
                    &fitted,
                    msg_font(),
                    msg_color,
                );
            }
        }

        // The call-signature ghost owns `quiet_line` past its end. A span that
        // runs on to a later line reaches the galley's full width there, over
        // the ghost, and the two tooltips would compete: stop it at the end
        // of the line. Squiggle and tint are untouched.
        let hover_ex = if !same_line && quiet_line == Some(diag.line) {
            ex.min(gp.x + loc_eol.min.x + 8.0).max(sx + 1.0)
        } else {
            ex
        };
        // The hover region is collected, not interacted with here: several
        // diagnostics on one span each opened their OWN tooltip at the same
        // spot, drawn on top of each other. See the grouping pass below.
        hover_spans.push((
            di,
            egui::Rect::from_min_max(egui::pos2(sx, sy_top), egui::pos2(hover_ex, sy_bot + 3.0)),
        ));
    }

    // ── Hover tooltip: ONE per group of overlapping spans ─────────────────
    // Grouped by geometry, never by where the pointer is. The tooltip is
    // interactive — the user moves into it to click Copy or a docs link — and
    // at that moment the pointer has left every span; a group chosen from the
    // pointer would dissolve right then and close the tooltip under the click.
    let rects: Vec<egui::Rect> = hover_spans.iter().map(|(_, r)| *r).collect();
    let mut drawn: Option<egui::Rect> = None;
    for group in group_overlapping(&rects) {
        let members: Vec<usize> = group.iter().map(|&k| hover_spans[k].0).collect();
        let area = group
            .iter()
            .map(|&k| hover_spans[k].1)
            .reduce(|a, b| a.union(b))
            .unwrap_or(egui::Rect::NOTHING);
        // Keyed by the group's first diagnostic, so the id — and with it the
        // open tooltip — holds steady while the set does not change. And by
        // the file: both editor views draw this overlay on the same layer, and
        // two groups sharing an index shared one id — whose tooltip then
        // opened in neither view.
        let hover = ui.interact(
            area,
            egui::Id::new(("inline_diag", file)).with(members[0]),
            egui::Sense::hover(),
        );
        let shown = tooltip_order(diags, &members);

        // Copying is a button in the tooltip. It used to be Ctrl+C while the
        // pointer rested on the span, which overwrote the clipboard with the
        // error even when the user had just selected code to copy — the mouse
        // parked on a squiggle was enough.
        //
        // The "Copied!" moment is kept in egui's temp data, keyed by this
        // group, because the tooltip is rebuilt every frame.
        let copied_id = hover.id.with("copied");
        let now = ui.input(|i| i.time);
        let since_copy = ui
            .ctx()
            .data(|d| d.get_temp::<f64>(copied_id))
            .map(|t| now - t)
            .filter(|dt| (0.0..COPIED_FLASH_SECS).contains(dt));

        // `Tooltip::show` is what `on_hover_ui` calls; used directly for the
        // rect it reports, which the editor passes need (see the return).
        let tip = egui::Tooltip::for_enabled(&hover).show(|ui: &mut egui::Ui| {
            ui.set_max_width(420.0);
            for (n, &i) in shown.iter().enumerate() {
                if n > 0 {
                    ui.separator();
                }
                diagnostic_tooltip_entry(ui, &diags[i]);
            }
            ui.add_space(2.0);
            let label = copy_button_label(shown.len(), since_copy.is_some());
            // The button also keeps EVERY tooltip open while the pointer
            // travels into it — egui holds a tooltip only when it carries an
            // interactive widget, which before was the docs link of an E-code
            // alone — so it now stays over the lines below a little longer.
            // Unavoidable: a button that closes as you reach it is no button.
            if ui
                .add(egui::Button::new(egui::RichText::new(label).size(10.5)).small())
                .clicked()
            {
                let text =
                    diagnostic_copy_text(diags, &shown, file, |d| char_column(line_index, d));
                ui.ctx().copy_text(text);
                ui.ctx().data_mut(|d| d.insert_temp(copied_id, now));
                // Flip the label on the next frame; that frame asks for the
                // one wake-up below that flips it back.
                ui.ctx().request_repaint();
            } else if let Some(dt) = since_copy {
                // One wake-up when the flash ends — no repaint loop while idle.
                ui.ctx()
                    .request_repaint_after_secs((COPIED_FLASH_SECS - dt) as f32);
            }
        });
        if let Some(tip) = tip {
            let r = tip.response.rect;
            drawn = Some(drawn.map_or(r, |d| d.union(r)));
        }
    }
    drawn
}

/// One diagnostic inside the hover tooltip: icon + full message, then its code
/// (a docs link for a rustc error code).
fn diagnostic_tooltip_entry(ui: &mut egui::Ui, diag: &LspDiagnostic) {
    let icon = match diag.severity {
        crate::lsp::DiagSeverity::Error => ph::X_CIRCLE,
        crate::lsp::DiagSeverity::Warning => ph::WARNING,
        crate::lsp::DiagSeverity::Info => ph::INFO,
        crate::lsp::DiagSeverity::Hint => ph::DOT_OUTLINE,
    };
    ui.label(egui::RichText::new(format!("{icon}  {}", diag.message)).size(12.0));
    let Some(c) = &diag.code else {
        return;
    };
    match rustc_error_doc_url(c) {
        // Clickable link → opens the rust error index in the browser.
        Some(url) => {
            ui.hyperlink_to(
                // `ARROW_SQUARE_OUT`, not a raw `↗`. Reading the bundled fonts'
                // cmaps afterwards showed U+2197 is one of the ten arrows
                // NotoEmoji does carry, so this one was probably rendering — the
                // guard flagged it on the block rule, not on measured tofu. Kept
                // as phosphor anyway: the standing rule is icons in UI text,
                // whatever the font happens to cover today.
                egui::RichText::new(format!(
                    "[{c}]  open docs {}",
                    egui_phosphor::regular::ARROW_SQUARE_OUT
                ))
                .size(10.5)
                .color(egui::Color32::from_rgb(110, 165, 240)),
                url,
            );
        }
        None => {
            ui.label(
                egui::RichText::new(format!("[{c}]"))
                    .size(10.5)
                    .color(egui::Color32::from_rgb(140, 150, 170)),
            );
        }
    }
}

/// Partition hover spans into groups that share a tooltip: two spans belong
/// together when they OVERLAP, and the relation is transitive (A–B and B–C put
/// all three in one group). Spans that merely touch at an edge — two adjacent
/// diagnostics on one line — stay apart. Each group lists indices into `rects`
/// in ascending order; groups are ordered by their first index.
pub(crate) fn group_overlapping(rects: &[egui::Rect]) -> Vec<Vec<usize>> {
    let overlaps = |a: &egui::Rect, b: &egui::Rect| {
        a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y
    };
    // Union-find over a handful of spans: the visible diagnostics of a screen.
    let mut parent: Vec<usize> = (0..rects.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for i in 0..rects.len() {
        for j in i + 1..rects.len() {
            if overlaps(&rects[i], &rects[j]) {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                if ri != rj {
                    parent[ri.max(rj)] = ri.min(rj);
                }
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut slot_of_root: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for i in 0..rects.len() {
        let r = root(&mut parent, i);
        let slot = *slot_of_root.entry(r).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[slot].push(i);
    }
    groups
}

/// The diagnostics of one tooltip, in the order they are listed: most severe
/// first, then by position — so the order never depends on which source
/// published first. The same finding reported twice (rust-analyzer's native
/// pass AND cargo check both emit `unused_mut`) is listed once: same range, same
/// code, same first line. The copy with the longer message wins, since rustc
/// appends its `note:` / `help:` lines to the same headline.
pub(crate) fn tooltip_order(diags: &[LspDiagnostic], members: &[usize]) -> Vec<usize> {
    fn rank(s: crate::lsp::DiagSeverity) -> u8 {
        match s {
            crate::lsp::DiagSeverity::Error => 0,
            crate::lsp::DiagSeverity::Warning => 1,
            crate::lsp::DiagSeverity::Info => 2,
            crate::lsp::DiagSeverity::Hint => 3,
        }
    }
    let mut kept: Vec<usize> = Vec::new();
    for &i in members {
        let d = &diags[i];
        let twin = kept.iter().position(|&k| {
            let o = &diags[k];
            (o.line, o.col, o.end_line, o.end_col) == (d.line, d.col, d.end_line, d.end_col)
                && o.code == d.code
                && o.headline() == d.headline()
        });
        match twin {
            Some(p) if diags[kept[p]].message.len() < d.message.len() => kept[p] = i,
            Some(_) => {}
            None => kept.push(i),
        }
    }
    kept.sort_by_key(|&i| {
        let d = &diags[i];
        (rank(d.severity), d.line, d.col, i)
    });
    kept
}

/// Draw a single inferred-type inlay hint as dim ghost text just past the END
/// of the line at `eol_idx` (the char index of that line's last character in
/// `display_code`). Positioned through the galley so it tracks scrolling, and
/// clipped to the visible editor area. Purely visual — the text is NOT part of
/// the document (accepting it is handled separately, by the caller's Tab
/// binding).
///
/// It sits after the line rather than inline after the binding name because an
/// overlay can't reflow the real text: an inline hint painted over the ` =
/// initializer …`, so it read as garbage (`parser:=Parser…`).
/// Shortest inline form worth drawing. Below this the hint is a couple of
/// letters and an ellipsis, which says less than the marker does.
const MIN_INLINE_CHARS: usize = 12;

/// Drawn at the right edge when the line leaves no room at all — hover it for
/// the type.
const MARKER: &str = ": …";

pub fn show_inlay_hint(
    ui: &egui::Ui,
    galley_pos: egui::Pos2,
    text_clip_rect: egui::Rect,
    galley: &egui::text::Galley,
    eol_idx: usize,
    label: &str,
    font_size: f32,
) {
    let painter = ui.painter().with_clip_rect(text_clip_rect);
    let loc = galley.pos_from_cursor(egui::text::CCursor::new(eol_idx));
    let y_top = galley_pos.y + loc.min.y;
    let y_bot = galley_pos.y + loc.max.y;
    // Skip when scrolled out of the visible editor.
    if y_bot < text_clip_rect.top() || y_top > text_clip_rect.bottom() {
        return;
    }
    // A gap past the line's end, mirroring the inline-diagnostic messages.
    // rust-analyzer's type-hint label already includes the leading `: `
    // (renderColons default); render it verbatim, dimmed like an editor hint.
    let font = egui::FontId::monospace(font_size);
    let color = egui::Color32::from_rgb(150, 165, 180);
    let char_w = painter
        .layout_no_wrap("M".to_owned(), font.clone(), color)
        .size()
        .x
        .max(1.0_f32);
    let y_mid = (y_top + y_bot) * 0.5;
    let eol_x = galley_pos.x + loc.max.x + 16.0;
    let right = text_clip_rect.right() - 4.0;

    // The hint is ANCHORED after the line and never slides left.
    //
    // It used to: `x = eol_x.min(clip.right() - text_w - 4)`, added so a short
    // hint on a horizontally-scrolled line stayed visible. Once real embedded
    // types came through whole, that clamp collapsed to the LEFT EDGE and
    // painted a 900 px type straight across the code it was annotating. Room is
    // made by shortening the type instead — `shorten_type` keeps the head and
    // gives up the nesting, and its most collapsed form fits almost anything.
    let room = ((right - eol_x) / char_w).floor().max(0.0) as usize;
    let (text, x) = if room >= MIN_INLINE_CHARS {
        (shorten_type(label, room), eol_x)
    } else {
        // No room after the line at all: it runs past the right edge. Rather
        // than draw nothing (the type would be unreachable) or slide across the
        // code, pin a marker at the edge — three characters of overlap instead
        // of the whole line, and the hover below still has the full type.
        let w = MARKER.chars().count() as f32 * char_w;
        (MARKER.to_owned(), (right - w).max(text_clip_rect.left()))
    };

    let drawn = painter.text(
        egui::pos2(x, y_mid),
        egui::Align2::LEFT_CENTER,
        &text,
        font,
        color,
    );

    // Whatever was shown, the WHOLE type is one hover away. That is the half
    // that makes shortening acceptable: the detail is given up on screen, not
    // lost.
    if text != label {
        ui.interact(
            drawn,
            egui::Id::new(("inlay_hint", eol_idx)),
            egui::Sense::hover(),
        )
        .on_hover_text(label);
    }
}

#[cfg(test)]
mod copy_button_tests {
    use super::{char_column, click_in_tooltip, copy_button_label, diagnostic_copy_text};
    use crate::editor::gui::text_pos::LineIndex;
    use crate::lsp::{DiagSeverity, LspDiagnostic};
    use eframe::egui::{Rect, pos2};

    fn diag(
        sev: DiagSeverity,
        line: u32,
        col: u32,
        code: Option<&str>,
        message: &str,
    ) -> LspDiagnostic {
        LspDiagnostic {
            severity: sev,
            message: message.to_owned(),
            line,
            col,
            end_line: line,
            end_col: col + 5,
            code: code.map(str::to_owned),
            source: "rustc".to_owned(),
        }
    }

    /// The report's error: the paste names the file, line and column, the
    /// severity and the code — rustc's own `path:line:col` form.
    #[test]
    fn one_error_is_copied_with_its_place_and_code() {
        let diags = vec![diag(
            DiagSeverity::Error,
            304,
            12,
            Some("E0425"),
            "cannot find value `selected_idex_out_of_vetical_visibility` in this scope",
        )];
        assert_eq!(
            diagnostic_copy_text(&diags, &[0], Some("src/main.rs"), |d| d.col),
            "src/main.rs:304:12: error[E0425]: cannot find value \
             `selected_idex_out_of_vetical_visibility` in this scope"
        );
    }

    /// rustc's extra lines stay, indented under their headline, so each entry
    /// of a "Copy all" still starts at column 0; blank lines are not padded.
    #[test]
    fn further_message_lines_follow_indented() {
        let diags = vec![diag(
            DiagSeverity::Warning,
            9,
            5,
            Some("unused_mut"),
            "variable does not need to be mutable\n\n`#[warn(unused_mut)]` on by default  ",
        )];
        assert_eq!(
            diagnostic_copy_text(&diags, &[0], Some("src/pins.rs"), |d| d.col),
            "src/pins.rs:9:5: warning[unused_mut]: variable does not need to be mutable\n\
             \n  `#[warn(unused_mut)]` on by default"
        );
    }

    /// A group copies in the TOOLTIP's order (`shown`), not the list's, one
    /// entry per line; no code means no brackets.
    #[test]
    fn a_group_copies_every_entry_in_tooltip_order() {
        let diags = vec![
            diag(DiagSeverity::Hint, 3, 1, None, "consider a shorter name"),
            diag(DiagSeverity::Error, 3, 1, Some("E0308"), "mismatched types"),
        ];
        assert_eq!(
            diagnostic_copy_text(&diags, &[1, 0], Some("src/main.rs"), |d| d.col),
            "src/main.rs:3:1: error[E0308]: mismatched types\n\
             src/main.rs:3:1: hint: consider a shorter name"
        );
    }

    #[test]
    fn without_a_file_the_place_is_line_and_column() {
        let diags = vec![diag(DiagSeverity::Info, 7, 2, None, "note")];
        assert_eq!(
            diagnostic_copy_text(&diags, &[0], None, |d| d.col),
            "7:2: info: note"
        );
    }

    /// rust-analyzer counts columns in UTF-16 units, rustc in characters: an
    /// emoji earlier on the line made the copied column one too high each.
    #[test]
    fn the_copied_column_counts_characters_not_utf16_units() {
        let line = r#"fn main() { let _s = "😀😀"; let _x = nope; }"#;
        let text = format!("// first line\n{line}\n");
        let at = line.find("nope").unwrap();
        let utf16_col = line[..at].encode_utf16().count() as u32 + 1;
        let char_col = line[..at].chars().count() as u32 + 1;
        assert_eq!(utf16_col, char_col + 2, "two astral chars before it");
        let d = diag(DiagSeverity::Error, 2, utf16_col, Some("E0425"), "x");
        assert_eq!(char_column(&LineIndex::new(&text), &d), char_col);
    }

    /// An ASCII line is the same either way; a line past the end is column 1
    /// rather than a panic.
    #[test]
    fn the_copied_column_is_unchanged_on_ascii_and_safe_past_the_end() {
        let index = LineIndex::new("let x = nope;\n");
        let d = diag(DiagSeverity::Error, 1, 9, None, "x");
        assert_eq!(char_column(&index, &d), 9);
        let gone = diag(DiagSeverity::Error, 40, 9, None, "x");
        assert_eq!(char_column(&index, &gone), 1);
    }

    /// A click in the tooltip drawn this frame or the last one counts — the
    /// other view draws it later in the frame than this view asks; an older
    /// tooltip, a click outside it, or no click at all does not.
    #[test]
    fn a_click_counts_only_in_a_current_tooltip() {
        let tip = Some((
            10,
            Rect::from_min_max(pos2(100.0, 100.0), pos2(300.0, 160.0)),
        ));
        let inside = Some(pos2(150.0, 130.0));
        assert!(click_in_tooltip(tip, 10, inside));
        assert!(click_in_tooltip(tip, 11, inside));
        assert!(!click_in_tooltip(tip, 12, inside), "a stale tooltip");
        assert!(!click_in_tooltip(tip, 11, Some(pos2(50.0, 130.0))));
        assert!(!click_in_tooltip(tip, 11, None), "no click this frame");
        assert!(!click_in_tooltip(None, 11, inside));
    }

    #[test]
    fn the_label_counts_a_group_and_confirms_a_copy() {
        assert!(copy_button_label(1, false).ends_with(" Copy"));
        assert!(copy_button_label(3, false).ends_with(" Copy all (3)"));
        assert!(copy_button_label(1, true).ends_with(" Copied!"));
        assert!(copy_button_label(3, true).ends_with(" Copied!"));
    }
}

#[cfg(test)]
mod tooltip_group_tests {
    use super::{group_overlapping, tooltip_order};
    use crate::lsp::{DiagSeverity, LspDiagnostic};
    use eframe::egui::{Rect, pos2};

    fn r(x0: f32, x1: f32, y0: f32) -> Rect {
        Rect::from_min_max(pos2(x0, y0), pos2(x1, y0 + 20.0))
    }

    fn diag(sev: DiagSeverity, col: u32, code: &str, message: &str, source: &str) -> LspDiagnostic {
        LspDiagnostic {
            severity: sev,
            message: message.to_owned(),
            line: 289,
            col,
            end_line: 289,
            end_col: col + 29,
            code: Some(code.to_owned()),
            source: source.to_owned(),
        }
    }

    /// The report: three diagnostics on one variable name opened three
    /// tooltips on top of each other. They must share one.
    #[test]
    fn spans_on_the_same_name_share_one_tooltip() {
        let rects = [
            r(100.0, 400.0, 0.0),
            r(100.0, 400.0, 0.0),
            r(100.0, 400.0, 0.0),
        ];
        assert_eq!(group_overlapping(&rects), vec![vec![0, 1, 2]]);
    }

    #[test]
    fn a_chain_of_overlaps_is_one_group() {
        let rects = [r(0.0, 50.0, 0.0), r(200.0, 300.0, 0.0), r(40.0, 210.0, 0.0)];
        assert_eq!(group_overlapping(&rects), vec![vec![0, 1, 2]]);
    }

    /// Two neighbouring diagnostics that only touch keep their own tooltips —
    /// and so do the same columns on different lines.
    #[test]
    fn touching_or_other_line_spans_stay_apart() {
        let rects = [r(0.0, 50.0, 0.0), r(50.0, 90.0, 0.0), r(0.0, 50.0, 20.0)];
        assert_eq!(group_overlapping(&rects), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn no_spans_no_groups() {
        assert!(group_overlapping(&[]).is_empty());
    }

    /// rust-analyzer and cargo check both report `unused_mut`: listed once, and
    /// the copy carrying rustc's extra `note:` line is the one kept.
    #[test]
    fn the_same_finding_from_two_sources_is_listed_once() {
        let diags = vec![
            diag(
                DiagSeverity::Warning,
                9,
                "unused_mut",
                "variable does not need to be mutable",
                "rust-analyzer",
            ),
            diag(
                DiagSeverity::Warning,
                9,
                "unused_mut",
                "variable does not need to be mutable\n`#[warn(unused_mut)]` on by default",
                "rustc",
            ),
        ];
        assert_eq!(tooltip_order(&diags, &[0, 1]), vec![1]);
        assert_eq!(tooltip_order(&diags, &[1, 0]), vec![1]);
    }

    /// Different codes on the same span are different findings.
    #[test]
    fn different_codes_on_one_span_are_all_listed() {
        let diags = vec![
            diag(
                DiagSeverity::Warning,
                9,
                "unused_mut",
                "variable does not need to be mutable",
                "rustc",
            ),
            diag(
                DiagSeverity::Warning,
                9,
                "unused_variables",
                "unused variable: `x`",
                "rustc",
            ),
        ];
        assert_eq!(tooltip_order(&diags, &[0, 1]).len(), 2);
    }

    /// "indiferent de ordine": whatever order the sources published in, the
    /// error leads and the rest follow by position.
    #[test]
    fn errors_lead_whatever_the_arrival_order() {
        let diags = vec![
            diag(DiagSeverity::Hint, 5, "h", "hint", "rust-analyzer"),
            diag(DiagSeverity::Warning, 20, "w2", "later warning", "rustc"),
            diag(DiagSeverity::Warning, 9, "w1", "earlier warning", "rustc"),
            diag(
                DiagSeverity::Error,
                30,
                "E0308",
                "mismatched types",
                "rustc",
            ),
        ];
        let expect = vec![3, 2, 1, 0];
        assert_eq!(tooltip_order(&diags, &[0, 1, 2, 3]), expect);
        assert_eq!(tooltip_order(&diags, &[3, 1, 0, 2]), expect);
    }
}

#[cfg(test)]
mod tests {
    use super::{PILL_GAP, fit_to_width, inline_message_x, shorten_type};

    /// The type from the report that started this: 105 characters.
    const REAL: &str = "Ssd1306Async<I2CInterface<I2c<'_, Async>>, DisplaySize128x32,                         BufferedGraphicsModeAsync<DisplaySize128x32>>";

    #[test]
    fn a_type_that_fits_is_untouched() {
        assert_eq!(shorten_type(REAL, 200), REAL);
    }

    /// The point of collapsing rather than cutting: the head and the ARITY
    /// survive. A blind character cut at 40 gives
    /// `Ssd1306Async<I2CInterface<I2c<'_, Asyn…`, which hides that the type has
    /// three parameters at all.
    #[test]
    fn collapsing_keeps_the_head_and_the_shape() {
        let out = shorten_type(REAL, 60);
        assert!(out.starts_with("Ssd1306Async<"), "{out}");
        assert!(out.ends_with('>'), "still a closed generic: {out}");
        assert!(out.contains('…'), "something was given up: {out}");
        assert!(
            out.chars().count() <= 60,
            "{} chars: {out}",
            out.chars().count()
        );
    }

    /// Less room gives up more nesting, monotonically — never MORE detail for
    /// less space.
    #[test]
    fn a_smaller_budget_never_shows_more() {
        let mut last = usize::MAX;
        for budget in [200, 90, 60, 40, 20] {
            let n = shorten_type(REAL, budget).chars().count();
            assert!(n <= budget.max(1), "budget {budget} produced {n} chars");
            assert!(n <= last, "budget {budget} grew the label back to {n}");
            last = n;
        }
    }

    /// The rung between "collapse the nesting" and "collapse everything".
    ///
    /// Without it the ladder fell off a cliff: 78 characters at one budget, 15
    /// at the next, so every budget in between drew a bare `Name<…>` while it
    /// had room for three times that.
    #[test]
    fn a_middle_budget_gives_up_arguments_not_everything() {
        let out = shorten_type(REAL, 55);
        assert!(
            out.chars().count() > 20,
            "far more than the bare head: {out}"
        );
        assert!(out.chars().count() <= 55, "{out}");
        assert!(
            out.contains("I2CInterface") || out.contains("DisplaySize128x32"),
            "at least one argument survives whole: {out}"
        );
    }

    /// Arguments are given up LONGEST first, so the cheapest information goes
    /// last.
    #[test]
    fn the_longest_argument_is_given_up_first() {
        let t = "Pair<AnExtremelyLongArgumentNameHere, u8>";
        let out = shorten_type(t, 20);
        assert!(out.contains("u8"), "the short one survives: {out}");
        assert!(!out.contains("AnExtremelyLong"), "{out}");
    }

    /// The most collapsed form is short by construction — that is what lets the
    /// caller stop sliding the hint over the code to make room.
    #[test]
    fn the_last_resort_form_is_tiny() {
        let out = shorten_type(REAL, 20);
        assert!(out.chars().count() <= 20, "{out}");
        assert!(
            out.starts_with("Ssd1306Async"),
            "the name always survives: {out}"
        );
    }

    /// `->` inside `impl Fn(A) -> B` is not a closing bracket; treating it as
    /// one unbalances the walk and mangles every type after it.
    #[test]
    fn an_arrow_is_not_a_closing_bracket() {
        let t = "Map<Iter<'a, u8>, impl Fn(u8) -> u16>";
        let out = shorten_type(t, 24);
        assert!(out.starts_with("Map<"), "{out}");
        assert!(out.ends_with('>'), "{out}");
        assert_eq!(out.matches('<').count(), out.matches('>').count(), "{out}");
    }

    /// A type with no generics has nothing to collapse; it must still respect
    /// the budget rather than overflow.
    #[test]
    fn a_plain_type_falls_back_to_a_cut() {
        let out = shorten_type("AVeryLongConcreteTypeNameWithNoGenericsAtAll", 20);
        assert_eq!(out.chars().count(), 20);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn a_non_ascii_type_survives_the_last_resort_cut() {
        let out = shorten_type("Măsurători<Frecvență<u32>>", 8);
        assert!(out.chars().count() <= 8, "{out}");
    }

    /// No pill on the line: the message keeps the position it always had. This
    /// is the common case — most lines carry no "N refs" indicator at all.
    #[test]
    fn a_line_without_a_pill_is_left_where_it_was() {
        assert_eq!(inline_message_x(300.0, None), 300.0);
    }

    /// The reported bug: the pill starts at end-of-line + 14 and the message at
    /// + 16, so the message was painted through it. It now clears the pill's
    /// real right edge by the requested gap.
    #[test]
    fn a_message_clears_the_pill_by_the_full_gap() {
        let pill_right = 352.0;
        let x = inline_message_x(300.0, Some(pill_right));
        assert_eq!(x, pill_right + PILL_GAP);
        assert!(x - pill_right >= 10.0, "at least 10px, as asked");
    }

    /// `max`, not `pill_right + gap` outright. A "1 ref" pill can end LEFT of
    /// where the message would have started on its own, and moving the message
    /// backwards to hug it would be a new bug wearing the old one's clothes.
    #[test]
    fn a_short_pill_never_drags_the_message_backwards() {
        assert_eq!(inline_message_x(500.0, Some(120.0)), 500.0);
    }

    #[test]
    fn a_message_that_fits_is_not_touched() {
        assert_eq!(
            fit_to_width("never used", 400.0, 6.0).as_deref(),
            Some("never used")
        );
    }

    /// Cut to the room that is left, keeping the ellipsis the 72-char cap
    /// already uses — so a width-elided message reads as truncated, not broken.
    #[test]
    fn a_message_too_wide_is_elided_to_what_fits() {
        let long = "fields `normal`, `night`, and `max` are never read";
        let out = fit_to_width(long, 60.0, 6.0).expect("10 chars is plenty of room");
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
        assert!(long.starts_with(&out[..out.len() - '…'.len_utf8()]));
    }

    /// A sliver of room says nothing worth the pixels; the hover tooltip and the
    /// error list still carry the whole message.
    #[test]
    fn too_little_room_draws_nothing_rather_than_a_stub() {
        assert_eq!(fit_to_width("mismatched types", 30.0, 6.0), None);
    }

    /// A degenerate font measurement must not divide by zero or silently blank
    /// every message in the editor.
    #[test]
    fn a_zero_width_measurement_falls_back_to_the_whole_text() {
        assert_eq!(
            fit_to_width("mismatched types", 100.0, 0.0).as_deref(),
            Some("mismatched types")
        );
    }

    /// Counted in CHARACTERS: rustc quotes identifiers, and a byte cut would
    /// panic in the middle of one the user named in Romanian.
    #[test]
    fn a_non_ascii_message_survives_the_cut() {
        let msg = "cannot find value `măsurători` in this scope";
        let out = fit_to_width(msg, 90.0, 6.0).expect("15 chars fit");
        assert_eq!(out.chars().count(), 15);
    }

    /// Negative room (the pill pushed the message past the clip edge) must be
    /// treated as no room, not as a huge one via a wrapped cast.
    #[test]
    fn no_room_at_all_is_not_mistaken_for_unlimited_room() {
        assert_eq!(fit_to_width("mismatched types", -200.0, 6.0), None);
    }
}
