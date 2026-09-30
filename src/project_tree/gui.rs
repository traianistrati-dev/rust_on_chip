//! Project tree GUI — file browser with create/rename/delete operations.

use crate::app::ProjectFileId;
use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
use crate::project_tree::clipboard;
use crate::project_tree::logic::SRC_ROOT;
use crate::{build, lsp};
use eframe::egui;
use egui_phosphor::regular as ph;
use std::collections::BTreeMap;

/// Tree node representing a file or folder.
#[derive(Clone, Debug)]
enum TreeNode {
    File(usize), // index into user_src_files
    Folder(BTreeMap<String, TreeNode>),
}

/// Drag-and-drop payload: the tree item being dragged onto a folder (drop
/// target). `Send + Sync + 'static` as egui requires.
#[derive(Clone)]
enum DraggedItem {
    /// A user file, by its `user_src_files` index.
    File(usize),
    /// A folder, by its `src/`-relative path (moves with all its contents).
    Folder(String),
}

/// A validated file rename the tree wants performed. Carried to the app rather
/// than applied here: the module-reference rewrite has to run BEFORE the file
/// moves, and only the app can talk to rust-analyzer.
#[derive(Clone, Debug)]
pub struct RenameRequest {
    pub old_path: String,
    /// Project-root-relative, same folder as `old_path`.
    pub new_path: String,
}

/// The last path segment of a `src/`-relative path (the bare file/folder name).
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// How long the primary button must be held STILL on an item before its drag
/// arms (the payload is set). Holding still is required because moving the
/// pointer early cancels egui's press-on-widget tracking — which is exactly
/// what makes a quick click/drag gesture NOT start a move.
const DRAG_HOLD_SECS: f64 = 0.4;

/// Hover affordance for an interactive tree row (file / folder header): a
/// subtle tint over the row plus the pointing-hand cursor — makes it visible
/// WHERE a click / right-click menu / hold-to-drag actually works. The cursor
/// then progresses hand → grab (while the button is held, see the call sites)
/// → grabbing/no-drop (once the drag arms; set by the payload block at the end
/// of the tree, which runs last and therefore wins).
fn row_hover_feedback(ui: &egui::Ui, rect: egui::Rect, contains_pointer: bool) {
    if contains_pointer {
        ui.painter().rect_filled(
            rect.expand2(egui::vec2(2.0, 0.0)),
            3.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// Long-press-to-drag gate, fully stateless: `true` once the PRIMARY button has
/// been held on `resp` for ≥ [`DRAG_HOLD_SECS`] (press-start time comes from
/// egui's own pointer state — no temp-memory bookkeeping that could go stale).
/// A normal click, a right-click (secondary → `primary_down()` is false), or a
/// context-menu interaction never arms, so dragging can't hijack those — the
/// caller only sets the DnD payload when this returns `true`.
fn drag_armed(ui: &egui::Ui, resp: &egui::Response) -> bool {
    if !resp.is_pointer_button_down_on() {
        return false;
    }
    ui.input(|i| {
        i.pointer.primary_down()
            && i.pointer
                .press_start_time()
                .is_some_and(|t| i.time - t >= DRAG_HOLD_SECS)
    })
}

/// If `path` is an auto-generated folder that must NOT be moved, return a short
/// explanation; otherwise `None`. `pins/` and `pins/configs/` are recreated
/// every frame by the pin/peripheral sync, so moving them breaks codegen.
fn generated_folder_reason(path: &str) -> Option<&'static str> {
    match path {
        "src/pins" => Some("the `pins/` folder is auto-generated from your pin configuration"),
        "src/pins/configs" => {
            Some("`pins/configs/` is auto-generated from the Virtual Modules (USART/SPI/I2C)")
        }
        p if p.starts_with("src/pins/configs/") => Some(
            "an I2C bus folder is auto-generated from its Virtual Module - its device files are renamed with the devices",
        ),
        _ => None,
    }
}

/// `pins/configs/` or a folder in it: every file there is generated, and one
/// put there by hand is pruned by the next regeneration.
pub(crate) fn in_generated_configs(folder: &str) -> bool {
    folder == "src/pins/configs" || folder.starts_with("src/pins/configs/")
}

/// If `path` (relative to the PROJECT ROOT) is an auto-generated file that must
/// NOT be moved, return a short human explanation; otherwise `None`. These are
/// rebuilt each frame from the MCU / pin configuration (see
/// `ProjectTreeState::sync_pin_files` / `sync_config_files`), so moving one
/// would be silently undone or would break the generated module tree.
///
/// Only the firmware's `src/` has generated files — a library crate's paths
/// never match, which is exactly why extracted libraries carry no restrictions.
pub(crate) fn generated_file_reason(path: &str) -> Option<&'static str> {
    if path == "src/pins/mod.rs" || path == "src/pins/configs/mod.rs" {
        return Some(
            "it's an auto-generated module file (rebuilt from your pin / peripheral configuration)",
        );
    }
    if path.starts_with("src/pins/configs/") {
        return Some(
            "it's an auto-generated peripheral init file — edit it via the MCU Configurator (Virtual Modules)",
        );
    }
    // Generated pin files sit directly under src/pins/ as `pin<…>.rs`.
    if let Some(fname) = path.strip_prefix("src/pins/") {
        if !fname.contains('/') && fname.starts_with("pin") && fname.ends_with(".rs") {
            return Some("it's an auto-generated pin file (rebuilt from your pin configuration)");
        }
    }
    None
}

/// Validate a rename typed into a tree row and build the new project-root-
/// relative path, or explain the refusal in a sentence fit for the tree notice.
///
/// A rename is a NAME, never a path: typing `sub/x.rs` used to change the
/// in-memory path while `std::fs::rename` failed on the missing directory, so
/// memory and disk silently diverged until the next full write. Moving is what
/// drag-and-drop is for, so a separator is refused rather than half-honoured.
///
/// `taken` answers whether a project-root-relative path is already used - the
/// caller feeds it BOTH files and folders, because a file may not take a
/// folder's name either (the old check looked only at files). It must match
/// CASE-INSENSITIVELY: on Windows `b.rs` and `B.rs` are one file, so an exact
/// comparison would report the name free and then let `fs::rename` overwrite
/// the other file.
pub(crate) fn validate_rename(
    old_path: &str,
    typed: &str,
    taken: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let clean = typed.trim();
    if clean.is_empty() {
        return Err("Type a name first.".to_owned());
    }
    if clean.contains('/') || clean.contains('\\') {
        return Err(format!(
            "`{clean}` looks like a path - a rename takes a NAME. Drag the file onto a folder to move it."
        ));
    }
    if clean == "." || clean == ".." || clean.contains('\0') {
        return Err(format!("`{clean}` is not a usable file name."));
    }
    if let Some(reason) = generated_file_reason(old_path) {
        return Err(format!(
            "Can't rename `{}` - {reason}.",
            base_name(old_path)
        ));
    }

    let new_path = match old_path.rfind('/') {
        Some(i) => format!("{}/{clean}", &old_path[..i]),
        None => clean.to_owned(),
    };
    if new_path == old_path {
        return Err(String::new()); // unchanged: cancel quietly, nothing to say
    }
    // Windows renames are case-insensitive, so `radar.rs` -> `Radar.rs` can
    // leave the file untouched while everything downstream believes it moved.
    // Refused on every platform so a project can't behave differently on one.
    if new_path.eq_ignore_ascii_case(old_path) {
        return Err(format!(
            "`{}` and `{clean}` differ only in capitalisation - Windows treats those as the same file.",
            base_name(old_path)
        ));
    }
    if taken(&new_path) {
        return Err(format!("`{clean}` already exists here."));
    }
    Ok(new_path)
}

const TREE_NOTICE_ID: &str = "__tree_move_notice__";

/// Show a transient amber banner (`msg`) at the top of the tree for a few
/// seconds — used to explain why a drag-drop move was refused. Stored in egui
/// temp memory (with an expiry time) so it survives across frames without a
/// dedicated state field.
pub(crate) fn set_tree_notice(ctx: &egui::Context, msg: String) {
    let expiry = ctx.input(|i| i.time) + 6.0;
    ctx.memory_mut(|m| {
        m.data
            .insert_temp(egui::Id::new(TREE_NOTICE_ID), (msg, expiry))
    });
}

fn show_tree_notice(ui: &mut egui::Ui) {
    let id = egui::Id::new(TREE_NOTICE_ID);
    let Some((msg, expiry)) = ui.memory(|m| m.data.get_temp::<(String, f64)>(id)) else {
        return;
    };
    if ui.input(|i| i.time) >= expiry {
        ui.memory_mut(|m| m.data.remove::<(String, f64)>(id));
        return;
    }
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(70, 45, 20))
        .inner_margin(egui::Margin::same(5))
        .corner_radius(egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(ph::WARNING)
                        .size(12.0)
                        .color(egui::Color32::from_rgb(240, 190, 90)),
                );
                ui.label(
                    egui::RichText::new(msg)
                        .size(10.5)
                        .color(egui::Color32::from_rgb(235, 215, 165)),
                );
            });
        });
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(250));
}

/// Apply a drag-drop move of `item` into `target_folder` (`""` = src/ root),
/// dispatching to the file or folder mover. Shared guard: refuse dropping into
/// the auto-managed `pins/configs/` (files there are pruned by the sync).
fn apply_move(
    ui: &egui::Ui,
    item: &DraggedItem,
    target_folder: &str,
    user_src_files: &mut Vec<(String, String)>,
    user_src_folders: &mut Vec<String>,
    workspace_dir: &std::path::Path,
    save_needed: &mut bool,
) {
    if target_folder == "src/pins/configs" || target_folder.starts_with("src/pins/configs/") {
        set_tree_notice(
            ui.ctx(),
            "Can't move into `pins/configs/` — it's auto-managed by the MCU Configurator."
                .to_string(),
        );
        return;
    }
    match item {
        DraggedItem::File(idx) => apply_file_move(
            ui,
            *idx,
            target_folder,
            user_src_files,
            workspace_dir,
            save_needed,
        ),
        DraggedItem::Folder(src) => apply_folder_move(
            ui,
            src,
            target_folder,
            user_src_files,
            user_src_folders,
            workspace_dir,
            save_needed,
        ),
    }
}

/// Move user file `idx` into `target_folder`: refuse auto-generated files (with
/// notice), no-op when already there, refuse on name collision. Renames on disk
/// and updates the in-memory path.
fn apply_file_move(
    ui: &egui::Ui,
    idx: usize,
    target_folder: &str,
    user_src_files: &mut [(String, String)],
    workspace_dir: &std::path::Path,
    save_needed: &mut bool,
) {
    let Some((old_path, _)) = user_src_files.get(idx) else {
        return;
    };
    let old_path = old_path.clone();
    let fname = base_name(&old_path).to_string();

    let new_path = if target_folder.is_empty() {
        fname.clone()
    } else {
        format!("{target_folder}/{fname}")
    };
    if new_path == old_path {
        return; // dropped into its current folder — nothing to do
    }
    if let Err(reason) = validate_move(&old_path, &new_path, |cand| {
        user_src_files
            .iter()
            .any(|(p, _)| p.eq_ignore_ascii_case(cand))
    }) {
        set_tree_notice(ui.ctx(), reason);
        return;
    }

    let old_dest = workspace_dir.join(&old_path);
    let new_dest = workspace_dir.join(&new_path);
    if let Some(parent) = new_dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Reported rather than swallowed, and the in-memory path is left alone on
    // failure: the old `let _ =` moved the path in memory whether or not the
    // disk agreed, and the two then drifted until the next full write.
    if let Err(e) = std::fs::rename(&old_dest, &new_dest) {
        set_tree_notice(ui.ctx(), format!("Could not move `{fname}` — {e}"));
        return;
    }
    user_src_files[idx].0 = new_path;
    *save_needed = true;
}

/// Validate a file move, or explain the refusal.
///
/// Shares the shape of [`validate_rename`] because a move IS a rename of the
/// leading directory, and every trap is the same one: a case-insensitive
/// filesystem, a destination the pin sync owns, and `src/main.rs`, which is
/// GENERATED and so never appears in `user_src_files` for `taken` to find.
pub(crate) fn validate_move(
    old_path: &str,
    new_path: &str,
    taken: impl Fn(&str) -> bool,
) -> Result<(), String> {
    let fname = base_name(old_path);
    if let Some(reason) = generated_file_reason(old_path) {
        return Err(format!("Can't move `{fname}` - {reason}."));
    }
    // Checked on the DESTINATION too. The source guard alone let a file be
    // dropped INTO `src/pins/`, where a name matching `pin*.rs` is swept out of
    // the tree by the next pin sync - the file simply disappears.
    if let Some(reason) = generated_file_reason(new_path) {
        return Err(format!("Can't move `{fname}` there - {reason}."));
    }
    // The destination FOLDER can be auto-managed even when the file name is
    // innocent: a declaration written into `src/pins/mod.rs` lands inside the
    // GENERATED markers and the next pin sync wipes it.
    if let Some(reason) = generated_folder_reason(parent_of_path(new_path)) {
        return Err(format!("Can't move `{fname}` there - {reason}."));
    }
    // Crossing a crate boundary changes which CRATE owns the module, so no
    // re-export can keep the old paths working (the re-export would need a
    // dependency edge pointing back the way it came). Refused rather than
    // half-done.
    if crate_of(old_path) != crate_of(new_path) {
        return Err(format!(
            "`{fname}` would move between crates - copy it instead, then delete the original."
        ));
    }
    // A crate root is not a module that can be re-parented, and `mod.rs` names
    // its FOLDER - moving either one unhooks a whole crate or subtree with no
    // declaration anywhere to fix up.
    let stem = fname.trim_end_matches(".rs");
    if matches!(stem, "lib" | "main") && parent_of_path(old_path).ends_with("src") {
        return Err(format!("`{fname}` is a crate root - it cannot be moved."));
    }
    if stem == "mod" {
        return Err(
            "`mod.rs` takes its name from its folder - move the folder instead.".to_owned(),
        );
    }
    if new_path.eq_ignore_ascii_case("src/main.rs") || taken(new_path) {
        return Err(format!(
            "`{fname}` already exists in that folder - rename one first."
        ));
    }
    Ok(())
}

