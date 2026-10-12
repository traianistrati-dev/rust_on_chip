//! "Clone project" — duplicate the current on-disk project (all files + its
//! workspace / detached libraries) into a new folder, skipping `target/` and
//! `.git/`. The IDE stays on the current project; only the copy is written.
//!
//! Motivation: changing the Runtime (Blocking ⇄ Native ⇄ Async) regenerates
//! different code, so a snapshot lets you keep the previous variant.

use crate::app::AppIde;
use eframe::egui;
use egui_phosphor::regular as ph;
use std::path::Path;

/// The Clone-project modal's state.
pub(crate) struct CloneProjectDialog {
    /// Absolute destination directory (default: a sibling `<name>-copy`).
    pub dest: String,
    /// Switch the IDE to the clone once it's written (default: stay on the
    /// current project).
    pub open_after: bool,
    pub error: Option<String>,
    /// `Some((file_count, dest))` after a successful clone → the modal shows a
    /// done screen instead of the form.
    pub done: Option<(usize, String)>,
}

impl CloneProjectDialog {
    pub(crate) fn new(project_dir: &Path) -> Self {
        Self {
            dest: default_clone_dest(project_dir),
            open_after: false,
            error: None,
            done: None,
        }
    }
}

/// A non-existing sibling dir `<name>-copy`, `<name>-copy-2`, … next to the
/// project, so the default never clobbers an existing folder.
fn default_clone_dest(project_dir: &Path) -> String {
    let parent = project_dir.parent().unwrap_or(project_dir);
    let name = project_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_owned());
    let mut cand = parent.join(format!("{name}-copy"));
    let mut n = 2;
    while cand.exists() {
        cand = parent.join(format!("{name}-copy-{n}"));
        n += 1;
    }
    cand.to_string_lossy().into_owned()
}

/// A copy is another board: the client certificate and its key stay with
/// the original (two boards with one identity throw each other off the
/// broker). The other secrets are copied.
pub(super) fn forget_identity(dest: &Path) {
    use crate::panels::mcu_module::codegen::iot_gen;
    let path = dest.join(iot_gen::SECRETS_PATH);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let mut out = text.clone();
    for name in iot_gen::CLIENT_SECRETS {
        let prefix = format!("pub const {name}: &str = ");
        let present = out.lines().any(|l| l.trim_start().starts_with(&prefix));
        if present && iot_gen::read_secret(&out, name).as_deref() != Some("") {
            out = iot_gen::write_secret(&out, name, "");
        }
    }
    if out != text {
        let _ = std::fs::write(&path, out);
    }
}

/// Recursively copy `src` into `dst`, skipping any `target/` or `.git/` dir
/// (build artifacts + version history — the latter per the feature's scope).
/// Creates `dst` + parents. Returns the number of files copied.
pub(super) fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dst)?;
    let mut count = 0;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        // Skip build output + git metadata by NAME, whatever the type — `.git`
        // is a DIRECTORY in a normal repo but a FILE in a submodule / worktree.
        let n = name.to_string_lossy();
        if n == "target" || n == ".git" {
            continue;
        }
        let ty = entry.file_type()?;
        if ty.is_dir() {
            count += copy_tree(&entry.path(), &dst.join(&name))?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), dst.join(&name))?;
            count += 1;
        }
        // Symlinks (rare in these projects) are ignored.
    }
    Ok(count)
}

impl AppIde {
    /// Duplicate the current on-disk project into `dest`. The IDE stays on the
    /// current project. Returns the file count on success.
    pub(super) fn clone_project_to(&self, dest: &Path) -> Result<usize, String> {
        let Some(src) = self.project_dir.as_ref() else {
            return Err("Save the project first — there's nothing on disk to clone yet.".into());
        };
        if dest.as_os_str().is_empty() {
            return Err("Enter a destination folder.".into());
        }
        if dest.exists() {
            return Err(format!(
                "{} already exists — pick another name.",
                dest.display()
            ));
        }
        // Copying a folder into itself (or a subfolder) would recurse forever.
        if dest.starts_with(src) {
            return Err("The destination is inside the project — pick a folder outside it.".into());
        }
        let copied = copy_tree(src, dest).map_err(|e| {
            // Best-effort: leave a half-copy rather than deleting — the user picked
            // the path, and deleting on error is riskier than an orphan folder.
            format!("Copy failed: {e}")
        })?;
        forget_identity(dest);
        Ok(copied)
    }

