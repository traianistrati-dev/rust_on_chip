//! Build the module graph by parsing source text (pure logic, tested).
//!
//! Nodes come from the file list (`foo/bar.rs` → module `foo::bar`,
//! `foo/mod.rs` → `foo`, main.rs → the crate root). Edges come from `mod x;`
//! declarations (containment) and from `use` / inline path chains resolved
//! against the known module paths (dependency). Resolution is deliberately
//! approximate — a plain-text scan, not name resolution — but it is exact for
//! the common shapes this IDE generates (`use crate::a::b`, `super::x`,
//! `pins::configs::usart1::init(...)`).

use std::collections::{HashMap, HashSet};

/// Kind of a top-level item shown inside a module node (Phase 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymKind {
    Fn,
    Struct,
    Enum,
    Trait,
}

/// One top-level item of a module — listed inside its node, UML-package style.
/// `line` is 1-based in the module's file (used for click-to-jump), exact for
/// the text the graph was parsed from (unlike LSP positions, which refer to
/// rust-analyzer's last-synced text and would mis-jump after unsaved edits).
#[derive(Clone, Debug)]
pub struct SymbolItem {
    pub name: String,
    pub kind: SymKind,
    pub line: usize,
    /// 1-based line of the item's closing brace (== `line` for `struct Foo;`).
    /// Together with `line` this is the item's SPAN, used to attribute a
    /// reference site to its enclosing top-level item.
    pub end_line: usize,
    /// 0-based column (chars) of the name on its line — the position a
    /// `textDocument/references` request must point at. Keyword/modifier
    /// prefixes are ASCII, so char count == UTF-16 units here.
    pub col: usize,
}

/// One module in the graph.
#[derive(Clone, Debug)]
pub struct ModuleNode {
    /// Full Rust path (`"mw_radar::utils"`); empty string for the crate root.
    pub path: String,
    /// Display name — the last path segment (`"utils"`), `"main"` for the root.
    pub name: String,
    /// Workspace-relative file (`"mw_radar/utils.rs"`); `"main.rs"` for the root.
    pub file_rel: String,
    /// The crate this module belongs to, as its path prefix: empty for the
    /// firmware (the graph's root crate), `"mw_radar"` for an extracted
    /// library. `crate::` and `super::` resolve against THIS, not the graph
    /// root — otherwise a library's `use crate::data::…` would look for a
    /// firmware module named `data`.
    pub krate: String,
    /// Index into `user_src_files`; `None` = main.rs.
    pub file: Option<usize>,
    /// Number of `fn` items (incl. methods) — a size badge, not a precise count.
    pub fn_count: usize,
    /// Number of `struct` / `enum` / `trait` items.
    pub ty_count: usize,
    /// Top-level items (brace-depth-0 `fn` / `struct` / `enum` / `trait`), in
    /// file order. Methods inside `impl` blocks are counted in `fn_count` but
    /// not listed here (they'd bloat the node).
    pub symbols: Vec<SymbolItem>,
    /// Depth-0 `impl` block spans, each resolved to the ROW of the implemented
    /// type (`impl Parser { … }` / `impl Frame for HmmdFrame { … }` → the
    /// `Parser` / `HmmdFrame` row). Call sites inside methods attribute to
    /// that row — without this, modules that keep their logic in impl methods
    /// (most library code) produced NO outgoing call edges at all.
    pub impl_spans: Vec<(usize, usize, usize)>,
    /// GHOST node for an external crate (std / HAL / …), appended by
    /// [`add_external_nodes`]: no file, no symbols, not clickable.
    pub is_external: bool,
    /// A file of a DETACHED library (see [`mark_detached`]): drawn in the
    /// LIBRARIES panel's amber, so the diagram says what the tree says.
    pub detached: bool,
    /// Why no call search can reach this module right now - a detached
    /// library the running rust-analyzer has not loaded. `None` otherwise. Set
    /// by [`mark_detached`].
    pub untraced: Option<Untraced>,
    /// An extra hover line for what the graph alone does not say - a detached
    /// copy, or the registry version behind an external node.
    pub note: String,
}

impl ModuleNode {
    /// The symbol row whose line SPAN contains 1-based `line` — brace-depth
    /// based, so it is immune to indentation style. (The first version used
    /// "column-0 line = item boundary", which real code broke: statements
    /// written at column 0 inside `fn main` — e.g. `radar.read_data(|rx| {` —
    /// became anonymous boundaries and every call site after them was
    /// attributed to nothing, so its call edge silently vanished.)
    /// Falls back to the enclosing `impl` block's type row (see `impl_spans`).
    pub fn enclosing_row(&self, line: usize) -> Option<usize> {
        self.symbols
            .iter()
            .position(|s| line >= s.line && line <= s.end_line)
            .or_else(|| {
                self.impl_spans
                    .iter()
                    .find(|&&(s, e, _)| line >= s && line <= e)
                    .map(|&(_, _, row)| row)
            })
    }
}

/// The whole module graph. Edge tuples are `(from, to)` node indices.
#[derive(Clone, Debug, Default)]
pub struct ModuleGraph {
    pub nodes: Vec<ModuleNode>,
    /// `user → used` module references (solid arrows).
    pub deps: Vec<(usize, usize)>,
    /// `parent → child` from `mod child;` declarations (dashed lines).
    pub contains: Vec<(usize, usize)>,
}

impl ModuleGraph {
    // The call-edge focus is the SELECTED NODE, and nothing else.
    //
    // There used to be a `focus_set` here returning a membership MASK, because
    // a `mod.rs` / `lib.rs` focused its whole package subtree: the argument was
    // that a facade of `pub mod` declarations has nothing of its own to draw,
    // so focusing it alone looked inert.
    //
    // It was removed. Expanding put every package member at hop 0, so all the
    // package's interior wiring was level 1 and the depth control could not
    // trim it - selecting one library root drew twenty modules' worth of edges
    // at the lowest setting. Worse, the toolbar label decided "is this a
    // package root?" with its OWN rule (`mod.rs` only, where the focus used
    // `mod.rs` or `lib.rs`), so a library root focused twenty modules while the
    // toolbar named one and nothing on screen said otherwise.
    //
    // A facade now shows no call edges, which is the truth: it has no code, so
    // it makes no calls. The old behaviour hid that by drawing other modules'
    // calls under its name.
}

/// `"foo/bar.rs"` → `"foo::bar"`, `"foo/mod.rs"` → `"foo"`, `"utils.rs"` → `"utils"`.
pub fn module_path_of(rel: &str) -> String {
    let (krate, rest) = split_crate(rel);
    let no_ext = rest.strip_suffix(".rs").unwrap_or(rest);
    let no_mod = no_ext.strip_suffix("/mod").unwrap_or(no_ext);
    // A crate-root file adds nothing below the crate itself.
    let inner = if no_mod == "lib" || no_mod == "main" {
        String::new()
    } else {
        no_mod.replace('/', "::")
    };
    match (krate.is_empty(), inner.is_empty()) {
        (true, _) => inner,
        (false, true) => krate,
        (false, false) => format!("{krate}::{inner}"),
    }
}

/// Split a project-root-relative path into `(crate root path segment, path
/// inside the crate's src/)`.
///
/// The firmware IS the graph's root crate, so its files get an empty prefix and
/// keep bare module paths (`pins::configs`). A library crate extracted next to
/// `src/` gets its own name as the prefix, so `mw_radar/src/data.rs` becomes
/// `mw_radar::data` and `mw_radar/src/lib.rs` becomes `mw_radar` — which is
/// exactly what makes `mod data;` inside lib.rs resolve to a real node, and
/// what you would write to reach it from the firmware.
pub fn split_crate(rel: &str) -> (String, &str) {
    if let Some(rest) = rel.strip_prefix("src/") {
        return (String::new(), rest);
    }
    if let Some((krate, rest)) = rel.split_once("/src/") {
        let name = crate_prefix_of_dir(krate);
        return (name, rest);
    }
    (String::new(), rel)
}

/// Which local library crates each crate of the graph may reach, and by what
/// name its code reaches them - read from the MANIFESTS.
///
/// The graph used to decide by folder name alone: every folder with a `src/`
/// became a crate that any other crate reached by that name. So a library kept
/// in the project but NOT linked - a detached copy of a crate the firmware takes
/// from crates.io under the very same name - got a dependency arrow from
/// `main.rs`, and the registry crate the code really uses never showed up among
/// the externals, hidden behind the local folder's name.
///
/// A crate ABSENT from `reach` is unknown: its manifest is missing or does not
/// parse right now - typically mid-edit - and its chains keep the old
/// name-only rule rather than losing their arrows.
#[derive(Clone, Debug, Default)]
pub struct CrateLinks {
    /// Crate prefix (`""` = the firmware) → (name as its code writes it →
    /// crate prefix of the local library that name means).
    reach: HashMap<String, HashMap<String, String>>,
    /// The firmware's registry dependencies: name as code writes it → version.
    registry: HashMap<String, String>,
    /// Every library folder that has a manifest - see [`CrateLinks::link_crate`].
    lib_dirs: Vec<String>,
}