/// The parent folder of a project-root-relative path (`""` at the root).
fn parent_of_path(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Which crate a project-root-relative path belongs to: `None` for the firmware
/// crate (`src/…`), otherwise the library directory that owns it.
fn crate_of(path: &str) -> Option<&str> {
    match path.split_once('/') {
        Some((SRC_ROOT, _)) => None,
        Some((lib, _)) => Some(lib),
        // A file at the project root belongs to the firmware crate's manifest.
        None => None,
    }
}

/// Move folder `src` (and everything under it) into `target_folder`: refuse
/// auto-generated folders, refuse moving a folder into itself/a descendant,
/// no-op when already there, refuse on collision. Renames on disk and rewrites
/// every affected folder + file path (mirrors the folder-rename logic).
fn apply_folder_move(
    ui: &egui::Ui,
    src: &str,
    target_folder: &str,
    user_src_files: &mut [(String, String)],
    user_src_folders: &mut [String],
    workspace_dir: &std::path::Path,
    save_needed: &mut bool,
) {
    let name = base_name(src).to_string();

    if let Some(reason) = generated_folder_reason(src) {
        set_tree_notice(ui.ctx(), format!("Can't move `{name}/` — {reason}."));
        return;
    }
    // Released over its own header (e.g. an in-place hold that never left the
    // row) — just a no-op, not worth a warning banner.
    if target_folder == src {
        return;
    }
    // Can't drop a folder into one of its own descendants.
    if target_folder.starts_with(&format!("{src}/")) {
        set_tree_notice(
            ui.ctx(),
            "Can't move a folder into its own subfolder.".to_string(),
        );
        return;
    }

    let new_path = if target_folder.is_empty() {
        name.clone()
    } else {
        format!("{target_folder}/{name}")
    };
    if new_path == *src {
        return; // already in that folder
    }
    // Case-insensitive, like the file paths: on Windows `Drivers` and `drivers`
    // are one directory, so an exact compare would call the name free and then
    // let `fs::rename` merge them.
    let collides = user_src_folders
        .iter()
        .any(|f| f.eq_ignore_ascii_case(&new_path))
        || user_src_files
            .iter()
            .any(|(p, _)| p.eq_ignore_ascii_case(&new_path));
    if collides {
        set_tree_notice(
            ui.ctx(),
            format!("`{name}/` already exists in that folder — rename one first."),
        );
        return;
    }
    // Same crate rule as a file move: crossing the boundary re-homes every
    // module inside the folder into another crate, and no re-export can bridge
    // that. Newly reachable now that a drop onto a library folder is applied at
    // all — it used to be discarded before this ran.
    if crate_of(src) != crate_of(&new_path) {
        set_tree_notice(
            ui.ctx(),
            format!("`{name}/` would move between crates — copy it instead."),
        );
        return;
    }

    let old_dest = workspace_dir.join(src);
    let new_dest = workspace_dir.join(&new_path);
    if let Some(parent) = new_dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Reported, not swallowed: the path rewrite below must not happen when the
    // directory did not actually move.
    if let Err(e) = std::fs::rename(&old_dest, &new_dest) {
        set_tree_notice(ui.ctx(), format!("Could not move `{name}/` — {e}"));
        return;
    }

    let old_prefix = format!("{src}/");
    for f in user_src_folders.iter_mut() {
        if *f == src {
            *f = new_path.clone();
        } else if let Some(rest) = f.strip_prefix(&old_prefix) {
            *f = format!("{new_path}/{rest}");
        }
    }
    for (p, _) in user_src_files.iter_mut() {
        if let Some(rest) = p.strip_prefix(&old_prefix) {
            *p = format!("{new_path}/{rest}");
        }
    }
    *save_needed = true;
}

/// egui id for "focus the inline new-item input on its first frame" (file / folder).
fn inline_focus_id(is_folder: bool) -> egui::Id {
    egui::Id::new(if is_folder {
        "__inline_new_folder_focus__"
    } else {
        "__inline_new_file_focus__"
    })
}

// ── Context-menu icon palette ────────────────────────────────────────────────
// Only the ICON of a menu entry carries a colour; the label keeps the theme's
// own text colour so it still brightens on hover and dims when disabled. Each
// colour marks a FAMILY of command, and where the tree already paints the thing
// the entry acts on, the menu reuses that exact colour — "Extract to library
// crate…" gets the green of the member-library PACKAGE icon, "Detach from
// workspace" the amber of a detached one — so an entry reads as "this is about
// THAT kind of item".

/// Creating a new file.
const ICON_NEW: egui::Color32 = egui::Color32::from_rgb(120, 200, 145);
/// Folders — the same gold the tree paints folder rows with.
const ICON_FOLDER: egui::Color32 = egui::Color32::from_rgb(200, 165, 70);
/// Leaves the IDE (the OS file manager).
const ICON_EXTERNAL: egui::Color32 = egui::Color32::from_rgb(110, 170, 240);
/// Neutral clipboard-ish utilities (Copy path, Duplicate).
const ICON_UTIL: egui::Color32 = egui::Color32::from_rgb(150, 165, 190);
/// Opens another view of the same file (Reference pane).
const ICON_VIEW: egui::Color32 = egui::Color32::from_rgb(110, 195, 205);
/// Workspace-member library — the green of the tree's PACKAGE icon.
const ICON_LIBRARY: egui::Color32 = egui::Color32::from_rgb(140, 190, 145);
/// Detached library — the amber of the tree's detached PACKAGE icon, its
/// name in LIBRARIES, and every file of it in the Structure diagram. One
/// constant, so "detached" is one colour wherever it is drawn.
pub(crate) const ICON_LIBRARY_DETACHED: egui::Color32 = egui::Color32::from_rgb(190, 165, 110);
/// Editing an existing item in place (Rename).
const ICON_EDIT: egui::Color32 = egui::Color32::from_rgb(175, 160, 235);
/// Destructive (Delete) — a file or folder.
const ICON_DANGER: egui::Color32 = egui::Color32::from_rgb(220, 80, 60);
/// Destructive on a library — the softer red the library menus already used.
const ICON_DANGER_LIBRARY: egui::Color32 = egui::Color32::from_rgb(230, 130, 115);

/// Text size shared by every context-menu entry.
const MENU_TEXT_SIZE: f32 = 11.5;

/// Share of the tree's height given to the project half when the user has never
/// dragged the splitter. Also the value [`AppIde`](crate::app::AppIde) falls
/// back to when no ratio was persisted — kept here, next to the layout that
/// compares against it, so the two can't drift apart.
pub const DEFAULT_SPLIT_RATIO: f32 = 0.6;

/// Gap above the LIBRARIES separator, and the height egui gives a horizontal
/// `Separator`. Both feed the section's minimum height, so they are constants
/// rather than literals at the call site — a change in one must move the other.
const LIBS_HEADER_PAD: f32 = 4.0;
const SEPARATOR_H: f32 = 6.0;

/// Height of the drag handle between the project half and LIBRARIES.
const SPLIT_HANDLE_H: f32 = 6.0;

/// Height to give the LIBRARIES section. `min_h` is its floor — the separator
/// plus the header row carrying the "+" / git-clone buttons, which must never
/// be clipped.
///
/// With no libraries and an untouched splitter the section sits exactly on that
/// floor: there is nothing to reveal, and reserving 40% of the panel for an
/// empty section would only shrink the tree. Once the user drags the handle,
/// their ratio governs in both states.
fn libs_section_h(total_h: f32, min_h: f32, split_ratio: f32, has_libs: bool) -> f32 {
    if has_libs || split_ratio != DEFAULT_SPLIT_RATIO {
        ((total_h - SPLIT_HANDLE_H) * (1.0 - split_ratio)).max(min_h)
    } else {
        min_h
    }
}

/// Label for a context-menu entry: `icon` painted in `icon_color`, the text
/// left at [`egui::Color32::PLACEHOLDER`] — egui substitutes the button's own
/// colour there when it paints the galley, so the label keeps reacting to
/// hover / enabled state exactly like an uncoloured `RichText` would.
fn menu_label(icon: &str, text: &str, icon_color: egui::Color32) -> egui::text::LayoutJob {
    menu_label_with_text_color(icon, text, icon_color, egui::Color32::PLACEHOLDER)
}

/// [`menu_label`] with the LABEL tinted as well — the deliberate exception for
/// the destructive entries, where a whole red row is the warning. Everything
/// else keeps a neutral label so the coloured glyph is what tells commands
/// apart. A fixed label colour means no hover brightening, which is fine (and
/// was already the case) for a row that must read the same at all times.
fn menu_label_danger(icon: &str, text: &str, color: egui::Color32) -> egui::text::LayoutJob {
    menu_label_with_text_color(icon, text, color, color)
}

fn menu_label_with_text_color(
    icon: &str,
    text: &str,
    icon_color: egui::Color32,
    text_color: egui::Color32,
) -> egui::text::LayoutJob {
    let font = egui::FontId::proportional(MENU_TEXT_SIZE);
    let mut job = egui::text::LayoutJob::default();
    job.append(
        icon,
        0.0,
        egui::TextFormat {
            font_id: font.clone(),
            color: icon_color,
            ..Default::default()
        },
    );
    job.append(
        text,
        4.0, // gap between glyph and label, was a literal space in the format!()
        egui::TextFormat {
            font_id: font,
            color: text_color,
            ..Default::default()
        },
    );
    job
}

/// The "Copy" entry, shared by the file / folder / library context menus.
///
/// It stages the item for [`clipboard`] — a copy that survives into ANOTHER
/// IDE window — which is why it is worded plainly as "Copy" while the entry
/// that puts a path on the system clipboard stays "Copy path".
fn copy_menu_item(
    ui: &mut egui::Ui,
    kind: clipboard::ClipKind,
    rel_path: &str,
    clip_copy: &mut Option<clipboard::CopyRequest>,
) {
    let label = match kind {
        clipboard::ClipKind::Library => "Copy library",
        _ => "Copy",
    };
    if ui
        .button(menu_label(ph::CLIPBOARD_TEXT, label, ICON_UTIL))
        .on_hover_text(format!(
            "Copy this {} so it can be pasted here or in ANOTHER IDE window \
             (text files only)",
            kind.noun()
        ))
        .clicked()
    {
        *clip_copy = Some(clipboard::CopyRequest {
            kind,
            path: rel_path.to_owned(),
        });
        ui.close();
    }
}

/// The "Paste" entry for a folder / the `src` root / the LIBRARIES header.
///
/// It names what it will paste — "Paste drivers (folder)" — because a menu
/// cannot read the system clipboard and so has to offer the LAST staged
/// payload rather than whatever the user copied most recently anywhere. Seeing
/// the name is what makes that honest. Disabled, with the reason, when nothing
/// is staged or the payload doesn't belong here.
///
/// `libraries_only` is the LIBRARIES header: a crate directory is the only
/// thing that can be pasted at the project root.
fn paste_menu_item(
    ui: &mut egui::Ui,
    target_dir: &str,
    libraries_only: bool,
    clip_paste: &mut Option<clipboard::PasteRequest>,
) {
    // Manifest only — this runs every frame the menu is open, and reading the
    // payload's files just to write a label would re-read a whole library
    // crate at frame rate.
    let id = clipboard::latest_id();
    let manifest = id.as_deref().and_then(clipboard::load_manifest);
    let is_lib = manifest
        .as_ref()
        .is_some_and(|m| m.kind == clipboard::ClipKind::Library);
    // A library is a crate directory: it belongs next to src/, never inside it.
    // Anything else belongs inside the tree, never loose at the root.
    let fits = is_lib == libraries_only;

    let label = match &manifest {
        Some(m) => format!("Paste {} ({})", m.name, m.kind.noun()),
        None => "Paste".to_owned(),
    };
    let resp = ui
        .add_enabled(
            manifest.is_some() && fits,
            egui::Button::new(menu_label(ph::CLIPBOARD, &label, ICON_UTIL)),
        )
        .on_disabled_hover_text(match (&manifest, libraries_only) {
            (None, _) => "Nothing copied yet — use Copy on a file, folder or library".to_owned(),
            (Some(m), true) => format!("{} is a {}, not a library crate", m.name, m.kind.noun()),
            (Some(m), false) => format!(
                "{} is a library crate — paste it on the LIBRARIES header instead",
                m.name
            ),
        });
    if resp.clicked() {
        *clip_paste = Some(clipboard::PasteRequest {
            target_dir: target_dir.to_owned(),
            id,
        });
        ui.close();
    }
}

/// The "Show in Explorer" + "Copy path" pair, shared by the file and folder
/// context menus and by the LIBRARIES header.
///
/// `project_dir` is the SAVED project folder — deliberately not the tree's
/// `workspace_dir`, which falls back to the temp check-workspace when nothing
/// has been saved yet. Revealing that would open `%TEMP%\embedded_ide_0_check`
/// and look like it worked, so with no saved project the entries are shown
/// disabled with the reason.
///
/// `rel` is project-root-relative, as everything in the tree is.
fn reveal_menu_items(ui: &mut egui::Ui, project_dir: Option<&std::path::Path>, rel: &str) {
    let abs = project_dir.map(|d| d.join(rel));
    let enabled = abs.is_some();
    let disabled_hint = "Save the project first (Ctrl+S) — it has no folder on disk yet";
    // The label dims itself (PLACEHOLDER colour), the icon has to be dimmed by
    // hand or a disabled entry would still show a fully saturated glyph.
    let icon = |c: egui::Color32| {
        if enabled { c } else { c.gamma_multiply(0.4) }
    };

    let reveal = ui
        .add_enabled(
            enabled,
            egui::Button::new(menu_label(
                ph::FOLDER_OPEN,
                "Show in Explorer",
                icon(ICON_EXTERNAL),
            )),
        )
        .on_disabled_hover_text(disabled_hint);
    if reveal.clicked() {
        if let Some(p) = &abs {
            // Errors are surfaced through the tooltip-free path: a failure here
            // means the file manager could not be launched at all, which is
            // rare and not worth a modal. It is logged for the Activity tab.
            if let Err(e) = crate::reveal::open(p) {
                eprintln!("[reveal] {e}");
            }
        }
        ui.close();
    }

    let copy = ui
        .add_enabled(
            enabled,
            egui::Button::new(menu_label(ph::COPY, "Copy path", icon(ICON_UTIL))),
        )
        .on_disabled_hover_text(disabled_hint);
    if copy.clicked() {
        if let Some(p) = &abs {
            // The absolute path is what you paste into a terminal.
            ui.ctx().copy_text(p.to_string_lossy().to_string());
        }
        ui.close();
    }
}

/// Arm the inline new-item input for `parent` (`""` = src/ root): set the
/// pending name + parent state and request focus next frame. Called from the
/// "New File" / "New Folder" context-menu entries.
fn begin_inline_new(
    ui: &egui::Ui,
    is_folder: bool,
    parent: &str,
    name_state: &mut Option<String>,
    parent_state: &mut Option<String>,
) {
    *name_state = Some(String::new());
    *parent_state = Some(parent.to_string());
    ui.memory_mut(|m| m.data.insert_temp(inline_focus_id(is_folder), true));
}

/// Render the inline "new file / new folder" name input as a tree row (at
/// `indent`) while its pending state targets `parent`. Enter creates the item
/// (in memory + on disk) and clears the state; Esc / focus-loss cancels. Only
/// one input is active at a time (single `name_state` Option). No-op when the
/// pending state doesn't target this `parent`.
#[allow(clippy::too_many_arguments)]
fn inline_new_item(
    ui: &mut egui::Ui,
    indent: f32,
    parent: &str,
    is_folder: bool,
    name_state: &mut Option<String>,
    parent_state: &mut Option<String>,
    user_src_files: &mut Vec<(String, String)>,
    user_src_folders: &mut Vec<String>,
    selected: &mut ProjectFileId,
    workspace_dir: &std::path::Path,
    save_needed: &mut bool,
) {
    if parent_state.as_deref() != Some(parent) || name_state.is_none() {
        return;
    }
    let mut create = false;
    let mut cancel = false;
    let focus_id = inline_focus_id(is_folder);
    // Only treat a focus-loss as "cancel" AFTER the input has actually held
    // keyboard focus for a frame — on the frame it appears (before
    // `request_focus` takes effect) a stray interaction elsewhere would fire
    // `lost_focus()` and instantly close it (the "flickers open then vanishes"
    // bug).
    let had_focus_id = focus_id.with("had_focus");
    ui.horizontal(|ui| {
        ui.add_space(indent);
        let icon = if is_folder { ph::FOLDER } else { ph::FILE };
        ui.label(
            egui::RichText::new(icon)
                .size(11.5)
                .color(egui::Color32::from_rgb(150, 180, 240)),
        );
        if let Some(name) = name_state.as_mut() {
            let resp = ui.add(
                egui::TextEdit::singleline(name)
                    .desired_width(ui.available_width())
                    .hint_text(if is_folder {
                        "new folder"
                    } else {
                        "new_file.rs"
                    }),
            );
            if ui.memory(|m| m.data.get_temp::<bool>(focus_id).unwrap_or(true)) {
                resp.request_focus();
                ui.memory_mut(|m| m.data.insert_temp(focus_id, false));
            }
            let had_focus = ui.memory(|m| m.data.get_temp::<bool>(had_focus_id).unwrap_or(false));
            if resp.has_focus() {
                ui.memory_mut(|m| m.data.insert_temp(had_focus_id, true));
            }
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                create = true;
            } else if ui.input(|i| i.key_pressed(egui::Key::Escape))
                || (had_focus && resp.lost_focus())
            {
                cancel = true;
            }
        }
    });

    if create {
        if let Some(name) = name_state.take() {
            let clean = name.trim().to_string();
            if !clean.is_empty() {
                let full = if parent.is_empty() {
                    clean.clone()
                } else {
                    format!("{parent}/{clean}")
                };
                let collides = user_src_folders.iter().any(|f| f == &full)
                    || user_src_files.iter().any(|(p, _)| p == &full);
                if collides {
                    set_tree_notice(ui.ctx(), format!("`{clean}` already exists here."));
                } else if is_folder {
                    let dest = workspace_dir.join(&full);
                    let _ = std::fs::create_dir_all(&dest);
                    user_src_folders.push(full);
                    *save_needed = true;
                } else {
                    let dest = workspace_dir.join(&full);
                    if let Some(p) = dest.parent() {
                        let _ = std::fs::create_dir_all(p);
                    }
                    let _ = std::fs::write(&dest, "// New file\n");
                    user_src_files.push((full, "// New file\n".to_string()));
                    *selected = ProjectFileId::UserFile(user_src_files.len() - 1);
                    *save_needed = true;
                }
            }
        }
        *parent_state = None;
        ui.memory_mut(|m| m.data.remove::<bool>(had_focus_id));
    } else if cancel {
        *name_state = None;
        *parent_state = None;
        ui.memory_mut(|m| m.data.remove::<bool>(had_focus_id));
    }
}

