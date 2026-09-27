//! `Ctrl+R` — rename a symbol project-wide via rust-analyzer's
//! `textDocument/rename`. The popup collects the new name; on submit the current
//! file is synced to RA, the rename is requested, and the returned cross-file
//! edits are applied in `AppIde::apply_rename_edits` (see `app.rs`).

use crate::app::AppIde;
use crate::editor::gui::text_pos::{char_and_byte_at, is_ident_char};
use eframe::egui;

/// The identifier (variable / function / type / …) surrounding char index
/// `cursor` in `text`, or "" if the cursor isn't on one.
pub fn identifier_at(text: &str, cursor: usize) -> String {
    // Walks out from the cursor instead of collecting the whole file; a cursor
    // past the end clamps to the end, as before.
    let (_, at) = char_and_byte_at(text, cursor);
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_ident_char(c))
        .last()
        .map_or(at, |(b, _)| b);
    let end = text[at..]
        .char_indices()
        .find(|&(_, c)| !is_ident_char(c))
        .map_or(text.len(), |(b, _)| at + b);
    text[start..end].to_owned()
}

/// 1-based line numbers where `name` still appears in `content` as a WHOLE
/// word (not as part of a longer identifier).
///
/// Used to audit a finished rename. rust-analyzer's reference search does not
/// reach every syntactic position — notably expressions inside a **const
/// generic argument**, `Parser::<'a, R, 0, { CommandID::None.raw() }>`, which
/// it lowers as a separate anonymous const body. Those occurrences are left
/// behind, and the rename otherwise reports success, so the stale name is only
/// discovered later as a compile error.
///
/// Deliberately textual and advisory: a hit may legitimately be a DIFFERENT
/// symbol that shares the name (another type's `raw()`, a field, a word in a
/// comment). It drives a warning the user can inspect — never an automatic
/// edit.
pub fn whole_word_lines(content: &str, name: &str) -> Vec<usize> {
    if name.is_empty() {
        return Vec::new();
    }
    let is_id = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    for (n, line) in content.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(name) {
            let start = from + rel;
            let end = start + name.len();
            // Boundaries are checked on CHARS, so a multi-byte neighbour can't
            // be mistaken for a word break.
            let before_ok = start == 0 || !line[..start].chars().next_back().is_some_and(is_id);
            let after_ok = end >= bytes.len() || !line[end..].chars().next().is_some_and(is_id);
            if before_ok && after_ok {
                out.push(n + 1);
                break; // at most one hit per line, like the project search
            }
            from = end;
        }
    }
    out
}

