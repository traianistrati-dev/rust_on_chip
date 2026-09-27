//! "Publish…" on a library crate: check, rehearse, then upload.
//!
//! Four things in one window, because they are one question:
//!
//! * what the registry would refuse, checked HERE rather than delegated to
//!   `cargo publish --dry-run`, which only warns about the two fields that
//!   actually get an upload rejected (see [`crate::publish`]);
//! * the missing `[package]` fields, editable in place — the extract-crate
//!   dialog is create-only, so until now the only way to fill in `repository`
//!   or a blank `description` was to hand-edit the manifest in the tree. They
//!   reach `Cargo.toml` through **Preview**, which writes them all at once and
//!   opens the manifest in the editor: what the user then reads is exactly
//!   what cargo will package, which no per-field "Write" button could promise;
//! * where it goes and with what credentials (see
//!   [`crate::publish_target`] for why a typed index URL travels in the
//!   environment and never on the command line);
//! * `cargo publish`, streamed live — as a rehearsal first, and then for real.
//!
//! The upload is the one irreversible thing this IDE does: a published version
//! can never be overwritten and its code cannot be deleted, only yanked. So it
//! is gated on the crate's own name being typed, on every blocker being clear,
//! on no edited field still being unwritten, and on the project being saved —
//! cargo packages what is on disk, and publishing a stale file cannot be taken
//! back. The last click is on a modal naming the crate, the version and the
//! registry, not on the button the mouse was already resting over.

