//! Leftmost "Code Editor" panel (the [Editor][MCU][Project] layout).
//!
//! Owns: the toolbar (Copy + Errors/Types toggles), the code editor widget
//! itself, the embedded bottom diagnostics panel, the LSP completion popup,
//! and the inline-diagnostic overlays.  It also writes the edited text back
//! into `generated_code` (main.rs) or the matching user source file.
//!
//! Implemented as one inherent method on `AppIde`; it borrows the
//! `project_files` snapshot (the project tree, rendered after it, needs the
//! same snapshot).

use super::AppIde;
use super::ProjectFileId;
use crate::lsp;
use crate::panels::mcu_module::project_gen::ProjectFiles;
use eframe::egui;
use egui_code_editor::{CodeEditor, ColorTheme, Syntax};

/// The key for "extract the selection into a function", with Ctrl+Alt.
///
/// A constant so the guard test below can name it: `egui-winit` REWRITES a few
/// keystrokes into clipboard events before egui ever sees a key, and a shortcut
/// that lands on one of those is silently dead. See `extract_fn_key_survives`.
const EXTRACT_FN_KEY: egui::Key = egui::Key::M;

/// The key for "toggle the case of the selection", with Ctrl. Named for the same
/// guard test as [`EXTRACT_FN_KEY`].
const TOGGLE_CASE_KEY: egui::Key = egui::Key::U;

pub(crate) mod add_dep;
mod brace_block;
mod breakpoint_gutter;
pub(crate) mod cargo_complete;
mod code_action;
mod comment;
pub(super) mod completion;
mod context_menu;
mod crate_search;
mod debug_hover;
mod delete_line;
mod diag_embed;
#[cfg(test)]
mod diag_panel_drag_tests;
pub(crate) mod diff_gutter;
mod doc_md;
mod duplicate_line;
mod error_list;
#[cfg(test)]
mod escape_focus_tests;
pub(crate) mod extract_fn;
pub(crate) mod file_cycle;
pub(crate) mod find_replace;
pub(super) mod fold;
mod fold_ui;
mod format;
mod generics;
mod idle_sync;
pub(crate) mod impl_picker;
pub(super) mod inlay_hint;
mod kbd_scope;
mod let_annotation;
mod move_lines;
pub(crate) mod multi_cursor;
mod rename;
#[cfg(test)]
mod replace_enter_tests;
mod snippet;
mod toggle_case;
mod toolbar;
pub(crate) mod usages;
mod word_select;

pub(crate) use toolbar::FLASH_PIPELINES;

/// Default code-editor font size (points); the zoom baseline (Ctrl+0 resets to it).
pub(crate) const DEFAULT_EDITOR_FONT_SIZE: f32 = 13.0;
/// Zoom clamp range for the editor font.
const MIN_EDITOR_FONT_SIZE: f32 = 7.0;
const MAX_EDITOR_FONT_SIZE: f32 = 40.0;

