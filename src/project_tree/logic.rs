//! Project tree logic — file operations, directory scanning, filesystem watching.

use crate::panels::mcu_module::codegen::common as codegen;
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use std::path::Path;

/// State for the project tree (files and folders in src/).
#[derive(Debug, Clone)]
pub struct ProjectTreeState {
    /// `(path_relative_to_src, content)` for every user-created file.
    pub user_src_files: Vec<(String, String)>,
    /// Explicitly-created folders inside src/.
    pub user_src_folders: Vec<String>,
    /// `(path, content)` of every `pins/configs/` file the sync pruned this
    /// session, newest content per path - per path AND device for an I2C
    /// device file, whose path the next device takes over on a renumber. A
    /// file that comes back - the bus re-wired after a pad was cleared, a
    /// Runtime switched away and back, a device's Remove undone - comes back
    /// with what the user wrote in it, not as a fresh template. Memory only: a
    /// new or opened project starts empty.
    pub config_graveyard: Vec<(String, String)>,
}

/// What [`ProjectTreeState::sync_config_files`] did besides splicing - what
/// the app has to follow up on the disk, in rust-analyzer and in state keyed
/// by path.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConfigSync {
    /// `(old, new)` of every file that moved, its content with it: a device
    /// renamed or renumbered, or a file from before buses were folders.
    pub moved: Vec<(String, String)>,
    /// Every file pruned (its content is in the graveyard).
    pub removed: Vec<String>,
    /// `(old path, content before the move)` of each file from before buses
    /// were folders that was moved into one.
    pub migrated: Vec<(String, String)>,
    /// `(path, content)` of an old `<bus>.rs` dropped because `<bus>/mod.rs`
    /// was there too - the two at once do not compile.
    pub conflicts: Vec<(String, String)>,
    /// `(path, content)` of a device file from before buses were folders
    /// that no device claimed: dropped, so the user has to hear where a copy
    /// is - nothing will ever generate its flat name again.
    pub unplaced: Vec<(String, String)>,
}

