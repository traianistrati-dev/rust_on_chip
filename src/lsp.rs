//! Async rust-analyzer LSP client.
//!
//! # Lifecycle
//! ```text
//! call start()
//!   → spawn launch thread
//!       → spawn write thread  (rx → RA stdin)
//!       → send initialize
//!       → loop: read RA stdout → handle_incoming()
//!                 on initialize response → send initialized, set Indexing
//!                 on publishDiagnostics  → update LspState.diagnostics, set Ready
//!
//! call did_open() / did_change() from UI thread at any time.
//! ```
//!
//! # Stale-thread safety
//! A `generation` counter in `LspState` is incremented on every `start()`.
//! `handle_incoming` checks the generation before touching shared state, so
//! a lingering read-thread from a previous MCU type never corrupts new results.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

impl DiagSeverity {
    fn from_lsp(n: u64) -> Self {
        match n {
            1 => Self::Error,
            2 => Self::Warning,
            3 => Self::Info,
            _ => Self::Hint,
        }
    }
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error)
    }
    pub fn is_warning(&self) -> bool {
        matches!(self, Self::Warning)
    }
}

#[derive(Clone, Debug)]
pub struct LspDiagnostic {
    pub severity: DiagSeverity,
    pub message: String,
    /// 1-based line number (converted from LSP 0-based)
    pub line: u32,
    /// 1-based column
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    /// e.g. `"E0308"` or `"unused_variables"`
    pub code: Option<String>,
    /// LSP `source`: `"rust-analyzer"` for native (in-memory) diagnostics, or
    /// `"rustc"` / `"clippy"` for flycheck (cargo check) ones. Flycheck positions
    /// can't be re-mapped after an edit until the next check runs, so the inline
    /// overlay hides them while a re-check is pending (see `flycheck_stale`).
    pub source: String,
}

impl LspDiagnostic {
    /// First line of the message, for compact one-line rows. Rust-analyzer
    /// messages are frequently multi-line (e.g. "mismatched types\nexpected …")
    /// — the remainder is shown only on expand (RA tab detail) or hover.
    pub fn headline(&self) -> &str {
        self.message.lines().next().unwrap_or("").trim_end()
    }

    /// `true` when the message has meaningful content beyond the first line,
    /// so callers can hint that more is available (e.g. append "…").
    pub fn has_more_lines(&self) -> bool {
        self.message.lines().skip(1).any(|l| !l.trim().is_empty())
    }

    /// `true` for a numbered rustc compiler-error code (`E0425`, `E0308`, …) —
    /// as opposed to a named lint (`unused_variables`, `dead_code`, …).
    ///
    /// Both are reported with `source == "rustc"` over LSP, but they come from
    /// different places: a numbered error is a type/name-resolution "hard
    /// error" rust-analyzer's own in-memory analyzer computes and re-publishes
    /// on every edit — confirmed empirically (`rust-analyzer diagnostics
    /// --disable-build-scripts`, no cargo-check involved, finds `E0425`
    /// instantly even in a nested-module project). A named lint like
    /// `unused_variables`/`dead_code` genuinely requires an actual `cargo
    /// check`/`cargo clippy` pass (confirmed the same way: zero native
    /// diagnostics for those with `checkOnSave` off) — see
    /// `editor_panel::usages`'s doc comment for that investigation.
    ///
    /// `flycheck_stale()` exists to hide *genuinely* flycheck-sourced
    /// diagnostics once an edit shifts their line/col — but it must NOT also
    /// hide numbered hard errors, since those are live and always current.
    /// Same code-shape check already used for `rustc_error_doc_url` in the
    /// inline diagnostics overlay.
    pub fn is_rustc_error_code(&self) -> bool {
        self.code
            .as_deref()
            .and_then(|c| c.strip_prefix('E'))
            .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
    }
}

/// Why a completion request came back with nothing to show. Kept apart from an
/// empty list because the three have different cures, and a note reading "no
/// suggestions here" for all of them made a refused request undiagnosable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionFailure {
    /// `result: null` — rust-analyzer does not analyse the file at this point
    /// (most often: no `mod …;` declares it).
    Null,
    /// An error reply that survived the automatic retries.
    Error { code: i64, message: String },
}

/// LSP error codes meaning "asked at a bad moment, ask again": the document
/// changed under the request (`ContentModified`), or the server dropped it
/// (`RequestCancelled`, `ServerCancelled`). A `didChange` for ANY file — a Save
/// flush, the idle re-sync of another view, a settle re-verify — makes
/// rust-analyzer answer an in-flight completion this way within milliseconds.
pub fn is_transient_lsp_error(code: i64) -> bool {
    // -32802 ServerCancelled, -32801 ContentModified, -32800 RequestCancelled.
    (-32802..=-32800).contains(&code)
}

/// How many times a cancelled completion is re-sent before it is reported.
const COMPLETION_RETRIES: u8 = 3;

/// A single item returned by a `textDocument/completion` response.
#[derive(Clone, Debug, Default)]
pub struct CompletionItem {
    pub label: String,
    /// LSP CompletionItemKind (1=Text, 2=Method, 3=Function, 5=Field, 6=Variable, …)
    pub kind: u8,
    /// Short type / signature string shown inline next to the label
    pub detail: String,
    /// Text actually inserted when the item is accepted
    /// (falls back to `label` when the server doesn't send `insertText`)
    pub insert_text: String,
    /// True when `insert_text` is an LSP snippet (`insertTextFormat == 2`,
    /// e.g. `foo(${1:a})$0`) — expanded on accept by `snippet::expand`.
    pub insert_is_snippet: bool,
    /// Raw markdown documentation, exactly as rust-analyzer sent it (fences
    /// included). Parsed for display by `editor_panel::doc_md`.
    pub documentation: String,
}

/// One text replacement from a `textDocument/rename` WorkspaceEdit. Positions
/// are 0-based LSP (line, character) ranges. Applied across files to perform a
/// project-wide rename.
#[derive(Clone, Debug)]
pub struct RenameEdit {
    /// Path relative to the workspace root, e.g. `"src/main.rs"`.
    pub rel_path: String,
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
    pub new_text: String,
}

/// One `textDocument/codeAction` result (an RA assist / quick-fix). The `edits`
/// are `None` when RA returned the action lazily — the caller then sends
/// `codeAction/resolve` with `raw` (RA requires the whole action object back,
/// not just its `data`) to obtain them.
#[derive(Clone, Debug)]
pub struct CodeAction {
    pub title: String,
    /// Parsed `WorkspaceEdit`, or `None` until resolved.
    pub edits: Option<Vec<RenameEdit>>,
    /// The original JSON action, sent verbatim to `codeAction/resolve`.
    pub raw: serde_json::Value,
}

impl CodeAction {
    /// True when this action can produce edits — either inline or via resolve
    /// (has a `data` field). Command-only actions (no edit, no data) are
    /// skipped: we don't run `workspace/executeCommand` in v1.
    pub fn is_applicable(&self) -> bool {
        self.edits.is_some() || !self.raw["data"].is_null()
    }
}

/// One `textDocument/inlayHint` — an inferred-type annotation rust-analyzer
/// would draw after an untyped `let` binding. We request them one line at a
/// time (the cursor's line) and keep only **type** hints; the `label` is what
/// we draw as ghost text (e.g. `": u32"`) and `text_edits` are the edits that
/// splice the type into the source when the user presses Tab. RA fills
/// `text_edits` eagerly because we do NOT advertise inlay-hint resolve support,
/// so accepting a hint needs no extra round-trip.
#[derive(Clone, Debug)]
pub struct InlayHint {
    /// 0-based position where the label sits (just after the binding name).
    pub line: u32,
    pub character: u32,
    /// The hint text, e.g. `": u32"`.
    pub label: String,
    /// Edits that materialize the type into the source (may be empty).
    pub text_edits: Vec<RenameEdit>,
}

/// A `textDocument/definition` target: the file + 0-based position RA points to.
#[derive(Clone, Debug)]
pub struct DefinitionLoc {
    /// Absolute filesystem path (decoded from the `file://` URI).
    pub path: String,
    /// The URI exactly as rust-analyzer sent it. A go-to asked FROM this file
    /// (the Definition tab's chained F12) sends it back unchanged: rebuilding
    /// it from `path` would canonicalize it, which resolves junctions and can
    /// name a file the analyzer's VFS does not know by that spelling.
    pub uri: String,
    pub line: u32,
    pub character: u32,
}

/// One item from `textDocument/documentSymbol` (fn/struct/enum/const/static/
/// trait/method/field/…) in a file — used to fade never-referenced items and
/// offer a "references" list on the rest. Positions are 0-based LSP (line,
/// UTF-16 character), like the rest of this module.
#[derive(Clone, Debug)]
pub struct SymbolInfo {
    pub name: String,
    /// LSP `SymbolKind` (6=Method, 8=Field, 9=Constructor, 10=Enum, 11=Interface
    /// [trait], 12=Function, 13=Variable [static], 14=Constant, 22=EnumMember,
    /// 23=Struct); see [`is_trackable_symbol_kind`].
    pub kind: u8,
    /// The whole item's span — used to fade it when unused.
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
    /// The name's own position — used as the query point for `references`.
    pub sel_line: u32,
    pub sel_char: u32,
    /// True when the symbol sits inside an `impl Trait for Type` block.
    /// `references` on such members misses calls dispatched through a generic
    /// trait bound (those bind to the TRAIT's declaration), so an empty result
    /// here doesn't mean "unused" — these items must never be faded.
    pub in_trait_impl: bool,
}

/// `true` for the `SymbolKind`s worth tracking (fn/method/struct/enum/const/
/// static/trait/field/…) — containers like Module/Namespace/File are excluded
/// (we still recurse INTO them, just don't fade/count them as items themselves).
pub fn is_trackable_symbol_kind(kind: u8) -> bool {
    matches!(kind, 6 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 22 | 23)
}

/// One usage site from `textDocument/references`.
#[derive(Clone, Debug)]
pub struct ReferenceLoc {
    /// Absolute filesystem path (decoded from the `file://` URI), like `DefinitionLoc`.
    pub path: String,
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LspStatus {
    #[default]
    Stopped,
    Starting,
    /// initialize response received; waiting for first diagnostic push
    Indexing,
    Ready,
    /// Fatal — rust-analyzer not found, or exited unexpectedly
    Failed(String),
}

impl LspStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Indexing | Self::Ready)
    }
    pub fn label(&self) -> &str {
        match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting…",
            Self::Indexing => "Indexing…",
            Self::Ready => "Ready",
            Self::Failed(_) => "Failed",
        }
    }
}

// ── Per-file open state ───────────────────────────────────────────────────────

/// Tracks the LSP state for one open file.
struct OpenFileState {
    /// LSP document version (incremented on every `textDocument/didChange`).
    doc_version: u64,
    /// The text last sent to RA so no-op frames are skipped.
    last_sent_code: String,
}

// ── LspState ──────────────────────────────────────────────────────────────────

pub struct LspState {
    pub status: LspStatus,
    /// Diagnostics keyed by relative path, e.g. `"src/main.rs"`.
    pub diagnostics: HashMap<String, Vec<LspDiagnostic>>,
    /// Incremented on every `start()` so stale threads know to bail out.
    pub generation: u64,
    /// The `linkedProjects` this session was STARTED with - empty when it was
    /// told none. Fixed for the life of the process, so the Structure tab reads
    /// it to say whether a detached library's calls are traced now, rather than
    /// whether they would be after a restart.
    pub linked_projects: Vec<String>,
    /// Channel to the write thread; `None` while stopped.
    sender: Option<mpsc::Sender<String>>,
    /// Per-file open state.  Key = relative path, e.g. `"src/main.rs"`.
    open_files: HashMap<String, OpenFileState>,
    /// Files for which a `didChange` was sent but no `publishDiagnostics` has
    /// arrived since — their current diagnostics are stale (computed for the
    /// previous text). The inline overlay hides them until RA re-publishes, so a
    /// fixed/deleted error never lingers at a stale position. Key = rel path.
    awaiting_diagnostics: std::collections::HashSet<String>,
    /// Edit generation: bumped on every real `didChange`. Compared against
    /// `fresh_check_gen` to know whether flycheck (cargo check) diagnostics are
    /// stale (an edit happened since the last completed check).
    edit_gen: u64,
    /// `edit_gen` captured when the current cargo-check pass began.
    check_begin_gen: u64,
    /// The `edit_gen` the most recently COMPLETED cargo-check reflects.
    fresh_check_gen: u64,
    /// Workspace root URI (e.g. `file:///tmp/embedded_ide_0_check`).
    pub root_uri: String,
    /// True while RA is running a background `cargo check` pass.
    /// Set to `true` on `$/progress begin` for check tokens; cleared on `end`.
    pub checking: bool,
    /// True once RA reported its INDEXING pass finished (`$/progress end` for a
    /// token naming "index"). That is the point where the crate graph and the
    /// sysroot are loaded — before it, a `didOpen`ed document gets analysed as a
    /// detached file (no `core`, so no unsized coercions and no const-eval),
    /// which produces false type errors that then stick. The app waits for this
    /// before handing RA the first document. `status == Ready` is NOT a
    /// substitute: it flips on the first `$/progress end` of ANY rust-prefixed
    /// token, which can be an early phase such as "Fetching metadata".
    pub indexed: bool,
    /// Whether rust-analyzer has sent any `experimental/serverStatus` this
    /// session. Once it has, `quiescent` is the word on whether the workspace
    /// is loaded; before, and for a server that never sends one, see
    /// [`LspState::workspace_loaded`].
    server_status_seen: bool,
    /// `quiescent` from the last `experimental/serverStatus`: the workspace
    /// fetch, build scripts, proc-macros, the file scan AND cache priming are
    /// all done. It drops back to `false` whenever a refetch starts (a manifest
    /// change), and rises again when that settles.
    quiescent: bool,
    /// When the `initialize` handshake completed — the clock for a server that
    /// never reports its status.
    initialized_at: Option<std::time::Instant>,
    /// A `didSave` asked for while the workspace was still loading, held until
    /// it is loaded (see [`LspState::did_save`]). One slot: the flycheck it
    /// starts covers the whole workspace, so a second held save adds nothing.
    held_save: Option<String>,
    /// Set when this session's rust-analyzer exited on its own before its
    /// workspace had finished loading — the case the app restarts once by
    /// itself, instead of leaving a "failed to start" for the user to click.
    pub exited_during_load: bool,
    /// When the last `didSave` went out — starts the flycheck queue-latency clock.
    last_did_save_at: Option<std::time::Instant>,
    /// When RA reported the current cargo-check began (`$/progress` "begin").
    check_started_at: Option<std::time::Instant>,
    /// Queue latency of the current check (didSave → begin), captured at begin.
    check_queued: std::time::Duration,
    /// Completed flycheck spans `(queued, ran)`, drained by the app into the
    /// Activity log — the post-save "Checking…" wall time that no in-app
    /// recorder can wrap (it runs inside rust-analyzer).
    pub finished_checks: Vec<(std::time::Duration, std::time::Duration)>,
    /// Most recent completion items from rust-analyzer.
    ///
    /// Never edited in place: every change assigns a new `Arc`. The popup
    /// takes a cheap clone of it each frame, and a reader still holding one
    /// can tell the list is unchanged by `Arc::ptr_eq` alone.
    pub completion_items: Arc<Vec<CompletionItem>>,
    /// Set to `true` when a completion response (success OR error) arrives.
    pub completion_response_received: bool,
    /// The request id of the pending completion request, if any.
    completion_req_id: Option<u64>,
    /// Counter for outgoing requests (starts at 1; incremented before each send → first = 2).
    next_req_id: u64,
    /// When the last completion request was sent (for spinner timeout).
    pub completion_request_sent_at: Option<std::time::Instant>,
    /// Why the last answered completion request produced no items, if it
    /// failed rather than returning an empty list. Cleared by each new request.
    pub completion_failure: Option<CompletionFailure>,
    /// `(rel_path, line, character, trigger)` of the last completion request,
    /// so a cancelled one can be re-sent without the UI asking again.
    completion_params: Option<(String, u32, u32, Option<char>)>,
    /// Re-sends already spent on the current completion request.
    completion_retries: u8,
    /// The request id of the pending `textDocument/rename`, if any.
    rename_req_id: Option<u64>,
    /// In-flight `workspace/willRenameFiles` (a FILE rename, not a symbol one).
    /// Kept apart from `rename_req_id` so a module rename and a Ctrl+R symbol
    /// rename can never drain each other's reply.
    will_rename_req_id: Option<u64>,
    /// Set when that reply arrives (including a refusal, which is an empty
    /// edit list) so the poller stops waiting.
    pub will_rename_response_received: bool,
    /// Text edits rust-analyzer wants applied for the file rename.
    pub will_rename_edits: Vec<RenameEdit>,
    /// Set when a rename response (success OR error) arrives; the app then
    /// applies `rename_edits` and clears this.
    pub rename_response_received: bool,
    /// The edits returned by the last rename (empty on error / no-op).
    pub rename_edits: Vec<RenameEdit>,
    /// The `textDocument/codeAction` requests of one Ctrl+Enter, in the order
    /// they were asked, each with its answer once it lands. Several, because
    /// rust-analyzer's assists depend on WHERE they are asked (see
    /// `request_code_actions`); the list is published when all have answered.
    code_action_pending: Vec<(u64, Option<Vec<CodeAction>>)>,
    /// Set when a codeAction list response arrives; consumed by the app.
    pub code_action_response_received: bool,
    /// The code actions returned by the last request.
    pub code_actions: Vec<CodeAction>,
    /// The request id of the pending `codeAction/resolve`, if any.
    code_action_resolve_req_id: Option<u64>,
    /// Set when a resolve response arrives; consumed by the app.
    pub code_action_resolve_received: bool,
    /// The resolved edits (`None` when resolve produced no edit).
    pub code_action_resolved: Option<Vec<RenameEdit>>,
    /// The request id of the pending `textDocument/definition`, if any.
    definition_req_id: Option<u64>,
    /// `(file, 1-based line)` the last definition request asked about, so an
    /// empty answer can be explained rather than just reported. Mirrors
    /// `inlay_for_file` / `inlay_for_line`.
    pub definition_for: Option<(String, u32)>,
    /// Same, for the last code-action request (Ctrl+Enter).
    pub code_action_for: Option<(String, u32)>,
    /// The request id of the pending `textDocument/implementation` (Ctrl+F12),
    /// if any. Its response funnels into the SAME `definition_results` slot, so
    /// the whole F12 navigation pipeline downstream serves both.
    implementation_req_id: Option<u64>,
    /// Set when a definition response arrives; consumed by the app.
    pub definition_response_received: bool,
    /// Whether that pending/last answer came from `textDocument/implementation`
    /// (Ctrl+F12) rather than `textDocument/definition` (F12). The two share
    /// this slot, and only the request knows which question was asked — the
    /// answers are the same shape.
    pub definition_is_impl: bool,
    /// EVERY target the last F12 / Ctrl+F12 resolved to; empty when there was
    /// none.
    ///
    /// A `Vec`, not an `Option`, because `textDocument/implementation` is
    /// genuinely multi-valued: a trait implemented by three types answers with
    /// three locations. This slot held ONE `DefinitionLoc` from the day Ctrl+F12
    /// was added, inherited from `textDocument/definition` — which really is
    /// single-valued for Rust, so the truncation was invisible there. On the
    /// implementation path it meant the editor could only ever navigate to
    /// whichever impl rust-analyzer happened to list first, no matter which type
    /// the caret was on.
    pub definition_results: Vec<DefinitionLoc>,
    /// rust-analyzer's error text when the last go-to request FAILED rather than
    /// found nothing. The two used to arrive as the same empty answer, and a
    /// chained F12 from a file the analyzer no longer has loaded ("file not
    /// found") read as "no definition here".
    pub definition_error: Option<String>,
    /// The request id of the pending `textDocument/documentSymbol`, if any.
    symbols_req_id: Option<u64>,
    /// The rel_path the pending/last `symbols_result` was requested for.
    symbols_for_file: String,
    /// Set when a documentSymbol response arrives; consumed by the app.
    pub symbols_response_received: bool,
    pub symbols_result: Vec<SymbolInfo>,
    /// The request id of the pending `textDocument/inlayHint`, if any.
    inlay_req_id: Option<u64>,
    /// The rel_path + 0-based line the pending/last inlay request was for — so
    /// the app can discard a result that arrived after the cursor moved.
    inlay_for_file: String,
    inlay_for_line: u32,
    /// Set when an inlayHint response arrives; consumed by the app.
    pub inlay_response_received: bool,
    /// The (type-only) inlay hints from the last request (cursor-line scope).
    pub inlay_result: Vec<InlayHint>,
    /// In-flight `textDocument/references` requests: request id → the caller's
    /// own index for that symbol (its position in the app's item list) — lets
    /// many reference lookups run concurrently for one file (one per symbol),
    /// unlike the single-slot `_req_id` fields above.
    references_pending: HashMap<u64, usize>,
    /// Completed reference results, keyed by that same index; drained by the app.
    pub references_results: HashMap<usize, Vec<ReferenceLoc>>,
    /// Same request/result shape, but a SEPARATE channel for the Structure
    /// tab's call-graph pass. It cannot share `references_pending`: the usages
    /// poll drains `take_reference_results` indiscriminately, so a shared map
    /// would let one consumer steal the other's replies.
    calls_refs_pending: HashMap<u64, usize>,
    calls_refs_results: HashMap<usize, Vec<ReferenceLoc>>,
    /// The running rust-analyzer process. Held so it can be KILLED on restart /
    /// app exit — dropping a `std::process::Child` only detaches it (it does NOT
    /// terminate the process), which used to leave orphaned rust-analyzer
    /// instances accumulating across restarts, each still watching and
    /// re-analyzing the workspace on every file write.
    child: Option<std::process::Child>,
    /// A bounded, human-readable trace of RA's startup: `$/progress` titles
    /// ("Fetching metadata", "Building CrateGraph", …) and every `window/`
    /// showMessage / logMessage (warning+). Shown in the Analyzer tab so a failed
    /// workspace load is DIAGNOSABLE instead of a silent stuck "Checking…".
    /// Cleared on `reset()`; capped at `LOAD_LOG_CAP`. Every line also goes to
    /// [`ra_trace_path`] on disk, which a restart does not wipe.
    pub load_log: Vec<String>,
    /// The last lines rust-analyzer wrote to stderr — its panics and error
    /// logs. The reason an "exited unexpectedly" had none: stderr used to go
    /// to the null device. Capped at `STDERR_TAIL`; cleared per session.
    stderr_tail: std::collections::VecDeque<String>,
    /// stderr lines already copied into `load_log` this session, so a chatty
    /// server cannot push the load phases out of it.
    stderr_logged: usize,
    /// The panic that explains this session's exit, on one line, and whether
    /// it was FATAL (see [`RaPanic::fatal`]): the first one, unless a fatal one
    /// came after a worker's. Kept apart from `stderr_tail` because the
    /// backtrace after it is longer than the tail.
    first_panic: Option<(String, bool)>,
}