impl CrateLinks {
    /// From the firmware's `Cargo.toml` and the user files, which carry every
    /// library's own `<folder>/Cargo.toml`.
    ///
    /// Three ways a manifest links a local crate, and all three count: a `path`
    /// dependency; `{ workspace = true }`, inherited from a `path` in the root's
    /// `[workspace.dependencies]`; and a `[patch.<registry>]` entry, which puts
    /// the local folder in place of that registry crate for EVERY crate that
    /// asks for it - the likeliest reason to keep a same-named copy at all.
    pub fn from_manifests(root_manifest: &str, user_files: &[(String, String)]) -> Self {
        use crate::publish;
        if !publish::manifest_parses(root_manifest) {
            return Self::default();
        }
        // Every manifest by its folder, the firmware's under "".
        let mut manifests: HashMap<String, &str> = user_files
            .iter()
            .filter_map(|(rel, c)| Some((rel.strip_suffix("/Cargo.toml")?.to_owned(), c.as_str())))
            .collect();
        manifests.insert(String::new(), root_manifest);
        let lib_dirs: Vec<String> = manifests
            .keys()
            .filter(|d| !d.is_empty())
            .cloned()
            .collect();

        // The workspace a crate in `dir` belongs to: the nearest folder at or
        // above it whose manifest has a `[workspace]` table - else the crate is
        // its own. `{ workspace = true }` and `[patch]` are both read THERE,
        // their paths relative to it: a detached library that is its own
        // workspace inherits from its own table, not the firmware's.
        let ws_root_of = |dir: &str| -> String {
            let mut d = dir;
            loop {
                if manifests
                    .get(d)
                    .is_some_and(|m| publish::has_workspace_table(m))
                {
                    return d.to_owned();
                }
                if d.is_empty() {
                    return dir.to_owned();
                }
                d = d.rsplit_once('/').map_or("", |(up, _)| up);
            }
        };
        let links_of = |dir: &str, manifest: &str| {
            let ws = ws_root_of(dir);
            let ws_manifest = manifests.get(ws.as_str()).copied().unwrap_or(manifest);
            let to_prefix = |path: &str| join_rel(&ws, path).map(|f| crate_prefix_of_dir(&f));
            let mut m: HashMap<String, String> = publish::patch_path_deps(ws_manifest)
                .into_iter()
                .filter_map(|(name, path)| Some((name.replace('-', "_"), to_prefix(&path)?)))
                .collect();
            let inherited: HashMap<String, String> = publish::workspace_path_deps(ws_manifest)
                .into_iter()
                .filter_map(|(name, path)| Some((name, to_prefix(&path)?)))
                .collect();
            for name in publish::inherited_deps(manifest) {
                if let Some(prefix) = inherited.get(&name) {
                    m.insert(name.replace('-', "_"), prefix.clone());
                }
            }
            m.extend(path_links(dir, manifest));
            m
        };
        let mut reach = HashMap::new();
        reach.insert(String::new(), links_of("", root_manifest));
        for dir in &lib_dirs {
            let content = manifests[dir];
            // Left UNKNOWN when it does not parse, never "links nothing": that
            // would drop every arrow the library draws to another one for as
            // long as its manifest is half-typed.
            if publish::manifest_parses(content) {
                reach.insert(crate_prefix_of_dir(dir), links_of(dir, content));
            }
        }
        let registry = publish::registry_deps(root_manifest)
            .into_iter()
            .map(|(name, version)| (name.replace('-', "_"), version))
            .collect();
        Self {
            reach,
            registry,
            lib_dirs,
        }
    }

    /// The crate a node's CHAINS are judged by - usually its own. The
    /// exception is a library's file outside its `src/` (`examples/`,
    /// `tests/`, `benches/`): the graph files those under the firmware's empty
    /// prefix, and judging them by the firmware's manifest cut them off from
    /// their own library. They are judged by no manifest instead - the old
    /// name-only rule, exactly as before any of this.
    fn link_crate(&self, node_krate: &str, file_rel: &str) -> String {
        if node_krate.is_empty() {
            if let Some(d) = self
                .lib_dirs
                .iter()
                .find(|d| file_rel.starts_with(&format!("{d}/")))
            {
                // A key `reach` can never hold (crate prefixes have no `/`).
                return format!("{d}/");
            }
        }
        node_krate.to_owned()
    }

    /// Whether crate `krate`'s manifest was read. When not, its chains use the
    /// old name-only rule.
    fn knows(&self, krate: &str) -> bool {
        self.reach.contains_key(krate)
    }

    /// The local crate prefix that crate `from` means by `name`, if its
    /// manifest links one.
    fn target(&self, from: &str, name: &str) -> Option<&str> {
        self.reach
            .get(from)?
            .get(&name.replace('-', "_"))
            .map(String::as_str)
    }

    /// The crates whose module NAMES a chain from `krate` may use bare: its
    /// own, and those of every crate it links - `use mylib::parser;` followed
    /// by `parser::decode()`, which the IDE's own Extract writes.
    fn reachable<'a>(&'a self, krate: &'a str) -> Vec<&'a str> {
        let mut v = vec![krate];
        if let Some(m) = self.reach.get(krate) {
            v.extend(m.values().map(String::as_str));
        }
        v
    }
}

/// A manifest's `path` dependencies as (name in code → local crate prefix).
/// `dir` is the manifest's folder, project-root-relative (`""` = the root).
fn path_links(dir: &str, manifest: &str) -> HashMap<String, String> {
    crate::publish::path_deps(manifest)
        .into_iter()
        .filter_map(|d| {
            let folder = join_rel(dir, &d.path)?;
            Some((d.name.replace('-', "_"), crate_prefix_of_dir(&folder)))
        })
        .collect()
}

/// `dir` joined with a manifest-relative `path`, as a project-root-relative
/// folder: `/` separators, `.` dropped, `..` applied. `None` for a path that is
/// absolute or climbs out of the project - no crate of the graph lives there.
fn join_rel(dir: &str, path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.starts_with('/') || path.contains(':') {
        return None;
    }
    let mut segs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for s in path.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop()?;
            }
            s => segs.push(s),
        }
    }
    (!segs.is_empty()).then(|| segs.join("/"))
}

/// The prefix a library folder's modules carry in the graph: its last segment,
/// `-` read as `_` the way Rust reads a crate name. The ONE rule, shared by
/// [`split_crate`] and [`CrateLinks`], so the two can never disagree about
/// which crate a folder is.
fn crate_prefix_of_dir(dir: &str) -> String {
    dir.rsplit('/').next().unwrap_or(dir).replace('-', "_")
}

/// Why a detached library's calls are not traced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Untraced {
    /// Cargo would load it now, but the RUNNING rust-analyzer was started
    /// without it: `linkedProjects` is fixed when the analyzer starts.
    NeedsRestart,
    /// Cargo refuses it: it sits inside this project's workspace, which does
    /// not list it.
    Refused,
}

/// One detached library: its folder, and - when its calls are not traced -
/// why. `None` means the running rust-analyzer has it loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetachedLib {
    pub dir: String,
    pub untraced: Option<Untraced>,
}

/// Mark every file of a DETACHED library - the folders the LIBRARIES panel
/// lists as detached (`extract_crate::detached_libs`), so the two agree by
/// construction - for the diagram to draw in that panel's amber.
pub fn mark_detached(graph: &mut ModuleGraph, libs: &[DetachedLib]) {
    for n in graph.nodes.iter_mut().filter(|n| !n.is_external) {
        let Some(lib) = libs
            .iter()
            .find(|l| n.file_rel.starts_with(&format!("{}/", l.dir)))
        else {
            continue;
        };
        n.detached = true;
        n.untraced = lib.untraced;
        let base = "Detached library: kept in this project but not in its workspace, \
                    so the firmware does not build this copy.";
        n.note = match lib.untraced {
            None => format!("{base} rust-analyzer loads it on its own, so its calls are traced."),
            Some(Untraced::NeedsRestart) => format!(
                "{base}\n\nCargo can load it now, but the running rust-analyzer was \
                 started without it. Restart the analyzer to trace its calls."
            ),
            Some(Untraced::Refused) => format!(
                "{base}\n\nIts calls cannot be traced: cargo refuses a package that \
                 sits inside this project's workspace without being listed in it, so \
                 rust-analyzer cannot load it. Add \"{}\" to `exclude` under \
                 [workspace] in Cargo.toml, then restart the analyzer.",
                lib.dir
            ),
        };
    }
}

