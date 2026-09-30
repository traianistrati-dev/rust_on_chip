//! Applying the project tree's copy / paste signals.
//!
//! The tree raises intent (`CopyRequest` / `PasteRequest`); everything that
//! actually reads or mutates `user_src_files` happens here, in the one place
//! that owns the whole file list. See [`crate::project_tree::clipboard`] for
//! why the payload lives in a shared staging directory rather than on the
//! system clipboard.

use super::AppIde;
use crate::project_tree::clipboard::{self, ClipKind, CopyRequest, PasteRequest};
use crate::project_tree::gui::set_tree_notice;
use eframe::egui;

impl AppIde {
    /// Stage a tree item for pasting — here or in another IDE window.
    ///
    /// Reads the IN-MEMORY buffers, not the disk: unsaved edits are part of
    /// what you see in the tree, so they have to be part of what you copy.
    pub(super) fn apply_clip_copy(&mut self, ctx: &egui::Context, req: CopyRequest) {
        // Every payload path is rooted at the item's own base name, so a paste
        // can rename the item as one unit by swapping that first segment.
        let base = req.path.rsplit('/').next().unwrap_or(&req.path).to_owned();
        let prefix = format!("{}/", req.path);

        let mut files = Vec::new();
        let mut skipped_binary = 0usize;
        for (path, content) in &self.project_tree.user_src_files {
            let rest = if req.kind == ClipKind::File {
                if path != &req.path {
                    continue;
                }
                String::new()
            } else if let Some(r) = path.strip_prefix(&prefix) {
                r.to_owned()
            } else {
                continue;
            };
            // A file the tree model could not read as text is an empty string
            // here (see `logic::scan_src_dir`). Copying it would silently
            // produce a 0-byte file at the far end, so it is counted out loud
            // instead. A genuinely empty source file is indistinguishable and
            // is treated the same way only when it has a non-text extension.
            if content.is_empty() && !is_texty(path) {
                skipped_binary += 1;
                continue;
            }
            let rel = if rest.is_empty() {
                base.clone()
            } else {
                format!("{base}/{rest}")
            };
            files.push((rel, content.clone()));
        }

        if files.is_empty() {
            set_tree_notice(ctx, format!("Nothing to copy in `{}`.", req.path));
            return;
        }

        let source = self
            .project_dir
            .as_ref()
            .and_then(|d| d.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unsaved project".to_owned());

        let count = files.len();
        match clipboard::stage(req.kind, &base, &source, &files, skipped_binary) {
            Ok(id) => {
                // The token is what makes Ctrl+V work, and what orders two
                // windows that copied at nearly the same moment. It is short
                // on purpose: if it lands in a text editor it is a harmless
                // one-line marker, not a wall of source.
                ctx.copy_text(clipboard::token_for(&id));
                let skipped = if skipped_binary > 0 {
                    format!(", {skipped_binary} non-text skipped")
                } else {
                    String::new()
                };
                set_tree_notice(
                    ctx,
                    format!(
                        "Copied `{base}` ({count} file(s){skipped}) — paste it here or in another IDE window."
                    ),
                );
            }
            Err(e) => set_tree_notice(ctx, format!("Copy failed: {e}")),
        }
    }

    /// Paste a staged payload into `req.target_dir`.
    pub(super) fn apply_clip_paste(
        &mut self,
        ctx: &egui::Context,
        req: PasteRequest,
        save_needed: &mut bool,
    ) {
        let payload = match req.id.as_deref() {
            Some(id) => clipboard::load(id),
            None => clipboard::latest(),
        };
        let Some(payload) = payload else {
            set_tree_notice(
                ctx,
                "Nothing to paste — the copied item is no longer available.".into(),
            );
            return;
        };

        // A crate directory belongs at the project root; anything else belongs
        // inside the tree. The menu already refuses the wrong pairing, but
        // Ctrl+V can reach here with any selection, so the rule is enforced
        // where it is decided rather than only where it is displayed.
        let is_lib = payload.manifest.kind == ClipKind::Library;
        if is_lib != req.target_dir.is_empty() {
            let msg = if is_lib {
                format!(
                    "`{}` is a library crate — paste it on the LIBRARIES header.",
                    payload.manifest.name
                )
            } else {
                format!(
                    "`{}` is a {} — only a library crate can sit at the project root.",
                    payload.manifest.name,
                    payload.manifest.kind.noun()
                )
            };
            set_tree_notice(ctx, msg);
            return;
        }

        // Ctrl+V reaches here with any selection, a generated file's too.
        if crate::project_tree::gui::in_generated_configs(&req.target_dir) {
            set_tree_notice(
                ctx,
                "Can't paste into `pins/configs/` - it's auto-managed by the MCU Configurator."
                    .into(),
            );
            return;
        }

        let taken: std::collections::HashSet<&str> = self
            .project_tree
            .user_src_files
            .iter()
            .map(|(p, _)| p.as_str())
            .chain(
                self.project_tree
                    .user_src_folders
                    .iter()
                    .map(|f| f.as_str()),
            )
            .collect();
        let new_files = clipboard::paste_paths(&payload, &req.target_dir, |p| taken.contains(p));
        if new_files.is_empty() {
            set_tree_notice(ctx, "The copied item has no files left to paste.".into());
            return;
        }

        // Folders first: a file whose parent folder is not tracked renders as a
        // phantom top-level entry until the project is reopened.
        for (path, _) in &new_files {
            let mut parts: Vec<&str> = path.split('/').collect();
            parts.pop(); // the file itself
            for i in 1..=parts.len() {
                let folder = parts[..i].join("/");
                if !folder.is_empty() && !self.project_tree.user_src_folders.contains(&folder) {
                    self.project_tree.user_src_folders.push(folder);
                }
            }
        }

        let count = new_files.len();
        // Mirror into the scratch workspace the same way New File / Rename do,
        // so rust-analyzer sees the files before the next Save.
        let workspace_dir = self
            .project_dir
            .clone()
            .unwrap_or_else(crate::workspace::dir);
        for (path, content) in new_files {
            let full = workspace_dir.join(&path);
            if let Some(parent) = full.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&full, &content);
            self.project_tree.user_src_files.push((path, content));
        }

        // A pasted crate arrives DETACHED, exactly like a `git clone` does: it
        // becomes a `[workspace] member` only through "Add to workspace", whose
        // cargo-metadata pre-check is what keeps a bad crate from taking
        // rust-analyzer down with it.
        let hint = if is_lib {
            " — it arrived DETACHED; use \"Add to workspace\" to build it"
        } else {
            ""
        };
        set_tree_notice(
            ctx,
            format!(
                "Pasted `{}` — {count} file(s){hint}.",
                payload.manifest.name
            ),
        );
        *save_needed = true;
    }
}

/// Extensions the tree is expected to hold as text. Used only to tell a
/// genuinely empty source file from one the model failed to read — an empty
/// `.rs` is worth copying, an empty `.png` never is.
fn is_texty(path: &str) -> bool {
    const TEXT_EXT: &[&str] = &[
        "rs",
        "toml",
        "md",
        "txt",
        "x",
        "json",
        "ron",
        "cfg",
        "yml",
        "yaml",
        "lock",
        "gitignore",
        "c",
        "h",
        "cpp",
        "hpp",
        "ld",
        "s",
        "asm",
        "sh",
        "py",
    ];
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, ext)) => TEXT_EXT.contains(&ext.to_ascii_lowercase().as_str()),
        // No extension at all (LICENSE, Makefile, …) — assume text.
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_extensions_are_the_ones_that_get_skipped() {
        assert!(is_texty("src/main.rs"));
        assert!(is_texty("Cargo.toml"));
        assert!(is_texty("memory.x"));
        assert!(is_texty("LICENSE"), "no extension reads as text");
        assert!(!is_texty("assets/logo.png"));
        assert!(!is_texty("blob.bin"));
        // Case is not a signal.
        assert!(!is_texty("A.PNG"));
        assert!(is_texty("A.RS"));
    }
}