impl Default for LspState {
    fn default() -> Self {
        Self {
            status: LspStatus::Stopped,
            diagnostics: HashMap::new(),
            generation: 0,
            linked_projects: Vec::new(),
            sender: None,
            open_files: HashMap::new(),
            awaiting_diagnostics: std::collections::HashSet::new(),
            edit_gen: 0,
            check_begin_gen: 0,
            fresh_check_gen: 0,
            root_uri: String::new(),
            checking: false,
            indexed: false,
            server_status_seen: false,
            quiescent: false,
            initialized_at: None,
            held_save: None,
            exited_during_load: false,
            last_did_save_at: None,
            check_started_at: None,
            check_queued: std::time::Duration::ZERO,
            finished_checks: Vec::new(),
            completion_items: Arc::default(),
            completion_response_received: false,
            completion_req_id: None,
            next_req_id: 1,
            completion_request_sent_at: None,
            completion_failure: None,
            completion_params: None,
            completion_retries: 0,
            rename_req_id: None,
            will_rename_req_id: None,
            will_rename_response_received: false,
            will_rename_edits: Vec::new(),
            rename_response_received: false,
            rename_edits: Vec::new(),
            code_action_pending: Vec::new(),
            code_action_response_received: false,
            code_actions: Vec::new(),
            code_action_resolve_req_id: None,
            code_action_resolve_received: false,
            code_action_resolved: None,
            definition_req_id: None,
            definition_for: None,
            code_action_for: None,
            implementation_req_id: None,
            definition_response_received: false,
            definition_is_impl: false,
            definition_results: Vec::new(),
            definition_error: None,
            symbols_req_id: None,
            symbols_for_file: String::new(),
            symbols_response_received: false,
            symbols_result: Vec::new(),
            inlay_req_id: None,
            inlay_for_file: String::new(),
            inlay_for_line: 0,
            inlay_response_received: false,
            inlay_result: Vec::new(),
            references_pending: HashMap::new(),
            references_results: HashMap::new(),
            calls_refs_pending: HashMap::new(),
            calls_refs_results: HashMap::new(),
            child: None,
            load_log: Vec::new(),
            stderr_tail: std::collections::VecDeque::new(),
            stderr_logged: 0,
            first_panic: None,
        }
    }
}

/// Cap on `LspState::load_log` — a startup trace, not a full server log.
const LOAD_LOG_CAP: usize = 250;

/// How long after the `initialize` handshake a server that has sent no
/// `experimental/serverStatus` is taken to be loaded (see
/// [`LspState::workspace_loaded`]). rust-analyzer sends its first status on
/// its first loop turn - well under a second after the handshake, measured -
/// so this only ever decides for a server without the extension.
const SERVER_STATUS_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Last rust-analyzer stderr lines kept for an exit message.
const STDERR_TAIL: usize = 40;

/// stderr lines copied into the Analyzer tab's log per session; the rest still
/// reach [`ra_trace_path`] and `stderr_tail`.
const STDERR_TO_LOAD_LOG: usize = 60;

/// Where the rust-analyzer trace is kept across restarts and app launches: the
/// Analyzer tab's log lines, with a clock time, plus RA's own stderr.
///
/// The in-memory log is wiped by every restart — which is exactly the moment a
/// user reaches for after a failed load, so the reason was gone by the time
/// anyone looked. Per instance, like the LSP debug log.
pub fn ra_trace_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "rust_on_chip_ra_trace{}.log",
        crate::workspace::suffix()
    ))
}

/// Bytes after which the trace rotates to `<name>.1`.
const RA_TRACE_CAP: u64 = 1024 * 1024;

/// Append one line to [`ra_trace_path`], stamped with the Activity tab's clock.
/// Best effort: a trace that cannot be written must never disturb the analyzer.
fn append_ra_trace(line: &str) {
    if cfg!(test) {
        return;
    }
    let path = ra_trace_path();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= RA_TRACE_CAP) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "{}  {line}",
            crate::activity::fmt_clock(std::time::SystemTime::now())
        );
    }
}

/// The status message for a rust-analyzer that exited on its own: the exit
/// code when there is one, then the session's fatal panic when there was one -
/// otherwise its last stderr lines, after any earlier panic it recovered from.
///
/// The fatal panic replaces the tail because the tail alone said nothing: a
/// backtrace is longer than `STDERR_TAIL`, so an exit 101 used to be reported
/// as its last three frames ("28: 0x7ffc… - BaseThreadInitThunk"). A panic the
/// server survived is no cause, so it goes before the tail, not instead of it.
fn exit_message(code: Option<i32>, panic: Option<(&str, bool)>, stderr_tail: &[String]) -> String {
    let mut msg = match code {
        Some(c) => format!("rust-analyzer exited unexpectedly (exit code {c})."),
        None => "rust-analyzer exited unexpectedly.".to_owned(),
    };
    match panic {
        Some((text, true)) => {
            msg.push_str("\nPanic: ");
            msg.push_str(text);
            return msg;
        }
        Some((text, false)) => {
            msg.push_str("\nEarlier panic: ");
            msg.push_str(text);
        }
        None => {}
    }
    let last: Vec<&str> = stderr_tail
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if !last.is_empty() {
        let from = last.len().saturating_sub(3);
        msg.push_str("\nLast output: ");
        msg.push_str(&last[from..].join("\n"));
    }
    msg
}

/// One panic rust-analyzer reported on stderr.
#[derive(Debug, PartialEq)]
struct RaPanic {
    /// The thread's name: `LspServer` is the main loop, whose panic ends the
    /// server; a worker's is caught and the server carries on.
    thread: String,
    /// `thread '<name>' panicked at <location>: <message>`, on one line.
    text: String,
}

impl RaPanic {
    /// A panic on a thread whose panic ends the process: the main loop
    /// (`LspServer`) or `main`. A worker's runs a request under `catch_unwind`,
    /// and the proc-macro server's is its own; the server survives both.
    fn fatal(&self) -> bool {
        matches!(self.thread.as_str(), "LspServer" | "main")
    }
}

/// Picks rust-analyzer's panics out of its stderr, one line at a time.
///
/// A panic is two lines - `thread 'LspServer' (22288) panicked at <path>:180:21:`
/// and then its message - so the location line is held until the message
/// arrives. The old one-line form (`panicked at 'msg', <path>`) is complete as
/// it stands.
#[derive(Default)]
struct PanicWatch {
    /// `(thread, location)` of a panic whose message line has not come yet.
    pending: Option<(String, String)>,
}

impl PanicWatch {
    /// Feed one stderr line; returns the panics it completes, in order.
    fn feed(&mut self, line: &str) -> Vec<RaPanic> {
        let line = line.trim();
        let mut done = Vec::new();
        if let Some((thread, after)) = Self::panic_head(line) {
            // A new panic before the last one's message: report that one bare
            // rather than lose it.
            if let Some((t, loc)) = self.pending.take() {
                done.push(Self::finish(t, &loc, ""));
            }
            match after.strip_suffix(':') {
                Some(location) => self.pending = Some((thread, location.to_owned())),
                None => done.push(Self::finish(thread, after, "")),
            }
        } else if !line.is_empty()
            && let Some((thread, location)) = self.pending.take()
        {
            done.push(Self::finish(thread, &location, line));
        }
        done
    }

    /// `(thread name, text after "panicked at ")` when `line` opens a panic.
    fn panic_head(line: &str) -> Option<(String, &str)> {
        let rest = line.strip_prefix("thread '")?;
        let (thread, rest) = rest.split_once('\'')?;
        let (_, after) = rest.split_once("panicked at ")?;
        Some((thread.to_owned(), after.trim()))
    }

    fn finish(thread: String, location: &str, message: &str) -> RaPanic {
        let mut text = format!("thread '{thread}' panicked at {location}");
        if !message.is_empty() {
            text.push_str(": ");
            text.push_str(message);
        }
        RaPanic { thread, text }
    }
}

impl LspState {
    // ── Sending helpers ───────────────────────────────────────────────────────

    /// Append a line to the RA startup trace (Analyzer tab), oldest dropped past
    /// the cap. Timestamps aren't added — order is the useful signal.
    pub fn push_load_log(&mut self, line: impl Into<String>) {
        let line = line.into();
        append_ra_trace(&line);
        self.load_log.push(line);
        if self.load_log.len() > LOAD_LOG_CAP {
            let overflow = self.load_log.len() - LOAD_LOG_CAP;
            self.load_log.drain(0..overflow);
        }
    }

    fn send_raw(&self, json: String) {
        if let Some(tx) = &self.sender {
            let _ = tx.send(json);
        }
    }

    /// Returns `true` if `textDocument/didOpen` has been sent for `rel_path`.
    pub fn is_file_open(&self, rel_path: &str) -> bool {
        self.open_files.contains_key(rel_path)
    }

    /// `true` when `rel_path` is open and the text rust-analyzer last received
    /// for it equals `text` — i.e. no edits are pending sync for this file.
    /// Used to gate the inline diagnostic overlay so it never draws diagnostics
    /// computed for a different (older) version of the file at stale positions.
    pub fn last_sent_matches(&self, rel_path: &str, text: &str) -> bool {
        self.open_files
            .get(rel_path)
            .map(|f| f.last_sent_code == text)
            .unwrap_or(false)
    }

    /// `true` when RA has published diagnostics for `rel_path` *since* the last
    /// `didChange` — i.e. the current `diagnostics` reflect the latest text, not
    /// a stale version. `false` between sending an edit and RA's response.
    pub fn diagnostics_fresh(&self, rel_path: &str) -> bool {
        !self.awaiting_diagnostics.contains(rel_path)
    }

    /// `true` when flycheck (cargo check) diagnostics may be stale: there has
    /// been a real edit since the last COMPLETED check began, so rustc's
    /// reported line/cols no longer match the current text (a fixed/commented
    /// error lingers on its old line). The inline overlay hides flycheck-sourced
    /// diagnostics while this holds; RA's own (native) diagnostics are re-mapped
    /// on every `didChange`, so they stay visible. Cleared when the next check
    /// completes (`$/progress` "end").
    pub fn flycheck_stale(&self) -> bool {
        self.fresh_check_gen < self.edit_gen
    }

    /// Whether a workspace file may be handed to rust-analyzer as a text
    /// document. Every `textDocument/*` message we send declares
    /// `languageId: "rust"`, so opening anything else makes RA parse it as Rust
    /// and report a "Syntax Error: expected an item" on every line — which is
    /// what a library crate's `Cargo.toml` did once library files joined
    /// `user_src_files`. RA still reads manifests itself, through cargo
    /// metadata; it just must not receive them as source documents.
    ///
    /// Guarded here rather than at the call sites so no future caller can
    /// reintroduce it.
    fn is_rust_document(rel_path: &str) -> bool {
        rel_path.ends_with(".rs")
    }

