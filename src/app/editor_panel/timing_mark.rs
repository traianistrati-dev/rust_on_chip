//! The editor's "Time this function" / "Time selected lines": a region for the
//! Profile tab's Timing mode (see [`crate::timing`]).
//!
//! "Time this function" times what the caret is ON. rust-analyzer is asked
//! what that is - the go-to-definition question F12 asks -, so a call to a
//! closure, a method, a function behind a macro's arguments or another
//! module's path is followed exactly as the compiler resolves it. When the
//! caret names nothing callable (a keyword, a variable, a type), it is the
//! innermost function or closure around the caret. A call that cannot be
//! followed is refused with why, never quietly replaced by the function the
//! call sits in.

use crate::app::{AppIde, GotoOrigin, ProjectFileId};

/// `(1-based line, 0-based char column)` of char index `at` in `text`.
fn line_col(text: &str, at: usize) -> (usize, usize) {
    let (mut line, mut col) = (1, 0);
    for c in text.chars().take(at) {
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// The char column of LSP column `utf16` (UTF-16 units) on `line`.
fn char_col(line: &str, utf16: u32) -> usize {
    let mut units = 0;
    for (i, c) in line.chars().enumerate() {
        if units >= utf16 as usize {
            return i;
        }
        units += c.len_utf16();
    }
    line.chars().count()
}

/// A "Time this function" waiting for rust-analyzer's answer.
pub(crate) struct TimingAsk {
    /// The file and caret it was asked at: what is timed when the answer
    /// names nothing callable.
    rel: String,
    line: usize,
    col: usize,
    /// The name of the call the caret is on, if it is on one.
    call: Option<String>,
    /// The name of the path the caret is on: a call's, a function passed by
    /// name, a variable.
    path: Option<String>,
}

impl AppIde {
    /// "Time this function" / "Time selected lines" from the editor's menu.
    pub(super) fn mark_timing_region(
        &mut self,
        file: &ProjectFileId,
        text: &str,
        selection: Option<(usize, usize)>,
        whole_fn: bool,
    ) {
        let Some(shown) = crate::editor::gui::text_pos::selected_file_rel_path(
            file,
            &self.project_tree.user_src_files,
        ) else {
            return;
        };
        let Some((a, b)) = selection else {
            self.set_status_msg("Place the caret in the code to time first.".to_owned());
            return;
        };
        if self.timing.is_busy() {
            self.set_status_msg("Stop the measurement first, then mark more code.".to_owned());
            return;
        }
        if !whole_fn {
            let chars: Vec<char> = text.chars().collect();
            let (_, first, last) = super::comment::selected_lines(&chars, a, b);
            self.mark_timing_at(
                shown,
                text.to_owned(),
                (first as u32 + 1, last as u32 + 1),
                0,
                false,
            );
            return;
        }
        let (line, col) = line_col(text, a);
        let call = crate::timing::call_name_at(text, line, col);
        let path = crate::timing::path_name_at(text, line, col);
        // Ask rust-analyzer what the caret is on, as F12 does.
        let usable = {
            let lsp = self.lsp_state.lock().unwrap();
            matches!(lsp.status, crate::lsp::LspStatus::Ready)
                && (lsp.indexed || lsp.is_file_open(&shown))
        };
        if usable && shown.ends_with(".rs") {
            let (l16, c16) = crate::editor::gui::text_pos::lsp_cursor_pos(text, a);
            let sent = {
                let mut lsp = self.lsp_state.lock().unwrap();
                lsp.did_change(&shown, text, false);
                lsp.request_definition(&shown, l16, c16)
            };
            if sent {
                // The one answer slot is this question's now.
                self.impl_picker = None;
                self.pending_goto = None;
                self.lsp_asker.definition = GotoOrigin::Timing;
                self.definition_in_flight = true;
                self.timing_ask = Some(TimingAsk {
                    rel: shown,
                    line,
                    col,
                    call,
                    path,
                });
                return;
            }
        }
        // No rust-analyzer to ask: the function or closure around the caret -
        // but a call under it would be followed by no one, and timing the
        // function it sits in instead is exactly what the user did not ask.
        if let Some(name) = call {
            self.set_status_msg(format!(
                concat!(
                    "Cannot time this: the caret is on a call to `{}`, and rust-analyzer ",
                    "is not ready to say where it is defined. Wait for it, or open the ",
                    "definition and use Time this function there.",
                ),
                name
            ));
            return;
        }
        // A function passed by name (`for_each(toggle)`) is no call, but it
        // is what the caret means just the same.
        if let Some(name) = path.filter(|n| self.project_defines(n)) {
            self.set_status_msg(format!(
                concat!(
                    "Cannot time this: rust-analyzer is not ready to say which `{}` the ",
                    "caret is on. Wait for it, or open its definition and use Time this ",
                    "function there.",
                ),
                name
            ));
            return;
        }
        self.mark_at_caret(shown, text.to_owned(), line, col);
    }

    /// Whether a project file defines a function or closure named `name`.
    fn project_defines(&self, name: &str) -> bool {
        self.source_texts()
            .values()
            .any(|t| crate::timing::defines_callable(t, name))
    }

    /// "Time this function" with no call under the caret: the function or
    /// closure whose NAME the caret is on - `print_menu` in `let print_menu =
    /// async |..|`, which is left of where the closure itself starts -, else
    /// the innermost one around the caret.
    fn mark_at_caret(&mut self, rel: String, text: String, line: usize, col: usize) {
        let (first, col) = crate::timing::callable_at_definition(&text, line, col)
            .unwrap_or((line as u32, col as u32));
        self.mark_timing_at(rel, text, (first, first), col, true);
    }

    /// rust-analyzer's answer to a "Time this function": `locs` is where the
    /// symbol under the caret is defined.
    pub(crate) fn timing_definition_answer(
        &mut self,
        locs: Vec<crate::lsp::DefinitionLoc>,
        error: Option<String>,
    ) {
        let Some(ask) = self.timing_ask.take() else {
            return;
        };
        let texts = self.source_texts();
        let mut callables: Vec<(String, u32, u32)> = Vec::new();
        let mut library: Option<String> = None;
        // Where rust-analyzer pointed when it is nothing callable: file,
        // 1-based line, that line's code.
        let mut not_callable: Option<(String, u32, String, crate::timing::NotCallable)> = None;
        for loc in &locs {
            let rel =
                crate::app::project_file_for_def(&loc.path, &self.project_tree.user_src_files)
                    .and_then(|id| {
                        crate::editor::gui::text_pos::selected_file_rel_path(
                            &id,
                            &self.project_tree.user_src_files,
                        )
                    });
            let Some(rel) = rel else {
                library = Some(loc.path.replace('\\', "/"));
                continue;
            };
            let text = texts.get(&rel).map(String::as_str).unwrap_or_default();
            let line = text.lines().nth(loc.line as usize).unwrap_or_default();
            let col = char_col(line, loc.character);
            match crate::timing::callable_at_definition(text, loc.line as usize + 1, col) {
                Some((l, c)) if !callables.contains(&(rel.clone(), l, c)) => {
                    callables.push((rel, l, c));
                }
                Some(_) => {}
                None => {
                    let why = crate::timing::not_callable(text, &rel, loc.line as usize + 1, col);
                    not_callable = Some((rel, loc.line + 1, line.trim().to_owned(), why));
                }
            }
        }
        let name = ask.call.clone().unwrap_or_default();
        match (callables.len(), &ask.call) {
            (1, _) => {
                let (rel, line, col) = callables.remove(0);
                let text = texts.get(&rel).cloned().unwrap_or_default();
                self.mark_timing_at(rel, text, (line, line), col, true);
            }
            (n, _) if n > 1 => self.set_status_msg(format!(
                concat!(
                    "Cannot time this: rust-analyzer gives {} definitions for `{}` - open the ",
                    "one you mean and use Time this function inside it.",
                ),
                n, name
            )),
            // The caret is on a call, and it is not to a function of the
            // project: say so, never time the function around the call.
            (_, Some(_)) if library.is_some() => self.set_status_msg(format!(
                concat!(
                    "Cannot time this: `{}` is defined outside the project ({}), and a ",
                    "library's code cannot be timed. To time the call itself, select its ",
                    "line and use Time selected lines.",
                ),
                name,
                library.as_deref().unwrap_or_default()
            )),
            (_, Some(_)) if not_callable.is_some() => {
                use crate::timing::NotCallable;
                let Some((rel, line, code, why)) = not_callable else {
                    return;
                };
                let msg = match why {
                    NotCallable::Unparsed(why) => format!("Cannot time this: {why}."),
                    NotCallable::TraitFnWithoutBody => format!(
                        concat!(
                            "Cannot time this: `{}` is a trait method with no body there ",
                            "({}:{}) - the type decides which impl runs. Open one (Ctrl+F12) ",
                            "and use Time this function inside it.",
                        ),
                        name, rel, line
                    ),
                    NotCallable::ExternFn => format!(
                        concat!(
                            "Cannot time this: `{}` is declared in an `extern` block ({}:{}): ",
                            "its code is not Rust of this project. To time the call, select ",
                            "its line and use Time selected lines.",
                        ),
                        name, rel, line
                    ),
                    NotCallable::Other => format!(
                        concat!(
                            "Cannot time this: `{}` is not a function or a closure of the ",
                            "project - rust-analyzer points at {}:{}: `{}`.",
                        ),
                        name, rel, line, code
                    ),
                };
                self.set_status_msg(msg);
            }
            (_, Some(_)) => self.set_status_msg(format!(
                "Cannot time this: rust-analyzer found no definition for `{name}`{}.",
                error.map(|e| format!(" ({e})")).unwrap_or_default()
            )),
            // On a function passed by name that rust-analyzer did not find.
            _ if ask.path.as_deref().is_some_and(|n| self.project_defines(n)) => {
                self.set_status_msg(format!(
                    "Cannot time this: rust-analyzer found no definition for `{}`{}.",
                    ask.path.clone().unwrap_or_default(),
                    error.map(|e| format!(" ({e})")).unwrap_or_default()
                ));
            }
            // On no call: the function or closure the caret names or is in.
            _ => {
                let text = texts.get(&ask.rel).cloned().unwrap_or_default();
                self.mark_at_caret(ask.rel, text, ask.line, ask.col);
            }
        }
        self.egui_ctx.request_repaint();
    }

    /// Mark `lines` of `rel` (whose text is `text`) - for a whole function,
    /// the one at `(lines.0, col)` - and show the Timing table. Anything that
    /// cannot be timed is refused in the status bar, with why.
    fn mark_timing_at(
        &mut self,
        rel: String,
        text: String,
        lines: (u32, u32),
        col: u32,
        whole_fn: bool,
    ) {
        if let Err(why) = crate::timing::instrumentable(&rel) {
            self.set_status_msg(format!("Cannot time this: {why}."));
            return;
        }
        let shown = crate::editor::gui::text_pos::selected_file_rel_path(
            &self.selected_file,
            &self.project_tree.user_src_files,
        );
        let asked = crate::timing::TimingRegion {
            file: rel,
            first: lines.0,
            last: lines.1,
            whole_fn,
            col,
            func: String::new(),
            fn_copy: (0, 1),
            anchor: Vec::new(),
            copy: (0, 1),
        };
        // Stored as what it covers, so marking the same code twice is one row
        // and the table shows the lines that will really be timed.
        let (region, placed) = match crate::timing::mark(&text, asked) {
            Ok(m) => m,
            Err(why) => {
                self.set_status_msg(format!("Cannot time this: {why}."));
                return;
            }
        };
        self.build_tab = crate::app::BuildPanelTab::Profile;
        self.profile_mode = crate::profile::ProfileMode::Timing;
        let mut what = if whole_fn {
            format!("fn {}", placed.func)
        } else if placed.lines.0 == placed.lines.1 {
            format!("line {} in {}", placed.lines.0, placed.func)
        } else {
            format!(
                "lines {}-{} in {}",
                placed.lines.0, placed.lines.1, placed.func
            )
        };
        // A task that never returns was turned into one pass of its loop.
        if whole_fn && !region.whole_fn {
            let lines = if placed.lines.0 == placed.lines.1 {
                format!("line {}", placed.lines.0)
            } else {
                format!("lines {}-{}", placed.lines.0, placed.lines.1)
            };
            what = format!(
                concat!(
                    "one pass of the loop of {} ({}) - it never returns, so its own time ",
                    "would never end",
                ),
                placed.func, lines
            );
        }
        if shown.as_deref() != Some(region.file.as_str()) {
            what.push_str(&format!(" ({})", region.file));
        }
        // Rows of this file whose code changed can never be measured again
        // (Measure refuses them): marking anew replaces them.
        let before = self.timing.regions.len();
        self.timing
            .regions
            .retain(|s| s.file != region.file || crate::timing::resolve(&text, s).is_ok());
        let dropped = match before - self.timing.regions.len() {
            0 => String::new(),
            1 => " (1 row whose code had changed is gone)".to_owned(),
            n => format!(" ({n} rows whose code had changed are gone)"),
        };
        // The same code, found where it is now - not by the line numbers the
        // row was stored with, which an edit above it has moved.
        let same = self.timing.regions.iter().position(|s| {
            s.file == region.file
                && s.whole_fn == region.whole_fn
                && crate::timing::resolve(&text, s).is_ok_and(|(_, p)| {
                    p.lines == (region.first, region.last) && p.fn_col == placed.fn_col
                })
        });
        if let Some(i) = same {
            self.timing.regions[i] = region;
            self.set_status_msg(format!("Already timed: {what}.{dropped}"));
        } else if self.timing.regions.len() >= crate::timing::MAX_REGIONS {
            self.set_status_msg(format!(
                "At most {} regions: remove one in the Profile tab first.{dropped}",
                crate::timing::MAX_REGIONS
            ));
        } else {
            self.timing.regions.push(region);
            self.set_status_msg(format!(
                "Timing {what}: press Measure in the Profile tab.{dropped}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{char_col, line_col};

    #[test]
    fn a_char_index_is_a_line_and_a_char_column() {
        let text = "fn a() {\n    ăb(x);\n}\n";
        assert_eq!(line_col(text, 0), (1, 0));
        // `b` after a two-byte `ă`: columns count chars, as syn's spans do.
        let at = text.chars().position(|c| c == 'b').unwrap();
        assert_eq!(line_col(text, at), (2, 5));
    }

    #[test]
    fn an_lsp_column_is_turned_into_a_char_column() {
        // `😀` is two UTF-16 units, one char.
        assert_eq!(char_col("let 😀 = f(x);", 7), 6);
        assert_eq!(char_col("    show(1)", 4), 4);
        assert_eq!(char_col("ab", 9), 2);
    }
}
