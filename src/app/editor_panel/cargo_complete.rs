//! Cargo.toml dependency completion.
//!
//! `Ctrl+Space` inside a dependency table suggests embedded-relevant crates;
//! after a crate is chosen (`name = ""`), it suggests that crate's available
//! versions; and inside `features = [ … ]` it suggests that crate's features.
//!
//! Crate names come in two groups. The curated, **offline** set leads — a raw
//! crates.io search is far too noisy for an embedded IDE to be the only list.
//! Below it, a **live** crates.io search ([`super::crate_search`]) keeps only
//! names containing what was typed. Without that second group a crate outside
//! the curated set could not be completed at all, however exact its name.
//!
//! Versions AND features come from one **live** fetch of the crates.io sparse
//! index in a background thread — features are per-version and sit in the same
//! entry, so a second request would be pure waste.

use crate::app::{AppIde, ProjectFileId};
use eframe::egui;
use egui::text_edit::TextEditOutput;
use std::sync::{Arc, Mutex};

/// One suggestion row in the Cargo completion popup.
#[derive(Clone)]
pub(crate) struct CargoItem {
    pub label: String,
    pub detail: String,
    pub action: CargoAccept,
    /// Came from the live crates.io search rather than the curated list; the
    /// popup draws a divider above the first such row.
    pub from_search: bool,
}

/// The one line the popup shows besides its rows.
#[derive(Debug, PartialEq)]
pub(crate) enum PopupNote {
    /// Waiting on crates.io; the text says for what.
    Loading(&'static str),
    /// Nothing went wrong, but there is something to say — typically why the
    /// list is empty. Without it an empty answer closes the popup silently,
    /// which is indistinguishable from `Ctrl+Space` being broken.
    Info(String),
    Error(String),
}

/// What happens when an item is accepted.
#[derive(Clone)]
pub(crate) enum CargoAccept {
    /// Insert `<name> = ""` and start fetching that crate's versions.
    CrateName(String),
    /// Insert the chosen version string (the cursor is already inside quotes).
    Version(String),
    /// Insert a feature name into a `features = [ … ]` array. `quoted` is true
    /// when the caret is already inside a pair of quotes, so only the bare name
    /// goes in; otherwise the quotes are inserted too.
    Feature { name: String, quoted: bool },
}

/// What one crate's sparse-index entry gives us. Both completions are served
/// from the SAME fetch: features are per-version and already sit next to the
/// version numbers in the index, so asking twice would be pure waste.
pub(crate) struct IndexData {
    /// The crate's name exactly as published, read from the entry itself. It can
    /// differ from the name that was asked for — in case, or in `-` vs `_` once
    /// [`fetch_versions`] has followed crates.io to the published spelling — and
    /// Cargo accepts only this one.
    pub name: Option<String>,
    /// Non-yanked versions, newest first.
    pub versions: Vec<String>,
    /// Version -> its user-facing feature names.
    pub features: std::collections::BTreeMap<String, Vec<String>>,
}

/// Async result of fetching one crate's entry from the sparse index.
pub(crate) enum VersionFetch {
    Loading,
    Done(IndexData),
    Error(String),
}

/// Per-`AppIde` state for the Cargo.toml completion popup. Mirrors the LSP
/// completion fields but is entirely self-contained (no rust-analyzer).
#[derive(Default)]
pub(crate) struct CargoCompleteState {
    /// True while the popup is visible.
    pub open: bool,
    /// Highlighted row.
    pub sel: usize,
    /// Items shown in the last rendered frame — keyboard accept reads these so
    /// it always acts on exactly what the user sees.
    pub items: Vec<CargoItem>,
    /// Accept deferred from a mouse click (applied at the top of next frame).
    pub pending: Option<CargoAccept>,
    /// The manifest the popup belongs to. There is more than one now (the
    /// firmware's and every extracted library's), and this state is per-`AppIde`
    /// — without it a pending accept would be applied to whichever manifest
    /// happens to be open next frame, at an offset that means nothing there.
    pub for_file: Option<crate::app::ProjectFileId>,
    /// The crate whose versions are currently being fetched / cached.
    pub version_crate: String,
    /// Shared background fetch result for `version_crate`.
    pub version_fetch: Option<Arc<Mutex<VersionFetch>>>,
    /// Live crates.io name search: answers by query, debounce and spacing.
    pub search: super::crate_search::CrateSearch,
    /// Index of the line the popup was opened on. The popup is a question about
    /// THAT line: once the caret is on another one it closes. A line NUMBER,
    /// not a char offset — an extra caret typing above would move the offset
    /// on every keystroke and close the popup for no reason.
    /// Without this a popup showing only a note ("searching…") let Enter or an
    /// arrow through to the editor and rode along to the next line, where it
    /// filled with rows and took the NEXT Enter as an accept — inserting a
    /// crate nobody picked, or splicing one into the middle of a line.
    pub anchor_line: Option<usize>,
    /// The user moved the highlight with the arrows since the list opened.
    /// Until then row 0 is simply "the best match"; after, it is a choice.
    pub moved: bool,
}

/// Where the cursor sits inside a Cargo.toml dependency table.
#[derive(Debug, PartialEq)]
pub(crate) enum CargoCtx {
    /// Typing a crate name (key) — replace `[start..cursor]` with the picked crate.
    Name { start: usize, prefix: String },
    /// Typing a version value for `crate_name` — replace `[start..cursor]`.
    Version {
        crate_name: String,
        start: usize,
        prefix: String,
    },
    /// Typing inside `features = [ … ]` for `crate_name`.
    Feature {
        crate_name: String,
        /// The version requirement written in the manifest, if any — features
        /// differ between releases, so the list is taken from the version the
        /// project actually uses rather than always the newest.
        version_req: Option<String>,
        /// Feature names already in the array; never suggested again.
        already: Vec<String>,
        /// The caret sits between quotes (insert the bare name).
        quoted: bool,
        /// `workspace = true`: the crate and its real name live in the root's
        /// `[workspace.dependencies]`, so the key here may legitimately differ
        /// from the published spelling.
        inherited: bool,
        start: usize,
        prefix: String,
    },
}

impl AppIde {
    /// Drive the Cargo.toml completion popup: apply a pending accept, react to
    /// Ctrl+Space, then (re)filter and render. Called instead of the LSP
    /// completion handler when the open file is `Cargo.toml`.
    pub(super) fn handle_cargo_completion(
        &mut self,
        ui: &mut egui::Ui,
        editor_resp: &TextEditOutput,
        display_code: &mut String,
        displayed_file: ProjectFileId,
        ctrl_space_pressed: bool,
    ) {
        let cursor_char_idx = editor_resp
            .state
            .cursor
            .char_range()
            .map(|r| r.primary.index.0);

        // ── 0. Different manifest than the popup was opened for? ──────────────
        // Offsets and the item list belong to the file they were computed in;
        // carrying them into another manifest would splice text at a position
        // that means nothing there.
        //
        // `displayed_file`, NOT `self.selected_file`: in the Reference view the
        // two differ, and keying on the main editor's file wrote a library's
        // manifest, completion included, over the firmware's Cargo.toml.
        if self.ed.cargo_complete.for_file != Some(displayed_file) {
            self.ed.cargo_complete.open = false;
            self.ed.cargo_complete.pending = None;
            self.ed.cargo_complete.items.clear();
            self.ed.cargo_complete.sel = 0;
            self.ed.cargo_complete.anchor_line = None;
            self.ed.cargo_complete.for_file = Some(displayed_file);
        }

        // ── 1. Apply a pending accept (keyboard or mouse) ─────────────────────
        if let Some(accept) = self.ed.cargo_complete.pending.take() {
            if let Some(cur) = cursor_char_idx {
                self.apply_cargo_accept(ui, editor_resp, display_code, displayed_file, cur, accept);
            }
            // The text and caret just changed; the cursor index from this frame
            // is stale against the new text. Let the next frame (which sees the
            // re-positioned caret) recompute context and render the follow-up
            // version popup.
            return;
        }

        // ── 2. Ctrl+Space → (re)open at the current context ───────────────────
        if ctrl_space_pressed {
            if let Some(cur) = cursor_char_idx {
                if let Some(ctx) = cargo_context(display_code, cur) {
                    self.open_cargo_popup(&ctx);
                    self.ed.cargo_complete.anchor_line = Some(line_index_of(display_code, cur));
                } else {
                    self.ed.cargo_complete.open = false;
                }
            }
        }

        // ── 3. Render (filter against the live prefix) ────────────────────────
        if !self.ed.cargo_complete.open {
            return;
        }
        let Some(cur) = cursor_char_idx else {
            self.ed.cargo_complete.open = false;
            return;
        };
        let line = line_index_of(display_code, cur);
        if *self.ed.cargo_complete.anchor_line.get_or_insert(line) != line {
            // The caret moved to another line: the question was about that one.
            self.ed.cargo_complete.open = false;
            return;
        }
        let Some(ctx) = cargo_context(display_code, cur) else {
            // Cursor left a completable position.
            self.ed.cargo_complete.open = false;
            return;
        };

        // Rebuild the visible item list from the current context.
        let (items, note) = self.cargo_items_for(&ctx);
        let sel = keep_selection(
            &self.ed.cargo_complete.items,
            self.ed.cargo_complete.sel,
            self.ed.cargo_complete.moved,
            &items,
        );
        self.ed.cargo_complete.items = items.clone();

        if items.is_empty() && note.is_none() {
            // Nothing matches and nothing to say about it — drop the popup.
            self.ed.cargo_complete.open = false;
            return;
        }
        self.ed.cargo_complete.sel = sel;

        self.render_cargo_popup(ui, editor_resp, &items, note);
    }

    /// Configure the popup for a freshly detected context.
    fn open_cargo_popup(&mut self, ctx: &CargoCtx) {
        self.ed.cargo_complete.open = true;
        self.ed.cargo_complete.sel = 0;
        self.ed.cargo_complete.moved = false;
        // A fresh question: last popup's rows must not steer the selection.
        self.ed.cargo_complete.items.clear();
        match ctx {
            CargoCtx::Version { crate_name, .. } | CargoCtx::Feature { crate_name, .. } => {
                self.ensure_version_fetch(crate_name)
            }
            // An explicit Ctrl+Space is the user asking again — a search that
            // failed offline gets another chance.
            CargoCtx::Name { .. } => self.ed.cargo_complete.search.forget_errors(),
        }
    }

