//! Source text → flowchart, via `syn` (pure logic, tested).
//!
//! `syn` rather than a hand-written scanner because the whole value of this
//! view is that the BRANCHES are right: a diamond in the wrong place is worse
//! than no diamond. A brace scanner can find `if`, but `else if` chains, match
//! arms, `if let`, closures and macro bodies are exactly where hand-rolled
//! parsers rot.
//!
//! Two consequences of that choice, both handled here rather than papered over:
//!
//! * **`syn` fails on half-typed code.** [`charts_of`] returns the error WITH
//!   its line, so the tab can keep showing the last good chart and say why it
//!   is stale, instead of blanking while the user types.
//! * **`syn` drops comments** — they are not tokens. The generated-init markers
//!   are therefore found in the raw TEXT ([`generated_ranges`]) and matched
//!   against statement line numbers.
//!
//! Labels are sliced out of the ORIGINAL source by span, so a box reads exactly
//! what the user wrote (`dist < THRESHOLD && is_night()`), not a reconstruction.
//! That needs `proc-macro2/span-locations`; without it `Span::start()` does not
//! exist at all.

use proc_macro2::Span;
use std::collections::HashMap;
use syn::spanned::Spanned;
use syn::visit::Visit;

// ── Model ────────────────────────────────────────────────────────────────────

/// Classic flowchart shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Stadium — `Start` / `END`.
    Terminal,
    /// Rectangle — a run of plain statements, collapsed into one box.
    Process,
    /// Parallelogram — a statement that talks to the outside world.
    Io,
    /// Diamond — `if` / `match` / a loop's test.
    Decision,
    /// Rectangle with side bars ("predefined process") — a call to another
    /// function of this project. Clicking it jumps to the definition.
    Subroutine,
    /// The generated init block, collapsed to one dimmed box. Not the user's
    /// algorithm, so it must not spend forty rectangles of the reader's screen.
    Generated,
    /// A declaration card on the whole-file canvas - a struct's fields, an
    /// enum's variants, a `const`, a run of `use`s. Not flow: nothing enters or
    /// leaves it. What it declares is in [`FlowNode::decl`].
    Decl,
}

impl FlowNode {
    /// A declaration card: `text` is its header, `rows` the lines under it.
    pub fn card(text: String, rows: Vec<String>, hidden: usize, line: usize, tag: DeclTag) -> Self {
        let mut n = Self::new(text, Shape::Decl, line);
        n.detail = rows;
        n.hidden = hidden;
        n.decl = Some(tag);
        n
    }
}

/// What a [`Shape::Decl`] card declares - its colour, whether it is dimmed,
/// and the element it stands for (whose full text its tooltip shows: the card
/// itself is cut to fit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeclTag {
    pub kind: ElementKind,
    pub generated: bool,
    /// Index into [`FileModel::elements`].
    pub element: usize,
}

/// Where a non-structured edge goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Leave `n` loop levels (1 = the innermost). A labelled `break 'outer`
    /// resolves to its real depth — treating every break as depth 1 would draw
    /// an arrow to the wrong loop, which is a lie, not an approximation.
    Break(usize),
    /// Back to the head of the loop `n` levels out.
    Continue(usize),
    /// Out of the function.
    Return,
}

/// One box.
#[derive(Clone, Debug)]
pub struct FlowNode {
    pub text: String,
    /// Further statements folded into the same box (a run of plain code).
    pub detail: Vec<String>,
    /// Statements past the detail cap — rendered as "+N more".
    pub hidden: usize,
    pub shape: Shape,
    /// 1-based source line — what a click on the box jumps to.
    pub line: usize,
    /// Contains an `.await`: a point where the cooperative executor may switch
    /// to another task. On an async project this is the single most useful
    /// thing the chart can mark.
    pub awaits: bool,
    /// Contains a `?`: an error path leaves here for the function's end.
    pub try_exit: bool,
    /// For [`Shape::Subroutine`] — the callee's definition line in this file.
    pub goto_line: Option<usize>,
    /// For [`Shape::Subroutine`] — the callee's [`Chart::key`], which is what
    /// opening it selects. The line alone cannot say WHICH of two same-named
    /// functions was meant.
    pub goto_key: Option<String>,
    /// For [`Shape::Decl`] - what the card declares.
    pub decl: Option<DeclTag>,
}

impl FlowNode {
    fn new(text: String, shape: Shape, line: usize) -> Self {
        Self {
            text,
            detail: Vec::new(),
            hidden: 0,
            shape,
            line,
            awaits: false,
            try_exit: false,
            goto_line: None,
            goto_key: None,
            decl: None,
        }
    }

    /// Lines of text the box shows (the first one plus any folded statements).
    pub fn lines(&self) -> usize {
        1 + self.detail.len() + usize::from(self.hidden > 0)
    }

    /// Every row a painter draws for this box, in order: the label, the folded
    /// statements, then the "+N more" tail.
    ///
    /// Here rather than in the painter because the measuring pass needs the
    /// same list: `box_size` used to walk `text` + `detail` while `draw_box`
    /// painted `text` + `detail` + the tail row, so a box whose statements were
    /// folded away was sized for a row it does not draw and not for one it
    /// does. The tail is short enough that the gap has never shown, which is
    /// exactly why it has to be a shared list rather than a shared habit.
    pub fn rows(&self) -> Vec<String> {
        let mut v = Vec::with_capacity(self.lines());
        v.push(self.text.clone());
        v.extend(self.detail.iter().cloned());
        if self.hidden > 0 {
            v.push(format!("+{} more", self.hidden));
        }
        v
    }
}

/// One labelled way out of a [`Flow::Branch`].
#[derive(Clone, Debug)]
pub struct Arm {
    pub label: String,
    pub body: Flow,
}

/// A loop's entry test, or the absence of one.
///
/// `While` and `For` are kept apart even though both draw a diamond: their
/// EDGE LABELS differ ("YES / NO" against "each / done"), and deriving that
/// from the label text would tie the layout to a string the parser happens to
/// build.
#[derive(Clone, Debug)]
pub enum LoopHead {
    /// `loop { … }` — nothing to test, the back edge is the only way round.
    Infinite,
    /// `while c { … }`
    While(FlowNode),
    /// `for x in it { … }`
    For(FlowNode),
}

impl LoopHead {
    /// The diamond above the body, if this loop has one.
    pub fn node(&self) -> Option<&FlowNode> {
        match self {
            Self::Infinite => None,
            Self::While(n) | Self::For(n) => Some(n),
        }
    }

    /// Labels for the two edges out of the test: (into the body, out of the loop).
    pub fn labels(&self) -> (&'static str, &'static str) {
        match self {
            Self::Infinite => ("", ""),
            Self::While(_) => ("YES", "NO"),
            Self::For(_) => ("each", "done"),
        }
    }
}

/// The structured body of a function.
#[derive(Clone, Debug)]
pub enum Flow {
    Node(FlowNode),
    /// Top-to-bottom on one spine. An EMPTY sequence is meaningful: it is the
    /// missing `else` of an `if`, drawn as a plain line down to the join.
    Seq(Vec<Flow>),
    Branch {
        cond: FlowNode,
        arms: Vec<Arm>,
    },
    Loop {
        head: LoopHead,
        body: Box<Flow>,
        /// 1-based line of the `loop` / `while` / `for` keyword.
        line: usize,
    },
    Jump {
        node: FlowNode,
        target: Target,
    },
}

/// What kind of thing starts this chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// `#[entry]`, `#[embassy_executor::main]`, `#[esp_rtos::main]`, RTIC `#[init]`.
    Main,
    /// `#[embassy_executor::task]`, RTIC `#[idle]` / software `#[task]`.
    Task,
    /// `#[interrupt]`, RTIC `#[task(binds = …)]` — started by hardware, not by
    /// a caller.
    Interrupt,
    /// An ordinary function: reachable only because something calls it.
    Function,
}

impl EntryKind {
    pub fn is_entry(self) -> bool {
        !matches!(self, Self::Function)
    }

    /// Short word shown beside the chart's name.
    pub fn word(self) -> &'static str {
        match self {
            Self::Main => "entry",
            Self::Task => "task",
            Self::Interrupt => "irq",
            Self::Function => "fn",
        }
    }
}

/// One function, drawn as one chart.
#[derive(Clone, Debug)]
pub struct Chart {
    /// What the chart is SELECTED by — unique in its file (see [`Element::key`]).
    /// Equal to `name` for a free function outside any inline `mod` and for an
    /// inherent method, which is what keeps a saved selection from before keys
    /// existed pointing at the same function.
    pub key: String,
    /// `"main"`, `"radar_task"`, `"Parser::feed"` — what the reader sees.
    pub name: String,
    pub kind: EntryKind,
    /// 1-based line of the function's name.
    pub line: usize,
    pub is_async: bool,
    pub body: Flow,
    /// No path returns — so the chart gets NO `END` terminal.
    ///
    /// Firmware's `fn main() -> !` ends in `loop {}`; drawing an `END` under it
    /// would state something false about the program. The textbook example has
    /// an END because its loop has an exit.
    pub diverges: bool,
}

/// A `syn` parse failure, with the line it points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub line: usize,
    pub message: String,
}

/// What an [`Element`] is: one kind per item `syn` can hand back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementKind {
    /// The file's inner attributes (`#![no_std]`, `#![no_main]`), as one row.
    CrateAttrs,
    /// A function with a body - free, method, or trait default. Has a chart.
    Fn(EntryKind),
    /// A trait method with no body, or a foreign `extern` fn: a signature only.
    RequiredFn,
    Struct,
    Enum,
    Union,
    Trait,
    TraitAlias,
    Impl,
    Const,
    Static,
    TypeAlias,
    /// An inline `mod x { … }` - a container.
    Mod,
    /// `mod x;` - the module lives in another file.
    ModDecl,
    /// A contiguous run of `use` items, folded into one row.
    Use,
    ExternCrate,
    MacroRules,
    /// An item-level macro call: `bind_interrupts!(…)`, `esp_app_desc!()`.
    MacroCall,
    /// `extern "C" { … }` - a container of foreign items.
    ForeignMod,
    /// Anything `syn` could only keep as raw tokens.
    Other,
}

impl ElementKind {
    /// The short word shown beside the element.
    pub fn word(self) -> &'static str {
        match self {
            Self::CrateAttrs => "#![..]",
            Self::Fn(k) => k.word(),
            Self::RequiredFn => "fn;",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Union => "union",
            Self::Trait => "trait",
            Self::TraitAlias => "trait",
            Self::Impl => "impl",
            Self::Const => "const",
            Self::Static => "static",
            Self::TypeAlias => "type",
            Self::Mod | Self::ModDecl => "mod",
            Self::Use => "use",
            Self::ExternCrate => "crate",
            Self::MacroRules => "macro",
            Self::MacroCall => "macro!",
            Self::ForeignMod => "extern",
            Self::Other => "item",
        }
    }

    /// Holds other elements (members are indented under it).
    pub fn is_container(self) -> bool {
        matches!(
            self,
            Self::Impl | Self::Trait | Self::Mod | Self::ForeignMod
        )
    }
}

/// One item of a file, as the outline lists it.
#[derive(Clone, Debug)]
pub struct Element {
    /// Unique in its file and stable across edits that do not touch it: the
    /// item's PATH, not its position. `main`, `Parser::feed`,
    /// `<Foo<T> as Debug>::fmt`, `app::init`, `struct Frame`, `const
    /// app::LIMIT`. Only byte-identical paths fall back to an occurrence
    /// suffix, `setup#2`: a function written twice under two `#[cfg]`s, and -
    /// commonly - a type whose methods are split over two `impl Uart` blocks
    /// (the picker tells those apart by line, `gui::pick_label`). Such a
    /// suffix is positional, so a twin added ABOVE shifts it.
    ///
    /// Functions carry no kind prefix, so a function key is the chart name the
    /// tab saved before keys existed.
    pub key: String,
    /// The label within its container: `feed` under `impl Parser`.
    pub name: String,
    pub kind: ElementKind,
    /// 0 at the top of the file, +1 per enclosing container.
    pub depth: usize,
    /// Index of the enclosing container in the same list.
    pub parent: Option<usize>,
    /// 1-based line of the item's name (or keyword) - what a click jumps to.
    pub ident_line: usize,
    /// 1-based first line, doc comments and attributes included.
    pub start_line: usize,
    /// 1-based last line: the closing brace or the semicolon.
    pub end_line: usize,
    /// One line the row shows: `fn feed(&mut self, b: u8) -> bool`,
    /// `struct Frame · 3 fields`, `impl<T> Debug for Foo<T>`.
    pub signature: String,
    /// Further lines for the tooltip: fields, variants, each `use` of a run.
    pub detail: Vec<String>,
    /// Lies wholly inside a GENERATED marker pair.
    pub generated: bool,
    /// Inside a `#[cfg(test)]` module.
    pub test_code: bool,
    /// Index into [`FileModel::charts`] for an element that has a body.
    pub chart: Option<usize>,
}

