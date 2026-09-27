//! Find / Replace for the code editor.
//!
//! A single bar above the editor with four modes, opened by:
//! - `Ctrl+F`        — find in the current file
//! - `Ctrl+H`        — replace in the current file
//! - `Ctrl+Shift+F`  — find across the whole project tree
//! - `Ctrl+Shift+H`  — replace across the whole project tree
//!
//! Matching is case-sensitive literal text. Find and Replace also run on `Enter`.
//! In-file find selects the current match in the editor and scrolls to it (via
//! `pending_select` applied after the editor renders, + `pending_scroll_to_line`);
//! project find lists every hit and clicking one opens that file at the line.

use crate::app::{AppIde, ProjectFileId};
use crate::editor::gui::text_pos::GalleyRows;
use eframe::egui;
use egui_phosphor::regular as ph;
use std::sync::Arc;

/// Which of the four search/replace modes the bar is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FindMode {
    #[default]
    FindFile,
    ReplaceFile,
    FindProject,
    ReplaceProject,
}

impl FindMode {
    fn is_replace(self) -> bool {
        matches!(self, FindMode::ReplaceFile | FindMode::ReplaceProject)
    }
    fn is_project(self) -> bool {
        matches!(self, FindMode::FindProject | FindMode::ReplaceProject)
    }
    fn title(self) -> &'static str {
        match self {
            FindMode::FindFile => "Find in file",
            FindMode::ReplaceFile => "Replace in file",
            FindMode::FindProject => "Find in project",
            FindMode::ReplaceProject => "Replace in project",
        }
    }
}

/// One project-wide search hit (at most one per line).
pub struct ProjectMatch {
    pub file: ProjectFileId,
    pub path: String,
    pub line: usize, // 1-based
    pub preview: String,
}

/// Find/Replace bar state, stored on `AppIde`.
#[derive(Default)]
pub struct FindReplace {
    pub open: bool,
    pub mode: FindMode,
    pub query: String,
    pub replace: String,
    /// Request focus on the query field next frame.
    focus_query: bool,
    /// Request focus on the replace field next frame — set when a Replace mode
    /// is opened pre-filled with the identifier under the cursor, so the user
    /// edits the new name straight away.
    focus_replace: bool,
    /// Current in-file match index.
    current: usize,
    /// Status text shown in the bar (`3/12`, `No results`, `Replaced 5`, …).
    status: String,
    /// Project-wide search results.
    results: Vec<ProjectMatch>,
    /// `(start, end)` char range to select in the editor after it renders.
    pub pending_select: Option<(usize, usize)>,
    /// Set only while the results list holds occurrences a rename could not
    /// reach: the name to rename them TO. Drives the "Rename these too" button,
    /// so that action can never appear for an ordinary search.
    leftover_new_name: Option<String>,
    /// One of the bar's text fields had keyboard focus when the bar last
    /// rendered. Feeds the editor panel's keyboard-scope gate: the bar is part
    /// of the MAIN editor's editing scope, so Ctrl+F / Ctrl+Shift+F must keep
    /// working while typing a query.
    pub had_focus: bool,
    /// The in-file matches, shared by the bar and the painter (see
    /// [`FindReplace::match_starts_in`]).
    matches: MatchCache,
}

/// [`match_starts`] of `query` in `text`, kept until either differs.
#[derive(Default)]
struct MatchCache {
    query: String,
    text: String,
    starts: Arc<[usize]>,
}

impl FindReplace {
    /// Can F3 / Shift+F3 step a match right now?
    ///
    /// Exactly the condition under which the Previous / Next buttons are drawn:
    /// an open bar in a single-file find. Replace and project-wide search have
    /// no match cursor to step, and a closed bar has nothing at all — the
    /// caller checks this BEFORE consuming the key, so F3 stays available to
    /// whatever else might want it rather than being silently swallowed.
    pub(super) fn can_step(&self) -> bool {
        self.open && !self.mode.is_replace() && !self.mode.is_project()
    }