    /// Build the filtered item list for the current context, plus the one note
    /// the popup shows beside it (loading, empty-because, or error).
    fn cargo_items_for(&mut self, ctx: &CargoCtx) -> (Vec<CargoItem>, Option<PopupNote>) {
        match ctx {
            CargoCtx::Name { prefix, .. } => self.crate_name_rows(prefix),
            CargoCtx::Version {
                crate_name, prefix, ..
            } => {
                self.ensure_version_fetch(crate_name);
                let guard = self
                    .ed
                    .cargo_complete
                    .version_fetch
                    .as_ref()
                    .map(|a| a.lock().unwrap());
                match guard.as_deref() {
                    Some(VersionFetch::Loading) | None => (
                        Vec::new(),
                        Some(PopupNote::Loading("crates.io — fetching versions…")),
                    ),
                    Some(VersionFetch::Error(e)) => (
                        Vec::new(),
                        Some(PopupNote::Error(format!("crates.io: {e}"))),
                    ),
                    Some(VersionFetch::Done(data)) => {
                        if let Some(note) = spelling_note(data, crate_name) {
                            return (Vec::new(), Some(note));
                        }
                        let pl = prefix.to_lowercase();
                        let items = data
                            .versions
                            .iter()
                            .filter(|v| pl.is_empty() || v.to_lowercase().starts_with(&pl))
                            .take(60)
                            .enumerate()
                            .map(|(i, v)| CargoItem {
                                label: v.clone(),
                                detail: if i == 0 {
                                    "latest".into()
                                } else {
                                    String::new()
                                },
                                action: CargoAccept::Version(v.clone()),
                                from_search: false,
                            })
                            .collect();
                        (items, None)
                    }
                }
            }
            CargoCtx::Feature {
                crate_name,
                version_req,
                already,
                prefix,
                quoted,
                inherited,
                ..
            } => {
                self.ensure_version_fetch(crate_name);
                let guard = self
                    .ed
                    .cargo_complete
                    .version_fetch
                    .as_ref()
                    .map(|a| a.lock().unwrap());
                match guard.as_deref() {
                    Some(VersionFetch::Loading) | None => (
                        Vec::new(),
                        Some(PopupNote::Loading("crates.io — fetching features…")),
                    ),
                    Some(VersionFetch::Error(e)) => (
                        Vec::new(),
                        Some(PopupNote::Error(format!("crates.io: {e}"))),
                    ),
                    Some(VersionFetch::Done(data)) => {
                        // An inherited entry's key is not a claim about the
                        // spelling — the root manifest may rename it.
                        if let Some(note) = spelling_note(data, crate_name).filter(|_| !inherited) {
                            return (Vec::new(), Some(note));
                        }
                        let ver = pick_version(&data.versions, version_req.as_deref());
                        let names = ver
                            .and_then(|v| data.features.get(v))
                            .cloned()
                            .unwrap_or_default();
                        (filter_features(&names, prefix, already, *quoted), None)
                    }
                }
            }
        }
    }

    /// Crate-name rows: the curated list, then live crates.io hits below it.
    fn crate_name_rows(&mut self, prefix: &str) -> (Vec<CargoItem>, Option<PopupNote>) {
        let mut items = filter_crates(prefix);
        let (view, fire) = self
            .ed
            .cargo_complete
            .search
            .view(prefix, std::time::Instant::now());
        if let Some((query, slot)) = fire {
            std::thread::spawn(move || super::crate_search::run(&query, &slot));
        }
        let live = live_rows(&view.hits, prefix, &items);
        merge_name_rows(&mut items, live, prefix);

        let note = if view.pending {
            Some(PopupNote::Loading("crates.io — searching…"))
        } else if let Some(e) = view.error {
            Some(PopupNote::Error(format!("crates.io search: {e}")))
        } else {
            name_note(prefix, items.is_empty(), view.truncated)
        };
        (items, note)
    }

    /// Start a background fetch of `name`'s versions if not already cached.
    fn ensure_version_fetch(&mut self, name: &str) {
        if self.ed.cargo_complete.version_crate == name
            && self.ed.cargo_complete.version_fetch.is_some()
        {
            return;
        }
        let shared = Arc::new(Mutex::new(VersionFetch::Loading));
        self.ed.cargo_complete.version_crate = name.to_string();
        self.ed.cargo_complete.version_fetch = Some(shared.clone());
        let name = name.to_string();
        std::thread::spawn(move || {
            let result = match fetch_versions(&name) {
                Ok(d) if d.versions.is_empty() => {
                    VersionFetch::Error("no published versions".into())
                }
                Ok(d) => VersionFetch::Done(d),
                Err(e) => VersionFetch::Error(e),
            };
            *shared.lock().unwrap() = result;
        });
    }

    /// Apply an accepted suggestion to the editor text + cursor.
    fn apply_cargo_accept(
        &mut self,
        ui: &mut egui::Ui,
        editor_resp: &TextEditOutput,
        display_code: &mut String,
        displayed_file: ProjectFileId,
        cursor: usize,
        accept: CargoAccept,
    ) {
        let Some(ctx) = cargo_context(display_code, cursor) else {
            self.ed.cargo_complete.open = false;
            return;
        };
        let chars: Vec<char> = display_code.chars().collect();
        // `cargo_context` clamps the cursor; the splices below index with it
        // directly, so clamp here too or a caret left over from a longer text
        // panics with "range start index N out of range". (It really happened:
        // the popup state is shared across manifests of different lengths.)
        let cursor = cursor.min(chars.len());
        match accept {
            CargoAccept::CrateName(name) => {
                if let CargoCtx::Name { start, .. } = ctx {
                    let start = start.min(cursor);
                    let insert = format!("{name} = \"\"");
                    let before: String = chars[..start].iter().collect();
                    let after: String = chars[cursor..].iter().collect();
                    *display_code = format!("{before}{insert}{after}");
                    // Cursor between the two quotes (just before the closing one).
                    let new_cursor = start + insert.chars().count() - 1;
                    self.store_cargo_cursor(ui, editor_resp, new_cursor);
                    self.persist_cargo_toml(displayed_file, display_code);
                    // Switch straight to version completion for this crate.
                    self.ensure_version_fetch(&name);
                    self.ed.cargo_complete.open = true;
                    self.ed.cargo_complete.sel = 0;
                    self.ed.cargo_complete.moved = false;
                    self.ed.cargo_complete.items.clear();
                    ui.ctx().request_repaint();
                }
            }
            CargoAccept::Feature { name, quoted } => {
                if let CargoCtx::Feature { start, .. } = ctx {
                    let start = start.min(cursor);
                    // Outside quotes (`features = [|]`) the quotes come with it.
                    let insert = if quoted {
                        name.clone()
                    } else {
                        format!("\"{name}\"")
                    };
                    let before: String = chars[..start].iter().collect();
                    let after: String = chars[cursor..].iter().collect();
                    *display_code = format!("{before}{insert}{after}");
                    let new_cursor = start + insert.chars().count();
                    self.store_cargo_cursor(ui, editor_resp, new_cursor);
                    self.persist_cargo_toml(displayed_file, display_code);
                    self.ed.cargo_complete.open = false;
                }
            }
            CargoAccept::Version(ver) => {
                if let CargoCtx::Version { start, .. } = ctx {
                    let start = start.min(cursor);
                    let before: String = chars[..start].iter().collect();
                    let after: String = chars[cursor..].iter().collect();
                    *display_code = format!("{before}{ver}{after}");
                    let new_cursor = start + ver.chars().count();
                    self.store_cargo_cursor(ui, editor_resp, new_cursor);
                    self.persist_cargo_toml(displayed_file, display_code);
                    self.ed.cargo_complete.open = false;
                }
            }
        }
    }

    /// Persist the edited manifest text into its backing store, mirroring the
    /// editor write-back.
    ///
    /// Needed because the write-back runs EARLIER in the frame than the
    /// completion, so an accepted suggestion would otherwise be dropped. A
    /// library crate's manifest is an ordinary user file — handling only
    /// `CargoToml` here silently lost every completion accepted there.
    ///
    /// Keyed on the file the VIEW shows: the Reference view can show a library's
    /// manifest while the main editor holds another file entirely.
    fn persist_cargo_toml(&mut self, file: ProjectFileId, display_code: &str) {
        match file {
            ProjectFileId::CargoToml => self.cargo_toml = display_code.to_owned(),
            ProjectFileId::UserFile(i) => {
                if let Some(entry) = self.project_tree.user_src_files.get_mut(i) {
                    entry.1 = display_code.to_owned();
                }
            }
            _ => {}
        }
    }

    /// Re-position the editor caret for next frame (the widget already rendered).
    fn store_cargo_cursor(&self, ui: &mut egui::Ui, editor_resp: &TextEditOutput, char_idx: usize) {
        let mut st = editor_resp.state.clone();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(char_idx),
        )));
        st.store(ui.ctx(), editor_resp.response.id);
    }

    /// Render the popup. Mouse clicks set `cargo_complete.pending` (applied next
    /// frame, same path as keyboard accept).
    fn render_cargo_popup(
        &mut self,
        ui: &mut egui::Ui,
        editor_resp: &TextEditOutput,
        items: &[CargoItem],
        note: Option<PopupNote>,
    ) {
        let popup_pos = if let Some(char_range) = editor_resp.state.cursor.char_range() {
            let idx = char_range.primary.index.0;
            let count = editor_resp.galley.job.text.chars().count();
            let clamped = idx.min(count.saturating_sub(1));
            let local = editor_resp
                .galley
                .pos_from_cursor(egui::text::CCursor::new(clamped));
            editor_resp.response.rect.left_top()
                + local.min.to_vec2()
                + egui::vec2(0.0, local.height() + 2.0)
        } else {
            editor_resp.response.rect.left_top()
        };

        let sel = self.ed.cargo_complete.sel;
        egui::Area::new(egui::Id::new("cargo_completion_popup"))
            .fixed_pos(popup_pos)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(&ui.ctx().global_style()).show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.set_min_width(360.0);
                    ui.set_max_width(360.0);

                    if !items.is_empty() {
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| self.cargo_popup_rows(ui, items, sel));
                    }

                    match &note {
                        None => {}
                        Some(PopupNote::Loading(text)) => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(
                                    egui::RichText::new(format!("  {text}"))
                                        .size(11.5)
                                        .color(egui::Color32::from_rgb(160, 175, 200)),
                                );
                            });
                            ui.ctx().request_repaint();
                        }
                        Some(PopupNote::Info(text)) => {
                            ui.label(
                                egui::RichText::new(text)
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(140, 155, 180)),
                            );
                        }
                        Some(PopupNote::Error(text)) => {
                            ui.label(
                                egui::RichText::new(text)
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(210, 120, 110)),
                            );
                        }
                    }
                });
            });
    }

    /// The selectable rows, with a divider above the first live crates.io row.
    fn cargo_popup_rows(&mut self, ui: &mut egui::Ui, items: &[CargoItem], sel: usize) {
        for (i, item) in items.iter().enumerate() {
            if item.from_search && (i == 0 || !items[i - 1].from_search) {
                if i > 0 {
                    ui.separator();
                }
                ui.label(
                    egui::RichText::new("crates.io")
                        .size(10.5)
                        .color(egui::Color32::from_rgb(120, 140, 165)),
                );
            }
            let selected = i == sel;
            let fg = if selected {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_rgb(200, 210, 230)
            };
            let sel_bg = egui::Color32::from_rgb(40, 90, 160);
            let hover_bg = egui::Color32::from_rgb(50, 60, 80);
            let detail_fg = if selected {
                egui::Color32::from_rgb(160, 195, 255)
            } else {
                egui::Color32::from_rgb(110, 130, 155)
            };

            let row_h = 19.0;
            let avail_w = ui.available_width();
            let (rect, row_resp) =
                ui.allocate_exact_size(egui::vec2(avail_w, row_h), egui::Sense::click());
            if selected {
                ui.painter().rect_filled(rect, 2.0, sel_bg);
            } else if row_resp.hovered() {
                ui.painter().rect_filled(rect, 2.0, hover_bg);
            }
            let painter = ui.painter();
            let label =
                painter.layout_no_wrap(item.label.clone(), egui::FontId::monospace(12.0), fg);
            let label_w = label.size().x;
            painter.galley(
                rect.left_center() + egui::vec2(4.0, -label.size().y / 2.0),
                label,
                fg,
            );
            if !item.detail.is_empty() {
                // A crates.io name can be long (`hmmd_mmwave_sensor_async`), so
                // the detail gets what the label leaves, never a fixed width
                // that would paint over the name.
                let font = egui::FontId::monospace(10.5);
                let char_w = painter
                    .layout_no_wrap("0".into(), font.clone(), detail_fg)
                    .size()
                    .x
                    .max(1.0);
                let room = rect.width() - 8.0 - label_w - 16.0;
                let max_chars = ((room / char_w).floor().max(0.0) as usize).min(44);
                if let Some(det) = fit_detail(&item.detail, max_chars) {
                    painter.text(
                        rect.right_center() - egui::vec2(4.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        det,
                        font,
                        detail_fg,
                    );
                }
            }
            if row_resp.clicked() {
                self.ed.cargo_complete.pending = Some(item.action.clone());
                self.ed.cargo_complete.open = false;
            }
            if selected {
                row_resp.scroll_to_me(None);
            }
        }
    }
}