impl Element {
    /// Can be opened on its own: a function (its chart), a container (its
    /// members), or a type (its card). A `const`, a `use` run or a macro call
    /// is one line - it is read in the whole-file view, not opened.
    pub fn openable(&self) -> bool {
        self.chart.is_some()
            || self.kind.is_container()
            || matches!(
                self.kind,
                ElementKind::Struct | ElementKind::Enum | ElementKind::Union
            )
    }

    /// How the element is named where it is picked: `impl Debug for Foo`,
    /// `struct Frame`, `mod app` - and a function by its own name.
    pub fn label(&self) -> String {
        match self.kind {
            ElementKind::Fn(_) => self.name.clone(),
            ElementKind::Impl => format!("impl {}", self.name),
            ElementKind::ForeignMod => self.name.clone(),
            k => format!("{} {}", k.word(), self.name),
        }
    }
}

/// Everything the Flow tab reads from one file, from ONE `syn` pass.
#[derive(Clone, Debug, Default)]
pub struct FileModel {
    /// Every item, in source order, members right after their container.
    pub elements: Vec<Element>,
    /// Every function with a body, in source order.
    pub charts: Vec<Chart>,
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Every function of `src` as a chart, in source order - [`parse_file`]'s
/// charts, for the callers that want nothing else.
pub fn charts_of(src: &str) -> Result<Vec<Chart>, SyntaxError> {
    parse_file(src).map(|m| m.charts)
}

/// Every item of `src` as an [`Element`], plus a chart for every function with
/// a body - from ONE `syn` pass, which is the expensive part (tens of
/// milliseconds on a big file, on the UI thread).
///
/// Walks into `impl`, `trait`, `extern` and inline `mod` blocks. The last is
/// what makes RTIC readable, since `#[rtic::app] mod app { … }` puts the whole
/// application inside one module item.
///
/// Charts are built only after every function is known, so a call can resolve
/// to one defined further down the file.
pub fn parse_file(src: &str) -> Result<FileModel, SyntaxError> {
    // `span-locations` makes proc-macro2 keep a copy of EVERY parsed source, plus
    // its line table, in a thread-local map that nothing else ever empties. The
    // Flow tab parses the live buffer on the UI thread at each edit, so a long
    // session with the tab open held one full copy of the file per keystroke.
    // Safe here: no span outlives a call — charts, nodes, elements and the
    // syntax error carry plain line numbers, read before this function returns.
    proc_macro2::extra::invalidate_current_thread_spans();
    let file = syn::parse_file(src).map_err(|e| SyntaxError {
        line: e.span().start().line.max(1),
        message: e.to_string(),
    })?;
    let mut b = Builder {
        lines: src.lines().collect(),
        generated: generated_ranges(src),
        loops: Vec::new(),
        callees: Callees::default(),
        cur_self: None,
    };
    let mut w = Walk::default();
    b.crate_attrs(&file.attrs, &mut w);
    b.walk(&file.items, &Ctx::default(), &mut w);
    dedupe_keys(&mut w.elements);
    b.callees = Callees::index(&w);

    let mut charts = Vec::with_capacity(w.bodies.len());
    for body in &w.bodies {
        b.cur_self = body.self_base.clone();
        let key = w.elements[body.element].key.clone();
        charts.push(b.function(key, &body.name, body.kind, body.sig, body.stmts));
    }
    for (i, body) in w.bodies.iter().enumerate() {
        w.elements[body.element].chart = Some(i);
    }
    Ok(FileModel {
        elements: w.elements,
        charts,
    })
}

/// 1-based inclusive line ranges covered by a GENERATED marker pair.
///
/// Matched on the marker PREFIX rather than the exact constants, because the
/// generator writes three spellings of it — `GEN_BEGIN` / `GEN_END` in
/// `codegen::common` for main.rs, plus the bare `// <<< GENERATED>>>` the
/// peripheral config files open with. An unclosed opener runs to end of file,
/// which is what a half-written file looks like.
pub fn generated_ranges(src: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<usize> = None;
    let total = src.lines().count();
    for (i, line) in src.lines().enumerate() {
        let t = line.trim_start();
        if !t.starts_with("// <<< GENERATED") {
            continue;
        }
        if t.contains("END") {
            if let Some(start) = open.take() {
                out.push((start, i + 1));
            }
        } else if open.is_none() {
            open = Some(i + 1);
        }
    }
    if let Some(start) = open {
        out.push((start, total.max(start)));
    }
    out
}

// ── Builder ──────────────────────────────────────────────────────────────────

/// Plain statements folded into one box before the rest become "+N more".
const RUN_DETAIL_MAX: usize = 5;
/// Longest label kept on a box; the rest is elided.
const LABEL_MAX: usize = 56;

/// Longest one-line signature kept on an outline row; the tooltip has the rest.
const SIG_MAX: usize = 96;

struct Builder<'a> {
    lines: Vec<&'a str>,
    generated: Vec<(usize, usize)>,
    /// Innermost-last stack of loop labels, for resolving `break 'outer`.
    loops: Vec<Option<String>>,
    /// Every function of the file with a body, indexed for resolving a call to
    /// the one it names.
    callees: Callees,
    /// The self type (last path segment) of the `impl` or `trait` whose method
    /// is being charted - what `Self::f()` names.
    cur_self: Option<String>,
}

/// What the item walk collects: the elements, and the bodies still to chart.
#[derive(Default)]
struct Walk<'s> {
    elements: Vec<Element>,
    bodies: Vec<Body<'s>>,
}

/// A function with a body, waiting for its chart.
struct Body<'s> {
    element: usize,
    /// The chart's display name: `feed`'s is `Parser::feed`.
    name: String,
    ident: String,
    kind: EntryKind,
    sig: &'s syn::Signature,
    stmts: &'s [syn::Stmt],
    /// The enclosing `impl`'s self type or the enclosing trait, last segment.
    self_base: Option<String>,
    /// The innermost enclosing inline `mod`.
    module: Option<String>,
    /// Called as `x.f()` - a method, or a trait default method.
    is_method: bool,
}

/// Where the walk is: the container being filled and what encloses it.
#[derive(Clone, Default)]
struct Ctx {
    parent: Option<usize>,
    depth: usize,
    /// Enclosing inline modules, outermost first.
    mods: Vec<String>,
    test: bool,
}

impl Ctx {
    /// `app::` inside `mod app`, empty at the top of the file.
    fn prefix(&self) -> String {
        self.mods.iter().map(|m| format!("{m}::")).collect()
    }

    /// The context for the members of container `parent`.
    fn inside(&self, parent: usize, module: Option<&str>, test: bool) -> Ctx {
        let mut mods = self.mods.clone();
        if let Some(m) = module {
            mods.push(m.to_string());
        }
        Ctx {
            parent: Some(parent),
            depth: self.depth + 1,
            mods,
            test: self.test || test,
        }
    }
}

/// A call as the source spells it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    /// `x.f()`
    Method(String),
    /// `f()`, `Type::f()`, `module::f()`, `Self::f()` - the path's segments.
    Path(Vec<String>),
}

/// Every function with a body, indexed the ways a call can name one.
///
/// Resolution is STRICT: a call becomes a clickable subroutine only when it can
/// mean exactly one function here. The old map was keyed by the bare name, so
/// `b.feed()` opened `A::feed` and `uart.read(..)` opened a free `fn read` - a
/// box that opens the wrong function is worse than no box, the same rule as the
/// diamonds (see the module doc).
#[derive(Default)]
struct Callees {
    /// `(key, ident line)` per body, in body order.
    target: Vec<(String, usize)>,
    free: HashMap<String, Vec<usize>>,
    methods: HashMap<String, Vec<usize>>,
    /// `(self type, name)` - `Parser::feed`, and `Self::feed` inside it.
    typed: HashMap<(String, String), Vec<usize>>,
    /// `(module, name)` - `app::init`.
    in_mod: HashMap<(String, String), Vec<usize>>,
}

impl Callees {
    fn index(w: &Walk<'_>) -> Self {
        let mut c = Callees::default();
        for (i, b) in w.bodies.iter().enumerate() {
            let e = &w.elements[b.element];
            c.target.push((e.key.clone(), e.ident_line));
            if let Some(t) = &b.self_base {
                c.typed
                    .entry((t.clone(), b.ident.clone()))
                    .or_default()
                    .push(i);
            }
            if b.is_method {
                c.methods.entry(b.ident.clone()).or_default().push(i);
            } else {
                c.free.entry(b.ident.clone()).or_default().push(i);
                if let Some(m) = &b.module {
                    c.in_mod
                        .entry((m.clone(), b.ident.clone()))
                        .or_default()
                        .push(i);
                }
            }
        }
        c
    }

    /// The one body `call` can mean, or `None` when it is ambiguous or unknown.
    fn resolve(&self, call: &Call, cur_self: Option<&String>) -> Option<usize> {
        let one = |v: Option<&Vec<usize>>| match v {
            Some(v) if v.len() == 1 => Some(v[0]),
            _ => None,
        };
        match call {
            Call::Method(m) => one(self.methods.get(m)),
            Call::Path(segs) => match segs.as_slice() {
                [] => None,
                [f] => one(self.free.get(f)),
                [.., q, f] => {
                    let q = if q == "Self" {
                        cur_self?.clone()
                    } else {
                        q.clone()
                    };
                    let pair = (q, f.clone());
                    one(self.typed.get(&pair))
                        .or_else(|| one(self.in_mod.get(&pair)))
                        .or_else(|| {
                            matches!(pair.0.as_str(), "crate" | "self" | "super")
                                .then(|| one(self.free.get(f)))
                                .flatten()
                        })
                }
            },
        }
    }
}

/// Give byte-identical keys an occurrence suffix (`setup`, `setup#2`), in
/// source order. Only true twins get one - a function written twice under two
/// `#[cfg]`s - since anything else already differs in its path.
fn dedupe_keys(elements: &mut [Element]) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for e in elements {
        let n = seen.entry(e.key.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            e.key = format!("{}#{n}", e.key);
        }
    }
}

/// `#[cfg(test)]` exactly - `cfg(not(test))` is the opposite.
fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && matches!(&a.meta, syn::Meta::List(l) if l.tokens.to_string().trim() == "test")
    })
}

fn vis_span(v: &syn::Visibility) -> Option<Span> {
    match v {
        syn::Visibility::Public(p) => Some(p.span),
        syn::Visibility::Restricted(r) => Some(r.pub_token.span),
        syn::Visibility::Inherited => None,
    }
}

/// The first line of an item: its first attribute (a doc comment is one), else
/// its visibility, else its keyword.
///
/// Read off the tokens rather than `item.span()`: that re-tokenizes the whole
/// item to find its ends, which for a file means paying for it a second time.
fn first_line(attrs: &[syn::Attribute], vis: Option<&syn::Visibility>, keyword: Span) -> usize {
    attrs
        .first()
        .map(|a| a.pound_token.span)
        .or_else(|| vis.and_then(vis_span))
        .unwrap_or(keyword)
        .start()
        .line
}

/// Where a signature starts: its first qualifier, else `fn`.
fn sig_start(sig: &syn::Signature) -> Span {
    sig.constness
        .map(|t| t.span)
        .or(sig.asyncness.map(|t| t.span))
        .or(sig.unsafety.map(|t| t.span))
        .or(sig.abi.as_ref().map(|a| a.extern_token.span))
        .unwrap_or(sig.fn_token.span)
}

/// The closing delimiter of a macro call.
fn macro_end(m: &syn::Macro) -> Span {
    match &m.delimiter {
        syn::MacroDelimiter::Paren(d) => d.span.close(),
        syn::MacroDelimiter::Brace(d) => d.span.close(),
        syn::MacroDelimiter::Bracket(d) => d.span.close(),
    }
}

