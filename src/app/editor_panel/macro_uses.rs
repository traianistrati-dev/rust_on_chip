//! Uses of an item inside `macro_rules!` bodies — the ones rust-analyzer's
//! `references` never returns.
//!
//! rust-analyzer resolves a token only inside a macro CALL. A token written in
//! a `macro_rules!` DEFINITION is never resolved, even when the macro is
//! invoked and its expansion calls the item. Measured on rust-analyzer 1.98:
//! `Node::value` used only as `Node::value(…)` in `mode_menu!`'s body, the
//! macro invoked twice — `references` answered `[]`, while `cargo check`
//! reported no dead code. That empty answer faded live code, and every count
//! of an item also used in a body (`ENABLED_NAME`, `BACK`, `Node`) came out
//! short.
//!
//! So the bodies are read here, as text over the code mask: every identifier
//! in a TRANSCRIBER — the `=> { … }` half of a rule; the matcher is a pattern
//! — of a macro that is actually invoked, with what surrounds it (`Node::`
//! before it, a `.` before it, a `(` after it, the struct literal it sits
//! in), so a match can depend on the item's kind (see
//! [`MacroIndex::uses_of`]). A name alone would let any `Foo::new` in any macro
//! keep every `new` in the project from fading.
//!
//! Each file is read once per text ([`FactsCache`]): the app runs unoptimised,
//! and re-lexing the whole project on every typing pause cost hundreds of ms.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// What comes before an identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Before {
    /// Nothing that qualifies it: `ENABLED_NAME`, `BACK`.
    Bare,
    /// A `.` — a method or a field.
    Dot,
    /// `Q::` — `Q` the type or module it is reached through: `Node`, `Self`,
    /// `crate`; `Vec` for a turbofish `Vec::<u8>::new`; `>` for a type unknown
    /// here (`<T as Trait>::f`, `<$t>::f`); `$` for a metavariable (`$t::f`),
    /// which may be a type OR a module; empty for a leading `::`.
    Path(String),
}

/// What comes after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum After {
    /// `(` — a call.
    Call,
    /// A `!` that is not `!=` — a macro invocation.
    Bang,
    /// `::` — the path goes on (`Node::value`, `name::<T>`).
    Path,
    /// A single `:` — a struct literal's `field: value`.
    Colon,
    /// `,` or `}` — a struct literal's shorthand `Node { name, … }`.
    End,
    Other,
}

/// One identifier in a transcriber, classified.
#[derive(Clone, Debug)]
struct Ident {
    name: String,
    /// Char index into its file, and its 0-based line.
    ci: usize,
    line: u32,
    before: Before,
    after: After,
    /// The first segment of the path it ends (`embassy_stm32` in
    /// `embassy_stm32::init`, `$crate` for `$crate::f`), empty when bare.
    root: String,
    /// Inside a struct literal's braces (`Node { … }`, `$t { … }`).
    in_literal: bool,
}

/// One `macro_rules!` definition of a file.
#[derive(Clone, Debug)]
struct Def {
    name: String,
    /// `#[macro_export]`: usable from other crates, so taken as invoked.
    exported: bool,
    /// Each rule's transcriber, `[start, end)` inside its braces.
    transcribers: Vec<(usize, usize)>,
    /// The identifiers of those transcribers, a nested definition's excluded
    /// (it is read as its own macro).
    idents: Vec<Ident>,
}

/// One invocation `name!(…)` / `name![…]` / `name!{…}` of a file.
#[derive(Clone, Debug)]
struct Call {
    name: String,
    /// The first segment of its path (`defmt` in `defmt::info!`), if any.
    root: Option<String>,
    /// The definition of this file whose transcriber holds it — the
    /// innermost — or `None` in plain code.
    inside: Option<usize>,
}

/// Everything one file contributes, read once per text.
#[derive(Debug, Default)]
pub(crate) struct FileFacts {
    defs: Vec<Def>,
    calls: Vec<Call>,
    /// Types whose members a `use X::*` / `use X::{…}` brings in by bare
    /// name (`use Kind::*` makes a bare `Back` the variant).
    globbed: Vec<String>,
}

/// One use of an item found in a macro body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MacroUse {
    /// Workspace-relative path of the file holding the macro.
    pub(crate) rel: String,
    /// 0-based line and char index of the identifier.
    pub(crate) line: u32,
    pub(crate) ci: usize,
    /// The macro whose body it is in.
    pub(crate) macro_name: String,
}

/// A tracked item, as much of it as matching needs.
pub(crate) struct ItemShape<'a> {
    pub(crate) name: &'a str,
    /// LSP `SymbolKind`.
    pub(crate) kind: u8,
    /// `(parent SymbolKind, name)` — see `lsp::SymbolInfo::container`.
    pub(crate) container: Option<(u8, &'a str)>,
    /// The item is itself a `macro_rules!` (rust-analyzer reports one as a
    /// Function): its uses are `name!` invocations.
    pub(crate) is_macro: bool,
    /// The crate the item lives in (`structure_map::parse::split_crate`).
    pub(crate) krate: &'a str,
}

/// One identifier of an invoked macro's transcriber, ready for matching.
#[derive(Clone, Debug)]
struct Occurrence {
    /// Index into [`MacroIndex::macros`].
    macro_idx: usize,
    ident: Ident,
}

/// A definition, placed: its file and crate.
#[derive(Clone, Debug)]
struct MacroRef {
    rel: String,
    krate: String,
    name: String,
}

/// Every identifier in the transcribers of invoked macros, across a crate set.
#[derive(Debug, Default)]
pub(crate) struct MacroIndex {
    macros: Vec<MacroRef>,
    /// The crates each macro is invoked from (its own, when exported).
    invoked_from: Vec<HashSet<String>>,
    occurrences: HashMap<String, Vec<Occurrence>>,
    /// Every library crate's name (`split_crate`'s prefix), for paths rooted
    /// at one: `mylib::helper` in a body is `mylib`'s, nobody else's.
    crates: HashSet<String>,
    /// Types brought in by a glob or brace import anywhere.
    globbed: HashSet<String>,
}

/// Whether `rel` is inside a crate's `src/` — the only files that belong to a
/// crate. A build script, `tests/`, `examples/` and `benches/` are their own
/// compilation units, and `split_crate` would put them in the firmware.
pub(crate) fn in_crate_src(rel: &str) -> bool {
    rel.ends_with(".rs") && (rel.starts_with("src/") || rel.contains("/src/"))
}

/// Words after which the next identifier is DECLARED, not used.
const DECLARES: [&str; 12] = [
    "fn", "let", "mut", "ref", "const", "static", "struct", "enum", "union", "type", "mod", "trait",
];

/// Words before a `{` that make it a declaration's body, not a struct
/// literal.
const NOT_LITERAL: [&str; 9] = [
    "struct", "union", "enum", "impl", "trait", "for", "mod", "dyn", "fn",
];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_open(c: char) -> bool {
    matches!(c, '(' | '[' | '{')
}

