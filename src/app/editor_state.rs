//! The state of ONE code editor view.
//!
//! Everything here is a property of a VIEW, not of a file: which popup is open,
//! where the caret was, what the find bar holds. `AppIde` carries two of them —
//! the main editor's and the Reference editor's — and swaps them around the
//! second editor's frame (see [`AppIde::with_editor`]).
//!
//! That swap is what makes a second full-featured editor affordable. The ~470
//! places that read this state all mean "the editor being drawn right now", and
//! the two views never run at the same time: the main panel renders first, the
//! Reference tab later inside the MCU panel. So one swap point replaces ~470
//! individual decisions about which view is meant.
//!
//! What is NOT here is as deliberate as what is. State keyed by FILE stays on
//! `AppIde` and stays shared — breakpoints, folds, the git diff cache, the LSP
//! connection. A breakpoint is a property of the file, not of the window
//! looking at it, and splitting those would break the case that already works:
//! the same file's marks agreeing in both views.

use crate::app::{PinHighlight, ProjectFileId, editor_panel, lsp};
use crate::editor::gui::text_pos::LineIndexCache;
use eframe::egui;
use egui_code_editor::{Completer, Syntax};

#[derive(Default)]
pub(crate) struct EditorState {
    /// The code editor's egui widget id, captured after each render. Needed a
    /// frame LATER, and before the widget exists: a folded editor is
    /// non-interactive and therefore unfocused, so the keystroke that unfolds it
    /// must also hand focus back — otherwise the file stays untypable until the
    /// user clicks into it.
    pub(crate) editor_widget_id: Option<egui::Id>,

    /// Set by a fold toggle: `(rel path, the block's header line, the screen y
    /// it had BEFORE the toggle)`. The next frame re-anchors the scroll offset
    /// so that line stays exactly where it was — folding 200 lines otherwise
    /// slides the whole page under the pointer.
    pub(crate) fold_anchor: Option<(String, usize, f32)>,
    /// Ctrl+Shift+Q or the menu's "toggle collapse all" was asked for, and
    /// the file it was asked on. Applied after the render, where the current
    /// galley exists to measure a fold anchor against — the way a gutter click
    /// already works. Without an anchor, expanding every function above the
    /// caret left the scroll offset alone and the caret slid off the bottom of
    /// the view.
    ///
    /// Keyed by file, and dropped on any frame showing another one: a request
    /// raised on a library's `Cargo.toml` (where nothing consumes it) used to
    /// fire on the next Rust file opened, folding every function unasked.
    pub(crate) fold_all_requested: Option<String>,

    /// Code-completion engine — stores the trie, current prefix and popup state.
    /// Must live in the App (not a local) so state is preserved across frames.
    pub(crate) completer: Completer,

    /// True when the LSP completion popup is visible.
    /// Idle LSP re-sync (see `editor_panel::idle_sync`): the file and text hash
    /// this view drew, and when that pair last changed.
    ///
    /// Per view because each editor shows its own file and settles on its own
    /// clock; the two must not share one deadline.
    pub(crate) idle_sync: Option<(String, u64, std::time::Instant)>,
    pub(crate) completion_open: bool,

    /// Transient note shown at the cursor when a completion request came back
    /// EMPTY — a silent popup flash was undiagnosable. Carries the reason
    /// (e.g. "the file has no `mod …;` declaration") + when it appeared.
    pub(crate) completion_note: Option<(String, std::time::Instant)>,

    /// Index of the currently highlighted row in the completion popup.
    pub(crate) completion_sel: usize,

    /// Character-offset in the editor text where completion was triggered.
    /// Used to compute the live prefix for filtering and to close the popup
    /// when the cursor moves away.
    pub(crate) completion_trigger_idx: usize,

    /// Completion item deferred from a mouse-click on a popup row.
    /// Applied at the start of the next frame (before the editor renders);
    /// carries the whole item so snippet expansion sees `insert_is_snippet`.
    pub(crate) completion_pending_insert: Option<lsp::CompletionItem>,

    /// Filtered completion list from the last rendered frame.
    /// Key handlers (Tab / Enter / Arrow) use this so they always operate
    /// on the same slice the user sees, not the full unfiltered LSP list.
    /// Re-ordered only when the LSP list or the typed prefix changes: see
    /// `completion::CompletionRows`.
    pub(crate) completion_filtered_items: editor_panel::completion::CompletionRows,

    /// Cargo.toml dependency-completion popup (crate names + live crates.io
    /// versions). Independent of rust-analyzer.
    pub(crate) cargo_complete: editor_panel::cargo_complete::CargoCompleteState,