/// `Parser` for `impl Parser`, `Parser<T>` and `crate::p::Parser` - what a call
/// spells before `::f`. `None` for a reference or a tuple type.
fn type_base(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Lines of an element: `(ident, start, end)`.
type Lines = (usize, usize, usize);

impl<'a> Builder<'a> {
    fn wholly_generated(&self, start: usize, end: usize) -> bool {
        self.generated.iter().any(|&(a, b)| start >= a && end <= b)
    }

    /// Append one element and return its index.
    #[allow(clippy::too_many_arguments)]
    fn push(
        &self,
        w: &mut Walk<'_>,
        ctx: &Ctx,
        kind: ElementKind,
        key: String,
        name: String,
        (ident_line, start_line, end_line): Lines,
        signature: String,
        detail: Vec<String>,
    ) -> usize {
        w.elements.push(Element {
            key,
            name,
            kind,
            depth: ctx.depth,
            parent: ctx.parent,
            ident_line,
            start_line: start_line.min(ident_line),
            end_line: end_line.max(ident_line),
            signature: truncate(&squeeze(&signature), SIG_MAX),
            detail,
            generated: self.wholly_generated(start_line, end_line),
            test_code: ctx.test,
            chart: None,
        });
        w.elements.len() - 1
    }

    /// The file's inner attributes, as one row at the top.
    fn crate_attrs(&self, attrs: &[syn::Attribute], w: &mut Walk<'_>) {
        let (Some(first), Some(last)) = (attrs.first(), attrs.last()) else {
            return;
        };
        let metas: Vec<String> = attrs.iter().map(|a| self.snippet(a.meta.span())).collect();
        let start = first.pound_token.span.start().line;
        let end = last.bracket_token.span.close().end().line;
        let detail = metas.iter().map(|m| format!("#![{m}]")).collect();
        self.push(
            w,
            &Ctx::default(),
            ElementKind::CrateAttrs,
            "#![..]".to_string(),
            "crate attributes".to_string(),
            (start, start, end),
            metas.join(", "),
            detail,
        );
    }

    /// Walk `items`, appending one element per item (members after their
    /// container) and one pending body per function.
    fn walk<'s>(&self, items: &'s [syn::Item], ctx: &Ctx, w: &mut Walk<'s>) {
        // The `use` run still open at this level: consecutive `use` items fold
        // into ONE row, the way a run of plain statements folds into one box.
        let mut use_run: Option<(usize, Vec<String>)> = None;
        for item in items {
            if let syn::Item::Use(u) = item {
                self.use_item(u, ctx, &mut use_run, w);
                continue;
            }
            use_run = None;
            self.item(item, ctx, w);
        }
    }

    fn use_item(
        &self,
        u: &syn::ItemUse,
        ctx: &Ctx,
        run: &mut Option<(usize, Vec<String>)>,
        w: &mut Walk<'_>,
    ) {
        let tree = self.snippet(u.tree.span());
        let line = u.use_token.span.start().line;
        let end = u.semi_token.span.end().line;
        let signature = |trees: &[String]| match trees {
            [one] => one.clone(),
            _ => format!("{}: {}", plural(trees.len(), "import"), trees.join(", ")),
        };
        match run {
            Some((i, trees)) => {
                trees.push(tree.clone());
                let generated = {
                    let e = &w.elements[*i];
                    self.wholly_generated(e.start_line, end)
                };
                let e = &mut w.elements[*i];
                e.end_line = end;
                e.detail.push(tree);
                e.signature = truncate(&signature(trees), SIG_MAX);
                e.generated = generated;
            }
            None => {
                let start = first_line(&u.attrs, Some(&u.vis), u.use_token.span);
                let i = self.push(
                    w,
                    ctx,
                    ElementKind::Use,
                    format!("use {}{tree}", ctx.prefix()),
                    "use".to_string(),
                    (line, start, end),
                    tree.clone(),
                    vec![tree.clone()],
                );
                *run = Some((i, vec![tree]));
            }
        }
    }

    fn item<'s>(&self, item: &'s syn::Item, ctx: &Ctx, w: &mut Walk<'s>) {
        let prefix = ctx.prefix();
        match item {
            syn::Item::Fn(f) => {
                let ident = f.sig.ident.to_string();
                let kind = entry_kind(&f.attrs);
                let sig = f.sig.span();
                let i = self.push(
                    w,
                    ctx,
                    ElementKind::Fn(kind),
                    format!("{prefix}{ident}"),
                    ident.clone(),
                    (
                        f.sig.ident.span().start().line,
                        first_line(&f.attrs, Some(&f.vis), sig_start(&f.sig)),
                        f.block.brace_token.span.close().end().line,
                    ),
                    self.text_between(f.sig.ident.span().start(), sig.end()),
                    vec![self.snippet(sig)],
                );
                w.bodies.push(Body {
                    element: i,
                    name: ident.clone(),
                    ident,
                    kind,
                    sig: &f.sig,
                    stmts: &f.block.stmts,
                    self_base: None,
                    module: ctx.mods.last().cloned(),
                    is_method: false,
                });
            }
            syn::Item::Impl(im) => self.impl_block(im, ctx, w),
            syn::Item::Trait(t) => self.trait_block(t, ctx, w),
            syn::Item::Struct(s) => {
                let ident = s.ident.to_string();
                let head = self.head(&s.ident, &s.generics);
                let (signature, fields) = match &s.fields {
                    syn::Fields::Named(n) => (
                        format!("{head} · {}", plural(n.named.len(), "field")),
                        self.named_fields(&n.named),
                    ),
                    syn::Fields::Unnamed(u) => {
                        let tys: Vec<String> = u
                            .unnamed
                            .iter()
                            .map(|f| self.snippet(f.ty.span()))
                            .collect();
                        (format!("{head}({})", tys.join(", ")), tys)
                    }
                    syn::Fields::Unit => (head.clone(), Vec::new()),
                };
                let end = match (&s.semi_token, &s.fields) {
                    (Some(t), _) => t.span.end().line,
                    (None, syn::Fields::Named(n)) => n.brace_token.span.close().end().line,
                    (None, _) => s.ident.span().end().line,
                };
                let mut detail = vec![format!("struct {head}")];
                detail.extend(fields);
                self.push(
                    w,
                    ctx,
                    ElementKind::Struct,
                    format!("struct {prefix}{ident}"),
                    ident,
                    (
                        s.ident.span().start().line,
                        first_line(&s.attrs, Some(&s.vis), s.struct_token.span),
                        end,
                    ),
                    signature,
                    detail,
                );
            }
            syn::Item::Enum(en) => {
                let ident = en.ident.to_string();
                let head = self.head(&en.ident, &en.generics);
                let mut detail = vec![format!("enum {head}")];
                detail.extend(en.variants.iter().map(|v| self.variant(v)));
                self.push(
                    w,
                    ctx,
                    ElementKind::Enum,
                    format!("enum {prefix}{ident}"),
                    ident,
                    (
                        en.ident.span().start().line,
                        first_line(&en.attrs, Some(&en.vis), en.enum_token.span),
                        en.brace_token.span.close().end().line,
                    ),
                    format!("{head} · {}", plural(en.variants.len(), "variant")),
                    detail,
                );
            }
            syn::Item::Union(un) => {
                let ident = un.ident.to_string();
                let head = self.head(&un.ident, &un.generics);
                let mut detail = vec![format!("union {head}")];
                detail.extend(self.named_fields(&un.fields.named));
                self.push(
                    w,
                    ctx,
                    ElementKind::Union,
                    format!("union {prefix}{ident}"),
                    ident,
                    (
                        un.ident.span().start().line,
                        first_line(&un.attrs, Some(&un.vis), un.union_token.span),
                        un.fields.brace_token.span.close().end().line,
                    ),
                    format!("{head} · {}", plural(un.fields.named.len(), "field")),
                    detail,
                );
            }
            syn::Item::Const(c) => {
                let ident = c.ident.to_string();
                let text = self.text_between(c.const_token.span.end(), c.semi_token.span.start());
                self.push(
                    w,
                    ctx,
                    ElementKind::Const,
                    format!("const {prefix}{ident}"),
                    ident,
                    (
                        c.ident.span().start().line,
                        first_line(&c.attrs, Some(&c.vis), c.const_token.span),
                        c.semi_token.span.end().line,
                    ),
                    text.clone(),
                    vec![format!("const {text};")],
                );
            }
            syn::Item::Static(s) => {
                let ident = s.ident.to_string();
                let text = self.text_between(s.static_token.span.end(), s.semi_token.span.start());
                self.push(
                    w,
                    ctx,
                    ElementKind::Static,
                    format!("static {prefix}{ident}"),
                    ident,
                    (
                        s.ident.span().start().line,
                        first_line(&s.attrs, Some(&s.vis), s.static_token.span),
                        s.semi_token.span.end().line,
                    ),
                    text.clone(),
                    vec![format!("static {text};")],
                );
            }
            syn::Item::Type(t) => {
                let ident = t.ident.to_string();
                let text = self.text_between(t.type_token.span.end(), t.semi_token.span.start());
                self.push(
                    w,
                    ctx,
                    ElementKind::TypeAlias,
                    format!("type {prefix}{ident}"),
                    ident,
                    (
                        t.ident.span().start().line,
                        first_line(&t.attrs, Some(&t.vis), t.type_token.span),
                        t.semi_token.span.end().line,
                    ),
                    text.clone(),
                    vec![format!("type {text};")],
                );
            }
            syn::Item::Mod(m) => {
                let ident = m.ident.to_string();
                let test = is_cfg_test(&m.attrs);
                let kw = m.unsafety.map(|t| t.span).unwrap_or(m.mod_token.span);
                let start = first_line(&m.attrs, Some(&m.vis), kw);
                let line = m.ident.span().start().line;
                match &m.content {
                    Some((brace, inner)) => {
                        let tests = if test || ctx.test {
                            " · test code"
                        } else {
                            ""
                        };
                        let i = self.push(
                            w,
                            ctx,
                            ElementKind::Mod,
                            format!("mod {prefix}{ident}"),
                            ident.clone(),
                            (line, start, brace.span.close().end().line),
                            format!("{ident} · {}{tests}", plural(inner.len(), "item")),
                            vec![format!("mod {ident}")],
                        );
                        if test {
                            w.elements[i].test_code = true;
                        }
                        let inner_ctx = ctx.inside(i, Some(&ident), test);
                        self.walk(inner, &inner_ctx, w);
                    }
                    None => {
                        let end = m.semi.map(|t| t.span.end().line).unwrap_or(line);
                        self.push(
                            w,
                            ctx,
                            ElementKind::ModDecl,
                            format!("mod {prefix}{ident}"),
                            ident.clone(),
                            (line, start, end),
                            format!("{ident};"),
                            vec![format!("mod {ident}; - the module lives in its own file")],
                        );
                    }
                }
            }
            syn::Item::ExternCrate(ec) => {
                let ident = ec.ident.to_string();
                let text = self.text_between(ec.crate_token.span.end(), ec.semi_token.span.start());
                self.push(
                    w,
                    ctx,
                    ElementKind::ExternCrate,
                    format!("extern crate {prefix}{ident}"),
                    ident,
                    (
                        ec.ident.span().start().line,
                        first_line(&ec.attrs, Some(&ec.vis), ec.extern_token.span),
                        ec.semi_token.span.end().line,
                    ),
                    text.clone(),
                    vec![format!("extern crate {text};")],
                );
            }
            syn::Item::Macro(m) => {
                let path = self.snippet(m.mac.path.span());
                let kw = m.mac.path.span();
                let line = kw.start().line;
                let end = m
                    .semi_token
                    .map(|t| t.span.end().line)
                    .unwrap_or_else(|| macro_end(&m.mac).end().line);
                let start = first_line(&m.attrs, None, kw);
                match &m.ident {
                    Some(id) => {
                        let ident = id.to_string();
                        self.push(
                            w,
                            ctx,
                            ElementKind::MacroRules,
                            format!("{path}! {prefix}{ident}"),
                            ident.clone(),
                            (id.span().start().line, start, end),
                            ident.clone(),
                            vec![format!("{path}! {ident}")],
                        );
                    }
                    None => {
                        let call = self.snippet(m.mac.span());
                        self.push(
                            w,
                            ctx,
                            ElementKind::MacroCall,
                            format!("macro {prefix}{path}"),
                            format!("{path}!"),
                            (line, start, end),
                            call.clone(),
                            vec![truncate(&call, 400)],
                        );
                    }
                }
            }
            syn::Item::ForeignMod(fm) => {
                let abi = fm
                    .abi
                    .name
                    .as_ref()
                    .map(|n| format!("\"{}\"", n.value()))
                    .unwrap_or_default();
                let kw = fm
                    .unsafety
                    .map(|t| t.span)
                    .unwrap_or(fm.abi.extern_token.span);
                let i = self.push(
                    w,
                    ctx,
                    ElementKind::ForeignMod,
                    format!("extern {prefix}{abi}"),
                    format!("extern {abi}"),
                    (
                        fm.abi.extern_token.span.start().line,
                        first_line(&fm.attrs, None, kw),
                        fm.brace_token.span.close().end().line,
                    ),
                    format!("{abi} · {}", plural(fm.items.len(), "item")),
                    vec![format!("extern {abi}")],
                );
                let inner = ctx.inside(i, None, false);
                for it in &fm.items {
                    self.foreign_item(it, &inner, w);
                }
            }
            syn::Item::TraitAlias(ta) => {
                let ident = ta.ident.to_string();
                let text = self.text_between(ta.ident.span().start(), ta.semi_token.span.start());
                self.push(
                    w,
                    ctx,
                    ElementKind::TraitAlias,
                    format!("trait {prefix}{ident}"),
                    ident,
                    (
                        ta.ident.span().start().line,
                        first_line(&ta.attrs, Some(&ta.vis), ta.trait_token.span),
                        ta.semi_token.span.end().line,
                    ),
                    text.clone(),
                    vec![format!("trait {text};")],
                );
            }
            // `Item::Use` is folded by `walk`; `Verbatim` and anything a future
            // syn adds is shown as raw text rather than dropped.
            other => {
                let span = other.span();
                let text = self.snippet(span);
                self.push(
                    w,
                    ctx,
                    ElementKind::Other,
                    format!("item {prefix}{}", truncate(&text, 40)),
                    "item".to_string(),
                    (span.start().line, span.start().line, span.end().line),
                    text.clone(),
                    vec![truncate(&text, 400)],
                );
            }
        }
    }

    fn impl_block<'s>(&self, im: &'s syn::ItemImpl, ctx: &Ctx, w: &mut Walk<'s>) {
        let prefix = ctx.prefix();
        let ty = self.snippet(im.self_ty.span());
        let trait_ = im.trait_.as_ref().map(|(bang, path, _)| {
            format!(
                "{}{}",
                if bang.is_some() { "!" } else { "" },
                self.snippet(path.span())
            )
        });
        let kw = im
            .defaultness
            .map(|t| t.span)
            .or(im.unsafety.map(|t| t.span))
            .unwrap_or(im.impl_token.span);
        let header = self.text_between(kw.start(), im.self_ty.span().end());
        let shown = match &trait_ {
            Some(t) => format!("{t} for {ty}"),
            None => ty.clone(),
        };
        let i = self.push(
            w,
            ctx,
            ElementKind::Impl,
            // The header already starts with `impl` (or `unsafe impl`).
            format!("{prefix}{}", squeeze(&header)),
            shown.clone(),
            (
                im.impl_token.span.start().line,
                first_line(&im.attrs, None, kw),
                im.brace_token.span.close().end().line,
            ),
            format!("{shown} · {}", plural(im.items.len(), "item")),
            vec![header],
        );
        let inner = ctx.inside(i, None, false);
        let base = type_base(&im.self_ty);
        // `Type::f` for an inherent method, `<Type as Trait>::f` for a trait
        // one: the two are different functions, and only the second spelling
        // tells them apart when both exist.
        let path = |ident: &str| match &trait_ {
            Some(t) => format!("{prefix}<{ty} as {t}>::{ident}"),
            None => format!("{prefix}{ty}::{ident}"),
        };
        for it in &im.items {
            match it {
                syn::ImplItem::Fn(f) => {
                    let ident = f.sig.ident.to_string();
                    let kind = entry_kind(&f.attrs);
                    let sig = f.sig.span();
                    let e = self.push(
                        w,
                        &inner,
                        ElementKind::Fn(kind),
                        path(&ident),
                        ident.clone(),
                        (
                            f.sig.ident.span().start().line,
                            first_line(&f.attrs, Some(&f.vis), sig_start(&f.sig)),
                            f.block.brace_token.span.close().end().line,
                        ),
                        self.text_between(f.sig.ident.span().start(), sig.end()),
                        vec![self.snippet(sig)],
                    );
                    w.bodies.push(Body {
                        element: e,
                        name: format!("{ty}::{ident}"),
                        ident,
                        kind,
                        sig: &f.sig,
                        stmts: &f.block.stmts,
                        self_base: base.clone(),
                        module: ctx.mods.last().cloned(),
                        is_method: true,
                    });
                }
                syn::ImplItem::Const(c) => {
                    let ident = c.ident.to_string();
                    let text =
                        self.text_between(c.const_token.span.end(), c.semi_token.span.start());
                    self.push(
                        w,
                        &inner,
                        ElementKind::Const,
                        format!("const {}", path(&ident)),
                        ident,
                        (
                            c.ident.span().start().line,
                            first_line(&c.attrs, Some(&c.vis), c.const_token.span),
                            c.semi_token.span.end().line,
                        ),
                        text.clone(),
                        vec![format!("const {text};")],
                    );
                }
                syn::ImplItem::Type(t) => {
                    let ident = t.ident.to_string();
                    let text =
                        self.text_between(t.type_token.span.end(), t.semi_token.span.start());
                    self.push(
                        w,
                        &inner,
                        ElementKind::TypeAlias,
                        format!("type {}", path(&ident)),
                        ident,
                        (
                            t.ident.span().start().line,
                            first_line(&t.attrs, Some(&t.vis), t.type_token.span),
                            t.semi_token.span.end().line,
                        ),
                        text.clone(),
                        vec![format!("type {text};")],
                    );
                }
                syn::ImplItem::Macro(m) => {
                    self.member_macro(&m.mac, &m.attrs, m.semi_token, &inner, w)
                }
                other => self.member_other(other.span(), &inner, w),
            }
        }
    }

    fn trait_block<'s>(&self, t: &'s syn::ItemTrait, ctx: &Ctx, w: &mut Walk<'s>) {
        let prefix = ctx.prefix();
        let ident = t.ident.to_string();
        let kw = t
            .unsafety
            .map(|x| x.span)
            .or(t.auto_token.map(|x| x.span))
            .unwrap_or(t.trait_token.span);
        let open = t.brace_token.span.open().start();
        let shown = self.text_between(t.ident.span().start(), open);
        let i = self.push(
            w,
            ctx,
            ElementKind::Trait,
            format!("trait {prefix}{ident}"),
            ident.clone(),
            (
                t.ident.span().start().line,
                first_line(&t.attrs, Some(&t.vis), kw),
                t.brace_token.span.close().end().line,
            ),
            format!("{} · {}", shown.trim(), plural(t.items.len(), "item")),
            vec![self.text_between(kw.start(), open)],
        );
        let inner = ctx.inside(i, None, false);
        for it in &t.items {
            match it {
                syn::TraitItem::Fn(f) => {
                    let name = f.sig.ident.to_string();
                    let sig = f.sig.span();
                    let key = format!("{prefix}{ident}::{name}");
                    let start = first_line(&f.attrs, None, sig_start(&f.sig));
                    let line = f.sig.ident.span().start().line;
                    let after = self.text_between(f.sig.ident.span().start(), sig.end());
                    match &f.default {
                        // A default body is real code - it gets a chart.
                        Some(block) => {
                            let kind = entry_kind(&f.attrs);
                            let e = self.push(
                                w,
                                &inner,
                                ElementKind::Fn(kind),
                                key,
                                name.clone(),
                                (line, start, block.brace_token.span.close().end().line),
                                after,
                                vec![self.snippet(sig)],
                            );
                            w.bodies.push(Body {
                                element: e,
                                name: format!("{ident}::{name}"),
                                ident: name,
                                kind,
                                sig: &f.sig,
                                stmts: &block.stmts,
                                self_base: Some(ident.clone()),
                                module: ctx.mods.last().cloned(),
                                is_method: true,
                            });
                        }
                        None => {
                            let end = f.semi_token.map(|s| s.span.end().line).unwrap_or(line);
                            self.push(
                                w,
                                &inner,
                                ElementKind::RequiredFn,
                                key,
                                name,
                                (line, start, end),
                                format!("{after};"),
                                vec![format!("{};", self.snippet(sig))],
                            );
                        }
                    }
                }
                syn::TraitItem::Const(c) => {
                    let name = c.ident.to_string();
                    let end = c.semi_token.span.end().line;
                    let text =
                        self.text_between(c.const_token.span.end(), c.semi_token.span.start());
                    self.push(
                        w,
                        &inner,
                        ElementKind::Const,
                        format!("const {prefix}{ident}::{name}"),
                        name,
                        (
                            c.ident.span().start().line,
                            first_line(&c.attrs, None, c.const_token.span),
                            end,
                        ),
                        text.clone(),
                        vec![format!("const {text};")],
                    );
                }
                syn::TraitItem::Type(ty) => {
                    let name = ty.ident.to_string();
                    let text =
                        self.text_between(ty.type_token.span.end(), ty.semi_token.span.start());
                    self.push(
                        w,
                        &inner,
                        ElementKind::TypeAlias,
                        format!("type {prefix}{ident}::{name}"),
                        name,
                        (
                            ty.ident.span().start().line,
                            first_line(&ty.attrs, None, ty.type_token.span),
                            ty.semi_token.span.end().line,
                        ),
                        text.clone(),
                        vec![format!("type {text};")],
                    );
                }
                syn::TraitItem::Macro(m) => {
                    self.member_macro(&m.mac, &m.attrs, m.semi_token, &inner, w)
                }
                other => self.member_other(other.span(), &inner, w),
            }
        }
    }

    fn foreign_item(&self, it: &syn::ForeignItem, ctx: &Ctx, w: &mut Walk<'_>) {
        let prefix = ctx.prefix();
        match it {
            syn::ForeignItem::Fn(f) => {
                let ident = f.sig.ident.to_string();
                let sig = f.sig.span();
                let after = self.text_between(f.sig.ident.span().start(), sig.end());
                self.push(
                    w,
                    ctx,
                    ElementKind::RequiredFn,
                    format!("extern fn {prefix}{ident}"),
                    ident,
                    (
                        f.sig.ident.span().start().line,
                        first_line(&f.attrs, Some(&f.vis), sig_start(&f.sig)),
                        f.semi_token.span.end().line,
                    ),
                    format!("{after};"),
                    vec![format!("{};", self.snippet(sig))],
                );
            }
            syn::ForeignItem::Static(s) => {
                let ident = s.ident.to_string();
                let text = self.text_between(s.static_token.span.end(), s.semi_token.span.start());
                self.push(
                    w,
                    ctx,
                    ElementKind::Static,
                    format!("extern static {prefix}{ident}"),
                    ident,
                    (
                        s.ident.span().start().line,
                        first_line(&s.attrs, Some(&s.vis), s.static_token.span),
                        s.semi_token.span.end().line,
                    ),
                    text.clone(),
                    vec![format!("static {text};")],
                );
            }
            syn::ForeignItem::Type(t) => {
                let ident = t.ident.to_string();
                self.push(
                    w,
                    ctx,
                    ElementKind::TypeAlias,
                    format!("extern type {prefix}{ident}"),
                    ident.clone(),
                    (
                        t.ident.span().start().line,
                        first_line(&t.attrs, Some(&t.vis), t.type_token.span),
                        t.semi_token.span.end().line,
                    ),
                    format!("{ident};"),
                    vec![format!("type {ident};")],
                );
            }
            syn::ForeignItem::Macro(m) => self.member_macro(&m.mac, &m.attrs, m.semi_token, ctx, w),
            other => self.member_other(other.span(), ctx, w),
        }
    }

    /// A macro call inside an `impl`, `trait` or `extern` block.
    fn member_macro(
        &self,
        mac: &syn::Macro,
        attrs: &[syn::Attribute],
        semi: Option<syn::Token![;]>,
        ctx: &Ctx,
        w: &mut Walk<'_>,
    ) {
        let path = self.snippet(mac.path.span());
        let kw = mac.path.span();
        let end = semi
            .map(|t| t.span.end().line)
            .unwrap_or_else(|| macro_end(mac).end().line);
        let call = self.snippet(mac.span());
        self.push(
            w,
            ctx,
            ElementKind::MacroCall,
            format!("macro {}{path}", ctx.prefix()),
            format!("{path}!"),
            (kw.start().line, first_line(attrs, None, kw), end),
            call.clone(),
            vec![truncate(&call, 400)],
        );
    }

    /// A member `syn` only kept as raw tokens.
    fn member_other(&self, span: Span, ctx: &Ctx, w: &mut Walk<'_>) {
        let text = self.snippet(span);
        self.push(
            w,
            ctx,
            ElementKind::Other,
            format!("item {}{}", ctx.prefix(), truncate(&text, 40)),
            "item".to_string(),
            (span.start().line, span.start().line, span.end().line),
            text.clone(),
            vec![truncate(&text, 400)],
        );
    }

    /// `Frame<T>` - the name and its generic parameters, no `where` clause.
    fn head(&self, ident: &syn::Ident, generics: &syn::Generics) -> String {
        let end = generics
            .gt_token
            .map(|g| g.spans[0].end())
            .unwrap_or_else(|| ident.span().end());
        self.text_between(ident.span().start(), end)
    }

    /// `name: Type` per named field - built, not sliced, so a field's doc
    /// comment does not end up in the row.
    fn named_fields(
        &self,
        fields: &syn::punctuated::Punctuated<syn::Field, syn::Token![,]>,
    ) -> Vec<String> {
        fields
            .iter()
            .map(|f| {
                let name = f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default();
                truncate(&format!("{name}: {}", self.snippet(f.ty.span())), SIG_MAX)
            })
            .collect()
    }

    /// `Idle`, `Busy(u8)`, `At { x, y }`, `Night = 3`.
    fn variant(&self, v: &syn::Variant) -> String {
        let mut s = v.ident.to_string();
        match &v.fields {
            syn::Fields::Named(n) => {
                let names: Vec<String> = n
                    .named
                    .iter()
                    .filter_map(|f| f.ident.as_ref().map(|i| i.to_string()))
                    .collect();
                s.push_str(&format!(" {{ {} }}", names.join(", ")));
            }
            syn::Fields::Unnamed(u) => {
                let tys: Vec<String> = u
                    .unnamed
                    .iter()
                    .map(|f| self.snippet(f.ty.span()))
                    .collect();
                s.push_str(&format!("({})", tys.join(", ")));
            }
            syn::Fields::Unit => {}
        }
        if let Some((_, e)) = &v.discriminant {
            s.push_str(&format!(" = {}", self.snippet(e.span())));
        }
        truncate(&s, SIG_MAX)
    }

    fn function(
        &mut self,
        key: String,
        name: &str,
        kind: EntryKind,
        sig: &syn::Signature,
        stmts: &[syn::Stmt],
    ) -> Chart {
        self.loops.clear();
        let body = self.block(stmts);
        let never = matches!(
            &sig.output,
            syn::ReturnType::Type(_, t) if matches!(**t, syn::Type::Never(_))
        );
        Chart {
            key,
            name: name.to_string(),
            kind,
            line: sig.ident.span().start().line,
            is_async: sig.asyncness.is_some(),
            diverges: never || !falls_through(&body),
            body,
        }
    }

    /// A block's statements as a sequence, with runs of plain statements folded
    /// into single boxes.
    fn block(&mut self, stmts: &[syn::Stmt]) -> Flow {
        let mut out: Vec<Flow> = Vec::new();
        let mut run: Option<FlowNode> = None;
        for st in stmts {
            self.stmt(st, &mut out, &mut run);
        }
        flush(&mut run, &mut out);
        Flow::Seq(out)
    }

    fn stmt(&mut self, st: &syn::Stmt, out: &mut Vec<Flow>, run: &mut Option<FlowNode>) {
        let line = st.span().start().line;
        if self.in_generated(line) {
            let mut n = FlowNode::new("generated setup".to_string(), Shape::Generated, line);
            n.detail.push(self.label(st.span()));
            self.push_plain(n, out, run);
            return;
        }
        match st {
            // A nested `fn` / `struct` definition is not part of the flow.
            syn::Stmt::Item(_) => {}
            syn::Stmt::Local(l) => {
                let mut n = self.plain_node(l.span(), line);
                // `let x = if c { a } else { b };` is a VALUE, not a branch in
                // the function's spine — it stays one box.
                if let Some(init) = &l.init {
                    self.mark(&mut n, &init.expr);
                }
                self.push_plain(n, out, run);
            }
            syn::Stmt::Macro(m) => {
                let mut n = self.plain_node(m.span(), line);
                if is_io_macro(&m.mac.path) {
                    n.shape = Shape::Io;
                }
                self.push_plain(n, out, run);
            }
            syn::Stmt::Expr(e, _) => self.expr_stmt(e, line, out, run),
        }
    }

    fn expr_stmt(
        &mut self,
        e: &syn::Expr,
        line: usize,
        out: &mut Vec<Flow>,
        run: &mut Option<FlowNode>,
    ) {
        match e {
            syn::Expr::If(i) => {
                flush(run, out);
                let f = self.if_expr(i);
                out.push(f);
            }
            syn::Expr::Match(m) => {
                flush(run, out);
                let f = self.match_expr(m);
                out.push(f);
            }
            syn::Expr::Loop(l) => {
                flush(run, out);
                let label = l.label.as_ref().map(|x| x.name.ident.to_string());
                let mut f = self.loop_body(LoopHead::Infinite, label, &l.body.stmts, line);
                // `loop {}` — a spin with no statements. Left empty it draws a
                // back edge from nothing to nothing, which reads as a broken
                // chart rather than as an idle loop; the `panic_handler` every
                // generated main.rs carries is exactly this shape.
                if l.body.stmts.is_empty()
                    && let Flow::Loop { body, .. } = &mut f
                {
                    **body = Flow::Seq(vec![Flow::Node(FlowNode::new(
                        self.label(e.span()),
                        Shape::Process,
                        line,
                    ))]);
                }
                out.push(f);
            }
            syn::Expr::While(w) => {
                flush(run, out);
                let text = format!("while {}", self.snippet(w.cond.span()));
                let head = LoopHead::While(self.decision(text, line));
                let label = w.label.as_ref().map(|x| x.name.ident.to_string());
                let f = self.loop_body(head, label, &w.body.stmts, line);
                out.push(f);
            }
            syn::Expr::ForLoop(fl) => {
                flush(run, out);
                let text = format!(
                    "for {} in {}",
                    self.snippet(fl.pat.span()),
                    self.snippet(fl.expr.span())
                );
                let head = LoopHead::For(self.decision(text, line));
                let label = fl.label.as_ref().map(|x| x.name.ident.to_string());
                let f = self.loop_body(head, label, &fl.body.stmts, line);
                out.push(f);
            }
            // A bare block (or `unsafe { … }`) adds nesting but no control flow.
            syn::Expr::Block(b) => {
                flush(run, out);
                let f = self.block(&b.block.stmts);
                out.push(f);
            }
            syn::Expr::Unsafe(u) => {
                flush(run, out);
                let f = self.block(&u.block.stmts);
                out.push(f);
            }
            syn::Expr::Break(b) => {
                flush(run, out);
                let depth = self.depth_of(b.label.as_ref());
                let node = self.plain_node(b.span(), line);
                out.push(Flow::Jump {
                    node,
                    target: Target::Break(depth),
                });
            }
            syn::Expr::Continue(c) => {
                flush(run, out);
                let depth = self.depth_of(c.label.as_ref());
                let node = self.plain_node(c.span(), line);
                out.push(Flow::Jump {
                    node,
                    target: Target::Continue(depth),
                });
            }
            syn::Expr::Return(r) => {
                flush(run, out);
                let node = self.plain_node(r.span(), line);
                out.push(Flow::Jump {
                    node,
                    target: Target::Return,
                });
            }
            other => {
                let mut n = self.plain_node(other.span(), line);
                self.mark(&mut n, other);
                self.push_plain(n, out, run);
            }
        }
    }

    fn if_expr(&mut self, i: &syn::ExprIf) -> Flow {
        let line = i.if_token.span.start().line;
        let text = self.snippet(i.cond.span());
        let cond = self.decision(text, line);
        let then = self.block(&i.then_branch.stmts);
        let mut arms = vec![Arm {
            label: "YES".to_string(),
            body: then,
        }];
        let no = match i.else_branch.as_ref().map(|(_, e)| &**e) {
            // `else if` becomes a nested diamond in the NO arm — which is what
            // an else-if chain actually is.
            Some(syn::Expr::If(inner)) => self.if_expr(inner),
            Some(syn::Expr::Block(b)) => self.block(&b.block.stmts),
            Some(other) => {
                let l = other.span().start().line;
                let mut seq = Vec::new();
                let mut run = None;
                self.expr_stmt(other, l, &mut seq, &mut run);
                flush(&mut run, &mut seq);
                Flow::Seq(seq)
            }
            // No `else`: an empty arm, drawn as a plain line down to the join.
            None => Flow::Seq(Vec::new()),
        };
        arms.push(Arm {
            label: "NO".to_string(),
            body: no,
        });
        Flow::Branch { cond, arms }
    }

    fn match_expr(&mut self, m: &syn::ExprMatch) -> Flow {
        let line = m.match_token.span.start().line;
        let text = format!("match {}", self.snippet(m.expr.span()));
        let cond = self.decision(text, line);
        let mut arms = Vec::new();
        for a in &m.arms {
            let mut label = self.snippet(a.pat.span());
            if let Some((_, guard)) = &a.guard {
                label = format!("{label} if {}", self.snippet(guard.span()));
            }
            let body = match &*a.body {
                syn::Expr::Block(b) => self.block(&b.block.stmts),
                other => {
                    let l = other.span().start().line;
                    let mut seq = Vec::new();
                    let mut run = None;
                    self.expr_stmt(other, l, &mut seq, &mut run);
                    flush(&mut run, &mut seq);
                    Flow::Seq(seq)
                }
            };
            arms.push(Arm {
                label: truncate(&label, 28),
                body,
            });
        }
        Flow::Branch { cond, arms }
    }

    fn loop_body(
        &mut self,
        head: LoopHead,
        label: Option<String>,
        stmts: &[syn::Stmt],
        line: usize,
    ) -> Flow {
        self.loops.push(label);
        let body = self.block(stmts);
        self.loops.pop();
        Flow::Loop {
            head,
            body: Box::new(body),
            line,
        }
    }

    /// How many loop levels a `break` / `continue` leaves: 1 for the innermost,
    /// more when it names an enclosing label.
    fn depth_of(&self, label: Option<&syn::Lifetime>) -> usize {
        let Some(lt) = label else { return 1 };
        let want = lt.ident.to_string();
        self.loops
            .iter()
            .rev()
            .position(|l| l.as_deref() == Some(want.as_str()))
            .map(|i| i + 1)
            .unwrap_or(1)
    }

    fn decision(&self, text: String, line: usize) -> FlowNode {
        FlowNode::new(truncate(&text, LABEL_MAX), Shape::Decision, line)
    }

    /// A plain statement box (classification happens in [`Self::mark`]).
    fn plain_node(&self, span: Span, line: usize) -> FlowNode {
        FlowNode::new(self.label(span), Shape::Process, line)
    }

    /// Text of `span`, squeezed and truncated for a box label.
    fn label(&self, span: Span) -> String {
        truncate(&self.snippet(span), LABEL_MAX)
    }

    /// Classify `expr` and stamp the node with what the scan found.
    fn mark(&self, n: &mut FlowNode, expr: &syn::Expr) {
        let mut scan = Scan::default();
        scan.visit_expr(expr);
        n.awaits = scan.awaits;
        n.try_exit = scan.try_exit;
        // A call into this project's own code wins over the I-O heuristic: the
        // reader can OPEN a subroutine box, so saying so is worth more than
        // saying the statement touches a peripheral.
        let cur_self = self.cur_self.as_ref();
        if let Some(i) = scan
            .calls
            .iter()
            .find_map(|c| self.callees.resolve(c, cur_self))
        {
            let (key, line) = &self.callees.target[i];
            n.shape = Shape::Subroutine;
            n.goto_line = Some(*line);
            n.goto_key = Some(key.clone());
        } else if scan.io {
            n.shape = Shape::Io;
        }
    }

    /// Verbatim source of `span`, whitespace squeezed to single spaces.
    fn snippet(&self, span: Span) -> String {
        self.text_between(span.start(), span.end())
    }

    /// Verbatim source from `s` to `e`, whitespace squeezed to single spaces.
    fn text_between(&self, s: proc_macro2::LineColumn, e: proc_macro2::LineColumn) -> String {
        if s.line == 0 || s.line > self.lines.len() {
            return String::new();
        }
        let cut = |line: usize, from: usize, to: Option<usize>| -> String {
            let cs: Vec<char> = self.lines[line - 1].chars().collect();
            let a = from.min(cs.len());
            let b = to.map(|t| t.min(cs.len())).unwrap_or(cs.len()).max(a);
            cs[a..b].iter().collect()
        };
        let raw = if s.line == e.line {
            cut(s.line, s.column, Some(e.column))
        } else {
            let mut parts = vec![cut(s.line, s.column, None)];
            for i in (s.line + 1)..e.line.min(self.lines.len() + 1) {
                parts.push(self.lines[i - 1].trim().to_string());
            }
            if e.line <= self.lines.len() {
                parts.push(cut(e.line, 0, Some(e.column)).trim().to_string());
            }
            parts.join(" ")
        };
        squeeze(&raw)
    }

    fn in_generated(&self, line: usize) -> bool {
        self.generated.iter().any(|&(a, b)| line >= a && line <= b)
    }

    /// Append a plain box, folding it into the previous one when both are the
    /// same foldable shape.
    ///
    /// Only `Process` and `Generated` fold. An I-O parallelogram or a
    /// subroutine box is the very thing the reader is looking for — folding one
    /// into a run of `let`s would hide it.
    fn push_plain(&self, n: FlowNode, out: &mut Vec<Flow>, run: &mut Option<FlowNode>) {
        let foldable = matches!(n.shape, Shape::Process | Shape::Generated);
        match run {
            Some(prev) if foldable && prev.shape == n.shape => {
                prev.awaits |= n.awaits;
                prev.try_exit |= n.try_exit;
                // Fold what the incoming box would SHOW. A generated box
                // carries its statement in `detail` and a fixed title in
                // `text`, so folding the title instead repeated the words
                // "generated setup" once per statement.
                let mut rows = n.detail;
                if rows.is_empty() {
                    rows.push(n.text);
                }
                for row in rows {
                    if prev.detail.len() < RUN_DETAIL_MAX {
                        prev.detail.push(row);
                    } else {
                        prev.hidden += 1;
                    }
                }
            }
            _ => {
                flush(run, out);
                if foldable {
                    *run = Some(n);
                } else {
                    out.push(Flow::Node(n));
                }
            }
        }
    }
}

