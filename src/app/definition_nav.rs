//! Walking definitions inside the read-only Definition tab: F12 again from the
//! page on screen, and Back / Forward over the pages visited.
//!
//! The tab used to be a dead end. It showed the file F12 had landed in — a crate
//! in the registry, or std — as plain labels, with no caret to press F12 from
//! and no way back once a second definition replaced the first. Three things
//! stood in the way, and each has its own piece here:
//!
//! - **No position.** The rows are labels, and egui will not say where a label
//!   was clicked. A click now records a caret of the tab's own, which is where
//!   F12, Ctrl+F12 and Ctrl+Click ask from.
//! - **No keyboard.** A label takes no focus, so after a click in the tab NOTHING
//!   was focused — and the main editor's "nobody is focused" fallback took F12
//!   and jumped from ITS caret. The tab now owns the keyboard after a click in
//!   it (`def_owns_kbd`, read by the editor's keyboard-scope gate).
//! - **No request.** `request_definition` only builds workspace URIs. These
//!   files are asked about by the URI rust-analyzer itself reported, and never
//!   opened (see `LspState::request_goto_at_uri` for what opening one costs).
//!
//! The history is a browser's: a new jump from an older page abandons the pages
//! ahead of it. Entries are rebuilt from disk on every step rather than cached
//! whole — a view holds the entire file plus up to 600 laid-out rows, a few MB
//! on a large HAL file, where an entry is a path, two positions and an offset.

use super::{AppIde, DefinitionView, EditorSlot, GotoDoc, GotoOrigin, McuTab, PendingGoto};
use crate::lsp;
use eframe::egui;
use egui_phosphor::regular as ph;

/// How many pages Back can go. Beyond it the oldest is forgotten.
pub(crate) const DEF_HISTORY_CAP: usize = 50;

/// One step through the history.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Step {
    Back,
    Forward,
}

/// Where the Definition tab scrolls on its next draw.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum DefScroll {
    /// A new page: the definition near the top, two lines of context above.
    Target,
    /// A page revisited: exactly where it was left, so Back returns to the line
    /// being read rather than to the top of the item.
    Restore(egui::Vec2),
}

/// A visited page, reduced to what rebuilds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DefEntry {
    pub path: String,
    pub uri: String,
    /// Where rust-analyzer pointed: 0-based line, UTF-16 column.
    pub line: u32,
    pub character: u32,
    /// The item's extent, kept so a step back skips the full-file scan that
    /// finds it — 250 ms on a 40 000-line register file in a debug build.
    pub extent: (usize, usize),
    pub scroll: egui::Vec2,
    pub word: String,
    pub caret: Option<(usize, usize)>,
    /// `crate/src/file.rs:42`, for the buttons' hover text.
    pub title: String,
}

/// The pages behind and ahead of the one on screen. The page on screen is not
/// in here: it lives in `definition_view`, and joins a stack only when left.
#[derive(Debug)]
pub(crate) struct DefHistory<T> {
    back: Vec<T>,
    fwd: Vec<T>,
}

impl<T> Default for DefHistory<T> {
    fn default() -> Self {
        Self {
            back: Vec::new(),
            fwd: Vec::new(),
        }
    }
}

impl<T> DefHistory<T> {
    /// A NEW jump: the page being left goes behind, and everything ahead is
    /// abandoned — as in a browser, following a link from an older page.
    pub(crate) fn push(&mut self, leaving: T) {
        self.back.push(leaving);
        self.cap_back();
        self.fwd.clear();
    }

    /// The page one step away, taken off its stack.
    ///
    /// Two-phase on purpose: the caller builds the page and only then hands the
    /// page it left to [`record`](Self::record). An entry that no longer opens
    /// (a registry folder cargo cleaned up) is simply dropped, and the page on
    /// screen is not lost with it.
    pub(crate) fn take(&mut self, step: Step) -> Option<T> {
        match step {
            Step::Back => self.back.pop(),
            Step::Forward => self.fwd.pop(),
        }
    }

    /// File the page just left on the stack OPPOSITE the step taken.
    pub(crate) fn record(&mut self, step: Step, leaving: T) {
        match step {
            Step::Back => self.fwd.push(leaving),
            Step::Forward => {
                self.back.push(leaving);
                self.cap_back();
            }
        }
    }

