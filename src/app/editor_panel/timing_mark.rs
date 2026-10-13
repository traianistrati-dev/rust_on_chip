//! The editor's "Time this function" / "Time selected lines": a region for the
//! Profile tab's Timing mode (see [`crate::timing`]).

use crate::app::AppIde;
use crate::app::ProjectFileId;

impl AppIde {
    /// Mark the function around the selection (`whole_fn`), or the whole
    /// statements it touches, in `text` - the file on screen - and show the
    /// Timing table. A selection that is not code, or not in the firmware's
    /// own files, is refused in the status bar.
    pub(super) fn mark_timing_region(
        &mut self,
        file: &ProjectFileId,
        text: &str,
        selection: Option<(usize, usize)>,
        whole_fn: bool,
    ) {
        let Some(rel) = crate::editor::gui::text_pos::selected_file_rel_path(
            file,
            &self.project_tree.user_src_files,
        ) else {
            return;
        };
        if let Err(why) = crate::timing::instrumentable(&rel) {
            self.set_status_msg(format!("Cannot time this: {why}."));
            return;
        }
        let Some((a, b)) = selection else {
            self.set_status_msg("Place the caret in the code to time first.".to_owned());
            return;
        };
        if self.timing.is_busy() {
            self.set_status_msg("Stop the measurement first, then mark more code.".to_owned());
            return;
        }
        let chars: Vec<char> = text.chars().collect();
        let (_, first, last) = super::comment::selected_lines(&chars, a, b);
        let asked = crate::timing::TimingRegion {
            file: rel,
            first: first as u32 + 1,
            last: last as u32 + 1,
            whole_fn,
            func: String::new(),
            fn_copy: (0, 1),
            anchor: Vec::new(),
            copy: (0, 1),
        };
        // Stored as what it covers, so marking the same code twice is one row
        // and the table shows the lines that will really be timed.
        let (region, placed) = match crate::timing::mark(text, asked) {
            Ok(m) => m,
            Err(why) => {
                self.set_status_msg(format!("Cannot time this: {why}."));
                return;
            }
        };
        self.build_tab = crate::app::BuildPanelTab::Profile;
        self.profile_mode = crate::profile::ProfileMode::Timing;
        let what = if whole_fn {
            format!("fn {}", placed.func)
        } else if placed.lines.0 == placed.lines.1 {
            format!("line {} in {}", placed.lines.0, placed.func)
        } else {
            format!(
                "lines {}-{} in {}",
                placed.lines.0, placed.lines.1, placed.func
            )
        };
        // Rows of this file whose code changed can never be measured again
        // (Measure refuses them): marking anew replaces them.
        let before = self.timing.regions.len();
        self.timing
            .regions
            .retain(|s| s.file != region.file || crate::timing::resolve(text, s).is_ok());
        let dropped = match before - self.timing.regions.len() {
            0 => String::new(),
            1 => " (1 row whose code had changed is gone)".to_owned(),
            n => format!(" ({n} rows whose code had changed are gone)"),
        };
        // The same code, found where it is now - not by the line numbers the
        // row was stored with, which an edit above it has moved.
        let same = self.timing.regions.iter().position(|s| {
            s.file == region.file
                && s.whole_fn == region.whole_fn
                && crate::timing::resolve(text, s)
                    .is_ok_and(|(_, p)| p.lines == (region.first, region.last))
        });
        if let Some(i) = same {
            self.timing.regions[i] = region;
            self.set_status_msg(format!("Already timed: {what}.{dropped}"));
        } else if self.timing.regions.len() >= crate::timing::MAX_REGIONS {
            self.set_status_msg(format!(
                "At most {} regions: remove one in the Profile tab first.{dropped}",
                crate::timing::MAX_REGIONS
            ));
        } else {
            self.timing.regions.push(region);
            self.set_status_msg(format!(
                "Timing {what}: press Measure in the Profile tab.{dropped}"
            ));
        }
    }
}
