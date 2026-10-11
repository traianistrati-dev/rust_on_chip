//! Rust-aware word selection: double-click and Ctrl(+Shift)+Left/Right.
//!
//! egui segments words with Unicode UAX#29, where `:` between letters is a
//! *MidLetter* — so `radar_delay_gate_val:Option` (unspaced `let name:Type`)
//! counts as ONE word. Double-clicking the name selected the whole thing, and
//! Ctrl+Right jumped clean over the `:` (egui special-cases only `.`, for
//! `www.example.com`; `:` and `'` have the same problem). Code wants plain
//! identifier runs, so both paths are overridden here:
//!
//! - double-click → the `[A-Za-z0-9_]*` run around the clicked position;
//! - Ctrl+Left / Ctrl+Right (+Shift to select) → class-based word movement
//!   ([`next_word_boundary`] / [`prev_word_boundary`]), where identifiers,
//!   punctuation and spaces are separate classes — so a jump stops AT the `:`;
//! - Alt+Left / Alt+Right (+Shift to select) → the same, except that inside
//!   an identifier it stops at every `_` ([`next_subword_boundary`] /
//!   [`prev_subword_boundary`]): `THRESHOLD_HOLD_MAX_VAL` one part a press.

use crate::app::AppIde;
use crate::editor::gui::text_pos::{GalleyRows, LineIndex};
use eframe::egui;

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One word-wise caret move, taken from the keyboard before the editor saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WordMove {
    pub(super) right: bool,
    /// Shift held: the selection grows or shrinks from its anchor.
    pub(super) extend: bool,
    /// Alt rather than Ctrl: one identifier part (`_`-separated), not a word.
    pub(super) part: bool,
}

/// The rects [`AppIde::highlight_selected_word`] fills for the selection
/// `lo..hi` of `index`'s text: every whole-word occurrence of it on screen.
/// Only a single identifier counts; any other selection yields nothing.
///
/// Only the chars whose row can meet the clip are scanned, plus one on each
/// side for the whole-word test. That finds exactly what a scan of the whole
/// text found there: two whole-word occurrences of an identifier can never
/// overlap, so where a scan starts cannot change which ones it sees.
pub(super) fn for_each_selected_word_rect(
    rows: &GalleyRows,
    gp: egui::Pos2,
    clip: egui::Rect,
    index: &LineIndex,
    lo: usize,
    hi: usize,
    mut paint: impl FnMut(egui::Rect),
) {
    let n = index.total_chars();
    if lo >= hi || hi > n {
        return;
    }
    let text = index.text();
    let target = &text[index.byte_of_char(lo)..index.byte_of_char(hi)];
    // Reject anything that isn't one identifier token (whitespace, symbols,
    // multi-word selections, or a leading digit → not a variable name).
    if target.chars().any(|c| !is_ident(c)) || target.starts_with(|c: char| c.is_ascii_digit()) {
        return;
    }
    let target: Vec<char> = target.chars().collect();
    let wl = hi - lo;

    let Some(span) = rows.chars_meeting_band(gp.y, clip.top(), clip.bottom()) else {
        return;
    };
    // Clamped to the text: the galley can be an edit behind it.
    let first = *span.start();
    let last = (*span.end()).min(n - wl);
    if first > last {
        return;
    }
    let from = first.saturating_sub(1);
    let to = (last + wl + 1).min(n);
    let chars: Vec<char> = text[index.byte_of_char(from)..index.byte_of_char(to)]
        .chars()
        .collect();

    let mut i = first;
    while i <= last {
        let k = i - from;
        if chars[k..k + wl] == target[..]
            && (i == 0 || !is_ident(chars[k - 1]))
            && (i + wl == n || !is_ident(chars[k + wl]))
        {
            // Map the match's char range to a screen rect via the galley.
            let loc_s = rows.pos(i);
            let loc_e = rows.pos(i + wl);
            let y_top = gp.y + loc_s.min.y;
            let y_bot = gp.y + loc_s.max.y;
            // A whole identifier never wraps, but guard anyway: if start/end
            // land on different rows, extend to the line end.
            let same_row = (loc_s.min.y - loc_e.min.y).abs() < (y_bot - y_top).max(1.0) * 0.5;
            let x_l = gp.x + loc_s.min.x;
            let x_r = if same_row {
                gp.x + loc_e.min.x
            } else {
                gp.x + rows.galley().rect.width()
            };
            // Skip occurrences scrolled out of the visible editor region.
            if y_bot >= clip.top() && y_top <= clip.bottom() && x_r > x_l {
                paint(egui::Rect::from_min_max(
                    egui::pos2(x_l, y_top),
                    egui::pos2(x_r, y_bot),
                ));
            }
            i += wl; // jump past this match
        } else {
            i += 1;
        }
    }
}

