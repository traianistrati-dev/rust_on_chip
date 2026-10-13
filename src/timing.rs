//! The Profile tab's "Timing" mode: the exact time chosen code takes, measured
//! on the target by a cycle counter.
//!
//! Halt-sampling (`crate::flamegraph`) answers "where does the CPU spend its
//! time", and a firmware that waits 98% of the time leaves it nothing to see.
//! This answers "how long does THIS take": the user marks a function or a few
//! statements, and only the BUILD COPY of the project is instrumented - a guard
//! before the first statement, a `drop` after the last, both on lines that
//! already exist, so no line number moves. A generated module appended to the
//! copy's `main.rs` keeps count / min / max / last / total per region in a
//! `#[no_mangle]` table; the firmware is flashed and run through
//! `probe-rs dap-server` like the Debug tab does, and the table is read over
//! the probe once a second.
//!
//! Counters: CSR `0x7e2` (PCCR) on ESP32-C2/C3/C6/H2 - what esp-hal itself
//! reads -, `mcycle` on C5/C61, DWT CYCCNT on Cortex-M3/M4/M7/M33. Cortex-M0
//! has no cycle counter and Xtensa is not built by this IDE.

use crate::flamegraph::{from_base64, send, wait_or_kill, wait_response};
use serde_json::json;
use std::collections::HashMap;
use std::io::Read;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// ── Regions and where their guards go (pure) ────────────────────────────────

/// A region the user asked to time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimingRegion {
    /// Project-relative file of the firmware crate (`src/main.rs`).
    pub file: String,
    /// 1-based lines chosen; for a whole function, any line inside it.
    pub first: u32,
    pub last: u32,
    pub whole_fn: bool,
    /// A whole function: the 0-based char column of the caret it was marked
    /// at, then (once anchored) of the function's or closure's start - with
    /// `first`, WHICH of the callables of that line it is: `.filter(|x| ..)
    /// .for_each(|x| { .. })` puts two on one.
    pub col: u32,
    /// The path of the function it is in (`fade`, `Led::set`,
    /// `<Level as From<u8>>::from`), as [`place`] found it when it was
    /// marked. Empty: not anchored, the line numbers are taken as they are.
    pub func: String,
    /// Which of the file's functions with that path it is, of how many:
    /// `#[cfg]` copies share one.
    pub fn_copy: (u32, u32),
    /// Statements: the trimmed text of every line covered - what finds them
    /// again after an edit moved them (see [`locate`]). Empty for a whole
    /// function, which its path finds.
    pub anchor: Vec<String>,
    /// Which of the identical runs of those lines in the function it is, of
    /// how many: `delay(500);` twice in a loop is two places.
    pub copy: (u32, u32),
}

/// `r` as what `p` found: the lines it covers, its function, and what finds
/// them again after an edit.
pub fn anchored(text: &str, r: TimingRegion, p: &Placement) -> TimingRegion {
    let anchor: Vec<String> = if r.whole_fn {
        Vec::new()
    } else {
        text.lines()
            .skip((p.lines.0 as usize).saturating_sub(1))
            .take((p.lines.1 + 1).saturating_sub(p.lines.0) as usize)
            .map(|l| l.trim().to_owned())
            .collect()
    };
    // `place` parsed this text already: `functions` cannot fail on it.
    let fns = functions(text, &r.file).unwrap_or_default();
    let same: Vec<&FnSeen> = fns.iter().filter(|f| f.path == p.func).collect();
    let index = same
        .iter()
        .position(|f| f.sig == p.fn_line && f.col == p.fn_col)
        .unwrap_or(0);
    let copy = match same.get(index) {
        Some(f) if !anchor.is_empty() => {
            let runs = runs_in(text, f, &anchor);
            let at = runs
                .iter()
                .position(|&i| i + 1 == p.lines.0 as usize)
                .unwrap_or(0);
            (at as u32, runs.len() as u32)
        }
        _ => (0, 1),
    };
    TimingRegion {
        first: p.lines.0,
        last: p.lines.1,
        func: p.func.clone(),
        col: p.fn_col,
        fn_copy: (index as u32, same.len().max(1) as u32),
        anchor,
        copy,
        ..r
    }
}

/// The 0-based first lines of every run of `anchor`'s lines inside `f`.
fn runs_in(text: &str, f: &FnSeen, anchor: &[String]) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let n = anchor.len();
    let lo = f.sig.saturating_sub(1) as usize;
    let hi = (f.close as usize).min(lines.len());
    (lo..hi)
        .filter(|&i| n > 0 && i + n <= hi && lines[i..i + n] == anchor[..])
        .collect()
}

/// Where `r` is in `text` now. A function is the same copy of its path -
/// the second of two `#[cfg]` twins stays the second -, statements the same
/// run of every line they covered inside it. When a count changed (a copy
/// added or gone) nothing says which is which any more, and the region is
/// refused, as it is when its lines changed: nothing is ever timed under a
/// label that names other code.
pub fn locate(text: &str, r: &TimingRegion) -> Result<(u32, u32), String> {
    find(text, r).map(|(first, last, _)| (first, last))
}

/// [`locate`], with the column of a whole function's start.
fn find(text: &str, r: &TimingRegion) -> Result<(u32, u32, u32), String> {
    if r.func.is_empty() {
        return Ok((r.first, r.last, r.col));
    }
    let fns = functions(text, &r.file)?;
    let same: Vec<&FnSeen> = fns.iter().filter(|f| f.path == r.func).collect();
    let (index, count) = r.fn_copy;
    let f = match same.get(index as usize) {
        Some(f) if same.len() as u32 == count => *f,
        _ if same.is_empty() => {
            return Err(format!(
                "{}: `{}` is not there any more - mark it again",
                r.file, r.func
            ));
        }
        _ => {
            return Err(format!(
                "{}: there are {} functions `{}` now ({} when it was marked) - mark it again",
                r.file,
                same.len(),
                r.func,
                count
            ));
        }
    };
    if r.whole_fn || r.anchor.is_empty() {
        return Ok((f.sig, f.close, f.col));
    }
    let runs = runs_in(text, f, &r.anchor);
    let (index, count) = r.copy;
    match runs.get(index as usize) {
        Some(&i) if runs.len() as u32 == count => {
            Ok((i as u32 + 1, (i + r.anchor.len()) as u32, r.col))
        }
        _ if runs.is_empty() => Err(format!(
            "{}: the code timed in `{}` (lines {}-{}) has changed - mark it again",
            r.file, r.func, r.first, r.last
        )),
        _ => Err(format!(
            "{}: the code timed in `{}` (lines {}-{}) is there {} times now ({} when it was marked) - mark it again",
            r.file,
            r.func,
            r.first,
            r.last,
            runs.len(),
            count
        )),
    }
}

/// Where `r` goes in `text` now, checked: found again ([`locate`]), and
/// still whole statements - or the whole body - of the same function,
/// covering exactly those lines. Lines that are now in a block comment, a
/// macro or a string would otherwise place the code AROUND them under the
/// row's label.
pub fn resolve(text: &str, r: &TimingRegion) -> Result<(TimingRegion, Placement), String> {
    let (first, last, col) = find(text, r)?;
    let moved = TimingRegion {
        first,
        last,
        col,
        ..r.clone()
    };
    let p = place(text, &moved)?;
    if r.whole_fn && p.loop_body.is_some() {
        return Err(format!(
            "{}: `{}` now never returns (it ends in an endless loop) - mark it again",
            r.file, r.func
        ));
    }
    if !r.func.is_empty() && (p.func != r.func || p.lines != (first, last)) {
        return Err(format!(
            "{}: the code timed in `{}` (lines {}-{}) has changed - mark it again",
            r.file, r.func, r.first, r.last
        ));
    }
    Ok((moved, p))
}

/// A region as the editor marks it: placed, anchored, and checked to place
/// exactly the same way when Measure looks for it again. A statement that
/// shares a line with a block's brace or with another statement cannot be:
/// the lines stored for it place differently a second time.
///
/// A whole function or closure that ends in an endless `loop` - an embassy
/// task, `fn main() -> !` - never returns, so its own time never ends and its
/// row would stay empty: one pass of that loop is marked instead, as
/// statements (the caller sees `whole_fn` turned off).
pub fn mark(text: &str, asked: TimingRegion) -> Result<(TimingRegion, Placement), String> {
    let placed = place(text, &asked)?;
    if asked.whole_fn
        && let Some((first, last)) = placed.loop_body
    {
        return mark(
            text,
            TimingRegion {
                whole_fn: false,
                first,
                last,
                col: 0,
                ..asked
            },
        );
    }
    let region = anchored(text, asked, &placed);
    match resolve(text, &region) {
        Ok((_, again)) if again == placed => Ok((region, placed)),
        _ => Err(concat!(
            "a statement there shares a line with a brace or with another statement: ",
            "put the code to time on lines of its own",
        )
        .to_owned()),
    }
}

/// A function of a file: its path, signature line and closing line.
struct FnSeen {
    path: String,
    sig: u32,
    /// The 0-based char column it starts at: the `fn` keyword, the closure.
    col: u32,
    close: u32,
    /// What it is: what the tests tell a closure from a function by.
    #[cfg_attr(not(test), allow(dead_code))]
    kind: Callable,
}

/// What a body that can be timed as a whole is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Callable {
    /// A free function (of a module).
    Fn,
    /// A function of an `impl` or a trait.
    Method,
    /// A closure, `async` or not, named by the `let` it is bound to.
    Closure,
}

/// What the path so far has entered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Mod,
    Impl,
    Trait,
    Fn,
    Closure,
}

/// Why `text` does not parse, `None` when it does.
pub fn parse_error(text: &str, file: &str) -> Option<String> {
    functions(text, file).err()
}

/// Every function body of `text`.
fn functions(text: &str, file: &str) -> Result<Vec<FnSeen>, String> {
    use syn::visit::Visit;
    proc_macro2::extra::invalidate_current_thread_spans();
    let parsed = syn::parse_file(text).map_err(|e| {
        format!(
            "{file} does not parse (line {}): {e}",
            e.span().start().line.max(1)
        )
    })?;
    let mut f = Finder::new(0, 0, false);
    f.visit_file(&parsed);
    Ok(f.seen)
}

/// The name of the call whose name the caret is on, at `(1-based line,
/// 0-based char column)`: a function, a method, a closure - also inside a
/// macro's arguments (`println!("{}", f(x))`) and as `<T as Tr>::f(..)`.
/// `None` when the caret is on no call's name. Used only to tell, when
/// rust-analyzer cannot be asked, that the caret meant a call.
pub fn call_name_at(text: &str, line: usize, col: usize) -> Option<String> {
    use syn::visit::Visit;
    struct Calls {
        line: usize,
        col: usize,
        found: Option<String>,
    }
    impl Calls {
        fn on(&mut self, ident: &proc_macro2::Ident) {
            let (a, b) = (ident.span().start(), ident.span().end());
            if a.line == self.line && a.column <= self.col && self.col <= b.column {
                self.found = Some(ident.to_string());
            }
        }
    }
    impl<'ast> Visit<'ast> for Calls {
        fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
            if let syn::Expr::Path(p) = &*c.func
                && let Some(last) = p.path.segments.last()
            {
                self.on(&last.ident);
            }
            syn::visit::visit_expr_call(self, c);
        }
        fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
            self.on(&m.method);
            syn::visit::visit_expr_method_call(self, m);
        }
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            // The arguments of `println!`, `info!`, `join!`, `assert!`: one
            // comma-separated list of expressions, spans and all.
            use syn::parse::Parser;
            let args = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
            if let Ok(exprs) = args.parse2(m.tokens.clone()) {
                for e in &exprs {
                    self.visit_expr(e);
                }
            }
        }
    }
    proc_macro2::extra::invalidate_current_thread_spans();
    let file = syn::parse_file(text).ok()?;
    let mut calls = Calls {
        line,
        col,
        found: None,
    };
    calls.visit_file(&file);
    calls.found
}

/// The function or closure a definition names, from the position of its
/// name (what rust-analyzer's "go to definition" answers): the name of a
/// `fn`, or of a `let` that binds a closure. `(1-based line, 0-based char
/// column)` of the callable's start, `None` for anything else - a struct, a
/// variable, a field.
pub fn callable_at_definition(text: &str, line: usize, col: usize) -> Option<(u32, u32)> {
    use syn::spanned::Spanned;
    use syn::visit::Visit;
    struct Names {
        line: usize,
        col: usize,
        found: Option<(u32, u32)>,
    }
    impl Names {
        fn at(&self, ident: &proc_macro2::Ident) -> bool {
            let (a, b) = (ident.span().start(), ident.span().end());
            a.line == self.line && a.column <= self.col && self.col <= b.column
        }
        fn sig(&mut self, sig: &syn::Signature) {
            if self.at(&sig.ident) {
                let f = sig.fn_token.span.start();
                self.found = Some((f.line as u32, f.column as u32));
            }
        }
    }
    impl<'ast> Visit<'ast> for Names {
        fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
            self.sig(&f.sig);
            syn::visit::visit_item_fn(self, f);
        }
        fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
            self.sig(&f.sig);
            syn::visit::visit_impl_item_fn(self, f);
        }
        fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
            if f.default.is_some() {
                self.sig(&f.sig);
            }
            syn::visit::visit_trait_item_fn(self, f);
        }
        fn visit_local(&mut self, l: &'ast syn::Local) {
            if let Some(init) = &l.init
                && let syn::Expr::Closure(c) = &*init.expr
                && let Some(ident) = pat_ident(&l.pat)
                && self.at(ident)
            {
                let at = c.span().start();
                self.found = Some((at.line as u32, at.column as u32));
            }
            syn::visit::visit_local(self, l);
        }
    }
    proc_macro2::extra::invalidate_current_thread_spans();
    let file = syn::parse_file(text).ok()?;
    let mut names = Names {
        line,
        col,
        found: None,
    };
    names.visit_file(&file);
    names.found
}

/// Why the name at a definition's position is nothing
/// [`callable_at_definition`] can time - for the refusal to say the truth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotCallable {
    /// A trait's method with no default body: the type decides the impl.
    TraitFnWithoutBody,
    /// A function of an `extern` block: not Rust code of the project.
    ExternFn,
    /// The file does not parse - why.
    Unparsed(String),
    /// A struct, an enum variant, a variable, a parameter, a field.
    Other,
}