impl AppIde {
    /// Render the leftmost code editor panel (toolbar + editor + diagnostics).
    pub(super) fn show_editor_panel(
        &mut self,
        ui: &mut egui::Ui,
        project_files: &Option<ProjectFiles>,
    ) {
        // ── Ordering invariant vs. the project tree ───────────────────────────
        // IMPORTANT: the tree panel must NEVER run BETWEEN computing
        // display_code and the end-of-frame write-back — a tree click would
        // switch `selected_file` mid-frame and the write-back would store the
        // OLD file's text into the NEW file. Running the WHOLE editor panel
        // before the tree (the [Editor][MCU][Project] layout since 2026-07-10)
        // keeps the pair atomic: a click this frame takes effect next frame.
        let mut display_code: String = if let ProjectFileId::UserFile(i) = self.selected_file {
            self.project_tree
                .user_src_files
                .get(i)
                .map(|(_, c)| c.clone())
                .unwrap_or_default()
        } else if self.selected_file == ProjectFileId::MainRs {
            // Always read from self.generated_code — not from the project_files
            // snapshot built at the start of this frame.  The snapshot is stale
            // whenever load_project_from_dir() runs in the same frame (Open
            // Project), which would otherwise show the previous project's code
            // and then immediately overwrite generated_code via the write-back.
            self.generated_code.clone()
        } else {
            match project_files {
                Some(files) => self.selected_file.content(files).to_owned(),
                None => self.generated_code.clone(),
            }
        };
        // The selected file's project-root-relative path, needed wherever a
        // `UserFile` has to be classified by extension — a user file can now be
        // a library crate's `Cargo.toml`, not just Rust source.
        let selected_path = match self.selected_file {
            ProjectFileId::UserFile(i) => self
                .project_tree
                .user_src_files
                .get(i)
                .map(|(p, _)| p.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        let display_syntax = self.selected_file.syntax(&selected_path);
        let selected_is_manifest = self.selected_file.is_cargo_manifest(&selected_path);
        // Token re-spacing (Shift+Alt+F) is Rust syntax — `:` → `: `, `,` → `, `
        // would rewrite a linker script (`memory.x`), a .ron or a Markdown table.
        // Those files still get the indent-only pass.
        let respace_on_format = match self.selected_file {
            ProjectFileId::MainRs | ProjectFileId::BuildRs => true,
            ProjectFileId::UserFile(_) => selected_path.ends_with(".rs"),
            _ => false,
        };
        // The file `display_code` was built for. Captured before the bottom diag
        // panel (rendered below) can switch `selected_file` on a diagnostic
        // click, so a queued scroll-to-line only fires once the editor actually
        // shows that file (next frame for a cross-file jump).
        let displayed_file = self.selected_file;

        // ── Panel 1: Code Editor (leftmost) ───────────────────────────────────
        // Cap the width so the editor can never starve the MCU Configurator
        // (the central panel, which has no width of its own and takes only what
        // the side panels leave).
        let avail = ui.available_width();
        // What the other two columns need. The tree's width is last frame's
        // (this panel is built first — see `AppIde::tree_width`); collapsed it
        // costs nothing. Floored at the editor's own minimum, so a window too
        // small for everyone still produces a usable cap instead of one below
        // `min_width`.
        let editor_max =
            (avail - crate::app::MCU_MIN_W - self.tree_width).max(crate::app::EDITOR_MIN_W);
        // Read before the closure borrows `self`.
        let collapsed = self.side_panels_collapsed;
        // The body is bound ONCE and then moved into whichever container runs
        // (only one arm executes, so a single `FnOnce` is fine). Collapsed, the
        // editor IS the central panel and fills the window naturally — no width
        // juggling, and the 70 % cap below simply doesn't apply.
        // NOTE: the body keeps its original indentation so this stays a small,
        // reviewable diff rather than a ~900-line reindent.
        let body = |ui: &mut egui::Ui| {
            // Header row
            self.show_editor_toolbar(ui, &display_code);

            ui.separator();

            // ── Diagnostics panel (bottom, manually resizable) ────
            // Its top Y bounds the editor region below, so the inline
            // diagnostic overlay can be clipped to what's actually visible.
            // `source_rewritten` is set when a Clippy "Fix"/"Apply all"
            // rewrote a source buffer in-place — we then refresh display_code
            // (captured above, before the panel ran) so the editor shows the
            // change and the write-back below doesn't revert it.
            let mut source_rewritten = false;
            let diag_panel_top = self.show_editor_diag_panel(ui, &mut source_rewritten);
            if source_rewritten {
                match displayed_file {
                    ProjectFileId::MainRs => display_code = self.generated_code.clone(),
                    ProjectFileId::UserFile(i) => {
                        if let Some((_, c)) = self.project_tree.user_src_files.get(i) {
                            display_code = c.clone();
                        }
                    }
                    _ => {}
                }
            }
            self.show_code_view(
                ui,
                crate::app::EditorSlot::Main,
                displayed_file,
                display_code,
                &display_syntax,
                selected_is_manifest,
                respace_on_format,
                diag_panel_top,
            );
        };

        if collapsed {
            // The MCU Configurator is hidden, so the editor takes the central
            // slot and fills everything the Project tree (a right panel added
            // before this) leaves — no width juggling, and the 70 % cap below
            // doesn't apply.
            egui::CentralPanel::default().show(ui, body);
        } else {
            // `editor_max` (computed above) reserves the MCU zone's minimum and
            // the tree's current width. It replaced a flat 70 % of the window,
            // which said nothing about whether what was left could actually hold
            // a chip diagram: on a half-screen window the editor kept a width
            // dragged while maximised and squeezed the MCU zone to a strip.
            egui::Panel::left("code_editor")
                .resizable(true)
                .default_size(avail * 0.5)
                .min_size(crate::app::EDITOR_MIN_W)
                .max_size(editor_max)
                .show(ui, body);
        }
    }

    /// Scroll the editor vertically so the primary caret stays visible when it
    /// moves off-screen (keyboard navigation / selection). Only acts when the
    /// caret actually moved this frame, so it never fights the user scrolling
    /// the wheel away from the caret. See the call site for why egui's built-in
    /// caret-follow doesn't reach the editor's outer (vertical) ScrollArea.
    ///
    /// "Moved" is decided in BUFFER space. The galley index is the projection's,
    /// and a fold toggled above the caret shifts that index by every hidden
    /// character without the caret going anywhere — comparing projection
    /// indices read that as a move and jumped the view back down to a caret
    /// the user had scrolled away from, right after they folded a block.
    fn scroll_caret_into_view(
        &mut self,
        ui: &egui::Ui,
        editor_resp: &egui::text_edit::TextEditOutput,
        editor_id: &str,
        visible: egui::Rect,
        fold_map: &fold::FoldMap,
        follow: bool,
    ) {
        let Some(range) = editor_resp.state.cursor.char_range() else {
            return;
        };
        // Clamp to the galley: a stale cursor (e.g. left past the end of a file
        // that just shrank from a Clippy "Fix") would otherwise make
        // `pos_from_cursor` index out of bounds and panic.
        let primary = range
            .primary
            .index
            .0
            .min(editor_resp.galley.text().chars().count());
        // Only follow when the caret moved (typing / arrows / selection), so the
        // user can still freely scroll the wheel while the caret sits off-screen.
        let in_buffer = fold_map.to_buffer(primary);
        let moved = self.ed.last_caret_idx != Some(in_buffer);
        self.ed.last_caret_idx = Some(in_buffer);
        // `!follow`: a fold anchor just placed the view. Whatever moved the
        // caret was the fold (clamped out of a hidden body onto its header),
        // not the user, and following it would nudge the pinned header.
        if !moved || !follow {
            return;
        }

        // Caret rectangle in screen space (galley_pos already includes the
        // current scroll offset).
        let caret = editor_resp
            .galley
            .pos_from_cursor(egui::text::CCursor::new(primary));
        let caret_top = editor_resp.galley_pos.y + caret.min.y;
        let caret_bottom = editor_resp.galley_pos.y + caret.max.y;
        let margin = caret.height().max(8.0); // keep ~one line of context

        // How far (and which way) to move so the caret sits inside the band.
        let delta = if caret_top < visible.top() + margin {
            caret_top - (visible.top() + margin) // negative → scroll up
        } else if caret_bottom > visible.bottom() - margin {
            caret_bottom - (visible.bottom() - margin) // positive → scroll down
        } else {
            0.0
        };
        if delta == 0.0 {
            return;
        }

        // The outer vertical ScrollArea egui_code_editor builds with
        // `id_salt("{id}_outer_scroll")` on this same `ui`.
        let scroll_id =
            crate::app::helpers::scroll_id::scroll_area_id(ui, format!("{editor_id}_outer_scroll"));
        if let Some(mut state) = egui::containers::scroll_area::State::load(ui.ctx(), scroll_id) {
            state.offset.y = (state.offset.y + delta).max(0.0);
            state.store(ui.ctx(), scroll_id);
            ui.ctx().request_repaint();
        }
    }

    /// The code view itself: the editor widget, every shortcut and overlay
    /// around it, and the write-back — everything between the toolbar and the
    /// rename popup.
    ///
    /// Extracted so BOTH editors run it. The main panel wraps it in its own
    /// chrome (toolbar, bottom diagnostics panel); the Reference tab calls it
    /// directly, inside [`AppIde::with_editor`], so every `self.ed.*` in here
    /// means that view's popups, caret and find bar.
    ///
    /// `displayed_file` is the file this view is showing — NOT `selected_file`,
    /// which belongs to the main editor and which the bottom diagnostics panel
    /// may have changed earlier in the same frame.
    ///
    /// `diag_panel_top` is the y the editor region is clipped to: the top of the
    /// main editor's diagnostics panel, or simply the bottom of the available
    /// space for a view that has none.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn show_code_view(
        &mut self,
        ui: &mut egui::Ui,
        slot: crate::app::EditorSlot,
        displayed_file: ProjectFileId,
        mut display_code: String,
        display_syntax: &Syntax,
        selected_is_manifest: bool,
        respace_on_format: bool,
        diag_panel_top: Option<f32>,
    ) {
        let is_main = slot == crate::app::EditorSlot::Main;

        // Use a unique id per file so egui's TextEditState (galley,
        // cursor, undo stack) is never shared between files.
        // A fixed id caused the editor to keep the previous file's
        // rendered galley when switching to a new file.
        // Per FILE (egui keys its `TextEditState` — galley, caret, undo — by
        // widget id, and a fixed id kept the previous file's galley) AND per
        // VIEW, so the two editors never share a caret over one buffer.
        let id_prefix = if is_main {
            "code_editor"
        } else {
            "reference_editor"
        };
        let editor_id: String = match &displayed_file {
            ProjectFileId::UserFile(i) => {
                let path = self
                    .project_tree
                    .user_src_files
                    .get(*i)
                    .map(|(p, _)| p.as_str())
                    .unwrap_or("?");
                format!("{id_prefix}:user:{path}")
            }
            ProjectFileId::MainRs => format!("{id_prefix}:main_rs"),
            ProjectFileId::CargoToml => format!("{id_prefix}:cargo_toml"),
            ProjectFileId::CargoConfig => format!("{id_prefix}:cargo_config"),
            ProjectFileId::MemoryX => format!("{id_prefix}:memory_x"),
            ProjectFileId::BuildRs => format!("{id_prefix}:build_rs"),
            ProjectFileId::GitIgnore => format!("{id_prefix}:gitignore"),
        };

        // ── LSP completion: pre-editor key consumption ───────────────
        // Consume navigation / accept keys BEFORE show_with_completer
        // so the built-in Completer never sees them when our popup is open.
        //
        // Mouse clicks on popup items set `completion_pending_insert` last
        // frame; apply them here so the same accept path is used for both
        // keyboard and mouse.
        // -- Keyboard-scope gate -------------------------------
        // Every editor shortcut below is consumed GLOBALLY, before
        // any widget sees the key. That was fine while this was the
        // only text field that mattered; with another text input
        // focused (the second Reference editor, the Git commit box,
        // the rename popup, the terminal) those consumes fired on
        // the WRONG file and stole the keystroke from the focused
        // field: Ctrl+Space in the second editor opened the MAIN
        // editor's completion, and accepting it would have edited
        // the other file.
        //
        // Scope rule: shortcuts stay active unless some OTHER
        // text-editing widget owns the keyboard. "Is a text edit"
        // is detected by the focused id having a stored
        // `TextEditState`; a focused button, or no focus at all,
        // keeps the shortcuts live (the pre-second-editor status
        // quo). The editor's and the find bar's own focus come
        // from LAST frame (those widgets render after this code) -
        // a one-frame lag on a focus change is invisible here.
        // The second editor owns the keyboard: shortcuts must not fire
        // here, but Ctrl+Space is FORWARDED to it (it renders later in
        // the frame, so it cannot consume the key itself — the event is
        // swallowed below before any TextEdit sees it).
        //
        // Only while the Reference view is still drawing: its pass is the one
        // place that refreshes the flag, so once it stops running (MCU zone
        // collapsed, reference file gone) a `true` would otherwise stick and
        // keep the main editor's keyboard switched off for good.
        let frame = ui.ctx().cumulative_frame_nr();
        if !is_main {
            self.reference_drawn_frame = Some(frame);
        }
        let reference_live = kbd_scope::reference_view_live(self.reference_drawn_frame, frame);
        let reference_owns_kbd = self.reference_was_focused && reference_live;
        // The Definition tab owns the keyboard after a click in it. Its rows are
        // labels and take no focus, so a click there leaves NOTHING focused —
        // and the fallback below then handed F12 to this editor, which jumped
        // from its own caret. See `definition_keeps_kbd` for when it lets go.
        let definition_live = kbd_scope::view_live(self.def_drawn_frame, frame);
        // Every shortcut below is gated on this, and `&&` short-circuits —
        // so when it is false `consume_key` is never called and the event
        // SURVIVES for the other view, which runs later in the frame. That
        // is what lets one body serve two editors without either one eating
        // the other's keys. (Ctrl+Space is the exception: it is swallowed
        // unconditionally to silence the built-in completer, so it has to be
        // forwarded by hand — see below.)
        // No text field holds the keyboard: nothing focused, or a button / menu
        // item is. A context-menu action lands exactly here.
        let no_text_focus = match ui.ctx().memory(|m| m.focused()) {
            None => true,
            Some(fid) => egui::TextEdit::load_state(ui.ctx(), fid).is_none(),
        };
        if is_main {
            self.def_owns_kbd =
                kbd_scope::definition_keeps_kbd(self.def_owns_kbd, definition_live, no_text_focus);
        }
        let definition_owns_kbd = is_main && self.def_owns_kbd;
        // Two scopes. `nav_kbd_active` is for keys that open or move through
        // things — the find bar, Ctrl+Tab, F3, F8 — and stays on while the
        // Definition tab holds the keyboard: someone reading a definition may
        // well want to search the project for it. `editor_kbd_active` is for
        // keys that act at THIS editor's caret or edit its text, and those must
        // not fire into a file the user is not looking at.
        let nav_kbd_active = if is_main {
            !reference_owns_kbd
                && (self.ed.editor_was_focused || self.ed.find.had_focus || no_text_focus)
        } else {
            // The second view owns the keyboard exactly when it is focused.
            // The "nobody is focused" fallback above belongs to the main
            // editor alone, or both would claim the same keystroke.
            reference_owns_kbd || self.ed.find.had_focus
        };
        let editor_kbd_active = nav_kbd_active && !definition_owns_kbd;
        // Close a popup whose OWNER no longer holds the keyboard: it
        // would eat Enter/Escape for a caret the user has left.
        //
        // Scoped BY OWNER, not by `editor_kbd_active` — that is THIS pass's
        // scope, and both passes run every frame. See `owner_lost_keyboard`
        // for the two ways reading the wrong one killed a popup one frame
        // after it opened.
        let owner_lost_kbd = kbd_scope::owner_lost_keyboard(
            self.completion_owner,
            is_main,
            editor_kbd_active,
            reference_owns_kbd,
            reference_live,
            no_text_focus,
        );
        if owner_lost_kbd {
            // The owner's popup, which is not necessarily this editor's:
            // each view keeps its own list now, so closing `self.ed`'s here
            // would leave the Reference editor's open and eating Enter.
            let owner = self.completion_owner;
            let ed = self.ed_of(owner);
            if ed.completion_open {
                crate::lsp::debug_log(&format!(
                    "COMPLETION_CLOSE reason=owner-lost-keyboard owner={owner:?} main_pass={is_main}"
                ));
            }
            ed.completion_open = false;
            ed.completion_note = None;
        }
        // No cross-view arbitration on these three: each list belongs to the
        // view that opened it, so a view closes its own when it loses the keys.
        if !editor_kbd_active {
            self.ed.cargo_complete.open = false;
            self.ed.code_action_popup_open = false;
            self.ed.add_dep.open = false;
        }

        // A deferred accept — a mouse click in the popup, or a keyboard accept
        // this same block routed here from the other view's pass. Claimed only
        // by the view that OWNS the popup, or the choice would land in the
        // wrong file.
        let mut lsp_accepted: Option<lsp::CompletionItem> = if self.completion_owner == slot {
            self.ed.completion_pending_insert.take()
        } else {
            None
        };
        if lsp_accepted.is_some() {
            self.ed.completion_open = false;
        }

        // Popup nav/accept keys are consumed HERE for both editors —
        // this block runs before either renders, and `lsp_accepted` /
        // `completion_pending_insert` carry the choice to the owner.
        // Read the OWNER's popup: these keys are consumed here for BOTH
        // editors, because this block runs before either one renders and the
        // Reference editor's `TextEdit` would otherwise see Enter first.
        let owner = self.completion_owner;
        // Never in the Reference pass for the MAIN editor's list. The main pass
        // always runs first and already consumed what was meant for it; by the
        // time the Reference pass runs, the main `TextEdit` has acted on every
        // key left over, so taking one here would use the same Enter twice —
        // a newline in the buffer AND an accept of a list that only just
        // arrived. (The main pass does serve the Reference list: that view
        // renders later, and its `TextEdit` must not see the key first.)
        let serves_owner = owner == slot || (is_main && owner == crate::app::EditorSlot::Reference);
        if serves_owner && self.ed_of(owner).completion_open {
            let has_items = !self.ed_of(owner).completion_filtered_items.is_empty();
            // Escape closes the popup whether it holds rows or is still the
            // "rust-analyzer…" spinner. Gating it on rows too left a slow answer
            // impossible to dismiss: the list popped up anyway once it arrived.
            if !has_items {
                ui.input_mut(|inp| {
                    if inp.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                        crate::lsp::debug_log("COMPLETION_CLOSE reason=escape-while-waiting");
                        let ed = if owner == self.ed_slot {
                            &mut self.ed
                        } else {
                            &mut self.ed_ref
                        };
                        ed.completion_open = false;
                    }
                });
            } else {
                let count = self.ed_of(owner).completion_filtered_items.len();
                ui.input_mut(|inp| {
                    // `ed_of` inlined: the borrow has to span the whole block.
                    // It names the OWNER's list, which in this view's pass may
                    // be the parked one.
                    let ed = if owner == self.ed_slot {
                        &mut self.ed
                    } else {
                        &mut self.ed_ref
                    };
                    if inp.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                        crate::lsp::debug_log("COMPLETION_CLOSE reason=escape");
                        ed.completion_open = false;
                    } else if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        // Clamp against the FILTERED count so selection never
                        // goes out of the visible list.
                        ed.completion_sel = (ed.completion_sel + 1).min(count.saturating_sub(1));
                    } else if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        ed.completion_sel = ed.completion_sel.saturating_sub(1);
                    } else if inp.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                        || inp.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    {
                        // Use the filtered list — guaranteed same items as shown.
                        let sel = ed.completion_sel.min(count.saturating_sub(1));
                        if let Some(item) = ed.completion_filtered_items.get(sel).cloned() {
                            // Apply here only if the MAIN editor owns
                            // the popup; otherwise hand it to the
                            // Reference editor through the same
                            // deferred slot mouse accepts already use —
                            // it renders later this frame and applies it
                            // to ITS buffer.
                            if owner == slot {
                                // This view owns the list and draws next —
                                // apply it here.
                                lsp_accepted = Some(item);
                            } else {
                                // The other view owns it; park the choice where
                                // its own pass will claim it.
                                ed.completion_pending_insert = Some(item);
                            }
                        }
                        ed.completion_open = false;
                    }
                });
            }
        }
        // ── Cargo.toml completion popup: navigation / accept keys ────
        // Same key set as the LSP popup; consumed before the editor so
        // Enter/Tab don't reach the TextEdit. Accept is deferred through
        // `cargo_complete.pending` (the same path mouse clicks use).
        // Escape is taken even with no rows: the popup can now be just a note
        // ("searching…", "type 3+ characters"), and one Escape cannot close.
        // Only in a manifest: a flag left over from Cargo.toml must not eat the
        // Escape meant for a chooser open in a `.rs` file.
        //
        // And never while the Find bar is typing: its Enter means "next match",
        // and the popup would take it as "insert the highlighted crate" at a
        // caret the user is not even looking at.
        if self.ed.find.had_focus {
            self.ed.cargo_complete.open = false;
        }
        if selected_is_manifest && self.ed.cargo_complete.open {
            let count = self.ed.cargo_complete.items.len();
            ui.input_mut(|inp| {
                if inp.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.ed.cargo_complete.open = false;
                } else if count == 0 {
                    // No rows: navigation and accept keys belong to the editor.
                } else if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.ed.cargo_complete.sel =
                        (self.ed.cargo_complete.sel + 1).min(count.saturating_sub(1));
                    self.ed.cargo_complete.moved = true;
                } else if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.ed.cargo_complete.sel = self.ed.cargo_complete.sel.saturating_sub(1);
                    self.ed.cargo_complete.moved = true;
                } else if inp.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                    || inp.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                {
                    let sel = self.ed.cargo_complete.sel.min(count.saturating_sub(1));
                    if let Some(item) = self.ed.cargo_complete.items.get(sel) {
                        self.ed.cargo_complete.pending = Some(item.action.clone());
                    }
                    self.ed.cargo_complete.open = false;
                }
            });
        }
        // ── Code-action chooser popup: nav / accept keys ─────────────
        // MUST run BEFORE the editor: the popup renders after the editor
        // (`show_code_action_popup`), so if Enter were consumed only there
        // the editor would already have inserted a newline into the code
        // (splitting the identifier the assist targets). Consuming here
        // keeps the accept clean. A choice is deferred to next frame's
        // `poll_code_actions` (so the edit applies at frame top).
        // ── "Add dependency" crate chooser: same rule, and FIRST ─────
        // It opens from the code-action popup and replaces it, so it must
        // claim the keys before that block can see them.
        if self.ed.add_dep.open && !self.ed.add_dep.items.is_empty() {
            let count = self.ed.add_dep.items.len();
            let busy = self.ed.add_dep.fetch.is_some();
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.ed.add_dep.open = false;
                    self.ed.add_dep.fetch = None;
                    self.ed.add_dep.note = None;
                } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.ed.add_dep.sel = (self.ed.add_dep.sel + 1).min(count - 1);
                } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.ed.add_dep.sel = self.ed.add_dep.sel.saturating_sub(1);
                } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) && !busy {
                    // Consumed even while a fetch is in flight, so Enter
                    // does not fall through and split the line underneath.
                    self.ed.add_dep.choice = Some(self.ed.add_dep.sel.min(count - 1));
                }
            });
        }
        // The extra "Add dependency" row means the list can be one longer
        // than `code_actions`, and non-empty when `code_actions` is empty.
        let action_rows =
            self.ed.code_actions.len() + usize::from(self.ed.code_action_add_dep.is_some());
        if self.ed.code_action_popup_open && !self.ed.add_dep.open && action_rows > 0 {
            let count = action_rows;
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.ed.code_action_popup_open = false;
                } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.ed.code_action_sel = (self.ed.code_action_sel + 1).min(count - 1);
                } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.ed.code_action_sel = self.ed.code_action_sel.saturating_sub(1);
                } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                    self.ed.code_action_choice = Some(self.ed.code_action_sel.min(count - 1));
                }
            });
        }
        // Go-to chooser (Ctrl+F12 with several implementations). Same rule as
        // the code-action list: Enter is consumed HERE, before the editor, or it
        // splits the line under the caret instead of navigating.
        let picker_rows = self
            .impl_picker
            .as_ref()
            .filter(|p| p.slot == slot)
            .map(|p| p.targets.len())
            .unwrap_or(0);
        if picker_rows > 0 {
            let mut take: Option<usize> = None;
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.impl_picker = None;
                } else if let Some(p) = self.impl_picker.as_mut() {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        p.sel = (p.sel + 1).min(picker_rows - 1);
                    } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        p.sel = p.sel.saturating_sub(1);
                    } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                        take = Some(p.sel.min(picker_rows - 1));
                    }
                }
            });
            if let Some(i) = take {
                self.take_impl_picker_choice(i);
            }
        }
        // ── Inline type hint: Tab accepts (inserts the inferred type) ─
        // Only when a ghost hint is showing, no completion / code-action
        // popup is up (their key handling ran above and would have
        // consumed it first), AND the caret sits on the hint's line
        // at/after the name — so Tab still inserts a tab when indenting at
        // line start. Consumed here so the editor doesn't also type a tab;
        // the edit is applied at frame top next `init_frame`.
        let hint_pos = self.ed.inlay_hint.as_ref().map(|h| (h.line, h.character));
        if let Some((hint_line, hint_char)) = hint_pos {
            let popup_up = self.ed.completion_open
                || self.ed.code_action_popup_open
                || self.ed.add_dep.open
                || (self.ed.cargo_complete.open && !self.ed.cargo_complete.items.is_empty());
            let caret_ok = self.ed.last_caret_idx.map_or(false, |idx| {
                let (l, c) = crate::editor::gui::text_pos::lsp_cursor_pos(&display_code, idx);
                l == hint_line && c >= hint_char
            });
            if editor_kbd_active
                && !popup_up
                && caret_ok
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
            {
                self.ed.inlay_accept_pending = true;
            }
        }
        // Detect Ctrl+Space BEFORE the editor so egui doesn't pass it
        // to the TextEdit as a literal character.
        // These flags are `mut` so the right-click context menu (handled
        // after the editor renders) can drive the exact same code paths.
        //
        // NOT `consume_key`: holding the shortcut delivers key-REPEAT
        // events every frame, and each one used to re-fire the
        // completion request (clearing the items → the popup flickered
        // open/closed). All Ctrl+Space events are consumed here, but
        // only the initial (non-repeat) press triggers.
        let mut ctrl_space_pressed = false;
        // Forwarded to the Reference editor's own render pass.
        let mut ref_ctrl_space = false;
        // The second view cannot read this key itself: the main panel runs
        // first and swallows it (`false` in the retain below) so the crate's
        // built-in keyword popup never sees it. So the main pass decides who
        // it was for and parks it; the second view collects it here.
        if !is_main {
            ctrl_space_pressed = std::mem::take(&mut self.reference_ctrl_space);
        }
        ui.input_mut(|i| {
            if !is_main {
                return;
            }
            i.events.retain(|e| match e {
                egui::Event::Key {
                    key: egui::Key::Space,
                    pressed: true,
                    repeat,
                    modifiers,
                    ..
                } if modifiers.ctrl => {
                    if !*repeat {
                        if editor_kbd_active {
                            ctrl_space_pressed = true;
                        } else if reference_owns_kbd {
                            ref_ctrl_space = true;
                        }
                    }
                    false // swallow presses AND repeats
                }
                _ => true,
            });
        });
        if is_main {
            self.reference_ctrl_space = ref_ctrl_space;
        }
        // Ctrl+/ → toggle line comments on the selection (consumed before
        // the editor so `/` is never typed into the text).
        // Ctrl+Shift+/ → wrap the selected lines in ONE `/* … */`.
        // Consumed BEFORE the plain Ctrl+/ below: `consume_key` is lenient
        // about Shift (same trap as Ctrl+Shift+Up vs Ctrl+Up), so checking
        // the Shift variant second would let the line-comment shortcut
        // swallow this key-down first.
        let mut ctrl_shift_slash_pressed = editor_kbd_active
            && ui.input_mut(|i| {
                let cs = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
                // BOTH keys, because Shift+`/` is `?` on most layouts and
                // egui reports the SHIFTED character: `Key::Slash` alone
                // matched nothing, which is why the shortcut did nothing at
                // all. Which of the two arrives depends on the keyboard, so
                // either counts. Not `||` on one line — both must be
                // consumed, or the unconsumed one is typed into the buffer.
                let q = i.consume_key(cs, egui::Key::Questionmark);
                let sl = i.consume_key(cs, egui::Key::Slash);
                q || sl
            });
        let mut ctrl_slash_pressed = editor_kbd_active
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Slash));
        // Ctrl+Shift+Q → collapse every function body, or expand everything
        // if anything is folded. Consumed here with the other shortcuts and
        // applied after the render (`toggle_fold_all`), where the galley of
        // the current projection exists to anchor the view against — it lands
        // next frame, exactly like a click on a fold caret.
        let fold_all_pressed = editor_kbd_active
            && ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Q)
            });
        // Ctrl+Shift+Up / Down → multi-cursor add/undo (see
        // `multi_cursor` module docs). Consumed BEFORE Ctrl+Up/Down
        // below: `consume_key` is lenient about Shift (see that
        // comment), so checking the Shift variant first stops the
        // plain Ctrl+Up/Down (move line) shortcut from also matching
        // the same key-down event.
        let mc_up_pressed = editor_kbd_active
            && ui.input_mut(|i| {
                i.consume_key(
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::ArrowUp,
                )
            });
        let mc_down_pressed = editor_kbd_active
            && ui.input_mut(|i| {
                i.consume_key(
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::ArrowDown,
                )
            });
        // A PLAIN arrow key moves every caret, not just the primary.
        //
        // Peeked, never consumed: egui's TextEdit needs the very same
        // event to move the primary — we only mirror it onto the
        // extras. Checked AFTER the Ctrl+Shift consumes above, so an
        // "add caret" press is already gone from the queue.
        //
        // Shift+arrow extends every caret's OWN selection; a plain arrow
        // moves and collapses. Ctrl-modified arrows are excluded — those
        // are add-caret (Ctrl+Shift+Up/Down) and move-line (Ctrl+Up/Down),
        // which mean something else entirely.
        let mc_caret_move = ui.input(|i| {
            use multi_cursor::CaretMove;
            let m = i.modifiers;
            if m.ctrl || m.command || m.alt {
                return None;
            }
            for (key, dir) in [
                (egui::Key::ArrowLeft, CaretMove::Left),
                (egui::Key::ArrowRight, CaretMove::Right),
                (egui::Key::ArrowUp, CaretMove::Up),
                (egui::Key::ArrowDown, CaretMove::Down),
            ] {
                if i.key_pressed(key) {
                    return Some((dir, m.shift));
                }
            }
            None
        });
        let mc_caret_move = mc_caret_move.filter(|_| editor_kbd_active);
        // Escape drops the extra carets - skipped while a completion popup
        // is open, because dismissing that wins (it renders later in the frame
        // and would otherwise never see the key). The editor itself keeps its
        // focus on Escape: see `EDITOR_KEYS`.
        let escape_pressed_raw = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let popup_open = self.ed.completion_open || self.ed.cargo_complete.open;
        let mc_escape_pressed = editor_kbd_active && !popup_open && escape_pressed_raw;
        // Ctrl+Shift+Tab / Ctrl+Tab → MRU file switching (VS Code style:
        // hold Ctrl to walk the history, release to commit). Consumed
        // BEFORE the editor so Tab never inserts indentation. The Shift
        // variant must be checked first (consume_key is Shift-lenient).
        let mut cycle_prev_pressed = nav_kbd_active
            && ui.input_mut(|i| {
                i.consume_key(
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::Tab,
                )
            });
        let mut cycle_next_pressed = nav_kbd_active
            && !cycle_prev_pressed
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Tab));
        // Ctrl+Left / Ctrl+Right (+Shift = select) → word movement.
        // Consumed BEFORE the editor so egui's own word jump never
        // runs: it segments with UAX#29, where `:` is a MidLetter, so
        // `name:Type` is ONE word and the jump swallowed both sides
        // (see `word_select`). The Shift variants are checked first —
        // `consume_key` is Shift-lenient (see the multi-cursor note).
        let word_move: Option<(bool, bool)> = if !editor_kbd_active {
            None
        } else {
            ui.input_mut(|i| {
                let cs = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
                if i.consume_key(cs, egui::Key::ArrowRight) {
                    Some((true, true))
                } else if i.consume_key(cs, egui::Key::ArrowLeft) {
                    Some((false, true))
                } else if i.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowRight) {
                    Some((true, false))
                } else if i.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowLeft) {
                    Some((false, false))
                } else {
                    None
                }
            })
        };
        // Ctrl+Up / Ctrl+Down → move the selected lines up / down.
        let mut ctrl_up_pressed = editor_kbd_active
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowUp));
        let mut ctrl_down_pressed = editor_kbd_active
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowDown));
        // Ctrl+C state (peeked, not consumed — the editor still copies any
        // selection). A triple-click full-definition selection (header through
        // closing brace) copies on it. It no longer copies a hovered
        // diagnostic: that overwrote a selection the user meant to copy, and
        // is now a button in the error tooltip.
        let copy_requested = editor_kbd_active
            && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
        // Ctrl+Shift+X → cut the whole line(s) at the cursor/selection;
        // plain Ctrl+X keeps egui's native cut-the-*selection* behaviour.
        // egui maps BOTH to `Event::Cut` (Shift is ignored for the cut
        // shortcut) and may not deliver a `Key::X` at all — so distinguish
        // by the live Shift state: Shift held → strip the `Event::Cut` so
        // the native selection-cut doesn't fire, and do the whole-line cut
        // ourselves; no Shift → leave the native cut alone. The `consume_key`
        // is a fallback for platforms that send the key instead of a Cut.
        let mut cut_line_pressed = editor_kbd_active
            && ui.input_mut(|i| {
                let cut_event = i.events.iter().any(|e| matches!(e, egui::Event::Cut));
                let line = cut_event && i.modifiers.shift;
                if line {
                    i.events.retain(|e| !matches!(e, egui::Event::Cut));
                }
                let key =
                    i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::X);
                line || key
            });
        // Ctrl+D → duplicate the line(s) at the cursor / selection.
        let mut ctrl_d_pressed = editor_kbd_active
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::D));
        // Ctrl+U → toggle the case of the selection, or of the identifier
        // under the caret. Consumed even when there is nothing to toggle:
        // egui's own TextEdit binds Ctrl+U to "delete from the line start to
        // the selection end", and letting it through erased code.
        //
        // Not while the Find bar is typing: there the press would rewrite a
        // selection out of sight, so it stays with that field.
        //
        // Only a FIRST press acts. `consume_key` counts key repeats, and a held
        // toggle would flip at the repeat rate and stop on whichever case the
        // release happened to land.
        let mut toggle_case_pressed = editor_kbd_active
            && !self.ed.find.had_focus
            && ui.input_mut(|i| consume_first_press(i, egui::Modifiers::CTRL, TOGGLE_CASE_KEY));
        // Shift+Alt+F → re-indent the whole file by block nesting.
        // (Moved off Ctrl+Shift+F, which now opens project-wide search.)
        // Unlike the Ctrl-based shortcuts, Alt+Shift doesn't suppress the
        // character event, so egui also delivers `Event::Text("F")` — strip
        // it too, or the formatter would type an "F" into the code.
        let mut format_pressed = editor_kbd_active
            && ui.input_mut(|i| {
                let pressed =
                    i.consume_key(egui::Modifiers::ALT | egui::Modifiers::SHIFT, egui::Key::F);
                if pressed {
                    i.events.retain(
                        |e| !matches!(e, egui::Event::Text(t) if t.eq_ignore_ascii_case("f")),
                    );
                }
                pressed
            });
        // Ctrl+R → rename the symbol at the cursor project-wide.
        let mut ctrl_r_pressed = editor_kbd_active
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::R));
        // Ctrl+F12 → go to the IMPLEMENTATION of the symbol at the
        // cursor (the `impl … for …` site, where plain F12 on a trait
        // method lands on the trait's declaration). Consumed before
        // plain F12 so the Ctrl variant never falls through.
        let mut ctrl_f12_pressed = editor_kbd_active
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F12));
        // Ctrl+[ / Ctrl+] → select the innermost `{ … }` block around
        // the caret and copy it (refactored off the old implicit
        // trigger — selecting a `{`/`}` — which hijacked Ctrl+C).
        let mut select_block_pressed = editor_kbd_active
            && ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::CTRL, egui::Key::OpenBracket)
                    || i.consume_key(egui::Modifiers::CTRL, egui::Key::CloseBracket)
            });
        // Ctrl+Enter → rust-analyzer code actions (assists / quick-fixes)
        // at the cursor. Consumed before the editor so it never inserts
        // a newline. Ignored while the code-action popup is already open
        // (its own Enter handling wins there).
        // Ctrl+Alt+M — move the selected lines into a new function.
        //
        // NOT Ctrl+Alt+Insert, which cannot work on Windows in this stack: in
        // `egui-winit`, `is_copy_command` matches `ctrl && Key::Insert` and does
        // NOT look at alt, so the keystroke is turned into `Event::Copy` and
        // `return`s before any `Event::Key` is pushed. No amount of code here
        // recovers it — the key event never exists. (`Shift+Insert` goes the
        // same way, to `Event::Paste`.) `M` is what IntelliJ binds Extract
        // Method to anyway, and nothing rewrites it.
        //
        // Consumed here, before the editor, and gated on `editor_kbd_active` so
        // the OTHER view's pass can claim it when this one does not own the
        // keyboard.
        let mut extract_pressed = editor_kbd_active
            && !self.ed.extract.active
            && ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::ALT, EXTRACT_FN_KEY)
            });

        // A code-action popup flagged open with NOTHING to show is a latch with
        // no way out: the renderer returns early (so you see nothing), the
        // nav block is skipped (so Escape cannot reach it), and the gate below
        // then refuses every Ctrl+Enter for the rest of the session. Whatever
        // put it in that state, it cannot be a state we stay in.
        if self.ed.code_action_popup_open
            && self.ed.code_actions.is_empty()
            && self.ed.code_action_add_dep.is_none()
        {
            self.ed.code_action_popup_open = false;
        }
        // Consumed whether or not it can act, and REFUSED OUT LOUD. A shortcut
        // that silently does nothing is indistinguishable from a broken one —
        // which is exactly how this arrived as a bug report with nothing to go
        // on.
        let ctrl_enter_raw = editor_kbd_active
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter));
        let blocked = if self.ed.add_dep.open {
            Some("a crate chooser is open - Esc first")
        } else if self.ed.code_action_popup_open {
            Some("the action list is open - Esc first")
        } else if self.ed.code_action_in_flight {
            Some("still waiting on rust-analyzer")
        } else {
            None
        };
        if ctrl_enter_raw {
            if let Some(why) = blocked {
                self.set_status_msg(format!("Ctrl+Enter: {why}"));
            }
        }
        let ctrl_enter_pressed = ctrl_enter_raw && blocked.is_none();
        // F12 → show the definition of the symbol at the cursor.
        let mut f12_pressed = editor_kbd_active
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F12));

        // Find / Replace bar shortcuts (consumed before the editor so the
        // keys never reach the TextEdit). Shift variants are checked first
        // so Ctrl+Shift+F isn't swallowed by the plain Ctrl+F branch.
        // Replace opens PRE-FILLED with the identifier under the cursor
        // (query searches for it, replace field starts from it + gets
        // focus) — quick rename of the symbol you're on.
        //
        // The word is read from last frame's caret and the text as it stands
        // HERE, before the editor renders, but only by the actions that use it.
        // The view's line index keeps that text for the context-menu arms
        // further down, after the editor may have changed `display_code`, and
        // an unchanged text costs one comparison instead of a whole-file scan.
        let word_src = self
            .ed
            .last_caret_idx
            .map(|idx| (self.ed.line_index.get(&display_code), idx));
        let word_under_cursor = || {
            word_src
                .as_ref()
                .map(|(text, idx)| rename::identifier_at(text.text(), *idx))
                .unwrap_or_default()
        };
        if nav_kbd_active {
            ui.input_mut(|i| {
                use find_replace::FindMode as M;
                let ctrl = egui::Modifiers::CTRL;
                let ctrl_shift = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
                if i.consume_key(ctrl_shift, egui::Key::F) {
                    self.ed.find.open_with(M::FindProject);
                } else if i.consume_key(ctrl_shift, egui::Key::H) {
                    self.ed
                        .find
                        .open_replace_with_word(M::ReplaceProject, &word_under_cursor());
                } else if i.consume_key(ctrl, egui::Key::F) {
                    self.ed.find.open_with(M::FindFile);
                } else if i.consume_key(ctrl, egui::Key::H) {
                    self.ed
                        .find
                        .open_replace_with_word(M::ReplaceFile, &word_under_cursor());
                }
            });
        }

        // Ctrl + `+` / `-` / `0` → zoom the editor text in / out / reset.
        // (egui's global keyboard zoom is disabled in `app::new`, so these
        // reach us here.) `consume_key` matches Shift loosely, so Ctrl++
        // (= Ctrl+Shift+=) is caught by the `Plus` arm.
        // Consumed ONLY while the pointer is over the editor panel — the
        // Structure diagram has its own Ctrl+± zoom, routed by hover.
        if ui.rect_contains_pointer(ui.max_rect()) {
            ui.input_mut(|i| {
                let cmd = egui::Modifiers::COMMAND;
                if i.consume_key(cmd, egui::Key::Num0) {
                    self.editor_font_size = DEFAULT_EDITOR_FONT_SIZE;
                } else if i.consume_key(cmd, egui::Key::Plus)
                    || i.consume_key(cmd, egui::Key::Equals)
                {
                    self.editor_font_size = (self.editor_font_size + 1.0).min(MAX_EDITOR_FONT_SIZE);
                } else if i.consume_key(cmd, egui::Key::Minus) {
                    self.editor_font_size = (self.editor_font_size - 1.0).max(MIN_EDITOR_FONT_SIZE);
                }
            });
        }

        // Find / Replace bar, drawn above the editor when open. Renders
        // before the editor-height calc so the editor sizes below it.
        // F3 / Shift+F3 step the find bar's matches — the keyboard twins of its
        // Previous / Next buttons.
        //
        // Consumed HERE, not inside the bar: this is where `editor_kbd_active`
        // lives, and the `&&` short-circuit is what leaves the key intact for
        // the OTHER editor when this one does not own the keyboard.
        //
        // Shift FIRST. `consume_key` is lenient about Shift (the same trap as
        // Ctrl+Shift+/ vs Ctrl+/), so checking the plain key first would let it
        // swallow the shifted press and Shift+F3 would step forwards.
        let (mut find_prev, mut find_next) = (false, false);
        if nav_kbd_active && self.ed.find.can_step() {
            ui.input_mut(|i| {
                find_prev = i.consume_key(egui::Modifiers::SHIFT, egui::Key::F3);
                find_next = !find_prev && i.consume_key(egui::Modifiers::NONE, egui::Key::F3);
            });
        }

        // F8 / Shift+F8 step the ERROR list — the keyboard twins of clicking one
        // of its rows. Consumed here for the same two reasons F3 is: this is
        // where `editor_kbd_active` lives, and the `&&` short-circuit leaves the
        // key intact for the other editor when this one has not got the
        // keyboard. Shift FIRST, same `consume_key` leniency trap.
        //
        // The direction travels down to `handle_editor_completion`, which is
        // where the error rows and the caret position both already exist.
        //
        // Not gated on the file HAVING errors, unlike F3's `can_step()`: the
        // count is not known until the rows are built downstream, and F8 is
        // bound nowhere else in the app (the debugger took F5 / F10 / F11), so
        // swallowing it on a clean file costs nothing.
        let mut err_step: Option<bool> = None;
        if nav_kbd_active {
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::SHIFT, error_list::ERROR_STEP_KEY) {
                    err_step = Some(false);
                } else if i.consume_key(egui::Modifiers::NONE, error_list::ERROR_STEP_KEY) {
                    err_step = Some(true);
                }
            });
        }
        self.show_find_replace_bar(ui, &mut display_code, displayed_file, find_next, find_prev);

        // Size the editor to fill the height left over after the
        // (resizable) diagnostics panel, so dragging that panel's handle
        // grows/shrinks the code area in lock-step.  `available_height`
        // here is already the space remaining below the toolbar and
        // above the bottom diagnostics panel.
        // The (zoomable) editor font, captured before the mutable
        // `self.ed.completer` borrow below. `row_h` tracks it so the min-row
        // estimate stays right as the user zooms.
        let font_size = self.editor_font_size;
        let row_h = ui
            .fonts_mut(|f| f.row_height(&egui::FontId::monospace(font_size)))
            .max(1.0);
        let editor_rows = (((ui.available_height() - 10.0) / row_h).floor() as usize).max(3);

        // The on-screen editor region. `available_rect_before_wrap` does
        // NOT exclude the bottom panel (egui only moves the cursor, not
        // max_rect), so the editor's scroll area actually overflows under
        // the panel. Bound the bottom explicitly to the diagnostics panel's
        // top so the inline overlay can't paint over (or into) it.
        let editor_clip = {
            let mut r = ui.available_rect_before_wrap();
            if let Some(top) = diag_panel_top {
                r.max.y = r.max.y.min(top);
            }
            r
        };

        // Rust files (main.rs / user src / build.rs / memory.x) use our
        // lifetime-aware renderer so `'a` doesn't spill the string colour;
        // the `#`-comment config files (Cargo.toml/.cargo/config/.gitignore)
        // keep the stock CodeEditor. Both return a `TextEditOutput`.
        let is_rust_file = !matches!(
            displayed_file,
            ProjectFileId::CargoToml | ProjectFileId::CargoConfig | ProjectFileId::GitIgnore
        ) && !selected_is_manifest;
        // While our LSP completion popup is open (or Ctrl+Space was just
        // pressed to open it), hide the crate's built-in keyword popup so
        // the two don't overlap — the LSP popup is the one that wins.
        // On top of that, the keyword popup is DISABLED outright (user
        // request 2026-07-05: no auto-popup while typing; completion is
        // on-demand via Ctrl+Space / `.` / `::`). Flip the const to
        // bring the auto keyword popup back — nothing was removed.
        const KEYWORD_COMPLETER_ENABLED: bool = false;
        let suppress_keyword_completer =
            !KEYWORD_COMPLETER_ENABLED || self.ed.completion_open || ctrl_space_pressed;

        // ── Live "usages" analysis (fade unused fn/struct/enum/const/…,
        // offer a references popup on the rest) — RA `documentSymbol` +
        // `references`, debounced, kept fresh only for the exact text
        // shown below. `usages_rel_path` is also reused after the editor
        // to place the "N refs" pill overlay.
        let usages_rel_path = crate::editor::gui::text_pos::selected_file_rel_path(
            &displayed_file,
            &self.project_tree.user_src_files,
        );
        // Idle re-sync: hand rust-analyzer this view's text once typing has
        // paused, so the inline overlay and the inferred-type hint come back
        // without a Ctrl+S. Placed BEFORE the usages pass below and before
        // `handle_editor_completion`, and on a shorter timer than either: a
        // version bump must never cancel the very requests this sync enables.
        //
        // `.rs` only — the predicate `LspState::did_change` itself applies.
        // `is_rust_file` also admits a user file like a `.md` or a library's
        // `memory.x`, which rust-analyzer never holds, so the sync could never
        // finish and kept a repaint scheduled for as long as it was shown.
        if let Some(rel) = &usages_rel_path {
            if is_rust_file && rel.ends_with(".rs") {
                self.tick_idle_sync(ui.ctx(), rel, &display_code);
            }
        }

        // Unused imports, computed ONCE and handed to both the fade below and
        // the pulse further down, so the two can never disagree about what is
        // unused. rust-analyzer does not report this lint natively — it arrives
        // through flycheck — so these come from the last Cargo Check / Clippy
        // run, guarded on the file's text still matching what was compiled.
        let unused_imports: Vec<(usize, usize)> = match &usages_rel_path {
            Some(rel) if is_rust_file => self.unused_import_spans(rel, &display_code),
            _ => Vec::new(),
        };
        let dead_ranges: Vec<(usize, usize)> = match &usages_rel_path {
            Some(rel) if is_rust_file => {
                self.tick_usages(rel, &display_code);
                let mut r = self.usages_dead_ranges(rel, &display_code);
                r.extend(unused_imports.iter().copied());
                r
            }
            _ => Vec::new(),
        };
        // Generic parameters the item declares without using, that an `impl`
        // of it does use: underlined instead of faded (they are live code).
        let underline_ranges: Vec<(usize, usize)> =
            self.generic_underline_ranges(&display_code).to_vec();

        // ── Code folding ──────────────────────────────────────────────
        // An edit and a fold cannot coexist: the editor writes back the
        // text it was GIVEN, which while folded is a projection missing
        // whole lines — writing that back would delete the hidden code.
        // So any keystroke that could modify the buffer unfolds this file
        // FIRST, before the editor renders, and the keystroke then lands on
        // the full text as usual. Everything below therefore runs either
        // fully folded (and read-only for this frame) or not folded at all.
        let fold_key = usages_rel_path.clone().filter(|_| is_rust_file);
        if let Some(rel) = &fold_key {
            // The line-op shortcuts CONSUMED their key events further up, so
            // `edit_pending` can no longer see them — their flags have to be
            // checked directly. Each one rewrites the buffer using caret
            // indices taken from the galley, which while folded belongs to
            // the projection: without this they would edit the wrong lines.
            let line_op = ctrl_shift_slash_pressed
                || ctrl_slash_pressed
                || ctrl_up_pressed
                || ctrl_down_pressed
                || cut_line_pressed
                || ctrl_d_pressed
                || format_pressed
                || mc_up_pressed
                || mc_down_pressed;
            // Ask the narrow question while folded: is some OTHER text
            // field focused? Everything else counts as ours. The find bar,
            // the rename popup and the Reference editor are exactly the
            // cases that must NOT unfold this file, and they all hold a
            // real `TextEditState` of their own. `editor_kbd_active` is a
            // wider heuristic and has read "somebody else owns the
            // keyboard" when nobody did — which, on the unfold path, left
            // the file untypable until the block was expanded by hand.
            let owns_kbd = if self.folds.contains_key(rel) {
                let other_text_field = ui.ctx().memory(|m| m.focused()).is_some_and(|fid| {
                    Some(fid) != self.ed.editor_widget_id
                        && egui::TextEdit::load_state(ui.ctx(), fid).is_some()
                });
                // The OTHER view owning the keys — which only the main pass can
                // be told about. In the Reference pass `reference_was_focused`
                // is this view's own focus, and reading it raw made a folded
                // Reference file skip the unfold before Copy/Cut/line ops. The
                // latched `reference_owns_kbd` also stops a stale flag (MCU zone
                // collapsed) from blocking the main editor's unfold.
                // The Definition tab likewise: a Ctrl+C there unfolded this
                // file and cleared its undo history.
                !(is_main && (reference_owns_kbd || definition_owns_kbd)) && !other_text_field
            } else {
                editor_kbd_active
            };
            // Ctrl+U counts only when it will really change the text: a press
            // on whitespace, or on `123`, must not expand every block for
            // nothing. Read from the STORED caret, which between frames is in
            // buffer space — the same text `display_code` holds.
            let case_edit = toggle_case_pressed
                && self.folds.contains_key(rel)
                && self
                    .fold_ids
                    .get(&editor_id)
                    .and_then(|&id| egui::TextEdit::load_state(ui.ctx(), id))
                    .is_some_and(|st| {
                        let carets =
                            self.case_toggle_carets(st.cursor.char_range(), displayed_file);
                        toggle_case::toggle_case(&display_code, &carets).is_some()
                    });
            let editing = owns_kbd && (line_op || case_edit || fold::edit_pending(ui));
            if editing && self.folds.contains_key(rel) {
                // The caret needs no translation here: between frames it is
                // always in BUFFER space (see the two conversion points
                // around the editor render below), so it already means the
                // same place in the full text.
                //
                // The undo history does need clearing. egui snapshots
                // whatever text it is shown, so it holds PROJECTIONS, and a
                // Ctrl+Z after the unfold would write one back over the
                // file, deleting every folded body at once. Cleared here
                // rather than at the end of the frame, because an undo
                // pressed on THIS frame would otherwise still find it.
                if let Some(id) = self.ed.editor_widget_id {
                    if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                        st.clear_undoer();
                        st.store(ui.ctx(), id);
                    }
                }
                self.folds.remove(rel);
            }
            // Ctrl+Shift+Q. After the unfold-on-edit check: a frame that both
            // edits and toggles should end up unfolded.
            if fold_all_pressed && !editing {
                self.ed.fold_all_requested = Some(rel.clone());
            }
        }
        // A request belongs to the frame's file; it does not wait around for
        // the next one that can consume it.
        if fold_key.is_none() {
            self.ed.fold_all_requested = None;
        }
        let mut fold_map = match &fold_key {
            Some(rel) => match self.folds.get(rel) {
                Some(set) if !set.is_empty() => fold::FoldMap::with_regions(
                    &display_code,
                    set,
                    &self.ed.fold_regions.get(&display_code),
                ),
                _ => fold::FoldMap::identity(&display_code),
            },
            None => fold::FoldMap::identity(&display_code),
        };
        let folded = !fold_map.is_identity();

        // Snapshot right before the editor mutates `display_code`, so
        // the multi-cursor replay below can diff exactly what the
        // editor itself changed this frame (typing / backspace / paste)
        // — not any earlier same-frame mutation like the find/replace
        // bar's own edits, above.
        let text_before_typing = display_code.clone();

        // While folded the editor is handed the PROJECTION, and only the
        // delta between what it was given and what it returns is adopted
        // (below); `display_code` keeps holding the real buffer throughout,
        // for the write-back and for every analysis below.
        let mut editor_text = if folded {
            fold_map.display().to_owned()
        } else {
            display_code.clone()
        };
        // ── Caret in, caret out ──────────────────────────────────────
        // The invariant: OUTSIDE the editor render the stored caret is in
        // BUFFER space, always. That is what the ~20 places below expect —
        // they pair `editor_resp.state.cursor` with `display_code` — and it
        // is also fold-independent, so a fold toggled between frames needs
        // no fixing up anywhere.
        //
        // The editor itself is the one exception: it is shown the
        // projection, so it has to be handed a projected caret, and the
        // one it gives back is projected too. Converted here and converted
        // straight back after the render.
        if folded {
            if let Some(id) = self.fold_ids.get(&editor_id).copied() {
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                    if let Some(r) = st.cursor.char_range() {
                        let to = |c: egui::text::CCursor| {
                            egui::text::CCursor::new(fold_map.to_display_clamped(c.index.0))
                        };
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            to(r.primary),
                            to(r.secondary),
                        )));
                        st.store(ui.ctx(), id);
                    }
                }
            }
        }
        // The outer scroll offset the galley is about to be laid out at. Read
        // BEFORE the render: when the content shrinks, egui's `ScrollArea::end`
        // clamps and stores a SMALLER one before this frame's code sees it, and
        // a fold correction added to that clamped value undershot — collapse-all
        // deep in a large file landed at the top of the file.
        let drawn_offset = {
            let scroll_id = crate::app::helpers::scroll_id::scroll_area_id(
                ui,
                format!("{editor_id}_outer_scroll"),
            );
            egui::containers::scroll_area::State::load(ui.ctx(), scroll_id)
                .map_or(0.0, |s| s.offset.y)
        };
        let mut editor_resp = if is_rust_file {
            crate::editor::gui::code_editor::show_rust_with_completer(
                ui,
                &mut editor_text,
                &ColorTheme::GRUVBOX,
                font_size,
                editor_rows,
                &display_syntax,
                &editor_id,
                &mut self.ed.completer,
                suppress_keyword_completer,
                crate::editor::gui::code_editor::Marks {
                    // Phase 3: the projection is editable. What comes back
                    // is never adopted wholesale — only its DELTA, mapped
                    // into the buffer below — so the hidden lines survive.
                    read_only: false,
                    dead: &fold_map.map_ranges(&dead_ranges),
                    underline: &fold_map.map_ranges(&underline_ranges),
                },
                fold_map.line_numbers(),
            )
        } else {
            // Config files (Cargo.toml, .cargo/config.toml, .gitignore)
            // on the stock editor. `show_with_completer` would drive the
            // crate's keyword popup unconditionally — which is how
            // Cargo.toml kept popping up a list of Rust keywords + words
            // from the file on EVERY character typed, ignoring the
            // `suppress_keyword_completer` decision the Rust path honours.
            // Inline the two completer calls instead, behind the same
            // flag. Cargo.toml keeps its OWN crate/version completion on
            // Ctrl+Space (`handle_cargo_completion`), untouched by this.
            let mut out = CodeEditor::default()
                .id_source(editor_id.clone())
                .with_rows(editor_rows)
                .with_fontsize(font_size)
                .with_theme(ColorTheme::GRUVBOX)
                .with_numlines(true)
                .show(ui, &mut display_code, &display_syntax);
            // The stock editor locks its focus with `lock_focus(true)` alone,
            // so Escape - the key that closes the Cargo.toml crate popup -
            // would still drop it and swallow everything typed after. Give it
            // the Rust path's keys; its TextEdit set its own filter earlier in
            // this pass, and this one wins at the next pass's `begin_pass`.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    out.response.id,
                    crate::editor::gui::code_editor::EDITOR_KEYS,
                )
            });
            // Set even when suppressed: the completer keys its popup to
            // this id, and a stale one from another editor would misplace
            // it the moment the flag flips back on.
            self.ed.completer.text_edit_id = Some(out.response.id);
            if !suppress_keyword_completer {
                self.ed.completer.handle_input(ui.ctx());
                self.ed
                    .completer
                    .show(&display_syntax, &ColorTheme::GRUVBOX, font_size, &mut out);
            }
            out
        };
        // Remembered for the NEXT frame: the unfold-on-edit rule needs this
        // id before the widget exists (see above).
        self.ed.editor_widget_id = Some(editor_resp.response.id);
        self.fold_ids
            .insert(editor_id.clone(), editor_resp.response.id);

        // Adopt what the editor produced — but ONLY on the Rust path, which
        // is the one handed `editor_text`. A config file (Cargo.toml,
        // .cargo/config.toml, .gitignore) goes through the stock `CodeEditor` in the
        // branch above, which edits `display_code` DIRECTLY; assigning
        // `editor_text` over it there wrote back the pre-edit clone and
        // erased every keystroke as it was typed.
        let mut folded_own_edit = false;
        // A delta was adopted or refused below: either way `display_code` or
        // this file's fold set changed, the two things `fold_map` is built from.
        let mut fold_inputs_changed = false;
        if is_rust_file && !folded {
            display_code = editor_text;
        } else if is_rust_file {
            // Folded: adopting the text would write the projection — the
            // file minus its hidden lines — over the buffer. Adopt the
            // DELTA instead, translated into buffer coordinates, and the
            // hidden lines are untouched. An undo arrives here as just
            // another delta, so it is correct for free.
            if let Some((ds, de, ins)) = fold::text_delta(fold_map.display(), &editor_text) {
                fold_inputs_changed = true;
                let (bs, be) = (fold_map.to_buffer(ds), fold_map.to_buffer(de));
                let chars: Vec<char> = display_code.chars().collect();
                let (bs, be) = (bs.min(chars.len()), be.min(chars.len()));
                // The projection is the buffer minus whole LINES, so a
                // range that grew in the translation is one that spans a
                // folded block: Delete pressed at the end of a header line,
                // Backspace at the start of the closing brace's line.
                // Applying it would silently take the whole hidden body
                // with it. Expand the block and drop the keystroke instead
                // — one visible no-op beats an invisible deletion.
                if bs <= be && be - bs == de - ds {
                    // Line bookkeeping BEFORE the splice: heads below the
                    // edit have to move with their block.
                    let at_line = chars[..bs].iter().filter(|&&c| c == '\n').count();
                    let removed = chars[bs..be].iter().filter(|&&c| c == '\n').count();
                    let added = ins.chars().filter(|&c| c == '\n').count();
                    let mut next: String = chars[..bs].iter().collect();
                    next.push_str(&ins);
                    next.extend(&chars[be..]);
                    display_code = next;
                    folded_own_edit = true;
                    if let Some(rel) = &fold_key {
                        if let Some(set) = self.folds.get(rel) {
                            let moved = fold::shift_heads(set, at_line, removed, added);
                            // A head the edit deleted outright takes its
                            // block's lines back into view, which makes
                            // every older snapshot in egui's undo history a
                            // projection of a structure that no longer
                            // exists — undoing one would write it back and
                            // delete that body for real. A head that merely
                            // SHIFTED is harmless: the history stays usable
                            // and Ctrl+Z keeps working through the fold.
                            if moved.len() != set.len() {
                                if let Some(mut st) =
                                    egui::TextEdit::load_state(ui.ctx(), editor_resp.response.id)
                                {
                                    st.clear_undoer();
                                    st.store(ui.ctx(), editor_resp.response.id);
                                }
                            }
                            if moved.is_empty() {
                                self.folds.remove(rel);
                            } else {
                                self.folds.insert(rel.clone(), moved);
                            }
                        }
                    }
                } else if let Some(rel) = &fold_key {
                    self.folds.remove(rel);
                    if let Some(mut st) =
                        egui::TextEdit::load_state(ui.ctx(), editor_resp.response.id)
                    {
                        st.clear_undoer();
                        st.store(ui.ctx(), editor_resp.response.id);
                    }
                }
            }
        }
        // The fold set may have shifted with the edit above, and
        // `display_code` may have changed — rebuild the projection so every
        // reader below (the gutter, the anchor, the write-back) sees one
        // consistent pair. Without a delta neither input moved, and the map
        // built before the render already is that pair.
        if folded && fold_inputs_changed {
            fold_map = match fold_key.as_ref().and_then(|rel| self.folds.get(rel)) {
                Some(set) if !set.is_empty() => fold::FoldMap::with_regions(
                    &display_code,
                    set,
                    &self.ed.fold_regions.get(&display_code),
                ),
                _ => fold::FoldMap::identity(&display_code),
            };
        }

        // ── Keep the caret in view when it moves off-screen ───────────
        // egui_code_editor nests a horizontal ScrollArea *inside* the
        // vertical one; the inner area consumes BOTH axes' scroll
        // targets and only applies its own, so egui's own
        // "scroll caret into view" never reaches the outer (vertical)
        // ScrollArea. Result: Shift+Up/Down (or typing) past the visible
        // area extends the selection but the window doesn't follow. We
        // drive the outer ScrollArea's offset ourselves.
        // A fold toggled last frame: put its header back at the same screen
        // position before anything else touches the scroll offset.
        let anchored = match &fold_key {
            Some(rel) => {
                self.apply_fold_anchor(ui, &editor_resp, &editor_id, &fold_map, rel, drawn_offset)
            }
            None => false,
        };
        self.scroll_caret_into_view(
            ui,
            &editor_resp,
            &editor_id,
            editor_clip,
            &fold_map,
            !anchored,
        );
        // Jump to a clicked diagnostic's line (queued by the bottom
        // panel). Runs after caret-follow so its precise offset wins.
        self.apply_pending_scroll(ui, &editor_resp, &editor_id, displayed_file, &fold_map);
        // Recorded every frame, so it is the caret of the file on screen or
        // none - never one left over from the file shown before.
        self.ed.caret_at = editor_resp
            .state
            .cursor
            .char_range()
            .map(|r| (displayed_file, fold_map.to_buffer(r.primary.index.0)));

        // The other half of the caret invariant (see the conversion before
        // the render): what the editor hands back is in projection space.
        // Converted HERE, not straight after the render, because the three
        // scroll helpers above are the only readers that pair the caret with
        // the GALLEY — which is the projection. Everything below pairs it
        // with `display_code`, the real buffer.
        let mut caret_in_galley = None;
        if folded {
            if let Some(r) = editor_resp.state.cursor.char_range() {
                caret_in_galley = Some(r.primary.index.0);
                let to = |c: egui::text::CCursor| {
                    egui::text::CCursor::new(fold_map.to_buffer(c.index.0))
                };
                let range = egui::text::CCursorRange::two(to(r.primary), to(r.secondary));
                editor_resp.state.cursor.set_char_range(Some(range));
                let mut st = editor_resp.state.clone();
                st.cursor.set_char_range(Some(range));
                st.store(ui.ctx(), editor_resp.response.id);
            }
        }

        // Everything from here on pairs `display_code` (the buffer) with the
        // galley the editor just built. While folded those two describe
        // different texts, so an overlay would paint on the wrong line —
        // they are skipped for that frame rather than lied to. The fade and
        // underline marks are unaffected: they went into the layout already
        // translated (`fold_map.map_ranges`).
        //
        // The overlays and gutters below position what they draw through one
        // row table of this galley, built on the first lookup any of them makes.
        let galley_rows = crate::editor::gui::text_pos::GalleyRows::new(&editor_resp.galley);
        // Where each "N refs" pill ended, for the inline diagnostic message that
        // is drawn much later (via `handle_editor_completion`) and used to paint
        // straight through them.
        //
        // A LOCAL, not a field: both producer and consumer are reached from this
        // one function, so "these coordinates die with this frame's galley" is a
        // language guarantee here instead of a rule someone has to remember. It
        // is declared OUTSIDE the fold guard on purpose — the pills are skipped
        // while folded but the messages are not, and an empty list is exactly
        // what the message should see then.
        let mut pill_edges: Vec<(u32, f32)> = Vec::new();
        if !folded {
            // Highlight every occurrence of the word the user selected
            // (double-click / Ctrl+Shift+Left/Right). Painted here — while
            // `display_code` still matches the galley the editor just built —
            // and before the diagnostics overlay so squiggles render on top.
            // Double-click: replace egui's UAX#29 word selection (which
            // glues `name:Type` into one "word" via the `:` MidLetter rule)
            // with the plain identifier run under the pointer.
            self.fix_double_click_selection(ui, &editor_resp, &display_code);
            // Ctrl(+Shift)+Left/Right: our own word jump, for the same
            // reason — the keys were consumed before the editor rendered.
            if let Some((right, extend)) = word_move {
                self.apply_word_move(ui, &editor_resp, &display_code, right, extend);
            }
            self.highlight_selected_word(
                &editor_resp,
                &galley_rows,
                &display_code,
                editor_clip,
                ui,
            );
            // Highlight all occurrences of the active find query (current one
            // in amber), so matches show even when the find field has focus.
            self.paint_find_matches(&editor_resp, &galley_rows, &display_code, editor_clip, ui);
            // Triple-clicking a `{`/`}` or a definition's header line
            // highlights the WHOLE definition in white and copies it on
            // Ctrl+C. (The single-block highlight moved off "selecting a
            // brace" to the explicit Ctrl+[ / Ctrl+] shortcut, applied
            // after the context menu below.)
            self.highlight_full_definition(
                &editor_resp,
                &display_code,
                displayed_file,
                editor_clip,
                ui,
                copy_requested,
            );
            // Unused generic parameters AND unused imports pulse a translucent
            // white highlight on top of their fade. Drawn before the "N refs"
            // pills so a pill can never end up under the wash.
            //
            // One call, one list: two overlays would each read the clock
            // themselves and the phases would only agree by luck.
            let mut pulse: Vec<(usize, usize)> = self.generic_pulse_ranges(&display_code).to_vec();
            pulse.extend(unused_imports.iter().copied());
            // Both read the text through the view's line index, asked for only
            // when one of them has something to draw.
            if !pulse.is_empty() || !underline_ranges.is_empty() {
                let index = self.ed.line_index.get(&display_code);
                generics::show_unused_pulse_overlay(
                    ui,
                    editor_resp.galley_pos,
                    editor_clip,
                    &galley_rows,
                    &index,
                    &pulse,
                );
                // …and the underlined ones explain themselves on hover.
                generics::show_impl_only_tooltips(
                    ui,
                    editor_resp.galley_pos,
                    editor_clip,
                    &galley_rows,
                    &index,
                    &underline_ranges,
                );
            }
            // "N refs" indicator + popup on every used item (unused ones were
            // already faded by the highlighter, above, via `dead_ranges`).
            if let Some(rel) = &usages_rel_path {
                pill_edges = self.show_usages_overlay(
                    ui,
                    editor_resp.galley_pos,
                    editor_clip,
                    &editor_resp.galley,
                    &display_code,
                    rel,
                );
            }
        } // end `if !folded` — galley-dependent overlays

        // ── Multi-cursor (Ctrl+Shift+Up/Down) ─────────────────────────
        // Add/remove an extra caret, then replay this frame's text edit
        // (if any) at every one of them — mutates `display_code` further.
        // When it does, the line-op shortcuts below are skipped for this
        // frame: they assume a single cursor/selection, and `editor_resp`
        // still reflects positions from BEFORE this replay.
        // ── A click in the error tooltip must not eject the caret ──────
        // (Escape no longer does: the editor keeps it - see `EDITOR_KEYS`.)
        //
        // A click in the inline-error tooltip (its Copy button, its docs
        // link) takes focus from the editor - egui surrenders it on any click
        // outside the focused widget, during the text box above - so take it
        // back, but only when the editor was the focused widget LAST frame:
        // a click there while the Find bar held focus must not yank it into
        // the editor. The flag is forced true when we restore, because
        // `has_focus()` is still false on this very frame; without it the
        // next frame's keyboard gate would read this editor as unfocused and
        // close its completion and code-action lists. The tooltip may be the OTHER
        // view's: see `click_in_tooltip`. Where the button was RELEASED, not
        // `interact_pos`: a move later in the same event batch overwrites
        // that, and a quick click-and-away would then miss the tooltip.
        let tooltip_click = crate::editor::gui::diagnostics_overlay::click_in_tooltip(
            self.diag_tooltip_at,
            ui.ctx().cumulative_frame_nr(),
            ui.input(|i| {
                i.pointer
                    .any_click()
                    .then(|| {
                        i.events.iter().find_map(|e| match e {
                            egui::Event::PointerButton {
                                pos,
                                pressed: false,
                                ..
                            } => Some(*pos),
                            _ => None,
                        })
                    })
                    .flatten()
            }),
        );
        if tooltip_click && self.ed.editor_was_focused {
            editor_resp.response.request_focus();
            self.ed.editor_was_focused = true;
            if !is_main {
                self.reference_was_focused = true;
            }
        } else {
            self.ed.editor_was_focused = editor_resp.response.has_focus();
            // The main panel runs BEFORE this view and needs last frame's
            // answer, so the second view's focus is mirrored where that gate
            // can find it without reaching into `ed_ref`.
            if !is_main {
                self.reference_was_focused = self.ed.editor_was_focused;
            }
        }

        let mc_shift = self.handle_multi_cursor(
            &mut display_code,
            &text_before_typing,
            &editor_resp,
            displayed_file,
            mc_up_pressed,
            mc_down_pressed,
            mc_escape_pressed,
            mc_caret_move,
        );
        let mc_replayed = mc_shift.is_some();
        // The primary caret's own edit already landed correctly, but an
        // extra caret ABOVE it (the only place Ctrl+Shift+Up ever adds
        // one) may have changed the buffer's length before it — shift
        // egui's stored cursor to match, or it visibly drifts the next
        // time something is typed. Applies from next frame (this
        // frame's caret was already painted using the un-shifted
        // position — a one-frame lag, same as `apply_pending_scroll`
        // elsewhere in this file).
        if let Some(shift) = mc_shift.filter(|&s| s != 0) {
            if let Some(r) = editor_resp.state.cursor.char_range() {
                let new_idx = (r.primary.index.0 as isize + shift).max(0) as usize;
                let mut st = editor_resp.state.clone();
                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(new_idx),
                )));
                st.store(ui.ctx(), editor_resp.response.id);
            }
        }
        // Same rule as the overlay block above: these all pair the buffer
        // with the folded galley, so they sit out a folded frame.
        if !folded {
            self.paint_extra_cursors(
                ui,
                editor_resp.galley_pos,
                editor_clip,
                &editor_resp.galley,
                &display_code,
            );
            // (the caret is painted for BOTH states, just below)
            // All three take the file THIS view is showing. The diff cache moved
            // into the view's own state — it holds exactly one file's hunks, so
            // sharing it made two views on different files recompute over each
            // other every frame — while the breakpoint set stays shared and
            // keyed by path, which is what makes the same file's dots agree in
            // both views.
            //
            // Git gutter marks (live diff vs HEAD, sees unsaved edits) +
            // click-to-revert. A revert mutates `display_code`; the write-back
            // below persists it (same as the context-menu Cut).
            self.tick_diff_gutter(&display_code, displayed_file);
            self.paint_diff_gutter(
                ui,
                &editor_resp,
                &galley_rows,
                editor_clip,
                &display_code,
                displayed_file,
            );
            // Breakpoint dots + click-to-toggle in the line-number column.
            self.paint_breakpoint_gutter(
                ui,
                &editor_resp,
                &galley_rows,
                editor_clip,
                &display_code,
                displayed_file,
            );
            // Hover-to-evaluate: value tooltip for the identifier under the
            // pointer while a debug session is halted.
            self.paint_debug_hover(ui, &editor_resp, editor_clip, &display_code);
        }

        // Painted in both states, unlike the overlays above — but the
        // caret in `editor_resp.state` has been converted to buffer space
        // by now, and this places it against the GALLEY. While folded the
        // projection-space index is passed in explicitly.
        self.paint_primary_caret(ui, &editor_resp, editor_clip, caret_in_galley);

        // Fold carets + the "N lines hidden" badge. LAST on purpose: they
        // share the number column with the breakpoint strip, and egui gives
        // a click to the widget registered latest — so the caret wins the
        // primary button while the strip keeps the secondary one. Outside
        // the `!folded` guard, since unfolding must stay possible.
        if let Some(rel) = fold_key.clone() {
            self.paint_fold_gutter(
                ui,
                &editor_resp,
                &galley_rows,
                editor_clip,
                &display_code,
                &fold_map,
                &rel,
                font_size,
            );
            // Ctrl+Shift+Q / the menu item, applied here where this frame's
            // galley can be measured for an anchor. Lands next frame, like a
            // gutter click.
            // Hashed only while a request is pending: without one the answer
            // is None whatever the text did.
            let requested = self.ed.fold_all_requested.take();
            let text_changed = requested.is_some()
                && self
                    .fold_guard
                    .get(&rel)
                    .is_some_and(|(_, prev)| *prev != fold_ui::text_sig(&display_code));
            match fold_ui::fold_all_request(requested, &rel, text_changed) {
                Some(fold_ui::Request::Fire) => {
                    self.toggle_fold_all(&editor_resp, editor_clip, &display_code, &fold_map, &rel);
                }
                // This frame also changed the text (a keystroke coalesced with
                // the shortcut): `guard_folds` would read the fresh folds as
                // "the file changed from outside" and drop them. Next frame.
                Some(fold_ui::Request::Wait) => self.ed.fold_all_requested = Some(rel.clone()),
                Some(fold_ui::Request::Drop) | None => {}
            }
            // Last, so it sees every fold change this frame — including the
            // one the gutter just made.
            self.guard_folds(
                &rel,
                &display_code,
                editor_resp.response.id,
                ui.ctx(),
                folded_own_edit,
            );
        }

        // ── Ctrl+Enter code actions (RA assists / quick-fixes) ────────
        if ctrl_enter_pressed {
            let cursor_idx = editor_resp
                .state
                .cursor
                .char_range()
                .map(|r| r.primary.index.0);
            let sel_end_idx = editor_resp
                .state
                .cursor
                .char_range()
                .map(|r| r.secondary.index.0);
            let anchor = editor_resp
                .state
                .cursor
                .char_range()
                .map(|cr| {
                    let clamped = cr.primary.index.0.min(
                        editor_resp
                            .galley
                            .job
                            .text
                            .chars()
                            .count()
                            .saturating_sub(1),
                    );
                    let local = editor_resp
                        .galley
                        .pos_from_cursor(egui::text::CCursor::new(clamped));
                    editor_resp.response.rect.left_top()
                        + local.min.to_vec2()
                        + egui::vec2(0.0, local.height() + 4.0)
                })
                .unwrap_or_else(|| editor_resp.response.rect.left_top());
            self.trigger_code_actions(&display_code, cursor_idx, sel_end_idx, anchor, slot);
        }
        self.show_code_action_popup(ui);
        self.show_add_dep_popup(ui);
        self.show_impl_picker(ui);

        // ── Right-click context menu ──────────────────────────────────
        // Lists every editor command with its shortcut. A click drives
        // the same flags the keyboard shortcut sets (so both share one
        // code path); Copy / Select-All are applied directly. The menu
        // acts on the current caret (right-click doesn't move it), which
        // matches the "…where the cursor is" shortcut semantics.
        let is_rs = matches!(
            displayed_file,
            ProjectFileId::MainRs | ProjectFileId::UserFile(_)
        );
        let is_cargo = selected_is_manifest;
        let mut menu_action: Option<context_menu::EditorAction> = None;
        editor_resp.response.context_menu(|ui| {
            menu_action = context_menu::editor_menu(ui, is_rs, is_cargo);
        });
        {
            use context_menu::EditorAction as A;
            match menu_action {
                Some(A::ExtractFn) => extract_pressed = true,
                Some(A::DeleteLine) => cut_line_pressed = true,
                Some(A::DuplicateLine) => ctrl_d_pressed = true,
                Some(A::ToggleCase) => toggle_case_pressed = true,
                Some(A::Comment) => ctrl_slash_pressed = true,
                Some(A::BlockComment) => ctrl_shift_slash_pressed = true,
                Some(A::MoveUp) => ctrl_up_pressed = true,
                Some(A::MoveDown) => ctrl_down_pressed = true,
                Some(A::NextFile) => cycle_next_pressed = true,
                Some(A::PrevFile) => cycle_prev_pressed = true,
                Some(A::Format) => format_pressed = true,
                // Picked up by `toggle_fold_all` next frame, once the galley
                // to anchor against has been laid out — the keyboard flag
                // takes the same route. Only for a file that can fold: a
                // library manifest lists the item too.
                Some(A::ToggleFoldAll) => {
                    if let Some(rel) = &fold_key {
                        self.ed.fold_all_requested = Some(rel.clone());
                    }
                }
                Some(A::Rename) => ctrl_r_pressed = true,
                Some(A::GoToDef) => f12_pressed = true,
                Some(A::GoToImpl) => ctrl_f12_pressed = true,
                Some(A::AddWatch) => {
                    // Prefer the current selection (lets you watch an
                    // expression like `self.buf[0]`); else the identifier
                    // under the caret. Reveal the Debug tab so the new
                    // watch is visible.
                    let expr = editor_resp
                        .state
                        .cursor
                        .char_range()
                        .and_then(|r| {
                            let lo = r.primary.index.0.min(r.secondary.index.0);
                            let hi = r.primary.index.0.max(r.secondary.index.0);
                            (lo != hi).then(|| {
                                let chars: Vec<char> = display_code.chars().collect();
                                chars[lo..hi.min(chars.len())].iter().collect::<String>()
                            })
                        })
                        .unwrap_or_else(word_under_cursor);
                    if !expr.trim().is_empty() {
                        self.debugger.add_watch(expr);
                        self.build_tab = crate::app::BuildPanelTab::Debug;
                    }
                }
                Some(A::SelectBlock) => select_block_pressed = true,
                Some(A::Completion) => ctrl_space_pressed = true,
                Some(A::Find) => self.ed.find.open_with(find_replace::FindMode::FindFile),
                Some(A::Replace) => self.ed.find.open_replace_with_word(
                    find_replace::FindMode::ReplaceFile,
                    &word_under_cursor(),
                ),
                Some(A::FindInProject) => {
                    self.ed.find.open_with(find_replace::FindMode::FindProject)
                }
                Some(A::ReplaceInProject) => self.ed.find.open_replace_with_word(
                    find_replace::FindMode::ReplaceProject,
                    &word_under_cursor(),
                ),
                Some(A::Cut) => {
                    // Cut the selection (mirrors the native Ctrl+X): copy
                    // it, remove it, collapse the cursor to the cut point.
                    if let Some(r) = editor_resp.state.cursor.char_range() {
                        let lo = r.primary.index.0.min(r.secondary.index.0);
                        let hi = r.primary.index.0.max(r.secondary.index.0);
                        if lo != hi {
                            let chars: Vec<char> = display_code.chars().collect();
                            let hi = hi.min(chars.len());
                            ui.ctx().copy_text(chars[lo..hi].iter().collect::<String>());
                            let mut new: String = chars[..lo].iter().collect();
                            new.extend(&chars[hi..]);
                            display_code = new;
                            let mut st = editor_resp.state.clone();
                            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(lo),
                                egui::text::CCursor::new(lo),
                            )));
                            st.store(ui.ctx(), editor_resp.response.id);
                        }
                    }
                }
                Some(A::Copy) => {
                    if let Some(r) = editor_resp.state.cursor.char_range() {
                        let lo = r.primary.index.0.min(r.secondary.index.0);
                        let hi = r.primary.index.0.max(r.secondary.index.0);
                        let chars: Vec<char> = display_code.chars().collect();
                        let text = if lo != hi {
                            chars[lo..hi.min(chars.len())].iter().collect::<String>()
                        } else {
                            current_line(&chars, lo)
                        };
                        if !text.is_empty() {
                            ui.ctx().copy_text(text);
                        }
                    }
                }
                Some(A::SelectAll) => {
                    let len = display_code.chars().count();
                    let mut st = editor_resp.state.clone();
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(len),
                    )));
                    st.store(ui.ctx(), editor_resp.response.id);
                }
                Some(A::ZoomIn) => {
                    self.editor_font_size = (self.editor_font_size + 1.0).min(MAX_EDITOR_FONT_SIZE)
                }
                Some(A::ZoomOut) => {
                    self.editor_font_size = (self.editor_font_size - 1.0).max(MIN_EDITOR_FONT_SIZE)
                }
                Some(A::ZoomReset) => self.editor_font_size = DEFAULT_EDITOR_FONT_SIZE,
                None => {}
            }
        }

        // ── Extract function (Ctrl+Alt+Insert) ────────────────────────────
        // After the editor rendered, so the selection it works on is the one
        // the editor just reported — and after the context menu, so a click
        // on its entry lands in the SAME frame the shortcut would.
        if extract_pressed {
            let sel = editor_resp
                .state
                .cursor
                .char_range()
                .map(|r| (r.primary.index.0, r.secondary.index.0));
            let anchor = editor_resp.response.rect.left_top() + egui::vec2(24.0, 24.0);
            self.begin_extract_fn(&display_code, displayed_file, is_rust_file, sel, anchor);
        }
        // A finished extraction rewrites the buffer. Applied HERE, before the
        // write-back below, so the new text is what gets persisted — the same
        // rule the context-menu line ops follow.
        if let Some((next, caret)) = self.show_extract_fn_popup(ui, &display_code, displayed_file) {
            display_code = next;
            let mut st = editor_resp.state.clone();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(caret),
            )));
            st.store(ui.ctx(), editor_resp.response.id);
        }

        // ── Ctrl+[ / Ctrl+] — select + copy the block at the caret ────
        // After the context-menu mapping so both paths land here.
        if select_block_pressed {
            self.select_brace_block(ui, &editor_resp, &display_code);
        }

        // ── MRU file switching (Ctrl+Tab / Ctrl+Shift+Tab) ────────────
        // Runs after the context-menu mapping so both paths land here.
        // Switching mid-frame is safe: the write-back below persists to
        // the captured `displayed_file`, and the editor shows the new
        // file next frame (same as tree-click / diagnostics nav).
        // Main editor only: the history and `selected_file` are its own, and
        // a second cycler would drag the main view around from a tab.
        if is_main && (cycle_next_pressed || cycle_prev_pressed) {
            // Drop stale entries first (deleted files, toolchain-hidden
            // fixed files) so session indices stay valid throughout.
            let rust_embedded = matches!(
                self.selected_toolchain(),
                Some(crate::panels::mcu_module::mcu_catalog::ToolchainKind::RustEmbedded)
            );
            let user_files = &self.project_tree.user_src_files;
            self.file_cycle.purge(|e| match e {
                file_cycle::HistEntry::User(p) => user_files.iter().any(|(q, _)| q == p),
                file_cycle::HistEntry::Fixed(ProjectFileId::MemoryX | ProjectFileId::BuildRs) => {
                    rust_embedded
                }
                file_cycle::HistEntry::Fixed(_) => true,
            });
            if let Some(entry) = self.file_cycle.begin_or_step(cycle_next_pressed) {
                if let Some(id) = entry.to_id(&self.project_tree.user_src_files) {
                    self.selected_file = id;
                    // Don't re-note during the session — promotion
                    // happens once, on commit (Ctrl release).
                    self.last_selected_file = id;
                }
            }
        }
        // Commit the session once Ctrl is released; keep repainting
        // while it's open so the release is noticed promptly and the
        // overlay below stays live.
        if self.file_cycle.is_cycling() {
            if !ui.input(|i| i.modifiers.ctrl) {
                self.file_cycle.commit();
            } else {
                show_file_cycle_overlay(ui.ctx(), &self.file_cycle);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50));
            }
        }

        // Ctrl+Shift+X cuts (not just deletes) the line(s): copy them to
        // the clipboard first so they can be pasted, then the line op below
        // removes them. Skipped when multi-cursor already replayed this
        // frame's edit — `editor_resp`'s positions are stale relative to
        // the just-mutated `display_code` (see `mc_replayed` above).
        if cut_line_pressed && !mc_replayed {
            if let Some(r) = editor_resp.state.cursor.char_range() {
                let lo = r.primary.index.0.min(r.secondary.index.0);
                let hi = r.primary.index.0.max(r.secondary.index.0);
                let cut = delete_line::cut_text(&display_code, lo, hi);
                if !cut.is_empty() {
                    ui.ctx().copy_text(cut);
                }
            }
        }

        // ── Editor line operations on the selection ───────────────────
        // Ctrl+/ toggles line comments (`//` for .rs, `#` for TOML /
        // .gitignore); Ctrl+Up / Ctrl+Down move the selected lines;
        // Ctrl+X deletes the line(s) at the cursor/selection; Ctrl+Shift+F
        // re-indents the whole file by block nesting. Each re-selects /
        // re-positions the cursor so the result persists next frame.
        // Applied before the write-back below so the new text persists;
        // the cursor is stored (on a clone — `store()` consumes the state,
        // which handle_editor_completion still reads) for the next frame.
        // Skipped entirely when `mc_replayed` (see above).
        let line_op: Option<(String, usize, usize)> = if mc_replayed {
            None
        } else {
            editor_resp.state.cursor.char_range().and_then(|r| {
                let lo = r.primary.index.0.min(r.secondary.index.0);
                let hi = r.primary.index.0.max(r.secondary.index.0);
                if ctrl_shift_slash_pressed {
                    // Only where `/* … */` is actually a comment. A TOML or
                    // .gitignore line comment is `#` and has no block form,
                    // so wrapping there would just corrupt the file.
                    (display_syntax.comment() == "//")
                        .then(|| comment::toggle_block_comment(&display_code, lo, hi))
                } else if ctrl_slash_pressed {
                    Some(comment::toggle_line_comments(
                        &display_code,
                        lo,
                        hi,
                        display_syntax.comment(),
                    ))
                } else if ctrl_up_pressed {
                    Some(move_lines::move_lines(&display_code, lo, hi, false))
                } else if ctrl_down_pressed {
                    Some(move_lines::move_lines(&display_code, lo, hi, true))
                } else if cut_line_pressed {
                    Some(delete_line::delete_lines(&display_code, lo, hi))
                } else if ctrl_d_pressed {
                    Some(duplicate_line::duplicate_lines(&display_code, lo, hi))
                } else if format_pressed {
                    let (new, c) = format::format_code(&display_code, lo, respace_on_format);
                    Some((new, c, c))
                } else {
                    None
                }
            })
        };
        if let Some((new_code, new_lo, new_hi)) = line_op {
            display_code = new_code;
            let mut st = editor_resp.state.clone();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(new_lo),
                egui::text::CCursor::new(new_hi),
            )));
            st.store(ui.ctx(), editor_resp.response.id);
        }

        // ── Ctrl+U: toggle case ───────────────────────────────────────
        // Not through `line_op`, which re-stores the selection as
        // `CCursorRange::two(lo, hi)` and so always puts the caret on the
        // right end: a selection made leftwards would come back reversed, and
        // Shift+Left would shrink it instead of growing it. The toggle never
        // changes the text's length (see `toggle_case`), so the selection and
        // every extra caret are already correct — nothing is stored at all.
        // Skipped when `mc_replayed`, like the line ops.
        if toggle_case_pressed && !mc_replayed {
            let carets =
                self.case_toggle_carets(editor_resp.state.cursor.char_range(), displayed_file);
            if let Some(new_code) = toggle_case::toggle_case(&display_code, &carets) {
                display_code = new_code;
                // An accept from a list left open replaces `[word start..caret]`
                // and would throw the toggled word away.
                if self.ed.completion_open {
                    crate::lsp::debug_log("COMPLETION_CLOSE reason=toggle-case");
                    self.ed.completion_open = false;
                }
                self.ed.cargo_complete.open = false;
            }
        }

        // ── Select the current Find match ─────────────────────────────
        // The find bar (drawn above the editor) records the match's char
        // range; apply it to the editor's cursor here, now that we have
        // `editor_resp`. Takes effect next frame (scroll was already queued
        // via `pending_scroll_to_line`).
        if let Some((s, e)) = self.ed.find.pending_select.take() {
            let mut st = editor_resp.state.clone();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(s),
                egui::text::CCursor::new(e),
            )));
            st.store(ui.ctx(), editor_resp.response.id);
            ui.ctx().request_repaint();
        }

        // ── Write user edits back ────────────────────────────────────
        // display_code is a local clone; persist changes here. Use
        // `displayed_file` (the file this text was built for), NOT
        // `self.selected_file`, which the bottom diag panel may have just
        // switched to a different file on a click — writing back to that
        // would overwrite the newly-opened file with this file's content.
        if let ProjectFileId::UserFile(i) = displayed_file {
            if let Some(entry) = self.project_tree.user_src_files.get_mut(i) {
                if display_code != entry.1 {
                    // In-memory only; the LSP flush (on Project Save — there is
                    // no idle debounce, whatever older comments said, or
                    // Project Save, see app::init_frame) writes it to the
                    // workspace and notifies RA — not on every keystroke.
                    entry.1 = display_code.clone();
                }
            }
        } else if displayed_file == ProjectFileId::MainRs && display_code != self.generated_code {
            self.generated_code = display_code.clone();
        } else {
            // Editable project config files — persist edits to the
            // matching field (the per-frame snapshot reads them back).
            let slot = match displayed_file {
                ProjectFileId::CargoToml => Some(&mut self.cargo_toml),
                ProjectFileId::CargoConfig => Some(&mut self.cargo_config),
                ProjectFileId::MemoryX => Some(&mut self.memory_x),
                ProjectFileId::BuildRs => Some(&mut self.build_rs),
                ProjectFileId::GitIgnore => Some(&mut self.gitignore),
                _ => None,
            };
            if let Some(slot) = slot {
                if *slot != display_code {
                    *slot = display_code.clone();
                }
            }
        }

        // Any Cargo manifest — the firmware's, or an extracted
        // library's (a plain user file at `<crate>/Cargo.toml`). Gating
        // on the `CargoToml` id alone left library manifests on the
        // rust-analyzer path, so Ctrl+Space there did nothing.
        if selected_is_manifest {
            // Cargo.toml gets crate-name + crates.io-version completion
            // instead of the rust-analyzer driver.
            self.handle_cargo_completion(
                ui,
                &editor_resp,
                &mut display_code,
                displayed_file,
                ctrl_space_pressed,
            );
        } else {
            // A Cargo popup does not survive leaving the manifest: its state is
            // only ever refreshed by `handle_cargo_completion`, so left open it
            // would linger invisibly and claim keys on the next visit.
            self.ed.cargo_complete.open = false;
            // Highlight the clicked diagnostic's line (colour keyed by
            // severity) and the F12 definition line (yellow), but only
            // while the editor shows the file each belongs to.
            let highlight: Option<(u32, egui::Color32)> = match self.ed.highlighted_error_line {
                Some((f, line, color)) if f == displayed_file => Some((line as u32, color)),
                _ => None,
            };
            let def_line: Option<u32> = match self.ed.highlighted_def_line {
                Some((f, line)) if f == displayed_file => Some(line as u32),
                _ => None,
            };
            let pin_pulse = self.pin_pulse_bands(ui.ctx(), displayed_file);
            self.handle_editor_completion(
                ui,
                &editor_resp,
                editor_clip,
                display_code,
                lsp_accepted,
                ctrl_space_pressed,
                ctrl_r_pressed,
                f12_pressed,
                ctrl_f12_pressed,
                highlight,
                def_line,
                pin_pulse,
                slot,
                displayed_file,
                &pill_edges,
                err_step,
            );
        }
        // Rename input popup (shown while active; sends the request on
        // submit). Rendered after the editor so it overlays the code.
        self.show_rename_popup(ui, slot);
    }

    /// Paint the primary caret ourselves, on top of the editor.
    ///
    /// egui's TextEdit draws its caret only while `input.focused` — the
    /// OS-window focus flag — is true. On Windows that flag goes stale when a
    /// `Focused(true)` event is missed (observed at app start and after
    /// Alt+Tab): typing still works (the widget keeps egui focus) but the
    /// caret is invisible. Painting it here, gated only on WIDGET focus, makes
    /// it impossible to lose; when egui's own caret does draw, the two overlap
    /// pixel-for-pixel (same galley position, same colour, same width formula
    /// as the theme's `modify_style`).
    /// `galley_idx`: the caret's index INTO THE GALLEY, when that is not what
    /// `editor_resp.state` holds — while folded the stored caret has already
    /// been converted back to buffer space, and the galley is the projection.
    fn paint_primary_caret(
        &self,
        ui: &egui::Ui,
        editor_resp: &egui::text_edit::TextEditOutput,
        clip: egui::Rect,
        galley_idx: Option<usize>,
    ) {
        if !editor_resp.response.has_focus() {
            return;
        }
        let Some(idx) = galley_idx.or_else(|| {
            editor_resp
                .state
                .cursor
                .char_range()
                .map(|r| r.primary.index.0)
        }) else {
            return;
        };
        // Clamp against a stale cursor index (file may have just shrunk).
        let idx = idx.min(editor_resp.galley.text().chars().count());
        let loc = editor_resp
            .galley
            .pos_from_cursor(egui::text::CCursor::new(idx));
        let x = editor_resp.galley_pos.x + loc.min.x;
        let y_top = editor_resp.galley_pos.y + loc.min.y;
        let y_bot = editor_resp.galley_pos.y + loc.max.y;
        if y_bot < clip.top() || y_top > clip.bottom() {
            return;
        }
        // Same width the editor theme gives egui's caret (`fontsize * 0.1`),
        // same colour egui would use.
        let stroke = egui::Stroke::new(
            self.editor_font_size * 0.1,
            ui.visuals().text_cursor.stroke.color,
        );
        ui.painter()
            .with_clip_rect(clip)
            .line_segment([egui::pos2(x, y_top), egui::pos2(x, y_bot)], stroke);
    }

    /// The lines + band colour of the "here is your pin" pulse on
    /// `displayed_file` — one line for a pin click, one per wired pin for a
    /// module click. Empty when there is nothing to pulse.
    ///
    /// The alpha follows a sine so the bands fade in and out instead of blinking
    /// on/off, and the whole highlight clears itself after `PIN_PULSE_SECS` — a
    /// permanent stripe would just become another thing to dismiss. Repaints are
    /// requested while it runs, otherwise egui would idle mid-pulse.
    fn pin_pulse_bands(
        &mut self,
        ctx: &egui::Context,
        displayed_file: ProjectFileId,
    ) -> Vec<(u32, egui::Color32)> {
        let Some(hl) = &self.ed.highlighted_pin_lines else {
            return Vec::new();
        };
        let elapsed = ctx.input(|i| i.time) - hl.start;
        if elapsed >= crate::app::PIN_PULSE_SECS {
            self.ed.highlighted_pin_lines = None;
            return Vec::new();
        }
        ctx.request_repaint();
        if hl.file != displayed_file {
            return Vec::new(); // still counting down, just not on screen
        }
        let phase = (elapsed * std::f64::consts::TAU * crate::app::PIN_PULSE_HZ).sin();
        let alpha = ((0.5 + 0.5 * phase) as f32 * crate::app::PIN_PULSE_ALPHA) as u8;
        let color = egui::Color32::from_rgba_unmultiplied(255, 214, 90, alpha);
        hl.lines.iter().map(|l| (*l as u32, color)).collect()
    }

    /// Apply a queued "jump to diagnostic line": scroll the editor so the target
    /// line sits on roughly the 10th row from the top. Only fires once the
    /// editor is displaying the target file (`displayed_file`), so a cross-file
    /// jump waits one frame for the file switch to take effect.
    ///
    /// The queued line is a BUFFER line; the scroll offset counts galley ROWS.
    /// While folded those differ by every hidden line above the target, so the
    /// line is translated through the projection first — a target inside a
    /// folded block lands on that block's header.
    fn apply_pending_scroll(
        &mut self,
        ui: &egui::Ui,
        editor_resp: &egui::text_edit::TextEditOutput,
        editor_id: &str,
        displayed_file: ProjectFileId,
        fold_map: &fold::FoldMap,
    ) {
        let Some((file, line_1based)) = self.ed.pending_scroll_to_line else {
            return;
        };
        // Wait until the editor actually shows that file (display_code matches).
        if file != displayed_file {
            return;
        }
        self.ed.pending_scroll_to_line = None;

        // Put the error line on the ~10th visible row (9 lines of context above).
        const ROWS_ABOVE: f32 = 9.0;
        let row_h = editor_resp
            .galley
            .pos_from_cursor(egui::text::CCursor::new(0))
            .height()
            .max(1.0);
        let row = fold_map.display_row_of(line_1based.saturating_sub(1)) as f32;
        let offset_y = ((row - ROWS_ABOVE) * row_h).max(0.0);

        let scroll_id =
            crate::app::helpers::scroll_id::scroll_area_id(ui, format!("{editor_id}_outer_scroll"));
        if let Some(mut state) = egui::containers::scroll_area::State::load(ui.ctx(), scroll_id) {
            state.offset.y = offset_y;
            state.store(ui.ctx(), scroll_id);
            // Suppress caret-follow from snapping back to the (stale) caret.
            // In buffer space, like every other reader of `last_caret_idx`.
            self.ed.last_caret_idx = editor_resp
                .state
                .cursor
                .char_range()
                .map(|r| fold_map.to_buffer(r.primary.index.0));
            ui.ctx().request_repaint();
        }
    }

    /// Paint a translucent cyan band over every occurrence of the identifier the
    /// user currently has selected — the classic "highlight all references of the
    /// symbol under the cursor" behaviour. A selection appears on a double-click
    /// (selects the word) or Ctrl+Shift+Left/Right (extends by word).
    ///
    /// Only a single whole identifier counts as a "variable": an empty / multi-
    /// token / non-identifier selection paints nothing. Matches are whole-word
    /// (so selecting `x` doesn't light up the `x` inside `max`).
    ///
    /// Only the rows on screen are scanned and positioned (see
    /// [`word_select::for_each_selected_word_rect`]); `rows` must describe
    /// `editor_resp.galley`.
    fn highlight_selected_word(
        &mut self,
        editor_resp: &egui::text_edit::TextEditOutput,
        rows: &crate::editor::gui::text_pos::GalleyRows,
        display_code: &str,
        clip: egui::Rect,
        ui: &egui::Ui,
    ) {
        let Some(range) = editor_resp.state.cursor.char_range() else {
            return;
        };
        let lo = range.primary.index.0.min(range.secondary.index.0);
        let hi = range.primary.index.0.max(range.secondary.index.0);
        if lo == hi {
            return; // no selection — nothing to highlight
        }

        // RGB (52, 232, 235) at 20% opacity → alpha ≈ 0.20 × 255 = 51, so the
        // code stays readable through the band (like the diagnostic tints).
        let color = egui::Color32::from_rgba_unmultiplied(52, 232, 235, 51);
        let painter = ui.painter().with_clip_rect(clip);
        let index = self.ed.line_index.get(display_code);
        word_select::for_each_selected_word_rect(
            rows,
            editor_resp.galley_pos,
            clip,
            &index,
            lo,
            hi,
            |rect| {
                painter.rect_filled(rect, 2.0, color);
            },
        );
    }
}

