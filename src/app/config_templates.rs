//! What follows a `pins/configs/` sync that kept an EDITED file on its old
//! template - see `project_tree::templates`: the new template is saved beside
//! the file, the user hears about it, and the tree's Reset / Keep act on it.

use super::AppIde;
use crate::project_tree::gui::TemplateAction;
use crate::project_tree::logic::ConfigSync;
use crate::project_tree::templates;
use eframe::egui;
use std::collections::BTreeMap;

/// Carried across frames.
#[derive(Default)]
pub(super) struct ConfigTemplates {
    /// `path -> template` of every file with an offer pending, as the last
    /// sync had it: the copies kept beside them.
    offers: BTreeMap<String, String>,
    /// The notice after an Apply that kept edited files, until it is closed.
    notice: Option<Vec<String>>,
}

impl ConfigTemplates {
    /// The offers in hand.
    pub(super) fn offers(&self) -> Vec<(String, String)> {
        self.offers
            .iter()
            .map(|(p, b)| (p.clone(), b.clone()))
            .collect()
    }
}

/// `.rustonchip/templates/<path>.txt`, as the notice spells it.
fn sidecar_name(path: &str) -> String {
    format!("{}/{path}.txt", templates::SIDECARS)
}

impl AppIde {
    /// Right after `sync_config_files`, with what it reported. `after_apply`:
    /// the sync follows a Runtime / Init-API Apply - the moment the user
    /// expects every file to change, and so the one that gets a window.
    pub(super) fn follow_template_sync(&mut self, report: &ConfigSync, after_apply: bool) {
        self.config_templates.offers = report.offered.iter().cloned().collect();
        // On disk now when there is a folder, so the notice can point at it.
        let saved = self.reconcile_config_templates();
        if !report.kept.is_empty() {
            self.announce_kept(report, saved, after_apply);
        }
        // A kept file whose own code frames the line otherwise than its module
        // says: in the notice after an Apply, else on the status line - after
        // the kept files' line, which it outranks.
        if let Some(first) = report.warnings.first() {
            if after_apply {
                self.config_templates
                    .notice
                    .get_or_insert_with(Vec::new)
                    .extend(report.warnings.iter().cloned());
            } else {
                self.set_status_msg(match report.warnings.len() {
                    1 => first.clone(),
                    n => format!("{first} (and {} more config files)", n - 1),
                });
            }
        }
    }

    /// The kept files of `report`: a notice after an Apply, a status line
    /// otherwise. `saved`: the copies beside them are on disk.
    fn announce_kept(&mut self, report: &ConfigSync, saved: bool, after_apply: bool) {
        let where_ = if saved {
            "beside it"
        } else {
            "beside it at the next Save"
        };
        if after_apply {
            let mut lines = vec![format!(
                "You had edited these config files, so the new template did NOT replace them. Their GENERATED block is up to date; the rest is as you left it, and main.rs may now call an `init` it does not have. The new template is saved {where_}:"
            )];
            for (path, _) in &report.kept {
                lines.push(format!("    {path}  ->  {}", sidecar_name(path)));
            }
            lines.push(
                "Right-click a file in the tree: Reset to template takes the new one (your version is copied first), Keep my version turns it down for good."
                    .to_owned(),
            );
            self.config_templates
                .notice
                .get_or_insert_with(Vec::new)
                .extend(lines);
        } else {
            let what = match report.kept.as_slice() {
                [(path, _)] => {
                    format!("{}: you edited it", path.rsplit('/').next().unwrap_or(path))
                }
                many => format!("{} config files you edited", many.len()),
            };
            self.set_status_msg(format!(
                "{what} - its new template is saved {where_} (right-click in the tree: Reset to template)"
            ));
        }
    }