/// Build a hierarchical tree from user_src_files and user_src_folders.
fn build_tree(
    user_src_files: &[(String, String)],
    user_src_folders: &[String],
) -> BTreeMap<String, TreeNode> {
    let mut root: BTreeMap<String, TreeNode> = BTreeMap::new();

    // First, ensure all folders exist in the tree
    for folder in user_src_folders {
        insert_folder_path(&mut root, folder);
    }

    // Then, insert all files
    for (i, (path, _)) in user_src_files.iter().enumerate() {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.is_empty() {
            continue;
        }
        insert_file_path(&mut root, &parts, i);
    }

    root
}

/// Helper: insert a folder path into the tree (recursive).
fn insert_folder_path(root: &mut BTreeMap<String, TreeNode>, path: &str) {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    insert_folder_path_recursive(root, &parts);
}

fn insert_folder_path_recursive(current: &mut BTreeMap<String, TreeNode>, parts: &[&str]) {
    if parts.is_empty() {
        return;
    }
    let part = parts[0];
    let rest = &parts[1..];
    let node = current
        .entry(part.to_string())
        .or_insert_with(|| TreeNode::Folder(BTreeMap::new()));
    if let TreeNode::Folder(children) = node {
        insert_folder_path_recursive(children, rest);
    }
}

/// Helper: insert a file path into the tree (recursive).
fn insert_file_path(root: &mut BTreeMap<String, TreeNode>, parts: &[&str], file_idx: usize) {
    if parts.is_empty() {
        return;
    }
    insert_file_path_recursive(root, parts, file_idx);
}

fn insert_file_path_recursive(
    current: &mut BTreeMap<String, TreeNode>,
    parts: &[&str],
    file_idx: usize,
) {
    if parts.is_empty() {
        return;
    }
    if parts.len() == 1 {
        // Last part is the filename
        current.insert(parts[0].to_string(), TreeNode::File(file_idx));
    } else {
        // Navigate deeper
        let part = parts[0];
        let rest = &parts[1..];
        let node = current
            .entry(part.to_string())
            .or_insert_with(|| TreeNode::Folder(BTreeMap::new()));
        if let TreeNode::Folder(children) = node {
            insert_file_path_recursive(children, rest, file_idx);
        }
    }
}

/// Which files carry a diagnostic badge: `(has errors, has warnings)` per
/// project-root-relative path, merged from the last Cargo Check result and
/// rust-analyzer's live map. A file with neither has no entry.
///
/// The caller fills it under two short locks, one at a time, so the tree
/// renders holding neither: the LSP reader thread and the flush worker lock
/// `LspState` too, and used to wait out a whole tree render.
#[derive(Default)]
pub struct DiagBadges(std::collections::HashMap<String, (bool, bool)>);

impl DiagBadges {
    /// Adds a Cargo Check result, as `has_errors_in` / `has_warnings_in` see it.
    pub fn add_build(&mut self, result: &build::BuildResult) {
        for d in &result.diagnostics {
            if let Some(file) = &d.file {
                self.mark(file, d.is_error(), d.is_warning());
            }
        }
    }

    /// Adds rust-analyzer's diagnostics, as `error_count_for(f) > 0` /
    /// `warning_count_for(f) > 0` see them: no `flycheck_stale` filter, since
    /// those counts apply none.
    pub fn add_lsp(&mut self, lsp: &lsp::LspState) {
        for (file, ds) in &lsp.diagnostics {
            let err = ds.iter().any(|d| d.severity.is_error());
            let warn = ds.iter().any(|d| d.severity.is_warning());
            self.mark(file, err, warn);
        }
    }

    fn mark(&mut self, file: &str, err: bool, warn: bool) {
        if !(err || warn) {
            return;
        }
        // Looked up first so a file already marked costs no key allocation.
        if let Some(flags) = self.0.get_mut(file) {
            flags.0 |= err;
            flags.1 |= warn;
        } else {
            self.0.insert(file.to_owned(), (err, warn));
        }
    }

    pub fn errors_in(&self, file: &str) -> bool {
        self.0.get(file).is_some_and(|&(err, _)| err)
    }

    pub fn warnings_in(&self, file: &str) -> bool {
        self.0.get(file).is_some_and(|&(_, warn)| warn)
    }
}