/// Build the graph from main.rs plus the user source files
/// (`(rel_path, content)`, as in `project_tree.user_src_files`).
pub fn build_graph(main_rs: &str, user_files: &[(String, String)]) -> ModuleGraph {
    build_graph_with(main_rs, user_files, &CrateLinks::default())
}

/// [`build_graph`] with the manifests' answer to which local crates each
/// crate reaches - see [`CrateLinks`]. Without it (the plain form) any folder
/// is reachable by its name, which is the old rule.
pub fn build_graph_with(
    main_rs: &str,
    user_files: &[(String, String)],
    links: &CrateLinks,
) -> ModuleGraph {
    // ── Nodes ─────────────────────────────────────────────────────────────
    // `file_rel` is project-root-relative, like every other path the app
    // handles — the Structure tab feeds it straight to the LSP error lookup and
    // to click-to-open, so a bare "main.rs" would resolve to nothing.
    let mut nodes = vec![make_node(
        String::new(),
        "main",
        "src/main.rs",
        None,
        main_rs,
    )];
    for (i, (rel, content)) in user_files.iter().enumerate() {
        if !rel.ends_with(".rs") {
            continue; // defensive: only Rust files become modules
        }
        let path = module_path_of(rel);
        if path.is_empty() {
            continue; // a bare "mod.rs" at src/ root — not a module
        }
        let name = path.rsplit("::").next().unwrap_or(&path).to_owned();
        nodes.push(make_node(path, &name, rel, Some(i), content));
    }

    // Path → node index (for edge resolution).
    let by_path: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.path.clone(), i))
        .collect();

    // The crate of every node, by index - what `resolve` fences hits with.
    let krates: Vec<String> = nodes.iter().map(|n| n.krate.clone()).collect();

    // ── Edges ─────────────────────────────────────────────────────────────
    let mut deps: HashSet<(usize, usize)> = HashSet::new();
    let mut contains: HashSet<(usize, usize)> = HashSet::new();
    let texts: Vec<(usize, &str)> = std::iter::once((0usize, main_rs))
        .chain(nodes.iter().skip(1).map(|n| {
            let idx = by_path[&n.path];
            (idx, user_files[n.file.unwrap()].1.as_str())
        }))
        .collect();

    for (idx, text) in texts {
        let cur_path = nodes[idx].path.clone();
        let krate = nodes[idx].krate.clone();
        let link = links.link_crate(&krate, &nodes[idx].file_rel);
        let blanked = crate::rust_lex::blank_non_code(text);
        for line in blanked.lines() {
            // `mod child;` → containment edge.
            if let Some(name) = mod_decl(line) {
                let child_path = if cur_path.is_empty() {
                    name.clone()
                } else {
                    format!("{cur_path}::{name}")
                };
                if let Some(&child) = by_path.get(&child_path) {
                    if child != idx {
                        contains.insert((idx, child));
                    }
                }
            }

            // Path chains (`use crate::a::b`, `super::x::y`, `a::b::c(...)`).
            for chain in scan_chains(line) {
                if let Some(target) =
                    resolve(&chain, &cur_path, &krate, &link, &by_path, links, &krates)
                {
                    if target != idx {
                        deps.insert((idx, target));
                    }
                }
            }
        }
    }

    // A dep edge that duplicates a containment edge adds only clutter — the
    // dashed containment line already links the pair.
    let deps: Vec<(usize, usize)> = {
        let mut v: Vec<_> = deps.into_iter().filter(|e| !contains.contains(e)).collect();
        v.sort_unstable();
        v
    };
    let contains: Vec<(usize, usize)> = {
        let mut v: Vec<_> = contains.into_iter().collect();
        v.sort_unstable();
        v
    };

    ModuleGraph {
        nodes,
        deps,
        contains,
    }
}

fn make_node(
    path: String,
    name: &str,
    file_rel: &str,
    file: Option<usize>,
    content: &str,
) -> ModuleNode {
    let (fn_count, ty_count, symbols, impl_spans) = scan_items(content);
    ModuleNode {
        krate: split_crate(file_rel).0,
        path,
        name: name.to_owned(),
        file_rel: file_rel.to_owned(),
        file,
        fn_count,
        ty_count,
        symbols,
        impl_spans,
        is_external: false,
        detached: false,
        untraced: None,
        note: String::new(),
    }
}

/// Append GHOST nodes for every EXTERNAL crate the project uses (std / core /
/// the HAL / …) plus a dep edge from each using module. Detection: a bare
/// path chain (`cortex_m::asm::nop`, `core::str::from_utf8`) whose first
/// segment is lowercase, has ≥ 2 segments, and resolves to NO local module —
/// uppercase firsts (types/variants like `State::Idle`) are skipped, and
/// `crate::`/`super::`/`self::` chains are local by definition.
pub fn add_external_nodes(graph: &mut ModuleGraph, main_rs: &str, user_files: &[(String, String)]) {
    add_external_nodes_with(graph, main_rs, user_files, &CrateLinks::default());
}

/// What an external node's hover adds: the registry version the firmware asks
/// for, and - the case that used to HIDE the node - a local folder of the same
/// name that the code does not use.
fn external_note(links: &CrateLinks, name: &str, local_dirs: &HashMap<String, String>) -> String {
    let mut lines = Vec::new();
    if let Some(v) = links.registry.get(&name.replace('-', "_")) {
        lines.push(format!("crates.io {v}"));
    }
    if let Some(dir) = local_dirs.get(name) {
        lines.push(format!(
            "A local copy is in {dir}/ - not what this code uses: the manifest does not link it."
        ));
    }
    lines.join("\n")
}

/// [`add_external_nodes`] with the manifests' answer to which local crates
/// each crate reaches - see [`CrateLinks`]. A folder named like a registry
/// dependency no longer hides that dependency's node.
pub fn add_external_nodes_with(
    graph: &mut ModuleGraph,
    main_rs: &str,
    user_files: &[(String, String)],
    links: &CrateLinks,
) {
    let by_path: HashMap<String, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.is_external)
        .map(|(i, n)| (n.path.clone(), i))
        .collect();
    // Any local module NAME disqualifies a chain head: `utils::checksum()`
    // after a `use super::utils;` doesn't resolve absolutely, but "utils" is a
    // sibling module — without this it became a phantom external crate.
    //
    // Per CRATE when that crate's manifest is known: a bare head is a module
    // of the chain's own crate or of one it LINKS (`use mylib::parser;` then
    // `parser::decode()`) - never of a crate it does not link, which is how a
    // detached library's root name used to hide the registry crate.
    let local_names: HashSet<(String, String)> = graph
        .nodes
        .iter()
        .filter(|n| !n.is_external)
        .map(|n| (n.krate.clone(), n.name.clone()))
        .collect();
    let all_names: HashSet<&str> = local_names.iter().map(|(_, n)| n.as_str()).collect();
    let krates: Vec<String> = graph.nodes.iter().map(|n| n.krate.clone()).collect();
    // Where each local library lives, for the note on a same-named external.
    let local_dirs: HashMap<String, String> = graph
        .nodes
        .iter()
        .filter(|n| !n.is_external && !n.krate.is_empty())
        .filter_map(|n| {
            n.file_rel
                .split_once("/src/")
                .map(|(d, _)| (n.krate.clone(), d.to_owned()))
        })
        .collect();
    let mut extern_idx: HashMap<String, usize> = HashMap::new();
    let mut dep_set: HashSet<(usize, usize)> = graph.deps.iter().copied().collect();

    let texts: Vec<(usize, &str)> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.is_external)
        .map(|(i, n)| {
            let text = match n.file {
                None => main_rs,
                Some(fi) => user_files.get(fi).map(|(_, c)| c.as_str()).unwrap_or(""),
            };
            (i, text)
        })
        .collect();

    for (idx, text) in texts {
        let cur_path = graph.nodes[idx].path.clone();
        let krate = graph.nodes[idx].krate.clone();
        let link = links.link_crate(&krate, &graph.nodes[idx].file_rel);
        for raw_line in text.lines() {
            let line = raw_line.split("//").next().unwrap_or("");
            for chain in scan_chains(line) {
                if matches!(chain[0].as_str(), "crate" | "super" | "self") {
                    continue; // local by definition
                }
                if resolve(&chain, &cur_path, &krate, &link, &by_path, links, &krates).is_some() {
                    continue; // resolves to a project module
                }
                let first = &chain[0];
                if !first.chars().next().is_some_and(|c| c.is_lowercase()) {
                    continue; // `State::Idle`, `Vec::new`, … — not a crate
                }
                let sibling = if links.knows(&link) {
                    links
                        .reachable(&link)
                        .iter()
                        .any(|k| local_names.contains(&((*k).to_owned(), first.clone())))
                } else {
                    all_names.contains(first.as_str())
                };
                if sibling {
                    continue; // sibling-module reference, not an extern crate
                }
                let eidx = *extern_idx.entry(first.clone()).or_insert_with(|| {
                    let id = graph.nodes.len();
                    graph.nodes.push(ModuleNode {
                        path: first.clone(),
                        name: first.clone(),
                        file_rel: format!("[extern] {first}"),
                        krate: String::new(),
                        file: None,
                        fn_count: 0,
                        ty_count: 0,
                        symbols: Vec::new(),
                        impl_spans: Vec::new(),
                        is_external: true,
                        detached: false,
                        untraced: None,
                        note: external_note(links, first, &local_dirs),
                    });
                    id
                });
                if idx != eidx && dep_set.insert((idx, eidx)) {
                    graph.deps.push((idx, eidx));
                }
            }
        }
    }
}