/// Emit the pending run, if any.
fn flush(run: &mut Option<FlowNode>, out: &mut Vec<Flow>) {
    if let Some(n) = run.take() {
        out.push(Flow::Node(n));
    }
}

// ── Classification ───────────────────────────────────────────────────────────

/// Method names that unmistakably touch the outside world.
///
/// A deliberately conservative list: a false parallelogram is a lie about the
/// program, while a missed one only costs a rectangle. Phase 3 replaces this
/// with the IDE's OWN knowledge — it generated `usart1`, so it knows what that
/// handle is — and the heuristic then only has to cover hand-written code.
const IO_METHODS: &[&str] = &[
    "read",
    "read_exact",
    "read_data",
    "read_byte",
    "read_bytes",
    "read_raw",
    "write",
    "write_all",
    "write_str",
    "write_fmt",
    "write_byte",
    "write_bytes",
    "flush",
    "send",
    "recv",
    "receive",
    "transfer",
    "transfer_in_place",
    "transaction",
    "set_high",
    "set_low",
    "toggle",
    "set_duty",
    "set_duty_cycle",
    "is_high",
    "is_low",
    "get_level",
    "set_level",
    "blocking_read",
    "blocking_write",
    "blocking_flush",
    "wait_for_high",
    "wait_for_low",
    "wait_for_rising_edge",
    "wait_for_falling_edge",
    "wait_for_any_edge",
];