    /// The page one step away, without moving.
    pub(crate) fn peek(&self, step: Step) -> Option<&T> {
        match step {
            Step::Back => self.back.last(),
            Step::Forward => self.fwd.last(),
        }
    }

    /// `(behind, ahead)` of the page on screen.
    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.back.len(), self.fwd.len())
    }

    pub(crate) fn clear(&mut self) {
        self.back.clear();
        self.fwd.clear();
    }

    fn cap_back(&mut self) {
        if self.back.len() > DEF_HISTORY_CAP {
            let excess = self.back.len() - DEF_HISTORY_CAP;
            self.back.drain(..excess);
        }
    }
}

/// Byte range of every line of `text`, with exactly `str::lines`'s rules: no
/// `\n`, a trailing `\r` dropped, and no empty line after a final newline.
///
/// Computed once per page. The tab used to collect `lines()` into a fresh `Vec`
/// EVERY frame — about 80 ms a frame in a debug build on the 40 000-line
/// register files a HAL is made of, which chained F12 visits far more often.
pub(crate) fn line_ranges(text: &str) -> Vec<(usize, usize)> {
    let base = text.as_ptr() as usize;
    text.lines()
        .map(|l| {
            let start = l.as_ptr() as usize - base;
            (start, start + l.len())
        })
        .collect()
}

