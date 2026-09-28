//! "Extract to library crate" — turn a folder of `src/` into a sibling Cargo
//! crate that can be published to crates.io.
//!
//! Paths in and out are relative to the PROJECT ROOT (`src/mw_radar/frame.rs`),
//! matching `user_src_files`.
//!
//! Everything here is pure: [`plan_extract`] takes the current file set and
//! returns an [`ExtractPlan`] describing every write, removal and rewrite. The
//! caller performs the I/O. That split is what makes the tricky parts (module
//! tree, reference rewriting, manifest patching) testable.
//!
//! Layout produced for a folder `mw_radar`:
//!
//! ```text
//! <root>/mw_radar/Cargo.toml        — publishable manifest
//! <root>/mw_radar/src/lib.rs        — from mw_radar/mod.rs, or generated
//! <root>/mw_radar/src/<rest>.rs     — the moved files
//! ```
//!
//! The root manifest gains `[workspace] members` + `[dependencies.<name>]`, and
//! the remaining sources get `crate::<folder>::` rewritten to `<crate>::`.

/// Publishable metadata for the new crate, collected in the dialog.
#[derive(Clone, Debug)]
pub struct CrateMeta {
    pub name: String,
    pub version: String,
    pub edition: String,
    pub license: String,
    pub description: String,
    /// `#![no_std]` at the top of `lib.rs` — the default for embedded drivers.
    pub no_std: bool,
}

impl Default for CrateMeta {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: "0.1.0".to_owned(),
            edition: "2021".to_owned(),
            license: "MIT OR Apache-2.0".to_owned(),
            description: String::new(),
            no_std: true,
        }
    }
}

/// Everything the caller must apply, computed without touching the filesystem.
#[derive(Clone, Debug, Default)]
pub struct ExtractPlan {
    /// Directory of the new crate, relative to the project root.
    pub crate_dir: String,
    /// The folder that was extracted (root-relative, e.g. `src/mw_radar`) —
    /// the caller removes it from the tree and from disk.
    pub source_folder: String,
    /// Files to create, as paths relative to the PROJECT ROOT.
    pub new_files: Vec<(String, String)>,
    /// Project-root-relative paths to drop from `user_src_files`.
    pub removed: Vec<String>,
    /// Updated content for files that kept referring to the moved module, as
    /// `(project-root-relative path, new content)`.
    pub rewritten: Vec<(String, String)>,
    /// New `main.rs`, when it referenced the moved module.
    pub rewritten_main: Option<String>,
    /// Root `Cargo.toml` with the workspace + path dependency added.
    pub root_cargo_toml: String,
    /// Things the user has to fix by hand — shown before confirming.
    pub warnings: Vec<String>,
}

/// The Rust identifier a crate is reachable under: crates.io allows `-`, `use`
/// does not.
pub fn crate_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// Format a library FOLDER / crate name as true snake_case: every character
/// that is not an ASCII letter, digit or `_` (spaces, `-`, `.`, `/`, …) becomes
/// `_`, and letters are lower-cased (`mmWave` → `mmwave`). Applied live as the
/// user types in the clone/rename fields, so it must be idempotent and must NOT
/// collapse or trim `_` — that would fight the caret while typing `foo_bar`.
pub fn to_snake_case(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The trimmed crate name, or why it cannot be one. Shared by extraction and
/// by creating an empty library so both reject the same things.
fn validate_crate_name(raw: &str) -> Result<&str, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("Crate name is required.".to_owned());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Crate name may only contain letters, digits, `-` and `_`.".to_owned());
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err("Crate name cannot start with a digit.".to_owned());
    }
    Ok(name)
}

/// A brand-new, empty library crate: the manifest plus a `lib.rs` stub.
#[derive(Clone, Debug)]
pub struct NewCratePlan {
    pub crate_dir: String,
    /// Files to create, as paths relative to the PROJECT ROOT.
    pub new_files: Vec<(String, String)>,
    /// Root `Cargo.toml` with the workspace member + path dependency added.
    pub root_cargo_toml: String,
}

/// Plan an empty library crate next to `src/`.
///
/// Same manifest as an extracted crate (so a library created here and one
/// extracted from existing code are indistinguishable afterwards), and the same
/// idempotent root-manifest patch — including the `[dependencies]` entry, so
/// the firmware can `use` it straight away.
pub fn plan_new_crate(meta: &CrateMeta, root_cargo_toml: &str) -> Result<NewCratePlan, String> {
    let name = validate_crate_name(&meta.name)?;
    let dir = name.to_owned();
    let ident = crate_ident(name);

    let mut lib = String::new();
    if meta.no_std {
        lib.push_str("#![no_std]\n\n");
    }
    lib.push_str(&format!(
        "//! `{name}` — library crate of this project.\n\
         //!\n\
         //! Reach it from the firmware as `{ident}::…`.\n\n\
         // Declare your modules here, one per file in this folder:\n\
         //   pub mod driver;\n"
    ));

    Ok(NewCratePlan {
        new_files: vec![
            (format!("{dir}/Cargo.toml"), member_cargo_toml(meta)),
            (format!("{dir}/src/lib.rs"), lib),
        ],
        root_cargo_toml: patch_root_manifest(root_cargo_toml, &dir, name),
        crate_dir: dir,
    })
}

/// Removing a library crate: what to delete and the manifest without it.
#[derive(Clone, Debug, Default)]
pub struct DeleteCratePlan {
    pub crate_dir: String,
    /// Project-root-relative paths to drop from `user_src_files`.
    pub removed_files: Vec<String>,
    pub root_cargo_toml: String,
    /// Files that still mention the crate — the build will break until they are
    /// fixed, so the user is told before confirming.
    pub warnings: Vec<String>,
}

/// Plan removing library `dir`: its files, its workspace membership and its
/// path dependency.
pub fn plan_delete_crate(
    dir: &str,
    user_files: &[(String, String)],
    main_rs: &str,
    root_cargo_toml: &str,
) -> DeleteCratePlan {
    let prefix = format!("{dir}/");
    let ident = crate_ident(dir);
    let uses = format!("{ident}::");

    let mut warnings = Vec::new();
    for (path, content) in user_files
        .iter()
        .filter(|(p, _)| !p.starts_with(&prefix))
        .chain(std::iter::once(&(
            "src/main.rs".to_owned(),
            main_rs.to_owned(),
        )))
    {
        if content.contains(&uses) {
            warnings.push(format!("{path} still refers to `{ident}::…`"));
        }
    }

    DeleteCratePlan {
        crate_dir: dir.to_owned(),
        removed_files: user_files
            .iter()
            .filter(|(p, _)| p.starts_with(&prefix))
            .map(|(p, _)| p.clone())
            .collect(),
        root_cargo_toml: unpatch_root_manifest(root_cargo_toml, dir, dir),
        warnings,
    }
}