/// Floating "recent files" list shown while a Ctrl+Tab cycling session is
/// active (Ctrl held): the MRU history with the current target highlighted —
/// without it the user can't see what they're cycling through.
fn show_file_cycle_overlay(ctx: &egui::Context, fc: &file_cycle::FileCycle) {
    use egui_phosphor::regular as ph;
    let (entries, cursor) = fc.view();
    egui::Area::new(egui::Id::new("__file_cycle_overlay__"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 90.0))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Recent files — Tab: next · Shift+Tab: back · release Ctrl: open",
                    )
                    .size(10.0)
                    .color(egui::Color32::from_gray(140)),
                );
                ui.separator();
                for (i, e) in entries.iter().take(10).enumerate() {
                    let current = cursor == Some(i);
                    let marker = if current { ph::ARROW_RIGHT } else { " " };
                    ui.label(
                        egui::RichText::new(format!("{marker}  {}", e.label()))
                            .size(11.5)
                            .monospace()
                            .color(if current {
                                egui::Color32::from_rgb(120, 190, 255)
                            } else {
                                egui::Color32::from_gray(190)
                            }),
                    );
                }
            });
        });
}

impl AppIde {
    /// Every caret Ctrl+U acts on, as `(anchor, head)`: the primary, plus the
    /// multi-cursor extras when they belong to `file` (they are only cleared
    /// on a file switch later in the frame).
    fn case_toggle_carets(
        &self,
        primary: Option<egui::text::CCursorRange>,
        file: ProjectFileId,
    ) -> Vec<(usize, usize)> {
        let mut carets: Vec<(usize, usize)> = primary
            .map(|r| (r.secondary.index.0, r.primary.index.0))
            .into_iter()
            .collect();
        if self.ed.extra_cursors_file == Some(file) {
            carets.extend(self.ed.extra_cursors.iter().map(|c| (c.anchor, c.head)));
        }
        carets
    }
}