    /// Show occurrences a rename left behind, as a normal project-search result
    /// list the user can click through.
    ///
    /// Reuses this bar rather than inventing a notice widget: the leftovers are
    /// exactly a "find in project" result, and they need to be NAVIGABLE — a
    /// toast saying "2 occurrences remain" would make the user hunt for them.
    /// Focus is deliberately NOT taken, so the warning can't swallow the next
    /// keystroke.
    pub fn show_rename_leftovers(
        &mut self,
        old_name: &str,
        new_name: &str,
        results: Vec<ProjectMatch>,
    ) {
        self.open = true;
        self.mode = FindMode::FindProject;
        self.query = old_name.to_owned();
        self.focus_query = false;
        self.focus_replace = false;
        self.current = 0;
        self.status = format!(
            "{} rust-analyzer could not reach",
            match results.len() {
                1 => "1 occurrence".to_string(),
                n => format!("{n} occurrences"),
            }
        );
        self.results = results;
        self.leftover_new_name = Some(new_name.to_owned());
    }

    /// The pending leftover rename target, if the results list is showing one.
    fn leftover_target(&self) -> Option<&str> {
        self.leftover_new_name.as_deref()
    }

    /// Open (or re-target) the bar in `mode`, focusing the query field.
    pub fn open_with(&mut self, mode: FindMode) {
        self.leftover_new_name = None;
        self.open = true;
        self.mode = mode;
        self.focus_query = true;
        self.focus_replace = false;
        self.current = 0;
        self.status.clear();
        self.results.clear();
    }

    /// Open a Replace mode pre-filled with `word` (the identifier under the
    /// cursor): the find field searches for it, the replace field starts from
    /// it (edit to the new name), and focus goes to the replace field. When
    /// `word` is empty this is just [`open_with`].
    pub fn open_replace_with_word(&mut self, mode: FindMode, word: &str) {
        self.open_with(mode);
        if !word.is_empty() {
            self.query = word.to_owned();
            self.replace = word.to_owned();
            self.focus_query = false;
            self.focus_replace = true;
        }
    }

    /// [`match_starts`] of the query in `text`, over the whole text.
    ///
    /// The bar (for the `i/N` status and F3) and the painter both need it every
    /// frame. Kept against a copy of the query and the text, so the scan reruns
    /// only when one of them changed: a hit costs one compare of each.
    fn match_starts_in(&mut self, text: &str) -> Arc<[usize]> {
        if self.query.is_empty() {
            // No scan and no copy of the text: an empty query matches nothing.
            return Arc::default();
        }
        let cache = &mut self.matches;
        if cache.query != self.query || cache.text != text {
            cache.starts = match_starts(text, &self.query).into();
            cache.query.clone_from(&self.query);
            cache.text.clear();
            cache.text.push_str(text);
        }
        Arc::clone(&cache.starts)
    }
}

/// Non-overlapping char-index start positions of `query` in `text`
/// (case-sensitive, literal).
fn match_starts(text: &str, query: &str) -> Vec<usize> {
    let q: Vec<char> = query.chars().collect();
    let m = q.len();
    if m == 0 {
        return Vec::new();
    }
    let t: Vec<char> = text.chars().collect();
    let n = t.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + m <= n {
        if t[i..i + m] == q[..] {
            out.push(i);
            i += m;
        } else {
            i += 1;
        }
    }
    out
}

/// 1-based line number of char index `idx` in `text`.
fn line_of(text: &str, idx: usize) -> usize {
    text.chars().take(idx).filter(|&c| c == '\n').count() + 1
}

