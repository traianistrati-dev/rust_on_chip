//! `Ctrl+Alt+Insert` — move the selected lines into a new function and leave a
//! call behind.
//!
//! The selected lines are cut, a popup collects the new function's name,
//! parameters and return type, and on submit the lines reappear as a `fn` right
//! after the one they came out of, with a call in their place.
//!
//! **This is not rust-analyzer's `extract_function`.** That assist infers the
//! parameters for you — borrows, mutability, lifetimes — and it is reachable
//! from Ctrl+Enter now that code actions are asked over the selection. This is
//! the other half of the pair: you dictate the signature. Neither replaces the
//! other, and the hard part RA does well is exactly the part this one hands to
//! you on purpose.

use crate::app::{AppIde, ProjectFileId};
use eframe::egui;

/// What the extraction will do, worked out when the popup opens and re-checked
/// when it is submitted.
#[derive(Clone)]
pub(crate) struct ExtractPlan {
    /// The lines being moved out, with their common indent already stripped.
    pub body: String,
    /// The indent the call takes — the common indent of the cut lines.
    pub indent: String,
    /// Char range of the lines being cut, newline included.
    pub cut: (usize, usize),
    /// Exactly what sat in `cut` when the plan was made. Re-checked before the
    /// splice: the popup holds focus so the text should not move, but a plan
    /// applied to text it was not made for would corrupt the file, and that is
    /// not a risk worth taking on a "should not".
    pub cut_text: String,
    /// Char index the new function is inserted at — just past the closing brace
    /// of the function the selection came out of.
    pub insert_at: usize,
    /// The selection mentions `self`, so the new function needs a receiver and
    /// the call needs `self.`.
    pub uses_self: bool,
    /// The selection contains `.await`.
    pub is_async: bool,
    /// Control flow that changes meaning once moved. Shown, never blocking —
    /// `?` is legitimate the moment you give the function a `Result` return
    /// type, and the compiler has the last word on the rest.
    pub warnings: Vec<&'static str>,
}

/// Per-view state for the popup.
#[derive(Default)]
pub(crate) struct ExtractFnState {
    pub active: bool,
    pub name: String,
    pub params: String,
    pub ret: String,
    /// Request focus on the name field next frame.
    pub focus: bool,
    pub pos: egui::Pos2,
    pub plan: Option<ExtractPlan>,
    /// The file the plan was made for — a plan applied to another file would
    /// splice at offsets that mean nothing there.
    pub file: Option<ProjectFileId>,
}

// ── Pure core ────────────────────────────────────────────────────────────────