/// Char class for word-wise movement. Runs of one class are consumed whole, so
/// `:` (Punct) ends the identifier before it instead of joining the two.
#[derive(Clone, Copy, PartialEq)]
enum Class {
    Ident,
    Punct,
    /// Space / tab — skipped before the run being consumed.
    Space,
    /// Hard stop: a word jump never silently crosses lines.
    Newline,
}

fn class_of(c: char) -> Class {
    if c == '\n' {
        Class::Newline
    } else if is_ident(c) {
        Class::Ident
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Punct
    }
}

/// Caret target one word to the RIGHT of `i` (Ctrl+Right): skip spaces, then
/// consume the whole run under the caret. `name:Type` therefore stops at the
/// `:`, and `::`/`>>` are consumed as single punctuation runs.
pub(super) fn next_word_boundary(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    let mut j = i.min(n);
    if j >= n {
        return n;
    }
    // Sitting on the line break: step over exactly it.
    if chars[j] == '\n' {
        return j + 1;
    }
    while j < n && class_of(chars[j]) == Class::Space {
        j += 1;
    }
    if j >= n || chars[j] == '\n' {
        return j; // stop at the line end rather than jumping into the next line
    }
    let class = class_of(chars[j]);
    while j < n && class_of(chars[j]) == class {
        j += 1;
    }
    j
}

/// Caret target one word to the LEFT of `i` (Ctrl+Left) — mirror of
/// [`next_word_boundary`], looking at the char before the caret.
pub(super) fn prev_word_boundary(chars: &[char], i: usize) -> usize {
    let mut j = i.min(chars.len());
    if j == 0 {
        return 0;
    }
    if chars[j - 1] == '\n' {
        return j - 1;
    }
    while j > 0 && class_of(chars[j - 1]) == Class::Space {
        j -= 1;
    }
    if j == 0 || chars[j - 1] == '\n' {
        return j;
    }
    let class = class_of(chars[j - 1]);
    while j > 0 && class_of(chars[j - 1]) == class {
        j -= 1;
    }
    j
}

/// Caret target one identifier PART to the right of `i` (Alt+Right): inside an
/// identifier the jump stops before every `_`, so `|THRESHOLD_HOLD_MIN_VAL`
/// goes `THRESHOLD|`, `THRESHOLD_HOLD|`, … one part per press. The `_` (or a
/// run of them) is crossed on the way to the next part. Anywhere else —
/// spaces, punctuation, a line end — it is exactly [`next_word_boundary`].
pub(super) fn next_subword_boundary(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    let mut k = i.min(n);
    while k < n && class_of(chars[k]) == Class::Space {
        k += 1;
    }
    if k >= n || class_of(chars[k]) != Class::Ident {
        return next_word_boundary(chars, i);
    }
    while k < n && chars[k] == '_' {
        k += 1;
    }
    while k < n && is_ident(chars[k]) && chars[k] != '_' {
        k += 1;
    }
    k
}

/// Caret target one identifier PART to the left of `i` (Alt+Left) — the
/// mirror of [`next_subword_boundary`]: it stops just after every `_`, at the
/// start of a part, so `…_MIN_VAL|` goes `…_MIN_|VAL`, then `…_|MIN_VAL`.
pub(super) fn prev_subword_boundary(chars: &[char], i: usize) -> usize {
    let mut k = i.min(chars.len());
    while k > 0 && class_of(chars[k - 1]) == Class::Space {
        k -= 1;
    }
    if k == 0 || class_of(chars[k - 1]) != Class::Ident {
        return prev_word_boundary(chars, i);
    }
    while k > 0 && chars[k - 1] == '_' {
        k -= 1;
    }
    while k > 0 && is_ident(chars[k - 1]) && chars[k - 1] != '_' {
        k -= 1;
    }
    k
}

