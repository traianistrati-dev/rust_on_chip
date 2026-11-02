//! The user's own RTIC tasks, declared by their code below the markers.
//!
//! On RTIC the whole `#[rtic::app] mod app` sits inside the GENERATED block,
//! rebuilt on every regeneration, so a task written in it would not survive.
//! RTIC 2 also takes a task DECLARED in an `extern "Rust"` block inside the
//! app and implemented outside it - rtic-macros calls the implementation by
//! its name, which the app's `use super::*` reaches at the crate root. So the
//! user writes the task below the markers, and the block declares it:
//!
//! - a top-level `fn` whose first parameter is `app::<its own name>::Context`
//!   is a task - the signature the user has to write anyway for it to compile;
//!   an `async fn` a software task, a plain `fn` a hardware one;
//! - `// @task(..)` right above it (over `///` and `#[..]` lines only) holds
//!   RTIC's own `#[task(..)]` arguments, copied through as written - plus
//!   `start`, the IDE's own, which spawns it at the end of `#[init]`;
//! - `// @idle(shared = [..])` above `fn idle_loop` locks Shared resources
//!   from the idle loop too;
//! - `struct AppShared` with `fn app_shared() -> AppShared` beside it: each
//!   field one `#[shared]` resource, built by that fn in `#[init]`.
//!
//! A generated Local a task claims leaves `#[idle]`; one a `shared = [..]`
//! names moves to `#[shared]`. What cannot be declared is said by a
//! `compile_error!` in the block, naming the task - never by a line number,
//! which would rewrite the block whenever a line is added above it.
//!
//! Nothing is persisted: the user's text is the record. A project with no
//! such fn generates exactly what it did before.

use std::collections::BTreeSet;

use super::common::{code_only, user_spans};

/// The interrupts the software tasks' dispatchers run on, in the order they
/// are taken: one per distinct software priority. Present under these names
/// in every STM32F1 PAC (f100/f101/f102/f103/f107) and bound by nothing the
/// IDE generates - stm32f1xx-hal defines no RCC/FLASH handler, PVD fires only
/// with EXTI16 armed, WWDG has no driver there, and the HAL's DMA is blocking.
/// The two DMA channels come last: the only ones a user might `listen()` on.
pub const F1_DISPATCHERS: [&str; 6] = [
    "RCC",
    "FLASH",
    "PVD",
    "WWDG",
    "DMA1_CHANNEL7",
    "DMA1_CHANNEL6",
];

/// Above each of the user's tasks - and `AppShared` / `app_shared` - while
/// the runtime is not RTIC: a task names RTIC's `app`, which nothing else
/// writes, so it is compiled out rather than failing the build, and taken off
/// on the way back (the idle loop's own fence is `RTIC_IDLE_FENCE`).
pub const RTIC_TASK_FENCE: &str =
    "#[cfg(any())] // RTIC task: built only on the RTIC runtime, whose `app` it names\n";

/// Vectors no task may bind: the core's exceptions, SysTick the monotonic's.
const EXCEPTIONS: &[&str] = &[
    "SysTick",
    "PendSV",
    "SVCall",
    "NMI",
    "HardFault",
    "MemoryManagement",
    "BusFault",
    "UsageFault",
    "DebugMonitor",
];

/// Names the generated app already uses - its functions, and the heads of
/// the paths it writes inside `mod app`: RTIC puts a `mod <task>` there, which
/// would shadow `gpio::..`, `rtic::export::..` or the device path.
const RESERVED: &[&str] = &[
    "gpio",
    "adc",
    "pac",
    "serial",
    "dma",
    "prelude",
    "rtic",
    "rtic_monotonics",
    "stm32f1xx_hal",
    "cortex_m",
    "cortex_m_rt",
    "core",
    "init",
    "idle",
    "idle_loop",
    "main",
    "app",
    "Mono",
    "pins",
    "exti0",
    "exti1",
    "exti2",
    "exti3",
    "exti4",
    "exti9_5",
    "exti15_10",
];

/// One task of the user's, as read from its signature and marker.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserTask {
    pub name: String,
    pub is_async: bool,
    /// An `async fn` ending in `-> !`: RTIC wants `Context<'static>` then.
    pub diverges: bool,
    pub binds: Option<String>,
    pub priority: u8,
    /// RTIC's arguments as written, minus `start`, `priority = 1` added when
    /// none is given.
    pub args: String,
    /// Bare names in `local = [..]`: generated Locals this task takes.
    pub claimed: Vec<String>,
    /// `name: T = e` entries in `local = [..]`: RTIC's own task locals.
    pub declared: Vec<String>,
    /// Names in `shared = [..]`, `&` taken off.
    pub shared: Vec<String>,
    pub start: bool,
    /// Its parameters after the context, verbatim (`by: u32`).
    pub inputs: Vec<String>,
    /// The project module it is written in (`tasks` for `src/tasks.rs`),
    /// `None` for main.rs: the block imports it from there into `mod app`.
    pub module: Option<String>,
}

/// Everything below (and above) the markers that shapes the app.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserApp {
    pub tasks: Vec<UserTask>,
    /// `struct AppShared`'s fields `(name, type)`.
    pub app_shared: Vec<(String, String)>,
    pub idle_shared: Vec<String>,
    /// What the scan refused, each already a full sentence.
    pub refusals: Vec<String>,
}

impl UserApp {
    /// Nothing that changes the generated app.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
            && self.app_shared.is_empty()
            && self.idle_shared.is_empty()
            && self.refusals.is_empty()
    }
}

/// How far an item's visibility reaches up the module tree.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Vis {
    /// Its own module only: no `pub`, `pub(self)`, `pub(in crate::m)`.
    Private,
    /// That many modules up: `pub(super)` 1, `pub(in super::super)` 2.
    Up(usize),
    /// The whole crate: `pub`, `pub(crate)`, `pub(in crate)`.
    Crate,
}

impl Vis {
    /// Whether the crate root - `mod app` - sees an item with this
    /// visibility declared in a module `depth` levels down (main.rs = 0).
    fn reaches_root(self, depth: usize) -> bool {
        match self {
            Vis::Crate => true,
            Vis::Up(n) => depth <= n,
            Vis::Private => depth == 0,
        }
    }
}

/// The visibility a `pub(..)`'s parentheses hold.
fn vis_of(inside: &str) -> Vis {
    let inside = inside.trim();
    let path = inside
        .strip_prefix("in ")
        .map_or(inside, str::trim_start)
        .replace(char::is_whitespace, "");
    let segments: Vec<&str> = path.split("::").collect();
    if path == "crate" {
        Vis::Crate
    } else if segments.iter().all(|s| *s == "super") {
        Vis::Up(segments.len())
    } else {
        Vis::Private
    }
}

/// A top-level item of the user's text that matters here.
#[derive(Debug)]
enum Item {
    Fn {
        name: String,
        is_async: bool,
        /// Whether `mod app` reaches it when it lives in a module of its own.
        vis: Vis,
        /// The signature as code, up to its body's `{`.
        sig: String,
    },
    /// A `use`, its path as written (`crate::app`).
    Use { path: String },
    Struct {
        name: String,
        /// The text between its braces, as code.
        body: String,
    },
}

/// One item, with where its first line starts in the file and the index of
/// that line in its region's raw lines.
struct Found {
    at: usize,
    line: usize,
    item: Item,
}

/// The top-level `fn`s and `struct`s of `file[from..to]`. Comments and
/// literals are read through [`code_only`], which keeps every line where it
/// was, so its lines index the raw ones.
fn items(file: &str, from: usize, to: usize) -> (Vec<&str>, Vec<Found>) {
    let raw: Vec<&str> = file[from..to].split('\n').collect();
    let code = code_only(&file[from..to]);
    let code: Vec<&str> = code.split('\n').collect();
    let mut starts = Vec::with_capacity(raw.len());
    let mut at = from;
    for l in &raw {
        starts.push(at);
        at += l.len() + 1;
    }
    let mut found = Vec::new();
    let mut depth = 0i32;
    for (i, line) in code.iter().enumerate() {
        if depth == 0
            && let Some(item) = item_at(&code[i..])
        {
            found.push(Found {
                at: starts[i],
                line: i,
                item,
            });
        }
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                // Never below the top: off RTIC the text below the markers
                // opens inside `fn main`, closed there by its `}`.
                '}' => depth = (depth - 1).max(0),
                _ => {}
            }
        }
    }
    (raw, found)
}