/// Count `fn` and `struct`/`enum`/`trait` item lines (badge-grade accuracy)
/// and collect the TOP-LEVEL ones with their line spans.
///
/// "Top-level" = the declaration appears at **brace depth 0**, and the item's
/// span runs until the depth returns to 0 (its closing brace) — never by
/// indentation, which real code violates (column-0 statements inside `fn
/// main`). Items inside `impl`/`mod` blocks sit at depth ≥ 1, so the badge
/// counts them but the symbol list skips them. Braces inside comments and
/// string literals are blanked out first (`rust_lex::blank_non_code`), so they
/// cannot skew the depth.
fn scan_items(text: &str) -> (usize, usize, Vec<SymbolItem>, Vec<(usize, usize, usize)>) {
    let mut fns = 0;
    let mut tys = 0;
    let mut symbols: Vec<SymbolItem> = Vec::new();
    // Depth-0 impl blocks: (start, end, implemented-type NAME) — resolved to
    // symbol rows after the scan (the type may be declared below its impl).
    let mut pending_impls: Vec<(usize, usize, Option<String>)> = Vec::new();
    let mut depth: i32 = 0;
    // The depth-0 block the scan is currently inside — a symbol item or an
    // `impl` — and whether its opening `{` has been seen yet (multi-line
    // signatures put it lines later; `struct Foo;` never opens one and closes
    // at the `;`).
    let mut open_sym: Option<usize> = None;
    let mut open_impl: Option<(usize, Option<String>, usize)> = None;
    let mut entered_body = false;
    // Comments and string literals blanked to SPACES, so the brace depth below
    // counts only real braces while every column stays where it was (`col` is
    // computed from the line's length).
    //
    // `split("//")` handled one of the three cases. The other two were not
    // noise: a `{` inside a `/* … */` pinned the depth at >= 1 for the rest of
    // the file, so every later item stopped being seen at depth 0 and vanished
    // from the diagram entirely.
    let blanked = crate::rust_lex::blank_non_code(text);
    for (li, line) in blanked.lines().enumerate() {
        let line: &str = line;
        let t = strip_modifiers(line.trim_start());
        let (kind, rest) = if let Some(r) = t.strip_prefix("fn ") {
            fns += 1;
            (Some(SymKind::Fn), r)
        } else if let Some(r) = t.strip_prefix("struct ") {
            tys += 1;
            (Some(SymKind::Struct), r)
        } else if let Some(r) = t.strip_prefix("enum ") {
            tys += 1;
            (Some(SymKind::Enum), r)
        } else if let Some(r) = t.strip_prefix("trait ") {
            tys += 1;
            (Some(SymKind::Trait), r)
        } else {
            (None, t)
        };
        if depth == 0 && open_sym.is_none() && open_impl.is_none() {
            if let Some(kind) = kind {
                let name: String = rest
                    .chars()
                    .take_while(|&c| c.is_alphanumeric() || c == '_')
                    .collect();
                if !name.is_empty() {
                    // Column of the name = chars before `rest` on the raw line
                    // (prefixes are ASCII, so chars == UTF-16 units).
                    let col = line.chars().count() - rest.chars().count();
                    open_sym = Some(symbols.len());
                    symbols.push(SymbolItem {
                        name,
                        kind,
                        line: li + 1,
                        end_line: li + 1,
                        col,
                    });
                }
            } else if let Some(r) = t.strip_prefix("impl") {
                // Word boundary: `impl` / `impl<…>` / `impl Foo`, not `implxyz`.
                if r.starts_with(|c: char| !c.is_alphanumeric() && c != '_') || r.is_empty() {
                    open_impl = Some((li + 1, impl_target_name(r), li + 1));
                }
            }
        }
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth = (depth - 1).max(0),
                _ => {}
            }
        }
        if open_sym.is_some() || open_impl.is_some() {
            if let Some(row) = open_sym {
                symbols[row].end_line = li + 1;
            }
            if let Some((_, _, end)) = &mut open_impl {
                *end = li + 1;
            }
            if !entered_body && line.contains('{') {
                entered_body = true;
            }
            if entered_body {
                if depth == 0 {
                    // The block's closing brace was reached on this line.
                    if let Some((s, name, e)) = open_impl.take() {
                        pending_impls.push((s, e, name));
                    }
                    open_sym = None;
                    entered_body = false;
                }
            } else if depth == 0 && line.contains(';') {
                open_sym = None; // bodyless item: `struct Foo;` / `struct P(u8);`
                open_impl = None;
            }
        }
    }
    // An unclosed impl at EOF (mid-typing) still gets its span so far.
    if let Some((s, name, e)) = open_impl.take() {
        pending_impls.push((s, e, name));
    }
    // Resolve impl targets to symbol rows (unresolved → dropped: the type
    // lives elsewhere, e.g. `impl Display for ExternalType`).
    let impl_spans: Vec<(usize, usize, usize)> = pending_impls
        .into_iter()
        .filter_map(|(s, e, name)| {
            let name = name?;
            let row = symbols.iter().position(|sym| sym.name == name)?;
            Some((s, e, row))
        })
        .collect();
    (fns, tys, symbols, impl_spans)
}