// ── Pure helpers (tested) ─────────────────────────────────────────────────────

/// Detect what the cursor (char index) is positioned to complete in Cargo.toml.
pub(crate) fn cargo_context(text: &str, cursor: usize) -> Option<CargoCtx> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());

    // Current line bounds (char indices).
    let line_start = chars[..cursor]
        .iter()
        .rposition(|&c| c == '\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let line_end = chars[cursor..]
        .iter()
        .position(|&c| c == '\n')
        .map(|p| cursor + p)
        .unwrap_or(chars.len());
    let line: String = chars[line_start..line_end].iter().collect();
    let col = cursor - line_start; // char offset within the line

    // Skip comments / section headers themselves.
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with('[') {
        return None;
    }

    // Which dependency table are we in?
    let section = current_section(&chars, line_start)?;

    let line_chars: Vec<char> = line.chars().collect();
    let first_eq = line_chars.iter().position(|&c| c == '=');

    // A `features = [ … ]` array wins over everything else: it can span lines,
    // so it is checked against the whole text before the line-local rules.
    if let Some(ctx) = feature_ctx(&chars, cursor, &section) {
        return Some(ctx);
    }

    match first_eq {
        // No '=' yet, or cursor is on the key side → crate-name completion.
        Some(eq) if col <= eq => name_ctx(&section, &line_chars, line_start, col),
        None => name_ctx(&section, &line_chars, line_start, col),
        // Cursor on the value side → version completion.
        Some(eq) => match version_ctx(&section, &line_chars, line_start, col, eq)? {
            CargoCtx::Version {
                crate_name,
                start,
                prefix,
            } => Some(CargoCtx::Version {
                crate_name: crates_io_name(&entry_text(&chars, &section, line_start), crate_name)?,
                start,
                prefix,
            }),
            other => Some(other),
        },
    }
}

/// The name to look `key`'s entry up under on crates.io, or `None` when
/// crates.io is not where it comes from.
///
/// The KEY is only the local name. `embedded_io = { package = "embedded-io" }`
/// is a crate called `embedded-io`, and looking the key up instead reported a
/// perfectly valid line as misspelled. `registry = "…"` names another registry
/// altogether, whose crates crates.io knows nothing about.
fn crates_io_name(entry: &str, key: String) -> Option<String> {
    if field_token(entry, "registry").is_some() || field_token(entry, "registry-index").is_some() {
        return None;
    }
    Some(field_of(entry, "package").unwrap_or(key))
}

/// Is the caret inside a `features = [ … ]` array, and for which crate?
///
/// Scans BACKWARDS for an unclosed `[`, which is what makes the multi-line form
/// (`features = [\n  "a",\n  "b",\n]`, the usual shape under
/// `[dependencies.foo]`) work as well as the inline one. The walk stops at a
/// table header, so a stray bracket earlier in the file cannot drag the search
/// out of the current entry.
fn feature_ctx(chars: &[char], cursor: usize, section: &DepSection) -> Option<CargoCtx> {
    let open = unclosed_bracket(chars, cursor)?;
    // The key right before the `[` must be `features`.
    let mut i = open;
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    if i == 0 || chars[i - 1] != '=' {
        return None;
    }
    i -= 1;
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    let key_end = i;
    while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_' || chars[i - 1] == '-') {
        i -= 1;
    }
    if chars[i..key_end].iter().collect::<String>() != "features" {
        return None;
    }

    // Which crate does this array belong to?
    let key_line_start = chars[..i]
        .iter()
        .rposition(|&c| c == '\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let crate_name = match section {
        DepSection::Entry(name) => name.clone(),
        // Inline table: `foo = { …, features = [ … ] }` — the crate is the key
        // before the line's FIRST `=`.
        DepSection::Table => {
            let line_end = chars[key_line_start..]
                .iter()
                .position(|&c| c == '\n')
                .map(|p| key_line_start + p)
                .unwrap_or(chars.len());
            let eq = chars[key_line_start..line_end]
                .iter()
                .position(|&c| c == '=')?;
            chars[key_line_start..key_line_start + eq]
                .iter()
                .collect::<String>()
                .trim()
                .to_string()
        }
    };
    if crate_name.is_empty() {
        return None;
    }

    // A path / git dependency has no index entry — nothing to suggest, and the
    // "crate not found" error a lookup would produce reads like a defect.
    let entry = entry_text(chars, section, key_line_start);
    if field_of(&entry, "path").is_some() || field_of(&entry, "git").is_some() {
        return None;
    }
    let crate_name = crates_io_name(&entry, crate_name)?;

    // Everything already in the array, and where the current word starts.
    let already = quoted_strings(&chars[open + 1..cursor]);
    let region = &chars[open + 1..cursor];
    let quoted = region.iter().filter(|&&c| c == '"').count() % 2 == 1;
    let start = if quoted {
        open + 2 + region.iter().rposition(|&c| c == '"').unwrap()
    } else {
        // Between elements: the word begins after the last `[` `,` or space.
        open + 1
            + region
                .iter()
                .rposition(|&c| c == ',' || c.is_whitespace())
                .map(|p| p + 1)
                .unwrap_or(0)
    };
    let prefix: String = chars[start.min(cursor)..cursor].iter().collect();
    // Anything else in there (a nested array, an `=`) means this is not a plain
    // list of feature names.
    if !prefix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }

    Some(CargoCtx::Feature {
        crate_name,
        version_req: field_of(&entry, "version"),
        already,
        quoted,
        inherited: field_token(&entry, "workspace").is_some_and(|(v, q)| !q && v == "true"),
        start: start.min(cursor),
        prefix,
    })
}

