//! Project tree logic — file operations, directory scanning, filesystem watching.

use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use std::path::Path;

/// State for the project tree (files and folders in src/).
#[derive(Debug, Clone)]
pub struct ProjectTreeState {
    /// `(path_relative_to_src, content)` for every user-created file.
    pub user_src_files: Vec<(String, String)>,
    /// Explicitly-created folders inside src/.
    pub user_src_folders: Vec<String>,
}

/// Name for a duplicate of `path` (relative to `src/`): the first free
/// `<stem>_<n>` in the SAME folder, keeping the extension —
/// `my_file.rs` → `my_file_1.rs`, and `foo/a.rs` → `foo/a_1.rs`.
///
/// A trailing `_<digits>` on the source is stripped first, so duplicating a
/// duplicate keeps counting on the same base (`my_file_1.rs` → `my_file_2.rs`)
/// instead of growing `my_file_1_1.rs`. `exists` decides what's taken, so the
/// caller can answer from the in-memory file list.
pub fn duplicate_path(path: &str, exists: impl Fn(&str) -> bool) -> String {
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    // Split on the LAST dot; a leading dot is part of the name (".gitignore"),
    // not an extension.
    let (stem, ext) = match file.rfind('.') {
        Some(i) if i > 0 => (&file[..i], &file[i..]),
        _ => (file, ""),
    };
    let base = strip_copy_suffix(stem);
    (1..)
        .map(|n| format!("{dir}{base}_{n}{ext}"))
        .find(|cand| !exists(cand))
        .expect("an unbounded counter always reaches a free name")
}

/// `my_file_3` → `my_file`; anything else unchanged. Requires a non-empty
/// all-digit tail AND a non-empty base, so `_1` and `foo_` stay as they are.
fn strip_copy_suffix(stem: &str) -> &str {
    match stem.rfind('_') {
        Some(i)
            if i > 0 && i + 1 < stem.len() && stem[i + 1..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            &stem[..i]
        }
        _ => stem,
    }
}

/// The firmware crate's source directory, as a path prefix.
///
/// Every path in `user_src_files` / `user_src_folders` is relative to the
/// PROJECT ROOT, not to `src/`. That is what lets library crates extracted out
/// of the project (`mw_radar/src/lib.rs`) live in the very same list — one flat
/// file set, grouped into sections only when the tree is drawn.
pub const SRC_ROOT: &str = "src";

/// `src/<rest>` — the root-relative path of a firmware source file.
pub fn src_path(rest: &str) -> String {
    format!("{SRC_ROOT}/{rest}")
}

impl ProjectTreeState {
    /// Create a new empty project tree state.
    pub fn new() -> Self {
        Self {
            user_src_files: Vec::new(),
            user_src_folders: Vec::new(),
        }
    }

