//! Live "usages" analysis for the currently displayed `.rs` file: every fn/
//! struct/enum/const/static/trait/method/field/… is checked against rust-
//! analyzer's `references` so never-used items can be faded in the editor and
//! used items get a small "N refs" indicator that pops up their call sites.
//!
//! Driven entirely by rust-analyzer's own in-memory analysis (`documentSymbol` +
//! `references`) — NOT Cargo Check/Clippy's `dead_code` lint, which needs a full
//! compile and only runs on demand (see the Clippy tab). `references` is a much
//! cheaper, purely semantic query RA already answers live, so this stays fresh
//! without a Build/Clippy run — confirmed by testing `rust-analyzer diagnostics`
//! directly: RA's own diagnostic pass does NOT report `dead_code`, but
//! `references` is a separate, simpler query it has always supported (it backs
//! Ctrl+R rename and F12 already in this codebase).
//!
//! Recomputed (debounced ~1.2 s idle) whenever the displayed file's text settles,
//! and immediately on switching files. While the live text differs from the
//! snapshot `items` were computed against, nothing is drawn — the same "hide
//! until fresh" rule the inline diagnostic overlay already uses, so an edit
//! never leaves a fade/pill at a stale (now wrong) position.

use crate::app::AppIde;
use crate::editor::gui::text_pos::{GalleyRows, LineIndex, LineIndexCache};
use crate::lsp::LspStatus;
use eframe::egui;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long the displayed file's text must sit unchanged before a fresh
/// documentSymbol/references pass is (re)requested. Raised from 1.2s: each
/// pass costs one whole-crate find-all-references per symbol (serialized, see
/// `pump_references`), so re-running it on every brief typing pause piled load
/// onto rust-analyzer.
pub(super) const DEBOUNCE: Duration = Duration::from_millis(2500);

/// Key-space stride separating reference-request generations: the request key
/// is `refs_run * STRIDE + item_index`, so a late response from a superseded
/// run can be recognised and discarded instead of landing on the wrong item.
const REFS_RUN_STRIDE: usize = 100_000;

/// One usage site: an absolute filesystem path + 0-based LSP position.
#[derive(Clone, Debug)]
struct UsageRef {
    path: String,
    line: u32,
    /// The `macro_rules!` whose body holds this use — a site rust-analyzer
    /// never reports, read from the text (see `macro_uses`).
    via_macro: Option<String>,
}

/// One tracked item (fn/struct/enum/const/…) in the displayed file.
#[derive(Clone, Debug)]
struct UsageItem {
    name: String,
    /// LSP `SymbolKind` — part of the identity key for carrying a previous
    /// run's references over to the new run (instant fade/pill continuity).
    kind: u8,
    // Name position (0-based LSP) — the reference query point.
    sel_line: u32,
    sel_char: u32,
    // Char indices into `computed_for_text`, converted once by [`usage_item`]
    // when the documentSymbol reply lands, so the per-frame overlays do not
    // walk the file for them.
    /// The name: the row the "N refs" indicator sits on.
    sel_ci: usize,
    /// The end of the name's line: where the indicator starts.
    eol_ci: usize,
    /// Whole-item span start — the "fade" range when unused.
    start_ci: usize,
    /// Whole-item span end, as converted (not yet raised to `start_ci`).
    end_ci: usize,
    /// Inside an `impl Trait for Type` block. `references` misses calls
    /// dispatched through a generic trait bound (they bind to the TRAIT's
    /// declaration), so an empty result must NOT fade these — the impl may
    /// well be the live implementation behind every one of those calls.
    in_trait_impl: bool,
    /// `None` while the reference lookup for this item hasn't resolved yet
    /// (nothing is drawn for it until it does). Both sources below, merged —
    /// see [`UsageItem::merge`].
    references: Option<Vec<UsageRef>>,
    /// rust-analyzer's answer. The only part carried over to the next run:
    /// the macro uses are re-read with the text, so an old run's would point
    /// at lines an edit has moved.
    ra_refs: Option<Vec<UsageRef>>,
    /// Uses inside `macro_rules!` bodies, which rust-analyzer never resolves.
    macro_refs: Vec<UsageRef>,
}

impl UsageItem {
    /// `references` from its two sources: nothing until rust-analyzer has
    /// answered (an item still resolving draws nothing), then its sites and
    /// the macro bodies' together, by file and line.
    fn merge(&mut self) {
        self.references = self.ra_refs.as_ref().map(|ra| {
            let mut all = ra.clone();
            all.extend(self.macro_refs.iter().cloned());
            // The two sources spell paths differently (rust-analyzer's URI
            // decoding, the workspace join), so they sort by a normal form.
            all.sort_by_cached_key(|r| (r.path.replace('\\', "/").to_lowercase(), r.line));
            all
        });
    }
}

#[derive(Default)]
pub struct UsagesState {
    /// The file (`"src/main.rs"` / `"src/foo.rs"`) `items` are for.
    rel_path: String,
    items: Vec<UsageItem>,
    /// Exact text snapshot `items`' positions match. Empty = nothing computed
    /// yet. The instant the live text no longer matches this, the overlay
    /// draws nothing until a fresh pass lands (positions would otherwise point
    /// at the wrong code after an edit).
    computed_for_text: String,
    /// `computed_for_text.chars().count()`, counted alongside the item positions.
    computed_total_chars: usize,
    /// The text a documentSymbol request is currently in flight for, so a
    /// still-unchanged buffer isn't re-requested every frame while waiting.
    pending_text: Option<String>,
    /// The text seen on the PREVIOUS tick — compared against the current tick's
    /// text to detect an actual edit. (Comparing against `computed_for_text` /
    /// `pending_text` instead would be wrong: those stay "old" for the whole
    /// debounce+request+response window, so every frame before the first fetch
    /// completes would look like "just changed" and the timer would never
    /// elapse — which is exactly what happened before this field existed.)
    last_seen_text: String,
    /// Debounce timer, reset whenever the live text changes since the last tick.
    last_change_at: Option<Instant>,
    /// Index into `items` whose "references" popup is open, if any.
    open_popup: Option<usize>,
    /// Item indices whose `references` still need fetching, drained ONE AT A
    /// TIME by [`pump_references`]. Firing all of them at once (the old
    /// behaviour) meant N concurrent whole-crate find-all-references queries
    /// per settle (~40 for a big file) — rust-analyzer's queue backed up more
    /// and more over a session, degrading everything LSP-based.
    refs_queue: std::collections::VecDeque<usize>,
    /// Generation counter for reference requests (see [`REFS_RUN_STRIDE`]).
    refs_run: u64,
    /// `true` while one reference request is in flight (next is sent on reply).
    refs_inflight: bool,
    /// Unused generic parameters — see [`generics`](super::generics). Purely
    /// syntactic, so unlike the RA-driven items above it needs no debounce and
    /// can never be stale: it is recomputed the moment the text differs from
    /// `generics_for_text`, and only ever used for that exact text.
    generic_marks: super::generics::GenericMarks,
    generics_for_text: String,
    /// What [`AppIde::unused_import_spans`] last derived from the build results.
    unused_imports: LintSpans,
    /// What the `unused_variables` half of [`AppIde::usages_dead_ranges`] last
    /// derived from the build results.
    unused_variables: LintSpans,
}

/// Send the next queued reference lookup, if none is in flight — one at a time
/// so rust-analyzer is never flooded. Free function (not a method) so callers
/// can hold the `lsp_state` lock and borrow `usages` mutably at the same time.
fn pump_references(usages: &mut UsagesState, lsp: &mut crate::lsp::LspState) {
    if usages.refs_inflight {
        return;
    }
    while let Some(idx) = usages.refs_queue.pop_front() {
        if let Some(item) = usages.items.get(idx) {
            let key = usages.refs_run as usize * REFS_RUN_STRIDE + idx;
            lsp.request_references(&usages.rel_path, item.sel_line, item.sel_char, key);
            usages.refs_inflight = true;
            return;
        }
    }
}

/// Naming-convention / attribute markers of items invoked from OUTSIDE the
/// crate's own source (the embedded runtime, an interrupt vector, the linker,
/// the test harness, …) rather than by a normal Rust call. `references`
/// legitimately comes back empty for these, so they must never be treated as
/// dead code even though nothing in the file calls them by name.
const EXTERNALLY_INVOKED_MARKERS: [&str; 8] = [
    "#[entry]",         // cortex-m-rt binary entry point
    "#[interrupt",      // cortex-m-rt / PAC interrupt handler
    "#[exception",      // cortex-m-rt CPU exception handler
    "#[no_mangle]",     // called by its C-ABI symbol name, not a Rust call
    "#[export_name",    // same, custom exported symbol name
    "#[test]",          // invoked by the test harness
    "#[panic_handler]", // invoked by the runtime on panic
    "#[global_allocator]",
];

/// [`is_externally_invoked_in`] over a whole text, for the tests.
#[cfg(test)]
fn is_externally_invoked(name: &str, start_line: u32, text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    is_externally_invoked_in(name, start_line, &lines)
}

