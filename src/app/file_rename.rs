//! Renaming a project-tree file, with its module references.
//!
//! Renaming `mylib/src/radar.rs` to `radar_io.rs` must also rewrite
//! `mod radar;`, `use radar::*;` and every `radar::` path — including from
//! other crates in the workspace. Doing that by text search is not safe (a
//! dependency crate named `radar` silently shadows the module rather than
//! erroring, and `radar` occurs inside strings, comments and longer
//! identifiers), so the rewrite is asked of rust-analyzer.
//!
//! # Why `workspace/willRenameFiles`
//!
//! rust-analyzer offers two routes and they cannot be combined:
//!
//! * `textDocument/rename` on the `radar` token of `mod radar;` returns the
//!   text edits AND file-move operations, but only if the client advertises
//!   `workspaceEdit.resourceOperations` — and the IDE would then have to
//!   perform the moves itself from an LSP payload.
//! * `workspace/willRenameFiles` returns TEXT EDITS ONLY: rust-analyzer
//!   deliberately drops the file-system half, because a client that asks this
//!   question is already doing the move.
//!
//! The second is this IDE's shape exactly, so that is what is used. The cost of
//! the choice is recorded in [`crate::lsp::LspState::request_will_rename`]:
//! advertising the capability makes the first route return no text edits, which
//! is why `resourceOperations` is deliberately not advertised.
//!
//! # Ordering
//!
//! The question must be asked BEFORE the file moves — rust-analyzer resolves
//! the old path against its VFS and calls `is_dir()` on it. So a rename is a
//! two-step, asynchronous flow: ask, then (on the reply, or on a timeout) apply
//! the edits and move the file. [`PendingRename`] is that intermediate state.

use super::AppIde;
use crate::project_tree::gui::{RenameRequest, set_tree_notice};
use eframe::egui;

/// How long to wait for rust-analyzer's answer before renaming anyway.
///
/// A rename that hangs because the analyzer died is worse than a rename that
/// loses its reference update: the user asked for the file to be renamed, and
/// the existing symbol rename already has a known "in flight forever" failure
/// mode that this deliberately does not copy.
const WILL_RENAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// A rename waiting on rust-analyzer's `willRenameFiles` reply.
pub(super) struct PendingRename {
    pub req: RenameRequest,
    pub started: std::time::Instant,
    /// The project this rename belongs to. `load_project_from_dir` already
    /// clears the whole slot, but the reply is addressed by project-root-
    /// RELATIVE path, so a stale one would rewrite whatever project is loaded
    /// now at the old project's coordinates. Cheap second lock on that door.
    pub project: Option<std::path::PathBuf>,
}

/// Why a file's module references cannot be rewritten. `None` = go ahead.
///
/// Every one of these makes rust-analyzer answer with an empty edit list or
/// worse — silently produce a broken rewrite — so they are checked here rather
/// than discovered afterwards.
fn reference_update_blocker(app: &AppIde, req: &RenameRequest) -> Option<String> {
    // Only Rust files have module references. A `.toml` / `.md` / `.x` rename
    // is a plain move and is not a defect.
    if !req.old_path.ends_with(".rs") {
        return Some(String::new());
    }
    let new_stem = stem(&req.new_path);
    let old_stem = stem(&req.old_path);

    // A crate root has no `mod` declaration naming it; rust-analyzer returns an
    // empty change for one, and renaming it silently breaks the crate.
    if is_crate_root(&req.old_path) {
        return Some(format!(
            "`{old_stem}.rs` is a crate root - it has no `mod` declaration to rename."
        ));
    }
    // The DESTINATION matters just as much: renaming a module to `main.rs`
    // lands on the generated crate root, which is not in `user_src_files` and
    // so is invisible to the collision check.
    if is_crate_root(&req.new_path) {
        return Some(format!(
            "`{new_stem}.rs` is a crate root - pick another name."
        ));
    }
    // `mod.rs` names its DIRECTORY, not itself. rust-analyzer refuses it in
    // both directions (`("mod", _) => None`), so say so rather than let the
    // request come back empty.
    if old_stem == "mod" || new_stem == "mod" {
        return Some(
            "`mod.rs` takes its name from its folder — rename the folder instead.".to_owned(),
        );
    }
    // The new name becomes a module identifier. `radar-io` or `2radar` are
    // valid file names and invalid modules; rust-analyzer swallows the failure
    // and would leave the file renamed with no declaration updated.
    if !is_ident(&new_stem) {
        return Some(format!(
            "`{new_stem}` can't be a module name — use letters, digits and `_`, not starting with a digit."
        ));
    }
    // `#[path = "..."]` decides the file a module loads from, and rust-analyzer
    // has no awareness of it at all: it would rewrite the declaration while the
    // attribute still points at the old file.
    if app.path_attr_targets(&req.old_path) {
        return Some(format!(
            "`{old_stem}.rs` is loaded through a `#[path = \"...\"]` attribute - update that by hand."
        ));
    }
    // A `radar.rs` + `radar/` pair: the directory holds the submodules and
    // would have to move too. `willRenameFiles` drops exactly that operation.
    if app.owns_module_dir(&req.old_path) {
        return Some(format!(
            "`{old_stem}` owns a `{old_stem}/` folder of submodules - rename both by hand for now."
        ));
    }
    // And the mirror case: landing next to an existing `radar_io/mod.rs` is
    // E0761, "file for module found at both paths".
    if app.owns_module_dir(&req.new_path) {
        return Some(format!(
            "`{new_stem}/` already exists as a module folder - `{new_stem}.rs` next to it is ambiguous."
        ));
    }
    // rust-analyzer positions its edits against the text it last saw, and it
    // only ever sees a Project Save. Applying them to buffers that have moved
    // on would splice at stale offsets, so the rename degrades to a plain move
    // instead of corrupting files.
    if !app.unsaved_source_files().is_empty() {
        return Some("save the project first so rust-analyzer sees the current text".to_owned());
    }
    None
}