    /// Make the copies beside the config files say what the record says now:
    /// every pending offer written, every ended one - followed, kept, pruned,
    /// moved - gone. After each sync, Keep and Save, on the UI thread: the
    /// Save worker runs on a snapshot that may be behind what the user did
    /// meanwhile. Only the copies - the record goes with the Save, which
    /// writes the files it describes. False without a folder, or when a
    /// write failed (the next sync or Save tries again).
    pub(super) fn reconcile_config_templates(&self) -> bool {
        let Some(root) = self.project_dir.as_ref() else {
            return false;
        };
        let files = &self.project_tree.user_src_files;
        let record = self
            .project_tree
            .templates
            .kept(|p| files.iter().any(|(f, _)| f == p));
        match templates::reconcile_sidecars(root, &record, &self.config_templates.offers()) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("[!] config template copies not written: {e}");
                false
            }
        }
    }

    /// A Reset or Keep picked in the tree.
    pub(super) fn apply_template_action(&mut self, action: TemplateAction) {
        match action {
            TemplateAction::Reset(path) => self.reset_config_template(&path),
            TemplateAction::Keep(path) => self.keep_config_version(&path),
        }
    }

    /// Write the template over `path` at the next sync - which this asks for -
    /// after copying the file as it is to `<path>.mine.txt` beside it. Refused
    /// where no copy can be kept: a project with no folder yet.
    pub(super) fn reset_config_template(&mut self, path: &str) {
        let Some(root) = self.project_dir.clone() else {
            self.set_status_msg(
                "Reset to template needs the project saved first, so your version can be kept beside it"
                    .to_owned(),
            );
            return;
        };
        let content = self
            .project_tree
            .user_src_files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, c)| c.clone())
            .unwrap_or_default();
        // `None`: the same text is copied there already.
        let copy = templates::backup(&root, path, &content);
        if let Some(file) = &copy
            && let Err(e) = templates::write_sidecar(&root, file, &content)
        {
            self.set_status_msg(format!(
                "Reset to template refused: could not keep a copy of {path} at {} ({e})",
                file.display()
            ));
            return;
        }
        self.project_tree.reset_requested.insert(path.to_owned());
        // The regeneration that writes it: the same re-arm an Open does.
        self.mcu_state_hash = 0;
        let name = path.rsplit('/').next().unwrap_or(path);
        self.set_status_msg(match copy {
            Some(file) => format!(
                "{name} is its template again - your version: {}",
                file.display()
            ),
            None => format!("{name} is its template again"),
        });
    }

    /// Keep the user's `path` as it is and turn the offered template down: it
    /// is not offered again, and the file does not follow it on its own.
    pub(super) fn keep_config_version(&mut self, path: &str) {
        if let Some(stamp) = self.project_tree.templates.files.get_mut(path)
            && let Some(offered) = stamp.offered.take()
        {
            stamp.declined.insert(offered);
        }
        if let Some(mark) = self.project_tree.template_marks.get_mut(path) {
            mark.offered = false;
        }
        self.config_templates.offers.remove(path);
        self.reconcile_config_templates();
        let name = path.rsplit('/').next().unwrap_or(path);
        self.set_status_msg(format!("Kept your version of {name}"));
    }

    pub(super) fn show_config_templates_notice(&mut self, ui: &egui::Ui) {
        let Some(lines) = self.config_templates.notice.as_ref() else {
            return;
        };
        let mut close = false;
        egui::Window::new("Edited config files kept")
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for line in lines {
                            if line.starts_with("    ") {
                                ui.label(
                                    egui::RichText::new(line.trim_start())
                                        .monospace()
                                        .size(11.0),
                                );
                            } else {
                                ui.add_space(4.0);
                                ui.label(line);
                            }
                        }
                    });
                ui.add_space(6.0);
                if ui.button("OK").clicked() {
                    close = true;
                }
            });
        if close {
            self.config_templates.notice = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_tree::templates::{RECORD, Stamp, TemplateRecord};

    const PATH: &str = "src/pins/configs/usart1.rs";
    const TEXT: &str =
        "// <<< GENERATED>>>\n// <<< GENERATED END >>>\n\npub fn init() { mine(); }\n";

    fn app(dir: Option<&std::path::Path>) -> AppIde {
        let ctx = egui::Context::default();
        let mut app = AppIde::new(&eframe::CreationContext::_new_kittest(ctx), None, None);
        app.project_dir = dir.map(|d| d.to_path_buf());
        app.project_tree.user_src_files = vec![(PATH.to_owned(), TEXT.to_owned())];
        app
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("eide_cfg_tmpl_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// No folder, no copy - so no Reset: it would throw the edits away.
    #[test]
    fn a_reset_without_a_folder_is_refused() {
        let mut app = app(None);
        app.mcu_state_hash = 7;
        app.reset_config_template(PATH);
        assert!(app.project_tree.reset_requested.is_empty());
        assert_eq!(app.mcu_state_hash, 7, "no regeneration asked for");
        assert!(app.export_msg.contains("saved first"), "{}", app.export_msg);
    }

    /// A Reset copies the file beside it - out of git - and asks for the
    /// regeneration that writes the template.
    #[test]
    fn a_reset_keeps_a_copy_and_asks_for_the_regeneration() {
        let dir = scratch("reset");
        let mut app = app(Some(&dir));
        app.mcu_state_hash = 7;
        app.reset_config_template(PATH);
        let copy = dir
            .join(templates::SIDECARS)
            .join(format!("{PATH}.mine.txt"));
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), TEXT);
        assert_eq!(
            std::fs::read_to_string(dir.join(templates::SIDECARS).join(".gitignore")).unwrap(),
            "*\n"
        );
        assert!(app.project_tree.reset_requested.contains(PATH));
        assert_eq!(app.mcu_state_hash, 0, "the regeneration is re-armed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Keep my version turns the template down, drops its copy, and leaves
    /// the project unsaved until the record says so too - for a Save, not
    /// for the gates that care about the source on disk.
    #[test]
    fn keep_turns_the_offer_down_and_is_a_change_to_save() {
        let dir = scratch("keep");
        let mut record = TemplateRecord::default();
        record.files.insert(
            PATH.to_owned(),
            Stamp {
                offered: Some(5),
                seen: Some(5),
                ..Stamp::default()
            },
        );
        templates::save(&dir, &record).unwrap();
        templates::write_sidecar(&dir, &templates::sidecar(&dir, PATH), "the template").unwrap();
        let mut app = app(Some(&dir));
        app.project_tree.templates = TemplateRecord::load(&dir);
        assert!(!app.unsaved_files().iter().any(|p| p == RECORD));
        app.keep_config_version(PATH);
        let stamp = &app.project_tree.templates.files[PATH];
        assert_eq!(stamp.offered, None);
        assert_eq!(stamp.declined, [5].into());
        assert!(!templates::sidecar(&dir, PATH).exists());
        assert!(app.unsaved_files().iter().any(|p| p == RECORD));
        // ...but not one cargo or rust-analyzer would read stale: a rename
        // still gets its reference edits, a publish still runs.
        assert!(!app.unsaved_source_files().iter().any(|p| p == RECORD));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After a sync the copies say what the record says: a pending offer is
    /// written - over a stale one - and an ended one goes.
    #[test]
    fn a_sync_leaves_the_copies_as_the_record_says() {
        let dir = scratch("reconcile");
        let mut app = app(Some(&dir));
        let file = templates::sidecar(&dir, PATH);
        templates::write_sidecar(&dir, &file, "an older offer").unwrap();
        app.project_tree.templates.files.insert(
            PATH.to_owned(),
            Stamp {
                offered: Some(1),
                ..Stamp::default()
            },
        );
        let report = ConfigSync {
            offered: vec![(PATH.to_owned(), "the offer now".to_owned())],
            ..ConfigSync::default()
        };
        app.follow_template_sync(&report, false);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "the offer now");
        // The template followed: no offer, no copy.
        app.project_tree
            .templates
            .files
            .get_mut(PATH)
            .unwrap()
            .offered = None;
        app.follow_template_sync(&ConfigSync::default(), false);
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