/// Renaming a library crate.
#[derive(Clone, Debug, Default)]
pub struct RenameCratePlan {
    pub old_dir: String,
    pub new_dir: String,
    /// `(old path, new path)` for every file of the crate.
    pub moved: Vec<(String, String)>,
    /// Sources outside the crate whose `old::` references were rewritten.
    pub rewritten: Vec<(String, String)>,
    pub rewritten_main: Option<String>,
    pub root_cargo_toml: String,
}

/// Plan renaming library `dir` to `new_name`.
///
/// Rewrites `old_ident::` to `new_ident::` everywhere outside the crate —
/// without that the rename would silently break every use site.
pub fn plan_rename_crate(
    dir: &str,
    new_name: &str,
    user_files: &[(String, String)],
    main_rs: &str,
    root_cargo_toml: &str,
) -> Result<RenameCratePlan, String> {
    let new_name = validate_crate_name(new_name)?;
    if new_name == dir {
        return Err("That is already the crate's name.".to_owned());
    }
    let (old_ident, new_ident) = (crate_ident(dir), crate_ident(new_name));
    let prefix = format!("{dir}/");

    let mut plan = RenameCratePlan {
        old_dir: dir.to_owned(),
        new_dir: new_name.to_owned(),
        // Members and the dependency are re-pointed in one pass.
        root_cargo_toml: unpatch_root_manifest(root_cargo_toml, dir, new_name),
        ..Default::default()
    };
    plan.root_cargo_toml = patch_root_manifest(&plan.root_cargo_toml, new_name, new_name);

    for (path, content) in user_files {
        if let Some(rest) = path.strip_prefix(&prefix) {
            let new_path = format!("{new_name}/{rest}");
            // The crate's own manifest carries its name.
            let body = if rest == "Cargo.toml" {
                content.replacen(&format!("\"{dir}\""), &format!("\"{new_name}\""), 1)
            } else {
                content.clone()
            };
            plan.moved.push((path.clone(), new_path.clone()));
            if body != *content {
                plan.rewritten.push((new_path, body));
            }
        } else {
            let new = content.replace(&format!("{old_ident}::"), &format!("{new_ident}::"));
            if new != *content {
                plan.rewritten.push((path.clone(), new));
            }
        }
    }
    let new_main = main_rs.replace(&format!("{old_ident}::"), &format!("{new_ident}::"));
    if new_main != main_rs {
        plan.rewritten_main = Some(new_main);
    }
    Ok(plan)
}