/// `true` when the item named `name`, starting at 0-based line `start_line` in
/// `lines` (`text.lines()`, collected once per documentSymbol reply by the
/// caller rather than once per symbol), is externally invoked (see
/// [`EXTERNALLY_INVOKED_MARKERS`]) — either `fn main` itself (the
/// `#[entry]`-annotated binary entry point in this `no_std` project; excluded
/// by name too since a plain `fn main` is never called from source even without
/// the attribute) or preceded by one of the marker attributes. Scans upward
/// from the item's own line (inclusive, since rust-analyzer's `documentSymbol`
/// range sometimes starts at a leading attribute and sometimes at the `fn`/`pub`
/// keyword — checking both is cheap), skipping blank lines, `//` doc comments,
/// and other attributes, stopping at the first real code line above the item's
/// own.
fn is_externally_invoked_in(name: &str, start_line: u32, lines: &[&str]) -> bool {
    if name == "main" {
        return true;
    }
    let mut i = (start_line as usize + 1).min(lines.len());
    while i > 0 {
        i -= 1;
        let l = lines.get(i).map(|s| s.trim()).unwrap_or("");
        if l.is_empty() || l.starts_with("//") {
            continue;
        }
        if l.starts_with("#[") {
            if EXTERNALLY_INVOKED_MARKERS.iter().any(|m| l.contains(m)) {
                return true;
            }
            continue;
        }
        // Real code: on the item's own line (first iteration) this is just its
        // signature — keep scanning above for attributes. Once we've moved
        // above that line, real code means we've run out of attributes.
        if i < start_line as usize {
            break;
        }
    }
    false
}

/// The tracked item for one documentSymbol entry, with its positions converted
/// to char indices in `index`, the text that reply was computed for.
///
/// Converted once here rather than in the overlays, which run every frame and
/// walked the file from offset 0 for each item. This cannot go stale: a
/// `references` reply never moves an item, and both readers draw nothing unless
/// the live text equals `computed_for_text`.
fn usage_item(
    s: crate::lsp::SymbolInfo,
    index: &LineIndex,
    ra_refs: Option<Vec<UsageRef>>,
    macro_refs: Vec<UsageRef>,
) -> UsageItem {
    let mut item = UsageItem {
        sel_ci: index.pos_to_char_idx(s.sel_line + 1, s.sel_char + 1),
        eol_ci: index.line_end_char_idx(s.sel_line + 1),
        start_ci: index.pos_to_char_idx(s.start_line + 1, s.start_char + 1),
        end_ci: index.pos_to_char_idx(s.end_line + 1, s.end_char + 1),
        name: s.name,
        kind: s.kind,
        sel_line: s.sel_line,
        sel_char: s.sel_char,
        in_trait_impl: s.in_trait_impl,
        references: None,
        ra_refs,
        macro_refs,
    };
    item.merge();
    item
}

/// The uses of the item `s` (at `start_ci`..`sel_ci` of `index`, in crate
/// `krate`) inside the bodies of `macros`, as usage sites under `workspace`
/// — where the popup can navigate to them like any other.
fn macro_refs_of(
    s: &crate::lsp::SymbolInfo,
    index: &LineIndex,
    krate: &str,
    macros: &super::macro_uses::MacroIndex,
    workspace: &std::path::Path,
) -> Vec<UsageRef> {
    // rust-analyzer reports a `macro_rules!` as a Function. Told apart by the
    // code right before the name — not by the whole range, which starts at
    // the item's doc comment and attributes: a helper documented as "called
    // from the macro_rules body" would otherwise match only `helper!`.
    let start = index.pos_to_char_idx(s.start_line + 1, s.start_char + 1);
    let sel = index.pos_to_char_idx(s.sel_line + 1, s.sel_char + 1);
    let is_macro = s.kind == 12
        && start <= sel
        && index
            .text()
            .get(index.byte_of_char(start)..index.byte_of_char(sel))
            .and_then(|t| t.trim_end().strip_suffix('!'))
            .and_then(|t| t.trim_end().strip_suffix("macro_rules"))
            .is_some_and(|p| !p.ends_with(|c: char| c.is_alphanumeric() || c == '_'));
    let shape = super::macro_uses::ItemShape {
        name: &s.name,
        kind: s.kind,
        container: s.container.as_ref().map(|(k, n)| (*k, n.as_str())),
        is_macro,
        krate,
    };
    macros
        .uses_of(&shape)
        .into_iter()
        .map(|u| UsageRef {
            path: workspace.join(&u.rel).to_string_lossy().into_owned(),
            line: u.line,
            via_macro: Some(u.macro_name),
        })
        .collect()
}

/// One `unused_variables` / `unused_imports` diagnostic of the displayed file,
/// copied out of a build result while its lock is held. Only the fields the
/// spans are derived from, so the per-frame copy is a few short strings rather
/// than a clone of both results.
#[derive(Clone, Debug, PartialEq)]
struct LintSite {
    message: String,
    line: Option<u32>,
    col: Option<u32>,
}

/// Appends the `code` diagnostics `result` reports for `rel_path`, in order.
fn push_lint_sites(
    out: &mut Vec<LintSite>,
    result: &crate::build::BuildResult,
    rel_path: &str,
    code: &str,
) {
    out.extend(
        result
            .diagnostics
            .iter()
            .filter(|d| d.file.as_deref() == Some(rel_path) && d.code.as_deref() == Some(code))
            .map(|d| LintSite {
                message: d.message.clone(),
                line: d.line,
                col: d.col,
            }),
    );
}

/// Derives a file's spans from its lint sites, given an index of its text.
type DeriveSpans = fn(&[LintSite], &LineIndex) -> Vec<(usize, usize)>;

/// Spans derived from one lint's sites, kept while the file, the sites and the
/// text are all unchanged. They change only when a build lands or the text is
/// edited, while deriving them walks the whole file.
#[derive(Default)]
struct LintSpans {
    rel_path: String,
    sites: Vec<LintSite>,
    /// The index the spans were derived through; its text is the key text.
    /// `None` until the first derivation.
    index: Option<Arc<LineIndex>>,
    spans: Vec<(usize, usize)>,
}

impl LintSpans {
    /// `derive(&sites, index of text)`, recomputed only when `rel_path`,
    /// `sites` or `text` differs from the last call. `line_index` is the view's
    /// cache, asked only on a recompute.
    fn get(
        &mut self,
        rel_path: &str,
        sites: Vec<LintSite>,
        text: &str,
        line_index: &mut LineIndexCache,
        derive: DeriveSpans,
    ) -> Vec<(usize, usize)> {
        // No site derives no span, whatever the text: skip the index.
        if sites.is_empty() {
            return Vec::new();
        }
        let hit = self.rel_path == rel_path
            && self.sites == sites
            && self.index.as_ref().is_some_and(|ix| ix.text() == text);
        if !hit {
            let index = line_index.get(text);
            self.spans = derive(&sites, &index);
            self.index = Some(index);
            self.sites = sites;
            rel_path.clone_into(&mut self.rel_path);
        }
        self.spans.clone()
    }
}

/// The fade span for an `unused_variables` diagnostic, self-verified against
/// the CURRENT text: `None` unless the identifier named in the diagnostic's
/// message (rustc's `"unused variable: \`x\`"` format) still appears at its
/// reported position. Guards against a Build/Clippy result gone stale after a
/// later edit shifted the code — the diagnostic just silently stops applying
/// rather than fading the wrong span.
fn unused_variable_range(d: &LintSite, index: &LineIndex) -> Option<(usize, usize)> {
    let name = d.message.split('`').nth(1)?;
    if name.is_empty() {
        return None;
    }
    let start = index.pos_to_char_idx(d.line?, d.col?);
    let end = (start + name.chars().count()).min(index.total_chars());
    if end <= start || index.text()[index.byte_of_char(start)..index.byte_of_char(end)] != *name {
        return None; // stale — the code at that position no longer matches
    }
    Some((start, end))
}

/// Every `unused_variables` site's fade span, in site order.
fn unused_variable_ranges(sites: &[LintSite], index: &LineIndex) -> Vec<(usize, usize)> {
    sites
        .iter()
        .filter_map(|d| unused_variable_range(d, index))
        .collect()
}

/// Is `c` part of a Rust identifier? Used for the whole-word test below, so
/// looking for `Read` never matches inside `ReadExactError`.
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Every WHOLE-WORD occurrence of `needle` inside `chars[from..to)`.
///
/// Whole-word on both sides, which is the whole point: `use a::{Read,
/// ReadExactError, Write};` with `Read` unused must land on `Read`, not on the
/// first four letters of its neighbour.
fn word_positions(chars: &[char], needle: &[char], from: usize, to: usize) -> Vec<usize> {
    let hi = to.min(chars.len());
    if needle.is_empty() || hi < needle.len() {
        return Vec::new();
    }
    (from..=(hi - needle.len()))
        .filter(|&i| {
            chars[i..i + needle.len()] == *needle
                && (i == 0 || !is_ident_char(chars[i - 1]))
                && chars
                    .get(i + needle.len())
                    .is_none_or(|c| !is_ident_char(*c))
        })
        .collect()
}