/// Macros that print or log — the `Print X` parallelogram of the textbook.
const IO_MACROS: &[&str] = &[
    "print", "println", "eprint", "eprintln", "info", "warn", "error", "debug", "trace",
];

fn is_io_macro(path: &syn::Path) -> bool {
    path.segments
        .last()
        .map(|s| IO_MACROS.contains(&s.ident.to_string().as_str()))
        .unwrap_or(false)
}

/// What one statement's expression contains.
///
/// Closures and inner `async` blocks are NOT descended into: a `?` or `.await`
/// inside a closure belongs to the closure, not to the function being drawn,
/// and an arrow leaving this box for the function's end would be wrong.
#[derive(Default)]
struct Scan {
    awaits: bool,
    try_exit: bool,
    io: bool,
    calls: Vec<Call>,
}

impl<'ast> Visit<'ast> for Scan {
    fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}

    fn visit_expr_async(&mut self, _: &'ast syn::ExprAsync) {}

    fn visit_expr_await(&mut self, e: &'ast syn::ExprAwait) {
        self.awaits = true;
        syn::visit::visit_expr_await(self, e);
    }

    fn visit_expr_try(&mut self, e: &'ast syn::ExprTry) {
        self.try_exit = true;
        syn::visit::visit_expr_try(self, e);
    }

    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        let name = e.method.to_string();
        if IO_METHODS.contains(&name.as_str()) {
            self.io = true;
        }
        self.calls.push(Call::Method(name));
        syn::visit::visit_expr_method_call(self, e);
    }

    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*e.func {
            let segs = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            self.calls.push(Call::Path(segs));
        }
        syn::visit::visit_expr_call(self, e);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if is_io_macro(&m.path) {
            self.io = true;
        }
    }
}