/// `lib.rs` / `main.rs` at the root of any crate in the project.
fn is_crate_root(path: &str) -> bool {
    matches!(
        path.rsplit('/').next().unwrap_or(path),
        "lib.rs" | "main.rs"
    ) && path.rsplit('/').nth(1).is_none_or(|d| d == "src")
}

/// The file name without its directory or `.rs`.
fn stem(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".rs")
        .to_owned()
}

/// Every `#[path = "..."]` string literal in `src`.
///
/// A deliberately small scanner rather than a parse: the only use is to REFUSE
/// a rename, so over-matching costs a manual rename while under-matching costs
/// broken code.
fn path_attr_values(src: &str) -> impl Iterator<Item = String> + '_ {
    src.match_indices("#[path").filter_map(|(i, _)| {
        let rest = &src[i..];
        let open = rest.find('"')?;
        // Bail out if the attribute never closes on this line - that is a
        // malformed attribute, not a path we should reason about.
        let line_end = rest.find('\n').unwrap_or(rest.len());
        if open > line_end {
            return None;
        }
        let after = &rest[open + 1..];
        let close = after.find('"')?;
        Some(after[..close].to_owned())
    })
}

/// Does a `#[path]` value name `target` (a bare file name)? Compared on the
/// last segment so `"sub/radar.rs"` and `"radar.rs"` both count, and
/// case-insensitively because Windows resolves them the same way.
fn attr_points_at(value: &str, target: &str) -> bool {
    value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .eq_ignore_ascii_case(target)
}