/// Where the name `needle` sits, for a diagnostic that pointed at `line_1`.
///
/// Two passes, both self-verifying:
/// 1. **On the reported line.** rustc points at the item for a whole unused
///    `use`, and at the name itself for one inside a brace list — either way
///    the name is on that line, so this covers both.
/// 2. **A UNIQUE occurrence anywhere in the file**, for a brace list broken
///    over several lines: the diagnostic's primary span is then the `use` on
///    the first line while the name sits three lines down. Uniqueness is what
///    makes it safe, and it is nearly always true for exactly the reason the
///    lint fired — a name that is unused occurs once, in the import that
///    introduced it. Two occurrences (a mention in a comment, say) and this
///    gives up rather than guess.
fn locate_import_name(
    index: &LineIndex,
    chars: &[char],
    needle: &[char],
    line_1: u32,
) -> Option<usize> {
    let line_start = index.pos_to_char_idx(line_1, 1);
    let line_end = index.line_end_char_idx(line_1);
    if let Some(&i) = word_positions(chars, needle, line_start, line_end).first() {
        return Some(i);
    }
    let all = word_positions(chars, needle, 0, chars.len());
    (all.len() == 1).then(|| all[0])
}

/// The spans to fade and pulse for one `unused_imports` diagnostic, verified
/// against the CURRENT text.
///
/// Three things differ from [`unused_variable_range`], and each one is a real
/// shape rustc emits:
/// * **plural** — `unused imports: \`A\`, \`B\`` puts several names in ONE
///   diagnostic, so every backticked piece counts, not just the first;
/// * **the name is a PATH** for a whole unused import
///   (``unused import: `embedded_io_async::Write` ``) and a bare identifier for
///   one inside braces (``unused import: `ReadExactError` ``) — searching for
///   the backticked text literally handles both without telling them apart;
/// * **the reported column need not point at the name** — for a whole unused
///   `use` the primary span starts at `use`, so anchoring on the column the way
///   the variable lint does would find nothing.
///
/// Empty when nothing verifies, which is the same silent-stop behaviour the
/// variable fade has: a stale Build/Clippy result stops applying rather than
/// marking the wrong span.
///
/// `chars` is `index.text()` collected, once for all sites.
fn unused_import_ranges(d: &LintSite, index: &LineIndex, chars: &[char]) -> Vec<(usize, usize)> {
    let Some(line) = d.line else {
        return Vec::new();
    };
    d.message
        .split('`')
        .enumerate()
        // Odd pieces are the ones BETWEEN a pair of backticks.
        .filter(|(i, part)| i % 2 == 1 && !part.is_empty())
        .filter_map(|(_, name)| {
            let needle: Vec<char> = name.chars().collect();
            locate_import_name(index, chars, &needle, line)
                .map(|start| (start, start + needle.len()))
        })
        .collect()
}