/// What the name at `(1-based line, 0-based char column)` is, when
/// [`callable_at_definition`] finds no callable there.
pub fn not_callable(text: &str, file: &str, line: usize, col: usize) -> NotCallable {
    use syn::visit::Visit;
    struct Kinds {
        line: usize,
        col: usize,
        found: NotCallable,
    }
    impl Kinds {
        fn at(&self, ident: &proc_macro2::Ident) -> bool {
            let (a, b) = (ident.span().start(), ident.span().end());
            a.line == self.line && a.column <= self.col && self.col <= b.column
        }
    }
    impl<'ast> Visit<'ast> for Kinds {
        fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
            if f.default.is_none() && self.at(&f.sig.ident) {
                self.found = NotCallable::TraitFnWithoutBody;
            }
            syn::visit::visit_trait_item_fn(self, f);
        }
        fn visit_foreign_item_fn(&mut self, f: &'ast syn::ForeignItemFn) {
            if self.at(&f.sig.ident) {
                self.found = NotCallable::ExternFn;
            }
            syn::visit::visit_foreign_item_fn(self, f);
        }
    }
    if let Some(why) = parse_error(text, file) {
        return NotCallable::Unparsed(why);
    }
    proc_macro2::extra::invalidate_current_thread_spans();
    let Ok(parsed) = syn::parse_file(text) else {
        return NotCallable::Other;
    };
    let mut kinds = Kinds {
        line,
        col,
        found: NotCallable::Other,
    };
    kinds.visit_file(&parsed);
    kinds.found
}

/// The name of the path the caret is on - a call's callee, a function
/// passed by name (`for_each(toggle)`), a variable - also inside a macro's
/// arguments. `None` on anything else.
pub fn path_name_at(text: &str, line: usize, col: usize) -> Option<String> {
    use syn::visit::Visit;
    struct Paths {
        line: usize,
        col: usize,
        found: Option<String>,
    }
    impl<'ast> Visit<'ast> for Paths {
        fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
            if let Some(last) = p.path.segments.last() {
                let (a, b) = (last.ident.span().start(), last.ident.span().end());
                if a.line == self.line && a.column <= self.col && self.col <= b.column {
                    self.found = Some(last.ident.to_string());
                }
            }
            syn::visit::visit_expr_path(self, p);
        }
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            use syn::parse::Parser;
            let args = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
            if let Ok(exprs) = args.parse2(m.tokens.clone()) {
                for e in &exprs {
                    self.visit_expr(e);
                }
            }
        }
    }
    proc_macro2::extra::invalidate_current_thread_spans();
    let file = syn::parse_file(text).ok()?;
    let mut paths = Paths {
        line,
        col,
        found: None,
    };
    paths.visit_file(&file);
    paths.found
}

/// Whether `text` defines a function or a let-bound closure named `name`.
pub fn defines_callable(text: &str, name: &str) -> bool {
    functions(text, "")
        .map(|fns| fns.iter().any(|f| f.path.rsplit("::").next() == Some(name)))
        .unwrap_or(false)
}

/// At most this many regions at once.
pub const MAX_REGIONS: usize = 16;

/// Whether `rel` is a file of the firmware crate itself, where
/// `crate::__roc` - the module appended to `src/main.rs` - resolves.
pub fn instrumentable(rel: &str) -> Result<(), String> {
    if !rel.ends_with(".rs") {
        return Err(format!("{rel} is not Rust code"));
    }
    if !rel.starts_with("src/") {
        return Err(format!(
            concat!(
                "{} is in a library of the workspace: only the firmware's own files ",
                "(src/) can be timed",
            ),
            rel
        ));
    }
    if rel.starts_with("src/bin/") || rel == "src/lib.rs" {
        return Err(format!("{rel} is not part of the firmware binary"));
    }
    Ok(())
}

/// Where a region's guard and drop go, and what the code there is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// `(1-based line, 0-based CHAR column)` of the guard.
    pub start: (usize, usize),
    /// Where the explicit `drop` goes; `None` when the region runs to the end
    /// of its block, where the guard's own `Drop` records it - also on
    /// `return`, `?`, `break`.
    pub end: Option<(usize, usize)>,
    /// The path of the function the region is in, and the line and char
    /// column it starts at.
    pub func: String,
    pub fn_line: u32,
    pub fn_col: u32,
    /// The lines actually covered: whole statements.
    pub lines: (u32, u32),
    /// The region awaits: what it measures is wall time, other tasks
    /// included.
    pub awaits: bool,
    /// A whole function or closure that ends in an endless `loop` (an
    /// embassy task, `fn main() -> !`): the lines of that loop's statements.
    /// Its time never ends, so [`mark`] times one pass of the loop instead.
    pub loop_body: Option<(u32, u32)>,
}

/// The endless `loop` that `stmts` end in - one with no `break` out of it, no
/// `return` and no `?`, also inside a macro's arguments -: `Some(Ok(lines))`
/// of its statements, `Some(Err(why))` when one pass of it cannot be timed
/// (empty, or statements on a line of its braces: they would place as the
/// whole loop), `None` when they end otherwise.
fn endless_loop_body(stmts: &[syn::Stmt]) -> Option<Result<(u32, u32), &'static str>> {
    use syn::spanned::Spanned;
    use syn::visit::Visit;
    struct Exits {
        label: Option<String>,
        /// Loops entered inside it: an unlabeled `break` there leaves them.
        depth: usize,
        found: bool,
    }
    impl<'ast> Visit<'ast> for Exits {
        fn visit_expr_break(&mut self, b: &'ast syn::ExprBreak) {
            match &b.label {
                Some(l) => {
                    self.found |= self.label.as_deref() == Some(l.ident.to_string().as_str())
                }
                None => self.found |= self.depth == 0,
            }
            syn::visit::visit_expr_break(self, b);
        }
        fn visit_expr_return(&mut self, _: &'ast syn::ExprReturn) {
            self.found = true;
        }
        fn visit_expr_try(&mut self, _: &'ast syn::ExprTry) {
            self.found = true;
        }
        fn visit_expr_loop(&mut self, l: &'ast syn::ExprLoop) {
            self.depth += 1;
            syn::visit::visit_expr_loop(self, l);
            self.depth -= 1;
        }
        fn visit_expr_while(&mut self, w: &'ast syn::ExprWhile) {
            self.depth += 1;
            syn::visit::visit_expr_while(self, w);
            self.depth -= 1;
        }
        fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
            self.depth += 1;
            syn::visit::visit_expr_for_loop(self, f);
            self.depth -= 1;
        }
        // Their own `return` / `?` / `break` leave them, not the loop.
        fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
        fn visit_expr_async(&mut self, _: &'ast syn::ExprAsync) {}
        fn visit_item(&mut self, _: &'ast syn::Item) {}
        // `info!("{}", read()?)`: what a macro's arguments do is in its
        // tokens. A `?`, `return` or `break` there may leave the loop - and
        // a format string is one literal, so `{:?}` is not one.
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            fn exits(tokens: proc_macro2::TokenStream) -> bool {
                tokens.into_iter().any(|t| match t {
                    proc_macro2::TokenTree::Ident(i) => i == "return" || i == "break",
                    proc_macro2::TokenTree::Punct(p) => p.as_char() == '?',
                    proc_macro2::TokenTree::Group(g) => exits(g.stream()),
                    proc_macro2::TokenTree::Literal(_) => false,
                })
            }
            self.found |= exits(m.tokens.clone());
        }
    }
    let last = stmts
        .iter()
        .rev()
        .find(|s| !matches!(s, syn::Stmt::Item(_)))?;
    let syn::Stmt::Expr(syn::Expr::Loop(l), _) = last else {
        return None;
    };
    let mut exits = Exits {
        label: l.label.as_ref().map(|l| l.name.ident.to_string()),
        depth: 0,
        found: false,
    };
    exits.visit_block(&l.body);
    if exits.found {
        return None;
    }
    let body: Vec<&syn::Stmt> = l
        .body
        .stmts
        .iter()
        .filter(|s| !matches!(s, syn::Stmt::Item(_)))
        .collect();
    let (Some(first), Some(last)) = (body.first(), body.last()) else {
        return Some(Err("its loop is empty"));
    };
    let (first, last) = (
        first.span().start().line as u32,
        last.span().end().line as u32,
    );
    let (open, close) = (
        l.body.brace_token.span.open().start().line as u32,
        l.body.brace_token.span.close().start().line as u32,
    );
    if first <= open || last >= close {
        return Some(Err(
            "its loop's statements share a line with the loop's braces: put them on lines of their own to time one pass",
        ));
    }
    Some(Ok((first, last)))
}

/// A whole body's placement: one pass of the endless loop it ends in, or why
/// that loop makes it untimeable.
fn whole_body(stmts: &[syn::Stmt], name: &str) -> Result<Option<(u32, u32)>, String> {
    match endless_loop_body(stmts) {
        None => Ok(None),
        Some(Ok(lines)) => Ok(Some(lines)),
        Some(Err(why)) => Err(format!(
            "`{name}` never returns (it ends in an endless loop), and {why}"
        )),
    }
}

/// Where `r` goes in `text`: whole statements of the innermost block holding
/// the chosen lines, or a whole function body.
pub fn place(text: &str, r: &TimingRegion) -> Result<Placement, String> {
    use syn::visit::Visit;
    // Span locations keep every parsed source per thread; nothing returned
    // here holds a span, so they can go.
    proc_macro2::extra::invalidate_current_thread_spans();
    let file = syn::parse_file(text).map_err(|e| {
        format!(
            "{} does not parse (line {}): {e}",
            r.file,
            e.span().start().line.max(1)
        )
    })?;
    // A whole function is the one around `first`, whatever `last` says.
    let (lo, hi) = if r.whole_fn {
        (r.first, r.first)
    } else {
        (r.first.min(r.last), r.first.max(r.last))
    };
    let mut f = Finder::new(lo as usize, hi as usize, r.whole_fn);
    f.col = r.col as usize;
    f.visit_file(&file);
    match f.hit {
        Some(Ok(p)) => Ok(p),
        Some(Err(e)) => Err(e),
        None if r.whole_fn => Err(format!(
            "{}:{} is not inside a function body",
            r.file, r.first
        )),
        None => Err(format!(
            "{}:{}-{}: select whole statements inside a function body",
            r.file, r.first, r.last
        )),
    }
}

struct Finder {
    lo: usize,
    hi: usize,
    whole_fn: bool,
    /// Inside a `const` / `static` initializer or a `const fn`: no guard can
    /// run there.
    const_depth: usize,
    /// The path so far: modules, impls, traits, functions, closures - and
    /// what each one is.
    scope: Vec<String>,
    kinds: Vec<Scope>,
    /// Closures about to be visited that a `let` names: `(address, name)`.
    closure_names: Vec<(usize, String)>,
    /// The functions around the visit: path and signature line.
    fns: Vec<(String, u32, u32)>,
    /// A whole function: the caret's column on line `lo`, which decides
    /// whether a closure of that line holds it.
    col: usize,
    /// Every function body met.
    seen: Vec<FnSeen>,
    /// `const_depth` outside each function entered.
    depths: Vec<usize>,
    /// The innermost match so far (visits are pre-order: deeper wins).
    hit: Option<Result<Placement, String>>,
}

/// The name a `let` pattern binds: `f`, `mut f`, `f: T`.
fn pat_name(p: &syn::Pat) -> Option<String> {
    pat_ident(p).map(ToString::to_string)
}

fn pat_ident(p: &syn::Pat) -> Option<&proc_macro2::Ident> {
    match p {
        syn::Pat::Ident(i) => Some(&i.ident),
        syn::Pat::Type(t) => pat_ident(&t.pat),
        _ => None,
    }
}

/// `#[naked]`, or the 2024 edition's `#[unsafe(naked)]`.
fn is_naked(a: &syn::Attribute) -> bool {
    match &a.meta {
        syn::Meta::Path(p) => p.is_ident("naked"),
        syn::Meta::List(l) => l.path.is_ident("unsafe") && l.tokens.to_string().trim() == "naked",
        syn::Meta::NameValue(_) => false,
    }
}

/// A type as a function's path names it: the last segment of a path with
/// its generic arguments (`From<u8>`), so two impls of one generic trait are
/// two paths.
fn type_name(t: &syn::Type) -> String {
    match t {
        syn::Type::Path(p) => p.path.segments.last().map(segment).unwrap_or_default(),
        syn::Type::Reference(r) => format!(
            "&{}{}",
            if r.mutability.is_some() { "mut " } else { "" },
            type_name(&r.elem)
        ),
        syn::Type::Ptr(p) => format!(
            "*{} {}",
            if p.mutability.is_some() {
                "mut"
            } else {
                "const"
            },
            type_name(&p.elem)
        ),
        syn::Type::Slice(s) => format!("[{}]", type_name(&s.elem)),
        syn::Type::Array(a) => format!("[{}; _]", type_name(&a.elem)),
        syn::Type::Tuple(t) => format!(
            "({})",
            t.elems.iter().map(type_name).collect::<Vec<_>>().join(", ")
        ),
        syn::Type::Paren(p) => type_name(&p.elem),
        syn::Type::Group(g) => type_name(&g.elem),
        _ => "_".to_owned(),
    }
}

/// One path segment with its generic arguments.
fn segment(s: &syn::PathSegment) -> String {
    let syn::PathArguments::AngleBracketed(a) = &s.arguments else {
        return s.ident.to_string();
    };
    let args: Vec<String> = a
        .args
        .iter()
        .map(|g| match g {
            syn::GenericArgument::Type(t) => type_name(t),
            syn::GenericArgument::Lifetime(l) => l.to_string(),
            syn::GenericArgument::AssocType(at) => format!("{}={}", at.ident, type_name(&at.ty)),
            _ => "_".to_owned(),
        })
        .collect();
    format!("{}<{}>", s.ident, args.join(", "))
}

fn pos(lc: proc_macro2::LineColumn) -> (usize, usize) {
    (lc.line, lc.column)
}

