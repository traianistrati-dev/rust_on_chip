//! The Profile tab's "Heat in editor": every line the on-target sampler caught
//! the core on gets a full-width tint, stronger the more of the run the line
//! took itself. The numbers hover in the blank space past the line's end, and
//! the whole list is the Profile tab's "By line" view
//! (`crate::flamegraph::ByLine`).
//!
//! Drawn only while the file's text is EXACTLY what the run compiled: after an
//! edit the sampled line numbers may point at other code, and a band on the
//! wrong line is worse than none.

use crate::app::{AppIde, ProjectFileId};
use crate::editor::gui::text_pos::{GalleyRows, selected_file_rel_path};
use crate::flamegraph::{FlameState, std_err_pct};
use eframe::egui;

/// The tint for a line holding `share` (0..=1) of the run's hottest line's
/// own samples. Over the text, so translucent even at its strongest: the floor
/// keeps a cold line visible, the ceiling keeps the hottest one readable.
pub(crate) fn heat_band(share: f32) -> egui::Color32 {
    let a = 14.0 + 62.0 * share.clamp(0.0, 1.0);
    egui::Color32::from_rgba_unmultiplied(255, 110, 40, a as u8)
}

impl AppIde {
    /// The files of the last sample run whose live text is no longer what was
    /// compiled: their rows in the by-line table keep the old line numbers.
    /// Only worked out while that table is the one on screen.
    pub(super) fn flame_stale_files(&self) -> Vec<String> {
        if !self.flame_by_line
            || self.build_tab != crate::app::BuildPanelTab::Profile
            || self.profile_mode != crate::profile::ProfileMode::Runtime
        {
            return Vec::new();
        }
        let st = self.flame_state.lock().unwrap();
        let FlameState::Done(res) = &*st else {
            return Vec::new();
        };
        let mut files: Vec<&str> = res.by_line.lines.iter().map(|l| l.file.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        // Compared in place - this runs every frame the table is up, and
        // `source_texts` would copy the whole project to answer it.
        let live = |rel: &str| -> Option<&str> {
            if rel == "src/main.rs" {
                return Some(&self.generated_code);
            }
            self.project_tree
                .user_src_files
                .iter()
                .find(|(p, _)| p == rel)
                .map(|(_, c)| c.as_str())
        };
        files
            .into_iter()
            .filter(|f| res.texts.get(*f).map(String::as_str) != live(f))
            .map(str::to_owned)
            .collect()
    }

    /// Tint the sampled lines of the file this view shows. Call with the
    /// editor's output, before the diff gutter (whose bars stay on top).
    pub(super) fn paint_profile_heat(
        &mut self,
        ui: &egui::Ui,
        editor_resp: &egui::text_edit::TextEditOutput,
        // The rows of `editor_resp.galley`, shared with the other overlays.
        rows: &GalleyRows,
        clip: egui::Rect,
        display_code: &str,
        displayed_file: ProjectFileId,
        // (1-based line, right edge) of each "N refs" pill drawn this frame.
        pill_edges: &[(u32, f32)],
    ) {
        self.ed.heat_sensor_lines.clear();
        if !self.flame_heat {
            return;
        }
        let Some(rel) = selected_file_rel_path(&displayed_file, &self.project_tree.user_src_files)
        else {
            return;
        };
        let st = self.flame_state.lock().unwrap();
        let FlameState::Done(res) = &*st else {
            return;
        };
        if res.texts.get(&rel).map(String::as_str) != Some(display_code) {
            return;
        }
        let by = &res.by_line;
        // Scaled to the RUN's hottest line, not this file's: a file whose only
        // sampled line took 1% must not glow like the real hotspot. `lines`
        // is sorted hottest first.
        let max_self = by.lines.first().map_or(0, |l| l.self_n);
        if max_self == 0 {
            return;
        }
        let index = self.ed.line_index.get(display_code);
        let gp = editor_resp.galley_pos;
        let painter = ui.painter().with_clip_rect(clip);
        let n = by.samples.max(1);
        // Overlays at a line's end with tooltips of their own: the type hint
        // (on the BINDING's line - in a multi-line `let` not the caret's) and
        // the call-signature ghost. Both are drawn after this, so these are
        // last frame's, the same lag as the Tab gate in mod.rs.
        let ends: Vec<(u32, f32)> = [self.ed.inlay_hint_right, self.ed.sig_ghost_right]
            .into_iter()
            .flatten()
            .collect();
        // The editor's own vertical scrollbar senses the lane at its right
        // edge; a sensor over it would pop the numbers while scrolling.
        let scroll = &ui.spacing().scroll;
        let right = clip.right() - (scroll.bar_width + scroll.bar_outer_margin);
        for l in by.lines.iter().filter(|l| l.file == rel && l.self_n > 0) {
            let Some(ci) = index.line_start_char(l.line as usize - 1) else {
                continue;
            };
            let loc = rows.pos(ci);
            let (top, bot) = (gp.y + loc.min.y, gp.y + loc.max.y);
            if bot < clip.top() || top > clip.bottom() {
                continue;
            }
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(clip.left(), top),
                    egui::pos2(clip.right(), bot),
                ),
                0.0,
                heat_band(l.self_n as f32 / max_self as f32),
            );
            // The numbers hover only in the blank space PAST the line's text
            // and whatever is drawn after it ("N refs" pill, type hint,
            // signature ghost). egui shows one tooltip per layer, so a sensor
            // over any of them would hold its tooltip hostage; the number
            // column belongs to the breakpoint strip and the fold carets.
            let eol = rows.pos(index.line_end_char_idx(l.line));
            let mut x0 = gp.x + eol.min.x + 8.0;
            for &(ln, edge) in pill_edges.iter().chain(&ends) {
                if ln == l.line {
                    x0 = x0.max(edge + 4.0);
                }
            }
            if x0 >= right {
                continue;
            }
            // Clipped to the editor: a line cut by its edge must not reach
            // over the toolbar or the diagnostics panel's handle.
            let hit = egui::Rect::from_min_max(egui::pos2(x0, top), egui::pos2(right, bot))
                .intersect(clip);
            if !hit.is_positive() {
                continue;
            }
            self.ed.heat_sensor_lines.push(l.line);
            ui.interact(
                hit,
                egui::Id::new("profile_heat").with(&rel).with(l.line),
                egui::Sense::hover(),
            )
            .on_hover_text(format!(
                "Self {:.1}% ± {:.1}  ({} of {} samples)\nTotal {:.1}%  (on the stack)\nin {}",
                l.self_n as f32 * 100.0 / n as f32,
                std_err_pct(l.self_n, n),
                l.self_n,
                by.samples,
                l.total_n as f32 * 100.0 / n as f32,
                l.func,
            ));
        }
    }
}
