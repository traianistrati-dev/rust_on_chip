//! LSP completion driver + inline diagnostics, run after the editor widget.
//!
//! Consumes the editor's `TextEditOutput` (cursor + galley) and the text the
//! user just typed.  Applies an accepted completion, detects new triggers
//! (`.`, `::`, Ctrl+Space), renders the completion popup, and finally draws
//! the inline diagnostic overlays.

use super::doc_md;
use crate::app::{AppIde, ProjectFileId};
use crate::editor::gui::text_pos::{
    LineIndex, diags_for_file, lsp_completion_prefix, lsp_cursor_pos, lsp_kind_icon,
    lsp_word_start, selected_file_rel_path,
};
use crate::editor::gui::{show_diagnostics_overlay, show_inlay_hint};
use crate::lsp;
use eframe::egui;
use egui::text_edit::TextEditOutput;
use std::sync::Arc;

// The **inline** diagnostic overlay — squiggles and inline message text drawn
// over the code — is gated at the call site by `self.inline_errors_enabled`
// (toggled from the editor toolbar, default on).
//
// Only **errors and info** are drawn inline; warnings (and hints) are filtered
// out at the call site below and remain in the bottom panel (Cargo Check /
// rust-analyzer tabs) to keep the code view uncluttered. Diagnostics refresh on
// a Project Save — the ONLY moment rust-analyzer is re-synced; there is no
// idle debounce, despite what this comment claimed for a long time (see
// `app::init_frame`), so their positions no longer lag behind active typing.

/// Whether the inline overlay draws `severity`, given the two toolbar switches.
///
/// The split is error / not-error, and NOT `Error | Info` as it once was. That
/// old rule meant the non-error half was never really drawn: warnings and hints
/// were discarded outright, and rust-analyzer publishes almost nothing at
/// severity `Info` — it reports lints as `Warning` and weak lints as `Hint`. So
/// the editor showed errors and nothing else, and no switch admitted it.
fn wanted_inline(severity: lsp::DiagSeverity, show_errors: bool, show_info: bool) -> bool {
    if severity.is_error() {
        show_errors
    } else {
        show_info
    }
}