    /// Send `textDocument/didOpen` for `rel_path` and record the text.
    ///
    /// `rel_path` is relative to the workspace root, e.g. `"src/main.rs"`.
    pub fn did_open(&mut self, rel_path: &str, text: &str) {
        if self.sender.is_none() || !Self::is_rust_document(rel_path) {
            return;
        }
        self.open_files.insert(
            rel_path.to_owned(),
            OpenFileState {
                doc_version: 1,
                last_sent_code: text.to_owned(),
            },
        );
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "method":  "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri":        uri,
                        "languageId": "rust",
                        "version":    1,
                        "text":       text,
                    }
                }
            })
            .to_string(),
        );
    }

    /// Send `textDocument/didChange` for `rel_path` when the text has changed.
    /// Auto-opens the file via `didOpen` if it hasn't been opened yet.
    ///
    /// `force` re-sends (with a bumped version) even when the text is unchanged,
    /// so rust-analyzer re-runs its analysis. `rel_path` is relative to the
    /// workspace root. Returns `true` when a message actually went out
    /// (didOpen or didChange); `false` when the text was already in sync.
    pub fn did_change(&mut self, rel_path: &str, text: &str, force: bool) -> bool {
        // Not just tidiness: the auto-open below returns `true` unconditionally,
        // so without this a non-Rust file would report "synced" on every flush
        // for ever — and each one re-triggers a cargo flycheck.
        if self.sender.is_none() || !Self::is_rust_document(rel_path) {
            return false;
        }
        // Auto-open the file on first access.
        if !self.open_files.contains_key(rel_path) {
            self.did_open(rel_path, text);
            return true;
        }
        let file = self.open_files.get_mut(rel_path).unwrap();
        let changed = text != file.last_sent_code;
        if !changed && !force {
            return false;
        }
        file.last_sent_code = text.to_owned();
        file.doc_version += 1;
        let version = file.doc_version;
        // A real text change makes the current diagnostics stale (their line/col
        // now cling to shifted/removed code) until RA re-publishes — gate them
        // out via `diagnostics_fresh`. A *forced* no-op re-send (Project Save
        // re-verify with identical text) must NOT mark them stale: RA won't
        // re-publish for unchanged text, so the gate would get stuck hiding
        // perfectly valid diagnostics forever.
        if changed {
            self.awaiting_diagnostics.insert(rel_path.to_owned());
            // A real edit invalidates the last cargo-check's diagnostics until a
            // fresh check runs (see `flycheck_stale`).
            self.edit_gen += 1;
        }
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "method":  "textDocument/didChange",
                "params": {
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{ "text": text }],
                }
            })
            .to_string(),
        );
        true
    }

    /// Send `textDocument/didSave` for `rel_path`. With `checkOnSave: true` this
    /// makes rust-analyzer re-run its flycheck (cargo check) so its flycheck
    /// diagnostics refresh — without it they stay frozen at the startup check
    /// and a fixed error lingers forever in the panel.
    ///
    /// HELD, not sent, while the workspace is still loading, and sent by
    /// [`LspState::release_held_save`] once it is. A `didSave` that lands while
    /// rust-analyzer is scanning the workspace files KILLS it: the handler asks
    /// for the file's source root, which that scan has not assigned yet, and
    /// the panic ("Unable to get `FileSourceRootInput` … this is a bug") takes
    /// the whole server down with exit code 101 - rust-lang/rust-analyzer#19406.
    /// That is what 4 of 10 startups did here, in both windows, because the
    /// startup flush fired on `Ready`, which flips at the end of "Fetching".
    pub fn did_save(&mut self, rel_path: &str) {
        if self.sender.is_none() {
            return;
        }
        if !self.open_files.contains_key(rel_path) {
            return;
        }
        // Start the flycheck queue-latency clock (stopped at $/progress begin).
        // Also while held: from the user's side the check is already waiting.
        self.last_did_save_at = Some(std::time::Instant::now());
        if !self.workspace_loaded() {
            self.held_save = Some(rel_path.to_owned());
            return;
        }
        // One going out now covers one still held: without this, a server
        // that never reports its status could get both, a frame apart.
        self.held_save = None;
        self.send_did_save(rel_path);
    }

    /// Whether rust-analyzer has finished loading the workspace, so a `didSave`
    /// is safe (see [`LspState::did_save`]).
    ///
    /// The server's own `experimental/serverStatus` `quiescent` is the answer
    /// once it has sent one - which rust-analyzer does on its first loop turn.
    /// A server that sends none gets [`SERVER_STATUS_GRACE`] after the
    /// handshake and is then trusted as before, so an analyzer without the
    /// extension still gets its saves.
    pub fn workspace_loaded(&self) -> bool {
        if self.server_status_seen {
            self.quiescent
        } else {
            self.initialized_at
                .is_some_and(|t| t.elapsed() >= SERVER_STATUS_GRACE)
        }
    }

    /// Send the `didSave` [`LspState::did_save`] held back while loading, now
    /// that the workspace is loaded. Returns whether one went out. Called when
    /// a `serverStatus` arrives and on every app frame, which covers a server
    /// that never sends one.
    pub fn release_held_save(&mut self) -> bool {
        if !self.workspace_loaded() {
            return false;
        }
        let Some(rel_path) = self.held_save.take() else {
            return false;
        };
        // Closed since: the flycheck rust-analyzer starts by itself once the
        // workspace settles covers what this one would have. Its queue clock
        // goes too, or the status would wait on a check nobody asked for.
        if self.sender.is_none() || !self.open_files.contains_key(&rel_path) {
            self.last_did_save_at = None;
            return false;
        }
        self.send_did_save(&rel_path);
        true
    }

    fn send_did_save(&mut self, rel_path: &str) {
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "method":  "textDocument/didSave",
                "params":  { "textDocument": { "uri": uri } }
            })
            .to_string(),
        );
    }

    /// Seconds the current cargo-check pass has been running, while `checking`.
    /// Drives the live "Checking… Ns" status label.
    pub fn checking_elapsed_secs(&self) -> Option<u64> {
        self.check_started_at.map(|t| t.elapsed().as_secs())
    }

    /// True between a `didSave` and rust-analyzer's `$/progress begin` for the
    /// flycheck it triggers — the QUEUE phase, before `checking` turns on.
    /// The status bar treats this as "Checking…" too: it used to be a gap with
    /// no spinner (and thus no scheduled repaint), where the app could sleep.
    pub fn flycheck_pending(&self) -> bool {
        self.last_did_save_at.is_some()
    }

    /// Request completions at the given cursor position in `rel_path`.
    ///
    /// `trigger_char = None`  → manual invocation (Ctrl+Space, triggerKind=1)
    /// `trigger_char = Some(c)` → auto-trigger (typed `.` or `:`, triggerKind=2)
    /// `rel_path` is relative to the workspace root, e.g. `"src/main.rs"`.
    pub fn request_completion(
        &mut self,
        rel_path: &str,
        line: u32,
        character: u32,
        trigger_char: Option<char>,
    ) {
        if self.sender.is_none() {
            return;
        }
        self.completion_params = Some((rel_path.to_owned(), line, character, trigger_char));
        self.completion_retries = 0;
        self.completion_failure = None;
        self.completion_items = Arc::default();
        self.completion_response_received = false;
        self.completion_request_sent_at = Some(std::time::Instant::now());
        self.send_completion();
    }

    /// Re-send the last completion request after a transient error reply.
    /// Returns `false` once the retries are spent (or there is nothing to
    /// re-send), in which case the caller reports the error.
    fn retry_completion(&mut self) -> bool {
        if self.sender.is_none()
            || self.completion_params.is_none()
            || self.completion_retries >= COMPLETION_RETRIES
        {
            return false;
        }
        self.completion_retries += 1;
        // The spinner's timeout restarts: a retry is a fresh wait, not the tail
        // of the refused one.
        self.completion_request_sent_at = Some(std::time::Instant::now());
        self.send_completion();
        true
    }

    fn send_completion(&mut self) {
        let Some((rel_path, line, character, trigger_char)) = self.completion_params.clone() else {
            return;
        };
        self.next_req_id += 1;
        let id = self.next_req_id;
        self.completion_req_id = Some(id);
        lsp_log(&format!(
            "COMPLETION_REQ id={id} file={rel_path} line={line} char={character} \
             trigger={trigger_char:?} retry={}",
            self.completion_retries
        ));
        let uri = format!("{}/{}", self.root_uri, rel_path);
        let trigger_kind: u32 = if trigger_char.is_some() { 2 } else { 1 };
        let mut params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "triggerKind": trigger_kind }
        });
        if let Some(c) = trigger_char {
            params["context"]["triggerCharacter"] = serde_json::json!(c.to_string());
        }
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id":      id,
                "method":  "textDocument/completion",
                "params":  params
            })
            .to_string(),
        );
    }

    /// Request a project-wide rename of the symbol at `(line, character)` in
    /// `rel_path` to `new_name` (`textDocument/rename`). The result arrives
    /// asynchronously; poll [`take_rename_result`].
    pub fn request_rename(&mut self, rel_path: &str, line: u32, character: u32, new_name: &str) {
        if self.sender.is_none() {
            return;
        }
        self.next_req_id += 1;
        let id = self.next_req_id;
        self.rename_req_id = Some(id);
        self.rename_response_received = false;
        self.rename_edits.clear();
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id":      id,
                "method":  "textDocument/rename",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                    "newName":  new_name,
                }
            })
            .to_string(),
        );
    }

    /// Ask what text must change if `old_rel` is renamed to `new_rel`
    /// (`workspace/willRenameFiles`). Poll [`take_will_rename_result`].
    ///
    /// Sent BEFORE the file moves: rust-analyzer resolves the old path against
    /// its VFS and calls `is_dir()` on it, so both must still exist. It replies
    /// with TEXT EDITS ONLY - it deliberately drops the file-system half of the
    /// change (`file_system_edits.clear()`) because the client is the one doing
    /// the move. That is exactly this IDE's shape, and it is why this is used
    /// instead of `textDocument/rename` on the `mod` declaration.
    ///
    /// Known server-side limits, all silent (an empty reply, never an error):
    /// the two paths must share a parent directory, `mod.rs` is refused in
    /// either direction, and the file must already be reachable in the module
    /// tree.
    pub fn request_will_rename(&mut self, old_rel: &str, new_rel: &str) {
        if self.sender.is_none() {
            return;
        }
        self.next_req_id += 1;
        let id = self.next_req_id;
        self.will_rename_req_id = Some(id);
        self.will_rename_response_received = false;
        self.will_rename_edits.clear();
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id":      id,
                "method":  "workspace/willRenameFiles",
                "params": {
                    "files": [{
                        "oldUri": format!("{}/{}", self.root_uri, old_rel),
                        "newUri": format!("{}/{}", self.root_uri, new_rel),
                    }]
                }
            })
            .to_string(),
        );
    }

    /// Take the file-rename edits once RA has responded. `None` while nothing
    /// is ready; `Some(vec![])` when RA had nothing to change (or refused), so
    /// the caller stops waiting and moves the file anyway.
    pub fn take_will_rename_result(&mut self) -> Option<Vec<RenameEdit>> {
        if self.will_rename_response_received {
            self.will_rename_response_received = false;
            Some(std::mem::take(&mut self.will_rename_edits))
        } else {
            None
        }
    }

    /// Take the rename edits once RA has responded, clearing the pending state.
    /// Returns `None` while no response is ready (and `Some(vec![])` for a
    /// no-op / failed rename, so the caller can stop waiting).
    pub fn take_rename_result(&mut self) -> Option<Vec<RenameEdit>> {
        if self.rename_response_received {
            self.rename_response_received = false;
            Some(std::mem::take(&mut self.rename_edits))
        } else {
            None
        }
    }

    /// Request the assists / quick-fixes available in `rel_path`
    /// (`textDocument/codeAction`, Ctrl+Enter), one request per range, and
    /// publish them as ONE list once every request has answered — in the order
    /// the ranges were given, duplicates (same title) kept once. Poll
    /// [`take_code_actions_result`].
    ///
    /// Each range is `(line, character, end_line, end_character)`: the
    /// SELECTION, or the same position twice when there is none. The first one
    /// is what `code_action_for` reports.
    ///
    /// The range is not decoration: rust-analyzer offers a different set of
    /// assists for a span than for a point, and the useful ones — "Extract into
    /// function", "Extract into variable", "Convert to guarded return" — are
    /// span-only. Sending a zero-width range made every one of them
    /// unreachable, which is why Ctrl+Enter never offered an extraction.
    ///
    /// Several points, because assists are just as position-sensitive: "Add
    /// explicit type" lives on a `let` pattern, a closure's own assists inside
    /// the closure. Asking at only one of them silently lost the other set.
    pub fn request_code_actions(&mut self, rel_path: &str, ranges: &[(u32, u32, u32, u32)]) {
        if self.sender.is_none() || ranges.is_empty() {
            return;
        }
        self.code_action_for = Some((rel_path.to_owned(), ranges[0].0 + 1));
        self.code_action_pending.clear();
        self.code_action_response_received = false;
        self.code_actions.clear();
        let uri = format!("{}/{}", self.root_uri, rel_path);
        for &(line, character, end_line, end_character) in ranges {
            self.next_req_id += 1;
            let id = self.next_req_id;
            self.code_action_pending.push((id, None));
            let pos = serde_json::json!({ "line": line, "character": character });
            let end = serde_json::json!({ "line": end_line, "character": end_character });
            self.send_raw(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id":      id,
                    "method":  "textDocument/codeAction",
                    "params": {
                        "textDocument": { "uri": uri },
                        "range":        { "start": pos, "end": end },
                        // No diagnostics in context (v1 = assists at the cursor,
                        // not diagnostic quick-fixes); `only` unset → RA
                        // returns all.
                        "context":      { "diagnostics": [] },
                    }
                })
                .to_string(),
            );
        }
    }

    /// Is `req_id` one of the code-action requests still waiting for an answer?
    fn is_code_action_req(&self, req_id: u64) -> bool {
        self.code_action_pending.iter().any(|(id, _)| *id == req_id)
    }

    /// Record the answer to one of the pending code-action requests (an error
    /// or `null` answers with an empty list). Returns `false` when `req_id` is
    /// not one of them. When the last one lands, the merged list is published.
    fn record_code_actions(&mut self, req_id: u64, actions: Vec<CodeAction>) -> bool {
        let Some(slot) = self
            .code_action_pending
            .iter_mut()
            .find(|(id, _)| *id == req_id)
        else {
            return false;
        };
        slot.1 = Some(actions);
        if self.code_action_pending.iter().all(|(_, a)| a.is_some()) {
            let parts = std::mem::take(&mut self.code_action_pending);
            self.code_actions = merge_code_actions(parts.into_iter().filter_map(|(_, a)| a));
            self.code_action_response_received = true;
        }
        true
    }

    /// Take the code-action list once RA responded (`Some(vec)`; empty = none).
    pub fn take_code_actions_result(&mut self) -> Option<Vec<CodeAction>> {
        if self.code_action_response_received {
            self.code_action_response_received = false;
            Some(std::mem::take(&mut self.code_actions))
        } else {
            None
        }
    }

    /// Resolve a lazily-returned code action (`codeAction/resolve`). RA requires
    /// the WHOLE action object back (with its `data`), so `action_raw` is sent
    /// verbatim. Poll [`take_code_action_resolve_result`].
    /// Returns `false` when nothing went out (no live session) — the caller
    /// must not wait for an answer then.
    #[must_use]
    pub fn request_code_action_resolve(&mut self, action_raw: serde_json::Value) -> bool {
        if self.sender.is_none() {
            return false;
        }
        self.next_req_id += 1;
        let id = self.next_req_id;
        self.code_action_resolve_req_id = Some(id);
        self.code_action_resolve_received = false;
        self.code_action_resolved = None;
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id":      id,
                "method":  "codeAction/resolve",
                "params":  action_raw,
            })
            .to_string(),
        );
        true
    }

    /// Take the resolved edits (`Some(Some(edits))` = resolved, `Some(None)` =
    /// resolve produced no edit, `None` = still waiting).
    pub fn take_code_action_resolve_result(&mut self) -> Option<Option<Vec<RenameEdit>>> {
        if self.code_action_resolve_received {
            self.code_action_resolve_received = false;
            Some(self.code_action_resolved.take())
        } else {
            None
        }
    }

    /// Request the definition of the symbol at `(line, character)` in `rel_path`
    /// (`textDocument/definition`). Result arrives async; poll
    /// [`take_definition_result`].
    /// Returns `false` when nothing went out because there is no live session —
    /// the caller must NOT then wait for an answer. It used to return `()`, and
    /// the F12 path armed `definition_in_flight` regardless: with rust-analyzer
    /// down the app polled for a reply that could never arrive, for the rest of
    /// the session, and the keypress looked like it did nothing.
    #[must_use]
    pub fn request_definition(&mut self, rel_path: &str, line: u32, character: u32) -> bool {
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.request_goto(&uri, Some(rel_path), line, character, false)
    }

    /// Request the implementation(s) of the symbol at `(line, character)` —
    /// `textDocument/implementation` (Ctrl+F12). Where F12 on a trait method
    /// lands on the trait's declaration, this resolves the `impl … for …`
    /// sites instead (the first one, when several exist). Shares the
    /// definition result slot: poll [`take_definition_result`].
    /// As [`request_definition`](Self::request_definition): `false` = not sent.
    #[must_use]
    pub fn request_implementation(&mut self, rel_path: &str, line: u32, character: u32) -> bool {
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.request_goto(&uri, Some(rel_path), line, character, true)
    }

    /// F12 / Ctrl+F12 at a position in a file OUTSIDE the workspace — a crate
    /// in the registry or a sysroot file, as shown by the Definition tab — named
    /// by the URI rust-analyzer itself reported for it.
    ///
    /// Never opened first (no `didOpen` / `didChange`). Measured against the
    /// real server: a library file in the crate graph answers without one, and
    /// `didOpen` of a file OUTSIDE the graph makes rust-analyzer reload the whole
    /// workspace and run a flycheck — seconds of work for a read-only glance.
    #[must_use]
    pub fn request_goto_at_uri(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
        implementation: bool,
    ) -> bool {
        self.request_goto(uri, None, line, character, implementation)
    }

    /// The one sender behind all three go-to requests. `rel_path` is recorded
    /// for the "nothing found" explanation, which only knows workspace files.
    fn request_goto(
        &mut self,
        uri: &str,
        rel_path: Option<&str>,
        line: u32,
        character: u32,
        implementation: bool,
    ) -> bool {
        if self.sender.is_none() {
            return false;
        }
        self.next_req_id += 1;
        let id = self.next_req_id;
        // The two requests share one result slot and the reader dispatches on
        // whichever id matches, so the other kind's straggler must be dropped:
        // it would be consumed as THIS request's answer — a jump to wherever the
        // previous keystroke pointed. And the `definition_req_id` arm is tested
        // FIRST in the reader, so a Ctrl+F12 would otherwise be answered with a
        // stale F12 result.
        if implementation {
            self.implementation_req_id = Some(id);
            self.definition_req_id = None;
        } else {
            self.definition_req_id = Some(id);
            self.implementation_req_id = None;
        }
        // Ctrl+F12 once never recorded what it asked about, so the "nothing
        // found" message explained a line the LAST F12 was on, in a possibly
        // different file. Same question, same bookkeeping — and `None` for a file
        // outside the workspace, which that explanation cannot speak for.
        self.definition_for = rel_path.map(|r| (r.to_owned(), line + 1));
        self.definition_response_received = false;
        self.definition_is_impl = implementation;
        self.definition_results.clear();
        self.definition_error = None;
        let method = if implementation {
            "textDocument/implementation"
        } else {
            "textDocument/definition"
        };
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id":      id,
                "method":  method,
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                }
            })
            .to_string(),
        );
        true
    }

    /// Forget the go-to request in flight, and an answer not yet taken.
    ///
    /// For navigation that moved on while it was pending — Back in the
    /// Definition tab. Its answer would otherwise land AFTER the step and push
    /// the page just left back on top of the history, wiping Forward. Clearing
    /// the id is enough: the reader matches replies by id and drops the rest.
    pub fn cancel_goto(&mut self) {
        self.definition_req_id = None;
        self.implementation_req_id = None;
        self.definition_response_received = false;
        self.definition_results.clear();
        self.definition_error = None;
    }

    /// Take every definition / implementation target once RA responded.
    /// `Some(locs)` = answered (an EMPTY vec means "none found", stop waiting),
    /// `None` = still waiting.
    pub fn take_definition_results(&mut self) -> Option<Vec<DefinitionLoc>> {
        if self.definition_response_received {
            self.definition_response_received = false;
            Some(std::mem::take(&mut self.definition_results))
        } else {
            None
        }
    }

    /// Request every fn/struct/enum/const/… defined in `rel_path`
    /// (`textDocument/documentSymbol`). Result arrives async; poll
    /// [`take_document_symbols_result`].
    pub fn request_document_symbols(&mut self, rel_path: &str) {
        if self.sender.is_none() {
            return;
        }
        self.next_req_id += 1;
        let id = self.next_req_id;
        self.symbols_req_id = Some(id);
        self.symbols_for_file = rel_path.to_owned();
        self.symbols_response_received = false;
        self.symbols_result.clear();
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id":      id,
                "method":  "textDocument/documentSymbol",
                "params": { "textDocument": { "uri": uri } }
            })
            .to_string(),
        );
    }

    /// Take the documentSymbol result once ready: `(rel_path it was requested
    /// for, the flattened item list)` — the caller checks the path in case it
    /// switched files while the request was in flight.
    pub fn take_document_symbols_result(&mut self) -> Option<(String, Vec<SymbolInfo>)> {
        if self.symbols_response_received {
            self.symbols_response_received = false;
            Some((
                self.symbols_for_file.clone(),
                std::mem::take(&mut self.symbols_result),
            ))
        } else {
            None
        }
    }

    /// Request inlay hints for a SINGLE line of `rel_path` — the range is
    /// narrowed to `line` (0-based) so the request is tiny. Used to show the
    /// inferred type of an untyped `let` on the cursor's line. Poll
    /// [`take_inlay_result`].
    pub fn request_inlay_hints(&mut self, rel_path: &str, line: u32) {
        if self.sender.is_none() {
            return;
        }
        self.next_req_id += 1;
        let id = self.next_req_id;
        self.inlay_req_id = Some(id);
        self.inlay_for_file = rel_path.to_owned();
        self.inlay_for_line = line;
        self.inlay_response_received = false;
        self.inlay_result.clear();
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id":      id,
                "method":  "textDocument/inlayHint",
                "params": {
                    "textDocument": { "uri": uri },
                    "range": {
                        "start": { "line": line,     "character": 0 },
                        "end":   { "line": line + 1, "character": 0 },
                    }
                }
            })
            .to_string(),
        );
    }

    /// Take the inlay-hint result once ready: `(rel_path, 0-based line, hints)`.
    /// The caller re-checks the path/line since the cursor may have moved on.
    pub fn take_inlay_result(&mut self) -> Option<(String, u32, Vec<InlayHint>)> {
        if self.inlay_response_received {
            self.inlay_response_received = false;
            Some((
                self.inlay_for_file.clone(),
                self.inlay_for_line,
                std::mem::take(&mut self.inlay_result),
            ))
        } else {
            None
        }
    }

    /// Request every usage site of the symbol at `(line, character)` in
    /// `rel_path` (`textDocument/references`, declaration excluded). `local_idx`
    /// is an opaque caller-assigned key (e.g. the symbol's index in the app's own
    /// list) used to match this specific result when it arrives — lets many
    /// reference lookups for one file's symbols run concurrently. Poll
    /// [`take_reference_results`].
    pub fn request_references(
        &mut self,
        rel_path: &str,
        line: u32,
        character: u32,
        local_idx: usize,
    ) {
        if self.sender.is_none() {
            return;
        }
        self.next_req_id += 1;
        let id = self.next_req_id;
        self.references_pending.insert(id, local_idx);
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id":      id,
                "method":  "textDocument/references",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                    "context": { "includeDeclaration": false }
                }
            })
            .to_string(),
        );
    }

    /// Drain every reference lookup that has completed since the last call,
    /// keyed by the `local_idx` passed to [`request_references`].
    pub fn take_reference_results(&mut self) -> HashMap<usize, Vec<ReferenceLoc>> {
        std::mem::take(&mut self.references_results)
    }

    /// Like [`request_references`], but on the Structure tab's own channel —
    /// its replies land in [`take_calls_reference_results`], out of reach of
    /// the usages poll (which drains the shared map indiscriminately).
    pub fn request_references_for_calls(
        &mut self,
        rel_path: &str,
        line: u32,
        character: u32,
        local_idx: usize,
    ) {
        if self.sender.is_none() {
            return;
        }
        self.next_req_id += 1;
        let id = self.next_req_id;
        self.calls_refs_pending.insert(id, local_idx);
        let uri = format!("{}/{}", self.root_uri, rel_path);
        self.send_raw(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id":      id,
                "method":  "textDocument/references",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                    "context": { "includeDeclaration": false }
                }
            })
            .to_string(),
        );
    }

    /// Drain the completed call-graph reference lookups (Structure tab).
    pub fn take_calls_reference_results(&mut self) -> HashMap<usize, Vec<ReferenceLoc>> {
        std::mem::take(&mut self.calls_refs_results)
    }

    /// `true` while ANY whole-crate references search is in flight (usages pass
    /// or the Structure call-graph pass) — used to keep those passes serialized
    /// with each other, never flooding rust-analyzer with parallel searches.
    /// `true` while rust-analyzer owes us ANY answer.
    ///
    /// A `textDocument/didChange` bumps the document version, and rust-analyzer
    /// answers every request issued against the older version with "content
    /// modified". Nothing wedges — each reply path clears its own id — but two
    /// cancellations are silently WRONG rather than merely absent: a lost
    /// `references` reply is recorded as "0 references", which fades live code
    /// as dead in the usages overlay and drops a symbol's call edges from the
    /// Structure tab for that content hash.
    ///
    /// So the idle re-sync waits its turn instead of interrupting.
    pub fn any_request_in_flight(&self) -> bool {
        self.completion_req_id.is_some()
            || self.rename_req_id.is_some()
            || self.will_rename_req_id.is_some()
            || !self.code_action_pending.is_empty()
            || self.code_action_resolve_req_id.is_some()
            || self.definition_req_id.is_some()
            || self.implementation_req_id.is_some()
            || self.symbols_req_id.is_some()
            || self.inlay_req_id.is_some()
            || self.references_busy()
    }

    pub fn references_busy(&self) -> bool {
        !self.references_pending.is_empty() || !self.calls_refs_pending.is_empty()
    }

    // ── Diagnostic helpers ────────────────────────────────────────────────────

    pub fn error_count_for(&self, path: &str) -> usize {
        self.diagnostics
            .get(path)
            .map(|ds| ds.iter().filter(|d| d.severity.is_error()).count())
            .unwrap_or(0)
    }

    // Test-only: the project tree's `DiagBadges` is pinned against it.
    #[cfg(test)]
    pub fn warning_count_for(&self, path: &str) -> usize {
        self.diagnostics
            .get(path)
            .map(|ds| ds.iter().filter(|d| d.severity.is_warning()).count())
            .unwrap_or(0)
    }

    pub fn total_errors(&self) -> usize {
        let stale = self.flycheck_stale();
        self.diagnostics
            .values()
            .flat_map(|v| v.iter())
            .filter(|d| {
                d.severity.is_error()
                    && (d.source == "rust-analyzer" || d.is_rustc_error_code() || !stale)
            })
            .count()
    }

    pub fn total_warnings(&self) -> usize {
        let stale = self.flycheck_stale();
        self.diagnostics
            .values()
            .flat_map(|v| v.iter())
            .filter(|d| {
                d.severity.is_warning()
                    && (d.source == "rust-analyzer" || d.is_rustc_error_code() || !stale)
            })
            .count()
    }

    /// Terminate the rust-analyzer child process: a best-effort polite LSP
    /// `exit` notification, then a guaranteed `kill()` + reap. Called on every
    /// restart (`reset`) and on app exit (`AppIde::on_exit`) — without this the
    /// process outlives us (dropping a `Child` only detaches) and keeps
    /// watching + re-analyzing the workspace forever.
    pub fn kill_child(&mut self) {
        // Best effort — the write thread may or may not deliver this before the
        // kill lands; the kill below is the guarantee.
        self.send_raw(r#"{"jsonrpc":"2.0","method":"exit"}"#.to_owned());
        if let Some(mut child) = self.child.take() {
            // Kill the whole process TREE first: rust-analyzer spawns helpers
            // (proc-macro server, flycheck cargo) that survive a plain `kill()`
            // of the parent and then linger as orphans. See `kill_process_tree`.
            kill_process_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Stop any running session and reset to `Stopped`.
    /// Incrementing `generation` makes stale background threads bail out
    /// silently without corrupting the fresh state.
    pub fn reset(&mut self) {
        self.kill_child();
        self.generation += 1;
        self.linked_projects.clear();
        self.status = LspStatus::Stopped;
        self.diagnostics.clear();
        self.sender = None; // write thread's Receiver will close → it exits
        self.open_files.clear();
        self.awaiting_diagnostics.clear();
        self.edit_gen = 0;
        self.check_begin_gen = 0;
        self.fresh_check_gen = 0;
        self.root_uri = String::new();
        self.checking = false;
        self.indexed = false;
        self.server_status_seen = false;
        self.quiescent = false;
        self.initialized_at = None;
        self.held_save = None;
        self.exited_during_load = false;
        self.last_did_save_at = None;
        self.check_started_at = None;
        self.check_queued = std::time::Duration::ZERO;
        self.finished_checks.clear();
        self.completion_items = Arc::default();
        self.completion_req_id = None;
        self.completion_failure = None;
        self.completion_params = None;
        self.completion_retries = 0;
        self.rename_req_id = None;
        self.rename_response_received = false;
        self.rename_edits.clear();
        self.will_rename_req_id = None;
        self.will_rename_response_received = false;
        self.will_rename_edits.clear();
        self.definition_req_id = None;
        self.implementation_req_id = None;
        self.definition_response_received = false;
        self.definition_is_impl = false;
        self.definition_results.clear();
        self.definition_error = None;
        self.symbols_req_id = None;
        self.symbols_for_file.clear();
        self.symbols_response_received = false;
        self.symbols_result.clear();
        self.inlay_req_id = None;
        self.inlay_for_file.clear();
        self.inlay_for_line = 0;
        self.inlay_response_received = false;
        self.inlay_result.clear();
        self.code_action_pending.clear();
        self.code_action_response_received = false;
        self.code_actions.clear();
        self.code_action_resolve_req_id = None;
        self.code_action_resolve_received = false;
        self.code_action_resolved = None;
        self.references_pending.clear();
        self.references_results.clear();
        self.calls_refs_pending.clear();
        self.calls_refs_results.clear();
        self.load_log.clear();
        self.stderr_tail.clear();
        self.stderr_logged = 0;
        self.first_panic = None;
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Prepare the shared state and spawn `rust-analyzer` for `workspace_dir`.
///
/// The workspace files (Cargo.toml, src/main.rs, …) must already exist on disk.
/// Call `lsp_state.lock().unwrap().did_open(text)` once the status reaches
/// `LspStatus::Indexing`, then `did_change(text)` on every code update.
pub fn start(workspace_dir: &Path, state: Arc<Mutex<LspState>>, ctx: eframe::egui::Context) {
    let workspace_dir = workspace_dir.to_path_buf();
    let root_uri = path_to_uri(&workspace_dir);

    {
        let mut s = state.lock().unwrap();
        // Kill any previous rust-analyzer FIRST — otherwise it lingers as an
        // orphan, still watching + re-analyzing this same workspace on every
        // file write (a main driver of the everything-gets-slower degradation).
        s.kill_child();
        s.generation += 1;
        s.status = LspStatus::Starting;
        s.diagnostics.clear();
        s.root_uri = root_uri.clone();
        s.open_files.clear();
        // Drop any old sender — signals the old write thread to exit.
        s.sender = None;
    }
    ctx.request_repaint();

    thread::spawn(move || {
        launch(workspace_dir, root_uri, state, ctx);
    });
}

// ── Internal ──────────────────────────────────────────────────────────────────

fn launch(
    workspace_dir: PathBuf,
    root_uri: String,
    state: Arc<Mutex<LspState>>,
    ctx: eframe::egui::Context,
) {
    // Snapshot our generation so we can detect restarts.
    let my_gen = state.lock().unwrap().generation;

    // Reap rust-analyzers orphaned by PREVIOUS sessions (crash, Task-Manager
    // kill, anything that skipped `on_exit`). `kill_child` can only reach the
    // current process's own child — a fresh IDE launch has no handle to
    // yesterday's RA, which keeps watching this same workspace forever.
    // Observed live: three orphaned RA pairs from prior sessions, each
    // re-analyzing every Save and competing for the flycheck target-dir lock —
    // the "save time grows past 20 s over time" degradation. Runs on this
    // background thread (the per-pid `tasklist` / `ps` probe costs ~100 ms).
    sweep_stale_ras(&workspace_dir);

    let mut ra_cmd = Command::new("rust-analyzer");
    // Below normal priority, which its cargo / rustc / proc-macro server
    // inherit: loading a workspace keeps every core busy for a minute or more,
    // and at the IDE's own priority that minute was a frozen window. It still
    // gets all the CPU nobody else wants, so the load is no slower on an idle
    // machine.
    crate::build::no_window_below_normal(&mut ra_cmd);
    // Own process group (unix) so the tree can be killed as one — see
    // `spawn_in_own_group` / `kill_process_tree`.
    spawn_in_own_group(&mut ra_cmd);
    let mut child = match ra_cmd
        .current_dir(&workspace_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Piped, not null: a panic or a failed load is written HERE, and with
        // stderr discarded an exit mid-load said nothing at all.
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let mut s = state.lock().unwrap();
            if s.generation == my_gen {
                s.status = LspStatus::Failed(format!(
                    "Could not launch `rust-analyzer`: {e}\n\
                     Install from https://rust-analyzer.github.io or via rustup:\n\
                     rustup component add rust-analyzer"
                ));
                ctx.request_repaint();
            }
            return;
        }
    };

    // Register the pid so the NEXT launch can reap this RA even if this
    // process dies without running `on_exit` (see `sweep_stale_ras`).
    let ra_pid = child.id();
    register_ra_pid(&workspace_dir, ra_pid);

    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    // Channel: any thread with a Sender → write thread → RA stdin
    let (tx, rx) = mpsc::channel::<String>();

    // Store sender + child in LspState (only if our generation is still
    // current). The child handle lives in the shared state so `kill_child`
    // (restart / app exit) can actually terminate the process.
    {
        let mut s = state.lock().unwrap();
        if s.generation != my_gen {
            // Already restarted — this just-spawned RA is already stale; kill
            // it rather than leaking it as an orphan.
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        s.sender = Some(tx.clone());
        s.child = Some(child);
        s.push_load_log("• rust-analyzer process started");
    }

    // ── stderr thread ─────────────────────────────────────────────────────────
    // Drained for as long as the process lives (an unread pipe would block the
    // server once its buffer fills). Lines go to the disk trace always, to the
    // Analyzer tab up to a cap, and the last few into the exit message.
    //
    // It also watches for panics, and keeps one for the exit message: its
    // backtrace pushes it out of `stderr_tail` long before the exit. A panic on
    // the `LspServer` thread - rust-analyzer's main loop - ends the server there
    // and then, yet the process lingered for another ~50 s finishing a build
    // nobody could read, every core busy under a status that still said
    // "ready". It is stopped at once instead.
    {
        let state = Arc::clone(&state);
        thread::spawn(move || {
            let mut panics = PanicWatch::default();
            let mut killed = false;
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                {
                    let mut s = state.lock().unwrap();
                    if s.generation != my_gen {
                        continue; // a restarted session's leftovers: drain only
                    }
                    s.stderr_tail.push_back(line.clone());
                    if s.stderr_tail.len() > STDERR_TAIL {
                        s.stderr_tail.pop_front();
                    }
                    if s.stderr_logged < STDERR_TO_LOAD_LOG {
                        s.stderr_logged += 1;
                        s.push_load_log(format!("[stderr] {line}"));
                    } else {
                        append_ra_trace(&format!("[stderr] {line}"));
                    }
                    for p in panics.feed(&line) {
                        let fatal = p.fatal();
                        // The first panic, unless a fatal one follows a worker's:
                        // rust-analyzer catches a request handler's panic and
                        // carries on, so that one explains no exit.
                        if !s.first_panic.as_ref().is_some_and(|(_, f)| *f || !fatal) {
                            s.first_panic = Some((p.text.clone(), fatal));
                        }
                        // Killed UNDER the lock and only while `child` is still
                        // ours: that handle keeps Windows from reusing the pid,
                        // and the read loop can only drop it under this lock. A
                        // ~100 ms stall of the UI once per crash, the same one
                        // `kill_child` costs on every restart.
                        if p.thread == "LspServer" && !killed && s.child.is_some() {
                            killed = true;
                            s.push_load_log(
                                "[error] rust-analyzer's main loop panicked - it can no \
                                 longer answer, so it is stopped now",
                            );
                            kill_process_tree(ra_pid);
                        }
                    }
                }
            }
        });
    }

    // ── Write thread ──────────────────────────────────────────────────────────
    thread::spawn(move || {
        let mut stdin = stdin;
        for msg in rx {
            if write_lsp(&mut stdin, &msg).is_err() {
                break;
            }
        }
        // stdin dropped here → RA gets EOF on its stdin
    });

    // What this session is told to load, kept on the state for the
    // Structure tab - unless a newer session has already taken over.
    let linked = linked_projects(&workspace_dir);
    {
        let mut s = state.lock().unwrap();
        if s.generation == my_gen {
            s.linked_projects = linked.clone().unwrap_or_default();
        }
    }
    // ── Send `initialize` ─────────────────────────────────────────────────────
    let _ = tx.send(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id":      1,
            "method":  "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri":   root_uri,
                "workspaceFolders": [{ "uri": root_uri, "name": "project" }],
                "capabilities": client_capabilities(),
                "initializationOptions": initialization_options_with(linked.as_deref()),
            }
        })
        .to_string(),
    );

    // ── Read loop (this thread IS the read thread) ─────────────────────────────
    let mut reader = BufReader::new(stdout);
    let tx_read = tx.clone(); // for sending `initialized` + `didOpen` from this thread

    loop {
        match read_lsp(&mut reader) {
            Some(msg) => handle_incoming(msg, &state, &ctx, &tx_read, &root_uri, my_gen),
            None => break, // EOF — RA exited
        }
    }

    // RA exited (or we got EOF).
    //
    // Reap OUR exited child (releases the process handle) and keep its exit
    // code for the message. If the generation moved on, `state.child` already
    // belongs to the NEW RA — leave it alone (ours was killed+reaped by
    // `kill_child`). The wait runs outside the lock: stdout is closed, so the
    // process is gone or going, but the UI thread must never wait on it.
    let child = {
        let mut s = state.lock().unwrap();
        if s.generation != my_gen {
            return;
        }
        s.child.take()
    };
    let code = child
        .and_then(|mut c| c.wait().ok())
        .and_then(|status| status.code());
    // A moment for the stderr thread to hand over the lines written just
    // before the exit — the panic message is usually the very last thing.
    thread::sleep(std::time::Duration::from_millis(150));
    let mut s = state.lock().unwrap();
    if s.generation == my_gen && s.status.is_active() {
        let tail: Vec<String> = s.stderr_tail.iter().cloned().collect();
        let panic = s
            .first_panic
            .as_ref()
            .map(|(text, fatal)| (text.as_str(), *fatal));
        let msg = exit_message(code, panic, &tail);
        s.push_load_log(format!("[error] {msg}"));
        // Read BEFORE the status changes: the app restarts once, by itself, a
        // server that died while it was still loading.
        s.exited_during_load = !s.workspace_loaded();
        s.status = LspStatus::Failed(msg);
        ctx.request_repaint();
    }
}