/// The rects [`AppIde::paint_find_matches`] fills: `(index into starts, rect)`
/// for each match of length `wl` that is on screen.
///
/// `starts` is sorted, so the matches whose row can meet the clip are one run,
/// found by binary search; only those are positioned and tested.
fn for_each_visible_match(
    rows: &GalleyRows,
    gp: egui::Pos2,
    clip: egui::Rect,
    starts: &[usize],
    wl: usize,
    mut paint: impl FnMut(usize, egui::Rect),
) {
    let Some(span) = rows.chars_meeting_band(gp.y, clip.top(), clip.bottom()) else {
        return;
    };
    let from = starts.partition_point(|&s| s < *span.start());
    let to = starts.partition_point(|&s| s <= *span.end());
    for (idx, &start) in starts.iter().enumerate().take(to).skip(from) {
        let loc_s = rows.pos(start);
        let loc_e = rows.pos(start + wl);
        let y_top = gp.y + loc_s.min.y;
        let y_bot = gp.y + loc_s.max.y;
        let same_row = (loc_s.min.y - loc_e.min.y).abs() < (y_bot - y_top).max(1.0) * 0.5;
        let x_l = gp.x + loc_s.min.x;
        let x_r = if same_row {
            gp.x + loc_e.min.x
        } else {
            gp.x + rows.galley().rect.width()
        };
        if y_bot >= clip.top() && y_top <= clip.bottom() && x_r > x_l {
            paint(
                idx,
                egui::Rect::from_min_max(egui::pos2(x_l, y_top), egui::pos2(x_r, y_bot)),
            );
        }
    }
}

impl AppIde {
    /// Render the Find/Replace bar (when open) above the editor. Mutates
    /// `display_code` in place for in-file replace; project replace updates the
    /// underlying file buffers (and re-syncs `display_code` so the editor's
    /// write-back doesn't revert the current file).
    /// `key_next` / `key_prev` are F3 / Shift+F3, consumed by the caller — the
    /// only place that knows whether THIS editor owns the keyboard. They do
    /// exactly what the Previous / Next buttons do, and the caller has already
    /// checked [`FindReplace::can_step`] before taking the key.
    pub(super) fn show_find_replace_bar(
        &mut self,
        ui: &mut egui::Ui,
        display_code: &mut String,
        displayed_file: ProjectFileId,
        key_next: bool,
        key_prev: bool,
    ) {
        if !self.ed.find.open {
            self.ed.find.had_focus = false;
            return;
        }
        // Esc closes the bar.
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.ed.find.open = false;
            self.ed.find.had_focus = false;
            return;
        }
        // Re-observed from this frame's widgets below.
        self.ed.find.had_focus = false;