/// Index of the `[` that is still open at `cursor`, searching back to the
/// nearest table header.
fn unclosed_bracket(chars: &[char], cursor: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = cursor;
    while i > 0 {
        i -= 1;
        match chars[i] {
            ']' => depth += 1,
            '[' => {
                if depth == 0 {
                    // A `[` at the start of its line is a table header, not an
                    // array — the search has left the entry.
                    let at_line_start = chars[..i]
                        .iter()
                        .rev()
                        .take_while(|&&c| c != '\n')
                        .all(|c| c.is_whitespace());
                    return (!at_line_start).then_some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// The text of the dependency entry that owns `key_line_start`: the line after
/// its key for an inline table, or the table body up to the next header for
/// `[dependencies.foo]`.
///
/// Without the key: a crate may well be CALLED `registry` or `package`, and
/// its own name must not read as that field.
fn entry_text(chars: &[char], section: &DepSection, key_line_start: usize) -> String {
    match section {
        DepSection::Table => {
            let end = chars[key_line_start..]
                .iter()
                .position(|&c| c == '\n')
                .map(|p| key_line_start + p)
                .unwrap_or(chars.len());
            let line = &chars[key_line_start..end];
            let value = line.iter().position(|&c| c == '=').map_or(0, |eq| eq + 1);
            line[value..].iter().collect()
        }
        DepSection::Entry(_) => {
            // The table's header is the last line STARTING with `[` — not merely
            // the last `[`, which inside `[dependencies.foo]` is as likely the
            // `features = [` array above the caret, and would hide every field
            // (`package`, `path`, …) written before that array.
            let before: String = chars[..key_line_start.min(chars.len())].iter().collect();
            let mut header = 0;
            let mut offset = 0;
            for l in before.split_inclusive('\n') {
                if l.trim_start().starts_with('[') {
                    header = offset;
                }
                offset += l.chars().count();
            }
            let text: String = chars[header..].iter().collect();
            // Up to the next table header.
            let mut out = String::new();
            for (n, l) in text.lines().enumerate() {
                if n > 0 && l.trim_start().starts_with('[') {
                    break;
                }
                out.push_str(l);
                out.push('\n');
            }
            out
        }
    }
}

/// The value of field `key` inside a dependency entry, and whether it was a
/// quoted string: `version = "1"` is `("1", true)`, `workspace = true` is
/// `("true", false)`.
///
/// A field starts only where a field CAN start — the beginning of the entry or
/// a line, or right after `{` or `,`. Any looser and a dependency KEY reads as
/// a field: `windows-registry = "0.5"` held a `registry = "0.5"`, which took
/// version and feature completion away from `signal-hook-registry`,
/// `oid-registry` and every other `*-registry` crate.
fn field_token(entry: &str, key: &str) -> Option<(String, bool)> {
    let chars: Vec<char> = entry.chars().collect();
    let key: Vec<char> = key.chars().collect();
    let blank = |c: &char| *c == ' ' || *c == '\t';
    for i in 0..chars.len() {
        if !chars[i..].starts_with(&key) {
            continue;
        }
        let prev = chars[..i].iter().rev().find(|c| !blank(c));
        if !matches!(prev, None | Some('{') | Some(',') | Some('\n')) {
            continue;
        }
        let mut j = i + key.len();
        while chars.get(j).is_some_and(blank) {
            j += 1;
        }
        if chars.get(j) != Some(&'=') {
            continue;
        }
        j += 1;
        while chars.get(j).is_some_and(blank) {
            j += 1;
        }
        return match chars.get(j) {
            // Basic `"…"` and literal `'…'` strings are both valid TOML. Taking
            // "the next `\"`" instead read `package = 'x', version = "1"` as a
            // crate called `1`.
            Some(&q) if q == '"' || q == '\'' => {
                let body: String = chars[j + 1..].iter().take_while(|&&c| c != q).collect();
                Some((body, true))
            }
            _ => {
                let raw: String = chars[j..]
                    .iter()
                    // `#` too: `workspace = true  # renamed in the root` is `true`.
                    .take_while(|&&c| c != ',' && c != '}' && c != '\n' && c != '#')
                    .collect();
                Some((raw.trim().to_owned(), false))
            }
        };
    }
    None
}

/// The string value of `key = "…"` inside a dependency entry.
fn field_of(entry: &str, key: &str) -> Option<String> {
    field_token(entry, key).and_then(|(v, quoted)| quoted.then_some(v))
}

/// Every `"…"` string in `region`.
fn quoted_strings(region: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for &c in region {
        match (c, &mut cur) {
            ('"', None) => cur = Some(String::new()),
            ('"', Some(s)) => {
                out.push(std::mem::take(s));
                cur = None;
            }
            (c, Some(s)) => s.push(c),
            _ => {}
        }
    }
    out
}

/// The dependency table that encloses `line_start`, if any.
fn current_section(chars: &[char], line_start: usize) -> Option<DepSection> {
    let prefix: String = chars[..line_start].iter().collect();
    let header = prefix
        .lines()
        .rev()
        .map(|l| l.trim())
        .find(|l| l.starts_with('[') && l.ends_with(']'))?;
    let inner = header.trim_start_matches('[').trim_end_matches(']').trim();
    parse_dep_section(inner)
}

#[derive(Debug, PartialEq, Clone)]
enum DepSection {
    /// `[dependencies]` / `[dev-dependencies]` / `[target.'…'.dependencies]`.
    Table,
    /// `[dependencies.<crate>]` — only a version is completable here.
    Entry(String),
}

fn parse_dep_section(inner: &str) -> Option<DepSection> {
    const KINDS: [&str; 3] = ["dev-dependencies", "build-dependencies", "dependencies"];
    if KINDS.iter().any(|k| inner.ends_with(k)) {
        return Some(DepSection::Table);
    }
    for k in KINDS {
        let needle = format!("{k}.");
        if let Some(pos) = inner.rfind(&needle) {
            let name = inner[pos + needle.len()..].trim();
            if !name.is_empty() {
                return Some(DepSection::Entry(name.to_string()));
            }
        }
    }
    None
}

fn name_ctx(
    section: &DepSection,
    line: &[char],
    line_start: usize,
    col: usize,
) -> Option<CargoCtx> {
    // Crate names are only typed in a `[dependencies]`-style table.
    if *section != DepSection::Table {
        return None;
    }
    let typed: String = line[..col].iter().collect();
    let trimmed = typed.trim_start();
    let lead_ws = typed.chars().count() - trimmed.chars().count();
    // Must look like a crate-name prefix (letters/digits/-/_), possibly empty.
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(CargoCtx::Name {
        start: line_start + lead_ws,
        prefix: trimmed.to_string(),
    })
}

fn version_ctx(
    section: &DepSection,
    line: &[char],
    line_start: usize,
    col: usize,
    first_eq: usize,
) -> Option<CargoCtx> {
    // Last '=' before the cursor.
    let veq = line[..col].iter().rposition(|&c| c == '=')?;

    // The key immediately before that '=' (for inner table fields).
    let key_end = line[..veq].iter().rposition(|&c| !c.is_whitespace());
    let key: String = match key_end {
        Some(end) => {
            let key_start = line[..=end]
                .iter()
                .rposition(|&c| c.is_whitespace() || c == '{' || c == ',')
                .map(|p| p + 1)
                .unwrap_or(0);
            line[key_start..=end].iter().collect()
        }
        None => String::new(),
    };

    let crate_name = match section {
        DepSection::Entry(name) => {
            // Only the `version = "…"` field is a version here.
            if key != "version" {
                return None;
            }
            name.clone()
        }
        DepSection::Table => {
            if veq == first_eq {
                // `crate = "…"` simple form — key is the crate name.
                line[..first_eq]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_string()
            } else if key == "version" {
                // `crate = { version = "…" }` — outer key is the crate name.
                line[..first_eq]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_string()
            } else {
                // Some other inline field (features/path/git/…) — not a version.
                return None;
            }
        }
    };
    if crate_name.is_empty() {
        return None;
    }

    // We only complete inside an open quoted string: the count of '"' between
    // the value's '=' and the cursor must be odd (one unclosed opening quote).
    let region = &line[veq + 1..col];
    let quotes = region.iter().filter(|&&c| c == '"').count();
    if quotes % 2 == 0 {
        return None;
    }
    let open_q = veq + 1 + region.iter().rposition(|&c| c == '"').unwrap();
    let prefix: String = line[open_q + 1..col].iter().collect();
    Some(CargoCtx::Version {
        crate_name,
        start: line_start + open_q + 1,
        prefix,
    })
}

/// The published version whose features to show: the newest one matching what
/// the manifest asks for, else the newest overall.
///
/// A plain prefix match, not a semver resolver: `"0.2"` picks the newest
/// `0.2.x`, `"1"` the newest `1.x`. Leading `^ ~ = >=` and spaces are stripped.
/// Full requirement matching would mean pulling in a semver crate to change the
/// answer only for ranges nobody writes in a firmware manifest.
fn pick_version<'a>(versions: &'a [String], req: Option<&str>) -> Option<&'a String> {
    let req = req
        .map(|r| r.trim_start_matches(['^', '~', '=', '>', '<', ' ']).trim())
        .filter(|r| !r.is_empty());
    let Some(req) = req else {
        return versions.first();
    };
    versions
        .iter()
        .find(|v| *v == req || v.starts_with(&format!("{req}.")))
        .or_else(|| versions.first())
}

/// Feature rows for the popup: prefix matches first, then substring, with what
/// is already in the array removed — re-suggesting a feature you can see two
/// characters to the left is noise.
fn filter_features(
    names: &[String],
    prefix: &str,
    already: &[String],
    quoted: bool,
) -> Vec<CargoItem> {
    let p = prefix.to_lowercase();
    let mut starts: Vec<&String> = Vec::new();
    let mut contains: Vec<&String> = Vec::new();
    for n in names {
        if already.iter().any(|a| a == n) {
            continue;
        }
        let low = n.to_lowercase();
        if p.is_empty() || low.starts_with(&p) {
            starts.push(n);
        } else if low.contains(&p) {
            contains.push(n);
        }
    }
    starts
        .into_iter()
        .chain(contains)
        .take(60)
        .map(|n| CargoItem {
            label: n.clone(),
            detail: if n == "default" {
                "enabled unless default-features = false".into()
            } else {
                String::new()
            },
            action: CargoAccept::Feature {
                name: n.clone(),
                quoted,
            },
            from_search: false,
        })
        .collect()
}

/// Curated crates matching `prefix` (case-insensitive), best first: prefix
/// matches rank before substring matches, alphabetical within each.
///
/// Shared with the "Add dependency" code action, which offers the SAME list the
/// manifest's own `Ctrl+Space` does — one curated set, one ranking.
pub(super) fn crate_matches(prefix: &str) -> Vec<(&'static str, &'static str)> {
    let p = prefix.to_lowercase();
    let mut starts: Vec<&(&str, &str)> = Vec::new();
    let mut contains: Vec<&(&str, &str)> = Vec::new();
    for entry in EMBEDDED_CRATES {
        let name = entry.0.to_lowercase();
        if p.is_empty() || name.starts_with(&p) {
            starts.push(entry);
        } else if name.contains(&p) {
            contains.push(entry);
        }
    }
    starts
        .into_iter()
        .chain(contains)
        .take(80)
        .map(|(n, d)| (*n, *d))
        .collect()
}

/// Filter the curated embedded-crate list by `prefix` (case-insensitive).
fn filter_crates(prefix: &str) -> Vec<CargoItem> {
    crate_matches(prefix)
        .into_iter()
        .map(|(name, desc)| CargoItem {
            label: name.to_string(),
            detail: desc.to_string(),
            action: CargoAccept::CrateName(name.to_string()),
            from_search: false,
        })
        .collect()
}

/// Live crates.io rows for `prefix`, to go BELOW the curated `curated` rows.
///
/// Only names CONTAINING the prefix are kept (`-` and `_` read alike): the
/// search also matches descriptions and keywords, and `q=embassy` alone answers
/// 808 crates. Names the curated rows already show are dropped. Ranked exact
/// name, then name-prefix, then substring; most downloaded first within each.
/// The label is the name exactly as published — that is what gets inserted,
/// and Cargo accepts no other spelling.
fn live_rows(
    hits: &[super::crate_search::Hit],
    prefix: &str,
    curated: &[CargoItem],
) -> Vec<CargoItem> {
    use super::crate_search::normalize;
    let p = normalize(prefix);
    // Below the search threshold no query of its own was sent, and the cached
    // hits of every EARLIER query filtered by one or two letters would be
    // hundreds of unrelated crates.
    if p.chars().count() < super::crate_search::MIN_QUERY_CHARS {
        return Vec::new();
    }
    let shown: std::collections::HashSet<String> =
        curated.iter().map(|c| normalize(&c.label)).collect();
    let mut rows: Vec<(u8, &super::crate_search::Hit)> = hits
        .iter()
        .filter_map(|h| {
            let n = normalize(&h.name);
            if shown.contains(&n) {
                return None;
            }
            let rank = if n == p {
                0
            } else if n.starts_with(&p) {
                1
            } else if n.contains(&p) {
                2
            } else {
                return None;
            };
            Some((rank, h))
        })
        .collect();
    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(b.1.downloads.cmp(&a.1.downloads))
            .then_with(|| a.1.name.cmp(&b.1.name))
    });
    // No cap: a silent `take(40)` hid name matches with nothing on screen to
    // say so. Every row contains 3+ typed characters, so the list stays short
    // in practice — and it scrolls.
    rows.into_iter()
        .map(|(_, h)| CargoItem {
            label: h.name.clone(),
            detail: h.description.clone(),
            action: CargoAccept::CrateName(h.name.clone()),
            from_search: true,
        })
        .collect()
}

/// What to say under the crate-name rows once nothing is pending or failed:
/// why the list is empty, or that crates.io matched more than it listed.
///
/// Said out loud because the alternative — the popup vanishing — is exactly
/// what made a freshly published crate look like a broken `Ctrl+Space`. And a
/// truncated answer is flagged even with rows showing: a short word matches
/// hundreds of crates (`pca`: 390), and the wanted one may be among those not
/// fetched, so its absence must not read as "does not exist".
fn name_note(prefix: &str, empty: bool, truncated: Option<u64>) -> Option<PopupNote> {
    let min = super::crate_search::MIN_QUERY_CHARS;
    let n = prefix.chars().count();
    if n == 0 {
        return None;
    }
    if n < min {
        return empty
            .then(|| PopupNote::Info(format!("Type {min}+ characters to search crates.io")));
    }
    if let Some(total) = truncated {
        return Some(PopupNote::Info(format!(
            "{total} matches on crates.io, not all listed — type more"
        )));
    }
    // crates.io matches whole words: `hmm` does not find `hmmd_…`.
    empty.then(|| {
        PopupNote::Info(format!(
            "No crates.io match for `{prefix}` — try a whole word"
        ))
    })
}

/// Put the live rows under the curated ones — except an EXACT live name when no
/// curated row is exact: that one goes on top.
///
/// Typing all of `time` and pressing Enter must not give `embassy-time` just
/// because the curated list contains it as a substring and leads the popup.
fn merge_name_rows(items: &mut Vec<CargoItem>, mut live: Vec<CargoItem>, prefix: &str) {
    use super::crate_search::normalize;
    let p = normalize(prefix);
    let exact = |i: &CargoItem| !p.is_empty() && normalize(&i.label) == p;
    if !items.iter().any(exact) && live.first().is_some_and(exact) {
        items.insert(0, live.remove(0));
    }
    items.extend(live);
}

/// The row to highlight after the list was rebuilt.
///
/// Before the user moves, row 0 stays row 0: the top row is simply the best
/// match, whatever it now is. Once they have moved — back to row 0 included —
/// the highlight follows the CRATE, not the index — a crates.io answer landing mid-navigation re-sorts
/// the rows, and Enter would otherwise accept a crate the user never selected.
fn keep_selection(old: &[CargoItem], sel: usize, moved: bool, new: &[CargoItem]) -> usize {
    let last = new.len().saturating_sub(1);
    if !moved {
        return 0;
    }
    old.get(sel)
        .and_then(|o| {
            new.iter()
                .position(|n| n.label == o.label && n.from_search == o.from_search)
        })
        .unwrap_or(sel.min(last))
}

/// Zero-based number of the line holding char index `cursor`.
fn line_index_of(text: &str, cursor: usize) -> usize {
    text.chars().take(cursor).filter(|&c| c == '\n').count()
}

/// The error to show when the index answered for a DIFFERENT spelling than the
/// manifest wrote — offering its versions would look like success, and then
/// Cargo rejects the line: `no matching package named …`.
fn spelling_note(data: &IndexData, written: &str) -> Option<PopupNote> {
    let published = data.name.as_deref()?;
    (published != written).then(|| {
        PopupNote::Error(format!(
            "Published as `{published}` — Cargo accepts only that spelling"
        ))
    })
}

/// `detail` cut to `max_chars` with an ellipsis, or `None` when too little room
/// is left for it to say anything.
fn fit_detail(detail: &str, max_chars: usize) -> Option<String> {
    let n = detail.chars().count();
    if n <= max_chars {
        return Some(detail.to_owned());
    }
    if max_chars < 6 {
        return None;
    }
    let cut: String = detail.chars().take(max_chars - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// Path of a crate inside the crates.io sparse index (lower-cased name).
fn sparse_index_path(name: &str) -> String {
    let n = name.to_lowercase();
    let c: Vec<char> = n.chars().collect();
    match c.len() {
        0 => n,
        1 => format!("1/{n}"),
        2 => format!("2/{n}"),
        3 => format!("3/{}/{}", c[0], n),
        _ => {
            let a: String = c[..2].iter().collect();
            let b: String = c[2..4].iter().collect();
            format!("{a}/{b}/{n}")
        }
    }
}

/// The feature names a crate publishes at the version matching `version_req`.
///
/// `None` when the answer isn't knowable right now — offline, crate not found,
/// index unreachable. Callers must treat that as "don't know", never as "the
/// feature is missing": warning about a feature on a failed lookup would be
/// worse than not checking at all.
///
/// Short timeout on purpose: this runs on the UI thread during an import, and a
/// user with no network must wait seconds, not minutes. The request's own
/// budget covers DNS, the connect and the body, but not a TLS handshake that
/// trickles in, so the UI stops waiting on its own at the same 4 s.
pub(crate) fn known_features(name: &str, version_req: &str) -> Option<Vec<String>> {
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(4);
    let owned = name.to_owned();
    let data =
        crate::net::within(BUDGET, move || fetch_versions_with_timeout(&owned, BUDGET))?.ok()?;
    let version = pick_version(&data.versions, Some(version_req))?;
    data.features.get(version).cloned()
}

/// What [`fetch_versions_with_timeout`] reports for a 404.
const CRATE_NOT_FOUND: &str = "crate not found";

/// Fetch a crate's sparse-index entry (versions + per-version features),
/// following a `-` / `_` misspelling to the name it was published under.
///
/// The index is keyed by the EXACT spelling — `hmmd-mmwave-sensor-async` is a
/// 404 for a crate published as `hmmd_mmwave_sensor_async` — so on a 404 the
/// crates.io API, which resolves either spelling, is asked for the published
/// one. The answer's [`IndexData::name`] then differs from `name`, and each
/// caller decides what that means: "Add dependency" writes the published name,
/// `Ctrl+Space` on a written manifest line says the line is misspelled.
///
/// `known_features` deliberately does NOT follow: its question is about the
/// manifest as written, and Cargo would reject that spelling.
pub(super) fn fetch_versions(name: &str) -> Result<IndexData, String> {
    // The interactive completion has no deadline of its own — it already runs on
    // a background thread and the popup simply shows "Loading…".
    let timeout = std::time::Duration::from_secs(30);
    match fetch_versions_with_timeout(name, timeout) {
        Err(e) if e == CRATE_NOT_FOUND => match super::crate_search::canonical_name(name) {
            Ok(Some(published)) if published != name => {
                fetch_versions_with_timeout(&published, timeout)
            }
            // No such crate under any spelling — or the API could not say, in
            // which case the index's own answer is still the truest one.
            _ => Err(e),
        },
        other => other,
    }
}

fn fetch_versions_with_timeout(
    name: &str,
    timeout: std::time::Duration,
) -> Result<IndexData, String> {
    let url = format!("https://index.crates.io/{}", sparse_index_path(name));
    let body = crate::net::agent(timeout)
        .get(&url)
        .header(
            "User-Agent",
            concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION"),
                " (crate version lookup)"
            ),
        )
        .call()
        .map_err(|e| index_error(&url, &e))?
        .into_body()
        .read_to_string()
        .map_err(|e| index_error(&url, &e))?;
    Ok(parse_index(&body))
}

/// How a failed sparse-index fetch reads: a 404 is [`CRATE_NOT_FOUND`], which
/// [`fetch_versions`] acts on, anything else a failure.
fn index_error(url: &str, e: &ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(404) => CRATE_NOT_FOUND.to_string(),
        other => crate::net::describe(url, other),
    }
}

/// Parse a sparse-index body (newline-delimited JSON): non-yanked versions,
/// newest first, plus each one's feature names.
fn parse_index(body: &str) -> IndexData {
    let mut name = None;
    let mut versions = Vec::new();
    let mut features = std::collections::BTreeMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            // Every line carries it, yanked ones included — and it is the
            // published spelling whatever case the (lower-cased) path had.
            if let Some(n) = v.get("name").and_then(|n| n.as_str()) {
                name = Some(n.to_owned());
            }
            if v.get("yanked").and_then(|y| y.as_bool()).unwrap_or(false) {
                continue;
            }
            let Some(ver) = v.get("vers").and_then(|s| s.as_str()) else {
                continue;
            };
            versions.push(ver.to_string());
            features.insert(ver.to_string(), parse_features(&v));
        }
    }
    versions.reverse(); // index lists oldest->newest; show newest first.
    IndexData {
        name,
        versions,
        features,
    }
}