impl AppIde {
    /// Apply/trigger LSP completion and draw diagnostics, after the editor.
    ///
    /// `display_code` is the current editor text (already written back); it is
    /// taken by value because nothing after this stage reads it again.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn handle_editor_completion(
        &mut self,
        ui: &mut egui::Ui,
        editor_resp: &TextEditOutput,
        editor_clip: egui::Rect,
        mut display_code: String,
        lsp_accepted: Option<lsp::CompletionItem>,
        ctrl_space_pressed: bool,
        ctrl_r_pressed: bool,
        f12_pressed: bool,
        ctrl_f12_pressed: bool,
        // (1-based line, band colour) of the clicked diagnostic to highlight, if
        // in this file (colour keyed by severity).
        highlight: Option<(u32, egui::Color32)>,
        // 1-based line of the F12 definition to highlight (yellow), if in this file.
        def_line: Option<u32>,
        // (1-based line, band colour) per line of the pulsing "here is your pin"
        // highlight that follows a jump from the Pins canvas — one line for a pin
        // click, one per wired pin for a module click. The alpha is recomputed
        // every frame by the caller, so these are just colours to paint.
        pin_pulse: Vec<(u32, egui::Color32)>,
        // Which editor drew `editor_resp`, and which project file it holds.
        //
        // The MAIN editor passes `(Main, self.selected_file)` — identical to the
        // behaviour before a second editor existed. The Reference editor passes
        // its own file and gets ONLY completion: rename, go-to-definition,
        // diagnostic overlays and type hints all anchor popups or write results
        // through state that belongs to the main editor, so they are skipped
        // rather than left half-wired.
        slot: crate::app::EditorSlot,
        owner_file: ProjectFileId,
        // (1-based line, right edge) of every "N refs" pill drawn earlier in
        // THIS frame, so the inline diagnostic message can step around them
        // instead of painting through them. Empty whenever the usages overlay
        // did not run — folded, no LSP path, or the analysis not yet caught up
        // with the buffer — and an empty list simply restores the old position,
        // which is the right one when there is no pill to dodge.
        pill_edges: &[(u32, f32)],
        // F8 / Shift+F8: step to the next / previous error of this file.
        // `Some(true)` = forwards. Consumed in `show_code_view`, where
        // `editor_kbd_active` decides which of the two editors owns the
        // keyboard; the direction arrives here because this is where the error
        // rows and the caret both already exist.
        err_step: Option<bool>,
        // Buffer → galley positions. While folded the galley is the
        // projection, so a buffer index has to be translated before it is
        // looked up in it — and one inside a folded block draws nothing.
        fold_map: &super::fold::FoldMap,
    ) {
        // ── LSP completion: post-editor apply + trigger + popup ───────
        let cursor_char_idx = editor_resp
            .state
            .cursor
            .char_range()
            .map(|r| r.primary.index.0);

        // Apply accepted completion: replace [word_start..cursor] with the
        // item's text. Snippet items (functions/methods with `snippetSupport`)
        // expand to the full call — `foo(a, b)` — and the caret selects the
        // first parameter; plain items land the caret after the inserted text.
        if let Some(item) = lsp_accepted {
            if let Some(cur_idx) = cursor_char_idx {
                let chars: Vec<char> = display_code.chars().collect();
                // Clamp against a stale cursor (text may have shrunk since the
                // cursor was recorded) so the slices below can't panic.
                let cur_idx = cur_idx.min(chars.len());
                let word_start = lsp_word_start(&display_code, cur_idx).min(cur_idx);
                let (mut insert_text, first_stop) = if item.insert_is_snippet {
                    super::snippet::expand(&item.insert_text)
                } else {
                    (item.insert_text.clone(), None)
                };

                // `let name = ` context: a call accepted right after the `=`
                // completes the whole statement — the binding gets the fn's
                // return type and the line is closed:
                //   let my_value: Option<u32> = get_param_value(tx, rx, …);
                // Elsewhere the plain call is inserted, unchanged.
                let mut annotation: Option<(usize, String)> = None;
                if super::let_annotation::is_callable_kind(item.kind) && insert_text.ends_with(')')
                {
                    if let Some(ann_at) = super::let_annotation::let_context(&chars, word_start) {
                        if let Some(ret) = super::let_annotation::return_type(&item.detail) {
                            annotation = Some((ann_at, format!(": {ret}")));
                            // Close the statement only when nothing follows on
                            // the line (don't double up an existing `;`).
                            let line_rest_empty = chars[cur_idx..]
                                .iter()
                                .take_while(|&&c| c != '\n')
                                .all(|c| c.is_whitespace());
                            if line_rest_empty {
                                insert_text.push(';');
                            }
                        }
                    }
                }
                let (ann_at, ann) = annotation.unwrap_or((word_start, String::new()));
                let ann_len = ann.chars().count();

                let before: String = chars[..ann_at].iter().collect();
                let mid: String = chars[ann_at..word_start].iter().collect();
                let after: String = chars[cur_idx..].iter().collect();
                display_code = format!("{}{}{}{}{}", before, ann, mid, insert_text, after);

                // Caret for next frame: select the first-parameter placeholder
                // (typing replaces it), else sit right after the insert. All
                // offsets shift by the `: Type` annotation inserted before it.
                let end = word_start + ann_len + insert_text.chars().count();
                let (sel_start, sel_end) = first_stop
                    .map(|(s, e)| (word_start + ann_len + s, word_start + ann_len + e))
                    .unwrap_or((end, end));
                let mut st = editor_resp.state.clone();
                st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(sel_start),
                    egui::text::CCursor::new(sel_end),
                )));
                st.store(ui.ctx(), editor_resp.response.id);
                // Mouse accepts move focus to the popup — hand it back so the
                // user can type the argument straight away.
                ui.ctx()
                    .memory_mut(|m| m.request_focus(editor_resp.response.id));

                // Persist the change in memory so the write-back picks it up; the
                // LSP flush on Project Save handles disk + RA. There is no idle
                // debounce: `lsp_flush_requested` is set only under a Save or a
                // workspace rewrite (`flush_after_save` in `app.rs`).
                // Keyed on the OWNER: an accept driven from the Reference editor
                // must land in ITS file, never in whatever the main editor shows.
                if let ProjectFileId::UserFile(i) = owner_file {
                    if let Some(entry) = self.project_tree.user_src_files.get_mut(i) {
                        entry.1 = display_code.clone();
                    }
                } else if owner_file == ProjectFileId::MainRs {
                    self.generated_code = display_code.clone();
                }
            }
        }

        // Trigger detection
        // LSP completions are available for any .rs file open in RA.
        let lsp_file_tracked = matches!(
            owner_file,
            ProjectFileId::MainRs | ProjectFileId::UserFile(_)
        );
        // Compute the relative path for the currently edited file.
        // Used for all LSP requests (did_change, request_completion, etc.)
        let current_rel_path: Option<String> =
            selected_file_rel_path(&owner_file, &self.project_tree.user_src_files);
        {
            let lsp_ready = lsp_file_tracked
                && current_rel_path.is_some()
                && matches!(self.lsp_state.lock().unwrap().status, lsp::LspStatus::Ready);
            if ctrl_space_pressed {
                let lsp = self.lsp_state.lock().unwrap();
                crate::lsp::debug_log(&format!(
                    "COMPLETION_TRIGGER slot={slot:?} ed_slot={:?} file={current_rel_path:?} \
                     ready={lsp_ready} status={:?} caret={cursor_char_idx:?} open_before={}",
                    self.ed_slot, lsp.status, self.ed.completion_open
                ));
            }
            if lsp_ready {
                let rel = current_rel_path.as_deref().unwrap_or("src/main.rs");
                // Manual Ctrl+Space
                if ctrl_space_pressed {
                    if let Some(idx) = cursor_char_idx {
                        let (line, col) = lsp_cursor_pos(&display_code, idx);
                        // Sync the latest editor text to RA BEFORE the
                        // completion request — the frame's did_change (sent
                        // at the top of update()) used last frame's code.
                        {
                            let mut lsp = self.lsp_state.lock().unwrap();
                            lsp.did_change(rel, &display_code, false);
                            lsp.request_completion(rel, line, col, None);
                        }
                        self.ed.completion_trigger_idx = idx;
                        self.ed.completion_sel = 0;
                        // The last popup's rows: left in place they made the
                        // key block treat the spinner as a list, so Enter
                        // accepted an item from the PREVIOUS popup.
                        self.ed.completion_filtered_items.clear();
                        self.ed.completion_open = true;
                        self.take_completion_ownership(slot);
                        self.ed.completion_note = None;
                    }
                }

                // Auto-trigger on `.`  (method / field access)
                let dot_trigger = editor_resp.response.changed()
                    && cursor_char_idx
                        .map(|idx| {
                            let chars: Vec<char> = display_code.chars().collect();
                            idx > 0 && chars.get(idx - 1) == Some(&'.')
                        })
                        .unwrap_or(false);
                if dot_trigger && !ctrl_space_pressed {
                    if let Some(idx) = cursor_char_idx {
                        let (line, col) = lsp_cursor_pos(&display_code, idx);
                        {
                            let mut lsp = self.lsp_state.lock().unwrap();
                            lsp.did_change(rel, &display_code, false);
                            lsp.request_completion(rel, line, col, Some('.'));
                        }
                        self.ed.completion_trigger_idx = idx;
                        self.ed.completion_sel = 0;
                        // The last popup's rows: left in place they made the
                        // key block treat the spinner as a list, so Enter
                        // accepted an item from the PREVIOUS popup.
                        self.ed.completion_filtered_items.clear();
                        self.ed.completion_open = true;
                        self.take_completion_ownership(slot);
                    }
                }

                // Auto-trigger on `::` (Rust path separator)
                let colon_trigger = !dot_trigger
                    && !ctrl_space_pressed
                    && editor_resp.response.changed()
                    && cursor_char_idx
                        .map(|idx| {
                            let chars: Vec<char> = display_code.chars().collect();
                            idx >= 2
                                && chars.get(idx - 1) == Some(&':')
                                && chars.get(idx - 2) == Some(&':')
                        })
                        .unwrap_or(false);
                if colon_trigger {
                    if let Some(idx) = cursor_char_idx {
                        let (line, col) = lsp_cursor_pos(&display_code, idx);
                        {
                            let mut lsp = self.lsp_state.lock().unwrap();
                            lsp.did_change(rel, &display_code, false);
                            lsp.request_completion(rel, line, col, Some(':'));
                        }
                        self.ed.completion_trigger_idx = idx;
                        self.ed.completion_sel = 0;
                        // The last popup's rows: left in place they made the
                        // key block treat the spinner as a list, so Enter
                        // accepted an item from the PREVIOUS popup.
                        self.ed.completion_filtered_items.clear();
                        self.ed.completion_open = true;
                        self.take_completion_ownership(slot);
                    }
                }
            }

            // Close popup if cursor moved back past the trigger point,
            // or too far ahead (user navigated away from the trigger word).
            //
            // OWNER-gated: both editors run this every frame over the shared
            // state, and `completion_trigger_idx` belongs to the one that
            // opened the popup. Comparing it against the OTHER editor's caret
            // yields a meaningless delta — almost always negative — which
            // closed the popup one frame after it opened.
            if self.ed.completion_open && self.completion_owner == slot {
                if let Some(idx) = cursor_char_idx {
                    let cursor = idx as isize;
                    let trigger = self.ed.completion_trigger_idx as isize;
                    let delta = cursor - trigger;
                    // delta < 0  → user deleted back past trigger point
                    // delta > 80 → user moved far forward (switched context)
                    if delta < 0 || delta > 80 {
                        crate::lsp::debug_log(&format!(
                            "COMPLETION_CLOSE reason=caret-moved delta={delta} slot={slot:?}"
                        ));
                        self.ed.completion_open = false;
                    }
                }
            }
        }

        // ── Rename (Ctrl+R): capture the symbol + open the rename popup ──
        // Both editors: `rename_rel` / `rename_popup_pos` live in the view's own
        // `EditorState` now, so each anchors its popup over its own code. The
        // ANSWER still arrives through one inbox — `LspAsker::rename` records
        // who asked so the frame-top apply writes into the right view.
        if ctrl_r_pressed && lsp_file_tracked {
            if let (Some(idx), Some(rel)) = (cursor_char_idx, current_rel_path.clone()) {
                let word = super::rename::identifier_at(&display_code, idx);
                if !word.is_empty() {
                    let (line, col) = lsp_cursor_pos(&display_code, idx);
                    self.ed.rename_active = true;
                    self.ed.rename_focus = true;
                    // Kept so the finished rename can be audited for
                    // occurrences rust-analyzer did not reach.
                    self.ed.rename_old_name = word.clone();
                    self.ed.rename_input = word;
                    self.ed.rename_rel = rel;
                    self.ed.rename_line = line;
                    self.ed.rename_char = col;
                    // Anchor the popup just below the cursor.
                    self.ed.rename_popup_pos = editor_resp
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
                }
            }
        }

        // ── F12 / Ctrl+F12: go to definition / implementation ─────────────────
        // From either editor: `slot` is recorded with the request, and a
        // project-file answer opens in the view that asked.
        // Both funnel through the same result slot and navigation pipeline;
        // Ctrl+F12 resolves the `impl … for …` site where F12 on a trait
        // method would land on the trait's declaration.
        if (f12_pressed || ctrl_f12_pressed) && lsp_file_tracked {
            if let (Some(idx), Some(rel)) = (cursor_char_idx, current_rel_path.clone()) {
                // `lsp_file_tracked` is a file-IDENTITY test, so a library's
                // Cargo.toml passes it. rust-analyzer has nothing to say about
                // one, and without this check an F12 there would restart the
                // analyzer for a request that can never be answered.
                if rel.ends_with(".rs") {
                    let (line, col) = lsp_cursor_pos(&display_code, idx);
                    // Captured NOW, not when the answer lands: by then the
                    // galley that can place the caret on screen is gone and the
                    // text may have moved on. One anchors the chooser, the other
                    // decides which of its rows leads — rust-analyzer answers
                    // `implementation` for the TRAIT ITEM, so the receiver type
                    // the user wrote exists only on this line.
                    self.definition_caret_line = display_code
                        .lines()
                        .nth(line as usize)
                        .unwrap_or_default()
                        .to_owned();
                    self.definition_anchor = {
                        let clamped = idx.min(
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
                    };
                    // A new question replaces the old answer: leaving the
                    // chooser up would let Enter navigate to a row resolved from
                    // a caret that has since moved. So does an older PARKED
                    // question: left alive, it went out after this one and took
                    // the single answer slot from it.
                    self.impl_picker = None;
                    self.pending_goto = None;
                    // Can it answer RIGHT NOW? `Ready` alone is not enough: it
                    // flips on the first `$/progress` end of any rust-prefixed
                    // token, and `did_change` below auto-opens the document —
                    // doing that before the index is built is what produces
                    // phantom type errors on a detached file.
                    let usable = {
                        let lsp = self.lsp_state.lock().unwrap();
                        // Already-open document: `did_change` below opens
                        // nothing, so the detached-file risk that `indexed`
                        // guards against cannot apply. Needed as an alternative
                        // because `indexed` depends on a load-progress token
                        // rust-analyzer does not always send — gating on it
                        // alone parked every F12 behind a condition that never
                        // became true.
                        matches!(lsp.status, lsp::LspStatus::Ready)
                            && (lsp.indexed || lsp.is_file_open(&rel))
                    };
                    if usable {
                        let sent = {
                            let mut lsp = self.lsp_state.lock().unwrap();
                            lsp.did_change(&rel, &display_code, false);
                            if ctrl_f12_pressed {
                                lsp.request_implementation(&rel, line, col)
                            } else {
                                lsp.request_definition(&rel, line, col)
                            }
                        };
                        // Only wait for an answer that was actually asked for.
                        // This used to be unconditional: with the analyzer down
                        // the request was dropped inside `request_definition`
                        // and the flag stayed true for the rest of the session,
                        // polling a reply that could never arrive.
                        // Which view asked decides where the definition
                        // OPENS — the main editor switches `selected_file`, the
                        // Reference tab switches its own file.
                        self.lsp_asker.definition = crate::app::GotoOrigin::Editor(slot);
                        self.definition_in_flight = sent;
                    } else {
                        // Park it: the frame loop starts the analyzer, waits for
                        // it, and re-issues this exact request.
                        self.pending_goto = Some(crate::app::PendingGoto {
                            doc: crate::app::GotoDoc::Project {
                                file: owner_file,
                                rel,
                                text_hash: Self::content_hash(&display_code),
                            },
                            line,
                            col,
                            implementation: ctrl_f12_pressed,
                            since: std::time::Instant::now(),
                            restart_fired: false,
                            origin: crate::app::GotoOrigin::Editor(slot),
                        });
                    }
                }
            }
        }

        // ── LSP completion popup ───────────────────────────────────────
        // Rendered ONLY by the editor that asked. Both editors run this
        // function each frame over the same shared state, so without the owner
        // check the popup would be drawn twice — once anchored to the wrong
        // caret — and both would fight over the selection index.
        if self.ed.completion_open && self.completion_owner == slot {
            // A pointer copy under the lock, not a deep copy of every item.
            let all_items = Arc::clone(&self.lsp_state.lock().unwrap().completion_items);

            if !all_items.is_empty() {
                // ── Prefix-first ordering ────────────────────────────────
                // The identifier word ending at the cursor (e.g. the "n" typed
                // before pressing Ctrl+Space). `lsp_word_start` stops at `.` / `:`
                // / whitespace, so a `.`/`::` trigger keeps only the part after the
                // separator. Using the whole word (not just text typed after the
                // trigger) is what lets the list lead with what's already typed.
                let prefix = cursor_char_idx
                    .map(|cur| {
                        let word_start = lsp_word_start(&display_code, cur);
                        lsp_completion_prefix(&display_code, word_start, cur)
                    })
                    .unwrap_or_default();

                // Persisted in the view, so next frame's key handlers see
                // exactly the same items the user sees right now. Re-ordered
                // only when the list or the prefix changed.
                let filtered = self
                    .ed
                    .completion_filtered_items
                    .update(&all_items, &prefix);

                if filtered.is_empty() {
                    // Nothing matches the current prefix — hide the popup.
                    self.ed.completion_open = false;
                } else {
                    // Items on screen — any earlier "why empty" note is stale.
                    self.ed.completion_note = None;
                    // Clamp selection into the visible filtered range.
                    self.ed.completion_sel = self.ed.completion_sel.min(filtered.len() - 1);

                    // ── Wheel moves the SELECTION, not just the viewport ──────
                    // The selected row calls `scroll_to_me` every frame, so a
                    // freely scrolling viewport snaps straight back and the
                    // wheel looks dead. Moving the selection instead makes the
                    // list follow it.
                    //
                    // ONE notch = ONE item. This reads the raw `MouseWheel`
                    // EVENTS, not `smooth_scroll_delta`: egui only exposes the
                    // smoothed delta, which keeps decaying across several
                    // frames, so stepping from it flew through three-plus items
                    // per notch however it was scaled. One event = one notch.
                    let notches: i32 = ui.input(|i| {
                        i.events
                            .iter()
                            .filter_map(|e| match e {
                                egui::Event::MouseWheel { delta, .. } if delta.y.abs() > 0.0 => {
                                    Some(delta.y.signum() as i32)
                                }
                                _ => None,
                            })
                            .sum()
                    });
                    if notches != 0 {
                        let last = filtered.len() - 1;
                        self.ed.completion_sel = if notches > 0 {
                            // Positive y scrolls the CONTENT down, i.e. moves
                            // towards the items above.
                            self.ed.completion_sel.saturating_sub(notches as usize)
                        } else {
                            (self.ed.completion_sel + (-notches) as usize).min(last)
                        };
                    }
                    let sel = self.ed.completion_sel;

                    // ── Popup screen position ────────────────────────────
                    let popup_pos = if let Some(char_range) = editor_resp.state.cursor.char_range()
                    {
                        let cursor_idx = char_range.primary.index.0;
                        let text_char_count = editor_resp.galley.job.text.chars().count();
                        let clamped = cursor_idx.min(text_char_count.saturating_sub(1));
                        let cursor_local = editor_resp
                            .galley
                            .pos_from_cursor(egui::text::CCursor::new(clamped));
                        let offset = egui::vec2(0.0, cursor_local.height() + 2.0);
                        editor_resp.response.rect.left_top() + cursor_local.min.to_vec2() + offset
                    } else {
                        editor_resp.response.rect.left_top()
                    };

                    // ── Render popup ─────────────────────────────────────
                    // Mouse click → deferred insert.
                    if let Some(i) = show_completion_list(ui.ctx(), popup_pos, &filtered, sel) {
                        self.ed.completion_pending_insert = Some(filtered[i].clone());
                        self.ed.completion_open = false;
                    }

                    // ── Detail panel, beside the focused item ─────────────────
                    // Was hover-only, which meant you had to leave the keyboard
                    // to read the signature of the item you were already on.
                    // Same anchor as the list, so the two read as one widget.
                    if let Some(item) = filtered.get(sel) {
                        if !item.detail.is_empty() || !item.documentation.is_empty() {
                            const LIST_W: f32 = 440.0;
                            const DETAIL_W: f32 = 380.0;
                            const GAP: f32 = 6.0;
                            // Prefer the right, FLIP to the left when it would
                            // not fit.
                            //
                            // Fixing it to the right does NOT push it off
                            // screen — `Area` constrains itself back in
                            // (context.rs `constrain_window_rect_to_area`
                            // slides it left). That is the problem: it slid on
                            // top of the LIST, so the panel looked like it had
                            // vanished. Choosing the side ourselves is the only
                            // way to land somewhere that doesn't collide. The
                            // second editor (narrow middle zone) hits this
                            // almost immediately.
                            let screen = ui.ctx().content_rect();
                            let right_x = popup_pos.x + LIST_W + GAP;
                            let left_x = popup_pos.x - GAP - DETAIL_W;
                            let detail_x = if right_x + DETAIL_W <= screen.right() {
                                right_x
                            } else if left_x >= screen.left() {
                                left_x
                            } else {
                                // Neither side fits: hug the right edge rather
                                // than hang off it, so the text stays readable.
                                (screen.right() - DETAIL_W).max(screen.left())
                            };
                            // Parsed once per focused item, not once per frame.
                            let doc = self
                                .ed
                                .completion_filtered_items
                                .doc_lines(&item.documentation);
                            egui::Area::new(egui::Id::new("lsp_completion_detail"))
                                .fixed_pos(egui::pos2(detail_x, popup_pos.y))
                                .order(egui::Order::Foreground)
                                .show(ui.ctx(), |ui| {
                                    egui::Frame::popup(&ui.ctx().global_style()).show(ui, |ui| {
                                        ui.set_max_width(DETAIL_W);
                                        // As tall as actually fits below the
                                        // anchor — the old fixed 300 px cut
                                        // documentation off for no reason,
                                        // while the hover tooltip it replaced
                                        // was screen-bounded and read better.
                                        let room =
                                            (ui.ctx().content_rect().bottom() - popup_pos.y - 24.0)
                                                .max(120.0);
                                        // `max_height` ALONE is not enough, and
                                        // silently does nothing here: ScrollArea
                                        // takes `available_rect_before_wrap()
                                        // .at_most(max_size)`, and an `Area`
                                        // sizes its Ui from LAST frame's
                                        // measured size. That latches — the
                                        // panel can never grow past the height
                                        // it first happened to measure (a short
                                        // item's docs), so every later item was
                                        // capped at that. `min_scrolled_height`
                                        // is applied after the `at_most`, so it
                                        // is the one knob that escapes the
                                        // latch; `auto_shrink` still collapses
                                        // the final rect when docs are short.
                                        egui::ScrollArea::vertical()
                                            .id_salt("lsp_completion_detail_scroll")
                                            .max_height(room)
                                            .min_scrolled_height(room)
                                            .auto_shrink([false, true])
                                            .show(ui, |ui| {
                                                // Signature first: monospace and
                                                // tinted like a type, since that
                                                // is what it usually is.
                                                if !item.detail.is_empty() {
                                                    ui.label(
                                                        egui::RichText::new(&item.detail)
                                                            .monospace()
                                                            .size(11.0)
                                                            .color(egui::Color32::from_rgb(
                                                                150, 200, 255,
                                                            )),
                                                    );
                                                }
                                                if !item.detail.is_empty()
                                                    && !item.documentation.is_empty()
                                                {
                                                    ui.separator();
                                                }
                                                if !item.documentation.is_empty() {
                                                    render_doc(ui, doc);
                                                }
                                            });
                                    });
                                });
                        }
                    }
                }
            }
            // all_items is empty: either RA hasn't responded yet, or
            // it responded with no completions / an error.
            else if !lsp_file_tracked {
                // Nothing below would ever close it or say why.
                crate::lsp::debug_log(&format!(
                    "COMPLETION_STUCK reason=file-not-tracked owner_file={owner_file:?}"
                ));
            } else {
                let (resp_received, timed_out, failure) = {
                    let lsp = self.lsp_state.lock().unwrap();
                    let received = lsp.completion_response_received;
                    let timeout = lsp
                        .completion_request_sent_at
                        .map(|t| t.elapsed().as_secs() > 6)
                        .unwrap_or(false);
                    (received, timeout, lsp.completion_failure.clone())
                };

                if resp_received || timed_out {
                    // RA answered (empty) or request is stale — close the
                    // popup, but SAY WHY at the cursor: the silent one-frame
                    // flash ("apare și dispare") was undiagnosable. The most
                    // common real cause is a file that no `mod …;` declares —
                    // rust-analyzer detaches it and answers `null` to every
                    // completion request in it.
                    crate::lsp::debug_log(&format!(
                        "COMPLETION_CLOSE reason=no-items received={resp_received} \
                         timed_out={timed_out} failure={failure:?}"
                    ));
                    self.ed.completion_open = false;
                    // Each cause named apart: one note for all of them hid a
                    // refused request behind "no suggestions here".
                    let note = if timed_out && !resp_received {
                        "rust-analyzer did not answer (busy / indexing) — try again".to_owned()
                    } else {
                        match failure {
                            Some(lsp::CompletionFailure::Error { code, message }) => {
                                empty_completion_note_for_error(code, &message)
                            }
                            Some(lsp::CompletionFailure::Null) => {
                                self.unlinked_module_hint(owner_file).unwrap_or_else(|| {
                                    "rust-analyzer does not analyse this file here (null answer)"
                                        .to_owned()
                                })
                            }
                            None => self
                                .unlinked_module_hint(owner_file)
                                .unwrap_or_else(|| "no suggestions here".to_owned()),
                        }
                    };
                    self.ed.completion_note = Some((note, std::time::Instant::now()));
                } else {
                    // Still waiting — show a small spinner popup.
                    let popup_pos = cursor_char_idx.and_then(|_| {
                        editor_resp.state.cursor.char_range().map(|cr| {
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
                                + egui::vec2(0.0, local.height() + 2.0)
                        })
                    });
                    if let Some(pos) = popup_pos {
                        egui::Area::new(egui::Id::new("lsp_completion_loading"))
                            .fixed_pos(pos)
                            .order(egui::Order::Foreground)
                            .show(ui.ctx(), |ui| {
                                egui::Frame::popup(&ui.ctx().global_style()).show(ui, |ui| {
                                    ui.add_space(2.0);
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(
                                            egui::RichText::new("  rust-analyzer…")
                                                .size(11.5)
                                                .color(egui::Color32::from_rgb(160, 175, 200)),
                                        );
                                    });
                                    ui.add_space(2.0);
                                });
                            });
                        ui.ctx().request_repaint();
                    }
                }
            }
        }

        // ── "Why was the list empty?" note ─────────────────────────────────
        // Shown at the cursor for a few seconds after a completion request came
        // back with nothing (most often: the file has no `mod …;` declaration,
        // so rust-analyzer does not analyze it at all). Cleared by its timeout,
        // by typing, or by the next successful popup.
        if let Some((note, at)) = self
            .ed
            .completion_note
            .clone()
            .filter(|_| self.completion_owner == slot)
        {
            if at.elapsed().as_secs_f32() > 6.0 || editor_resp.response.changed() {
                self.ed.completion_note = None;
            } else {
                let pos = editor_resp
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
                            + egui::vec2(0.0, local.height() + 2.0)
                    })
                    .unwrap_or_else(|| editor_resp.response.rect.left_top());
                egui::Area::new(egui::Id::new("lsp_completion_note"))
                    .fixed_pos(pos)
                    .order(egui::Order::Foreground)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(&ui.ctx().global_style()).show(ui, |ui| {
                            ui.set_max_width(460.0);
                            ui.label(
                                egui::RichText::new(&note)
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(230, 190, 90)),
                            );
                        });
                    });
                // Keep frames coming so the timeout fires without input.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(400));
            }
        }

        // Every position lookup below goes through this, instead of walking
        // `display_code` from offset 0 once per diagnostic per frame. Taken
        // here because `display_code` is final from this point on (an accepted
        // completion above rewrites it).
        let line_index = self.ed.line_index.get(&display_code);

        // ── Pin-jump pulse band ───────────────────────────────────────
        // Painted OUTSIDE the diagnostics gate below: it must show up on any
        // file the editor can display, whether or not rust-analyzer tracks it
        // and whether or not the inline-errors toggle is on.
        //
        // No slot gate: the caller reads it from the VIEW's own
        // `highlighted_pin_lines`, and only the editor a pin click targeted has
        // one — so a view that was not the target is handed an empty list.
        for (line, color) in pin_pulse {
            crate::editor::gui::show_line_band(
                ui,
                editor_resp.galley_pos,
                editor_clip,
                &editor_resp.galley,
                &line_index,
                line,
                color,
            );
        }

        // ── Call-signature ghost: decided BEFORE the overlay ──────────
        // The ghost takes the end of the line holding the call's `(`, which is
        // where that line's inline message would go — "arguments to this
        // function are incorrect", or the count error itself. Two texts at one
        // spot overprint, and the ghost says more (the count included), so
        // the overlay is told to leave that line's message out; the full
        // message stays in the hover tooltip and the error list.
        let sig_ghost = self.update_signature_hint(
            &line_index,
            cursor_char_idx,
            current_rel_path.as_deref(),
            lsp_file_tracked,
            slot,
        );
        // Where it goes in the galley — nowhere when its line is inside a
        // folded block, or heads one: the "… N lines" badge sits at that
        // line's end, and the ghost would paint over it and its click.
        let sig_eol = sig_ghost.as_ref().and_then(|g| {
            let eol = fold_map.to_display(g.eol_idx)?;
            let heads_a_fold = g.eol_idx < line_index.total_chars()
                && fold_map.to_display(g.eol_idx + 1).is_none();
            (!heads_a_fold).then_some(eol)
        });
        // Only a ghost that is drawn takes the line's message.
        let quiet_line = sig_ghost
            .as_ref()
            .filter(|_| sig_eol.is_some())
            .map(|g| g.line);

        // ── Diagnostic overlays ───────────────────────────────────────
        // Both editors. Everything here is driven by `current_rel_path` and
        // bounded by `editor_clip`, and the two highlight bands arrive as
        // parameters from the view's own state — nothing was ever specific to
        // the main one.
        // Two independent halves of ONE overlay: errors, and everything that is
        // not an error. Either switch on its own is reason to build it.
        let (show_errors, show_info) = (self.inline_errors_enabled, self.inline_info_enabled);
        if lsp_file_tracked && (show_errors || show_info) {
            // Only draw the inline overlay when RA holds the CURRENT text for the
            // displayed file (per-file, not a global check). With pending edits
            // the diagnostics are stale — their line/col cling to a row that was
            // moved or deleted, so a squiggle/message "sticks" after the bad line
            // is gone. They reappear (refreshed) once the LSP debounce re-verifies
            // — on the next Project Save, and ONLY then. This comment used to
            // promise a 3-second idle re-verify; that path was deleted, so the
            // blank window lasts until Ctrl+S. The toolbar badge beside the file
            // name names that state, because a blank overlay and a clean file are
            // otherwise the same zero pixels.
            // Errors that live inside `#[entry] fn main`
            // come from cargo-check (RA can't expand the entry macro), so they
            // surface a moment after that check completes.
            let diags: Vec<lsp::LspDiagnostic> = current_rel_path
                .as_deref()
                .map(|rel| {
                    let lsp = self.lsp_state.lock().unwrap();
                    // Show the diagnostics only when RA holds the CURRENT text for
                    // this file AND has re-published since the last edit was sent.
                    // `last_sent_matches` hides them the instant you type (before
                    // the flush); `diagnostics_fresh` then keeps them hidden in the
                    // window between the flush (didChange) and RA's fresh publish —
                    // otherwise the OLD diagnostics paint over the NEW text at
                    // shifted line/cols (a fixed error lingering on the wrong line).
                    if lsp.last_sent_matches(rel, &display_code) && lsp.diagnostics_fresh(rel) {
                        // Flycheck (cargo check) diagnostics keep rustc's old
                        // line/cols until the next check finishes, so they'd paint
                        // on stale/commented lines after an edit. Hide them until a
                        // fresh check completes; RA's native diagnostics are
                        // re-mapped per edit and stay visible — this ALSO includes
                        // numbered hard errors (`E0425`, …), which RA computes
                        // natively regardless of `source == "rustc"` (see
                        // `LspDiagnostic::is_rustc_error_code`).
                        let flycheck_stale = lsp.flycheck_stale();
                        // Filtered by reference, and only what survives is
                        // cloned: the filters are pure, so the list (and each
                        // survivor's index, which keys its tooltip) is the one
                        // cloning everything first produced.
                        diags_for_file(&lsp.diagnostics, rel)
                            .iter()
                            // One toolbar switch per half.
                            //
                            // This used to read `Error | Info`, which meant the
                            // non-error half was effectively never drawn:
                            // warnings and hints were dropped outright, and
                            // rust-analyzer publishes almost nothing at
                            // severity `Info` — lints come through as `Warning`
                            // and weak lints as `Hint`. So the editor showed
                            // errors and nothing else, with no switch saying so.
                            .filter(|d| wanted_inline(d.severity, show_errors, show_info))
                            .filter(|d| {
                                d.source == "rust-analyzer"
                                    || d.is_rustc_error_code()
                                    || !flycheck_stale
                            })
                            // A flycheck (rustc/clippy) diagnostic carries the
                            // line/col from the LAST completed cargo check. If
                            // that line is now blank, commented out, or past the
                            // end of the file, whatever it complained about is
                            // gone — the squiggle is provably stale, so don't
                            // paint it. Numbered hard errors (`E0308`, …) reach
                            // here despite `flycheck_stale` by design (RA does
                            // not publish them natively for nested files), which
                            // is exactly why they used to stick to commented
                            // lines until the next Save.
                            .filter(|d| {
                                d.source == "rust-analyzer" || !line_is_gone(&line_index, d.line)
                            })
                            .cloned()
                            .collect()
                    } else {
                        Vec::new()
                    }
                })
                .unwrap_or_default();

            // Clip strictly to the VISIBLE editor area. The code editor wraps the
            // text in nested scroll areas, so `text_clip_rect` (and the editor's
            // response rect) cover the *full* galley — every line, even scrolled-
            // off ones — which is why clipping to those let squiggles/messages for
            // off-screen lines bleed into the bottom panel. `editor_clip` is the
            // editor's on-screen region (captured before it filled the space; its
            // bottom edge is the top of the diagnostics panel), so it bounds the
            // overlay to what's actually visible.
            let visible_clip = editor_clip;
            let tooltip = show_diagnostics_overlay(
                ui,
                editor_resp.galley_pos,
                visible_clip,
                &editor_resp.galley,
                &diags,
                &line_index,
                current_rel_path.as_deref(),
                highlight,
                def_line,
                pill_edges,
                quiet_line,
            );
            // Remembered for the focus hand-back in `show_code_view`, which
            // runs before this in BOTH views — see `click_in_tooltip`.
            if let Some(rect) = tooltip {
                self.diag_tooltip_at = Some((ui.ctx().cumulative_frame_nr(), rect));
            }
        }

        // ── Floating error list, top-right of the editor ──────────────
        // Deliberately OUTSIDE the `inline_errors_enabled` gate above: that
        // toggle hides the squiggles, and "I cannot see where the error is" is
        // precisely the state it produces. The list is also what the bottom
        // panel is not — scoped to the file on screen, and visible without
        // opening anything.
        //
        // Both editors: it is anchored to `editor_clip` and jumps through the
        // view's OWN `ed`, so the Reference view gets a working list of its own
        // file rather than a half-wired one.
        {
            use super::error_list::{self, Freshness};
            let (rows, fresh) = match current_rel_path.as_deref() {
                None => (Vec::new(), Freshness::Live),
                Some(rel) => {
                    // One lock at a time, never both — the deadlock rule the
                    // rest of this file follows.
                    let (entries, ready, live) = {
                        let lsp = self.lsp_state.lock().unwrap();
                        (
                            error_list::entries_from_lsp(diags_for_file(&lsp.diagnostics, rel)),
                            matches!(lsp.status, lsp::LspStatus::Ready),
                            lsp.last_sent_matches(rel, &display_code) && lsp.diagnostics_fresh(rel),
                        )
                    };
                    // Cargo only fills in while rust-analyzer is DOWN.
                    //
                    // `first_error_line` falls back whenever RA merely has
                    // nothing to say, which is right for a one-shot jump but
                    // wrong for a box that stays on screen: an error you just
                    // fixed lingers in the last cargo result, and the list
                    // would keep insisting on it after RA had already
                    // published the file clean.
                    if entries.is_empty() && !ready {
                        let build = self.build_state.lock().unwrap();
                        let e = build
                            .result()
                            .map(|r| error_list::entries_from_cargo(&r.for_file(rel)))
                            .unwrap_or_default();
                        (error_list::rows_for(&e), Freshness::Cargo)
                    } else {
                        (
                            error_list::rows_for(&entries),
                            if live {
                                Freshness::Live
                            } else {
                                Freshness::Stale
                            },
                        )
                    }
                }
            };
            let salt = match slot {
                crate::app::EditorSlot::Main => "main",
                crate::app::EditorSlot::Reference => "ref",
            };
            // A click on a row, or F8 / Shift+F8 stepping from the caret. Both
            // end in the same jump, so neither can drift from the other.
            //
            // Stepping walks the WHOLE list, not the six rows on screen: the
            // seventh error is precisely the one the box cannot show you.
            let target = error_list::show(ui, salt, editor_clip, &rows, fresh).or_else(|| {
                let forward = err_step?;
                let caret = cursor_char_idx
                    .map(|i| line_index.line_of_char(i) as u32 + 1)
                    .unwrap_or(0);
                error_list::step(&rows, caret, forward)
            });
            if let Some(line) = target {
                self.ed.pending_scroll_to_line = Some((owner_file, line as usize));
                self.ed.highlighted_error_line = Some((
                    owner_file,
                    line as usize,
                    crate::app::diag_highlight_color(lsp::DiagSeverity::Error),
                ));
            }
        }

        // ── Inferred-type ghost hint (cursor line only) ────────────────
        // Both editors: the Tab that accepts a hint is consumed in the shared
        // `show_code_view`, against whichever view holds the keyboard, and
        // `inlay_accept_pending` is that view's own.
        // Independent of the inline-errors toggle: request/clear the hint for
        // the caret's untyped `let`, then draw it as dim ghost text at the END
        // of the line (Tab to insert — handled in `mod.rs`, applied in
        // `init_frame`). End-of-line, not inline after the name: an overlay
        // can't push the real code aside, so an inline hint overlapped the ` =
        // initializer` — drawing after the line keeps both readable.
        let inlay_line = self.update_inlay_hint(
            &display_code,
            cursor_char_idx,
            current_rel_path.as_deref(),
            slot,
        );
        // Whether the hint is on screen: Tab accepts only a hint the user can
        // see (`mod.rs`), and the signature ghost can take its line.
        self.ed.inlay_hint_drawn = false;
        if let (Some(line), Some(hint)) = (inlay_line, self.ed.inlay_hint.as_ref()) {
            // Only draw a hint that still belongs to the caret's current line,
            // and not on the line the call-signature ghost has taken: both go
            // to the end of the line, and the signature is what is wrong there.
            if hint.line == line && quiet_line != Some(hint.line + 1) {
                let eol_idx = line_index.line_end_char_idx(hint.line + 1);
                show_inlay_hint(
                    ui,
                    editor_resp.galley_pos,
                    editor_clip,
                    &editor_resp.galley,
                    eol_idx,
                    &hint.label,
                    self.editor_font_size,
                );
                self.ed.inlay_hint_drawn = true;
            }
        }

        // ── Call-signature ghost ───────────────────────────────────────
        // Gated, like the type hint, by the "Types" toolbar button — not by
        // the inline-errors one: it answers "what does this call want?",
        // which is worth seeing whether or not the squiggles are.
        if let (Some(ghost), Some(eol)) = (&sig_ghost, sig_eol) {
            let pill_right = pill_edges
                .iter()
                .find(|(l, _)| *l == ghost.line)
                .map(|(_, r)| *r);
            super::signature_hint::draw_signature_ghost(
                ui,
                editor_resp.galley_pos,
                editor_clip,
                &editor_resp.galley,
                eol,
                pill_right,
                self.editor_font_size,
                ghost,
                egui::Id::new(("sig_hint", current_rel_path.as_deref(), slot)),
            );
        }
    }

    /// Hand the completion popup to `slot`, closing the other view's list.
    ///
    /// Only the OWNER's popup is ever closed or drawn, so a list the other view
    /// still had open would otherwise stay flagged open for good, with nothing
    /// on screen: that view's idle re-sync waits on it, Tab stops accepting a
    /// type hint and Escape stops dropping extra carets there.
    fn take_completion_ownership(&mut self, slot: crate::app::EditorSlot) {
        let previous = self.completion_owner;
        if previous != slot {
            let ed = self.ed_of(previous);
            ed.completion_open = false;
            ed.completion_note = None;
        }
        self.completion_owner = slot;
    }

    /// If the displayed file is NOT declared by its parent module (`mod x;`
    /// missing in the folder's `mod.rs`, or in `main.rs` for top-level files),
    /// return a hint naming the exact missing line. rust-analyzer detaches
    /// such files — no completions, no diagnostics — and every completion
    /// request in them answers `null`, which used to read as a popup that
    /// "appears and instantly disappears".
    ///
    /// `file` is the file the popup was opened in — the Reference editor's
    /// own, not `selected_file`, which is always the MAIN editor's and made the
    /// note describe a file the user was not looking at.
    fn unlinked_module_hint(&self, file: ProjectFileId) -> Option<String> {
        let ProjectFileId::UserFile(i) = file else {
            return None; // main.rs (and config files) are always linked
        };
        let (name, _) = self.project_tree.user_src_files.get(i)?;
        let stem = name.rsplit('/').next()?.strip_suffix(".rs")?.to_owned();
        if stem == "mod" {
            return None; // a mod.rs is declared by ITS parent — keep it simple
        }
        let (parent_label, parent_text) = match name.rsplit_once('/') {
            Some((dir, _)) => {
                let parent_rel = format!("{dir}/mod.rs");
                let text = self
                    .project_tree
                    .user_src_files
                    .iter()
                    .find(|(n, _)| *n == parent_rel)?
                    .1
                    .as_str();
                (parent_rel.clone(), text)
            }
            None => ("src/main.rs".to_owned(), self.generated_code.as_str()),
        };
        (!mod_declared_in(parent_text, &stem)).then(|| {
            format!(
                "no suggestions — this file is not in the module tree: add \
                 `mod {stem};` to {parent_label} (rust-analyzer skips \
                 undeclared files entirely)"
            )
        })
    }
}