/// Consume every press of `key` matching `modifiers` — key repeats included —
/// and report whether one of them was a FIRST press. `consume_key` treats a
/// repeat like a press, which is wrong for a toggle held down.
fn consume_first_press(
    i: &mut egui::InputState,
    modifiers: egui::Modifiers,
    key: egui::Key,
) -> bool {
    let mut first = false;
    i.events.retain(|e| match e {
        egui::Event::Key {
            key: k,
            pressed: true,
            repeat,
            modifiers: m,
            ..
        } if *k == key && m.matches_logically(modifiers) => {
            first |= !*repeat;
            false
        }
        _ => true,
    });
    first
}

/// The text of the line containing char index `idx` (no trailing newline).
/// Used by the context-menu "Copy" when there is no selection.
fn current_line(chars: &[char], idx: usize) -> String {
    let idx = idx.min(chars.len());
    let start = chars[..idx]
        .iter()
        .rposition(|&c| c == '\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let end = chars[idx..]
        .iter()
        .position(|&c| c == '\n')
        .map(|p| idx + p)
        .unwrap_or(chars.len());
    chars[start..end].iter().collect()
}

#[cfg(test)]
mod first_press_tests {
    use super::consume_first_press;
    use eframe::egui::{Event, InputState, Key, Modifiers};

    fn key(k: Key, repeat: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers,
        }
    }

    fn run(events: Vec<Event>) -> (bool, usize) {
        let mut i = InputState::default();
        i.events = events;
        let hit = consume_first_press(&mut i, Modifiers::CTRL, Key::U);
        (hit, i.events.len())
    }

    #[test]
    fn a_press_fires_and_its_repeats_are_swallowed() {
        let events = vec![
            key(Key::U, false, Modifiers::CTRL),
            key(Key::U, true, Modifiers::CTRL),
        ];
        assert_eq!(run(events), (true, 0));
    }

    /// Holding the key: later frames carry only repeats. They must neither
    /// toggle again nor reach egui's own Ctrl+U, which deletes text.
    #[test]
    fn a_frame_of_repeats_is_swallowed_without_firing() {
        let events = vec![
            key(Key::U, true, Modifiers::CTRL),
            key(Key::U, true, Modifiers::CTRL),
        ];
        assert_eq!(run(events), (false, 0));
    }

    #[test]
    fn other_keys_and_a_bare_u_are_left_alone() {
        let events = vec![
            key(Key::D, false, Modifiers::CTRL),
            key(Key::U, false, Modifiers::NONE),
        ];
        assert_eq!(run(events), (false, 2));
    }

    /// Lenient like `consume_key`: egui's delete binding ignores Shift and Alt,
    /// so a stricter match would let Ctrl+Shift+U keep erasing code.
    #[test]
    fn extra_shift_or_alt_still_matches() {
        assert_eq!(
            run(vec![key(Key::U, false, Modifiers::CTRL | Modifiers::SHIFT)]),
            (true, 0)
        );
        assert_eq!(
            run(vec![key(Key::U, false, Modifiers::CTRL | Modifiers::ALT)]),
            (true, 0)
        );
    }
}