/// The last code char before `at`, not before `floor`, skipping whitespace
/// and comments.
fn prev_code(chars: &[char], mask: &[bool], floor: usize, at: usize) -> Option<usize> {
    (floor..at)
        .rev()
        .find(|&i| mask[i] && !chars[i].is_whitespace())
}

/// The first code char at or after `at`, before `ceil`.
fn next_code(chars: &[char], mask: &[bool], at: usize, ceil: usize) -> Option<usize> {
    (at..ceil).find(|&i| mask[i] && !chars[i].is_whitespace())
}

/// Where the identifier ending at `end` (inclusive) starts.
fn word_start(chars: &[char], end: usize) -> usize {
    let mut s = end;
    while s > 0 && is_ident(chars[s - 1]) {
        s -= 1;
    }
    s
}

/// The identifier ending at `end` (inclusive).
fn word_ending(chars: &[char], end: usize) -> String {
    chars[word_start(chars, end)..=end].iter().collect()
}

/// The bracket closing the one at `open`, over code only.
fn matching_close(chars: &[char], mask: &[bool], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in chars.iter().enumerate().skip(open) {
        if !mask[i] {
            continue;
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The innermost bracket still open at `i`, looking back no further than
/// `floor`.
fn enclosing_open(chars: &[char], mask: &[bool], floor: usize, i: usize) -> Option<usize> {
    let mut depth = 0usize;
    for k in (floor..i).rev() {
        if !mask[k] {
            continue;
        }
        match chars[k] {
            ')' | ']' | '}' => depth += 1,
            '(' | '[' | '{' if depth > 0 => depth -= 1,
            '(' | '[' | '{' => return Some(k),
            _ => {}
        }
    }
    None
}

/// Each rule's transcriber inside a `macro_rules!` body `(body_open,
/// body_close)`: `MATCHER => TRANSCRIBER ;` repeated. Stops at anything else.
fn transcribers(
    chars: &[char],
    mask: &[bool],
    body_open: usize,
    body_close: usize,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut p = body_open + 1;
    while let Some(m) = next_code(chars, mask, p, body_close) {
        if !is_open(chars[m]) {
            break;
        }
        let Some(m_close) = matching_close(chars, mask, m) else {
            break;
        };
        let Some(arrow) = next_code(chars, mask, m_close + 1, body_close) else {
            break;
        };
        if chars.get(arrow..arrow + 2) != Some(&['=', '>']) {
            break;
        }
        let Some(t) = next_code(chars, mask, arrow + 2, body_close) else {
            break;
        };
        if !is_open(chars[t]) {
            break;
        }
        let Some(t_close) = matching_close(chars, mask, t) else {
            break;
        };
        out.push((t + 1, t_close));
        p = t_close + 1;
        if let Some(semi) = next_code(chars, mask, p, body_close)
            && chars[semi] == ';'
        {
            p = semi + 1;
        }
    }
    out
}

/// Whether the `macro_rules!` at `at` carries `#[macro_export]` — read over
/// code only, back to the end of the previous item.
fn is_exported(chars: &[char], mask: &[bool], at: usize) -> bool {
    let mut k = at;
    while k > 0 && !(mask[k - 1] && matches!(chars[k - 1], ';' | '{' | '}')) {
        k -= 1;
    }
    let code: String = (k..at).filter(|&i| mask[i]).map(|i| chars[i]).collect();
    code.contains("macro_export")
}

/// The first segment of the path whose `::` ends just before `at`, with a
/// metavariable's `$` kept (`$crate`). Empty when `at` is not after `::`.
fn path_root(chars: &[char], mask: &[bool], floor: usize, at: usize) -> String {
    let mut start = at;
    let mut root = String::new();
    while let Some(p) = prev_code(chars, mask, floor, start) {
        if !(chars[p] == ':' && p > floor && chars[p - 1] == ':') {
            break;
        }
        let Some(k) = prev_code(chars, mask, floor, p - 1) else {
            break;
        };
        if !is_ident(chars[k]) {
            break;
        }
        let ws = word_start(chars, k);
        let w = word_ending(chars, k);
        root = if ws > floor && chars[ws - 1] == '$' {
            format!("${w}")
        } else {
            w
        };
        start = ws;
    }
    root
}

/// The type a turbofish `Name::<…>` names, from the `>` at `close` ending
/// it — `None` for `<T as Trait>` / `<$t>`, whose `<` opens the path.
fn turbofish_owner(chars: &[char], mask: &[bool], floor: usize, close: usize) -> Option<String> {
    let mut depth = 0usize;
    for k in (floor..=close).rev() {
        if !mask[k] {
            continue;
        }
        match chars[k] {
            '>' if k > floor && matches!(chars[k - 1], '-' | '=') => {}
            '>' => depth += 1,
            '<' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let c = prev_code(chars, mask, floor, k)?;
                    if chars[c] != ':' || c == floor || chars[c - 1] != ':' {
                        return None;
                    }
                    let q = prev_code(chars, mask, floor, c - 1)?;
                    if !is_ident(chars[q]) {
                        return None;
                    }
                    let ws = word_start(chars, q);
                    // `$t::<u8>::new`: the caller's type — a metavariable.
                    return Some(if ws > floor && chars[ws - 1] == '$' {
                        "$".to_owned()
                    } else {
                        word_ending(chars, q)
                    });
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether the word ending at `end` makes the NEXT identifier a declaration.
/// `'static` is a lifetime, and `mut` / `const` after `&`, `*`, `raw` or a
/// lifetime are a reference or pointer TYPE (`&mut T`, `*const T`,
/// `&raw mut X`, `&'a mut T`), not a binding.
fn declares(chars: &[char], mask: &[bool], floor: usize, end: usize) -> bool {
    let w = word_ending(chars, end);
    if !DECLARES.contains(&w.as_str()) {
        return false;
    }
    let start = word_start(chars, end);
    let is_lifetime = |ws: usize| ws > 0 && chars[ws - 1] == '\'';
    if is_lifetime(start) {
        return false;
    }
    if matches!(w.as_str(), "mut" | "const") {
        match prev_code(chars, mask, floor, start) {
            Some(k) if matches!(chars[k], '&' | '*') => return false,
            Some(k)
                if is_ident(chars[k])
                    && (word_ending(chars, k) == "raw" || is_lifetime(word_start(chars, k))) =>
            {
                return false;
            }
            _ => {}
        }
    }
    true
}

/// What the `{` at `open` (inside `[floor, …)`) opens: `Some(true)` for a
/// struct literal (`Node {`, `$t {`), `Some(false)` for an `enum` body,
/// `None` for anything else.
fn brace_kind(chars: &[char], mask: &[bool], floor: usize, open: usize) -> Option<bool> {
    if chars[open] != '{' {
        return None;
    }
    let k = prev_code(chars, mask, floor, open)?;
    if !is_ident(chars[k]) {
        return None;
    }
    let ws = word_start(chars, k);
    let name = word_ending(chars, k);
    let metavar = ws > floor && chars[ws - 1] == '$';
    let head_at = if metavar { ws - 1 } else { ws };
    let head = prev_code(chars, mask, floor, head_at)
        .filter(|&h| is_ident(chars[h]))
        .map(|h| word_ending(chars, h));
    match head.as_deref() {
        Some("enum") => Some(false),
        Some(h) if NOT_LITERAL.contains(&h) => None,
        _ if metavar || name.starts_with(char::is_uppercase) => Some(true),
        _ => None,
    }
}

/// Whether `i` is a variant NAME of the enum body opened at `open`: right
/// after the `{` or a `,`, attributes (`#[…]`) allowed — not a discriminant
/// (`Ctrl = BASE`), which is a use.
fn at_variant_name(chars: &[char], mask: &[bool], open: usize, i: usize) -> bool {
    let mut at = i;
    loop {
        match prev_code(chars, mask, open, at) {
            Some(p) if p == open || chars[p] == ',' => return true,
            Some(p) if chars[p] == ']' => {
                let Some(lb) = enclosing_open(chars, mask, open + 1, p) else {
                    return false;
                };
                match prev_code(chars, mask, open, lb) {
                    Some(h) if chars[h] == '#' => at = h,
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
}

/// The identifier `[i, e)` of a transcriber `[s, t)`, classified — or `None`
/// when it is not a use at all: a metavariable (`$v`), a lifetime, a name
/// being DECLARED (`fn value`, `let x`, an enum's variant), a nested
/// `macro_rules!` name. `enclosing` is the innermost bracket still open at
/// `i`, tracked by the caller in one forward pass.
fn classify(
    chars: &[char],
    mask: &[bool],
    (s, t): (usize, usize),
    (i, e): (usize, usize),
    enclosing: Option<usize>,
) -> Option<Ident> {
    if chars[i].is_ascii_digit() || (i > 0 && chars[i - 1] == '\'') {
        return None;
    }
    let before = match prev_code(chars, mask, s, i) {
        None => Before::Bare,
        Some(p) => match chars[p] {
            '$' => return None,
            '.' if p > s && chars[p - 1] == '.' => Before::Bare, // a range `..x`
            '.' => Before::Dot,
            ':' if p > s && chars[p - 1] == ':' => {
                let q = match prev_code(chars, mask, s, p - 1) {
                    Some(k) if is_ident(chars[k]) => {
                        let ws = word_start(chars, k);
                        let w = word_ending(chars, k);
                        // `$t::f`: whatever the caller passes — a type or a
                        // module, unknown here. `$crate` names a crate.
                        if w != "crate" && ws > s && chars[ws - 1] == '$' {
                            "$".to_owned()
                        } else {
                            w
                        }
                    }
                    Some(k) if chars[k] == '>' => {
                        turbofish_owner(chars, mask, s, k).unwrap_or_else(|| ">".to_owned())
                    }
                    _ => String::new(),
                };
                Before::Path(q)
            }
            '!' => {
                let named = prev_code(chars, mask, s, p)
                    .is_some_and(|k| is_ident(chars[k]) && word_ending(chars, k) == "macro_rules");
                if named {
                    return None;
                }
                Before::Bare
            }
            c if is_ident(c) => {
                if declares(chars, mask, s, p) {
                    return None;
                }
                Before::Bare
            }
            _ => Before::Bare,
        },
    };
    let brace = enclosing.and_then(|o| brace_kind(chars, mask, s, o));
    if brace == Some(false) && enclosing.is_some_and(|o| at_variant_name(chars, mask, o, i)) {
        return None; // a variant DECLARED in an enum the body defines
    }
    let after = match next_code(chars, mask, e, t) {
        Some(n) => match chars[n] {
            '(' => After::Call,
            '!' if chars.get(n + 1) != Some(&'=') => After::Bang,
            ':' if chars.get(n + 1) == Some(&':') => After::Path,
            ':' => After::Colon,
            ',' | '}' => After::End,
            _ => After::Other,
        },
        None => After::Other,
    };
    Some(Ident {
        name: chars[i..e].iter().collect(),
        ci: i,
        line: 0,
        before,
        after,
        root: path_root(chars, mask, s, i),
        in_literal: brace == Some(true),
    })
}

impl FileFacts {
    /// Read one file's text: its `macro_rules!` definitions (with their
    /// transcribers' identifiers classified), its invocations and its glob
    /// imports.
    pub(crate) fn read(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let mask = crate::rust_lex::code_mask(&chars);
        let newlines: Vec<usize> = (0..chars.len()).filter(|&k| chars[k] == '\n').collect();
        let mut facts = FileFacts::default();

        // Definitions, with their whole bodies for the nesting below.
        let mut bodies: Vec<(usize, usize)> = Vec::new();
        if text.contains("macro_rules") {
            for i in 0..chars.len() {
                if chars[i] != 'm'
                    || !mask[i]
                    || (i > 0 && is_ident(chars[i - 1]))
                    || !chars[i..]
                        .starts_with(&['m', 'a', 'c', 'r', 'o', '_', 'r', 'u', 'l', 'e', 's'])
                    || chars.get(i + 11).is_some_and(|&c| is_ident(c))
                {
                    continue;
                }
                let Some(bang) = next_code(&chars, &mask, i + 11, chars.len()) else {
                    continue;
                };
                if chars[bang] != '!' {
                    continue;
                }
                let Some(n) = next_code(&chars, &mask, bang + 1, chars.len()) else {
                    continue;
                };
                if !is_ident(chars[n]) {
                    continue;
                }
                let mut ne = n;
                while ne < chars.len() && is_ident(chars[ne]) {
                    ne += 1;
                }
                let Some(open) = next_code(&chars, &mask, ne, chars.len()) else {
                    continue;
                };
                if !is_open(chars[open]) {
                    continue;
                }
                let Some(close) = matching_close(&chars, &mask, open) else {
                    continue;
                };
                facts.defs.push(Def {
                    name: chars[n..ne].iter().collect(),
                    exported: is_exported(&chars, &mask, i),
                    transcribers: transcribers(&chars, &mask, open, close),
                    idents: Vec::new(),
                });
                bodies.push((open, close));
            }
        }

        // Each definition's identifiers — a nested definition's body skipped:
        // it runs only if invoked, and is read as its own macro.
        for d in 0..facts.defs.len() {
            let nested: Vec<(usize, usize)> = bodies
                .iter()
                .enumerate()
                .filter(|&(o, b)| o != d && bodies[d].0 < b.0 && b.1 < bodies[d].1)
                .map(|(_, &b)| b)
                .collect();
            let mut idents = Vec::new();
            for &(s, t) in &facts.defs[d].transcribers {
                // The brackets open at `i`, innermost last: one forward pass
                // instead of a walk back per identifier, which made a long
                // body quadratic.
                let mut open: Vec<usize> = Vec::new();
                let mut i = s;
                while i < t {
                    if let Some(&(_, c)) = nested.iter().find(|&&(o, c)| o <= i && i <= c) {
                        i = c + 1; // balanced: the stack is unchanged
                        continue;
                    }
                    if !mask[i] || !is_ident(chars[i]) || (i > s && is_ident(chars[i - 1])) {
                        if mask[i] {
                            match chars[i] {
                                '(' | '[' | '{' => open.push(i),
                                ')' | ']' | '}' => {
                                    open.pop();
                                }
                                _ => {}
                            }
                        }
                        i += 1;
                        continue;
                    }
                    let mut e = i;
                    while e < t && is_ident(chars[e]) {
                        e += 1;
                    }
                    if let Some(mut id) =
                        classify(&chars, &mask, (s, t), (i, e), open.last().copied())
                    {
                        id.line = newlines.partition_point(|&k| k < i) as u32;
                        idents.push(id);
                    }
                    i = e;
                }
            }
            facts.defs[d].idents = idents;
        }

        // Invocations, and glob / brace imports (`Kind::*`, `Kind::{…}`, which
        // only ever appear in `use`).
        let mut name = String::new();
        let mut i = 0;
        while i < chars.len() {
            if !mask[i] || !is_ident(chars[i]) || (i > 0 && is_ident(chars[i - 1])) {
                i += 1;
                continue;
            }
            let mut e = i;
            while e < chars.len() && is_ident(chars[e]) {
                e += 1;
            }
            if chars.get(e..e + 2) == Some(&[':', ':'])
                && matches!(chars.get(e + 2), Some('*' | '{'))
            {
                facts.globbed.push(chars[i..e].iter().collect());
            }
            if let Some(bang) = next_code(&chars, &mask, e, chars.len())
                && chars[bang] == '!'
                && chars.get(bang + 1) != Some(&'=')
                && let Some(d) = next_code(&chars, &mask, bang + 1, chars.len())
                && is_open(chars[d])
            {
                name.clear();
                name.extend(&chars[i..e]);
                if name != "macro_rules" {
                    // The innermost definition whose transcriber holds it:
                    // ranked by the start of THAT transcriber — a nested
                    // definition sits inside its outer's, so it starts later.
                    let inside = facts
                        .defs
                        .iter()
                        .enumerate()
                        .filter_map(|(d, def)| {
                            def.transcribers
                                .iter()
                                .find(|&&(s, t)| s <= i && i < t)
                                .map(|&(s, _)| (d, s))
                        })
                        .max_by_key(|&(_, s)| s)
                        .map(|(d, _)| d);
                    let root = path_root(&chars, &mask, 0, i);
                    facts.calls.push(Call {
                        name: name.clone(),
                        root: (!root.is_empty()).then_some(root),
                        inside,
                    });
                }
            }
            i = e;
        }
        facts
    }
}

/// The [`FileFacts`] of every project file, kept while its text is unchanged:
/// a typing pause then re-reads only the file that changed.
#[derive(Default)]
pub(crate) struct FactsCache {
    files: HashMap<String, (String, Arc<FileFacts>)>,
}

impl FactsCache {
    /// The facts of `files` — `(workspace-relative path, text)` — read only
    /// where a text changed; files that left the project are forgotten. Files
    /// outside a crate's `src/` are skipped (see [`in_crate_src`]).
    pub(crate) fn index<'a>(
        &mut self,
        files: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> MacroIndex {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut facts: Vec<(String, Arc<FileFacts>)> = Vec::new();
        for (rel, text) in files {
            if !in_crate_src(rel) || !seen.insert(rel) {
                continue;
            }
            let hit = self
                .files
                .get(rel)
                .filter(|(t, _)| t == text)
                .map(|(_, f)| Arc::clone(f));
            let f = hit.unwrap_or_else(|| {
                let f = Arc::new(FileFacts::read(text));
                self.files
                    .insert(rel.to_owned(), (text.to_owned(), Arc::clone(&f)));
                f
            });
            facts.push((rel.to_owned(), f));
        }
        self.files.retain(|rel, _| seen.contains(rel.as_str()));
        MacroIndex::from_facts(&facts)
    }
}

impl MacroIndex {
    /// [`FactsCache::index`] without a cache.
    #[cfg(test)]
    pub(crate) fn scan<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        FactsCache::default().index(files)
    }

    /// The index of a file that is its own compilation unit — `examples/`,
    /// `tests/`, `benches/`, a build script: only its own macros expand
    /// into it.
    pub(crate) fn of_unit(rel: &str, text: &str) -> Self {
        Self::from_facts(&[(rel.to_owned(), Arc::new(FileFacts::read(text)))])
    }

    /// Put the files' facts together: which macros run, and the identifiers
    /// of their transcribers.
    fn from_facts(files: &[(String, Arc<FileFacts>)]) -> Self {
        let krate_of = |rel: &str| crate::panels::structure_map::parse::split_crate(rel).0;
        let krates: Vec<String> = files.iter().map(|(rel, _)| krate_of(rel)).collect();
        let mut index = MacroIndex {
            crates: krates.iter().filter(|k| !k.is_empty()).cloned().collect(),
            globbed: files
                .iter()
                .flat_map(|(_, f)| f.globbed.iter().cloned())
                .collect(),
            ..Default::default()
        };
        // Module names of each crate (`fmt` for `src/fmt.rs` or
        // `src/fmt/mod.rs`): a call `fmt::info!` reaches the crate's own macro.
        let mut modules: HashSet<(String, String)> = HashSet::new();
        for ((rel, _), krate) in files.iter().zip(&krates) {
            let inner = crate::panels::structure_map::parse::split_crate(rel).1;
            for seg in inner.split('/') {
                let stem = seg.strip_suffix(".rs").unwrap_or(seg);
                if stem != "mod" && stem != "lib" && stem != "main" {
                    modules.insert((krate.clone(), stem.to_owned()));
                }
            }
        }
        // Every definition, numbered across the files.
        let mut first: Vec<usize> = Vec::new();
        for ((rel, f), krate) in files.iter().zip(&krates) {
            first.push(index.macros.len());
            index.macros.extend(f.defs.iter().map(|d| MacroRef {
                rel: rel.clone(),
                krate: krate.clone(),
                name: d.name.clone(),
            }));
        }
        let n = index.macros.len();
        if n == 0 {
            return index;
        }
        let def = |m: usize| def_at(files, &first, m);

        // Which macros run: exported ones, those invoked from plain code,
        // then — until nothing changes — those invoked from the body of one
        // that runs. A body nobody invokes calls nothing. A macro that is not
        // exported is reachable only from its own crate, and a call through
        // another crate's path (`defmt::info!`) is that crate's macro.
        let mut invoked = vec![false; n];
        index.invoked_from = vec![HashSet::new(); n];
        for (m, r) in index.macros.iter().enumerate() {
            if def(m).exported {
                invoked[m] = true;
                index.invoked_from[m].insert(r.krate.clone());
            }
        }
        // Definitions by name: a call to a macro the project does not define
        // (`println!`, `info!`) costs one lookup.
        let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
        for (m, r) in index.macros.iter().enumerate() {
            by_name.entry(r.name.as_str()).or_default().push(m);
        }
        let by_name: HashMap<String, Vec<usize>> = by_name
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect();
        loop {
            let mut changed = false;
            for (fi, (_, f)) in files.iter().enumerate() {
                let caller = &krates[fi];
                for call in &f.calls {
                    let Some(targets) = by_name.get(&call.name) else {
                        continue;
                    };
                    if call.inside.is_some_and(|d| !invoked[first[fi] + d]) {
                        continue;
                    }
                    for &m in targets {
                        let target = &index.macros[m];
                        let exported = def(m).exported;
                        let reachable = match call.root.as_deref() {
                            None | Some("crate" | "self" | "super" | "$crate") => {
                                exported || target.krate == *caller
                            }
                            Some(r) if index.crates.contains(r) => exported && target.krate == r,
                            Some(r) if modules.contains(&(caller.clone(), r.to_owned())) => {
                                exported || target.krate == *caller
                            }
                            Some(_) => false,
                        };
                        if reachable {
                            changed |= !invoked[m];
                            invoked[m] = true;
                            changed |= index.invoked_from[m].insert(caller.clone());
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }

        // The identifiers of every invoked macro's transcribers.
        for (m, &is_invoked) in invoked.iter().enumerate() {
            if !is_invoked {
                continue;
            }
            for id in &def(m).idents {
                index
                    .occurrences
                    .entry(id.name.clone())
                    .or_default()
                    .push(Occurrence {
                        macro_idx: m,
                        ident: id.clone(),
                    });
            }
        }
        index
    }

    /// The uses of `item` in the transcribers of invoked macros — matched to
    /// its kind, never by name alone:
    ///
    /// | item | counts as |
    /// |---|---|
    /// | associated fn / const (`impl Node`) | `Node::name`, `Self::name` |
    /// | method (`&self`) | `.name(`, or `Node::name` |
    /// | field | `.name`, or `name:` / `name,` in a struct literal |
    /// | enum variant | `Kind::name`, `Self::name`; bare under `use Kind::*` |
    /// | anything else | bare or by module path, never after a `.` or a type |
    /// | a `macro_rules!` | `name!` |
    ///
    /// A trait's items take any qualifier; `$t::name` and `<T>::name` any
    /// type. Only macros of the item's crate, or invoked from it, count —
    /// and a path rooted at a crate (`mylib::f`, `$crate::f`) only for that
    /// crate. Ordered by file, then position.
    pub(crate) fn uses_of(&self, item: &ItemShape) -> Vec<MacroUse> {
        let Some(occurrences) = self.occurrences.get(item.name) else {
            return Vec::new();
        };
        let mut uses: Vec<MacroUse> = occurrences
            .iter()
            .filter(|o| {
                let m = &self.macros[o.macro_idx];
                let crate_ok = match o.ident.root.as_str() {
                    "$crate" => m.krate == item.krate,
                    r if self.crates.contains(r) => r == item.krate,
                    _ => {
                        m.krate == item.krate || self.invoked_from[o.macro_idx].contains(item.krate)
                    }
                };
                // `defmt::info!` is another crate's macro, not a use of ours.
                let root = o.ident.root.as_str();
                let foreign_macro = item.is_macro
                    && !root.is_empty()
                    && !matches!(root, "crate" | "self" | "super" | "$crate")
                    && !self.crates.contains(root);
                crate_ok && !foreign_macro && matches_item(&o.ident, item, &self.globbed)
            })
            .map(|o| {
                let m = &self.macros[o.macro_idx];
                MacroUse {
                    rel: m.rel.clone(),
                    line: o.ident.line,
                    ci: o.ident.ci,
                    macro_name: m.name.clone(),
                }
            })
            .collect();
        uses.sort_by(|a, b| (&a.rel, a.ci).cmp(&(&b.rel, b.ci)));
        uses.dedup_by(|a, b| a.rel == b.rel && a.ci == b.ci);
        uses
    }
}

/// Definition number `m` across `files`, whose first definitions are numbered
/// `first`.
fn def_at<'a>(files: &'a [(String, Arc<FileFacts>)], first: &[usize], m: usize) -> &'a Def {
    let fi = first.partition_point(|&s| s <= m) - 1;
    &files[fi].1.defs[m - first[fi]]
}

/// A path qualifier that can only be a type: `Self`, an unknown `>`, an
/// UpperCamel name (modules and crates are snake_case), or a primitive.
fn names_a_type(q: &str) -> bool {
    q == "Self"
        || q == ">"
        || q.starts_with(char::is_uppercase)
        || matches!(
            q,
            "u8" | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "usize"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "isize"
                | "f32"
                | "f64"
                | "bool"
                | "char"
                | "str"
        )
}

/// Whether the identifier `o` is a use of `item` — see [`MacroIndex::uses_of`].
fn matches_item(o: &Ident, item: &ItemShape, globbed: &HashSet<String>) -> bool {
    if item.is_macro {
        return o.after == After::Bang;
    }
    if o.after == After::Bang {
        return false;
    }
    // The qualifier an associated item needs: its own type, `Self`, or one
    // unknown here (`>`); a trait's items, any.
    let qualifier_ok = |q: &str| match item.container {
        Some((11, _)) => true,
        Some((_, c)) => q == c || q == "Self" || q == ">" || q == "$",
        None => true,
    };
    match (item.kind, &o.before) {
        // Method.
        (6, Before::Dot) => matches!(o.after, After::Call | After::Path),
        (6, Before::Path(q)) => item.container.is_some() && qualifier_ok(q),
        (6, Before::Bare) => false,
        // Field.
        (8, Before::Dot) => o.after != After::Call,
        (8, Before::Bare) => o.in_literal && matches!(o.after, After::Colon | After::End),
        (8, Before::Path(_)) => false,
        // Enum variant: bare only where a glob import makes it one.
        (22, Before::Path(q)) => qualifier_ok(q),
        (22, Before::Bare) => {
            o.after != After::Path && item.container.is_some_and(|(_, e)| globbed.contains(e))
        }
        (22, Before::Dot) => false,
        // An associated fn / const / static: needs its qualifier.
        (_, Before::Path(q)) if item.container.is_some() => qualifier_ok(q),
        (_, _) if item.container.is_some() => false,
        // Free fn, const, static, type, trait: bare, or through a module or
        // crate path — never after a `.`, and never after a TYPE (`Kind::`,
        // `u16::`), which only leads to an associated item.
        (_, Before::Path(q)) => !names_a_type(q),
        (_, b) => *b != Before::Dot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user's own file, trimmed: `value` and `value_bool` are used ONLY
    /// in `mode_menu!`'s body, and the macro is invoked twice.
    const MENU: &str = r#"impl Node {
    pub const fn submenu(name: &'static str, items: &'static [Node]) -> Self {
        Self { name, kind: Kind::Submenu(items)}
    }
    pub const fn value(name: &'static str, v: &'static Value) -> Self {
        Self { name, kind: Kind::Value(v)}
    }
    pub const fn value_bool(name: &'static str, v: &'static ValueBool) -> Self {
        Self { name, kind: Kind::ValueBool(v)}
    }
}

pub const BACK: Node = Node { name: "<<", kind: Kind::Back};
pub const ENABLED_NAME: &str = "Enabled";

macro_rules! mode_menu {
    ($menu: ident, $crb: ident, $v: ident) => {
        static $crb: [Node; 5] = [
            Node::value_bool(ENABLED_NAME, &$v.enabled_boost),
            Node::value("Max Light %", &$v.crb_max_light),
            Node::value("Activate after sec", &$v.crb_after_sec),
            Node::value("Activate under cm", &$v.crb_under_cm),
            BACK,
        ];
        static $menu: [Node; 7] = [
            Node::value_bool(ENABLED_NAME, &$v.enabled),
            Node::value("Fade IN sec", &$v.fade_in),
            Node::value("Absence ON sec", &$v.duration_on),
            Node::value("Fade OUT sec", &$v.fade_out),
            Node::value("Max Light %", &$v.max_light),
            Node::submenu("Close-Range Boost", &$crb),
            BACK,
        ];
    };
}

mode_menu!(NORMAL_MENU, NORMAL_CRB_MENU, NORMAL);
mode_menu!(NIGHT_MENU, NIGHT_CRB_MENU, NIGHT);
"#;

    const REL: &str = "src/menu_navigator/menu.rs";

    fn item<'a>(name: &'a str, kind: u8, container: Option<(u8, &'a str)>) -> ItemShape<'a> {
        ItemShape {
            name,
            kind,
            container,
            is_macro: false,
            krate: "",
        }
    }

    fn count(index: &MacroIndex, it: &ItemShape) -> usize {
        index.uses_of(it).len()
    }

    /// One file of the firmware, its macro invoked.
    fn one(body: &str) -> MacroIndex {
        let src = format!("macro_rules! m {{ ($t:ident, $x:expr) => {{ {body} }} }}\nm!(A, b);\n");
        MacroIndex::scan([("src/a.rs", src.as_str())])
    }

    /// The report: 7 uses of `value`, 2 of `value_bool`, each on its body line.
    #[test]
    fn the_reported_functions_are_found_in_the_macro_body() {
        let index = MacroIndex::scan([(REL, MENU)]);
        let node = Some((19, "Node"));
        let value = index.uses_of(&item("value", 12, node));
        assert_eq!(value.len(), 7);
        assert!(
            value
                .iter()
                .all(|u| u.macro_name == "mode_menu" && u.rel == REL)
        );
        let lines: Vec<&str> = MENU.lines().collect();
        assert!(
            value
                .iter()
                .all(|u| lines[u.line as usize].contains("Node::value("))
        );
        assert_eq!(count(&index, &item("value_bool", 12, node)), 2);
        assert_eq!(count(&index, &item("submenu", 12, node)), 1);
    }

    /// Bare uses, types, fields: what rust-analyzer's counts were short of.
    #[test]
    fn constants_types_and_fields_used_in_the_body_count_too() {
        let index = MacroIndex::scan([(REL, MENU)]);
        assert_eq!(count(&index, &item("ENABLED_NAME", 14, None)), 2);
        assert_eq!(count(&index, &item("BACK", 14, None)), 2);
        assert_eq!(count(&index, &item("Node", 23, None)), 12);
        assert_eq!(
            count(&index, &item("enabled_boost", 8, Some((23, "Values")))),
            1
        );
        assert_eq!(
            count(&index, &item("max_light", 8, Some((23, "Values")))),
            1
        );
    }

    /// The qualifier decides: `Node::value` is not `Other::value`, a method
    /// is a `.value(` and a free function never follows a `.` or a type.
    #[test]
    fn the_match_depends_on_the_kind_and_the_qualifier() {
        let index = MacroIndex::scan([(REL, MENU)]);
        assert_eq!(count(&index, &item("value", 12, Some((19, "Other")))), 0);
        assert_eq!(
            count(&index, &item("value", 6, Some((19, "Node")))),
            7,
            "UFCS"
        );
        let index = one("x.get(); y.len; Foo::get(1); get(2)");
        assert_eq!(
            count(&index, &item("get", 6, Some((19, "Bar")))),
            1,
            ".get( only"
        );
        assert_eq!(
            count(&index, &item("get", 12, Some((19, "Foo")))),
            1,
            "Foo::get"
        );
        assert_eq!(
            count(&index, &item("get", 12, None)),
            1,
            "free fn: get( only"
        );
        assert_eq!(
            count(&index, &item("len", 8, Some((23, "S")))),
            1,
            "field .len"
        );
        assert_eq!(
            count(&index, &item("len", 6, Some((19, "S")))),
            0,
            "no call"
        );
    }

    /// A type from a metavariable (`$t::f`) or a qualified path (`<$t>::f`)
    /// is unknown here: any type's item may be meant. `$crate` names a crate.
    #[test]
    fn a_metavariable_type_matches_any_container() {
        let index = one("$t::value(1); <$t>::make(); $t::get(&$x); $t::Back");
        assert_eq!(count(&index, &item("value", 12, Some((19, "Node")))), 1);
        assert_eq!(count(&index, &item("make", 12, Some((19, "Node")))), 1);
        assert_eq!(count(&index, &item("get", 6, Some((19, "Foo")))), 1);
        assert_eq!(count(&index, &item("Back", 22, Some((10, "Kind")))), 1);
        let src = "#[macro_export]\nmacro_rules! e { () => { $crate::value(1) } }\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(count(&index, &item("value", 12, Some((19, "Node")))), 0);
        assert_eq!(count(&index, &item("value", 12, None)), 1);
    }

    /// A turbofish names its type; only `<…>::` leaves it unknown.
    #[test]
    fn a_turbofish_qualifier_is_its_type() {
        let index = one("let v = heapless::Vec::<u8, 8>::new();");
        assert_eq!(count(&index, &item("new", 12, Some((19, "Node")))), 0);
        assert_eq!(count(&index, &item("new", 12, Some((19, "Vec")))), 1);
    }

    /// `&mut X`, `*const T`, `&raw mut X`, `&'static T` are uses, not
    /// declarations.
    #[test]
    fn reference_and_pointer_types_are_uses() {
        let index = one(
            "static A: &'static Node = &X; f(&mut STATE); let p: *const Node = q; \
             let r: *mut Node = s; let h = &raw mut HEAP; let t = &raw const TABLE; let mut fresh = 1;",
        );
        assert_eq!(count(&index, &item("Node", 23, None)), 3);
        assert_eq!(count(&index, &item("STATE", 13, None)), 1);
        assert_eq!(count(&index, &item("HEAP", 13, None)), 1);
        assert_eq!(count(&index, &item("TABLE", 14, None)), 1);
        assert_eq!(
            count(&index, &item("fresh", 12, None)),
            0,
            "`let mut fresh` declares"
        );
    }

    /// A free item is never reached through a type: `Kind::Value(…)` is the
    /// variant, not the struct `Value`; `u16::MAX` is not the project's MAX.
    #[test]
    fn a_free_item_is_not_reached_through_a_type() {
        let index = one("Kind::Value(1); u16::MAX; embassy_stm32::init(p)");
        assert_eq!(count(&index, &item("Value", 23, None)), 0);
        assert_eq!(count(&index, &item("MAX", 14, None)), 0);
        assert_eq!(
            count(&index, &item("init", 12, None)),
            1,
            "a module path may lead to it"
        );
    }

    /// A bare variant counts only under a glob import; a struct of the same
    /// name, or a variant DECLARED in the body, never.
    #[test]
    fn bare_variants_need_a_glob_import() {
        let body = "static V: Value = Value { x: 1 }; pub enum $t { Idle, Busy } Back";
        let index = one(body);
        assert_eq!(count(&index, &item("Value", 22, Some((10, "Kind")))), 0);
        assert_eq!(count(&index, &item("Idle", 22, Some((10, "State")))), 0);
        assert_eq!(count(&index, &item("Back", 22, Some((10, "Kind")))), 0);
        let src =
            format!("use crate::menu::Kind::*;\nmacro_rules! m {{ () => {{ {body} }} }}\nm!();\n");
        let index = MacroIndex::scan([("src/a.rs", src.as_str())]);
        assert_eq!(count(&index, &item("Back", 22, Some((10, "Kind")))), 1);
        assert_eq!(
            count(&index, &item("Idle", 22, Some((10, "State")))),
            0,
            "declared"
        );
    }

    /// `name:` is a field only in a struct LITERAL; a shorthand init counts.
    #[test]
    fn fields_count_in_struct_literals_only() {
        let index = one(
            "pub struct $t { pub pin: u8 } fn f(port: u8) {} let c = |mode: u8| mode; \
             let n = Node { name, kind: 1 }; let m = $t { level: 2 };",
        );
        for declared in ["pin", "port", "mode"] {
            assert_eq!(
                count(&index, &item(declared, 8, Some((23, "Gpio")))),
                0,
                "{declared}"
            );
        }
        assert_eq!(
            count(&index, &item("name", 8, Some((23, "Node")))),
            1,
            "shorthand"
        );
        assert_eq!(count(&index, &item("kind", 8, Some((23, "Node")))), 1);
        assert_eq!(
            count(&index, &item("level", 8, Some((23, "Out")))),
            1,
            "a literal of a metavariable type"
        );
    }

    /// A body nobody invokes calls nothing — rustc reports those fns as dead.
    #[test]
    fn a_macro_never_invoked_counts_nothing() {
        let src = "macro_rules! m { () => { helper() } }\nfn main() { let x = a != b; }\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(count(&index, &item("helper", 12, None)), 0);
        let src = "macro_rules! m { () => { helper() } }\nfn f() { if m != 0 {} }\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(
            count(&index, &item("helper", 12, None)),
            0,
            "`m != 0` is no call"
        );
    }

    /// Invoked only from another macro's body: runs exactly when that one
    /// does. Calling itself does not make a macro run.
    #[test]
    fn invocations_from_other_bodies_follow_through() {
        let src = "macro_rules! inner { () => { deep() } }\n\
                   macro_rules! outer { () => { inner!(); } }\n\
                   macro_rules! lonely { () => { lonely!(); alone() } }\n";
        let quiet = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(
            count(&quiet, &item("deep", 12, None)),
            0,
            "outer is never invoked"
        );
        assert_eq!(
            count(&quiet, &item("alone", 12, None)),
            0,
            "self-recursion is no call"
        );
        let called = format!("{src}fn main() {{ outer!(); }}\n");
        let index = MacroIndex::scan([("src/a.rs", called.as_str())]);
        assert_eq!(count(&index, &item("deep", 12, None)), 1);
        let inner = ItemShape {
            is_macro: true,
            ..item("inner", 12, None)
        };
        assert_eq!(
            count(&index, &inner),
            1,
            "the macro itself: its `inner!` in outer"
        );
    }

    /// A macro DEFINED in an invoked body runs only if it is invoked itself.
    #[test]
    fn a_nested_definition_runs_only_when_invoked() {
        let src = "macro_rules! outer { () => { macro_rules! inner { () => { helper(); deep!(); } } } }\n\
                   macro_rules! deep { () => { deeper() } }\nouter!();\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(count(&index, &item("helper", 12, None)), 0);
        assert_eq!(count(&index, &item("deeper", 12, None)), 0);
        let src = src.replace("} } } }", "} } inner!(); } }");
        let index = MacroIndex::scan([("src/a.rs", src.as_str())]);
        assert_eq!(count(&index, &item("helper", 12, None)), 1);
        assert_eq!(count(&index, &item("deeper", 12, None)), 1);
    }

    /// `#[macro_export]` macros are usable from other crates: taken as invoked.
    #[test]
    fn an_exported_macro_counts_as_invoked() {
        let src = "#[macro_export]\nmacro_rules! m { () => { $crate::helper() } }\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(count(&index, &item("helper", 12, None)), 1);
    }

    /// Strings, comments, metavariables, the matcher and declarations are not
    /// uses; the three delimiters and several rules are all read.
    #[test]
    fn what_is_not_a_use() {
        let src = "macro_rules! m {\n\
                   ($helper:ident) => { let helper = 1; \"helper\"; /* helper */ $helper; fn helper() {} };\n\
                   [$x:expr] => [ helper(1) ];\n\
                   (@b) => ( helper(2) );\n\
                   }\nm!(a);\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        let uses = index.uses_of(&item("helper", 12, None));
        let lines: Vec<&str> = src.lines().collect();
        assert_eq!(uses.len(), 2, "{uses:?}");
        assert!(
            uses.iter()
                .all(|u| lines[u.line as usize].contains("helper("))
        );
    }

    /// Another file of the same crate holds the macro; another crate's macro
    /// does not count, and neither does anything outside a crate's `src/`.
    #[test]
    fn files_of_the_crate_count_other_units_do_not() {
        let mac = "macro_rules! m { () => { Node::value(1) } }\n";
        let call = "fn main() { m!(); }\n";
        let node = Some((19, "Node"));
        let index = MacroIndex::scan([("src/macros.rs", mac), ("src/main.rs", call)]);
        assert_eq!(count(&index, &item("value", 12, node)), 1);
        for unit in ["build.rs", "mylib/tests/it.rs", "examples/x.rs"] {
            let index = MacroIndex::scan([(unit, mac), ("src/main.rs", call)]);
            assert_eq!(count(&index, &item("value", 12, node)), 0, "{unit}");
        }
        let index = MacroIndex::scan([("other/src/lib.rs", mac), ("other/src/main.rs", call)]);
        assert_eq!(
            count(&index, &item("value", 12, node)),
            0,
            "another crate's macro"
        );
    }

    /// A macro is another crate's business unless exported; a call through a
    /// foreign path (`defmt::info!`) is not the project's macro; `$crate::` and
    /// `mylib::` paths in a body belong to that crate alone.
    #[test]
    fn crates_keep_their_own_macros_and_paths() {
        let a = "macro_rules! reg { () => { helper() } }\n";
        let b = "macro_rules! reg { () => { 0 } }\nfn f() { reg!(); }\n";
        let index = MacroIndex::scan([("a/src/lib.rs", a), ("b/src/lib.rs", b)]);
        let a_helper = ItemShape {
            krate: "a",
            ..item("helper", 12, None)
        };
        assert_eq!(count(&index, &a_helper), 0, "b's reg! is b's own");

        let fw =
            "macro_rules! info { ($m:expr) => { write($m) } }\nfn f() { defmt::info!(\"x\"); }\n";
        let index = MacroIndex::scan([("src/main.rs", fw)]);
        assert_eq!(
            count(&index, &item("write", 12, None)),
            0,
            "defmt's info! is not ours"
        );

        let drivers = "#[macro_export]\nmacro_rules! log_init { () => { $crate::init() } }\n";
        let fw = "fn main() { drivers::log_init!(); }\nfn init() {}\n";
        let index = MacroIndex::scan([("drivers/src/lib.rs", drivers), ("src/main.rs", fw)]);
        assert_eq!(
            count(&index, &item("init", 12, None)),
            0,
            "$crate is drivers"
        );
        let drivers_init = ItemShape {
            krate: "drivers",
            ..item("init", 12, None)
        };
        assert_eq!(count(&index, &drivers_init), 1);

        let fw = "macro_rules! go { () => { mylib::helper() } }\nfn main() { go!(); }\n";
        let lib = "pub fn helper() {}\n";
        let index = MacroIndex::scan([("src/main.rs", fw), ("mylib/src/lib.rs", lib)]);
        let lib_helper = ItemShape {
            krate: "mylib",
            ..item("helper", 12, None)
        };
        assert_eq!(
            count(&index, &lib_helper),
            1,
            "a firmware body calling mylib::helper"
        );
        assert_eq!(
            count(&index, &item("helper", 12, None)),
            0,
            "not the firmware's"
        );
    }

    /// Repetitions and nested groups are read like any other body text.
    #[test]
    fn repetitions_are_read() {
        let src = "macro_rules! all { ($($x:expr),*) => { [$(wrap($x)),*] } }\nall!(1, 2);\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(count(&index, &item("wrap", 12, None)), 1);
    }

    /// A file is read again only when its text changes; one that left the
    /// project is forgotten.
    #[test]
    fn the_cache_rereads_only_what_changed() {
        let mut cache = FactsCache::default();
        let mac = "macro_rules! m { () => { helper() } }\n";
        cache.index([("src/a.rs", mac), ("src/b.rs", "fn main() { m!(); }")]);
        let before = Arc::clone(&cache.files["src/a.rs"].1);
        let index = cache.index([("src/a.rs", mac), ("src/b.rs", "fn main() { m!(); m!(); }")]);
        assert!(
            Arc::ptr_eq(&before, &cache.files["src/a.rs"].1),
            "a.rs reused"
        );
        assert_eq!(count(&index, &item("helper", 12, None)), 1);
        let index = cache.index([("src/a.rs", mac)]);
        assert!(!cache.files.contains_key("src/b.rs"));
        assert_eq!(
            count(&index, &item("helper", 12, None)),
            0,
            "nobody invokes m now"
        );
    }

    /// A metavariable before `::` may be a MODULE: `$t::run()` over
    /// `spawn_all!(display, buttons)` uses each module's free `run`.
    #[test]
    fn a_metavariable_may_name_a_module() {
        let src = "macro_rules! spawn_all { ($($t:ident),*) => { $( s.spawn($t::run()); \
                   $crate::$t::init(); let b = [0u8; $t::SIZE]; )* } }\n\
                   fn main() { spawn_all!(display, buttons); }\n";
        let index = MacroIndex::scan([("src/main.rs", src)]);
        assert_eq!(count(&index, &item("run", 12, None)), 1);
        assert_eq!(count(&index, &item("init", 12, None)), 1);
        assert_eq!(count(&index, &item("SIZE", 14, None)), 1);
        assert_eq!(
            count(&index, &item("run", 12, Some((19, "Task")))),
            1,
            "or a type"
        );
    }

    /// A discriminant is a use; only the variant NAMES of an enum the body
    /// declares are skipped, attributes in front of them included.
    #[test]
    fn enum_discriminants_are_uses() {
        let index = one(
            "pub enum Reg { Ctrl = BASE, Stat = BASE + 4, Data = offset(2), \
             #[default] Idle, #[cfg(x)] Busy = BASE }",
        );
        assert_eq!(count(&index, &item("BASE", 14, None)), 3);
        assert_eq!(count(&index, &item("offset", 12, None)), 1);
        for variant in ["Ctrl", "Idle", "Busy"] {
            assert_eq!(
                count(&index, &item(variant, 22, Some((10, "Reg")))),
                0,
                "{variant}"
            );
        }
    }

    /// The call is in the innermost definition even when the outer macro has
    /// a LATER rule: ranked by the transcriber that holds it.
    #[test]
    fn a_later_rule_does_not_steal_a_nested_call() {
        let src = "macro_rules! outer { (a) => { macro_rules! inner { () => { deep!(); } } }; \
                   (b) => { 0 }; }\n\
                   macro_rules! deep { () => { deeper() } }\nouter!(a);\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(
            count(&index, &item("deeper", 12, None)),
            0,
            "inner is never invoked"
        );
    }

    /// A file that is its own unit sees its own macros.
    #[test]
    fn a_unit_outside_src_sees_its_own_macros() {
        let src = "fn setup() {}\nmacro_rules! pin { () => { setup() } }\nfn main() { pin!(); }\n";
        let index = MacroIndex::of_unit("mylib/examples/blink.rs", src);
        assert_eq!(count(&index, &item("setup", 12, None)), 1);
    }

    /// A macro item's uses are calls of IT: `defmt::info!` in a body is
    /// another crate's macro.
    #[test]
    fn a_foreign_rooted_call_is_not_a_use_of_our_macro() {
        let src = "macro_rules! info { () => { 0 } }\n\
                   macro_rules! log { () => { defmt::info!(\"x\"); info!(); } }\n\
                   fn main() { log!(); }\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        let info = ItemShape {
            is_macro: true,
            ..item("info", 12, None)
        };
        assert_eq!(count(&index, &info), 1, "only the bare info!()");
    }
}