    /// Load project tree state from a project directory: the firmware's `src/`
    /// plus every workspace-member crate's directory (extracted libraries), all
    /// with paths relative to `root`.
    pub fn load_from_dir(root: &Path) -> Self {
        let mut files = Vec::new();
        let mut folders = Vec::new();

        let src_dir = root.join(SRC_ROOT);
        if src_dir.exists() {
            Self::scan_src_dir(root, &src_dir, &mut files, &mut folders);
        }

        // Library crates: read the members out of the root manifest rather than
        // guessing from directory names, so only real crates are picked up.
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
        let members = crate::panels::mcu_module::project_gen::workspace_members(&manifest);
        for member in &members {
            let dir = root.join(member);
            if dir.is_dir() {
                if !folders.contains(member) {
                    folders.push(member.clone());
                }
                Self::scan_src_dir(root, &dir, &mut files, &mut folders);
            }
        }

        // DETACHED libraries: a cloned crate that is not (yet) a `[workspace]`
        // member still owns its `Cargo.toml`. It must be scanned too, or it
        // would vanish from the tree on every reload/restart (leaving only its
        // "Add to workspace" affordance unreachable). A top-level dir counts as
        // a detached library iff it has a `Cargo.toml` and is not a member.
        // `src`, `target` and hidden dirs (`.git`, `.cargo`, …) are never crates.
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().replace('\\', "/");
                if name == SRC_ROOT
                    || name == "target"
                    || name.starts_with('.')
                    || members.iter().any(|m| m == &name)
                    || folders.contains(&name)
                {
                    continue;
                }
                if path.join("Cargo.toml").is_file() {
                    folders.push(name);
                    Self::scan_src_dir(root, &path, &mut files, &mut folders);
                }
            }
        }

        Self {
            user_src_files: files,
            user_src_folders: folders,
        }
    }

    /// Scan ONE workspace-member directory into the tree, appending files and
    /// folders not already tracked. Used after a `git clone` adds a member, so
    /// the rest of the in-memory tree (and any unsaved edits) is preserved —
    /// unlike a full `load_from_dir`. `member` is project-root-relative.
    pub fn add_member_dir(&mut self, root: &Path, member: &str) {
        let dir = root.join(member);
        if !dir.is_dir() {
            return;
        }
        let mut files = Vec::new();
        let mut folders = vec![member.to_string()];
        Self::scan_src_dir(root, &dir, &mut files, &mut folders);
        for f in folders {
            if !self.user_src_folders.contains(&f) {
                self.user_src_folders.push(f);
            }
        }
        for (p, c) in files {
            if !self.user_src_files.iter().any(|(pp, _)| pp == &p) {
                self.user_src_files.push((p, c));
            }
        }
    }

    /// Recursively scan `dir`, recording paths relative to `root` (the PROJECT
    /// ROOT). Skips the generated `src/main.rs` and build/VCS directories.
    fn scan_src_dir(
        root: &Path,
        dir: &Path,
        files: &mut Vec<(String, String)>,
        folders: &mut Vec<String>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                // A library crate can carry its own build output; never pull
                // gigabytes of `target/` into the tree.
                let name = rel.rsplit('/').next().unwrap_or_default();
                if name == "target" || name == ".git" {
                    continue;
                }
                if !folders.contains(&rel) {
                    folders.push(rel);
                }
                Self::scan_src_dir(root, &path, files, folders);
            } else if path.is_file() {
                if rel == src_path("main.rs") {
                    continue; // always generated — skip
                }
                // Normalize to LF at the door. The in-memory buffers must be
                // pure LF: the git gutter's baseline (`git show HEAD:…`) is
                // LF-normalized, so CRLF read from a Windows checkout made
                // EVERY line "differ" — the whole body showed as a phantom
                // permanently-added band, and real edits produced marks at
                // wrong positions (the diff could only anchor on the rare LF
                // lines). Codegen emits LF, so this also stops files being
                // written back with mixed endings.
                let content = std::fs::read_to_string(&path)
                    .unwrap_or_default()
                    .replace("\r\n", "\n");
                files.push((rel, content));
            }
        }
    }

    /// Handle filesystem events: Create, Remove, and Rename operations.
    pub fn handle_fs_events(&mut self, events: Vec<(String, FsEventKind)>) {
        for (rel, kind) in events {
            self.apply_fs_event(&rel, kind, &mut None);
        }
    }

    /// Apply ONE filesystem event. `selected` is an index into
    /// `user_src_files` (the file open in the editor, which refers to it by
    /// index) and follows it across every index this shifts; it becomes `None`
    /// when that file is gone.
    ///
    /// Returns `false` for a Rename whose source the tree does not know - a
    /// temp file renamed into place, or the watcher's echo of a rename the IDE
    /// already applied. The destination has then simply appeared, which only
    /// the caller can read from disk.
    pub fn apply_fs_event(
        &mut self,
        rel: &str,
        kind: FsEventKind,
        selected: &mut Option<usize>,
    ) -> bool {
        match kind {
            FsEventKind::Create => {
                if !self.user_src_files.iter().any(|(p, _)| p == rel) {
                    let content = String::new();
                    self.user_src_files.push((rel.to_owned(), content));
                }
            }
            FsEventKind::Remove => {
                // A folder's contents go with it. A folder moved out of the
                // watched tree is ONE event: nothing is reported for what was
                // inside, which used to stay behind as ghost files.
                let dir_rel = rel.trim_end_matches('/');
                let prefix = format!("{dir_rel}/");
                self.remove_files_where(|p| p == rel || p.starts_with(&prefix), selected);
                self.user_src_folders
                    .retain(|f| f != dir_rel && !f.starts_with(&prefix));
            }
            FsEventKind::Rename { old_rel, new_rel } => {
                let mut known = false;
                // File rename, in place: the entry keeps its index and its
                // in-memory content, so the editor stays on it.
                if let Some(i) = self.user_src_files.iter().position(|(p, _)| *p == old_rel) {
                    known = true;
                    match self.user_src_files.iter().position(|(p, _)| *p == new_rel) {
                        // Onto a file the tree has (an atomic save): the
                        // destination keeps its slot and takes the moved
                        // file's content, and the source's slot goes.
                        Some(j) if j != i => {
                            let content = std::mem::take(&mut self.user_src_files[i].1);
                            self.user_src_files[j].1 = content;
                            if *selected == Some(i) {
                                *selected = Some(j);
                            }
                            self.user_src_files.remove(i);
                            *selected = selected.map(|s| if s > i { s - 1 } else { s });
                        }
                        _ => self.user_src_files[i].0 = new_rel.clone(),
                    }
                }
                // Folder rename - the folder, every folder under it and every
                // file under it.
                let old_prefix = format!("{old_rel}/");
                let mut moved_folder = false;
                for f in &mut self.user_src_folders {
                    if *f == old_rel {
                        *f = new_rel.clone();
                    } else if let Some(rest) = f.strip_prefix(&old_prefix) {
                        *f = format!("{new_rel}/{rest}");
                    } else {
                        continue;
                    }
                    moved_folder = true;
                }
                if moved_folder {
                    known = true;
                    // Renamed onto a folder the tree already listed.
                    let mut seen = std::collections::HashSet::new();
                    self.user_src_folders.retain(|f| seen.insert(f.clone()));
                }
                for (path, _) in &mut self.user_src_files {
                    if let Some(rest) = path.strip_prefix(&old_prefix) {
                        *path = format!("{new_rel}/{rest}");
                        known = true;
                    }
                }
                return known;
            }
        }
        true
    }

    /// Drop every file whose path matches, carrying `selected` across the
    /// shift (`None` when its own file is dropped).
    fn remove_files_where(&mut self, drop: impl Fn(&str) -> bool, selected: &mut Option<usize>) {
        let sel = *selected;
        let mut idx = 0;
        let mut dropped_before = 0;
        let mut sel_dropped = false;
        self.user_src_files.retain(|(p, _)| {
            let gone = drop(p);
            if gone {
                match sel {
                    Some(s) if s == idx => sel_dropped = true,
                    Some(s) if idx < s => dropped_before += 1,
                    _ => {}
                }
            }
            idx += 1;
            !gone
        });
        *selected = if sel_dropped {
            None
        } else {
            sel.map(|s| s - dropped_before)
        };
    }

    /// Keep the `src/pins/` scaffold in step with the pin configuration:
    /// register the folder, ensure `pins/mod.rs` exists, sweep away stray
    /// per-pin files, and declare `pub mod configs;` when the per-peripheral
    /// init modules exist (they are written by [`Self::sync_config_files`]).
    ///
    /// **Per-pin files are no longer generated.** Each configured pin used to
    /// get its own `src/pins/pin<N>_<name>_<type>.rs` holding a HAL type alias,
    /// renamed whenever the pin's function changed. That was turned off
    /// deliberately in `4899d16` ("disabled auto adding pin files") — the pin
    /// bindings live in `main.rs`'s generated block and the reusable helpers in
    /// the user's own `pins/utils/`, so a file per pin was churn nobody read.
    ///
    /// The parameter is kept: this is the hook the pin configuration already
    /// calls on every change, and re-enabling means filling `configured` again
    /// (the rest of the function still handles creation, renaming and pruning).
    /// `configured` staying empty is exactly what makes step 3 a sweeper for
    /// files left behind by projects generated before that commit.
    pub fn sync_pin_files(&mut self, _all_pins: &[(usize, String, PinFunction)]) {
        const MOD_PATH: &str = "src/pins/mod.rs";

        // Empty on purpose — see the note above. Typed as it was so the code
        // below (which still creates / splices / prunes) needs no changes.
        let configured: Vec<(String, usize, &str, &PinFunction)> = Vec::new();
        let active_slugs: Vec<&str> = configured.iter().map(|(s, ..)| s.as_str()).collect();

        // 1. Ensure pins/ folder is registered
        let folder = src_path("pins");
        if !self.user_src_folders.contains(&folder) {
            self.user_src_folders.push(folder);
        }

        // 2. Ensure pins/mod.rs exists
        if !self.user_src_files.iter().any(|(p, _)| p == MOD_PATH) {
            self.user_src_files
                .push((MOD_PATH.to_string(), String::new()));
        }

        // 3. Drop pin files that are no longer configured
        self.user_src_files.retain(|(path, _)| {
            let Some(fname) = path.strip_prefix("src/pins/") else {
                return true;
            };
            if fname == "mod.rs" {
                return true;
            }
            if !fname.starts_with("pin") || !fname.ends_with(".rs") {
                return true;
            }
            let slug = &fname[..fname.len() - 3];
            active_slugs.contains(&slug)
        });

        // 4. Create or update pin files (with GENERATED marker preservation)
        for (slug, num, name, func) in &configured {
            let file_path = src_path(&format!("pins/{slug}.rs"));
            let generated_content = generate_pin_content(*num, name, func);
            let wrapped_content = format!(
                "// <<< GENERATED>>>\n{}\n// <<< GENERATED END >>>\n",
                generated_content
            );

            if let Some((_, file_content)) = self
                .user_src_files
                .iter_mut()
                .find(|(p, _)| p == &file_path)
            {
                // Update existing file: preserve user code, update only GENERATED section
                let existing = file_content.clone();
                let updated = splice_pin_file(&existing, &wrapped_content);
                *file_content = updated;
            } else {
                // Create new file
                self.user_src_files.push((file_path, wrapped_content));
            }
        }

        // 5. Rebuild mod.rs (preserve custom code outside GENERATED section).
        // Also declare `pub mod configs;` when per-peripheral init modules exist
        // under `pins/configs/` (synced separately by `sync_config_files`).
        let has_configs = self
            .user_src_files
            .iter()
            .any(|(p, _)| p.starts_with("src/pins/configs/"));
        let mut generated_section: String = configured
            .iter()
            .map(|(slug, ..)| format!("pub mod {slug};\n"))
            .collect();
        if has_configs {
            generated_section.push_str("pub mod configs;\n");
        }

        let generated_with_markers = format!(
            "// <<< GENERATED>>>\n{}\n// <<< GENERATED END >>>\n",
            generated_section.trim()
        );

        if let Some((_, mod_content)) = self.user_src_files.iter_mut().find(|(p, _)| p == MOD_PATH)
        {
            let existing = mod_content.as_str();
            if let (Some(begin_pos), Some(end_pos)) = (
                existing.find("// <<< GENERATED>>>"),
                existing.find("// <<< GENERATED END >>>"),
            ) {
                let before = &existing[..begin_pos].trim_end();
                let after = &existing[end_pos + "// <<< GENERATED END >>>".len()..].trim_start();
                if before.is_empty() && after.is_empty() {
                    *mod_content = generated_with_markers;
                } else if before.is_empty() {
                    *mod_content = format!("{}\n\n{}", generated_with_markers.trim(), after);
                } else if after.is_empty() {
                    *mod_content = format!("{}\n\n{}", before, generated_with_markers.trim());
                } else {
                    *mod_content = format!(
                        "{}\n\n{}\n\n{}",
                        before,
                        generated_with_markers.trim(),
                        after
                    );
                }
            } else if !existing.trim().is_empty() {
                *mod_content = format!("{}\n\n{}", generated_with_markers.trim(), existing);
            } else {
                *mod_content = generated_with_markers;
            }
        }
    }

    /// Sync the per-peripheral init modules under `src/pins/configs/` from the
    /// codegen output `files = (file_name, generated_body)` (one per configured
    /// USART/SPI/I2C). Mirrors [`sync_pin_files`]: registers the folder, ensures
    /// `configs/mod.rs`, drops orphaned files, splices each file's GENERATED
    /// block (preserving user code outside it), and rebuilds `configs/mod.rs`
    /// with `pub mod <periph>;`. When `files` is empty the whole subtree is
    /// dropped. Call this BEFORE `sync_pin_files` so the latter can add
    /// `pub mod configs;` to `pins/mod.rs`.
    /// `force` = rewrite each existing config file in FULL (the whole template,
    /// not just the constants block). Used on a Runtime / Init-API Apply, where
    /// the `init()` template itself changes (blocking ⇄ async ⇄ native) and a
    /// constants-only splice would leave the old implementation in place. A
    /// normal (baud/param) change passes `false` so user edits below the markers
    /// survive.
    ///
    /// A Custom module's file is the exception to both: it is written only when
    /// it does not exist yet, and never touched again — not spliced, not forced.
    /// Every Update produces a new revision file, so a pin change still reaches
    /// the code, while whatever the user wrote into an existing one stays.
    pub fn sync_config_files(
        &mut self,
        files: &[(String, String)],
        force: bool,
        // Stems whose OLDER files must survive the prune below — a Custom module
        // writes each Update to a new `custom_<name>_<n>.rs`, and the previous
        // revisions are kept on disk (they are not in `configs/mod.rs`, so they
        // are never compiled and can't clash with the current struct).
        keep_prefixes: &[String],
    ) {
        const DIR: &str = "src/pins/configs";
        const MOD_PATH: &str = "src/pins/configs/mod.rs";
        const GEN_BEGIN: &str = "// <<< GENERATED>>>";
        const GEN_END: &str = "// <<< GENERATED END >>>";

        if files.is_empty() {
            // No configured peripherals → drop the entire configs/ subtree.
            self.user_src_files
                .retain(|(p, _)| !p.starts_with("src/pins/configs/"));
            self.user_src_folders.retain(|f| f != DIR);
            return;
        }

        // 1. Register the folder.
        if !self.user_src_folders.iter().any(|f| f == DIR) {
            self.user_src_folders.push(DIR.to_string());
        }
        // 2. Ensure configs/mod.rs exists.
        if !self.user_src_files.iter().any(|(p, _)| p == MOD_PATH) {
            self.user_src_files
                .push((MOD_PATH.to_string(), String::new()));
        }

        // Active module stems (file names without `.rs`).
        let active: Vec<String> = files
            .iter()
            .map(|(name, _)| name.trim_end_matches(".rs").to_string())
            .collect();

        // Does `stem` belong to a Custom module? Its file is `custom_<name>` at
        // revision 0 and `custom_<name>_<n>` after the n-th Update, and
        // `keep_prefixes` holds exactly those `custom_<name>` roots — so the same
        // test that keeps old revisions on disk also tells a hand-authored module
        // apart from a peripheral one, with no name-prefix guesswork.
        let is_custom_stem = |stem: &str| {
            keep_prefixes
                .iter()
                .any(|p| stem == p || stem.starts_with(&format!("{p}_")))
        };

        // 3. Drop config files no longer configured.
        self.user_src_files.retain(|(path, _)| {
            let Some(rest) = path.strip_prefix("src/pins/configs/") else {
                return true;
            };
            let stem = rest.trim_end_matches(".rs");
            rest == "mod.rs" || active.iter().any(|a| a == stem) || is_custom_stem(stem)
        });

        // 4. Create / update each config file. The codegen `body` already wraps
        //    ONLY the constants in `// <<< GENERATED>>>` markers; everything below
        //    (use block + get_config/init) is editable. On update we re-splice
        //    just that constants block, so the user's edits to the rest survive.
        for (name, body) in files {
            let file_path = src_path(&format!("pins/configs/{name}"));
            // A Custom module's file belongs to the user once it exists. Splicing
            // it ran on every regeneration — the first frame after the IDE starts
            // included — and wiped any field added to the struct.
            if is_custom_stem(name.trim_end_matches(".rs"))
                && self.user_src_files.iter().any(|(p, _)| p == &file_path)
            {
                continue;
            }
            if let Some((_, content)) = self
                .user_src_files
                .iter_mut()
                .find(|(p, _)| p == &file_path)
            {
                if force {
                    // Template swapped (runtime / api style) → replace the whole
                    // file; the editable region carries the init that must change.
                    if *content != *body {
                        *content = body.clone();
                    }
                } else if let Some(block) = extract_gen_block(body) {
                    let existing = content.clone();
                    let updated = splice_pin_file(&existing, &block);
                    if *content != updated {
                        *content = updated;
                    }
                }
            } else {
                // New file: write the full generated content (consts + editable
                // remainder).
                self.user_src_files.push((file_path, body.clone()));
            }
        }

        // 5. Rebuild configs/mod.rs (`pub mod usart1;` …), preserving user code.
        //
        // A Custom module ALSO gets `pub use <stem>::*;`, so its struct is
        // reachable as `pins::configs::MyThing` — main.rs calls it through the
        // full path, but the user's own code shouldn't have to name a file whose
        // stem changes on every Update. The peripheral configs deliberately do
        // NOT get this: they all define `init` / `get_config`, and glob-importing
        // two of them into one namespace is a compile error.
        let gen_section: String = active
            .iter()
            .map(|s| {
                if is_custom_stem(s) {
                    format!("pub mod {s};\npub use {s}::*;\n")
                } else {
                    format!("pub mod {s};\n")
                }
            })
            .collect();
        let wrapped_mod = format!("{GEN_BEGIN}\n{}\n{GEN_END}\n", gen_section.trim());
        if let Some((_, mod_content)) = self.user_src_files.iter_mut().find(|(p, _)| p == MOD_PATH)
        {
            let existing = mod_content.clone();
            let updated = splice_pin_file(&existing, &wrapped_mod);
            if *mod_content != updated {
                *mod_content = updated;
            }
        }
    }

    /// Initialize the pins/ scaffold (folder + empty mod.rs).
    pub fn init_pins_scaffold(&mut self) {
        let folder = src_path("pins");
        let mod_path = src_path("pins/mod.rs");
        if !self.user_src_folders.contains(&folder) {
            self.user_src_folders.push(folder);
        }
        if !self.user_src_files.iter().any(|(p, _)| p == &mod_path) {
            self.user_src_files.push((mod_path, String::new()));
        }
    }
}