/// The identifier run `[start, end)` around char index `idx`: the char at the
/// index, or — clicking just past a word's last char — the one before it.
/// `None` when neither side is an identifier char (punctuation / whitespace) —
/// the caller then leaves egui's own selection alone.
pub(super) fn ident_run_at(chars: &[char], idx: usize) -> Option<(usize, usize)> {
    let anchor = if idx < chars.len() && is_ident(chars[idx]) {
        idx
    } else if idx > 0 && chars.get(idx - 1).is_some_and(|&c| is_ident(c)) {
        idx - 1
    } else {
        return None;
    };
    let mut start = anchor;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    let mut end = anchor + 1;
    while end < chars.len() && is_ident(chars[end]) {
        end += 1;
    }
    Some((start, end))
}

impl AppIde {
    /// On a double-click, replace egui's UAX#29 word selection with the plain
    /// identifier run under the pointer (stored for next frame — the widget
    /// already rendered with egui's selection this frame).
    pub(super) fn fix_double_click_selection(
        &self,
        ui: &egui::Ui,
        editor_resp: &egui::text_edit::TextEditOutput,
        display_code: &str,
    ) {
        if !editor_resp.response.double_clicked() {
            return;
        }
        let Some(pos) = ui.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        let ccursor = editor_resp
            .galley
            .cursor_from_pos(pos - editor_resp.galley_pos);
        let chars: Vec<char> = display_code.chars().collect();
        let Some((start, end)) = ident_run_at(&chars, ccursor.index.0.min(chars.len())) else {
            return; // not on an identifier — keep egui's selection
        };
        let mut st = editor_resp.state.clone();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(start),
            egui::text::CCursor::new(end),
        )));
        st.store(ui.ctx(), editor_resp.response.id);
    }

    /// Apply a word move that was consumed before the editor (so egui's UAX#29
    /// jump never ran): Ctrl(+Shift)+Left/Right by whole words, or
    /// Alt(+Shift)+Left/Right by identifier parts when `part` is set. `extend`
    /// keeps the anchor — that's the Shift variant, which grows/shrinks the
    /// selection.
    ///
    /// Like the double-click fix, the new cursor is stored for NEXT frame: the
    /// widget already rendered with the old one. The key press repaints anyway,
    /// so the move lands immediately.
    pub(super) fn apply_word_move(
        &self,
        ui: &egui::Ui,
        editor_resp: &egui::text_edit::TextEditOutput,
        display_code: &str,
        mv: WordMove,
    ) {
        let Some(range) = editor_resp.state.cursor.char_range() else {
            return; // editor not focused / no caret yet
        };
        let chars: Vec<char> = display_code.chars().collect();
        let cur = range.primary.index.0.min(chars.len());
        let target = match (mv.right, mv.part) {
            (true, false) => next_word_boundary(&chars, cur),
            (false, false) => prev_word_boundary(&chars, cur),
            (true, true) => next_subword_boundary(&chars, cur),
            (false, true) => prev_subword_boundary(&chars, cur),
        };
        let extend = mv.extend;
        let primary = egui::text::CCursor::new(target);
        // `CCursorRange::two(min, max)` assigns by position, but here the moved
        // end must stay `primary` (Shift+arrow grows FROM the anchor either
        // way), so the range is built field-wise.
        let new = if extend {
            egui::text::CCursorRange {
                primary,
                secondary: range.secondary,
                h_pos: None,
            }
        } else {
            egui::text::CCursorRange::one(primary)
        };
        let mut st = editor_resp.state.clone();
        st.cursor.set_char_range(Some(new));
        st.store(ui.ctx(), editor_resp.response.id);
        ui.ctx().request_repaint();
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        ident_run_at, next_subword_boundary, next_word_boundary, prev_subword_boundary,
        prev_word_boundary,
    };
    use eframe::egui;

    fn run(text: &str, idx: usize) -> Option<(usize, usize)> {
        ident_run_at(&text.chars().collect::<Vec<_>>(), idx)
    }

    /// Walk `steps` word jumps from `from` and return the selected substring —
    /// what Ctrl+Shift+Right/Left would highlight after that many presses.
    fn walk(text: &str, from: usize, steps: usize, right: bool) -> &str {
        let chars: Vec<char> = text.chars().collect();
        let mut at = from;
        for _ in 0..steps {
            at = if right {
                next_word_boundary(&chars, at)
            } else {
                prev_word_boundary(&chars, at)
            };
        }
        let (a, b) = if right { (from, at) } else { (at, from) };
        &text[a..b] // ASCII fixtures: char index == byte index
    }

    /// The reported case: `:` must END the word, so a jump never swallows both
    /// the binding name and its type.
    #[test]
    fn word_jump_stops_at_the_type_colon() {
        let src = "radar_delay_gate_val:Option< u32 >";
        // Left → right: the first press selects only the name.
        assert_eq!(walk(src, 0, 1, true), "radar_delay_gate_val");
        // The next press takes the `:` alone, then `Option`.
        assert_eq!(walk(src, 0, 2, true), "radar_delay_gate_val:");
        assert_eq!(walk(src, 0, 3, true), "radar_delay_gate_val:Option");
        // Right → left from the end: `>`, ` u32`, ` <`, `Option` — stopping at
        // the colon, so the type comes out whole and the name is untouched.
        let end = src.len();
        assert_eq!(walk(src, end, 1, false), ">");
        assert_eq!(walk(src, end, 2, false), "u32 >");
        assert_eq!(walk(src, end, 3, false), "< u32 >");
        assert_eq!(walk(src, end, 4, false), "Option< u32 >");
        // Only the 5th press reaches the `:` — never together with the name.
        assert_eq!(walk(src, end, 5, false), ":Option< u32 >");
    }

    /// Punctuation runs (`::`, `>>`) are consumed as one word, identifiers stop
    /// at any separator.
    #[test]
    fn punctuation_runs_and_paths() {
        let src = "pins::configs::usart1";
        assert_eq!(walk(src, 0, 1, true), "pins");
        assert_eq!(walk(src, 0, 2, true), "pins::");
        assert_eq!(walk(src, 0, 3, true), "pins::configs");
        let src = "Vec<Vec<u32>>";
        let end = src.len();
        assert_eq!(walk(src, end, 1, false), ">>");
        assert_eq!(walk(src, end, 2, false), "u32>>");
    }

    /// Spaces are skipped before the run; a line break is a hard stop, so a
    /// jump never silently crosses into the next line.
    #[test]
    fn spaces_are_skipped_and_newlines_stop_the_jump() {
        let src = "let  x = 1\nlet y";
        assert_eq!(walk(src, 0, 1, true), "let");
        assert_eq!(walk(src, 0, 2, true), "let  x"); // double space skipped
        // From just before the newline: the jump stops ON it, then steps over.
        let nl = src.find('\n').unwrap();
        assert_eq!(walk(src, nl, 1, true), "\n");
        assert_eq!(walk(src, nl, 2, true), "\nlet");
        // Backwards from the start of line 2 lands at the line end, not inside
        // line 1's last word.
        assert_eq!(walk(src, nl + 1, 1, false), "\n");
    }

    /// [`walk`], by identifier parts (Alt+Shift+Right/Left).
    fn walk_parts(text: &str, from: usize, steps: usize, right: bool) -> &str {
        let chars: Vec<char> = text.chars().collect();
        let mut at = from;
        for _ in 0..steps {
            at = if right {
                next_subword_boundary(&chars, at)
            } else {
                prev_subword_boundary(&chars, at)
            };
        }
        let (a, b) = if right { (from, at) } else { (at, from) };
        &text[a..b]
    }

    /// The reported case: from the start of the name, each press takes one
    /// more `_`-separated part — `THRESHOLD` first, without its `_` — and
    /// back from the end the same parts come off one at a time.
    #[test]
    fn a_part_jump_stops_at_every_underscore() {
        let src = "THRESHOLD_HOLD_MAX_VAL:f32 = 90;";
        assert_eq!(walk_parts(src, 0, 1, true), "THRESHOLD");
        assert_eq!(walk_parts(src, 0, 2, true), "THRESHOLD_HOLD");
        assert_eq!(walk_parts(src, 0, 3, true), "THRESHOLD_HOLD_MAX");
        assert_eq!(walk_parts(src, 0, 4, true), "THRESHOLD_HOLD_MAX_VAL");
        assert_eq!(
            walk_parts(src, 0, 5, true),
            "THRESHOLD_HOLD_MAX_VAL:",
            "then as Ctrl"
        );
        let end = src.find(':').unwrap();
        assert_eq!(walk_parts(src, end, 1, false), "VAL");
        assert_eq!(walk_parts(src, end, 2, false), "MAX_VAL");
        assert_eq!(walk_parts(src, end, 3, false), "HOLD_MAX_VAL");
        assert_eq!(walk_parts(src, end, 4, false), "THRESHOLD_HOLD_MAX_VAL");
    }

    /// Outside an identifier, and on one without `_`, a part jump is a word
    /// jump; leading and doubled underscores are crossed with the next part.
    #[test]
    fn a_part_jump_elsewhere_is_a_word_jump() {
        let src = "pub const Node: u32_value = _unused + a__b;\nx";
        assert_eq!(walk_parts(src, 0, 1, true), "pub");
        assert_eq!(walk_parts(src, 0, 2, true), "pub const");
        assert_eq!(walk_parts(src, 0, 3, true), "pub const Node");
        let u = src.find("u32").unwrap();
        assert_eq!(walk_parts(src, u, 1, true), "u32");
        assert_eq!(walk_parts(src, u, 2, true), "u32_value");
        let lead = src.find("_unused").unwrap();
        assert_eq!(walk_parts(src, lead, 1, true), "_unused");
        let ab = src.find("a__b").unwrap();
        assert_eq!(walk_parts(src, ab, 1, true), "a");
        assert_eq!(walk_parts(src, ab, 2, true), "a__b");
        assert_eq!(walk_parts(src, ab + 4, 1, false), "b");
        assert_eq!(walk_parts(src, ab + 4, 2, false), "a__b");
        let nl = src.find('\n').unwrap();
        assert_eq!(walk_parts(src, nl, 1, true), "\n", "a line end stops it");
        assert_eq!(walk_parts(src, nl + 1, 1, false), "\n");
    }

    #[test]
    fn a_part_jump_clamps_at_the_buffer_edges() {
        let chars: Vec<char> = "a_b".chars().collect();
        assert_eq!(next_subword_boundary(&chars, 3), 3);
        assert_eq!(next_subword_boundary(&chars, 9), 3);
        assert_eq!(prev_subword_boundary(&chars, 0), 0);
        assert_eq!(prev_subword_boundary(&[], 0), 0);
    }

    #[test]
    fn word_jump_clamps_at_the_buffer_edges() {
        let chars: Vec<char> = "ab".chars().collect();
        assert_eq!(next_word_boundary(&chars, 2), 2);
        assert_eq!(next_word_boundary(&chars, 9), 2); // out-of-range index
        assert_eq!(prev_word_boundary(&chars, 0), 0);
        assert_eq!(prev_word_boundary(&[], 0), 0);
    }

    /// The reported case: unspaced `let name:Type` — clicking the name selects
    /// ONLY the name, clicking the type selects only the type.
    #[test]
    fn unspaced_type_annotation_selects_name_only() {
        let src = "let radar_delay_gate_val:Option<u32>";
        let name_start = 4;
        let name_end = src.find(':').unwrap();
        // Click in the middle of the name.
        assert_eq!(run(src, 10), Some((name_start, name_end)));
        // Click inside `Option` → just `Option`.
        let opt_start = name_end + 1;
        let opt_end = src.find('<').unwrap();
        assert_eq!(run(src, opt_start + 2), Some((opt_start, opt_end)));
    }

    #[test]
    fn click_just_past_last_char_still_hits_the_word() {
        let src = "foo bar";
        assert_eq!(run(src, 3), Some((0, 3))); // caret right after `foo`
    }

    #[test]
    fn word_boundaries_at_line_edges() {
        assert_eq!(run("foo", 0), Some((0, 3)));
        assert_eq!(run("foo", 3), Some((0, 3)));
    }

    #[test]
    fn punctuation_and_whitespace_yield_none() {
        let src = "a + b";
        assert_eq!(run(src, 2), None); // on `+`
        // On the space after `a` — the char before is an identifier, so the
        // click still resolves to `a` (matches editors' forgiving behaviour).
        assert_eq!(run(src, 1), Some((0, 1)));
        assert_eq!(run("  ", 1), None);
        assert_eq!(run("", 0), None);
    }

    #[test]
    fn underscores_and_digits_are_part_of_the_word() {
        let src = "count_2ab = 7";
        assert_eq!(run(src, 5), Some((0, 9)));
    }

    /// What the selected-word highlight filled before it was limited to the rows
    /// on screen: a scan of the whole text, every match positioned with the
    /// galley's own walk, then culled.
    fn old_rects(
        galley: &egui::Galley,
        gp: egui::Pos2,
        clip: egui::Rect,
        display_code: &str,
        lo: usize,
        hi: usize,
    ) -> Vec<egui::Rect> {
        let mut out = Vec::new();
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        let chars: Vec<char> = display_code.chars().collect();
        if hi > chars.len() {
            return out;
        }
        let target = &chars[lo..hi];
        if target.iter().any(|&c| !is_ident(c)) || target[0].is_ascii_digit() {
            return out;
        }
        let wl = hi - lo;
        let n = chars.len();
        let mut i = 0;
        while i + wl <= n {
            if &chars[i..i + wl] == target
                && (i == 0 || !is_ident(chars[i - 1]))
                && (i + wl == n || !is_ident(chars[i + wl]))
            {
                let loc_s = galley.pos_from_cursor(egui::text::CCursor::new(i));
                let loc_e = galley.pos_from_cursor(egui::text::CCursor::new(i + wl));
                let y_top = gp.y + loc_s.min.y;
                let y_bot = gp.y + loc_s.max.y;
                let same_row = (loc_s.min.y - loc_e.min.y).abs() < (y_bot - y_top).max(1.0) * 0.5;
                let x_l = gp.x + loc_s.min.x;
                let x_r = if same_row {
                    gp.x + loc_e.min.x
                } else {
                    gp.x + galley.rect.width()
                };
                if y_bot >= clip.top() && y_top <= clip.bottom() && x_r > x_l {
                    out.push(egui::Rect::from_min_max(
                        egui::pos2(x_l, y_top),
                        egui::pos2(x_r, y_bot),
                    ));
                }
                i += wl;
            } else {
                i += 1;
            }
        }
        out
    }

    /// Same rects in the same order, for texts the galley was laid out from and
    /// for texts it is an edit behind, with clips that cut through rows so the
    /// whole-word test runs at the edges of the scanned chars.
    #[test]
    fn visible_occurrences_paint_exactly_what_the_whole_file_pass_painted() {
        use crate::editor::gui::text_pos::{
            GalleyRows, LineIndex, galley_rows_tests::galleys, test_galleys,
        };
        let code = "let x = x + max(x, x_1);\nfoo(x)\n  x\nx\nself.a = self.b;\n\
                    self\nself_ x\nă x ăx x\n2x x2 x";
        let long: String = (0..60).map(|i| format!("x{i} x x_ {i}x\n")).collect();
        let named = test_galleys(&[(code, f32::INFINITY), (code, 40.0), (&long, f32::INFINITY)]);
        let named_texts = [code, code, long.as_str()].map(str::to_owned);
        // A third of the generated galleys (still both wrap widths) keeps this
        // under a second in the dev build.
        let all = galleys()
            .into_iter()
            .step_by(3)
            .chain(named_texts.into_iter().zip(named));
        let mut compared = 0usize;
        for (text, galley) in all {
            let rows = GalleyRows::new(&galley);
            let h = galley.rect.height();
            let variants = [
                text.clone(),
                format!("x\n{text}"),
                text.chars().skip(1).collect(),
                format!("{text} x a"),
            ];
            for shown in &variants {
                let index = LineIndex::new(shown);
                let n = shown.chars().count();
                let mut selections: Vec<(usize, usize)> = (0..n)
                    .step_by((n / 16).max(3))
                    .flat_map(|lo| (1..=3).map(move |len| (lo, lo + len)))
                    .filter(|&(_, hi)| hi <= n + 1)
                    .collect();
                selections.extend([(0, n), (n, n + 2)]);
                for &(lo, hi) in &selections {
                    if lo >= hi {
                        continue;
                    }
                    for gp in [egui::pos2(0.0, 0.0), egui::pos2(12.5, -20.0)] {
                        for (top, bottom) in [
                            (-50.0, h + 50.0),
                            (7.0, 21.0),
                            (h * 0.5, h * 0.5 + 10.0),
                            (h, h),
                        ] {
                            let clip = egui::Rect::from_min_max(
                                egui::pos2(0.0, top),
                                egui::pos2(400.0, bottom),
                            );
                            let mut got = Vec::new();
                            super::for_each_selected_word_rect(
                                &rows,
                                gp,
                                clip,
                                &index,
                                lo,
                                hi,
                                |r| got.push(r),
                            );
                            let want = old_rects(&galley, gp, clip, shown, lo, hi);
                            assert_eq!(got, want, "{lo}..{hi} of {shown:?} laid out {text:?}");
                            compared += want.len();
                        }
                    }
                }
            }
        }
        assert!(
            compared > 1000,
            "the cases must actually paint something: {compared}"
        );
    }
}