/// Drop `dir` from `[workspace] members` and remove its `[dependencies.<name>]`
/// block. `name` is normally the same as `dir`; renaming passes the NEW name so
/// the old entries go and [`patch_root_manifest`] can add the new ones.
fn unpatch_root_manifest(existing: &str, dir: &str, _name: &str) -> String {
    let mut out = String::with_capacity(existing.len());
    let mut skipping = false;
    for line in existing.lines() {
        let t = line.trim();
        // A `[dependencies.<dir>]` block runs until the next section header.
        if skipping {
            if t.starts_with('[') {
                skipping = false;
            } else {
                continue;
            }
        }
        if t == format!("[dependencies.{dir}]") {
            skipping = true;
            continue;
        }
        if t.starts_with("members") && t.contains(&format!("\"{dir}\"")) {
            let cleaned = drop_member(line, dir);
            out.push_str(&cleaned);
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Remove `"dir"` (and its separator) from a `members = [...]` line.
fn drop_member(line: &str, dir: &str) -> String {
    let quoted = format!("\"{dir}\"");
    line.replace(&format!("{quoted}, "), "")
        .replace(&format!(", {quoted}"), "")
        .replace(&quoted, "")
}

/// "Detach" a library from the workspace WITHOUT touching its files: drop it
/// from `[workspace] members` AND remove any `[dependencies.<dir>]` block. Both
/// halves matter — a lingering `path` dependency still forces `cargo metadata`
/// (and rust-analyzer) to resolve the crate, so removing only the member would
/// not un-break a workspace an incompatible library had killed. Inverse of
/// [`add_workspace_member`].
pub fn remove_workspace_member(existing: &str, crate_dir: &str) -> String {
    unpatch_root_manifest(existing, crate_dir, crate_dir)
}

/// Directories that carry their own `Cargo.toml` at the project root but are NOT
/// listed as `[workspace] members` — i.e. cloned libraries sitting DETACHED,
/// awaiting an explicit "Add to workspace". `user_files` are project-root-
/// relative; a crate manifest one level down (`foo/Cargo.toml`) marks `foo`.
/// `src/` is never a candidate (it's the firmware, not a library). Sorted +
/// deduped for a stable UI.
pub fn detached_libs(user_files: &[(String, String)], members: &[String]) -> Vec<String> {
    let mut dirs: Vec<String> = user_files
        .iter()
        .filter_map(|(p, _)| {
            let dir = p.strip_suffix("/Cargo.toml")?;
            // Depth 1 only: `foo/Cargo.toml`, not `foo/bar/Cargo.toml` (that is
            // a nested manifest inside the crate, not a top-level library).
            (!dir.is_empty() && !dir.contains('/') && dir != "src").then(|| dir.to_owned())
        })
        .filter(|d| !members.iter().any(|m| m == d))
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// The library folders cargo loads together with the firmware, for everything
/// that tells the user whether a library is part of it: the Structure tab's
/// detached verdict, the LIBRARIES panel and rust-analyzer's `linkedProjects`.
/// Pass it to [`detached_libs`] as `members`.
///
/// The explicit `[workspace] members`, plus the folders the root reaches
/// through a `path` dependency - in any dependency table, per-target ones
/// included, or inherited from `[workspace.dependencies]` (measured with
/// cargo 1.98):
/// - with a `[workspace]` table, every one its `exclude` does not cover:
///   cargo makes each an implicit member, optional or `cfg`-gated alike;
/// - otherwise - no `[workspace]` (the IDE's own templates), or the folder
///   excluded - only what cargo resolves for the firmware: nothing optional
///   the default features leave out, and nothing under `[target.<spec>]`,
///   whose spec this cannot evaluate. A gated one left out stays detached, so
///   rust-analyzer still gets it as a linked project instead of indexing it
///   nowhere.
///
/// `[patch]` is left out: it builds its folder only when the registry crate it
/// replaces is in the graph. A root that does not parse yields its members
/// alone, read as text.
pub fn built_lib_dirs(root_manifest: &str) -> Vec<String> {
    let mut dirs = crate::panels::mcu_module::project_gen::workspace_members(root_manifest);
    let workspace = crate::publish::has_workspace_table(root_manifest);
    // Matched the way cargo matches `exclude`: by path component.
    let exclude: Vec<Vec<String>> = crate::publish::workspace_array(root_manifest, "exclude")
        .iter()
        .map(|e| crate::lsp::cargo_components(e))
        .collect();
    for dep in crate::publish::path_dep_uses(root_manifest) {
        let Some(dir) = project_folder(&dep.path) else {
            continue;
        };
        let gated = dep.per_target
            || (dep.optional && !crate::publish::enabled_by_default(root_manifest, &dep.name));
        let implicit_member = workspace
            && !exclude
                .iter()
                .any(|e| crate::lsp::cargo_components(&dir).starts_with(e));
        if gated && !implicit_member {
            continue;
        }
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// A `path` from the root manifest as the project folder it names, resolved
/// the way cargo resolves a path dependency: `\` a separator on Windows, `.`
/// dropped, `..` applied. `None` for the project itself, anywhere outside it,
/// or an absolute path.
fn project_folder(path: &str) -> Option<String> {
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    if path.starts_with('/') || path.contains(':') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Build the plan for extracting `folder` (a path relative to the PROJECT
/// ROOT, e.g. `src/mw_radar`, no trailing slash) into a crate from `meta`.
///
/// `Err` when the request cannot work at all — an empty folder, a bad name, or
/// a directory that already exists in the manifest.
pub fn plan_extract(
    folder: &str,
    user_files: &[(String, String)],
    main_rs: &str,
    root_cargo_toml: &str,
    meta: &CrateMeta,
) -> Result<ExtractPlan, String> {
    let name = validate_crate_name(&meta.name)?;

    let prefix = format!("{folder}/");
    let moved: Vec<&(String, String)> = user_files
        .iter()
        .filter(|(p, _)| p.starts_with(&prefix))
        .collect();
    if moved.is_empty() {
        return Err(format!("`{folder}/` has no files to extract."));
    }

    let crate_dir = name.to_owned();
    let ident = crate_ident(name);
    // The module path the folder had inside the app crate.
    let folder_ident = folder
        .rsplit('/')
        .next()
        .unwrap_or(folder)
        .replace('-', "_");

    let mut plan = ExtractPlan {
        crate_dir: crate_dir.clone(),
        source_folder: folder.to_owned(),
        ..Default::default()
    };

    // ── Move the files ───────────────────────────────────────────────────────
    // `<folder>/mod.rs` becomes the crate root; everything else keeps its
    // relative position under the new `src/`.
    let mut has_mod_rs = false;
    let mut top_level_mods: Vec<String> = Vec::new();
    for (path, content) in &moved {
        let rel = &path[prefix.len()..];
        let mut body = (*content).clone();
        let dest_rel = if rel == "mod.rs" {
            has_mod_rs = true;
            // A promoted `mod.rs` becomes the crate root, and a crate root is
            // where `#![no_std]` has to live — an inner attribute is invalid
            // anywhere else. Without this the library silently pulled in `std`
            // and only failed at link time for the bare-metal target.
            body = with_no_std(&body, meta.no_std);
            "lib.rs".to_owned()
        } else {
            rel.to_owned()
        };
        if let Some(m) = top_level_module_of(rel) {
            if !top_level_mods.contains(&m) {
                top_level_mods.push(m);
            }
        }
        plan.new_files
            .push((format!("{crate_dir}/src/{dest_rel}"), body));
        plan.removed.push((*path).clone());

        // References the moved code cannot keep: they pointed at the app crate.
        for (line_no, line) in content.lines().enumerate() {
            if line.contains("crate::") && !line.trim_start().starts_with("//") {
                plan.warnings.push(format!(
                    "{path}:{} refers to `crate::…` — inside the library that now \
                     means the library itself, not the firmware. Pass what it \
                     needs in as a parameter (an `embedded-hal` trait) instead.",
                    line_no + 1
                ));
            }
        }
    }
    if has_mod_rs {
        plan.warnings.push(format!(
            "`{folder}/mod.rs` became `{crate_dir}/src/lib.rs`. Any `super::…` \
             in it pointed at the firmware crate and no longer resolves."
        ));
    } else {
        // No mod.rs: synthesize a crate root that re-exports the modules.
        top_level_mods.sort();
        let mut lib = String::new();
        if meta.no_std {
            lib.push_str("#![no_std]\n\n");
        }
        lib.push_str(&format!(
            "//! `{name}` — extracted from the firmware project.\n\n"
        ));
        for m in &top_level_mods {
            lib.push_str(&format!("pub mod {m};\n"));
        }
        plan.new_files
            .push((format!("{crate_dir}/src/lib.rs"), lib));
    }

    plan.new_files
        .push((format!("{crate_dir}/Cargo.toml"), member_cargo_toml(meta)));

    // ── Rewrite what stays behind ────────────────────────────────────────────
    // `crate::mw_radar::Foo` → `mw_radar::Foo`, and the `mod mw_radar;`
    // declaration disappears with the folder.
    for (path, content) in user_files {
        if path.starts_with(&prefix) {
            continue;
        }
        let new = rewrite_refs(content, &folder_ident, &ident);
        if new != *content {
            plan.rewritten.push((path.clone(), new));
        }
    }
    let new_main = rewrite_refs(main_rs, &folder_ident, &ident);
    if new_main != main_rs {
        plan.rewritten_main = Some(new_main);
    }

    // ── Patch the root manifest ──────────────────────────────────────────────
    plan.root_cargo_toml = patch_root_manifest(root_cargo_toml, &crate_dir, name);

    if meta.description.trim().is_empty() {
        plan.warnings.push(
            "No description — crates.io rejects a publish without one. You can \
             fill it in later in the new Cargo.toml."
                .to_owned(),
        );
    }
    Ok(plan)
}

/// Prepend `#![no_std]` to a crate root that doesn't already declare it.
///
/// Inner attributes must come before any item, so it goes at the very top —
/// after a leading `//!` doc block, which is also allowed to precede items and
/// which the user would not want pushed down.
fn with_no_std(src: &str, want: bool) -> String {
    if !want
        || src
            .lines()
            .any(|l| l.trim_start().starts_with("#![no_std]"))
    {
        return src.to_owned();
    }
    let insert_at = src
        .lines()
        .take_while(|l| l.trim_start().starts_with("//!") || l.trim().is_empty())
        .map(|l| l.len() + 1)
        .sum::<usize>()
        .min(src.len());
    let (head, tail) = src.split_at(insert_at);
    format!("{head}#![no_std]\n\n{}", tail.trim_start_matches('\n'))
}

/// The top-level module name a path inside the folder contributes: `foo.rs` →
/// `foo`, `bar/baz.rs` → `bar`. `mod.rs` contributes nothing.
fn top_level_module_of(rel: &str) -> Option<String> {
    match rel.split_once('/') {
        Some((dir, _)) => Some(dir.replace('-', "_")),
        None => {
            let stem = rel.strip_suffix(".rs")?;
            if stem == "mod" {
                None
            } else {
                Some(stem.replace('-', "_"))
            }
        }
    }
}

/// Replace `crate::<folder>` with `<crate_ident>` and drop the now-dangling
/// `mod <folder>;` declaration.
fn rewrite_refs(src: &str, folder_ident: &str, crate_ident: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let t = line.trim();
        // `mod mw_radar;` / `pub mod mw_radar;` — the module is gone.
        if t == format!("mod {folder_ident};") || t == format!("pub mod {folder_ident};") {
            continue;
        }
        out.push_str(&line.replace(&format!("crate::{folder_ident}"), crate_ident));
        out.push('\n');
    }
    // Keep the original's trailing-newline shape.
    if !src.ends_with('\n') {
        out.pop();
    }
    out
}

/// A publishable manifest for the new crate.
fn member_cargo_toml(meta: &CrateMeta) -> String {
    format!(
        "[package]\n\
         name        = \"{name}\"\n\
         version     = \"{version}\"\n\
         edition     = \"{edition}\"\n\
         license     = \"{license}\"\n\
         description = \"{desc}\"\n\
         # Fill these in before `cargo publish`:\n\
         # repository  = \"https://github.com/…\"\n\
         # readme      = \"README.md\"\n\
         # keywords    = [\"embedded\", \"no-std\"]\n\
         # categories  = [\"embedded\", \"no-std\"]\n\
         \n\
         [lib]\n\
         # The bare-metal target has no `test` crate, so a test/bench/doctest\n\
         # harness cannot be built for it — leaving these on gives\n\
         # \"can't find crate for `test`\". Same reason the firmware's [[bin]]\n\
         # sets them. Test this crate on the host instead:\n\
         #   cargo test -p {name} --target <host-triple>\n\
         test    = false\n\
         bench   = false\n\
         doctest = false\n\
         \n\
         [dependencies]\n\
         # Depend on TRAITS, not on a concrete HAL, so this crate stays usable\n\
         # on any chip (and is worth publishing):\n\
         # embedded-hal = \"1.0\"\n",
        name = meta.name.trim(),
        version = meta.version.trim(),
        edition = meta.edition.trim(),
        license = meta.license.trim(),
        desc = meta.description.trim().replace('"', "'"),
    )
}

/// Add `[workspace] members` and the path dependency to the root manifest.
///
/// Both go in the user tail, AFTER the generated block, so a chip change won't
/// wipe them. The dependency has to be written as `[dependencies.<name>]`: the
/// generated block already opened a `[dependencies]` table, and a second one is
/// a TOML redefinition error that cargo rejects outright. Idempotent — running
/// the extraction twice must not duplicate either section.
/// Add `crate_dir` to `[workspace] members` (creating the table/list if
/// needed), WITHOUT touching `[dependencies]`. Used for a CLONED external
/// library: it joins the workspace so cargo builds it, but is NOT forced as a
/// firmware dependency — an external crate may not even compile for the
/// firmware's target, which would break the firmware build. The user adds the
/// path dependency by hand when they actually `use` it.
pub fn add_workspace_member(existing: &str, crate_dir: &str) -> String {
    let mut out = existing.trim_end().to_owned();
    let members = crate::panels::mcu_module::project_gen::workspace_members(existing);
    if !members.iter().any(|m| m == crate_dir) {
        // Whether a `members` list EXISTS, not whether it has entries. An empty
        // one (`members = []`, what unpatching the last crate leaves behind)
        // must be extended in place — treating it as "no workspace" appended a
        // SECOND `[workspace]` table, which is a TOML redefinition error, and
        // every rename stacked another one.
        if has_members_list(existing) {
            out = extend_members_list(&out, crate_dir);
        } else {
            out.push_str(&format!("\n\n[workspace]\nmembers = [\"{crate_dir}\"]\n"));
        }
    }
    out
}

pub fn patch_root_manifest(existing: &str, crate_dir: &str, name: &str) -> String {
    let mut out = add_workspace_member(existing, crate_dir);
    if !out.contains(&format!("[dependencies.{name}]")) {
        out.push_str(&format!(
            "\n[dependencies.{name}]\npath = \"{crate_dir}\"\n"
        ));
    }
    out
}

/// `true` when the manifest already has a `members = …` line under
/// `[workspace]` — even an empty one.
fn has_members_list(cargo_toml: &str) -> bool {
    let mut in_workspace = false;
    for line in cargo_toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_workspace = t == "[workspace]";
            continue;
        }
        if in_workspace
            && t.strip_prefix("members")
                .is_some_and(|r| r.trim_start().starts_with('='))
        {
            return true;
        }
    }
    false
}

/// Insert `crate_dir` into an existing `members = [...]` array.
fn extend_members_list(src: &str, crate_dir: &str) -> String {
    let mut out = String::with_capacity(src.len() + crate_dir.len() + 8);
    let mut done = false;
    for line in src.lines() {
        if !done {
            if let Some(close) = line.rfind(']') {
                if line.trim_start().starts_with("members") || line.trim() == "]" {
                    let (head, tail) = line.split_at(close);
                    let sep = if head.trim_end().ends_with('[') {
                        ""
                    } else {
                        ", "
                    };
                    out.push_str(&format!("{head}{sep}\"{crate_dir}\"{tail}\n"));
                    done = true;
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> CrateMeta {
        CrateMeta {
            name: "mw_radar".to_owned(),
            description: "USART radar driver".to_owned(),
            ..Default::default()
        }
    }

    fn files() -> Vec<(String, String)> {
        vec![
            (
                "src/mw_radar/frame.rs".to_owned(),
                "pub struct Frame;\n".to_owned(),
            ),
            (
                "src/mw_radar/parser.rs".to_owned(),
                "pub fn parse() {}\n".to_owned(),
            ),
            (
                "src/app.rs".to_owned(),
                "mod mw_radar;\nuse crate::mw_radar::frame::Frame;\n".to_owned(),
            ),
        ]
    }

    /// The bare-metal target ships no `test` crate. `cargo check --workspace`
    /// makes the library a PRIMARY package, and a lib target with the default
    /// `test = true` then fails with "can't find crate for `test`" — exactly
    /// what the firmware's `[[bin]] test = false` has always prevented.
    #[test]
    fn member_manifest_disables_the_test_harness() {
        let p = plan_extract("src/mw_radar", &files(), "", "", &meta()).unwrap();
        let toml = p
            .new_files
            .iter()
            .find(|(a, _)| a == "mw_radar/Cargo.toml")
            .map(|(_, c)| c.clone())
            .expect("member manifest");
        assert!(toml.contains("[lib]"), "{toml}");
        assert!(toml.contains("test    = false"), "{toml}");
        assert!(toml.contains("bench   = false"), "{toml}");
        assert!(toml.contains("doctest = false"), "{toml}");
    }

    #[test]
    fn moves_files_under_the_new_crate_src() {
        let p = plan_extract("src/mw_radar", &files(), "", "", &meta()).unwrap();
        let paths: Vec<&str> = p.new_files.iter().map(|(a, _)| a.as_str()).collect();
        assert!(paths.contains(&"mw_radar/src/frame.rs"));
        assert!(paths.contains(&"mw_radar/src/parser.rs"));
        assert!(paths.contains(&"mw_radar/Cargo.toml"));
        assert_eq!(p.removed.len(), 2, "only the folder's files are removed");
    }

    /// Without a `mod.rs` we must synthesize a crate root, or nothing compiles.
    #[test]
    fn generates_lib_rs_declaring_every_module() {
        let p = plan_extract("src/mw_radar", &files(), "", "", &meta()).unwrap();
        let lib = p
            .new_files
            .iter()
            .find(|(a, _)| a == "mw_radar/src/lib.rs")
            .map(|(_, c)| c.clone())
            .expect("lib.rs generated");
        assert!(lib.contains("#![no_std]"), "no_std requested:\n{lib}");
        assert!(lib.contains("pub mod frame;"), "{lib}");
        assert!(lib.contains("pub mod parser;"), "{lib}");
    }

    /// An existing `mod.rs` IS the crate root — don't generate a second one,
    /// and it must still get `#![no_std]` (a promoted mod.rs used to keep its
    /// content verbatim, so the library silently linked `std`).
    #[test]
    fn existing_mod_rs_becomes_lib_rs() {
        let mut f = files();
        f.push((
            "src/mw_radar/mod.rs".to_owned(),
            "pub mod frame;\npub mod parser;\n".to_owned(),
        ));
        let p = plan_extract("src/mw_radar", &f, "", "", &meta()).unwrap();
        let libs: Vec<_> = p
            .new_files
            .iter()
            .filter(|(a, _)| a == "mw_radar/src/lib.rs")
            .collect();
        assert_eq!(libs.len(), 1, "exactly one crate root");
        assert_eq!(libs[0].1, "#![no_std]\n\npub mod frame;\npub mod parser;\n");
        assert!(
            p.warnings.iter().any(|w| w.contains("super::")),
            "must warn that super:: breaks: {:?}",
            p.warnings
        );
    }

    #[test]
    fn rewrites_references_and_drops_the_mod_declaration() {
        let p = plan_extract("src/mw_radar", &files(), "", "", &meta()).unwrap();
        let (_, app) = p
            .rewritten
            .iter()
            .find(|(a, _)| a == "src/app.rs")
            .expect("app.rs rewritten");
        assert_eq!(app, "use mw_radar::frame::Frame;\n");
    }

    #[test]
    fn rewrites_main_rs_too() {
        let main = "mod mw_radar;\nfn main() { crate::mw_radar::parser::parse(); }\n";
        let p = plan_extract("src/mw_radar", &files(), main, "", &meta()).unwrap();
        assert_eq!(
            p.rewritten_main.unwrap(),
            "fn main() { mw_radar::parser::parse(); }\n"
        );
    }

    /// Code moving into the library that still says `crate::` is the one thing
    /// the user MUST fix by hand — it has to be reported, not silently moved.
    #[test]
    fn warns_about_crate_references_inside_the_moved_code() {
        let f = vec![(
            "src/mw_radar/frame.rs".to_owned(),
            "use crate::pins::configs::usart1;\n".to_owned(),
        )];
        let p = plan_extract("src/mw_radar", &f, "", "", &meta()).unwrap();
        assert!(
            p.warnings
                .iter()
                .any(|w| w.contains("src/mw_radar/frame.rs:1")),
            "{:?}",
            p.warnings
        );
    }

    /// A second `[dependencies]` table would be a TOML redefinition error, so
    /// the dependency must be added as `[dependencies.<name>]`.
    #[test]
    fn root_manifest_uses_a_dependency_subtable() {
        let root = "[package]\nname = \"p\"\n\n[dependencies]\ncortex-m = \"0.7\"\n";
        let p = plan_extract("src/mw_radar", &files(), "", root, &meta()).unwrap();
        assert!(p.root_cargo_toml.contains("[dependencies.mw_radar]"));
        assert!(p.root_cargo_toml.contains("path = \"mw_radar\""));
        assert_eq!(
            p.root_cargo_toml.matches("[dependencies]").count(),
            1,
            "must not open a second [dependencies] table:\n{}",
            p.root_cargo_toml
        );
        assert!(p.root_cargo_toml.contains("[workspace]"));
    }

    /// Running it twice must not duplicate the workspace or the dependency.
    #[test]
    fn patching_the_root_manifest_is_idempotent() {
        let root = "[package]\nname = \"p\"\n";
        let once = plan_extract("src/mw_radar", &files(), "", root, &meta())
            .unwrap()
            .root_cargo_toml;
        let twice = plan_extract("src/mw_radar", &files(), "", &once, &meta())
            .unwrap()
            .root_cargo_toml;
        assert_eq!(once.trim_end(), twice.trim_end());
    }

    #[test]
    fn extends_an_existing_members_list() {
        let root = "[workspace]\nmembers = [\"other\"]\n";
        let p = plan_extract("src/mw_radar", &files(), "", root, &meta()).unwrap();
        assert!(
            p.root_cargo_toml
                .contains("members = [\"other\", \"mw_radar\"]"),
            "{}",
            p.root_cargo_toml
        );
        assert_eq!(p.root_cargo_toml.matches("[workspace]").count(), 1);
    }

    #[test]
    fn to_snake_case_replaces_specials_and_lowercases() {
        assert_eq!(
            to_snake_case("hmmd-mmWave-sensor-embede-io-lib"),
            "hmmd_mmwave_sensor_embede_io_lib"
        );
        assert_eq!(to_snake_case("My Cool Lib"), "my_cool_lib");
        assert_eq!(to_snake_case("a.b/c"), "a_b_c");
        // Underscores + digits pass through (typing stays natural).
        assert_eq!(to_snake_case("foo_bar1"), "foo_bar1");
        // Idempotent.
        assert_eq!(to_snake_case(&to_snake_case("A-b C")), "a_b_c");
    }

    #[test]
    fn remove_workspace_member_drops_member_and_dependency() {
        // A library that was promoted (member + hand-added path dep) — Detach
        // must strip BOTH, or `cargo metadata` still resolves the path dep.
        let root = "[package]\nname = \"fw\"\n\n[workspace]\nmembers = [\"lib_a\", \"lib_b\"]\n\n\
                    [dependencies.lib_b]\npath = \"lib_b\"\n";
        let out = remove_workspace_member(root, "lib_b");
        assert!(out.contains("members = [\"lib_a\"]"), "{out}");
        assert!(
            !out.contains("lib_b"),
            "no trace of the detached crate:\n{out}"
        );
        assert_eq!(out.matches("[workspace]").count(), 1);
    }

    #[test]
    fn detached_libs_are_root_manifests_that_are_not_members() {
        let files = vec![
            ("mmwave/Cargo.toml".to_owned(), String::new()),
            ("mmwave/src/lib.rs".to_owned(), String::new()),
            ("mw_radar/Cargo.toml".to_owned(), String::new()),
            ("src/main.rs".to_owned(), String::new()),
            // Nested manifest inside a crate — NOT a top-level library.
            ("mmwave/vendor/dep/Cargo.toml".to_owned(), String::new()),
        ];
        let members = vec!["mw_radar".to_owned()];
        // mmwave has a manifest and is not a member → detached; mw_radar is a
        // member; the nested vendor manifest is ignored.
        assert_eq!(detached_libs(&files, &members), vec!["mmwave".to_owned()]);
        // Once promoted, it stops being detached.
        assert!(detached_libs(&files, &["mw_radar".to_owned(), "mmwave".to_owned()]).is_empty());
    }

    /// Found by review: cargo loads a library the root reaches through a
    /// `path` dependency - as an implicit member when the root has a
    /// `[workspace]` table, as a plain dependency when it has none - yet the
    /// Structure tab called it detached, said the firmware does not build it
    /// and advised `exclude`. Measured with cargo 1.98.
    #[test]
    fn a_path_dependency_is_never_detached() {
        let lib = |name: &str| format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n");
        let files = vec![
            ("mylib/Cargo.toml".to_owned(), lib("mylib")),
            ("mylib/src/lib.rs".to_owned(), String::new()),
            ("other/Cargo.toml".to_owned(), lib("other")),
            ("src/main.rs".to_owned(), String::new()),
        ];
        let fw = "[package]\nname = \"fw\"\nversion = \"0.1.0\"\n";
        let mut roots = vec![
            // What Extract + Detach leave, with the dependency written inline.
            format!(
                "{fw}\n[workspace]\nmembers = []\n\n[dependencies]\nmylib = {{ path = \"mylib\" }}\n"
            ),
            // The IDE's own templates: no `[workspace]` at all.
            format!("{fw}\n[dependencies]\nmylib = {{ path = \"./mylib/\" }}\n"),
            format!("{fw}\n[workspace]\nmembers = []\n\n[dependencies.mylib]\npath = \"mylib\"\n"),
            // Gated, but a `[workspace]` makes it an implicit member anyway.
            format!(
                "{fw}\n[workspace]\n\n[target.'cfg(target_os = \"linux\")'.dependencies]\nmylib = {{ path = \"mylib\" }}\n"
            ),
            format!(
                "{fw}\n[workspace]\n\n[dependencies]\nmylib = {{ path = \"mylib\", optional = true }}\n"
            ),
            // Optional, and the default feature turns it on - one way or another.
            format!(
                "{fw}\n[dependencies]\nmylib = {{ path = \"mylib\", optional = true }}\n\n[features]\ndefault = [\"mylib\"]\n"
            ),
            format!(
                "{fw}\n[dependencies]\nmylib = {{ path = \"mylib\", optional = true }}\n\n[features]\ndefault = [\"radar\"]\nradar = [\"dep:mylib\"]\n"
            ),
            format!(
                "{fw}\n[dependencies]\nmylib = {{ path = \"mylib\", optional = true }}\n\n[features]\ndefault = [\"mylib/fast\"]\n"
            ),
            // A weak `?/` too: cargo resolves it, though the build compiles
            // nothing of it (measured).
            format!(
                "{fw}\n[dependencies]\nmylib = {{ path = \"mylib\", optional = true }}\n\n[features]\ndefault = [\"mylib?/fast\"]\n"
            ),
            // Excluded, but a plain dependency still loads it (measured).
            format!(
                "{fw}\n[workspace]\nmembers = []\nexclude = [\"mylib\"]\n\n[dependencies]\nmylib = {{ path = \"mylib\" }}\n"
            ),
            format!("{fw}\n[dev-dependencies]\nmylib = {{ path = \"mylib\" }}\n"),
            format!(
                "{fw}\n[workspace]\nmembers = []\n\n[workspace.dependencies]\nmylib = {{ path = \"mylib\" }}\n\n[dependencies]\nmylib = {{ workspace = true }}\n"
            ),
        ];
        if cfg!(windows) {
            roots.push(format!(
                "{fw}\n[dependencies]\nmylib = {{ path = 'mylib\\' }}\n"
            ));
        }
        for root in &roots {
            assert_eq!(
                detached_libs(&files, &built_lib_dirs(root)),
                vec!["other".to_owned()],
                "{root}"
            );
        }
        // Controls, where both folders stay detached: no dependency, one that
        // leaves the project, and - found by the reviews - one cargo does not
        // load for the firmware (measured with `cargo metadata
        // --filter-platform <target>`, as rust-analyzer runs it): with no
        // `[workspace]`, or with the folder excluded from it, an optional one
        // the default features leave out, or one gated to another target. It
        // must stay a linked project.
        for root in [
            format!("{fw}\n[workspace]\nmembers = []\n"),
            format!("{fw}\n[dependencies]\nmylib = {{ path = \"../elsewhere/mylib\" }}\n"),
            format!("{fw}\n[dependencies]\nmylib = {{ path = \"mylib\", optional = true }}\n"),
            format!(
                "{fw}\n[workspace]\nmembers = []\nexclude = [\"mylib\"]\n\n[dependencies]\nmylib = {{ path = \"mylib\", optional = true }}\n"
            ),
            format!(
                "{fw}\n[workspace]\nmembers = []\nexclude = [\"mylib\"]\n\n[target.'cfg(target_os = \"linux\")'.dependencies]\nmylib = {{ path = \"mylib\" }}\n"
            ),
            format!(
                "{fw}\n[dependencies]\nmylib = {{ path = \"mylib\", optional = true }}\n\n[features]\ndefault = []\nmylib = [\"dep:mylib\"]\n"
            ),
            format!(
                "{fw}\n[target.'cfg(target_os = \"linux\")'.dependencies]\nmylib = {{ path = \"mylib\" }}\n"
            ),
        ] {
            assert_eq!(
                detached_libs(&files, &built_lib_dirs(&root)),
                vec!["mylib".to_owned(), "other".to_owned()],
                "{root}"
            );
        }
    }

    #[test]
    fn rejects_bad_names_and_empty_folders() {
        let bad = CrateMeta {
            name: "mw radar".to_owned(),
            ..meta()
        };
        assert!(plan_extract("src/mw_radar", &files(), "", "", &bad).is_err());
        assert!(plan_extract("src/nope", &files(), "", "", &meta()).is_err());
    }

    /// `#![no_std]` is an inner attribute: it must precede every item, but a
    /// leading `//!` doc block is allowed above it and must not be displaced.
    #[test]
    fn no_std_goes_after_the_doc_header_and_is_never_duplicated() {
        let mut f = files();
        f.push((
            "src/mw_radar/mod.rs".to_owned(),
            "//! Radar driver.\n//! Second line.\n\npub mod frame;\n".to_owned(),
        ));
        let p = plan_extract("src/mw_radar", &f, "", "", &meta()).unwrap();
        let lib = p
            .new_files
            .iter()
            .find(|(a, _)| a == "mw_radar/src/lib.rs")
            .map(|(_, c)| c.clone())
            .unwrap();
        assert_eq!(
            lib,
            "//! Radar driver.\n//! Second line.\n\n#![no_std]\n\npub mod frame;\n"
        );

        // Already declared → untouched.
        let mut g = files();
        g.push((
            "src/mw_radar/mod.rs".to_owned(),
            "#![no_std]\npub mod frame;\n".to_owned(),
        ));
        let p2 = plan_extract("src/mw_radar", &g, "", "", &meta()).unwrap();
        let lib2 = p2
            .new_files
            .iter()
            .find(|(a, _)| a == "mw_radar/src/lib.rs")
            .map(|(_, c)| c.clone())
            .unwrap();
        assert_eq!(lib2.matches("#![no_std]").count(), 1, "{lib2}");
    }

    /// A new empty library must be indistinguishable from an extracted one:
    /// same manifest (including the `[lib]` harness switches) and the same
    /// idempotent root patch.
    #[test]
    fn a_new_crate_matches_an_extracted_one() {
        let root = "[package]\nname = \"p\"\n\n[dependencies]\ncortex-m = \"0.7\"\n";
        let p = plan_new_crate(&meta(), root).unwrap();

        let manifest = p
            .new_files
            .iter()
            .find(|(a, _)| a == "mw_radar/Cargo.toml")
            .map(|(_, c)| c.clone())
            .expect("manifest");
        assert!(
            manifest.contains("name        = \"mw_radar\""),
            "{manifest}"
        );
        assert!(manifest.contains("test    = false"), "{manifest}");

        let lib = p
            .new_files
            .iter()
            .find(|(a, _)| a == "mw_radar/src/lib.rs")
            .map(|(_, c)| c.clone())
            .expect("lib.rs stub");
        assert!(lib.starts_with("#![no_std]"), "{lib}");

        // Wired into the workspace AND usable from the firmware immediately.
        assert!(p.root_cargo_toml.contains("[workspace]"));
        assert!(p.root_cargo_toml.contains("[dependencies.mw_radar]"));
        assert_eq!(
            p.root_cargo_toml.matches("[dependencies]").count(),
            1,
            "must not open a second [dependencies] table"
        );
    }

    #[test]
    fn a_new_crate_rejects_the_same_names_extraction_does() {
        let bad = CrateMeta {
            name: "mw radar".to_owned(),
            ..meta()
        };
        assert!(plan_new_crate(&bad, "").is_err());
        let digit = CrateMeta {
            name: "1radar".to_owned(),
            ..meta()
        };
        assert!(plan_new_crate(&digit, "").is_err());
    }

    /// Deleting must undo everything the extraction wired up, or the manifest
    /// keeps pointing at a directory that is gone and cargo refuses to load.
    #[test]
    fn deleting_unwires_the_crate_from_the_manifest() {
        let root = "[package]\nname = \"p\"\n\n[dependencies]\ncortex-m = \"0.7\"\n\
                    \n[workspace]\nmembers = [\"mw_radar\"]\n\
                    \n[dependencies.mw_radar]\npath = \"mw_radar\"\n";
        let files = vec![
            ("mw_radar/Cargo.toml".to_owned(), String::new()),
            ("mw_radar/src/lib.rs".to_owned(), String::new()),
            ("src/app.rs".to_owned(), "use mw_radar::frame;\n".to_owned()),
        ];
        let p = plan_delete_crate("mw_radar", &files, "", root);

        assert_eq!(p.removed_files.len(), 2, "only the crate's own files");
        assert!(!p.root_cargo_toml.contains("[dependencies.mw_radar]"));
        assert!(
            p.root_cargo_toml.contains("members = []"),
            "{}",
            p.root_cargo_toml
        );
        assert!(
            p.root_cargo_toml.contains("cortex-m"),
            "unrelated dependencies survive:\n{}",
            p.root_cargo_toml
        );
        // The user is told what will stop compiling.
        assert!(
            p.warnings.iter().any(|w| w.contains("src/app.rs")),
            "{:?}",
            p.warnings
        );
    }

    /// A rename that did not fix the use sites would silently break the build.
    #[test]
    fn renaming_moves_the_files_and_rewrites_use_sites() {
        let root = "[workspace]\nmembers = [\"mw_radar\"]\n\
                    \n[dependencies.mw_radar]\npath = \"mw_radar\"\n";
        let files = vec![
            (
                "mw_radar/Cargo.toml".to_owned(),
                "[package]\nname        = \"mw_radar\"\n".to_owned(),
            ),
            (
                "mw_radar/src/lib.rs".to_owned(),
                "pub mod frame;\n".to_owned(),
            ),
            (
                "src/app.rs".to_owned(),
                "use mw_radar::frame::Frame;\n".to_owned(),
            ),
        ];
        let p = plan_rename_crate("mw_radar", "radar_hal", &files, "fn main(){}", root).unwrap();

        assert_eq!(p.new_dir, "radar_hal");
        assert!(p.moved.contains(&(
            "mw_radar/src/lib.rs".to_owned(),
            "radar_hal/src/lib.rs".to_owned()
        )));
        // The member manifest's own `name` follows.
        let manifest = p
            .rewritten
            .iter()
            .find(|(a, _)| a == "radar_hal/Cargo.toml")
            .map(|(_, c)| c.clone())
            .expect("manifest rewritten");
        assert!(manifest.contains("\"radar_hal\""), "{manifest}");
        // And every use site outside the crate.
        let app = p
            .rewritten
            .iter()
            .find(|(a, _)| a == "src/app.rs")
            .map(|(_, c)| c.clone())
            .expect("use site rewritten");
        assert_eq!(app, "use radar_hal::frame::Frame;\n");
        // Root manifest re-pointed, with no leftovers.
        assert!(p.root_cargo_toml.contains("[dependencies.radar_hal]"));
        assert!(!p.root_cargo_toml.contains("[dependencies.mw_radar]"));
        assert!(!p.root_cargo_toml.contains("\"mw_radar\""));
    }

    /// Regression: renaming unpatches then patches, and the unpatch leaves
    /// `members = []`. Treating an EMPTY list as "no workspace section" made
    /// the patch append a second `[workspace]` table — a TOML redefinition
    /// error — and every further rename or new library stacked another one.
    #[test]
    fn there_is_never_more_than_one_workspace_table() {
        let root = "[package]\nname = \"p\"\n\n[workspace]\nmembers = [\"mw_radar\"]\n\
                    \n[dependencies.mw_radar]\npath = \"mw_radar\"\n";
        let files = vec![(
            "mw_radar/Cargo.toml".to_owned(),
            "[package]\nname = \"mw_radar\"\n".to_owned(),
        )];

        let after_rename = plan_rename_crate("mw_radar", "radar_hal", &files, "", root)
            .unwrap()
            .root_cargo_toml;
        assert_eq!(
            after_rename.matches("[workspace]").count(),
            1,
            "one workspace table:\n{after_rename}"
        );
        assert!(
            after_rename.contains("members = [\"radar_hal\"]"),
            "{after_rename}"
        );

        // …and adding another library after that still does not duplicate it.
        let meta2 = CrateMeta {
            name: "test11".to_owned(),
            ..meta()
        };
        let after_new = plan_new_crate(&meta2, &after_rename)
            .unwrap()
            .root_cargo_toml;
        assert_eq!(
            after_new.matches("[workspace]").count(),
            1,
            "still one workspace table:\n{after_new}"
        );
        assert!(
            after_new.contains("\"radar_hal\"") && after_new.contains("\"test11\""),
            "both members kept:\n{after_new}"
        );
    }

    /// The same trap, reached by deleting the last library and adding one.
    #[test]
    fn an_emptied_members_list_is_reused_not_replaced() {
        let root = "[workspace]\nmembers = [\"only\"]\n\n[dependencies.only]\npath = \"only\"\n";
        let emptied = plan_delete_crate("only", &[], "", root).root_cargo_toml;
        assert!(emptied.contains("members = []"), "{emptied}");

        let out = plan_new_crate(&meta(), &emptied).unwrap().root_cargo_toml;
        assert_eq!(out.matches("[workspace]").count(), 1, "{out}");
        assert!(out.contains("members = [\"mw_radar\"]"), "{out}");
    }

    #[test]
    fn renaming_rejects_a_bad_or_unchanged_name() {
        assert!(plan_rename_crate("mw_radar", "mw radar", &[], "", "").is_err());
        assert!(plan_rename_crate("mw_radar", "mw_radar", &[], "", "").is_err());
    }

    /// crates.io names may contain `-`; `use` paths may not.
    #[test]
    fn hyphenated_crate_names_map_to_underscore_paths() {
        let m = CrateMeta {
            name: "mw-radar".to_owned(),
            ..meta()
        };
        let p = plan_extract("src/mw_radar", &files(), "", "", &m).unwrap();
        let (_, app) = p.rewritten.iter().find(|(a, _)| a == "src/app.rs").unwrap();
        assert_eq!(app, "use mw_radar::frame::Frame;\n");
        assert!(p.new_files.iter().any(|(a, _)| a == "mw-radar/Cargo.toml"));
    }
}