use super::{AppIde, ProjectFileId};
use crate::publish::{self, Severity};
use crate::publish_target::{self, Target};
use crate::terminal::{LineKind, TerminalState};
use eframe::egui;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// The open dialog. `None` on `AppIde` means closed.
pub(super) struct PublishDialog {
    /// Project-root-relative directory of the library crate.
    pub dir: String,
    /// Draft values for the `[package]` fields, keyed by the same names as
    /// [`publish::EDITABLE_FIELDS`]. Seeded from the manifest on open.
    pub fields: Vec<(&'static str, String)>,
    /// Which boxes the user has typed in, parallel to `fields`.
    ///
    /// An untouched box FOLLOWS the manifest instead of holding what it was
    /// seeded with. Preview sends the user into `Cargo.toml` in the editor, so
    /// a box frozen at open time would offer to write a stale value back over
    /// whatever they then typed there - and the "press Preview first" gate
    /// would insist on it.
    pub touched: Vec<bool>,
    /// Live output of the cargo run in flight - rehearsal or upload.
    pub log: Arc<Mutex<TerminalState>>,
    pub stop: Arc<AtomicBool>,
    /// Set while a cargo run is in flight, so the buttons disable themselves.
    pub running: Arc<AtomicBool>,
    /// Whether the run in flight is the rehearsal. Meaningless unless `running`
    /// is set. Read to tell a harmless `--dry-run` apart from an upload that
    /// must not have the project folder rewritten under it.
    pub running_dry: bool,
    /// The cargo child, so closing the window can kill it. Without this the
    /// process outlived the dialog: `stop` only makes the readers drop their
    /// pipes, and an upload kept going with nowhere to report.
    pub child: Arc<Mutex<Option<std::process::Child>>>,
    pub error: Option<String>,

    // ── Phase B: where, with what credentials, and are you sure ─────────────
    /// Registries offered in the picker: crates.io, whatever the user's own
    /// cargo config already defines, and a custom index.
    pub targets: Vec<Target>,
    pub target: usize,
    /// The custom index URL, kept even while another target is selected so
    /// flipping back does not lose it.
    pub custom_index: String,
    /// Paste a token, or leave `use_saved_token` on and pass none at all.
    ///
    /// Held ONLY here, for the life of this window. Never written to disk: the
    /// IDE's existing secret store (the AI key) is plain text in the config
    /// folder, which is not a bar a publishing credential should clear.
    pub token: String,
    pub show_token: bool,
    /// Let cargo use the credentials it already has (`cargo login`).
    pub use_saved_token: bool,
    /// Typed confirmation. A publish cannot be undone, so the crate's own name
    /// has to be typed - a checkbox is too easy to click past.
    pub confirm: String,
    /// Preview asked for a project save that has not been taken yet.
    ///
    /// [`AppIde::request_save`] is a ONE-SHOT flag, consumed early in the frame
    /// and thrown away when a save is already running. So a Preview pressed
    /// while the header still says "Saving" wrote the buffer, cleared the
    /// "press Preview first" gate, and left the disk stale with nothing on
    /// screen saying so. This re-asks until the request can be honoured.
    pub save_pending: bool,
    /// The "are you sure" modal is up. Set by the Publish button, which no
    /// longer starts anything itself: the last click before an irreversible
    /// upload should be on a sentence naming what is about to happen, not on a
    /// button the mouse was already resting over.
    pub confirm_open: bool,
}

impl PublishDialog {
    pub fn new(dir: String, manifest: &str, targets: Vec<Target>) -> Self {
        // An inherited field has no literal value to edit here - its text
        // lives in `[workspace.package]`. Seeding the box with the sentinel
        // let the user type into it and write that sentence into the manifest.
        let fields: Vec<(&'static str, String)> = publish::EDITABLE_FIELDS
            .iter()
            .map(|(key, _)| {
                let v = publish::package_field(manifest, key)
                    .filter(|_| !publish::is_workspace_inherited(manifest, key))
                    .unwrap_or_default();
                (*key, v)
            })
            .collect();
        Self {
            dir,
            touched: vec![false; fields.len()],
            fields,
            log: Arc::new(Mutex::new(TerminalState::default())),
            stop: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
            running_dry: true,
            child: Arc::new(Mutex::new(None)),
            targets,
            target: 0,
            custom_index: String::new(),
            token: String::new(),
            show_token: false,
            use_saved_token: true,
            confirm: String::new(),
            save_pending: false,
            confirm_open: false,
            error: None,
        }
    }

    /// The registry to publish to and the token to pass, exactly as the widgets
    /// have them at this instant.
    ///
    /// Read three times a frame: by the confirmation modal, to name the
    /// destination the user is about to accept; inside the window, to explain a
    /// disabled button; and outside it, to launch cargo. The launch is the one
    /// that matters. Recomputed each time rather than captured, so a token or a
    /// registry changed in the same frame as the click is the one that travels
    /// - and so the modal cannot name one index while cargo is handed another.
    fn chosen(&self) -> (Target, Option<String>) {
        let target = match &self.targets[self.target] {
            Target::Custom { .. } => Target::Custom {
                index: self.custom_index.clone(),
            },
            other => other.clone(),
        };
        (target, (!self.use_saved_token).then(|| self.token.clone()))
    }
}

/// The drafts that differ from the manifest, with their trimmed values, in
/// [`publish::EDITABLE_FIELDS`] order.
///
/// One definition for two readers: the gate that greys out both cargo runs
/// until Preview has run, and Preview itself. They disagreeing is the whole
/// failure mode - a button saying "nothing to write" over a field that never
/// got written.
///
/// A BLANK box means "leave this field alone", never "delete it": the per-row
/// Write button refused an empty value for the same reason, and removing a key
/// is now done in the manifest Preview opens. An inherited field belongs to
/// `[workspace.package]` and is not this table's to shadow.
fn unwritten(manifest: &str, fields: &[(&'static str, String)]) -> Vec<(&'static str, String)> {
    // Nothing is writable into a manifest Preview will refuse to touch, so
    // nothing is pending. Otherwise the buttons would say "press Preview first"
    // about a Preview that answers "there is nothing here to write into" - a
    // loop with no way out. The blocker from `check_manifest` is what should be
    // speaking in that state.
    if !publish::parses(manifest) || !publish::has_package_table(manifest) {
        return Vec::new();
    }
    fields
        .iter()
        .filter_map(|(key, value)| {
            let value = value.trim();
            let differs = !value.is_empty()
                && !publish::is_workspace_inherited(manifest, key)
                && publish::package_field(manifest, key).unwrap_or_default() != value;
            differs.then(|| (*key, value.to_owned()))
        })
        .collect()
}

/// The `[package]` metadata rows, and the width the value boxes ended up with.
///
/// TWO columns, with the boxes as the LAST one on purpose. An [`egui::Grid`]
/// offers a non-final cell only the width its column had LAST frame, and
/// [`egui::TextEdit`] clamps `desired_width` to what it is offered - so a box in
/// a middle column collapsed to `interact_size.x` on the first frame and could
/// never grow back out of it. That is why these were ~40px wide however far the
/// window was dragged open, with `desired_width(330.0)` set on them. Last
/// column plus `INFINITY` makes them follow the window instead.
///
/// The returned width is what `the_boxes_fill_the_window` measures: the bug was
/// pure geometry, so the test has to see geometry.
fn metadata_grid(
    ui: &mut egui::Ui,
    manifest: &str,
    fields: &mut [(&'static str, String)],
    touched: &mut [bool],
) -> f32 {
    let mut box_width: f32 = 0.0;
    egui::Grid::new("publish_fields")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            for (i, (key, hint)) in publish::EDITABLE_FIELDS.iter().enumerate() {
                let required = i < 2;
                let label = if required {
                    egui::RichText::new(format!("{key} *"))
                        .size(11.0)
                        .color(egui::Color32::from_rgb(230, 190, 120))
                } else {
                    egui::RichText::new(*key).size(11.0)
                };
                ui.label(label).on_hover_text(*hint);
                // An inherited field has no literal value here to edit - and
                // with the per-row Write button gone, a typeable box would be
                // silently written into the package table by Preview,
                // shadowing the workspace's own value.
                let inherited = publish::is_workspace_inherited(manifest, key);
                // Until the user takes a box over, it MIRRORS the manifest -
                // which Preview has just opened in the editor for them to edit.
                if !touched[i] {
                    fields[i].1 = publish::package_field(manifest, key)
                        .filter(|_| !inherited)
                        .unwrap_or_default();
                }
                let value = &mut fields[i].1;
                let resp = ui
                    .add_enabled(
                        !inherited,
                        egui::TextEdit::singleline(value)
                            .desired_width(f32::INFINITY)
                            .hint_text(if inherited {
                                "inherited from [workspace.package]"
                            } else {
                                *hint
                            }),
                    )
                    .on_disabled_hover_text(
                        "Inherited from [workspace.package] - change it there.",
                    );
                if resp.changed() {
                    touched[i] = true;
                }
                box_width = box_width.max(resp.rect.width());
                ui.end_row();
            }
        });
    box_width
}

/// Where an upload is actually going, spelled out.
///
/// [`Target::label`] says "Custom registry…", which is the right thing in a
/// picker and the wrong thing to confirm an irreversible upload against.
fn destination(target: &Target) -> String {
    match target {
        Target::CratesIo => "crates.io".to_owned(),
        Target::Configured { name, index } => format!("{name} ({index})"),
        Target::Custom { index } if index.trim().is_empty() => "(no index URL)".to_owned(),
        Target::Custom { index } => index.clone(),
    }
}

impl AppIde {
    /// The registries to offer: crates.io, every one the user's own cargo
    /// config already defines, and a custom index.
    ///
    /// Reading their config is what makes the picker useful rather than a
    /// guess at a vendor list — and a registry cargo already knows needs no
    /// index from us at all, just its name.
    pub(super) fn publish_targets(&self, crate_dir: &str) -> Vec<Target> {
        let mut out = vec![Target::CratesIo];
        let mut configs: Vec<std::path::PathBuf> = Vec::new();
        // Deepest first, because the first definition of a name wins below and
        // cargo's precedence works the same way round: config files are unified
        // walking UP from the working directory, and a deeper one beats
        // `$CARGO_HOME`. Cargo is launched in the crate's own folder and passed
        // only `--registry <name>`, so the crate's config is what a name really
        // resolves to - and that index is what the confirmation then puts in
        // front of the user.
        if let Some(root) = &self.project_dir {
            configs.push(root.join(crate_dir).join(".cargo").join("config.toml"));
            configs.push(root.join(".cargo").join("config.toml"));
        }
        // Then the user's cargo home, honouring CARGO_HOME.
        if let Some(home) = std::env::var_os("CARGO_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|h| std::path::PathBuf::from(h).join(".cargo"))
            })
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cargo"))
            })
        {
            configs.push(home.join("config.toml"));
            // Cargo still reads the extension-less name.
            configs.push(home.join("config"));
        }
        for path in configs {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (name, index) in crate::publish_target::parse_registries(&text) {
                if !out
                    .iter()
                    .any(|t| matches!(t, Target::Configured { name: n, .. } if *n == name))
                {
                    out.push(Target::Configured { name, index });
                }
            }
        }
        out.push(Target::Custom {
            index: String::new(),
        });
        out
    }

    /// The library's `Cargo.toml`, project-root-relative.
    fn lib_manifest_path(dir: &str) -> String {
        format!("{dir}/Cargo.toml")
    }