        let mode = self.ed.find.mode;
        let mut do_next = key_next;
        let mut do_prev = key_prev;
        let mut do_search = false;
        let mut do_replace_all = false;
        let mut do_leftover_rename = false;
        let mut query_changed = false;
        let mut close = false;
        let mut clicked_result: Option<usize> = None;
        // Enter runs Replace All only on a FIRST press. The find field takes
        // the focus back after Enter, so a held Enter's key repeats reach it
        // again, and a replacement that contains the query (`value` →
        // `value2`) would grow at the repeat rate. Find still steps on repeats,
        // as F3 does.
        let enter_first_press = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed: true,
                        repeat: false,
                        ..
                    }
                )
            })
        });

        let frame = egui::Frame::new()
            .fill(egui::Color32::from_rgb(40, 40, 47))
            .inner_margin(egui::Margin::same(6))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(70, 70, 82),
            ));

        frame.show(ui, |ui| {
            // ── Row 1: title + query + nav + status + close ──
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{}  {}", ph::MAGNIFYING_GLASS, mode.title()))
                        .size(11.0)
                        .color(egui::Color32::from_rgb(160, 170, 190)),
                );
                let q = ui.add(
                    egui::TextEdit::singleline(&mut self.ed.find.query)
                        .desired_width(230.0)
                        .hint_text("find"),
                );
                if self.ed.find.focus_query {
                    q.request_focus();
                    self.ed.find.focus_query = false;
                }
                self.ed.find.had_focus |= q.has_focus();
                query_changed = q.changed();
                let enter = crate::app::helpers::text_field::ended_with_enter(ui, &q);

                if !mode.is_replace() && !mode.is_project() {
                    if ui
                        .button(ph::ARROW_UP)
                        .on_hover_text("Previous (Shift+F3)")
                        .clicked()
                    {
                        do_prev = true;
                    }
                    if ui
                        .button(ph::ARROW_DOWN)
                        .on_hover_text("Next (F3)")
                        .clicked()
                    {
                        do_next = true;
                    }
                }
                if mode.is_project() && ui.button("Search").clicked() {
                    do_search = true;
                }

                // Enter runs the mode's primary action.
                if enter {
                    match mode {
                        FindMode::FindFile => do_next = true,
                        FindMode::FindProject => do_search = true,
                        FindMode::ReplaceFile | FindMode::ReplaceProject => {
                            do_replace_all = enter_first_press
                        }
                    }
                    self.ed.find.focus_query = true; // keep focus for repeated Enter
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(ph::X).on_hover_text("Close (Esc)").clicked() {
                        close = true;
                    }
                    if !self.ed.find.status.is_empty() {
                        ui.label(
                            egui::RichText::new(&self.ed.find.status)
                                .size(11.0)
                                .color(egui::Color32::from_rgb(150, 160, 175)),
                        );
                    }
                });
            });

            // ── Row 2: replacement field + Replace All (replace modes) ──
            if mode.is_replace() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{}  with", ph::ARROW_BEND_DOWN_RIGHT))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(160, 170, 190)),
                    );
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.ed.find.replace)
                            .desired_width(230.0)
                            .hint_text("replace with"),
                    );
                    if self.ed.find.focus_replace {
                        r.request_focus();
                        self.ed.find.focus_replace = false;
                    }
                    self.ed.find.had_focus |= r.has_focus();
                    let renter = crate::app::helpers::text_field::ended_with_enter(ui, &r);
                    if ui.button("Replace All").clicked() || renter {
                        do_replace_all = true;
                    }
                });
            }

            // ── Leftover-rename row ──
            // Shown ONLY after a rename that rust-analyzer could not complete.
            // The action is textual, so it stays behind an explicit click with
            // the affected lines listed right below it — never automatic.
            if let Some(new_name) = self.ed.find.leftover_target().map(str::to_owned) {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(
                        egui::RichText::new(format!("{} Rename incomplete", ph::WARNING))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(230, 180, 90)),
                    );
                    if ui
                        .button(format!("Rename these to \"{new_name}\""))
                        .on_hover_text(
                            "rust-analyzer cannot see inside a const-generic argument \
                             (Parser::<.., { X::y() }>) — it reports no reference there, so \
                             these were left behind.\n\nThis replaces the whole word on the \
                             lines listed below. Review them first: a line may hold a \
                             DIFFERENT symbol that happens to share the name.",
                        )
                        .clicked()
                    {
                        do_leftover_rename = true;
                    }
                    ui.label(
                        egui::RichText::new("review the lines first")
                            .size(10.0)
                            .color(egui::Color32::from_gray(140))
                            .italics(),
                    );
                });
            }

            // ── Results list (project modes) ──
            if mode.is_project() && !self.ed.find.results.is_empty() {
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(190.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (idx, m) in self.ed.find.results.iter().enumerate() {
                            let text = format!("{}:{}  {}", m.path, m.line, m.preview);
                            if ui
                                .add(
                                    egui::Label::new(
                                        egui::RichText::new(text).size(11.0).monospace(),
                                    )
                                    .sense(egui::Sense::click())
                                    .truncate(),
                                )
                                .on_hover_text("Open")
                                .clicked()
                            {
                                clicked_result = Some(idx);
                            }
                        }
                    });
            }
        });
        ui.add_space(4.0);

        if close {
            self.ed.find.open = false;
            return;
        }

        // ── Apply actions (outside the closures to avoid borrow tangles) ──
        if let Some(idx) = clicked_result {
            if let Some(m) = self.ed.find.results.get(idx) {
                let (file, line) = (m.file, m.line);
                self.selected_file = file;
                self.ed.pending_scroll_to_line = Some((file, line));
            }
        }

        if do_leftover_rename {
            self.apply_leftover_rename(displayed_file, display_code);
            return;
        }

        match mode {
            FindMode::FindProject => {
                if do_search {
                    self.run_project_search();
                }
            }
            FindMode::ReplaceProject => {
                if do_replace_all {
                    self.run_project_replace(displayed_file, display_code);
                }
            }
            FindMode::ReplaceFile => {
                if do_replace_all && !self.ed.find.query.is_empty() {
                    let count = display_code.matches(self.ed.find.query.as_str()).count();
                    *display_code = display_code
                        .replace(self.ed.find.query.as_str(), self.ed.find.replace.as_str());
                    self.ed.find.status = format!("Replaced {count}");
                }
            }
            FindMode::FindFile => {
                let starts = self.ed.find.match_starts_in(display_code);
                if starts.is_empty() {
                    self.ed.find.status = if self.ed.find.query.is_empty() {
                        String::new()
                    } else {
                        "No results".to_string()
                    };
                } else {
                    if do_next {
                        self.ed.find.current = (self.ed.find.current + 1) % starts.len();
                    } else if do_prev {
                        self.ed.find.current =
                            (self.ed.find.current + starts.len() - 1) % starts.len();
                    } else if query_changed {
                        self.ed.find.current = 0;
                    }
                    self.ed.find.current = self.ed.find.current.min(starts.len() - 1);
                    if do_next || do_prev || query_changed {
                        let start = starts[self.ed.find.current];
                        let end = start + self.ed.find.query.chars().count();
                        self.ed.find.pending_select = Some((start, end));
                        self.ed.pending_scroll_to_line =
                            Some((displayed_file, line_of(display_code, start)));
                    }
                    self.ed.find.status = format!("{}/{}", self.ed.find.current + 1, starts.len());
                }
            }
        }
    }

    /// Overlay-highlight every occurrence of the active find query in the
    /// currently-shown file, so matches stay visible even while the find field
    /// (not the editor) holds focus. The current in-file match is emphasised in
    /// amber; the rest are translucent cyan. Painted after the editor, like
    /// [`AppIde::highlight_selected_word`].
    ///
    /// `rows` must describe `editor_resp.galley`. The matches (and so the `i/N`
    /// colouring) are still found over the whole text; only the positioning
    /// is limited to the rows on screen.
    pub(super) fn paint_find_matches(
        &mut self,
        editor_resp: &egui::text_edit::TextEditOutput,
        rows: &GalleyRows,
        display_code: &str,
        clip: egui::Rect,
        ui: &egui::Ui,
    ) {
        if !self.ed.find.open || self.ed.find.query.is_empty() {
            return;
        }
        let starts = self.ed.find.match_starts_in(display_code);
        if starts.is_empty() {
            return;
        }
        let wl = self.ed.find.query.chars().count();
        let file_find = matches!(
            self.ed.find.mode,
            FindMode::FindFile | FindMode::ReplaceFile
        );
        let cur_idx = self.ed.find.current.min(starts.len() - 1);
        let base = egui::Color32::from_rgba_unmultiplied(52, 232, 235, 45);
        let current = egui::Color32::from_rgba_unmultiplied(255, 200, 60, 96);

        let painter = ui.painter().with_clip_rect(clip);
        let gp = editor_resp.galley_pos;
        for_each_visible_match(rows, gp, clip, &starts, wl, |idx, rect| {
            let color = if file_find && idx == cur_idx {
                current
            } else {
                base
            };
            painter.rect_filled(rect, 2.0, color);
        });
    }

    /// Every file the project search/replace spans: `(id, display_path, content)`.
    /// Config files that don't apply to the toolchain (empty `memory.x` /
    /// `build.rs` for ESP) are skipped.
    /// Audit a just-finished rename: every project file is scanned for the OLD
    /// name as a whole word, and anything still there is surfaced in the find
    /// bar. Nothing is edited — see [`super::rename::whole_word_lines`] for why
    /// this must stay advisory.
    pub(in crate::app) fn report_rename_leftovers(&mut self, old_name: &str, new_name: &str) {
        if old_name.is_empty() || new_name.is_empty() || old_name == new_name {
            return;
        }
        let mut results = Vec::new();
        for (id, path, content) in self.searchable_files() {
            let lines: Vec<&str> = content.lines().collect();
            for line_no in super::rename::whole_word_lines(&content, old_name) {
                results.push(ProjectMatch {
                    file: id,
                    path: path.clone(),
                    line: line_no,
                    preview: lines
                        .get(line_no - 1)
                        .map(|l| l.trim().chars().take(140).collect())
                        .unwrap_or_default(),
                });
            }
        }
        if !results.is_empty() {
            self.ed
                .find
                .show_rename_leftovers(old_name, new_name, results);
        }
    }

    fn searchable_files(&self) -> Vec<(ProjectFileId, String, String)> {
        let mut v = vec![(
            ProjectFileId::MainRs,
            "src/main.rs".to_string(),
            self.generated_code.clone(),
        )];
        for (i, (name, content)) in self.project_tree.user_src_files.iter().enumerate() {
            v.push((ProjectFileId::UserFile(i), name.clone(), content.clone()));
        }
        v.push((
            ProjectFileId::CargoToml,
            "Cargo.toml".into(),
            self.cargo_toml.clone(),
        ));
        v.push((
            ProjectFileId::CargoConfig,
            ".cargo/config.toml".into(),
            self.cargo_config.clone(),
        ));
        if !self.memory_x.is_empty() {
            v.push((
                ProjectFileId::MemoryX,
                "memory.x".into(),
                self.memory_x.clone(),
            ));
        }
        if !self.build_rs.is_empty() {
            v.push((
                ProjectFileId::BuildRs,
                "build.rs".into(),
                self.build_rs.clone(),
            ));
        }
        v.push((
            ProjectFileId::GitIgnore,
            ".gitignore".into(),
            self.gitignore.clone(),
        ));
        v
    }

    /// Rename the leftover occurrences the user just approved — only the lines
    /// currently listed in the results, only as whole words.
    ///
    /// `display_code` is re-synced afterwards for the same reason
    /// [`Self::run_project_replace`] does it: the editor writes its buffer back
    /// at end of frame and would otherwise revert the edit in the open file.
    fn apply_leftover_rename(&mut self, displayed_file: ProjectFileId, display_code: &mut String) {
        let Some(new_name) = self.ed.find.leftover_new_name.clone() else {
            return;
        };
        let old_name = self.ed.find.query.clone();

        // Group the approved lines per file before editing anything.
        let mut per_file: Vec<(ProjectFileId, Vec<usize>)> = Vec::new();
        for m in &self.ed.find.results {
            match per_file.iter_mut().find(|(id, _)| *id == m.file) {
                Some((_, lines)) => lines.push(m.line),
                None => per_file.push((m.file, vec![m.line])),
            }
        }

        let mut total = 0usize;
        let mut files = 0usize;
        for (id, lines) in per_file {
            let content = self.searchable_content(id);
            let (updated, n) =
                super::rename::replace_whole_word_on_lines(&content, &old_name, &new_name, &lines);
            if n > 0 {
                self.set_searchable_content(id, updated);
                total += n;
                files += 1;
            }
        }

        if displayed_file != ProjectFileId::MainRs || total > 0 {
            *display_code = self.searchable_content(displayed_file);
        }

        self.ed.find.results.clear();
        self.ed.find.leftover_new_name = None;
        self.ed.find.query = new_name;
        self.ed.find.status = format!("Renamed {total} leftover(s) in {files} file(s)");
    }

    /// In-memory content of a searchable file by id.
    fn searchable_content(&self, id: ProjectFileId) -> String {
        match id {
            ProjectFileId::MainRs => self.generated_code.clone(),
            ProjectFileId::CargoToml => self.cargo_toml.clone(),
            ProjectFileId::CargoConfig => self.cargo_config.clone(),
            ProjectFileId::MemoryX => self.memory_x.clone(),
            ProjectFileId::BuildRs => self.build_rs.clone(),
            ProjectFileId::GitIgnore => self.gitignore.clone(),
            ProjectFileId::UserFile(i) => self
                .project_tree
                .user_src_files
                .get(i)
                .map(|(_, c)| c.clone())
                .unwrap_or_default(),
        }
    }

    /// Overwrite a searchable file's content by id.
    fn set_searchable_content(&mut self, id: ProjectFileId, content: String) {
        match id {
            ProjectFileId::MainRs => self.generated_code = content,
            ProjectFileId::CargoToml => self.cargo_toml = content,
            ProjectFileId::CargoConfig => self.cargo_config = content,
            ProjectFileId::MemoryX => self.memory_x = content,
            ProjectFileId::BuildRs => self.build_rs = content,
            ProjectFileId::GitIgnore => self.gitignore = content,
            ProjectFileId::UserFile(i) => {
                if let Some(e) = self.project_tree.user_src_files.get_mut(i) {
                    e.1 = content;
                }
            }
        }
    }

    /// Populate `self.ed.find.results` with every line in the project containing the
    /// query (one hit per line; capped to keep the list responsive).
    fn run_project_search(&mut self) {
        self.ed.find.results.clear();
        let query = self.ed.find.query.clone();
        if query.is_empty() {
            self.ed.find.status.clear();
            return;
        }
        const CAP: usize = 1000;
        let mut capped = false;
        for (id, path, content) in self.searchable_files() {
            for (n, line) in content.lines().enumerate() {
                if line.contains(&query) {
                    self.ed.find.results.push(ProjectMatch {
                        file: id,
                        path: path.clone(),
                        line: n + 1,
                        preview: line.trim().chars().take(140).collect(),
                    });
                    if self.ed.find.results.len() >= CAP {
                        capped = true;
                        break;
                    }
                }
            }
            if capped {
                break;
            }
        }
        let n = self.ed.find.results.len();
        self.ed.find.status = match n {
            0 => "No results".to_string(),
            _ if capped => format!("{n}+ matches"),
            _ => format!("{n} matches"),
        };
    }

    /// Replace every occurrence of the query across all project files. Re-syncs
    /// `display_code` to the (possibly edited) current file so the editor's
    /// write-back doesn't revert it.
    fn run_project_replace(&mut self, displayed_file: ProjectFileId, display_code: &mut String) {
        let query = self.ed.find.query.clone();
        let replacement = self.ed.find.replace.clone();
        if query.is_empty() {
            return;
        }
        let mut total = 0usize;
        let mut files = 0usize;
        for (id, _, content) in self.searchable_files() {
            let count = content.matches(query.as_str()).count();
            if count > 0 {
                self.set_searchable_content(
                    id,
                    content.replace(query.as_str(), replacement.as_str()),
                );
                total += count;
                files += 1;
            }
        }
        *display_code = self.searchable_content(displayed_file);
        self.ed.find.results.clear();
        self.ed.find.status = format!("Replaced {total} in {files} file(s)");
    }
}