/// Every `unused_imports` site's spans, sorted and deduplicated.
fn unused_import_spans_in(sites: &[LintSite], index: &LineIndex) -> Vec<(usize, usize)> {
    let chars: Vec<char> = index.text().chars().collect();
    let mut out: Vec<(usize, usize)> = sites
        .iter()
        .flat_map(|d| unused_import_ranges(d, index, &chars))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

impl AppIde {
    /// Snapshot every `.rs` file's CURRENT content into `build_text_snapshot`
    /// (`"src/main.rs"` / `"src/{rel}"` → text) — call right alongside
    /// `write_project` whenever a Cargo Check / Clippy run is kicked off, so
    /// the snapshot always matches exactly what got compiled. Lets
    /// `usages_dead_ranges` tell whether that run's `unused_variables`
    /// diagnostics still apply to the live text (exact match) or have gone
    /// stale from a later edit (any edit anywhere in the file, not just at the
    /// diagnostic's own position — e.g. adding a use of the variable elsewhere
    /// doesn't move the `let`, but must still invalidate the "unused" fade).
    pub(super) fn snapshot_build_text(&mut self) {
        self.build_text_snapshot.clear();
        self.build_text_snapshot
            .insert("src/main.rs".to_owned(), self.generated_code.clone());
        for (rel, content) in &self.project_tree.user_src_files {
            self.build_text_snapshot
                .insert(rel.clone(), content.clone());
        }
    }

    /// Char-index spans of the unused IMPORTS in the displayed file — faded like
    /// any other unused thing, and pulsed like an unused generic parameter.
    ///
    /// Same source and same staleness guard as the `unused_variables` half of
    /// [`usages_dead_ranges`](Self::usages_dead_ranges): rust-analyzer does not
    /// report this lint natively (it arrives through flycheck), so it comes from
    /// the last Cargo Check / Clippy run and is used ONLY while the file's live
    /// text still matches EXACTLY what was compiled.
    ///
    /// Computed once per frame by the caller and handed to BOTH the fade and the
    /// pulse, so the two can never disagree about what is unused.
    pub(super) fn unused_import_spans(
        &mut self,
        rel_path: &str,
        display_code: &str,
    ) -> Vec<(usize, usize)> {
        if self.build_text_snapshot.get(rel_path).map(String::as_str) != Some(display_code) {
            return Vec::new();
        }
        let sites = self.lint_sites(rel_path, "unused_imports");
        self.ed.usages.unused_imports.get(
            rel_path,
            sites,
            display_code,
            &mut self.ed.line_index,
            unused_import_spans_in,
        )
    }

    /// The `code` diagnostics the last Cargo Check and then the last Clippy run
    /// reported for `rel_path`, copied out under each lock in turn.
    fn lint_sites(&self, rel_path: &str, code: &str) -> Vec<LintSite> {
        let mut sites = Vec::new();
        for state in [&self.build_state, &self.clippy_state] {
            if let crate::build::BuildState::Done(result) = &*state.lock().unwrap() {
                push_lint_sites(&mut sites, result, rel_path, code);
            }
        }
        sites
    }

    /// Advance the usages pipeline for the displayed `.rs` file: reset on a
    /// file switch, poll any in-flight LSP responses, and — once the text has
    /// settled and RA is `Ready` — (re)issue `documentSymbol` for it. Call once
    /// per frame, before rendering the editor (so `usages_dead_ranges` below
    /// reflects this frame's poll).
    pub(super) fn tick_usages(&mut self, rel_path: &str, text: &str) {
        if self.ed.usages.rel_path != rel_path {
            self.ed.usages = UsagesState {
                rel_path: rel_path.to_owned(),
                ..Default::default()
            };
        }
        self.poll_usages();

        // Unused generic parameters: local, LSP-free, so it runs on every edit
        // instead of waiting out the debounce below (memoized on the text, so
        // it is one scan per keystroke, not one per frame). `.rs` only — a
        // library's Cargo.toml also arrives here as a `UserFile`.
        if self.ed.usages.generics_for_text != text {
            self.ed.usages.generic_marks = if rel_path.ends_with(".rs") {
                super::generics::analyze(text)
            } else {
                Default::default()
            };
            self.ed.usages.generics_for_text = text.to_owned();
        }

        if self.ed.usages.last_seen_text != text {
            self.ed.usages.last_seen_text = text.to_owned();
            self.ed.usages.last_change_at = Some(Instant::now());
        }

        let settled = self
            .ed
            .usages
            .last_change_at
            .is_none_or(|t| t.elapsed() > DEBOUNCE);
        let stale = self.ed.usages.computed_for_text != text;
        let already_pending = self.ed.usages.pending_text.as_deref() == Some(text);
        if stale && settled && !already_pending {
            let ready = matches!(self.lsp_state.lock().unwrap().status, LspStatus::Ready);
            if ready {
                self.ed.usages.pending_text = Some(text.to_owned());
                self.lsp_state
                    .lock()
                    .unwrap()
                    .request_document_symbols(rel_path);
            }
        }
    }

    /// Drain completed `documentSymbol` / `references` LSP responses into
    /// `self.ed.usages`. A fresh symbol list immediately fires one `references`
    /// request per item (concurrent — keyed by its index in the new list).
    fn poll_usages(&mut self) {
        let symbols = {
            let mut lsp = self.lsp_state.lock().unwrap();
            lsp.take_document_symbols_result()
        };
        if let Some((file, syms)) = symbols {
            if file == self.ed.usages.rel_path {
                let text = self.ed.usages.pending_text.take().unwrap_or_default();
                // Carry the previous run's resolved references over by (name,
                // kind) so the fade/pill stays continuous while the serialized
                // refresh below re-verifies each item one by one. Only
                // rust-analyzer's: the macro uses are re-read just below.
                let cache: std::collections::HashMap<(String, u8), Vec<UsageRef>> =
                    std::mem::take(&mut self.ed.usages.items)
                        .into_iter()
                        .filter_map(|it| Some(((it.name, it.kind), it.ra_refs?)))
                        .collect();
                // Both built once for the whole reply, from the exact text its
                // positions describe.
                let index = LineIndex::new(&text);
                let lines: Vec<&str> = text.lines().collect();
                // Uses inside `macro_rules!` bodies, which rust-analyzer never
                // resolves: read from every project file — the displayed one
                // as this reply describes it. Once per settled run, no request.
                let rel = self.ed.usages.rel_path.clone();
                // A build script, `tests/`, `examples/` is its own unit: only
                // its own macros expand into it, and none of its into the crate.
                let macros = if !super::macro_uses::in_crate_src(&rel) {
                    super::macro_uses::MacroIndex::of_unit(&rel, &text)
                } else {
                    let mut files: Vec<(&str, &str)> = vec![(rel.as_str(), text.as_str())];
                    if rel != "src/main.rs" {
                        files.push(("src/main.rs", self.generated_code.as_str()));
                    }
                    files.extend(
                        self.project_tree
                            .user_src_files
                            .iter()
                            .filter(|(r, _)| *r != rel)
                            .map(|(r, t)| (r.as_str(), t.as_str())),
                    );
                    self.ed.macro_facts.index(files)
                };
                let krate = crate::panels::structure_map::parse::split_crate(&rel).0;
                let workspace = crate::workspace::dir();
                self.ed.usages.items = syms
                    .into_iter()
                    // Entry points / externally-invoked items (`fn main`, an
                    // `#[interrupt]` handler, …) are never called from the
                    // crate's own source — `references` legitimately comes back
                    // empty for them, so they must be excluded here rather than
                    // shown as "dead code".
                    .filter(|s| !is_externally_invoked_in(&s.name, s.start_line, &lines))
                    .map(|s| {
                        let ra_refs = cache.get(&(s.name.clone(), s.kind)).cloned();
                        let macro_refs = macro_refs_of(&s, &index, &krate, &macros, &workspace);
                        usage_item(s, &index, ra_refs, macro_refs)
                    })
                    .collect();
                self.ed.usages.computed_total_chars = index.total_chars();
                self.ed.usages.computed_for_text = text;
                self.ed.usages.open_popup = None;
                // Refresh every item's references — SERIALIZED (one in-flight
                // request; the next goes out when the reply lands), never the
                // old fire-all-at-once flood. A fresh run supersedes any
                // still-queued indices from the previous one.
                self.ed.usages.refs_run += 1;
                self.ed.usages.refs_queue = (0..self.ed.usages.items.len()).collect();
                self.ed.usages.refs_inflight = false;
                let mut lsp = self.lsp_state.lock().unwrap();
                pump_references(&mut self.ed.usages, &mut lsp);
            }
            // A response for a file we've since navigated away from — drop it;
            // `take_document_symbols_result` already removed it from `lsp_state`.
        }

        let refs = {
            let mut lsp = self.lsp_state.lock().unwrap();
            lsp.take_reference_results()
        };
        if !refs.is_empty() {
            for (key, locs) in refs {
                // Any reply frees the in-flight slot; only replies from the
                // CURRENT run are applied (a superseded run's key would land on
                // the wrong item).
                self.ed.usages.refs_inflight = false;
                if key / REFS_RUN_STRIDE == self.ed.usages.refs_run as usize {
                    let idx = key % REFS_RUN_STRIDE;
                    if let Some(item) = self.ed.usages.items.get_mut(idx) {
                        item.ra_refs = Some(
                            locs.into_iter()
                                .map(|r| UsageRef {
                                    path: r.path,
                                    line: r.line,
                                    via_macro: None,
                                })
                                .collect(),
                        );
                        item.merge();
                    }
                }
            }
            let mut lsp = self.lsp_state.lock().unwrap();
            pump_references(&mut self.ed.usages, &mut lsp);
        }
    }

    /// Char-index `[start, end)` ranges to dim in the currently displayed file:
    /// never-referenced items (fn/struct/enum/const/…, from the live RA
    /// `references` analysis above) PLUS never-read local `let` bindings.
    ///
    /// Local variables aren't file-level items, so `documentSymbol` never sees
    /// them, and — confirmed by probing `rust-analyzer` directly with this
    /// app's own `checkOnSave: false` setting — RA's native analysis never
    /// reports `unused_variables` without a real compile (`source` comes back
    /// `"rustc"`, i.e. flycheck-only; two live probes returned zero diagnostics
    /// with checkOnSave off). So, for JUST this lint, we fall back to whatever
    /// the last Cargo Check / Clippy run found (same source the Clippy tab
    /// already shows it in) — gated on `build_text_snapshot`: that run's
    /// diagnostics are used ONLY while the file's live text still matches
    /// EXACTLY what was compiled (an edit anywhere in the file — not just at
    /// the diagnostic's own position — must invalidate them: adding a use of
    /// the variable elsewhere doesn't move the `let`, but clears the "unused"
    /// verdict all the same). The per-diagnostic identifier re-check below is
    /// extra insurance on top of that, not a substitute for it.
    pub(super) fn usages_dead_ranges(
        &mut self,
        rel_path: &str,
        display_code: &str,
    ) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();

        // Generic parameters first — computed from this exact text in
        // `tick_usages`, so no freshness guard is needed (or wanted: it is the
        // one part of the fade that stays live while you type).
        if self.ed.usages.generics_for_text == display_code {
            // Only the fully-unused ones. The `impl`-only parameters are
            // underlined instead — fading them would be plain wrong.
            ranges.extend_from_slice(&self.ed.usages.generic_marks.unused);
        }

        if self.ed.usages.rel_path == rel_path && self.ed.usages.computed_for_text == display_code {
            ranges.extend(self.ed.usages.items.iter().filter_map(|item| {
                let refs = item.references.as_ref()?;
                if !refs.is_empty() {
                    return None;
                }
                // Trait-impl members: zero references only means "no DIRECT
                // call" — generic trait-bound dispatch binds to the trait's
                // declaration, so these are very often live. Never fade them.
                if item.in_trait_impl {
                    return None;
                }
                // Converted against `computed_for_text`, which equals
                // `display_code` here.
                Some((item.start_ci, item.end_ci.max(item.start_ci)))
            }));
        }

        if self.build_text_snapshot.get(rel_path).map(String::as_str) == Some(display_code) {
            let sites = self.lint_sites(rel_path, "unused_variables");
            ranges.extend(self.ed.usages.unused_variables.get(
                rel_path,
                sites,
                display_code,
                &mut self.ed.line_index,
                unused_variable_ranges,
            ));
        }

        ranges
    }

    /// The unused-generic ranges for `display_code`, or empty when the cached
    /// scan is for different text. Same data the fade uses — handed to the
    /// pulsing overlay so the two can never disagree about what is unused.
    pub(super) fn generic_pulse_ranges(&self, display_code: &str) -> &[(usize, usize)] {
        if self.ed.usages.generics_for_text == display_code {
            &self.ed.usages.generic_marks.unused
        } else {
            &[]
        }
    }

    /// Ranges to underline: generic parameters the item itself doesn't use but
    /// an `impl` of it does. Empty when the cached scan is for different text.
    pub(super) fn generic_underline_ranges(&self, display_code: &str) -> &[(usize, usize)] {
        if self.ed.usages.generics_for_text == display_code {
            &self.ed.usages.generic_marks.underline
        } else {
            &[]
        }
    }

    /// Paint the "N refs" indicator on every used item's signature line and
    /// handle its click (open/close a floating popup listing call sites, itself
    /// clickable to navigate there). A no-op unless the usages analysis is
    /// fresh for the exact `display_code` passed in (same guard as
    /// `usages_dead_ranges` — keeps the two in lock-step).
    pub(super) fn show_usages_overlay(
        &mut self,
        ui: &egui::Ui,
        galley_pos: egui::Pos2,
        clip: egui::Rect,
        galley: &egui::text::Galley,
        display_code: &str,
        rel_path: &str,
    ) -> Vec<(u32, f32)> {
        if self.ed.usages.rel_path != rel_path || self.ed.usages.computed_for_text != display_code {
            return Vec::new();
        }

        const PILL_BG: egui::Color32 = egui::Color32::from_rgba_premultiplied(40, 58, 76, 220);
        const PILL_FG: egui::Color32 = egui::Color32::from_rgb(150, 195, 235);

        // Pass 1 (read-only borrow of `self.ed.usages.items`): paint every pill and
        // remember its rect + click state. Nothing here mutates `self` — the
        // popup (which navigates on click) is handled in pass 2, after this
        // borrow ends.
        let mut clicked: Option<usize> = None;
        let mut pill_rects: std::collections::HashMap<usize, egui::Rect> = Default::default();
        // Right edge of each pill, keyed by 1-BASED source line, for the inline
        // diagnostic message drawn later in the same frame — it used to start at
        // end-of-line + 16 and land on top of the pill, which starts at + 14.
        //
        // Returned rather than stored: `show_usages_overlay` and the diagnostics
        // overlay are both reached from one `show_code_view` frame, so a value
        // that cannot outlive that frame is exactly what is wanted. A field on
        // `EditorState` would have needed a "clear it before every read" rule
        // that nothing enforces.
        let mut pill_edges: Vec<(u32, f32)> = Vec::new();
        {
            // Equal to `display_code.chars().count()`: the gate above holds.
            let total_chars = self.ed.usages.computed_total_chars;
            // `galley.pos_from_cursor` by binary search: one row table for all
            // the pills instead of a row walk per lookup.
            let rows = GalleyRows::new(galley);
            let painter = ui.painter().with_clip_rect(clip);
            let gp = galley_pos;
            for (i, item) in self.ed.usages.items.iter().enumerate() {
                let Some(refs) = &item.references else {
                    continue; // still resolving
                };
                if refs.is_empty() {
                    continue; // unused → faded by the highlighter, no pill
                }

                let sel_ci = item.sel_ci.min(total_chars);
                let loc_sel = rows.pos(sel_ci);
                let y_top = gp.y + loc_sel.min.y;
                let y_bot = gp.y + loc_sel.max.y;
                if y_bot < clip.top() || y_top > clip.bottom() {
                    continue; // scrolled out of view
                }
                // Only a pill that is drawn needs its line end.
                let eol_ci = item.eol_ci.min(total_chars);
                let loc_eol = rows.pos(eol_ci);

                let label = format!(
                    "{} {}",
                    refs.len(),
                    if refs.len() == 1 { "ref" } else { "refs" }
                );
                let font = egui::FontId::proportional(10.0);
                let galley_txt = painter.layout_no_wrap(label.clone(), font.clone(), PILL_FG);
                let pad = 3.0;
                let x0 = gp.x + loc_eol.min.x + 14.0;
                let h = (y_bot - y_top).max(galley_txt.size().y + pad);
                let rect = egui::Rect::from_min_size(
                    egui::pos2(x0, y_top + (y_bot - y_top - h).max(0.0) * 0.5),
                    egui::vec2(galley_txt.size().x + pad * 2.0, h),
                );
                painter.rect_filled(rect, 3.0, PILL_BG);
                painter.text(
                    rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    &label,
                    font,
                    PILL_FG,
                );
                pill_rects.insert(i, rect);
                // The key is the same 1-based line, `sel_line + 1`, that
                // `usage_item` converted into `eol_ci`. Deriving it any other way
                // would invite the off-by-one that the three overlays' three
                // different line bases make so easy — and one that fails
                // silently, as a message that simply does not move.
                pill_edges.push((item.sel_line + 1, rect.right()));

                let resp = ui.interact(
                    rect,
                    egui::Id::new("usages_pill").with(rel_path).with(i),
                    egui::Sense::click(),
                );
                if resp.clicked() {
                    clicked = Some(i);
                }
                resp.on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(format!(
                        "`{}` — {label}, click to see call sites",
                        item.name
                    ));
            }
        }

        if let Some(i) = clicked {
            self.ed.usages.open_popup = if self.ed.usages.open_popup == Some(i) {
                None
            } else {
                Some(i)
            };
        }

        // Pass 2: the floating popup (needs `&mut self` to navigate on click),
        // now that pass 1's borrow of `self.ed.usages.items` has ended.
        if let Some(idx) = self.ed.usages.open_popup {
            match pill_rects.get(&idx) {
                Some(&anchor) => self.show_usage_popup(ui, anchor, idx),
                None => self.ed.usages.open_popup = None, // pill scrolled off-screen
            }
        }
        pill_edges
    }

    /// The floating "References to `name`" popup anchored below a pill; each
    /// row navigates to that call site (opens the file, scrolls, highlights).
    fn show_usage_popup(&mut self, ui: &egui::Ui, anchor: egui::Rect, idx: usize) {
        let Some(item) = self.ed.usages.items.get(idx) else {
            self.ed.usages.open_popup = None;
            return;
        };
        let name = item.name.clone();
        let refs = item.references.clone().unwrap_or_default();

        let mut close = false;
        let mut nav_to: Option<(String, u32)> = None;

        egui::Area::new(egui::Id::new("usages_popup"))
            .fixed_pos(anchor.left_bottom() + egui::vec2(0.0, 4.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(&ui.ctx().global_style()).show(ui, |ui| {
                    ui.set_min_width(240.0);
                    ui.label(
                        egui::RichText::new(format!("References to `{name}`"))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(170, 180, 200)),
                    );
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            for r in &refs {
                                let short = crate::app::short_path(&r.path);
                                // A use read from a macro body says so: it is a
                                // text match, and the line is the macro's.
                                let label = match &r.via_macro {
                                    Some(m) => format!("{short}:{} · in {m}!", r.line + 1),
                                    None => format!("{short}:{}", r.line + 1),
                                };
                                // A real `Link`, not a click-sensing `Label`:
                                // each row navigates, so it has to LOOK like one
                                // — link colour, underline on hover and the
                                // pointing-hand cursor, all of which `Link`
                                // gives for free (and themed).
                                let resp = ui.link(egui::RichText::new(label).size(11.0));
                                if resp.clicked() {
                                    nav_to = Some((r.path.clone(), r.line));
                                    close = true;
                                }
                            }
                        });
                    ui.add_space(2.0);
                    if ui
                        .add(egui::Button::new(egui::RichText::new("Close").size(10.0)))
                        .clicked()
                    {
                        close = true;
                    }
                });
            });

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }

        if let Some((path, line)) = nav_to {
            if let Some(id) =
                crate::app::project_file_for_def(&path, &self.project_tree.user_src_files)
            {
                // Navigate the view the pill was clicked in. Jumping the MAIN
                // editor for a reference clicked in the second one would move
                // the file being read FROM out from under the reader.
                if self.ed_slot == crate::app::EditorSlot::Reference {
                    if let Some(p) = crate::editor::gui::text_pos::selected_file_rel_path(
                        &id,
                        &self.project_tree.user_src_files,
                    ) {
                        self.reference_file = Some(p);
                    }
                } else {
                    self.selected_file = id;
                }
                self.ed.pending_scroll_to_line = Some((id, line as usize + 1));
                self.ed.highlighted_def_line = Some((id, line as usize + 1));
            }
        }
        if close {
            self.ed.usages.open_popup = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LineIndex, LintSite, LintSpans, is_externally_invoked, is_externally_invoked_in,
        macro_refs_of, push_lint_sites, unused_import_ranges, unused_import_spans_in,
        unused_variable_range, unused_variable_ranges, usage_item, word_positions,
    };
    use crate::build::{BuildResult, Diagnostic};
    use crate::editor::gui::text_pos::{
        LineIndexCache, lsp_cursor_pos, lsp_line_end_char_idx, lsp_pos_to_char_idx,
    };

    fn diag(line: u32, col: u32, message: &str) -> crate::build::Diagnostic {
        crate::build::Diagnostic {
            level: "warning".into(),
            message: message.into(),
            rendered: String::new(),
            file: Some("src/main.rs".into()),
            line: Some(line),
            col: Some(col),
            code: Some("unused_variables".into()),
            fixes: Vec::new(),
            rename: None,
        }
    }

    /// The fields `push_lint_sites` copies out of `d`.
    fn site(d: &Diagnostic) -> LintSite {
        LintSite {
            message: d.message.clone(),
            line: d.line,
            col: d.col,
        }
    }

    /// One diagnostic's import spans, through the per-site helper.
    fn import_ranges(d: &Diagnostic, text: &str) -> Vec<(usize, usize)> {
        let chars: Vec<char> = text.chars().collect();
        unused_import_ranges(&site(d), &LineIndex::new(text), &chars)
    }

    /// One diagnostic's variable fade span, through the per-site helper.
    fn variable_range(d: &Diagnostic, text: &str) -> Option<(usize, usize)> {
        unused_variable_range(&site(d), &LineIndex::new(text))
    }

    // ── unused imports ───────────────────────────────────────────────────────

    fn imp(line: u32, col: u32, message: &str) -> crate::build::Diagnostic {
        crate::build::Diagnostic {
            level: "warning".into(),
            message: message.into(),
            rendered: String::new(),
            file: Some("src/main.rs".into()),
            line: Some(line),
            col: Some(col),
            code: Some("unused_imports".into()),
            fixes: Vec::new(),
            rename: None,
        }
    }

    fn spans(text: &str, d: &crate::build::Diagnostic) -> Vec<String> {
        import_ranges(d, text)
            .into_iter()
            .map(|(s, e)| text.chars().skip(s).take(e - s).collect())
            .collect()
    }

    /// The case from the report: one name inside a brace list. rustc points its
    /// primary span at that name.
    #[test]
    fn one_name_inside_a_brace_list() {
        let text = "use embedded_io_async::{Read, ReadExactError, Write};\n";
        let d = imp(1, 31, "unused import: `ReadExactError`");
        assert_eq!(spans(text, &d), ["ReadExactError"]);
    }

    /// `Read` must not land on the first four letters of `ReadExactError`.
    #[test]
    fn a_name_that_prefixes_its_neighbour_lands_on_itself() {
        let text = "use embedded_io_async::{Read, ReadExactError, Write};\n";
        let d = imp(1, 25, "unused import: `Read`");
        let got = import_ranges(&d, text);
        assert_eq!(got.len(), 1, "{got:?}");
        let (s, e) = got[0];
        assert_eq!(text.chars().skip(s).take(e - s).collect::<String>(), "Read");
        // …and it is the standalone one, not the prefix of the longer name.
        assert_eq!(s, text.find("Read, ").expect("the standalone one"));
    }

    /// A whole unused `use`: the backticked text is the PATH, and rustc's
    /// column points at `use`, not at the name.
    #[test]
    fn a_whole_unused_use_is_found_from_its_path() {
        let text = "use core::fmt::Write;\nfn main() {}\n";
        let d = imp(1, 1, "unused import: `core::fmt::Write`");
        assert_eq!(spans(text, &d), ["core::fmt::Write"]);
    }

    /// Plural: several names in ONE diagnostic.
    #[test]
    fn every_name_in_a_plural_message_counts() {
        let text = "use a::{Read, Write, Seek};\n";
        let d = imp(1, 9, "unused imports: `Read`, `Seek`");
        assert_eq!(spans(text, &d), ["Read", "Seek"]);
    }

    /// A brace list broken over several lines: the primary span is the `use` on
    /// line 1 while the name sits further down. The unique-occurrence fallback
    /// is what finds it — and an unused name occurs exactly once, in the import
    /// that introduced it.
    #[test]
    fn a_multiline_brace_list_falls_back_to_the_unique_occurrence() {
        let text = "use a::{\n    Read,\n    Write,\n};\nfn main() { let _ = Read; }\n";
        // `Write` appears once (only in the import) -> found.
        let d = imp(1, 1, "unused import: `Write`");
        assert_eq!(spans(text, &d), ["Write"]);
    }

    /// Two occurrences and the fallback declines rather than guessing which.
    #[test]
    fn the_fallback_gives_up_when_the_name_is_not_unique() {
        let text = "use a::{\n    Write,\n};\n// Write is mentioned here too\n";
        let d = imp(1, 1, "unused import: `Write`");
        assert!(import_ranges(&d, text).is_empty());
    }

    /// A Build/Clippy result gone stale after an edit must stop applying, not
    /// mark the wrong span — the same silent-stop the variable fade has.
    #[test]
    fn a_name_no_longer_in_the_file_marks_nothing() {
        let text = "use a::{Read, Write};\n";
        let d = imp(1, 15, "unused import: `Seek`");
        assert!(import_ranges(&d, text).is_empty());
    }

    #[test]
    fn a_message_without_backticks_marks_nothing() {
        let text = "use a::Read;\n";
        assert!(import_ranges(&imp(1, 1, "unused import"), text).is_empty());
    }

    #[test]
    fn a_diagnostic_without_a_line_marks_nothing() {
        let mut d = imp(1, 1, "unused import: `Read`");
        d.line = None;
        assert!(import_ranges(&d, "use a::Read;\n").is_empty());
    }

    #[test]
    fn unused_variable_range_matches_live_position() {
        let text = "fn main() {\n    let x = 100;\n}\n";
        // rustc reports 1-based line 2, col 9 (the `x`).
        let d = diag(2, 9, "unused variable: `x`");
        let (start, end) = variable_range(&d, text).expect("position still matches");
        let got: String = text.chars().skip(start).take(end - start).collect();
        assert_eq!(got, "x");
    }

    #[test]
    fn unused_variable_range_rejects_stale_position() {
        // The diagnostic was computed for `x`, but the live text at that
        // position now reads something else — must not fade the wrong span.
        let text = "fn main() {\n    let y = 100;\n}\n";
        let d = diag(2, 9, "unused variable: `x`");
        assert!(variable_range(&d, text).is_none());
    }

    #[test]
    fn unused_variable_range_needs_a_quoted_name() {
        let d = diag(1, 1, "no backticks here");
        assert!(variable_range(&d, "let x = 1;").is_none());
    }

    #[test]
    fn plain_fn_main_is_excluded_by_name_alone() {
        // No attribute at all — `fn main` is still never called from source.
        let text = "fn main() {\n    loop {}\n}\n";
        assert!(is_externally_invoked("main", 0, text));
    }

    #[test]
    fn entry_annotated_main_is_excluded() {
        let text = "#[entry]\nfn main() -> ! {\n    loop {}\n}\n";
        assert!(is_externally_invoked("main", 1, text));
    }

    #[test]
    fn interrupt_handler_is_excluded() {
        let text = "#[interrupt]\nfn USART1() {\n    // handle it\n}\n";
        assert!(is_externally_invoked("USART1", 1, text));
    }

    #[test]
    fn stacked_attributes_still_find_the_marker() {
        // #[allow(...)] sits between the real marker and the fn line.
        let text = "#[interrupt]\n#[allow(non_snake_case)]\nfn TIM2() {}\n";
        assert!(is_externally_invoked("TIM2", 2, text));
    }

    #[test]
    fn ordinary_function_is_not_excluded() {
        let text = "pub fn helper() -> u8 {\n    42\n}\n";
        assert!(!is_externally_invoked("helper", 0, text));
    }

    #[test]
    fn unrelated_attribute_does_not_exclude() {
        // #[allow(dead_code)] is not in the marker list — a genuinely unused
        // fn wearing it must still be eligible for fading.
        let text = "#[allow(dead_code)]\nfn unused_helper() {}\n";
        assert!(!is_externally_invoked("unused_helper", 1, text));
    }

    #[test]
    fn doc_comment_between_attribute_and_fn_is_skipped() {
        let text = "#[no_mangle]\n/// Doc comment.\npub extern \"C\" fn ffi_thing() {}\n";
        assert!(is_externally_invoked("ffi_thing", 2, text));
    }

    /// `poll_usages` splits the file once and asks about every symbol with the
    /// same lines, including a symbol whose start line is past the end.
    #[test]
    fn one_collected_line_list_serves_every_symbol() {
        let text = "#[interrupt]\nfn TIM2() {}\n\npub fn helper() {}\n#[test]\nfn t() {}\n";
        let lines: Vec<&str> = text.lines().collect();
        let asked = [
            ("TIM2", 1, true),
            ("helper", 3, false),
            ("t", 5, true),
            ("main", 3, true),
            // Past the end the scan starts at the last line, which is code.
            ("gone", 40, false),
        ];
        for (name, start_line, want) in asked {
            assert_eq!(
                is_externally_invoked_in(name, start_line, &lines),
                want,
                "{name}"
            );
            assert_eq!(
                is_externally_invoked(name, start_line, text),
                want,
                "{name}"
            );
        }
    }

    // ── item positions ───────────────────────────────────────────────────────

    /// Texts whose shape decides a clamp: empty, trailing newline or not, CRLF,
    /// multi-byte BMP chars and astral chars (two UTF-16 units each), plus
    /// deterministic pseudo-random mixes of them.
    fn position_texts() -> Vec<String> {
        const NAMED: &[&str] = &[
            "",
            "\n",
            "a",
            "a\n",
            "ab\ncd",
            "a\r\nb\r\n",
            "ăîș\nțâ",
            "x😀y\n😀",
            "fn main() {\n    let x = 1;\n}\n",
            "impl Foo {\n    fn ƒ(&self) -> 𝄞 {}\n}",
        ];
        const ALPHABET: &[char] = &['a', ' ', '\n', '\n', '\r', 'ă', '😀'];
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let generated = (0..60).map(|_| {
            let len = (next() % 24) as usize;
            (0..len)
                .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
                .collect::<String>()
        });
        NAMED
            .iter()
            .map(|s| (*s).to_owned())
            .chain(generated)
            .collect()
    }

    /// The positions stored once per reply are exactly what the overlays used to
    /// convert every frame, on the same text: past-EOF lines, columns past the
    /// line end and UTF-16 columns included. Each field gets different inputs,
    /// so a crossed wire between them fails too.
    #[test]
    fn stored_positions_match_the_per_frame_conversions() {
        for text in position_texts() {
            let index = LineIndex::new(&text);
            assert_eq!(index.total_chars(), text.chars().count(), "{text:?}");
            let lines = text.split('\n').count() as u32 + 2;
            let widest = text
                .split('\n')
                .map(|l| l.encode_utf16().count())
                .max()
                .unwrap_or(0) as u32
                + 3;
            for line in 0..=lines {
                for col in 0..=widest {
                    let s = crate::lsp::SymbolInfo {
                        name: "item".into(),
                        kind: 12,
                        start_line: lines - line,
                        start_char: widest - col,
                        end_line: line / 2,
                        end_char: col * 2,
                        sel_line: line,
                        sel_char: col,
                        in_trait_impl: false,
                        container: None,
                    };
                    let item = usage_item(s.clone(), &index, None, Vec::new());
                    let at = |l: u32, c: u32| lsp_pos_to_char_idx(&text, l + 1, c + 1);
                    let ctx = format!("{text:?} line {line} col {col}");
                    assert_eq!(item.sel_ci, at(s.sel_line, s.sel_char), "sel {ctx}");
                    assert_eq!(
                        item.eol_ci,
                        lsp_line_end_char_idx(&text, s.sel_line + 1),
                        "eol {ctx}"
                    );
                    let (start, end) = (at(s.start_line, s.start_char), at(s.end_line, s.end_char));
                    assert_eq!(
                        (item.start_ci, item.end_ci.max(item.start_ci)),
                        (start, end.max(start)),
                        "fade range {ctx}"
                    );
                    assert_eq!(item.end_ci, end, "end {ctx}");
                }
            }
        }
    }

    /// Everything else is carried over from the symbol unchanged.
    #[test]
    fn usage_item_keeps_the_symbol_fields_and_references() {
        let s = crate::lsp::SymbolInfo {
            name: "run".into(),
            kind: 6,
            start_line: 1,
            start_char: 4,
            end_line: 3,
            end_char: 5,
            sel_line: 2,
            sel_char: 11,
            in_trait_impl: true,
            container: None,
        };
        let refs = vec![super::UsageRef {
            path: "/p/src/main.rs".into(),
            line: 7,
            via_macro: None,
        }];
        let item = usage_item(s, &LineIndex::new(""), Some(refs), Vec::new());
        assert_eq!((item.name.as_str(), item.kind), ("run", 6));
        assert_eq!((item.sel_line, item.sel_char), (2, 11));
        assert!(item.in_trait_impl);
        let got = item.references.expect("references kept");
        assert_eq!((got[0].path.as_str(), got[0].line), ("/p/src/main.rs", 7));
        // An empty text puts every position at 0.
        assert_eq!(
            (item.sel_ci, item.eol_ci, item.start_ci, item.end_ci),
            (0, 0, 0, 0)
        );
    }

    fn symbol(
        name: &str,
        kind: u8,
        line: u32,
        col: u32,
        container: Option<(u8, &str)>,
    ) -> crate::lsp::SymbolInfo {
        crate::lsp::SymbolInfo {
            name: name.into(),
            kind,
            start_line: line,
            start_char: 0,
            end_line: line,
            end_char: col + name.len() as u32,
            sel_line: line,
            sel_char: col,
            in_trait_impl: false,
            container: container.map(|(k, n)| (k, n.to_owned())),
        }
    }

    /// rust-analyzer's empty answer plus two uses read from a macro body: the
    /// item has two sites, so it is neither faded nor pill-less — and nothing
    /// shows before rust-analyzer has answered.
    #[test]
    fn macro_uses_join_rust_analyzers_answer() {
        let mac = |line| super::UsageRef {
            path: "/w/src/menu.rs".into(),
            line,
            via_macro: Some("mode_menu".into()),
        };
        let s = symbol("value", 12, 0, 0, Some((19, "Node")));
        let mut item = usage_item(s, &LineIndex::new(""), None, vec![mac(9), mac(3)]);
        assert!(item.references.is_none(), "still resolving draws nothing");
        item.ra_refs = Some(Vec::new());
        item.merge();
        let lines: Vec<u32> = item
            .references
            .as_ref()
            .unwrap()
            .iter()
            .map(|r| r.line)
            .collect();
        assert_eq!(lines, [3, 9]);
    }

    /// The report, end to end: `Node::value`, used only in an invoked
    /// macro's body, gets that body's line as a site under the workspace; the
    /// macro itself gets its invocation inside another invoked macro.
    #[test]
    fn the_reported_function_gets_its_macro_body_site() {
        let text = "impl Node {\n    pub const fn value() -> Self { todo!() }\n}\n\
                    macro_rules! menu { () => { static A: Node = Node::value(); } }\n\
                    menu!();\n";
        let rel = "src/menu.rs";
        let index = LineIndex::new(text);
        let macros = super::super::macro_uses::MacroIndex::scan([(rel, text)]);
        let ws = std::path::Path::new("/w");
        let value = symbol("value", 12, 1, 17, Some((19, "Node")));
        let refs = macro_refs_of(&value, &index, "", &macros, ws);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 3);
        assert_eq!(refs[0].via_macro.as_deref(), Some("menu"));
        assert!(
            refs[0].path.replace('\\', "/").ends_with("/w/src/menu.rs"),
            "{}",
            refs[0].path
        );
        let other = symbol("value", 12, 1, 17, Some((19, "Other")));
        assert!(macro_refs_of(&other, &index, "", &macros, ws).is_empty());
    }

    /// rust-analyzer's range starts at the doc comment: one that mentions
    /// `macro_rules` must not make a fn a macro (it would then match only
    /// `value!`), while a real `macro_rules!` still is one.
    #[test]
    fn a_doc_comment_does_not_make_a_function_a_macro() {
        let text = "impl Node {\n    /// Called from the menu macro_rules body.\n    \
                    pub const fn value() -> Self { todo!() }\n}\n\
                    macro_rules! menu { () => { static A: Node = Node::value(); } }\n\
                    macro_rules! outer { () => { menu!(); } }\n\
                    outer!();\n";
        let index = LineIndex::new(text);
        let macros = super::super::macro_uses::MacroIndex::scan([("src/menu.rs", text)]);
        let ws = std::path::Path::new("/w");
        let mut value = symbol("value", 12, 2, 17, Some((19, "Node")));
        value.start_line = 1; // the `///` line, as rust-analyzer sends it
        assert_eq!(macro_refs_of(&value, &index, "", &macros, ws).len(), 1);
        let mut menu = symbol("menu", 12, 4, 13, None);
        menu.start_char = 0; // from `macro_rules!`
        let refs = macro_refs_of(&menu, &index, "", &macros, ws);
        assert_eq!(refs.len(), 1, "its `menu!` in outer");
        assert_eq!(refs[0].via_macro.as_deref(), Some("outer"));
    }

    // ── flycheck lint spans ──────────────────────────────────────────────────

    /// `unused_variable_range` as it was, verbatim: the whole text walked and
    /// collected per diagnostic.
    fn old_unused_variable_range(d: &Diagnostic, display_code: &str) -> Option<(usize, usize)> {
        let name = d.message.split('`').nth(1)?;
        if name.is_empty() {
            return None;
        }
        let start = lsp_pos_to_char_idx(display_code, d.line?, d.col?);
        let chars: Vec<char> = display_code.chars().collect();
        let end = (start + name.chars().count()).min(chars.len());
        if end <= start || chars[start..end].iter().collect::<String>() != name {
            return None;
        }
        Some((start, end))
    }

    /// `locate_import_name` as it was, verbatim.
    fn old_locate_import_name(
        display_code: &str,
        chars: &[char],
        needle: &[char],
        line_1: u32,
    ) -> Option<usize> {
        let line_start = lsp_pos_to_char_idx(display_code, line_1, 1);
        let line_end = lsp_line_end_char_idx(display_code, line_1);
        if let Some(&i) = word_positions(chars, needle, line_start, line_end).first() {
            return Some(i);
        }
        let all = word_positions(chars, needle, 0, chars.len());
        (all.len() == 1).then(|| all[0])
    }

    /// `unused_import_ranges` as it was, verbatim.
    fn old_unused_import_ranges(d: &Diagnostic, display_code: &str) -> Vec<(usize, usize)> {
        let Some(line) = d.line else {
            return Vec::new();
        };
        let chars: Vec<char> = display_code.chars().collect();
        d.message
            .split('`')
            .enumerate()
            .filter(|(i, part)| i % 2 == 1 && !part.is_empty())
            .filter_map(|(_, name)| {
                let needle: Vec<char> = name.chars().collect();
                old_locate_import_name(display_code, &chars, &needle, line)
                    .map(|start| (start, start + needle.len()))
            })
            .collect()
    }

    /// The old `unused_import_spans` loop, over the results that were `Done`.
    fn old_import_spans(
        results: &[&BuildResult],
        rel_path: &str,
        text: &str,
    ) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for result in results {
            for d in &result.diagnostics {
                if d.file.as_deref() == Some(rel_path)
                    && d.code.as_deref() == Some("unused_imports")
                {
                    out.extend(old_unused_import_ranges(d, text));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The old `unused_variables` half of `usages_dead_ranges`, over the
    /// results that were `Done`.
    fn old_variable_ranges(
        results: &[&BuildResult],
        rel_path: &str,
        text: &str,
    ) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        for result in results {
            ranges.extend(
                result
                    .diagnostics
                    .iter()
                    .filter(|d| {
                        d.file.as_deref() == Some(rel_path)
                            && d.code.as_deref() == Some("unused_variables")
                    })
                    .filter_map(|d| old_unused_variable_range(d, text)),
            );
        }
        ranges
    }

    /// The `app::AppIde::lint_sites` copy-out, over the same results.
    fn sites_of(results: &[&BuildResult], rel_path: &str, code: &str) -> Vec<LintSite> {
        let mut sites = Vec::new();
        for result in results {
            push_lint_sites(&mut sites, result, rel_path, code);
        }
        sites
    }

    /// The import shapes rustc reports, then the word-walk texts: identifier
    /// and separator chars, CRLF, multi-byte and astral chars, empty texts.
    fn lint_texts() -> Vec<String> {
        const SHAPES: &[&str] = &[
            "use embedded_io_async::{Read, ReadExactError, Write};\n",
            "use core::fmt::Write;\nfn main() {}\n",
            "use a::{\n    Read,\n    Write,\n};\nfn main() { let _ = Read; }\n",
            "use a::{\r\n    Write,\r\n};\r\n// Write is mentioned here too\r\n",
            "fn main() {\n    let x = 100;\n    let 𝔸ă = 1;\n}\n",
        ];
        SHAPES
            .iter()
            .map(|s| (*s).to_owned())
            .chain(crate::editor::gui::text_pos::word_walk_tests::word_texts())
            .collect()
    }

    fn xorshift(mut state: u64) -> impl FnMut() -> u64 {
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        }
    }

    /// Diagnostics for `text`. Most names are cut from the text at the position
    /// reported, so a good share verify; the rest cover a stale position, a
    /// missing line or column, positions past the end, empty and plural names,
    /// other files and other lint codes.
    fn lint_diagnostics(text: &str, next: &mut impl FnMut() -> u64) -> Vec<Diagnostic> {
        const WORDS: &[&str] = &["a", "Z", "_7", "𝔸", "ă٣", "Read", ""];
        let chars: Vec<char> = text.chars().collect();
        let lines = text.split('\n').count() as u64;
        let widest = text
            .split('\n')
            .map(|l| l.encode_utf16().count())
            .max()
            .unwrap_or(0) as u64;
        (0..12)
            .map(|_| {
                let from = (next() % (chars.len() as u64 + 1)) as usize;
                let cut: String = chars
                    .iter()
                    .skip(from)
                    .take((next() % 5) as usize)
                    .collect();
                let word = WORDS[(next() % WORDS.len() as u64) as usize];
                let message = match next() % 6 {
                    0 | 1 => format!("unused variable: `{cut}`"),
                    2 => format!("unused import: `{cut}`"),
                    3 => format!("unused imports: `{cut}`, `{word}`"),
                    4 => format!("unused import: `{word}`"),
                    _ => "no backticks here".to_owned(),
                };
                let (line0, col0) = lsp_cursor_pos(text, from);
                let (line, col) = match next() % 6 {
                    0 => (None, Some(col0 + 1)),
                    1 => (Some(line0 + 1), None),
                    2 => (
                        Some((next() % (lines + 3)) as u32),
                        Some((next() % (widest + 4)) as u32),
                    ),
                    3 => (Some(u32::MAX), Some(u32::MAX)),
                    _ => (Some(line0 + 1), Some(col0 + 1)),
                };
                let file = match next() % 5 {
                    0 => None,
                    1 => Some("src/other.rs"),
                    _ => Some("src/main.rs"),
                };
                let code = match next() % 6 {
                    0 => None,
                    1 => Some("dead_code"),
                    2 | 3 => Some("unused_imports"),
                    _ => Some("unused_variables"),
                };
                Diagnostic {
                    level: "warning".into(),
                    message,
                    rendered: String::new(),
                    file: file.map(str::to_owned),
                    line,
                    col,
                    code: code.map(str::to_owned),
                    fixes: Vec::new(),
                    rename: None,
                }
            })
            .collect()
    }

    /// Per diagnostic, the line-index versions give exactly what the whole-text
    /// walks gave, for either lint shape on every text.
    #[test]
    fn per_site_spans_match_the_old_whole_text_walks() {
        let mut next = xorshift(0x2545_F491_4F6C_DD1D);
        let (mut variables, mut imports) = (0, 0);
        for text in lint_texts() {
            let index = LineIndex::new(&text);
            let chars: Vec<char> = text.chars().collect();
            for d in lint_diagnostics(&text, &mut next) {
                let ctx = format!("{text:?} {d:?}");
                let old = old_unused_variable_range(&d, &text);
                assert_eq!(unused_variable_range(&site(&d), &index), old, "{ctx}");
                variables += usize::from(old.is_some());
                let old = old_unused_import_ranges(&d, &text);
                assert_eq!(
                    unused_import_ranges(&site(&d), &index, &chars),
                    old,
                    "{ctx}"
                );
                imports += usize::from(!old.is_empty());
            }
        }
        // Enough of them verify for the comparison to mean something.
        assert!(variables > 1000, "{variables}");
        assert!(imports > 1000, "{imports}");
    }

    /// Per file, the memoized spans equal the old per-frame loops over both
    /// results, on a first call and on a repeat, with one memo and one line
    /// index cache shared across every text, file and result combination.
    /// Each text is also asked with the previous text's results, the way a
    /// result outlives an edit: same file and sites, different text.
    #[test]
    fn memoized_file_spans_match_the_old_per_frame_loops() {
        let mut next = xorshift(0x9E37_79B9_7F4A_7C15);
        let texts = lint_texts();
        // (Cargo Check result, Clippy result, which of them are `Done`) per text.
        let runs: Vec<(BuildResult, BuildResult, u64)> = texts
            .iter()
            .map(|text| {
                let mut diagnostics = lint_diagnostics(text, &mut next);
                let clippy = BuildResult {
                    success: true,
                    diagnostics: diagnostics.split_off(6),
                };
                let build = BuildResult {
                    success: true,
                    diagnostics,
                };
                (build, clippy, next() % 4)
            })
            .collect();
        let (mut vars_memo, mut imports_memo) = (LintSpans::default(), LintSpans::default());
        let mut cache = LineIndexCache::default();
        let (mut variables, mut imports) = (0, 0);
        for (i, text) in texts.iter().enumerate() {
            for (build, clippy, which) in [&runs[i.saturating_sub(1)], &runs[i]] {
                let done: Vec<&BuildResult> = match which {
                    0 => vec![build],
                    1 => vec![clippy],
                    2 => Vec::new(),
                    _ => vec![build, clippy],
                };
                for rel in ["src/main.rs", "src/other.rs"] {
                    let ctx = format!("{text:?} {rel} {} results", done.len());
                    let old = old_variable_ranges(&done, rel, text);
                    for _ in 0..2 {
                        let sites = sites_of(&done, rel, "unused_variables");
                        let got =
                            vars_memo.get(rel, sites, text, &mut cache, unused_variable_ranges);
                        assert_eq!(got, old, "variables {ctx}");
                    }
                    variables += usize::from(!old.is_empty());
                    let old = old_import_spans(&done, rel, text);
                    for _ in 0..2 {
                        let sites = sites_of(&done, rel, "unused_imports");
                        let got =
                            imports_memo.get(rel, sites, text, &mut cache, unused_import_spans_in);
                        assert_eq!(got, old, "imports {ctx}");
                    }
                    imports += usize::from(!old.is_empty());
                }
            }
        }
        assert!(variables > 100, "{variables}");
        assert!(imports > 200, "{imports}");
    }

    /// The memo derives again when the file, the sites or the text changes,
    /// and at no other time. No site derives nothing and leaves the memo alone.
    #[test]
    fn lint_spans_rederive_only_when_a_key_part_changes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DERIVED: AtomicUsize = AtomicUsize::new(0);
        fn counting(sites: &[LintSite], index: &LineIndex) -> Vec<(usize, usize)> {
            DERIVED.fetch_add(1, Ordering::SeqCst);
            unused_variable_ranges(sites, index)
        }
        let derived = || DERIVED.load(Ordering::SeqCst);
        let sites = |col: u32| {
            vec![LintSite {
                message: "unused variable: `x`".into(),
                line: Some(2),
                col: Some(col),
            }]
        };
        let (text, edited) = (
            "fn main() {\n    let x = 1;\n}\n",
            "fn main() {\n    let x = 2;\n}\n",
        );
        let mut memo = LintSpans::default();
        let mut cache = LineIndexCache::default();

        let (x, none) = (vec![(20, 21)], Vec::<(usize, usize)>::new());
        assert_eq!(
            memo.get("src/main.rs", sites(9), text, &mut cache, counting),
            x
        );
        assert_eq!(derived(), 1);
        assert_eq!(
            memo.get("src/main.rs", sites(9), text, &mut cache, counting),
            x
        );
        assert_eq!(derived(), 1);
        // The view's cache moving on to another text does not matter.
        cache.get("something else");
        assert_eq!(
            memo.get("src/main.rs", sites(9), text, &mut cache, counting),
            x
        );
        assert_eq!(derived(), 1);

        assert_eq!(
            memo.get("src/main.rs", sites(9), edited, &mut cache, counting),
            x
        );
        assert_eq!(derived(), 2);
        assert_eq!(
            memo.get("src/main.rs", sites(10), edited, &mut cache, counting),
            none
        );
        assert_eq!(derived(), 3);
        assert_eq!(
            memo.get("src/lib.rs", sites(10), edited, &mut cache, counting),
            none
        );
        assert_eq!(derived(), 4);

        assert_eq!(
            memo.get("src/lib.rs", Vec::new(), edited, &mut cache, counting),
            none
        );
        assert_eq!(derived(), 4);
        assert_eq!(
            memo.get("src/lib.rs", sites(10), edited, &mut cache, counting),
            none
        );
        assert_eq!(derived(), 4);
        // Both derivations give nothing for no site, so skipping them is exact.
        let index = LineIndex::new(text);
        assert!(unused_variable_ranges(&[], &index).is_empty());
        assert!(unused_import_spans_in(&[], &index).is_empty());
    }
}