/// Replace `old` with `new` as a WHOLE WORD, but only on the given 1-based
/// `lines`. Returns the new text and how many occurrences changed.
///
/// Scoped to the exact lines the user was shown and approved — a whole-file
/// replace could touch occurrences that were never in the reviewed list.
/// Line endings are preserved by rebuilding from the original separators.
pub fn replace_whole_word_on_lines(
    content: &str,
    old: &str,
    new: &str,
    lines: &[usize],
) -> (String, usize) {
    if old.is_empty() || lines.is_empty() {
        return (content.to_owned(), 0);
    }
    let is_id = |c: char| c.is_alphanumeric() || c == '_';
    let mut count = 0usize;
    let mut out = String::with_capacity(content.len());
    // `split_inclusive` keeps each line's own terminator, so a file with no
    // trailing newline stays that way.
    for (idx, line) in content.split_inclusive('\n').enumerate() {
        if !lines.contains(&(idx + 1)) {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        while let Some(rel) = rest.find(old) {
            let (before, after_start) = rest.split_at(rel);
            let after = &after_start[old.len()..];
            let before_ok = before.chars().next_back().is_none_or(|c| !is_id(c));
            let after_ok = after.chars().next().is_none_or(|c| !is_id(c));
            out.push_str(before);
            if before_ok && after_ok {
                out.push_str(new);
                count += 1;
            } else {
                out.push_str(old);
            }
            rest = after;
        }
        out.push_str(rest);
    }
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::{identifier_at, replace_whole_word_on_lines, whole_word_lines};

    #[test]
    fn line_scoped_replace_only_touches_listed_lines() {
        let src = "a.raw();\nb.raw();\nc.raw();\n";
        let (out, n) = replace_whole_word_on_lines(src, "raw", "as_u16", &[2]);
        assert_eq!(out, "a.raw();\nb.as_u16();\nc.raw();\n");
        assert_eq!(n, 1);
    }

    #[test]
    fn line_scoped_replace_skips_partial_words() {
        let src = "raw_notes + draw() + raw();\n";
        let (out, n) = replace_whole_word_on_lines(src, "raw", "as_u16", &[1]);
        assert_eq!(out, "raw_notes + draw() + as_u16();\n");
        assert_eq!(n, 1);
    }

    #[test]
    fn line_scoped_replace_handles_repeats_on_one_line() {
        let (out, n) = replace_whole_word_on_lines("raw(); raw();\n", "raw", "x", &[1]);
        assert_eq!(out, "x(); x();\n");
        assert_eq!(n, 2);
    }

    #[test]
    fn line_scoped_replace_rewrites_the_const_generic_case() {
        let src =
            "let p = super::Parser::<'a, R, 0, { super::CommandID::None.raw() }>::new(&H, &T);\n";
        let (out, n) = replace_whole_word_on_lines(src, "raw", "as_u16", &[1]);
        assert!(out.contains("{ super::CommandID::None.as_u16() }"));
        assert_eq!(n, 1);
    }

    #[test]
    fn line_scoped_replace_preserves_a_missing_trailing_newline() {
        let (out, _) = replace_whole_word_on_lines("raw()", "raw", "x", &[1]);
        assert_eq!(out, "x()");
        let (out2, _) = replace_whole_word_on_lines("raw()\n", "raw", "x", &[1]);
        assert_eq!(out2, "x()\n");
    }

    #[test]
    fn line_scoped_replace_is_a_no_op_without_targets() {
        let (out, n) = replace_whole_word_on_lines("raw();\n", "raw", "x", &[]);
        assert_eq!(out, "raw();\n");
        assert_eq!(n, 0);
    }

    #[test]
    fn leftover_scan_matches_only_whole_words() {
        let src = "\
let a = x.raw();
let b = raw_notes;
let c = draw();
let d = obj.raw;
";
        // `raw_notes` and `draw` must NOT count; `.raw()` and `.raw` must.
        assert_eq!(whole_word_lines(src, "raw"), vec![1, 4]);
    }

    #[test]
    fn leftover_scan_finds_const_generic_position() {
        // The exact shape rust-analyzer's rename misses.
        let src =
            "let p = super::Parser::<'a, R, 0, { super::CommandID::None.raw() }>::new(&H, &T);";
        assert_eq!(whole_word_lines(src, "raw"), vec![1]);
    }

    #[test]
    fn leftover_scan_reports_each_line_once() {
        assert_eq!(whole_word_lines("raw(); raw(); raw();", "raw"), vec![1]);
    }

    #[test]
    fn leftover_scan_survives_non_ascii_neighbours() {
        // A multi-byte char next to the match must not panic or be read as a
        // word character.
        assert_eq!(whole_word_lines("// măsurat: raw() -> ok", "raw"), vec![1]);
    }

    #[test]
    fn leftover_scan_is_empty_for_a_clean_rename() {
        assert!(whole_word_lines("let a = x.as_u16();", "raw").is_empty());
        assert!(whole_word_lines("anything", "").is_empty());
    }

    #[test]
    fn finds_identifier_around_cursor() {
        let text = "let my_var = 1;";
        // cursor in the middle of `my_var`
        assert_eq!(identifier_at(text, 7), "my_var");
        // cursor at the start of `my_var`
        assert_eq!(identifier_at(text, 4), "my_var");
        // cursor right after `my_var` (on the space)
        assert_eq!(identifier_at(text, 10), "my_var");
    }

    #[test]
    fn empty_when_not_on_identifier() {
        // cursor between the space and `+`, no identifier char on either side
        assert_eq!(identifier_at("a + b", 2), "");
    }

    /// The walk out from the cursor returns exactly what slicing a collected
    /// `Vec<char>` returned: multi-byte and astral identifier chars, cursors
    /// past the end, empty texts.
    #[test]
    fn identifier_at_matches_the_collected_walk() {
        use crate::editor::gui::text_pos::word_walk_tests::{indices, word_texts};
        fn old(text: &str, cursor: usize) -> String {
            let chars: Vec<char> = text.chars().collect();
            let is_id = |c: char| c.is_alphanumeric() || c == '_';
            let mut start = cursor.min(chars.len());
            while start > 0 && is_id(chars[start - 1]) {
                start -= 1;
            }
            let mut end = cursor.min(chars.len());
            while end < chars.len() && is_id(chars[end]) {
                end += 1;
            }
            chars[start..end].iter().collect()
        }
        let mut non_empty = 0;
        for text in word_texts() {
            for i in indices(&text) {
                let word = identifier_at(&text, i);
                assert_eq!(word, old(&text, i), "{text:?} cursor {i}");
                non_empty += usize::from(!word.is_empty());
            }
        }
        assert!(non_empty > 1000, "only {non_empty} cursors sat on a word");
    }
}

impl AppIde {
    /// Render the rename input popup (shown while `rename_active`). On submit it
    /// sends the rename request to RA; on Esc / Cancel it just closes.
    pub(super) fn show_rename_popup(&mut self, ui: &mut egui::Ui, slot: crate::app::EditorSlot) {
        if !self.ed.rename_active {
            return;
        }
        let mut submit = false;
        let mut cancel = false;

        egui::Area::new(egui::Id::new("rename_popup"))
            .fixed_pos(self.ed.rename_popup_pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(&ui.ctx().global_style()).show(ui, |ui| {
                    ui.set_min_width(200.0);
                    ui.label(
                        egui::RichText::new("Rename symbol (everywhere)")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(170, 180, 200)),
                    );
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.ed.rename_input)
                            .desired_width(200.0)
                            .hint_text("new name"),
                    );
                    if self.ed.rename_focus {
                        resp.request_focus();
                        self.ed.rename_focus = false;
                    }
                    if crate::app::helpers::text_field::ended_with_enter(ui, &resp) {
                        submit = true;
                    }
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked() {
                            submit = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        ui.label(
                            egui::RichText::new("Enter / Esc")
                                .size(9.0)
                                .color(egui::Color32::from_rgb(120, 130, 150)),
                        );
                    });
                });
            });

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }

        if cancel {
            self.ed.rename_active = false;
            return;
        }
        if submit {
            self.ed.rename_active = false;
            let new_name = self.ed.rename_input.trim().to_owned();
            if new_name.is_empty() {
                return;
            }
            // Kept for the post-rename leftover audit (see `whole_word_lines`).
            self.ed.rename_new_name = new_name.clone();
            // Sync the current file to RA first (it may hold debounced-stale text),
            // then request the rename; the response is applied in init_frame.
            let rel = self.ed.rename_rel.clone();
            let content = self.file_content_for(&rel);
            let mut lsp = self.lsp_state.lock().unwrap();
            lsp.did_change(&rel, &content, false);
            lsp.request_rename(&rel, self.ed.rename_line, self.ed.rename_char, &new_name);
            drop(lsp);
            // The answer lands at frame top, before any view has drawn.
            self.lsp_asker.rename = slot;
            self.ed.rename_in_flight = true;
            self.egui_ctx.request_repaint();
        }
    }

    /// Current in-memory content of a project-root-relative file
    /// (`src/main.rs`, `src/app.rs`, `mw_radar/src/lib.rs`, …).
    pub(super) fn file_content_for(&self, rel: &str) -> String {
        if rel == "src/main.rs" {
            return self.generated_code.clone();
        }
        self.project_tree
            .user_src_files
            .iter()
            .find(|(p, _)| p == rel)
            .map(|(_, c)| c.clone())
            .unwrap_or_default()
    }
}