/// Whether `stmts` await outside closures and `async` blocks of their own.
fn awaits(stmts: &[syn::Stmt]) -> bool {
    use syn::visit::Visit;
    struct Awaits(bool);
    impl<'ast> Visit<'ast> for Awaits {
        fn visit_expr_await(&mut self, _: &'ast syn::ExprAwait) {
            self.0 = true;
        }
        fn visit_expr_closure(&mut self, _: &'ast syn::ExprClosure) {}
        fn visit_expr_async(&mut self, _: &'ast syn::ExprAsync) {}
        fn visit_item(&mut self, _: &'ast syn::Item) {}
    }
    let mut a = Awaits(false);
    for s in stmts {
        a.visit_stmt(s);
    }
    a.0
}

impl Finder {
    fn new(lo: usize, hi: usize, whole_fn: bool) -> Self {
        Finder {
            lo,
            hi,
            whole_fn,
            const_depth: 0,
            scope: Vec::new(),
            kinds: Vec::new(),
            closure_names: Vec::new(),
            fns: Vec::new(),
            col: 0,
            seen: Vec::new(),
            depths: Vec::new(),
            hit: None,
        }
    }

    fn func(&self) -> (String, u32, u32) {
        self.fns
            .last()
            .cloned()
            .unwrap_or_else(|| ("?".to_owned(), 0, 0))
    }

    /// A function's body, for "time this function".
    fn function(&mut self, sig: &syn::Signature, body: &syn::Block, path: &str, naked: bool) {
        use syn::spanned::Spanned;
        let sig_line = sig.fn_token.span.start().line;
        let close = body.brace_token.span.close().end().line;
        if !self.whole_fn || self.lo < sig_line || self.lo > close {
            return;
        }
        let name = sig.ident.to_string();
        let never = matches!(&sig.output, syn::ReturnType::Type(_, t) if matches!(**t, syn::Type::Never(_)));
        self.hit = Some(if sig.constness.is_some() {
            Err(format!(
                "`{name}` is a const fn: nothing can be timed in it"
            ))
        } else if naked {
            // Its body must be one `naked_asm!` and nothing else (E0787).
            Err(format!(
                "`{name}` is a naked function: nothing can be timed in it"
            ))
        } else if never && endless_loop_body(&body.stmts).is_none() {
            Err(format!(
                concat!(
                    "`{}` never returns (`-> !`), so its time never ends - select the ",
                    "statements of its loop instead",
                ),
                name
            ))
        } else if let Err(why) = whole_body(&body.stmts, &name) {
            Err(why)
        } else {
            // Before the first statement that RUNS, not right after `{`:
            // past the body's inner attributes (`#![allow(..)]`, a guard in
            // front of one does not compile) and its leading items -
            // cortex-m-rt turns the `static mut`s that open an
            // `#[interrupt]` / `#[exception]` body into `&mut` resources only
            // while they come first.
            let start = match body.stmts.iter().find(|s| !matches!(s, syn::Stmt::Item(_))) {
                Some(s) => s.span().start(),
                None => body.brace_token.span.close().start(),
            };
            Ok(Placement {
                start: pos(start),
                end: None,
                func: path.to_owned(),
                fn_line: sig_line as u32,
                fn_col: sig.fn_token.span.start().column as u32,
                lines: (sig_line as u32, close as u32),
                awaits: sig.asyncness.is_some() || awaits(&body.stmts),
                loop_body: whole_body(&body.stmts, &name).ok().flatten(),
            })
        });
    }

    /// A closure's body, for "time this function": like a fn's, when it has
    /// braces - `async |..| { .. }`, `|..| async move { .. }` too.
    fn closure(
        &self,
        c: &syn::ExprClosure,
        path: &str,
        name: &str,
        start: usize,
        start_col: usize,
        close: usize,
    ) -> Result<Placement, String> {
        use syn::spanned::Spanned;
        if self.const_depth > 0 {
            return Err(format!(
                "`{name}` is in a const context: nothing can be timed in it"
            ));
        }
        let (block, async_block) = match &*c.body {
            syn::Expr::Block(b) => (&b.block, false),
            syn::Expr::Async(a) => (&a.block, true),
            _ => {
                return Err(format!(
                    concat!(
                        "the closure `{}` has no braces around its body: write it as ",
                        "`|..| {{ .. }}` to time it",
                    ),
                    name
                ));
            }
        };
        let loop_body = whole_body(&block.stmts, name)?;
        let at = match block
            .stmts
            .iter()
            .find(|s| !matches!(s, syn::Stmt::Item(_)))
        {
            Some(s) => s.span().start(),
            None => block.brace_token.span.close().start(),
        };
        Ok(Placement {
            start: pos(at),
            end: None,
            func: path.to_owned(),
            fn_line: start as u32,
            fn_col: start_col as u32,
            lines: (start as u32, close as u32),
            awaits: c.asyncness.is_some() || async_block || awaits(&block.stmts),
            loop_body,
        })
    }

    fn enter(&mut self, sig: &syn::Signature, body: &syn::Block, attrs: &[syn::Attribute]) {
        let method = matches!(self.kinds.last(), Some(Scope::Impl | Scope::Trait));
        self.scope.push(sig.ident.to_string());
        self.kinds.push(Scope::Fn);
        let path = self.scope.join("::");
        let sig_line = sig.fn_token.span.start().line as u32;
        self.seen.push(FnSeen {
            path: path.clone(),
            sig: sig_line,
            col: sig.fn_token.span.start().column as u32,
            close: body.brace_token.span.close().end().line as u32,
            kind: if method {
                Callable::Method
            } else {
                Callable::Fn
            },
        });
        // A fn item is runtime code even inside a `const` block (an impl in
        // `const _: () = { .. }`); only a `const fn` body is const.
        self.depths.push(self.const_depth);
        // A naked fn's body is as closed to a guard as a const one.
        let naked = attrs.iter().any(is_naked);
        self.const_depth = if sig.constness.is_some() || naked {
            self.const_depth + 1
        } else {
            0
        };
        self.function(sig, body, &path, naked);
        self.fns
            .push((path, sig_line, sig.fn_token.span.start().column as u32));
    }

    fn leave(&mut self, _sig: &syn::Signature) {
        self.fns.pop();
        self.scope.pop();
        self.kinds.pop();
        self.const_depth = self.depths.pop().unwrap_or(0);
    }
}

impl<'ast> syn::visit::Visit<'ast> for Finder {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        self.scope.push(m.ident.to_string());
        self.kinds.push(Scope::Mod);
        syn::visit::visit_item_mod(self, m);
        self.scope.pop();
        self.kinds.pop();
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let ty = type_name(&i.self_ty);
        self.scope.push(match &i.trait_ {
            Some((_, path, _)) => format!(
                "<{ty} as {}>",
                path.segments.last().map(segment).unwrap_or_default()
            ),
            None => ty,
        });
        self.kinds.push(Scope::Impl);
        syn::visit::visit_item_impl(self, i);
        self.scope.pop();
        self.kinds.pop();
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        self.scope.push(t.ident.to_string());
        self.kinds.push(Scope::Trait);
        syn::visit::visit_item_trait(self, t);
        self.scope.pop();
        self.kinds.pop();
    }

    fn visit_local(&mut self, l: &'ast syn::Local) {
        if let Some(init) = &l.init
            && let syn::Expr::Closure(c) = &*init.expr
            && let Some(name) = pat_name(&l.pat)
        {
            self.closure_names
                .push((c as *const syn::ExprClosure as usize, name));
        }
        syn::visit::visit_local(self, l);
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        use syn::spanned::Spanned;
        let key = c as *const syn::ExprClosure as usize;
        let name = match self.closure_names.iter().rposition(|(k, _)| *k == key) {
            Some(i) => self.closure_names.remove(i).1,
            None => "{closure}".to_owned(),
        };
        let (from, to) = (c.span().start(), c.body.span().end());
        let (start, close) = (from.line, to.line);
        self.scope.push(name.clone());
        self.kinds.push(Scope::Closure);
        let path = self.scope.join("::");
        self.seen.push(FnSeen {
            path: path.clone(),
            sig: start as u32,
            col: from.column as u32,
            close: close as u32,
            kind: Callable::Closure,
        });
        // The caret INSIDE the closure, not just on one of its lines: a
        // `.filter(|b| ..)` on the caret's line must not take a "time this
        // function" meant for the function around it.
        let at = (self.lo, self.col);
        if self.whole_fn && (from.line, from.column) <= at && at <= (to.line, to.column) {
            self.hit = Some(self.closure(c, &path, &name, start, from.column, close));
        }
        self.fns.push((path, start as u32, from.column as u32));
        syn::visit::visit_expr_closure(self, c);
        self.fns.pop();
        self.scope.pop();
        self.kinds.pop();
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.enter(&f.sig, &f.block, &f.attrs);
        syn::visit::visit_item_fn(self, f);
        self.leave(&f.sig);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.enter(&f.sig, &f.block, &f.attrs);
        syn::visit::visit_impl_item_fn(self, f);
        self.leave(&f.sig);
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        if let Some(body) = &f.default {
            self.enter(&f.sig, body, &f.attrs);
            syn::visit::visit_trait_item_fn(self, f);
            self.leave(&f.sig);
        }
    }

    fn visit_item_const(&mut self, i: &'ast syn::ItemConst) {
        self.const_depth += 1;
        syn::visit::visit_item_const(self, i);
        self.const_depth -= 1;
    }

    fn visit_item_static(&mut self, i: &'ast syn::ItemStatic) {
        self.const_depth += 1;
        syn::visit::visit_item_static(self, i);
        self.const_depth -= 1;
    }

    fn visit_expr_const(&mut self, e: &'ast syn::ExprConst) {
        self.const_depth += 1;
        syn::visit::visit_expr_const(self, e);
        self.const_depth -= 1;
    }

    fn visit_trait_item_const(&mut self, i: &'ast syn::TraitItemConst) {
        self.const_depth += 1;
        syn::visit::visit_trait_item_const(self, i);
        self.const_depth -= 1;
    }

    // An enum's discriminants, an array's length, a const generic argument:
    // const contexts too.
    fn visit_variant(&mut self, v: &'ast syn::Variant) {
        self.const_depth += 1;
        syn::visit::visit_variant(self, v);
        self.const_depth -= 1;
    }

    fn visit_expr_repeat(&mut self, e: &'ast syn::ExprRepeat) {
        self.visit_expr(&e.expr);
        self.const_depth += 1;
        self.visit_expr(&e.len);
        self.const_depth -= 1;
    }

    fn visit_type_array(&mut self, t: &'ast syn::TypeArray) {
        self.const_depth += 1;
        syn::visit::visit_type_array(self, t);
        self.const_depth -= 1;
    }

    fn visit_generic_argument(&mut self, g: &'ast syn::GenericArgument) {
        let constant = matches!(g, syn::GenericArgument::Const(_));
        self.const_depth += usize::from(constant);
        syn::visit::visit_generic_argument(self, g);
        self.const_depth -= usize::from(constant);
    }

    fn visit_impl_item_const(&mut self, i: &'ast syn::ImplItemConst) {
        self.const_depth += 1;
        syn::visit::visit_impl_item_const(self, i);
        self.const_depth -= 1;
    }

    fn visit_block(&mut self, b: &'ast syn::Block) {
        use syn::spanned::Spanned;
        let open = b.brace_token.span.open().start().line;
        let close = b.brace_token.span.close().end().line;
        // STRICTLY inside the braces: lines that start an `if … {` select the
        // whole `if` in the block around it.
        if !self.whole_fn
            && self.const_depth == 0
            && !self.fns.is_empty()
            && open < self.lo
            && self.hi < close
        {
            let is_item = |s: &syn::Stmt| matches!(s, syn::Stmt::Item(_));
            let first = b
                .stmts
                .iter()
                .position(|s| !is_item(s) && s.span().end().line >= self.lo);
            let last = b
                .stmts
                .iter()
                .rposition(|s| !is_item(s) && s.span().start().line <= self.hi);
            if let (Some(first), Some(last)) = (first, last)
                && first <= last
            {
                let s0 = b.stmts[first].span().start();
                let s1 = b.stmts[last].span().end();
                let (func, fn_line, fn_col) = self.func();
                self.hit = Some(Ok(Placement {
                    start: pos(s0),
                    // The block's last statement - a tail expression or not -
                    // ends where the block does: the guard's Drop records
                    // there, and a tail stays a tail.
                    end: (last + 1 < b.stmts.len()).then(|| pos(s1)),
                    func,
                    fn_line,
                    fn_col,
                    lines: (s0.line as u32, s1.line as u32),
                    awaits: awaits(&b.stmts[first..=last]),
                    loop_body: None,
                }));
            }
        }
        syn::visit::visit_block(self, b);
    }
}

/// The guard that opens region `id`.
fn guard(id: usize) -> String {
    format!("let __roc_g{id} = crate::__roc::Span::start({id}); ")
}

/// The statement that closes region `id` early.
fn closer(id: usize) -> String {
    format!(" core::mem::drop(__roc_g{id});")
}

/// Byte offset of `(1-based line, char column)` in `text`.
fn offset(text: &str, (line, col): (usize, usize)) -> Option<usize> {
    // `syn::parse_file` drops a leading byte-order mark before the spans are
    // counted, so line 1's columns start after it.
    let col = if line == 1 && text.starts_with('\u{feff}') {
        col + 1
    } else {
        col
    };
    let mut start = 0;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            return match l.char_indices().nth(col) {
                Some((b, _)) => Some(start + b),
                None => (l.chars().count() == col).then_some(start + l.len()),
            };
        }
        start += l.len();
    }
    None
}

/// `text` with the guards and drops of `regions` (`(id, placement)`) in it.
/// Same lines: everything goes in at token columns, never at a line's end
/// (where a `//` comment would swallow it).
pub fn instrument(text: &str, regions: &[(usize, Placement)]) -> Result<String, String> {
    // (offset, order, text). At one offset drops go first (a region that
    // ends where the next begins), the inner region's first; then guards,
    // the outer region's first: the one that ends later - a region that runs
    // to its block's end (no drop) latest of all - and of two that end
    // together, the one covering more lines (a whole function).
    let mut edits: Vec<(usize, (u8, i64, i64), String)> = Vec::new();
    for (id, p) in regions {
        let s = offset(text, p.start).ok_or("a region's start is outside the file")?;
        let e = match p.end {
            Some(end) => Some(offset(text, end).ok_or("a region's end is outside the file")?),
            None => None,
        };
        let reach = e.map_or(i64::MIN, |e| -(e as i64));
        let span = -i64::from(p.lines.1.saturating_sub(p.lines.0));
        edits.push((s, (1, reach, span), guard(*id)));
        if let Some(e) = e {
            edits.push((e, (0, -(s as i64), 0), closer(*id)));
        }
    }
    edits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut out = text.to_owned();
    // Back to front, so earlier offsets stay valid; at one offset the texts
    // are joined in order first.
    let mut i = edits.len();
    while i > 0 {
        let at = edits[i - 1].0;
        let mut j = i;
        while j > 0 && edits[j - 1].0 == at {
            j -= 1;
        }
        let joined: String = edits[j..i].iter().map(|e| e.2.as_str()).collect();
        out.insert_str(at, &joined);
        i = j;
    }
    Ok(out)
}

// ── The generated module (pure) ─────────────────────────────────────────────

/// The cycle counter a target has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Counter {
    /// ESP32-C2/C3/C6/H2: custom CSR 0x7e2 (PCCR), enabled by 0x7e0/0x7e1.
    EspPccr,
    /// ESP32-C5/C61: the standard `mcycle`.
    Mcycle,
    /// Cortex-M3/M4/M7/M33: DWT CYCCNT. `unlock`: write the ARMv7-M lock
    /// key first (an M7 ignores DWT writes without it); ARMv8-M has no lock.
    Dwt { unlock: bool },
}

impl Counter {
    /// What the numbers are counted by, for the table's header.
    pub fn name(self) -> &'static str {
        match self {
            Counter::EspPccr => "cycle counter (CSR 0x7E2)",
            Counter::Mcycle => "cycle counter (mcycle)",
            Counter::Dwt { .. } => "cycle counter (DWT CYCCNT)",
        }
    }

    /// The tag the table carries, so the host knows what it reads.
    fn tag(self) -> u32 {
        match self {
            Counter::EspPccr => 1,
            Counter::Mcycle => 2,
            Counter::Dwt { .. } => 3,
        }
    }
}