/// The NAME of the type an `impl` line implements — `rest` is everything after
/// the `impl` keyword. Handles generics and trait impls by scanning at
/// angle-bracket depth 0: `<const N: usize> Parser<N> {` → `Parser`,
/// ` Frame for HmmdFrame {` → `HmmdFrame`, ` fmt::Display for Config` →
/// `Config` (last path segment; `impl Trait for Type` targets the TYPE).
pub(crate) fn impl_target_name(rest: &str) -> Option<String> {
    // Flatten to the angle-depth-0 text, stopping at the body brace.
    let mut flat = String::new();
    let mut depth = 0i32;
    for c in rest.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = (depth - 1).max(0),
            '{' if depth == 0 => break,
            _ if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    let flat = flat.split(" where").next().unwrap_or(&flat).trim();
    // `impl Trait for Type` → the part after the LAST ` for `; else the whole.
    let target = flat.rsplit(" for ").next().unwrap_or(flat).trim();
    // Drop reference/mut sugar (`&mut Foo`), then the first token's last
    // path segment, identifier chars only.
    let target = target.trim_start_matches('&').trim_start();
    let target = target.strip_prefix("mut ").unwrap_or(target).trim_start();
    let token = target.split_whitespace().next()?;
    let seg = token.rsplit("::").next().unwrap_or(token);
    let name: String = seg
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Strip leading visibility / item modifiers so `pub async fn` matches `fn `.
fn strip_modifiers(mut s: &str) -> &str {
    loop {
        let before = s;
        for p in [
            "pub(crate) ",
            "pub(super) ",
            "pub(in crate) ",
            "pub ",
            "async ",
            "unsafe ",
            "const ",
            "extern \"C\" ",
        ] {
            if let Some(rest) = s.strip_prefix(p) {
                s = rest;
            }
        }
        if s == before {
            return s;
        }
    }
}

/// Detect a file-module declaration: `[pub …] mod name;` → `Some(name)`.
/// Inline `mod name { … }` blocks are skipped (they aren't separate files).
fn mod_decl(line: &str) -> Option<String> {
    let t = strip_modifiers(line.trim_start());
    let rest = t.strip_prefix("mod ")?;
    let name = rest.trim().strip_suffix(';')?.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .then(|| name.to_owned())
}

/// Extract every `ident(::ident)+` chain from a (comment-stripped) line.
/// Chains stop at any non-path character (`(`, `<`, `{`, whitespace, …), so
/// `pins::configs::usart1::init(&mut afio)` yields
/// `[pins, configs, usart1, init]`.
fn scan_chains(line: &str) -> Vec<Vec<String>> {
    let b: Vec<char> = line.chars().collect();
    let is_start = |c: char| c.is_alphabetic() || c == '_';
    let is_cont = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if is_start(b[i]) && (i == 0 || (!is_cont(b[i - 1]) && b[i - 1] != ':')) {
            let mut segs: Vec<String> = Vec::new();
            loop {
                let s = i;
                while i < b.len() && is_cont(b[i]) {
                    i += 1;
                }
                segs.push(b[s..i].iter().collect());
                if i + 2 < b.len() && b[i] == ':' && b[i + 1] == ':' && is_start(b[i + 2]) {
                    i += 2;
                    continue;
                }
                break;
            }
            if segs.len() >= 2 {
                out.push(segs);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Resolve a path chain to a module node — the LONGEST known module path the
/// chain's prefix reaches. `cur_path` is the module the chain appears in.
///
/// Rules (kept intentionally simple):
///   * `crate::rest…`  → absolute; must consume ≥ 1 chain segment.
///   * `super::…::rest` → pop one path segment per `super`; if `rest` reaches a
///     deeper known module use it, otherwise the parent module itself is the
///     target (a `use super::ITEM` IS a dependency on the parent).
///   * `self::rest`    → relative to `cur_path`; must go deeper than it.
///   * bare `a::b…`    → try relative to `cur_path` first (must go deeper),
///     then as a crate-absolute path (covers `pins::configs::usart1::init(…)`
///     written in main.rs).
fn resolve(
    chain: &[String],
    cur_path: &str,
    krate: &str,
    // The crate whose manifest judges this chain - see `CrateLinks::link_crate`.
    link: &str,
    by_path: &HashMap<String, usize>,
    links: &CrateLinks,
    krates: &[String],
) -> Option<usize> {
    let cur_segs: Vec<&str> = if cur_path.is_empty() {
        Vec::new()
    } else {
        cur_path.split("::").collect()
    };
    // Everything above this is another crate — `crate::` lands here and
    // `super::` may not climb past it.
    let root_segs: Vec<&str> = if krate.is_empty() {
        Vec::new()
    } else {
        vec![krate]
    };
    // A hit in ANOTHER crate is only allowed through [`absolute`], which asks
    // the manifest. The firmware's own paths carry no prefix, so from `main`
    // a "relative" lookup IS a lookup of every unprefixed name - a detached
    // library's folder included - and needs the same fence.
    let own = |i: &usize| !links.knows(link) || krates.get(*i).is_some_and(|k| k == krate);

    match chain[0].as_str() {
        "crate" => longest_match(&root_segs, &chain[1..], root_segs.len(), by_path).filter(own),
        "super" => {
            let mut supers = 0;
            while supers < chain.len() && chain[supers] == "super" {
                supers += 1;
            }
            if supers > cur_segs.len().saturating_sub(root_segs.len()) {
                return None; // walked above the crate root
            }
            let base = &cur_segs[..cur_segs.len() - supers];
            let rest = &chain[supers..];
            // Deeper module under the parent, else the parent module itself.
            longest_match(base, rest, base.len(), by_path)
                .or_else(|| by_path.get(&base.join("::")).copied())
                .filter(own)
        }
        "self" => longest_match(&cur_segs, &chain[1..], cur_segs.len(), by_path).filter(own),
        _ => longest_match(&cur_segs, chain, cur_segs.len(), by_path)
            .filter(own)
            .or_else(|| absolute(chain, krate, link, by_path, links, krates)),
    }
}

/// A bare path read from the crate root: this crate's own modules, or another
/// LOCAL crate - but only one this crate's manifest links.
///
/// With no readable manifest it is the old rule: any module, by name.
fn absolute(
    chain: &[String],
    krate: &str,
    link: &str,
    by_path: &HashMap<String, usize>,
    links: &CrateLinks,
    krates: &[String],
) -> Option<usize> {
    if !links.knows(link) {
        return longest_match(&[], chain, 0, by_path);
    }
    // A local crate this one links, under the name its manifest gives it. The
    // head is swapped for the crate's own prefix, so a key that differs from
    // the folder (`radar = { path = "mw_radar" }`) still lands.
    if let Some(target) = links.target(link, &chain[0]) {
        return longest_match(&[target], &chain[1..], 0, by_path);
    }
    // Otherwise only this crate's own modules - never another crate's,
    // whatever it is called. A folder named like a crates.io dependency is
    // not what the code means unless the manifest says so.
    longest_match(&[], chain, 0, by_path).filter(|i| krates.get(*i).is_some_and(|k| k == krate))
}

/// Find the longest known module path among `base ++ chain[..k]` prefixes,
/// requiring the match to consume more than `min_len` total segments (so a
/// relative lookup can't "resolve" to the module it already sits in).
fn longest_match(
    base: &[&str],
    chain: &[String],
    min_len: usize,
    by_path: &HashMap<String, usize>,
) -> Option<usize> {
    let mut segs: Vec<&str> = base.to_vec();
    segs.extend(chain.iter().map(String::as_str));
    for len in (min_len + 1..=segs.len()).rev() {
        let candidate = segs[..len].join("::");
        if let Some(&idx) = by_path.get(&candidate) {
            return Some(idx);
        }
    }
    None
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_path_conversion() {
        assert_eq!(module_path_of("foo/bar.rs"), "foo::bar");
        assert_eq!(module_path_of("foo/mod.rs"), "foo");
        assert_eq!(module_path_of("utils.rs"), "utils");
        assert_eq!(module_path_of("a/b/c.rs"), "a::b::c");
    }

    /// Names of every top-level symbol the crate-root node lists.
    fn root_symbols(main_rs: &str) -> Vec<String> {
        build_graph(main_rs, &[]).nodes[0]
            .symbols
            .iter()
            .map(|s| s.name.clone())
            .collect()
    }

    /// The measured failure: a `{` inside a block comment pinned the brace
    /// depth at >= 1 for the rest of the file, so nothing after it was ever
    /// seen at depth 0 and it vanished from the diagram entirely. The comment
    /// strip only handled `//`.
    #[test]
    fn a_brace_inside_a_block_comment_does_not_swallow_later_items() {
        let src = "pub fn first() {}
                   /* prose with an unclosed { in it */
                   pub fn second() {}
";
        assert_eq!(root_symbols(src), ["first", "second"]);
    }

    /// The same for a brace inside a string literal, and for a `//` comment
    /// that follows code on the same line (the one case the old strip caught).
    #[test]
    fn braces_in_strings_and_trailing_comments_do_not_skew_the_depth() {
        let src = "pub fn first() {}
                   pub const OPEN: &str = \"{\";
                   pub fn second() {} // trailing }
                   pub fn third() {}
";
        // `const` is not a tracked `SymKind`, so OPEN is deliberately absent —
        // what matters is that its brace-bearing string did not hide the two
        // functions after it.
        assert_eq!(root_symbols(src), ["first", "second", "third"]);
    }

    /// A `fn` written inside a comment is prose, not an item — blanking must
    /// not turn the diagram into a list of things the reader only described.
    #[test]
    fn an_item_written_inside_a_comment_is_not_listed() {
        let src = "pub fn real() {}
                   // pub fn imaginary() {}
                   /* pub fn also_imaginary() {} */
";
        assert_eq!(root_symbols(src), ["real"]);
    }

    fn sample() -> (String, Vec<(String, String)>) {
        let main_rs = "\
mod pins;
mod mw_radar;
use crate::mw_radar::read_report::HmmdFrame;
fn main() {
    pins::configs::usart1::init();
}
";
        let files = vec![
            (
                "pins/mod.rs".into(),
                "pub mod configs;\npub fn setup() {}\n".into(),
            ),
            ("pins/configs/mod.rs".into(), "pub mod usart1;\n".into()),
            ("pins/configs/usart1.rs".into(), "pub fn init() {}\n".into()),
            (
                "mw_radar/mod.rs".into(),
                "pub mod read_report;\npub mod utils;\npub struct Parser;\n".into(),
            ),
            (
                "mw_radar/read_report.rs".into(),
                "use super::utils::checksum;\npub struct HmmdFrame;\n".into(),
            ),
            (
                "mw_radar/utils.rs".into(),
                "pub fn checksum() -> u8 { 0 }\n".into(),
            ),
        ];
        (main_rs.to_owned(), files)
    }

    fn idx(g: &ModuleGraph, path: &str) -> usize {
        g.nodes.iter().position(|n| n.path == path).unwrap()
    }

    /// The Structure tab, against the code the IDE really generates.
    ///
    /// The tab has no per-chip anything - it parses Rust - so what this checks
    /// is that the parser survives the shape of a generated `main.rs`, for
    /// every family at once.
    #[test]
    fn every_bundled_chip_draws_a_structure_diagram() {
        use crate::panels::mcu_module::builtins::builtin_definitions;

        for d in builtin_definitions() {
            let mcu = d.build_mcu();
            let main_rs = mcu.fresh_main_rs();
            let g = build_graph(&main_rs, &[]);

            let root = g
                .nodes
                .first()
                .unwrap_or_else(|| panic!("{}: no root node", d.id));
            let names: Vec<&str> = root.symbols.iter().map(|s| s.name.as_str()).collect();
            assert!(
                names.contains(&"main"),
                "{}: the entry point is not in the diagram: {names:?}",
                d.id
            );

            // Without this the generated `pins::configs::*` modules are drawn
            // as orphans - and the project does not compile either. A missing
            // `pub mod pins;` has already shipped once, in the two embassy
            // headers, so it is worth asserting rather than assuming.
            assert!(
                main_rs.lines().any(|l| {
                    let t = l.trim();
                    t == "mod pins;" || t == "pub mod pins;"
                }),
                "{}: main.rs never declares the pins module",
                d.id
            );

            // Statements inside `fn main` sit at COLUMN 0 in generated code -
            // everything between the GENERATED markers is unindented. An item
            // scan keyed on indentation would end `main` at the first of them
            // and orphan every call site after it, which is exactly how call
            // edges vanished before `enclosing_row` became brace-based.
            let main_row = root
                .symbols
                .iter()
                .position(|s| s.name == "main")
                .expect("checked above");
            let lines: Vec<&str> = main_rs.lines().collect();
            let closing = lines
                .iter()
                .rposition(|l| l.trim() == "}")
                .map(|i| i + 1)
                .unwrap_or_else(|| panic!("{}: main.rs does not end in a block", d.id));
            assert_eq!(
                root.enclosing_row(closing),
                Some(main_row),
                "{}: the end of main.rs is not attributed to `main` - depth tracking broke",
                d.id
            );
        }
    }

    #[test]
    fn builds_nodes_and_containment() {
        let (main_rs, files) = sample();
        let g = build_graph(&main_rs, &files);
        assert_eq!(g.nodes.len(), 7); // main + 6 files
        assert_eq!(g.nodes[0].name, "main");
        let main = 0;
        let pins = idx(&g, "pins");
        let configs = idx(&g, "pins::configs");
        let usart1 = idx(&g, "pins::configs::usart1");
        let mw = idx(&g, "mw_radar");
        let rr = idx(&g, "mw_radar::read_report");
        let ut = idx(&g, "mw_radar::utils");
        for e in [
            (main, pins),
            (main, mw),
            (pins, configs),
            (configs, usart1),
            (mw, rr),
            (mw, ut),
        ] {
            assert!(g.contains.contains(&e), "missing containment {e:?}");
        }
    }

    #[test]
    fn resolves_dependency_edges() {
        let (main_rs, files) = sample();
        let g = build_graph(&main_rs, &files);
        let main = 0;
        let usart1 = idx(&g, "pins::configs::usart1");
        let rr = idx(&g, "mw_radar::read_report");
        let ut = idx(&g, "mw_radar::utils");
        // use crate::mw_radar::read_report::HmmdFrame → main → read_report
        assert!(g.deps.contains(&(main, rr)));
        // inline pins::configs::usart1::init() → main → usart1 (deepest match)
        assert!(g.deps.contains(&(main, usart1)));
        // use super::utils::checksum → read_report → utils
        assert!(g.deps.contains(&(rr, ut)));
        // no self-edges, no dep duplicating a containment edge
        assert!(g.deps.iter().all(|(a, b)| a != b));
        assert!(g.deps.iter().all(|e| !g.contains.contains(e)));
    }

    #[test]
    fn use_super_item_depends_on_parent_module() {
        let main_rs = "mod a;\n".to_owned();
        let files = vec![
            (
                "a/mod.rs".into(),
                "pub mod b;\npub const K: u8 = 1;\n".into(),
            ),
            ("a/b.rs".into(), "use super::K;\n".into()),
        ];
        let g = build_graph(&main_rs, &files);
        let a = idx(&g, "a");
        let b = idx(&g, "a::b");
        assert!(
            g.deps.contains(&(b, a)),
            "super::ITEM should point at parent"
        );
    }

    #[test]
    fn ignores_external_crates_and_counts_items() {
        let main_rs = "use cortex_m::asm;\nfn main() { cortex_m::asm::nop(); }\n";
        let g = build_graph(main_rs, &[]);
        assert!(
            g.deps.is_empty(),
            "external crate paths must not create edges"
        );
        assert_eq!(g.nodes[0].fn_count, 1);
    }

    #[test]
    fn extracts_top_level_symbols_with_lines() {
        let text = "\
pub struct Parser<const N: usize> {
    len: usize,
}
pub enum State { Idle, Busy }
impl Parser<8> {
    pub fn feed(&mut self, b: u8) {}
}
pub fn checksum(data: &[u8]) -> u8 { 0 }
trait Frame {}
";
        let g = build_graph("mod a;\n", &[("a.rs".into(), text.into())]);
        let a = &g.nodes[1];
        let names: Vec<(&str, SymKind, usize)> = a
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.line))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Parser", SymKind::Struct, 1),
                ("State", SymKind::Enum, 4),
                ("checksum", SymKind::Fn, 8),
                ("Frame", SymKind::Trait, 9),
            ],
            "top-level items only — the indented `fn feed` method is excluded"
        );
        // …but the badge still counts the method.
        assert_eq!(a.fn_count, 2);
        assert_eq!(a.ty_count, 3);
    }

    /// External-crate ghosts: lowercase unresolved chain heads become extern
    /// nodes with dep edges; local types, sibling modules and `crate::` paths
    /// don't.
    #[test]
    fn external_nodes_detected_without_phantoms() {
        let main_rs = "\
mod a;
use cortex_m::asm;
fn main() {
    core::str::from_utf8(&[]);
    a::helper();
}
";
        let files = vec![(
            "a.rs".into(),
            "pub fn helper() {}\npub struct Cfg;\nfn f() { Cfg::default(); }\n".into(),
        )];
        let mut g = build_graph(&main_rs, &files);
        let before = g.nodes.len();
        add_external_nodes(&mut g, &main_rs, &files);
        let externs: Vec<&str> = g.nodes[before..].iter().map(|n| n.name.as_str()).collect();
        assert!(externs.contains(&"cortex_m"), "use-line crate: {externs:?}");
        assert!(externs.contains(&"core"), "inline crate path: {externs:?}");
        assert!(!externs.contains(&"a"), "local module must not ghost");
        assert!(!externs.contains(&"Cfg"), "uppercase type must not ghost");
        // main (node 0) depends on both externs.
        let cm = g.nodes.iter().position(|n| n.name == "cortex_m").unwrap();
        assert!(g.deps.contains(&(0, cm)));
        assert!(g.nodes[cm].is_external && g.nodes[cm].symbols.is_empty());
    }

    /// Call sites inside `impl` methods must attribute to the implemented
    /// TYPE's row — modules keeping their logic in impl methods (most library
    /// code) previously produced no outgoing call edges at all.
    #[test]
    fn impl_method_sites_attribute_to_the_type_row() {
        let text = "\
pub struct Parser {
    len: usize,
}
impl Parser {
    pub fn feed(&mut self, b: u8) {
        helper(b);
    }
}
pub struct HmmdFrame;
impl Frame for HmmdFrame {
    fn decode(&self) {
        helper(0);
    }
}
impl core::fmt::Display for External {
    fn fmt(&self) {}
}
";
        let g = build_graph(text, &[]);
        let node = &g.nodes[0];
        let parser = node
            .symbols
            .iter()
            .position(|s| s.name == "Parser")
            .unwrap();
        let frame = node
            .symbols
            .iter()
            .position(|s| s.name == "HmmdFrame")
            .unwrap();
        // Site inside `impl Parser` (line 6) → the Parser row.
        assert_eq!(node.enclosing_row(6), Some(parser));
        // Site inside `impl Frame for HmmdFrame` (line 12) → the TYPE's row.
        assert_eq!(node.enclosing_row(12), Some(frame));
        // Impl on an external type resolves to no local row → dropped.
        assert_eq!(node.enclosing_row(16), None);
    }

    #[test]
    fn impl_target_name_shapes() {
        assert_eq!(impl_target_name(" Parser {").as_deref(), Some("Parser"));
        assert_eq!(
            impl_target_name("<const N: usize> Parser<N> {").as_deref(),
            Some("Parser")
        );
        assert_eq!(
            impl_target_name(" Frame for HmmdFrame {").as_deref(),
            Some("HmmdFrame")
        );
        assert_eq!(
            impl_target_name(" fmt::Display for Config {").as_deref(),
            Some("Config")
        );
        assert_eq!(
            impl_target_name("<'a> Iterator for &mut Cursor<'a> where Self: Sized {").as_deref(),
            Some("Cursor")
        );
    }

    /// Regression (user report): statements written at COLUMN 0 inside `fn
    /// main` plus call sites inside a closure. The old column-0 anchor rule
    /// treated `let x = 1;` / `radar.read_data(|rx| {` as anonymous item
    /// boundaries, so every site after them mapped to no symbol and its call
    /// edge vanished. Brace-depth spans are immune to indentation.
    #[test]
    fn spans_survive_column0_statements_and_closures() {
        let text = "\
#[entry]
fn main() {
let x = 1;
radar.read_data(|rx| {
    helper();
});
}
pub fn other(
    a: u8,
) -> u8 {
    a
}
";
        let g = build_graph(text, &[]);
        let node = &g.nodes[0];
        let names: Vec<(&str, usize, usize)> = node
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.line, s.end_line))
            .collect();
        assert_eq!(
            names,
            vec![("main", 2, 7), ("other", 8, 12)],
            "col-0 statements must not end main's span; multi-line signature spans to its brace"
        );
        assert_eq!(
            node.enclosing_row(5),
            Some(0),
            "closure site belongs to main"
        );
        assert_eq!(
            node.enclosing_row(3),
            Some(0),
            "col-0 statement belongs to main"
        );
        assert_eq!(
            node.enclosing_row(11),
            Some(1),
            "body of the multi-line-sig fn"
        );
        assert_eq!(
            node.enclosing_row(1),
            None,
            "the attr line is outside any item"
        );
    }

    #[test]
    fn cyclic_uses_do_not_break_the_graph() {
        let main_rs = "mod a;\nmod b;\n".to_owned();
        let files = vec![
            ("a.rs".into(), "use crate::b::f;\n".into()),
            ("b.rs".into(), "use crate::a::g;\n".into()),
        ];
        let g = build_graph(&main_rs, &files);
        let a = idx(&g, "a");
        let b = idx(&g, "b");
        assert!(g.deps.contains(&(a, b)) && g.deps.contains(&(b, a)));
    }

    /// An extracted library's `lib.rs` IS that crate's root, so `mod data;`
    /// inside it must link to the `data` node. Regression: `lib.rs` was treated
    /// as a module named `lib`, so `mod data;` resolved to `lib::data` — which
    /// does not exist — and the whole library rendered as a `lib` node floating
    /// unconnected next to its own modules.
    #[test]
    fn library_lib_rs_is_the_crate_root_and_owns_its_modules() {
        let files = vec![
            (
                "mw_radar/src/lib.rs".to_string(),
                "#![no_std]\npub mod data;\npub mod radar;\n".to_string(),
            ),
            (
                "mw_radar/src/data.rs".to_string(),
                "pub enum ParameterID { A }\n".to_string(),
            ),
            (
                "mw_radar/src/radar.rs".to_string(),
                "use crate::data::ParameterID;\npub fn go() {}\n".to_string(),
            ),
        ];
        let g = build_graph("fn main() {}", &files);

        let root = idx(&g, "mw_radar");
        let data = idx(&g, "mw_radar::data");
        let radar = idx(&g, "mw_radar::radar");
        assert!(
            g.contains.contains(&(root, data)) && g.contains.contains(&(root, radar)),
            "lib.rs must own its modules, got {:?}",
            g.contains
        );
        // `crate::` inside a library means THAT library, not the firmware.
        assert!(
            g.deps.contains(&(radar, data)),
            "use crate::data must resolve inside the library, got {:?}",
            g.deps
        );
    }

    /// A module path is relative to its CRATE root, not the project root.
    /// Regression: after paths became project-root-relative, every node kept a
    /// leading `src` segment — and the diagram colours packages by that first
    /// segment, so the whole graph turned one colour.
    #[test]
    fn module_path_strips_the_crate_source_root() {
        // The firmware is the graph's root crate — bare module paths.
        assert_eq!(module_path_of("src/app.rs"), "app");
        assert_eq!(
            module_path_of("src/pins/configs/usart1.rs"),
            "pins::configs::usart1"
        );
        assert_eq!(module_path_of("src/pins/mod.rs"), "pins");
        // A library crate is namespaced under its own name — which is both how
        // you reach it from the firmware and what makes its `mod x;` resolve.
        assert_eq!(module_path_of("mw_radar/src/lib.rs"), "mw_radar");
        assert_eq!(module_path_of("mw_radar/src/frame.rs"), "mw_radar::frame");
        assert_eq!(
            module_path_of("mw_radar/src/proto/mod.rs"),
            "mw_radar::proto"
        );
        // crates.io allows `-`; a module path does not.
        assert_eq!(module_path_of("mw-radar/src/frame.rs"), "mw_radar::frame");
    }

    /// Different packages must stay distinguishable by their first segment —
    /// that is exactly what the colour palette keys on.
    #[test]
    fn first_segment_identifies_the_package() {
        let first = |p: &str| module_path_of(p).split("::").next().unwrap().to_owned();
        assert_eq!(first("src/pins/mod.rs"), "pins");
        assert_ne!(first("src/pins/mod.rs"), first("src/drivers/uart.rs"));
    }
}