/// The entry-point kind an attribute list declares.
fn entry_kind(attrs: &[syn::Attribute]) -> EntryKind {
    for a in attrs {
        let Some(last) = a.path().segments.last() else {
            continue;
        };
        match last.ident.to_string().as_str() {
            // `#[entry]`, `#[embassy_executor::main]`, `#[esp_rtos::main]`.
            "entry" | "main" | "init" => return EntryKind::Main,
            // `#[esp_hal::handler]` is the ESP's GPIO interrupt: the generator
            // emits it, and it read as a plain `fn` in the list.
            "interrupt" | "handler" => return EntryKind::Interrupt,
            // RTIC's `#[task(binds = EXTI0)]` IS an interrupt handler; a plain
            // `#[task]` (RTIC software task, embassy task) is not.
            "task" => {
                let binds = match &a.meta {
                    syn::Meta::List(l) => l.tokens.to_string().contains("binds"),
                    _ => false,
                };
                return if binds {
                    EntryKind::Interrupt
                } else {
                    EntryKind::Task
                };
            }
            "idle" => return EntryKind::Task,
            _ => {}
        }
    }
    EntryKind::Function
}

// ── Reachability ─────────────────────────────────────────────────────────────

/// Whether control can reach the END of this piece of flow.
///
/// Drives whether the chart gets an `END` terminal at all. A `Seq` needs EVERY
/// element to fall through: once one does not, whatever follows is unreachable
/// and so is the sequence's own exit.
pub fn falls_through(f: &Flow) -> bool {
    match f {
        Flow::Node(_) => true,
        Flow::Jump { .. } => false,
        Flow::Seq(v) => v.iter().all(falls_through),
        Flow::Branch { arms, .. } => arms.iter().any(|a| falls_through(&a.body)),
        // A tested loop can always fail its test and fall out; an infinite one
        // is only left by a `break` aimed at it.
        Flow::Loop { head, body, .. } => match head {
            LoopHead::Infinite => breaks_out(body, 1),
            _ => true,
        },
    }
}

/// Whether `f` contains a `break` that leaves the loop `k` levels above it.
fn breaks_out(f: &Flow, k: usize) -> bool {
    match f {
        Flow::Node(_) => false,
        Flow::Jump { target, .. } => matches!(target, Target::Break(d) if *d >= k),
        Flow::Seq(v) => v.iter().any(|x| breaks_out(x, k)),
        Flow::Branch { arms, .. } => arms.iter().any(|a| breaks_out(&a.body, k)),
        Flow::Loop { body, .. } => breaks_out(body, k + 1),
    }
}

// ── Text helpers ─────────────────────────────────────────────────────────────

/// Collapse every run of whitespace to a single space and trim.
fn squeeze(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

/// At most `max` characters, with an ellipsis when something was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chart(src: &str) -> Chart {
        charts_of(src).expect("parses").pop().expect("one chart")
    }

    /// `charts_of` empties proc-macro2's span map before each parse, so every
    /// line number has to come out of the parse that produced it — a repeated
    /// parse, and a failing one in between, must not shift them.
    #[test]
    fn line_numbers_survive_repeated_parses() {
        let src = "\n\nfn first() {}\n\n\nfn second() {}\n";
        let lines = |cs: Vec<Chart>| cs.iter().map(|c| c.line).collect::<Vec<_>>();
        let once = lines(charts_of(src).expect("parses"));
        assert_eq!(once, [3, 6]);
        let err = charts_of("\n\n\nfn broken( {").expect_err("a syntax error");
        assert_eq!(err.line, 4);
        assert_eq!(lines(charts_of(src).expect("parses")), once);
    }

    fn seq(f: &Flow) -> &[Flow] {
        match f {
            Flow::Seq(v) => v,
            other => panic!("expected a Seq, got {other:?}"),
        }
    }

    /// The readability rule the whole view depends on: a run of ordinary
    /// statements is ONE box. Five `let`s drawn as five rectangles is what makes
    /// a real function unreadable.
    #[test]
    fn a_run_of_plain_statements_collapses_to_one_box() {
        let c = chart("fn f() { let a = 1; let b = 2; let c = a + b; x = c; }");
        let body = seq(&c.body);
        assert_eq!(body.len(), 1, "expected one folded box, got {body:?}");
        let Flow::Node(n) = &body[0] else {
            panic!("not a node")
        };
        assert_eq!(n.shape, Shape::Process);
        assert_eq!(n.text, "let a = 1;");
        assert_eq!(n.detail.len(), 3, "the other three statements fold in");
    }

    /// An I-O parallelogram must never be swallowed by the run beside it — it
    /// is the thing the reader came for.
    #[test]
    fn an_io_statement_breaks_the_run() {
        let c = chart("fn f() { let a = 1; uart.write(&buf); let b = 2; }");
        let body = seq(&c.body);
        assert_eq!(body.len(), 3, "run / io / run, got {body:?}");
        let Flow::Node(io) = &body[1] else {
            panic!("not a node")
        };
        assert_eq!(io.shape, Shape::Io);
    }

    #[test]
    fn a_print_macro_is_io() {
        let c = chart("fn f() { println!(\"{}\", x); }");
        let Flow::Node(n) = &seq(&c.body)[0] else {
            panic!()
        };
        assert_eq!(n.shape, Shape::Io);
    }

    /// A call to a function of this same file is a subroutine box that knows
    /// where to jump.
    #[test]
    fn a_local_call_becomes_a_clickable_subroutine() {
        let src = "fn helper() {}\nfn f() {\n    helper();\n}\n";
        let c = charts_of(src).unwrap().pop().unwrap();
        let Flow::Node(n) = &seq(&c.body)[0] else {
            panic!()
        };
        assert_eq!(n.shape, Shape::Subroutine);
        assert_eq!(n.goto_line, Some(1), "jumps to `helper`'s own line");
    }

    /// A call to something that is not in this file stays a plain box — the
    /// chart must not offer a jump it cannot make.
    #[test]
    fn an_unknown_call_is_not_a_subroutine() {
        let c = chart("fn f() { some_external_thing(); }");
        let Flow::Node(n) = &seq(&c.body)[0] else {
            panic!()
        };
        assert_eq!(n.shape, Shape::Process);
        assert_eq!(n.goto_line, None);
    }

    /// `if` with no `else` still gets a NO arm — an empty one, which is the
    /// line that goes straight down to the join.
    #[test]
    fn an_if_without_else_has_an_empty_no_arm() {
        let c = chart("fn f() { if x > 3 { go(); } }");
        let Flow::Branch { cond, arms } = &seq(&c.body)[0] else {
            panic!("not a branch")
        };
        assert_eq!(cond.text, "x > 3", "the label is the user's own text");
        assert_eq!(cond.shape, Shape::Decision);
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[1].label, "NO");
        assert!(matches!(&arms[1].body, Flow::Seq(v) if v.is_empty()));
    }

    /// `else if` is a diamond INSIDE the NO arm, not a third arm of the first
    /// diamond — that is what the code actually does.
    #[test]
    fn else_if_nests_in_the_no_arm() {
        let c = chart("fn f() { if a { p(); } else if b { q(); } else { r(); } }");
        let Flow::Branch { arms, .. } = &seq(&c.body)[0] else {
            panic!()
        };
        let Flow::Branch { cond, arms: inner } = &arms[1].body else {
            panic!("the NO arm should hold the second diamond")
        };
        assert_eq!(cond.text, "b");
        assert_eq!(inner.len(), 2);
    }

    #[test]
    fn a_match_gets_one_labelled_arm_per_pattern() {
        let src =
            "fn f() { match mode { Mode::Night => a(), Mode::Day | Mode::Dusk => b(), _ => {} } }";
        let c = chart(src);
        let Flow::Branch { cond, arms } = &seq(&c.body)[0] else {
            panic!()
        };
        assert_eq!(cond.text, "match mode");
        let labels: Vec<&str> = arms.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(labels, ["Mode::Night", "Mode::Day | Mode::Dusk", "_"]);
    }

    #[test]
    fn a_match_guard_is_part_of_the_arm_label() {
        let c = chart("fn f() { match n { x if x > 3 => a(), _ => b() } }");
        let Flow::Branch { arms, .. } = &seq(&c.body)[0] else {
            panic!()
        };
        assert_eq!(arms[0].label, "x if x > 3");
    }

    /// Firmware's endless main loop: no path returns, so no END terminal.
    #[test]
    fn an_endless_loop_leaves_the_chart_without_an_end() {
        let c = chart("fn main() { init(); loop { tick(); } }");
        assert!(c.diverges, "nothing after an endless loop can be reached");
    }

    /// The same loop with a way out DOES end — the textbook example's shape.
    #[test]
    fn a_loop_with_a_break_can_reach_the_end() {
        let c = chart("fn main() { loop { x += 1; if x > 20 { break; } } }");
        assert!(!c.diverges);
    }

    /// A `break` in a NESTED loop does not open the outer one.
    #[test]
    fn a_break_in_an_inner_loop_does_not_end_the_outer() {
        let c = chart("fn main() { loop { loop { break; } } }");
        assert!(
            c.diverges,
            "the break belongs to the inner loop; the outer one is still endless"
        );
    }

    /// ...unless it names the outer loop's label.
    #[test]
    fn a_labelled_break_reaches_the_loop_it_names() {
        let c = chart("fn main() { 'outer: loop { loop { break 'outer; } } }");
        assert!(!c.diverges, "break 'outer leaves both loops");
    }

    #[test]
    fn a_return_type_of_never_diverges_even_with_a_reachable_tail() {
        let c = chart("fn main() -> ! { setup(); loop { if done() { break; } } }");
        assert!(c.diverges, "`-> !` is the function's own promise");
    }

    #[test]
    fn a_while_loop_can_always_fall_out() {
        let c = chart("fn f() { while go() { step(); } }");
        assert!(!c.diverges);
    }

    /// `.await` is marked, and it survives folding into a run.
    #[test]
    fn await_is_marked_on_the_box() {
        let c = chart("async fn f() { let x = 1; rx.read(&mut b).await; }");
        let nodes = seq(&c.body);
        let awaited = nodes.iter().any(|f| matches!(f, Flow::Node(n) if n.awaits));
        assert!(awaited, "the await point must be visible: {nodes:?}");
        assert!(c.is_async);
    }

    /// A `?` in a closure is the CLOSURE's early exit, not the function's —
    /// drawing an arrow to this function's end would be wrong.
    #[test]
    fn a_question_mark_inside_a_closure_is_not_this_functions_exit() {
        let c = chart("fn f() { run(|| { inner()?; Ok(()) }); }");
        let Flow::Node(n) = &seq(&c.body)[0] else {
            panic!()
        };
        assert!(!n.try_exit);
    }

    #[test]
    fn a_question_mark_in_the_statement_itself_is_marked() {
        let c = chart("fn f() { let v = thing()?; }");
        let Flow::Node(n) = &seq(&c.body)[0] else {
            panic!()
        };
        assert!(n.try_exit);
    }

    /// The generated init block is ONE dimmed box, however many statements it
    /// holds — it is not the user's algorithm.
    #[test]
    fn the_generated_block_collapses_to_a_single_box() {
        let src = "fn main() {\n\
                   // <<< GENERATED BEGIN — do not edit between these markers >>>\n\
                   let p = init();\n\
                   let mut u = Uart::new(p.U1);\n\
                   let mut s = Spi::new(p.S1);\n\
                   // <<< GENERATED END >>>\n\
                   run();\n\
                   }\n";
        let c = charts_of(src).unwrap().pop().unwrap();
        let body = seq(&c.body);
        assert_eq!(body.len(), 2, "one generated box + the user's code");
        let Flow::Node(g) = &body[0] else { panic!() };
        assert_eq!(g.shape, Shape::Generated);
        assert_eq!(g.detail.len(), 3, "all three init statements fold in");
    }

    /// The folded rows must be the STATEMENTS, not the box's own title. Folding
    /// `text` instead printed "generated setup" once per statement — a box that
    /// says its own name five times and hides what it actually does.
    #[test]
    fn the_generated_box_lists_the_statements_not_its_own_title() {
        let src = "fn main() {
                   // <<< GENERATED BEGIN >>>
                   let p = init();
                   let u = Uart::new(p.U1);
                   // <<< GENERATED END >>>
                   }