/// The counter of a target, or why there is none to use.
pub fn counter_for(target: &str, chip: &str) -> Result<Counter, String> {
    let chip = chip.to_ascii_lowercase();
    if target.starts_with("riscv") {
        // esp-hal reads PCCR wherever esp-metadata says `cpu_has_csr_pc`
        // (C2/C3/C6/H2), and `mcycle` elsewhere (C5/C61).
        let pccr = chip.starts_with("esp32")
            && !chip.starts_with("esp32c5")
            && !chip.starts_with("esp32c61");
        return Ok(if pccr {
            Counter::EspPccr
        } else {
            Counter::Mcycle
        });
    }
    if target.starts_with("thumbv7m") || target.starts_with("thumbv7em") {
        return Ok(Counter::Dwt { unlock: true });
    }
    if target.starts_with("thumbv8m.main") {
        return Ok(Counter::Dwt { unlock: false });
    }
    if target.starts_with("thumbv6m") || target.starts_with("thumbv8m.base") {
        return Err(concat!(
            "Cortex-M0/M0+ has no cycle counter (no DWT CYCCNT): timing is not supported ",
            "on this chip yet.",
        )
        .to_owned());
    }
    Err(format!("timing is not supported for the target {target}"))
}

/// The module appended to the build copy's `main.rs`: the table, the guard,
/// and the counter - core only, no dependency, edition 2021 and 2024 alike.
pub fn module_source(counter: Counter, regions: usize) -> String {
    let (now, setup) = match counter {
        Counter::EspPccr => (
            r#"let c: u32;
        unsafe { core::arch::asm!("csrr {0}, 0x7e2", out(reg) c, options(nomem, nostack, preserves_flags)) };
        c"#,
            // esp-hal's RNG already relies on PCCR counting; it is set up only
            // when it does not move, and what was there is kept for the host.
            r#"let (pcer, pcmr): (u32, u32);
        unsafe {
            core::arch::asm!("csrr {0}, 0x7e0", out(reg) pcer, options(nomem, nostack, preserves_flags));
            core::arch::asm!("csrr {0}, 0x7e1", out(reg) pcmr, options(nomem, nostack, preserves_flags));
            write_volatile(addr_of_mut!((*t()).aux0), pcer);
            write_volatile(addr_of_mut!((*t()).aux1), pcmr);
        }
        let a = now();
        let b = now();
        if a == b {
            unsafe {
                core::arch::asm!("csrw 0x7e0, {0}", in(reg) 1u32, options(nomem, nostack, preserves_flags));
                core::arch::asm!("csrw 0x7e1, {0}", in(reg) 1u32, options(nomem, nostack, preserves_flags));
            }
        }"#,
        ),
        Counter::Mcycle => (
            r#"let c: u32;
        unsafe { core::arch::asm!("csrr {0}, mcycle", out(reg) c, options(nomem, nostack, preserves_flags)) };
        c"#,
            "",
        ),
        Counter::Dwt { unlock } => (
            "unsafe { read_volatile(0xE000_1004 as *const u32) }",
            // DEMCR.TRCENA, the lock key (an M7 ignores DWT writes without it
            // after a power cycle), then DWT_CTRL.CYCCNTENA.
            if unlock {
                r#"unsafe {
            let demcr = 0xE000_EDFC as *mut u32;
            write_volatile(demcr, read_volatile(demcr) | (1 << 24));
            write_volatile(0xE000_1FB0 as *mut u32, 0xC5AC_CE55);
            let ctrl = 0xE000_1000 as *mut u32;
            write_volatile(ctrl, read_volatile(ctrl) | 1);
        }"#
            } else {
                r#"unsafe {
            let demcr = 0xE000_EDFC as *mut u32;
            write_volatile(demcr, read_volatile(demcr) | (1 << 24));
            let ctrl = 0xE000_1000 as *mut u32;
            write_volatile(ctrl, read_volatile(ctrl) | 1);
        }"#
            },
        ),
    };
    let irq = match counter {
        Counter::EspPccr | Counter::Mcycle => {
            r#"#[inline(always)]
    fn irq_off() -> usize {
        let m: usize;
        unsafe { core::arch::asm!("csrrci {0}, mstatus, 8", out(reg) m, options(nostack)) };
        compiler_fence(Ordering::SeqCst);
        m
    }
    #[inline(always)]
    fn irq_restore(m: usize) {
        compiler_fence(Ordering::SeqCst);
        if m & 8 != 0 {
            unsafe { core::arch::asm!("csrsi mstatus, 8", options(nostack)) };
        }
    }"#
        }
        Counter::Dwt { .. } => {
            r#"#[inline(always)]
    fn irq_off() -> usize {
        let m: u32;
        unsafe {
            core::arch::asm!("mrs {0}, PRIMASK", out(reg) m, options(nomem, nostack, preserves_flags));
            core::arch::asm!("cpsid i", options(nomem, nostack, preserves_flags));
        }
        compiler_fence(Ordering::SeqCst);
        m as usize
    }
    #[inline(always)]
    fn irq_restore(m: usize) {
        compiler_fence(Ordering::SeqCst);
        if m & 1 == 0 {
            unsafe { core::arch::asm!("cpsie i", options(nomem, nostack, preserves_flags)) };
        }
    }"#
        }
    };
    format!(
        r#"
// -- RustOnChip "Measure time": in the build copy only, never in your files --
#[allow(dead_code, unused, unsafe_code, non_upper_case_globals, non_snake_case, clippy::all)]
mod __roc {{
    use core::ptr::{{addr_of, addr_of_mut, read_volatile, write_volatile}};
    use core::sync::atomic::{{compiler_fence, fence, Ordering}};

    pub const N: usize = {n};

    #[repr(C)]
    pub struct Stat {{
        seq: u32,
        count: u32,
        min: u32,
        max: u32,
        last: u32,
        total_lo: u32,
        total_hi: u32,
        seq_end: u32,
    }}

    #[repr(C)]
    pub struct Table {{
        magic: u32,
        n: u32,
        counter: u32,
        started: u32,
        aux0: u32,
        aux1: u32,
        _pad: [u32; 2],
        stats: [Stat; N],
    }}

    const EMPTY: Stat = Stat {{ seq: 0, count: 0, min: u32::MAX, max: 0, last: 0, total_lo: 0, total_hi: 0, seq_end: 0 }};

    #[used]
    #[unsafe(no_mangle)]
    pub static mut __ROC_TIMING: Table = Table {{
        magic: 0x524F_4354,
        n: N as u32,
        counter: {tag},
        started: 0,
        aux0: 0,
        aux1: 0,
        _pad: [0; 2],
        stats: [EMPTY; N],
    }};

    #[inline(always)]
    fn t() -> *mut Table {{
        addr_of_mut!(__ROC_TIMING)
    }}

    #[inline(always)]
    fn now() -> u32 {{
        {now}
    }}

    {irq}

    #[inline(always)]
    fn setup() {{
        unsafe {{
            if read_volatile(addr_of!((*t()).started)) != 0 {{
                return;
            }}
        }}
        {setup}
        unsafe {{ write_volatile(addr_of_mut!((*t()).started), 1) }};
    }}

    pub struct Span {{
        id: u16,
        t0: u32,
    }}

    impl Span {{
        #[inline(always)]
        pub fn start(id: u16) -> Self {{
            setup();
            let t0 = now();
            Span {{ id, t0 }}
        }}
    }}

    impl Drop for Span {{
        #[inline(always)]
        fn drop(&mut self) {{
            let dt = now().wrapping_sub(self.t0);
            let id = self.id as usize;
            if id >= N {{
                return;
            }}
            let m = irq_off();
            unsafe {{
                let s = addr_of_mut!((*t()).stats[id]);
                // The probe reads the record front to back in several
                // transactions: the new number goes into the LAST word first
                // and the first word last, so a read that overlaps this
                // update finds the two different and is dropped.
                let seq = read_volatile(addr_of!((*s).seq)).wrapping_add(1);
                write_volatile(addr_of_mut!((*s).seq_end), seq);
                fence(Ordering::SeqCst);
                write_volatile(addr_of_mut!((*s).count), read_volatile(addr_of!((*s).count)).wrapping_add(1));
                if dt < read_volatile(addr_of!((*s).min)) {{
                    write_volatile(addr_of_mut!((*s).min), dt);
                }}
                if dt > read_volatile(addr_of!((*s).max)) {{
                    write_volatile(addr_of_mut!((*s).max), dt);
                }}
                write_volatile(addr_of_mut!((*s).last), dt);
                let lo = read_volatile(addr_of!((*s).total_lo));
                let (sum, carry) = lo.overflowing_add(dt);
                write_volatile(addr_of_mut!((*s).total_lo), sum);
                if carry {{
                    write_volatile(addr_of_mut!((*s).total_hi), read_volatile(addr_of!((*s).total_hi)).wrapping_add(1));
                }}
                fence(Ordering::SeqCst);
                write_volatile(addr_of_mut!((*s).seq), seq);
            }}
            irq_restore(m);
        }}
    }}
}}
"#,
        n = regions.max(1),
        tag = counter.tag(),
        now = now,
        irq = irq,
        setup = setup,
    )
}

/// The magic word that starts the table: "ROCT".
const MAGIC: u32 = 0x524F_4354;
/// Table header and record sizes, in bytes.
const HEADER: usize = 32;
const RECORD: usize = 32;

/// One region's numbers, in counter ticks (CPU cycles).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegionStat {
    pub count: u32,
    pub min: u32,
    pub max: u32,
    pub last: u32,
    pub total: u64,
}

impl RegionStat {
    pub fn avg(&self) -> Option<f64> {
        (self.count > 0).then(|| self.total as f64 / f64::from(self.count))
    }
}

/// What a read of the table says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRead {
    /// Whether the firmware has started its counter (a region ran).
    pub started: bool,
    /// Per region; `None` for a record caught mid-update, or cut short.
    pub stats: Vec<Option<RegionStat>>,
}

/// Decode the table's bytes.
pub fn parse_table(bytes: &[u8], regions: usize) -> Result<TableRead, String> {
    let word = |i: usize| {
        bytes
            .get(i * 4..i * 4 + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if word(0) != Some(MAGIC) {
        return Err(
            "the timing table is not in the target's memory (another firmware is running?)"
                .to_owned(),
        );
    }
    let started = word(3).unwrap_or(0) != 0;
    let mut stats = Vec::with_capacity(regions);
    for r in 0..regions {
        let base = (HEADER + r * RECORD) / 4;
        let words: Option<Vec<u32>> = (0..RECORD / 4).map(|k| word(base + k)).collect();
        // `seq` is read first and `seq_end` last: equal means no update ran
        // in between (see the module's `Drop`).
        stats.push(words.filter(|w| w[0] == w[7]).map(|w| RegionStat {
            count: w[1],
            min: w[2],
            max: w[3],
            last: w[4],
            total: u64::from(w[5]) | (u64::from(w[6]) << 32),
        }));
    }
    Ok(TableRead { started, stats })
}

/// The table's size for `regions` regions.
pub fn table_size(regions: usize) -> usize {
    HEADER + RECORD * regions.max(1)
}

// ── The session ─────────────────────────────────────────────────────────────

/// What a region is, as the run measured it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionInfo {
    /// The region as marked: what the table matches its rows on.
    pub region: TimingRegion,
    /// The lines the build covered.
    pub lines: (u32, u32),
    pub func: String,
    pub awaits: bool,
    /// The first line's text, trimmed.
    pub code: String,
}

/// A run's numbers so far.
#[derive(Clone, Debug)]
pub struct TimingRun {
    pub regions: Vec<RegionInfo>,
    pub stats: Vec<RegionStat>,
    /// Counter ticks per second (the CPU clock), when known.
    pub hz: Option<u64>,
    pub counter: Counter,
    /// Successful table reads.
    pub reads: usize,
    /// probe-rs refused a read while the core ran, so every read halts it
    /// for a moment - a region caught in one takes that moment longer.
    pub halting: bool,
    /// Reads that halted the core: a region ran while the probe read its
    /// record, so that poll read the table again with the core stopped.
    pub halted_reads: usize,
    /// The firmware has started its counter: some region ran.
    pub started: bool,
    /// Why the reads stopped before Stop was pressed (the board reset, the
    /// probe went away): the numbers so far stay on screen with it.
    pub ended: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub enum TimingState {
    #[default]
    Idle,
    /// `cargo build --release` of the instrumented copy.
    Building,
    /// probe-rs flashing and starting it.
    Flashing(String),
    /// Running; the table is read once a second.
    Running(TimingRun),
    /// Stopped; the last numbers stay.
    Stopped(TimingRun),
    Failed(String),
}

impl TimingState {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            TimingState::Building | TimingState::Flashing(_) | TimingState::Running(_)
        )
    }
}

/// Everything a session needs.
pub struct TimingJob {
    pub project_dir: PathBuf,
    pub target: String,
    pub chip: String,
    pub probe: Option<String>,
    pub toolchain: crate::panels::mcu_module::ToolchainKind,
    pub regions: Vec<RegionInfo>,
    pub hz: Option<u64>,
    pub counter: Counter,
    /// Each instrumented file and the regions timed in it.
    pub guards: Vec<(String, Vec<usize>)>,
}

/// The Timing mode's session: the regions marked, the run, and the
/// dap-server it holds the probe through.
#[derive(Default)]
pub struct Timing {
    pub regions: Vec<TimingRegion>,
    pub state: Arc<Mutex<TimingState>>,
    stop: Arc<AtomicBool>,
    /// The running `probe-rs dap-server`, so the IDE's exit can end it.
    server: Arc<Mutex<Option<Child>>>,
    /// The build copy holds instrumented files until their build is over.
    pub restore: bool,
    /// Bumped when the project changes: a session started before publishes
    /// nothing when it ends.
    generation: Arc<std::sync::atomic::AtomicU64>,
}

impl Timing {
    pub fn is_busy(&self) -> bool {
        self.state.lock().unwrap().is_busy()
    }

    /// Whether the build copy no longer needs its instrumented files.
    pub fn build_over(&self) -> bool {
        !matches!(*self.state.lock().unwrap(), TimingState::Building)
    }

    /// Build the instrumented copy already written to `job.project_dir`,
    /// flash and run it, and read its table until [`Timing::stop`].
    pub fn start(&self, job: TimingJob, ctx: eframe::egui::Context) {
        if self.is_busy() {
            return;
        }
        self.stop.store(false, Ordering::SeqCst);
        *self.state.lock().unwrap() = TimingState::Building;
        ctx.request_repaint();
        let (state, stop, server, generation) = (
            Arc::clone(&self.state),
            Arc::clone(&self.stop),
            Arc::clone(&self.server),
            Arc::clone(&self.generation),
        );
        let mine = generation.load(Ordering::SeqCst);
        thread::spawn(move || {
            // The state stays busy until probe-rs is gone: published earlier,
            // a Measure in between would start a session this thread then
            // overwrites - and the build copy would get its clean files back
            // while the new build reads them.
            let end = run(&job, &state, &stop, &server, &ctx);
            *state.lock().unwrap() = match end {
                // The project it measured is gone.
                _ if generation.load(Ordering::SeqCst) != mine => TimingState::Idle,
                Ok(Some(run)) => TimingState::Stopped(run),
                // Stopped before anything ran.
                Ok(None) => TimingState::Idle,
                Err(e) => TimingState::Failed(e),
            };
            ctx.request_repaint();
        });
    }