// ── Process-tree control (per platform) ──────────────────────────────────────
// rust-analyzer is never one process: it spawns a proc-macro server and a
// flycheck `cargo`, and killing only the parent leaves those behind. Windows
// gets that from `taskkill /T`; the unixes have no such flag, so RA is put in
// its OWN PROCESS GROUP at spawn and the whole group is signalled at once.

/// Put the child in its own process group, so it can later be killed as a tree.
/// No-op on Windows, where `taskkill /T` walks the tree instead.
///
/// Side effect worth knowing: a process in its own group no longer receives the
/// terminal's signals (a Ctrl+C aimed at the IDE won't reach RA). That is what
/// we want — RA's lifetime is managed explicitly here — and the case it leaves
/// open, the IDE dying without cleaning up, is exactly what `sweep_stale_ras`
/// covers on the next launch.
fn spawn_in_own_group(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // 0 = new group whose pgid is the child's pid
    }
    cmd
}

/// Kill `pid` **and its children**, forcefully.
///
/// Unix: signal the process GROUP (negative pid) — that is the tree, because
/// [`spawn_in_own_group`] made the child a group leader. Falls back to the lone
/// process for a pid that isn't one (an RA registered by an older build of this
/// IDE, before the group existed).
pub(crate) fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = crate::build::no_window(&mut Command::new("taskkill"))
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }
    #[cfg(unix)]
    {
        // SIGKILL, not SIGTERM: the polite path is the LSP `exit` notification
        // sent before this, mirroring the `/F` on the Windows side.
        let group = -(pid as i32);
        let killed = unsafe { libc::kill(group, libc::SIGKILL) };
        if killed != 0 {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
    #[cfg(not(any(windows, unix)))]
    let _ = pid;
}

// ── Stale rust-analyzer sweep (pid file) ─────────────────────────────────────
// Every RA spawn is registered in `<workspace>/ra.pids`; every launch first
// kills any registered pid that is STILL a live rust-analyzer. This is what
// catches orphans across app restarts — `kill_child` covers only the clean
// paths (in-session restart, `on_exit`), so a crash or a Task-Manager kill
// used to leave RA pairs alive for days, all re-analyzing this workspace on
// every Save. PID-reuse safety: a pid is killed only after the OS confirms its
// image name is still rust-analyzer — a recycled pid must never be shot.

fn ra_pid_file(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("ra.pids")
}

/// Parse the pid-file contents: one pid per line; junk lines ignored.
fn parse_pid_lines(text: &str) -> Vec<u32> {
    text.lines().filter_map(|l| l.trim().parse().ok()).collect()
}

/// Extract the image name from one `tasklist /FO CSV /NH` output line
/// (`"rust-analyzer.exe","16340",…`) — `None` for the "INFO: No tasks…"
/// message or malformed lines.
fn tasklist_image_name(csv_line: &str) -> Option<String> {
    let first = csv_line.split("\",\"").next()?;
    let name = first.trim().trim_start_matches('"');
    (!name.is_empty() && !name.starts_with("INFO:")).then(|| name.to_owned())
}

/// Does `ps -p <pid> -o comm=` output name rust-analyzer?
///
/// The two unixes disagree on what `comm` is: Linux prints the bare command
/// (truncated to 15 chars — `rust-analyzer` is 13, so it survives whole), macOS
/// prints the full executable PATH. Taking the last path component handles both.
/// Empty output = no such process, which is the common case.
///
/// Compiled everywhere (only the unixes CALL it) so the parser stays testable
/// on this host — the same reasoning as the udev scan in [`crate::required_tools`].
#[cfg_attr(windows, allow(dead_code))]
fn ps_comm_is_rust_analyzer(ps_output: &str) -> bool {
    ps_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .any(|l| {
            l.rsplit(['/', '\\'])
                .next()
                .unwrap_or(l)
                .starts_with("rust-analyzer")
        })
}

/// Is `pid` still a live rust-analyzer? The guard against PID reuse: between
/// the pid being written and this sweep, the OS may have handed that number to
/// something else entirely.
fn pid_is_rust_analyzer(pid: u32) -> bool {
    #[cfg(windows)]
    {
        crate::build::no_window(&mut Command::new("tasklist"))
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .ok()
            .map(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(tasklist_image_name)
                    .any(|name| name.to_lowercase().starts_with("rust-analyzer"))
            })
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        // `ps` rather than /proc: macOS has no /proc, and this one command is
        // specified by POSIX, so it answers on every unix the IDE can run on.
        Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()
            .map(|out| ps_comm_is_rust_analyzer(&String::from_utf8_lossy(&out.stdout)))
            .unwrap_or(false)
    }
}

/// Kill every pid registered in the workspace pid file that is still a live
/// rust-analyzer (its helper children — proc-macro server, flycheck cargo — go
/// with it), then clear the file.
fn sweep_stale_ras(workspace_dir: &Path) {
    let path = ra_pid_file(workspace_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    for pid in parse_pid_lines(&text) {
        if pid_is_rust_analyzer(pid) {
            kill_process_tree(pid);
        }
    }
    let _ = std::fs::remove_file(&path);
}

/// Append `pid` to the workspace pid file (created on first use).
fn register_ra_pid(workspace_dir: &Path, pid: u32) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ra_pid_file(workspace_dir))
    {
        let _ = writeln!(f, "{pid}");
    }
}

// ── LSP framing ───────────────────────────────────────────────────────────────

fn write_lsp(sink: &mut impl Write, json: &str) -> std::io::Result<()> {
    write!(sink, "Content-Length: {}\r\n\r\n", json.len())?;
    sink.write_all(json.as_bytes())?;
    sink.flush()
}

fn read_lsp<R: BufRead>(reader: &mut R) -> Option<serde_json::Value> {
    // Read headers until blank line
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            return None; // EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length: ") {
            content_length = v.trim().parse().ok()?;
        }
    }
    if content_length == 0 {
        return None;
    }
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

// ── Message handler ───────────────────────────────────────────────────────────

/// Debug-log a line into the shared LSP log from OUTSIDE this module (e.g. the
/// Structure tab's call-graph pass) — same file, same debug-only gating.
pub fn debug_log(line: &str) {
    lsp_log(line);
}

// ── LSP debug log ────────────────────────────────────────────────────────────
// OFF by default, even in debug builds. It used to be on for every debug run,
// which is how it reached 108 MB in a single session: one `open + append +
// close` per protocol message on an ever-growing file, and the traffic peaks
// exactly at Save (didChange per file, didSave, publishDiagnostics, symbol and
// inlay-hint bursts). Two more guards below: the file is now kept OPEN, and it
// is capped, so a long session can't make the next Save slower than the last.
//
// Turn it on with EIDE_LSP_LOG=1 (accepts 1 / true / on / yes).

/// Bytes after which the log rotates to `<name>.1`. Two files, bounded disk,
/// and the recent history — the part worth reading — always survives.
#[cfg(debug_assertions)]
const LOG_CAP: u64 = 16 * 1024 * 1024;

/// Is the LSP debug log turned on for this run? Read once — an env lookup per
/// protocol message is exactly the kind of cost this whole change is about.
///
/// Public so hot call sites can skip BUILDING their message: `debug_log(&format!(…))`
/// formats the string whether or not anything consumes it.
pub fn log_enabled() -> bool {
    #[cfg(debug_assertions)]
    {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ENABLED.get_or_init(|| {
            std::env::var("EIDE_LSP_LOG").is_ok_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "yes"
                )
            })
        })
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

/// The open log file plus the byte count that drives rotation. Keeping the
/// handle open is the point: reopening per line is what made this expensive.
#[cfg(debug_assertions)]
static LOG_FILE: Mutex<Option<(std::fs::File, u64)>> = Mutex::new(None);

/// Append a line to the LSP debug log in the system temp dir.
///
/// File: `<TEMP>/rust_on_chip_lsp.log` — plus this instance's slot suffix, so
/// two IDE windows don't interleave their handshakes into one unreadable file.
/// No-op in release, and in debug unless `EIDE_LSP_LOG` is set.
#[cfg(debug_assertions)]
fn lsp_log(line: &str) {
    if !log_enabled() {
        return;
    }
    let path = std::env::temp_dir().join(format!(
        "rust_on_chip_lsp{}.log",
        crate::workspace::suffix()
    ));
    let Ok(mut slot) = LOG_FILE.lock() else {
        return;
    };
    // Rotate BEFORE writing, so the cap is a real ceiling. Dropping the handle
    // first keeps the rename working on Windows.
    if slot
        .as_ref()
        .is_some_and(|(_, written)| *written >= LOG_CAP)
    {
        *slot = None;
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if slot.is_none() {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path);
        let Ok(file) = file else {
            return;
        };
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        *slot = Some((file, written));
    }
    if let Some((file, written)) = slot.as_mut() {
        if writeln!(file, "{line}").is_ok() {
            *written += line.len() as u64 + 1;
        }
    }
}
#[cfg(not(debug_assertions))]
fn lsp_log(_: &str) {}