    /// Primary caret char-index from the previous frame, in BUFFER space, used
    /// to scroll the editor so the caret stays in view when it moves off-screen
    /// (e.g. Shift+Up/Down selection past the visible area). Buffer space, not
    /// the galley's: a fold toggled above the caret shifts the galley index
    /// without the caret going anywhere.
    pub(crate) last_caret_idx: Option<usize>,

    /// The primary caret as this view last drew it, in BUFFER space, WITH the
    /// file it is in - for the tabs beside the editor that follow it (Flow).
    /// `last_caret_idx` cannot say which file it belongs to: the editor draws
    /// before the project tree, so for the frame of a click there the caret
    /// is still the previous file's while `selected_file` is already the new
    /// one. `None` while the file shown has no caret.
    pub(crate) caret_at: Option<(ProjectFileId, usize)>,

    /// Pending "jump to this diagnostic": the target file and its 1-based line.
    /// Set when a row in the Cargo Check / rust-analyzer tab is clicked; applied
    /// once the editor is displaying that file (scrolls the line to row ~10).
    pub(crate) pending_scroll_to_line: Option<(ProjectFileId, usize)>,

    /// The file + 1-based line + band colour of the last-clicked diagnostic.
    /// Highlighted with a translucent band (colour keyed by severity, see
    /// `diag_highlight_color`) in the editor until another diagnostic is clicked.
    pub(crate) highlighted_error_line: Option<(ProjectFileId, usize, egui::Color32)>,

    /// The file + 1-based line of the last F12 go-to-definition that landed in a
    /// project file. Highlighted with a translucent yellow band (like the
    /// Definition tab) until the next F12.
    pub(crate) highlighted_def_line: Option<(ProjectFileId, usize)>,

    /// The pulsing "here is your pin" highlight, or `None` when none is running.
    pub(crate) highlighted_pin_lines: Option<PinHighlight>,

    /// Live "usages" analysis (fn/struct/enum/const/… fade-if-unused + a
    /// "references" popup) for whichever `.rs` file is currently displayed. See
    /// `editor_panel::usages`.
    pub(crate) usages: editor_panel::usages::UsagesState,

    /// Extra caret positions for Ctrl+Shift+Up/Down multi-cursor editing (char
    /// indices into the displayed file, in the order they were added — last
    /// added is popped first by Ctrl+Shift+Down). See `editor_panel::multi_cursor`.
    pub(crate) extra_cursors: Vec<editor_panel::multi_cursor::ExtraCaret>,

    /// Which file `extra_cursors` belongs to — cleared on a file switch so
    /// stale positions never leak into an unrelated file.
    pub(crate) extra_cursors_file: Option<ProjectFileId>,

    /// The primary caret's char index at the end of the previous frame — lets
    /// multi-cursor replay tell a Backspace (deletes BEFORE the cursor) apart
    /// from a Delete-key press (deletes AFTER it) — and, since it stores the
    /// whole `(anchor, head)` selection, typing OVER a selection apart from
    /// either, because then each caret replaces its OWN span.
    pub(crate) mc_prev_primary_sel: Option<(usize, usize)>,

    /// Did the code editor hold keyboard focus last frame? egui surrenders the
    /// focused widget on Escape before any of our code runs, so this is the
    /// only way to know whether the caret that just vanished was OURS — and
    /// therefore whether to take the focus back.
    pub(crate) editor_was_focused: bool,

    // ── Rename symbol (Ctrl+R → textDocument/rename) ─────────────────────────
    /// While `true`, the rename input popup is shown.
    pub(crate) rename_active: bool,

    /// The new name being typed in the rename popup (pre-filled with the symbol).
    pub(crate) rename_input: String,

    /// The symbol's name BEFORE the rename, captured when the popup opens, so
    /// the applied edits can be audited for occurrences RA did not reach.
    pub(crate) rename_old_name: String,

    /// The name submitted in the rename popup, so leftovers can be offered the
    /// same target.
    pub(crate) rename_new_name: String,

    /// File + 0-based (line, char) where the rename was triggered.
    pub(crate) rename_rel: String,

    pub(crate) rename_line: u32,

    pub(crate) rename_char: u32,

    /// Screen position to anchor the rename popup at.
    pub(crate) rename_popup_pos: egui::Pos2,

    /// `true` after a rename request was sent, until RA's edits are applied.
    pub(crate) rename_in_flight: bool,