/// The item whose head is the first of `lines`, if it is a `fn` or a
/// `struct` (`pub`, `pub(..)`, `async`, `unsafe` before it).
fn item_at(lines: &[&str]) -> Option<Item> {
    let mut rest = lines.first()?.trim_start();
    let mut is_async = false;
    let mut vis = Vis::Private;
    loop {
        if rest.starts_with("#[") {
            // An attribute on the item's own line: past its balanced `]`.
            let mut depth = 0i32;
            let end = rest.char_indices().find_map(|(i, c)| match c {
                '[' => {
                    depth += 1;
                    None
                }
                ']' => {
                    depth -= 1;
                    (depth == 0).then_some(i)
                }
                _ => None,
            })?;
            rest = rest[end + 1..].trim_start();
        } else if let Some(r) = rest.strip_prefix("pub(") {
            let (inside, after) = r.split_once(')')?;
            vis = vis_of(inside);
            rest = after.trim_start();
        } else if let Some(r) = rest.strip_prefix("pub ") {
            vis = Vis::Crate;
            rest = r.trim_start();
        } else if let Some(r) = rest.strip_prefix("async ") {
            is_async = true;
            rest = r.trim_start();
        } else if let Some(r) = rest.strip_prefix("unsafe ") {
            rest = r.trim_start();
        } else {
            break;
        }
    }
    let ident = |s: &str| -> String {
        s.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect()
    };
    if let Some(r) = rest.strip_prefix("fn ") {
        let name = ident(r.trim_start());
        // The signature, over as many lines as it takes, up to its body - a
        // `{` or `;` outside its brackets: `buf: [u8; 4]` is a parameter.
        let mut sig = String::new();
        let mut depth = 0i32;
        'sig: for l in lines {
            for (i, c) in l.char_indices() {
                match c {
                    '(' | '[' => depth += 1,
                    ')' | ']' => depth -= 1,
                    '{' | ';' if depth <= 0 => {
                        sig.push_str(&l[..i]);
                        break 'sig;
                    }
                    _ => {}
                }
            }
            sig.push_str(l);
            sig.push(' ');
        }
        return (!name.is_empty()).then_some(Item::Fn {
            name,
            is_async,
            vis,
            sig,
        });
    }
    if let Some(r) = rest.strip_prefix("use ") {
        let path = r.split(';').next().unwrap_or_default().trim().to_owned();
        return (!path.is_empty()).then_some(Item::Use { path });
    }
    if let Some(r) = rest.strip_prefix("struct ") {
        let name = ident(r.trim_start());
        let text = lines.join("\n");
        let open = text.find('{')?;
        let mut depth = 0i32;
        for (i, c) in text[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(Item::Struct {
                            name,
                            body: text[open + 1..open + i].to_owned(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// `text` split at the commas outside any `()`, `[]`, `{}` or generic
/// `<>`, each part trimmed, empty ones dropped. A `<` opens only right after
/// a name or `::` (`Option<`, `Vec::<`), never as `<<` or `<=`; a `>` closes
/// only an open one and never as `->` - so `1 << 3` and `fn() -> u8` do not
/// swallow the commas after them.
pub fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let prev = chars[..i]
            .iter()
            .rev()
            .find(|c| !c.is_whitespace())
            .copied();
        let next = chars.get(i + 1).copied();
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '<' if prev.is_some_and(|p| p.is_alphanumeric() || p == '_' || p == ':')
                && !matches!(next, Some('<' | '=')) =>
            {
                angle += 1;
            }
            '>' if angle > 0 && chars.get(i.wrapping_sub(1)) != Some(&'-') => angle -= 1,
            ',' if depth == 0 && angle == 0 => {
                out.push(cur.trim().to_owned());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur.trim().to_owned());
    out.retain(|s| !s.is_empty());
    out
}

/// The text inside the parentheses that open `s`, or `None` when they do not
/// close on that line.
fn parenthesised(s: &str) -> Option<&str> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[1..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// For a `fn` named `name` with signature `sig`: whether its first parameter
/// is `app::<name>::Context` (also `crate::app::` / `self::app::`, with or
/// without a lifetime), its other parameters, whether it returns `!`, and
/// whether its context is `Context<'static>`.
fn task_signature(name: &str, sig: &str) -> Option<(Vec<String>, bool, bool)> {
    // After `fn <name>` - a `pub(crate)` before it has parentheses too.
    let after_name = sig.find(&format!("fn {name}"))? + 3 + name.len();
    let open = after_name + sig[after_name..].find('(')?;
    let params = parenthesised(&sig[open..])?;
    let after = &sig[open + params.len() + 2..];
    let params = split_args(params);
    let first = params.first()?;
    let (_, ty) = first.split_once(':')?;
    let ty: String = ty.chars().filter(|c| !c.is_whitespace()).collect();
    let ty = ty
        .strip_prefix("crate::")
        .or_else(|| ty.strip_prefix("self::"))
        .unwrap_or(&ty);
    let rest = ty.strip_prefix(&format!("app::{name}::Context"))?;
    if !(rest.is_empty() || rest.starts_with('<')) {
        return None;
    }
    let diverges = after
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .starts_with("->!");
    Some((params[1..].to_vec(), diverges, rest == "<'static>"))
}

/// A marker line: `// @<word>(..)`, spaces allowed after `//` and before the
/// `(` - its word, and the rest of the line from the `(`.
fn marker(line: &str) -> Option<(&str, &str)> {
    let m = line
        .trim()
        .strip_prefix("//")?
        .trim_start()
        .strip_prefix('@')?;
    let p = m.find('(')?;
    let word = m[..p].trim_end();
    (!word.is_empty() && word.chars().all(|c| c.is_ascii_alphabetic())).then(|| (word, &m[p..]))
}

/// Whether `word` reads as `task` or `idle` misspelt: at most two edits from
/// either, case aside - `tsak` and `Idle` are, `todo` and `see` are not.
fn near_marker_word(word: &str) -> bool {
    let w: Vec<char> = word.to_ascii_lowercase().chars().collect();
    ["task", "idle"].iter().any(|k| {
        let k: Vec<char> = k.chars().collect();
        let mut row: Vec<usize> = (0..=k.len()).collect();
        for (i, a) in w.iter().enumerate() {
            let mut prev = row[0];
            row[0] = i + 1;
            for (j, b) in k.iter().enumerate() {
                let cur = row[j + 1];
                row[j + 1] = (row[j + 1] + 1)
                    .min(row[j] + 1)
                    .min(prev + usize::from(a != b));
                prev = cur;
            }
        }
        row[k.len()] <= 2
    })
}

/// The marker of the item whose head is raw line `line`: the first line above
/// it that is not `///` or `#[..]`, when it is a `// @{kind}(..)`.
fn marker_above(raw: &[&str], line: usize, kind: &str) -> Option<usize> {
    raw[..line]
        .iter()
        .enumerate()
        .rev()
        .map(|(i, l)| (i, l.trim()))
        .find(|(_, l)| !(l.starts_with("///") || l.starts_with("#[")))
        .filter(|(_, l)| marker(l).is_some_and(|(w, _)| w == kind))
        .map(|(i, _)| i)
}

/// What one `// @task(..)` holds.
#[derive(Default)]
struct Args {
    rtic: Vec<String>,
    priority: Option<u8>,
    binds: Option<String>,
    claimed: Vec<String>,
    declared: Vec<String>,
    shared: Vec<String>,
    /// `shared = [..]` as written, `&` kept: what `#[idle]` is given.
    shared_raw: Vec<String>,
    start: bool,
    local_task: bool,
}

fn parse_args(text: &str) -> Result<Args, String> {
    let mut a = Args::default();
    for entry in split_args(text) {
        if entry == "start" {
            a.start = true;
            continue;
        }
        if entry == "local_task" {
            a.local_task = true;
            a.rtic.push(entry);
            continue;
        }
        let (key, value) = entry
            .split_once('=')
            .map(|(k, v)| (k.trim(), v.trim()))
            .ok_or_else(|| format!("`{entry}` is no `key = value` RTIC knows"))?;
        let list = || {
            value
                .strip_prefix('[')
                .and_then(|v| v.strip_suffix(']'))
                .map(split_args)
                .ok_or_else(|| format!("`{key}` takes a list, `{key} = [..]`"))
        };
        match key {
            "priority" => {
                a.priority = Some(
                    value
                        .parse()
                        .map_err(|_| format!("`priority = {value}` is no number"))?,
                );
            }
            "binds" => a.binds = Some(value.to_owned()),
            "local" => {
                for l in list()? {
                    match l.split_once(':') {
                        Some((n, _)) => a.declared.push(n.trim().to_owned()),
                        None => a.claimed.push(l),
                    }
                }
            }
            "shared" => {
                for s in list()? {
                    a.shared.push(s.trim_start_matches('&').trim().to_owned());
                    a.shared_raw.push(s);
                }
            }
            "local_task" => a.local_task = value == "true",
            _ => {}
        }
        a.rtic.push(entry);
    }
    if a.priority.is_none() {
        a.rtic.insert(0, "priority = 1".to_owned());
    }
    Ok(a)
}

/// Read the user's tasks off `existing` - its text above and below the
/// markers. Syntax-level refusals are decided here; the ones that need the
/// app's resources, in [`plan`].
pub fn scan(existing: &str) -> UserApp {
    scan_project(existing, &[])
}

/// [`scan`], and the project's own `modules` main.rs declares too
/// ([`task_modules`]): tasks only there - `idle_loop`'s marker and
/// `AppShared` are main.rs's.
pub fn scan_project(existing: &str, modules: &[(String, String)]) -> UserApp {
    let mut app = UserApp::default();
    let mut used_markers: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut app_shared_fn = false;
    let mut regions = Vec::new();
    let mut sources: Vec<(Option<&str>, &str, (usize, usize))> = user_spans(existing)
        .into_iter()
        .map(|span| (None, existing, span))
        .collect();
    for (name, text) in modules {
        sources.push((Some(name.as_str()), text.as_str(), (0, text.len())));
    }
    for (r, &(module, file, (from, to))) in sources.iter().enumerate() {
        let (raw, found) = items(file, from, to);
        for f in &found {
            match &f.item {
                Item::Fn { name, .. } if module.is_none() && name == "app_shared" => {
                    app_shared_fn = true;
                }
                Item::Fn { name, .. }
                    if module.is_none() && name == super::common::RTIC_IDLE_FN =>
                {
                    if let Some(m) = marker_above(&raw, f.line, "idle") {
                        used_markers.insert((r, m));
                        let text = marker(raw[m]).map_or("", |(_, rest)| rest);
                        match parenthesised(text).map(parse_args) {
                            Some(Ok(a)) => app.idle_shared.extend(a.shared_raw),
                            Some(Err(why)) => app.refusals.push(format!("RTIC idle: {why}")),
                            None => app
                                .refusals
                                .push("RTIC idle: its `// @idle(` is not closed".to_owned()),
                        }
                    }
                }
                Item::Fn {
                    name,
                    is_async,
                    vis,
                    sig,
                } => {
                    let Some((inputs, diverges, is_static)) = task_signature(name, sig) else {
                        continue;
                    };
                    let above = marker_above(&raw, f.line, "task");
                    let args = match above {
                        Some(m) => {
                            used_markers.insert((r, m));
                            let text = marker(raw[m]).map_or("", |(_, rest)| rest);
                            match parenthesised(text) {
                                Some(t) => parse_args(t),
                                None => Err("its `// @task(` is not closed".to_owned()),
                            }
                        }
                        None => parse_args(""),
                    };
                    let args = args.and_then(|a| {
                        if let Some(m) = module
                            && !vis.reaches_root(m.split("::").count())
                        {
                            Err(format!(
                                "in `{m}` it has to be `pub` - the app calls it from `mod app`"
                            ))
                        } else if *is_async && is_static && !diverges {
                            Err(
                                "`Context<'static>` is for a task that never returns - end it \
                                 with `-> !` around a loop, or take `Context<'_>`"
                                    .to_owned(),
                            )
                        } else {
                            Ok(a)
                        }
                    });
                    match args.and_then(|a| task(name, *is_async, diverges, inputs, a)) {
                        Ok(t) => app.tasks.push(UserTask {
                            module: module.map(str::to_owned),
                            ..t
                        }),
                        Err(why) => app.refusals.push(format!("RTIC task `{name}`: {why}")),
                    }
                }
                Item::Struct { name, body } if module.is_none() && name == "AppShared" => {
                    for field in split_args(body) {
                        // Past its attributes and its `pub` / `pub(..)`.
                        let mut f = field.trim();
                        while let Some(r) = f.strip_prefix("#[") {
                            f = r.split_once(']').map_or("", |(_, r)| r.trim_start());
                        }
                        if let Some(r) = f.strip_prefix("pub(") {
                            f = r.split_once(')').map_or("", |(_, r)| r.trim_start());
                        } else if let Some(r) = f.strip_prefix("pub ") {
                            f = r.trim_start();
                        }
                        match f.split_once(':') {
                            Some((n, ty))
                                if !n.trim().is_empty()
                                    && n.trim()
                                        .chars()
                                        .all(|c| c.is_alphanumeric() || c == '_') =>
                            {
                                app.app_shared
                                    .push((n.trim().to_owned(), ty.trim().to_owned()));
                            }
                            _ => app.refusals.push(format!(
                                "RTIC shared: `AppShared`'s field `{field}` is not read - write \
                                 it `name: Type`"
                            )),
                        }
                    }
                }
                Item::Struct { .. } | Item::Use { .. } => {}
            }
        }
        regions.push(raw);
    }
    if !app.app_shared.is_empty() && !app_shared_fn {
        app.refusals.push(
            "RTIC shared: `struct AppShared` needs `fn app_shared() -> AppShared` beside it, \
             which `#[init]` calls for the shared values"
                .to_owned(),
        );
        app.app_shared.clear();
    }
    // A marker that declares nothing - above no task, or misspelt - is said:
    // silence would read as a task that does not run.
    for (r, raw) in regions.iter().enumerate() {
        let mut depth = 0i32;
        let code = code_only(&raw.join("\n"));
        // Each marker-shaped line swapped for a sentinel, which survives
        // `code_only` only where the line is code - not inside `/* */`.
        let probe: Vec<&str> = raw
            .iter()
            .map(|l| if marker(l).is_some() { "\u{1}" } else { *l })
            .collect();
        let live = code_only(&probe.join("\n"));
        let live: Vec<&str> = live.split('\n').collect();
        for (i, (l, c)) in raw.iter().zip(code.split('\n')).enumerate() {
            let t = l.trim();
            // `// @task(..)`, `// @idle(..)` - or one of them misspelt.
            let stray = marker(t).is_some_and(|(w, _)| near_marker_word(w))
                && live.get(i).is_some_and(|c| c.contains('\u{1}'));
            if depth == 0 && stray && !used_markers.contains(&(r, i)) {
                app.refusals.push(format!(
                    "RTIC: `{t}` is above no task - a task is a top-level fn whose first \
                     parameter is `app::<its name>::Context`, and `// @idle(..)` goes right \
                     above `fn idle_loop`"
                ));
            }
            for ch in c.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth = (depth - 1).max(0),
                    _ => {}
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut vectors = BTreeSet::new();
    app.tasks.retain(|t| {
        let dup_name = !seen.insert(t.name.clone());
        if dup_name {
            app.refusals.push(format!(
                "RTIC task `{}`{}: another task has this name already - RTIC names each \
                 task's `app::<name>` module after it, so rename one fn",
                t.name,
                t.module
                    .as_deref()
                    .map(|m| format!(" in `{m}`"))
                    .unwrap_or_default()
            ));
        }
        let dup_vector = t.binds.as_ref().is_some_and(|v| !vectors.insert(v.clone()));
        if dup_vector {
            app.refusals.push(format!(
                "RTIC task `{}`: `binds = {}` is bound by another task already",
                t.name,
                t.binds.as_deref().unwrap_or_default()
            ));
        }
        !dup_name && !dup_vector
    });
    app
}

/// One task from its parts, or why it cannot be declared.
fn task(
    name: &str,
    is_async: bool,
    diverges: bool,
    inputs: Vec<String>,
    a: Args,
) -> Result<UserTask, String> {
    if RESERVED.contains(&name) {
        return Err("the generated app uses this name already - rename the fn".to_owned());
    }
    let priority = a.priority.unwrap_or(1);
    match (&a.binds, is_async) {
        (None, false) => {
            return Err(
                "a plain `fn` is a hardware task and needs `binds = <interrupt>` in its \
                 `// @task(..)`; a software task is an `async fn`"
                    .to_owned(),
            );
        }
        (Some(_), true) => {
            return Err("an `async fn` is a software task: `binds` is for a plain `fn`".to_owned());
        }
        (Some(v), false) if EXCEPTIONS.contains(&v.as_str()) => {
            return Err(format!(
                "`binds = {v}` is a core exception{}",
                if v == "SysTick" {
                    ", and SysTick drives the generated `Mono`"
                } else {
                    ""
                }
            ));
        }
        (Some(_), false) if !inputs.is_empty() => {
            return Err("a hardware task takes its context alone".to_owned());
        }
        _ => {}
    }
    if priority == 0 {
        return Err(
            "priority 0 is idle's level, and the app's `#[idle]` is generated - use 1 or more"
                .to_owned(),
        );
    }
    if a.start && a.local_task {
        return Err(
            "`start` spawns it from `#[init]`, and a `local_task` is spawned only from its own \
             executor's tasks"
                .to_owned(),
        );
    }
    if priority > 16 {
        return Err("the F1's NVIC has 16 priority levels".to_owned());
    }
    if a.start && (!is_async || !inputs.is_empty()) {
        return Err(
            "`start` spawns it in `#[init]` with no arguments: a software task with none"
                .to_owned(),
        );
    }
    Ok(UserTask {
        name: name.to_owned(),
        is_async,
        diverges,
        binds: a.binds,
        priority,
        args: a.rtic.join(", "),
        claimed: a.claimed,
        declared: a.declared,
        shared: a.shared,
        start: a.start,
        inputs,
        module: None,
    })
}

/// What the block makes of the user's app.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// `dispatchers = [..]`, empty for none.
    pub dispatchers: Vec<String>,
    /// Generated Locals moved to `#[shared]`, in `Local` order, with types.
    pub promoted: Vec<(String, String)>,
    /// `AppShared`'s fields, after the promoted ones.
    pub app_shared: Vec<(String, String)>,
    /// Locals `#[idle]` no longer takes: claimed by a task, or shared.
    pub idle_drop: BTreeSet<String>,
    /// What `#[idle]` locks: `// @idle(shared = [..])`.
    pub idle_shared: Vec<String>,
    /// The readout and `extern "Rust"` block, inside `mod app`.
    pub extern_block: String,
    /// `<task>::spawn().ok();` lines for `#[init]`.
    pub spawns: String,
    /// `compile_error!` items, after `mod app`.
    pub errors: String,
}

impl Plan {
    /// Whether `#[shared]` has fields - and the crate root needs
    /// `rtic::Mutex` for `.lock()` in the bodies below the markers.
    pub fn has_shared(&self) -> bool {
        !self.promoted.is_empty() || !self.app_shared.is_empty()
    }
}

/// The app for `user` over the generated `locals` (`(name, type)` in `Local`
/// order). `held` are the Locals an armed pin's task takes, `bound` the
/// vectors those tasks bind.
pub fn plan(
    user: &UserApp,
    locals: &[(String, String)],
    held: &BTreeSet<String>,
    bound: &BTreeSet<String>,
) -> Plan {
    let mut errors: Vec<String> = user.refusals.clone();
    let names = || locals.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>();
    // `&x` and `x` name one resource.
    let bare = |s: &str| s.trim_start_matches('&').trim().to_owned();
    // An `AppShared` field named like a generated resource would be a second
    // `#[shared]` field of that name.
    let app_shared: Vec<(String, String)> = user
        .app_shared
        .iter()
        .filter(|(f, _)| {
            let clash = locals.iter().any(|(l, _)| l == f);
            if clash {
                errors.push(format!(
                    "RTIC shared: `AppShared`'s field `{f}` is named like a generated resource - \
                     rename it"
                ));
            }
            !clash
        })
        .cloned()
        .collect();
    let shared_ok =
        |n: &str| locals.iter().any(|(l, _)| l == n) || app_shared.iter().any(|(f, _)| f == n);
    // Who claims what, and what is shared by anyone (idle included).
    let idle_bare: Vec<String> = user.idle_shared.iter().map(|s| bare(s)).collect();
    let mut all_shared: BTreeSet<&str> = idle_bare.iter().map(String::as_str).collect();
    for t in &user.tasks {
        all_shared.extend(t.shared.iter().map(String::as_str));
    }
    let mut owner: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    let mut tasks: Vec<&UserTask> = Vec::new();
    'task: for t in &user.tasks {
        let refuse = |why: String| format!("RTIC task `{}`: {why}", t.name);
        if let Some(v) = &t.binds
            && bound.contains(v)
        {
            errors.push(refuse(format!(
                "`binds = {v}` is an armed pin's EXTI vector, generated here - that pin's code \
                 goes in its `on_<pad>_in_edge` hook"
            )));
            continue;
        }
        if let Some(v) = &t.binds
            && F1_DISPATCHERS.contains(&v.as_str())
            && user.tasks.iter().any(|o| o.is_async)
        {
            errors.push(refuse(format!(
                "`binds = {v}`: the software tasks' dispatchers run on {}, in that order - \
                 bind another interrupt",
                F1_DISPATCHERS.join(", ")
            )));
            continue;
        }
        for c in &t.claimed {
            let why = if !locals.iter().any(|(n, _)| n == c) {
                format!(
                    "`local = [{c}]` names no resource - the Local resources are: {}",
                    names().join(", ")
                )
            } else if held.contains(c) {
                format!("`{c}` belongs to its pin's EXTI task - its code goes in the pin's hook")
            } else if all_shared.contains(c.as_str()) {
                format!("`{c}` is shared by a `shared = [..]` - one or the other")
            } else if let Some(o) = owner.get(c.as_str()) {
                format!("`{c}` is claimed by task `{o}` already - a Local has one owner")
            } else {
                continue;
            };
            errors.push(refuse(why));
            continue 'task;
        }
        for d in &t.declared {
            if locals.iter().any(|(n, _)| n == d) {
                errors.push(refuse(format!(
                    "its own local `{d}` is named like a generated resource - rename it"
                )));
                continue 'task;
            }
        }
        for s in &t.shared {
            let why = if !shared_ok(s) {
                format!(
                    "`shared = [{s}]` names nothing shared - the generated resources are: {}{}",
                    names().join(", "),
                    if app_shared.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; `AppShared`'s: {}",
                            app_shared
                                .iter()
                                .map(|(n, _)| n.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                )
            } else if held.contains(s) {
                format!("`{s}` belongs to its pin's EXTI task - it cannot be shared")
            } else {
                continue;
            };
            errors.push(refuse(why));
            continue 'task;
        }
        for c in &t.claimed {
            owner.insert(c, &t.name);
        }
        tasks.push(t);
    }
    // What idle locks must exist too.
    let mut idle_shared = Vec::new();
    for s in &user.idle_shared {
        let b = bare(s);
        if shared_ok(&b) && !held.contains(&b) {
            idle_shared.push(s.clone());
        } else {
            errors.push(format!(
                "RTIC idle: `shared = [{s}]` names nothing it can share - the generated \
                 resources are: {}",
                names().join(", ")
            ));
        }
    }
    let shared_names: BTreeSet<&str> = tasks
        .iter()
        .flat_map(|t| t.shared.iter().map(String::as_str))
        .chain(
            idle_bare
                .iter()
                .map(String::as_str)
                .filter(|n| shared_ok(n)),
        )
        .collect();
    let promoted: Vec<(String, String)> = locals
        .iter()
        .filter(|(n, _)| shared_names.contains(n.as_str()))
        .cloned()
        .collect();
    let idle_drop: BTreeSet<String> = promoted
        .iter()
        .map(|(n, _)| n.clone())
        .chain(tasks.iter().flat_map(|t| t.claimed.iter().cloned()))
        .collect();
    // One dispatcher per distinct software priority, from the pool minus
    // every vector a task binds - the list grows and shrinks at its tail.
    let priorities: BTreeSet<u8> = tasks
        .iter()
        .filter(|t| t.is_async)
        .map(|t| t.priority)
        .collect();
    let taken: BTreeSet<&str> = tasks.iter().filter_map(|t| t.binds.as_deref()).collect();
    let pool: Vec<String> = F1_DISPATCHERS
        .iter()
        .filter(|v| !taken.contains(**v) && !bound.contains(**v))
        .map(|v| (*v).to_owned())
        .collect();
    if priorities.len() > pool.len() {
        errors.push(format!(
            "RTIC: {} software priority levels, and {} free dispatcher interrupts ({}) - put \
             tasks on fewer levels",
            priorities.len(),
            pool.len(),
            pool.join(", ")
        ));
    }
    let dispatchers: Vec<String> = pool.into_iter().take(priorities.len()).collect();

    let mut extern_block = String::new();
    if !tasks.is_empty() {
        extern_block.push_str(
            "\n    // Your tasks, declared from their signatures below the markers\n\
             \x20   // (`fn name(cx: app::name::Context..)`, `// @task(..)` right above it):\n",
        );
        for t in &tasks {
            let mut what = match &t.binds {
                Some(v) => format!("hardware, {v}, priority {}", t.priority),
                None => format!("software, priority {}", t.priority),
            };
            if t.start {
                what.push_str(", started in #[init]");
            }
            if !t.claimed.is_empty() {
                what.push_str(&format!(", local: {}", t.claimed.join(", ")));
            }
            if !t.shared.is_empty() {
                what.push_str(&format!(", shared: {}", t.shared.join(", ")));
            }
            let at = t
                .module
                .as_ref()
                .map_or(String::new(), |m| format!(" (in {m})"));
            extern_block.push_str(&format!("    //   {}{at}: {what}\n", t.name));
        }
        if !dispatchers.is_empty() {
            extern_block.push_str(&format!(
                "    // Dispatchers, one per software priority: {}.\n",
                dispatchers.join(", ")
            ));
        }
        // A task written in a module of the project's: brought into
        // `mod app`, where RTIC calls it by its name.
        for t in &tasks {
            if let Some(m) = &t.module {
                extern_block.push_str(&format!("    use crate::{m}::{};\n", t.name));
            }
        }
        extern_block.push_str("    extern \"Rust\" {\n");
        for t in &tasks {
            let inputs: String = t.inputs.iter().map(|i| format!(", {i}")).collect();
            let decl = match (t.is_async, t.diverges) {
                (true, false) => format!("async fn {n}(_: {n}::Context<'_>{inputs});", n = t.name),
                (true, true) => format!(
                    "async fn {n}(_: {n}::Context<'static>{inputs}) -> !;",
                    n = t.name
                ),
                (false, _) => format!("fn {n}(_: {n}::Context);", n = t.name),
            };
            extern_block.push_str(&format!("        #[task({})]\n        {decl}\n", t.args));
        }
        extern_block.push_str("    }\n");
    }
    let started: Vec<&&UserTask> = tasks.iter().filter(|t| t.start).collect();
    let spawns = if started.is_empty() {
        String::new()
    } else {
        let mut s = "        // Your tasks marked `start` below the markers.\n".to_owned();
        for t in started {
            s.push_str(&format!("        {}::spawn().ok();\n", t.name));
        }
        s
    };
    let errors: String = errors
        .iter()
        .map(|e| format!("compile_error!({e:?});\n"))
        .collect();
    Plan {
        dispatchers,
        promoted,
        app_shared,
        idle_drop,
        idle_shared,
        extern_block,
        spawns,
        errors,
    }
}

/// The project's own modules the RTIC tasks are read off besides main.rs
/// ([`scan_project`]): each `mod name;` main.rs declares, and each one those
/// files declare in turn (`a::b` in `src/a/b.rs`) - `(path, text)` of the
/// files among `files` (`(path, text)`, paths `src/..`). Not `pins`, which
/// the IDE writes; see [`modules_left_out`] for the ones passed over.
pub fn task_modules(main_rs: &str, files: &[(String, String)]) -> Vec<(String, String)> {
    walk_modules(main_rs, files).0
}

/// The modules [`task_modules`] passes over although a task may be in them,
/// one line each for the System tab: a `mod` under `#[cfg]` / `#[path]`, and
/// a nested one `mod app` cannot reach.
pub fn modules_left_out(main_rs: &str, files: &[(String, String)]) -> Vec<String> {
    walk_modules(main_rs, files).1
}

/// The two files a module path can live in: `a::b` -> `src/a/b.rs`,
/// `src/a/b/mod.rs`.
pub fn module_paths(module: &str) -> [String; 2] {
    let dir = module.replace("::", "/");
    [format!("src/{dir}.rs"), format!("src/{dir}/mod.rs")]
}

fn walk_modules(main_rs: &str, files: &[(String, String)]) -> (Vec<(String, String)>, Vec<String>) {
    let mut read: Vec<(String, String)> = Vec::new();
    let mut left: Vec<String> = Vec::new();
    let file_of = |module: &str| {
        let paths = module_paths(module);
        files
            .iter()
            .find(|(p, _)| paths.contains(p))
            .map(|(_, text)| text.clone())
    };
    // Whether a module, or one below it, would make a task: one there or
    // one refused there - by the files' own paths, as it is not walked.
    let holds_tasks = |module: &str| {
        files.iter().any(|(path, text)| {
            let Some(name) = path
                .strip_prefix("src/")
                .and_then(|p| p.strip_suffix(".rs"))
                .map(|p| p.strip_suffix("/mod").unwrap_or(p).replace('/', "::"))
            else {
                return false;
            };
            (name == module || name.starts_with(&format!("{module}::"))) && {
                let app = scan_project("", &[(name, text.clone())]);
                !app.tasks.is_empty() || !app.refusals.is_empty()
            }
        })
    };
    // The module the text is (none: main.rs), the text, the spans to read.
    type Pending = (Option<String>, String, Vec<(usize, usize)>);
    let mut queue: Vec<Pending> = vec![(None, main_rs.to_owned(), user_spans(main_rs).to_vec())];
    while let Some((parent, text, spans)) = queue.pop() {
        let depth = parent.as_deref().map_or(0, |p| p.split("::").count());
        for (from, to) in spans {
            let code = attributes_on_one_line(&code_only(&text[from..to]));
            let mut depth_braces = 0i32;
            // A `#[cfg]` or `#[path]` over a `mod` makes which file it is, and
            // whether it is built, a question this text cannot settle.
            let mut guessed = false;
            for line in code.lines() {
                let t = line.trim();
                let (attributes, item) = leading_attributes(t);
                if depth_braces == 0
                    && attributes
                        .iter()
                        .any(|a| a.starts_with("#[cfg") || a.starts_with("#[path"))
                {
                    guessed = true;
                }
                let attributed = guessed;
                if !item.is_empty() {
                    guessed = false;
                }
                if depth_braces == 0
                    && let Some((vis, name)) = mod_decl(item)
                    && !(parent.is_none() && name == "pins")
                    && !read.iter().any(|(n, _)| {
                        *n == parent
                            .as_ref()
                            .map_or(name.to_owned(), |p| format!("{p}::{name}"))
                    })
                {
                    let module = match &parent {
                        Some(p) => format!("{p}::{name}"),
                        None => name.to_owned(),
                    };
                    let found = file_of(&module);
                    // A `#![cfg(..)]` atop the module's own file gates it too.
                    let gated = found.as_deref().is_some_and(|f| {
                        attributes_on_one_line(&code_only(f))
                            .lines()
                            .any(|l| l.trim_start().starts_with("#![cfg"))
                    });
                    if attributed || gated {
                        // `#[path]` points elsewhere; a gated module with no
                        // task in or below it is nothing to report.
                        if found.is_none() || holds_tasks(&module) {
                            left.push(format!(
                                "mod {module}: under #[cfg] or #[path] - its tasks are not read"
                            ));
                        }
                    } else if let Some(found) = found {
                        if !vis.reaches_root(depth) {
                            if holds_tasks(&module) {
                                let at = parent.as_deref().unwrap_or("main.rs");
                                left.push(format!(
                                    "mod {module}: private to {at} - mod app cannot reach a task \
                                     in or below it; declare it pub mod {name};"
                                ));
                            }
                        } else {
                            // No depth cap: each level's path is longer, and
                            // only a file of the project's can be one.
                            let len = found.len();
                            queue.push((Some(module.clone()), found.clone(), vec![(0, len)]));
                            read.push((module, found));
                        }
                    }
                }
                for c in line.chars() {
                    match c {
                        '{' => depth_braces += 1,
                        '}' => depth_braces = (depth_braces - 1).max(0),
                        _ => {}
                    }
                }
            }
        }
    }
    (read, left)
}

/// `line`'s leading attributes, and the rest of it past them:
/// `#[allow(x)] #[path = "x.rs"] mod x;`. An attribute left open is the rest.
fn leading_attributes(line: &str) -> (Vec<&str>, &str) {
    let mut attributes = Vec::new();
    let mut rest = line;
    while rest.starts_with("#[") {
        let mut depth = 0i32;
        let Some(end) = rest.char_indices().find_map(|(i, c)| match c {
            '[' => {
                depth += 1;
                None
            }
            ']' => {
                depth -= 1;
                (depth == 0).then_some(i)
            }
            _ => None,
        }) else {
            break;
        };
        attributes.push(&rest[..=end]);
        rest = rest[end + 1..].trim_start();
    }
    (attributes, rest)
}

/// `code` (as [`code_only`] gives it) with every attribute rustfmt wrapped
/// over several lines - `#[cfg(any(\n feature = "a",\n ..))]` - on one.
fn attributes_on_one_line(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut depth = 0usize;
    for c in code.chars() {
        match c {
            '[' if depth > 0 || out.ends_with('#') || out.ends_with("#!") => depth += 1,
            ']' if depth > 0 => depth -= 1,
            '\n' if depth > 0 => {
                out.push(' ');
                continue;
            }
            _ => {}
        }
        out.push(c);
    }
    out
}

/// The visibility and the name of the module a `mod name;` line declares.
fn mod_decl(line: &str) -> Option<(Vis, &str)> {
    let mut rest = line;
    let mut vis = Vis::Private;
    if let Some(r) = rest.strip_prefix("pub(") {
        let (inside, after) = r.split_once(')')?;
        vis = vis_of(inside);
        rest = after.trim_start();
    } else if let Some(r) = rest.strip_prefix("pub ") {
        vis = Vis::Crate;
        rest = r.trim_start();
    }
    let rest = rest.strip_prefix("mod ")?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let (name, tail) = rest.split_at(end);
    (!name.is_empty() && tail.trim() == ";").then_some((vis, name))
}

/// What the System tab shows of the user's tasks: read back off the block
/// main.rs holds - its readout, its dispatchers line, its `compile_error!`s -
/// so it is what was generated, not a second guess at it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TasksView {
    /// One per declared task: `blink: software, priority 2, local: pc13_out`.
    pub tasks: Vec<String>,
    /// `RCC, FLASH`, when there are software tasks.
    pub dispatchers: Option<String>,
    /// Each `compile_error!`'s message.
    pub errors: Vec<String>,
    /// [`modules_left_out`] - not in the block, filled by the caller.
    pub left_out: Vec<String>,
}

/// [`TasksView`] of `main_rs`'s generated block.
pub fn view(main_rs: &str) -> TasksView {
    let mut v = TasksView::default();
    let block = match (
        main_rs.find(super::common::GEN_BEGIN),
        main_rs.find(super::common::GEN_END),
    ) {
        (Some(b), Some(e)) if b < e => &main_rs[b..e],
        _ => return v,
    };
    let mut in_readout = false;
    for line in block.lines() {
        let t = line.trim();
        if t.starts_with("// Your tasks, declared from their signatures") {
            in_readout = true;
        } else if in_readout && let Some(task) = t.strip_prefix("//   ") {
            v.tasks.push(task.to_owned());
        } else if let Some(d) = t.strip_prefix("// Dispatchers, one per software priority: ") {
            v.dispatchers = Some(d.trim_end_matches('.').to_owned());
            in_readout = false;
        } else if t.starts_with("extern \"Rust\"") {
            in_readout = false;
        } else if let Some(msg) = t
            .strip_prefix("compile_error!(\"")
            .and_then(|m| m.strip_suffix("\");"))
        {
            // Written with `{:?}`: undo its escapes.
            v.errors
                .push(msg.replace("\\\"", "\"").replace("\\\\", "\\"));
        }
    }
    v
}

/// The start of the line of every one of the user's RTIC items in `file`:
/// task fns, `struct AppShared`, `fn app_shared`.
///
/// `AppShared` and `app_shared` only beside a task, or already fenced: in a
/// project that never used RTIC they are just the user's own names.
fn task_items_at(file: &str) -> Vec<usize> {
    let mut tasks = Vec::new();
    let mut shared = Vec::new();
    for (from, to) in user_spans(file) {
        let (_, found) = items(file, from, to);
        for f in found {
            match &f.item {
                Item::Fn { name, sig, .. } if task_signature(name, sig).is_some() => {
                    tasks.push(f.at);
                }
                Item::Fn { name, .. } if name == "app_shared" => shared.push(f.at),
                Item::Struct { name, .. } if name == "AppShared" => shared.push(f.at),
                // What a task's module imports to write one - `app`, `Mono`,
                // the monotonic's prelude - exists on RTIC alone.
                Item::Use { path } if rtic_only_use(path) => shared.push(f.at),
                _ => {}
            }
        }
    }
    let fenced = |at: &usize| file[..*at].ends_with(RTIC_TASK_FENCE);
    if !tasks.is_empty() || shared.iter().any(fenced) {
        tasks.extend(shared);
    }
    tasks.sort_unstable();
    tasks
}

/// Whether a `use` names what only the RTIC runtime has: its `app`, the
/// generated `Mono`, the `rtic` crates.
fn rtic_only_use(path: &str) -> bool {
    let segments: Vec<&str> = path.split("::").map(str::trim).collect();
    if segments.len() > 1 && matches!(segments[0], "rtic" | "rtic_monotonics") {
        return true;
    }
    // Up to the crate root - `crate::`, or `super::` as often as the
    // module is deep - then `app` or `Mono`.
    let ups = match segments.first() {
        Some(&"crate") => 1,
        _ => segments.iter().take_while(|s| **s == "super").count(),
    };
    let Some(next) = segments.get(ups).filter(|_| ups > 0) else {
        return false;
    };
    let name: String = next
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    name == "app" || name == "Mono"
}

/// Off RTIC, [`RTIC_TASK_FENCE`] above every one of the user's RTIC items;
/// back on RTIC, taken off again - so a round trip gives the same file back.
/// Nothing is ever removed: every line of them is the user's.
pub fn fence_tasks(mut file: String, rtic: bool) -> String {
    for at in task_items_at(&file).into_iter().rev() {
        let fenced = file[..at].ends_with(RTIC_TASK_FENCE);
        if rtic && fenced {
            file.replace_range(at - RTIC_TASK_FENCE.len()..at, "");
        } else if !rtic && !fenced {
            file.insert_str(at, RTIC_TASK_FENCE);
        }
    }
    file
}

/// What a user writes below the markers of an F103 RTIC project wired with
/// PC13 an output and USART1: a shared counter, a task started in `#[init]`
/// that takes the LED, one that shares the counter and the UART, a hardware
/// task on a vector nothing else binds. The unit tests and the `EIDE_F1_TASKS`
/// cross-compile both use it, so the matrix compiles what they assert on.
#[cfg(test)]
pub(crate) const EXAMPLE_TASKS: &str = concat!(
    "\n",
    "/// Values several tasks share: one RTIC `#[shared]` resource per field.\n",
    "struct AppShared {\n",
    "    count: u32,\n",
    "}\n",
    "\n",
    "fn app_shared() -> AppShared {\n",
    "    AppShared { count: 0 }\n",
    "}\n",
    "\n",
    "// @task(priority = 2, local = [pc13_out], start)\n",
    "async fn blink(cx: app::blink::Context<'_>) {\n",
    "    loop {\n",
    "        cx.local.pc13_out.toggle();\n",
    "        Mono::delay(500.millis()).await;\n",
    "    }\n",
    "}\n",
    "\n",
    "// @task(priority = 3, shared = [count, serial1])\n",
    "async fn report(mut cx: app::report::Context<'_>, by: u32) {\n",
    "    let n = cx.shared.count.lock(|c| {\n",
    "        *c += by;\n",
    "        *c\n",
    "    });\n",
    "    cx.shared.serial1.lock(|s| {\n",
    "        let _ = (s, n);\n",
    "    });\n",
    "}\n",
    "\n",
    "// @task(binds = TIM3, priority = 4)\n",
    "fn tick(_cx: app::tick::Context) {}\n",
);

/// [`EXAMPLE_TASKS`] split across files: `blink` and `tick` in
/// `src/tasks.rs` ([`EXAMPLE_MODULE`]), the rest below main.rs's markers with
/// `mod tasks;` - the `EIDE_F1_TASKS=module` cross-compile.
#[cfg(test)]
pub(crate) const EXAMPLE_MAIN_WITH_MODULE: &str = concat!(
    "\n",
    "mod tasks;\n",
    "\n",
    "/// Values several tasks share: one RTIC `#[shared]` resource per field.\n",
    "struct AppShared {\n",
    "    count: u32,\n",
    "}\n",
    "\n",
    "fn app_shared() -> AppShared {\n",
    "    AppShared { count: 0 }\n",
    "}\n",
    "\n",
    "// @task(priority = 3, shared = [count, serial1])\n",
    "async fn report(mut cx: app::report::Context<'_>, by: u32) {\n",
    "    let n = cx.shared.count.lock(|c| {\n",
    "        *c += by;\n",
    "        *c\n",
    "    });\n",
    "    cx.shared.serial1.lock(|s| {\n",
    "        let _ = (s, n);\n",
    "    });\n",
    "}\n",
);

/// `src/tasks.rs` of [`EXAMPLE_MAIN_WITH_MODULE`].
#[cfg(test)]
pub(crate) const EXAMPLE_MODULE: &str = concat!(
    "//! Tasks in a module of their own: the generated block imports them\n",
    "//! into `mod app`, where RTIC calls them.\n",
    "use crate::Mono;\n",
    "use crate::app;\n",
    "use rtic_monotonics::systick::prelude::*;\n",
    "\n",
    "// @task(priority = 2, local = [pc13_out], start)\n",
    "pub async fn blink(cx: app::blink::Context<'_>) {\n",
    "    loop {\n",
    "        cx.local.pc13_out.toggle();\n",
    "        Mono::delay(500.millis()).await;\n",
    "    }\n",
    "}\n",
    "\n",
    "// @task(binds = TIM3, priority = 4)\n",
    "pub fn tick(_cx: app::tick::Context) {}\n",
);

/// `src/tasks/hw.rs` of the `EIDE_F1_TASKS=nested` cross-compile:
/// [`EXAMPLE_MODULE`]'s hardware task one module deeper, `pub mod hw;` in
/// `src/tasks.rs` in its place.
#[cfg(test)]
pub(crate) const EXAMPLE_NESTED: &str = concat!(
    "use crate::app;\n",
    "\n",
    "// @task(binds = TIM3, priority = 4)\n",
    "pub fn tick(_cx: app::tick::Context) {}\n",
);

/// [`EXAMPLE_MODULE`] with its `tick` moved to [`EXAMPLE_NESTED`].
#[cfg(test)]
pub(crate) fn example_module_nested() -> String {
    let tick = "\n// @task(binds = TIM3, priority = 4)\npub fn tick(_cx: app::tick::Context) {}\n";
    assert!(EXAMPLE_MODULE.ends_with(tick));
    format!(
        "{}\npub mod hw;\n",
        &EXAMPLE_MODULE[..EXAMPLE_MODULE.len() - tick.len()]
    )
}

/// The idle loop [`EXAMPLE_TASKS`] goes with, in place of the seed: it locks
/// the counter and spawns `report`.
#[cfg(test)]
pub(crate) const EXAMPLE_IDLE: &str = concat!(
    "// @idle(shared = [count])\n",
    "fn idle_loop(mut cx: app::idle::Context) -> ! {\n",
    "    loop {\n",
    "        app::report::spawn(1).ok();\n",
    "        let _seen = cx.shared.count.lock(|c| *c);\n",
    "        cortex_m::asm::wfi();\n",
    "    }\n",
    "}\n",
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::codegen::common::{GEN_BEGIN, GEN_END};

    fn tail(user: &str) -> String {
        format!("// head\n{GEN_BEGIN}\nmod app {{}}\n{GEN_END}\n{user}")
    }

    fn locals(names: &[&str]) -> Vec<(String, String)> {
        names
            .iter()
            .map(|n| ((*n).to_owned(), format!("T_{n}")))
            .collect()
    }

    #[test]
    fn a_task_is_a_fn_whose_context_is_its_own() {
        let app = scan(&tail(concat!(
            "/// blinks\n",
            "// @task(priority = 2, local = [pc13_out], start)\n",
            "#[allow(unused)]\n",
            "pub(crate) async fn blink(cx: crate::app::blink::Context<'_>) {\n",
            "    let _ = \"fn fake(cx: app::fake::Context)\";\n",
            "}\n",
            "async fn report(\n    mut cx: app::report::Context<'_>,\n    by: u32,\n) {}\n",
            "// fn commented(cx: app::commented::Context) {}\n",
            "mod inner { fn nested(cx: app::nested::Context) {} }\n",
            "fn idle_loop(cx: app::idle::Context) -> ! { loop {} }\n",
            "fn other(cx: app::blink::Context) {}\n",
            "async fn forever(cx: app::forever::Context<'static>) -> ! { loop {} }\n",
        )));
        assert!(app.refusals.is_empty(), "{:?}", app.refusals);
        let names: Vec<&str> = app.tasks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["blink", "report", "forever"]);
        let blink = &app.tasks[0];
        assert_eq!(blink.args, "priority = 2, local = [pc13_out]");
        assert!(blink.start && blink.is_async);
        assert_eq!(blink.claimed, ["pc13_out"]);
        assert_eq!(app.tasks[1].args, "priority = 1");
        assert_eq!(app.tasks[1].inputs, ["by: u32"]);
        assert!(app.tasks[2].diverges);
    }

    #[test]
    fn marker_args_are_rticks_own_verbatim() {
        let app = scan(&tail(concat!(
            "// @task(priority = 3, local = [buf: [u8; 4] = [0; 4], led], shared = [&count, x])\n",
            "async fn t(cx: app::t::Context<'_>) {}\n",
        )));
        let t = &app.tasks[0];
        assert_eq!(
            t.args,
            "priority = 3, local = [buf: [u8; 4] = [0; 4], led], shared = [&count, x]"
        );
        assert_eq!(t.declared, ["buf"]);
        assert_eq!(t.claimed, ["led"]);
        assert_eq!(t.shared, ["count", "x"]);
    }

    #[test]
    fn every_refusal_names_its_task() {
        for (src, says) in [
            (
                "// @task(priority = 0)\nasync fn a(cx: app::a::Context<'_>) {}\n",
                "priority 0",
            ),
            (
                "// @task(priority = 17)\nasync fn a(cx: app::a::Context<'_>) {}\n",
                "16 priority",
            ),
            ("fn a(cx: app::a::Context) {}\n", "needs `binds"),
            (
                "// @task(binds = TIM2)\nasync fn a(cx: app::a::Context<'_>) {}\n",
                "software task",
            ),
            (
                "// @task(binds = SysTick)\nfn a(cx: app::a::Context) {}\n",
                "Mono",
            ),
            (
                "// @task(start)\nasync fn a(cx: app::a::Context<'_>, x: u8) {}\n",
                "`start`",
            ),
            ("async fn init(cx: app::init::Context<'_>) {}\n", "rename"),
            (
                "// @task(priority = x)\nasync fn a(cx: app::a::Context<'_>) {}\n",
                "no number",
            ),
        ] {
            let app = scan(&tail(src));
            assert!(app.tasks.is_empty(), "{src}");
            assert!(
                app.refusals.iter().any(|r| r.contains(says)),
                "{src}: {:?}",
                app.refusals
            );
        }
    }

    #[test]
    fn a_marker_above_nothing_is_said() {
        let app = scan(&tail("// @task(priority = 2)\nfn plain() {}\n"));
        assert_eq!(app.refusals.len(), 1, "{:?}", app.refusals);
        assert!(app.refusals[0].contains("above no task"));
        // Misspelt above a task: the task is still read (with no args), and
        // the marker said - silence would run it at the wrong priority.
        let typo = scan(&tail(
            "// @tsak(priority = 2)\nasync fn a(cx: app::a::Context<'_>) {}\n",
        ));
        assert_eq!(typo.tasks.len(), 1);
        assert!(typo.refusals[0].contains("@tsak"), "{:?}", typo.refusals);
        // Not a marker: prose, a bare `@`.
        let prose = scan(&tail("// @see the manual (page 3)\n// @ notes\n"));
        assert!(prose.refusals.is_empty(), "{:?}", prose.refusals);
    }

    #[test]
    fn app_shared_needs_its_fn() {
        let without = scan(&tail("struct AppShared {\n    count: u32,\n}\n"));
        assert!(without.app_shared.is_empty());
        assert!(without.refusals[0].contains("app_shared"));
        let with = scan(&tail(
            "struct AppShared {\n    count: u32,\n    pub buf: [u8; 4],\n}\nfn app_shared() -> AppShared { todo!() }\n",
        ));
        assert_eq!(
            with.app_shared,
            [
                ("count".to_owned(), "u32".to_owned()),
                ("buf".to_owned(), "[u8; 4]".to_owned())
            ]
        );
    }

    #[test]
    fn the_plan_splits_resources_and_picks_dispatchers() {
        let app = scan(&tail(concat!(
            "struct AppShared { count: u32 }\nfn app_shared() -> AppShared { todo!() }\n",
            "// @task(priority = 2, local = [led], start)\nasync fn blink(cx: app::blink::Context<'_>) {}\n",
            "// @task(priority = 3, shared = [count, serial1])\nasync fn report(cx: app::report::Context<'_>, by: u32) {}\n",
            "// @task(binds = TIM2, priority = 4)\nfn tick(cx: app::tick::Context) {}\n",
            "// @idle(shared = [count])\nfn idle_loop(cx: app::idle::Context) -> ! { loop {} }\n",
        )));
        let p = plan(
            &app,
            &locals(&["pb1_in", "led", "serial1", "iwdg"]),
            &BTreeSet::from(["pb1_in".to_owned()]),
            &BTreeSet::from(["EXTI1".to_owned()]),
        );
        assert_eq!(p.errors, "");
        assert_eq!(p.dispatchers, ["RCC", "FLASH"]);
        assert_eq!(p.promoted, [("serial1".to_owned(), "T_serial1".to_owned())]);
        assert_eq!(
            p.idle_drop,
            BTreeSet::from(["led".to_owned(), "serial1".to_owned()])
        );
        assert_eq!(p.idle_shared, ["count"]);
        assert!(p.has_shared());
        assert_eq!(
            p.spawns,
            "        // Your tasks marked `start` below the markers.\n        blink::spawn().ok();\n"
        );
        assert!(p.extern_block.contains(
            "        #[task(priority = 2, local = [led])]\n        async fn blink(_: blink::Context<'_>);\n"
        ));
        assert!(
            p.extern_block
                .contains("        async fn report(_: report::Context<'_>, by: u32);\n")
        );
        assert!(p.extern_block.contains(
            "        #[task(binds = TIM2, priority = 4)]\n        fn tick(_: tick::Context);\n"
        ));
        // Exactly one attribute per declaration: rtic-macros refuses more.
        let decls = &p.extern_block[p.extern_block.find("extern").unwrap()..];
        assert!(
            !decls.contains("///") && !decls.contains("#[allow"),
            "{decls}"
        );
    }

    #[test]
    fn resource_refusals() {
        for (src, says) in [
            (
                "// @task(local = [nope])\nasync fn a(cx: app::a::Context<'_>) {}\n",
                "names no resource",
            ),
            (
                "// @task(local = [pb1_in])\nasync fn a(cx: app::a::Context<'_>) {}\n",
                "EXTI task",
            ),
            (
                "// @task(local = [led])\nasync fn a(cx: app::a::Context<'_>) {}\n// @task(local = [led])\nasync fn b(cx: app::b::Context<'_>) {}\n",
                "claimed by task `a`",
            ),
            (
                "// @task(shared = [nope])\nasync fn a(cx: app::a::Context<'_>) {}\n",
                "names nothing shared",
            ),
            (
                "// @task(binds = EXTI1)\nfn a(cx: app::a::Context) {}\n",
                "armed pin",
            ),
            (
                "// @task(local = [led])\nasync fn a(cx: app::a::Context<'_>) {}\n// @task(shared = [led])\nasync fn b(cx: app::b::Context<'_>) {}\n",
                "one or the other",
            ),
            (
                "// @task(binds = RCC)\nfn h(cx: app::h::Context) {}\nasync fn s(cx: app::s::Context<'_>) {}\n",
                "dispatchers",
            ),
        ] {
            let p = plan(
                &scan(&tail(src)),
                &locals(&["pb1_in", "led"]),
                &BTreeSet::from(["pb1_in".to_owned()]),
                &BTreeSet::from(["EXTI1".to_owned()]),
            );
            assert!(p.errors.contains(says), "{src}: {}", p.errors);
            assert!(
                p.errors.starts_with("compile_error!(\"RTIC"),
                "{}",
                p.errors
            );
        }
    }

    #[test]
    fn dispatchers_skip_bound_vectors_and_keep_their_order() {
        let mut src = String::new();
        for p in 1..=7 {
            src.push_str(&format!(
                "// @task(priority = {p})\nasync fn t{p}(cx: app::t{p}::Context<'_>) {{}}\n"
            ));
        }
        let p = plan(&scan(&tail(&src)), &[], &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(p.dispatchers.len(), 6);
        assert!(
            p.errors.contains("7 software priority levels"),
            "{}",
            p.errors
        );
    }

    #[test]
    fn tasks_are_fenced_off_rtic_and_given_back() {
        let user = concat!(
            "struct AppShared { count: u32 }\n",
            "fn app_shared() -> AppShared { AppShared { count: 0 } }\n",
            "// @task(priority = 2)\n",
            "async fn blink(cx: app::blink::Context<'_>) {}\n",
            "fn helper() {}\n",
        );
        let file = tail(user);
        let off = fence_tasks(file.clone(), false);
        assert_eq!(off.matches(RTIC_TASK_FENCE).count(), 3, "{off}");
        assert!(
            off.contains(&format!("{RTIC_TASK_FENCE}async fn blink")),
            "{off}"
        );
        assert!(
            !off.contains(&format!("{RTIC_TASK_FENCE}fn helper")),
            "{off}"
        );
        assert_eq!(fence_tasks(off.clone(), false), off, "once");
        assert_eq!(fence_tasks(off, true), file);
        // A fenced task is still read as one.
        assert_eq!(scan(&fence_tasks(file, false)).tasks.len(), 1);
    }

    /// The shapes the review found read wrong: an array input (its `;`), an
    /// attribute on the fn's own line, a marker spaced out, and `local_task`.
    #[test]
    fn odd_but_valid_spellings_are_read() {
        let app = scan(&tail(concat!(
            "//@task (priority = 2, local_task)\n",
            "#[inline(never)] async fn report(cx: app::report::Context<'_>, buf: [u8; 4]) {}\n",
        )));
        assert!(app.refusals.is_empty(), "{:?}", app.refusals);
        let t = &app.tasks[0];
        assert_eq!(t.inputs, ["buf: [u8; 4]"]);
        assert_eq!(t.args, "priority = 2, local_task");
        let fenced = fence_tasks(
            tail("async fn r(cx: app::r::Context<'_>, b: [u8; 2]) {}\n"),
            false,
        );
        assert!(
            fenced.contains(&format!("{RTIC_TASK_FENCE}async fn r")),
            "{fenced}"
        );
        assert_eq!(
            split_args("mask: u32 = 1 << 3, led, f: fn() -> u8, v: Vec<Option<u8>>, x"),
            [
                "mask: u32 = 1 << 3",
                "led",
                "f: fn() -> u8",
                "v: Vec<Option<u8>>",
                "x"
            ]
        );
    }

    #[test]
    fn more_refusals() {
        for (src, says) in [
            ("async fn gpio(cx: app::gpio::Context<'_>) {}\n", "rename"),
            (
                "// @task(binds = TIM3, priority = 0)\nfn h(cx: app::h::Context) {}\n",
                "priority 0",
            ),
            (
                "// @task(local_task, start)\nasync fn a(cx: app::a::Context<'_>) {}\n",
                "local_task",
            ),
            (
                "async fn a(cx: app::a::Context<'static>) {}\n",
                "never returns",
            ),
            (
                "struct AppShared { #[x] n u32 }\nfn app_shared() -> AppShared { todo!() }\n",
                "not read",
            ),
        ] {
            let app = scan(&tail(src));
            assert!(
                app.refusals.iter().any(|r| r.contains(says)),
                "{src}: {:?}",
                app.refusals
            );
        }
        // An AppShared field named like a generated resource.
        let p = plan(
            &scan(&tail(
                "struct AppShared { led: u8 }\nfn app_shared() -> AppShared { todo!() }\n",
            )),
            &locals(&["led"]),
            &BTreeSet::new(),
            &BTreeSet::new(),
        );
        assert!(
            p.errors.contains("named like a generated resource"),
            "{}",
            p.errors
        );
        assert!(p.app_shared.is_empty());
    }

    #[test]
    fn app_shared_fields_with_attributes_and_visibility() {
        let app = scan(&tail(concat!(
            "struct AppShared {\n    #[allow(dead_code)]\n    pub(crate) count: u32,\n    pub other: bool,\n}\n",
            "fn app_shared() -> AppShared { todo!() }\n",
        )));
        assert!(app.refusals.is_empty(), "{:?}", app.refusals);
        assert_eq!(
            app.app_shared,
            [
                ("count".to_owned(), "u32".to_owned()),
                ("other".to_owned(), "bool".to_owned())
            ]
        );
    }

    /// Idle's `shared = [&x]` keeps its `&`, as a task's args do - RTIC
    /// refuses `&x` beside an exclusive `x`.
    #[test]
    fn idle_shares_as_written() {
        let app = scan(&tail(concat!(
            "struct AppShared { count: u32 }\nfn app_shared() -> AppShared { todo!() }\n",
            "// @task(shared = [&count])\nasync fn a(cx: app::a::Context<'_>) {}\n",
            "// @idle(shared = [&count])\nfn idle_loop(cx: app::idle::Context) -> ! { loop {} }\n",
        )));
        let p = plan(&app, &[], &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(p.errors, "");
        assert_eq!(p.idle_shared, ["&count"]);
    }

    /// Only `@task` / `@idle` - or near misses - are markers: a `// @todo(..)`
    /// is prose, and one inside `/* */` is not code.
    #[test]
    fn only_task_and_idle_are_markers() {
        let app = scan(&tail(concat!(
            "// @todo(alice): tidy\n// @see(datasheet)\n",
            "/*\n// @task(priority = 2)\nasync fn old(cx: app::old::Context<'_>) {}\n*/\n",
        )));
        assert!(app.refusals.is_empty(), "{:?}", app.refusals);
        let typo = scan(&tail("// @Taks(priority = 2)\nfn plain() {}\n"));
        assert!(!typo.refusals.is_empty());
    }

    /// The modules main.rs declares at its top level, read off the project's
    /// files: `src/<name>.rs` or `src/<name>/mod.rs` - not `pins`, not a
    /// commented-out or nested `mod`, not one with no file.
    #[test]
    fn task_modules_are_the_ones_main_declares() {
        let main = tail(concat!(
            "mod tasks;\npub(crate) mod more;\n// mod commented;\nmod missing;\n",
            "mod inline { mod nested; }\npub mod pins;\n",
        ));
        let files = vec![
            ("src/tasks.rs".to_owned(), "a".to_owned()),
            ("src/more/mod.rs".to_owned(), "b".to_owned()),
            ("src/commented.rs".to_owned(), "c".to_owned()),
            ("src/nested.rs".to_owned(), "d".to_owned()),
            ("src/pins/mod.rs".to_owned(), "e".to_owned()),
        ];
        assert_eq!(
            task_modules(&main, &files),
            [
                ("tasks".to_owned(), "a".to_owned()),
                ("more".to_owned(), "b".to_owned())
            ]
        );
    }

    /// A task in a module: read like one in main.rs, `pub` required, and
    /// imported into `mod app` by the block - its readout says where.
    #[test]
    fn a_task_in_a_module_is_imported_into_the_app() {
        let modules = vec![(
            "tasks".to_owned(),
            concat!(
                "use crate::app;\n",
                "// @task(priority = 2, local = [led])\n",
                "pub async fn blink(cx: app::blink::Context<'_>) {}\n",
                "async fn hidden(cx: crate::app::hidden::Context<'_>) {}\n",
                "fn idle_loop(cx: app::idle::Context) -> ! { loop {} }\n",
            )
            .to_owned(),
        )];
        let app = scan_project(&tail("mod tasks;\nuse tasks::*;\n"), &modules);
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].module.as_deref(), Some("tasks"));
        assert!(
            app.refusals
                .iter()
                .any(|r| r.contains("`hidden`") && r.contains("`pub`")),
            "{:?}",
            app.refusals
        );
        let p = plan(&app, &locals(&["led"]), &BTreeSet::new(), &BTreeSet::new());
        assert!(
            p.extern_block
                .contains("    //   blink (in tasks): software, priority 2"),
            "{}",
            p.extern_block
        );
        assert!(
            p.extern_block
                .contains("    use crate::tasks::blink;\n    extern \"Rust\" {\n"),
            "{}",
            p.extern_block
        );
        // Fenced in its own file off RTIC, and back.
        let text = &modules[0].1;
        let off = fence_tasks(text.clone(), false);
        assert!(
            off.contains(&format!("{RTIC_TASK_FENCE}pub async fn blink")),
            "{off}"
        );
        assert_eq!(fence_tasks(off, true), *text);
    }

    /// Off RTIC a task module's own imports of RTIC's things go with its
    /// tasks - `app`, `Mono` and the monotonic's prelude exist on RTIC alone -
    /// and come back with them.
    #[test]
    fn a_task_modules_rtic_imports_are_fenced_too() {
        let off = fence_tasks(EXAMPLE_MODULE.to_owned(), false);
        for line in [
            "use crate::Mono;",
            "use crate::app;",
            "use rtic_monotonics::systick::prelude::*;",
            "pub async fn blink",
            "pub fn tick",
        ] {
            assert!(
                off.contains(&format!("{RTIC_TASK_FENCE}{line}")),
                "{line}:\n{off}"
            );
        }
        assert_eq!(fence_tasks(off, true), EXAMPLE_MODULE);
        // A module with no task keeps its imports.
        let plain = "use crate::app;\nfn helper() {}\n".to_owned();
        assert_eq!(fence_tasks(plain.clone(), false), plain);
    }

    /// What the module review found: `pub(self)` cannot be reached from
    /// `mod app`, two tasks may not share a name, and a `mod` under an
    /// attribute is not guessed at.
    #[test]
    fn module_edge_cases() {
        let module = |text: &str| vec![("tasks".to_owned(), text.to_owned())];
        let private = scan_project(
            &tail("mod tasks;\n"),
            &module("pub(self) async fn a(cx: crate::app::a::Context<'_>) {}\n"),
        );
        assert!(private.tasks.is_empty());
        assert!(
            private.refusals[0].contains("has to be `pub`"),
            "{:?}",
            private.refusals
        );
        let crate_vis = scan_project(
            &tail("mod tasks;\n"),
            &module("pub(crate) async fn a(cx: crate::app::a::Context<'_>) {}\n"),
        );
        assert_eq!(crate_vis.tasks.len(), 1, "{:?}", crate_vis.refusals);

        let twice = scan_project(
            &tail("async fn a(cx: app::a::Context<'_>) {}\n"),
            &module("pub async fn a(cx: crate::app::a::Context<'_>) {}\n"),
        );
        assert_eq!(twice.tasks.len(), 1);
        assert!(
            twice
                .refusals
                .iter()
                .any(|r| r.contains("`a` in `tasks`") && r.contains("this name")),
            "{:?}",
            twice.refusals
        );

        let files = vec![
            ("src/tasks.rs".to_owned(), "a".to_owned()),
            ("src/jobs.rs".to_owned(), "b".to_owned()),
        ];
        let main = tail("#[cfg(feature = \"x\")]\nmod tasks;\n#[path = \"jobs.rs\"] mod jobs;\n");
        assert!(task_modules(&main, &files).is_empty());
    }

    /// Modules declared by modules: read as `a::b` off `src/a/b.rs` when
    /// `mod app` reaches them, imported by their whole path; the ones passed
    /// over are said - only when a task could be in them.
    #[test]
    fn nested_modules() {
        let task = |name: &str, vis: &str| {
            format!("use crate::app;\n{vis} async fn {name}(cx: app::{name}::Context<'_>) {{}}\n")
        };
        let files = vec![
            (
                "src/a.rs".to_owned(),
                "pub mod b;\nmod hidden;\nmod quiet;\npub(super) mod c;\n".to_owned(),
            ),
            (
                "src/a/b/mod.rs".to_owned(),
                format!("pub(crate) mod deep;\n{}", task("one", "pub")),
            ),
            ("src/a/b/deep.rs".to_owned(), task("two", "pub(super)")),
            ("src/a/hidden.rs".to_owned(), task("three", "pub")),
            ("src/a/quiet.rs".to_owned(), "fn helper() {}\n".to_owned()),
            ("src/a/c.rs".to_owned(), task("four", "pub")),
            ("src/t.rs".to_owned(), task("five", "pub")),
            ("src/u.rs".to_owned(), "fn helper() {}\n".to_owned()),
        ];
        let main = tail(concat!(
            "mod a;\n#[allow(unused)]\nmod t;\n#[cfg(test)]\nmod u;\n",
            "#[path = \"x.rs\"]\nmod v;\n",
        ));
        let mut names: Vec<String> = task_modules(&main, &files)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        names.sort();
        assert_eq!(names, ["a", "a::b", "a::b::deep", "a::c", "t"]);
        assert_eq!(
            modules_left_out(&main, &files),
            [
                "mod v: under #[cfg] or #[path] - its tasks are not read",
                "mod a::hidden: private to a - mod app cannot reach a task in or below it; \
                 declare it pub mod hidden;",
            ]
        );

        let app = scan_project(&main, &task_modules(&main, &files));
        let mut tasks: Vec<(String, String)> = app
            .tasks
            .iter()
            .map(|t| (t.name.clone(), t.module.clone().unwrap_or_default()))
            .collect();
        tasks.sort();
        assert_eq!(
            tasks,
            [
                ("five".to_owned(), "t".to_owned()),
                ("four".to_owned(), "a::c".to_owned()),
                ("one".to_owned(), "a::b".to_owned()),
            ]
        );
        // `pub(super)` two levels down reaches `a::b`, not the crate root.
        assert!(
            app.refusals
                .iter()
                .any(|r| r.contains("`two`") && r.contains("`a::b::deep`")),
            "{:?}",
            app.refusals
        );
        let p = plan(&app, &locals(&[]), &BTreeSet::new(), &BTreeSet::new());
        assert!(
            p.extern_block.contains("    use crate::a::b::one;\n"),
            "{}",
            p.extern_block
        );
        assert_eq!(module_paths("a::b"), ["src/a/b.rs", "src/a/b/mod.rs"]);

        // The cross-compile's split: blink in `tasks`, tick in `tasks::hw`.
        let files = vec![
            ("src/tasks.rs".to_owned(), example_module_nested()),
            ("src/tasks/hw.rs".to_owned(), EXAMPLE_NESTED.to_owned()),
        ];
        let modules = task_modules(&tail(EXAMPLE_MAIN_WITH_MODULE), &files);
        assert_eq!(modules.len(), 2);
        let app = scan_project(&tail(EXAMPLE_MAIN_WITH_MODULE), &modules);
        assert!(
            app.tasks
                .iter()
                .any(|t| t.name == "tick" && t.module.as_deref() == Some("tasks::hw")),
            "{app:?}"
        );
    }

    /// What the nested-module review found: a task below a module with none
    /// of its own still names it; an attribute wrapped over lines, after
    /// another, or inside the file still gates; `super::super::app` is fenced;
    /// `pub(in super::super)` reaches the root; no depth cap.
    #[test]
    fn nested_module_edge_cases() {
        let task = |name: &str| {
            format!("use crate::app;\npub async fn {name}(cx: app::{name}::Context<'_>) {{}}\n")
        };
        let files = vec![
            ("src/a.rs".to_owned(), "mod b;\n".to_owned()),
            ("src/a/b.rs".to_owned(), "pub mod c;\n".to_owned()),
            ("src/a/b/c.rs".to_owned(), task("deep")),
            ("src/g.rs".to_owned(), task("wrapped")),
            ("src/h.rs".to_owned(), task("second")),
            (
                "src/i.rs".to_owned(),
                format!("#![cfg(feature = \"i\")]\n{}", task("inner")),
            ),
            ("src/q/r.rs".to_owned(), "pub mod s;\n".to_owned()),
        ];
        let main = tail(concat!(
            "pub mod a;\n",
            "#[cfg(any(\n    feature = \"x\",\n    feature = \"y\"\n))]\nmod g;\n",
            "#[allow(unused)] #[cfg(feature = \"h\")] mod h;\n",
            "mod i;\n",
        ));
        assert_eq!(
            task_modules(&main, &files)
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>(),
            ["a"]
        );
        let left = modules_left_out(&main, &files);
        for m in ["mod g:", "mod h:", "mod i:", "mod a::b: private to a"] {
            assert!(left.iter().any(|l| l.starts_with(m)), "{m}: {left:?}");
        }

        let off = fence_tasks(
            "use super::super::app;\nuse super::helpers;\npub fn tick(_cx: app::tick::Context) {}\n"
                .to_owned(),
            false,
        );
        assert!(
            off.contains(&format!("{RTIC_TASK_FENCE}use super::super::app;")),
            "{off}"
        );
        assert!(
            !off.contains(&format!("{RTIC_TASK_FENCE}use super::helpers;")),
            "{off}"
        );
        assert!(rtic_only_use("crate::app as rtic_app"));
        assert!(!rtic_only_use("crate::application"));

        let up = scan_project(
            "",
            &[(
                "a::b".to_owned(),
                "pub(in super::super) async fn one(cx: crate::app::one::Context<'_>) {}\n"
                    .to_owned(),
            )],
        );
        assert_eq!(up.tasks.len(), 1, "{:?}", up.refusals);

        // Eight levels down, read all the way.
        let mut files = Vec::new();
        let mut dir = String::from("src");
        for level in 0..8 {
            let name = format!("m{level}");
            let body = if level == 7 {
                task("bottom")
            } else {
                format!("pub mod m{};\n", level + 1)
            };
            files.push((format!("{dir}/{name}.rs"), body));
            dir = format!("{dir}/{name}");
        }
        let modules = task_modules(&tail("mod m0;\n"), &files);
        assert_eq!(modules.len(), 8);
        assert_eq!(scan_project(&tail("mod m0;\n"), &modules).tasks.len(), 1);
    }

    /// The tab's view reads the block back: tasks, dispatchers, refusals.
    #[test]
    fn the_view_reads_the_block_back() {
        use crate::panels::mcu_module::codegen::common::{GEN_BEGIN, GEN_END};
        let app = scan(&tail(concat!(
            "// @task(priority = 2, local = [led], start)\nasync fn blink(cx: app::blink::Context<'_>) {}\n",
            "// @task(local = [nope])\nasync fn bad(cx: app::bad::Context<'_>) {}\n",
        )));
        let p = plan(&app, &locals(&["led"]), &BTreeSet::new(), &BTreeSet::new());
        let main = format!(
            "{GEN_BEGIN}\nmod app {{\n{}}}\n{}{GEN_END}\n",
            p.extern_block, p.errors
        );
        let v = view(&main);
        assert_eq!(
            v.tasks,
            ["blink: software, priority 2, started in #[init], local: led"]
        );
        assert_eq!(v.dispatchers.as_deref(), Some("RCC"));
        assert_eq!(v.errors.len(), 1);
        assert!(
            v.errors[0].starts_with("RTIC task `bad`: `local = [nope]`"),
            "{:?}",
            v.errors
        );
        assert_eq!(view("no block"), TasksView::default());
    }

    /// `AppShared` in a project with no task is just the user's struct: never
    /// fenced off.
    #[test]
    fn app_shared_alone_is_left_alone() {
        let file = tail(
            "struct AppShared { n: u8 }\nfn app_shared() -> AppShared { AppShared { n: 0 } }\n",
        );
        assert_eq!(fence_tasks(file.clone(), false), file);
    }
}