/// Serialize `value` to JSON, stopping after `limit` bytes.
///
/// The reason this exists: the old preview did `value.to_string()` and then
/// kept the first 200 characters, so every `documentSymbol` answer — the whole
/// symbol tree of a file — was serialized in full to be thrown away. Writing
/// into a sink that refuses more than `limit` bytes stops serde at the limit
/// instead. `from_utf8_lossy` closes the other half of that bug: the old byte
/// slice `&r[..200]` panics when byte 200 lands inside a multi-byte character.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
fn truncated_json(value: &serde_json::Value, limit: usize) -> String {
    struct LimitWriter {
        buf: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for LimitWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let room = self.limit.saturating_sub(self.buf.len());
            if room == 0 {
                // Any error stops `to_writer` — that IS the early exit.
                return Err(std::io::Error::other("limit reached"));
            }
            let take = room.min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            Ok(take)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut w = LimitWriter {
        buf: Vec::with_capacity(limit.min(256)),
        limit,
    };
    let complete = serde_json::to_writer(&mut w, value).is_ok();
    let mut out = String::from_utf8_lossy(&w.buf).into_owned();
    if !complete {
        out.push_str("…(truncated)");
    }
    out
}

fn handle_incoming(
    msg: serde_json::Value,
    state: &Arc<Mutex<LspState>>,
    ctx: &eframe::egui::Context,
    tx: &mpsc::Sender<String>,
    root_uri: &str,
    my_gen: u64,
) {
    // Guard: if generation advanced we are a stale thread — stop processing.
    if state.lock().unwrap().generation != my_gen {
        return;
    }

    let method = msg["method"].as_str().unwrap_or("");

    // Log all response messages (no "method") for debugging. The `log_enabled`
    // gate comes FIRST: everything below — serializing the payload, formatting
    // the line — is pure cost when nothing will read it, and this runs on every
    // single response RA sends.
    #[cfg(debug_assertions)]
    if method.is_empty() && log_enabled() {
        let id = &msg["id"];
        let preview = match (msg.get("result"), msg.get("error")) {
            (Some(r), _) => format!("result={}", truncated_json(r, 200)),
            (_, Some(e)) => format!("error={}", truncated_json(e, 500)),
            _ => "?".to_owned(),
        };
        lsp_log(&format!("RESPONSE id={id} {preview}"));
    }

    match method {
        // ── Initialize response ───────────────────────────────────────────────
        "" if msg.get("id") == Some(&serde_json::Value::Number(1.into()))
            && msg.get("result").is_some() =>
        {
            // Confirm handshake.
            let _ = tx.send(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.to_owned());
            let mut s = state.lock().unwrap();
            if s.generation == my_gen {
                s.status = LspStatus::Indexing;
                s.initialized_at = Some(std::time::Instant::now());
                s.push_load_log("• initialize handshake OK — loading workspace…");
            }
            ctx.request_repaint();
        }

        // ── Server status (`experimental/serverStatus`) ───────────────────────
        // The only word on whether the workspace is really loaded: `quiescent`
        // waits for the fetch, build scripts, proc-macros, the file scan and
        // cache priming, and drops back while a refetch runs. Sent on every
        // change of health, quiescence or message. A held `didSave` goes out
        // the moment it says the workspace is loaded.
        "experimental/serverStatus" => {
            let params = &msg["params"];
            let quiescent = params["quiescent"].as_bool().unwrap_or(false);
            let health = params["health"].as_str().unwrap_or("ok");
            let message = params["message"].as_str().unwrap_or("").trim();
            let mut s = state.lock().unwrap();
            if s.generation != my_gen {
                return;
            }
            let was = s.server_status_seen && s.quiescent;
            s.server_status_seen = true;
            s.quiescent = quiescent;
            if quiescent && !was {
                s.push_load_log("• workspace loaded");
                // Loaded is more than indexed, and more than `Ready` promises.
                s.indexed = true;
                if matches!(s.status, LspStatus::Starting | LspStatus::Indexing) {
                    s.status = LspStatus::Ready;
                }
            } else if !quiescent && was {
                s.push_load_log("• workspace reloading…");
            }
            if health != "ok" && !message.is_empty() {
                let tag = if health == "error" {
                    "[error]"
                } else {
                    "[warn]"
                };
                for (i, line) in message
                    .lines()
                    .map(str::trim_end)
                    .filter(|l| !l.is_empty())
                    .enumerate()
                {
                    if i == 0 {
                        s.push_load_log(format!("{tag} {line}"));
                    } else {
                        s.push_load_log(format!("    {line}"));
                    }
                }
            }
            s.release_held_save();
            ctx.request_repaint();
        }

        // ── Diagnostics ───────────────────────────────────────────────────────
        "textDocument/publishDiagnostics" => {
            let params = &msg["params"];
            let uri = params["uri"].as_str().unwrap_or("");
            let rel_path = uri_to_rel(uri, root_uri);

            let diags: Vec<LspDiagnostic> = params["diagnostics"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(parse_diag)
                .collect();

            let mut s = state.lock().unwrap();
            if s.generation != my_gen {
                return;
            }
            // RA has re-evaluated this file → its diagnostics are fresh again,
            // so the inline overlay may show them (see `diagnostics_fresh`).
            s.awaiting_diagnostics.remove(&rel_path);
            if diags.is_empty() {
                s.diagnostics.remove(&rel_path);
            } else {
                s.diagnostics.insert(rel_path, diags);
            }
            // Transition Indexing → Ready on first diagnostic push.
            if matches!(s.status, LspStatus::Indexing | LspStatus::Starting) {
                s.status = LspStatus::Ready;
            }
            ctx.request_repaint();
        }

        // ── Window progress ────────────────────────────────────────────────────
        // RA sends $/progress for two kinds of work:
        //   1. Indexing  — token contains "rust" or "index";  "end" → Ready
        //   2. cargo check — token contains "cargo" or "check";
        //                    "begin" → checking=true, "end" → checking=false
        "$/progress" => {
            let kind = msg["params"]["value"]["kind"].as_str().unwrap_or("");
            let token_raw = msg["params"]["token"].as_str().unwrap_or("");
            // RA may use numeric tokens — convert to string for matching
            let token_num = msg["params"]["token"].as_u64().map(|n| n.to_string());
            let token = token_num.as_deref().unwrap_or(token_raw);

            let is_indexing = token.contains("rust") || token.contains("index");
            let is_check =
                token.contains("cargo") || token.contains("check") || token.contains("flycheck");

            let mut s = state.lock().unwrap();
            if s.generation != my_gen {
                return;
            }

            // Trace the loading phases (Analyzer tab). A `begin` carries a human
            // title ("Fetching metadata", "Building CrateGraph", "Indexing"); log
            // it so a load that STALLS shows where it got stuck. Check spans are
            // routine post-load noise — only log those before RA is Ready.
            if kind == "begin" {
                if let Some(title) = msg["params"]["value"]["title"].as_str() {
                    if !is_check || s.status != LspStatus::Ready {
                        s.push_load_log(format!("• {title}"));
                        ctx.request_repaint();
                    }
                }
            }

            // A finished load phase = crate graph + sysroot are in place.
            //
            // Matching only a token that NAMES indexing was too strict: the
            // token is whatever RA chose, and this server sends numeric ones —
            // so the flag never flipped, every gate on it fell back to its
            // timeout, and a deferred Go-to-definition waited for a condition
            // that could not arrive. `is_indexing` widens it to a rust-prefixed
            // token, but a NUMERIC token still matches neither test here; for
            // those the `experimental/serverStatus` handler above sets
            // `indexed` when the server reports `quiescent`.
            if kind == "end" && (is_indexing || token.to_ascii_lowercase().contains("index")) {
                s.indexed = true;
                ctx.request_repaint();
            }
            if is_indexing && kind == "end" && s.status == LspStatus::Indexing {
                s.status = LspStatus::Ready;
                ctx.request_repaint();
            }
            if is_check {
                match kind {
                    "begin" => {
                        s.checking = true;
                        // This check reflects all edits made up to now.
                        s.check_begin_gen = s.edit_gen;
                        // Queue latency: how long RA sat on the didSave before
                        // cargo actually started (a clogged RA shows up HERE,
                        // a slow cargo shows up in the run span).
                        s.check_queued = s
                            .last_did_save_at
                            .take()
                            .map(|t| t.elapsed())
                            .unwrap_or_default();
                        s.check_started_at = Some(std::time::Instant::now());
                        ctx.request_repaint();
                    }
                    "end" => {
                        s.checking = false;
                        // Its diagnostics are now fresh up to the gen it began at;
                        // any edit made *during* the check keeps `flycheck_stale`
                        // true until the next check completes.
                        s.fresh_check_gen = s.check_begin_gen;
                        if let Some(t) = s.check_started_at.take() {
                            let queued = s.check_queued;
                            s.finished_checks.push((queued, t.elapsed()));
                        }
                        ctx.request_repaint();
                    }
                    _ => {}
                }
            }
        }

        // ── Completion response (success) ─────────────────────────────────────
        // Any response (method == "") whose id is not 1 (initialize) and that
        // carries a "result" field is treated as a completion response.
        "" if msg.get("result").is_some()
            && msg.get("id").is_some()
            && msg["id"].as_u64().map_or(false, |n| n != 1) =>
        {
            if let Some(req_id) = msg["id"].as_u64() {
                let mut s = state.lock().unwrap();
                if s.generation != my_gen {
                    return;
                }
                // Rename response (a WorkspaceEdit) — must be checked before the
                // completion branch since both are id-keyed result messages.
                if s.rename_req_id == Some(req_id) {
                    s.rename_req_id = None;
                    s.rename_edits = parse_workspace_edit(&msg["result"], root_uri);
                    s.rename_response_received = true;
                    ctx.request_repaint();
                } else if s.will_rename_req_id == Some(req_id) {
                    // A file rename's edits. `null` is a legitimate answer
                    // ("nothing to change") and parses to an empty vec.
                    s.will_rename_req_id = None;
                    s.will_rename_edits = parse_workspace_edit(&msg["result"], root_uri);
                    s.will_rename_response_received = true;
                    ctx.request_repaint();
                } else if s.definition_req_id == Some(req_id) {
                    s.definition_req_id = None;
                    s.definition_results = parse_definition_list(&msg["result"]);
                    s.definition_response_received = true;
                    ctx.request_repaint();
                } else if s.implementation_req_id == Some(req_id) {
                    // Same Location | Location[] | LocationLink[] shapes as a
                    // definition response — funneled into the same slot.
                    s.implementation_req_id = None;
                    s.definition_results = parse_definition_list(&msg["result"]);
                    s.definition_response_received = true;
                    ctx.request_repaint();
                } else if s.symbols_req_id == Some(req_id) {
                    s.symbols_req_id = None;
                    s.symbols_result = parse_document_symbols(&msg["result"]);
                    s.symbols_response_received = true;
                    ctx.request_repaint();
                } else if s.inlay_req_id == Some(req_id) {
                    s.inlay_req_id = None;
                    let rel = s.inlay_for_file.clone();
                    s.inlay_result = parse_inlay_hints(&msg["result"], &rel);
                    s.inlay_response_received = true;
                    ctx.request_repaint();
                } else if s.is_code_action_req(req_id) {
                    // Parsed only once the id is known to be ours: every reply
                    // this far down the chain would otherwise be parsed too.
                    let actions = parse_code_actions(&msg["result"], root_uri);
                    s.record_code_actions(req_id, actions);
                    ctx.request_repaint();
                } else if s.code_action_resolve_req_id == Some(req_id) {
                    s.code_action_resolve_req_id = None;
                    // The resolved action carries its `edit` now.
                    let edits = parse_workspace_edit(&msg["result"]["edit"], root_uri);
                    s.code_action_resolved = (!edits.is_empty()).then_some(edits);
                    s.code_action_resolve_received = true;
                    ctx.request_repaint();
                } else if let Some(local_idx) = s.references_pending.remove(&req_id) {
                    s.references_results
                        .insert(local_idx, parse_references(&msg["result"]));
                    ctx.request_repaint();
                } else if let Some(local_idx) = s.calls_refs_pending.remove(&req_id) {
                    s.calls_refs_results
                        .insert(local_idx, parse_references(&msg["result"]));
                    ctx.request_repaint();
                } else if s.completion_req_id == Some(req_id) {
                    s.completion_req_id = None;
                    s.completion_response_received = true;
                    let result = &msg["result"];
                    // CompletionList { items: [...] }  OR  [...] directly
                    // `result` may also be JSON null — treat as empty list, but
                    // remember it was null: that one has its own cause.
                    if result.is_null() {
                        s.completion_failure = Some(CompletionFailure::Null);
                    }
                    let items_arr = result["items"].as_array().or_else(|| result.as_array());
                    s.completion_items = Arc::new(
                        items_arr
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(parse_completion_item)
                                    .take(60)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    );
                    lsp_log(&format!(
                        "COMPLETION_RESP id={req_id} items={} null={}",
                        s.completion_items.len(),
                        result.is_null()
                    ));
                    ctx.request_repaint();
                }
            }
        }

        // ── Completion response (error / cancel) ──────────────────────────────
        // RA returns {"id": N, "error": {...}} when it cannot fulfil a request
        // (e.g. the file won't compile, or the request was cancelled).
        // We must handle this or the spinner runs forever.
        "" if msg.get("error").is_some()
            && msg.get("id").is_some()
            && msg["id"].as_u64().map_or(false, |n| n != 1) =>
        {
            if let Some(req_id) = msg["id"].as_u64() {
                let mut s = state.lock().unwrap();
                if s.generation != my_gen {
                    return;
                }
                if s.rename_req_id == Some(req_id) {
                    // Rename failed / not allowed → empty edits, stop waiting.
                    s.rename_req_id = None;
                    s.rename_response_received = true;
                    ctx.request_repaint();
                } else if s.will_rename_req_id == Some(req_id) {
                    // Same: an error means "no edits", and the caller then
                    // renames the file without touching any references.
                    s.will_rename_req_id = None;
                    s.will_rename_response_received = true;
                    ctx.request_repaint();
                } else if s.definition_req_id == Some(req_id) {
                    s.definition_req_id = None;
                    s.definition_error = msg["error"]["message"].as_str().map(str::to_owned);
                    s.definition_response_received = true; // no result, stop waiting
                    ctx.request_repaint();
                } else if s.implementation_req_id == Some(req_id) {
                    s.implementation_req_id = None;
                    s.definition_error = msg["error"]["message"].as_str().map(str::to_owned);
                    s.definition_response_received = true; // no result, stop waiting
                    ctx.request_repaint();
                } else if s.symbols_req_id == Some(req_id) {
                    s.symbols_req_id = None;
                    s.symbols_response_received = true; // empty result, stop waiting
                    ctx.request_repaint();
                } else if s.inlay_req_id == Some(req_id) {
                    s.inlay_req_id = None;
                    s.inlay_response_received = true; // no hints, stop waiting
                    ctx.request_repaint();
                } else if s.record_code_actions(req_id, Vec::new()) {
                    // An error answers its part with an empty list, so the
                    // others still publish and nothing waits for ever.
                    ctx.request_repaint();
                } else if s.code_action_resolve_req_id == Some(req_id) {
                    s.code_action_resolve_req_id = None;
                    s.code_action_resolve_received = true; // no edit, stop waiting
                    ctx.request_repaint();
                } else if let Some(local_idx) = s.references_pending.remove(&req_id) {
                    // Treat as "0 references" rather than leaving it pending forever.
                    s.references_results.insert(local_idx, Vec::new());
                    ctx.request_repaint();
                } else if let Some(local_idx) = s.calls_refs_pending.remove(&req_id) {
                    // Same: an error reply must not wedge the call-graph pass.
                    s.calls_refs_results.insert(local_idx, Vec::new());
                    ctx.request_repaint();
                } else if s.completion_req_id == Some(req_id) {
                    s.completion_req_id = None;
                    let code = msg["error"]["code"].as_i64().unwrap_or(0);
                    let message = msg["error"]["message"].as_str().unwrap_or("").to_owned();
                    lsp_log(&format!(
                        "COMPLETION_ERR id={req_id} code={code} msg={message}"
                    ));
                    // A document changed under the request — a Save flush or the
                    // other view's idle re-sync is enough. Ask again: the
                    // `didChange` that cancelled it is already ahead of the new
                    // request in rust-analyzer's queue. Answering the popup with
                    // "nothing" instead was the spinner that vanished at once.
                    if is_transient_lsp_error(code) && s.retry_completion() {
                        return;
                    }
                    s.completion_failure = Some(CompletionFailure::Error { code, message });
                    s.completion_response_received = true;
                    // completion_items stays empty — App will close the popup.
                    ctx.request_repaint();
                }
            }
        }

        // ── Server-side messages (the workspace-load failure channel) ─────────
        // RA reports a failed `cargo metadata` / project load via these — the
        // exact diagnosis a stuck "Checking…" otherwise hides. `type`: 1=Error
        // 2=Warning 3=Info 4=Log. showMessage is user-facing (always logged);
        // logMessage is a firehose, so keep Error/Warning only.
        "window/showMessage" | "window/logMessage" => {
            let ty = msg["params"]["type"].as_u64().unwrap_or(4);
            let text = msg["params"]["message"].as_str().unwrap_or("").trim();
            let keep = method == "window/showMessage" || ty <= 2;
            if keep && !text.is_empty() {
                let tag = match ty {
                    1 => "[error]",
                    2 => "[warn]",
                    3 => "[info]",
                    _ => "[log]",
                };
                let mut s = state.lock().unwrap();
                if s.generation != my_gen {
                    return;
                }
                // Multi-line messages (cargo's backtrace) collapse to readable
                // rows so the Analyzer tab stays scannable.
                for (i, line) in text.lines().enumerate() {
                    let line = line.trim_end();
                    if line.is_empty() {
                        continue;
                    }
                    if i == 0 {
                        s.push_load_log(format!("{tag} {line}"));
                    } else {
                        s.push_load_log(format!("    {line}"));
                    }
                }
                ctx.request_repaint();
            }
        }

        // Ignore telemetry, etc.
        _ => {}
    }
}

fn parse_diag(v: &serde_json::Value) -> Option<LspDiagnostic> {
    let message = v["message"].as_str()?.to_owned();
    let severity = DiagSeverity::from_lsp(v["severity"].as_u64().unwrap_or(1));
    let start = &v["range"]["start"];
    let end_v = &v["range"]["end"];
    let line = start["line"].as_u64().unwrap_or(0) as u32 + 1;
    let col = start["character"].as_u64().unwrap_or(0) as u32 + 1;
    let end_line = end_v["line"].as_u64().unwrap_or(0) as u32 + 1;
    let end_col = end_v["character"].as_u64().unwrap_or(0) as u32 + 1;
    // code may be a string like "E0308" or an integer
    let code = v["code"]
        .as_str()
        .map(String::from)
        .or_else(|| v["code"].as_u64().map(|n| n.to_string()));
    let source = v["source"].as_str().unwrap_or("").to_owned();
    Some(LspDiagnostic {
        severity,
        message,
        line,
        col,
        end_line,
        end_col,
        code,
        source,
    })
}

// ── URI helpers ───────────────────────────────────────────────────────────────

pub fn path_to_uri(path: &Path) -> String {
    // On Windows, Path::canonicalize() returns extended-length paths prefixed
    // with \\?\ (e.g. \\?\C:\Users\...).  Replacing every backslash with a
    // forward slash would produce //?/C:/... — an invalid file URI that causes
    // rust-analyzer to return error -32603 "url is not a file" for every request.
    //
    // Fix: strip the \\?\ prefix before building the URI.
    // If canonicalize fails (directory doesn't exist yet), the fallback path
    // is already an absolute path without the \\?\ prefix.
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = canonical.to_string_lossy();

    #[cfg(windows)]
    {
        // Strip the Windows extended-length path prefix \\?\ if present,
        // then normalise backslashes to forward slashes.
        // Result: file:///C:/Users/foo/bar
        let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s);
        let normalised = stripped.replace('\\', "/");
        format!("file:///{normalised}")
    }
    #[cfg(not(windows))]
    {
        // Unix: /foo/bar  →  file:///foo/bar
        format!("file://{s}")
    }
}

/// Strip `root_uri + "/"` from an absolute URI to get the relative path.
/// `"file:///tmp/proj/src/main.rs"` → `"src/main.rs"`
///
/// On Windows, rust-analyzer may normalise the drive letter to lowercase
/// (`file:///c:/…`) while `path_to_uri` produces uppercase (`file:///C:/…`).
/// The comparison is therefore case-insensitive on Windows so that the key
/// stored in `LspState.diagnostics` is always the short relative path
/// (`src/main.rs`) rather than the full URI.
fn uri_to_rel(uri: &str, root_uri: &str) -> String {
    let prefix = format!("{root_uri}/");

    // Case-sensitive match first (non-Windows, or matching case on Windows)
    if let Some(rel) = uri.strip_prefix(&prefix) {
        return rel.to_owned();
    }

    // Case-insensitive fallback for Windows drive-letter mismatches
    #[cfg(windows)]
    {
        let uri_lc = uri.to_lowercase();
        let pfx_lc = prefix.to_lowercase();
        if uri_lc.starts_with(&pfx_lc) {
            // Preserve the original (potentially mixed-case) suffix
            return uri[prefix.len()..].to_owned();
        }
    }

    // Nothing matched — return the full URI as-is; the diags_for_main_rs
    // helper in app.rs will still find it by suffix matching.
    uri.to_owned()
}

/// Parse a `textDocument/rename` result (a WorkspaceEdit) into flat edits.
/// Handles both `documentChanges` (RA's default) and the older `changes` map.
fn parse_workspace_edit(result: &serde_json::Value, root_uri: &str) -> Vec<RenameEdit> {
    let mut out = Vec::new();
    if let Some(dcs) = result["documentChanges"].as_array() {
        for dc in dcs {
            // Skip rename/create/delete file ops (they have a "kind"); we only
            // apply text edits to existing documents.
            let uri = dc["textDocument"]["uri"].as_str().unwrap_or("");
            if uri.is_empty() {
                continue;
            }
            let rel = uri_to_rel(uri, root_uri);
            if let Some(edits) = dc["edits"].as_array() {
                out.extend(edits.iter().filter_map(|e| parse_text_edit(e, &rel)));
            }
        }
    } else if let Some(changes) = result["changes"].as_object() {
        for (uri, edits) in changes {
            let rel = uri_to_rel(uri, root_uri);
            if let Some(arr) = edits.as_array() {
                out.extend(arr.iter().filter_map(|e| parse_text_edit(e, &rel)));
            }
        }
    }
    out
}

/// Parse a `textDocument/codeAction` result: `(Command | CodeAction)[]`. Plain
/// `Command`s (no `edit`, no `data`, but a `command` field) are dropped —
/// `is_applicable` filters them anyway. Each `CodeAction`'s inline `edit` is
/// parsed now; lazy ones keep `edits: None` and resolve later.
/// One list from the answers of several code-action requests: in request
/// order, and an action offered at two positions (the same title) only once.
fn merge_code_actions(parts: impl IntoIterator<Item = Vec<CodeAction>>) -> Vec<CodeAction> {
    let mut out: Vec<CodeAction> = Vec::new();
    for action in parts.into_iter().flatten() {
        if !out.iter().any(|a| a.title == action.title) {
            out.push(action);
        }
    }
    out
}

fn parse_code_actions(result: &serde_json::Value, root_uri: &str) -> Vec<CodeAction> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|a| {
            let title = a["title"].as_str()?.to_owned();
            let edits = a
                .get("edit")
                .filter(|e| !e.is_null())
                .map(|e| parse_workspace_edit(e, root_uri));
            Some(CodeAction {
                title,
                edits,
                raw: a.clone(),
            })
        })
        .filter(CodeAction::is_applicable)
        .collect()
}

/// The `capabilities` the IDE announces in `initialize`.
///
/// Its own function so the promises in it can be tested - one of them,
/// `experimental.serverStatusNotification`, is what keeps a `didSave` from
/// killing a server that is still loading (see `LspState::did_save`).
fn client_capabilities() -> serde_json::Value {
    serde_json::json!({
        // File-operation capability, for `workspace/willRenameFiles`:
        // renaming a `.rs` file in the project tree asks rust-analyzer
        // what `mod` / `use` / path references have to change, and the
        // IDE applies those edits before doing the move itself.
        //
        // Advertising this ALSO changes `textDocument/rename`: when a
        // symbol rename would move a file (renaming a `mod` name), RA
        // strips the text edits and returns only the file-move op,
        // expecting us to ask again through willRenameFiles. That path
        // already did nothing here - without a `resourceOperations`
        // capability RA fails the whole request - so nothing regresses,
        // but it is why `resourceOperations` is deliberately NOT
        // advertised: the two mechanisms cannot both be used.
        "workspace": {
            "fileOperations": { "willRename": true },
        },
        "textDocument": {
            "synchronization": {
                "dynamicRegistration": false,
                "willSave": false,
                // We send didSave on flush so RA's `checkOnSave` flycheck
                // (cargo check) re-runs and refreshes stale flycheck
                // diagnostics — otherwise it only ran once at startup.
                "didSave": true,
            },
            "publishDiagnostics": {
                "relatedInformation": false,
                "versionSupport":     false,
            },
            "completion": {
                "completionItem": {
                    // Snippet completions (`foo(${1:a}, ${2:b})$0`) let
                    // accepting a function insert the full call with
                    // parameters; the editor flattens them via
                    // `editor_panel::snippet::expand` and selects the
                    // first argument.
                    "snippetSupport":      true,
                    "documentationFormat": ["plaintext", "markdown"],
                    "labelDetailsSupport": true,
                },
                "completionItemKind": {
                    "valueSet": [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25]
                },
                "contextSupport": true,
            },
            // Ask RA for the rich, nested `DocumentSymbol[]` shape (with a
            // separate `range` for the whole item + `selectionRange` for just
            // the name, and `children` for nested items) instead of the older
            // flat `SymbolInformation[]` — used to find every fn/struct/enum/
            // const/… so we can fade unused ones and offer a references list.
            "documentSymbol": {
                "hierarchicalDocumentSymbolSupport": true,
            },
            // Ctrl+Enter assists / quick-fixes. `codeActionLiteralSupport`
            // → RA may return `CodeAction` objects (with edits) not just
            // `Command`s; `resolveSupport` → RA may defer the `edit` and
            // we fetch it via `codeAction/resolve`.
            "codeAction": {
                "codeActionLiteralSupport": {
                    "codeActionKind": {
                        "valueSet": [
                            "", "quickfix", "refactor", "refactor.extract",
                            "refactor.inline", "refactor.rewrite", "source"
                        ]
                    }
                },
                "resolveSupport": { "properties": ["edit"] },
            },
            // Inferred-type inlay hints — drawn as ghost text on the
            // cursor's line; Tab inserts the type. We deliberately do
            // NOT advertise `resolveSupport` here, so rust-analyzer fills
            // each hint's `textEdits` eagerly (accepting needs no extra
            // `inlayHint/resolve` round-trip).
            "inlayHint": {
                "dynamicRegistration": false,
            },
        },
        "window": { "workDoneProgress": true },
        // `experimental/serverStatus`: its `quiescent` is the one signal
        // that the workspace has really finished loading. `$/progress`
        // ends cannot say it - the first one is merely "Fetching" - and
        // a `didSave` sent before it kills the server (see `did_save`).
        "experimental": { "serverStatusNotification": true },
    })
}

/// [`initialization_options`] plus `linkedProjects` when there are any. With
/// none it is byte for byte the plain object, and auto-discovery behaves as it
/// always has.
fn initialization_options_with(linked: Option<&[String]>) -> serde_json::Value {
    let mut opts = initialization_options();
    if let Some(projects) = linked {
        opts["linkedProjects"] = serde_json::json!(projects);
    }
    opts
}

/// [`initialization_options_with`] for the project in `workspace_dir`, as the
/// launch computes it (see [`linked_projects`]).
#[cfg(test)]
fn initialization_options_for(workspace_dir: &Path) -> serde_json::Value {
    initialization_options_with(linked_projects(workspace_dir).as_deref())
}

/// A path as cargo compares it: components, `\` a separator on Windows, `.`
/// dropped. `..` is kept as a LITERAL component, because cargo's
/// `Path::starts_with` does not resolve it either - so `x/../mylib` excludes
/// nothing, exactly as in cargo.
pub(crate) fn cargo_components(path: &str) -> Vec<String> {
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    path.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_owned)
        .collect()
}

/// Whether cargo loads the DETACHED library in `dir` on its own - and so
/// whether rust-analyzer can index it as a linked project, which is what gives
/// the Structure diagram its call paths inside that library.
///
/// A package inside a workspace's folder that the workspace does not list is
/// refused outright ("current package believes it's in a workspace when it's
/// not"), and that is the state a detached library is left in. Cargo accepts it
/// again when the workspace EXCLUDES it, or when the library's own manifest has
/// a `[workspace]` table. Measured on a real project: with neither, rust-analyzer
/// reported "Failed to load workspaces" and found no reference in the library;
/// with `exclude`, 249 references across its 7 files - and the firmware's own
/// analysis was untouched either way.
///
/// `exclude` is matched the way cargo matches it - the library's manifest path
/// starts with the root joined with the entry, compared by component - so
/// `'.\mylib'` and `"."` count, and a padded `" mylib"` does not (cargo refuses
/// it, measured).
pub fn cargo_can_load_detached(root_manifest: &str, dir: &str, lib_manifest: &str) -> bool {
    // No `[workspace]` in the root at all - the IDE's own templates - and the
    // root is no workspace cargo could put the library in: it loads on its own.
    if !crate::publish::has_workspace_table(root_manifest) {
        return true;
    }
    let lib = cargo_components(dir);
    crate::publish::workspace_array(root_manifest, "exclude")
        .iter()
        .any(|e| lib.starts_with(&cargo_components(e)))
        || crate::publish::has_workspace_table(lib_manifest)
}

/// Whether the RUNNING rust-analyzer was started with the detached library in
/// `dir` among its `linkedProjects` - the only thing that decides whether calls
/// into it are traced right now. `linked` is `LspState::linked_projects`.
pub fn ra_links_detached(linked: &[String], dir: &str) -> bool {
    let want = format!("/{}/Cargo.toml", cargo_components(dir).join("/"));
    linked
        .iter()
        .skip(1) // the firmware's own manifest
        .any(|p| p.replace('\\', "/").ends_with(&want))
}

/// What a launch would put in `LspState::linked_projects` right now - the
/// set `AppIde::recheck_linked_projects` compares against `running`, the one
/// the running analyzer was started with.
///
/// A manifest that is empty or not TOML - a half-typed file - decides
/// nothing. Read as "no `[workspace]`", a broken root ADDED a library cargo
/// refuses, so a typo restarted rust-analyzer into one that cannot load the
/// root at all, and fixing the typo restarted it again. So an unreadable root
/// gives `None` ("cannot tell"), and an unreadable library manifest keeps
/// the linkage `running` has for that one library: a folder whose manifest
/// STAYS unreadable - a template, a file New File left as `// New file` -
/// must not hide every other library's change. A library whose manifest is
/// GONE is an answer: the library left.
pub fn linked_projects_now(workspace_dir: &Path, running: &[String]) -> Option<Vec<String>> {
    let root = std::fs::read_to_string(workspace_dir.join("Cargo.toml")).ok()?;
    let readable = |m: &str| !m.trim().is_empty() && crate::publish::manifest_parses(m);
    if !readable(&root) {
        return None;
    }
    let libs = detached_candidates(workspace_dir, &root)
        .into_iter()
        .filter(|(name, _, text)| match text.as_deref() {
            None => false,
            Some(t) if readable(t) => cargo_can_load_detached(&root, name, t),
            Some(_) => ra_links_detached(running, name),
        })
        .map(|(_, manifest, _)| manifest.display().to_string())
        .collect();
    Some(with_firmware_first(workspace_dir, libs).unwrap_or_default())
}

/// The `linkedProjects` rust-analyzer should load for the project in
/// `workspace_dir`: the firmware's manifest FIRST, then every detached library
/// cargo can load. `None` when there is no such library.
///
/// Read from disk at launch, because the workspace copy is what RA loads -
/// which is why `write_project` must not prune a project's OWN detached library
/// from that copy (it once did, so this found nothing). A library cargo would
/// refuse is left out on purpose: handing it to RA only buys a "Failed to load
/// workspaces" message and not one reference.
fn linked_projects(workspace_dir: &Path) -> Option<Vec<String>> {
    let root = std::fs::read_to_string(workspace_dir.join("Cargo.toml")).ok()?;
    let libs = detached_candidates(workspace_dir, &root)
        .into_iter()
        .filter(|(name, _, text)| {
            text.as_deref()
                .is_some_and(|t| cargo_can_load_detached(&root, name, t))
        })
        .map(|(_, manifest, _)| manifest.display().to_string())
        .collect();
    with_firmware_first(workspace_dir, libs)
}

/// A folder of the workspace that may hold a detached library: its name, its
/// manifest's path, and that manifest's text (`None`: it has none).
type DetachedCandidate = (String, std::path::PathBuf, Option<String>);

/// Every folder of the workspace that may hold a detached library - not
/// hidden, not `src` or `target`, and not one the root builds.
fn detached_candidates(workspace_dir: &Path, root: &str) -> Vec<DetachedCandidate> {
    let built = crate::project_tree::extract_crate::built_lib_dirs(root);
    let Ok(entries) = std::fs::read_dir(workspace_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "src" || name == "target" || built.contains(&name) {
                return None;
            }
            let manifest = entry.path().join("Cargo.toml");
            let text = std::fs::read_to_string(&manifest).ok();
            Some((name, manifest, text))
        })
        .collect()
}

/// The firmware's manifest, then the libraries' sorted - `None` when there
/// is no library.
fn with_firmware_first(workspace_dir: &Path, mut libs: Vec<String>) -> Option<Vec<String>> {
    if libs.is_empty() {
        return None;
    }
    libs.sort();
    let mut all = vec![workspace_dir.join("Cargo.toml").display().to_string()];
    all.extend(libs);
    Some(all)
}