#[cfg(test)]
mod tests {
    use super::{FindMode, FindReplace, line_of, match_starts};

    fn bar(open: bool, mode: FindMode) -> FindReplace {
        FindReplace {
            open,
            mode,
            ..FindReplace::default()
        }
    }

    /// F3 steps a match, so it is only live where there IS a match cursor —
    /// exactly where the Previous / Next buttons are drawn.
    #[test]
    fn f3_steps_only_in_an_open_single_file_find() {
        assert!(bar(true, FindMode::FindFile).can_step());
    }

    /// A closed bar has nothing to step. The caller checks this BEFORE
    /// consuming the key, so F3 is not silently swallowed when it can do
    /// nothing.
    #[test]
    fn a_closed_bar_does_not_take_the_key() {
        assert!(!bar(false, FindMode::FindFile).can_step());
    }

    /// Replace and project-wide search have no match cursor — and no buttons.
    #[test]
    fn the_modes_without_nav_buttons_do_not_take_the_key() {
        for mode in [
            FindMode::ReplaceFile,
            FindMode::FindProject,
            FindMode::ReplaceProject,
        ] {
            assert!(!bar(true, mode).can_step(), "{mode:?}");
        }
    }

    #[test]
    fn finds_non_overlapping_matches() {
        assert_eq!(match_starts("ababab", "ab"), vec![0, 2, 4]);
        assert_eq!(match_starts("aaaa", "aa"), vec![0, 2]); // non-overlapping
        assert_eq!(match_starts("xyz", "q"), Vec::<usize>::new());
        assert_eq!(match_starts("abc", ""), Vec::<usize>::new());
    }