/// The LSP column (UTF-16 units) of char index `char_idx` in `line`.
pub(crate) fn utf16_col(line: &str, char_idx: usize) -> u32 {
    line.chars()
        .take(char_idx)
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// The char index of LSP column `col` (UTF-16 units) in `line`. A column inside
/// a surrogate pair lands on the next char; one past the end, on the end.
pub(crate) fn char_of_utf16(line: &str, col: u32) -> usize {
    let mut units = 0u32;
    for (i, c) in line.chars().enumerate() {
        if units >= col {
            return i;
        }
        units += c.len_utf16() as u32;
    }
    line.chars().count()
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier `[start, end)` at char index `idx` in `line`, or the one just
/// before it.
///
/// "Just before" because a click on the right half of a name's last letter
/// rounds to the boundary AFTER it — and rust-analyzer resolves that position to
/// the name as well. `None` on space or punctuation: nothing to ask about.
pub(crate) fn ident_near(line: &str, idx: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let at = if idx < chars.len() && is_ident(chars[idx]) {
        idx
    } else if idx > 0 && chars.get(idx - 1).is_some_and(|&c| is_ident(c)) {
        idx - 1
    } else {
        return None;
    };
    let mut start = at;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && is_ident(chars[end]) {
        end += 1;
    }
    Some((start, end))
}

/// Is a new jump landing on the page already on screen?
///
/// Paths are compared loosely: rust-analyzer reports `c:\…` with a lower-case
/// drive letter, and a page opened through another route may spell it `C:/…`.
pub(crate) fn same_place(a_path: &str, a_line: u32, b_path: &str, b_line: u32) -> bool {
    let norm = |p: &str| p.replace('/', "\\").to_lowercase();
    a_line == b_line && norm(a_path) == norm(b_path)
}

/// Why a go-to asked from the Definition tab found nothing, when there is
/// something to say. The editor's own explanation (`caret_silence_reason`) is
/// built on workspace files and would blame every miss here on "no analysis for
/// this file yet" — rust-analyzer publishes nothing for library files.
pub(crate) fn external_silence_reason(
    error: Option<&str>,
    ready: bool,
    indexed: bool,
) -> Option<String> {
    if let Some(e) = error {
        // Measured: a file outside the crate graph (a page kept from an earlier
        // project or analyzer session) answers exactly this.
        if e.contains("file not found") {
            return Some("this file is not part of the current project's dependencies".to_owned());
        }
        let short: String = e.chars().take(80).collect();
        return Some(format!("rust-analyzer: {short}"));
    }
    if !ready {
        return Some("the analyzer is not running".to_owned());
    }
    if !indexed {
        return Some("the analyzer is still indexing".to_owned());
    }
    // Measured too: an identifier inside a doc comment answers `null`, and the
    // tab shows a great deal of documentation. Code a `cfg` switches off — the
    // other chips' half of a HAL — has nothing to jump to either.
    Some("comments, macro bodies and code switched off by `cfg` have no definition".to_owned())
}

impl DefinitionView {
    /// Line `i` of the file, without its line break.
    pub(super) fn line(&self, i: usize) -> &str {
        self.line_ranges
            .get(i)
            .map_or("", |&(a, b)| &self.code[a..b])
    }

    pub(super) fn line_count(&self) -> usize {
        self.line_ranges.len()
    }

    /// What rebuilds this page later.
    fn entry(&self) -> DefEntry {
        DefEntry {
            path: self.path.clone(),
            uri: self.uri.clone(),
            line: self.target.0,
            character: self.target.1,
            extent: self.extent,
            scroll: self.scroll,
            word: self.word.clone(),
            caret: self.caret,
            title: self.title.clone(),
        }
    }
}

impl AppIde {
    /// Show `loc` in the Definition tab, the page on screen going into the
    /// history. Every jump that lands outside the project comes through here,
    /// from the editor and from the tab alike: one walk, not one per origin.
    pub(super) fn def_open(&mut self, loc: &lsp::DefinitionLoc) {
        // F12 on the item's own name comes straight back to this page. Scroll
        // to it, but record no step — Back would only show the same page again.
        if let Some(cur) = &self.definition_view
            && same_place(&cur.path, cur.target.0, &loc.path, loc.line)
        {
            self.def_scroll_to = Some(DefScroll::Target);
            self.def_show_tab();
            return;
        }
        // Built BEFORE anything moves: a file that cannot be read must leave
        // the page on screen and the history exactly as they were.
        let Some(view) = super::build_definition_view(loc, None) else {
            self.set_status_msg(format!(
                "{} Cannot open {}",
                ph::X_CIRCLE,
                super::short_path(&loc.path)
            ));
            return;
        };
        if let Some(cur) = self.definition_view.take() {
            self.def_history.push(cur.entry());
        }
        self.definition_view = Some(view);
        self.def_scroll_to = Some(DefScroll::Target);
        self.def_show_tab();
    }

    /// Bring the tab forward. It lives in the middle zone, so a collapsed
    /// layout would swallow the page — F12 would look like it did nothing.
    fn def_show_tab(&mut self) {
        if self.active_tab != McuTab::Definition {
            self.definition_return_tab = self.active_tab;
        }
        self.active_tab = McuTab::Definition;
        self.side_panels_collapsed = false;
    }

    /// Back or Forward one page.
    pub(super) fn def_step(&mut self, step: Step) {
        if self.definition_view.is_none() {
            return;
        }
        let Some(target) = self.def_history.take(step) else {
            return;
        };
        let loc = lsp::DefinitionLoc {
            path: target.path.clone(),
            uri: target.uri.clone(),
            line: target.line,
            character: target.character,
        };
        let Some(mut view) = super::build_definition_view(&loc, Some(target.extent)) else {
            self.set_status_msg(format!(
                "{} {} can no longer be read — dropped from the history",
                ph::X_CIRCLE,
                target.title
            ));
            return;
        };
        // Moving on abandons a jump still on its way: landing after the step,
        // it would push the page just left back on top and wipe Forward. Only
        // once the step is real — Alt+Left on the first page must not silently
        // drop the F12 the user is waiting for.
        self.def_cancel_tab_goto();
        view.word = target.word;
        view.caret = target.caret;
        let leaving = self
            .definition_view
            .take()
            .expect("checked at the top")
            .entry();
        self.def_history.record(step, leaving);
        self.definition_view = Some(view);
        self.def_scroll_to = Some(DefScroll::Restore(target.scroll));
    }

    /// The ✕: the page and the whole walk go, along with any jump the tab still
    /// has in flight, parked, or offered in a chooser.
    pub(super) fn def_close(&mut self) {
        self.def_cancel_tab_goto();
        self.definition_view = None;
        self.def_history.clear();
        self.def_owns_kbd = false;
        if self.active_tab == McuTab::Definition {
            self.active_tab = self.definition_return_tab;
        }
    }

    /// A project change: every go-to still on its way — in flight, parked, or
    /// offered in a chooser — asked about the project being LEFT (a parked one
    /// would send the old workspace-relative path to the new analyzer), and the
    /// tab's walk goes too: its pages are the old project's dependencies, for
    /// which the new analyzer answers "file not found".
    pub(super) fn drop_project_gotos(&mut self) {
        if self.definition_in_flight {
            self.lsp_state.lock().unwrap().cancel_goto();
            self.definition_in_flight = false;
        }
        self.pending_goto = None;
        self.impl_picker = None;
        self.def_close();
    }

    /// Drop every go-to the TAB asked for and has not landed yet. One the
    /// editor asked for is left alone: the user pressed F12 there, and it lands
    /// as a new page like any other.
    pub(super) fn def_cancel_tab_goto(&mut self) {
        if self.definition_in_flight && self.lsp_asker.definition == GotoOrigin::DefinitionTab {
            self.lsp_state.lock().unwrap().cancel_goto();
            self.definition_in_flight = false;
        }
        if self
            .pending_goto
            .as_ref()
            .is_some_and(|p| p.origin == GotoOrigin::DefinitionTab)
        {
            self.pending_goto = None;
        }
        if self
            .impl_picker
            .as_ref()
            .is_some_and(|p| p.origin == GotoOrigin::DefinitionTab)
        {
            self.impl_picker = None;
        }
    }

    /// F12 / Ctrl+F12 / Ctrl+Click in the tab, at 0-based `line` and UTF-16
    /// column `col` of the page on screen.
    ///
    /// `anchor` is where a chooser would open (under the caret) and `caret_line`
    /// the source line, which ranks the chooser's rows — the same two things the
    /// editor captures at its own keypress, for the same reason: the answer
    /// lands frames later, when neither can be recovered.
    pub(super) fn def_request_from_tab(
        &mut self,
        line: u32,
        col: u32,
        implementation: bool,
        anchor: egui::Pos2,
        caret_line: String,
    ) {
        let Some(view) = &self.definition_view else {
            return;
        };
        let uri = if view.uri.is_empty() {
            lsp::path_to_uri(std::path::Path::new(&view.path))
        } else {
            view.uri.clone()
        };
        // A new question replaces every older one, from any origin. An older
        // parked request left alive would go out after this one and take the
        // single answer slot from it.
        self.impl_picker = None;
        self.pending_goto = None;
        self.definition_caret_line = caret_line;
        self.definition_anchor = anchor;
        // `indexed`, not the editor's `indexed || is_file_open`: this file is
        // never opened, and before the crate graph is loaded rust-analyzer
        // answers "file not found" for it.
        let (ready, indexed) = {
            let lsp = self.lsp_state.lock().unwrap();
            (matches!(lsp.status, lsp::LspStatus::Ready), lsp.indexed)
        };
        if ready && indexed {
            let sent =
                self.lsp_state
                    .lock()
                    .unwrap()
                    .request_goto_at_uri(&uri, line, col, implementation);
            self.lsp_asker.definition = GotoOrigin::DefinitionTab;
            self.definition_in_flight = sent;
            if !sent {
                self.set_status_msg(format!(
                    "{} Go to definition: the analyzer is not reachable",
                    ph::X_CIRCLE
                ));
            }
        } else {
            // Park it; the frame loop starts or waits for the analyzer and
            // re-issues it (`poll_pending_goto`), with the usual spinner.
            self.pending_goto = Some(PendingGoto {
                doc: GotoDoc::External { uri },
                line,
                col,
                implementation,
                since: std::time::Instant::now(),
                restart_fired: false,
                origin: GotoOrigin::DefinitionTab,
            });
        }
    }

    /// Say where a project-file answer the tab asked for went. Only a status
    /// note: `goto_definition_target` has already opened it in the MAIN editor
    /// (the tab has none of its own), and the page and its history stay — a
    /// file of the project is one step of the walk, not the end of it.
    pub(super) fn def_note_opened_in_editor(&mut self, path: &str, line: u32) {
        self.set_status_msg(format!(
            "{} Opened {}:{} in the editor",
            ph::ARROW_LEFT,
            super::short_path(path),
            line + 1
        ));
    }
}

impl GotoOrigin {
    /// The editor a project-file answer opens in.
    pub(crate) fn editor(self) -> EditorSlot {
        match self {
            GotoOrigin::Editor(slot) => slot,
            GotoOrigin::DefinitionTab | GotoOrigin::Timing => EditorSlot::Main,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_jump_abandons_the_pages_ahead() {
        let mut h = DefHistory::default();
        h.push("a"); // on screen: b
        h.push("b"); // on screen: c
        assert_eq!(h.take(Step::Back), Some("b"));
        h.record(Step::Back, "c"); // on screen: b, ahead: c
        assert_eq!(h.counts(), (1, 1));
        h.push("b"); // a new jump from b
        assert_eq!(h.counts(), (2, 0), "Forward must be gone");
    }

    #[test]
    fn back_then_forward_returns_to_the_same_page() {
        let mut h = DefHistory::default();
        h.push("a"); // on screen: b
        let back = h.take(Step::Back).unwrap();
        h.record(Step::Back, "b");
        assert_eq!(back, "a");
        let fwd = h.take(Step::Forward).unwrap();
        h.record(Step::Forward, back);
        assert_eq!(fwd, "b");
        assert_eq!(h.counts(), (1, 0));
    }

    /// An entry that no longer opens is taken but never recorded: it is gone,
    /// and the page on screen keeps its place.
    #[test]
    fn a_failed_step_drops_only_the_unreadable_entry() {
        let mut h = DefHistory::default();
        h.push("gone");
        h.push("kept");
        assert_eq!(h.take(Step::Back), Some("kept"));
        // Building "kept" failed: nothing recorded.
        assert_eq!(h.counts(), (1, 0));
        assert_eq!(h.peek(Step::Back), Some(&"gone"));
    }

    #[test]
    fn the_history_is_capped_at_the_oldest_end() {
        let mut h = DefHistory::default();
        for i in 0..DEF_HISTORY_CAP + 7 {
            h.push(i);
        }
        assert_eq!(h.counts(), (DEF_HISTORY_CAP, 0));
        assert_eq!(h.peek(Step::Back), Some(&(DEF_HISTORY_CAP + 6)));
        // The oldest seven went.
        let mut oldest = None;
        while let Some(x) = h.take(Step::Back) {
            oldest = Some(x);
        }
        assert_eq!(oldest, Some(7));
    }

    #[test]
    fn forward_steps_respect_the_cap_too() {
        let mut h = DefHistory::default();
        for i in 0..DEF_HISTORY_CAP {
            h.push(i);
        }
        let t = h.take(Step::Back).unwrap();
        h.record(Step::Back, 999);
        let _ = h.take(Step::Forward);
        h.record(Step::Forward, t);
        assert_eq!(h.counts().0, DEF_HISTORY_CAP);
    }

    #[test]
    fn line_ranges_follow_str_lines() {
        for text in ["", "a", "a\n", "a\r\nb", "\n\nx\n", "ăș\r\nț"] {
            let got: Vec<&str> = line_ranges(text)
                .iter()
                .map(|&(a, b)| &text[a..b])
                .collect();
            let want: Vec<&str> = text.lines().collect();
            assert_eq!(got, want, "{text:?}");
        }
    }

    /// Positions go to rust-analyzer in UTF-16 units: a byte column returned
    /// nothing when measured. Diacritics are one unit, an emoji two.
    #[test]
    fn columns_convert_to_and_from_utf16() {
        let line = "let ș = 😀x;";
        let x = line.chars().position(|c| c == 'x').unwrap();
        assert_eq!(utf16_col(line, x), x as u32 + 1, "the emoji counts twice");
        assert_eq!(char_of_utf16(line, utf16_col(line, x)), x);
        assert_eq!(char_of_utf16("abc", 99), 3, "past the end lands on the end");
    }

    #[test]
    fn a_click_just_past_a_name_still_finds_it() {
        let line = "self.capacity()";
        assert_eq!(ident_near(line, 5), Some((5, 13)), "on the name");
        assert_eq!(ident_near(line, 13), Some((5, 13)), "right after it");
        assert_eq!(ident_near(line, 14), None, "between the parentheses");
        assert_eq!(ident_near("a + b", 2), None);
    }

    #[test]
    fn the_same_page_is_recognised_across_spellings() {
        assert!(same_place(r"c:\x\vec.rs", 7, "C:/x/vec.rs", 7));
        assert!(!same_place(r"c:\x\vec.rs", 7, r"c:\x\vec.rs", 8));
        assert!(!same_place(r"c:\x\vec.rs", 7, r"c:\x\map.rs", 7));
    }

    #[test]
    fn a_miss_from_the_tab_is_explained_by_its_real_cause() {
        let r = |e, ready, idx| external_silence_reason(e, ready, idx).unwrap();
        assert!(r(Some("file not found: C:\\x\\vec.rs"), true, true).contains("dependencies"));
        assert!(r(Some("content modified"), true, true).contains("content modified"));
        assert!(r(None, false, false).contains("not running"));
        assert!(r(None, true, false).contains("indexing"));
        assert!(r(None, true, true).contains("comments"));
    }
}
