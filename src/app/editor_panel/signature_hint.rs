//! Call-signature ghost hint.
//!
//! When the caret is inside a call whose arguments are wrong — a mismatched
//! type, or too few or too many — the callee's parameters are drawn as dim
//! ghost text at the end of the line holding the call's `(`, the wrong
//! parameter in the error colour:
//!
//! ```text
//! pins::utils::i2c_display::write_text(    (display: &mut Ssd1306Async<…>, text: &str, x: i32, y: i32)
//! ```
//!
//! Three questions, three sources — and none of them changes rust-analyzer's
//! documents, so nothing here can cancel another request:
//!   * WHERE the call is: a text scan over the code mask ([`calls_around`]).
//!   * WHETHER its arguments are wrong: diagnostics, matched to the call by
//!     POSITION, never by message wording ([`fault_of`]). rust-analyzer's own
//!     are PULLED per document version — it never pushes them — and cargo
//!     check's are added while they still describe the text on screen.
//!   * WHAT the callee expects: `textDocument/signatureHelp`, asked inside the
//!     parentheses (on the callee's name rust-analyzer answers `null`).

use super::AppIde;
use crate::editor::gui::diagnostics_overlay::{inline_message_x, shorten_type};
use crate::editor::gui::text_pos::{LineIndex, lsp_cursor_pos};
use crate::lsp::{self, SignatureHelp, SignatureReply};
use eframe::egui;
use std::sync::Arc;

/// One call expression the scan found. Char indices into the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallSite {
    /// Where the callee begins: its name with any `a::b::` path before it,
    /// or a method's name (not the receiver — `a.b(x).c(y)` holds two calls,
    /// and a caret on `a` is in neither). The caret is "in" the call from here
    /// through the `)`.
    start: usize,
    /// One past the callee's last char — its name, or a turbofish's `>`.
    /// rustc ends its "arguments to this function are incorrect" span here.
    callee_end: usize,
    /// The `(` and its matching `)`.
    open: usize,
    close: usize,
    /// Each top-level argument, trimmed of whitespace and comments, as
    /// `[start, end)` — the span rustc and rust-analyzer give a mismatched
    /// argument.
    args: Vec<(usize, usize)>,
    /// A hash of the call's text, callee through `)` — what decides whether a
    /// held ghost still describes it. A hash, not the text: the outermost call
    /// can be a whole `executor.run(|s| { … })` body.
    key: u64,
}

/// What is wrong with a call's arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Fault {
    /// Too few or too many arguments.
    count: bool,
    /// The arguments a type error points at, by index, ascending.
    bad_args: Vec<usize>,
}

/// One diagnostic, reduced to what attribution reads: its code, its span as
/// char indices into the text, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiagSpan {
    code: String,
    start: usize,
    end: usize,
    /// rust-analyzer's own (pulled), rather than cargo check's. Only cargo
    /// check labels innocent spans: "this argument has type `u8`…" sits
    /// exactly on a correctly passed argument when it is where a type was
    /// INFERRED from.
    pulled: bool,
}

/// Codes that can describe a call's arguments. Everything else inside a call
/// (an unknown method on an argument, a borrow error) is not about the call.
const ARG_CODES: [&str; 5] = ["E0308", "E0061", "E0060", "E0107", "E0277"];

/// Words that are followed by a `(` without calling anything.
const KEYWORDS: [&str; 28] = [
    "if", "while", "match", "return", "in", "for", "loop", "else", "let", "mut", "ref", "move",
    "async", "await", "unsafe", "where", "impl", "dyn", "as", "break", "continue", "yield",
    "const", "static", "fn", "pub", "use", "box",
];

/// Words that DECLARE the name after them: `fn name(`, `struct Name(` …
const DECLARES: [&str; 6] = ["fn", "struct", "enum", "union", "trait", "type"];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The last code char before `at`, skipping whitespace and comments.
fn prev_code(chars: &[char], mask: &[bool], at: usize) -> Option<usize> {
    (0..at)
        .rev()
        .find(|&i| mask[i] && !chars[i].is_whitespace())
}

/// Where the identifier ending at `end` (inclusive) starts.
fn ident_start(chars: &[char], end: usize) -> usize {
    let mut s = end;
    while s > 0 && is_ident(chars[s - 1]) {
        s -= 1;
    }
    s
}

/// `word_start` moved back over an `a::b::` path in front of it.
fn path_start(chars: &[char], word_start: usize) -> usize {
    let mut s = word_start;
    while s >= 3 && chars[s - 1] == ':' && chars[s - 2] == ':' && is_ident(chars[s - 3]) {
        s = ident_start(chars, s - 3);
    }
    s
}

/// How far back [`turbofish_open`] looks. A generic list is short; past this
/// the `>` is a shift or a comparison, and walking on to the start of a long
/// statement for every `>> (` in a lookup table made each scan quadratic.
const TURBOFISH_SCAN: usize = 512;