/// Display the project tree panel (left side of the IDE).
pub fn show_project_tree(
    ui: &mut egui::Ui,
    pkg_name: &str,
    toolchain: &ToolchainKind,
    selected: &mut ProjectFileId,
    // Error / warning badges of every file (cargo + rust-analyzer).
    badges: &DiagBadges,
    user_src_files: &mut Vec<(String, String)>,
    user_src_folders: &mut Vec<String>,
    new_src_name: &mut Option<String>,
    new_src_folder_name: &mut Option<String>,
    new_file_parent_folder: &mut Option<String>,
    new_folder_parent_folder: &mut Option<String>,
    _new_file_in_folder: &mut Option<(String, String)>,
    renaming_file: &mut Option<(usize, String)>,
    renaming_folder: &mut Option<(String, String)>,
    workspace_dir: &std::path::Path,
    // The SAVED project folder, or `None` before the first save. Distinct from
    // `workspace_dir`, which falls back to the temp check-workspace — revealing
    // that would silently open the wrong folder.
    project_dir: Option<&std::path::Path>,
    save_needed: &mut bool,
    // Folder the user asked to turn into a sibling library crate (project-root
    // relative); the caller opens the Extract dialog.
    extract_folder: &mut Option<String>,
    // Directory names of the `[workspace] members` — the extracted library
    // crates, each rendered as its own collapsible section below the project.
    lib_crates: &[String],
    // Libraries the root builds through a `path` dependency without listing
    // them as members: drawn with the members, but without Rename, Detach or
    // Delete - those edit the members list and the IDE's own
    // `[dependencies.<dir>]` table, and this dependency line is the user's.
    path_dep_libs: &[String],
    // Cloned libraries NOT yet in the workspace (own a Cargo.toml, not members);
    // shown in a DETACHED subsection with an "Add to workspace" action.
    detached_libs: &[String],
    // The detached lib (if any) whose "Add to workspace" cargo-metadata check is
    // currently running — its row spins and all add buttons disable.
    ws_add_pending: Option<&str>,
    // Share of the height given to the project half; dragged via the splitter.
    split_ratio: &mut f32,
    // Set when the LIBRARIES "+" button is clicked; the caller opens the dialog.
    new_library: &mut bool,
    // Set when the LIBRARIES "clone from git" button is clicked.
    clone_library: &mut bool,
    // Set when the project-header "Clone project" button is clicked.
    clone_project: &mut bool,
    // `(crate dir, is_rename)` when a library's pen / trash icon is clicked;
    // the caller opens the confirmation dialog.
    library_action: &mut Option<(String, bool)>,
    // Set when "Publish…" is picked on a library; the app opens the dialog.
    publish_lib: &mut Option<String>,
    // Set to a DETACHED lib's dir when its "Add to workspace" button is clicked.
    add_to_workspace: &mut Option<String>,
    // Set to a member lib's dir when its "Detach" button is clicked.
    detach_from_workspace: &mut Option<String>,
    // Project-root-relative path of a file to open READ-ONLY in the Reference tab.
    open_reference: &mut Option<String>,
    // Cross-instance copy/paste (see `project_tree::clipboard`). Raised here,
    // applied by the app — only it can read and mutate `user_src_files` as a
    // whole.
    clip_copy: &mut Option<clipboard::CopyRequest>,
    clip_paste: &mut Option<clipboard::PasteRequest>,
    // Set when a file row carrying the RED error badge is clicked; the app
    // scrolls the editor to that file's first error instead of its top.
    goto_error: &mut Option<ProjectFileId>,
    move_to_folder: &mut Option<String>,
    // A validated file rename for the app to perform (see `RenameRequest`).
    rename_request: &mut Option<RenameRequest>,
) {
    // Diagnostic status of the user files (cargo + rust-analyzer), so
    // `user_file_row` can flag them: `true` = has ERRORS (red icon), `false` =
    // only WARNINGS (amber icon); absent = clean. The fixed project files get
    // this in `file_row`. Keyed the same as the diagnostics / Structure's
    // `node_errors`.
    let file_diags: std::collections::HashMap<String, bool> = user_src_files
        .iter()
        .filter_map(|(rel, _)| {
            if badges.errors_in(rel) {
                return Some((rel.clone(), true));
            }
            badges.warnings_in(rel).then(|| (rel.clone(), false))
        })
        .collect();

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("package: {pkg_name}"))
                .size(12.0)
                .strong()
                .color(egui::Color32::LIGHT_YELLOW),
        );
        // "Clone project" — a snapshot of the whole project + libraries into a
        // new folder (e.g. before switching the Runtime, which regenerates code).
        // Needs a saved project on disk.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    project_dir.is_some(),
                    egui::Button::new(
                        egui::RichText::new(format!("{} Clone", ph::COPY)).size(10.5),
                    )
                    .frame(false),
                )
                .on_hover_text(
                    "Duplicate this project — every file plus its libraries — into a new \
                     folder. A snapshot before changing the Runtime, which regenerates \
                     different code. Copies the SAVED project (skips target/ and .git).",
                )
                .on_disabled_hover_text("Save the project first.")
                .clicked()
            {
                *clone_project = true;
            }
        });
    });
    // Transient "can't move" banner from a refused drag-drop (auto-cleared).
    show_tree_notice(ui);
    ui.add_space(2.0);

    // ── Split: project above, LIBRARIES below ────────────────────────────────
    let has_libs = !detached_libs.is_empty()
        || lib_crates.iter().chain(path_dep_libs).any(|c| {
            user_src_files
                .iter()
                .any(|(p, _)| p.starts_with(&format!("{c}/")))
        });
    let total_h = ui.available_height();

    // Floor for the LIBRARIES section: its separator and header row must ALWAYS
    // be fully visible, because that row carries the "+" and git-clone buttons
    // — the only way to get a FIRST library. Derived from the real spacing
    // instead of a guessed multiple of it: the previous
    // `interact_size.y + 3 * item_spacing.y` left out the separator's own
    // height, so with no libraries the header rendered clipped against the
    // bottom edge of the panel.
    let libs_min_h = {
        let sp = ui.spacing();
        LIBS_HEADER_PAD + SEPARATOR_H + sp.item_spacing.y * 2.0 + sp.interact_size.y
    };
    let libs_h = libs_section_h(total_h, libs_min_h, *split_ratio, has_libs);
    let project_h = (total_h - SPLIT_HANDLE_H - libs_h).max(0.0);
    // Drag-to-scroll would swallow the tree's own hold-to-drag file move —
    // the two gestures are the same input. egui 0.36 turns it on only for a
    // touch screen (`DragScroll::OnTouch`), so `Never` still matters there.
    let no_drag_scroll = egui::scroll_area::ScrollSource {
        drag: egui::scroll_area::DragScroll::Never,
        ..Default::default()
    };

    // Collected during the tree render below; an item dragged onto a folder sets
    // `(dragged_item, target_folder_rel_to_src)` — applied after the tree closure
    // so it doesn't clash with the `&mut` borrows used for rendering.
    let mut move_request: Option<(DraggedItem, String)> = None;
    // File-row signals, shared by the `src/` section AND every library-crate
    // section below it, applied once after all of them have rendered — the
    // indices they carry are only valid while `user_src_files` is unchanged.
    let mut to_delete: Option<usize> = None;
    let mut to_duplicate: Option<usize> = None;
    let mut do_rename_file: Option<usize> = None;
    let mut cancel_rename_file = false;

    // Paths are project-root-relative, so the full tree has `src` and every
    // library crate as top-level nodes. Built once (owned — it does not borrow
    // `user_src_files`); each section renders its own subtree.
    let full_tree = build_tree(user_src_files, user_src_folders);
    let subtree_of = |name: &str| match full_tree.get(name) {
        Some(TreeNode::Folder(children)) => children.clone(),
        _ => BTreeMap::new(),
    };

    // The project's own files scroll independently of the libraries below.
    egui::ScrollArea::vertical()
        .id_salt("tree_project_scroll")
        .max_height(project_h)
        .scroll_source(no_drag_scroll)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // .cargo/  — fixed folder, no context menu → dark-red + bold.
            egui::CollapsingHeader::new(
                egui::RichText::new(".cargo/")
                    .size(11.5)
                    .monospace()
                    .strong()
                    .color(egui::Color32::from_rgb(100, 50, 50)),
            )
            .default_open(true)
            .show(ui, |ui| {
                file_row(
                    ui,
                    8.0,
                    "config.toml",
                    ProjectFileId::CargoConfig,
                    selected,
                    open_reference,
                    goto_error,
                    project_dir,
                    badges,
                );
            });

            // src/  — fixed root, cannot be renamed/deleted → muted-red + bold.
            let src_ch = egui::CollapsingHeader::new(
                egui::RichText::new("src/")
                    .size(11.5)
                    .monospace()
                    .strong()
                    .color(egui::Color32::from_rgb(200, 100, 100)),
            )
            .default_open(true)
            .show(ui, |ui| {
                file_row(
                    ui,
                    8.0,
                    "main.rs",
                    ProjectFileId::MainRs,
                    selected,
                    open_reference,
                    goto_error,
                    project_dir,
                    badges,
                );

                // Inline "new file / new folder" input at the src/ root, rendered right
                // under main.rs where the item will be added.
                inline_new_item(
                    ui,
                    8.0,
                    SRC_ROOT,
                    false,
                    new_src_name,
                    new_file_parent_folder,
                    user_src_files,
                    user_src_folders,
                    selected,
                    workspace_dir,
                    save_needed,
                );
                inline_new_item(
                    ui,
                    8.0,
                    SRC_ROOT,
                    true,
                    new_src_folder_name,
                    new_folder_parent_folder,
                    user_src_files,
                    user_src_folders,
                    selected,
                    workspace_dir,
                    save_needed,
                );

                // This header IS `src/`, so it renders that subtree; library crates get
                // their own sections further down.
                let tree = subtree_of(SRC_ROOT);

                // Recursively render the tree
                render_tree_node(
                    ui,
                    &tree,
                    user_src_files,
                    user_src_folders,
                    selected,
                    8.0,
                    renaming_file,
                    &mut do_rename_file,
                    &mut cancel_rename_file,
                    &mut to_delete,
                    &mut to_duplicate,
                    open_reference,
                    clip_copy,
                    clip_paste,
                    goto_error,
                    move_to_folder,
                    renaming_folder,
                    workspace_dir,
                    project_dir,
                    save_needed,
                    new_src_name,
                    new_src_folder_name,
                    new_file_parent_folder,
                    new_folder_parent_folder,
                    SRC_ROOT, // this header IS src/, so children hang off it
                    &mut move_request,
                    extract_folder,
                    &file_diags,
                );
            });

            // Hover: tint + pointing hand — the src/ header is a drop target and hosts
            // the New File / New Folder context menu.
            row_hover_feedback(
                ui,
                src_ch.header_response.rect,
                src_ch.header_response.contains_pointer(),
            );

            // Dropping a dragged item on the `src/` header moves it to the src/ root.
            if src_ch
                .header_response
                .dnd_hover_payload::<DraggedItem>()
                .is_some()
            {
                ui.painter().rect_stroke(
                    src_ch.header_response.rect,
                    3.0,
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(120, 170, 240)),
                    egui::StrokeKind::Inside,
                );
            }
            if let Some(p) = src_ch.header_response.dnd_release_payload::<DraggedItem>() {
                move_request = Some(((*p).clone(), SRC_ROOT.to_owned()));
            }

            // (The move itself is applied AFTER the libraries section — see the
            // note at the single `apply_move` call site below.)

            src_ch.header_response.context_menu(|ui| {
                if ui
                    .button(menu_label(ph::FILE_PLUS, "New File", ICON_NEW))
                    .clicked()
                {
                    // SRC_ROOT, not "": `inline_new_item` renders only while the
                    // pending parent EQUALS the parent it is called with, and
                    // the src/ inputs above are called with SRC_ROOT — so a ""
                    // here armed a state nothing would ever draw, and New File
                    // / New Folder on src/ looked like dead menu entries.
                    // Paths are project-root-relative, so "" would also have
                    // built `foo.rs` at the project root rather than in src/.
                    begin_inline_new(ui, false, SRC_ROOT, new_src_name, new_file_parent_folder);
                    *new_src_folder_name = None;
                    *new_folder_parent_folder = None;
                    ui.close();
                }
                if ui
                    .button(menu_label(ph::FOLDER_PLUS, "New Folder", ICON_FOLDER))
                    .clicked()
                {
                    begin_inline_new(
                        ui,
                        true,
                        SRC_ROOT,
                        new_src_folder_name,
                        new_folder_parent_folder,
                    );
                    *new_src_name = None;
                    *new_file_parent_folder = None;
                    ui.close();
                }
                paste_menu_item(ui, SRC_ROOT, false, clip_paste);
                ui.separator();
                // The src/ ROOT of the firmware crate.
                reveal_menu_items(ui, project_dir, SRC_ROOT);
                // Git actions live in the bottom "Git" tab (commit/push/pull), not
                // here — kept out of the tree menu on purpose (moved 2026-07-07).
            });

            ui.add_space(2.0);
            file_row(
                ui,
                4.0,
                ".gitignore",
                ProjectFileId::GitIgnore,
                selected,
                open_reference,
                goto_error,
                project_dir,
                badges,
            );
            if *toolchain == ToolchainKind::RustEmbedded {
                file_row(
                    ui,
                    4.0,
                    "build.rs",
                    ProjectFileId::BuildRs,
                    selected,
                    open_reference,
                    goto_error,
                    project_dir,
                    badges,
                );
            }
            file_row(
                ui,
                4.0,
                "Cargo.toml",
                ProjectFileId::CargoToml,
                selected,
                open_reference,
                goto_error,
                project_dir,
                badges,
            );
            if *toolchain == ToolchainKind::RustEmbedded {
                file_row(
                    ui,
                    4.0,
                    "memory.x",
                    ProjectFileId::MemoryX,
                    selected,
                    open_reference,
                    goto_error,
                    project_dir,
                    badges,
                );
            }
        });

    // ── Splitter ─────────────────────────────────────────────────────────────
    // Same pattern as the bottom diagnostics panel's handle: allocate a thin
    // strip, interact with it for drags, and paint a line plus grip dots.
    //
    // Rendered even with NO libraries: the section still has a header worth
    // pulling up, and a handle that silently disappears reads as a broken
    // panel rather than an empty one.
    {
        let (handle_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), SPLIT_HANDLE_H),
            egui::Sense::hover(),
        );
        let drag = ui.interact(
            handle_rect,
            egui::Id::new("tree_split_handle"),
            egui::Sense::drag(),
        );
        let line_color = if drag.hovered() || drag.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            egui::Color32::from_rgb(100, 140, 200)
        } else {
            egui::Color32::from_gray(65)
        };
        let mid_y = handle_rect.center().y;
        ui.painter().hline(
            handle_rect.x_range(),
            mid_y,
            egui::Stroke::new(1.0_f32, line_color),
        );
        for dx in [-6.0_f32, 0.0, 6.0] {
            ui.painter().circle_filled(
                egui::pos2(handle_rect.center().x + dx, mid_y),
                1.5,
                line_color,
            );
        }
        if drag.dragged() && total_h > 0.0 {
            // Clamped so neither half can be dragged away entirely.
            *split_ratio = (*split_ratio + drag.drag_delta().y / total_h).clamp(0.15, 0.85);
        }
    }

    // ── Library crates ───────────────────────────────────────────────────────
    // Crates extracted out of the project (see `extract_crate`) live NEXT TO
    // src/ and are listed as `[workspace] members`. They render exactly like
    // the project's own files — delete / rename / new file / new folder all
    // work — because no generated-path guard can match a path outside `src/`.
    let libs: Vec<String> = lib_crates
        .iter()
        .chain(path_dep_libs)
        .filter(|c| full_tree.contains_key(c.as_str()))
        .cloned()
        .collect();
    ui.add_space(LIBS_HEADER_PAD);
    ui.separator();
    let libs_header = ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("LIBRARIES")
                .size(9.0)
                .color(egui::Color32::from_gray(110)),
        );
        // right_to_left: the first widget added lands furthest right.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(egui::RichText::new(ph::PLUS).size(11.0)).frame(false))
                .on_hover_text(
                    "New library crate (Cargo.toml + src/lib.rs, wired into the workspace)",
                )
                .clicked()
            {
                *new_library = true;
            }
            if ui
                .add(egui::Button::new(egui::RichText::new(ph::GIT_FORK).size(11.0)).frame(false))
                .on_hover_text(
                    "Clone a library from git into the project (its own repo, gitignored). \
                     It arrives DETACHED — promote it with \"Add to workspace\" when ready.",
                )
                .clicked()
            {
                *clone_library = true;
            }
        });
    });
    // A pasted library crate lands at the PROJECT ROOT, next to src/ — the only
    // place a workspace member can live. The header row is the drop point for
    // it, and the only one available when the section is still empty.
    libs_header
        .response
        .interact(egui::Sense::click())
        .context_menu(|ui| {
            paste_menu_item(ui, "", true, clip_paste);
            ui.separator();
            reveal_menu_items(ui, project_dir, "");
        });

    egui::ScrollArea::vertical()
        .id_salt("tree_libraries_scroll")
        .scroll_source(no_drag_scroll)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for lib in &libs {
                let via_path = path_dep_libs.contains(lib);
                let id = ui.make_persistent_id(("lib_crate_section", lib.as_str()));
                let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    true,
                );
                let open = state.is_open();

                // Custom header so the caret sits on the RIGHT (`mw_radar ^`).
                // `CollapsingState::show_header` would paint egui's own arrow on the
                // left; and in a right_to_left layout the FIRST widget added is the
                // rightmost, so the caret goes in before anything else.
                let mut toggle = false;
                let header = ui.horizontal(|ui| {
                    // The icon and the name are SEPARATE labels on purpose: phosphor is
                    // registered only in the Proportional family (`add_to_fonts`), so a
                    // glyph inside a `.monospace()` RichText renders as a tofu square.
                    // Same split every other tree row uses.
                    ui.label(
                        egui::RichText::new(ph::PACKAGE)
                            .size(11.5)
                            .color(egui::Color32::from_rgb(140, 190, 145)),
                    );
                    let name_resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(lib)
                                .size(11.5)
                                .monospace()
                                .strong()
                                .color(egui::Color32::from_rgb(140, 190, 145)),
                        )
                        .selectable(false)
                        .sense(egui::Sense::click()),
                    );
                    if via_path {
                        ui.label(
                            egui::RichText::new("path dependency")
                                .size(9.0)
                                .color(egui::Color32::from_gray(120)),
                        )
                        .on_hover_text(
                            "Loaded with the firmware through a `path` dependency in \
                             Cargo.toml, not a `[workspace] members` entry. To take it \
                             out, delete that dependency line.",
                        );
                    }
                    // Only the expand/collapse caret stays inline — every action (rename,
                    // detach, delete, open folder) lives in the right-click menu below.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let icon = if open {
                            ph::CARET_DOWN
                        } else {
                            ph::CARET_DOUBLE_UP
                        };
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new(icon).size(11.0))
                                    .frame(false),
                            )
                            .on_hover_text(if open {
                                "Collapse this library"
                            } else {
                                "Expand this library"
                            })
                            .clicked()
                        {
                            toggle = true;
                        }
                    });
                    name_resp
                });
                if toggle || header.inner.clicked() {
                    state.set_open(!open);
                }
                row_hover_feedback(ui, header.response.rect, header.response.contains_pointer());

                state.show_body_indented(&header.response, ui, |ui| {
                    inline_new_item(
                        ui,
                        8.0,
                        lib,
                        false,
                        new_src_name,
                        new_file_parent_folder,
                        user_src_files,
                        user_src_folders,
                        selected,
                        workspace_dir,
                        save_needed,
                    );
                    inline_new_item(
                        ui,
                        8.0,
                        lib,
                        true,
                        new_src_folder_name,
                        new_folder_parent_folder,
                        user_src_files,
                        user_src_folders,
                        selected,
                        workspace_dir,
                        save_needed,
                    );
                    let tree = subtree_of(lib);
                    render_tree_node(
                        ui,
                        &tree,
                        user_src_files,
                        user_src_folders,
                        selected,
                        8.0,
                        renaming_file,
                        &mut do_rename_file,
                        &mut cancel_rename_file,
                        &mut to_delete,
                        &mut to_duplicate,
                        open_reference,
                        clip_copy,
                        clip_paste,
                        goto_error,
                        move_to_folder,
                        renaming_folder,
                        workspace_dir,
                        project_dir,
                        save_needed,
                        new_src_name,
                        new_src_folder_name,
                        new_file_parent_folder,
                        new_folder_parent_folder,
                        lib,
                        &mut move_request,
                        extract_folder,
                        &file_diags,
                    );
                });
                state.store(ui.ctx());

                // Attach to the NAME label (`header.inner`), not the `horizontal`
                // container: the label senses clicks and sits on top, so a right-click on
                // the name never reached the container's menu (it looked like nothing
                // happened). The label reliably fires `secondary_clicked`.
                header.inner.context_menu(|ui| {
                    if ui
                        .button(menu_label(ph::FILE_PLUS, "New File", ICON_NEW))
                        .clicked()
                    {
                        begin_inline_new(ui, false, lib, new_src_name, new_file_parent_folder);
                        *new_src_folder_name = None;
                        *new_folder_parent_folder = None;
                        ui.close();
                    }
                    if ui
                        .button(menu_label(ph::FOLDER_PLUS, "New Folder", ICON_FOLDER))
                        .clicked()
                    {
                        begin_inline_new(
                            ui,
                            true,
                            lib,
                            new_src_folder_name,
                            new_folder_parent_folder,
                        );
                        *new_src_name = None;
                        *new_file_parent_folder = None;
                        ui.close();
                    }
                    ui.separator();
                    copy_menu_item(ui, clipboard::ClipKind::Library, lib, clip_copy);
                    if ui
                        .button(menu_label(ph::UPLOAD_SIMPLE, "Publish…", ICON_LIBRARY))
                        .on_hover_text(
                            "Check what a registry would refuse, fill in the missing Cargo.toml \
                             fields, rehearse with a dry run, and publish.",
                        )
                        .clicked()
                    {
                        *publish_lib = Some(lib.clone());
                        ui.close();
                    }
                    ui.separator();
                    if via_path {
                        ui.label(
                            egui::RichText::new(
                                "Loaded through a path dependency.\n\
                                 Delete that line in Cargo.toml to detach it.",
                            )
                            .size(10.0)
                            .color(egui::Color32::from_gray(140)),
                        );
                    } else {
                    if ui
                        .button(menu_label(ph::PENCIL_SIMPLE, "Rename library…", ICON_EDIT))
                        .clicked()
                    {
                        *library_action = Some((lib.clone(), true));
                        ui.close();
                    }
                    if ui
                        .button(menu_label(
                            ph::LINK_BREAK,
                            "Detach from workspace",
                            ICON_LIBRARY_DETACHED,
                        ))
                        .on_hover_text(
                            "Remove it from [workspace] members + any path dependency (keeps the \
                     files) — use this if it broke rust-analyzer / the build.",
                        )
                        .clicked()
                    {
                        *detach_from_workspace = Some(lib.clone());
                        ui.close();
                    }
                    if ui
                        .button(menu_label_danger(
                            ph::TRASH,
                            "Delete library…",
                            ICON_DANGER_LIBRARY,
                        ))
                        .clicked()
                    {
                        *library_action = Some((lib.clone(), false));
                        ui.close();
                    }
                    }
                    ui.separator();
                    reveal_menu_items(ui, project_dir, lib);
                });
            }

            // ── Detached libraries (cloned, not yet in the workspace) ────────────────
            // They own a Cargo.toml but are NOT `[workspace] members`, so cargo (and
            // rust-analyzer) ignore them. "Add to workspace" promotes one after a
            // cargo-metadata pre-check, so an incompatible crate can't break RA.
            if !detached_libs.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("NOT IN WORKSPACE")
                        .size(9.0)
                        .color(egui::Color32::from_rgb(180, 150, 90)),
                );
                ui.label(
                    egui::RichText::new(
                        "Cloned libraries — Add to workspace to build + analyze them.",
                    )
                    .size(9.5)
                    .color(egui::Color32::from_gray(120)),
                );
                for lib in detached_libs {
                    let id = ui.make_persistent_id(("detached_lib_section", lib.as_str()));
                    let mut state =
                        egui::collapsing_header::CollapsingState::load_with_default_open(
                            ui.ctx(),
                            id,
                            false,
                        );
                    let open = state.is_open();
                    let mut toggle = false;
                    let amber = ICON_LIBRARY_DETACHED;
                    let header = ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(ph::PACKAGE).size(11.5).color(amber));
                        let name_resp = ui.add(
                            egui::Label::new(
                                egui::RichText::new(lib).size(11.5).monospace().color(amber),
                            )
                            .selectable(false)
                            .sense(egui::Sense::click()),
                        );
                        // Only the caret stays inline (plus a transient spinner while its
                        // "Add to workspace" check runs — a status, not a button). Every
                        // action lives in the right-click menu below.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let icon = if open {
                                ph::CARET_DOWN
                            } else {
                                ph::CARET_DOUBLE_UP
                            };
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new(icon).size(11.0))
                                        .frame(false),
                                )
                                .on_hover_text(if open { "Collapse" } else { "Expand" })
                                .clicked()
                            {
                                toggle = true;
                            }
                            if ws_add_pending == Some(lib.as_str()) {
                                ui.add(egui::Spinner::new().size(12.0))
                                    .on_hover_text("Checking whether it can join the workspace…");
                            }
                        });
                        name_resp
                    });
                    if toggle || header.inner.clicked() {
                        state.set_open(!open);
                    }
                    row_hover_feedback(
                        ui,
                        header.response.rect,
                        header.response.contains_pointer(),
                    );
                    state.show_body_indented(&header.response, ui, |ui| {
                        let tree = subtree_of(lib);
                        render_tree_node(
                            ui,
                            &tree,
                            user_src_files,
                            user_src_folders,
                            selected,
                            8.0,
                            renaming_file,
                            &mut do_rename_file,
                            &mut cancel_rename_file,
                            &mut to_delete,
                            &mut to_duplicate,
                            open_reference,
                            clip_copy,
                            clip_paste,
                            goto_error,
                            move_to_folder,
                            renaming_folder,
                            workspace_dir,
                            project_dir,
                            save_needed,
                            new_src_name,
                            new_src_folder_name,
                            new_file_parent_folder,
                            new_folder_parent_folder,
                            lib,
                            &mut move_request,
                            extract_folder,
                            &file_diags,
                        );
                    });
                    state.store(ui.ctx());

                    // Attach to the NAME label (see the member row above) so a right-
                    // click on the library name opens the menu.
                    header.inner.context_menu(|ui| {
                        let checking = ws_add_pending == Some(lib.as_str());
                        if ui
                            .add_enabled(
                                ws_add_pending.is_none(),
                                egui::Button::new(menu_label(
                                    ph::PLUGS_CONNECTED,
                                    "Add to workspace",
                                    // The green of a workspace-MEMBER library:
                                    // the state this entry moves it into.
                                    ICON_LIBRARY,
                                )),
                            )
                            .on_hover_text(
                                "Wire it in as a [workspace] member + path dependency (runs a \
                         cargo-metadata check first).",
                            )
                            .on_disabled_hover_text(if checking {
                                "Checking this library…"
                            } else {
                                "A workspace check is already running…"
                            })
                            .clicked()
                        {
                            *add_to_workspace = Some(lib.clone());
                            ui.close();
                        }
                        ui.separator();
                        copy_menu_item(ui, clipboard::ClipKind::Library, lib, clip_copy);
                        if ui
                            .button(menu_label(ph::UPLOAD_SIMPLE, "Publish…", ICON_LIBRARY))
                            .on_hover_text(
                                "Check what a registry would refuse, fill in the missing Cargo.toml \
                                 fields, rehearse with a dry run, and publish.",
                            )
                            .clicked()
                        {
                            *publish_lib = Some(lib.clone());
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .button(menu_label(ph::PENCIL_SIMPLE, "Rename library…", ICON_EDIT))
                            .clicked()
                        {
                            *library_action = Some((lib.clone(), true));
                            ui.close();
                        }
                        if ui
                            .button(menu_label_danger(
                                ph::TRASH,
                                "Delete library…",
                                ICON_DANGER_LIBRARY,
                            ))
                            .clicked()
                        {
                            *library_action = Some((lib.clone(), false));
                            ui.close();
                        }
                        ui.separator();
                        reveal_menu_items(ui, project_dir, lib);
                    });
                }
            }
        });

    // ── Apply a drag-drop move ───────────────────────────────────────────────
    // Here, and NOT inside the project scroll area where it used to sit: the
    // libraries section renders after that closure, so a drop onto a folder
    // inside a library crate set `move_request` when its only reader had
    // already run. The request then died with the frame — no move, no notice,
    // no way to tell it had been ignored.
    if let Some((item, target)) = move_request.take() {
        apply_move(
            ui,
            &item,
            &target,
            user_src_files,
            user_src_folders,
            workspace_dir,
            save_needed,
        );
    }

    // ── Apply the file-row signals ───────────────────────────────────────────
    // Once, after EVERY section: the indices are positions in `user_src_files`,
    // and mutating it mid-render would invalidate the ones a later section
    // produced.

    // Duplication — copy under the next free `<stem>_<n>` name in the same
    // folder, and select the new file so it is obvious what happened.
    if let Some(idx) = to_duplicate {
        let (src_path, content) = user_src_files[idx].clone();
        let new_path = crate::project_tree::logic::duplicate_path(&src_path, |cand| {
            user_src_files.iter().any(|(p, _)| p == cand)
        });
        let dest = workspace_dir.join(&new_path);
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&dest, &content);
        user_src_files.push((new_path, content));
        *selected = ProjectFileId::UserFile(user_src_files.len() - 1);
        *save_needed = true;
    }

    // Deletion
    if let Some(idx) = to_delete {
        if *selected == ProjectFileId::UserFile(idx) {
            *selected = ProjectFileId::MainRs;
        }
        let dest = workspace_dir.join(&user_src_files[idx].0);
        let _ = std::fs::remove_file(&dest);
        user_src_files.remove(idx);
        *save_needed = true;
    }

    // Rename. The typed name is validated BEFORE it is consumed: a refusal
    // leaves the input open with the text still in it (it used to be `take()`n
    // and thrown away, so a rejected rename lost what you typed and said
    // nothing about why).
    //
    // Nothing is moved here. The rename is handed to the app as a REQUEST,
    // because renaming a `.rs` file has to ask rust-analyzer to rewrite the
    // `mod` / `use` / path references FIRST - `workspace/willRenameFiles` needs
    // the old path to still exist on disk and in the analyzer's VFS.
    if let Some(confirm_idx) = do_rename_file {
        let typed = renaming_file
            .as_ref()
            .map(|(_, n)| n.clone())
            .unwrap_or_default();
        let old_path = user_src_files[confirm_idx].0.clone();
        match validate_rename(&old_path, &typed, |cand| {
            user_src_files
                .iter()
                .any(|(p, _)| p.eq_ignore_ascii_case(cand))
                || user_src_folders.iter().any(|f| f.eq_ignore_ascii_case(cand))
                // `src/main.rs` is GENERATED and never appears in
                // `user_src_files`, so nothing above would notice a rename
                // landing on it.
                || cand.eq_ignore_ascii_case("src/main.rs")
        }) {
            Ok(new_path) => {
                *renaming_file = None;
                *rename_request = Some(RenameRequest { old_path, new_path });
            }
            // Empty reason = "nothing changed": close the input, say nothing.
            Err(reason) if reason.is_empty() => *renaming_file = None,
            Err(reason) => {
                // Re-arm the focus flag the input consumed on its first frame.
                // Without this the box stays open but unfocused, `lost_focus()`
                // fires next frame and cancels the edit - so a refusal would
                // still throw away what was typed, which is what this whole
                // branch exists to avoid.
                let fid = egui::Id::new(("__rename_file__", confirm_idx));
                ui.memory_mut(|m| m.data.insert_temp(fid, true));
                set_tree_notice(ui.ctx(), reason);
            }
        }
    } else if cancel_rename_file {
        *renaming_file = None;
    }

    // While a tree item is being dragged (payload armed via the hold gate), give
    // the cursor the drag icon — `Grabbing`, or `NoDrop` when the item is
    // auto-generated (can't be moved) — and float a name preview by the pointer.
    // Drawn ONCE, payload-driven (a per-widget preview would vanish once the
    // pointer leaves the source row), and at the END of the tree: egui's
    // last-write-wins cursor means running after every row overrides any hover
    // cursor a row set (a selectable Label's I-beam used to mask this for files).
    // Reading the payload here also picks up same-frame arming (no 1-frame lag).
    if let Some(payload) = egui::DragAndDrop::payload::<DraggedItem>(ui.ctx()) {
        let blocked = match &*payload {
            DraggedItem::File(idx) => user_src_files
                .get(*idx)
                .is_some_and(|(p, _)| generated_file_reason(p).is_some()),
            DraggedItem::Folder(p) => generated_folder_reason(p).is_some(),
        };
        ui.ctx().set_cursor_icon(if blocked {
            egui::CursorIcon::NoDrop
        } else {
            egui::CursorIcon::Grabbing
        });
        let label = match &*payload {
            DraggedItem::File(idx) => user_src_files
                .get(*idx)
                .map(|(p, _)| format!("{} {}", ph::FILE, base_name(p))),
            DraggedItem::Folder(p) => Some(format!("{} {}/", ph::FOLDER, base_name(p))),
        };
        if let (Some(label), Some(pos)) = (label, ui.ctx().pointer_interact_pos()) {
            egui::Area::new(egui::Id::new("__tree_drag_preview__"))
                .fixed_pos(pos + egui::vec2(12.0, 4.0))
                .order(egui::Order::Tooltip)
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        // `.extend()`: never wrap — near the panel edge the name
                        // otherwise breaks into a one-char-per-line column.
                        ui.add(egui::Label::new(egui::RichText::new(label).size(11.0)).extend());
                    });
                });
        }
    }

    // ── Ctrl+V ───────────────────────────────────────────────────────────────
    // egui-winit turns the paste shortcut into an `Event::Paste` and returns
    // WITHOUT emitting a key, so there is no `Key::V` to consume — the event is
    // the shortcut. It also means a paste is invisible here when the system
    // clipboard is empty, which is one more reason Copy puts a token on it.
    //
    // Skipped while any text widget holds focus: that paste belongs to the
    // editor, which is already consuming the same event. And anything that is
    // not one of our tokens is ordinary text, so Ctrl+V with a normal clipboard
    // is silently a no-op rather than a surprise.
    if clip_paste.is_none() && !ui.ctx().egui_wants_keyboard_input() {
        let pasted = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Paste(t) => Some(t.clone()),
                _ => None,
            })
        });
        if let Some(id) = pasted.as_deref().and_then(clipboard::id_from_token) {
            let is_lib = clipboard::load_manifest(id)
                .is_some_and(|m| m.kind == clipboard::ClipKind::Library);
            // A crate directory can only live at the project root; everything
            // else lands beside the selected file, which is the closest thing
            // the tree has to a "current folder" (folders aren't selectable).
            let target_dir = if is_lib {
                String::new()
            } else {
                match selected {
                    ProjectFileId::UserFile(i) => user_src_files
                        .get(*i)
                        .and_then(|(p, _)| p.rsplit_once('/'))
                        .map(|(dir, _)| dir.to_owned())
                        .unwrap_or_else(|| SRC_ROOT.to_owned()),
                    _ => SRC_ROOT.to_owned(),
                }
            };
            *clip_paste = Some(clipboard::PasteRequest {
                target_dir,
                id: Some(id.to_owned()),
            });
        }
    }
}