    /// Render the Clone-project modal (no-op when closed).
    pub(super) fn show_clone_project_dialog(&mut self, ui: &egui::Ui) {
        if self.clone_project_dialog.is_none() {
            return;
        }
        let src_label = self
            .project_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let mut close = false;
        // `(dest, open_after)` when the user presses Clone.
        let mut do_clone: Option<(String, bool)> = None;

        let dlg = self.clone_project_dialog.as_mut().unwrap();
        egui::Window::new(format!("{}  Clone project", ph::COPY))
            .id(egui::Id::new("clone_project_dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.set_width(560.0);
                if let Some((count, path)) = dlg.done.clone() {
                    ui.label(
                        egui::RichText::new(format!("{}  Cloned {count} files to:", ph::CHECK))
                            .color(egui::Color32::from_rgb(120, 200, 120)),
                    );
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(path).monospace().size(11.0));
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    return;
                }

                ui.label(
                    egui::RichText::new(
                        "Copies the current project — every file plus its workspace / detached \
                         libraries — into a new folder. Skips target/ and .git. The current \
                         project stays open.",
                    )
                    .color(egui::Color32::GRAY),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{} Copies the SAVED project on disk — Save first if you have unsaved edits.",
                        ph::WARNING,
                    ))
                    .size(10.5)
                    .color(egui::Color32::from_rgb(220, 180, 90)),
                );
                ui.add_space(8.0);
                if !src_label.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("From").size(11.0));
                        ui.label(egui::RichText::new(&src_label).monospace().size(10.5).color(
                            egui::Color32::from_gray(150),
                        ));
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("To").size(11.0));
                    ui.add(
                        egui::TextEdit::singleline(&mut dlg.dest)
                            .desired_width(440.0)
                            .font(egui::TextStyle::Monospace),
                    );
                });
                ui.add_space(6.0);
                ui.checkbox(&mut dlg.open_after, "Open cloned project after clone")
                    .on_hover_text(
                        "On: switch the IDE to the copy once it's written (unsaved edits in \
                         the current project are left as they are). Off: stay here.",
                    );
                if let Some(e) = &dlg.error {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(e)
                            .size(11.0)
                            .color(egui::Color32::from_rgb(230, 120, 110)),
                    );
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(egui::RichText::new(format!("{}  Clone", ph::COPY)).strong())
                        .clicked()
                    {
                        do_clone = Some((dlg.dest.clone(), dlg.open_after));
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

        if let Some((dest, open_after)) = do_clone {
            match self.clone_project_to(Path::new(&dest)) {
                Ok(count) => {
                    if open_after {
                        // Switch the IDE to the fresh copy (same path as File>Open)
                        // and close the modal — we've navigated away from the done
                        // screen.
                        self.load_project_from_dir(Path::new(&dest));
                        self.clone_project_dialog = None;
                    } else if let Some(d) = &mut self.clone_project_dialog {
                        d.done = Some((count, dest));
                        d.error = None;
                    }
                }
                Err(e) => {
                    if let Some(d) = &mut self.clone_project_dialog {
                        d.error = Some(e);
                    }
                }
            }
        }
        if close {
            self.clone_project_dialog = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_tree_skips_target_and_git() {
        let base = std::env::temp_dir().join(format!("eid_clone_test_{}", std::process::id()));
        let src = base.join("src_proj");
        let dst = base.join("dst_proj");
        let _ = std::fs::remove_dir_all(&base);
        // src/main.rs + a lib + target/ + .git/ that must NOT be copied.
        std::fs::create_dir_all(src.join("src")).unwrap();
        std::fs::write(src.join("Cargo.toml"), "x").unwrap();
        std::fs::write(src.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::create_dir_all(src.join("mylib/src")).unwrap();
        std::fs::write(src.join("mylib/src/lib.rs"), "pub fn f() {}").unwrap();
        std::fs::create_dir_all(src.join("target/debug")).unwrap();
        std::fs::write(src.join("target/debug/artifact"), "junk").unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join(".git/HEAD"), "ref").unwrap();

        let n = copy_tree(&src, &dst).unwrap();
        assert_eq!(n, 3, "Cargo.toml + main.rs + lib.rs");
        assert!(dst.join("src/main.rs").exists());
        assert!(dst.join("mylib/src/lib.rs").exists(), "library copied");
        assert!(!dst.join("target").exists(), "target skipped");
        assert!(!dst.join(".git").exists(), ".git skipped");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn default_dest_is_a_copy_sibling() {
        let d = default_clone_dest(Path::new("/home/u/blink"));
        assert!(d.ends_with("blink-copy"), "{d}");
    }
}