    /// Stop reading; the session lets go of the probe and ends.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Another project is open: its regions are line numbers in other files,
    /// and the last run measured other code. A session still going stops and
    /// shows nothing when it ends.
    pub fn forget(&mut self) {
        self.stop();
        self.regions.clear();
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut st = self.state.lock().unwrap();
        if !st.is_busy() {
            *st = TimingState::Idle;
        }
    }

    /// The IDE is closing: end the session now. A disconnect first - a
    /// killed dap-server leaves an ESP32-C3's debug module set up, and every
    /// attach after it fails until a power cycle - then a kill.
    pub fn kill_now(&self) {
        self.stop();
        let deadline = Instant::now() + Duration::from_millis(1500);
        while self.is_busy()
            && !matches!(*self.state.lock().unwrap(), TimingState::Building)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(50));
        }
        if let Some(mut child) = self.server.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Each file of `texts` the regions are in, instrumented, and `src/main.rs`
/// with the module appended - the files a build copy gets instead of the
/// user's - plus what each region covers.
pub fn instrumented_copies(
    texts: &HashMap<String, String>,
    regions: &[TimingRegion],
    counter: Counter,
) -> Result<(HashMap<String, String>, Vec<RegionInfo>), String> {
    let mut by_file: HashMap<&str, Vec<(usize, Placement)>> = HashMap::new();
    let mut infos = Vec::with_capacity(regions.len());
    for (id, r) in regions.iter().enumerate() {
        instrumentable(&r.file)?;
        let text = texts
            .get(&r.file)
            .ok_or_else(|| format!("{} is not in the project any more", r.file))?;
        let (moved, p) = resolve(text, r).map_err(|e| format!("Row {}: {e}", id + 1))?;
        infos.push(RegionInfo {
            // Where it is now: the table and the next Measure start here.
            region: anchored(text, moved, &p),
            lines: p.lines,
            func: p.func.clone(),
            awaits: p.awaits,
            code: code_of(texts, &r.file, p.lines.0),
        });
        by_file.entry(r.file.as_str()).or_default().push((id, p));
    }
    // Two rows on the same code would share one record's numbers.
    for (i, a) in infos.iter().enumerate() {
        if let Some(j) = infos[..i].iter().position(|b| {
            b.region.file == a.region.file
                && b.region.whole_fn == a.region.whole_fn
                && b.lines == a.lines
                && b.region.func == a.region.func
                && b.region.fn_copy == a.region.fn_copy
                && b.region.col == a.region.col
        }) {
            return Err(format!(
                "Rows {} and {} time the same code - remove one of them",
                j + 1,
                i + 1
            ));
        }
    }
    let mut out = HashMap::new();
    for (file, placed) in by_file {
        out.insert(file.to_owned(), instrument(&texts[file], &placed)?);
    }
    let main = "src/main.rs";
    let base = match out.remove(main) {
        Some(t) => t,
        None => texts
            .get(main)
            .cloned()
            .ok_or("the project has no src/main.rs")?,
    };
    out.insert(
        main.to_owned(),
        base + &module_source(counter, regions.len()),
    );
    Ok((out, infos))
}

/// The first instrumented file of the build copy that lost its timers - or
/// the module, for `src/main.rs`.
fn rewritten(job: &TimingJob) -> Option<String> {
    let read = |rel: &str| std::fs::read_to_string(job.project_dir.join(rel)).unwrap_or_default();
    if !read("src/main.rs").contains("mod __roc {") {
        return Some("src/main.rs".to_owned());
    }
    job.guards
        .iter()
        .find(|(rel, ids)| {
            let text = read(rel);
            ids.iter()
                .any(|id| !text.contains(&format!("crate::__roc::Span::start({id})")))
        })
        .map(|(rel, _)| rel.clone())
}

/// The address and size of `__ROC_TIMING` in the ELF.
fn table_symbol(elf: &Path) -> Result<(u64, u64), String> {
    use object::{Object, ObjectSymbol};
    let data = std::fs::read(elf).map_err(|e| format!("could not read {}: {e}", elf.display()))?;
    let obj = object::File::parse(&*data).map_err(|e| e.to_string())?;
    obj.symbols()
        .find(|s| s.name() == Ok("__ROC_TIMING"))
        .map(|s| (s.address(), s.size()))
        .ok_or_else(|| {
            concat!(
                "the built firmware has no timing table - was the build copy rewritten while ",
                "it built? Measure again.",
            )
            .to_owned()
        })
}

/// The session from build to the last read: its numbers, or `None` when it
/// was stopped before the board ran anything.
fn run(
    job: &TimingJob,
    state: &Arc<Mutex<TimingState>>,
    stop: &Arc<AtomicBool>,
    server_slot: &Arc<Mutex<Option<Child>>>,
    ctx: &eframe::egui::Context,
) -> Result<Option<TimingRun>, String> {
    // The probe first: dap-server never chooses between two by itself.
    let probe = match crate::probe::selector(job.probe.as_deref()) {
        Some(sel) => sel,
        None => crate::probe::pick_probe(&crate::probe::list_probes()?, &job.toolchain)?,
    };
    if stop.load(Ordering::SeqCst) {
        return Ok(None);
    }
    // Stop ends the release build too (the whole tree: cargo's rustc too).
    let built = match crate::flamegraph::build_elf_until(&job.project_dir, &job.target, Some(stop))
    {
        Ok(None) => return Ok(None),
        Ok(Some(elf)) => Ok(elf),
        Err(e) => Err(e),
    };
    if stop.load(Ordering::SeqCst) {
        // Stopped while building: nothing flashed, whatever the build said.
        return Ok(None);
    }
    // The build may have read a file the workspace got back in its clean
    // form meanwhile (a Save, a project switch): a region with no timer
    // stays at 0, a main.rs without the module fails the build.
    if let Some(rel) = rewritten(job) {
        return Err(format!(
            concat!(
                "the build copy of {} was rewritten while it built (a Save, or another ",
                "build), so the timers did not all make it in - Measure again",
            ),
            rel
        ));
    }
    let elf = built.map_err(|e| {
        // A region in a file outside main.rs's module tree cannot name the
        // module appended to main.rs.
        let outside = job.guards.iter().any(|(rel, _)| rel != "src/main.rs");
        if outside && e.contains("could not find `__roc`") {
            format!(
                concat!(
                    "{}\n\nA region is in a file the firmware's main.rs does not include as a ",
                    "module: remove it from the table.",
                ),
                e
            )
        } else {
            e
        }
    })?;
    let (addr, size) = table_symbol(&elf)?;
    let want = table_size(job.regions.len()) as u64;
    if size != 0 && size < want {
        return Err(format!("the timing table is {size} bytes, {want} expected"));
    }
    if stop.load(Ordering::SeqCst) {
        return Ok(None);
    }
    *state.lock().unwrap() = TimingState::Flashing("starting probe-rs".to_owned());
    ctx.request_repaint();

    let port = crate::flamegraph::free_port();
    let mut server = crate::build::no_window(&mut Command::new("probe-rs"))
        .current_dir(&job.project_dir)
        .args([
            "dap-server",
            "--single-session",
            "--port",
            &port.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not launch probe-rs dap-server: {e}"))?;
    let log = Arc::new(Mutex::new(String::new()));
    let drain = server.stderr.take().map(|mut se| {
        let log = log.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = se.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut l = log.lock().unwrap();
                l.push_str(&String::from_utf8_lossy(&buf[..n]));
                if l.len() > 8192 {
                    // On a char boundary: a split one panics, with the lock
                    // held.
                    let mut cut = l.len() - 8192;
                    while !l.is_char_boundary(cut) {
                        cut += 1;
                    }
                    *l = l.split_off(cut);
                }
            }
        })
    });
    // Where the IDE's exit can reach it (`Timing::kill_now`).
    *server_slot.lock().unwrap() = Some(server);
    let console = Arc::new(Mutex::new(Vec::<String>::new()));
    let result = session(job, &elf, &probe, port, addr, state, stop, &console, ctx);
    let taken = server_slot.lock().unwrap().take();
    if let Some(mut server) = taken {
        wait_or_kill(&mut server, Duration::from_secs(3));
    }
    if let Some(h) = drain {
        let _ = h.join();
    }
    result.map_err(|e| {
        crate::flamegraph::explain_dap_failure(
            e,
            &console.lock().unwrap(),
            &log.lock().unwrap(),
            &probe,
            &job.chip,
            true,
        )
    })
}