/// `true` when `parent_text` declares the child module `stem` — accepts
/// `mod x;`, `pub mod x;`, `pub(crate) mod x;` and `mod x {`; comment lines
/// don't count. Used by [`AppIde::unlinked_module_hint`].
fn mod_declared_in(parent_text: &str, stem: &str) -> bool {
    parent_text.lines().any(|l| {
        let l = l.trim();
        if l.starts_with("//") {
            return false;
        }
        let mut words = l.split_whitespace().peekable();
        while let Some(w) = words.next() {
            if w == "mod" {
                if let Some(&next) = words.peek() {
                    let ident = next.trim_end_matches([';', '{']).trim();
                    return ident == stem;
                }
            }
        }
        false
    })
}

/// `true` when 1-based `line` of the indexed text can no longer hold the code a
/// compiler diagnostic was computed for: it is blank, a pure `//` line comment,
/// or past the end of the file.
///
/// Used to drop flycheck diagnostics whose position went stale — commenting a
/// line out is the common case, and rustc never reports `mismatched types` on a
/// comment.
///
/// Through the index, not `text.lines().nth(..)`: that walked the file from the
/// top once per flycheck diagnostic per frame, under the `LspState` lock.
fn line_is_gone(line_index: &LineIndex, line: u32) -> bool {
    match line_index.line_str(line.saturating_sub(1) as usize) {
        Some(l) => {
            let t = l.trim_start();
            t.is_empty() || t.starts_with("//")
        }
        None => true, // the line was deleted outright
    }
}