/// The `initializationOptions` handed to rust-analyzer at startup.
///
/// Its own function, and guarded by tests, because this object is where a
/// setting quietly stops being true. `procMacro.enable` sat at `false` for
/// months behind a comment describing a workspace behaviour that had since been
/// fixed, and the cost was invisible from the code: a crate whose API is
/// macro-generated simply had no types, and every editor command over such a
/// value answered "nothing here".
fn initialization_options() -> serde_json::Value {
    serde_json::json!({

                // cargo-check-on-save is ENABLED so real compiler errors
                // (E0425 "cannot find value", type mismatches, …) show up inline
                // after a Project Save. RA's *native* pass alone does NOT
                // reliably publish these for nested user files, so without
                // flycheck the editor showed no inline errors at all.
                //
                // The save-slowness this once caused was NOT flycheck itself but
                // (a) a leaked serial-reader thread and (b) deleting Cargo.lock on
                // every save, which forced a full dependency re-resolve before
                // each check — both since fixed (Cargo.lock is now kept; see
                // `AppIde::reset_workspace_lock`), so flycheck is a fast
                // *incremental* check that runs asynchronously in RA (it never
                // blocks the app's save). Triggered by the `did_save` sent from
                // `AppIde::flush_lsp_to_workspace`.
                "checkOnSave":  true,

                // Proc-macro expansion is ENABLED.
                //
                // It was off for a long time, justified by this: our workspace
                // deleted `Cargo.lock` on every project write, which changes the
                // dependency resolution hash, so the expander DLL RA had cached
                // (e.g. `esp_hal_procmacros-<hash>.dll`) was gone and startup
                // logged "Cannot create expander for <dll>".
                //
                // That behaviour no longer exists. `AppIde::reset_workspace_lock`
                // runs ONLY on project open and on a chip/toolchain change; saves
                // keep the lock, so the hash is stable and the DLL survives.
                //
                // The old comment also claimed the experience was "not materially
                // affected" with expansion off. That was wrong, and it took a user
                // five rounds of questions to disprove: any crate whose API is
                // GENERATED by an attribute macro is invisible to rust-analyzer
                // without expansion. `ssd1306` builds its entire async surface
                // (`Ssd1306Async`, `BufferedGraphicsModeAsync`) that way, so such
                // a value's type came out `{unknown}` — no inlay hint, and then
                // every later line touching it answered "no definition" and "no
                // action", while the hand-written blocking API on the same screen
                // worked perfectly.
                //
                // If the DLL really is missing (a project never built), RA emits
                // `unresolved-proc-macro`, already suppressed below.
                "procMacro": { "enable": true },

                "diagnostics": {
                    "enable": true,
                    // Suppress the "proc-macro expansion is disabled" pseudo-error
                    // that RA emits for every attribute macro when expansion is off.
                    // All real compiler errors (type mismatches, borrow errors, …)
                    // are still reported through cargo-check diagnostics.
                    "disabled": ["unresolved-proc-macro"],
                },

                // Inlay hints. Sending NOTHING here left RA on its own defaults,
                // whose `maxLength` is 25 — which is why a real embedded type came
                // back as `Ssd1306<I2CInterface<BlockingI2c<…, …>>, …, …>`, with
                // the one thing the reader wanted to know inside the elision.
                //
                // 400, not 120 and not 25: the fitting is OURS now.
                // `diagnostics_overlay::shorten_type` collapses the generic
                // arguments against the pixels actually left on the line, and
                // the untouched label goes in the hover tooltip. A cap here can
                // only throw away information before either of those sees it —
                // rust-analyzer is guessing at a window width it cannot know.
                // The number is a bound on the pathological case, not a layout
                // decision.
                //
                // Chaining hints OFF: only one hint per line is ever drawn, and RA
                // reports chaining hints with the same LSP kind as type hints, so
                // they compete for that slot with the binding's own type.
                "inlayHints": {
                    "maxLength": 400,
                    "typeHints": { "enable": true },
                    "chainingHints": { "enable": false },
                    "closureReturnTypeHints": { "enable": "never" },
                },

                // Ask RA to include full documentation text in completion
                // responses rather than returning only a label.
                "completion": {
                    "fullFunctionSignatures": { "enable": true },
                },

                // Let RA read the target from .cargo/config.toml.
                // For ESP32-C3 this is riscv32imc-unknown-none-elf, which ensures
                // that cfg(target_arch = "riscv32") items in esp-hal are visible.
                //
                // `targetDir: true` → RA runs its cargo (flycheck checkOnSave +
                // build-script probing) in its OWN `target/rust-analyzer/`
                // directory instead of the shared `target/`. Without this, every
                // Save's flycheck held the cargo target-dir file lock, so the
                // Build / Clippy / Flash cargo invocations silently BLOCKED
                // waiting for it — a main driver of the "everything gets slower
                // after a save" degradation. Costs some extra disk space.
                "cargo": {
                    "noDefaultFeatures": false,
                    "targetDir": true,
                }
    })
}

/// Parse a `textDocument/inlayHint` result (`InlayHint[]`). Only **type** hints
/// (kind 1, or unspecified) are kept — parameter-name hints (kind 2) are
/// dropped. `label` may be a plain string or an `InlayHintLabelPart[]` (each
/// part's `value` concatenated). `rel` is the file the `textEdits` apply to.
fn parse_inlay_hints(result: &serde_json::Value, rel: &str) -> Vec<InlayHint> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|h| {
            // kind: 1 = Type, 2 = Parameter. Absent → treat as a type hint.
            if h["kind"].as_u64() == Some(2) {
                return None;
            }
            let pos = &h["position"];
            let line = pos["line"].as_u64()? as u32;
            let character = pos["character"].as_u64()? as u32;
            let label = inlay_label_text(&h["label"]);
            if label.trim().is_empty() {
                return None;
            }
            let text_edits = h["textEdits"]
                .as_array()
                .map(|es| es.iter().filter_map(|e| parse_text_edit(e, rel)).collect())
                .unwrap_or_default();
            Some(InlayHint {
                line,
                character,
                label,
                text_edits,
            })
        })
        .collect()
}

/// Flatten an inlay-hint `label` — a `String` or an `InlayHintLabelPart[]`
/// (whose `value` fields are concatenated).
fn inlay_label_text(label: &serde_json::Value) -> String {
    if let Some(s) = label.as_str() {
        return s.to_owned();
    }
    if let Some(parts) = label.as_array() {
        return parts
            .iter()
            .filter_map(|p| p["value"].as_str())
            .collect::<String>();
    }
    String::new()
}

/// Parse a `textDocument/definition` or `…/implementation` result
/// (Location / Location[] / LocationLink[]) into EVERY target it names.
///
/// This used to return the first location only, which is right for
/// `definition` — a Rust symbol has one — and silently wrong for
/// `implementation`, whose whole point is to answer with the N impls. The
/// array is the answer, not a wrapper around it.
fn parse_definition_list(result: &serde_json::Value) -> Vec<DefinitionLoc> {
    if let Some(arr) = result.as_array() {
        arr.iter().filter_map(parse_one_location).collect()
    } else if result.is_object() {
        parse_one_location(result).into_iter().collect()
    } else {
        // JSON null — "I have no answer", not a malformed one.
        Vec::new()
    }
}

/// One `Location { uri, range }` or `LocationLink { targetUri, … }`.
fn parse_one_location(loc: &serde_json::Value) -> Option<DefinitionLoc> {
    let (uri, range) = if let Some(u) = loc["uri"].as_str() {
        (u, &loc["range"])
    } else {
        let u = loc["targetUri"].as_str()?;
        // `targetSelectionRange` is the NAME; `targetRange` the whole item.
        // Prefer the name so the jump lands on the signature line.
        let r = if loc["targetSelectionRange"].is_object() {
            &loc["targetSelectionRange"]
        } else {
            &loc["targetRange"]
        };
        (u, r)
    };
    Some(DefinitionLoc {
        path: uri_to_path(uri),
        uri: uri.to_owned(),
        line: range["start"]["line"].as_u64()? as u32,
        character: range["start"]["character"].as_u64().unwrap_or(0) as u32,
    })
}

/// Recursively flatten a `textDocument/documentSymbol` response into every
/// trackable named item (fn/struct/enum/const/static/trait/method/field/…,
/// see [`is_trackable_symbol_kind`]), descending into `children` so items
/// nested in a `mod`/`impl`/`trait` body are covered too. Handles both the
/// modern hierarchical `DocumentSymbol[]` shape (has `range` + `selectionRange`
/// + optional `children`) and the older flat `SymbolInformation[]` shape (just
/// `location.range`, used as both spans) some servers fall back to.
fn parse_document_symbols(result: &serde_json::Value) -> Vec<SymbolInfo> {
    /// rust-analyzer names trait-impl block symbols `impl Trait for Type`
    /// (inherent impls are just `impl Type` — no ` for `).
    fn is_trait_impl_symbol(name: &str) -> bool {
        name.starts_with("impl") && name.contains(" for ")
    }

    fn walk(node: &serde_json::Value, out: &mut Vec<SymbolInfo>, in_trait_impl: bool) {
        let name = node["name"].as_str().unwrap_or("").to_owned();
        let kind = node["kind"].as_u64().unwrap_or(0) as u8;
        let child_in_trait_impl = in_trait_impl || is_trait_impl_symbol(&name);

        if let (Some(range), Some(sel)) = (node.get("range"), node.get("selectionRange")) {
            if !name.is_empty() && is_trackable_symbol_kind(kind) {
                if let Some(info) = symbol_from_ranges(name, kind, range, sel, in_trait_impl) {
                    out.push(info);
                }
            }
            if let Some(children) = node["children"].as_array() {
                for c in children {
                    walk(c, out, child_in_trait_impl);
                }
            }
        } else if let Some(loc) = node.get("location") {
            // Flat SymbolInformation — no separate selection span or children.
            if !name.is_empty() && is_trackable_symbol_kind(kind) {
                let r = &loc["range"];
                if let Some(info) = symbol_from_ranges(name, kind, r, r, in_trait_impl) {
                    out.push(info);
                }
            }
        }
    }

    fn symbol_from_ranges(
        name: String,
        kind: u8,
        range: &serde_json::Value,
        sel: &serde_json::Value,
        in_trait_impl: bool,
    ) -> Option<SymbolInfo> {
        Some(SymbolInfo {
            name,
            kind,
            start_line: range["start"]["line"].as_u64()? as u32,
            start_char: range["start"]["character"].as_u64()? as u32,
            end_line: range["end"]["line"].as_u64()? as u32,
            end_char: range["end"]["character"].as_u64()? as u32,
            sel_line: sel["start"]["line"].as_u64()? as u32,
            sel_char: sel["start"]["character"].as_u64()? as u32,
            in_trait_impl,
        })
    }

    let mut out = Vec::new();
    if let Some(arr) = result.as_array() {
        for node in arr {
            walk(node, &mut out, false);
        }
    }
    out
}

/// Parse a `textDocument/references` result (`Location[]`) into usage sites.
fn parse_references(result: &serde_json::Value) -> Vec<ReferenceLoc> {
    result
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|loc| {
                    let uri = loc["uri"].as_str()?;
                    let r = &loc["range"];
                    Some(ReferenceLoc {
                        path: uri_to_path(uri),
                        line: r["start"]["line"].as_u64()? as u32,
                        character: r["start"]["character"].as_u64().unwrap_or(0) as u32,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Decode a `file://` URI to a filesystem path (with minimal `%XX` decoding).
fn uri_to_path(uri: &str) -> String {
    let mut p = uri.strip_prefix("file://").unwrap_or(uri).to_owned();
    // Percent-decode common sequences (spaces etc.) without a full URL crate.
    if p.contains('%') {
        p = percent_decode(&p);
    }
    #[cfg(windows)]
    {
        // file:///C:/foo → /C:/foo → C:/foo, then backslashes.
        let stripped = p.strip_prefix('/').unwrap_or(&p);
        stripped.replace('/', "\\")
    }
    #[cfg(not(windows))]
    {
        p
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_text_edit(e: &serde_json::Value, rel: &str) -> Option<RenameEdit> {
    let s = &e["range"]["start"];
    let en = &e["range"]["end"];
    Some(RenameEdit {
        rel_path: rel.to_owned(),
        start_line: s["line"].as_u64()? as u32,
        start_char: s["character"].as_u64()? as u32,
        end_line: en["line"].as_u64()? as u32,
        end_char: en["character"].as_u64()? as u32,
        new_text: e["newText"].as_str().unwrap_or("").to_owned(),
    })
}

fn parse_completion_item(v: &serde_json::Value) -> Option<CompletionItem> {
    let label = v["label"].as_str()?.to_owned();
    let kind = v["kind"].as_u64().unwrap_or(6) as u8;

    // `detail` is the primary type annotation (e.g. "-> bool", "fn(...)").
    // rust-analyzer may put this in `detail` directly, or in
    // `labelDetails.detail` / `labelDetails.description`.
    let detail = {
        let from_detail = v["detail"].as_str().unwrap_or("").to_owned();
        if !from_detail.is_empty() {
            from_detail
        } else {
            // labelDetails.detail is typically the short type suffix (e.g. "(…) -> T")
            // labelDetails.description is typically the full qualified path
            let ld_detail = v["labelDetails"]["detail"].as_str().unwrap_or("");
            let ld_desc = v["labelDetails"]["description"].as_str().unwrap_or("");
            match (ld_detail.is_empty(), ld_desc.is_empty()) {
                (false, false) => format!("{ld_detail}  {ld_desc}"),
                (false, true) => ld_detail.to_owned(),
                (true, false) => ld_desc.to_owned(),
                (true, true) => String::new(),
            }
        }
    };

    // rust-analyzer delivers the replacement text through `textEdit.newText`
    // (both the plain-`TextEdit` and `InsertReplaceEdit` shapes carry it);
    // a bare `insertText` is the exception. Falling back to `label` used to be
    // harmless when labels were plain names, but with `snippetSupport` on RA
    // labels callables as `name(…)` — inserting THAT literally is a bug, so
    // the fallback chain must prefer the real edit text.
    let insert_text = v["textEdit"]["newText"]
        .as_str()
        .or_else(|| v["insertText"].as_str())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| label.clone());

    // LSP InsertTextFormat: 1 = PlainText (default when absent), 2 = Snippet.
    let insert_is_snippet = v["insertTextFormat"].as_u64() == Some(2);

    // `documentation` can be a plain string or { kind: "markdown", value: "..." }.
    // Kept as raw markdown: the completion detail panel parses it itself
    // (`editor_panel::doc_md`) so it can draw code examples in monospace.
    // Flattening the ``` fences here used to lose that distinction before the
    // UI ever saw it — and with it the only way to tell a rustdoc hidden line
    // (`# use std::fmt;` inside a fence) from a heading.
    let documentation = {
        let doc = &v["documentation"];
        if let Some(s) = doc.as_str() {
            s.trim().to_owned()
        } else if let Some(s) = doc["value"].as_str() {
            s.trim().to_owned()
        } else {
            String::new()
        }
    };

    Some(CompletionItem {
        label,
        kind,
        detail,
        insert_text,
        insert_is_snippet,
        documentation,
    })
}

#[cfg(test)]
mod inlay_hint_tests {
    use super::*;

    #[test]
    fn parses_type_hint_with_string_label_and_text_edit() {
        let result = serde_json::json!([
            {
                "position": { "line": 4, "character": 9 },
                "kind": 1,
                "label": ": u32",
                "textEdits": [
                    {
                        "range": {
                            "start": { "line": 4, "character": 9 },
                            "end":   { "line": 4, "character": 9 }
                        },
                        "newText": ": u32"
                    }
                ]
            }
        ]);
        let hints = parse_inlay_hints(&result, "src/main.rs");
        assert_eq!(hints.len(), 1);
        let h = &hints[0];
        assert_eq!((h.line, h.character), (4, 9));
        assert_eq!(h.label, ": u32");
        assert_eq!(h.text_edits.len(), 1);
        assert_eq!(h.text_edits[0].new_text, ": u32");
        assert_eq!(h.text_edits[0].rel_path, "src/main.rs");
    }

    #[test]
    fn concatenates_labelpart_arrays() {
        let result = serde_json::json!([
            {
                "position": { "line": 0, "character": 5 },
                "label": [ { "value": ": " }, { "value": "Vec<u8>" } ]
            }
        ]);
        let hints = parse_inlay_hints(&result, "src/main.rs");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].label, ": Vec<u8>");
        assert!(hints[0].text_edits.is_empty(), "no textEdits provided");
    }

    #[test]
    fn drops_parameter_name_hints() {
        // kind 2 = parameter-name hint; we only keep type hints.
        let result = serde_json::json!([
            { "position": { "line": 1, "character": 3 }, "kind": 2, "label": "count:" },
            { "position": { "line": 1, "character": 8 }, "kind": 1, "label": ": i64" }
        ]);
        let hints = parse_inlay_hints(&result, "src/lib.rs");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].label, ": i64");
    }

    #[test]
    fn empty_or_non_array_result_yields_nothing() {
        assert!(parse_inlay_hints(&serde_json::json!(null), "src/main.rs").is_empty());
        assert!(parse_inlay_hints(&serde_json::json!([]), "src/main.rs").is_empty());
    }
}

#[cfg(test)]
mod diagnostic_headline_tests {
    use super::*;

    fn diag(message: &str) -> LspDiagnostic {
        LspDiagnostic {
            severity: DiagSeverity::Error,
            message: message.to_owned(),
            line: 1,
            col: 1,
            end_line: 1,
            end_col: 1,
            code: None,
            source: String::new(),
        }
    }

    #[test]
    fn headline_is_first_line_only() {
        let d = diag("mismatched types\nexpected `u8`, found `u16`");
        assert_eq!(d.headline(), "mismatched types");
        assert!(d.has_more_lines());
    }

    #[test]
    fn single_line_has_no_more() {
        let d = diag("unused variable: `x`");
        assert_eq!(d.headline(), "unused variable: `x`");
        assert!(!d.has_more_lines());
    }

    #[test]
    fn trailing_blank_lines_do_not_count() {
        let d = diag("cannot find value `foo`\n\n  ");
        assert_eq!(d.headline(), "cannot find value `foo`");
        assert!(!d.has_more_lines(), "blank trailing lines aren't 'more'");
    }

    fn diag_with_code(code: Option<&str>) -> LspDiagnostic {
        let mut d = diag("x");
        d.code = code.map(String::from);
        d
    }

    #[test]
    fn numbered_compiler_codes_are_rustc_error_codes() {
        assert!(diag_with_code(Some("E0425")).is_rustc_error_code());
        assert!(diag_with_code(Some("E0308")).is_rustc_error_code());
    }

    #[test]
    fn named_lints_are_not_rustc_error_codes() {
        // These require an actual cargo-check/clippy pass (confirmed empirically
        // — see `is_rustc_error_code`'s doc comment) and must stay gated by
        // `flycheck_stale`, unlike numbered hard errors.
        assert!(!diag_with_code(Some("unused_variables")).is_rustc_error_code());
        assert!(!diag_with_code(Some("dead_code")).is_rustc_error_code());
        assert!(!diag_with_code(Some("clippy::needless_return")).is_rustc_error_code());
    }

    #[test]
    fn missing_or_malformed_code_is_not_a_rustc_error_code() {
        assert!(!diag_with_code(None).is_rustc_error_code());
        assert!(
            !diag_with_code(Some("E")).is_rustc_error_code(),
            "no digits after E"
        );
        assert!(
            !diag_with_code(Some("E12a4")).is_rustc_error_code(),
            "non-digit in the code"
        );
    }
}

#[cfg(test)]
mod ra_sweep_tests {
    use super::{parse_pid_lines, ps_comm_is_rust_analyzer, tasklist_image_name};

    /// The unix half of the PID-reuse guard. Tested on every host (the parser is
    /// portable even though only the unixes run `ps`), because getting it wrong
    /// means SIGKILL-ing whatever process inherited the recycled pid.
    #[test]
    fn ps_comm_recognises_rust_analyzer_on_both_unixes() {
        // Linux: bare command name (comm is truncated to 15 chars; this fits).
        assert!(ps_comm_is_rust_analyzer("rust-analyzer\n"));
        // macOS: full executable path.
        assert!(ps_comm_is_rust_analyzer(
            "/Users/me/.cargo/bin/rust-analyzer\n"
        ));
        // The proc-macro server counts too — same tree, same sweep.
        assert!(ps_comm_is_rust_analyzer("rust-analyzer-proc-macro-srv\n"));
    }

    #[test]
    fn ps_comm_rejects_anything_else() {
        // No such process → `ps -p <pid> -o comm=` prints nothing at all. This
        // is the case that MUST answer false: a dead pid may have been reused.
        assert!(!ps_comm_is_rust_analyzer(""));
        assert!(!ps_comm_is_rust_analyzer("\n  \n"));
        assert!(!ps_comm_is_rust_analyzer("cargo\n"));
        assert!(!ps_comm_is_rust_analyzer("/usr/bin/firefox\n"));
        // A path CONTAINING the name but not being it — only the last component
        // is the program.
        assert!(!ps_comm_is_rust_analyzer("/opt/rust-analyzer/bin/helper\n"));
    }

    #[test]
    fn pid_lines_parse_and_skip_junk() {
        assert_eq!(parse_pid_lines("16340\n22308\n"), vec![16340, 22308]);
        assert_eq!(parse_pid_lines("  123 \n\nnot-a-pid\n77"), vec![123, 77]);
        assert!(parse_pid_lines("").is_empty());
    }

    #[test]
    fn tasklist_csv_yields_image_name() {
        let line = r#""rust-analyzer.exe","16340","Console","1","540,120 K""#;
        assert_eq!(
            tasklist_image_name(line).as_deref(),
            Some("rust-analyzer.exe")
        );
    }

    #[test]
    fn tasklist_info_line_is_rejected() {
        // `tasklist /FI "PID eq X"` prints this when the pid no longer exists —
        // it must NOT look like a process (or a dead pid would get "killed",
        // i.e. taskkill run against a possibly reused pid).
        let line = "INFO: No tasks are running which match the specified criteria.";
        assert_eq!(tasklist_image_name(line), None);
        assert_eq!(tasklist_image_name(""), None);
    }
}

#[cfg(test)]
mod definition_list_tests {
    use super::parse_definition_list;

    fn loc(file: &str, line: u64) -> serde_json::Value {
        serde_json::json!({
            "uri": format!("file:///work/{file}"),
            "range": { "start": { "line": line, "character": 4 },
                       "end":   { "line": line, "character": 9 } },
        })
    }

    /// THE regression. `textDocument/implementation` answers with one location
    /// per impl; this parser used to keep `.first()` and drop the rest, so a
    /// trait implemented in three files could only ever navigate to one of them
    /// — and always the same one, since the order comes from rust-analyzer's
    /// crate traversal and not from the caret.
    #[test]
    fn every_implementation_survives_the_parse() {
        let result = serde_json::json!([
            loc("report_normal_mode.rs", 40),
            loc("report_debug_mode.rs", 55),
            loc("report_raw_mode.rs", 12),
        ]);
        let got = parse_definition_list(&result);
        assert_eq!(got.len(), 3, "no implementation may be dropped: {got:?}");
        assert_eq!(got[1].line, 55);
        assert!(got[1].path.ends_with("report_debug_mode.rs"));
    }

    /// A single `Location` object — the shape `definition` usually answers with
    /// — is one target, not zero.
    #[test]
    fn a_bare_location_object_is_one_target() {
        let got = parse_definition_list(&loc("parse_result.rs", 7));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].line, 7);
    }

    /// A chained F12 from the Definition tab sends this URI back as it came.
    /// Decoded and rebuilt, it would lose the analyzer's own spelling (here the
    /// lower-case, percent-encoded drive) and could name a file its VFS does not
    /// know by that name.
    #[test]
    fn the_analyzer_uri_is_kept_verbatim() {
        let raw = "file:///c%3A/Users/x/.cargo/registry/src/heapless-0.8.0/src/vec.rs";
        let got = parse_definition_list(&serde_json::json!({
            "uri": raw,
            "range": { "start": { "line": 3, "character": 1 },
                       "end":   { "line": 3, "character": 4 } },
        }));
        assert_eq!(got[0].uri, raw);
        assert!(got[0].path.ends_with("vec.rs"));
    }

    /// `LocationLink` carries the name span separately; the jump must land on
    /// the signature, not on the first line of a 200-line item.
    #[test]
    fn a_location_link_jumps_to_the_name_not_the_whole_item() {
        let result = serde_json::json!([{
            "targetUri": "file:///work/a.rs",
            "targetRange":          { "start": { "line": 10, "character": 0 },
                                      "end":   { "line": 90, "character": 1 } },
            "targetSelectionRange": { "start": { "line": 12, "character": 7 },
                                      "end":   { "line": 12, "character": 11 } },
        }]);
        let got = parse_definition_list(&result);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].line, 12, "targetSelectionRange is the name");
        assert_eq!(got[0].character, 7);
    }

    /// A link WITHOUT a selection range still resolves, rather than vanishing.
    #[test]
    fn a_location_link_without_a_selection_range_falls_back() {
        let result = serde_json::json!([{
            "targetUri": "file:///work/a.rs",
            "targetRange": { "start": { "line": 10, "character": 0 },
                             "end":   { "line": 90, "character": 1 } },
        }]);
        assert_eq!(parse_definition_list(&result)[0].line, 10);
    }

    /// `null` is rust-analyzer saying "I have no answer" — an empty list, and
    /// the caller reports it. An entry it cannot decode must not poison the
    /// ones it can.
    #[test]
    fn null_is_empty_and_junk_does_not_take_its_neighbours_down() {
        assert!(parse_definition_list(&serde_json::Value::Null).is_empty());
        let mixed = serde_json::json!([
            { "nonsense": true },
            loc("good.rs", 3),
        ]);
        let got = parse_definition_list(&mixed);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].line, 3);
    }
}