#[cfg(test)]
mod crate_link_tests {
    use super::*;

    fn files(extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = vec![
            (
                "mylib/Cargo.toml".into(),
                "[package]\nname = \"mylib\"\n".into(),
            ),
            (
                "mylib/src/lib.rs".into(),
                "pub mod a;\npub mod b;\npub fn run() {}\n".into(),
            ),
            (
                "mylib/src/a.rs".into(),
                "pub fn go() { crate::b::x(); }\n".into(),
            ),
            ("mylib/src/b.rs".into(), "pub fn x() {}\n".into()),
        ];
        v.extend(
            extra
                .iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned())),
        );
        v
    }

    fn at(g: &ModuleGraph, path: &str) -> usize {
        g.nodes
            .iter()
            .position(|n| n.path == path && !n.is_external)
            .unwrap_or_else(|| panic!("no node {path}"))
    }

    const MAIN: &str = "fn main() { mylib::run(); }\n";

    /// The reported case: the firmware takes `mylib` from crates.io, and a
    /// detached copy with the same name sits in the project. The code means
    /// the registry crate, so no arrow reaches the local folder - while the
    /// library's own structure is untouched.
    #[test]
    fn a_same_named_detached_folder_is_not_what_main_uses() {
        let root = "[package]\nname = \"fw\"\n\n[dependencies]\nmylib = \"0.1.0\"\n";
        let f = files(&[]);
        let links = CrateLinks::from_manifests(root, &f);
        let g = build_graph_with(MAIN, &f, &links);
        let lib = at(&g, "mylib");
        assert!(!g.deps.contains(&(0, lib)), "{:?}", g.deps);
        assert!(g.deps.iter().all(|(from, _)| *from != 0), "{:?}", g.deps);
        // Inside the library nothing changed.
        assert!(g.contains.contains(&(lib, at(&g, "mylib::a"))));
        assert!(
            g.deps.contains(&(at(&g, "mylib::a"), at(&g, "mylib::b"))),
            "crate::b::x()"
        );
    }

    #[test]
    fn a_path_dependency_is_what_main_uses() {
        let root = "[package]\nname = \"fw\"\n\n[dependencies]\nmylib = { path = \"mylib\" }\n";
        let f = files(&[]);
        let g = build_graph_with(MAIN, &f, &CrateLinks::from_manifests(root, &f));
        assert!(g.deps.contains(&(0, at(&g, "mylib"))));
    }

    /// The dependency key is the name the code writes; the folder can be
    /// called anything. Resolving by folder name missed this one entirely.
    #[test]
    fn a_key_that_differs_from_the_folder_still_resolves() {
        let root = "[package]\nname = \"fw\"\n\n[dependencies]\nradar = { path = \"./mylib/\" }\n";
        let f = files(&[]);
        let main = "fn main() { radar::a::go(); }\n";
        let g = build_graph_with(main, &f, &CrateLinks::from_manifests(root, &f));
        assert!(g.deps.contains(&(0, at(&g, "mylib::a"))), "{:?}", g.deps);
    }

    /// With externals on, the crates.io crate appears as the external it is -
    /// no longer hidden behind the local folder's name - and says so.
    #[test]
    fn the_registry_crate_shows_beside_its_detached_copy() {
        let root = "[package]\nname = \"fw\"\n\n[dependencies]\nmylib = \"0.1.0\"\n";
        let f = files(&[]);
        let links = CrateLinks::from_manifests(root, &f);
        let mut g = build_graph_with(MAIN, &f, &links);
        add_external_nodes_with(&mut g, MAIN, &f, &links);
        let ext = g
            .nodes
            .iter()
            .position(|n| n.is_external && n.name == "mylib")
            .expect("an external node for the registry crate");
        assert!(g.deps.contains(&(0, ext)));
        assert!(
            g.nodes[ext].note.contains("crates.io 0.1.0"),
            "{}",
            g.nodes[ext].note
        );
        assert!(
            g.nodes[ext].note.contains("mylib/"),
            "{}",
            g.nodes[ext].note
        );
    }

    /// A manifest that does not parse must not cost the diagram its edges:
    /// the old name-only rule stands in.
    #[test]
    fn an_unreadable_manifest_keeps_the_old_rule() {
        let f = files(&[]);
        let g = build_graph_with(MAIN, &f, &CrateLinks::from_manifests("not [toml", &f));
        assert!(g.deps.contains(&(0, at(&g, "mylib"))));
    }

    /// Found on the way: a library's bare path landed on a FIRMWARE module of
    /// the same name. A library cannot name the firmware at all.
    #[test]
    fn a_library_does_not_reach_firmware_modules() {
        let root = "[package]\nname = \"fw\"\n\n[dependencies]\nmylib = { path = \"mylib\" }\n";
        let f = files(&[
            ("pins/mod.rs", "pub fn setup() {}\n"),
            ("mylib/src/c.rs", "pub fn y() { pins::setup(); }\n"),
        ]);
        let g = build_graph_with(
            "mod pins;\nfn main() {}\n",
            &f,
            &CrateLinks::from_manifests(root, &f),
        );
        let c = at(&g, "mylib::c");
        assert!(!g.deps.contains(&(c, at(&g, "pins"))), "{:?}", g.deps);
    }

    #[test]
    fn detached_marks_only_that_folder() {
        let f = files(&[("pins/mod.rs", "pub fn setup() {}\n")]);
        let mut g = build_graph("mod pins;\nfn main() {}\n", &f);
        let lib = |untraced| DetachedLib {
            dir: "mylib".to_owned(),
            untraced,
        };
        mark_detached(&mut g, &[lib(Some(Untraced::Refused))]);
        for n in &g.nodes {
            let inside = n.file_rel.starts_with("mylib/");
            assert_eq!(n.detached, inside, "{}", n.file_rel);
            assert_eq!(n.untraced.is_some(), inside, "{}", n.file_rel);
        }
        let root = at(&g, "mylib");
        // Each state says what is true for IT - and only the refused one asks
        // for a manifest edit.
        let note = |g: &ModuleGraph| g.nodes[root].note.clone();
        assert!(note(&g).contains("exclude"), "{}", note(&g));
        mark_detached(&mut g, &[lib(Some(Untraced::NeedsRestart))]);
        assert!(note(&g).contains("Restart the analyzer"), "{}", note(&g));
        assert!(!note(&g).contains("exclude"), "{}", note(&g));
        mark_detached(&mut g, &[lib(None)]);
        assert_eq!(g.nodes[root].untraced, None);
        assert!(note(&g).contains("calls are traced"), "{}", note(&g));
    }

    /// Found by review: keying module names by crate made a module imported
    /// from a LINKED library a phantom external - the pattern the IDE's own
    /// Extract writes (`use mylib::a;` then `a::go()`).
    #[test]
    fn a_module_imported_from_a_linked_library_is_no_phantom_external() {
        let root = "[package]\nname = \"fw\"\n\n[dependencies]\nmylib = { path = \"mylib\" }\n";
        let f = files(&[]);
        let links = CrateLinks::from_manifests(root, &f);
        let main = "use mylib::a;\nfn main() { a::go(); }\n";
        let mut g = build_graph_with(main, &f, &links);
        add_external_nodes_with(&mut g, main, &f, &links);
        assert!(
            !g.nodes.iter().any(|n| n.is_external && n.name == "a"),
            "{:?}",
            g.nodes
                .iter()
                .filter(|n| n.is_external)
                .map(|n| &n.name)
                .collect::<Vec<_>>()
        );
    }

    /// Found by review: two other ways cargo links a local crate. Missing them
    /// dropped a real arrow, and the hover then claimed the opposite of what
    /// cargo does.
    #[test]
    fn inherited_and_patched_paths_link_too() {
        let f = files(&[]);
        let inherited = "[package]\nname = \"fw\"\n\n[workspace]\nmembers = [\"mylib\"]\n\n\
                         [workspace.dependencies]\nmylib = { path = \"mylib\" }\n\n\
                         [dependencies]\nmylib = { workspace = true }\n";
        let patched = "[package]\nname = \"fw\"\n\n[dependencies]\nmylib = \"0.1.0\"\n\n\
                       [patch.crates-io]\nmylib = { path = \"mylib\" }\n";
        for root in [inherited, patched] {
            let links = CrateLinks::from_manifests(root, &f);
            let mut g = build_graph_with(MAIN, &f, &links);
            assert!(g.deps.contains(&(0, at(&g, "mylib"))), "{root}");
            add_external_nodes_with(&mut g, MAIN, &f, &links);
            assert!(
                !g.nodes.iter().any(|n| n.is_external && n.name == "mylib"),
                "no ghost for a linked crate: {root}"
            );
        }
    }

    /// Found by review: a library manifest that does not parse - mid-edit -
    /// left that library "linking nothing", and its arrows to another library
    /// vanished. Unknown now means the old rule, for that crate alone.
    #[test]
    fn a_half_typed_library_manifest_keeps_its_arrows() {
        let root = "[package]\nname = \"fw\"\n\n[dependencies]\nliba = { path = \"liba\" }\n";
        let f: Vec<(String, String)> = vec![
            ("liba/Cargo.toml".into(), "[package\nname = ".into()),
            (
                "liba/src/lib.rs".into(),
                "pub fn a() { libb::x(); }\n".into(),
            ),
            (
                "libb/Cargo.toml".into(),
                "[package]\nname = \"libb\"\n".into(),
            ),
            ("libb/src/lib.rs".into(), "pub fn x() {}\n".into()),
        ];
        let links = CrateLinks::from_manifests(root, &f);
        let g = build_graph_with(MAIN, &f, &links);
        assert!(
            g.deps.contains(&(at(&g, "liba"), at(&g, "libb"))),
            "{:?}",
            g.deps
        );
    }

    /// Found by the second review: a library's file outside its `src/` is filed
    /// under the firmware's prefix, and was then judged by the FIRMWARE's
    /// manifest - its own library's arrows went, and a ghost claimed the code
    /// did not use the very copy it sits in. It keeps the old rule now.
    #[test]
    fn a_library_example_outside_src_keeps_its_arrows() {
        let root = "[package]\nname = \"fw\"\n\n[workspace]\nmembers = [\"mylib\"]\n";
        let demo = "fn main() { mylib::run(); }\n";
        let f = files(&[("mylib/examples/demo.rs", demo)]);
        let links = CrateLinks::from_manifests(root, &f);
        // main.rs names nothing: the only reference is the example's own,
        // so any ghost here would come from judging the example wrongly.
        let main = "fn main() {}\n";
        let mut g = build_graph_with(main, &f, &links);
        let d = g
            .nodes
            .iter()
            .position(|n| n.file_rel == "mylib/examples/demo.rs")
            .unwrap();
        assert!(g.deps.contains(&(d, at(&g, "mylib"))), "{:?}", g.deps);
        add_external_nodes_with(&mut g, main, &f, &links);
        assert!(
            !g.nodes.iter().any(|n| n.is_external && n.name == "mylib"),
            "no ghost for the library the example lives in"
        );
    }

    /// Found by the second review: `{ workspace = true }` is resolved against
    /// the crate's OWN workspace. A detached library that is its own workspace
    /// inherits from its own table, with paths relative to it.
    #[test]
    fn a_library_that_is_its_own_workspace_inherits_from_it() {
        let root = "[package]\nname = \"fw\"\n\n[workspace]\nmembers = []\n";
        let f: Vec<(String, String)> = vec![
            (
                "mylib/Cargo.toml".into(),
                "[package]\nname = \"mylib\"\n\n[workspace]\nmembers = [\"mylib-macros\"]\n\n\
                 [workspace.dependencies]\nmylib-macros = { path = \"mylib-macros\" }\n\n\
                 [dependencies]\nmylib-macros = { workspace = true }\n"
                    .into(),
            ),
            (
                "mylib/src/lib.rs".into(),
                "pub fn run() { mylib_macros::helper(); }\n".into(),
            ),
            (
                "mylib/mylib-macros/Cargo.toml".into(),
                "[package]\nname = \"mylib-macros\"\n".into(),
            ),
            (
                "mylib/mylib-macros/src/lib.rs".into(),
                "pub fn helper() {}\n".into(),
            ),
        ];
        let links = CrateLinks::from_manifests(root, &f);
        let g = build_graph_with("fn main() {}\n", &f, &links);
        assert!(
            g.deps.contains(&(at(&g, "mylib"), at(&g, "mylib_macros"))),
            "{:?}",
            g.deps
        );
    }

    #[test]
    fn joined_paths_stay_inside_the_project() {
        assert_eq!(join_rel("", "mylib").as_deref(), Some("mylib"));
        assert_eq!(join_rel("", "./mylib/").as_deref(), Some("mylib"));
        assert_eq!(join_rel("liba", "../libb").as_deref(), Some("libb"));
        assert_eq!(join_rel("liba", "..\\libb").as_deref(), Some("libb"));
        assert_eq!(join_rel("", "../elsewhere"), None);
        assert_eq!(join_rel("", "C:/abs"), None);
        assert_eq!(join_rel("", "/abs"), None);
    }
}