/// The `<` matching the turbofish `>` at `gt`, or `None` when that `>` closes
/// no generic list — the arrow of `=>` / `->`, or a comparison.
fn turbofish_open(chars: &[char], mask: &[bool], gt: usize) -> Option<usize> {
    if gt > 0 && matches!(chars[gt - 1], '-' | '=') {
        return None;
    }
    // `nest`: how many `]` / `}` the walk back is inside — an array type's
    // `[u8; 4]` or a const generic's `{ N + 1 }` belong to the list.
    let (mut depth, mut nest) = (0usize, 0usize);
    for i in (gt.saturating_sub(TURBOFISH_SCAN)..=gt).rev() {
        if !mask[i] {
            continue;
        }
        match chars[i] {
            ']' | '}' => nest += 1,
            // An unmatched `[` / `{` is the enclosing index or block.
            '[' | '{' => {
                if nest == 0 {
                    return None;
                }
                nest -= 1;
            }
            // A generic list never spans a statement.
            ';' if nest == 0 => return None,
            _ if nest > 0 => {}
            '>' if i > 0 && matches!(chars[i - 1], '-' | '=') => {}
            '>' => depth += 1,
            '<' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `(start, callee_end)` of the callee whose call opens at `open`, or `None`
/// when this `(` calls nothing: grouping or tuple parentheses, a macro's
/// `!(`, a declaration's `fn name(`, `if (` …
fn callee_before(chars: &[char], mask: &[bool], open: usize) -> Option<(usize, usize)> {
    let j = prev_code(chars, mask, open)?;
    match chars[j] {
        c if is_ident(c) => {
            let ws = ident_start(chars, j);
            if chars[ws].is_ascii_digit() {
                return None; // a number, `2(` — nothing callable
            }
            let word: String = chars[ws..=j].iter().collect();
            if KEYWORDS.contains(&word.as_str()) {
                return None;
            }
            if let Some(k) = prev_code(chars, mask, ws)
                && is_ident(chars[k])
            {
                let before: String = chars[ident_start(chars, k)..=k].iter().collect();
                if DECLARES.contains(&before.as_str()) {
                    return None;
                }
            }
            Some((path_start(chars, ws), j + 1))
        }
        // `name::<T>(` — only a TURBOFISH; a `<` without `::` before it is a
        // generic declaration (`fn f<T>(`) or a comparison (`a > (b)`).
        '>' => {
            let lt = turbofish_open(chars, mask, j)?;
            if lt < 3 || chars[lt - 1] != ':' || chars[lt - 2] != ':' || !is_ident(chars[lt - 3]) {
                return None;
            }
            Some((path_start(chars, ident_start(chars, lt - 3)), j + 1))
        }
        // `f(a)(b)`, `handlers[i](x)`: the caret is in the call only inside its
        // own parentheses.
        ')' | ']' => Some((open, j + 1)),
        _ => None,
    }
}

/// `(open, close)` char indices of a matched bracket pair.
type Pair = (usize, usize);

/// Matched `()` pairs, and the `[ … ]` spans of attributes (`#[…]`, `#![…]`),
/// over code only. Each bracket kind keeps its own stack, so a stray `]` in
/// half-typed code cannot unmatch every paren after it.
fn bracket_pairs(chars: &[char], mask: &[bool]) -> (Vec<Pair>, Vec<Pair>) {
    let (mut parens, mut attrs) = (Vec::new(), Vec::new());
    let (mut ps, mut bs) = (Vec::new(), Vec::new());
    for (i, &c) in chars.iter().enumerate() {
        if !mask[i] {
            continue;
        }
        match c {
            '(' => ps.push(i),
            ')' => {
                if let Some(o) = ps.pop() {
                    parens.push((o, i));
                }
            }
            '[' => bs.push(i),
            ']' => {
                if let Some(o) = bs.pop() {
                    let hash = (o >= 1 && chars[o - 1] == '#')
                        || (o >= 2 && chars[o - 1] == '!' && chars[o - 2] == '#');
                    if hash {
                        attrs.push((o, i));
                    }
                }
            }
            _ => {}
        }
    }
    (parens, attrs)
}

/// `[s, e)` with leading and trailing whitespace and comments dropped.
///
/// A comment is skipped by its own delimiters, not by the mask: a string or
/// char literal right against it (`/*a*/"x"`) is non-code too, and belongs to
/// the argument.
fn trim_span(chars: &[char], mask: &[bool], mut s: usize, mut e: usize) -> (usize, usize) {
    let at = |i: usize, pat: &str| {
        pat.chars()
            .enumerate()
            .all(|(k, c)| chars.get(i + k) == Some(&c))
    };
    loop {
        while s < e && chars[s].is_whitespace() {
            s += 1;
        }
        if s < e && !mask[s] && at(s, "//") {
            while s < e && chars[s] != '\n' {
                s += 1;
            }
            continue;
        }
        if s < e && !mask[s] && at(s, "/*") {
            let mut k = s + 2;
            while k + 1 < e && !at(k, "*/") {
                k += 1;
            }
            s = (k + 2).min(e);
            continue;
        }
        break;
    }
    loop {
        while e > s && chars[e - 1].is_whitespace() {
            e -= 1;
        }
        if e > s && !mask[e - 1] {
            // A block comment ending the argument: back to its `/*`.
            if e >= s + 4
                && at(e - 2, "*/")
                && let Some(k) = (s..e - 3).rev().find(|&k| at(k, "/*") && !mask[k])
            {
                e = k;
                continue;
            }
            // A line comment: the non-code run it ends, when that run starts
            // with `//` (a run starting with a string is the argument's own).
            let mut k = e - 1;
            while k > s && !mask[k - 1] {
                k -= 1;
            }
            if at(k, "//") {
                e = k;
                continue;
            }
        }
        break;
    }
    (s, e)
}

/// The top-level arguments between `open` and `close`. Commas inside nested
/// brackets, a turbofish's `::<A, B>` or a closure's `|a, b|` parameter list
/// separate nothing.
fn split_args(chars: &[char], mask: &[bool], open: usize, close: usize) -> Vec<(usize, usize)> {
    let mut args = Vec::new();
    let (mut depth, mut angle) = (0i32, 0i32);
    let mut arg_start = open + 1;
    let mut i = open + 1;
    while i < close {
        if !mask[i] {
            i += 1;
            continue;
        }
        match chars[i] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            // A turbofish opens with `::<`; inside it every `<` nests
            // (`::<Foo<u8>, u32>`).
            '<' if angle > 0 || (i >= 2 && chars[i - 1] == ':' && chars[i - 2] == ':') => {
                angle += 1
            }
            '>' if angle > 0 && !matches!(chars[i - 1], '-' | '=') => angle -= 1,
            // A closure's parameter list, when nothing but `move` / `async`
            // came before it in this argument: skip to its closing `|`.
            '|' if depth == 0 && angle == 0 => {
                let before: String = (arg_start..i)
                    .filter(|&k| mask[k])
                    .map(|k| chars[k])
                    .collect();
                if before
                    .split_whitespace()
                    .all(|w| w == "move" || w == "async")
                {
                    let end = (i + 1..close).find(|&k| mask[k] && chars[k] == '|');
                    i = end.map_or(close, |k| k + 1);
                    continue;
                }
            }
            ',' if depth == 0 && angle == 0 => {
                args.push(trim_span(chars, mask, arg_start, i));
                arg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let last = trim_span(chars, mask, arg_start, close);
    if last.0 < last.1 {
        args.push(last);
    }
    args
}

/// Every call whose span — callee through `)` — holds `caret`, innermost
/// first. A caret right before the `)` is inside; right after it is not.
pub(crate) fn calls_around(chars: &[char], caret: usize) -> Vec<CallSite> {
    let mask = crate::rust_lex::code_mask(chars);
    let (parens, attrs) = bracket_pairs(chars, &mask);
    let mut calls: Vec<CallSite> = parens
        .into_iter()
        .filter(|&(_, close)| caret <= close)
        .filter_map(|(open, close)| {
            let (start, callee_end) = callee_before(chars, &mask, open)?;
            if caret < start || attrs.iter().any(|&(a, b)| a < open && open < b) {
                return None;
            }
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::hash::DefaultHasher::new();
                chars[start..=close].hash(&mut h);
                h.finish()
            };
            Some(CallSite {
                start,
                callee_end,
                open,
                close,
                args: split_args(chars, &mask, open, close),
                key,
            })
        })
        .collect();
    calls.sort_by_key(|c| c.close - c.start);
    calls
}

/// What the diagnostics in `diags` say is wrong with `call`'s arguments —
/// `None` when none of them is about this call.
///
/// Matched by POSITION, never by wording:
///   * cargo check ends an E0308 / E0061 / E0060 span exactly where the
///     callee ends ("arguments to this function are incorrect", "this function
///     takes 4 arguments but 3 …"), or spans `(` … `)` for a missing argument;
///   * a type error on an argument spans exactly that argument — accepted
///     from cargo check only when it also flagged the callee, because its
///     "this argument has type `u8`…" label sits on an innocent argument that
///     merely fixed an inferred type;
///   * rust-analyzer's own count error (E0107) ends at the `)`.
///
/// So an error INSIDE an argument (a closure's body, an unknown method) is
/// not the call's, and neither is a mismatched RETURN type, whose span covers
/// the whole call, nor an E0277 that ends at a method's name (`.into()`'s
/// own bound, not what was passed).
pub(crate) fn fault_of(call: &CallSite, diags: &[DiagSpan]) -> Option<Fault> {
    let mut fault = Fault::default();
    let mut hit = false;
    let callee_flagged = diags
        .iter()
        .any(|d| matches!(d.code.as_str(), "E0308" | "E0277") && d.end == call.callee_end);
    for d in diags {
        let at_callee = d.end == call.callee_end;
        let whole_parens = d.start == call.open && d.end == call.close + 1;
        let arg = call
            .args
            .iter()
            .position(|&(s, e)| s == d.start && e == d.end);
        match d.code.as_str() {
            "E0061" | "E0060" if at_callee || whole_parens => {
                fault.count = true;
                hit = true;
            }
            // From the `)` alone (too few), the first extra argument (too
            // many), or the whole `( … )` inside a `#[entry]` fn — measured.
            "E0107" if d.end == call.close + 1 && d.start >= call.open => {
                fault.count = true;
                hit = true;
            }
            // rustc's "arguments to this function are incorrect".
            "E0308" if at_callee => hit = true,
            "E0308" | "E0277" => {
                if let Some(i) = arg
                    && (d.pulled || callee_flagged)
                {
                    hit = true;
                    if !fault.bad_args.contains(&i) {
                        fault.bad_args.push(i);
                    }
                }
            }
            _ => {}
        }
    }
    fault.bad_args.sort_unstable();
    hit.then_some(fault)
}

/// The innermost call around the caret that something is wrong with — not
/// merely the innermost call: with the caret inside `inner(…)` while it is
/// `outer` that is called wrongly, `outer`'s signature is the one wanted.
pub(crate) fn faulty_call<'a>(
    calls: &'a [CallSite],
    diags: &[DiagSpan],
) -> Option<(&'a CallSite, Fault)> {
    calls
        .iter()
        .find_map(|c| fault_of(c, diags).map(|f| (c, f)))
}

/// `label`'s parameter `p` split into its pattern and its type, at the first
/// top-level `:` that is not half of a `::`. `self` / `&mut self` have none.
fn split_param(p: &str) -> (&str, Option<&str>) {
    let chars: Vec<(usize, char)> = p.char_indices().collect();
    let mut depth = 0i32;
    for (k, &(b, c)) in chars.iter().enumerate() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ':' if depth == 0 => {
                let colon_pair = chars.get(k + 1).is_some_and(|&(_, n)| n == ':')
                    || (k > 0 && chars[k - 1].1 == ':');
                if !colon_pair {
                    return (p[..b].trim_end(), Some(p[b + 1..].trim_start()));
                }
            }
            _ => {}
        }
    }
    (p, None)
}

/// One run of ghost text, and whether it is drawn in the error colour.
pub(crate) type Segment = (String, bool);

/// The ghost text for `help`, the most informative form that fits in `room`
/// characters — or `None` when not even `(…)` does.
///
/// Degrades in steps rather than cutting: whole parameters, then types
/// collapsed from the inside out, then names only with the wrong parameter
/// still typed, then the wrong parameter alone, then `(…)`. A count error is
/// led by `3/4 args`, in the error colour.
pub(crate) fn ghost_text(
    help: &SignatureHelp,
    fault: &Fault,
    supplied: usize,
    room: usize,
) -> Option<Vec<Segment>> {
    let label: Vec<char> = help.label.chars().collect();
    let params: Vec<String> = help
        .params
        .iter()
        .map(|&(s, e)| {
            label[s.min(label.len())..e.min(label.len())]
                .iter()
                .collect()
        })
        .collect();
    let expected = params.len();
    let mut bad: Vec<bool> = vec![false; expected];
    for &i in &fault.bad_args {
        if let Some(b) = bad.get_mut(i) {
            *b = true;
        }
    }
    if fault.count {
        for b in bad.iter_mut().skip(supplied) {
            *b = true;
        }
    }
    let prefix: Option<Segment> = fault
        .count
        .then(|| (format!("{supplied}/{expected} args "), true));

    // One parameter rendered with its type collapsed to `budget` (None:
    // name only).
    let param = |i: usize, budget: Option<usize>| -> String {
        let (name, ty) = split_param(&params[i]);
        match (ty, budget) {
            (Some(ty), Some(b)) => format!("{name}: {}", shorten_type(ty, b)),
            (Some(_), None) => name.to_owned(),
            (None, _) => params[i].clone(),
        }
    };
    let assemble = |parts: Vec<Segment>| -> Vec<Segment> {
        let mut out: Vec<Segment> = prefix.iter().cloned().collect();
        out.push(("(".to_owned(), false));
        for (k, part) in parts.into_iter().enumerate() {
            if k > 0 {
                out.push((", ".to_owned(), false));
            }
            out.push(part);
        }
        out.push((")".to_owned(), false));
        out
    };
    let fits =
        |segs: &Vec<Segment>| segs.iter().map(|(t, _)| t.chars().count()).sum::<usize>() <= room;

    // Every parameter, at three type budgets and in full.
    let mut tries: Vec<Vec<Segment>> = Vec::new();
    for budget in [usize::MAX, 40, 24, 14] {
        tries.push(assemble(
            (0..expected)
                .map(|i| {
                    (
                        param(i, Some(budget.max(if bad[i] { 24 } else { 0 }))),
                        bad[i],
                    )
                })
                .collect(),
        ));
    }
    // Names only; the wrong parameters keep a short type.
    tries.push(assemble(
        (0..expected)
            .map(|i| (param(i, bad[i].then_some(24)), bad[i]))
            .collect(),
    ));
    // Only the wrong parameters, every run of the others one `…`.
    if bad.iter().any(|&b| b) {
        let mut parts: Vec<Segment> = Vec::new();
        for (i, &is_bad) in bad.iter().enumerate() {
            if is_bad {
                parts.push((param(i, Some(16)), true));
            } else if parts.last().is_none_or(|(t, _)| t != "…") {
                parts.push(("…".to_owned(), false));
            }
        }
        tries.push(assemble(parts));
    }
    tries.push(assemble(vec![("…".to_owned(), false)]));
    tries.into_iter().find(fits)
}

/// The last caret scan of one view: which text and caret it read, and the
/// calls around that caret. Finding them lexes the whole file, and the text
/// and the caret rarely change between frames.
pub(crate) struct SigScan {
    text: Arc<LineIndex>,
    caret: usize,
    calls: Vec<CallSite>,
}

/// One view's signature-hint state.
#[derive(Default)]
pub(crate) struct SigState {
    /// The token of the request in flight or last answered.
    asked: Option<u64>,
    /// Whether `asked` has been answered, and with what.
    answer: Option<Option<SignatureHelp>>,
    /// The last ghost drawn. Held while the picture is incomplete — the file
    /// out of sync, a pull in flight, cargo check running — as long as the
    /// same file's call reads the same, so the ghost neither blinks on every
    /// Save nor vanishes the moment you type elsewhere in the file.
    shown: Option<Shown>,
}

/// A ghost that was drawn, and what it was drawn for.
#[derive(Clone)]
struct Shown {
    rel: String,
    /// [`CallSite::key`] of the call it described.
    key: u64,
    fault: Fault,
    help: SignatureHelp,
}

/// What [`AppIde::update_signature_hint`] wants drawn this frame.
pub(crate) struct SigGhost {
    /// 1-based line of the call's `(` — where the ghost goes, and whose
    /// inline diagnostic message it replaces.
    pub(crate) line: u32,
    /// Buffer char index of that line's end.
    pub(crate) eol_idx: usize,
    pub(crate) help: SignatureHelp,
    pub(crate) fault: Fault,
    /// How many arguments the call passes.
    pub(crate) supplied: usize,
}

impl AppIde {
    /// Find the wrongly called function around the caret and return what to
    /// draw for it, asking rust-analyzer for whatever is missing. `rel` is the
    /// file's workspace path; `tracked` whether rust-analyzer analyses it.
    pub(super) fn update_signature_hint(
        &mut self,
        line_index: &Arc<LineIndex>,
        caret: Option<usize>,
        rel: Option<&str>,
        tracked: bool,
        slot: crate::app::EditorSlot,
    ) -> Option<SigGhost> {
        // One switch for both ghost hints: the "Types" toolbar button.
        let (true, true, Some(caret), Some(rel)) = (self.inlay_types_enabled, tracked, caret, rel)
        else {
            self.ed.sig = SigState::default();
            return None;
        };
        let text = line_index.text();
        let scan_fresh = self.ed.sig_scan.as_ref().is_some_and(|s| {
            s.caret == caret && (Arc::ptr_eq(&s.text, line_index) || s.text.text() == text)
        });
        if !scan_fresh {
            let chars: Vec<char> = text.chars().collect();
            self.ed.sig_scan = Some(SigScan {
                text: Arc::clone(line_index),
                caret,
                calls: calls_around(&chars, caret),
            });
        }
        let calls = &self.ed.sig_scan.as_ref().expect("just scanned").calls;
        if calls.is_empty() {
            self.ed.sig = SigState::default();
            return None;
        }

        // Everything rust-analyzer knows, under one lock — and the pull, sent
        // only while the caret is inside a call, once per stamp.
        let (in_sync, ready, stamp, complete, diags) = {
            let mut lsp = self.lsp_state.lock().unwrap();
            let in_sync = lsp.last_sent_matches(rel, text);
            let ready = matches!(lsp.status, lsp::LspStatus::Ready);
            if in_sync && ready {
                lsp.request_document_diagnostics(rel);
            }
            let mut diags: Vec<DiagSpan> = Vec::new();
            let span = |d: &lsp::LspDiagnostic, pulled: bool| {
                let code = d.code.as_deref().filter(|c| ARG_CODES.contains(c))?;
                Some(DiagSpan {
                    code: code.to_owned(),
                    start: line_index.pos_to_char_idx(d.line, d.col),
                    end: line_index.pos_to_char_idx(d.end_line, d.end_col),
                    pulled,
                })
            };
            let pulled = in_sync.then(|| lsp.pulled_diagnostics(rel)).flatten();
            if let Some(pulled) = pulled {
                diags.extend(pulled.iter().filter_map(|d| span(d, true)));
            }
            // cargo check's, while they describe this very text: no edit since
            // the check began, and published since the last one sent.
            if in_sync && !lsp.flycheck_stale() && lsp.diagnostics_fresh(rel) {
                diags.extend(
                    crate::editor::gui::text_pos::diags_for_file(&lsp.diagnostics, rel)
                        .iter()
                        .filter(|d| d.source != "rust-analyzer")
                        .filter_map(|d| span(d, false)),
                );
            }
            // The whole picture: rust-analyzer's answer for exactly this text,
            // no newer pull on its way, and no cargo check half-published.
            let complete = in_sync
                && pulled.is_some()
                && !lsp.diagnostics_pull_in_flight(rel)
                && !lsp.checking;
            // What a signature depends on besides the call itself: this file,
            // every other file (the callee lives in one), and the cargo checks
            // (one refresh each), so an edited callee is asked about again.
            let stamp = (
                lsp.doc_version(rel),
                lsp.edit_gen(),
                lsp.diag_refresh_gen(),
                lsp.generation,
            );
            (in_sync, ready, stamp, complete, diags)
        };

        let Some((call, fault)) = faulty_call(calls, &diags) else {
            // Nothing wrong that can be SEEN. With the whole picture that is
            // the answer; without it, keep what was shown while the same file's
            // call reads the same.
            if complete {
                self.ed.sig = SigState::default();
                return None;
            }
            let held = self.ed.sig.shown.clone().filter(|h| h.rel == rel)?;
            let call = calls.iter().find(|c| c.key == held.key)?;
            return Some(ghost_at(line_index, call, held.help, held.fault));
        };
        let call = call.clone();

        // The signature: asked once per (view, file, stamp, call, position).
        // Inside the wrong argument, so the answer is this call's even when
        // that argument is itself a call.
        let ask_at = fault
            .bad_args
            .first()
            .map_or(call.open + 1, |&i| call.args[i].0);
        let token = {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            (slot, rel, stamp, call.open, ask_at).hash(&mut h);
            h.finish()
        };
        if self.ed.sig.asked == Some(token) && self.ed.sig.answer.is_none() {
            match self.lsp_state.lock().unwrap().take_signature_reply(token) {
                Some(SignatureReply::Help(help)) => self.ed.sig.answer = Some(help),
                // An edit cancelled it: asked again just below, this frame.
                Some(SignatureReply::Cancelled) => self.ed.sig.asked = None,
                None => {}
            }
        }
        if self.ed.sig.asked != Some(token) {
            // Another call, position or text: whatever was asked or answered
            // is not this one's — drawing it would put the previous callee's
            // parameters on this call.
            self.ed.sig.asked = None;
            self.ed.sig.answer = None;
            if in_sync && ready {
                let (line, character) = lsp_cursor_pos(text, ask_at);
                let sent = self
                    .lsp_state
                    .lock()
                    .unwrap()
                    .request_signature_help(rel, line, character, token);
                if sent {
                    self.ed.sig.asked = Some(token);
                }
            }
        }

        let help = match &self.ed.sig.answer {
            Some(Some(help)) => help.clone(),
            // rust-analyzer has no signature for it (a macro's call, say).
            Some(None) => {
                self.ed.sig.shown = None;
                return None;
            }
            // Still waiting: what was shown for this same call stays up.
            None => {
                let held = self.ed.sig.shown.as_ref()?;
                if held.rel != rel || held.key != call.key {
                    return None;
                }
                held.help.clone()
            }
        };
        self.ed.sig.shown = Some(Shown {
            rel: rel.to_owned(),
            key: call.key,
            fault: fault.clone(),
            help: help.clone(),
        });
        Some(ghost_at(line_index, &call, help, fault))
    }
}

/// The ghost for `call`: anchored at the end of the line holding its `(`.
fn ghost_at(
    line_index: &LineIndex,
    call: &CallSite,
    help: SignatureHelp,
    fault: Fault,
) -> SigGhost {
    let line = line_index.line_of_char(call.open) as u32 + 1;
    SigGhost {
        line,
        eol_idx: line_index.line_end_char_idx(line),
        help,
        fault,
        supplied: call.args.len(),
    }
}

/// The ghost text's colour — the inferred-type hint's.
const GHOST: egui::Color32 = egui::Color32::from_rgb(150, 165, 180);
/// The wrong parameter's — the inline error message's.
const WRONG: egui::Color32 = egui::Color32::from_rgb(220, 95, 85);

/// Draw `ghost` after the end of its line: at display char `eol_disp` of
/// `galley`, clear of a "N refs" pill ending at `pill_right`. Never slides
/// left over the code — it gives up detail instead (see [`ghost_text`]), and
/// with no room at all pins `(…)` to the right edge. The whole signature and
/// its documentation are one hover away either way.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_signature_ghost(
    ui: &egui::Ui,
    galley_pos: egui::Pos2,
    clip: egui::Rect,
    galley: &egui::text::Galley,
    eol_disp: usize,
    pill_right: Option<f32>,
    font_size: f32,
    ghost: &SigGhost,
    hover_id: egui::Id,
) {
    let painter = ui.painter().with_clip_rect(clip);
    let loc = galley.pos_from_cursor(egui::text::CCursor::new(eol_disp));
    let (y_top, y_bot) = (galley_pos.y + loc.min.y, galley_pos.y + loc.max.y);
    if y_bot < clip.top() || y_top > clip.bottom() {
        return;
    }
    let font = egui::FontId::monospace(font_size);
    let char_w = painter
        .layout_no_wrap("M".to_owned(), font.clone(), GHOST)
        .size()
        .x
        .max(1.0);
    let x0 = inline_message_x(galley_pos.x + loc.max.x + 16.0, pill_right);
    let right = clip.right() - 4.0;
    let room = ((right - x0) / char_w).floor().max(0.0) as usize;
    let (segments, x) = match ghost_text(&ghost.help, &ghost.fault, ghost.supplied, room) {
        Some(s) => (s, x0),
        None => {
            let w = 3.0 * char_w;
            (vec![("(…)".to_owned(), true)], (right - w).max(clip.left()))
        }
    };
    let mut job = egui::text::LayoutJob::default();
    for (text, wrong) in &segments {
        job.append(
            text,
            0.0,
            egui::TextFormat::simple(font.clone(), if *wrong { WRONG } else { GHOST }),
        );
    }
    let laid = painter.layout_job(job);
    let pos = egui::pos2(x, (y_top + y_bot) * 0.5 - laid.size().y * 0.5);
    let rect = egui::Rect::from_min_size(pos, laid.size());
    painter.galley(pos, laid, GHOST);
    let mut full = ghost.help.label.clone();
    if let Some(doc) = &ghost.help.doc {
        full.push_str("\n\n");
        full.push_str(doc.trim());
    }
    ui.interact(rect.intersect(clip), hover_id, egui::Sense::hover())
        .on_hover_text(full);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The caret at the first `^` of `marked` (which is removed).
    fn at(marked: &str) -> (Vec<char>, usize) {
        let caret = marked.chars().position(|c| c == '^').expect("a caret");
        (marked.chars().filter(|&c| c != '^').collect(), caret)
    }

    fn text_of(chars: &[char], (s, e): (usize, usize)) -> String {
        chars[s..e].iter().collect()
    }

    fn callee(chars: &[char], c: &CallSite) -> String {
        chars[c.start..c.callee_end].iter().collect()
    }

    /// The user's own call, in their leading-comma style: one call, the path
    /// is the callee, four arguments — the grouping parentheses of
    /// `(1 * H) as i32` are not a call.
    #[test]
    fn the_reported_call_is_found_with_its_arguments() {
        let (chars, caret) = at(
            "        pins::utils::i2c_display::wri^te_text(\n            \
             display\n            , \"<<\"\n            , (1 * H) as i32\n            , H as u32\n        \
             ).ok();",
        );
        let calls = calls_around(&chars, caret);
        assert_eq!(calls.len(), 1, "{calls:?}");
        let c = &calls[0];
        assert_eq!(callee(&chars, c), "pins::utils::i2c_display::write_text");
        let args: Vec<String> = c.args.iter().map(|&a| text_of(&chars, a)).collect();
        assert_eq!(args, ["display", "\"<<\"", "(1 * H) as i32", "H as u32"]);
    }

    #[test]
    fn a_caret_in_the_arguments_or_before_the_close_is_in_the_call() {
        for marked in ["f(a, ^b)", "f(a, b^)", "f^(a, b)", "^f(a, b)"] {
            let (chars, caret) = at(marked);
            assert_eq!(calls_around(&chars, caret).len(), 1, "{marked}");
        }
        let (chars, caret) = at("f(a, b)^;");
        assert!(calls_around(&chars, caret).is_empty(), "after the `)`");
    }

    /// Parentheses that call nothing.
    #[test]
    fn grouping_macros_declarations_and_keywords_are_not_calls() {
        for marked in [
            "let x = (1 ^+ 2) as i32;",
            "println!(\"{}\", ^x);",
            "fn name(a: u8^) {}",
            "fn name<T>(a: T^) {}",
            "pub struct P(u8^);",
            "if (a^) {}",
            "match (a, ^b) {}",
            "for (i, ^x) in v {}",
            "let (a, ^b) = t;",
            "pub(cr^ate) fn x() {}",
            "#[task(pool_size = ^2)]",
            "x => (1, ^2),",
            "let ok = a > (b^ + 1);",
            "let f: fn(u8^) -> u8 = g;",
            "let s = \"f(^x)\";",
            "// f(^x)",
        ] {
            let (chars, caret) = at(marked);
            assert!(calls_around(&chars, caret).is_empty(), "{marked}");
        }
    }

    /// A method's callee is its name, not the receiver; a turbofish belongs to
    /// the callee.
    #[test]
    fn methods_and_turbofish() {
        let (chars, caret) = at("a.b(1).c(^2)");
        let calls = calls_around(&chars, caret);
        assert_eq!(calls.len(), 1, "only `c`: the caret is past `b`'s `)`");
        assert_eq!(callee(&chars, &calls[0]), "c");

        let (chars, caret) = at("^a.b(1)");
        assert!(
            calls_around(&chars, caret).is_empty(),
            "the receiver is in no call"
        );

        let (chars, caret) = at("it.collect::<Vec<u8>>(^)");
        assert_eq!(
            callee(&chars, &calls_around(&chars, caret)[0]),
            "collect::<Vec<u8>>"
        );
    }

    /// Nested calls come innermost first.
    #[test]
    fn nested_calls_come_innermost_first() {
        let (chars, caret) = at("outer(a, inner(^b), c)");
        let calls = calls_around(&chars, caret);
        let names: Vec<String> = calls.iter().map(|c| callee(&chars, c)).collect();
        assert_eq!(names, ["inner", "outer"]);
        let args: Vec<String> = calls[1].args.iter().map(|&a| text_of(&chars, a)).collect();
        assert_eq!(args, ["a", "inner(b)", "c"]);
    }

    /// Commas that separate no arguments.
    #[test]
    fn commas_inside_closures_generics_strings_and_brackets_split_nothing() {
        let (chars, caret) = at(
            "f(|a, b| a + b, move |x, y| x, g::<u8, u16>(1), \"x, y\", [1, 2], {a, b}, // c, d\n z,)^",
        );
        let c = calls_around(&chars, caret - 1)
            .into_iter()
            .next()
            .expect("the call");
        let args: Vec<String> = c.args.iter().map(|&a| text_of(&chars, a)).collect();
        assert_eq!(
            args,
            [
                "|a, b| a + b",
                "move |x, y| x",
                "g::<u8, u16>(1)",
                "\"x, y\"",
                "[1, 2]",
                "{a, b}",
                "z"
            ]
        );
        let (chars, caret) = at("f(^)");
        assert!(calls_around(&chars, caret)[0].args.is_empty());
        let (chars, caret) = at("f(a || b, ^c)");
        assert_eq!(
            calls_around(&chars, caret)[0].args.len(),
            2,
            "`||` is an or here"
        );
    }

    /// A comment around an argument is not part of its span.
    #[test]
    fn comments_are_trimmed_off_an_argument() {
        let (chars, caret) = at("f(\n  display // the screen\n  , /* x */ y^\n)");
        let c = &calls_around(&chars, caret)[0];
        let args: Vec<String> = c.args.iter().map(|&a| text_of(&chars, a)).collect();
        assert_eq!(args, ["display", "y"]);
        // A literal right against a comment is the argument's own.
        let (chars, caret) = at("f(/*a*/\"x\", 'c'/*b*/^)");
        let c = &calls_around(&chars, caret)[0];
        let args: Vec<String> = c.args.iter().map(|&a| text_of(&chars, a)).collect();
        assert_eq!(args, ["\"x\"", "'c'"]);
    }

    /// A generic list holds nested generics, array types and const blocks.
    #[test]
    fn turbofish_with_nested_generics_arrays_and_const_blocks() {
        for (marked, name) in [
            (
                "core::mem::transmute::<[u8; 4], u32>(by^tes)",
                "core::mem::transmute::<[u8; 4], u32>",
            ),
            ("size_of::<[u32; 8]>(^)", "size_of::<[u32; 8]>"),
            ("it.collect::<Vec<[u8; 4]>>(^)", "collect::<Vec<[u8; 4]>>"),
            ("foo::<{ N + 1 }>(a^)", "foo::<{ N + 1 }>"),
        ] {
            let (chars, caret) = at(marked);
            let calls = calls_around(&chars, caret);
            assert_eq!(calls.len(), 1, "{marked}");
            assert_eq!(callee(&chars, &calls[0]), name);
        }
        let (chars, caret) = at("f(transmute::<Foo<u8>, u32>(a), ^b)");
        let args: Vec<String> = calls_around(&chars, caret)[0]
            .args
            .iter()
            .map(|&a| text_of(&chars, a))
            .collect();
        assert_eq!(args, ["transmute::<Foo<u8>, u32>(a)", "b"]);
        for marked in [
            "let x = 1; let ok = a > (b^);",
            "fn main() { let ok = a > (b^); }",
        ] {
            let (chars, caret) = at(marked);
            assert!(calls_around(&chars, caret).is_empty(), "{marked}");
        }
    }

    /// rust-analyzer's own (pulled) diagnostic.
    fn d(code: &str, start: usize, end: usize) -> DiagSpan {
        DiagSpan {
            code: code.to_owned(),
            start,
            end,
            pulled: true,
        }
    }

    /// cargo check's.
    fn r(code: &str, start: usize, end: usize) -> DiagSpan {
        DiagSpan {
            pulled: false,
            ..d(code, start, end)
        }
    }

    fn call_of(marked: &str) -> (Vec<char>, CallSite) {
        let (chars, caret) = at(marked);
        let c = calls_around(&chars, caret)
            .into_iter()
            .next()
            .expect("a call");
        (chars, c)
    }

    /// cargo check: E0308 on the argument, a hint on the callee.
    #[test]
    fn a_mismatched_argument_marks_that_argument() {
        let (_, c) = call_of("write_text(d, t, x, ^y as u32)");
        let (s, e) = c.args[3];
        let f = fault_of(&c, &[r("E0308", s, e), r("E0308", 0, c.callee_end)]).unwrap();
        assert_eq!(
            f,
            Fault {
                count: false,
                bad_args: vec![3]
            }
        );
    }

    /// The callee-only hint is enough on its own (cargo check, no RA pull).
    #[test]
    fn the_callee_hint_alone_marks_the_call() {
        let (_, c) = call_of("write_text(d, t, x, ^y)");
        assert_eq!(
            fault_of(&c, &[r("E0308", 0, c.callee_end)]),
            Some(Fault::default())
        );
    }

    /// Count errors: cargo check's on the callee or the parentheses,
    /// rust-analyzer's on the `)`.
    #[test]
    fn count_errors_from_both_sources() {
        let (_, c) = call_of("write_text(d, t, ^x)");
        let count = Some(Fault {
            count: true,
            bad_args: vec![],
        });
        assert_eq!(fault_of(&c, &[r("E0061", 0, c.callee_end)]), count);
        assert_eq!(fault_of(&c, &[r("E0061", c.open, c.close + 1)]), count);
        assert_eq!(fault_of(&c, &[d("E0107", c.close, c.close + 1)]), count);
        // An extra argument: rust-analyzer spans it through the `)`.
        let (_, c) = call_of("f(a, ^b)");
        let (s, _) = c.args[1];
        assert_eq!(fault_of(&c, &[d("E0107", s, c.close + 1)]), count);
        // Inside a cortex-m `#[entry]` fn it spans the whole `( … )`.
        assert_eq!(fault_of(&c, &[d("E0107", c.open, c.close + 1)]), count);
    }

    /// Two wrong arguments: cargo check moves its ERROR onto the callee and
    /// leaves a hint on each argument; rust-analyzer's pull has one error per
    /// argument. Either way both are marked.
    #[test]
    fn two_wrong_arguments_are_both_marked() {
        let (_, c) = call_of("write_text(d, t, ^0u32, y as u32)");
        let (a2, a3) = (c.args[2], c.args[3]);
        let rustc = [
            r("E0308", 0, c.callee_end),
            r("E0308", a2.0, a2.1),
            r("E0308", a3.0, a3.1),
        ];
        assert_eq!(fault_of(&c, &rustc).unwrap().bad_args, [2, 3]);
        let pulled = [d("E0308", a3.0, a3.1), d("E0308", a2.0, a2.1)];
        assert_eq!(fault_of(&c, &pulled).unwrap().bad_args, [2, 3], "sorted");
    }

    /// Errors that are not about the call's arguments.
    #[test]
    fn errors_inside_an_argument_or_on_the_result_are_not_the_calls() {
        let (_, c) = call_of("f(x.bad(), |a| { a + \"s\" }^)");
        let (s0, e0) = c.args[0];
        let (s1, _) = c.args[1];
        assert!(
            fault_of(&c, &[d("E0599", s0, e0)]).is_none(),
            "an unknown method"
        );
        assert!(
            fault_of(&c, &[d("E0308", s1 + 6, s1 + 13)]).is_none(),
            "a closure body"
        );
        assert!(
            fault_of(&c, &[d("E0308", c.start, c.close + 1)]).is_none(),
            "a mismatched return type spans the whole call"
        );
    }

    /// The wrongly called function wins over the innermost one.
    #[test]
    fn the_faulty_call_is_picked_not_merely_the_innermost() {
        let (chars, caret) = at("outer(a, inner(^b))");
        let calls = calls_around(&chars, caret);
        let outer = &calls[1];
        let diags = [d("E0308", outer.args[0].0, outer.args[0].1)];
        let (picked, fault) = faulty_call(&calls, &diags).unwrap();
        assert_eq!(callee(&chars, picked), "outer");
        assert_eq!(fault.bad_args, [0]);
        // And an error in `inner`'s argument is `inner`'s.
        let inner = &calls[0];
        let diags = [d("E0308", inner.args[0].0, inner.args[0].1)];
        assert_eq!(
            callee(&chars, faulty_call(&calls, &diags).unwrap().0),
            "inner"
        );
    }

    /// rustc labels where a type was INFERRED from — "this argument has type
    /// `u8`…" exactly on a correctly passed argument, with nothing at the
    /// callee. That call is not wrong.
    #[test]
    fn a_rustc_inference_label_on_an_argument_is_not_a_fault() {
        let (_, c) = call_of("buf.push(^byte)");
        let a = c.args[0];
        assert!(fault_of(&c, &[r("E0308", a.0, a.1)]).is_none());
        assert_eq!(
            fault_of(&c, &[d("E0308", a.0, a.1)]).unwrap().bad_args,
            [0],
            "rust-analyzer's own error on it is"
        );
    }

    /// An E0277 ending at a method's name is that method's own bound
    /// (`.into()`, `.collect()`), not an argument error; on an argument, with
    /// rustc's "required by a bound introduced by this call", it is.
    #[test]
    fn e0277_counts_only_on_an_argument() {
        let (_, c) = call_of("let x: u8 = y.into(^);");
        assert!(fault_of(&c, &[r("E0277", c.start, c.callee_end)]).is_none());
        let (_, c) = call_of("takes(1, ^N)");
        let a = c.args[1];
        let both = [r("E0277", a.0, a.1), r("E0277", c.start, c.callee_end)];
        assert_eq!(fault_of(&c, &both).unwrap().bad_args, [1]);
    }

    fn help() -> SignatureHelp {
        // rust-analyzer's real label for the reported call, offsets included.
        let label = "fn write_text<D: DrawTarget<Color = BinaryColor>>(display: &mut Ssd1306Async<\
                     I2CInterface<I2c<'_, Async>>, DisplaySize128x32, BufferedGraphicsModeAsync<\
                     DisplaySize128x32>>, text: &str, x: i32, y: i32) -> Result<(), <Ssd1306Async<\
                     I2CInterface<I2c<'_, Async>>, DisplaySize128x32, BufferedGraphicsModeAsync<\
                     DisplaySize128x32>> as DrawTarget>::Error>";
        SignatureHelp {
            label: label.to_owned(),
            params: vec![(50, 171), (173, 183), (185, 191), (193, 199)],
            active: Some(3),
            doc: Some("Writes text using the 6x10 font.".to_owned()),
        }
    }

    fn plain(segs: &[Segment]) -> String {
        segs.iter().map(|(t, _)| t.as_str()).collect()
    }

    fn wrong(segs: &[Segment]) -> Vec<&str> {
        segs.iter()
            .filter(|(_, w)| *w)
            .map(|(t, _)| t.as_str())
            .collect()
    }

    #[test]
    fn the_offsets_slice_the_real_label_into_its_parameters() {
        let h = help();
        let label: Vec<char> = h.label.chars().collect();
        let params: Vec<String> = h
            .params
            .iter()
            .map(|&(s, e)| label[s..e].iter().collect())
            .collect();
        assert!(
            params[0].starts_with("display: &mut Ssd1306Async<"),
            "{}",
            params[0]
        );
        assert_eq!(params[1..], ["text: &str", "x: i32", "y: i32"]);
    }

    /// Plenty of room: every parameter whole, the wrong one in the error colour.
    #[test]
    fn with_room_every_parameter_is_shown_whole() {
        let fault = Fault {
            count: false,
            bad_args: vec![3],
        };
        let segs = ghost_text(&help(), &fault, 4, 400).unwrap();
        assert!(plain(&segs).starts_with("(display: &mut Ssd1306Async<I2CInterface<"));
        assert!(plain(&segs).ends_with(", text: &str, x: i32, y: i32)"));
        assert_eq!(wrong(&segs), ["y: i32"]);
    }

    /// Less room collapses types, then drops them, never the wrong parameter.
    #[test]
    fn less_room_gives_up_detail_but_never_the_wrong_parameter() {
        let fault = Fault {
            count: false,
            bad_args: vec![3],
        };
        let mut last = usize::MAX;
        for room in [120, 70, 40, 24, 12, 5] {
            let segs = ghost_text(&help(), &fault, 4, room).unwrap();
            let n = plain(&segs).chars().count();
            assert!(n <= room, "room {room}: {n} chars: {}", plain(&segs));
            assert!(n <= last, "room {room} grew back to {n}");
            last = n;
            if room >= 12 {
                assert_eq!(wrong(&segs), ["y: i32"], "room {room}: {}", plain(&segs));
            }
        }
        // Names only once no type fits (26 chars), then the wrong one alone.
        assert_eq!(
            plain(&ghost_text(&help(), &fault, 4, 30).unwrap()),
            "(display, text, x, y: i32)"
        );
        assert_eq!(
            plain(&ghost_text(&help(), &fault, 4, 24).unwrap()),
            "(…, y: i32)"
        );
        assert!(
            ghost_text(&help(), &fault, 4, 2).is_none(),
            "not even `(…)`"
        );
    }

    /// A count error leads with the count; a missing parameter is the wrong one.
    #[test]
    fn a_missing_argument_is_counted_and_marked() {
        let fault = Fault {
            count: true,
            bad_args: vec![],
        };
        let segs = ghost_text(&help(), &fault, 3, 60).unwrap();
        assert!(plain(&segs).starts_with("3/4 args ("), "{}", plain(&segs));
        assert_eq!(wrong(&segs), ["3/4 args ", "y: i32"]);
        let extra = ghost_text(&help(), &fault, 5, 60).unwrap();
        assert_eq!(wrong(&extra), ["5/4 args "], "nothing is missing");
    }

    #[test]
    fn a_pattern_parameter_keeps_its_pattern_and_a_path_type_its_colons() {
        assert_eq!(
            split_param("(a, b): (u8, u8)"),
            ("(a, b)", Some("(u8, u8)"))
        );
        assert_eq!(
            split_param("p: core::fmt::Arguments<'_>"),
            ("p", Some("core::fmt::Arguments<'_>"))
        );
        assert_eq!(split_param("&mut self"), ("&mut self", None));
    }
}