impl ConfigSync {
    /// Did a file leave a path - so the workspace copy and rust-analyzer still
    /// have it where it was?
    pub fn left_a_path(&self) -> bool {
        !self.moved.is_empty() || !self.removed.is_empty()
    }
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
            config_graveyard: Vec::new(),
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
            config_graveyard: Vec::new(),
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
        self.take_files(|_, p| drop(p), selected);
    }

    /// Take out every file `drop(index, path)` picks, returning them, and carry
    /// `selected` across the shift (`None` when its own file goes).
    fn take_files(
        &mut self,
        drop: impl Fn(usize, &str) -> bool,
        selected: &mut Option<usize>,
    ) -> Vec<(String, String)> {
        let sel = *selected;
        let mut dropped_before = 0;
        let mut sel_dropped = false;
        let mut kept = Vec::with_capacity(self.user_src_files.len());
        let mut gone = Vec::new();
        for (i, entry) in std::mem::take(&mut self.user_src_files)
            .into_iter()
            .enumerate()
        {
            if drop(i, &entry.0) {
                match sel {
                    Some(s) if s == i => sel_dropped = true,
                    Some(s) if i < s => dropped_before += 1,
                    _ => {}
                }
                gone.push(entry);
            } else {
                kept.push(entry);
            }
        }
        self.user_src_files = kept;
        *selected = if sel_dropped {
            None
        } else {
            sel.map(|s| s - dropped_before)
        };
        gone
    }

    /// Keep what the sync pruned, so it can come back ([`Self::config_graveyard`]).
    ///
    /// One per path - but an I2C device file is one per path AND device id: a
    /// renumber hands `device1.rs` to the next device, and burying that one's
    /// file must not erase the first device's, which an undo still wants.
    fn bury(&mut self, gone: Vec<(String, String)>, report: &mut ConfigSync) {
        let id = |body: &str| codegen::device_file_id(body).map(|(_, uid)| uid);
        for (path, body) in gone {
            report.removed.push(path.clone());
            let uid = id(&body);
            self.config_graveyard
                .retain(|(p, b)| !(*p == path && id(b) == uid));
            self.config_graveyard.push((path, body));
        }
    }

    /// The content last pruned from `path`, taken out of the graveyard.
    fn unbury(&mut self, path: &str) -> Option<String> {
        let i = self.config_graveyard.iter().position(|(p, _)| p == path)?;
        Some(self.config_graveyard.remove(i).1)
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
    ///
    /// A name may carry ONE folder level (`i2c1/mod.rs`, `i2c1/device1.rs`):
    /// `configs/mod.rs` then declares the folder once (`pub mod i2c1;`) and the
    /// folder's own `mod.rs` declares what is inside it. Such a folder is an I2C
    /// bus, and its device files MOVE when a device is renamed or renumbered -
    /// see [`Self::carry_bus_folders`] - instead of being pruned and written
    /// anew; they are never rewritten whole by `force` either, their template
    /// has nothing a Runtime changes.
    ///
    /// Nothing pruned is lost for the session: it goes to
    /// [`Self::config_graveyard`] and comes back if its file is generated again.
    ///
    /// `selected` is the index of the file open in the editor, which names its
    /// file by index; it follows that file across every index a prune shifts,
    /// and becomes `None` when the file itself is pruned.
    pub fn sync_config_files(
        &mut self,
        files: &[(String, String)],
        force: bool,
        // Stems whose OLDER files must survive the prune below — a Custom module
        // writes each Update to a new `custom_<name>_<n>.rs`, and the previous
        // revisions are kept on disk (they are not in `configs/mod.rs`, so they
        // are never compiled and can't clash with the current struct).
        keep_prefixes: &[String],
        selected: &mut Option<usize>,
    ) -> ConfigSync {
        const DIR: &str = "src/pins/configs";
        const MOD_PATH: &str = "src/pins/configs/mod.rs";
        const GEN_BEGIN: &str = "// <<< GENERATED>>>";
        const GEN_END: &str = "// <<< GENERATED END >>>";
        const UNDER: &str = "src/pins/configs/";
        let mut report = ConfigSync::default();

        if files.is_empty() {
            // No configured peripherals → drop the entire configs/ subtree.
            let gone = self.take_files(|_, p| p.starts_with(UNDER), selected);
            self.bury(gone, &mut report);
            self.user_src_folders
                .retain(|f| f != DIR && !f.starts_with(UNDER));
            return report;
        }

        // 1. Register the folder, and every folder a nested name lives in.
        if !self.user_src_folders.iter().any(|f| f == DIR) {
            self.user_src_folders.push(DIR.to_string());
        }
        for (name, _) in files {
            if let Some((folder, _)) = name.rsplit_once('/') {
                let folder = format!("{UNDER}{folder}");
                if !self.user_src_folders.contains(&folder) {
                    self.user_src_folders.push(folder);
                }
            }
        }
        // 2. Ensure configs/mod.rs exists - with what the user wrote around its
        //    markers, if a moment with no config files at all pruned it.
        if !self.user_src_files.iter().any(|(p, _)| p == MOD_PATH) {
            let old = self.unbury(MOD_PATH).unwrap_or_default();
            self.user_src_files.push((MOD_PATH.to_string(), old));
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

        // 3. Move what is already written to where it goes now - an I2C bus
        //    file from before buses were folders, and every device file whose
        //    name changed - BEFORE the prune below, which would take the
        //    user's code with it.
        self.carry_bus_folders(files, selected, &mut report);

        //    Then drop config files no longer configured. A Custom module's
        //    revisions are flat names, never nested.
        let gone = self.take_files(
            |_, path| {
                let Some(rest) = path.strip_prefix(UNDER) else {
                    return false;
                };
                let stem = rest.trim_end_matches(".rs");
                !(rest == "mod.rs"
                    || active.iter().any(|a| a == stem)
                    || (!rest.contains('/') && is_custom_stem(stem)))
            },
            selected,
        );
        self.bury(gone, &mut report);

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
            // A device file's template holds nothing a Runtime changes, and
            // everything below its markers is the user's.
            let is_device = name
                .split_once('/')
                .is_some_and(|(_, file)| codegen::parse_device_file_name(file).is_some());
            // Generated again after being pruned this session: back with the
            // user's code, and spliced (or forced) below like any file. Not a
            // device file: step 3 already chose ITS old content by device,
            // and a path alone may belong to another one.
            if !is_device
                && !self.user_src_files.iter().any(|(p, _)| p == &file_path)
                && let Some(old) = self.unbury(&file_path)
            {
                self.user_src_files.push((file_path.clone(), old));
            }
            if let Some((_, content)) = self
                .user_src_files
                .iter_mut()
                .find(|(p, _)| p == &file_path)
            {
                if force && !is_device {
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

        // A folder the prune emptied goes too - after step 4, which is what
        // fills a folder registered in step 1.
        let files_now = &self.user_src_files;
        self.user_src_folders.retain(|f| {
            if !f.starts_with(UNDER) {
                return true;
            }
            let inside = format!("{f}/");
            files_now.iter().any(|(p, _)| p.starts_with(&inside))
        });

        // 5. Rebuild configs/mod.rs (`pub mod usart1;` …), preserving user code.
        //
        // A Custom module ALSO gets `pub use <stem>::*;`, so its struct is
        // reachable as `pins::configs::MyThing` — main.rs calls it through the
        // full path, but the user's own code shouldn't have to name a file whose
        // stem changes on every Update. The peripheral configs deliberately do
        // NOT get this: they all define `init` / `get_config`, and glob-importing
        // two of them into one namespace is a compile error.
        //
        // A folder is ONE module however many files it holds: `i2c1/mod.rs`
        // declares it, and its other files are declared by that `mod.rs`.
        let mut top: Vec<&str> = Vec::new();
        for s in &active {
            let seg = match s.split_once('/') {
                None => s.as_str(),
                Some((folder, "mod")) => folder,
                Some(_) => continue,
            };
            if !top.contains(&seg) {
                top.push(seg);
            }
        }
        let gen_section: String = top
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
        debug_assert!(
            {
                let mut paths: Vec<&str> = self
                    .user_src_files
                    .iter()
                    .map(|(p, _)| p.as_str())
                    .filter(|p| p.starts_with(UNDER))
                    .collect();
                paths.sort_unstable();
                paths.windows(2).all(|w| w[0] != w[1])
            },
            "two files on one path after a config sync"
        );
        report
    }

    /// Step 3 of [`Self::sync_config_files`], for every I2C bus in `files`
    /// (a `<bus>/mod.rs`):
    ///
    /// - The bus file from before buses were folders, `<bus>.rs`, becomes the
    ///   folder's `mod.rs` in place - the same entry, so the user's `init`
    ///   and the editor's selection stay. When `<bus>/mod.rs` is there too,
    ///   `<bus>.rs` is left to the prune and reported as a conflict: both at
    ///   once do not compile.
    /// - Each device file that exists - in the folder, as a flat
    ///   `<bus>_<name>.rs` from before the folders, or in the graveyard - is
    ///   paired with the device file it is now ([`match_devices`]) and moved
    ///   there. One left unpaired is buried BY INDEX: the file taking its
    ///   path may already stand on it. An unpaired flat one is also reported
    ///   (`unplaced`), since nothing will ever bring its name back.
    ///
    /// Nothing is remembered between calls. What pairs a file with its device
    /// is written in the files themselves - the device's id, name and address -
    /// so a restart, an Open or a branch switch changes nothing.
    fn carry_bus_folders(
        &mut self,
        files: &[(String, String)],
        selected: &mut Option<usize>,
        report: &mut ConfigSync,
    ) {
        const UNDER: &str = "src/pins/configs/";
        let buses: Vec<&str> = files
            .iter()
            .filter_map(|(n, _)| n.strip_suffix("/mod.rs"))
            .collect();
        for bus in buses {
            let mod_path = format!("{UNDER}{bus}/mod.rs");
            let flat_path = format!("{UNDER}{bus}.rs");
            if let Some(i) = self
                .user_src_files
                .iter()
                .position(|(p, _)| *p == flat_path)
            {
                let body = self.user_src_files[i].1.clone();
                if self.user_src_files.iter().any(|(p, _)| *p == mod_path) {
                    report.conflicts.push((flat_path, body));
                } else {
                    report.migrated.push((flat_path.clone(), body));
                    report.moved.push((flat_path, mod_path.clone()));
                    self.user_src_files[i].0 = mod_path;
                }
            }

            let folder = format!("{UNDER}{bus}/");
            let mut targets: Vec<DevSig> = files
                .iter()
                .filter_map(|(name, body)| {
                    let file = name.strip_prefix(bus)?.strip_prefix('/')?;
                    let (k, slug) = codegen::parse_device_file_name(file)?;
                    Some(DevSig::nested(format!("{UNDER}{name}"), k, slug, body, bus))
                })
                .collect();
            // The flat name each device would have had before the folders.
            let legacy = {
                let devices: Vec<(usize, &str)> = targets
                    .iter()
                    .map(|t| (t.k.unwrap_or(0), t.slugs[0].as_str()))
                    .collect();
                codegen::legacy_device_stems_for(bus, &devices)
            };
            for (t, stem) in targets.iter_mut().zip(legacy) {
                t.legacy = Some(stem);
            }
            if targets.is_empty()
                && !self
                    .user_src_files
                    .iter()
                    .any(|(p, _)| p.starts_with(&folder))
            {
                continue;
            }
            // Where each candidate is: the tree (index) or the graveyard.
            let mut olds: Vec<(Result<usize, usize>, DevSig)> = Vec::new();
            for (i, (path, body)) in self.user_src_files.iter().enumerate() {
                if let Some(file) = path.strip_prefix(&folder) {
                    if let Some((k, slug)) = codegen::parse_device_file_name(file) {
                        olds.push((Ok(i), DevSig::nested(path.clone(), k, slug, body, bus)));
                    }
                } else if let Some(rest) = path
                    .strip_prefix(UNDER)
                    .and_then(|r| r.strip_prefix(bus))
                    .and_then(|r| r.strip_prefix('_'))
                    .and_then(|r| r.strip_suffix(".rs"))
                    && !rest.contains('/')
                {
                    // A flat `<bus>_<name>.rs` is a device file from before the
                    // folders whatever it holds: nothing else was ever
                    // generated under that name, and the old bus file is
                    // `<bus>.rs`, no `_`.
                    olds.push((Ok(i), DevSig::legacy(path.clone(), rest, body, bus)));
                }
            }
            // Every grave of the folder, also one on a path the tree holds: a
            // renumber hands a path to the next device, and the device whose
            // grave it is decides - not the path.
            for (g, (path, body)) in self.config_graveyard.iter().enumerate() {
                let Some(file) = path.strip_prefix(&folder) else {
                    continue;
                };
                if let Some((k, slug)) = codegen::parse_device_file_name(file) {
                    let mut sig = DevSig::nested(path.clone(), k, slug, body, bus);
                    sig.grave = true;
                    olds.push((Err(g), sig));
                }
            }

            let sigs: Vec<&DevSig> = olds.iter().map(|(_, s)| s).collect();
            let pairs = match_devices(&sigs, &targets);
            let mut paired = vec![false; olds.len()];
            let mut revived: Vec<(usize, String)> = Vec::new();
            for (o, t) in pairs {
                paired[o] = true;
                let to = targets[t].path.clone();
                match olds[o].0 {
                    Ok(i) => {
                        let from = self.user_src_files[i].0.clone();
                        if from == to {
                            continue;
                        }
                        if !from.starts_with(&folder) {
                            // From before the folders: its warning that a
                            // rename loses the code is no longer true.
                            let body = self.user_src_files[i].1.clone();
                            self.user_src_files[i].1 = body
                                .replace(codegen::LEGACY_DEVICE_WARNING, codegen::DEVICE_MOVE_NOTE);
                            report.migrated.push((from.clone(), body));
                        }
                        report.moved.push((from, to.clone()));
                        self.user_src_files[i].0 = to;
                    }
                    Err(g) => revived.push((g, to)),
                }
            }
            // Out of the graveyard BEFORE anything is buried, which reorders
            // it - and from the highest index down, so each one still holds.
            revived.sort_by_key(|a| std::cmp::Reverse(a.0));
            let mut graves: Vec<(String, String)> = Vec::new();
            for (g, to) in revived {
                graves.push((to, self.config_graveyard.remove(g).1));
            }
            let drop: Vec<usize> = olds
                .iter()
                .zip(&paired)
                .filter_map(|((at, _), p)| match at {
                    Ok(i) if !p => Some(*i),
                    _ => None,
                })
                .collect();
            // A flat file no device claimed is never generated again, so the
            // graveyard can never give it back: the user hears where a copy is.
            for i in &drop {
                let (path, body) = &self.user_src_files[*i];
                if !path.starts_with(&folder) {
                    report.unplaced.push((path.clone(), body.clone()));
                }
            }
            if !drop.is_empty() {
                let gone = self.take_files(|i, _| drop.contains(&i), selected);
                self.bury(gone, report);
            }
            self.user_src_files.extend(graves);
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

/// What can tell one I2C device file from another: where it is, the number
/// and name in its file name, and the address and id in its generated block.
#[derive(Debug, Clone)]
struct DevSig {
    path: String,
    k: Option<usize>,
    /// Every name this file may carry. A file from before the folders is
    /// ambiguous: `i2c1_sensor_2.rs` is a device called "sensor 2" or the
    /// second "sensor".
    slugs: Vec<String>,
    address: Option<u8>,
    uid: Option<u32>,
    /// The flat name from before the folders: what a flat file IS called, and
    /// what a target WOULD have been called (`<bus>_<slug>`, `<bus>_device<k>`,
    /// `_2` on a repeat).
    legacy: Option<String>,
    /// Kept in the graveyard: written this session, so its id is the live
    /// model's - a device with ANOTHER id is another device.
    grave: bool,
}

impl DevSig {
    /// A device file in a bus folder, `device<k>[_<slug>].rs`.
    fn nested(path: String, k: usize, slug: &str, body: &str, bus: &str) -> Self {
        DevSig {
            path,
            k: Some(k),
            slugs: vec![slug.to_owned()],
            address: codegen::device_file_address(body),
            uid: codegen::device_file_id(body)
                .filter(|(b, _)| *b == bus)
                .map(|(_, u)| u),
            legacy: None,
            grave: false,
        }
    }

    /// A flat `<bus>_<rest>.rs` from before the folders.
    fn legacy(path: String, rest: &str, body: &str, bus: &str) -> Self {
        let mut slugs = vec![rest.to_owned()];
        if let Some((base, n)) = rest.rsplit_once('_')
            && !base.is_empty()
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            slugs.push(base.to_owned());
        }
        // An unnamed device was `<bus>_device<k>`: it is `device<k>.rs` now.
        let k = rest
            .strip_prefix("device")
            .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|d| d.parse().ok());
        if k.is_some() {
            slugs.push(String::new());
        }
        DevSig {
            path,
            k,
            slugs,
            address: codegen::device_file_address(body),
            uid: None,
            legacy: Some(format!("{bus}_{rest}")),
            grave: false,
        }
    }
}

fn same_address(o: &DevSig, t: &DevSig) -> bool {
    o.address.is_some() && o.address == t.address
}

fn same_name(o: &DevSig, t: &DevSig) -> bool {
    o.slugs.iter().any(|s| t.slugs.contains(s))
}

/// Pair each old device file (`olds`) with the device file it is now
/// (`targets`), returning `(old, target)` indices.
///
/// Tiers, strongest first; a pair is taken only when it is the ONLY one its
/// tier allows for BOTH files among those still unpaired - a guess is worse
/// than a fresh file, and a fresh file is what an unpaired target gets.
///
/// 1. The device's id - written only once it has one, and taken only when
///    the name or the address agrees too: an `mcu.config` restored on its own
///    can hand the same uids to other devices.
/// 2. Name and address: a device whose number moved.
/// 3. The flat name from before the folders: a project's first sync, where
///    two unnamed devices at 0x00 differ in nothing else.
/// 4. Number and a set address: a device renamed.
/// 5. A set address alone: renamed AND renumbered in one frame (the panel's
///    and the canvas's edits arrive together).
/// 6. A name alone: a device whose address changed.
/// 7. The same path: nothing about it changed that could tell.
///
/// In no tier does a file from the graveyard go to a device with another id:
/// it was written this session, so that is another device - a new one that
/// happens to take a removed one's number and name.
fn match_devices(olds: &[&DevSig], targets: &[DevSig]) -> Vec<(usize, usize)> {
    let tiers: [fn(&DevSig, &DevSig) -> bool; 7] = [
        |o, t| o.uid.is_some() && o.uid == t.uid && (same_name(o, t) || same_address(o, t)),
        |o, t| same_name(o, t) && same_address(o, t),
        |o, t| o.legacy.is_some() && o.legacy == t.legacy,
        |o, t| o.k.is_some() && o.k == t.k && same_address(o, t) && o.address != Some(0),
        |o, t| same_address(o, t) && o.address != Some(0),
        |o, t| o.slugs.iter().any(|s| !s.is_empty() && t.slugs.contains(s)),
        |o, t| o.path == t.path,
    ];
    let other_device =
        |o: &DevSig, t: &DevSig| o.grave && o.uid.is_some() && t.uid.is_some() && o.uid != t.uid;
    let mut old_done = vec![false; olds.len()];
    let mut target_done = vec![false; targets.len()];
    let mut pairs = Vec::new();
    for tier in tiers {
        loop {
            let mut found: Vec<(usize, usize)> = Vec::new();
            for (o, old) in olds.iter().enumerate() {
                if old_done[o] {
                    continue;
                }
                let mut hits = targets.iter().enumerate().filter(|(t, target)| {
                    !target_done[*t] && tier(old, target) && !other_device(old, target)
                });
                let (Some((t, target)), None) = (hits.next(), hits.next()) else {
                    continue;
                };
                let rivals = olds
                    .iter()
                    .enumerate()
                    .filter(|(o2, other)| {
                        !old_done[*o2] && tier(other, target) && !other_device(other, target)
                    })
                    .count();
                if rivals == 1 {
                    found.push((o, t));
                }
            }
            if found.is_empty() {
                break;
            }
            for (o, t) in found {
                if !old_done[o] && !target_done[t] {
                    old_done[o] = true;
                    target_done[t] = true;
                    pairs.push((o, t));
                }
            }
        }
    }
    pairs
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
            &mut None,
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
            &mut None,
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
                &mut None,
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
            &mut None,
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
        state.sync_config_files(
            &[("usart1.rs".to_string(), v1.to_string())],
            false,
            &[],
            &mut None,
        );
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
        state.sync_config_files(
            &[("usart1.rs".to_string(), v2.to_string())],
            false,
            &[],
            &mut None,
        );

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
            &mut None,
        );

        // Apply switches the runtime → a completely different (native) template.
        let native = "// <<< GENERATED>>>\nconst BAUDRATE: u32 = 115200;\n// <<< GENERATED END >>>\n\nuse native;\npub fn init() -> (Tx, Rx) { /* native */ }\n";
        state.sync_config_files(
            &[("usart1.rs".to_string(), native.to_string())],
            true,
            &[],
            &mut None,
        );

        let body = &state
            .user_src_files
            .iter()
            .find(|(p, _)| p == path)
            .unwrap()
            .1;
        assert!(body.contains("(Tx, Rx)"), "new template applied:\n{body}");
        assert!(!body.contains("SerialIo"), "old template gone:\n{body}");
    }

    /// A config module may be a FOLDER: `configs/mod.rs` declares it once, by
    /// its folder name, and never names a file inside it - `pub mod i2c1/mod;`
    /// is not Rust. The folder shows in the tree while it holds a file.
    #[test]
    fn a_config_folder_is_declared_once_by_its_name() {
        let mut state = ProjectTreeState::new();
        let body = "// <<< GENERATED>>>\n// <<< GENERATED END >>>\n";
        let files: Vec<(String, String)> = [
            "usart1.rs",
            "i2c1/mod.rs",
            "i2c1/device1.rs",
            "i2c1/device2_imu.rs",
        ]
        .iter()
        .map(|n| (n.to_string(), body.to_string()))
        .collect();
        state.sync_config_files(&files, false, &[], &mut None);
        let mod_rs = &state
            .user_src_files
            .iter()
            .find(|(p, _)| p == "src/pins/configs/mod.rs")
            .unwrap()
            .1;
        assert_eq!(mod_rs.matches("pub mod i2c1;").count(), 1, "{mod_rs}");
        assert!(mod_rs.contains("pub mod usart1;"), "{mod_rs}");
        assert!(!mod_rs.contains("device"), "{mod_rs}");
        assert!(
            !mod_rs
                .lines()
                .any(|l| l.starts_with("pub mod") && l.contains('/')),
            "{mod_rs}"
        );
        for f in ["i2c1/mod.rs", "i2c1/device1.rs", "i2c1/device2_imu.rs"] {
            assert_file_exists(&state, &format!("src/pins/configs/{f}"));
        }
        assert_folder_exists(&state, "src/pins/configs/i2c1");

        // The bus goes: its whole folder goes, and so does the folder entry.
        state.sync_config_files(&files[..1], false, &[], &mut None);
        assert_file_not_exists(&state, "src/pins/configs/i2c1/mod.rs");
        assert_file_not_exists(&state, "src/pins/configs/i2c1/device1.rs");
        assert!(
            !state
                .user_src_folders
                .iter()
                .any(|f| f == "src/pins/configs/i2c1"),
            "{:?}",
            state.user_src_folders
        );

        // Nothing configured at all: the subtree goes, subfolders included.
        state.sync_config_files(&files, false, &[], &mut None);
        state.sync_config_files(&[], false, &[], &mut None);
        assert!(
            !state
                .user_src_folders
                .iter()
                .any(|f| f.starts_with("src/pins/configs")),
            "{:?}",
            state.user_src_folders
        );
        assert!(
            !state
                .user_src_files
                .iter()
                .any(|(p, _)| p.starts_with("src/pins/configs/"))
        );
    }

    /// The editor names its file by index. A prune in front of it shifts it
    /// down with the list; a prune of the file itself leaves no selection.
    #[test]
    fn a_config_prune_carries_the_open_file() {
        let body = "// <<< GENERATED>>>\n// <<< GENERATED END >>>\n";
        let both = [
            ("spi1.rs".to_string(), body.to_string()),
            ("usart1.rs".to_string(), body.to_string()),
        ];
        let mut state = ProjectTreeState::new();
        state
            .user_src_files
            .push(("src/app/logic.rs".into(), "fn a() {}".into()));
        state.sync_config_files(&both, false, &[], &mut None);
        let at = |s: &ProjectTreeState, p: &str| s.user_src_files.iter().position(|(q, _)| q == p);
        let usart = at(&state, "src/pins/configs/usart1.rs").unwrap();
        let spi = at(&state, "src/pins/configs/spi1.rs").unwrap();
        assert!(spi < usart, "the fixture needs spi1 in front of usart1");

        let mut selected = Some(usart);
        state.sync_config_files(&both[1..], false, &[], &mut selected);
        assert_eq!(
            selected,
            at(&state, "src/pins/configs/usart1.rs"),
            "followed its file"
        );

        let mut selected = at(&state, "src/pins/configs/usart1.rs");
        state.sync_config_files(
            &[("spi1.rs".to_string(), body.to_string())],
            false,
            &[],
            &mut selected,
        );
        assert_eq!(selected, None, "its own file was pruned");

        let mut selected = at(&state, "src/app/logic.rs");
        state.sync_config_files(&[], false, &[], &mut selected);
        assert_eq!(
            selected,
            at(&state, "src/app/logic.rs"),
            "a file outside configs/"
        );
    }

    // ── I2C bus folders: device files move with their device ───────────────

    use crate::panels::mcu_module::modules::{I2cDevice, I2cModuleConfig};

    const BUS: &str = "src/pins/configs/i2c1";

    /// What codegen emits for I2C1 with these devices: `mod.rs` plus a file
    /// per device, through the real helpers. `(name, address, uid)`.
    fn bus_files(devs: &[(&str, u8, u32)]) -> Vec<(String, String)> {
        let mut c = I2cModuleConfig::new(1);
        c.devices = devs
            .iter()
            .map(|(n, a, u)| I2cDevice {
                name: (*n).into(),
                address: *a,
                uid: *u,
            })
            .collect();
        let body = format!(
            "// <<< GENERATED>>>\npub const CLOCK_KHZ: u32 = 100;\n{}// <<< GENERATED END >>>\n\npub fn init() {{}}\n",
            codegen::i2c_device_mods(Some(&c))
        );
        codegen::i2c_bus_files("i2c1", body, Some(&c))
    }

    fn sync(state: &mut ProjectTreeState, files: &[(String, String)]) -> ConfigSync {
        state.sync_config_files(files, false, &[], &mut None)
    }

    fn text<'a>(state: &'a ProjectTreeState, path: &str) -> Option<&'a str> {
        state
            .user_src_files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, c)| c.as_str())
    }

    /// The user writes below the markers of `path`.
    fn write_below(state: &mut ProjectTreeState, path: &str, code: &str) {
        let f = state
            .user_src_files
            .iter_mut()
            .find(|(p, _)| p == path)
            .unwrap_or_else(|| panic!("{path} is not in the tree"));
        f.1.push_str(code);
        f.1.push('\n');
    }

    fn device_files(state: &ProjectTreeState) -> Vec<&str> {
        let mut v: Vec<&str> = state
            .user_src_files
            .iter()
            .map(|(p, _)| p.as_str())
            .filter(|p| p.starts_with(BUS) && !p.ends_with("/mod.rs"))
            .collect();
        v.sort_unstable();
        v
    }

    fn dev(f: &str) -> String {
        format!("{BUS}/{f}.rs")
    }

    /// Rename, remove the first, undo: each device's code goes where the
    /// device goes, and the removed one's comes back with it.
    #[test]
    fn a_device_file_moves_with_its_device() {
        let mut st = ProjectTreeState::new();
        sync(&mut st, &bus_files(&[("oled", 0x3C, 1), ("imu", 0x68, 2)]));
        write_below(&mut st, &dev("device1_oled"), "// MINE-OLED");
        write_below(&mut st, &dev("device2_imu"), "// MINE-IMU");

        let r = sync(
            &mut st,
            &bus_files(&[("display", 0x3C, 1), ("imu", 0x68, 2)]),
        );
        assert!(
            text(&st, &dev("device1_display"))
                .unwrap()
                .contains("MINE-OLED")
        );
        assert!(text(&st, &dev("device1_oled")).is_none());
        assert_eq!(r.moved, vec![(dev("device1_oled"), dev("device1_display"))]);
        assert!(r.removed.is_empty(), "{:?}", r.removed);

        sync(&mut st, &bus_files(&[("imu", 0x68, 2)]));
        assert_eq!(device_files(&st), vec![dev("device1_imu")]);
        assert!(text(&st, &dev("device1_imu")).unwrap().contains("MINE-IMU"));

        // Ctrl+Z: both where they were, the removed one's code included.
        sync(
            &mut st,
            &bus_files(&[("display", 0x3C, 1), ("imu", 0x68, 2)]),
        );
        assert_eq!(
            device_files(&st),
            vec![dev("device1_display"), dev("device2_imu")]
        );
        assert!(
            text(&st, &dev("device1_display"))
                .unwrap()
                .contains("MINE-OLED")
        );
        assert!(text(&st, &dev("device2_imu")).unwrap().contains("MINE-IMU"));
        assert!(
            !text(&st, &dev("device2_imu"))
                .unwrap()
                .contains("MINE-OLED")
        );
    }

    /// Three unnamed devices at 0x00 - nothing tells them apart but their id.
    /// Remove the first: each survivor keeps its OWN code on its new number.
    #[test]
    fn unnamed_devices_at_zero_are_told_apart_by_their_id() {
        let mut st = ProjectTreeState::new();
        sync(&mut st, &bus_files(&[("", 0, 1), ("", 0, 2), ("", 0, 3)]));
        write_below(&mut st, &dev("device1"), "// CODE-A");
        write_below(&mut st, &dev("device2"), "// CODE-B");
        write_below(&mut st, &dev("device3"), "// CODE-C");
        let mut selected = st
            .user_src_files
            .iter()
            .position(|(p, _)| *p == dev("device3"));
        let r = st.sync_config_files(
            &bus_files(&[("", 0, 2), ("", 0, 3)]),
            false,
            &[],
            &mut selected,
        );
        assert_eq!(device_files(&st), vec![dev("device1"), dev("device2")]);
        let one = text(&st, &dev("device1")).unwrap();
        let two = text(&st, &dev("device2")).unwrap();
        assert!(one.contains("CODE-B") && !one.contains("CODE-A"), "{one}");
        assert!(two.contains("CODE-C") && !two.contains("CODE-B"), "{two}");
        assert_eq!(r.removed, vec![dev("device1")], "the first device's file");
        assert_eq!(
            selected.map(|i| st.user_src_files[i].0.as_str()),
            Some(dev("device2").as_str()),
            "the open file is followed, not its old path"
        );
    }

    /// A bus nobody edited since it was loaded has no ids in its files. Its
    /// first edit still sends each file to the right device: by name and
    /// address, by number and address, or by address alone - two edits in one
    /// frame included.
    #[test]
    fn without_ids_names_numbers_and_addresses_decide() {
        // Remove the middle one.
        let mut st = ProjectTreeState::new();
        sync(
            &mut st,
            &bus_files(&[("oled", 0x3C, 0), ("imu", 0x68, 0), ("baro", 0x76, 0)]),
        );
        write_below(&mut st, &dev("device1_oled"), "// MINE-OLED");
        write_below(&mut st, &dev("device3_baro"), "// MINE-BARO");
        sync(&mut st, &bus_files(&[("oled", 0x3C, 1), ("baro", 0x76, 3)]));
        assert_eq!(
            device_files(&st),
            vec![dev("device1_oled"), dev("device2_baro")]
        );
        assert!(
            text(&st, &dev("device2_baro"))
                .unwrap()
                .contains("MINE-BARO")
        );

        // Remove the first AND rename the second, in one frame.
        let mut st = ProjectTreeState::new();
        sync(&mut st, &bus_files(&[("a", 0x10, 0), ("b", 0x20, 0)]));
        write_below(&mut st, &dev("device2_b"), "// MINE-B");
        sync(&mut st, &bus_files(&[("y", 0x20, 2)]));
        assert_eq!(device_files(&st), vec![dev("device1_y")]);
        assert!(text(&st, &dev("device1_y")).unwrap().contains("MINE-B"));

        // Nothing tells two files apart: the path decides, as it always did,
        // and nothing is doubled or lost from the tree.
        let mut st = ProjectTreeState::new();
        sync(&mut st, &bus_files(&[("s", 0, 0), ("s", 0, 0)]));
        sync(&mut st, &bus_files(&[("s", 0, 2)]));
        assert_eq!(device_files(&st), vec![dev("device1_s")]);
    }

    /// A project from before buses were folders: its bus file becomes the
    /// folder's `mod.rs` - the same entry, the user's `init` kept, the address
    /// gone - and its device files move in, their code with them, losing the
    /// warning that a rename throws it away.
    #[test]
    fn a_project_from_before_the_folders_moves_in() {
        let legacy_device = |addr: u8, mine: &str| {
            format!(
                "// <<< GENERATED>>>\n// Device config (from the Virtual Module) — auto-updated; edit in the module.\npub const DEVICE_ADDRESS: u8 = 0x{addr:02X};\n// <<< GENERATED END >>>\n\n// Everything below is editable.\n{}{mine}\n",
                codegen::LEGACY_DEVICE_WARNING
            )
        };
        let seed = |st: &mut ProjectTreeState| {
            st.user_src_files.push((
                "src/pins/configs/mod.rs".into(),
                "// <<< GENERATED>>>\npub mod i2c1;\npub mod i2c1_oled;\npub mod i2c1_imu;\n// <<< GENERATED END >>>\n".into(),
            ));
            st.user_src_files.push((
                "src/pins/configs/i2c1.rs".into(),
                "// <<< GENERATED>>>\npub const CLOCK_KHZ: u32 = 100;\npub const DEVICE_ADDRESS: u8 = 0x3C;\n// <<< GENERATED END >>>\n\npub fn init() { /* EDITED */ }\n".into(),
            ));
            st.user_src_files.push((
                "src/pins/configs/i2c1_oled.rs".into(),
                legacy_device(0x3C, "// MINE-OLED"),
            ));
            st.user_src_files.push((
                "src/pins/configs/i2c1_imu.rs".into(),
                legacy_device(0x68, "// MINE-IMU"),
            ));
        };

        let mut st = ProjectTreeState::new();
        seed(&mut st);
        let at = st
            .user_src_files
            .iter()
            .position(|(p, _)| p == "src/pins/configs/i2c1.rs");
        let mut selected = at;
        let r = st.sync_config_files(
            &bus_files(&[("oled", 0x3C, 0), ("imu", 0x68, 0)]),
            false,
            &[],
            &mut selected,
        );
        let m = text(&st, &format!("{BUS}/mod.rs")).unwrap();
        assert!(m.contains("/* EDITED */"), "{m}");
        assert!(
            m.contains("pub mod device1_oled;") && m.contains("pub mod device2_imu;"),
            "{m}"
        );
        assert!(!m.contains("DEVICE_ADDRESS: u8"), "{m}");
        assert_eq!(
            selected.map(|i| st.user_src_files[i].0.clone()),
            Some(format!("{BUS}/mod.rs"))
        );
        let oled = text(&st, &dev("device1_oled")).unwrap();
        assert!(
            oled.contains("MINE-OLED") && oled.contains("DEVICE_ADDRESS: u8 = 0x3C;"),
            "{oled}"
        );
        assert!(!oled.contains(codegen::LEGACY_DEVICE_WARNING), "{oled}");
        assert!(oled.contains(codegen::DEVICE_MOVE_NOTE), "{oled}");
        assert!(text(&st, &dev("device2_imu")).unwrap().contains("MINE-IMU"));
        for flat in ["i2c1.rs", "i2c1_oled.rs", "i2c1_imu.rs"] {
            assert!(
                text(&st, &format!("src/pins/configs/{flat}")).is_none(),
                "{flat} is still there"
            );
        }
        assert_eq!(r.migrated.len(), 3, "{:?}", r.migrated);
        let cfg_mod = text(&st, "src/pins/configs/mod.rs").unwrap();
        assert_eq!(cfg_mod.matches("pub mod i2c1;").count(), 1, "{cfg_mod}");
        assert!(!cfg_mod.contains("i2c1_oled"), "{cfg_mod}");

        // Reopened, and the FIRST thing the user does is an edit: the old
        // files are read by what they hold, not rebuilt from the new list.
        let mut st = ProjectTreeState::new();
        seed(&mut st);
        sync(
            &mut st,
            &bus_files(&[("display", 0x3C, 1), ("imu", 0x68, 2)]),
        );
        assert!(
            text(&st, &dev("device1_display"))
                .unwrap()
                .contains("MINE-OLED")
        );
        assert!(text(&st, &dev("device2_imu")).unwrap().contains("MINE-IMU"));
        let mut st = ProjectTreeState::new();
        seed(&mut st);
        sync(&mut st, &bus_files(&[("imu", 0x68, 2)]));
        assert_eq!(device_files(&st), vec![dev("device1_imu")]);
        assert!(text(&st, &dev("device1_imu")).unwrap().contains("MINE-IMU"));

        // A second device of the same name was `<bus>_<name>_2`.
        let mut st = ProjectTreeState::new();
        st.user_src_files.push((
            "src/pins/configs/i2c1_sensor.rs".into(),
            legacy_device(0x40, "// FIRST"),
        ));
        st.user_src_files.push((
            "src/pins/configs/i2c1_sensor_2.rs".into(),
            legacy_device(0x41, "// SECOND"),
        ));
        sync(
            &mut st,
            &bus_files(&[("sensor", 0x40, 0), ("sensor", 0x41, 0)]),
        );
        assert!(text(&st, &dev("device1_sensor")).unwrap().contains("FIRST"));
        assert!(
            text(&st, &dev("device2_sensor"))
                .unwrap()
                .contains("SECOND")
        );
    }

    /// `i2c1.rs` beside an `i2c1/mod.rs` does not compile. The folder's wins,
    /// and the dropped one is reported - with its content - not lost quietly.
    #[test]
    fn an_old_bus_file_beside_its_folder_is_reported() {
        let mut st = ProjectTreeState::new();
        sync(&mut st, &bus_files(&[("oled", 0x3C, 1)]));
        write_below(&mut st, &format!("{BUS}/mod.rs"), "// NEW-INIT");
        st.user_src_files.push((
            "src/pins/configs/i2c1.rs".into(),
            "pub fn init() { /* OLD */ }\n".into(),
        ));
        let r = sync(&mut st, &bus_files(&[("oled", 0x3C, 1)]));
        assert!(text(&st, "src/pins/configs/i2c1.rs").is_none());
        assert!(
            text(&st, &format!("{BUS}/mod.rs"))
                .unwrap()
                .contains("NEW-INIT")
        );
        assert_eq!(r.conflicts.len(), 1);
        assert!(r.conflicts[0].1.contains("/* OLD */"));
    }

    /// A Runtime Apply rewrites a bus's `mod.rs` whole - its `init` is what
    /// changed - but a device file only gets its generated block: nothing in
    /// it depends on the Runtime, and the rest is the user's.
    #[test]
    fn force_leaves_the_device_files_alone() {
        let mut st = ProjectTreeState::new();
        let files = bus_files(&[("oled", 0x3C, 1)]);
        sync(&mut st, &files);
        write_below(&mut st, &dev("device1_oled"), "// MINE");
        write_below(&mut st, &format!("{BUS}/mod.rs"), "// OLD-INIT");
        st.sync_config_files(&files, true, &[], &mut None);
        assert!(text(&st, &dev("device1_oled")).unwrap().contains("// MINE"));
        assert!(
            !text(&st, &format!("{BUS}/mod.rs"))
                .unwrap()
                .contains("OLD-INIT")
        );
    }

    /// A bus that leaves the generated files - a pad cleared, a Runtime with
    /// no config files - and comes back brings the user's code back with it.
    #[test]
    fn a_bus_that_comes_back_brings_its_code_back() {
        let usart = (
            "usart1.rs".to_string(),
            "// <<< GENERATED>>>\n// <<< GENERATED END >>>\n".to_string(),
        );
        let mut with_bus = bus_files(&[("oled", 0x3C, 1)]);
        with_bus.push(usart.clone());
        let mut st = ProjectTreeState::new();
        sync(&mut st, &with_bus);
        write_below(&mut st, &dev("device1_oled"), "// MINE-DEVICE");
        write_below(&mut st, &format!("{BUS}/mod.rs"), "// MINE-INIT");

        for away in [vec![usart.clone()], Vec::new()] {
            let r = sync(&mut st, &away);
            assert!(text(&st, &dev("device1_oled")).is_none());
            assert!(r.removed.contains(&dev("device1_oled")), "{:?}", r.removed);
            sync(&mut st, &with_bus);
            assert!(
                text(&st, &dev("device1_oled"))
                    .unwrap()
                    .contains("MINE-DEVICE")
            );
            assert!(
                text(&st, &format!("{BUS}/mod.rs"))
                    .unwrap()
                    .contains("MINE-INIT")
            );
        }
        assert_eq!(
            st.user_src_files
                .iter()
                .filter(|(p, _)| p.starts_with(BUS))
                .count(),
            2,
            "nothing doubled"
        );
    }

    /// A device file exactly as the build before the folders wrote it - its
    /// warning, its example - with the user's own `init_display` below the
    /// markers. Its code moves in; nothing in what the user wrote decides
    /// whether it is a device file.
    #[test]
    fn an_old_device_file_moves_in_whatever_the_user_wrote() {
        let head = "// <<< GENERATED>>>\n// Device config (from the Virtual Module) — auto-updated; edit in the module.\n// 7-bit address of the device on this bus — for YOUR code, not for `init`:\n// an I2C master takes the address per transaction.\npub const DEVICE_ADDRESS: u8 = 0x3C;\n// <<< GENERATED END >>>\n\n// Everything below is editable — your changes are preserved on regeneration.\n//\n// `oled` is one of the devices sharing the i2c1 bus. The bus driver is\n// built ONCE — see `i2c1` — and `main.rs` owns the handle; this file only says\n// which address on it is yours. Write the device's own routines here and\n// take the bus as an argument:\n//\n//     pub fn read_id<I: embedded_hal::i2c::I2c>(bus: &mut I) -> Option<u8> {\n//         let mut rx = [0u8; 1];\n//         bus.write_read(DEVICE_ADDRESS, &[0x00], &mut rx).ok()?;\n//         Some(rx[0])\n//     }\n//\n// Renaming this device in the panel renames this file, and the old one is\n// removed with whatever was below its markers. Move anything you want to\n// keep before you rename.\npub fn init_display<I>(bus: &mut I) { /* MINE */ }\n";
        let mut st = ProjectTreeState::new();
        st.user_src_files
            .push(("src/pins/configs/i2c1_oled.rs".into(), head.into()));
        st.user_src_files.push((
            "src/pins/configs/i2c1_imu.rs".into(),
            head.replace("0x3C", "0x68")
                .replace("/* MINE */", "/* IMU */"),
        ));
        let r = sync(&mut st, &bus_files(&[("oled", 0x3C, 0), ("imu", 0x68, 0)]));
        let oled = text(&st, &dev("device1_oled")).unwrap();
        assert!(oled.contains("/* MINE */"), "{oled}");
        assert!(
            !oled.contains("Move anything you want to"),
            "the old warning stayed:\n{oled}"
        );
        assert!(
            text(&st, &dev("device2_imu"))
                .unwrap()
                .contains("/* IMU */")
        );
        assert!(r.unplaced.is_empty(), "{:?}", r.unplaced);
    }

    /// Two unnamed devices still at 0x00 differ in nothing but their old flat
    /// names, and that is enough. When a first edit removes one, the file
    /// left over is not lost quietly: it is reported, with its text.
    #[test]
    fn old_unnamed_devices_at_zero_move_in_by_their_old_names() {
        let old = |mine: &str| {
            format!(
                "// <<< GENERATED>>>\npub const DEVICE_ADDRESS: u8 = 0x00;\n// <<< GENERATED END >>>\n{mine}\n"
            )
        };
        let seed = |st: &mut ProjectTreeState| {
            st.user_src_files
                .push(("src/pins/configs/i2c1_device1.rs".into(), old("// CODE-A")));
            st.user_src_files
                .push(("src/pins/configs/i2c1_device2.rs".into(), old("// CODE-B")));
        };
        let mut st = ProjectTreeState::new();
        seed(&mut st);
        let r = sync(&mut st, &bus_files(&[("", 0, 0), ("", 0, 0)]));
        assert!(text(&st, &dev("device1")).unwrap().contains("CODE-A"));
        assert!(text(&st, &dev("device2")).unwrap().contains("CODE-B"));
        assert!(r.unplaced.is_empty(), "{:?}", r.unplaced);

        let mut st = ProjectTreeState::new();
        seed(&mut st);
        let r = sync(&mut st, &bus_files(&[("", 0, 2)]));
        assert_eq!(device_files(&st), vec![dev("device1")]);
        assert_eq!(r.unplaced.len(), 1, "{:?}", r.unplaced);
        let (left, body) = &r.unplaced[0];
        assert!(left.starts_with("src/pins/configs/i2c1_device"), "{left}");
        assert!(body.contains("CODE-"), "{body}");
    }

    /// Three old devices called the same, at three addresses, and the first
    /// edit removes the first: each survivor finds its code by name AND
    /// address - `i2c1_sensor_2.rs` is a "sensor" too - before the old names
    /// (which now belong to the wrong ones) get a say. The removed one's file
    /// is reported.
    #[test]
    fn old_devices_of_one_name_move_in_by_name_and_address() {
        let old = |addr: u8, mine: &str| {
            format!(
                "// <<< GENERATED>>>\npub const DEVICE_ADDRESS: u8 = 0x{addr:02X};\n// <<< GENERATED END >>>\n{mine}\n"
            )
        };
        let mut st = ProjectTreeState::new();
        st.user_src_files.push((
            "src/pins/configs/i2c1_sensor.rs".into(),
            old(0x40, "// FIRST"),
        ));
        st.user_src_files.push((
            "src/pins/configs/i2c1_sensor_2.rs".into(),
            old(0x41, "// SECOND"),
        ));
        st.user_src_files.push((
            "src/pins/configs/i2c1_sensor_3.rs".into(),
            old(0x42, "// THIRD"),
        ));
        let r = sync(
            &mut st,
            &bus_files(&[("sensor", 0x41, 2), ("sensor", 0x42, 3)]),
        );
        assert!(
            text(&st, &dev("device1_sensor"))
                .unwrap()
                .contains("SECOND")
        );
        assert!(text(&st, &dev("device2_sensor")).unwrap().contains("THIRD"));
        assert_eq!(r.unplaced.len(), 1, "{:?}", r.unplaced);
        assert!(r.unplaced[0].1.contains("FIRST"));
    }

    /// Remove the first device twice, undo twice. The second removal buries
    /// a file on the SAME path as the first one's grave, and must not erase
    /// it: both undos bring each device's own code back.
    #[test]
    fn two_graves_on_one_path_are_two_devices() {
        let mut st = ProjectTreeState::new();
        let three = bus_files(&[("", 0x10, 1), ("", 0x20, 2), ("", 0x30, 3)]);
        sync(&mut st, &three);
        write_below(&mut st, &dev("device1"), "// CODE-A");
        write_below(&mut st, &dev("device2"), "// CODE-B");
        write_below(&mut st, &dev("device3"), "// CODE-C");
        sync(&mut st, &bus_files(&[("", 0x20, 2), ("", 0x30, 3)]));
        sync(&mut st, &bus_files(&[("", 0x30, 3)]));
        assert!(text(&st, &dev("device1")).unwrap().contains("CODE-C"));
        sync(&mut st, &bus_files(&[("", 0x20, 2), ("", 0x30, 3)]));
        sync(&mut st, &three);
        for (f, code) in [
            ("device1", "CODE-A"),
            ("device2", "CODE-B"),
            ("device3", "CODE-C"),
        ] {
            let t = text(&st, &dev(f)).unwrap();
            assert!(t.contains(code), "{f}:\n{t}");
            assert_eq!(t.matches("// CODE-").count(), 1, "{f}:\n{t}");
        }
    }

    /// A device removed and a NEW one added in its place - same number, no
    /// name, 0x00 - is another device: it starts from the template, and the
    /// removed one's code stays for its own undo.
    #[test]
    fn a_new_device_does_not_get_a_removed_ones_code() {
        let mut st = ProjectTreeState::new();
        sync(&mut st, &bus_files(&[("oled", 0x3C, 1), ("", 0, 2)]));
        write_below(&mut st, &dev("device2"), "// REMOVED-ONE");
        sync(&mut st, &bus_files(&[("oled", 0x3C, 1)]));
        sync(&mut st, &bus_files(&[("oled", 0x3C, 1), ("", 0, 3)]));
        assert!(!text(&st, &dev("device2")).unwrap().contains("REMOVED-ONE"));
        // Undo both: the removed one is back with its code.
        sync(&mut st, &bus_files(&[("oled", 0x3C, 1)]));
        sync(&mut st, &bus_files(&[("oled", 0x3C, 1), ("", 0, 2)]));
        assert!(text(&st, &dev("device2")).unwrap().contains("REMOVED-ONE"));
    }

    /// A moment with no config files at all prunes `configs/mod.rs` too; the
    /// code the user wrote around its markers comes back with the files.
    #[test]
    fn configs_mod_comes_back_with_its_code() {
        let usart = vec![(
            "usart1.rs".to_string(),
            "// <<< GENERATED>>>\n// <<< GENERATED END >>>\n".to_string(),
        )];
        let mut st = ProjectTreeState::new();
        sync(&mut st, &usart);
        write_below(
            &mut st,
            "src/pins/configs/mod.rs",
            "pub mod helpers_of_mine;",
        );
        sync(&mut st, &[]);
        sync(&mut st, &usart);
        let m = text(&st, "src/pins/configs/mod.rs").unwrap();
        assert!(
            m.contains("pub mod helpers_of_mine;") && m.contains("pub mod usart1;"),
            "{m}"
        );
    }

    /// Two devices leave and come back - several graves revived at once, each
    /// to its own file.
    #[test]
    fn two_devices_come_back_each_with_its_own_code() {
        let usart = (
            "usart1.rs".to_string(),
            "// <<< GENERATED>>>\n// <<< GENERATED END >>>\n".to_string(),
        );
        let mut with_bus = bus_files(&[("oled", 0x3C, 1), ("imu", 0x68, 2)]);
        with_bus.push(usart.clone());
        for away in [vec![usart.clone()], Vec::new()] {
            let mut st = ProjectTreeState::new();
            sync(&mut st, &with_bus);
            write_below(&mut st, &dev("device1_oled"), "// MINE-OLED");
            write_below(&mut st, &dev("device2_imu"), "// MINE-IMU");
            sync(&mut st, &away);
            sync(&mut st, &with_bus);
            let oled = text(&st, &dev("device1_oled")).unwrap();
            let imu = text(&st, &dev("device2_imu")).unwrap();
            assert!(
                oled.contains("MINE-OLED") && !oled.contains("MINE-IMU"),
                "{oled}"
            );
            assert!(
                imu.contains("MINE-IMU") && !imu.contains("MINE-OLED"),
                "{imu}"
            );
        }
    }

    fn sig(path: &str, k: usize, slug: &str, address: u8, uid: Option<u32>) -> DevSig {
        DevSig {
            path: path.into(),
            k: Some(k),
            slugs: vec![slug.into()],
            address: Some(address),
            uid,
            legacy: None,
            grave: false,
        }
    }

    /// The pairing's own rules, one by one.
    #[test]
    fn match_devices_takes_only_what_is_certain() {
        // A uid held by a device with another name AND address is not proof:
        // name and address win over it.
        let old = sig("o1", 1, "oled", 0x3C, Some(5));
        let targets = [
            sig("t0", 1, "imu", 0x68, Some(5)),
            sig("t1", 2, "oled", 0x3C, Some(9)),
        ];
        assert_eq!(match_devices(&[&old], &targets), vec![(0, 1)]);

        // Two hits in one tier: that tier passes, a later one decides.
        let old = sig("o1", 1, "s", 0x10, None);
        let targets = [sig("t0", 2, "s", 0x10, None), sig("t1", 1, "s", 0x10, None)];
        assert_eq!(match_devices(&[&old], &targets), vec![(0, 1)]);

        // Two old files that each hit only this target: neither takes it.
        let (a, b) = (sig("o1", 1, "a", 0x10, None), sig("o2", 2, "a", 0x10, None));
        assert!(match_devices(&[&a, &b], &[sig("t0", 3, "a", 0x10, None)]).is_empty());

        // 0x00 is every new device's address, not an identity - not with two
        // targets, not with one, not with the same number.
        let old = sig("o1", 1, "x", 0, None);
        let targets = [sig("t0", 1, "y", 0, None), sig("t1", 2, "z", 0, None)];
        assert!(match_devices(&[&old], &targets).is_empty());
        assert!(match_devices(&[&old], &[sig("t0", 2, "y", 0, None)]).is_empty());
        assert!(match_devices(&[&old], &[sig("t0", 1, "y", 0, None)]).is_empty());

        // A grave beside a live file on the same path: the grave is another
        // device's, so the live file takes the target - the grave neither
        // takes it nor counts as its rival.
        let mut grave = sig("same", 2, "", 0, Some(2));
        grave.grave = true;
        let live = sig("same", 2, "", 0, None);
        assert_eq!(
            match_devices(&[&grave, &live], &[sig("same", 2, "", 0, Some(3))]),
            vec![(1, 0)]
        );

        // A grave never goes to a device with another id - not even on the
        // same path.
        let mut grave = sig("same", 2, "", 0, Some(2));
        grave.grave = true;
        assert!(match_devices(&[&grave], &[sig("same", 2, "", 0, Some(3))]).is_empty());
        grave.uid = Some(3);
        assert_eq!(
            match_devices(&[&grave], &[sig("same", 2, "", 0, Some(3))]),
            vec![(0, 0)]
        );
    }
}