/// The leading whitespace shared by every non-blank line in `lines`.
///
/// Blank lines are skipped rather than counted as zero indent: one empty line in
/// the middle of a block would otherwise flatten the whole thing to column 0.
pub(crate) fn common_indent(lines: &[String]) -> String {
    let mut out: Option<&str> = None;
    for l in lines {
        if l.trim().is_empty() {
            continue;
        }
        let indent = &l[..l.len() - l.trim_start().len()];
        out = Some(match out {
            None => indent,
            Some(prev) => {
                let n = prev
                    .chars()
                    .zip(indent.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                &prev[..n]
            }
        });
    }
    out.unwrap_or("").to_owned()
}

/// The argument list for the call, from the parameter list of the declaration.
///
/// `a: u8, b: &mut Foo` → `a, b`. A receiver (`self`, `&self`, `&mut self`) is
/// dropped: it travels as `self.name(…)`, not as an argument. Everything before
/// the first `:` is the pattern, which for an ordinary parameter is the name —
/// good enough here, and wrong only for a destructuring pattern, which you would
/// not write in this box.
pub(crate) fn call_args(params: &str) -> String {
    split_params(params)
        .into_iter()
        .filter(|p| {
            let t = p.trim_start_matches('&').trim();
            let t = t.strip_prefix("mut ").unwrap_or(t).trim();
            t != "self"
        })
        .map(|p| p.split(':').next().unwrap_or("").trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Split a parameter list on top-level commas — commas inside `<>`, `()` or
/// `[]` belong to a type (`Foo<A, B>`), not to the list.
fn split_params(params: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in params.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(params[start..i].trim());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    let last = params[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out.into_iter().filter(|p| !p.is_empty()).collect()
}

/// Work out what extracting the selected lines would do, or `None` when it
/// cannot be done here.
///
/// `None` for an empty selection and — the load-bearing one — for a selection
/// that is not inside a function: there is nowhere to put the result, and a
/// `fn` dropped at the end of the file would be a guess, not an extraction.
pub(crate) fn plan(text: &str, sel_lo: usize, sel_hi: usize) -> Option<ExtractPlan> {
    if sel_lo == sel_hi {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let (line_ranges, first, last) = super::comment::selected_lines(&chars, sel_lo, sel_hi);

    let lines: Vec<String> = line_ranges[first..=last]
        .iter()
        .map(|&(s, e)| chars[s..e].iter().collect())
        .collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        return None;
    }
    let indent = common_indent(&lines);
    let body = lines
        .iter()
        .map(|l| l.strip_prefix(indent.as_str()).unwrap_or(l).to_owned())
        .collect::<Vec<_>>()
        .join("\n");

    // The cut takes the lines' own newline with them, so the call replaces the
    // whole rows rather than leaving an empty one behind.
    let cut_start = line_ranges[first].0;
    let cut_end = (line_ranges[last].1 + 1).min(chars.len());
    let cut_text: String = chars[cut_start..cut_end].iter().collect();

    // The function the selection came out of: the INNERMOST `fn` region that
    // contains it. `fold::regions` is the folding scanner — it already skips
    // strings, char literals and nested block comments, so a brace inside a
    // string cannot mis-parent the selection.
    let insert_line = super::fold::regions(text)
        .into_iter()
        .filter(|r| {
            matches!(r.kind, super::fold::RegionKind::Fn) && r.head <= first && last <= r.end
        })
        // The INNERMOST one, i.e. the latest `head` — not `.map(end).max()`,
        // which picks the region that ends last and is therefore the OUTERMOST.
        .max_by_key(|r| r.head)
        .map(|r| r.end)?;
    // Just past that closing brace's newline.
    let insert_at = (line_ranges.get(insert_line)?.1 + 1).min(chars.len());

    let mut warnings = Vec::new();
    for (needle, warn) in [
        ("return", "`return` will return from the NEW function"),
        ("break", "`break` has no loop to break out of"),
        ("continue", "`continue` has no loop to continue"),
        ("?", "`?` needs a matching return type"),
    ] {
        if contains_word(&body, needle) {
            warnings.push(warn);
        }
    }

    Some(ExtractPlan {
        uses_self: contains_word(&body, "self"),
        is_async: body.contains(".await"),
        warnings,
        body,
        indent,
        cut: (cut_start, cut_end),
        cut_text,
        insert_at,
    })
}

/// Whole-word search, except for punctuation like `?` which has no word
/// boundary to speak of.
fn contains_word(hay: &str, needle: &str) -> bool {
    if !needle.chars().next().is_some_and(char::is_alphanumeric) {
        return hay.contains(needle);
    }
    let is_id = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(rel) = hay[from..].find(needle) {
        let s = from + rel;
        let e = s + needle.len();
        let before = hay[..s].chars().next_back().is_some_and(is_id);
        let after = hay[e..].chars().next().is_some_and(is_id);
        if !before && !after {
            return true;
        }
        from = e;
    }
    false
}

/// The new text, and where to put the caret (the start of the call).
///
/// `None` when the plan no longer describes the text — see
/// [`ExtractPlan::cut_text`].
pub(crate) fn apply(
    text: &str,
    plan: &ExtractPlan,
    name: &str,
    params: &str,
    ret: &str,
) -> Option<(String, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let (lo, hi) = plan.cut;
    if hi > chars.len() || plan.insert_at > chars.len() || plan.insert_at < hi {
        return None;
    }
    if chars[lo..hi].iter().collect::<String>() != plan.cut_text {
        return None;
    }
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let params = params.trim();
    let ret = ret.trim();

    // ── The call ────────────────────────────────────────────────────────────
    let mut call = String::from(&plan.indent);
    if !ret.is_empty() {
        // Something to bind. `result` is a placeholder you rename with Ctrl+R —
        // guessing a better name from the return type would be a worse guess.
        call.push_str("let result = ");
    }
    if plan.uses_self {
        call.push_str("self.");
    }
    call.push_str(name);
    call.push('(');
    call.push_str(&call_args(params));
    call.push(')');
    if plan.is_async {
        call.push_str(".await");
    }
    call.push_str(";\n");

    // ── The function ────────────────────────────────────────────────────────
    let mut decl = String::from("\n");
    decl.push_str(&plan.indent);
    if plan.is_async {
        decl.push_str("async ");
    }
    decl.push_str("fn ");
    decl.push_str(name);
    decl.push('(');
    decl.push_str(params);
    decl.push(')');
    if !ret.is_empty() {
        decl.push_str(" -> ");
        decl.push_str(ret);
    }
    decl.push_str(" {\n");
    for line in plan.body.lines() {
        if line.trim().is_empty() {
            decl.push('\n');
        } else {
            decl.push_str(&plan.indent);
            decl.push_str("    ");
            decl.push_str(line);
            decl.push('\n');
        }
    }
    decl.push_str(&plan.indent);
    decl.push_str("}\n");

    // Built in ONE pass, front to back: `insert_at` is always past the cut (the
    // enclosing function ends after the selection it contains), so no index has
    // to be adjusted for an edit made earlier in the string.
    let mut out: String = chars[..lo].iter().collect();
    out.push_str(&call);
    out.extend(&chars[hi..plan.insert_at]);
    out.push_str(&decl);
    out.extend(&chars[plan.insert_at..]);
    Some((out, lo + plan.indent.chars().count()))
}

/// A name that will parse as a function name.
fn valid_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && !n.chars().next().is_some_and(|c| c.is_ascii_digit())
        && n.chars().all(|c| c.is_alphanumeric() || c == '_')
}

// ── Wiring ───────────────────────────────────────────────────────────────────

impl AppIde {
    /// Ctrl+Alt+Insert: work out the extraction and open the popup.
    ///
    /// Refuses, with a reason in the status bar, rather than doing something
    /// surprising: a non-Rust file has no functions, and a selection touching
    /// main.rs's GENERATED block would put the new `fn` outside the markers and
    /// the CALL inside them — where the next regeneration erases it.
    pub(super) fn begin_extract_fn(
        &mut self,
        display_code: &str,
        file: ProjectFileId,
        rust_file: bool,
        sel: Option<(usize, usize)>,
        anchor: egui::Pos2,
    ) {
        if !rust_file {
            self.set_status_msg("Extract function: only in Rust files".into());
            return;
        }
        let Some((a, b)) = sel.filter(|(a, b)| a != b) else {
            self.set_status_msg("Extract function: select the lines first".into());
            return;
        };
        let (lo, hi) = (a.min(b), a.max(b));
        let Some(p) = plan(display_code, lo, hi) else {
            self.set_status_msg("Extract function: the selection is not inside a function".into());
            return;
        };
        if file == ProjectFileId::MainRs && overlaps_generated(display_code, &p) {
            self.set_status_msg(
                "Extract function: that block is regenerated — the call would be erased".into(),
            );
            return;
        }
        self.ed.extract.name = String::new();
        // A receiver is the one parameter that can be filled in for you: the
        // code says `self`, so the function needs one and the call carries it.
        self.ed.extract.params = if p.uses_self {
            "&mut self".to_owned()
        } else {
            String::new()
        };
        self.ed.extract.ret = String::new();
        self.ed.extract.plan = Some(p);
        self.ed.extract.file = Some(file);
        self.ed.extract.pos = anchor;
        self.ed.extract.focus = true;
        self.ed.extract.active = true;
    }

    /// Draw the popup. Returns the new text when the extraction was applied.
    pub(super) fn show_extract_fn_popup(
        &mut self,
        ui: &mut egui::Ui,
        display_code: &str,
        file: ProjectFileId,
    ) -> Option<(String, usize)> {
        if !self.ed.extract.active {
            return None;
        }
        // The view moved to another file with the popup open.
        if self.ed.extract.file != Some(file) {
            self.ed.extract.active = false;
            return None;
        }
        let mut submit = false;
        let mut cancel = false;
        let warnings = self
            .ed
            .extract
            .plan
            .as_ref()
            .map(|p| p.warnings.clone())
            .unwrap_or_default();

        egui::Area::new(egui::Id::new("extract_fn_popup"))
            .fixed_pos(self.ed.extract.pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(&ui.ctx().global_style()).show(ui, |ui| {
                    ui.set_min_width(320.0);
                    ui.label(
                        egui::RichText::new("Extract the selected lines into a function")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(170, 180, 200)),
                    );
                    let mut enter = false;
                    let name = ui.add(
                        egui::TextEdit::singleline(&mut self.ed.extract.name)
                            .desired_width(300.0)
                            .hint_text("function name"),
                    );
                    if self.ed.extract.focus {
                        name.request_focus();
                        self.ed.extract.focus = false;
                    }
                    enter |= crate::app::helpers::text_field::ended_with_enter(ui, &name);
                    let params = ui.add(
                        egui::TextEdit::singleline(&mut self.ed.extract.params)
                            .desired_width(300.0)
                            .hint_text("parameters, e.g.  x: u8, buf: &mut [u8]"),
                    );
                    enter |= crate::app::helpers::text_field::ended_with_enter(ui, &params);
                    let ret = ui.add(
                        egui::TextEdit::singleline(&mut self.ed.extract.ret)
                            .desired_width(300.0)
                            .hint_text("return type (empty for none)"),
                    );
                    enter |= crate::app::helpers::text_field::ended_with_enter(ui, &ret);
                    if enter {
                        submit = true;
                    }
                    for w in &warnings {
                        ui.label(
                            egui::RichText::new(format!("!  {w}"))
                                .size(10.0)
                                .color(egui::Color32::from_rgb(220, 180, 90)),
                        );
                    }
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let ok = valid_name(&self.ed.extract.name);
                        if ui
                            .add_enabled(ok, egui::Button::new("Extract"))
                            .on_disabled_hover_text("Give the function a name first")
                            .clicked()
                        {
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
            self.ed.extract.active = false;
            self.ed.extract.plan = None;
            return None;
        }
        if !submit || !valid_name(&self.ed.extract.name) {
            return None;
        }
        let out = self.ed.extract.plan.as_ref().and_then(|p| {
            apply(
                display_code,
                p,
                &self.ed.extract.name,
                &self.ed.extract.params,
                &self.ed.extract.ret,
            )
        });
        self.ed.extract.active = false;
        self.ed.extract.plan = None;
        if out.is_none() {
            self.set_status_msg("Extract function: the code moved — select it again".into());
        }
        out
    }
}

/// Does the cut, or the place the new function would go, touch a GENERATED
/// block? Byte ranges from `generated_byte_ranges`, char ranges here — compared
/// after converting the plan's char indices, since the two only agree on ASCII.
fn overlaps_generated(text: &str, p: &ExtractPlan) -> bool {
    let ranges = crate::app::generated_byte_ranges(text);
    if ranges.is_empty() {
        return false;
    }
    let byte_of = |char_idx: usize| -> usize {
        text.char_indices()
            .nth(char_idx)
            .map_or(text.len(), |(b, _)| b)
    };
    let (lo, hi) = (byte_of(p.cut.0), byte_of(p.cut.1));
    let at = byte_of(p.insert_at);
    ranges
        .iter()
        .any(|&(s, e)| (lo < e && hi > s) || (at >= s && at < e))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = a + b;\n}\n";

    /// Char index of the first character of 1-based `line`.
    fn at(text: &str, line: usize) -> usize {
        text.split('\n').take(line - 1).map(|l| l.len() + 1).sum()
    }

    // ── common_indent ────────────────────────────────────────────────────────

    #[test]
    fn the_shared_indent_is_the_shortest_common_prefix() {
        let lines = ["    a".to_owned(), "        b".to_owned()];
        assert_eq!(common_indent(&lines), "    ");
    }

    /// One empty line in the middle must not flatten the block to column 0.
    #[test]
    fn a_blank_line_does_not_count_as_zero_indent() {
        let lines = ["    a".to_owned(), String::new(), "    b".to_owned()];
        assert_eq!(common_indent(&lines), "    ");
    }

    // ── call_args ────────────────────────────────────────────────────────────

    #[test]
    fn the_call_passes_the_names_without_the_types() {
        assert_eq!(call_args("x: u8, buf: &mut [u8]"), "x, buf");
        assert_eq!(call_args(""), "");
    }

    /// A comma inside a type belongs to the type, not to the list.
    #[test]
    fn a_generic_type_is_one_parameter() {
        assert_eq!(call_args("m: HashMap<u8, u16>, n: u8"), "m, n");
        assert_eq!(call_args("f: fn(u8, u8) -> u8"), "f");
    }

    /// A receiver travels as `self.name(…)`, never as an argument.
    #[test]
    fn a_receiver_is_not_an_argument() {
        assert_eq!(call_args("&mut self, x: u8"), "x");
        assert_eq!(call_args("&self"), "");
        assert_eq!(call_args("self, x: u8"), "x");
    }

    // ── plan ─────────────────────────────────────────────────────────────────

    #[test]
    fn the_plan_cuts_whole_lines_and_strips_their_indent() {
        // Half of line 2 through half of line 3 — both lines go.
        let p = plan(SRC, at(SRC, 2) + 6, at(SRC, 3) + 6).expect("a plan");
        assert_eq!(p.body, "let a = 1;\nlet b = 2;");
        assert_eq!(p.indent, "    ");
        assert_eq!(p.cut_text, "    let a = 1;\n    let b = 2;\n");
    }

    #[test]
    fn an_empty_or_blank_selection_has_no_plan() {
        assert!(plan(SRC, 5, 5).is_none());
        assert!(plan("\n\n\n", 0, 2).is_none());
    }

    /// Nowhere to put the result — a `fn` at the end of the file would be a
    /// guess, not an extraction.
    #[test]
    fn a_selection_outside_any_function_has_no_plan() {
        let src = "use core::mem;\nuse core::ptr;\nfn main() {}\n";
        assert!(plan(src, 0, at(src, 2) + 5).is_none());
    }

    /// The INNERMOST enclosing function wins, so extracting from a nested one
    /// lands after IT, not after the outer.
    #[test]
    fn the_new_function_goes_after_the_innermost_enclosing_one() {
        let src = "fn outer() {\n    fn inner() {\n        let x = 1;\n    }\n    inner();\n}\n";
        let p = plan(src, at(src, 3), at(src, 3) + 10).expect("a plan");
        // Line 4 closes `inner`; the insert point is the start of line 5.
        assert_eq!(p.insert_at, at(src, 5));
    }

    #[test]
    fn self_and_await_are_noticed() {
        let src = "fn f() {\n    self.x().await;\n}\n";
        let p = plan(src, at(src, 2), at(src, 2) + 5).expect("a plan");
        assert!(p.uses_self);
        assert!(p.is_async);
    }

    /// `myself` is not `self`.
    #[test]
    fn a_longer_word_is_not_a_receiver() {
        let src = "fn f() {\n    let myself = 1;\n}\n";
        let p = plan(src, at(src, 2), at(src, 2) + 5).expect("a plan");
        assert!(!p.uses_self);
    }

    #[test]
    fn control_flow_is_warned_about() {
        let src = "fn f() {\n    return 1;\n}\n";
        let p = plan(src, at(src, 2), at(src, 2) + 5).expect("a plan");
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
        assert!(p.warnings[0].contains("return"));
    }

    // ── apply ────────────────────────────────────────────────────────────────

    #[test]
    fn the_lines_become_a_function_and_leave_a_call() {
        let p = plan(SRC, at(SRC, 2), at(SRC, 3) + 6).expect("a plan");
        let (out, _) = apply(SRC, &p, "setup", "", "").expect("applied");
        assert_eq!(
            out,
            "fn main() {\n    \
             setup();\n    \
             let c = a + b;\n\
             }\n\
             \n    \
             fn setup() {\n        \
             let a = 1;\n        \
             let b = 2;\n    \
             }\n"
        );
    }

    #[test]
    fn a_return_type_binds_the_call() {
        let p = plan(SRC, at(SRC, 2), at(SRC, 2) + 6).expect("a plan");
        let (out, _) = apply(SRC, &p, "one", "", "u8").expect("applied");
        assert!(out.contains("    let result = one();\n"), "{out}");
        assert!(out.contains("fn one() -> u8 {"), "{out}");
    }

    #[test]
    fn a_receiver_and_an_await_reach_the_call_too() {
        let src = "fn f() {\n    self.tick().await;\n}\n";
        let p = plan(src, at(src, 2), at(src, 2) + 6).expect("a plan");
        let (out, _) = apply(src, &p, "step", "&mut self, n: u8", "").expect("applied");
        assert!(out.contains("    self.step(n).await;\n"), "{out}");
        assert!(out.contains("async fn step(&mut self, n: u8) {"), "{out}");
    }

    /// The caret lands on the call, past its indent.
    #[test]
    fn the_caret_lands_on_the_new_call() {
        let p = plan(SRC, at(SRC, 2), at(SRC, 2) + 6).expect("a plan");
        let (out, caret) = apply(SRC, &p, "one", "", "").expect("applied");
        assert!(out[caret..].starts_with("one();"), "{:?}", &out[caret..]);
    }

    /// A plan applied to text it was not made for must refuse, not corrupt.
    #[test]
    fn a_stale_plan_is_refused() {
        let p = plan(SRC, at(SRC, 2), at(SRC, 2) + 6).expect("a plan");
        let moved = format!("// a new line on top\n{SRC}");
        assert!(apply(&moved, &p, "one", "", "").is_none());
    }

    #[test]
    fn a_nameless_extraction_is_refused() {
        let p = plan(SRC, at(SRC, 2), at(SRC, 2) + 6).expect("a plan");
        assert!(apply(SRC, &p, "  ", "", "").is_none());
    }

    // ── valid_name ───────────────────────────────────────────────────────────

    #[test]
    fn the_name_has_to_parse_as_one() {
        assert!(valid_name("do_thing"));
        assert!(valid_name("f2"));
        assert!(!valid_name(""));
        assert!(!valid_name("2f"));
        assert!(!valid_name("do thing"));
        assert!(!valid_name("do-thing"));
    }
}