    #[test]
    fn match_starts_are_char_indices() {
        // A multi-byte char before the match: the index is in chars, not bytes.
        let starts = match_starts("é foo foo", "foo");
        assert_eq!(starts, vec![2, 6]);
    }

    #[test]
    fn line_numbers_are_one_based() {
        let text = "a\nbb\nccc";
        assert_eq!(line_of(text, 0), 1);
        assert_eq!(line_of(text, 2), 2); // first char of line 2
        assert_eq!(line_of(text, 5), 3);
    }

    /// The cached list is always what a fresh scan returns, and a repeated
    /// question with the same query and text reuses it.
    #[test]
    fn cached_match_starts_follow_the_query_and_the_text() {
        let mut find = FindReplace::default();
        let steps = [
            ("foo", "foo bar foo"),
            ("foo", "foo bar foo"),
            ("foo", "foo bar fo"),
            ("bar", "foo bar fo"),
            ("", "foo bar fo"),
            ("o", "é foo"),
            ("o", "é foo"),
            ("aa", "aaaa"),
        ];
        let mut prev: Option<std::sync::Arc<[usize]>> = None;
        let mut prev_key = ("", "");
        for (query, text) in steps {
            find.query = query.to_owned();
            let got = find.match_starts_in(text);
            assert_eq!(
                &got[..],
                &match_starts(text, query)[..],
                "{query:?} in {text:?}"
            );
            if let Some(p) = &prev
                && prev_key == (query, text)
                && !query.is_empty()
            {
                assert!(
                    std::sync::Arc::ptr_eq(p, &got),
                    "{query:?} in {text:?} rescanned"
                );
            }
            prev = Some(got);
            prev_key = (query, text);
        }
    }