/// The flashing step's messages: progress shown, failures returned, until
/// both the `initialized` event and the `launch` response came.
fn launch(
    stream: &mut TcpStream,
    seq: &mut i64,
    state: &Arc<Mutex<TimingState>>,
    console: &Arc<Mutex<Vec<String>>>,
    ctx: &eframe::egui::Context,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(240);
    let (mut initialized, mut launched) = (false, false);
    while !(initialized && launched) {
        if Instant::now() > deadline {
            return Err("probe-rs did not finish flashing within 4 minutes".to_owned());
        }
        // probe-rs can be quiet for a while (erasing, connecting): a read
        // that waited out the socket's timeout is that, one that ended at
        // once is a closed server.
        let asked = Instant::now();
        let Some(msg) = crate::flamegraph::read_msg(stream) else {
            if asked.elapsed() < Duration::from_secs(1) {
                return Err("probe-rs dap-server closed while flashing".to_owned());
            }
            continue;
        };
        crate::flamegraph::collect_output(&msg, console);
        if msg["type"] == "response" && msg["success"].as_bool() == Some(false) {
            let cmd = msg["command"].as_str().unwrap_or("request");
            return Err(format!(
                "{cmd} failed: {}",
                crate::debugger::response_error(&msg)
            ));
        }
        if msg["type"] == "event" {
            match msg["event"].as_str() {
                Some("initialized") => {
                    initialized = true;
                    send(stream, seq, "configurationDone", json!({}))?;
                }
                Some("progressStart" | "progressUpdate") => {
                    let text = msg["body"]["message"]
                        .as_str()
                        .or(msg["body"]["title"].as_str())
                        .unwrap_or("flashing")
                        .to_owned();
                    let pct = msg["body"]["percentage"]
                        .as_f64()
                        .map(|p| format!(" {p:.0}%"))
                        .unwrap_or_default();
                    *state.lock().unwrap() = TimingState::Flashing(format!("{text}{pct}"));
                    ctx.request_repaint();
                }
                Some("terminated" | "exited") => {
                    return Err("the session ended while starting".to_owned());
                }
                _ => {}
            }
        }
        if msg["type"] == "response" && msg["command"] == "launch" {
            launched = true;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn session(
    job: &TimingJob,
    elf: &Path,
    probe: &str,
    port: u16,
    addr: u64,
    state: &Arc<Mutex<TimingState>>,
    stop: &Arc<AtomicBool>,
    console: &Arc<Mutex<Vec<String>>>,
    ctx: &eframe::egui::Context,
) -> Result<Option<TimingRun>, String> {
    let sock = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect_timeout(&sock, Duration::from_millis(200)) {
            stream = Some(s);
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let mut stream = stream.ok_or("could not connect to probe-rs dap-server")?;
    // `launch` waits out probe-rs's quiet spells in these steps.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let mut seq = 1i64;
    send(
        &mut stream,
        &mut seq,
        "initialize",
        json!({"adapterID": "probe-rs", "supportsProgressReporting": true}),
    )?;
    wait_response(&mut stream, "initialize", console)?;
    // The Debug tab's launch: flash, then run - not halted after reset.
    let mut args = json!({
        "cwd": job.project_dir.to_string_lossy(),
        "chip": job.chip,
        "probe": probe,
        "connectUnderReset": false,
        "flashingConfig": { "flashingEnabled": true, "haltAfterReset": false },
        "coreConfigs": [{ "coreIndex": 0, "programBinary": elf.to_string_lossy(), "rttEnabled": false }],
        "consoleLogLevel": "Console",
    });
    if crate::rtt::idf_partition_table(&job.chip, &job.project_dir) {
        args["flashingConfig"]["formatOptions"] = json!({
            "idf_options": {
                "idf_partition_table": job.project_dir.join("partitions.csv").to_string_lossy()
            }
        });
    }
    // The last moment before the board is touched: a Stop pressed while
    // probe-rs started flashes nothing.
    if stop.load(Ordering::SeqCst) {
        let _ = send(
            &mut stream,
            &mut seq,
            "disconnect",
            json!({"terminateDebuggee": false}),
        );
        let _ = wait_response(&mut stream, "disconnect", console);
        return Ok(None);
    }
    send(&mut stream, &mut seq, "launch", args)?;
    launch(&mut stream, &mut seq, state, console, ctx)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(6)))
        .map_err(|e| e.to_string())?;

    let mut run = TimingRun {
        regions: job.regions.clone(),
        stats: vec![RegionStat::default(); job.regions.len()],
        hz: job.hz,
        counter: job.counter,
        reads: 0,
        halting: false,
        halted_reads: 0,
        started: false,
        ended: None,
    };
    *state.lock().unwrap() = TimingState::Running(run.clone());
    ctx.request_repaint();
    let size = table_size(job.regions.len());
    let mut thread_id = 1i64;
    while !stop.load(Ordering::SeqCst) {
        // A second between reads, in short steps so Stop answers at once.
        for _ in 0..10 {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let read = poll(
            &mut stream,
            &mut seq,
            addr,
            size,
            job.regions.len(),
            &mut run,
            &mut thread_id,
            console,
        );
        match read {
            Ok(t) => {
                run.started = t.started;
                for (i, s) in t.stats.into_iter().enumerate() {
                    if let Some(s) = s {
                        run.stats[i] = s;
                    }
                }
                run.reads += 1;
            }
            Err(e) => {
                run.ended = Some(e);
                break;
            }
        }
        *state.lock().unwrap() = TimingState::Running(run.clone());
        ctx.request_repaint();
    }
    // The core keeps running the measuring build; probe-rs lets go of it.
    let _ = send(
        &mut stream,
        &mut seq,
        "disconnect",
        json!({"terminateDebuggee": false}),
    );
    let _ = wait_response(&mut stream, "disconnect", console);
    // A failed read ends the run, not the numbers it gathered.
    Ok(Some(run))
}

/// The table's bytes, read as the core runs; `None` when probe-rs refused
/// or answered short (`unreadableBytes` cuts records: none of it then).
fn read_running(
    stream: &mut TcpStream,
    seq: &mut i64,
    addr: u64,
    size: usize,
    console: &Arc<Mutex<Vec<String>>>,
) -> Result<Option<Vec<u8>>, String> {
    send(
        stream,
        seq,
        "readMemory",
        json!({"memoryReference": format!("{addr:#x}"), "count": size}),
    )?;
    Ok(wait_response(stream, "readMemory", console)
        .ok()
        .and_then(|m| from_base64(m["body"]["data"].as_str()?))
        .filter(|b| b.len() >= size))
}

/// The table's bytes, read between a `pause` and a `continue`.
fn read_halted(
    stream: &mut TcpStream,
    seq: &mut i64,
    addr: u64,
    size: usize,
    thread_id: &mut i64,
    console: &Arc<Mutex<Vec<String>>>,
) -> Result<Vec<u8>, String> {
    send(stream, seq, "pause", json!({"threadId": *thread_id}))?;
    let stopped = crate::flamegraph::wait_event(stream, "stopped", console)?;
    if let Some(t) = stopped["body"]["threadId"].as_i64() {
        *thread_id = t;
    }
    let bytes = read_running(stream, seq, addr, size, console);
    send(stream, seq, "continue", json!({"threadId": *thread_id}))?;
    bytes?.ok_or_else(|| "probe-rs could not read the timing table".to_owned())
}

/// One poll of the table. Read as the core runs when probe-rs can; a record
/// a region updated meanwhile - the probe reads a record in several
/// transactions - is read again with the core halted, where nothing moves.
/// A region that runs every few microseconds would otherwise be caught
/// mid-update on every read and never shown.
#[allow(clippy::too_many_arguments)]
fn poll(
    stream: &mut TcpStream,
    seq: &mut i64,
    addr: u64,
    size: usize,
    regions: usize,
    run: &mut TimingRun,
    thread_id: &mut i64,
    console: &Arc<Mutex<Vec<String>>>,
) -> Result<TableRead, String> {
    let mut running = None;
    if !run.halting {
        match read_running(stream, seq, addr, size, console)? {
            Some(b) => {
                let t = parse_table(&b, regions)?;
                if t.stats.iter().all(Option::is_some) {
                    return Ok(t);
                }
                running = Some(t);
            }
            None => run.halting = true,
        }
    }
    run.halted_reads += 1;
    let halted = parse_table(
        &read_halted(stream, seq, addr, size, thread_id, console)?,
        regions,
    )?;
    Ok(match running {
        Some(t) => TableRead {
            started: t.started || halted.started,
            stats: halted
                .stats
                .into_iter()
                .zip(t.stats)
                .map(|(h, r)| h.or(r))
                .collect(),
        },
        None => halted,
    })
}

/// The first line of each region, for the table's code column.
pub fn code_of(texts: &HashMap<String, String>, file: &str, line: u32) -> String {
    texts
        .get(file)
        .and_then(|t| t.lines().nth((line as usize).saturating_sub(1)))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(first: u32, last: u32) -> TimingRegion {
        TimingRegion {
            file: "src/main.rs".into(),
            first,
            last,
            whole_fn: false,
            col: 0,
            func: String::new(),
            fn_copy: (0, 1),
            anchor: Vec::new(),
            copy: (0, 1),
        }
    }

    const SRC: &str = "\
fn fade(level: u32) -> u32 {
    let a = level * 2;
    let b = a + 1; // keep
    if b > 10 {
        return 0;
    }
    b * 3
}

async fn task() {
    let x = read().await;
    use_it(x);
}

fn main() -> ! {
    loop {
        step();
    }
}

const K: u32 = { 1 + 2 };
";

    #[test]
    fn statements_get_a_guard_and_a_drop_on_their_own_lines() {
        let p = place(SRC, &region(2, 3)).unwrap();
        assert_eq!(p.func, "fade");
        assert_eq!(p.lines, (2, 3));
        assert!(!p.awaits);
        let out = instrument(SRC, &[(0, p)]).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(out.lines().count(), SRC.lines().count(), "no line moves");
        assert_eq!(
            lines[1],
            "    let __roc_g0 = crate::__roc::Span::start(0); let a = level * 2;"
        );
        // Before the comment, not after it.
        assert_eq!(
            lines[2],
            "    let b = a + 1; core::mem::drop(__roc_g0); // keep"
        );
    }

    #[test]
    fn a_region_that_reaches_the_tail_ends_with_the_block() {
        let p = place(SRC, &region(3, 7)).unwrap();
        assert_eq!(
            p.end, None,
            "the guard's Drop closes it, the tail stays a tail"
        );
        let out = instrument(SRC, &[(4, p)]).unwrap();
        assert!(out.contains("    let __roc_g4 = crate::__roc::Span::start(4); let b = a + 1;"));
        assert!(!out.contains("drop(__roc_g4)"));
    }

    #[test]
    fn a_header_line_selects_the_whole_statement_around_it() {
        // Line 4 is `if b > 10 {`: the `if` as a statement of `fade`.
        let p = place(SRC, &region(4, 4)).unwrap();
        assert_eq!(p.lines, (4, 6));
        assert!(p.end.is_some());
        // Inside the `if`: its own block.
        let p = place(SRC, &region(5, 5)).unwrap();
        assert_eq!(p.lines, (5, 5));
    }

    #[test]
    fn functions_awaits_and_refusals() {
        let mut r = region(2, 2);
        r.whole_fn = true;
        let p = place(SRC, &r).unwrap();
        assert_eq!((p.func.as_str(), p.end), ("fade", None));
        let out = instrument(SRC, &[(1, p)]).unwrap();
        // Before its first statement, on that statement's line.
        assert!(out.starts_with(
            "fn fade(level: u32) -> u32 {\n    let __roc_g1 = crate::__roc::Span::start(1); let a"
        ));
        r.first = 11;
        assert!(place(SRC, &r).unwrap().awaits);
        assert!(place(SRC, &region(11, 11)).unwrap().awaits);
        assert!(!place(SRC, &region(12, 12)).unwrap().awaits);
        // `-> !` ending in a loop: one pass of the loop.
        r.first = 16;
        let (pass, p) = mark(SRC, r.clone()).unwrap();
        assert!(!pass.whole_fn);
        assert_eq!((p.func.as_str(), p.lines), ("main", (17, 17)));
        // Without a loop to time it by, refused.
        let mut never = region(1, 1);
        never.whole_fn = true;
        assert!(
            place("fn halt() -> ! {\n    panic!()\n}\n", &never)
                .unwrap_err()
                .contains("never returns")
        );
        // The loop body is fine.
        assert_eq!(place(SRC, &region(17, 17)).unwrap().func, "main");
        // Nothing in a const.
        assert!(place(SRC, &region(21, 21)).is_err());
        assert!(
            place("fn broken( {", &region(1, 1))
                .unwrap_err()
                .contains("does not parse")
        );
    }

    #[test]
    fn nested_regions_open_outer_first_and_close_inner_first() {
        let outer = place(SRC, &region(2, 3)).unwrap();
        let inner = place(SRC, &region(2, 2)).unwrap();
        let out = instrument(SRC, &[(0, outer), (1, inner)]).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[1],
            concat!(
                "    let __roc_g0 = crate::__roc::Span::start(0); ",
                "let __roc_g1 = crate::__roc::Span::start(1); let a = level * 2; ",
                "core::mem::drop(__roc_g1);",
            )
        );
        assert_eq!(
            lines[2],
            "    let b = a + 1; core::mem::drop(__roc_g0); // keep"
        );
    }

    #[test]
    fn non_ascii_columns_are_chars_not_bytes() {
        let src = "fn f() {\n    let s = \"ăîș\"; let t = 1;\n    g(t);\n}\n";
        let p = place(src, &region(2, 2)).unwrap();
        let out = instrument(src, &[(0, p)]).unwrap();
        assert_eq!(
            out.lines().nth(1).unwrap(),
            "    let __roc_g0 = crate::__roc::Span::start(0); let s = \"ăîș\"; let t = 1; core::mem::drop(__roc_g0);"
        );
    }

    #[test]
    fn the_counter_follows_the_target() {
        assert_eq!(
            counter_for("riscv32imc-unknown-none-elf", "esp32c3"),
            Ok(Counter::EspPccr)
        );
        assert_eq!(
            counter_for("riscv32imac-unknown-none-elf", "esp32c6"),
            Ok(Counter::EspPccr)
        );
        assert_eq!(
            counter_for("riscv32imac-unknown-none-elf", "esp32c5"),
            Ok(Counter::Mcycle)
        );
        assert_eq!(
            counter_for("riscv32imac-unknown-none-elf", "esp32c61"),
            Ok(Counter::Mcycle)
        );
        assert_eq!(
            counter_for("riscv32imac-unknown-none-elf", "RP235x"),
            Ok(Counter::Mcycle)
        );
        assert_eq!(
            counter_for("thumbv7em-none-eabihf", "STM32F411RE"),
            Ok(Counter::Dwt { unlock: true })
        );
        assert_eq!(
            counter_for("thumbv8m.main-none-eabihf", "RP235x"),
            Ok(Counter::Dwt { unlock: false })
        );
        assert!(counter_for("thumbv6m-none-eabi", "RP2040").is_err());
        assert!(counter_for("xtensa-esp32-none-elf", "esp32").is_err());
    }

    #[test]
    fn the_module_matches_the_table_the_host_reads() {
        for c in [
            Counter::EspPccr,
            Counter::Mcycle,
            Counter::Dwt { unlock: true },
        ] {
            let m = module_source(c, 3);
            assert!(m.contains("pub const N: usize = 3;"));
            assert!(m.contains("#[unsafe(no_mangle)]"));
            assert!(m.contains("pub static mut __ROC_TIMING: Table"));
            assert!(m.contains(&format!("counter: {},", c.tag())));
            // No line of the user's file moves: the module only appends.
            assert!(m.starts_with('\n'));
        }
        assert!(module_source(Counter::EspPccr, 1).contains("0x7e2"));
        assert!(module_source(Counter::Dwt { unlock: true }, 1).contains("0xC5AC_CE55"));
        assert!(!module_source(Counter::Dwt { unlock: false }, 1).contains("0xC5AC_CE55"));
        assert!(module_source(Counter::Dwt { unlock: false }, 1).contains("0xE000_1004"));
        // 8 header words, 8 words per record.
        assert_eq!(table_size(3), 32 + 3 * 32);
    }

    #[test]
    fn the_table_is_read_back() {
        let mut b = vec![0u8; table_size(2)];
        let put = |b: &mut Vec<u8>, w: usize, v: u32| {
            b[w * 4..w * 4 + 4].copy_from_slice(&v.to_le_bytes())
        };
        put(&mut b, 0, MAGIC);
        put(&mut b, 1, 2);
        put(&mut b, 3, 1);
        // Region 0: 4 runs, 100..400, last 250, total 1000.
        for (k, v) in [
            (0, 8),
            (1, 4),
            (2, 100),
            (3, 400),
            (4, 250),
            (5, 1000),
            (6, 0),
            (7, 8),
        ] {
            put(&mut b, 8 + k, v);
        }
        // Region 1: an update began (its last word moved on) but has not
        // finished.
        put(&mut b, 16, 3);
        put(&mut b, 23, 4);
        let t = parse_table(&b, 2).unwrap();
        assert!(t.started);
        let s = t.stats[0].unwrap();
        assert_eq!(
            (s.count, s.min, s.max, s.last, s.total),
            (4, 100, 400, 250, 1000)
        );
        assert_eq!(s.avg(), Some(250.0));
        assert_eq!(t.stats[1], None);
        // Cut short (probe-rs's `unreadableBytes`): no half record.
        let cut = parse_table(&b[..40], 1).unwrap();
        assert_eq!(cut.stats, vec![None]);
        b[0] = 0;
        assert!(parse_table(&b, 2).is_err());
    }

    #[test]
    fn a_whole_function_is_timed_from_its_first_statement_that_runs() {
        // cortex-m-rt makes the leading `static mut`s `&mut` resources only
        // while they come first; an inner attribute must stay first too.
        let src = "#[interrupt]\nfn TIM2() {\n    #![allow(unused)]\n    static mut TICKS: u32 = 0;\n    *TICKS += 1;\n}\n\nfn empty() {}\n";
        let mut r = region(2, 2);
        r.whole_fn = true;
        let p = place(src, &r).unwrap();
        let out = instrument(src, &[(0, p)]).unwrap();
        assert!(
            out.contains("    static mut TICKS: u32 = 0;\n    let __roc_g0 = crate::__roc::Span::start(0); *TICKS += 1;"),
            "{out}"
        );
        // No statement at all: just before the closing brace.
        r.first = 8;
        let p = place(src, &r).unwrap();
        let out = instrument(src, &[(1, p)]).unwrap();
        assert!(
            out.contains("fn empty() {let __roc_g1 = crate::__roc::Span::start(1); }"),
            "{out}"
        );
    }

    #[test]
    fn a_region_to_the_blocks_end_opens_before_one_inside_it() {
        let src = "fn tick() {\n    let a = read();\n    let b = a + 1;\n    write(b)\n}\n";
        let outer = place(src, &region(2, 4)).unwrap();
        let inner = place(src, &region(2, 2)).unwrap();
        assert_eq!(outer.end, None);
        // Whatever order they were marked in.
        for regions in [
            vec![(0, outer.clone()), (1, inner.clone())],
            vec![(1, inner), (0, outer)],
        ] {
            let out = instrument(src, &regions).unwrap();
            assert_eq!(
                out.lines().nth(1).unwrap(),
                concat!(
                    "    let __roc_g0 = crate::__roc::Span::start(0); ",
                    "let __roc_g1 = crate::__roc::Span::start(1); let a = read(); ",
                    "core::mem::drop(__roc_g1);",
                )
            );
        }
    }

    #[test]
    fn a_byte_order_mark_does_not_shift_line_one() {
        let src = "\u{feff}fn go() { let a = 1; }\n";
        let mut r = region(1, 1);
        r.whole_fn = true;
        let p = place(src, &r).unwrap();
        let out = instrument(src, &[(0, p)]).unwrap();
        assert_eq!(
            out,
            "\u{feff}fn go() { let __roc_g0 = crate::__roc::Span::start(0); let a = 1; }\n"
        );
    }

    fn marked(src: &str, first: u32, last: u32, whole_fn: bool) -> TimingRegion {
        let mut r = region(first, last);
        r.whole_fn = whole_fn;
        let p = place(src, &r).unwrap();
        anchored(src, r, &p)
    }

    #[test]
    fn an_edit_moves_a_region_and_never_onto_other_code() {
        let before = "fn a() {\n    x();\n}\n\nfn b() {\n    let v = 1;\n    y(v);\n    z();\n}\n";
        let b = marked(before, 6, 7, false);
        let whole_b = marked(before, 5, 5, true);
        assert_eq!((b.func.as_str(), b.fn_copy, b.copy), ("b", (0, 1), (0, 1)));
        assert_eq!(b.anchor, ["let v = 1;", "y(v);"]);
        // Two lines added in `a`, above both.
        let after = "fn a() {\n    x();\n    x();\n    x();\n}\n\nfn b() {\n    let v = 1;\n    y(v);\n    z();\n}\n";
        assert_eq!(locate(after, &b), Ok((8, 9)));
        assert_eq!(locate(after, &whole_b), Ok((7, 11)));
        let texts: HashMap<String, String> = [("src/main.rs".to_owned(), after.to_owned())].into();
        let (out, infos) =
            instrumented_copies(&texts, &[b.clone(), whole_b.clone()], Counter::EspPccr).unwrap();
        assert_eq!(infos[0].lines, (8, 9));
        assert_eq!(infos[1].func, "b");
        assert!(
            out["src/main.rs"].contains("let __roc_g0 = crate::__roc::Span::start(0); let v = 1;")
        );
        // A whole function does not care what changed inside it.
        let inside = after.replace("    z();\n", "    z();\n    z();\n");
        assert_eq!(locate(&inside, &whole_b), Ok((7, 12)));
        // Statements edited inside, or gone: refused, never timed elsewhere.
        for edited in [
            after.replace("    y(v);\n", "    w();\n    y(v);\n"),
            after.replace("let v = 1;", "let v = 2;"),
        ] {
            let texts: HashMap<String, String> = [("src/main.rs".to_owned(), edited)].into();
            let e = instrumented_copies(&texts, &[b.clone()], Counter::EspPccr).unwrap_err();
            assert!(
                e.starts_with("Row 1: ") && e.contains("has changed - mark it again"),
                "{e}"
            );
        }
    }

    #[test]
    fn a_duplicated_line_stays_the_same_copy_wherever_lines_are_added() {
        let before =
            "fn main() {\n    on();\n    delay();\n    off();\n    delay();\n    count();\n}\n";
        // The second `delay();` is marked.
        let r = marked(before, 5, 5, false);
        assert_eq!(
            (r.anchor.as_slice(), r.copy),
            (&["delay();".to_owned()][..], (1, 2))
        );
        // Lines above `main`, or inside it above both copies (the pin lines
        // a Pins change adds to the generated block): still the second.
        assert_eq!(locate(&format!("use x;\n{before}"), &r), Ok((6, 6)));
        let inside = before.replace(
            "fn main() {\n",
            "fn main() {\n    let p = 1;\n    let q = 2;\n",
        );
        assert_eq!(locate(&inside, &r), Ok((7, 7)));
        // One copy gone: nothing says which one is left.
        let one = before.replacen("    delay();\n", "", 1);
        assert!(
            locate(&one, &r)
                .unwrap_err()
                .contains("1 times now (2 when it was marked)")
        );
        // Both copies marked: two rows on two places, never one.
        let first = marked(before, 3, 3, false);
        let texts: HashMap<String, String> = [("src/main.rs".to_owned(), inside)].into();
        let (_, infos) = instrumented_copies(&texts, &[first, r], Counter::EspPccr).unwrap();
        assert_eq!((infos[0].lines, infos[1].lines), ((5, 5), (7, 7)));
    }

    #[test]
    fn functions_are_named_by_their_impl_and_told_apart_by_copy() {
        let src = "impl A {\n    pub fn new() -> Self {\n        a()\n    }\n}\nimpl B {\n    pub fn new() -> Self {\n        b()\n    }\n}\n";
        let b_new = marked(src, 7, 7, true);
        assert_eq!(b_new.func, "B::new");
        let moved = format!("fn pad() {{}}\nfn pad2() {{}}\n{src}");
        assert_eq!(locate(&moved, &b_new), Ok((9, 11)));
        let r = marked(
            "impl core::fmt::Display for B {\n    fn fmt(&self) {\n        w();\n    }\n}\n",
            3,
            3,
            false,
        );
        assert_eq!(r.func, "<B as Display>::fmt");
        // One generic trait, two impls: two paths.
        let from = "impl From<u8> for L {\n    fn from(v: u8) -> Self {\n        L\n    }\n}\nimpl From<u16> for L {\n    fn from(v: u16) -> Self {\n        L\n    }\n}\n";
        assert_eq!(marked(from, 7, 7, true).func, "<L as From<u16>>::from");
        // `#[cfg]` twins share a path: the copy decides, and a third is refused.
        let twins = "#[cfg(a)]\nfn f() {\n    x();\n}\n#[cfg(not(a))]\nfn f() {\n    y();\n}\n";
        let second = marked(twins, 6, 6, true);
        assert_eq!(second.fn_copy, (1, 2));
        assert_eq!(locate(&format!("use z;\n{twins}"), &second), Ok((7, 9)));
        let three = format!("{twins}#[cfg(b)]\nfn f() {{}}\n");
        assert!(
            locate(&three, &second)
                .unwrap_err()
                .contains("there are 3 functions `f` now")
        );
    }

    #[test]
    fn lines_commented_out_never_widen_a_row_to_the_code_around_them() {
        let src = "fn main() {\n    let p = 1;\n    loop {\n        a();\n        b();\n        c();\n    }\n}\n";
        let r = marked(src, 5, 5, false);
        assert_eq!(resolve(src, &r).unwrap().1.lines, (5, 5));
        // The IDE's block comment leaves `b();` itself untouched: found
        // again, but it is no statement any more - not the loop around it.
        let commented = src.replace(
            "        a();\n        b();\n        c();\n",
            "        /* a();\n        b();\n        c(); */\n",
        );
        assert_eq!(locate(&commented, &r), Ok((5, 5)));
        let texts: HashMap<String, String> = [("src/main.rs".to_owned(), commented)].into();
        let e = instrumented_copies(&texts, &[r], Counter::EspPccr).unwrap_err();
        assert!(e.contains("has changed - mark it again"), "{e}");
    }

    #[test]
    fn statements_in_const_contexts_are_refused_when_marked() {
        let src = "trait S {\n    const K: u32 = {\n        let b = 10;\n        b * 2\n    };\n}\nfn f() {\n    let a = [0u8; {\n        let n = 4;\n        n * 2\n    }];\n    g(a);\n}\nenum E {\n    A = {\n        let v = 1;\n        v\n    },\n}\n";
        // A trait's const, an array length, a discriminant: nothing can run a
        // guard there.
        assert!(place(src, &region(3, 3)).is_err());
        assert!(place(src, &region(16, 16)).is_err());
        // Inside the array length the statement is the whole `let a`.
        assert_eq!(place(src, &region(9, 9)).unwrap().lines, (8, 11));
    }

    #[test]
    fn a_mark_is_refused_unless_measure_finds_it_the_same_way() {
        // `let v` starts on the loop's `{` line: its lines would place as the
        // whole loop at Measure.
        let src = "fn main() {\n    loop { let v = adc.read(\n            &mut pin);\n        tx.write(v);\n    }\n}\n";
        assert!(
            mark(src, region(3, 3))
                .unwrap_err()
                .contains("lines of its own")
        );
        // `a(); b(` with only `b` chosen would take `a()` along.
        let src = "fn f() {\n    a(); b(\n    );\n    c();\n}\n";
        assert!(mark(src, region(3, 3)).is_err());
        // Laid out one statement per line, it is marked.
        let src = "fn f() {\n    a();\n    b();\n}\n";
        assert_eq!(mark(src, region(3, 3)).unwrap().1.lines, (3, 3));
    }

    #[test]
    fn a_closure_is_a_function_of_its_own() {
        let src = "fn outer() {\n    let work = async |a: u32| {\n        step(a).await;\n        other();\n    };\n    let v: Vec<u32> = xs.iter().map(|x| {\n        x + 1\n    }).collect();\n    let short = |x: u32| x * 2;\n    work(1).await;\n}\n";
        let mut r = region(3, 3);
        r.whole_fn = true;
        let p = place(src, &r).unwrap();
        assert_eq!(
            (p.func.as_str(), p.lines, p.awaits),
            ("outer::work", (2, 5), true)
        );
        let out = instrument(src, &[(0, p)]).unwrap();
        assert!(
            out.contains("        let __roc_g0 = crate::__roc::Span::start(0); step(a).await;"),
            "{out}"
        );
        // Unnamed: named by where it is, told apart by its copy.
        r.first = 7;
        assert_eq!(place(src, &r).unwrap().func, "outer::{closure}");
        // No braces: nothing to put a guard in.
        // The caret inside it (column 16); at column 0 it is `outer`'s line.
        r.first = 9;
        r.col = 16;
        assert!(place(src, &r).unwrap_err().contains("no braces"));
        r.col = 0;
        assert_eq!(place(src, &r).unwrap().func, "outer");
        // Statements inside a closure belong to it.
        assert_eq!(place(src, &region(4, 4)).unwrap().func, "outer::work");
        // Found again after lines added above.
        let work = marked(src, 3, 3, true);
        assert_eq!(locate(&format!("use a;\nuse b;\n{src}"), &work), Ok((4, 7)));
    }

    #[test]
    fn two_closures_on_one_line_are_told_apart_by_their_column() {
        let src = "fn show(items: &[u8]) {\n    items.iter().filter(|i| **i != 0).for_each(|i| {\n        draw(i);\n        flush();\n    });\n    let n = items.iter().filter(|b| **b != 0).count();\n}\n";
        // Statements of the `for_each` closure: marked and found again.
        assert_eq!(mark(src, region(3, 3)).unwrap().1.lines, (3, 3));
        // The caret inside the `for_each` closure, past the `filter` one.
        let mut r = region(2, 2);
        r.whole_fn = true;
        r.col = 50;
        let (whole, p) = mark(src, r.clone()).unwrap();
        assert_eq!((p.func.as_str(), p.lines), ("show::{closure}", (2, 5)));
        assert_eq!(locate(&format!("use a;\n{src}"), &whole), Ok((3, 6)));
        // At column 0 of a line with a brace-less closure: the function.
        r.first = 6;
        r.col = 0;
        assert_eq!(place(src, &r).unwrap().func, "show");
        // Inside the brace-less one: refused, it is what the caret is on.
        r.col = 32;
        assert!(place(src, &r).unwrap_err().contains("no braces"));
    }

    #[test]
    fn two_closures_on_one_line_are_two_rows_at_measure_too() {
        let src = "fn main() {\n    btn.on_press(|| { n += 1; }).on_release(|| { n -= 1; });\n}\n";
        let whole = |col: u32| {
            let mut r = region(2, 2);
            r.whole_fn = true;
            r.col = col;
            mark(src, r).unwrap().0
        };
        // Inside the first closure, inside the second.
        let (press, release) = (whole(22), whole(48));
        assert_ne!(press.col, release.col);
        let texts: HashMap<String, String> = [("src/main.rs".to_owned(), src.to_owned())].into();
        let (out, infos) =
            instrumented_copies(&texts, &[press.clone(), release], Counter::EspPccr).unwrap();
        assert_eq!(infos.len(), 2);
        assert!(
            out["src/main.rs"]
                .contains("|| { let __roc_g1 = crate::__roc::Span::start(1); n -= 1; }")
        );
        // The same row twice is still refused.
        assert!(
            instrumented_copies(&texts, &[press.clone(), press], Counter::EspPccr)
                .unwrap_err()
                .contains("time the same code")
        );
    }

    #[test]
    fn the_caret_on_a_call_is_seen_as_one_even_inside_a_macro() {
        let src = "fn main() {\n    show(1);\n    radar.begin_config();\n    println!(\"{}\", calc(x));\n    <Led as Drop>::drop(&mut led);\n    let v = x;\n}\n";
        assert_eq!(call_name_at(src, 2, 6).as_deref(), Some("show"));
        assert_eq!(call_name_at(src, 3, 14).as_deref(), Some("begin_config"));
        assert_eq!(call_name_at(src, 4, 21).as_deref(), Some("calc"));
        assert_eq!(call_name_at(src, 5, 21).as_deref(), Some("drop"));
        // An argument, a receiver, a variable: no call.
        assert_eq!(call_name_at(src, 2, 9), None);
        assert_eq!(call_name_at(src, 3, 6), None);
        assert_eq!(call_name_at(src, 6, 9), None);
    }

    #[test]
    fn a_definitions_position_names_its_callable() {
        let src = "fn main() {\n    let show = async |n: u32| {\n        draw(n);\n    };\n    let v = 1;\n}\nimpl Led {\n    pub fn set(&mut self) {\n        on();\n    }\n}\nstruct P(u8);\n";
        // rust-analyzer points at the NAME: of a let-bound closure, of a fn.
        assert_eq!(callable_at_definition(src, 2, 8), Some((2, 15)));
        assert_eq!(callable_at_definition(src, 1, 3), Some((1, 0)));
        assert_eq!(callable_at_definition(src, 8, 11), Some((8, 8)));
        // A variable, a struct: no callable.
        assert_eq!(callable_at_definition(src, 5, 8), None);
        assert_eq!(callable_at_definition(src, 12, 7), None);
        // Why not, said from the syntax, not from how a line looks.
        let decl = "trait S {\n    fn read<T>(&mut self) -> T\n    where\n        T: From<u16>;\n}\nunsafe extern \"C\" {\n    fn crc(p: *const u8) -> u32;\n}\nfn f() {\n    let g = if fast { fast_fn } else { slow_fn };\n}\n";
        assert_eq!(
            not_callable(decl, "src/s.rs", 2, 8),
            NotCallable::TraitFnWithoutBody
        );
        assert_eq!(not_callable(decl, "src/s.rs", 7, 8), NotCallable::ExternFn);
        assert_eq!(not_callable(decl, "src/s.rs", 10, 8), NotCallable::Other);
        assert!(matches!(
            not_callable("fn broken( {", "src/b.rs", 1, 3),
            NotCallable::Unparsed(_)
        ));
        // A function passed by name is a path the caret can be on.
        let pass =
            "fn main() {\n    leds.for_each(toggle);\n}\nfn toggle(l: &mut L) {\n    l.t();\n}\n";
        assert_eq!(path_name_at(pass, 2, 20).as_deref(), Some("toggle"));
        assert!(defines_callable(pass, "toggle") && !defines_callable(pass, "leds"));
        // And the callable it names is what gets marked, as a whole.
        let mut r = region(2, 2);
        r.whole_fn = true;
        r.col = 15;
        assert_eq!(place(src, &r).unwrap().func, "main::show");
        r.first = 8;
        r.col = 8;
        assert_eq!(place(src, &r).unwrap().func, "Led::set");
    }

    #[test]
    fn a_task_that_never_returns_is_timed_by_one_pass_of_its_loop() {
        // The user's embassy tasks: set up, then loop for ever.
        let src = "async fn tick() {\n    loop {\n        inc();\n        delay(999_990).await;\n    }\n}\nasync fn listen() {\n    let mut nav = Nav::new();\n    loop {\n        let e = next().await;\n        nav.feed(e);\n    }\n}\n";
        for (line, lines, awaits) in [(1, (3, 4), true), (7, (10, 11), true)] {
            let mut r = region(line, line);
            r.whole_fn = true;
            let (region, p) = mark(src, r).unwrap();
            assert!(!region.whole_fn, "line {line}");
            assert_eq!((p.lines, p.awaits), (lines, awaits));
        }
        // A loop that can end is no endless one: the function as a whole.
        for body in [
            "loop {\n        if done() {\n            break;\n        }\n    }",
            "loop {\n        if done() {\n            return;\n        }\n    }",
            "loop {\n        step()?;\n    }",
            "'outer: loop {\n        loop {\n            break 'outer;\n        }\n    }",
        ] {
            let src = format!("fn f() -> Result<(), E> {{\n    {body}\n}}\n");
            let mut r = region(1, 1);
            r.whole_fn = true;
            let (region, p) = mark(&src, r).unwrap();
            assert!(region.whole_fn && p.loop_body.is_none(), "{body}");
        }
        // One pass that would place as the whole loop: refused, not marked.
        for src in [
            "fn main() -> ! {\n    init();\n    loop { step(); }\n}\n",
            "fn main() -> ! {\n    loop {\n        a();\n        b(); }\n}\n",
            "async fn t() {\n    loop {}\n}\n",
        ] {
            let mut r = region(1, 1);
            r.whole_fn = true;
            let e = mark(src, r).unwrap_err();
            assert!(e.contains("never returns"), "{src}: {e}");
        }
        // A `?` inside a macro's arguments can leave the loop.
        let src = "async fn poll() -> Result<(), E> {\n    loop {\n        info!(\"t = {:?}\", read().await?);\n        wait().await;\n    }\n}\n";
        let mut r = region(1, 1);
        r.whole_fn = true;
        assert!(mark(src, r).unwrap().0.whole_fn);
        let src = "async fn poll() {\n    loop {\n        info!(\"t = {:?}\", read().await);\n        wait().await;\n    }\n}\n";
        let mut r = region(1, 1);
        r.whole_fn = true;
        assert!(!mark(src, r).unwrap().0.whole_fn, "`{{:?}}` is no `?`");
        // A whole row whose function became endless: refused at Measure.
        let finite = "fn f() {\n    loop {\n        g();\n        if done() {\n            break;\n        }\n    }\n}\n";
        let mut r = region(1, 1);
        r.whole_fn = true;
        let row = mark(finite, r).unwrap().0;
        assert!(row.whole_fn);
        let endless = finite.replace("            break;\n", "            h();\n");
        assert!(
            resolve(&endless, &row)
                .unwrap_err()
                .contains("now never returns")
        );
        // An inner loop's own `break` does not leave the outer one.
        let src = "fn f() {\n    loop {\n        loop {\n            break;\n        }\n        tick();\n    }\n}\n";
        let mut r = region(1, 1);
        r.whole_fn = true;
        assert!(!mark(src, r).unwrap().0.whole_fn);
    }

    #[test]
    fn a_naked_function_is_refused() {
        for attr in ["#[naked]", "#[unsafe(naked)]"] {
            let src = format!(
                "{attr}\npub unsafe extern \"C\" fn switch() {{\n    core::arch::naked_asm!(\n        \"bx lr\",\n    )\n}}\n"
            );
            let mut r = region(3, 3);
            r.whole_fn = true;
            assert!(
                place(&src, &r).unwrap_err().contains("naked function"),
                "{attr}"
            );
            assert!(place(&src, &region(3, 3)).is_err(), "{attr}");
        }
    }

    #[test]
    fn a_function_inside_a_const_block_is_runtime_code() {
        let src = "const _: () = {\n    impl Format for R {\n        fn format(&self) {\n            write(self.0);\n        }\n    }\n};\n";
        let mut r = region(3, 3);
        r.whole_fn = true;
        assert_eq!(place(src, &r).unwrap().func, "<R as Format>::format");
        assert_eq!(place(src, &region(4, 4)).unwrap().lines, (4, 4));
        // A const fn still is not.
        assert!(
            place(
                "const fn k() -> u32 {\n    let a = 1;\n    a\n}\n",
                &region(2, 2)
            )
            .is_err()
        );
    }

    #[test]
    fn a_region_ending_in_a_brace_is_found_by_all_its_lines() {
        let before = "fn f() {\n    if a {\n        b();\n        c();\n        d();\n    }\n    if e {\n        g();\n    }\n}\n";
        let r = marked(before, 2, 6, false);
        assert_eq!(r.anchor.len(), 5);
        // c() and d() deleted: the `if a` is not the code that was marked,
        // and the `if e` after it must not be swept in.
        let after = before.replace("        c();\n        d();\n", "");
        assert!(locate(&after, &r).unwrap_err().contains("has changed"));
    }

    #[test]
    fn the_copies_are_the_marked_files_and_main_rs_with_the_module() {
        let texts: HashMap<String, String> = [
            (
                "src/main.rs".to_owned(),
                "mod util;\nfn main() {\n    util::go();\n}\n".to_owned(),
            ),
            (
                "src/util.rs".to_owned(),
                "pub fn go() {\n    let a = 1;\n    let b = a;\n}\n".to_owned(),
            ),
            ("mylib/src/lib.rs".to_owned(), "pub fn x() {}\n".to_owned()),
        ]
        .into();
        let mut r = region(2, 2);
        r.file = "src/util.rs".into();
        let (out, infos) = instrumented_copies(&texts, &[r.clone()], Counter::EspPccr).unwrap();
        assert_eq!(out.len(), 2, "util.rs and main.rs, nothing else");
        assert!(out["src/util.rs"].contains("crate::__roc::Span::start(0); let a = 1;"));
        assert!(out["src/main.rs"].starts_with(&texts["src/main.rs"]));
        assert!(out["src/main.rs"].contains("pub const N: usize = 1;"));
        // Stored as what it covers, with the text that finds it again.
        let at = &infos[0].region;
        assert_eq!(
            (at.first, at.last, at.anchor.as_slice()),
            (2, 2, &["let a = 1;".to_owned()][..])
        );
        assert_eq!(
            (infos[0].func.as_str(), infos[0].code.as_str()),
            ("go", "let a = 1;")
        );
        // A library's file cannot see the module.
        r.file = "mylib/src/lib.rs".into();
        assert!(instrumented_copies(&texts, &[r.clone()], Counter::EspPccr).is_err());
        r.file = "src/gone.rs".into();
        assert!(
            instrumented_copies(&texts, &[r], Counter::EspPccr)
                .unwrap_err()
                .contains("not in the project")
        );
    }

    /// The generated module, with a guard using it, compiles for every
    /// target and counter it is offered for, on both editions: `rustc` alone,
    /// no dependency, seconds. Needs the targets installed (`rustup target`).
    #[test]
    #[ignore = "runs rustc for five targets: set EIDE_TIMING_RUSTC=1"]
    fn the_module_compiles_for_every_target() {
        if std::env::var("EIDE_TIMING_RUSTC").is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("roc_timing_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = concat!(
            "#![no_std]\n",
            "pub fn work(x: u32) -> u32 {\n",
            "    let __roc_g0 = crate::__roc::Span::start(0); let y = x.wrapping_mul(3); ",
            "core::mem::drop(__roc_g0);\n",
            "    let __roc_g1 = crate::__roc::Span::start(1); y + 1\n",
            "}\n",
        );
        let mut failed = Vec::new();
        for (target, counter) in [
            ("riscv32imc-unknown-none-elf", Counter::EspPccr),
            ("riscv32imac-unknown-none-elf", Counter::Mcycle),
            ("thumbv7m-none-eabi", Counter::Dwt { unlock: true }),
            ("thumbv7em-none-eabihf", Counter::Dwt { unlock: true }),
            ("thumbv8m.main-none-eabihf", Counter::Dwt { unlock: false }),
        ] {
            for edition in ["2021", "2024"] {
                let src = dir.join("lib.rs");
                std::fs::write(&src, format!("{root}{}", module_source(counter, 2))).unwrap();
                let out = Command::new("rustc")
                    .args([
                        "--edition",
                        edition,
                        "--crate-type",
                        "lib",
                        "-O",
                        "--target",
                        target,
                    ])
                    .args(["--emit", "obj", "-o"])
                    .arg(dir.join("lib.o"))
                    .arg(&src)
                    .output()
                    .unwrap();
                if !out.status.success() {
                    failed.push(format!(
                        "{target} / {edition}:\n{}",
                        String::from_utf8_lossy(&out.stderr)
                    ));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    }

    /// A REAL file's closures and the calls to them: every closure with braces
    /// is marked as itself - from inside it, and from the caret on a call to
    /// it - never as the function around it. `EIDE_TIMING_FILE`: a `.rs` file.
    #[test]
    #[ignore = "reads a project file: set EIDE_TIMING_FILE"]
    fn a_real_files_closures_are_marked_as_themselves() {
        let Ok(path) = std::env::var("EIDE_TIMING_FILE") else {
            return;
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let fns = functions(&text, "src/main.rs").unwrap();
        let mut closures = 0;
        for f in fns.iter().filter(|f| f.kind == Callable::Closure) {
            let mut r = region(f.sig + 1, f.sig + 1);
            r.whole_fn = true;
            match mark(&text, r) {
                Ok((region, p)) => {
                    eprintln!("{} lines {:?} awaits {}", p.func, p.lines, p.awaits);
                    assert_eq!(region.func, f.path);
                    assert_eq!(p.lines, (f.sig, f.close));
                    closures += 1;
                }
                Err(e) => eprintln!("{} (line {}): {e}", f.path, f.sig),
            }
        }
        // Every call by name of a bound closure is seen as a call, and the
        // binding rust-analyzer answers with names the closure.
        for f in fns.iter().filter(|f| f.kind == Callable::Closure) {
            let name = f.path.rsplit("::").next().unwrap();
            if name == "{closure}" {
                continue;
            }
            let bind = text.lines().nth(f.sig as usize - 1).unwrap();
            let at = bind.find(&format!("let {name}")).unwrap() + 4;
            let col = bind[..at].chars().count();
            assert_eq!(
                callable_at_definition(&text, f.sig as usize, col),
                Some((f.sig, f.col)),
                "{name}"
            );
            for (i, line) in text.lines().enumerate().skip(f.close as usize) {
                if let Some(at) = line.find(&format!("{name}(")) {
                    let col = line[..at].chars().count() + 1;
                    eprintln!("line {}: {name}(", i + 1);
                    assert_eq!(call_name_at(&text, i + 1, col).as_deref(), Some(name));
                }
            }
        }
        assert!(closures > 0, "no closure with braces in {path}");
    }

    /// A REAL file's functions as "Time this function" marks them: a task that
    /// never returns as one pass of its loop, every other one as itself.
    /// `EIDE_TIMING_FILE`: a `.rs` file.
    #[test]
    #[ignore = "reads a project file: set EIDE_TIMING_FILE"]
    fn a_real_files_functions_are_marked_as_they_can_be_timed() {
        let Ok(path) = std::env::var("EIDE_TIMING_FILE") else {
            return;
        };
        let text = std::fs::read_to_string(&path).unwrap();
        for f in functions(&text, "src/main.rs").unwrap() {
            let mut r = region(f.sig, f.sig);
            r.whole_fn = true;
            r.col = f.col;
            match mark(&text, r) {
                Ok((region, p)) if region.whole_fn => {
                    eprintln!("{}: whole, lines {:?}", f.path, p.lines)
                }
                Ok((_, p)) => eprintln!("{}: one pass of its loop, lines {:?}", f.path, p.lines),
                Err(e) => eprintln!("{}: refused - {e}", f.path),
            }
        }
    }

    /// Stop ends a release build at once - cargo and the rustc under it -
    /// rather than after it. `EIDE_TIMING_PROJECT` / `EIDE_TIMING_TARGET`:
    /// a project whose build takes longer than three seconds (a cold one).
    #[test]
    #[ignore = "starts a firmware build: set EIDE_TIMING_PROJECT/TARGET"]
    fn a_stopped_build_ends_at_once() {
        let (Ok(dir), Ok(target)) = (
            std::env::var("EIDE_TIMING_PROJECT"),
            std::env::var("EIDE_TIMING_TARGET"),
        ) else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let later = Arc::clone(&stop);
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(3));
            later.store(true, Ordering::SeqCst);
        });
        let t = Instant::now();
        let built = crate::flamegraph::build_elf_until(Path::new(&dir), &target, Some(&stop));
        assert_eq!(built, Ok(None));
        assert!(t.elapsed() < Duration::from_secs(8), "{:?}", t.elapsed());
    }

    /// A REAL project's `main.rs`, instrumented the way Measure does it, builds
    /// for its target, and its ELF has the table at the size the host reads.
    /// `EIDE_TIMING_PROJECT` is a COPY of a build workspace (its `main.rs` is
    /// rewritten, then put back); `EIDE_TIMING_TARGET` / `EIDE_TIMING_CHIP`.
    #[test]
    #[ignore = "builds a firmware: set EIDE_TIMING_PROJECT/TARGET/CHIP"]
    fn a_real_project_builds_instrumented() {
        let (Ok(dir), Ok(target), Ok(chip)) = (
            std::env::var("EIDE_TIMING_PROJECT"),
            std::env::var("EIDE_TIMING_TARGET"),
            std::env::var("EIDE_TIMING_CHIP"),
        ) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let path = dir.join("src").join("main.rs");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines = text.lines().count() as u32;
        let at = |whole_fn: bool, keep: &dyn Fn(&Placement) -> bool| {
            (1..=lines).find_map(|l| {
                let r = TimingRegion {
                    file: "src/main.rs".into(),
                    first: l,
                    last: l,
                    whole_fn,
                    col: 0,
                    func: String::new(),
                    fn_copy: (0, 1),
                    anchor: Vec::new(),
                    copy: (0, 1),
                };
                place(&text, &r).ok().filter(|p| keep(p))
            })
        };
        // A whole function, statements of the entry function, and a region
        // nested in those statements' block.
        // (A `-> !` entry with no other function has no whole one to time.)
        let stmts =
            at(false, &|p| p.func == "main" && p.end.is_some()).expect("statements in main");
        let mut regions = vec![stmts];
        regions.extend(at(true, &|_| true));
        regions.extend(at(false, &|p| p.func == "main" && p.end.is_none()));
        // A closure as a whole (an `async` one holds its guard across awaits).
        regions.extend(
            functions(&text, "src/main.rs")
                .unwrap()
                .iter()
                .filter(|f| f.kind == Callable::Closure)
                .find_map(|f| {
                    let mut r = region(f.sig + 1, f.sig + 1);
                    r.whole_fn = true;
                    place(&text, &r).ok()
                }),
        );
        let regions: Vec<(usize, Placement)> = regions.into_iter().enumerate().collect();
        for (id, p) in &regions {
            eprintln!(
                "region {id}: {} lines {:?} awaits {}",
                p.func, p.lines, p.awaits
            );
        }
        let counter = counter_for(&target, &chip).unwrap();
        let out = instrument(&text, &regions).unwrap() + &module_source(counter, regions.len());
        std::fs::write(&path, &out).unwrap();
        let built = crate::flamegraph::build_elf(&dir, &target);
        std::fs::write(&path, &text).unwrap();
        let elf = built.unwrap();
        let (addr, size) = table_symbol(&elf).unwrap();
        eprintln!("__ROC_TIMING at {addr:#x}, {size} bytes");
        assert!(addr != 0);
        assert!(size == 0 || size == table_size(regions.len()) as u64);
    }
}