/// Is this a plain Rust identifier? Raw identifiers and non-ASCII are excluded
/// on purpose: rustc refuses to auto-discover a module file whose name is not
/// ASCII (E0754), so allowing one here would produce a file that cannot be
/// declared.
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl AppIde {
    /// Start a tree rename. Asks rust-analyzer what has to change first when
    /// that is possible; otherwise renames straight away.
    pub(super) fn begin_file_rename(&mut self, ctx: &egui::Context, req: RenameRequest) {
        // One at a time: a second rename while one is in flight would apply its
        // edits against indices the first is about to shift.
        if self.pending_rename.is_some() {
            set_tree_notice(
                ctx,
                "A rename is already in progress — try again.".to_owned(),
            );
            return;
        }
        if let Some(reason) = reference_update_blocker(self, &req) {
            // An empty reason means "not a Rust file" - a plain move, nothing
            // to explain, so it reports as a clean rename.
            let outcome = if reason.is_empty() {
                Ok(0)
            } else {
                Err(reason)
            };
            self.finish_file_rename(ctx, req, outcome);
            return;
        }
        // rust-analyzer must be up and past its first index, and must already
        // know this file — its view is only refreshed on Project Save, so a file
        // created since then is not in its VFS and the answer would be empty.
        let ready = {
            let lsp = self.lsp_state.lock().unwrap();
            matches!(lsp.status, crate::lsp::LspStatus::Ready) && lsp.indexed
        };
        if !ready {
            self.finish_file_rename(ctx, req, Err("rust-analyzer isn't ready".to_owned()));
            return;
        }
        self.lsp_state
            .lock()
            .unwrap()
            .request_will_rename(&req.old_path, &req.new_path);
        self.pending_rename = Some(PendingRename {
            req,
            started: std::time::Instant::now(),
            project: self.project_dir.clone(),
        });
    }

    /// Consume the `willRenameFiles` reply (or give up on it) and complete the
    /// rename. Called every frame from `init_frame`.
    pub(super) fn poll_file_rename(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.pending_rename else {
            return;
        };
        let timed_out = pending.started.elapsed() > WILL_RENAME_TIMEOUT;
        let edits = self.lsp_state.lock().unwrap().take_will_rename_result();

        let (edits, note) = match (edits, timed_out) {
            (Some(e), _) => (e, None),
            (None, true) => (
                Vec::new(),
                Some("rust-analyzer did not answer in time".to_owned()),
            ),
            // Still waiting — keep the frames coming so the timeout can fire
            // even if nothing else asks for a repaint.
            (None, false) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
                return;
            }
        };

        let pending = self.pending_rename.take().expect("checked above");
        // The answer is addressed by project-root-RELATIVE path. If the project
        // changed while it was in flight, those paths now mean different files:
        // drop the whole thing rather than rewrite a stranger.
        if pending.project != self.project_dir {
            return;
        }
        // An empty answer is not an error to report as one: a module nothing
        // references has nothing to rewrite, which is the common case for a
        // freshly created file.
        let outcome = match (&note, edits.len()) {
            (Some(why), _) => Err(why.clone()),
            (None, 0) => Err("nothing referenced it".to_owned()),
            (None, n) => Ok(n),
        };
        // The edits are applied INSIDE `finish_file_rename`, after the move
        // succeeds - a failed move used to leave every reference rewritten to
        // point at a file that never moved.
        self.finish_file_rename_with(ctx, pending.req, outcome, edits);
    }

    /// Move the file, with no reference edits (the blocked / non-Rust paths).
    fn finish_file_rename(
        &mut self,
        ctx: &egui::Context,
        req: RenameRequest,
        outcome: Result<usize, String>,
    ) {
        self.finish_file_rename_with(ctx, req, outcome, Vec::new());
    }

    /// Move the file on disk and in the tree, then apply `edits`.
    ///
    /// Order matters: the edits rewrite OTHER files to name the new module, so
    /// applying them before a move that then fails would leave the project
    /// pointing at a file that does not exist. Nothing is written until the
    /// move has actually happened.
    fn finish_file_rename_with(
        &mut self,
        ctx: &egui::Context,
        req: RenameRequest,
        outcome: Result<usize, String>,
        edits: Vec<crate::lsp::RenameEdit>,
    ) {
        // Re-found by PATH, not by the index captured when the rename was
        // confirmed: the list can shift while the reply is in flight, and an
        // index would then rename a different file (or be out of range).
        let Some(idx) = self
            .project_tree
            .user_src_files
            .iter()
            .position(|(p, _)| *p == req.old_path)
        else {
            set_tree_notice(
                ctx,
                format!("`{}` is no longer in the project.", req.old_path),
            );
            return;
        };

        let root = self
            .project_dir
            .clone()
            .unwrap_or_else(crate::workspace::dir);
        if let Err(e) = std::fs::rename(root.join(&req.old_path), root.join(&req.new_path)) {
            // Reported, not swallowed: neither the in-memory path nor any
            // reference is touched, so memory and disk stay in agreement.
            set_tree_notice(ctx, format!("Could not rename on disk - {e}"));
            return;
        }
        self.project_tree.user_src_files[idx].0 = req.new_path.clone();

        // An edit addressed to the file that just moved has to follow it, or
        // `apply_rename_edits` would look up a path that no longer exists and
        // silently drop it (a `use crate::radar::X` inside radar.rs itself).
        let edits = edits
            .into_iter()
            .map(|mut e| {
                if e.rel_path == req.old_path {
                    e.rel_path = req.new_path.clone();
                }
                e
            })
            .collect();
        self.apply_rename_edits(edits);

        // `save_project_needed` is a per-frame local and this can complete from
        // `init_frame`, before that local exists - this is the flag for exactly
        // that case.
        self.workspace_write_requested = true;

        // The real file name, not `stem + ".rs"`: a `notes.md` rename would
        // otherwise be reported as `notes2.rs`.
        let name = req.new_path.rsplit('/').next().unwrap_or(&req.new_path);
        set_tree_notice(
            ctx,
            match outcome {
                Ok(n) => format!("Renamed to `{name}` and updated {n} reference(s)."),
                Err(why) => {
                    format!("Renamed to `{name}`, but references were not updated - {why}.")
                }
            },
        );
    }

    /// Does any `#[path = "..."]` attribute in the project point AT `path`?
    ///
    /// Keyed on the attribute's own string, not on the file stem. The first
    /// version compared the stem against `mod <stem>;`, which only ever matched
    /// when the attribute was redundant - it missed the whole reason it exists,
    /// `#[path = "impl_a.rs"] mod imp;`, where the module is `imp` and the file
    /// is `impl_a.rs`.
    pub(super) fn path_attr_targets(&self, path: &str) -> bool {
        let target = path.rsplit('/').next().unwrap_or(path);
        self.project_tree
            .user_src_files
            .iter()
            .map(|(_, c)| c.as_str())
            .chain(std::iter::once(self.generated_code.as_str()))
            .any(|c| path_attr_values(c).any(|v| attr_points_at(&v, target)))
    }

    /// Is there a `<stem>/` directory carrying this module's submodules?
    ///
    /// `starts_with` on the bare prefix would also match a SIBLING whose name
    /// merely begins the same way - renaming `radar.rs` must not be blocked by
    /// an unrelated `radar_io/`. The trailing slash is what makes it a folder
    /// test rather than a prefix test.
    pub(super) fn owns_module_dir(&self, path: &str) -> bool {
        let dir = format!("{}/", path.trim_end_matches(".rs"));
        self.project_tree
            .user_src_files
            .iter()
            .any(|(p, _)| p.starts_with(&dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_must_be_plain_ascii_identifiers() {
        assert!(is_ident("radar_io"));
        assert!(is_ident("_radar"));
        assert!(is_ident("r2d2"));
        assert!(!is_ident("radar-io"), "a hyphen is not an identifier");
        assert!(!is_ident("2radar"), "cannot start with a digit");
        assert!(!is_ident(""), "empty is not a name");
        // rustc refuses to auto-discover a module file with a non-ASCII name
        // (E0754), so it must not pass as a module identifier either.
        assert!(!is_ident("café"));
    }

    /// The guard exists for `#[path = "impl_a.rs"] mod imp;` - where the module
    /// name and the file stem DIFFER. Keying on the stem missed exactly that.
    #[test]
    fn path_attributes_are_matched_by_their_own_string() {
        let src = "#[path = \"impl_a.rs\"]
mod imp;
";
        let vals: Vec<String> = path_attr_values(src).collect();
        assert_eq!(vals, vec!["impl_a.rs".to_owned()]);
        assert!(attr_points_at("impl_a.rs", "impl_a.rs"));
        // A directory-qualified value still names the same file.
        assert!(attr_points_at("sub/impl_a.rs", "impl_a.rs"));
        assert!(attr_points_at("sub\\\\impl_a.rs", "impl_a.rs"));
        // Windows resolves these the same way, so the guard must too.
        assert!(attr_points_at("Impl_A.rs", "impl_a.rs"));
        assert!(!attr_points_at("other.rs", "impl_a.rs"));
    }

    #[test]
    fn a_malformed_path_attribute_is_ignored() {
        assert_eq!(
            path_attr_values(
                "#[path]
mod x;"
            )
            .count(),
            0
        );
        // Unterminated on its line - not a path we should reason about.
        assert_eq!(
            path_attr_values(
                "#[path = 
mod x;"
            )
            .count(),
            0
        );
        assert_eq!(path_attr_values("// no attributes here").count(), 0);
    }

    /// Both directions matter: renaming a crate root breaks the crate, and
    /// renaming something ONTO one lands on the generated main.rs.
    #[test]
    fn crate_roots_are_recognised_in_every_crate() {
        assert!(is_crate_root("src/main.rs"));
        assert!(is_crate_root("src/lib.rs"));
        assert!(is_crate_root("mylib/src/lib.rs"));
        assert!(!is_crate_root("src/radar.rs"));
        // A `main.rs` that is NOT a crate root - it sits in a submodule folder.
        assert!(!is_crate_root("src/bin/main.rs"));
        assert!(!is_crate_root("src/sublib.rs"));
    }

    #[test]
    fn stem_drops_the_folder_and_the_extension() {
        assert_eq!(stem("mylib/src/radar.rs"), "radar");
        assert_eq!(stem("radar.rs"), "radar");
        // No extension to drop.
        assert_eq!(stem("mylib/src/radar"), "radar");
    }
}
