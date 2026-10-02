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
//! before it, a `.` before it, a `(` after it), so a match can depend on the
//! item's kind (see [`MacroIndex::uses_of`]). A name alone would let any
//! `Foo::new` in any macro keep every `new` in the project from fading.

use std::collections::{HashMap, HashSet};

/// What comes before an identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Before {
    /// Nothing that qualifies it: `ENABLED_NAME`, `BACK`.
    Bare,
    /// A `.` — a method or a field.
    Dot,
    /// `Q::` — `Q` the qualifying segment (`Node`, `Self`, `crate`), `>` for
    /// a generic one (`Vec::<u8>::new`, `<T as Trait>::f`), empty for a
    /// leading `::`.
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
    Other,
}

/// One identifier in an invoked macro's transcriber.
#[derive(Clone, Debug)]
struct Occurrence {
    /// Index into [`MacroIndex::macros`].
    macro_idx: usize,
    /// Char index into its file, and its 0-based line.
    ci: usize,
    line: u32,
    before: Before,
    after: After,
}

/// One `macro_rules!` definition.
#[derive(Clone, Debug)]
struct MacroDef {
    name: String,
    /// Index into [`MacroIndex::files`].
    file: usize,
    /// `#[macro_export]`: usable from other crates, so taken as invoked.
    exported: bool,
    /// Each rule's transcriber, `[start, end)` inside its braces.
    transcribers: Vec<(usize, usize)>,
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

/// Every identifier in the transcribers of invoked macros, across a crate set.
#[derive(Debug, Default)]
pub(crate) struct MacroIndex {
    /// `(rel, crate)` of each scanned file.
    files: Vec<(String, String)>,
    macros: Vec<MacroDef>,
    /// The crates each macro is invoked from (its own, when exported).
    invoked_from: Vec<HashSet<String>>,
    occurrences: HashMap<String, Vec<Occurrence>>,
}

/// Words after which the next identifier is DECLARED, not used.
const DECLARES: [&str; 12] = [
    "fn", "let", "mut", "ref", "const", "static", "struct", "enum", "union", "type", "mod", "trait",
];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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

/// The identifier ending at `end` (inclusive).
fn word_ending(chars: &[char], end: usize) -> String {
    let mut s = end;
    while s > 0 && is_ident(chars[s - 1]) {
        s -= 1;
    }
    chars[s..=end].iter().collect()
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

fn is_open(c: char) -> bool {
    matches!(c, '(' | '[' | '{')
}

/// Whether `word` starts at `i` as a whole word of code.
fn word_at(chars: &[char], mask: &[bool], i: usize, word: &str) -> bool {
    let n = word.chars().count();
    mask[i]
        && (i == 0 || !is_ident(chars[i - 1]))
        && chars
            .get(i..i + n)
            .is_some_and(|w| w.iter().copied().eq(word.chars()))
        && chars.get(i + n).is_none_or(|&c| !is_ident(c))
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
    loop {
        let Some(m) = next_code(chars, mask, p, body_close) else {
            break;
        };
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

/// One file, read once: its chars and code mask.
struct Read {
    chars: Vec<char>,
    mask: Vec<bool>,
}

impl Read {
    fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let mask = crate::rust_lex::code_mask(&chars);
        Self { chars, mask }
    }
}

impl MacroIndex {
    /// Read `files` — `(workspace-relative path, text)` — for macro bodies and
    /// invocations. Non-`.rs` files and build scripts are skipped: a build
    /// script is its own compilation unit.
    pub(crate) fn scan<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let files: Vec<(&str, &str)> = files
            .into_iter()
            .filter(|(rel, _)| {
                rel.ends_with(".rs") && !(*rel == "build.rs" || rel.ends_with("/build.rs"))
            })
            .collect();
        let mut index = MacroIndex {
            files: files
                .iter()
                .map(|(rel, _)| {
                    let krate = crate::panels::structure_map::parse::split_crate(rel).0;
                    ((*rel).to_owned(), krate)
                })
                .collect(),
            ..Default::default()
        };
        let mut reads: HashMap<usize, Read> = HashMap::new();

        // 1. Definitions — only files that can hold one are lexed.
        for (f, (_, text)) in files.iter().enumerate() {
            if !text.contains("macro_rules") {
                continue;
            }
            let read = reads.entry(f).or_insert_with(|| Read::new(text));
            let (chars, mask) = (&read.chars, &read.mask);
            for i in 0..chars.len() {
                if !word_at(chars, mask, i, "macro_rules") {
                    continue;
                }
                let end = i + "macro_rules".len();
                let Some(bang) = next_code(chars, mask, end, chars.len()) else {
                    continue;
                };
                if chars[bang] != '!' {
                    continue;
                }
                let Some(n) = next_code(chars, mask, bang + 1, chars.len()) else {
                    continue;
                };
                if !is_ident(chars[n]) {
                    continue;
                }
                let mut ne = n;
                while ne < chars.len() && is_ident(chars[ne]) {
                    ne += 1;
                }
                let Some(open) = next_code(chars, mask, ne, chars.len()) else {
                    continue;
                };
                if !is_open(chars[open]) {
                    continue;
                }
                let Some(close) = matching_close(chars, mask, open) else {
                    continue;
                };
                index.macros.push(MacroDef {
                    name: chars[n..ne].iter().collect(),
                    file: f,
                    exported: is_exported(chars, mask, i),
                    transcribers: transcribers(chars, mask, open, close),
                });
            }
        }
        if index.macros.is_empty() {
            return index;
        }

        // 2. Invocations: `name!(`, `name![`, `name!{` — never `name != x`.
        //    Each one remembers whose transcriber it sits in, if any.
        let names: HashSet<String> = index.macros.iter().map(|m| m.name.clone()).collect();
        // (macro name, crate of the invoking file, the macro whose body it is in)
        let mut calls: Vec<(String, String, Option<usize>)> = Vec::new();
        for (f, (_, text)) in files.iter().enumerate() {
            if !names.iter().any(|n| text.contains(&format!("{n}!"))) {
                continue;
            }
            let read = reads.entry(f).or_insert_with(|| Read::new(text));
            let (chars, mask) = (&read.chars, &read.mask);
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
                let name: String = chars[i..e].iter().collect();
                if names.contains(&name)
                    && let Some(bang) = next_code(chars, mask, e, chars.len())
                    && chars[bang] == '!'
                    && chars.get(bang + 1) != Some(&'=')
                    && let Some(d) = next_code(chars, mask, bang + 1, chars.len())
                    && is_open(chars[d])
                {
                    let inside = index.macros.iter().position(|m| {
                        m.file == f && m.transcribers.iter().any(|&(s, t)| s <= i && i < t)
                    });
                    calls.push((name, index.files[f].1.clone(), inside));
                }
                i = e;
            }
        }

        // 3. Which macros run: exported ones, those invoked from plain code,
        //    then — until nothing changes — those invoked from the body of one
        //    that runs. A body nobody invokes calls nothing.
        let n = index.macros.len();
        let mut invoked = vec![false; n];
        index.invoked_from = vec![HashSet::new(); n];
        for (m, def) in index.macros.iter().enumerate() {
            if def.exported {
                invoked[m] = true;
                index.invoked_from[m].insert(index.files[def.file].1.clone());
            }
        }
        loop {
            let mut changed = false;
            for (name, krate, inside) in &calls {
                if inside.is_some_and(|o| !invoked[o]) {
                    continue;
                }
                for m in 0..n {
                    if index.macros[m].name == *name {
                        changed |= !invoked[m];
                        invoked[m] = true;
                        changed |= index.invoked_from[m].insert(krate.clone());
                    }
                }
            }
            if !changed {
                break;
            }
        }

        // 4. The identifiers of every invoked macro's transcribers.
        for (m, def) in index.macros.iter().enumerate() {
            if !invoked[m] {
                continue;
            }
            let read = &reads[&def.file];
            let (chars, mask) = (&read.chars, &read.mask);
            let newlines: Vec<usize> = (0..chars.len()).filter(|&k| chars[k] == '\n').collect();
            for &(s, t) in &def.transcribers {
                let mut i = s;
                while i < t {
                    if !mask[i] || !is_ident(chars[i]) || (i > s && is_ident(chars[i - 1])) {
                        i += 1;
                        continue;
                    }
                    let mut e = i;
                    while e < t && is_ident(chars[e]) {
                        e += 1;
                    }
                    if let Some(o) = classify(chars, mask, s, t, i, e) {
                        let line = newlines.partition_point(|&k| k < i) as u32;
                        index
                            .occurrences
                            .entry(chars[i..e].iter().collect())
                            .or_default()
                            .push(Occurrence {
                                macro_idx: m,
                                ci: i,
                                line,
                                ..o
                            });
                    }
                    i = e;
                }
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
    /// | field | `.name`, or `name:` in a struct literal |
    /// | enum variant | `Kind::name`, `Self::name`, bare |
    /// | anything else | bare or by path, never after a `.` |
    /// | a `macro_rules!` | `name!` |
    ///
    /// A trait's items take any qualifier. Only macros of the item's crate, or
    /// invoked from it, count. Ordered by file, then position.
    pub(crate) fn uses_of(&self, item: &ItemShape) -> Vec<MacroUse> {
        let Some(occurrences) = self.occurrences.get(item.name) else {
            return Vec::new();
        };
        let mut uses: Vec<MacroUse> = occurrences
            .iter()
            .filter(|o| {
                let def = &self.macros[o.macro_idx];
                let crate_ok = self.files[def.file].1 == item.krate
                    || self.invoked_from[o.macro_idx].contains(item.krate);
                crate_ok && matches_item(o, item)
            })
            .map(|o| {
                let def = &self.macros[o.macro_idx];
                MacroUse {
                    rel: self.files[def.file].0.clone(),
                    line: o.line,
                    ci: o.ci,
                    macro_name: def.name.clone(),
                }
            })
            .collect();
        uses.sort_by(|a, b| (&a.rel, a.ci).cmp(&(&b.rel, b.ci)));
        uses.dedup_by(|a, b| a.rel == b.rel && a.ci == b.ci);
        uses
    }
}

/// What surrounds the identifier `[i, e)` of a transcriber `[s, t)` — or
/// `None` when it is not a use at all: a metavariable (`$v`), a lifetime, a
/// name being DECLARED (`fn value`, `let x`), a nested `macro_rules!` name.
fn classify(
    chars: &[char],
    mask: &[bool],
    s: usize,
    t: usize,
    i: usize,
    e: usize,
) -> Option<Occurrence> {
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
                    Some(k) if is_ident(chars[k]) => word_ending(chars, k),
                    Some(k) if chars[k] == '>' => ">".to_owned(),
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
                if DECLARES.contains(&word_ending(chars, p).as_str()) {
                    return None;
                }
                Before::Bare
            }
            _ => Before::Bare,
        },
    };
    let after = match next_code(chars, mask, e, t) {
        Some(n) => match chars[n] {
            '(' => After::Call,
            '!' if chars.get(n + 1) != Some(&'=') => After::Bang,
            ':' if chars.get(n + 1) == Some(&':') => After::Path,
            ':' => After::Colon,
            _ => After::Other,
        },
        None => After::Other,
    };
    Some(Occurrence {
        macro_idx: 0,
        ci: 0,
        line: 0,
        before,
        after,
    })
}

/// Whether the occurrence `o` is a use of `item` — see [`MacroIndex::uses_of`].
fn matches_item(o: &Occurrence, item: &ItemShape) -> bool {
    if item.is_macro {
        return o.after == After::Bang;
    }
    if o.after == After::Bang {
        return false;
    }
    // The qualifier an associated item needs: its own type, `Self`, or a
    // generic one (`>`); a trait's items, any.
    let qualifier_ok = |q: &str| match item.container {
        Some((11, _)) => true,
        Some((_, c)) => q == c || q == "Self" || q == ">",
        None => true,
    };
    match (item.kind, &o.before) {
        // Method.
        (6, Before::Dot) => matches!(o.after, After::Call | After::Path),
        (6, Before::Path(q)) => item.container.is_some() && qualifier_ok(q),
        (6, Before::Bare) => false,
        // Field.
        (8, Before::Dot) => o.after != After::Call,
        (8, Before::Bare) => o.after == After::Colon,
        (8, Before::Path(_)) => false,
        // Enum variant.
        (22, Before::Path(q)) => qualifier_ok(q),
        (22, Before::Bare) => true,
        (22, Before::Dot) => false,
        // An associated fn / const / static: needs its qualifier.
        (_, Before::Path(q)) if item.container.is_some() => qualifier_ok(q),
        (_, _) if item.container.is_some() => false,
        // Free fn, const, static, type, trait: anything but a `.` before.
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
    /// is a `.value(` and a free function never follows a `.`.
    #[test]
    fn the_match_depends_on_the_kind_and_the_qualifier() {
        let index = MacroIndex::scan([(REL, MENU)]);
        assert_eq!(count(&index, &item("value", 12, Some((19, "Other")))), 0);
        assert_eq!(
            count(&index, &item("value", 6, Some((19, "Node")))),
            7,
            "Node::value as UFCS"
        );
        let src = "macro_rules! m { () => { x.get(); y.len; Foo::get(1); get(2) } }\nm!();\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
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
            2,
            "free fn: get( and Foo::get"
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

    /// A body nobody invokes calls nothing — rustc reports those fns as dead.
    #[test]
    fn a_macro_never_invoked_counts_nothing() {
        let src = "macro_rules! m { () => { helper() } }\nfn main() { let x = a != b; }\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(count(&index, &item("helper", 12, None)), 0);
        // `m != x` is no invocation either.
        let src = "macro_rules! m { () => { helper() } }\nfn f() { if m != 0 {} }\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(count(&index, &item("helper", 12, None)), 0);
    }

    /// Invoked only from another macro's body: runs exactly when that one
    /// does. And calling itself does not make a macro run.
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
    /// does not count, and neither does a build script.
    #[test]
    fn files_of_the_crate_count_build_scripts_and_other_crates_do_not() {
        let mac = "macro_rules! m { () => { Node::value(1) } }\n";
        let call = "fn main() { m!(); }\n";
        let node = Some((19, "Node"));
        let index = MacroIndex::scan([("src/macros.rs", mac), ("src/main.rs", call)]);
        assert_eq!(count(&index, &item("value", 12, node)), 1);
        let index = MacroIndex::scan([("build.rs", mac), ("src/main.rs", call)]);
        assert_eq!(
            count(&index, &item("value", 12, node)),
            0,
            "build.rs is its own unit"
        );
        let index = MacroIndex::scan([("other/src/lib.rs", mac), ("other/src/main.rs", call)]);
        let firmware_item = item("value", 12, node);
        assert_eq!(count(&index, &firmware_item), 0, "another crate's macro");
    }

    /// Repetitions and nested groups are read like any other body text.
    #[test]
    fn repetitions_are_read() {
        let src = "macro_rules! all { ($($x:expr),*) => { [$(wrap($x)),*] } }\nall!(1, 2);\n";
        let index = MacroIndex::scan([("src/a.rs", src)]);
        assert_eq!(count(&index, &item("wrap", 12, None)), 1);
    }
}