/// The feature names of one index entry. `features2` is the newer index field
/// (it carries the ones that reference optional dependencies) and is merged in.
/// `dep:foo` entries are Cargo's own plumbing for optional dependencies — they
/// are not written in a manifest, so they are dropped.
fn parse_features(entry: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in ["features", "features2"] {
        if let Some(map) = entry.get(key).and_then(|f| f.as_object()) {
            out.extend(
                map.keys()
                    .filter(|k| !k.starts_with("dep:"))
                    .map(|k| k.to_string()),
            );
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Curated set of embedded-relevant crates (name, one-line description). A raw
/// crates.io search returns far too much noise for an MCU project, so the name
/// list is hand-picked; versions are still fetched live from crates.io.
#[rustfmt::skip]
const EMBEDDED_CRATES: &[(&str, &str)] = &[
    // Cortex-M core / runtime
    ("cortex-m", "Low level access to Cortex-M processors"),
    ("cortex-m-rt", "Startup code & minimal runtime for Cortex-M"),
    ("cortex-m-semihosting", "Semihosting for Cortex-M"),
    ("cortex-m-rtic", "Real-Time Interrupt-driven Concurrency"),
    ("rtic", "Real-Time Interrupt-driven Concurrency framework"),
    ("critical-section", "Cross-platform critical sections"),
    ("bare-metal", "Abstractions for bare-metal programming"),
    ("vcell", "Volatile Cell"),
    ("volatile-register", "Volatile access to memory mapped registers"),
    // RISC-V
    ("riscv", "Low level access to RISC-V processors"),
    ("riscv-rt", "Startup code & runtime for RISC-V"),
    // HAL traits / IO
    ("embedded-hal", "Hardware Abstraction Layer traits"),
    ("embedded-hal-async", "Async HAL traits"),
    ("embedded-hal-bus", "Bus/sharing utilities for embedded-hal"),
    ("embedded-io", "Core IO traits for embedded"),
    ("embedded-io-async", "Async core IO traits"),
    ("nb", "Minimal non-blocking IO abstraction"),
    ("fugit", "Time units & duration for embedded"),
    // STM32 HALs / PACs
    ("stm32f1xx-hal", "HAL for STM32F1 series"),
    ("stm32f4xx-hal", "HAL for STM32F4 series"),
    ("stm32f0xx-hal", "HAL for STM32F0 series"),
    ("stm32f3xx-hal", "HAL for STM32F3 series"),
    ("stm32f7xx-hal", "HAL for STM32F7 series"),
    ("stm32l0xx-hal", "HAL for STM32L0 series"),
    ("stm32l4xx-hal", "HAL for STM32L4 series"),
    ("stm32g0xx-hal", "HAL for STM32G0 series"),
    ("stm32g4xx-hal", "HAL for STM32G4 series"),
    ("stm32h7xx-hal", "HAL for STM32H7 series"),
    ("stm32f1", "Peripheral access crate for STM32F1"),
    ("stm32f4", "Peripheral access crate for STM32F4"),
    ("stm32-hal2", "Multi-family STM32 HAL"),
    // Embassy async
    ("embassy-executor", "Async/await executor for embedded"),
    ("embassy-stm32", "Embassy HAL for STM32"),
    ("embassy-time", "Timekeeping & timers for Embassy"),
    ("embassy-sync", "Async synchronization primitives"),
    ("embassy-futures", "Async utilities for embedded"),
    ("embassy-net", "Async TCP/IP network stack"),
    ("embassy-usb", "Async USB device stack"),
    ("embassy-rp", "Embassy HAL for RP2040"),
    ("embassy-nrf", "Embassy HAL for nRF"),
    ("static_cell", "Statically allocated, initialized-at-runtime cell"),
    // ESP32
    ("esp-hal", "HAL for Espressif (no_std) chips"),
    ("esp-backtrace", "Backtrace support for ESP"),
    ("esp-println", "println!/log over UART/RTT for ESP"),
    ("esp-wifi", "Wi-Fi / BLE for ESP no_std"),
    ("esp-idf-hal", "embedded-hal for ESP-IDF (std)"),
    ("esp-idf-svc", "ESP-IDF services (Wi-Fi, MQTT, …)"),
    ("esp-idf-sys", "Raw bindings to ESP-IDF"),
    ("esp32", "Peripheral access crate for ESP32"),
    ("esp32c3", "Peripheral access crate for ESP32-C3"),
    // RP2040
    ("rp2040-hal", "HAL for the RP2040"),
    ("rp-pico", "Board support for the Raspberry Pi Pico"),
    ("rp2040-boot2", "Second-stage bootloaders for RP2040"),
    // nRF
    ("nrf52840-hal", "HAL for nRF52840"),
    ("nrf52833-hal", "HAL for nRF52833"),
    ("nrf52833-pac", "Peripheral access crate for nRF52833"),
    ("nrf-hal-common", "Common HAL code for nRF"),
    ("microbit-v2", "Board support for the BBC micro:bit v2"),
    // AVR
    ("avr-device", "Register access for AVR microcontrollers"),
    // Logging / debug
    ("defmt", "Efficient deferred-formatting logging"),
    ("defmt-rtt", "defmt transport over RTT"),
    ("defmt-test", "Embedded test harness using defmt"),
    ("rtt-target", "RTT target-side implementation"),
    ("panic-probe", "Panic handler printing via defmt/RTT"),
    ("panic-halt", "Panic handler that halts"),
    ("panic-semihosting", "Panic handler over semihosting"),
    ("panic-rtt-target", "Panic handler over RTT"),
    ("log", "Logging facade"),
    // Data / math (no_std)
    ("heapless", "Fixed-capacity data structures (no_std)"),
    ("fixed", "Fixed-point numbers"),
    ("micromath", "Embedded-friendly math approximations"),
    ("libm", "Pure-Rust libm math functions"),
    ("num-traits", "Numeric traits"),
    ("byteorder", "Read/write numbers in big/little endian"),
    ("bitflags", "Typed bitflag sets"),
    ("bitfield", "Bitfield macros"),
    ("void", "Uninhabited void type"),
    // USB
    ("usb-device", "USB device stack"),
    ("usbd-serial", "USB CDC-ACM serial class"),
    ("usbd-hid", "USB HID class"),
    // Graphics / displays / drivers
    ("embedded-graphics", "2D graphics for small displays"),
    ("embedded-graphics-core", "Core traits for embedded-graphics"),
    ("display-interface", "Generic display interface traits"),
    ("ssd1306", "Driver for SSD1306 OLED displays"),
    ("st7789", "Driver for ST7789 TFT displays"),
    ("ili9341", "Driver for ILI9341 TFT displays"),
    ("epd-waveshare", "Driver for Waveshare e-paper displays"),
    ("smart-leds", "Driver abstraction for addressable LEDs"),
    ("ws2812-spi", "WS2812 LEDs over SPI"),
    ("shared-bus", "Share an I2C/SPI bus between drivers"),
    ("mpu6050", "Driver for the MPU6050 IMU"),
    ("bmp280", "Driver for the BMP280 pressure sensor"),
    ("ds323x", "Driver for DS3231/DS3232 RTCs"),
    ("ina219", "Driver for the INA219 power monitor"),
    // Async / futures
    ("futures", "Async primitives & combinators"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(text: &str) -> Option<CargoCtx> {
        // Cursor is marked by '|' in the fixture.
        let cursor = text.find('|').expect("fixture needs a | cursor marker");
        let cleaned = text.replace('|', "");
        // char index of the cursor (fixtures are ASCII)
        cargo_context(&cleaned, cursor)
    }

    // ── features = [ … ] ─────────────────────────────────────────────────────

    /// `(crate, version_req, already, quoted, prefix)` of a Feature context.
    fn feat(text: &str) -> Option<(String, Option<String>, Vec<String>, bool, String)> {
        match ctx(text)? {
            CargoCtx::Feature {
                crate_name,
                version_req,
                already,
                quoted,
                prefix,
                ..
            } => Some((crate_name, version_req, already, quoted, prefix)),
            _ => None,
        }
    }

    #[test]
    fn feature_ctx_inline_table() {
        let (name, req, already, quoted, prefix) = feat(
            "[dependencies]\nembassy-stm32 = { version = \"0.2\", features = [\"stm32f4|\"] }\n",
        )
        .expect("inline features array");
        assert_eq!(name, "embassy-stm32");
        assert_eq!(req.as_deref(), Some("0.2"));
        assert!(quoted);
        assert_eq!(prefix, "stm32f4");
        // The string being typed is not "already there".
        assert_eq!(already, Vec::<String>::new());
    }

    #[test]
    fn feature_ctx_multi_line_table_entry() {
        let src = "[dependencies.defmt]\nversion = \"0.3\"\nfeatures = [\n    \"alloc\",\n    \"e|\",\n]\n";
        let (name, req, already, quoted, prefix) = feat(src).expect("multi-line features array");
        assert_eq!(name, "defmt");
        assert_eq!(req.as_deref(), Some("0.3"));
        assert_eq!(already, ["alloc"]);
        assert!(quoted);
        assert_eq!(prefix, "e");
    }

    #[test]
    fn feature_ctx_outside_quotes_still_completes() {
        // Ctrl+Space right after the opening bracket — the accept adds quotes.
        let (name, _, _, quoted, prefix) =
            feat("[dependencies]\nfoo = { version = \"1\", features = [|] }\n")
                .expect("empty array");
        assert_eq!(name, "foo");
        assert!(!quoted);
        assert_eq!(prefix, "");
    }

    #[test]
    fn feature_ctx_after_a_comma() {
        let (_, _, already, quoted, prefix) =
            feat("[dependencies]\nfoo = { features = [\"a\", |] }\n").expect("after comma");
        assert_eq!(already, ["a"]);
        assert!(!quoted);
        assert_eq!(prefix, "");
    }

    #[test]
    fn feature_ctx_ignores_path_and_git_dependencies() {
        assert!(
            feat("[dependencies]\nfoo = { path = \"../foo\", features = [\"a|\"] }\n").is_none()
        );
        assert!(
            feat("[dependencies]\nfoo = { git = \"http://x\", features = [\"a|\"] }\n").is_none()
        );
    }

    #[test]
    fn a_closed_array_is_not_a_feature_context() {
        // Caret after the `]` — the array is closed, so this is not it.
        assert!(feat("[dependencies]\nfoo = { features = [\"a\"] }|\n").is_none());
    }

    #[test]
    fn a_non_features_array_is_not_a_feature_context() {
        assert!(feat("[dependencies]\nfoo = { targets = [\"a|\"] }\n").is_none());
    }

    #[test]
    fn a_table_header_is_not_an_open_array() {
        // The `[` of `[dependencies]` must not be mistaken for an array.
        assert!(feat("[dependencies]\nfo|\n").is_none());
    }

    #[test]
    fn version_completion_still_wins_on_the_version_field() {
        assert!(matches!(
            ctx("[dependencies]\nfoo = { version = \"0.|\" }\n"),
            Some(CargoCtx::Version { .. })
        ));
    }

    // ── Index parsing / version pick / filtering ─────────────────────────────

    #[test]
    fn parse_index_reads_features_and_drops_dep_entries() {
        let body = "{\"vers\":\"0.1.0\",\"features\":{\"default\":[\"std\"],\"std\":[]}}\n\
                    {\"vers\":\"0.2.0\",\"features\":{\"default\":[]},\
                      \"features2\":{\"async\":[\"dep:tokio\"],\"dep:tokio\":[]}}\n";
        let d = parse_index(body);
        assert_eq!(d.versions, ["0.2.0", "0.1.0"]);
        assert_eq!(d.features["0.1.0"], ["default", "std"]);
        // `features2` merged in, `dep:` plumbing dropped.
        assert_eq!(d.features["0.2.0"], ["async", "default"]);
    }

    #[test]
    fn pick_version_matches_the_manifest_requirement() {
        let v: Vec<String> = ["1.2.0", "0.2.5", "0.2.1", "0.1.0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(pick_version(&v, Some("0.2")).unwrap(), "0.2.5");
        assert_eq!(pick_version(&v, Some("^0.2")).unwrap(), "0.2.5");
        assert_eq!(pick_version(&v, Some("0.2.1")).unwrap(), "0.2.1");
        assert_eq!(pick_version(&v, Some("1")).unwrap(), "1.2.0");
        // No requirement, or one nothing matches → newest.
        assert_eq!(pick_version(&v, None).unwrap(), "1.2.0");
        assert_eq!(pick_version(&v, Some("9")).unwrap(), "1.2.0");
    }

    #[test]
    fn filter_features_drops_what_is_already_there() {
        let names: Vec<String> = ["alloc", "default", "encoding-rzcobs", "std"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let items = filter_features(&names, "", &["default".to_string()], true);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["alloc", "encoding-rzcobs", "std"]);
    }

    #[test]
    fn filter_features_ranks_prefix_before_substring() {
        let names: Vec<String> = ["encoding-rzcobs", "std", "cobs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let items = filter_features(&names, "co", &[], true);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["cobs", "encoding-rzcobs"]);
    }

    #[test]
    fn name_context_in_dependencies() {
        let c = ctx("[dependencies]\nstm|\n");
        assert_eq!(
            c,
            Some(CargoCtx::Name {
                start: 15,
                prefix: "stm".to_string()
            })
        );
    }

    #[test]
    fn empty_name_context_offers_all() {
        let c = ctx("[dependencies]\n|\n");
        assert!(matches!(c, Some(CargoCtx::Name { prefix, .. }) if prefix.is_empty()));
    }

    #[test]
    fn version_context_simple_form() {
        let c = ctx("[dependencies]\ncortex-m = \"0.7|\"\n");
        match c {
            Some(CargoCtx::Version {
                crate_name, prefix, ..
            }) => {
                assert_eq!(crate_name, "cortex-m");
                assert_eq!(prefix, "0.7");
            }
            other => panic!("expected version ctx, got {other:?}"),
        }
    }

    #[test]
    fn version_context_table_form() {
        let c = ctx("[dependencies]\nserde = { version = \"1|\", features = [] }\n");
        match c {
            Some(CargoCtx::Version {
                crate_name, prefix, ..
            }) => {
                assert_eq!(crate_name, "serde");
                assert_eq!(prefix, "1");
            }
            other => panic!("expected version ctx, got {other:?}"),
        }
    }

    #[test]
    fn version_context_dotted_section() {
        let c = ctx("[dependencies.heapless]\nversion = \"0.|\"\n");
        match c {
            Some(CargoCtx::Version {
                crate_name, prefix, ..
            }) => {
                assert_eq!(crate_name, "heapless");
                assert_eq!(prefix, "0.");
            }
            other => panic!("expected version ctx, got {other:?}"),
        }
    }

    #[test]
    fn features_field_is_not_version() {
        // Was `None` before feature completion existed; the point of the test —
        // that this is not a VERSION — still holds.
        let c = ctx("[dependencies]\nserde = { features = [\"de|\"] }\n");
        assert!(matches!(c, Some(CargoCtx::Feature { .. })), "got {c:?}");
    }

    #[test]
    fn outside_dependency_table_is_none() {
        assert_eq!(ctx("[package]\nnam|\n"), None);
    }

    #[test]
    fn section_header_line_is_none() {
        assert_eq!(ctx("[depend|encies]\n"), None);
    }

    #[test]
    fn cursor_after_closed_string_is_none() {
        // Even number of quotes before the cursor → not inside a string.
        let c = ctx("[dependencies]\ncortex-m = \"0.7\"|\n");
        assert_eq!(c, None);
    }

    #[test]
    fn sparse_index_paths() {
        assert_eq!(sparse_index_path("a"), "1/a");
        assert_eq!(sparse_index_path("nb"), "2/nb");
        assert_eq!(sparse_index_path("log"), "3/l/log");
        assert_eq!(sparse_index_path("serde"), "se/rd/serde");
        assert_eq!(sparse_index_path("cortex-m"), "co/rt/cortex-m");
        assert_eq!(sparse_index_path("STM32"), "st/m3/stm32");
    }

    #[test]
    fn parse_versions_newest_first_skips_yanked() {
        let body = "\
{\"name\":\"x\",\"vers\":\"0.1.0\",\"yanked\":false}
{\"name\":\"x\",\"vers\":\"0.2.0\",\"yanked\":true}
{\"name\":\"x\",\"vers\":\"0.3.0\",\"yanked\":false}
";
        assert_eq!(parse_index(body).versions, vec!["0.3.0", "0.1.0"]);
    }

    #[test]
    fn filter_crates_ranks_prefix_first() {
        let items = filter_crates("stm32f1");
        assert!(!items.is_empty());
        assert!(items[0].label.starts_with("stm32f1"));
    }

    // ── Live crates.io rows ──────────────────────────────────────────────────

    fn hit(name: &str, downloads: u64) -> super::super::crate_search::Hit {
        super::super::crate_search::Hit {
            name: name.to_owned(),
            description: format!("{name} description"),
            downloads,
        }
    }

    fn labels(items: &[CargoItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    /// The reported case: a crate outside the curated list, published minutes
    /// ago, reached by typing part of its name.
    #[test]
    fn a_crate_outside_the_curated_list_is_offered_under_it() {
        let prefix = "hmmd";
        let curated = filter_crates(prefix);
        assert!(
            curated.is_empty(),
            "the fixture must be outside the curated list"
        );
        let rows = live_rows(&[hit("hmmd_mmwave_sensor_async", 0)], prefix, &curated);
        assert_eq!(labels(&rows), ["hmmd_mmwave_sensor_async"]);
        assert!(rows[0].from_search);
        assert!(
            matches!(&rows[0].action, CargoAccept::CrateName(n) if n == "hmmd_mmwave_sensor_async"),
            "the PUBLISHED spelling is what gets inserted"
        );
    }

    #[test]
    fn a_dash_and_an_underscore_match_each_other() {
        let rows = live_rows(&[hit("hmmd_mmwave_sensor_async", 0)], "hmmd-mmwave", &[]);
        assert_eq!(labels(&rows), ["hmmd_mmwave_sensor_async"]);
    }

    /// The search matches descriptions and keywords too; a row whose NAME does
    /// not contain what was typed is noise, and noise is what the curated list
    /// was there to avoid.
    #[test]
    fn a_hit_whose_name_lacks_the_prefix_is_dropped() {
        let rows = live_rows(&[hit("mr60fda2-proto", 900)], "mmwave", &[]);
        assert!(rows.is_empty(), "{:?}", labels(&rows));
    }

    #[test]
    fn a_curated_crate_is_not_listed_twice() {
        let curated = filter_crates("embassy-sync");
        let rows = live_rows(
            &[hit("embassy_sync", 10), hit("embassy-sync-x", 1)],
            "embassy-sync",
            &curated,
        );
        assert_eq!(labels(&rows), ["embassy-sync-x"]);
    }

    #[test]
    fn live_rows_rank_exact_then_prefix_then_substring() {
        let hits = [
            hit("my-hmmd", 1_000),
            hit("hmmd-tools", 5),
            hit("hmmd-core", 50),
            hit("hmmd", 0),
        ];
        let rows = live_rows(&hits, "hmmd", &[]);
        assert_eq!(
            labels(&rows),
            ["hmmd", "hmmd-core", "hmmd-tools", "my-hmmd"]
        );
    }

    #[test]
    fn an_empty_prefix_adds_no_live_rows() {
        assert!(live_rows(&[hit("anything", 1)], "", &[]).is_empty());
    }

    /// One or two letters send no query, and the cached answers of earlier
    /// queries filtered by `e` would flood the list with unrelated crates.
    #[test]
    fn a_prefix_below_the_search_threshold_adds_no_live_rows() {
        let cached = [hit("embassy-dt", 5), hit("heapless-bytes", 9)];
        assert!(live_rows(&cached, "e", &[]).is_empty());
        assert!(live_rows(&cached, "em", &[]).is_empty());
        assert_eq!(labels(&live_rows(&cached, "emb", &[])), ["embassy-dt"]);
    }

    /// An empty list must SAY why. Closing silently is what made a freshly
    /// published crate look like a broken `Ctrl+Space`.
    #[test]
    fn an_empty_name_list_explains_itself() {
        assert!(
            matches!(name_note("hm", true, None), Some(PopupNote::Info(t)) if t.contains("3+"))
        );
        assert!(
            matches!(name_note("zzqq", true, None), Some(PopupNote::Info(t)) if t.contains("zzqq"))
        );
        assert_eq!(name_note("stm", false, None), None);
        assert_eq!(name_note("hm", false, None), None);
        assert_eq!(name_note("", false, None), None);
    }

    /// A truncated answer is flagged even with rows on screen: `pca9685` was
    /// outside both pages for `pca`, and its absence must not read as "gone".
    #[test]
    fn a_truncated_answer_is_flagged_even_with_rows() {
        for empty in [false, true] {
            assert!(
                matches!(name_note("pca", empty, Some(390)), Some(PopupNote::Info(t)) if t.contains("390")),
                "empty = {empty}"
            );
        }
    }

    fn row(label: &str, from_search: bool) -> CargoItem {
        CargoItem {
            label: label.to_owned(),
            detail: String::new(),
            action: CargoAccept::CrateName(label.to_owned()),
            from_search,
        }
    }

    /// Typing all of `time` and pressing Enter must give `time`, not the
    /// curated `embassy-time` that merely contains it.
    #[test]
    fn an_exact_live_name_leads_unless_a_curated_row_is_exact() {
        let mut items = vec![row("embassy-time", false)];
        merge_name_rows(
            &mut items,
            vec![row("time", true), row("time-macros", true)],
            "time",
        );
        assert_eq!(labels(&items), ["time", "embassy-time", "time-macros"]);

        // A curated exact name keeps its place; the live rows stay below.
        let mut items = vec![row("heapless", false)];
        merge_name_rows(&mut items, vec![row("heapless-bytes", true)], "heapless");
        assert_eq!(labels(&items), ["heapless", "heapless-bytes"]);

        // No exact live row: plain append.
        let mut items = vec![row("rtt-target", false)];
        merge_name_rows(&mut items, vec![row("rtt-log", true)], "rtt");
        assert_eq!(labels(&items), ["rtt-target", "rtt-log"]);
    }

    #[test]
    fn a_moved_highlight_follows_its_crate_when_rows_resort() {
        let old = [row("embassy-sync", false), row("embassy-foo", true)];
        let new = [
            row("embassy-sync", false),
            row("embassy-embedded-hal", true),
            row("embassy-foo", true),
        ];
        assert_eq!(keep_selection(&old, 1, true, &new), 2);
        // Untouched, the top row stays the top row — the best match.
        assert_eq!(keep_selection(&old, 0, false, &new), 0);
        // Moved BACK to row 0 is a choice too: an exact name promoted above it
        // must not take the highlight.
        let promoted = [row("usb", true), row("usb-device", false)];
        assert_eq!(
            keep_selection(&[row("usb-device", false)], 0, true, &promoted),
            1
        );
        // The crate is gone: clamp, never out of range.
        assert_eq!(keep_selection(&old, 1, true, &[row("x", false)]), 0);
    }

    #[test]
    fn line_index_of_counts_newlines_before_the_caret() {
        let text = "[dependencies]\nțară = \"1\"\nhm";
        assert_eq!(line_index_of(text, 0), 0);
        assert_eq!(line_index_of(text, 14), 0);
        assert_eq!(line_index_of(text, 15), 1);
        assert_eq!(line_index_of(text, text.chars().count()), 2);
        // Typing ABOVE (an extra caret) does not move the line the popup is on.
        let typed_above = "[dependencies]\nțară = \"1\"x\nhm";
        assert_eq!(line_index_of(typed_above, typed_above.chars().count()), 2);
    }

    /// The KEY is not a field: `windows-registry` is a crates.io crate, not an
    /// entry pointing at another registry.
    #[test]
    fn a_key_ending_in_a_field_name_is_not_that_field() {
        for src in [
            "[dependencies]\nwindows-registry = \"0.|\"\n",
            "[dependencies]\nsignal-hook-registry = { version = \"1.|\" }\n",
            "[dependencies]\nregistry = \"0.|\"\n",
        ] {
            assert!(
                matches!(ctx(src), Some(CargoCtx::Version { .. })),
                "{src:?} lost version completion"
            );
        }
        let (name, ..) =
            feat("[dependencies]\noid-registry = { version = \"0.8\", features = [\"|\"] }\n")
                .expect("features of a *-registry crate");
        assert_eq!(name, "oid-registry");
        assert!(
            feat("[dependencies]\ntyped-path = { version = \"0.9\", features = [\"|\"] }\n")
                .is_some(),
            "a `*-path` crate is not a path dependency"
        );
    }

    /// The boundary rule itself, where no key is around to hide behind: in a
    /// `[dependencies.foo]` body `default-features` is not `features`.
    #[test]
    fn a_field_starts_only_where_a_field_can_start() {
        let body = "[dependencies.foo]\nversion = \"1\"\ndefault-features = false\n";
        assert_eq!(field_token(body, "features"), None);
        assert_eq!(
            field_token(body, "default-features"),
            Some(("false".to_owned(), false))
        );
        assert_eq!(field_of(body, "version").as_deref(), Some("1"));
        let inline = " { version = \"1\", path = \"../x\" }";
        assert_eq!(field_of(inline, "path").as_deref(), Some("../x"));
    }

    #[test]
    fn a_literal_string_package_is_read_whole() {
        let c = ctx("[dependencies]\nio = { package = 'embedded-io', version = \"0.|\" }\n");
        assert!(
            matches!(&c, Some(CargoCtx::Version { crate_name, .. }) if crate_name == "embedded-io"),
            "{c:?}"
        );
        assert_eq!(
            ctx("[dependencies]\nacme-hal = { version = \"1.|\", registry = 'acme' }\n"),
            None
        );
    }

    /// `workspace = true` hands the real name to the root manifest, so a key
    /// that differs from the published spelling is not a misspelling there.
    #[test]
    fn an_inherited_entry_is_marked() {
        // Table form with a comment after the bool — the comment is not the value.
        let c = ctx(
            "[dependencies.embedded_io]\nworkspace = true  # renamed in the root\nfeatures = [\"|\"]\n",
        );
        assert!(
            matches!(
                c,
                Some(CargoCtx::Feature {
                    inherited: true,
                    ..
                })
            ),
            "{c:?}"
        );
        let c = ctx("[dependencies]\nembedded_io = { workspace = true, features = [\"|\"] }\n");
        assert!(
            matches!(
                c,
                Some(CargoCtx::Feature {
                    inherited: true,
                    ..
                })
            ),
            "{c:?}"
        );
        let c = ctx("[dependencies]\nembedded-io = { version = \"0.6\", features = [\"|\"] }\n");
        assert!(
            matches!(
                c,
                Some(CargoCtx::Feature {
                    inherited: false,
                    ..
                })
            ),
            "{c:?}"
        );
    }

    /// `package` names the crate; the key is only the local name. Looking the key
    /// up told a valid line it was misspelled.
    #[test]
    fn a_renamed_dependency_is_looked_up_by_its_package() {
        let c =
            ctx("[dependencies]\nembedded_io = { package = \"embedded-io\", version = \"0.|\" }\n");
        assert!(
            matches!(&c, Some(CargoCtx::Version { crate_name, .. }) if crate_name == "embedded-io"),
            "{c:?}"
        );
        let (name, ..) = feat(
            "[dependencies]\nio = { package = \"embedded-io\", version = \"0.6\", features = [\"a|\"] }\n",
        )
        .expect("features of a renamed dependency");
        assert_eq!(name, "embedded-io");
        // Table form, with the `package` ABOVE a features array.
        let c = ctx(
            "[dependencies.io]\npackage = \"embedded-io\"\nfeatures = [\"alloc\"]\nversion = \"0.|\"\n",
        );
        assert!(
            matches!(&c, Some(CargoCtx::Version { crate_name, .. }) if crate_name == "embedded-io"),
            "{c:?}"
        );
    }

    #[test]
    fn another_registry_is_not_asked_of_crates_io() {
        assert_eq!(
            ctx("[dependencies]\nacme_hal = { registry = \"acme\", version = \"1|\" }\n"),
            None
        );
        assert!(
            feat("[dependencies]\nacme = { registry = \"acme\", features = [\"a|\"] }\n").is_none()
        );
    }

    #[test]
    fn a_misspelled_manifest_line_is_told_the_published_name() {
        let body = r#"{"name":"hmmd_mmwave_sensor_async","vers":"0.1.0","features":{}}"#;
        let data = parse_index(body);
        assert_eq!(data.name.as_deref(), Some("hmmd_mmwave_sensor_async"));
        assert!(matches!(
            spelling_note(&data, "hmmd-mmwave-sensor-async"),
            Some(PopupNote::Error(t)) if t.contains("`hmmd_mmwave_sensor_async`")
        ));
        // The control: the right spelling gets its versions, no note.
        assert_eq!(spelling_note(&data, "hmmd_mmwave_sensor_async"), None);
    }

    /// An entry without a `name` (every older fixture in this file) is not a
    /// mismatch — there is nothing to say it is misspelled.
    #[test]
    fn an_entry_without_a_name_is_not_a_misspelling() {
        let data = parse_index(r#"{"vers":"0.1.0"}"#);
        assert_eq!(spelling_note(&data, "anything"), None);
    }

    #[test]
    fn the_detail_yields_to_a_long_name() {
        assert_eq!(fit_detail("short", 44).as_deref(), Some("short"));
        assert_eq!(
            fit_detail("Async, sync, and closure-based", 10).as_deref(),
            Some("Async, sy…")
        );
        assert_eq!(fit_detail("Async driver for a sensor", 5), None);
    }

    /// "No such crate" is what sends a lookup on to the published spelling,
    /// so a 404 must not read as just another failure.
    #[test]
    fn an_index_404_reads_as_crate_not_found() {
        let url = "https://index.crates.io/hm/md/hmmd-mmwave-sensor-async";
        assert_eq!(
            index_error(url, &ureq::Error::StatusCode(404)),
            CRATE_NOT_FOUND
        );
        assert_ne!(
            index_error(url, &ureq::Error::StatusCode(500)),
            CRATE_NOT_FOUND
        );
    }

    /// What "Add dependency" leans on, against the real index: the dash guess
    /// for `use hmmd_mmwave_sensor_async` is a 404, and the fetch comes back
    /// under the published spelling instead of failing.
    #[test]
    #[ignore = "talks to crates.io; run with --ignored"]
    fn live_a_dash_guess_is_followed_to_the_published_name() {
        assert_eq!(
            fetch_versions_with_timeout(
                "hmmd-mmwave-sensor-async",
                std::time::Duration::from_secs(20)
            )
            .err()
            .as_deref(),
            Some(CRATE_NOT_FOUND),
            "the index itself is spelling-exact"
        );
        let data = fetch_versions("hmmd-mmwave-sensor-async").expect("resolved");
        assert_eq!(data.name.as_deref(), Some("hmmd_mmwave_sensor_async"));
        assert!(
            data.versions.iter().any(|v| v == "0.1.0"),
            "{:?}",
            data.versions
        );
    }

    /// A caret left over from a LONGER manifest must not index past the current
    /// text. Regression: the popup state is shared across manifests (the
    /// firmware's and every extracted library's), so switching between two of
    /// different lengths panicked with "range start index N out of range".
    /// `cargo_context` clamps; the splice in `apply_cargo_accept` did not.
    #[test]
    fn a_cursor_past_the_end_is_clamped_not_panicking() {
        let text = "[dependencies]
foo = \"1\"
";
        // Far past the end — what a stale caret from another file looks like.
        assert!(cargo_context(text, 10_000).is_some() || cargo_context(text, 10_000).is_none());
        // And the offsets it reports are always inside the text.
        if let Some(ctx) = cargo_context(text, 10_000) {
            let len = text.chars().count();
            let start = match ctx {
                CargoCtx::Name { start, .. }
                | CargoCtx::Version { start, .. }
                | CargoCtx::Feature { start, .. } => start,
            };
            assert!(start <= len, "start {start} > {len}");
        }
    }
}

#[cfg(test)]
mod unknowable_lookup_tests {
    use super::*;

    /// The composition `known_features` performs, on a body the index did not
    /// really give us.
    fn features_for(body: &str, req: &str) -> Option<Vec<String>> {
        let data = parse_index(body);
        let version = pick_version(&data.versions, Some(req))?;
        data.features.get(version).cloned()
    }

    /// An unreadable answer must come back as `None`, NEVER as `Some(vec![])`.
    ///
    /// The difference is the whole import preflight. `None` means "we could not
    /// check", which the report says out loud; `Some(vec![])` means "the crate
    /// publishes no such feature", which would put a false "no HAL support" on
    /// every chip imported behind a captive portal or a proxy that answers 200
    /// with a login page. A false alarm on every import is worse than the
    /// missing warning this check was added to fix.
    #[test]
    fn an_unreadable_index_answer_is_unknowable_not_empty() {
        for body in [
            "",
            "   \n\n  ",
            "<html><body>Sign in to continue</body></html>",
            // Valid JSON, but not an index entry.
            r#"{"message":"rate limited"}"#,
            // An entry whose only version is yanked: nothing left to match.
            r#"{"vers":"0.6.0","features":{"stm32g071cb":[]},"yanked":true}"#,
        ] {
            assert_eq!(
                features_for(body, "0.6"),
                None,
                "this body must be unknowable, not an empty feature list: {body:?}"
            );
        }
    }

    /// And the control: a real-shaped entry DOES answer, so the test above is
    /// pinning the failure path rather than a parser that never works.
    #[test]
    fn a_real_index_entry_still_answers() {
        let body = r#"{"vers":"0.6.0","features":{"stm32g071cb":[],"time":[]}}"#;
        let f = features_for(body, "0.6").expect("a well-formed entry answers");
        assert!(f.contains(&"stm32g071cb".to_owned()), "{f:?}");
        assert!(!f.contains(&"stm32wl30kb".to_owned()), "{f:?}");
    }
}