/// The note for a completion request rust-analyzer answered with an error.
/// A transient code only gets here after its retries ran out, so it says so.
fn empty_completion_note_for_error(code: i64, message: &str) -> String {
    let message = message.trim();
    if lsp::is_transient_lsp_error(code) {
        "rust-analyzer kept cancelling the request (files changing) — try again".to_owned()
    } else if message.is_empty() {
        format!("rust-analyzer refused the request (error {code})")
    } else {
        format!("rust-analyzer refused the request: {message}")
    }
}

/// Order completion items so those whose label starts with `prefix` (case-
/// insensitive) come first, keeping each group in the server's original order,
/// then the rest — so the popup leads with what the user has already typed.
/// An empty prefix returns the list unchanged (the server's relevance order).
fn order_by_prefix(items: Vec<lsp::CompletionItem>, prefix: &str) -> Vec<lsp::CompletionItem> {
    if prefix.is_empty() {
        return items;
    }
    let pl = prefix.to_lowercase();
    let (mut starts, rest): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|it| it.label.to_lowercase().starts_with(&pl));
    starts.extend(rest);
    starts
}

/// The completion popup's rows, kept in the view between frames.
///
/// The rows are `order_by_prefix` of rust-analyzer's list, and they were
/// rebuilt every frame the popup was open: a deep copy of every item under the
/// LSP lock, a lowercase copy of every label, and a second copy of the result.
/// Now they are rebuilt only when the list or the prefix changed. The key
/// handlers in `mod.rs` read these same rows, so what they accept is exactly
/// what was drawn.
#[derive(Default)]
pub(crate) struct CompletionRows {
    rows: Arc<Vec<lsp::CompletionItem>>,
    /// The list and prefix `rows` was built from; `None` after `clear`.
    ///
    /// Comparing the list by `Arc::ptr_eq` is exact: the `Arc` held here keeps
    /// the allocation alive, so its address cannot be reused, and `LspState`
    /// replaces its list rather than editing it.
    source: Option<(Arc<Vec<lsp::CompletionItem>>, String)>,
    /// The detail panel's documentation and its parsed lines, keyed on the
    /// text itself.
    doc: Option<(String, Vec<doc_md::DocLine>)>,
}