    // ── Code actions (Ctrl+Enter — RA assists / quick-fixes) ─────────────────
    /// `true` after a codeAction request, until the list arrives.
    pub(crate) code_action_in_flight: bool,
    /// When that request went out. rust-analyzer restarting, or dropping the
    /// request, otherwise leaves `code_action_in_flight` true forever — and the
    /// early return it guards then refuses every later Ctrl+Enter, silently.
    pub(crate) code_action_sent_at: Option<std::time::Instant>,

    /// What the `codeAction/resolve` in flight is for, until its edits arrive:
    /// a preview of the chooser's selected row, or the chosen action itself.
    pub(crate) code_action_resolve_for: Option<CodeActionResolve>,

    /// A line under the chooser saying why an expected action is missing —
    /// "Add explicit type" on a binding whose type cannot be written.
    pub(crate) code_action_note: Option<String>,

    /// "Add explicit type" built from the caret line's type hint, offered when
    /// rust-analyzer's own assist is missing from the list — see
    /// `code_action::hint_type_action`.
    pub(crate) code_action_hint_type: Option<lsp::CodeAction>,

    /// The code actions to choose from (popup shown when > 1).
    pub(crate) code_actions: Vec<lsp::CodeAction>,

    /// Whether the chooser popup is open.
    pub(crate) code_action_popup_open: bool,

    /// Highlighted row in the chooser popup.
    pub(crate) code_action_sel: usize,

    /// Screen anchor for the chooser popup (the cursor rect when triggered).
    pub(crate) code_action_popup_pos: egui::Pos2,

    /// Chooser selection deferred to next frame's `init_frame` (so the edit
    /// applies at frame TOP, avoiding the display_code write-back revert).
    pub(crate) code_action_choice: Option<usize>,

    /// The crate identifier the "Add dependency" row offers, for the caret the
    /// last Ctrl+Enter was fired on. `Some` puts an extra row at the TOP of the
    /// code-action list — rust-analyzer never produces it, because it does not
    /// know Cargo.toml exists.
    pub(crate) code_action_add_dep: Option<String>,

    /// The crate chooser that row opens.
    pub(crate) add_dep: editor_panel::add_dep::AddDepState,

    /// Ctrl+Alt+Insert: the "move these lines into a new function" popup.
    pub(crate) extract: editor_panel::extract_fn::ExtractFnState,

    /// The inferred-type hint to draw as ghost text after an untyped `let` on
    /// the cursor's line, if any (its `text_edits` insert the type on Tab).
    /// Cleared when the caret leaves an untyped `let`.
    pub(crate) inlay_hint: Option<lsp::InlayHint>,

    /// `(rel_path, 0-based line)` the last inlay request was fired for — so we
    /// re-request when the caret moves to a different `let` line, or after RA
    /// re-syncs (the request key is reset while the file is dirty).
    /// `(file, 0-based line, 0-based column of the binding NAME)` of the inlay
    /// request in flight or already answered for the caret's line.
    ///
    /// The column is kept because rust-analyzer places a type hint immediately
    /// after the name it belongs to, and a line can hold more than one `let`.
    /// Matching on the line alone took whichever hint came first in the array.
    pub(crate) inlay_requested: Option<(String, u32, u32)>,
    /// `(analyzer generation, indexed)` when that request went out.
    ///
    /// An EMPTY answer used to latch the line for good: `inlay_requested` was
    /// set and never cleared, so a reply that arrived while rust-analyzer was
    /// still indexing silenced that line until the caret left it. Re-asking
    /// every frame instead would be per-frame LSP traffic on any line whose type
    /// genuinely cannot be inferred, so the retry is tied to the two events that
    /// can change the answer: a restart, and the end of indexing.
    pub(crate) inlay_asked_at: (u64, bool),

    /// Set when Tab is pressed while the ghost hint shows; the type is inserted
    /// at frame TOP next `init_frame` (like code actions, to dodge the revert).
    pub(crate) inlay_accept_pending: bool,

    /// Whether the hint was DRAWN last frame. Tab accepts only a hint the user
    /// can see: the call-signature ghost can take its line and hide it.
    pub(crate) inlay_hint_drawn: bool,

    /// The last scan for the caret's untyped `let`, reused while the text and
    /// caret are unchanged: see `inlay_hint::InlayScan`.
    pub(crate) inlay_scan: Option<editor_panel::inlay_hint::InlayScan>,

    /// The last scan for the calls around the caret, reused while the text
    /// and caret are unchanged: see `signature_hint::SigScan`.
    pub(crate) sig_scan: Option<editor_panel::signature_hint::SigScan>,
    /// The call-signature ghost hint: its request, its answer, and what was
    /// last shown (see `signature_hint::SigState`).
    pub(crate) sig: editor_panel::signature_hint::SigState,