";
        let c = charts_of(src).unwrap().pop().unwrap();
        let Flow::Node(g) = &seq(&c.body)[0] else {
            panic!()
        };
        assert_eq!(g.text, "generated setup");
        assert_eq!(g.detail, ["let p = init();", "let u = Uart::new(p.U1);"]);
    }

    /// `loop {}` — the shape of every generated `panic_handler`. With an empty
    /// body the chart drew a back edge from nothing to nothing, which reads as
    /// broken rather than as an idle spin.
    #[test]
    fn an_empty_loop_still_has_something_to_loop_around() {
        let c = chart("fn panic() -> ! { loop {} }");
        let Flow::Loop { body, .. } = &seq(&c.body)[0] else {
            panic!("not a loop")
        };
        let inner = seq(body);
        assert_eq!(inner.len(), 1, "the loop needs a box to circle back to");
        let Flow::Node(n) = &inner[0] else { panic!() };
        assert_eq!(n.text, "loop {}");
        assert!(c.diverges, "and it still never ends");
    }

    #[test]
    fn generated_ranges_reads_the_config_file_spelling_too() {
        let src =
            "// <<< GENERATED>>>\npub const B: u32 = 9600;\n// <<< GENERATED END >>>\nuse x;\n";
        assert_eq!(generated_ranges(src), vec![(1, 3)]);
    }

    /// Entry points are what the chart list leads with, so each attribute shape
    /// the generator emits has to be recognised.
    #[test]
    fn every_generated_entry_attribute_is_recognised() {
        let cases: [(&str, EntryKind); 7] = [
            ("#[entry] fn main() {}", EntryKind::Main),
            (
                "#[embassy_executor::main] async fn main(s: Spawner) {}",
                EntryKind::Main,
            ),
            ("#[esp_rtos::main] async fn main() {}", EntryKind::Main),
            (
                "#[embassy_executor::task] async fn radar() {}",
                EntryKind::Task,
            ),
            ("#[interrupt] fn EXTI0() {}", EntryKind::Interrupt),
            (
                "#[task(binds = EXTI0, local = [led])] fn on_pin(c: on_pin::Context) {}",
                EntryKind::Interrupt,
            ),
            (
                "#[task] fn software(c: software::Context) {}",
                EntryKind::Task,
            ),
        ];
        for (src, want) in cases {
            let c = charts_of(src).unwrap().pop().unwrap();
            assert_eq!(c.kind, want, "for {src}");
        }
    }

    /// RTIC puts the whole application inside `#[rtic::app] mod app { … }`; a
    /// walk that stops at items would find nothing at all there.
    #[test]
    fn rtic_functions_are_found_inside_the_app_module() {
        let src = "#[rtic::app(device = pac)]\n\
                   mod app {\n\
                       #[init]\n\
                       fn init(cx: init::Context) -> (Shared, Local) { boot(); }\n\
                       #[task(binds = EXTI0)]\n\
                       fn on_pin(cx: on_pin::Context) { toggle(); }\n\
                   }\n";
        let charts = charts_of(src).unwrap();
        let names: Vec<&str> = charts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["init", "on_pin"]);
        assert_eq!(charts[1].kind, EntryKind::Interrupt);
    }

    #[test]
    fn impl_methods_are_charted_under_their_type() {
        let src = "struct P;\nimpl P {\n    fn feed(&mut self, b: u8) { self.n += 1; }\n}\n";
        let charts = charts_of(src).unwrap();
        assert_eq!(charts[0].name, "P::feed");
    }

    /// A half-typed file must not blank the tab — the caller keeps the last
    /// good chart and needs the line to say where the trouble is.
    #[test]
    fn a_syntax_error_reports_its_line() {
        let err = charts_of("fn a() {\n    if x {\n").unwrap_err();
        assert_eq!(err.line, 2);
    }

    /// Labels come out of the source verbatim, including operators and calls —
    /// a reconstruction would not match what the user reads in the editor.
    #[test]
    fn a_condition_label_is_the_users_own_text() {
        let c =
            chart("fn f() {\n    if dist < THRESHOLD && is_night() {\n        go();\n    }\n}\n");
        let Flow::Branch { cond, .. } = &seq(&c.body)[0] else {
            panic!()
        };
        assert_eq!(cond.text, "dist < THRESHOLD && is_night()");
        assert_eq!(cond.line, 2, "and it knows the line to jump to");
    }

    #[test]
    fn a_for_loop_reads_as_for_each() {
        let c = chart("fn f() { for byte in buf.iter() { feed(*byte); } }");
        let Flow::Loop { head, .. } = &seq(&c.body)[0] else {
            panic!()
        };
        let LoopHead::For(n) = head else { panic!() };
        assert_eq!(n.text, "for byte in buf.iter()");
    }

    #[test]
    fn long_labels_are_elided_not_wrapped() {
        let long = "a".repeat(200);
        let c = chart(&format!("fn f() {{ if {long} {{ go(); }} }}"));
        let Flow::Branch { cond, .. } = &seq(&c.body)[0] else {
            panic!()
        };
        assert_eq!(cond.text.chars().count(), LABEL_MAX);
        assert!(cond.text.ends_with('…'));
    }
}

/// The element model behind "All — whole file", and the keys that pick a chart.
#[cfg(test)]
mod elements {
    use super::*;

    /// One of every item kind `syn` hands back, plus members of each container.
    /// Line numbers matter: `radar_task` has a doc comment on 11, its
    /// attribute on 12, its name on 13 and its closing brace on 15.
    const ALL_KINDS: &str = r#"#![no_std]
#![no_main]

use core::fmt;
use core::cell::RefCell;

extern crate alloc;

mod pins;

/// The radar.
#[embassy_executor::task]
async fn radar_task() {
    helper();
}

fn helper() {}

struct Frame { a: u8, b: u16 }
struct Id(u8);
struct Marker;
enum Mode { Idle, Busy(u8), At { x: i32 }, Night = 3 }
union Bits { i: u32, f: f32 }
trait Feed { fn feed(&mut self, b: u8); fn reset(&mut self) { self.feed(0); } const N: usize; type Out; }
impl Feed for Frame { fn feed(&mut self, b: u8) { self.a = b; } const N: usize = 2; type Out = u8; }
impl Frame { fn new() -> Self { Frame { a: 0, b: 0 } } }
const LIMIT: u32 = 10;
static COUNT: u32 = 0;
type Handle = Frame;
macro_rules! twice { ($e:expr) => { $e; $e } }
bind_interrupts!(struct Irqs { USART1 => Handler; });
extern "C" { fn ext(x: i32) -> i32; }
mod inner { pub fn init() {} }
"#;

    fn model(src: &str) -> FileModel {
        parse_file(src).expect("parses")
    }