#[cfg(test)]
mod document_symbol_tests {
    use super::parse_document_symbols;

    fn range(l0: u64, c0: u64, l1: u64, c1: u64) -> serde_json::Value {
        serde_json::json!({ "start": { "line": l0, "character": c0 },
                            "end":   { "line": l1, "character": c1 } })
    }

    fn sym(name: &str, kind: u64, children: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "kind": kind,
            "range": range(0, 0, 9, 0),
            "selectionRange": range(0, 3, 0, 8),
            "children": children,
        })
    }

    /// Members of `impl Trait for Type` blocks are flagged (they must never be
    /// faded as unused — generic dispatch references bind to the trait), while
    /// trait declarations and inherent-impl methods are not.
    #[test]
    fn trait_impl_members_are_flagged() {
        let result = serde_json::json!([
            // The user's exact shape: trait in one file's symbols…
            sym(
                "ParserResult",
                11,
                serde_json::json!([sym("new_parser", 12, serde_json::json!([])),])
            ),
            // …its implementation (impl block = kind 19 Object, not tracked
            // itself; its children are).
            sym(
                "impl ParserResult<PAYLOAD_LEN, HAS_CMD_ID, RESERVED_LEN, HmmdFrame> for HmmdFrame",
                19,
                serde_json::json!([
                    sym("new_parser", 12, serde_json::json!([])),
                    sym("decode", 12, serde_json::json!([])),
                ]),
            ),
            // Inherent impl — members keep normal unused-fading behaviour.
            sym(
                "impl HmmdFrame",
                19,
                serde_json::json!([sym("helper", 12, serde_json::json!([])),])
            ),
        ]);

        let syms = parse_document_symbols(&result);
        let flag = |name: &str, expect: bool, nth: usize| {
            let s = syms.iter().filter(|s| s.name == name).nth(nth).unwrap();
            assert_eq!(s.in_trait_impl, expect, "{name} #{nth}");
        };
        flag("ParserResult", false, 0); // the trait itself
        flag("new_parser", false, 0); // trait's own declaration
        flag("new_parser", true, 1); // impl-for member
        flag("decode", true, 0); // impl-for member
        flag("helper", false, 0); // inherent impl member
    }
}

#[cfg(test)]
mod code_action_merge_tests {
    use super::*;

    fn action(title: &str) -> serde_json::Value {
        serde_json::json!({ "title": title, "data": { "id": title } })
    }

    fn reply(state: &Arc<Mutex<LspState>>, tx: &mpsc::Sender<String>, msg: serde_json::Value) {
        let generation = state.lock().unwrap().generation;
        handle_incoming(
            msg,
            state,
            &eframe::egui::Context::default(),
            tx,
            "file:///w",
            generation,
        );
    }

    fn titles(actions: &[CodeAction]) -> Vec<&str> {
        actions.iter().map(|a| a.title.as_str()).collect()
    }

    /// Two positions asked, answers arriving in reverse order: nothing is
    /// published until both are in, then in REQUEST order, duplicates once.
    #[test]
    fn answers_merge_in_request_order_once_all_are_in() {
        let (tx, _rx) = mpsc::channel();
        let mut s = LspState {
            sender: Some(tx.clone()),
            ..Default::default()
        };
        s.request_code_actions("src/main.rs", &[(3, 20, 3, 20), (3, 8, 3, 8)]);
        let ids: Vec<u64> = s.code_action_pending.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(
            s.code_action_for,
            Some(("src/main.rs".to_owned(), 4)),
            "the caret line"
        );
        let state = Arc::new(Mutex::new(s));

        let second = serde_json::json!({ "jsonrpc": "2.0", "id": ids[1],
            "result": [action("Add explicit type"), action("Inline variable"), action("Shared")] });
        reply(&state, &tx, second);
        assert!(
            state.lock().unwrap().take_code_actions_result().is_none(),
            "half an answer is not published"
        );

        let first = serde_json::json!({ "jsonrpc": "2.0", "id": ids[0],
            "result": [action("Add closure return type"), action("Shared")] });
        reply(&state, &tx, first);
        let got = state
            .lock()
            .unwrap()
            .take_code_actions_result()
            .expect("published");
        assert_eq!(
            titles(&got),
            [
                "Add closure return type",
                "Shared",
                "Add explicit type",
                "Inline variable"
            ]
        );
        assert!(!state.lock().unwrap().any_request_in_flight());
    }

    /// An error answers its part with nothing; the other part still shows.
    #[test]
    fn an_error_on_one_position_does_not_hold_the_other() {
        let (tx, _rx) = mpsc::channel();
        let mut s = LspState {
            sender: Some(tx.clone()),
            ..Default::default()
        };
        s.request_code_actions("src/main.rs", &[(1, 1, 1, 1), (1, 5, 1, 5)]);
        let ids: Vec<u64> = s.code_action_pending.iter().map(|(id, _)| *id).collect();
        let state = Arc::new(Mutex::new(s));
        reply(
            &state,
            &tx,
            serde_json::json!({ "jsonrpc": "2.0", "id": ids[0],
            "error": { "code": -32603, "message": "boom" } }),
        );
        reply(
            &state,
            &tx,
            serde_json::json!({ "jsonrpc": "2.0", "id": ids[1],
            "result": [action("Add explicit type")] }),
        );
        let got = state
            .lock()
            .unwrap()
            .take_code_actions_result()
            .expect("published");
        assert_eq!(titles(&got), ["Add explicit type"]);
    }
}

#[cfg(test)]
mod completion_retry_tests {
    use super::*;

    /// A state with a live (captured) channel and one completion in flight.
    /// Returns the state, the receiver of everything sent, and the request id.
    fn in_flight() -> (
        Arc<Mutex<LspState>>,
        mpsc::Receiver<String>,
        mpsc::Sender<String>,
        u64,
    ) {
        let (tx, rx) = mpsc::channel();
        let mut s = LspState::default();
        s.sender = Some(tx.clone());
        s.request_completion("src/main.rs", 4, 7, None);
        let id = s.completion_req_id.unwrap();
        (Arc::new(Mutex::new(s)), rx, tx, id)
    }

    fn reply(state: &Arc<Mutex<LspState>>, tx: &mpsc::Sender<String>, msg: serde_json::Value) {
        let generation = state.lock().unwrap().generation;
        handle_incoming(
            msg,
            state,
            &eframe::egui::Context::default(),
            tx,
            "file:///w",
            generation,
        );
    }

    fn error(id: u64, code: i64) -> serde_json::Value {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": "content modified" } })
    }

    /// The reported symptom: a `didChange` elsewhere cancels the request and the
    /// spinner closed on the "empty" answer. It must be asked again instead.
    #[test]
    fn a_cancelled_completion_is_resent_not_reported() {
        let (state, rx, tx, id) = in_flight();
        let _ = rx.try_recv(); // the original request
        reply(&state, &tx, error(id, -32801));

        let s = state.lock().unwrap();
        assert!(
            !s.completion_response_received,
            "the popup must keep waiting"
        );
        assert!(s.completion_failure.is_none());
        let new_id = s.completion_req_id.expect("a retry is in flight");
        assert_ne!(new_id, id);
        let sent: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(sent["method"], "textDocument/completion");
        assert_eq!(sent["id"], new_id);
        assert_eq!(sent["params"]["position"]["line"], 4);
        assert_eq!(sent["params"]["position"]["character"], 7);
    }

    #[test]
    fn retries_are_bounded_then_the_error_is_reported() {
        let (state, _rx, tx, mut id) = in_flight();
        for _ in 0..COMPLETION_RETRIES {
            reply(&state, &tx, error(id, -32801));
            id = state.lock().unwrap().completion_req_id.unwrap();
        }
        reply(&state, &tx, error(id, -32801));
        let s = state.lock().unwrap();
        assert!(s.completion_response_received);
        assert!(s.completion_req_id.is_none());
        assert!(matches!(
            s.completion_failure,
            Some(CompletionFailure::Error { code: -32801, .. })
        ));
    }

    #[test]
    fn a_real_error_is_reported_at_once() {
        let (state, _rx, tx, id) = in_flight();
        reply(&state, &tx, error(id, -32603));
        let s = state.lock().unwrap();
        assert!(s.completion_response_received);
        assert!(matches!(
            s.completion_failure,
            Some(CompletionFailure::Error { code: -32603, .. })
        ));
    }

    #[test]
    fn a_null_answer_is_told_apart_from_an_empty_list() {
        let (state, _rx, tx, id) = in_flight();
        reply(
            &state,
            &tx,
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": null }),
        );
        assert_eq!(
            state.lock().unwrap().completion_failure,
            Some(CompletionFailure::Null)
        );

        let mut s = state.lock().unwrap();
        s.request_completion("src/main.rs", 4, 7, None);
        assert!(
            s.completion_failure.is_none(),
            "a new request forgets the old cause"
        );
        let id = s.completion_req_id.unwrap();
        drop(s);
        reply(
            &state,
            &tx,
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": [] }),
        );
        assert!(state.lock().unwrap().completion_failure.is_none());
    }

    /// The popup re-orders its rows only when `completion_items` is a
    /// different `Arc`, so every change to the list must replace it: an answer,
    /// a new request and a reset.
    #[test]
    fn every_change_to_the_items_is_a_new_arc() {
        let (state, _rx, tx, id) = in_flight();
        let before = Arc::clone(&state.lock().unwrap().completion_items);
        reply(
            &state,
            &tx,
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": [
                { "label": "len" }, { "label": "new" }
            ] }),
        );
        let answered = Arc::clone(&state.lock().unwrap().completion_items);
        assert!(!Arc::ptr_eq(&before, &answered));
        let labels: Vec<&str> = answered.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["len", "new"]);

        let mut s = state.lock().unwrap();
        s.request_completion("src/main.rs", 4, 8, None);
        assert!(!Arc::ptr_eq(&answered, &s.completion_items));
        assert!(s.completion_items.is_empty());

        let requested = Arc::clone(&s.completion_items);
        s.reset();
        assert!(!Arc::ptr_eq(&requested, &s.completion_items));
        assert!(s.completion_items.is_empty());
    }
}

#[cfg(test)]
mod completion_item_tests {
    use super::parse_completion_item;

    /// rust-analyzer's usual shape: the replacement lives in `textEdit.newText`
    /// (no `insertText`), the label is the display form `name(…)`. The parser
    /// must take the edit text — inserting the label is the reported bug
    /// (`get_param_value(…)` appearing literally in code).
    #[test]
    fn text_edit_new_text_wins_over_label() {
        let v = serde_json::json!({
            "label": "get_param_value(…)",
            "kind": 3,
            "detail": "fn get_param_value(tx: &mut T) -> Option<u32>",
            "insertTextFormat": 2,
            "textEdit": {
                "range": { "start": { "line": 0, "character": 0 },
                           "end":   { "line": 0, "character": 5 } },
                "newText": "get_param_value(${1:tx})$0",
            },
        });
        let item = parse_completion_item(&v).expect("parses");
        assert_eq!(item.insert_text, "get_param_value(${1:tx})$0");
        assert!(item.insert_is_snippet);
    }

    /// `InsertReplaceEdit` also carries `newText` at the same key.
    #[test]
    fn insert_replace_edit_new_text_is_read() {
        let v = serde_json::json!({
            "label": "foo(…)",
            "kind": 3,
            "textEdit": {
                "insert":  { "start": { "line": 0, "character": 0 },
                             "end":   { "line": 0, "character": 3 } },
                "replace": { "start": { "line": 0, "character": 0 },
                             "end":   { "line": 0, "character": 3 } },
                "newText": "foo($1)$0",
            },
        });
        let item = parse_completion_item(&v).expect("parses");
        assert_eq!(item.insert_text, "foo($1)$0");
    }

    /// Fallback chain: `insertText` when no `textEdit`, label as last resort.
    #[test]
    fn fallback_chain_insert_text_then_label() {
        let with_insert = serde_json::json!({
            "label": "bar(…)",
            "kind": 3,
            "insertText": "bar",
        });
        let item = parse_completion_item(&with_insert).expect("parses");
        assert_eq!(item.insert_text, "bar");
        assert!(!item.insert_is_snippet, "no insertTextFormat -> plain text");

        let bare = serde_json::json!({ "label": "baz", "kind": 6 });
        let item = parse_completion_item(&bare).expect("parses");
        assert_eq!(item.insert_text, "baz");
    }
}

#[cfg(test)]
mod exit_message_tests {
    use super::exit_message;

    #[test]
    fn a_bare_exit_says_so() {
        assert_eq!(
            exit_message(None, None, &[]),
            "rust-analyzer exited unexpectedly."
        );
    }

    /// The point of piping stderr: the exit code and the panic reach the
    /// Analyzer tab instead of an unexplained "exited".
    #[test]
    fn the_exit_code_and_the_last_output_are_reported() {
        let tail: Vec<String> = [
            "",
            "thread 'main' panicked at crates/load-cargo/src/lib.rs:12:5:",
            "  ",
            "called `Result::unwrap()` on an `Err` value: Os { code: 5 }",
            "note: run with `RUST_BACKTRACE=1`",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let msg = exit_message(Some(101), None, &tail);
        assert!(
            msg.starts_with("rust-analyzer exited unexpectedly (exit code 101)."),
            "{msg}"
        );
        assert!(msg.contains("panicked at"), "{msg}");
        assert!(msg.ends_with("RUST_BACKTRACE=1`"), "{msg}");
        assert_eq!(
            msg.lines().count(),
            4,
            "the message and the last three lines: {msg}"
        );
    }

    /// What the user saw instead: an exit 101 explained by its last three
    /// backtrace frames. With the panic kept, the frames are not repeated.
    #[test]
    fn the_first_panic_replaces_the_backtrace_tail() {
        let tail: Vec<String> = [
            "27:     0x7ff6bb631daf - <unknown>",
            "28:     0x7ffc8bcf7384 - BaseThreadInitThunk",
            "29:     0x7ffc8bf5cc91 - RtlUserThreadStart",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let panic = "thread 'LspServer' panicked at base-db/src/lib.rs:180:21: \
                     Unable to get `FileSourceRootInput`";
        let msg = exit_message(Some(101), Some((panic, true)), &tail);
        assert_eq!(
            msg,
            format!("rust-analyzer exited unexpectedly (exit code 101).\nPanic: {panic}")
        );
    }

    /// A worker panic the server survived explains nothing on its own: it is
    /// named, and the tail still follows.
    #[test]
    fn a_survived_panic_keeps_the_tail() {
        let tail = vec!["Error: failed to load workspace".to_string()];
        let msg = exit_message(
            Some(1),
            Some(("thread 'Worker1' panicked at a.rs:1:1: x", false)),
            &tail,
        );
        assert_eq!(
            msg,
            "rust-analyzer exited unexpectedly (exit code 1).\n\
             Earlier panic: thread 'Worker1' panicked at a.rs:1:1: x\n\
             Last output: Error: failed to load workspace"
        );
    }
}

#[cfg(test)]
mod panic_watch_tests {
    use super::{PanicWatch, RaPanic};

    fn feed_all(lines: &[&str]) -> Vec<RaPanic> {
        let mut w = PanicWatch::default();
        lines.iter().flat_map(|l| w.feed(l)).collect()
    }

    /// The exact shape of the crash in the Analyzer trace: location line,
    /// message line, then a backtrace whose lines must not be taken for one.
    #[test]
    fn the_two_line_panic_rust_analyzer_writes() {
        let got = feed_all(&[
            "",
            r"thread 'LspServer' (22288) panicked at src\tools\rust-analyzer\crates\base-db\src\lib.rs:180:21:",
            "Unable to get `FileSourceRootInput` with `vfs::FileId` (FileId(67), path: <unknown>); this is a bug",
            "stack backtrace:",
            "   0:     0x7ffc54e519b9 - std::backtrace_rs::backtrace::win64::trace",
        ]);
        assert_eq!(
            got,
            vec![RaPanic {
                thread: "LspServer".into(),
                text: r"thread 'LspServer' panicked at src\tools\rust-analyzer\crates\base-db\src\lib.rs:180:21: Unable to get `FileSourceRootInput` with `vfs::FileId` (FileId(67), path: <unknown>); this is a bug".into(),
            }]
        );
    }

    /// Only `LspServer` is fatal, so the thread name must come out exactly -
    /// with and without the thread id newer toolchains print.
    #[test]
    fn the_thread_name_is_read_with_or_without_an_id() {
        let got = feed_all(&[
            "thread 'Worker1' (7776) panicked at crates/rust-analyzer/src/reload.rs:401:87:",
            "called `Result::unwrap()` on an `Err` value: \"SendError(..)\"",
            "thread 'main' panicked at crates/load-cargo/src/lib.rs:12:5:",
            "boom",
        ]);
        let threads: Vec<&str> = got.iter().map(|p| p.thread.as_str()).collect();
        assert_eq!(threads, ["Worker1", "main"]);
        assert!(
            got[1].text.ends_with("lib.rs:12:5: boom"),
            "{}",
            got[1].text
        );
    }

    /// Two panics back to back: the first is reported bare rather than lost,
    /// and blank lines never count as a message.
    #[test]
    fn a_panic_without_its_message_is_still_reported() {
        let got = feed_all(&[
            "thread 'LspServer' panicked at a.rs:1:1:",
            "   ",
            "thread 'main' panicked at b.rs:2:2:",
            "second",
        ]);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text, "thread 'LspServer' panicked at a.rs:1:1");
        assert_eq!(got[1].text, "thread 'main' panicked at b.rs:2:2: second");
    }

    /// Only the main loop and `main` end the process.
    #[test]
    fn only_the_main_loop_and_main_are_fatal() {
        let fatal = |thread: &str| {
            RaPanic {
                thread: thread.into(),
                text: String::new(),
            }
            .fatal()
        };
        assert!(fatal("LspServer"));
        assert!(fatal("main"));
        assert!(!fatal("Worker1"));
        assert!(!fatal("<unnamed>"));
    }

    /// The pre-1.73 one-line form carries its message inline.
    #[test]
    fn the_old_one_line_form_is_complete_at_once() {
        let got = feed_all(&["thread 'main' panicked at 'boom', src/main.rs:2:5"]);
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].text,
            "thread 'main' panicked at 'boom', src/main.rs:2:5"
        );
    }

    #[test]
    fn ordinary_lines_are_no_panic() {
        assert!(feed_all(&[
            "2026-09-24T14:03:47 WARN notify error: Input watch path is neither a file nor a directory.",
            "ERROR Received compiler message for unknown package",
            "thread 'main' is fine",
        ])
        .is_empty());
    }
}

#[cfg(test)]
mod held_save_tests {
    use super::*;

    /// A state as a launched session has it: a live channel, `main.rs` open,
    /// the handshake done.
    fn session() -> (
        Arc<Mutex<LspState>>,
        mpsc::Receiver<String>,
        mpsc::Sender<String>,
    ) {
        let (tx, rx) = mpsc::channel();
        let mut s = LspState {
            sender: Some(tx.clone()),
            root_uri: "file:///w".into(),
            status: LspStatus::Ready,
            initialized_at: Some(std::time::Instant::now()),
            ..LspState::default()
        };
        s.did_open("src/main.rs", "fn main() {}");
        let state = Arc::new(Mutex::new(s));
        drain(&rx);
        (state, rx, tx)
    }

    fn drain(rx: &mpsc::Receiver<String>) -> Vec<String> {
        rx.try_iter().collect()
    }

    fn saves(sent: &[String]) -> usize {
        sent.iter()
            .filter(|m| m.contains("textDocument/didSave"))
            .count()
    }