    /// An empty query neither scans nor keeps a copy of the text.
    #[test]
    fn an_empty_query_keeps_no_text() {
        let mut find = FindReplace::default();
        assert!(find.match_starts_in("some text").is_empty());
        assert!(find.matches.text.is_empty());
    }

    /// What the painter filled before it was limited to the rows on screen:
    /// every match positioned with the galley's own walk, then culled.
    fn old_rects(
        galley: &egui::Galley,
        gp: egui::Pos2,
        clip: egui::Rect,
        starts: &[usize],
        wl: usize,
    ) -> Vec<(usize, egui::Rect)> {
        let mut out = Vec::new();
        for (idx, &start) in starts.iter().enumerate() {
            let loc_s = galley.pos_from_cursor(egui::text::CCursor::new(start));
            let loc_e = galley.pos_from_cursor(egui::text::CCursor::new(start + wl));
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
                out.push((
                    idx,
                    egui::Rect::from_min_max(egui::pos2(x_l, y_top), egui::pos2(x_r, y_bot)),
                ));
            }
        }
        out
    }

    /// Same rects, same indices (so the same one is amber), for texts the galley
    /// was laid out from and for texts it is an edit behind.
    #[test]
    fn visible_matches_paint_exactly_what_the_whole_file_pass_painted() {
        use crate::editor::gui::text_pos::{GalleyRows, galley_rows_tests::galleys};
        let mut compared = 0usize;
        for (text, galley) in galleys() {
            let rows = GalleyRows::new(&galley);
            let h = galley.rect.height();
            let bands = [(-50.0, h + 50.0), (0.0, 14.0), (h * 0.5, h), (h, h)];
            let variants = [
                text.clone(),
                format!("a\n{text}"),
                text.chars().skip(1).collect(),
                format!("{text}aa a"),
            ];
            for shown in &variants {
                for query in ["a", "aa", "a\n", "😀", "Z a", "ă€"] {
                    let starts = match_starts(shown, query);
                    let wl = query.chars().count();
                    for gp in [egui::pos2(0.0, 0.0), egui::pos2(12.5, -20.0)] {
                        for (top, bottom) in bands {
                            let clip = egui::Rect::from_min_max(
                                egui::pos2(0.0, top),
                                egui::pos2(400.0, bottom),
                            );
                            let mut got = Vec::new();
                            super::for_each_visible_match(&rows, gp, clip, &starts, wl, |i, r| {
                                got.push((i, r))
                            });
                            let want = old_rects(&galley, gp, clip, &starts, wl);
                            assert_eq!(got, want, "{query:?} in {shown:?} laid out {text:?}");
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