impl CompletionRows {
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(crate) fn get(&self, i: usize) -> Option<&lsp::CompletionItem> {
        self.rows.get(i)
    }

    /// No rows until the next `update`.
    pub(crate) fn clear(&mut self) {
        self.rows = Arc::default();
        self.source = None;
    }

    /// The rows for `items` with `prefix` typed: always equal to
    /// `order_by_prefix(items.to_vec(), prefix)`, and stored for the key
    /// handlers.
    fn update(
        &mut self,
        items: &Arc<Vec<lsp::CompletionItem>>,
        prefix: &str,
    ) -> Arc<Vec<lsp::CompletionItem>> {
        let current = matches!(
            &self.source,
            Some((list, p)) if Arc::ptr_eq(list, items) && p == prefix
        );
        if !current {
            self.rows = if prefix.is_empty() {
                // `order_by_prefix` returns the list unchanged: share it.
                Arc::clone(items)
            } else {
                Arc::new(order_by_prefix(items.to_vec(), prefix))
            };
            self.source = Some((Arc::clone(items), prefix.to_owned()));
        }
        Arc::clone(&self.rows)
    }

    /// `doc_md::parse_doc(md)`, parsed again only when `md` changed.
    fn doc_lines(&mut self, md: &str) -> &[doc_md::DocLine] {
        if !matches!(&self.doc, Some((text, _)) if text == md) {
            self.doc = Some((md.to_owned(), doc_md::parse_doc(md)));
        }
        self.doc.as_ref().map_or(&[], |(_, lines)| lines)
    }
}