    fn find<'m>(m: &'m FileModel, key: &str) -> &'m Element {
        m.elements
            .iter()
            .find(|e| e.key == key)
            .unwrap_or_else(|| panic!("no element {key:?} in {:?}", keys(m)))
    }

    fn keys(m: &FileModel) -> Vec<&str> {
        m.elements.iter().map(|e| e.key.as_str()).collect()
    }

    /// Every item kind becomes an element - none is skipped the way the chart
    /// walk used to skip everything that was not a function.
    #[test]
    fn every_item_kind_is_an_element() {
        let m = model(ALL_KINDS);
        let got: Vec<(usize, &str, &str)> = m
            .elements
            .iter()
            .map(|e| (e.depth, e.kind.word(), e.name.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (0, "#![..]", "crate attributes"),
                (0, "use", "use"),
                (0, "crate", "alloc"),
                (0, "mod", "pins"),
                (0, "task", "radar_task"),
                (0, "fn", "helper"),
                (0, "struct", "Frame"),
                (0, "struct", "Id"),
                (0, "struct", "Marker"),
                (0, "enum", "Mode"),
                (0, "union", "Bits"),
                (0, "trait", "Feed"),
                (1, "fn;", "feed"),
                (1, "fn", "reset"),
                (1, "const", "N"),
                (1, "type", "Out"),
                (0, "impl", "Feed for Frame"),
                (1, "fn", "feed"),
                (1, "const", "N"),
                (1, "type", "Out"),
                (0, "impl", "Frame"),
                (1, "fn", "new"),
                (0, "const", "LIMIT"),
                (0, "static", "COUNT"),
                (0, "type", "Handle"),
                (0, "macro", "twice"),
                (0, "macro!", "bind_interrupts!"),
                (0, "extern", "extern \"C\""),
                (1, "fn;", "ext"),
                (0, "mod", "inner"),
                (1, "fn", "init"),
            ]
        );
    }

    /// What a row shows after its kind word - the declaration, not a copy of
    /// the word.
    #[test]
    fn the_row_text_is_the_declaration() {
        let m = model(ALL_KINDS);
        let sig = |k: &str| find(&m, k).signature.clone();
        assert_eq!(sig("#![..]"), "no_std, no_main");
        assert_eq!(
            sig("use core::fmt"),
            "2 imports: core::fmt, core::cell::RefCell"
        );
        assert_eq!(sig("radar_task"), "radar_task()");
        assert_eq!(sig("struct Frame"), "Frame · 2 fields");
        assert_eq!(sig("struct Id"), "Id(u8)");
        assert_eq!(sig("struct Marker"), "Marker");
        assert_eq!(sig("enum Mode"), "Mode · 4 variants");
        assert_eq!(sig("Feed::feed"), "feed(&mut self, b: u8);");
        assert_eq!(sig("impl Feed for Frame"), "Feed for Frame · 3 items");
        assert_eq!(sig("const LIMIT"), "LIMIT: u32 = 10");
        assert_eq!(sig("static COUNT"), "COUNT: u32 = 0");
        assert_eq!(sig("type Handle"), "Handle = Frame");
        assert_eq!(sig("mod pins"), "pins;");
        assert_eq!(
            find(&m, "struct Frame").detail,
            ["struct Frame", "a: u8", "b: u16"]
        );
        assert_eq!(
            find(&m, "enum Mode").detail,
            ["enum Mode", "Idle", "Busy(u8)", "At { x }", "Night = 3"]
        );
    }

    /// Three lines per element, for three different jobs: the click target,
    /// and the extent that includes the docs and ends at the brace.
    #[test]
    fn an_element_knows_its_name_line_and_its_whole_extent() {
        let m = model(ALL_KINDS);
        let r = find(&m, "radar_task");
        assert_eq!((r.start_line, r.ident_line, r.end_line), (11, 13, 15));
        assert_eq!(
            m.charts[r.chart.unwrap()].line,
            13,
            "the chart's line is the name"
        );
        let u = find(&m, "use core::fmt");
        assert_eq!(
            (u.start_line, u.end_line),
            (4, 5),
            "the run spans both uses"
        );
    }

    /// The invariants every consumer of the list leans on: members lie inside
    /// their container, siblings follow each other without overlapping, and
    /// each extent contains its name.
    #[test]
    fn extents_nest_and_follow_each_other() {
        for src in [ALL_KINDS, RTIC] {
            let m = model(src);
            for (i, e) in m.elements.iter().enumerate() {
                assert!(
                    e.start_line <= e.ident_line && e.ident_line <= e.end_line,
                    "{}: {}..{}..{}",
                    e.key,
                    e.start_line,
                    e.ident_line,
                    e.end_line
                );
                if let Some(p) = e.parent {
                    let p = &m.elements[p];
                    assert!(
                        p.start_line <= e.start_line && e.end_line <= p.end_line,
                        "{} in {}",
                        e.key,
                        p.key
                    );
                    assert_eq!(e.depth, p.depth + 1);
                }
                // `>=`: lines are the unit, and two members may share one
                // (`trait Feed { fn a(); fn b(); }`).
                if let Some(next) = m.elements[i + 1..].iter().find(|n| n.parent == e.parent) {
                    assert!(
                        next.start_line >= e.end_line,
                        "{} overlaps {}",
                        e.key,
                        next.key
                    );
                }
            }
        }
    }

    const RTIC: &str = "#[rtic::app(device = pac)]\nmod app {\n    #[init]\n    fn init(cx: init::Context) -> (Shared, Local) { boot(); }\n    fn boot() {}\n}\n";

    /// Keys are unique, and a function's key is the name the tab saved before
    /// keys existed - so an older project reopens on the same chart.
    #[test]
    fn keys_are_unique_and_keep_the_old_names() {
        for src in [ALL_KINDS, RTIC, TWINS] {
            let m = model(src);
            let mut ks = keys(&m);
            let n = ks.len();
            ks.sort();
            ks.dedup();
            assert_eq!(ks.len(), n, "duplicate keys in {:?}", keys(&m));
            for c in &m.charts {
                assert_eq!(
                    m.elements
                        .iter()
                        .filter(|e| e.chart.is_some_and(|i| m.charts[i].key == c.key))
                        .count(),
                    1,
                    "{} is one element's chart",
                    c.key
                );
            }
        }
        let m = model(ALL_KINDS);
        let chart = |k: &str| m.charts.iter().find(|c| c.key == k).unwrap();
        assert_eq!(chart("helper").name, "helper");
        assert_eq!(chart("Frame::new").name, "Frame::new");
        assert_eq!(chart("<Frame as Feed>::feed").name, "Frame::feed");
        assert_eq!(chart("Feed::reset").name, "Feed::reset");
        assert_eq!(chart("inner::init").name, "init");
    }

    /// The names that used to collide: the second of each pair could never be
    /// opened, because the tab picked charts by name and always found the first.
    const TWINS: &str = "struct Foo;\n\
                         impl Foo { fn new() -> Self { Foo } }\n\
                         impl Default for Foo { fn default() -> Self { Foo } }\n\
                         impl Clone for Foo { fn clone(&self) -> Self { Foo::new() } }\n\
                         trait Named { fn new() -> Self; }\n\
                         impl Named for Foo { fn new() -> Self { Foo } }\n\
                         mod a { pub fn init() {} }\n\
                         mod b { pub fn init() {} }\n\
                         #[cfg(feature = \"x\")]\nfn setup() {}\n\
                         #[cfg(not(feature = \"x\"))]\nfn setup() {}\n";

    #[test]
    fn same_named_functions_get_different_keys() {
        let m = model(TWINS);
        let ks: Vec<&str> = m.charts.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(
            ks,
            [
                "Foo::new",
                "<Foo as Default>::default",
                "<Foo as Clone>::clone",
                "<Foo as Named>::new",
                "a::init",
                "b::init",
                "setup",
                "setup#2",
            ]
        );
    }

    fn subroutines(f: &Flow, out: &mut Vec<(String, Option<String>)>) {
        match f {
            Flow::Node(n) | Flow::Jump { node: n, .. } => {
                if n.shape == Shape::Subroutine {
                    out.push((n.text.clone(), n.goto_key.clone()));
                }
            }
            Flow::Seq(v) => v.iter().for_each(|x| subroutines(x, out)),
            Flow::Branch { arms, .. } => arms.iter().for_each(|a| subroutines(&a.body, out)),
            Flow::Loop { body, .. } => subroutines(body, out),
        }
    }

    fn calls_in(m: &FileModel, key: &str) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        subroutines(
            &m.charts.iter().find(|c| c.key == key).unwrap().body,
            &mut out,
        );
        out
    }

    /// A call opens a function only when it can mean exactly one. The old map
    /// went by the bare name, so `b.feed()` opened `A::feed` and `u.read(..)`
    /// opened an unrelated free `fn read`.
    #[test]
    fn a_call_opens_only_the_one_function_it_can_mean() {
        let src = "struct A; struct B; struct U;\n\
                   impl A { fn feed(&self) {} fn tick(&self) { Self::feed(self); } }\n\
                   impl B { fn feed(&self) {} fn only_b(&self) {} }\n\
                   fn read() {}\n\
                   mod app { pub fn go() {} }\n\
                   fn run(b: B, u: U) {\n\
                       b.feed();\n\
                       u.read();\n\
                       b.only_b();\n\
                       A::feed(&A);\n\
                       app::go();\n\
                       crate::read();\n\
                       read();\n\
                       nowhere::go();\n\
                   }\n";
        let m = model(src);
        let calls = calls_in(&m, "run");
        let keys: Vec<Option<&str>> = calls.iter().map(|(_, k)| k.as_deref()).collect();
        assert_eq!(
            keys,
            [
                Some("B::only_b"),
                Some("A::feed"),
                Some("app::go"),
                Some("read"),
                Some("read"),
            ],
            "ambiguous `b.feed()`, the method call `u.read()` and the unknown `nowhere::go()` are not subroutines: {calls:?}"
        );
        // `Self::` means the impl the method is in.
        assert_eq!(calls_in(&m, "A::tick")[0].1.as_deref(), Some("A::feed"));
    }

    /// The generator's ESP GPIO interrupt reads as one.
    #[test]
    fn an_esp_handler_is_an_interrupt() {
        let m = model("#[esp_hal::handler]\nfn gpio_irq() {}\n");
        assert_eq!(m.charts[0].kind, EntryKind::Interrupt);
        assert_eq!(m.elements[0].kind.word(), "irq");
    }

    /// Only a run of `use`s folds: anything between two of them starts a new row.
    #[test]
    fn only_adjacent_uses_fold() {
        let m = model("use a::b;\nuse c::d;\nfn f() {}\nuse e::f;\n");
        let uses: Vec<&Element> = m
            .elements
            .iter()
            .filter(|e| e.kind == ElementKind::Use)
            .collect();
        assert_eq!(uses.len(), 2);
        assert_eq!(uses[0].detail, ["a::b", "c::d"]);
        assert_eq!(uses[1].detail, ["e::f"]);
    }

    /// Generated means WHOLLY inside a marker pair: a `main` that opens in the
    /// generated block and ends in the user's tail is the user's.
    #[test]
    fn generated_means_wholly_inside_the_markers() {
        let src = "fn main() {\n\
                   // <<< GENERATED BEGIN >>>\n\
                   let x = 1;\n\
                   // <<< GENERATED END >>>\n\
                   }\n\
                   // <<< GENERATED>>>\n\
                   pub const A: u32 = 1;\n\
                   // <<< GENERATED END >>>\n\
                   fn user() {}\n";
        let m = model(src);
        let flags: Vec<(&str, bool)> = m
            .elements
            .iter()
            .map(|e| (e.key.as_str(), e.generated))
            .collect();
        assert_eq!(flags, [("main", false), ("const A", true), ("user", false)]);

        // The shape the generator really writes: `main` OPENS inside the
        // block (its attribute is generated too) and closes in the user's
        // tail. Starting inside is not being inside.
        let src = "// <<< GENERATED BEGIN >>>\n\
                   use x::y;\n\
                   #[entry]\n\
                   fn main() -> ! {\n\
                   let p = 1;\n\
                   // <<< GENERATED END >>>\n\
                   loop {}\n\
                   }\n";
        let m = model(src);
        let main = find(&m, "main");
        assert!(!main.generated, "main ends in the user's tail");
        assert!(find(&m, "use x::y").generated);
    }

    /// `#[cfg(test)]` marks the module and everything in it; `cfg(not(test))`
    /// is the opposite and marks nothing.
    #[test]
    fn test_modules_are_marked() {
        let m = model(
            "#[cfg(test)]\nmod tests { fn t() {} }\n#[cfg(not(test))]\nmod real { fn r() {} }\n",
        );
        let flags: Vec<(&str, bool)> = m
            .elements
            .iter()
            .map(|e| (e.key.as_str(), e.test_code))
            .collect();
        assert_eq!(
            flags,
            [
                ("mod tests", true),
                ("tests::t", true),
                ("mod real", false),
                ("real::r", false),
            ]
        );
        assert!(find(&m, "mod tests").signature.ends_with("test code"));
    }

    /// Every item of every file the IDE generates is listed exactly once -
    /// checked against an independent walk of the same `syn` tree, on the text
    /// the generator really writes (main.rs AND the config files), for every
    /// bundled chip on both runtimes.
    #[test]
    fn every_generated_item_is_listed() {
        fn expected(items: &[syn::Item]) -> usize {
            let mut n = 0;
            let mut in_use = false;
            for it in items {
                if matches!(it, syn::Item::Use(_)) {
                    n += usize::from(!in_use);
                    in_use = true;
                    continue;
                }
                in_use = false;
                n += 1;
                n += match it {
                    syn::Item::Mod(m) => m.content.as_ref().map_or(0, |(_, i)| expected(i)),
                    syn::Item::Impl(i) => i.items.len(),
                    syn::Item::Trait(t) => t.items.len(),
                    syn::Item::ForeignMod(f) => f.items.len(),
                    _ => 0,
                };
            }
            n
        }
        let mut files = 0;
        for def in crate::panels::mcu_module::builtins::builtin_definitions() {
            for runtime in [
                crate::panels::mcu_module::mcu::Runtime::Blocking,
                crate::panels::mcu_module::mcu::Runtime::Async,
            ] {
                let mut mcu = def.build_mcu();
                mcu.runtime = runtime;
                let mut sources = vec![("main.rs".to_string(), mcu.fresh_main_rs())];
                sources.extend(mcu.config_files());
                for (path, src) in sources {
                    if !path.ends_with(".rs") {
                        continue;
                    }
                    let what = format!("{} {runtime:?} {path}", def.id);
                    let m = parse_file(&src)
                        .unwrap_or_else(|e| panic!("{what}: line {}: {}", e.line, e.message));
                    let file = syn::parse_file(&src).unwrap();
                    let want = expected(&file.items) + usize::from(!file.attrs.is_empty());
                    assert_eq!(m.elements.len(), want, "{what}: {:?}", keys(&m));
                    let with_chart = m.elements.iter().filter(|e| e.chart.is_some()).count();
                    assert_eq!(with_chart, m.charts.len(), "{what}");
                    files += 1;
                }
            }
        }
        assert!(files > 30, "only {files} generated files checked");
    }
}