/// Replace the GENERATED section in a pin file while preserving user code.
/// If no markers exist, wraps the new content and appends any existing code.
/// The `// <<< GENERATED>>> … // <<< GENERATED END >>>` block of `content`
/// (markers included), or `None` when absent. Used to splice just the constants
/// block of a config file, leaving the editable remainder untouched.
fn extract_gen_block(content: &str) -> Option<String> {
    const GEN_BEGIN: &str = "// <<< GENERATED>>>";
    const GEN_END: &str = "// <<< GENERATED END >>>";
    let begin = content.find(GEN_BEGIN)?;
    let end = content.find(GEN_END)? + GEN_END.len();
    Some(content[begin..end].to_string())
}

fn splice_pin_file(existing: &str, new_generated: &str) -> String {
    const GEN_BEGIN: &str = "// <<< GENERATED>>>";
    const GEN_END: &str = "// <<< GENERATED END >>>";

    if let (Some(begin_pos), Some(end_pos)) = (existing.find(GEN_BEGIN), existing.find(GEN_END)) {
        let before = &existing[..begin_pos].trim_end();
        let after_end = end_pos + GEN_END.len();
        let after = &existing[after_end..].trim_start();

        // Rebuild with new generated section
        if before.is_empty() && after.is_empty() {
            new_generated.to_string()
        } else if before.is_empty() {
            format!("{}\n\n{}", new_generated.trim(), after)
        } else if after.is_empty() {
            format!("{}\n\n{}", before, new_generated.trim())
        } else {
            format!("{}\n\n{}\n\n{}", before, new_generated.trim(), after)
        }
    } else {
        // No markers found: wrap and append existing code
        if existing.trim().is_empty() {
            new_generated.to_string()
        } else {
            format!("{}\n\n{}", new_generated.trim(), existing)
        }
    }
}