/// Recursively render tree nodes (files and folders).
#[allow(clippy::too_many_arguments)]
fn render_tree_node(
    ui: &mut egui::Ui,
    tree: &BTreeMap<String, TreeNode>,
    user_src_files: &mut Vec<(String, String)>,
    user_src_folders: &mut Vec<String>,
    selected: &mut ProjectFileId,
    indent: f32,
    renaming_file: &mut Option<(usize, String)>,
    do_rename_file: &mut Option<usize>,
    cancel_rename_file: &mut bool,
    to_delete: &mut Option<usize>,
    to_duplicate: &mut Option<usize>,
    open_reference: &mut Option<String>,
    clip_copy: &mut Option<clipboard::CopyRequest>,
    clip_paste: &mut Option<clipboard::PasteRequest>,
    goto_error: &mut Option<ProjectFileId>,
    move_to_folder: &mut Option<String>,
    renaming_folder: &mut Option<(String, String)>,
    workspace_dir: &std::path::Path,
    project_dir: Option<&std::path::Path>,
    save_needed: &mut bool,
    new_src_name: &mut Option<String>,
    new_src_folder_name: &mut Option<String>,
    new_file_parent_folder: &mut Option<String>,
    new_folder_parent_folder: &mut Option<String>,
    parent_path: &str,
    move_request: &mut Option<(DraggedItem, String)>,
    extract_folder: &mut Option<String>,
    // User-file diagnostic status: `true` = errors, `false` = warnings-only.
    file_diags: &std::collections::HashMap<String, bool>,
) {
    let default_tree_folder_color = egui::Color32::from_rgb(100, 105, 115);
    // While any inline edit is active (new file/folder input or a rename), don't
    // arm the folder drag-source overlay: its `Sense::drag()` interaction on the
    // header steals the pointer/focus from the just-opened input, so the input
    // flickers open and immediately cancels (reported as "it tries to move the
    // folder"). Dragging isn't meaningful mid-edit anyway.
    let editing = new_src_name.is_some()
        || new_src_folder_name.is_some()
        || renaming_file.is_some()
        || renaming_folder.is_some();

    for (name, node) in tree {
        match node {
            TreeNode::File(idx) => {
                let full_path = user_src_files[*idx].0.clone();
                let file_name = full_path
                    .split('/')
                    .last()
                    .unwrap_or(&full_path)
                    .to_string();
                let can_duplicate = generated_file_reason(&full_path).is_none();
                user_file_row(
                    ui,
                    indent,
                    &file_name,
                    *idx,
                    selected,
                    to_delete,
                    renaming_file,
                    do_rename_file,
                    cancel_rename_file,
                    to_duplicate,
                    open_reference,
                    clip_copy,
                    goto_error,
                    move_to_folder,
                    can_duplicate,
                    &full_path,
                    project_dir,
                    file_diags.get(&full_path).copied(),
                );
            }
            TreeNode::Folder(children) => {
                let folder_path = if parent_path.is_empty() {
                    name.clone()
                } else {
                    format!("{parent_path}/{name}")
                };
                let is_renaming = renaming_folder
                    .as_ref()
                    .map(|(f, _)| f == &folder_path)
                    .unwrap_or(false);

                if is_renaming {
                    let mut do_apply = false;
                    let mut do_cancel = false;
                    if let Some((_, new_name)) = renaming_folder.as_mut() {
                        let fid = egui::Id::new(("__rename_folder__", folder_path.as_str()));
                        ui.horizontal(|ui| {
                            ui.add_space(indent);
                            ui.label(
                                egui::RichText::new(ph::FOLDER)
                                    .size(11.5)
                                    .color(egui::Color32::from_rgb(200, 165, 70)),
                            );
                            let resp = ui.add(
                                egui::TextEdit::singleline(new_name)
                                    .desired_width(ui.available_width()),
                            );
                            if ui.memory(|m| m.data.get_temp::<bool>(fid).unwrap_or(true)) {
                                resp.request_focus();
                                ui.memory_mut(|m| m.data.insert_temp(fid, false));
                            }
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                do_apply = true;
                            } else if ui.input(|i| i.key_pressed(egui::Key::Escape))
                                || resp.lost_focus()
                            {
                                do_cancel = true;
                            }
                        });
                    }
                    if do_apply {
                        if let Some((_, new_name)) = renaming_folder.take() {
                            let clean = new_name.trim().to_string();
                            let new_path = if parent_path.is_empty() {
                                clean.clone()
                            } else {
                                format!("{parent_path}/{clean}")
                            };
                            let collides = user_src_folders.iter().any(|f| f == &new_path)
                                || user_src_files.iter().any(|(p, _)| p == &new_path);
                            if !clean.is_empty() && new_path != folder_path && !collides {
                                // Best-effort rename on disk (live workspace).
                                let old_dest = workspace_dir.join(&folder_path);
                                let new_dest = workspace_dir.join(&new_path);
                                let _ = std::fs::rename(&old_dest, &new_dest);
                                // Update the folder itself and any nested folders.
                                let old_prefix = format!("{folder_path}/");
                                for f in user_src_folders.iter_mut() {
                                    if *f == folder_path {
                                        *f = new_path.clone();
                                    } else if let Some(rest) = f.strip_prefix(&old_prefix) {
                                        *f = format!("{new_path}/{rest}");
                                    }
                                }
                                // Update every file under the renamed folder.
                                for (p, _) in user_src_files.iter_mut() {
                                    if let Some(rest) = p.strip_prefix(&old_prefix) {
                                        *p = format!("{new_path}/{rest}");
                                    }
                                }
                                *save_needed = true;
                            }
                        }
                    } else if do_cancel {
                        *renaming_folder = None;
                    }
                } else {
                    let ch = egui::CollapsingHeader::new(
                        egui::RichText::new(format!("{name}/"))
                            .size(11.5)
                            .monospace()
                            .color(default_tree_folder_color),
                    )
                    .default_open(true)
                    .show(ui, |ui| {
                        // Inline "new file / new folder" input as the first child
                        // of this folder (shown right where the item is added).
                        inline_new_item(
                            ui,
                            indent + 8.0,
                            &folder_path,
                            false,
                            new_src_name,
                            new_file_parent_folder,
                            user_src_files,
                            user_src_folders,
                            selected,
                            workspace_dir,
                            save_needed,
                        );
                        inline_new_item(
                            ui,
                            indent + 8.0,
                            &folder_path,
                            true,
                            new_src_folder_name,
                            new_folder_parent_folder,
                            user_src_files,
                            user_src_folders,
                            selected,
                            workspace_dir,
                            save_needed,
                        );
                        if children.is_empty()
                            && new_file_parent_folder.as_deref() != Some(folder_path.as_str())
                            && new_folder_parent_folder.as_deref() != Some(folder_path.as_str())
                        {
                            ui.label(
                                egui::RichText::new("  (empty)")
                                    .size(10.0)
                                    .color(egui::Color32::from_gray(95)),
                            );
                        }
                        render_tree_node(
                            ui,
                            children,
                            user_src_files,
                            user_src_folders,
                            selected,
                            indent + 8.0,
                            renaming_file,
                            do_rename_file,
                            cancel_rename_file,
                            to_delete,
                            to_duplicate,
                            open_reference,
                            clip_copy,
                            clip_paste,
                            goto_error,
                            move_to_folder,
                            renaming_folder,
                            workspace_dir,
                            project_dir,
                            save_needed,
                            new_src_name,
                            new_src_folder_name,
                            new_file_parent_folder,
                            new_folder_parent_folder,
                            &folder_path,
                            move_request,
                            extract_folder,
                            file_diags,
                        );
                    });

                    // Drag SOURCE: hold the primary button STILL on the header
                    // for ~0.4s to arm a move of the whole folder, then drag to a
                    // target. The payload is set directly on the header response —
                    // NO separate `ui.interact` overlay: an overlay competed with
                    // the header for pointer interactions, which broke the
                    // right-click context menu (New/Rename/Delete) and stole focus
                    // from the inline new-item input. A plain click (short) still
                    // toggles the header; a right-click (secondary) never arms.
                    // Skipped while an inline edit is open (see `editing` above).
                    if !editing && drag_armed(ui, &ch.header_response) {
                        egui::DragAndDrop::set_payload(
                            ui.ctx(),
                            DraggedItem::Folder(folder_path.clone()),
                        );
                    }

                    // Hover: tint the header + pointing hand (click toggles,
                    // right-click menu, hold-to-drag all work here); grab while
                    // the button is held (pre-arm feedback). The end-of-tree
                    // payload block upgrades to grabbing/no-drop once armed.
                    row_hover_feedback(
                        ui,
                        ch.header_response.rect,
                        ch.header_response.contains_pointer(),
                    );
                    if !editing
                        && ch.header_response.is_pointer_button_down_on()
                        && ui.input(|i| i.pointer.primary_down())
                    {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    }

                    // Drop TARGET: dragging an item onto this folder header moves
                    // it here. Highlight the header while an item hovers over it.
                    if ch
                        .header_response
                        .dnd_hover_payload::<DraggedItem>()
                        .is_some()
                    {
                        ui.painter().rect_stroke(
                            ch.header_response.rect,
                            3.0,
                            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(120, 170, 240)),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if let Some(p) = ch.header_response.dnd_release_payload::<DraggedItem>() {
                        *move_request = Some(((*p).clone(), folder_path.clone()));
                    }

                    ch.header_response.context_menu(|ui| {
                        // Nothing new goes into `pins/configs/`: the next
                        // regeneration prunes what it did not generate.
                        let can_add = !in_generated_configs(&folder_path);
                        if ui
                            .add_enabled(
                                can_add,
                                egui::Button::new(menu_label(ph::FILE_PLUS, "New File", ICON_NEW)),
                            )
                            .clicked()
                        {
                            begin_inline_new(
                                ui,
                                false,
                                &folder_path,
                                new_src_name,
                                new_file_parent_folder,
                            );
                            // Cancel any pending folder input so only one shows.
                            *new_src_folder_name = None;
                            *new_folder_parent_folder = None;
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                can_add,
                                egui::Button::new(menu_label(
                                    ph::FOLDER_PLUS,
                                    "New Folder",
                                    ICON_FOLDER,
                                )),
                            )
                            .clicked()
                        {
                            begin_inline_new(
                                ui,
                                true,
                                &folder_path,
                                new_src_folder_name,
                                new_folder_parent_folder,
                            );
                            *new_src_name = None;
                            *new_file_parent_folder = None;
                            ui.close();
                        }
                        ui.separator();
                        // Above the read-only guard below on purpose: a
                        // generated folder can't be renamed or deleted, but
                        // opening it is still perfectly reasonable.
                        reveal_menu_items(ui, project_dir, &folder_path);
                        // `pins/` and `pins/configs/` are rebuilt every frame by
                        // the pin/peripheral sync — renaming or deleting them
                        // would be undone, or would break codegen. Same guard
                        // that already refuses moving them.
                        if let Some(reason) = generated_folder_reason(&folder_path) {
                            ui.separator();
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} Read-only — {reason}",
                                    ph::LOCK_SIMPLE
                                ))
                                .size(10.5)
                                .color(egui::Color32::from_rgb(210, 170, 90)),
                            );
                            return;
                        }
                        ui.separator();
                        copy_menu_item(ui, clipboard::ClipKind::Folder, &folder_path, clip_copy);
                        paste_menu_item(ui, &folder_path, false, clip_paste);
                        ui.separator();
                        // Turn this folder into a sibling crate you can publish.
                        if ui
                            .button(menu_label(
                                ph::PACKAGE,
                                "Extract to library crate…",
                                // Same green as the PACKAGE icon of the library
                                // rows below — the thing this folder becomes.
                                ICON_LIBRARY,
                            ))
                            .on_hover_text(
                                "Move this folder into its own Cargo crate next to src/, \
                                 wire it up as a workspace member + path dependency, and \
                                 give it a publishable Cargo.toml.",
                            )
                            .clicked()
                        {
                            *extract_folder = Some(folder_path.clone());
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .button(menu_label(ph::PENCIL_SIMPLE, "Rename", ICON_EDIT))
                            .clicked()
                        {
                            *renaming_folder = Some((folder_path.clone(), name.clone()));
                            // Reset the focus flag so the edit box grabs focus next frame.
                            let fid = egui::Id::new(("__rename_folder__", folder_path.as_str()));
                            ui.memory_mut(|m| m.data.insert_temp(fid, true));
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .button(menu_label_danger(ph::TRASH, "Delete", ICON_DANGER))
                            .clicked()
                        {
                            // Delete folder and all contents
                            let prefix = format!("{folder_path}/");
                            let to_rm: Vec<usize> = user_src_files
                                .iter()
                                .enumerate()
                                .filter(|(_, (p, _))| p.starts_with(&prefix))
                                .map(|(i, _)| i)
                                .collect();
                            // If the selected file lives under this folder it's about
                            // to be removed; fall back to main.rs before indices shift.
                            if let ProjectFileId::UserFile(sel) = *selected {
                                if to_rm.contains(&sel) {
                                    *selected = ProjectFileId::MainRs;
                                }
                            }
                            for i in to_rm.into_iter().rev() {
                                // user_src_files paths are relative to src/.
                                let dest = workspace_dir.join(&user_src_files[i].0);
                                let _ = std::fs::remove_file(&dest);
                            }
                            // Drop the entries directly instead of relying on fs-watcher polling.
                            user_src_files.retain(|(p, _)| !p.starts_with(&prefix));
                            user_src_folders.retain(|f| f != &folder_path);
                            let dest = workspace_dir.join(&folder_path);
                            let _ = std::fs::remove_dir_all(&dest);
                            *save_needed = true;
                            ui.close();
                        }
                    });
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn file_row(
    ui: &mut egui::Ui,
    indent: f32,
    name: &str,
    id: ProjectFileId,
    selected: &mut ProjectFileId,
    open_reference: &mut Option<String>,
    // Set when a row carrying the RED error badge is clicked: the app then
    // scrolls the editor to that file's first error instead of its top.
    goto_error: &mut Option<ProjectFileId>,
    // The saved project folder, for the Show-in-Explorer / Copy-path entries.
    project_dir: Option<&std::path::Path>,
    badges: &DiagBadges,
) {
    let hi = egui::Color32::from_rgb(100, 180, 255);
    // Fixed project files have no Rename/Delete menu — mark them dark-red + bold.
    let fixed = egui::Color32::from_rgb(100, 50, 50);

    let row = ui.horizontal(|ui| {
        ui.add_space(indent);
        let is_sel = *selected == id;
        let color = if is_sel { hi } else { fixed };
        ui.label(egui::RichText::new(ph::FILE).size(11.5).color(color));
        // `selectable(false)`: keeps the label from forcing the text (I-beam)
        // hover cursor, which would override the drag cursor while an item is
        // dragged across this row (see `user_file_row`).
        let resp = ui.add(
            egui::Label::new(
                egui::RichText::new(name)
                    .size(11.5)
                    .monospace()
                    .strong()
                    .color(color),
            )
            .selectable(false)
            .sense(egui::Sense::click()),
        );
        // Computed BEFORE the click is handled (the badge below reuses it):
        // clicking a row that is MARKED with the error badge means "take me to
        // that error", so the click needs to know whether the badge is there.
        let cargo_path = id.cargo_path();
        let err = cargo_path.is_some_and(|p| badges.errors_in(p));
        if resp.clicked() {
            *selected = id;
            if err {
                *goto_error = Some(id);
            }
        }
        // The fixed files have no Rename/Delete (codegen owns them), but they
        // are just as worth consulting side-by-side — or opening in a file
        // manager — as a user file: reading memory.x or Cargo.toml while
        // editing main.rs is the common case. `file_row` only ever renders
        // fixed ids, so the path always resolves.
        if let Some(rel) = id.fixed_rel_path() {
            resp.context_menu(|ui| {
                if ui
                    .button(menu_label(ph::COLUMNS, "Open beside editor", ICON_VIEW))
                    .on_hover_text(
                        "Show this file READ-ONLY in the Reference tab, so you can consult it \
                         while editing another one",
                    )
                    .clicked()
                {
                    *open_reference = Some(rel.to_owned());
                    ui.close();
                }
                ui.separator();
                reveal_menu_items(ui, project_dir, rel);
            });
        }
        if let Some(cargo_path) = cargo_path {
            if err {
                ui.label(
                    egui::RichText::new(ph::X_CIRCLE)
                        .size(10.0)
                        .color(egui::Color32::from_rgb(220, 80, 70)),
                )
                .on_hover_text("This file has errors");
            } else {
                // Amber warning badge — only when there are warnings but NO errors.
                if badges.warnings_in(cargo_path) {
                    ui.label(
                        egui::RichText::new(ph::WARNING)
                            .size(10.0)
                            .color(egui::Color32::from_rgb(220, 180, 60)),
                    )
                    .on_hover_text("This file has warnings");
                }
            }
        }
    });
    // Hover: tint + pointing hand (the row is clickable).
    row_hover_feedback(ui, row.response.rect, row.response.contains_pointer());
}

#[allow(clippy::too_many_arguments)]
fn user_file_row(
    ui: &mut egui::Ui,
    indent: f32,
    name: &str,
    idx: usize,
    selected: &mut ProjectFileId,
    to_delete: &mut Option<usize>,
    renaming: &mut Option<(usize, String)>,
    do_rename: &mut Option<usize>,
    cancel_rename: &mut bool,
    // Set by the Duplicate menu entry; the caller copies the file (it owns the
    // file list). Offered only for files that are safe to rename/delete — a
    // copy inside `pins/` would be pruned by the next pin sync.
    to_duplicate: &mut Option<usize>,
    open_reference: &mut Option<String>,
    clip_copy: &mut Option<clipboard::CopyRequest>,
    // Set when a row carrying the RED error badge is clicked: the app then
    // scrolls the editor to that file's first error instead of its top.
    goto_error: &mut Option<ProjectFileId>,
    // Set by "Move to folder…"; the app opens the destination dialog.
    move_to_folder: &mut Option<String>,
    can_duplicate: bool,
    // Project-root-relative path of this file + the saved project folder, for
    // the Show-in-Explorer / Copy-path entries.
    rel_path: &str,
    project_dir: Option<&std::path::Path>,
    // Diagnostic badge at the row's end: `Some(true)` = errors (red), `Some(false)`
    // = warnings only (amber), `None` = clean.
    diag: Option<bool>,
) {
    let hi = egui::Color32::from_rgb(100, 180, 255);
    let normal = egui::Color32::from_rgb(200, 205, 215);
    let id = ProjectFileId::UserFile(idx);
    let is_renaming = renaming.as_ref().map(|(i, _)| *i == idx).unwrap_or(false);

    if is_renaming {
        ui.horizontal(|ui| {
            ui.add_space(indent);
            if let Some((_, new_name)) = renaming.as_mut() {
                let fid = egui::Id::new(("__rename_file__", idx));
                let resp = ui
                    .add(egui::TextEdit::singleline(new_name).desired_width(ui.available_width()));
                // Focus the field on the first frame it appears.
                if ui.memory(|m| m.data.get_temp::<bool>(fid).unwrap_or(true)) {
                    resp.request_focus();
                    ui.memory_mut(|m| m.data.insert_temp(fid, false));
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    *do_rename = Some(idx);
                } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) || resp.lost_focus() {
                    *cancel_rename = true;
                }
            }
        });
        return;
    }

    let row = ui.horizontal(|ui| {
        ui.add_space(indent);
        let is_sel = *selected == id;
        let color = if is_sel { hi } else { normal };
        ui.label(egui::RichText::new(ph::FILE).size(11.5).color(color));
        // Plain click sense: click selects, right-click opens the menu. A move
        // arms only via the hold gate (`drag_armed`) — hold the primary button
        // still ~0.4s, then drag; the payload is set directly (no drag sense, so
        // nothing competes with clicks). The floating preview + drag cursor are
        // drawn globally at the end of the tree while a payload exists.
        // `selectable(false)`: a selectable Label sets the text (I-beam) hover
        // cursor AFTER our global drag-cursor call, overriding it — which is why
        // files showed no drag cursor while folder headers (plain buttons) did.
        let resp = ui.add(
            egui::Label::new(
                egui::RichText::new(name)
                    .size(11.5)
                    .monospace()
                    .color(color),
            )
            .selectable(false)
            .sense(egui::Sense::click()),
        );
        // Diagnostic badge at the row's end — red error / amber warning — same
        // as the fixed project files' `file_row` and the Structure tab's flag.
        match diag {
            Some(true) => {
                ui.label(
                    egui::RichText::new(ph::X_CIRCLE)
                        .size(10.0)
                        .color(egui::Color32::from_rgb(220, 80, 70)),
                )
                .on_hover_text("This file has errors");
            }
            Some(false) => {
                ui.label(
                    egui::RichText::new(ph::WARNING)
                        .size(10.0)
                        .color(egui::Color32::from_rgb(220, 180, 60)),
                )
                .on_hover_text("This file has warnings");
            }
            None => {}
        }
        // Holding the primary button on the row (pre-arm) — reported to the
        // caller so the cursor can show "grab" while the 0.4s hold elapses.
        let held =
            resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down());
        if drag_armed(ui, &resp) {
            egui::DragAndDrop::set_payload(ui.ctx(), DraggedItem::File(idx));
        }
        if resp.clicked() {
            *selected = id;
            // `Some(true)` is exactly the condition that paints the red badge
            // (warnings are `Some(false)` and stay a plain selection) — the
            // jump follows the marker the user actually clicked on.
            if diag == Some(true) {
                *goto_error = Some(id);
            }
        }
        resp.context_menu(|ui| {
            if ui
                .button(menu_label(ph::PENCIL_SIMPLE, "Rename", ICON_EDIT))
                .clicked()
            {
                *renaming = Some((idx, name.to_string()));
                // Reset the focus flag so the edit box grabs focus next frame.
                let fid = egui::Id::new(("__rename_file__", idx));
                ui.memory_mut(|m| m.data.insert_temp(fid, true));
                ui.close();
            }
            if can_duplicate
                && ui
                    .button(menu_label(ph::COPY, "Duplicate", ICON_UTIL))
                    .on_hover_text("Copy this file next to it as <name>_1")
                    .clicked()
            {
                *to_duplicate = Some(idx);
                ui.close();
            }
            copy_menu_item(ui, clipboard::ClipKind::File, rel_path, clip_copy);
            // Dragging can only reach a folder that is currently on screen, and
            // it cannot say what the move implies. This can.
            if ui
                .button(menu_label(ph::FOLDER_OPEN, "Move to folder…", ICON_FOLDER))
                .on_hover_text("Move this file into another folder of the same crate")
                .clicked()
            {
                *move_to_folder = Some(rel_path.to_owned());
                ui.close();
            }
            ui.separator();
            if ui
                .button(menu_label(ph::COLUMNS, "Open beside editor", ICON_VIEW))
                .on_hover_text(
                    "Show this file READ-ONLY in the Reference tab, so you can consult it while editing another one",
                )
                .clicked()
            {
                *open_reference = Some(rel_path.to_owned());
                ui.close();
            }
            ui.separator();
            reveal_menu_items(ui, project_dir, rel_path);
            ui.separator();
            if ui
                .button(menu_label_danger(ph::TRASH, "Delete", ICON_DANGER))
                .clicked()
            {
                *to_delete = Some(idx);
                ui.close();
            }
        });
        held
    });
    // Hover: tint the whole row + pointing-hand cursor (click / menu / drag all
    // work here); while the button is held, grab — then the end-of-tree payload
    // block upgrades it to grabbing/no-drop once the drag arms.
    row_hover_feedback(ui, row.response.rect, row.response.contains_pointer());
    if row.inner {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An I2C bus folder is as generated as `pins/configs/` itself: it cannot
    /// be moved, renamed or deleted, nothing new goes into it - and the user's
    /// own `src/pins/utils/` stays theirs.
    #[test]
    fn bus_folders_are_generated_and_pins_utils_is_not() {
        assert!(generated_folder_reason("src/pins/configs/i2c1").is_some());
        assert!(generated_folder_reason("src/pins/configs/twim0").is_some());
        assert!(generated_folder_reason("src/pins/configs").is_some());
        assert!(generated_folder_reason("src/pins/utils").is_none());
        assert!(generated_folder_reason("src/pins_configs").is_none());
        assert!(in_generated_configs("src/pins/configs"));
        assert!(in_generated_configs("src/pins/configs/i2c1"));
        assert!(!in_generated_configs("src/pins"));
        assert!(!in_generated_configs("src/pins/utils"));
        assert!(generated_file_reason("src/pins/configs/i2c1/device1_oled.rs").is_some());
    }

    /// A rename is a NAME. Typing a path used to change the in-memory path
    /// while `fs::rename` failed on the missing directory - memory and disk
    /// diverged silently until the next full write.
    #[test]
    fn a_path_is_refused_not_half_applied() {
        for typed in ["sub/x.rs", "..\\x.rs", "a/b.rs"] {
            let err = validate_rename("src/a.rs", typed, |_| false)
                .expect_err(&format!("`{typed}` must be refused"));
            assert!(!err.is_empty(), "the refusal has to say something");
        }
    }

    /// Windows renames are case-insensitive, so this would leave the file
    /// untouched while everything downstream believed it moved.
    #[test]
    fn a_case_only_rename_is_refused() {
        let err = validate_rename("src/radar.rs", "Radar.rs", |_| false).unwrap_err();
        assert!(err.contains("capitalisation"), "{err}");
        // But a real rename that merely SHARES some letters is fine.
        assert_eq!(
            validate_rename("src/radar.rs", "radar_io.rs", |_| false).unwrap(),
            "src/radar_io.rs"
        );
    }

    /// The old check looked only at files, so a file could take a folder's name.
    #[test]
    fn a_folder_name_is_taken_too() {
        let taken = |p: &str| p == "src/drivers";
        let err = validate_rename("src/a.rs", "drivers", taken).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    /// Renaming an auto-generated file leaves the sync recreating the original
    /// and keeping the copy - you end up with both.
    #[test]
    fn generated_files_refuse_the_rename() {
        let err = validate_rename("src/pins/configs/usart1.rs", "uart1.rs", |_| false).unwrap_err();
        assert!(err.contains("auto-generated"), "{err}");
    }

    /// An unchanged name is not an error - it closes the input silently.
    #[test]
    fn an_unchanged_name_is_a_quiet_cancel() {
        assert_eq!(
            validate_rename("src/a.rs", "  a.rs  ", |_| false).unwrap_err(),
            "",
            "an empty reason means: say nothing"
        );
    }

    /// Windows resolves `b.rs` and `B.rs` to ONE file, so an exact comparison
    /// would call the name free and let `fs::rename` overwrite the other file.
    #[test]
    fn a_collision_is_case_insensitive() {
        let taken = |p: &str| p.eq_ignore_ascii_case("src/beta.rs");
        let err = validate_rename("src/a.rs", "Beta.rs", taken).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    /// The new path keeps the old folder.
    #[test]
    fn the_folder_is_preserved() {
        assert_eq!(
            validate_rename("mylib/src/radar.rs", "radar_io.rs", |_| false).unwrap(),
            "mylib/src/radar_io.rs"
        );
        // A file at the project root has no folder to keep.
        assert_eq!(
            validate_rename("notes.md", "readme.md", |_| false).unwrap(),
            "readme.md"
        );
    }

    /// Realistic floor with egui's default spacing: 4 pad + 6 separator
    /// + 2 * 3 item spacing + 18 row.
    const FLOOR: f32 = 34.0;

    /// The bug: with no libraries the section was sized by a formula that left
    /// out the separator, so the header row — which carries the ONLY buttons
    /// that can add a first library — rendered clipped at the panel's edge. It
    /// must always get at least its floor.
    #[test]
    fn an_empty_section_still_gets_its_whole_header() {
        assert_eq!(
            libs_section_h(600.0, FLOOR, DEFAULT_SPLIT_RATIO, false),
            FLOOR
        );
        // …and nothing more: an empty section must not eat into the tree.
        let project_h =
            600.0 - SPLIT_HANDLE_H - libs_section_h(600.0, FLOOR, DEFAULT_SPLIT_RATIO, false);
        assert!(
            project_h > 550.0,
            "the tree keeps the rest, got {project_h}"
        );
    }

    /// Dragging the handle works with no libraries too — that is how you make
    /// room in an empty section before cloning a library into it.
    #[test]
    fn dragging_the_handle_resizes_an_empty_section() {
        // Dragged UP from the default → the section grows past its floor.
        let h = libs_section_h(600.0, FLOOR, 0.4, false);
        assert!(h > FLOOR, "expected the drag to win, got {h}");
        assert!((h - (600.0 - SPLIT_HANDLE_H) * 0.6).abs() < 0.01);
    }

    /// With libraries present the ratio governs from the start.
    #[test]
    fn the_ratio_governs_once_there_are_libraries() {
        let h = libs_section_h(600.0, FLOOR, DEFAULT_SPLIT_RATIO, true);
        assert!((h - (600.0 - SPLIT_HANDLE_H) * 0.4).abs() < 0.01, "{h}");
    }

    /// The floor outranks the ratio: a splitter dragged all the way down still
    /// leaves the header visible.
    #[test]
    fn the_floor_survives_a_ratio_that_would_hide_the_header() {
        assert_eq!(libs_section_h(600.0, FLOOR, 0.99, true), FLOOR);
        // Same in a very short panel, where even the floor exceeds the share.
        assert_eq!(libs_section_h(40.0, FLOOR, 0.5, true), FLOOR);
    }

    /// The badges answer what each row used to compute with both locks held:
    /// `has_errors_in || error_count_for > 0`, and the same for warnings. With
    /// and without a Cargo result, for files in one source, both or neither,
    /// and for levels and severities that badge nothing.
    #[test]
    fn diag_badges_match_the_per_row_scans() {
        const FILES: &[&str] = &["src/main.rs", "src/a.rs", "lib/src/lib.rs", "Cargo.toml"];
        const LEVELS: &[&str] = &["error", "warning", "note", "help", "failure-note"];
        const SEVERITIES: &[lsp::DiagSeverity] = &[
            lsp::DiagSeverity::Error,
            lsp::DiagSeverity::Warning,
            lsp::DiagSeverity::Info,
            lsp::DiagSeverity::Hint,
        ];
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize
        };
        let (mut errors, mut warnings_only) = (0, 0);
        for round in 0..500 {
            let result = build::BuildResult {
                success: false,
                diagnostics: (0..next() % 6)
                    .map(|_| build::Diagnostic {
                        level: LEVELS[next() % LEVELS.len()].into(),
                        message: String::new(),
                        rendered: String::new(),
                        file: (next() % 6 != 0).then(|| FILES[next() % FILES.len()].into()),
                        line: None,
                        col: None,
                        code: None,
                        fixes: Vec::new(),
                        rename: None,
                    })
                    .collect(),
            };
            let mut lsp_state = lsp::LspState::default();
            for file in FILES {
                if next() % 3 == 0 {
                    let ds = (0..next() % 4)
                        .map(|_| lsp::LspDiagnostic {
                            severity: SEVERITIES[next() % SEVERITIES.len()],
                            message: String::new(),
                            line: 1,
                            col: 1,
                            end_line: 1,
                            end_col: 1,
                            code: None,
                            source: "rustc".into(),
                        })
                        .collect();
                    lsp_state.diagnostics.insert((*file).to_owned(), ds);
                }
            }
            let build_result = (next() % 3 != 0).then_some(&result);

            let mut badges = DiagBadges::default();
            if let Some(result) = build_result {
                badges.add_build(result);
            }
            badges.add_lsp(&lsp_state);

            for file in FILES.iter().chain(&["src/absent.rs", ""]) {
                let err = build_result.is_some_and(|r| r.has_errors_in(file))
                    || lsp_state.error_count_for(file) > 0;
                let warn = build_result.is_some_and(|r| r.has_warnings_in(file))
                    || lsp_state.warning_count_for(file) > 0;
                assert_eq!(
                    (badges.errors_in(file), badges.warnings_in(file)),
                    (err, warn),
                    "round {round} {file}"
                );
                errors += usize::from(err);
                warnings_only += usize::from(warn && !err);
            }
        }
        assert!(
            errors > 200 && warnings_only > 150,
            "{errors} {warnings_only}"
        );
    }
}