    /// Read a library's manifest out of the in-memory tree.
    fn lib_manifest(&self, dir: &str) -> Option<String> {
        let path = Self::lib_manifest_path(dir);
        self.project_tree
            .user_src_files
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, c)| c.clone())
    }

    pub(super) fn show_publish_dialog(&mut self, ui: &egui::Ui) {
        let Some(dir) = self.move_or_publish_dir() else {
            return;
        };
        let Some(manifest) = self.lib_manifest(&dir) else {
            // The crate lost its manifest while the window was open. Through
            // the closer, not a bare `None`: a cargo run may still be going,
            // and this is the one path that would leave it with no window, no
            // log and no kill.
            self.close_publish_dialog();
            return;
        };
        // Which siblings are already on a registry is not knowable offline, so
        // every path dependency is reported as a caution naming the crate.
        let findings = publish::check_manifest(&manifest, |_| None);
        let blockers = findings
            .iter()
            .filter(|f| f.severity == Severity::Blocker)
            .count();

        // The crate's OWN name, not the folder's - they need not match, and the
        // name cargo uploads is the one the confirmation has to spell out.
        // `name` cannot be inherited from a workspace, but reading it through
        // the same filter keeps the sentinel out of the button either way.
        let crate_name = publish::package_field(&manifest, "name")
            .filter(|_| !publish::is_workspace_inherited(&manifest, "name"))
            .unwrap_or_else(|| dir.rsplit('/').next().unwrap_or(&dir).to_owned());
        // The version is the whole reason the upload is irreversible - it is
        // the one value that can never be re-used - so the confirmation says
        // SOMETHING about it even when the number lives in the workspace root
        // and this manifest only points at it. Dropping it silently, which is
        // what filtering the inherited case away did, left the confirmation
        // naming two of the three things it exists to name.
        let version = if publish::is_workspace_inherited(&manifest, "version") {
            "from [workspace.package]".to_owned()
        } else {
            publish::package_field(&manifest, "version").unwrap_or_default()
        };

        let mut close = false;
        let mut preview = false;
        let mut start_dry_run = false;
        let mut arm_confirm = false;

        // Read before the window borrows `self`. Cheap, unlike
        // `unsaved_files()`, which takes a disk snapshot and so stays where it
        // is - inside `start_cargo_publish`, checked once per click.
        //
        // And read BEFORE the retry below clears `save_pending`, deliberately.
        // Reading it after would leave one frame where the flag is already
        // clear and the save has not started yet, so both cargo buttons would
        // come alive over a disk that is still stale.
        let waiting_on_save = self.save_in_progress.is_some()
            || self.publish_dialog.as_ref().is_some_and(|d| d.save_pending);

        // Re-ask for the save Preview wanted. One shot is not enough: the flag
        // is consumed early in the frame and dropped when a save is already
        // running. Once it is set with no save in flight it WILL be honoured -
        // nothing else can start one in between - so the flag is cleared then.
        if self.publish_dialog.as_ref().is_some_and(|d| d.save_pending) {
            if self.save_in_progress.is_none() {
                self.request_save = true;
                if let Some(d) = &mut self.publish_dialog {
                    d.save_pending = false;
                }
            }
            // The save runs EARLIER in the frame than this dialog, so it acts
            // on the next one - make sure there is one. Same reason as the
            // exit prompt's `request_repaint` in `dialogs.rs`.
            ui.ctx().request_repaint();
        }

        // ── "Are you sure?" ─────────────────────────────────────────────────
        // A modal rather than a second button in the same row: it dims what is
        // behind it, answers Esc, and - the point - cannot be under the mouse
        // that armed it. The typed crate name in the window is still the first
        // gate; this is the last one.
        //
        // Drawn BEFORE the window, off the flag the previous frame set. Not for
        // input blocking: egui promotes `top_modal_layer_current_frame` only in
        // `Focus::end_pass`, so `is_above_modal_layer` reads the PREVIOUS
        // frame's value and the window behind is live on the modal's first
        // frame whichever order they are drawn in. Drawn first because the
        // reader meets the question before the form it is about — and because
        // arming from a flag means the click that armed it cannot also be the
        // click that answers it.
        let mut start_publish = false;
        if self.publish_dialog.as_ref().is_some_and(|d| d.confirm_open) {
            let where_to = self
                .publish_dialog
                .as_ref()
                .map(|d| destination(&d.chosen().0))
                .unwrap_or_default();
            let mut dismiss = false;
            let resp = egui::Modal::new(egui::Id::new("publish_confirm")).show(ui.ctx(), |ui| {
                ui.set_max_width(430.0);
                ui.label(
                    egui::RichText::new(format!("Publish {crate_name}?"))
                        .size(13.0)
                        .strong(),
                );
                ui.add_space(8.0);
                // Crate, version, destination - one row each, so none of the
                // three can go missing for want of somewhere to put it.
                egui::Grid::new("publish_confirm_facts")
                    .num_columns(2)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        for (label, value) in [
                            ("crate", crate_name.as_str()),
                            ("version", version.as_str()),
                            ("to", where_to.as_str()),
                        ] {
                            ui.label(
                                egui::RichText::new(label)
                                    .size(11.0)
                                    .color(egui::Color32::from_gray(150)),
                            );
                            ui.label(
                                egui::RichText::new(if value.is_empty() {
                                    "(none)"
                                } else {
                                    value
                                })
                                .size(11.0)
                                .monospace()
                                .color(egui::Color32::from_rgb(200, 200, 160)),
                            );
                            ui.end_row();
                        }
                    });
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(
                        "Upload runs cargo publish now. That exact version can never be \
                         re-uploaded, overwritten or deleted - yanking only stops NEW dependents \
                         from resolving it.",
                    )
                    .size(10.5)
                    .color(egui::Color32::from_gray(170)),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(egui::RichText::new("Cancel").size(11.5)))
                        .clicked()
                    {
                        dismiss = true;
                    }
                    // Re-checked HERE, not only where the modal was armed.
                    // Ctrl+S is consumed at the top of the frame, before any
                    // widget and past any layer, so a save can start while this
                    // modal is up - and `start_cargo_publish` would then refuse
                    // the click this sentence promises will upload.
                    if ui
                        .add_enabled(
                            !waiting_on_save,
                            egui::Button::new(
                                egui::RichText::new(format!("Upload {crate_name}"))
                                    .size(11.5)
                                    .color(egui::Color32::from_rgb(240, 200, 190)),
                            )
                            .fill(egui::Color32::from_rgb(120, 50, 45)),
                        )
                        .on_disabled_hover_text(
                            "A project save is running - cargo reads the folder on disk.",
                        )
                        .clicked()
                    {
                        start_publish = true;
                    }
                });
            });
            if (dismiss || start_publish || resp.should_close())
                && let Some(d) = &mut self.publish_dialog
            {
                d.confirm_open = false;
            }
        }

        // Centred on open but deliberately NOT anchored: an anchor re-pins the
        // window every frame, and Preview would be worth nothing if the
        // manifest it opens in the editor sat under a window that cannot be
        // dragged off it. 620 x 520 is the content's size, not the window's.
        let size = super::helpers::window_size::outer_size(ui.ctx(), egui::vec2(620.0, 520.0));
        let centre = ui.ctx().content_rect().center() - 0.5 * size;
        egui::Window::new(format!("Publish {dir}"))
            .id(egui::Id::new("publish_dialog"))
            .collapsible(false)
            .resizable(true)
            .default_size(size)
            .default_pos(centre)
            .show(ui.ctx(), |ui| {
                let dlg = self.publish_dialog.as_mut().expect("checked above");

                ui.label(
                    egui::RichText::new(
                        "Check, preview, rehearse, then upload. The upload at the bottom is permanent.",
                    )
                    .size(10.5)
                    .color(egui::Color32::from_gray(150)),
                );
                ui.add_space(8.0);

                // ── What would be refused ───────────────────────────────────
                ui.label(egui::RichText::new("Before publishing").size(11.5).strong());
                ui.add_space(4.0);
                if findings.is_empty() {
                    ui.label(
                        egui::RichText::new("Nothing missing.")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(140, 200, 150)),
                    );
                }
                for f in &findings {
                    let (icon, color) = match f.severity {
                        Severity::Blocker => (
                            egui_phosphor::regular::X_CIRCLE,
                            egui::Color32::from_rgb(220, 90, 80),
                        ),
                        Severity::Advice => (
                            egui_phosphor::regular::WARNING,
                            egui::Color32::from_rgb(220, 180, 60),
                        ),
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(icon).size(11.0).color(color));
                        ui.label(egui::RichText::new(&f.message).size(10.5));
                    });
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── The metadata, editable here ─────────────────────────────
                ui.label(egui::RichText::new("Package metadata").size(11.5).strong())
                    .on_hover_text(
                        "The crate-creation dialog only ever writes five fields and never \
                         reopens, so this is where the rest get filled in. Preview is what \
                         puts them in the library's Cargo.toml.",
                    );
                ui.add_space(4.0);
                let _ = metadata_grid(ui, &manifest, &mut dlg.fields, &mut dlg.touched);
                ui.label(
                    egui::RichText::new(
                        "* required by crates.io. Nothing typed here reaches Cargo.toml until \
                         Preview - and an EMPTY box leaves its field as it is: to remove a key, \
                         delete it in the manifest Preview opens.",
                    )
                    .size(9.5)
                    .color(egui::Color32::from_gray(140)),
                );

                // Drafts that differ from the manifest. A BLANK box means
                // "leave this field alone", never "delete it" - the per-row
                // Write button refused an empty value for the same reason, and
                // removing a key is now done in the manifest Preview opens.
                let pending: Vec<&'static str> = unwritten(&manifest, &dlg.fields)
                    .into_iter()
                    .map(|(key, _)| key)
                    .collect();

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // ── Where it goes ───────────────────────────────────────────
                ui.label(egui::RichText::new("Registry").size(11.5).strong());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("publish_target")
                        .selected_text(
                            egui::RichText::new(dlg.targets[dlg.target].label()).size(11.0),
                        )
                        .width(300.0)
                        .show_ui(ui, |ui| {
                            for (i, t) in dlg.targets.iter().enumerate() {
                                ui.selectable_value(
                                    &mut dlg.target,
                                    i,
                                    egui::RichText::new(t.label()).size(11.0),
                                );
                            }
                        });
                    if let Target::Configured { index, .. } = &dlg.targets[dlg.target] {
                        ui.label(
                            egui::RichText::new(index)
                                .size(10.0)
                                .monospace()
                                .color(egui::Color32::from_gray(150)),
                        );
                    }
                });
                if matches!(dlg.targets[dlg.target], Target::Custom { .. }) {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Index URL").size(11.0));
                        ui.add(
                            egui::TextEdit::singleline(&mut dlg.custom_index)
                                .desired_width(400.0)
                                .hint_text("sparse+https://…/index/"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Templates:")
                                .size(10.0)
                                .color(egui::Color32::from_gray(140)),
                        );
                        for (name, url) in publish_target::INDEX_TEMPLATES {
                            if ui
                                .add(egui::Button::new(egui::RichText::new(*name).size(10.0)).frame(false))
                                .on_hover_text(*url)
                                .clicked()
                            {
                                dlg.custom_index = (*url).to_owned();
                            }
                        }
                    });
                }

                ui.add_space(8.0);
                ui.label(egui::RichText::new("Credentials").size(11.5).strong());
                ui.add_space(4.0);
                ui.checkbox(
                    &mut dlg.use_saved_token,
                    egui::RichText::new("Use the credentials cargo already has").size(11.0),
                )
                .on_hover_text(
                    "Passes no token at all - cargo reads the one `cargo login` stored. Untick to paste a token for this publish only; it is kept in memory and never written to disk.",
                );
                if !dlg.use_saved_token {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Token").size(11.0));
                        ui.add(
                            egui::TextEdit::singleline(&mut dlg.token)
                                .desired_width(360.0)
                                .password(!dlg.show_token),
                        );
                        ui.checkbox(&mut dlg.show_token, egui::RichText::new("show").size(10.0));
                    });
                    dlg.token.retain(|c| !c.is_whitespace());
                    ui.label(
                        egui::RichText::new(
                            "Passed to cargo through the environment, never as a command-line argument - an argument would be readable in the process list.",
                        )
                        .size(9.5)
                        .color(egui::Color32::from_gray(140)),
                    );
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                let (chosen, token) = dlg.chosen();
                let target_problem = publish_target::target_blocker(&chosen, token.as_deref());
                let confirmed = dlg.confirm.trim() == crate_name;

                // ── The rehearsal ───────────────────────────────────────────
                let busy = dlg.running.load(std::sync::atomic::Ordering::Relaxed);
                // Both cargo runs read the manifest ON DISK. Rehearsing or
                // uploading while an edited field is still only in this window
                // is a rehearsal of a different crate, so both wait for Preview.
                let unwritten_msg = format!(
                    "Press Preview first - {} {} still only in this window.",
                    pending.join(", "),
                    if pending.len() == 1 { "is" } else { "are" },
                );
                ui.horizontal(|ui| {
                    // Disabled while cargo runs: this writes the manifest and
                    // saves the project, and doing either under a live
                    // `cargo publish` is how a run fails for a reason nothing
                    // on screen explains.
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(
                                egui::RichText::new("Preview Cargo.toml").size(11.5),
                            ),
                        )
                        .on_hover_text(
                            "Writes every field above into the library's Cargo.toml, saves the \
                             project, and opens the manifest in the code editor - so what you \
                             read there is exactly what cargo will package.",
                        )
                        .on_disabled_hover_text("Already running")
                        .clicked()
                    {
                        preview = true;
                    }
                    if ui
                        .add_enabled(
                            !busy && !waiting_on_save && pending.is_empty(),
                            egui::Button::new(
                                egui::RichText::new("Run cargo publish --dry-run").size(11.5),
                            ),
                        )
                        .on_hover_text(
                            "Packages and builds the crate exactly as a publish would, and \
                             stops before the upload. It does NOT check the two fields \
                             crates.io rejects on - that is what the list above is for.",
                        )
                        .on_disabled_hover_text(if busy {
                            "Already running"
                        } else if waiting_on_save {
                            "Waiting for the project save to finish - cargo reads the folder on \
                             disk"
                        } else {
                            unwritten_msg.as_str()
                        })
                        .clicked()
                    {
                        // The rehearsal uses the SELECTED registry, not
                        // crates.io: a dry run against a different index is a
                        // rehearsal of a different publish.
                        start_dry_run = true;
                    }
                    if busy {
                        ui.spinner();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Close").size(11.5)).clicked() {
                            close = true;
                        }
                    });
                });
                // ── The real thing ──────────────────────────────────────────
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Publish for real")
                        .size(11.5)
                        .strong()
                        .color(egui::Color32::from_rgb(230, 150, 120)),
                );
                ui.label(
                    egui::RichText::new(
                        "A publish is permanent: the version can never be overwritten and the code cannot be deleted. Yanking stops NEW dependents, it does not remove anything.",
                    )
                    .size(10.0)
                    .color(egui::Color32::from_gray(150)),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Type `{crate_name}` to confirm")).size(11.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut dlg.confirm)
                            .desired_width(200.0)
                            .hint_text(&crate_name),
                    );
                });
                ui.horizontal(|ui| {
                    // `waiting_on_save` is in here so the modal cannot promise
                    // an upload that `start_cargo_publish` would then refuse:
                    // the last sentence before an irreversible action has to be
                    // true when it is read.
                    let ready = !busy
                        && !waiting_on_save
                        && blockers == 0
                        && pending.is_empty()
                        && target_problem.is_none()
                        && confirmed;
                    let why = if busy {
                        "Already running"
                    } else if waiting_on_save {
                        "Waiting for the project save to finish - cargo reads the folder on disk"
                    } else if blockers > 0 {
                        "Fix the blockers listed above first"
                    } else if !pending.is_empty() {
                        unwritten_msg.as_str()
                    } else if let Some(p) = &target_problem {
                        p.as_str()
                    } else {
                        "Type the crate name to confirm"
                    };
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(
                                egui::RichText::new(format!("Publish {crate_name}"))
                                    .size(11.5)
                                    .color(egui::Color32::from_rgb(240, 200, 190)),
                            )
                            .fill(egui::Color32::from_rgb(120, 50, 45)),
                        )
                        .on_hover_text("Asks once more, then uploads. This cannot be undone.")
                        .on_disabled_hover_text(why)
                        .clicked()
                    {
                        arm_confirm = true;
                    }
                    if !confirmed && !dlg.confirm.trim().is_empty() {
                        ui.label(
                            egui::RichText::new("that is not the crate name")
                                .size(10.0)
                                .color(egui::Color32::from_rgb(220, 150, 120)),
                        );
                    }
                });

                if blockers > 0 {
                    ui.label(
                        egui::RichText::new(format!(
                            "{blockers} blocker(s) above would still make the real upload fail."
                        ))
                        .size(10.0)
                        .color(egui::Color32::from_rgb(220, 150, 120)),
                    );
                }

                if let Some(e) = &dlg.error {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(e)
                            .size(10.5)
                            .color(egui::Color32::from_rgb(230, 130, 115)),
                    );
                }

                ui.add_space(6.0);
                let log = Arc::clone(&dlg.log);
                egui::ScrollArea::vertical()
                    .id_salt("publish_log")
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut guard = log.lock().unwrap();
                        // The reader thread repaints only a view drawn lately.
                        guard.drawn_pass =
                            ui.ctx().cumulative_pass_nr_for(egui::ViewportId::ROOT);
                        for line in &guard.lines {
                            // A terminal line is coloured SPANS (cargo's ANSI
                            // is parsed on the way in); the kind's colour is
                            // the fallback for a span that carries none.
                            let fallback = match line.kind {
                                LineKind::Input => egui::Color32::from_rgb(140, 190, 240),
                                LineKind::Stderr => egui::Color32::from_rgb(220, 150, 120),
                                LineKind::Notice => egui::Color32::from_rgb(150, 200, 150),
                                LineKind::Stdout => egui::Color32::from_gray(190),
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for (text, color) in &line.spans {
                                    ui.label(
                                        egui::RichText::new(text)
                                            .size(10.5)
                                            .monospace()
                                            .color(color.unwrap_or(fallback)),
                                    );
                                }
                            });
                        }
                    });
            });

        // Applied outside the window closure: both borrow `self`.
        if preview {
            self.apply_publish_preview(&dir, &manifest);
        }

        // Armed here, shown at the TOP of the next frame - see the modal above.
        // So there has to BE a next frame: with nothing else animating, egui
        // sits idle, and the confirmation would appear whenever the next input
        // happened to arrive rather than on the click that asked for it.
        if arm_confirm && let Some(d) = &mut self.publish_dialog {
            d.confirm_open = true;
            ui.ctx().request_repaint();
        }

        // Read once more, after the widgets: a token typed in this same frame
        // has to be the one that travels.
        // The two cannot both be set in one frame - `interact` records one
        // clicked id per pass. If they ever were, the REHEARSAL wins: this is
        // the fallback guarding the one irreversible action in the IDE, and
        // `!start_publish` had it the other way round.
        if (start_publish || start_dry_run)
            && let Some((target, token)) = self.publish_dialog.as_ref().map(|d| d.chosen())
        {
            let dry_run = start_dry_run || !start_publish;
            self.start_cargo_publish(&dir, target, token, dry_run, ui.ctx());
        }
        if close {
            self.close_publish_dialog();
        }
    }

    /// Drop the publish dialog, taking its cargo child with it.
    ///
    /// Never `publish_dialog = None` on its own. Dropping the dialog drops only
    /// this side of the `Arc`; the streaming thread holds its own clone, so the
    /// process keeps going with nowhere to report and no way to cancel it - the
    /// Close button was the only kill path, and its window no longer exists.
    /// Called from Close, from a project load, and before the dialog is
    /// replaced.
    pub(super) fn close_publish_dialog(&mut self) {
        if let Some(d) = &self.publish_dialog {
            d.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            // Kill it, do not merely stop reading it - then reap it, so a
            // killed cargo does not linger as a zombie.
            if let Some(mut c) = d.child.lock().unwrap().take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        self.publish_dialog = None;
    }

    /// Is a cargo run from the publish dialog still going — rehearsal or real?
    pub(super) fn publish_running(&self) -> bool {
        self.publish_dialog
            .as_ref()
            .is_some_and(|d| d.running.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Is a REAL upload in flight?
    ///
    /// The gate in [`AppIde::start_cargo_publish`] only runs one way — it stops
    /// cargo starting on top of a save. This is the other way: `write_project`
    /// walks the project root and rewrites every source file, which is the
    /// exact tree `cargo publish` is reading into its tarball. A rehearsal can
    /// be re-run, so only the upload is worth making the user wait for.
    pub(super) fn publish_uploading(&self) -> bool {
        self.publish_dialog
            .as_ref()
            .is_some_and(|d| !d.running_dry && d.running.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Preview: write every edited field into the library's `Cargo.toml`, then
    /// show that manifest in the code editor.
    ///
    /// This is what the per-row "Write" buttons became. Writing all of them on
    /// the way to the editor - rather than one at a time - means the text the
    /// user then reads IS the answer, with no half-written manifest to reason
    /// about. The project is saved as well, because cargo packages what is on
    /// disk: a preview the user approves has to be the text cargo will read.
    fn apply_publish_preview(&mut self, dir: &str, manifest: &str) {
        let path = Self::lib_manifest_path(dir);
        // Cloned, not borrowed: the error paths below need `&mut self`.
        let Some(fields) = self.publish_dialog.as_ref().map(|d| d.fields.clone()) else {
            return;
        };
        let Some(i) = self
            .project_tree
            .user_src_files
            .iter()
            .position(|(p, _)| *p == path)
        else {
            if let Some(d) = &mut self.publish_dialog {
                d.error = Some(format!("`{path}` is no longer in the project."));
            }
            return;
        };
        // Selected whatever happens below: the button's promise is that the
        // manifest is on screen, and it is worth even more when the reason
        // nothing could be written is IN that manifest.
        self.selected_file = ProjectFileId::UserFile(i);

        // Two different failures, two different sentences. `has_package_table`
        // answers false for a manifest that does not parse AND for one that
        // parses but is a workspace, and telling a half-typed file that it
        // "describes a workspace" sends the reader after the wrong thing.
        if !publish::parses(manifest) {
            if let Some(d) = &mut self.publish_dialog {
                d.error = Some(format!(
                    "`{path}` is not valid TOML at the moment - fix it in the editor first."
                ));
            }
            return;
        }
        // A manifest with no `[package]` is a virtual workspace, not a crate
        // that forgot its metadata. Writing into it would create a package with
        // no `name` above the `[workspace]` section, which cargo then refuses to
        // load - taking the build and rust-analyzer down with it.
        if !publish::has_package_table(manifest) {
            if let Some(d) = &mut self.publish_dialog {
                d.error = Some(format!(
                    "`{path}` has no [package] table - it describes a workspace, not a crate, so \
                     there is nothing here to write these fields into."
                ));
            }
            return;
        }
        // The SAME set the buttons are gated on, so Preview cannot leave behind
        // a field the gate still counts.
        let todo = unwritten(manifest, &fields);
        // Each edit is applied to the RESULT of the last one. Seven calls to
        // `set_package_field` off the original text would keep only the last.
        let mut updated = manifest.to_owned();
        for (key, value) in &todo {
            // `None` = the manifest stopped parsing (half-typed in the editor).
            // Refused rather than written over.
            // Unreachable in practice - the two guards above already proved the
            // text parses and has a `[package]`, and every later `updated` is
            // toml_edit's own output. Kept because the signature admits it, and
            // worded so it does not claim to know why.
            let Some(next) = publish::set_package_field(&updated, key, value) else {
                if let Some(d) = &mut self.publish_dialog {
                    d.error = Some(format!("could not write `{key}` into `{path}`."));
                }
                return;
            };
            updated = next;
        }
        if !todo.is_empty() {
            self.project_tree.user_src_files[i].1 = updated;
            self.workspace_write_requested = true;
        }
        // Asked for whether or not a field changed. The workspace flag only
        // refreshes the scratch copy rust-analyzer reads; cargo publishes from
        // the PROJECT folder, and BOTH cargo runs now refuse while anything is
        // unsaved. A Preview that wrote nothing but left the project dirty
        // would leave the user with two buttons that refuse on click and the
        // one button that could fix it doing nothing - which is what the hover
        // text promises it does.
        let can_save = self.project_dir.is_some() && self.selected_build_cfg().is_some();
        if let Some(d) = &mut self.publish_dialog {
            if can_save {
                d.save_pending = true;
                d.error = None;
            } else {
                // The save path needs both a folder and a chip. Said out loud
                // rather than skipped, because the button's hover promises a
                // save and both cargo runs refuse without one. Set AFTER the
                // clear, not before it, or it would wipe its own message.
                d.error = Some(
                    "Written to the buffer, but the project cannot be saved yet - it needs a \
                     folder and a selected chip. cargo reads the folder on disk."
                        .to_owned(),
                );
            }
            // Only the boxes that were WRITTEN go back to mirroring the
            // manifest. A box the user deliberately emptied is not in `todo`
            // (blank means "leave the field alone"), and resetting it too would
            // refill it from the manifest on the very next frame - undoing
            // their deletion in front of them with nothing said.
            for (key, _) in &todo {
                if let Some(pos) = publish::EDITABLE_FIELDS.iter().position(|(k, _)| k == key) {
                    d.touched[pos] = false;
                }
            }
        }
    }

    /// The library directory the publish dialog is open on.
    fn move_or_publish_dir(&self) -> Option<String> {
        self.publish_dialog.as_ref().map(|d| d.dir.clone())
    }

    /// Launch cargo on the library, streaming into the dialog's log.
    ///
    /// One function for both the rehearsal and the real thing, because the only
    /// differences are the flags and the environment — and keeping them in one
    /// place is what makes it impossible for the real publish to pick up
    /// `--allow-dirty` by accident (see [`publish_target::publish_command`]).
    ///
    /// Runs against the SAVED project folder, not the scratch workspace: cargo
    /// packages what is on disk, and the scratch copy carries no `.git`, which
    /// would silently change the dirty-tree answer.
    fn start_cargo_publish(
        &mut self,
        dir: &str,
        target: Target,
        token: Option<String>,
        dry_run: bool,
        ctx: &egui::Context,
    ) {
        let Some(root) = self.project_dir.clone() else {
            if let Some(d) = &mut self.publish_dialog {
                d.error =
                    Some("Save the project first - cargo packages what is on disk.".to_owned());
            }
            return;
        };
        // A save worker writes the very folder cargo is about to package, so
        // neither run may start on top of one. `unsaved_files` cannot stand in
        // for this: it compares memory against disk, and mid-save the file may
        // already match while the rest of the write is still going.
        if self.save_in_progress.is_some() {
            if let Some(d) = &mut self.publish_dialog {
                d.error = Some(
                    "A save is still running - cargo would package a folder being written."
                        .to_owned(),
                );
            }
            return;
        }
        // Unsaved edits are in the buffers, not on disk, and cargo reads disk.
        // BOTH runs, not just the upload: a rehearsal of text that never
        // reached the folder is a rehearsal of a different crate, and it
        // reports "dry run passed" for it.
        if !self.unsaved_files().is_empty() {
            if let Some(d) = &mut self.publish_dialog {
                d.error = Some(
                    "Save the project first (Ctrl+S) - cargo reads the folder on disk, not the \
                     editor buffers."
                        .to_owned(),
                );
            }
            return;
        }
        // Every refusal above set `error`; getting past them clears it. All
        // three are transient - a save that has since finished, a project
        // that has since been saved - so leaving the last one on screen would
        // have it sit under a run that is going perfectly well.
        if let Some(d) = &mut self.publish_dialog {
            d.error = None;
            d.running_dry = dry_run;
        }
        let Some(d) = &self.publish_dialog else {
            return;
        };
        let (log, stop, running, child_slot) = (
            Arc::clone(&d.log),
            Arc::clone(&d.stop),
            Arc::clone(&d.running),
            Arc::clone(&d.child),
        );
        let (args, env) = publish_target::publish_command(&target, token.as_deref(), dry_run);
        stop.store(false, std::sync::atomic::Ordering::Relaxed);
        running.store(true, std::sync::atomic::Ordering::Relaxed);
        {
            let mut g = log.lock().unwrap();
            g.lines.clear();
            // The echoed line names the variables but NEVER their values - a
            // token pasted into this window must not end up in a log the user
            // may screenshot.
            let shown_env: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
            let suffix = if shown_env.is_empty() {
                String::new()
            } else {
                format!("   [env: {}]", shown_env.join(", "))
            };
            g.push_plain(
                LineKind::Input,
                format!("> cargo {}{suffix}", args.join(" ")),
            );
        }
        let crate_dir = root.join(dir);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new("cargo");
            crate::build::no_window(&mut cmd)
                .current_dir(&crate_dir)
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            for (k, v) in &env {
                cmd.env(k, v);
            }
            let child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    running.store(false, std::sync::atomic::Ordering::Relaxed);
                    if let Ok(mut g) = log.lock() {
                        g.push_plain(LineKind::Notice, format!("could not launch cargo: {e}"));
                    }
                    ctx.request_repaint();
                    return;
                }
            };
            let mut child = child;
            let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            // Cargo says everything interesting on stderr, progress included.
            if let Some(err) = stderr {
                crate::terminal::spawn_reader(
                    err,
                    LineKind::Stdout,
                    Arc::clone(&log),
                    Arc::clone(&stop),
                    ctx.clone(),
                    Arc::clone(&done),
                );
            }
            if let Some(out) = stdout {
                crate::terminal::spawn_reader(
                    out,
                    LineKind::Stdout,
                    Arc::clone(&log),
                    Arc::clone(&stop),
                    ctx.clone(),
                    Arc::clone(&done),
                );
            }
            // Parked so Close can kill it - and LEFT there. Parking it and
            // taking it straight back out (two consecutive locks, which is what
            // this did) left the child in the slot for a few nanoseconds of the
            // whole run, so Close's kill never found one: the window shut and
            // the upload carried on regardless. Polled instead, so the child is
            // reachable for the entire run and `wait` still never happens with
            // the lock held.
            *child_slot.lock().unwrap() = Some(child);
            let status = loop {
                {
                    let mut slot = child_slot.lock().unwrap();
                    match slot.as_mut() {
                        // Close took it and killed it.
                        None => {
                            running.store(false, std::sync::atomic::Ordering::Relaxed);
                            ctx.request_repaint();
                            return;
                        }
                        Some(c) => match c.try_wait() {
                            Ok(Some(s)) => {
                                *slot = None;
                                break Ok(s);
                            }
                            // Still going - fall through to the sleep.
                            Ok(None) => {}
                            Err(e) => {
                                *slot = None;
                                break Err(e);
                            }
                        },
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            };
            let msg = match (status, dry_run) {
                (Ok(s), true) if s.success() => {
                    "dry run passed - the crate packages and builds. It does NOT mean the registry would accept it; see the checks above."
                        .to_owned()
                }
                (Ok(s), false) if s.success() => {
                    "published. This version is now permanent - it cannot be overwritten or deleted, only yanked."
                        .to_owned()
                }
                (Ok(s), _) => format!("cargo exited with {s}."),
                (Err(e), _) => format!("could not wait for cargo: {e}"),
            };
            // Cleared BEFORE touching the shared log: the reader threads hold
            // that same mutex, and one of them panicking would poison it, so an
            // `unwrap()` here would kill this thread with the flag still set -
            // both buttons then read "Already running" for good.
            running.store(false, std::sync::atomic::Ordering::Relaxed);
            if let Ok(mut g) = log.lock() {
                g.push_plain(LineKind::Notice, msg);
            }
            ctx.request_repaint();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{destination, unwritten};
    use crate::publish;
    use crate::publish_target::Target;

    /// The seven draft boxes, in `EDITABLE_FIELDS` order, from a shorter list.
    fn boxes(set: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
        publish::EDITABLE_FIELDS
            .iter()
            .map(|(key, _)| {
                let v = set
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| (*v).to_owned())
                    .unwrap_or_default();
                (*key, v)
            })
            .collect()
    }

    fn keys(v: Vec<(&'static str, String)>) -> Vec<&'static str> {
        v.into_iter().map(|(k, _)| k).collect()
    }

    const MANIFEST: &str = "[package]\nname = \"radar\"\nversion = \"0.1.0\"\n";

    /// Lay the metadata rows out headless and report the width the value boxes
    /// got. Three passes: an `egui::Grid` sizes a column from the frame BEFORE,
    /// so a single pass would report the seed value rather than the answer -
    /// and the collapsed-box bug was exactly a seed value that never grew.
    fn box_width(available: f32) -> f32 {
        let ctx = egui::Context::default();
        let mut fields = boxes(&[]);
        let mut touched = vec![false; fields.len()];
        let mut width = 0.0;
        for _ in 0..3 {
            let _ = crate::headless::run_ui(&ctx, Default::default(), |ui| {
                ui.set_max_width(available);
                width = super::metadata_grid(ui, MANIFEST, &mut fields, &mut touched);
            });
        }
        width
    }

    #[test]
    fn the_boxes_fill_the_window() {
        // The label column takes its own width and the grid one gap; nothing
        // else may. `desired_width(330.0)` in a middle column used to leave
        // these at `interact_size.x` - 40px - for the life of the window.
        let width = box_width(600.0);
        assert!(
            width > 450.0,
            "the value boxes got {width}px of a 600px window - they collapsed again"
        );
    }

    #[test]
    fn the_boxes_follow_the_window_as_it_widens() {
        let narrow = box_width(400.0);
        let wide = box_width(900.0);
        assert!(
            wide - narrow > 450.0,
            "500px more window gave the boxes only {}px more ({narrow} -> {wide})",
            wide - narrow
        );
    }

    /// Lay the rows out once against `manifest` and hand back the boxes.
    fn one_frame(manifest: &str, fields: &mut [(&'static str, String)], touched: &mut [bool]) {
        let ctx = egui::Context::default();
        let _ = crate::headless::run_ui(&ctx, Default::default(), |ui| {
            super::metadata_grid(ui, manifest, fields, touched);
        });
    }

    #[test]
    fn an_untouched_box_follows_the_manifest() {
        // Preview opens Cargo.toml in the editor for the user to edit. A box
        // frozen at open time would then offer to write the value it was
        // seeded with back over what they typed there - and the "press Preview
        // first" gate would insist on it.
        let mut fields = boxes(&[("description", "what the box was seeded with")]);
        let mut touched = vec![false; fields.len()];
        let edited = "[package]\nname = \"radar\"\ndescription = \"typed in the editor\"\n";
        one_frame(edited, &mut fields, &mut touched);
        assert_eq!(fields[0].1, "typed in the editor");
        assert!(
            unwritten(edited, &fields).is_empty(),
            "and so there is nothing left for Preview to write"
        );
    }

    #[test]
    fn a_touched_box_keeps_what_the_user_typed() {
        let mut fields = boxes(&[("description", "mine")]);
        let mut touched = vec![false; fields.len()];
        touched[0] = true;
        let m = "[package]\nname = \"radar\"\ndescription = \"theirs\"\n";
        one_frame(m, &mut fields, &mut touched);
        assert_eq!(fields[0].1, "mine");
        assert_eq!(keys(unwritten(m, &fields)), vec!["description"]);
    }

    #[test]
    fn an_inherited_box_never_shows_the_sentinel() {
        // `package_field` answers "(inherited from the workspace)" for these.
        // Putting that in an editable box is how it once got written into a
        // manifest as if it were a description.
        let m = "[package]\nname = \"radar\"\ndescription.workspace = true\n";
        let mut fields = boxes(&[]);
        let mut touched = vec![false; fields.len()];
        one_frame(m, &mut fields, &mut touched);
        assert_eq!(fields[0].1, "");
    }

    #[test]
    fn nothing_is_pending_in_a_manifest_preview_would_refuse() {
        // Otherwise both cargo buttons say "press Preview first" about a
        // Preview that answers "there is nothing here to write into" - a loop
        // with no way out. The blocker from `check_manifest` is what should be
        // speaking in that state, and it is.
        let workspace = "[workspace]\nmembers = [\"core\"]\n";
        let typed = boxes(&[("description", "a radar driver")]);
        assert!(unwritten(workspace, &typed).is_empty());
        assert!(
            publish::check_manifest(workspace, |_| None)
                .iter()
                .any(|f| f.severity == publish::Severity::Blocker),
            "and the window says why"
        );

        let broken = "[package\nname = \"radar\"\n";
        assert!(unwritten(broken, &typed).is_empty());
    }

    #[test]
    fn an_empty_box_is_not_an_edit() {
        // Every box blank: nothing to write, so nothing may block the buttons.
        assert!(unwritten(MANIFEST, &boxes(&[])).is_empty());
    }

    #[test]
    fn a_typed_value_is_reported_with_its_key() {
        let got = unwritten(MANIFEST, &boxes(&[("description", "a radar driver")]));
        assert_eq!(got, vec![("description", "a radar driver".to_owned())]);
    }

    #[test]
    fn surrounding_whitespace_is_not_an_edit() {
        let m = "[package]\ndescription = \"a radar driver\"\n";
        assert!(unwritten(m, &boxes(&[("description", "  a radar driver  ")])).is_empty());
    }

    #[test]
    fn an_untouched_dialog_never_demands_a_preview() {
        // Seeded through `PublishDialog::new` ITSELF, not by re-typing the
        // expression `unwritten` evaluates - which is what made the first
        // version of this test an identity check that held whatever the
        // seeding code did. What it guards is the coupling: if `new` ever
        // seeds a box differently from how `unwritten` reads the manifest
        // back, an untouched crate could never be published, because the gate
        // would report an edit that Preview then finds nothing to write.
        //
        // The array spellings are deliberate. `package_field` renders an array
        // with `to_string`, which keeps the manifest's OWN decor, so both the
        // odd spacing and the missing space have to survive the round trip.
        let m = "[package]\n\
                 name = \"radar\"\n\
                 description = \"a radar driver\"\n\
                 license = \"MIT\"\n\
                 keywords = [\"embedded\",\"radar\"]\n\
                 categories = [ \"embedded\" , \"no-std\" ]\n";
        let dlg = super::PublishDialog::new("libs/radar".to_owned(), m, vec![Target::CratesIo]);
        assert!(
            unwritten(m, &dlg.fields).is_empty(),
            "{:?}",
            unwritten(m, &dlg.fields)
        );
        assert!(
            dlg.touched.iter().all(|t| !t),
            "and every box starts out mirroring the manifest"
        );
    }

    #[test]
    fn an_inherited_field_is_never_written() {
        // The box is disabled and empty for an inherited field, but a manifest
        // edited by hand can inherit one the dialog was opened with a value
        // for. `[workspace.package]` owns it either way.
        let m = "[package]\nname = \"radar\"\ndescription.workspace = true\n";
        assert!(unwritten(m, &boxes(&[("description", "typed anyway")])).is_empty());
    }

    #[test]
    fn every_changed_field_is_listed_in_field_order() {
        let got = keys(unwritten(
            MANIFEST,
            &boxes(&[
                ("license", "MIT"),
                ("description", "a radar driver"),
                ("readme", "README.md"),
            ]),
        ));
        assert_eq!(got, vec!["description", "license", "readme"]);
    }

    #[test]
    fn a_blank_manifest_field_still_counts_as_missing() {
        // `description = ""` reads as absent everywhere else in this module,
        // so typing over it is an edit.
        let m = "[package]\nname = \"radar\"\ndescription = \"\"\n";
        assert_eq!(
            keys(unwritten(m, &boxes(&[("description", "a radar driver")]))),
            vec!["description"]
        );
    }

    #[test]
    fn the_confirmation_names_the_index_not_the_picker_label() {
        assert_eq!(destination(&Target::CratesIo), "crates.io");
        assert_eq!(
            destination(&Target::Configured {
                name: "internal".to_owned(),
                index: "sparse+https://reg.example/index/".to_owned(),
            }),
            "internal (sparse+https://reg.example/index/)"
        );
        assert_eq!(
            destination(&Target::Custom {
                index: "sparse+https://typed.example/index/".to_owned(),
            }),
            "sparse+https://typed.example/index/"
        );
        // `label()` would say "Custom registry…" here, which confirms nothing.
        assert_eq!(
            destination(&Target::Custom {
                index: "   ".to_owned()
            }),
            "(no index URL)"
        );
    }
}