/// Filesystem event type.
#[derive(Debug, Clone)]
pub enum FsEventKind {
    Create,
    Remove,
    Rename { old_rel: String, new_rel: String },
}

/// Generate HAL code for a pin type alias.
fn generate_pin_content(pin_num: usize, pin_name: &str, func: &PinFunction) -> String {
    let Some(mode) = func.hal_gpio_mode() else {
        return String::new();
    };

    // Parse STM32 pin name
    let upper = pin_name.to_uppercase();
    let mut chars = upper.chars();
    if chars.next() != Some('P') {
        return format!(
            "// Pin {pin_num} — {pin_name}\n// Function: {label}\n",
            label = func.label()
        );
    }
    let port = match chars.next() {
        Some(c) => c,
        None => {
            return format!(
                "// Pin {pin_num} — {pin_name}\n// Function: {label}\n",
                label = func.label()
            );
        }
    };
    let idx_str: String = chars.collect();
    let Ok(idx) = idx_str.parse::<u8>() else {
        return format!(
            "// Pin {pin_num} — {pin_name}\n// Function: {label}\n",
            label = func.label()
        );
    };

    let comment = match func {
        PinFunction::GpioInput | PinFunction::GpioOutput => String::new(),
        other => format!(" // {}", other.label()),
    };

    format!(
        "use stm32f1xx_hal::gpio::{{{mode}, Pin}};\n\
         pub type PinType = Pin<'{port}', {idx}, {mode}>;{comment}\n",
    )
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod duplicate_name_tests {
    use super::duplicate_path;

    /// Nothing taken → the plain `_1` form the user asked for.
    #[test]
    fn first_duplicate_gets_suffix_1() {
        assert_eq!(duplicate_path("my_file.rs", |_| false), "my_file_1.rs");
    }

    #[test]
    fn counts_up_past_taken_names() {
        let taken = ["my_file_1.rs", "my_file_2.rs"];
        let p = duplicate_path("my_file.rs", |c| taken.contains(&c));
        assert_eq!(p, "my_file_3.rs");
    }

    /// Duplicating a duplicate must not grow `_1_1`.
    #[test]
    fn duplicate_of_a_duplicate_keeps_the_same_base() {
        let taken = ["my_file.rs", "my_file_1.rs"];
        let p = duplicate_path("my_file_1.rs", |c| taken.contains(&c));
        assert_eq!(p, "my_file_2.rs");
    }

    #[test]
    fn stays_in_the_source_folder() {
        assert_eq!(
            duplicate_path("drivers/uart.rs", |_| false),
            "drivers/uart_1.rs"
        );
    }

    /// A trailing `_` or an all-digit stem is NOT a copy suffix.
    #[test]
    fn only_a_real_numeric_tail_is_stripped() {
        assert_eq!(duplicate_path("foo_.rs", |_| false), "foo__1.rs");
        assert_eq!(duplicate_path("foo_bar.rs", |_| false), "foo_bar_1.rs");
        assert_eq!(duplicate_path("_1.rs", |_| false), "_1_1.rs");
    }

    /// A leading dot is a name, not an extension — and a file may have none.
    #[test]
    fn handles_dotfiles_and_extensionless_names() {
        assert_eq!(duplicate_path(".gitignore", |_| false), ".gitignore_1");
        assert_eq!(duplicate_path("Makefile", |_| false), "Makefile_1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── Helper macros and functions ──────────────────────────────────────────

    macro_rules! setup_temp_project {
        () => {{
            let temp = TempDir::new().unwrap();
            let src = temp.path().join("src");
            fs::create_dir(&src).unwrap();
            (temp, src)
        }};
    }

    fn assert_file_exists(state: &ProjectTreeState, path: &str) {
        assert!(
            state.user_src_files.iter().any(|(p, _)| p == path),
            "File {} not found in state",
            path
        );
    }

    fn assert_file_not_exists(state: &ProjectTreeState, path: &str) {
        assert!(
            !state.user_src_files.iter().any(|(p, _)| p == path),
            "File {} found but shouldn't exist",
            path
        );
    }

    fn assert_file_content(state: &ProjectTreeState, path: &str, expected: &str) {
        let entry = state
            .user_src_files
            .iter()
            .find(|(p, _)| p == path)
            .expect(&format!("File {} not found", path));
        assert_eq!(entry.1, expected, "Content mismatch for {}", path);
    }

    fn assert_folder_exists(state: &ProjectTreeState, path: &str) {
        assert!(
            state.user_src_folders.contains(&path.to_string()),
            "Folder {} not found",
            path
        );
    }

    // ── Initialization Tests ─────────────────────────────────────────────────

    #[test]
    fn test_new_empty_state() {
        let state = ProjectTreeState::new();
        assert!(state.user_src_files.is_empty());
        assert!(state.user_src_folders.is_empty());
    }

    #[test]
    fn test_load_empty_directory() {
        let (_temp, src) = setup_temp_project!();
        let parent = src.parent().unwrap();
        let state = ProjectTreeState::load_from_dir(parent);
        assert!(state.user_src_files.is_empty());
        assert!(state.user_src_folders.is_empty());
    }

    #[test]
    fn test_load_picks_up_a_detached_library() {
        // A cloned lib that is NOT a `[workspace] member` must still be scanned
        // in, or it would vanish from the tree on every reload/restart.
        let (temp, src) = setup_temp_project!();
        let root = src.parent().unwrap();
        fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
        // Root manifest with NO members (the firmware only).
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"fw\"\n").unwrap();
        // A detached library: its own dir + Cargo.toml + a source file.
        let lib = root.join("mmwave");
        fs::create_dir(&lib).unwrap();
        fs::write(lib.join("Cargo.toml"), "[package]\nname = \"mmwave\"\n").unwrap();
        fs::create_dir(lib.join("src")).unwrap();
        fs::write(lib.join("src").join("lib.rs"), "pub fn go() {}\n").unwrap();
        // A hidden dir + `target` must NOT be treated as a library.
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git").join("Cargo.toml"), "x").unwrap();

        let state = ProjectTreeState::load_from_dir(root);
        assert_folder_exists(&state, "mmwave");
        assert_file_exists(&state, "mmwave/Cargo.toml");
        assert_file_exists(&state, "mmwave/src/lib.rs");
        assert_file_not_exists(&state, ".git/Cargo.toml");
        drop(temp);
    }

    #[test]
    fn test_load_nonexistent_path() {
        let nonexistent = PathBuf::from("/nonexistent/path/123456");
        let state = ProjectTreeState::load_from_dir(&nonexistent);
        assert!(state.user_src_files.is_empty());
        assert!(state.user_src_folders.is_empty());
    }

    // ── File Discovery Tests ─────────────────────────────────────────────────

    #[test]
    fn test_load_discovers_single_file() {
        let (_temp, src) = setup_temp_project!();
        fs::write(src.join("utils.rs"), "pub fn helper() {}").unwrap();

        let parent = src.parent().unwrap();
        let state = ProjectTreeState::load_from_dir(parent);

        assert_eq!(state.user_src_files.len(), 1);
        assert_file_exists(&state, "src/utils.rs");
        assert_file_content(&state, "src/utils.rs", "pub fn helper() {}");
    }

    #[test]
    fn test_load_discovers_nested_files() {
        let (_temp, src) = setup_temp_project!();
        fs::create_dir(src.join("helpers")).unwrap();
        fs::write(src.join("helpers/math.rs"), "").unwrap();
        fs::write(src.join("helpers/strings.rs"), "").unwrap();

        let parent = src.parent().unwrap();
        let state = ProjectTreeState::load_from_dir(parent);

        assert_file_exists(&state, "src/helpers/math.rs");
        assert_file_exists(&state, "src/helpers/strings.rs");
    }

    #[test]
    fn test_load_discovers_all_file_types() {
        let (_temp, src) = setup_temp_project!();
        fs::write(src.join("file.rs"), "").unwrap();
        fs::write(src.join("config.txt"), "").unwrap();
        fs::write(src.join("data.json"), "").unwrap();

        let parent = src.parent().unwrap();
        let state = ProjectTreeState::load_from_dir(parent);

        assert_file_exists(&state, "src/file.rs");
        assert_file_exists(&state, "src/config.txt");
        assert_file_exists(&state, "src/data.json");
    }

    #[test]
    fn test_load_skips_main_rs() {
        let (_temp, src) = setup_temp_project!();
        fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        fs::write(src.join("utils.rs"), "").unwrap();

        let parent = src.parent().unwrap();
        let state = ProjectTreeState::load_from_dir(parent);

        assert_file_not_exists(&state, "src/main.rs");
        assert_file_exists(&state, "src/utils.rs");
    }

    #[test]
    fn test_load_registers_folders() {
        let (_temp, src) = setup_temp_project!();
        fs::create_dir(src.join("helpers")).unwrap();
        fs::create_dir(src.join("core")).unwrap();
        fs::write(src.join("helpers/file.txt"), "").unwrap();
        fs::write(src.join("core/file.txt"), "").unwrap();

        let parent = src.parent().unwrap();
        let state = ProjectTreeState::load_from_dir(parent);

        assert_folder_exists(&state, "src/helpers");
        assert_folder_exists(&state, "src/core");
    }

    #[test]
    fn test_load_normalizes_paths() {
        let (_temp, src) = setup_temp_project!();
        fs::create_dir(src.join("utils")).unwrap();
        fs::write(src.join("utils/helper.rs"), "").unwrap();

        let parent = src.parent().unwrap();
        let state = ProjectTreeState::load_from_dir(parent);

        // All paths should use forward slashes
        for (path, _) in &state.user_src_files {
            assert!(!path.contains('\\'), "Path contains backslashes: {}", path);
        }
    }

    // ── Filesystem Events Tests ──────────────────────────────────────────────

    #[test]
    fn test_handle_create_event() {
        let mut state = ProjectTreeState::new();
        state.handle_fs_events(vec![("src/utils.rs".to_string(), FsEventKind::Create)]);

        assert_eq!(state.user_src_files.len(), 1);
        assert_file_exists(&state, "src/utils.rs");
        assert_file_content(&state, "src/utils.rs", "");
    }

    #[test]
    fn test_handle_create_event_skips_duplicates() {
        let mut state = ProjectTreeState::new();
        state
            .user_src_files
            .push(("src/utils.rs".to_string(), "existing content".to_string()));

        state.handle_fs_events(vec![("src/utils.rs".to_string(), FsEventKind::Create)]);

        assert_eq!(state.user_src_files.len(), 1);
        assert_file_content(&state, "src/utils.rs", "existing content");
    }

    #[test]
    fn test_handle_remove_file_event() {
        let mut state = ProjectTreeState::new();
        state
            .user_src_files
            .push(("src/utils.rs".to_string(), "content".to_string()));

        state.handle_fs_events(vec![("src/utils.rs".to_string(), FsEventKind::Remove)]);

        assert!(state.user_src_files.is_empty());
    }

    #[test]
    fn test_handle_remove_folder_event() {
        let mut state = ProjectTreeState::new();
        state.user_src_folders.push("src/helpers".to_string());
        state
            .user_src_files
            .push(("src/helpers/math.rs".to_string(), "".to_string()));
        state
            .user_src_files
            .push(("src/utils.rs".to_string(), "".to_string()));

        // When a folder is removed, the folder entry itself is removed
        state.handle_fs_events(vec![("src/helpers".to_string(), FsEventKind::Remove)]);

        assert!(!state.user_src_folders.contains(&"src/helpers".to_string()));
        // Its files go with it: a folder moved OUT of the watched tree is one
        // Remove on every backend, with no event for anything inside.
        assert_file_not_exists(&state, "src/helpers/math.rs");
        assert_file_exists(&state, "src/utils.rs");
    }

    #[test]
    fn test_handle_rename_file() {
        let mut state = ProjectTreeState::new();
        state
            .user_src_files
            .push(("old.rs".to_string(), "content".to_string()));

        state.handle_fs_events(vec![(
            "old.rs".to_string(),
            FsEventKind::Rename {
                old_rel: "old.rs".to_string(),
                new_rel: "new.rs".to_string(),
            },
        )]);

        assert_file_not_exists(&state, "old.rs");
        assert_file_exists(&state, "new.rs");
        assert_file_content(&state, "new.rs", "content");
    }

    #[test]
    fn test_handle_rename_folder_updates_children() {
        let mut state = ProjectTreeState::new();
        state.user_src_folders.push("oldname".to_string());
        state
            .user_src_files
            .push(("oldname/file1.rs".to_string(), "".to_string()));
        state
            .user_src_files
            .push(("oldname/file2.rs".to_string(), "".to_string()));

        state.handle_fs_events(vec![(
            "oldname".to_string(),
            FsEventKind::Rename {
                old_rel: "oldname".to_string(),
                new_rel: "newname".to_string(),
            },
        )]);

        assert_folder_exists(&state, "newname");
        assert_file_not_exists(&state, "oldname/file1.rs");
        assert_file_not_exists(&state, "oldname/file2.rs");
        assert_file_exists(&state, "newname/file1.rs");
        assert_file_exists(&state, "newname/file2.rs");
    }

    #[test]
    fn test_handle_multiple_events_sequence() {
        let mut state = ProjectTreeState::new();

        // Create
        state.handle_fs_events(vec![("a.rs".to_string(), FsEventKind::Create)]);
        assert_eq!(state.user_src_files.len(), 1);

        // Rename
        state.handle_fs_events(vec![(
            "a.rs".to_string(),
            FsEventKind::Rename {
                old_rel: "a.rs".to_string(),
                new_rel: "b.rs".to_string(),
            },
        )]);
        assert_file_not_exists(&state, "a.rs");
        assert_file_exists(&state, "b.rs");

        // Remove
        state.handle_fs_events(vec![("b.rs".to_string(), FsEventKind::Remove)]);
        assert!(state.user_src_files.is_empty());
    }

    // ── Pin Synchronization Tests ────────────────────────────────────────────

    #[test]
    fn test_sync_creates_pins_folder() {
        let mut state = ProjectTreeState::new();
        state.sync_pin_files(&[]);

        assert_folder_exists(&state, "src/pins");
    }

    #[test]
    fn test_sync_creates_mod_rs() {
        let mut state = ProjectTreeState::new();
        state.sync_pin_files(&[]);

        assert_file_exists(&state, "src/pins/mod.rs");
    }

    #[test]
    fn test_sync_preserves_custom_code() {
        let mut state = ProjectTreeState::new();
        let custom_code = "pub mod custom_utils;\npub fn helper() {}";
        let mod_content = format!(
            "// <<< GENERATED>>>\npub mod pin1_pa0_out;\n// <<< GENERATED END >>>\n\n{}",
            custom_code
        );
        state
            .user_src_files
            .push(("src/pins/mod.rs".to_string(), mod_content));
        state.user_src_folders.push("src/pins".to_string());

        let pins = vec![(1usize, "PA0".to_string(), PinFunction::GpioOutput)];
        state.sync_pin_files(&pins);

        let mod_file = state
            .user_src_files
            .iter()
            .find(|(p, _)| p == "src/pins/mod.rs")
            .unwrap();
        assert!(mod_file.1.contains("pub mod custom_utils;"));
        assert!(mod_file.1.contains("pub fn helper() {}"));
    }

    #[test]
    fn test_sync_ignores_unset_pins() {
        let mut state = ProjectTreeState::new();
        let pins = vec![(1usize, "PA0".to_string(), PinFunction::Unset)];
        state.sync_pin_files(&pins);

        assert_file_not_exists(&state, "src/pins/pin1_pa0.rs");
        let mod_file = state
            .user_src_files
            .iter()
            .find(|(p, _)| p == "src/pins/mod.rs")
            .unwrap();
        assert!(mod_file.1.trim().is_empty() || !mod_file.1.contains("pub mod"));
    }

    /// Per-pin files were disabled in `4899d16`, which turned step 3 into a
    /// SWEEPER: a project generated before that commit still carries
    /// `src/pins/pin*.rs`, and they must be cleared out rather than left to be
    /// declared by a `mod.rs` that no longer mentions them (which would not
    /// compile). Everything else under `src/pins/` is the user's and stays.
    #[test]
    fn test_sync_sweeps_pin_files_left_by_older_projects() {
        let mut state = ProjectTreeState::new();
        for path in [
            "src/pins/pin1_pa0_out.rs",  // legacy generated pin file
            "src/pins/pin13_pc13_in.rs", // …and another
            "src/pins/mod.rs",
            "src/pins/utils/gpio_out.rs", // user's own helpers — keep
            "src/pins/my_notes.rs",       // not a `pin*` file — keep
        ] {
            state.user_src_files.push((path.to_string(), String::new()));
        }

        state.sync_pin_files(&[(1usize, "PA0".to_string(), PinFunction::GpioOutput)]);

        assert_file_not_exists(&state, "src/pins/pin1_pa0_out.rs");
        assert_file_not_exists(&state, "src/pins/pin13_pc13_in.rs");
        assert_file_exists(&state, "src/pins/mod.rs");
        assert_file_exists(&state, "src/pins/utils/gpio_out.rs");
        assert_file_exists(&state, "src/pins/my_notes.rs");
    }

    /// `pins/mod.rs` must declare `pub mod configs;` exactly when the
    /// per-peripheral init modules exist — this is the live half of the
    /// function, and the STM32 config files do not compile without it.
    #[test]
    fn test_sync_declares_configs_module_only_when_present() {
        let mut state = ProjectTreeState::new();
        state.sync_pin_files(&[]);
        let mod_of = |s: &ProjectTreeState| {
            s.user_src_files
                .iter()
                .find(|(p, _)| p == "src/pins/mod.rs")
                .map(|(_, c)| c.clone())
                .expect("mod.rs")
        };
        assert!(
            !mod_of(&state).contains("pub mod configs;"),
            "nothing to declare yet"
        );

        state.user_src_files.push((
            "src/pins/configs/usart1.rs".to_string(),
            "pub fn init() {}".to_string(),
        ));
        state.sync_pin_files(&[]);
        assert!(
            mod_of(&state).contains("pub mod configs;"),
            "configs/ exists -> declared:\n{}",
            mod_of(&state)
        );

        // …and it goes away again with the last config file.
        state
            .user_src_files
            .retain(|(p, _)| !p.starts_with("src/pins/configs/"));
        state.sync_pin_files(&[]);
        assert!(
            !mod_of(&state).contains("pub mod configs;"),
            "declaration removed with the folder:\n{}",
            mod_of(&state)
        );
    }

    #[test]
    fn test_init_pins_scaffold() {
        let mut state = ProjectTreeState::new();
        state.init_pins_scaffold();

        assert_folder_exists(&state, "src/pins");
        assert_file_exists(&state, "src/pins/mod.rs");
    }

    #[test]
    fn test_init_pins_scaffold_idempotent() {
        let mut state = ProjectTreeState::new();
        state.init_pins_scaffold();
        let count_before = state.user_src_folders.len() + state.user_src_files.len();

        state.init_pins_scaffold();
        let count_after = state.user_src_folders.len() + state.user_src_files.len();

        assert_eq!(count_before, count_after, "Scaffold should be idempotent");
    }

    /// `configs/mod.rs` re-exports a Custom module's contents so its struct is
    /// reachable as `pins::configs::MyThing` — the stem changes on every Update,
    /// so user code must not have to name it. Peripheral configs must NOT get the
    /// glob: they all define `init`, and two of them in one namespace won't build.
    #[test]
    fn configs_mod_reexports_custom_modules_only() {
        let mut state = ProjectTreeState::new();
        let body = "// <<< GENERATED>>>\n// <<< GENERATED END >>>\n";
        state.sync_config_files(
            &[
                ("usart1.rs".to_string(), body.to_string()),
                ("custom_led_2.rs".to_string(), body.to_string()),
            ],
            false,
            &["custom_led".to_string()],
        );
        let mod_rs = &state
            .user_src_files
            .iter()
            .find(|(p, _)| p == "src/pins/configs/mod.rs")
            .unwrap()
            .1;
        assert!(mod_rs.contains("pub mod custom_led_2;"), "{mod_rs}");
        assert!(mod_rs.contains("pub use custom_led_2::*;"), "{mod_rs}");
        assert!(mod_rs.contains("pub mod usart1;"), "{mod_rs}");
        assert!(!mod_rs.contains("pub use usart1::*;"), "{mod_rs}");
    }

    /// A Custom module's file is written once and then left alone: neither a
    /// normal regeneration nor a forced one (Runtime Apply) may touch it. The
    /// reported case — a field added to the struct gone after every IDE start.
    #[test]
    fn an_existing_custom_module_file_is_never_rewritten() {
        let mut state = ProjectTreeState::new();
        let path = "src/pins/configs/custom_menu_nav.rs";
        let keep = ["custom_menu_nav".to_string()];
        let generated = "pub struct Encoder<A> {\n    pub a: A,\n}\n";
        state.sync_config_files(
            &[("custom_menu_nav.rs".to_string(), generated.to_string())],
            false,
            &keep,
        );
        let edited = "pub struct Encoder<A> {\n    pub a: A,\n    pub was_pressed: bool,\n}\n";
        state
            .user_src_files
            .iter_mut()
            .find(|(p, _)| p == path)
            .expect("written on first sync")
            .1 = edited.to_string();

        for force in [false, true] {
            state.sync_config_files(
                &[("custom_menu_nav.rs".to_string(), generated.to_string())],
                force,
                &keep,
            );
            let body = &state
                .user_src_files
                .iter()
                .find(|(p, _)| p == path)
                .unwrap()
                .1;
            assert_eq!(body, edited, "force = {force}");
        }

        // An Update is a new revision file: written, and the old one kept.
        state.sync_config_files(
            &[("custom_menu_nav_1.rs".to_string(), generated.to_string())],
            false,
            &keep,
        );
        let new = state
            .user_src_files
            .iter()
            .find(|(p, _)| p == "src/pins/configs/custom_menu_nav_1.rs")
            .expect("the new revision is written");
        assert_eq!(new.1, generated);
        let old = &state
            .user_src_files
            .iter()
            .find(|(p, _)| p == path)
            .unwrap()
            .1;
        assert_eq!(
            old, edited,
            "the previous revision stays as the user left it"
        );
    }

    /// A config file's constants (inside the GENERATED block) regenerate on a
    /// config change, but the editable remainder (use block + init fns the user
    /// may have edited) is preserved.
    #[test]
    fn config_file_constants_regenerate_body_preserved() {
        let mut state = ProjectTreeState::new();
        let path = "src/pins/configs/usart1.rs";
        let v1 = "// <<< GENERATED>>>\nconst BAUDRATE: u32 = 115200;\n// <<< GENERATED END >>>\n\nuse foo;\npub fn init() { /* orig */ }\n";
        state.sync_config_files(&[("usart1.rs".to_string(), v1.to_string())], false, &[]);
        assert!(state.user_src_files.iter().any(|(p, _)| p == path));

        // User edits the EDITABLE part (below the markers).
        {
            let f = state
                .user_src_files
                .iter_mut()
                .find(|(p, _)| p == path)
                .unwrap();
            f.1 = f.1.replace("/* orig */", "/* MY EDIT */");
        }

        // Regenerate with a new baud rate (only the constants block changes).
        let v2 = "// <<< GENERATED>>>\nconst BAUDRATE: u32 = 9600;\n// <<< GENERATED END >>>\n\nuse foo;\npub fn init() { /* orig */ }\n";
        state.sync_config_files(&[("usart1.rs".to_string(), v2.to_string())], false, &[]);

        let body = &state
            .user_src_files
            .iter()
            .find(|(p, _)| p == path)
            .unwrap()
            .1;
        assert!(
            body.contains("const BAUDRATE: u32 = 9600;"),
            "const updated:\n{body}"
        );
        assert!(!body.contains("115200"), "old const gone");
        assert!(
            body.contains("/* MY EDIT */"),
            "user body edit preserved:\n{body}"
        );
    }

    /// A Runtime / Init-API Apply passes `force = true`: the WHOLE config file is
    /// replaced (the editable `init()` template changes blocking → native/async),
    /// so a constants-only splice would leave stale code — the bug behind "MCU
    /// System code doesn't update on change".
    #[test]
    fn config_file_force_rewrites_the_whole_template() {
        let mut state = ProjectTreeState::new();
        let path = "src/pins/configs/usart1.rs";
        // Blocking (portable) template — its init lives BELOW the markers.
        let portable = "// <<< GENERATED>>>\nconst BAUDRATE: u32 = 115200;\n// <<< GENERATED END >>>\n\nuse portable;\npub fn init() -> SerialIo { /* portable */ }\n";
        state.sync_config_files(
            &[("usart1.rs".to_string(), portable.to_string())],
            false,
            &[],
        );

        // Apply switches the runtime → a completely different (native) template.
        let native = "// <<< GENERATED>>>\nconst BAUDRATE: u32 = 115200;\n// <<< GENERATED END >>>\n\nuse native;\npub fn init() -> (Tx, Rx) { /* native */ }\n";
        state.sync_config_files(&[("usart1.rs".to_string(), native.to_string())], true, &[]);

        let body = &state
            .user_src_files
            .iter()
            .find(|(p, _)| p == path)
            .unwrap()
            .1;
        assert!(body.contains("(Tx, Rx)"), "new template applied:\n{body}");
        assert!(!body.contains("SerialIo"), "old template gone:\n{body}");
    }
}