    fn status(state: &Arc<Mutex<LspState>>, tx: &mpsc::Sender<String>, quiescent: bool) {
        let generation = state.lock().unwrap().generation;
        handle_incoming(
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "experimental/serverStatus",
                "params": { "health": "ok", "quiescent": quiescent }
            }),
            state,
            &eframe::egui::Context::default(),
            tx,
            "file:///w",
            generation,
        );
    }

    /// The crash: a save during the load reached the server. It must wait
    /// for `quiescent`, and then go out exactly once.
    #[test]
    fn a_save_while_loading_waits_for_quiescent() {
        let (state, rx, tx) = session();
        status(&state, &tx, false);
        state.lock().unwrap().did_save("src/main.rs");
        state.lock().unwrap().did_save("src/main.rs");
        assert_eq!(saves(&drain(&rx)), 0, "nothing may reach a loading server");
        assert!(
            state.lock().unwrap().flycheck_pending(),
            "the check is still owed"
        );

        status(&state, &tx, true);
        assert_eq!(saves(&drain(&rx)), 1, "released once, on quiescent");
        status(&state, &tx, true);
        assert!(!state.lock().unwrap().release_held_save());
        assert_eq!(saves(&drain(&rx)), 0, "and never twice");
    }

    /// A refetch (a manifest change) opens the same window again.
    #[test]
    fn a_reload_holds_saves_again() {
        let (state, rx, tx) = session();
        status(&state, &tx, true);
        state.lock().unwrap().did_save("src/main.rs");
        assert_eq!(saves(&drain(&rx)), 1, "a loaded server gets it at once");

        status(&state, &tx, false);
        state.lock().unwrap().did_save("src/main.rs");
        assert_eq!(saves(&drain(&rx)), 0);
        status(&state, &tx, true);
        assert_eq!(saves(&drain(&rx)), 1);
    }

    /// A save sent directly replaces one still held - the grace can end
    /// between two frames, and the held one must not follow a frame later.
    #[test]
    fn a_direct_save_replaces_a_held_one() {
        let (state, rx, _tx) = session();
        state.lock().unwrap().did_save("src/main.rs");
        let past = std::time::Instant::now()
            .checked_sub(SERVER_STATUS_GRACE + std::time::Duration::from_secs(1))
            .expect("the clock goes back that far");
        state.lock().unwrap().initialized_at = Some(past);
        state.lock().unwrap().did_save("src/main.rs");
        assert_eq!(saves(&drain(&rx)), 1);
        assert!(!state.lock().unwrap().release_held_save());
        assert_eq!(saves(&drain(&rx)), 0, "and never a second time");
    }

    /// A server without the extension: held only for the grace period after
    /// the handshake, then trusted as before - by the per-frame release.
    #[test]
    fn a_server_that_never_reports_gets_its_saves_after_the_grace() {
        let (state, rx, _tx) = session();
        state.lock().unwrap().did_save("src/main.rs");
        assert_eq!(saves(&drain(&rx)), 0, "inside the grace it is held");

        let past = std::time::Instant::now()
            .checked_sub(SERVER_STATUS_GRACE + std::time::Duration::from_secs(1))
            .expect("the clock goes back that far");
        state.lock().unwrap().initialized_at = Some(past);
        assert!(state.lock().unwrap().release_held_save());
        assert_eq!(saves(&drain(&rx)), 1);
        state.lock().unwrap().did_save("src/main.rs");
        assert_eq!(saves(&drain(&rx)), 1, "and later ones go straight out");
    }

    /// Before the handshake nothing is loaded, whatever the clock says.
    #[test]
    fn nothing_is_loaded_before_the_handshake() {
        let s = LspState::default();
        assert!(!s.workspace_loaded());
    }

    /// Closed while held: dropped, and the status stops waiting for it.
    #[test]
    fn a_held_save_for_a_closed_file_is_dropped() {
        let (state, rx, tx) = session();
        status(&state, &tx, false);
        state.lock().unwrap().did_save("src/main.rs");
        state.lock().unwrap().open_files.remove("src/main.rs");
        status(&state, &tx, true);
        assert_eq!(saves(&drain(&rx)), 0);
        assert!(!state.lock().unwrap().flycheck_pending());
    }

    /// Loaded implies indexed and Ready, and the log says when it happened.
    #[test]
    fn quiescent_marks_the_session_loaded() {
        let (state, _rx, tx) = session();
        {
            let mut s = state.lock().unwrap();
            s.status = LspStatus::Indexing;
            s.indexed = false;
        }
        status(&state, &tx, true);
        let s = state.lock().unwrap();
        assert_eq!(s.status, LspStatus::Ready);
        assert!(s.indexed);
        assert!(s.workspace_loaded());
        assert!(
            s.load_log.iter().any(|l| l == "• workspace loaded"),
            "{:?}",
            s.load_log
        );
    }

    /// A health warning ("Failed to run build scripts…") reaches the log.
    #[test]
    fn a_health_warning_is_logged() {
        let (state, _rx, tx) = session();
        let generation = state.lock().unwrap().generation;
        handle_incoming(
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "experimental/serverStatus",
                "params": {
                    "health": "warning",
                    "quiescent": true,
                    "message": "Failed to run build scripts of some packages.\n\nPlease refer to the logs."
                }
            }),
            &state,
            &eframe::egui::Context::default(),
            &tx,
            "file:///w",
            generation,
        );
        let s = state.lock().unwrap();
        assert!(s.workspace_loaded(), "a warning is still loaded");
        assert!(
            s.load_log
                .iter()
                .any(|l| l == "[warn] Failed to run build scripts of some packages."),
            "{:?}",
            s.load_log
        );
    }

    /// `reset` forgets all of it: the next session starts unloaded.
    #[test]
    fn reset_forgets_the_load_state() {
        let (state, _rx, tx) = session();
        status(&state, &tx, false);
        state.lock().unwrap().did_save("src/main.rs");
        let mut s = state.lock().unwrap();
        s.exited_during_load = true;
        s.first_panic = Some(("x".into(), true));
        s.reset();
        assert!(!s.workspace_loaded());
        assert!(s.held_save.is_none());
        assert!(!s.exited_during_load);
        assert!(s.first_panic.is_none());
    }

    /// Without this promise rust-analyzer never sends a status, and every
    /// save waits out the grace period instead of the real load.
    #[test]
    fn the_client_asks_for_server_status() {
        assert_eq!(
            client_capabilities()["experimental"]["serverStatusNotification"],
            serde_json::json!(true)
        );
        // The rest survived the move out of `launch`.
        assert_eq!(
            client_capabilities()["window"]["workDoneProgress"],
            serde_json::json!(true)
        );
        assert_eq!(
            client_capabilities()["textDocument"]["synchronization"]["didSave"],
            serde_json::json!(true)
        );
    }
}

#[cfg(test)]
mod log_preview_tests {
    use super::truncated_json;

    /// The whole point: serialization STOPS at the limit instead of building
    /// the full payload and slicing it. A `documentSymbol` answer for a big
    /// file used to be serialized in its entirety to show 200 characters.
    #[test]
    fn stops_at_the_limit_and_says_so() {
        let big = serde_json::json!(
            (0..5_000)
                .map(|i| serde_json::json!({ "name": format!("symbol_{i}"), "kind": 12 }))
                .collect::<Vec<_>>()
        );
        let out = truncated_json(&big, 200);
        assert!(out.ends_with("…(truncated)"), "truncation must be visible");
        assert!(
            out.len() < 260,
            "cut near the limit, got {} bytes",
            out.len()
        );
        // serde_json::Value keeps object keys sorted, so "kind" comes first.
        assert!(
            out.starts_with(r#"[{"kind":12,"name":"symbol_0""#),
            "keeps the head"
        );
    }

    /// A payload that fits is logged whole, with no truncation marker.
    #[test]
    fn short_values_pass_through_intact() {
        let v = serde_json::json!({ "ok": true });
        assert_eq!(truncated_json(&v, 200), r#"{"ok":true}"#);
        assert_eq!(truncated_json(&serde_json::Value::Null, 200), "null");
    }

    /// The old `&r[..200]` panicked when byte 200 split a character. Cutting
    /// mid-character must produce a lossy string, never a panic.
    #[test]
    fn a_cut_inside_a_multibyte_character_does_not_panic() {
        // 'ă' is two bytes, so some limit lands between them whatever the
        // padding — walk a range to be sure one of them does.
        let v = serde_json::json!("ăăăăăăăăăă");
        for limit in 1..24 {
            let out = truncated_json(&v, limit);
            // A cut character becomes one U+FFFD, so the result can exceed the
            // limit slightly — bounded, which is all the caller needs.
            assert!(
                out.len() <= limit + "…(truncated)".len() + 3,
                "limit {limit} produced {} bytes",
                out.len()
            );
        }
    }
}

/// The startup options handed to rust-analyzer — the object where a setting
/// quietly stops being true.
#[cfg(test)]
mod initialization_options_guard {
    use super::initialization_options;

    /// Proc-macro expansion must stay ON.
    ///
    /// It was off for months behind a comment whose justification had already
    /// been fixed elsewhere, and the damage was silent: a crate that GENERATES
    /// its API with an attribute macro — `ssd1306` does exactly this for its
    /// whole async surface — has no types at all for rust-analyzer. The
    /// binding's type comes out `{unknown}`, so it gets no inlay hint, and then
    /// every later line touching that value answers "no definition" and "no
    /// action", while the hand-written blocking API on the same screen works.
    /// It took five rounds of user reports to pin down.
    ///
    /// Turning it back off "because the expander DLL is missing" buys exactly
    /// that again: a missing DLL only produces `unresolved-proc-macro`, which
    /// the next test keeps suppressed.
    #[test]
    fn proc_macro_expansion_is_enabled() {
        assert_eq!(
            initialization_options()["procMacro"]["enable"],
            serde_json::json!(true),
            "macro-generated APIs are invisible to rust-analyzer when this is off"
        );
    }

    /// The suppression is what makes expansion safe on a project that has never
    /// been built, so the two settings belong together.
    #[test]
    fn the_missing_expander_diagnostic_stays_suppressed() {
        let opts = initialization_options();
        let disabled = opts["diagnostics"]["disabled"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            disabled.iter().any(|d| d == "unresolved-proc-macro"),
            "got {disabled:?}"
        );
    }

    /// We must send an inlay-hint length of our own: rust-analyzer's default is
    /// 25 characters, which renders a real embedded type as
    /// `Ssd1306<I2CInterface<BlockingI2c<…, …>>, …, …>` — with the part the
    /// reader needs inside the elision.
    #[test]
    fn inlay_hints_carry_a_length_of_our_own() {
        let max = initialization_options()["inlayHints"]["maxLength"].as_u64();
        assert!(
            max.is_some_and(|n| n > 25),
            "expected more than rust-analyzer's 25-char default, got {max:?}"
        );
    }

    /// Only ONE hint per line is ever drawn, and rust-analyzer reports chaining
    /// hints with the same LSP kind as type hints — so a chain on the line would
    /// compete with the binding's own type for that single slot.
    #[test]
    fn chaining_hints_are_off_so_they_cannot_take_the_type_slot() {
        assert_eq!(
            initialization_options()["inlayHints"]["chainingHints"]["enable"],
            serde_json::json!(false)
        );
    }
}

#[cfg(test)]
mod linked_projects_tests {
    use super::{
        cargo_can_load_detached, initialization_options, initialization_options_for,
        linked_projects, linked_projects_now, ra_links_detached,
    };

    const LIB: &str = "[package]\nname = \"mylib\"\nversion = \"0.1.0\"\n";

    /// Found by review: `exclude` was compared as trimmed text, which disagreed
    /// with cargo both ways. These are the forms cargo was measured on.
    #[test]
    fn exclude_matches_the_way_cargo_matches_it() {
        let root = |ex: &str| {
            format!("[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\nexclude = [{ex}]\n")
        };
        for accepted in ["\"mylib\"", "\"./mylib/\"", "\".\""] {
            assert!(
                cargo_can_load_detached(&root(accepted), "mylib", LIB),
                "{accepted}"
            );
        }
        for refused in [
            "\" mylib\"",
            "\"mylib \"",
            "\"mylib/src\"",
            "\"x/../mylib\"",
        ] {
            assert!(
                !cargo_can_load_detached(&root(refused), "mylib", LIB),
                "{refused}"
            );
        }
        if cfg!(windows) {
            assert!(cargo_can_load_detached(&root("'.\\mylib'"), "mylib", LIB));
            assert!(cargo_can_load_detached(&root("'mylib\\'"), "mylib", LIB));
        }
    }

    /// The running analyzer's list decides "traced", matched by the library's
    /// own folder - never the firmware's manifest, never a prefix of a name.
    #[test]
    fn the_running_analyzer_is_asked_by_folder() {
        let linked = vec![
            "C:\\ws\\Cargo.toml".to_owned(),
            "C:\\ws\\mylib\\Cargo.toml".to_owned(),
        ];
        assert!(ra_links_detached(&linked, "mylib"));
        assert!(
            !ra_links_detached(&linked, "lib"),
            "not a suffix of a longer name"
        );
        assert!(!ra_links_detached(&linked[..1], "mylib"));
        assert!(!ra_links_detached(&[], "mylib"));
    }

    /// The two ways cargo accepts a nested package it does not own - and the
    /// state the IDE's Detach leaves one in, which it refuses.
    #[test]
    fn cargo_loads_a_detached_library_only_when_told_to() {
        let plain = "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\n";
        assert!(!cargo_can_load_detached(plain, "mylib", LIB));

        let excluded =
            "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\nexclude = [\"./mylib/\"]\n";
        assert!(cargo_can_load_detached(excluded, "mylib", LIB));
        assert!(
            !cargo_can_load_detached(excluded, "other", LIB),
            "only the one excluded"
        );

        let own_ws = format!("{LIB}\n[workspace]\n");
        assert!(cargo_can_load_detached(plain, "mylib", &own_ws));

        // Found by the second review: a root with NO `[workspace]` table - the
        // IDE's own templates - is no workspace at all, and cargo loads the
        // library on its own (measured).
        let no_ws = "[package]\nname = \"fw\"\n\n[dependencies]\n";
        assert!(cargo_can_load_detached(no_ws, "mylib", LIB));
    }

    fn scratch(tag: &str, root: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("eide_linked_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("mylib")).unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("Cargo.toml"), root).unwrap();
        std::fs::write(d.join("mylib/Cargo.toml"), LIB).unwrap();
        d
    }

    /// A loadable detached library is linked, after the firmware; anything
    /// else leaves the options exactly as they were.
    #[test]
    fn only_a_loadable_detached_library_is_linked() {
        let d = scratch(
            "yes",
            "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\nexclude = [\"mylib\"]\n",
        );
        let got = linked_projects(&d).expect("the excluded library is loadable");
        assert_eq!(got.len(), 2);
        assert!(
            got[0].ends_with("Cargo.toml") && !got[0].contains("mylib"),
            "firmware first: {got:?}"
        );
        assert!(got[1].contains("mylib"), "{got:?}");
        assert!(initialization_options_for(&d)["linkedProjects"].is_array());
        let _ = std::fs::remove_dir_all(&d);

        let d = scratch(
            "no",
            "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\n",
        );
        assert_eq!(linked_projects(&d), None, "cargo would refuse it");
        assert_eq!(
            initialization_options_for(&d),
            initialization_options(),
            "unchanged"
        );
        let _ = std::fs::remove_dir_all(&d);

        let d = scratch(
            "member",
            "[package]\nname = \"fw\"\n\n[workspace]\nmembers = [\"mylib\"]\n",
        );
        assert_eq!(
            linked_projects(&d),
            None,
            "a member is already in the workspace"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Found by review: with no `[workspace]` in the root - the IDE's own
    /// templates - a library reached through a `path` dependency is already
    /// part of the firmware's own load. Linking it again hands rust-analyzer
    /// the same crate twice.
    #[test]
    fn a_path_dependency_is_not_linked_twice() {
        let d = scratch(
            "pathdep",
            "[package]\nname = \"fw\"\n\n[dependencies]\nmylib = { path = \"mylib\" }\n",
        );
        assert_eq!(linked_projects(&d), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Found by the second review: without a `[workspace]` table cargo does
    /// not load a path dependency nothing turns on, or one gated to another
    /// target - so the firmware's load does not carry it, and it must stay a
    /// linked project, or rust-analyzer indexes it nowhere.
    #[test]
    fn a_gated_path_dependency_is_still_linked() {
        for (tag, root) in [
            (
                "optional",
                "[package]\nname = \"fw\"\n\n[dependencies]\nmylib = { path = \"mylib\", optional = true }\n",
            ),
            (
                "cfg",
                "[package]\nname = \"fw\"\n\n[target.'cfg(target_os = \"linux\")'.dependencies]\nmylib = { path = \"mylib\" }\n",
            ),
            // Excluded, a `[workspace]` does not make it a member either.
            (
                "excluded",
                "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\nexclude = [\"mylib\"]\n\n[dependencies]\nmylib = { path = \"mylib\", optional = true }\n",
            ),
        ] {
            let d = scratch(tag, root);
            assert_eq!(linked_projects(&d).map(|v| v.len()), Some(2), "{tag}");
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// Found by review: a root manifest that does not parse read as "no
    /// `[workspace]`", which ADDS a library cargo refuses - so a typo restarted
    /// the analyzer into one that cannot load the root at all, and fixing the
    /// typo restarted it again. The Detach state is where it flips.
    #[test]
    fn a_broken_root_manifest_is_no_answer() {
        let d = scratch("broken_root", "[package]\nname = \"fw\"\n");
        for broken in [
            "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\n\n[dependencies]\nx = { version = \"1\"\n",
            "",
            "   \n",
        ] {
            std::fs::write(d.join("Cargo.toml"), broken).unwrap();
            assert_eq!(linked_projects_now(&d, &[]), None, "{broken:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A library whose own `[workspace]` table is what lets cargo load it
    /// dropped out of the set while its manifest was half-typed, and came back
    /// once fixed - two restarts. It keeps the linkage it has instead, either way.
    #[test]
    fn a_broken_library_manifest_keeps_its_linkage() {
        let d = scratch(
            "broken_lib",
            "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\n",
        );
        std::fs::write(d.join("mylib/Cargo.toml"), format!("{LIB}\n[workspace]\n")).unwrap();
        let running = linked_projects_now(&d, &[]).expect("readable");
        assert_eq!(running.len(), 2, "its own [workspace] admits it");
        for broken in [
            format!("{LIB}\n[workspace\n"),
            String::new(),
            "// New file\n".to_owned(),
        ] {
            std::fs::write(d.join("mylib/Cargo.toml"), &broken).unwrap();
            assert_eq!(
                linked_projects_now(&d, &running),
                Some(running.clone()),
                "{broken:?}"
            );
            assert_eq!(linked_projects_now(&d, &[]), Some(vec![]), "{broken:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Found by the second review: one folder whose Cargo.toml STAYS
    /// unreadable made every answer "cannot tell", so no later linkage change
    /// restarted the analyzer - the frozen state the recheck exists to undo.
    #[test]
    fn an_unrelated_broken_manifest_does_not_hide_a_real_change() {
        let d = scratch(
            "unrelated",
            "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\nexclude = [\"mylib\"]\n",
        );
        std::fs::create_dir_all(d.join("tmpl")).unwrap();
        std::fs::write(d.join("tmpl/Cargo.toml"), "// New file\n").unwrap();
        let running = linked_projects_now(&d, &[]).expect("readable root");
        assert_eq!(running.len(), 2, "{running:?}");
        // The `exclude` line goes: cargo refuses mylib now, and the set says so.
        std::fs::write(
            d.join("Cargo.toml"),
            "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\n",
        )
        .unwrap();
        assert_eq!(linked_projects_now(&d, &running), Some(vec![]));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// What must keep answering: a readable manifest, a typo that is still
    /// TOML, and a library whose manifest is gone.
    #[test]
    fn a_readable_manifest_still_answers() {
        let detach_state = "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\n";
        let d = scratch("readable", detach_state);
        assert_eq!(linked_projects_now(&d, &[]), Some(vec![]));
        std::fs::write(
            d.join("Cargo.toml"),
            format!("{detach_state}\n[dependencies]\nx = {{ vesion = \"1\" }}\n"),
        )
        .unwrap();
        assert_eq!(
            linked_projects_now(&d, &[]),
            Some(vec![]),
            "a typo that is still TOML"
        );
        std::fs::write(d.join("mylib/Cargo.toml"), format!("{LIB}\n[workspace]\n")).unwrap();
        let running = linked_projects_now(&d, &[]).expect("readable");
        assert_eq!(running.len(), 2, "its own [workspace] admits it");
        // Gone is not unreadable: it leaves although the analyzer has it.
        std::fs::remove_file(d.join("mylib/Cargo.toml")).unwrap();
        assert_eq!(
            linked_projects_now(&d, &running),
            Some(vec![]),
            "a library that is gone"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// The startup crash against the REAL rust-analyzer, through the IDE's own
/// `start` - capabilities, priority, stderr watch and all. A three-file cargo
/// project reproduces it every time: a `didSave` sent at the first `Ready`
/// (the end of "Fetching") killed the server two seconds in.
#[cfg(test)]
mod live_rust_analyzer_tests {
    use super::*;
    use std::time::{Duration, Instant};

    const MAIN: &str =
        "mod util;\nfn main() {\n    let v = util::double(21);\n    println!(\"{v}\");\n}\n";

    fn tiny_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/main.rs"), MAIN).unwrap();
        std::fs::write(
            dir.path().join("src/util.rs"),
            "pub fn double(x: u32) -> u32 {\n    x * 2\n}\n",
        )
        .unwrap();
        dir
    }

    /// Start the analyzer and wait for the moment the old startup flush fired.
    fn start_until_ready(dir: &Path) -> Arc<Mutex<LspState>> {
        let state = Arc::new(Mutex::new(LspState::default()));
        start(dir, Arc::clone(&state), eframe::egui::Context::default());
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match state.lock().unwrap().status.clone() {
                LspStatus::Ready => break,
                LspStatus::Failed(why) => panic!("failed before Ready: {why}"),
                _ => {}
            }
            assert!(Instant::now() < deadline, "never became Ready");
            thread::sleep(Duration::from_millis(5));
        }
        state
    }

    /// Every process in the analyzer's tree with its priority class. One that
    /// exits between the listing and the lookup (a short `cargo metadata`) has
    /// no class left to read and is skipped.
    #[cfg(windows)]
    fn tree_priorities(root: u32) -> Vec<String> {
        let script = format!(
            "function Walk($id) {{ Get-CimInstance Win32_Process -Filter \"ParentProcessId=$id\" | \
             ForEach-Object {{ $c = (Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue).PriorityClass; \
             if ($c) {{ \"$($_.Name) $c\" }}; Walk $_.ProcessId }} }}; \
             \"root $((Get-Process -Id {root}).PriorityClass)\"; Walk {root}"
        );
        let out = crate::build::no_window(&mut Command::new("powershell"))
            .args(["-NoProfile", "-Command", &script])
            .output()
            .expect("powershell runs");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect()
    }

    #[test]
    #[ignore = "starts the real rust-analyzer (installed, ~10-30 s)"]
    fn a_save_at_the_first_ready_no_longer_kills_the_server() {
        let dir = tiny_project();
        let state = start_until_ready(dir.path());
        {
            // Exactly what the startup flush did at this moment.
            let mut s = state.lock().unwrap();
            s.did_open("src/main.rs", MAIN);
            s.did_save("src/main.rs");
        }

        #[cfg(windows)]
        {
            let pid = state
                .lock()
                .unwrap()
                .child
                .as_ref()
                .map(|c| c.id())
                .expect("running");
            let tree = tree_priorities(pid);
            assert!(
                tree.len() >= 2,
                "the proxy and the server at least: {tree:?}"
            );
            for p in &tree {
                assert!(p.ends_with(" BelowNormal"), "{p} in {tree:?}");
            }
            println!("process tree: {tree:?}");
        }

        // What the app does every frame, until the held save has produced a
        // finished check on a loaded workspace.
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            {
                let mut s = state.lock().unwrap();
                s.release_held_save();
                if let LspStatus::Failed(why) = &s.status {
                    panic!("the server died: {why}");
                }
                if s.workspace_loaded() && !s.finished_checks.is_empty() && s.held_save.is_none() {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "no check after the load");
            thread::sleep(Duration::from_millis(20));
        }
        // And it is still alive a moment later.
        thread::sleep(Duration::from_secs(2));
        let mut s = state.lock().unwrap();
        assert_eq!(s.status, LspStatus::Ready, "{:?}", s.load_log);
        assert!(
            s.load_log.iter().any(|l| l == "• workspace loaded"),
            "{:?}",
            s.load_log
        );
        s.reset();
    }

    /// The control: the same save sent PAST the gate still kills the server -
    /// so the test above is sensitive - and the stderr watch now stops the
    /// dead server at once, with the cause in the message, instead of letting
    /// it burn ~50 s first.
    #[test]
    #[ignore = "starts the real rust-analyzer (installed, ~10 s)"]
    fn a_dead_main_loop_is_stopped_at_once_and_explained() {
        let dir = tiny_project();
        let state = start_until_ready(dir.path());
        let sent_at = {
            let mut s = state.lock().unwrap();
            s.did_open("src/main.rs", MAIN);
            s.send_did_save("src/main.rs");
            Instant::now()
        };
        let deadline = sent_at + Duration::from_secs(60);
        let why = loop {
            if let LspStatus::Failed(why) = state.lock().unwrap().status.clone() {
                break why;
            }
            assert!(
                Instant::now() < deadline,
                "the server did not die: the race is gone?"
            );
            thread::sleep(Duration::from_millis(20));
        };
        let took = sent_at.elapsed();
        println!("failed after {took:?}: {why}");
        assert!(took < Duration::from_secs(15), "stopped late: {took:?}");
        assert!(
            why.contains("Panic: thread 'LspServer' panicked at"),
            "{why}"
        );
        assert!(why.contains("FileSourceRootInput"), "{why}");
        let mut s = state.lock().unwrap();
        assert!(
            s.exited_during_load,
            "the app's one automatic restart applies"
        );
        s.reset();
    }
}