/// Height of one completion row, item spacing excluded.
const COMPLETION_ROW_H: f32 = 19.0;

/// The completion list at `popup_pos`, with row `sel` highlighted and scrolled
/// into view. Returns the row clicked this frame, if any.
fn show_completion_list(
    ctx: &egui::Context,
    popup_pos: egui::Pos2,
    items: &[lsp::CompletionItem],
    sel: usize,
) -> Option<usize> {
    let mut clicked = None;
    // `interactable` defaults to true → mouse clicks work.
    egui::Area::new(egui::Id::new("lsp_completion_popup"))
        .fixed_pos(popup_pos)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(&ui.ctx().global_style()).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.set_min_width(440.0);
                ui.set_max_width(440.0);

                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, item) in items.iter().enumerate() {
                            if show_completion_row(ui, item, i == sel) {
                                clicked = Some(i);
                            }
                        }
                    }); // ScrollArea
            }); // Frame
        }); // Area
    clicked
}

/// One row of the completion list. Returns whether it was clicked.
///
/// Every row is still allocated, so row ids, hit-testing and the selected
/// row's `scroll_to_me` are exactly what they were. `ScrollArea::show_rows` is
/// not used on purpose: once off-screen rows stop existing, a list scrolled by
/// whole rows between two frames gives a row's rect to a new id while the old
/// id is gone, and debug builds outline that in red
/// (`warn_if_rect_changes_id`).
fn show_completion_row(ui: &mut egui::Ui, item: &lsp::CompletionItem, selected: bool) -> bool {
    let fg = if selected {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_rgb(200, 210, 230)
    };
    let sel_bg = egui::Color32::from_rgb(40, 90, 160);
    let hover_bg = egui::Color32::from_rgb(50, 60, 80);

    // Allocate the full row width for hit-testing.
    let avail_w = ui.available_width();
    let (rect, row_resp) =
        ui.allocate_exact_size(egui::vec2(avail_w, COMPLETION_ROW_H), egui::Sense::click());

    // Only what can show is painted. The label and background stay within a
    // few px of `rect`, so a row a whole row height clear of the clip rect
    // cannot reach a visible pixel, and its `format!` and text layout are
    // skipped.
    if ui.clip_rect().expand(COMPLETION_ROW_H).intersects(rect) {
        // Background (selected / hovered).
        if selected {
            ui.painter().rect_filled(rect, 2.0, sel_bg);
        } else if row_resp.hovered() {
            ui.painter().rect_filled(rect, 2.0, hover_bg);
        }

        let painter = ui.painter();
        let icon = lsp_kind_icon(item.kind);
        let label = format!("{} {}", icon, item.label);

        // Icon + label — left-aligned.
        painter.text(
            rect.left_center() + egui::vec2(4.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &label,
            egui::FontId::monospace(12.0),
            fg,
        );
    }

    // The type signature is deliberately NOT repeated per row. It used to be
    // right-aligned in the same 440 px as the label, so a long name
    // (`into_open_drain_output_with_state`) and its signature ran into each
    // other and both became unreadable. The panel beside the popup already
    // shows the focused item's full signature, untruncated.

    // Scroll selected item into view.
    if selected {
        row_resp.scroll_to_me(None);
    }

    // No hover tooltip: the panel to the right already shows the FOCUSED
    // item's signature and docs. Two popups describing two different items at
    // once was the confusing part.

    row_resp.clicked()
}

/// Draw rustdoc markdown in the completion detail panel.
///
/// Code examples get monospace on a tinted band: once the ` ``` ` fences are
/// stripped they are otherwise indistinguishable from the prose around them,
/// which is what made multi-paragraph docs hard to read. Parsing (including
/// which lines are code at all) lives in `doc_md`.
fn render_doc(ui: &mut egui::Ui, lines: &[doc_md::DocLine]) {
    // Prose stays the muted grey it always was; code borrows the editor's
    // warmer tone so the two are separable at a glance.
    const BODY: egui::Color32 = egui::Color32::from_rgb(200, 205, 215);
    const HEADING: egui::Color32 = egui::Color32::from_rgb(238, 242, 250);
    const CODE: egui::Color32 = egui::Color32::from_rgb(206, 214, 160);
    const COMMENT: egui::Color32 = egui::Color32::from_rgb(126, 137, 150);
    const CODE_BG: egui::Color32 = egui::Color32::from_rgb(38, 41, 48);

    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            doc_md::DocKind::Blank => {
                ui.add_space(4.0);
                i += 1;
            }

            // Consecutive code/comment lines form ONE band — a frame per line
            // would draw a stack of separate boxes instead of a block.
            doc_md::DocKind::Code | doc_md::DocKind::Comment => {
                let start = i;
                while i < lines.len()
                    && matches!(
                        lines[i].kind,
                        doc_md::DocKind::Code | doc_md::DocKind::Comment
                    )
                {
                    i += 1;
                }
                egui::Frame::new()
                    .fill(CODE_BG)
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .corner_radius(3.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 1.0;
                        for line in &lines[start..i] {
                            let color = if line.kind == doc_md::DocKind::Comment {
                                COMMENT
                            } else {
                                CODE
                            };
                            ui.label(
                                egui::RichText::new(&line.text)
                                    .monospace()
                                    .size(11.0)
                                    .color(color),
                            );
                        }
                    });
            }

            // egui has no font-weight axis (`FontId` is size + family only), so
            // heading levels separate by SIZE and brightness rather than by
            // 700/900 weight. `strong()` only shifts colour.
            doc_md::DocKind::Heading(level) => {
                if i > 0 {
                    ui.add_space(2.0);
                }
                let size = if level <= 1 { 13.0 } else { 12.0 };
                ui.label(
                    egui::RichText::new(&lines[i].text)
                        .size(size)
                        .strong()
                        .color(HEADING),
                );
                i += 1;
            }

            doc_md::DocKind::Body => {
                ui.label(egui::RichText::new(&lines[i].text).size(11.0).color(BODY));
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompletionRows, doc_md, line_is_gone, mod_declared_in, order_by_prefix,
        show_completion_list, wanted_inline,
    };
    use crate::editor::gui::text_pos::{LineIndex, lsp_kind_icon};
    use crate::lsp::{CompletionItem, DiagSeverity};
    use eframe::egui;
    use std::sync::Arc;

    /// The bug this guards: a warning is the MOST common non-error diagnostic
    /// rust-analyzer publishes, and the old `Error | Info` rule dropped it. With
    /// the info switch on, every non-error severity has to reach the editor.
    #[test]
    fn the_info_switch_covers_every_severity_that_is_not_an_error() {
        for sev in [
            DiagSeverity::Warning,
            DiagSeverity::Info,
            DiagSeverity::Hint,
        ] {
            assert!(
                wanted_inline(sev, false, true),
                "{sev:?} must be drawn when inline info is on"
            );
        }
    }

    /// The two switches are independent: neither half can turn the other on or
    /// off. That is the whole point of there being two buttons.
    #[test]
    fn the_two_switches_do_not_reach_into_each_other() {
        // Errors only.
        assert!(wanted_inline(DiagSeverity::Error, true, false));
        assert!(!wanted_inline(DiagSeverity::Warning, true, false));
        // Info only.
        assert!(!wanted_inline(DiagSeverity::Error, false, true));
        assert!(wanted_inline(DiagSeverity::Warning, false, true));
    }

    #[test]
    fn both_off_draws_nothing_at_all() {
        for sev in [
            DiagSeverity::Error,
            DiagSeverity::Warning,
            DiagSeverity::Info,
            DiagSeverity::Hint,
        ] {
            assert!(!wanted_inline(sev, false, false), "{sev:?} leaked through");
        }
    }

    /// The unlinked-file detector: every accepted `mod` declaration shape
    /// counts, comments and other modules don't.
    #[test]
    fn mod_declaration_shapes_are_recognised() {
        let parent = "// New file\n\
                      pub mod data;\n\
                      mod radar;\n\
                      pub(crate) mod send_models;\n\
                      mod inline { }\n\
                      // mod commented_out;\n\
                      pub use radar::*;\n";
        assert!(mod_declared_in(parent, "data"));
        assert!(mod_declared_in(parent, "radar"));
        assert!(mod_declared_in(parent, "send_models"));
        assert!(mod_declared_in(parent, "inline"));
        assert!(!mod_declared_in(parent, "commented_out"));
        // The real bug that motivated this: the declaration simply missing.
        assert!(!mod_declared_in(parent, "read_report_admin"));
        // `use radar::*` alone must NOT count as declaring `radar`… checked
        // via a parent that only re-exports:
        assert!(!mod_declared_in("pub use radar::*;\n", "radar"));
    }

    fn items(labels: &[&str]) -> Vec<CompletionItem> {
        labels
            .iter()
            .map(|l| CompletionItem {
                label: (*l).to_string(),
                ..Default::default()
            })
            .collect()
    }

    fn labels(items: Vec<CompletionItem>) -> Vec<String> {
        items.into_iter().map(|i| i.label).collect()
    }

    #[test]
    fn prefix_matches_lead_then_the_rest() {
        // RA's order mixes matches and non-matches; "n" items should bubble up
        // first (in their original order), the rest follow (in their order).
        let got = order_by_prefix(items(&["len", "new", "abs", "next", "map"]), "n");
        assert_eq!(labels(got), ["new", "next", "len", "abs", "map"]);
    }

    #[test]
    fn case_insensitive_and_empty_prefix_unchanged() {
        let got = order_by_prefix(items(&["Node", "abs", "new"]), "n");
        assert_eq!(labels(got), ["Node", "new", "abs"]);
        // Empty prefix preserves the server's order.
        let got = order_by_prefix(items(&["b", "a", "c"]), "");
        assert_eq!(labels(got), ["b", "a", "c"]);
    }

    /// xorshift64: deterministic, so a failing step replays.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    /// Every field differs per `serial`, so a row taken from the wrong list
    /// cannot compare equal by accident.
    fn full_item(label: &str, serial: usize) -> CompletionItem {
        CompletionItem {
            label: label.to_owned(),
            kind: (serial % 26) as u8,
            detail: format!("detail {serial}"),
            insert_text: format!("insert {serial}"),
            insert_is_snippet: serial.is_multiple_of(2),
            documentation: format!("doc {serial}"),
        }
    }

    /// Rows as the key handlers in `mod.rs` read them, one past the end too.
    fn read_rows(rows: &CompletionRows) -> Vec<String> {
        (0..rows.len() + 2)
            .map(|i| format!("{:?}", rows.get(i)))
            .collect()
    }

    fn read_vec(items: &[CompletionItem]) -> Vec<String> {
        (0..items.len() + 2)
            .map(|i| format!("{:?}", items.get(i)))
            .collect()
    }

    /// The memo answers exactly what re-ordering every frame did, through any
    /// mix of new answers, equal answers in a new `Arc`, prefix edits, new
    /// triggers (`clear`) and frames where nothing changed. Labels and prefixes
    /// include case pairs whose lowercase form is longer (`İ`), `ß`, astral
    /// chars and the empty string.
    #[test]
    fn completion_rows_always_equal_order_by_prefix() {
        const LABELS: &[&str] = &[
            "",
            "a",
            "A",
            "ab",
            "Ab",
            "aB",
            "b",
            "ß",
            "SS",
            "ss",
            "İ",
            "i",
            "i\u{307}x",
            "_x",
            "x_",
            "é",
            "É",
            "😀",
            "a😀",
        ];
        const PREFIXES: &[&str] = &[
            "", "a", "A", "ab", "AB", "b", "i", "İ", "i\u{307}", "ss", "ß", "é", "É", "x", "_",
            "😀", "zz",
        ];
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut rows = CompletionRows::default();
        let mut list: Arc<Vec<CompletionItem>> = Arc::default();
        let mut prefix = String::new();
        let mut serial = 0;
        let mut last: Option<Arc<Vec<CompletionItem>>> = None;
        let mut reused = 0;
        for step in 0..4000 {
            match rng.below(6) {
                0 => {
                    let n = rng.below(9);
                    list = Arc::new(
                        (0..n)
                            .map(|_| {
                                serial += 1;
                                full_item(LABELS[rng.below(LABELS.len())], serial)
                            })
                            .collect(),
                    );
                }
                1 => list = Arc::new(list.to_vec()),
                2 => prefix = PREFIXES[rng.below(PREFIXES.len())].to_owned(),
                3 => {
                    rows.clear();
                    assert!(rows.is_empty() && rows.get(0).is_none());
                    assert_eq!(rows.len(), 0);
                }
                _ => {}
            }
            let got = rows.update(&list, &prefix);
            let want = order_by_prefix(list.to_vec(), &prefix);
            assert_eq!(read_vec(&got), read_vec(&want), "step {step} {prefix:?}");
            assert_eq!(read_rows(&rows), read_vec(&want), "step {step} {prefix:?}");
            assert_eq!(rows.is_empty(), want.is_empty(), "step {step}");
            if last.as_ref().is_some_and(|l| Arc::ptr_eq(l, &got)) {
                reused += 1;
            }
            last = Some(got);
        }
        assert!(reused > 1000, "only {reused} frames reused the rows");
    }

    /// The point of the memo: an unchanged frame hands back the same rows, and
    /// each thing that can change them forces a rebuild.
    #[test]
    fn completion_rows_are_rebuilt_only_when_the_list_or_prefix_changes() {
        let list = Arc::new(items(&["len", "new", "next"]));
        let mut rows = CompletionRows::default();
        let first = rows.update(&list, "n");
        assert!(Arc::ptr_eq(&first, &rows.update(&list, "n")));
        assert!(!Arc::ptr_eq(&first, &rows.update(&list, "ne")));
        let again = rows.update(&list, "n");
        let equal_answer = Arc::new(list.to_vec());
        assert!(!Arc::ptr_eq(&again, &rows.update(&equal_answer, "n")));
        // No prefix: the list itself, not a copy of it.
        assert!(Arc::ptr_eq(&equal_answer, &rows.update(&equal_answer, "")));
        // A new trigger empties the rows, and the next frame rebuilds them
        // although neither the list nor the prefix changed.
        rows.clear();
        assert!(rows.is_empty());
        let rebuilt = rows.update(&equal_answer, "");
        assert_eq!(labels(rebuilt.to_vec()), ["len", "new", "next"]);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn doc_lines_equal_parse_doc_and_are_parsed_once_per_text() {
        let docs = [
            "",
            "Plain sentence.",
            "# Heading\n\nBody.\n\n```\n# use std::fmt;\nlet x = 1;\n## escaped\n#[attr]\n```\nAfter.",
            "Plain sentence.",
            "```rust\n// comment\nfn f() {}\n```",
        ];
        let mut rows = CompletionRows::default();
        for _ in 0..2 {
            for md in docs {
                // A fresh allocation each time: the key is the text, not where
                // it lives.
                let owned = md.to_owned();
                assert_eq!(rows.doc_lines(&owned), doc_md::parse_doc(md).as_slice());
            }
        }
        let text = docs[2].to_owned();
        let first = rows.doc_lines(&text).as_ptr();
        assert_eq!(first, rows.doc_lines(docs[2]).as_ptr());
        assert_ne!(first, rows.doc_lines(docs[1]).as_ptr());
    }

    /// The completion list exactly as it was drawn before off-screen rows were
    /// skipped: every row formatted and painted.
    fn show_list_painting_every_row(
        ctx: &egui::Context,
        popup_pos: egui::Pos2,
        items: &[CompletionItem],
        sel: usize,
    ) -> Option<usize> {
        let mut clicked = None;
        egui::Area::new(egui::Id::new("lsp_completion_popup"))
            .fixed_pos(popup_pos)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(&ui.ctx().global_style()).show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.set_min_width(440.0);
                    ui.set_max_width(440.0);

                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (i, item) in items.iter().enumerate() {
                                let selected = i == sel;
                                let fg = if selected {
                                    egui::Color32::WHITE
                                } else {
                                    egui::Color32::from_rgb(200, 210, 230)
                                };
                                let sel_bg = egui::Color32::from_rgb(40, 90, 160);
                                let hover_bg = egui::Color32::from_rgb(50, 60, 80);
                                let row_h = 19.0;
                                let avail_w = ui.available_width();
                                let (rect, row_resp) = ui.allocate_exact_size(
                                    egui::vec2(avail_w, row_h),
                                    egui::Sense::click(),
                                );
                                if selected {
                                    ui.painter().rect_filled(rect, 2.0, sel_bg);
                                } else if row_resp.hovered() {
                                    ui.painter().rect_filled(rect, 2.0, hover_bg);
                                }
                                let painter = ui.painter();
                                let icon = lsp_kind_icon(item.kind);
                                let label = format!("{} {}", icon, item.label);
                                painter.text(
                                    rect.left_center() + egui::vec2(4.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    &label,
                                    egui::FontId::monospace(12.0),
                                    fg,
                                );
                                if row_resp.clicked() {
                                    clicked = Some(i);
                                }
                                if selected {
                                    row_resp.scroll_to_me(None);
                                }
                            }
                        });
                });
            });
        clicked
    }

    type ListFn = fn(&egui::Context, egui::Pos2, &[CompletionItem], usize) -> Option<usize>;

    /// One frame's result: the clicked row, and every tessellated mesh with its
    /// clip rect.
    type FrameOut = (Option<usize>, Vec<(egui::Rect, egui::Mesh)>);

    fn run_list(
        list: ListFn,
        items: &[CompletionItem],
        pos: egui::Pos2,
        zoom: f32,
        frames: &[(usize, Vec<egui::Event>)],
    ) -> Vec<FrameOut> {
        let ctx = egui::Context::default();
        ctx.set_zoom_factor(zoom);
        let input = |n: usize, events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            time: Some(n as f64 / 60.0),
            predicted_dt: 1.0 / 60.0,
            events,
            ..Default::default()
        };
        // Every label's glyphs enter the font atlas first. Painting every row
        // would otherwise place them in another order, and the meshes would
        // differ in texture coordinates alone.
        let _ = crate::headless::run_ui(&ctx, input(0, Vec::new()), |ui| {
            for item in items {
                let label = format!("{} {}", lsp_kind_icon(item.kind), item.label);
                let font = egui::FontId::monospace(12.0);
                let _ = ui
                    .painter()
                    .layout_no_wrap(label, font, egui::Color32::WHITE);
            }
        });
        frames
            .iter()
            .enumerate()
            .map(|(n, (sel, events))| {
                let mut clicked = None;
                let out = crate::headless::run_ui(&ctx, input(n + 1, events.clone()), |ui| {
                    clicked = list(ui.ctx(), pos, items, *sel);
                });
                let meshes = ctx
                    .tessellate(out.shapes, out.pixels_per_point)
                    .into_iter()
                    .map(|p| match p.primitive {
                        egui::epaint::Primitive::Mesh(mesh) => (p.clip_rect, mesh),
                        egui::epaint::Primitive::Callback(_) => unreachable!("no callbacks"),
                    })
                    .collect();
                (clicked, meshes)
            })
            .collect()
    }

    /// Selection walking one row per frame past the bottom, held, jumping to
    /// the top, re-targeted mid-animation; then the pointer hovers the list,
    /// wheels it both ways (the list scrolls, then the selection pulls it
    /// back) and clicks a row.
    fn list_script(len: usize, pos: egui::Pos2) -> Vec<(usize, Vec<egui::Event>)> {
        let last = len - 1;
        let mut frames = Vec::new();
        let hold = |frames: &mut Vec<_>, sel: usize, n: usize| {
            for _ in 0..n {
                frames.push((sel, Vec::new()));
            }
        };
        hold(&mut frames, 0, 3);
        for sel in 0..=last {
            frames.push((sel, Vec::new()));
        }
        hold(&mut frames, last, 10);
        hold(&mut frames, 0, 10);
        for sel in [last / 2, last.min(3), last, last / 3] {
            hold(&mut frames, sel, 2);
        }
        let over = pos + egui::vec2(60.0, 40.0);
        let sel = last.min(5);
        frames.push((sel, vec![egui::Event::PointerMoved(over)]));
        for dy in [-1.0, -1.0, 1.0, -3.0] {
            let wheel = egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, dy),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            };
            frames.push((sel, vec![wheel]));
            hold(&mut frames, sel, 2);
        }
        // Past the scroll animation, so both halves of the click land on the
        // same row.
        hold(&mut frames, sel, 30);
        for pressed in [true, false] {
            let button = egui::Event::PointerButton {
                pos: over,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frames.push((sel, vec![button]));
        }
        hold(&mut frames, sel, 2);
        frames
    }

    /// Skipping the paint of rows far outside the clip rect changes nothing a
    /// renderer receives: per frame, the same clicked row and the same meshes,
    /// vertex for vertex. That also pins the scroll offset, since every row's
    /// position depends on it. Covers a 60-row list (rust-analyzer's cap), one
    /// pushed back on screen at 1.25 zoom, and a list too short to scroll.
    #[test]
    fn skipping_off_screen_rows_paints_the_same_meshes() {
        let many: Vec<CompletionItem> = (0..60)
            .map(|i| full_item(&format!("item_{i:02}_{}", "w".repeat(i % 9)), i * 7))
            .collect();
        let few: Vec<CompletionItem> = many[..3].to_vec();
        let mut clicks = 0;
        for (items, pos, zoom) in [
            (&many, egui::pos2(100.3, 50.7), 1.0),
            (&many, egui::pos2(700.6, 640.2), 1.25),
            (&few, egui::pos2(10.0, 10.0), 1.0),
        ] {
            let script = list_script(items.len(), pos);
            let old = run_list(show_list_painting_every_row, items, pos, zoom, &script);
            let new = run_list(show_completion_list, items, pos, zoom, &script);
            assert_eq!(old.len(), new.len());
            for (n, ((old_click, old_meshes), (new_click, new_meshes))) in
                old.iter().zip(&new).enumerate()
            {
                assert_eq!(old_click, new_click, "{pos:?} frame {n}: click");
                assert_eq!(old_meshes.len(), new_meshes.len(), "{pos:?} frame {n}");
                for (m, (a, b)) in old_meshes.iter().zip(new_meshes).enumerate() {
                    assert!(a == b, "{pos:?} frame {n}: mesh {m} differs");
                }
                clicks += usize::from(old_click.is_some());
            }
        }
        assert!(
            clicks >= 2,
            "the click landed on a row in only {clicks} lists"
        );
    }

    /// Regression: a rustc diagnostic keeps the line it was computed for, so
    /// after commenting that line out the squiggle used to sit on the comment
    /// until the next Save re-ran cargo check.
    #[test]
    fn a_commented_or_deleted_line_counts_as_gone() {
        let text = "fn a() {}
    // was: foo(bar);

fn b() {}
";
        let text = &LineIndex::new(text);
        assert!(!line_is_gone(text, 1), "real code stays");
        assert!(line_is_gone(text, 2), "commented out");
        assert!(line_is_gone(text, 3), "blank");
        assert!(!line_is_gone(text, 4), "real code stays");
        assert!(line_is_gone(text, 99), "past EOF - the line was deleted");
    }

    /// The index-based check answers exactly what the old
    /// `text.lines().nth(line - 1)` form did, on every line of texts with CRLF,
    /// a lone `'\r'`, indented comments, no trailing newline and line 0.
    #[test]
    fn line_is_gone_matches_the_lines_walk() {
        fn old(text: &str, line: u32) -> bool {
            match text.lines().nth(line.saturating_sub(1) as usize) {
                Some(l) => {
                    let t = l.trim_start();
                    t.is_empty() || t.starts_with("//")
                }
                None => true,
            }
        }
        for text in [
            "",
            "\n",
            "a",
            "a\n\n",
            "fn a() {}\r\n  // x\r\n\r\n\tb();\r",
            "\r\n\r",
            "  //\n x //\n/\n/ /\n    ",
            "ă\n  // ș\n😀",
        ] {
            let index = LineIndex::new(text);
            for line in (0..8).chain([u32::MAX]) {
                assert_eq!(
                    line_is_gone(&index, line),
                    old(text, line),
                    "{text:?} line {line}"
                );
            }
        }
    }
}