#[cfg(test)]
mod shortcut_guard {
    use super::{EXTRACT_FN_KEY, TOGGLE_CASE_KEY};
    use eframe::egui::Key;

    /// Keys `egui-winit` REWRITES into a clipboard event before egui ever sees
    /// an `Event::Key`, on Windows. The rewrite ignores `alt` entirely, so
    /// adding it to the chord does not dodge them.
    ///
    /// From `egui-winit`'s `is_cut_command` / `is_copy_command` /
    /// `is_paste_command`: with `ctrl`/`command` held, `X`, `C` and `V`; on
    /// Windows also `Ctrl+Insert` (copy), `Shift+Insert` (paste) and
    /// `Shift+Delete` (cut).
    const REWRITTEN_WITH_CTRL: [Key; 4] = [Key::X, Key::C, Key::V, Key::Insert];

    /// Keys `egui-winit` rewrites when SHIFT is held: `Shift+Insert` (paste)
    /// and `Shift+Delete` (cut). Anything bound as `Shift+<key>` has to dodge
    /// these two the same way.
    const REWRITTEN_WITH_SHIFT: [Key; 2] = [Key::Insert, Key::Delete];

    /// The bug this guards against was invisible: Ctrl+Alt+Insert produced no
    /// key event at all, so the shortcut did nothing and nothing anywhere said
    /// why. A dead shortcut looks exactly like a missing feature.
    /// The error-step key is bound BOTH bare and with Shift, so it has to
    /// survive the shifted rewrite as well.
    #[test]
    fn error_step_key_survives_the_shifted_clipboard_rewrite() {
        use crate::app::editor_panel::error_list::ERROR_STEP_KEY;
        assert!(
            !REWRITTEN_WITH_SHIFT.contains(&ERROR_STEP_KEY),
            "Shift+{ERROR_STEP_KEY:?} is rewritten into a clipboard event by              egui-winit — the key event never reaches egui, so stepping backwards              would silently do nothing"
        );
    }

    /// On X/C/V/Insert the toggle would never fire, and a chord rewritten into
    /// Cut or Paste would edit the selection instead of re-casing it.
    #[test]
    fn toggle_case_key_survives_the_clipboard_rewrite() {
        assert!(
            !REWRITTEN_WITH_CTRL.contains(&TOGGLE_CASE_KEY),
            "{TOGGLE_CASE_KEY:?} with Ctrl is rewritten into a clipboard event by \
             egui-winit — the key event never reaches egui, so the shortcut is dead"
        );
    }

    #[test]
    fn extract_fn_key_survives_the_clipboard_rewrite() {
        assert!(
            !REWRITTEN_WITH_CTRL.contains(&EXTRACT_FN_KEY),
            "{EXTRACT_FN_KEY:?} with Ctrl is rewritten into a clipboard event by \
             egui-winit — the key event never reaches egui, so the shortcut is dead"
        );
    }
}