    /// Request keyboard focus for the rename input on the frame it opens.
    pub(crate) rename_focus: bool,

    // ── Find / Replace (Ctrl+F / Ctrl+H / Ctrl+Shift+F / Ctrl+Shift+H) ───────
    /// Search bar state: mode, query/replacement text, results, match cursor.
    pub(crate) find: editor_panel::find_replace::FindReplace,

    /// Full-definition highlight set by a triple-click on a `{`/`}` — `(file,
    /// start, close)` inclusive char range, kept until the selection changes.
    pub(crate) full_block_selection: Option<(ProjectFileId, usize, usize)>,
    /// Live git diff vs HEAD for the file THIS view is showing.
    /// Per view, not shared: it caches exactly one file's hunks, so two
    /// views on different files would recompute over each other every
    /// frame.
    /// Editor gutter diff (live in-memory text vs HEAD) + revert-hunk state.
    pub(crate) diff_gutter: editor_panel::diff_gutter::DiffGutter,

    /// Line starts of the text this view's overlays draw against, reused
    /// across frames while that text is unchanged. Ask it with the exact text
    /// being drawn: see `text_pos::LineIndexCache`.
    pub(crate) line_index: LineIndexCache,

    /// Foldable regions of the text this view shows, lexed once per text
    /// rather than per frame: see `fold::RegionsCache`.
    pub(crate) fold_regions: editor_panel::fold::RegionsCache,
}

impl EditorState {
    /// A fresh view.
    ///
    /// Not `Default::default()`: the keyword completer has to be built with the
    /// Rust syntax so it has a word list, and `Default` cannot supply one.
    pub(crate) fn new() -> Self {
        Self {
            diff_gutter: editor_panel::diff_gutter::DiffGutter::default(),
            editor_widget_id: None,
            fold_anchor: None,
            fold_all_requested: None,
            // Completer: seeded with Rust keywords/types + learns words from code
            completer: Completer::new_with_syntax(&Syntax::rust())
                .with_auto_indent()
                .with_user_words(),
            idle_sync: None,
            completion_open: false,
            completion_note: None,
            completion_sel: 0,
            completion_trigger_idx: 0,
            completion_pending_insert: None,
            completion_filtered_items: editor_panel::completion::CompletionRows::default(),
            cargo_complete: editor_panel::cargo_complete::CargoCompleteState::default(),
            last_caret_idx: None,
            caret_at: None,
            pending_scroll_to_line: None,
            highlighted_error_line: None,
            highlighted_def_line: None,
            highlighted_pin_lines: None,
            usages: editor_panel::usages::UsagesState::default(),
            extra_cursors: Vec::new(),
            extra_cursors_file: None,
            mc_prev_primary_sel: None,
            editor_was_focused: false,
            rename_active: false,
            rename_input: String::new(),
            rename_old_name: String::new(),
            rename_new_name: String::new(),
            rename_rel: String::new(),
            rename_line: 0,
            rename_char: 0,
            rename_popup_pos: egui::Pos2::ZERO,
            rename_in_flight: false,
            code_action_in_flight: false,
            code_action_sent_at: None,
            code_action_resolve_for: None,
            code_action_note: None,
            code_action_hint_type: None,
            code_actions: Vec::new(),
            code_action_popup_open: false,
            code_action_sel: 0,
            code_action_popup_pos: egui::Pos2::ZERO,
            code_action_choice: None,
            code_action_add_dep: None,
            add_dep: editor_panel::add_dep::AddDepState::default(),
            extract: editor_panel::extract_fn::ExtractFnState::default(),
            inlay_hint: None,
            inlay_requested: None,
            inlay_asked_at: (0, false),
            inlay_accept_pending: false,
            inlay_hint_drawn: false,
            inlay_scan: None,
            sig_scan: None,
            sig: Default::default(),
            rename_focus: false,
            find: editor_panel::find_replace::FindReplace::default(),
            full_block_selection: None,
            line_index: LineIndexCache::default(),
            fold_regions: editor_panel::fold::RegionsCache::default(),
        }
    }
}

/// What an in-flight `codeAction/resolve` is for.
pub(crate) enum CodeActionResolve {
    /// The chooser's row at this index — resolved only to SHOW what it changes.
    Preview(usize),
    /// The chosen action — applied when its edit arrives.
    Apply(lsp::CodeAction),
}
