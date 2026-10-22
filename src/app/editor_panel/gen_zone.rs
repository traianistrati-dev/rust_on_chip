//! The GENERATED blocks, shaded: every line of a block the IDE rewrites, its
//! two markers included, gets a faint full-width tint, so that code stands
//! apart from the code that is the user's.
//!
//! Each block is found by the code that rewrites it, so the tint cannot
//! disagree with a regeneration:
//! - main.rs: `crate::app::generated_byte_ranges` (every block, one missing
//!   its END running to the file's end), from the file's first line where the
//!   backend writes the header anew (`Mcu::rebuilds_main_header`), less the
//!   hand-written clock region `common::keep_manual_clock` carries over;
//! - the files under `src/pins/`: `templates::block`, as `splice_pin_file`;
//! - the project files (Cargo.toml, .cargo/config.toml, .gitignore, memory.x,
//!   build.rs): `project_gen::generated_block`, as their refresh, and
//!   partitions.csv: `project_gen::partitions_csv_block`.
//!
//! Any other file has none. Only a look: typing inside a block stays allowed.

use super::fold::FoldMap;
use crate::app::{AppIde, ProjectFileId};
use crate::editor::gui::text_pos::GalleyRows;
use crate::panels::mcu_module::project_gen::{ConfigFile, generated_block, partitions_csv_block};
use eframe::egui;

/// A cool grey-blue, over the text like the Profile heat band, and faint
/// enough that the syntax colours read through it.
const GEN_TINT: egui::Color32 = egui::Color32::from_rgba_unmultiplied_const(110, 150, 220, 18);

/// Which code rewrites a file's block, and so where it finds the block.
#[derive(Clone, Copy)]
pub(super) enum Owner {
    /// `src/main.rs`; `header` when an update writes the header above the
    /// block anew as well.
    MainRs { header: bool },
    /// A file under `src/pins/`: `pins/mod.rs`, a pin's module, a config file.
    Pins,
    /// A project file at the root.
    Project(ConfigFile),
    /// `partitions.csv`, the flash store's table.
    Partitions,
}

impl Owner {
    /// The owner of `file`; `None` for a file the IDE writes no block into.
    /// `header`: whether main.rs's header is rewritten too.
    pub(super) fn of(
        file: ProjectFileId,
        user_files: &[(String, String)],
        header: bool,
    ) -> Option<Self> {
        Some(match file {
            ProjectFileId::MainRs => Self::MainRs { header },
            ProjectFileId::CargoToml => Self::Project(ConfigFile::CargoToml),
            ProjectFileId::CargoConfig => Self::Project(ConfigFile::CargoConfig),
            ProjectFileId::MemoryX => Self::Project(ConfigFile::MemoryX),
            ProjectFileId::BuildRs => Self::Project(ConfigFile::BuildRs),
            ProjectFileId::GitIgnore => Self::Project(ConfigFile::GitIgnore),
            ProjectFileId::PartitionsCsv => Self::Partitions,
            ProjectFileId::UserFile(i) => {
                let (path, _) = user_files.get(i)?;
                path.starts_with("src/pins/").then_some(Self::Pins)?
            }
        })
    }
}

/// main.rs's blocks as `generated_byte_ranges` locks them - the first from
/// the file's start when the header is rewritten too - with the hand-written
/// clock region cut out: its two markers stay with the block, the lines
/// between them are the user's. A region missing its END is not kept by a
/// regeneration, so it stays shaded.
fn main_rs_spans(text: &str, header: bool) -> Vec<(usize, usize)> {
    use crate::panels::mcu_module::codegen::common::{CLOCK_BEGIN, CLOCK_END, clock_region_span};
    let mut spans = crate::app::generated_byte_ranges(text);
    if header && let Some(first) = spans.first_mut() {
        first.0 = 0;
    }
    if let Some((cb, ce)) = clock_region_span(text)
        && let Some(i) = spans.iter().position(|&(b, e)| b <= cb && ce <= e)
    {
        let (b, e) = spans[i];
        spans.splice(
            i..=i,
            [(b, cb + CLOCK_BEGIN.len()), (ce - CLOCK_END.len(), e)],
        );
    }
    spans
}

/// The GENERATED blocks of `text`, as 0-based `(first, last)` line spans with
/// both markers included.
pub(super) fn generated_lines(text: &str, owner: Owner) -> Vec<(usize, usize)> {
    let spans = match owner {
        Owner::MainRs { header } => main_rs_spans(text, header),
        Owner::Pins => crate::project_tree::templates::block(text)
            .into_iter()
            .collect(),
        Owner::Project(file) => generated_block(file, text).into_iter().collect(),
        Owner::Partitions => partitions_csv_block(text).into_iter().collect(),
    };
    // Every end is just past a marker, so its line is the marker's.
    let line_of = |byte: usize| text[..byte].matches('\n').count();
    spans
        .into_iter()
        .map(|(b, e)| (line_of(b), line_of(e)))
        .collect()
}

/// The display lines showing a line of `spans`, as `(first, last)` runs.
///
/// A block's visible lines sit next to each other on screen - what a fold
/// hides between them is gone from the display - so each block is one run at
/// most. A block a fold hides whole has none: the header of that fold is the
/// user's line, not the block's.
fn display_runs(spans: &[(usize, usize)], map: &FoldMap) -> Vec<(usize, usize)> {
    spans
        .iter()
        .filter_map(|&(first, last)| {
            let mut shown = (first..=last).filter_map(|l| map.display_line_of(l));
            let top = shown.next()?;
            Some((top, shown.next_back().unwrap_or(top)))
        })
        .collect()
}

impl AppIde {
    /// Shade the GENERATED blocks of the file this view shows.
    ///
    /// Called before every other overlay, so the find hits, the heat band,
    /// the diff bars and the caret all draw over it. Painted in folded frames
    /// too, unlike most of them: it reaches the screen through `map` rather
    /// than by pairing `display_code` with the galley.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_generated_zone(
        &mut self,
        ui: &egui::Ui,
        editor_resp: &egui::text_edit::TextEditOutput,
        // The rows of `editor_resp.galley`, shared with the other overlays.
        rows: &GalleyRows,
        clip: egui::Rect,
        // The BUFFER: the blocks are found in it, then placed through `map`.
        display_code: &str,
        // The file THIS view shows, which decides how its block is found.
        displayed_file: ProjectFileId,
        map: &FoldMap,
    ) {
        let header = self.mcu.as_ref().is_some_and(|m| m.rebuilds_main_header());
        let Some(owner) = Owner::of(displayed_file, &self.project_tree.user_src_files, header)
        else {
            return;
        };
        let spans = generated_lines(display_code, owner);
        if spans.is_empty() {
            return;
        }
        // Where display line `d` starts, and the display's length in chars.
        // Unfolded the display is the buffer itself, which this view indexes.
        let index = map
            .is_identity()
            .then(|| self.ed.line_index.get(display_code));
        let start = |d: usize| match &index {
            Some(index) => index.line_start_char(d),
            None => map.display_line_start(d),
        };
        let total = match &index {
            Some(index) => index.total_chars(),
            None => map.display_chars(),
        };
        let gp = editor_resp.galley_pos;
        let painter = ui.painter().with_clip_rect(clip);
        for (top_line, bot_line) in display_runs(&spans, map) {
            let Some(first) = start(top_line) else {
                continue;
            };
            // The last line's own end, so a row it wraps onto is covered too.
            let end = start(bot_line + 1).map_or(total, |next| next - 1);
            let (top, bot) = (gp.y + rows.pos(first).min.y, gp.y + rows.pos(end).max.y);
            if bot < clip.top() || top > clip.bottom() {
                continue;
            }
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(clip.left(), top),
                    egui::pos2(clip.right(), bot),
                ),
                0.0,
                GEN_TINT,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::codegen::common::{CLOCK_BEGIN, CLOCK_END, GEN_BEGIN, GEN_END};
    use std::collections::BTreeSet;

    /// main.rs whose header an update keeps.
    const MAIN: Owner = Owner::MainRs { header: false };

    /// The marker pair of a `src/pins/` file.
    const PB: &str = "// <<< GENERATED>>>";
    const PE: &str = "// <<< GENERATED END >>>";

    #[test]
    fn each_file_is_read_by_the_code_that_rewrites_it() {
        let files = vec![
            ("src/pins/configs/usart1.rs".to_owned(), String::new()),
            ("src/pins/mod.rs".to_owned(), String::new()),
            ("src/radar.rs".to_owned(), String::new()),
        ];
        let of = |f| Owner::of(f, &files, false);
        assert!(matches!(
            of(ProjectFileId::MainRs),
            Some(Owner::MainRs { header: false })
        ));
        assert!(matches!(of(ProjectFileId::UserFile(0)), Some(Owner::Pins)));
        assert!(matches!(of(ProjectFileId::UserFile(1)), Some(Owner::Pins)));
        assert!(
            of(ProjectFileId::UserFile(2)).is_none(),
            "the user's own module"
        );
        assert!(of(ProjectFileId::UserFile(9)).is_none());
        assert!(matches!(
            of(ProjectFileId::PartitionsCsv),
            Some(Owner::Partitions)
        ));
        assert!(matches!(
            of(ProjectFileId::MemoryX),
            Some(Owner::Project(ConfigFile::MemoryX))
        ));
    }

    #[test]
    fn main_rs_blocks_span_both_markers_and_an_open_one_runs_to_the_end() {
        let code = format!("use x;\n{GEN_BEGIN}\nlet a = 1;\n{GEN_END}\nloop {{}}\n");
        assert_eq!(generated_lines(&code, MAIN), [(1, 3)]);
        let two = format!("{GEN_BEGIN}\na\n{GEN_END}\nmid\n{GEN_BEGIN}\nb\n");
        assert_eq!(generated_lines(&two, MAIN), [(0, 2), (4, 6)]);
        // The short marker of a pins file is not main.rs's.
        assert!(generated_lines(&format!("{PB}\na\n{PE}\n"), MAIN).is_empty());
    }

    /// The pins files follow `templates::block`: a BOM, a note before the
    /// marker, a block emptied in place, and a comment that only names the
    /// marker above or below the real one.
    #[test]
    fn a_pins_file_is_shaded_where_its_splice_finds_the_block() {
        let lines = |t: &str| generated_lines(t, Owner::Pins);
        assert_eq!(
            lines(&format!(
                "{PB}\npub const A: u8 = 1;\n{PE}\n\nfn mine() {{}}\n"
            )),
            [(0, 2)]
        );
        assert_eq!(lines(&format!("\u{feff}{PB}\nA\n{PE}\n")), [(0, 2)]);
        assert_eq!(
            lines(&format!("/* pinned */ {PB}\nA\n{PE}\nmine\n")),
            [(0, 2)]
        );
        assert_eq!(lines(&format!("use x;\n{PB}{PE}\nfn f() {{}}\n")), [(1, 1)]);
        let above = format!("// the block starts at {PB} below\nuse my::thing;\n\n{PB}\nA\n{PE}\n");
        assert_eq!(lines(&above), [(3, 5)]);
        let below =
            format!("{PB}\nA\n{PE}\n\n// BAUD comes from the {PB} block above\nfn init() {{}}\n");
        assert_eq!(lines(&below), [(0, 2)]);
        // No END: the splice finds no block, so nothing is the IDE's.
        assert!(lines(&format!("{PB}\nA\n")).is_empty());
    }

    #[test]
    fn the_project_files_the_ide_writes_are_shaded() {
        use crate::panels::mcu_module::project_gen::gen_config;
        let mut seen = 0;
        for d in crate::panels::mcu_module::builtins::builtin_definitions() {
            for file in [
                ConfigFile::CargoToml,
                ConfigFile::CargoConfig,
                ConfigFile::MemoryX,
                ConfigFile::BuildRs,
                ConfigFile::GitIgnore,
            ] {
                let mut text = gen_config(file, &d.project, &d.toolchain);
                if text.is_empty() {
                    continue; // memory.x / build.rs on an ESP
                }
                // A line of the user's tail that names the marker.
                let begin = text.lines().next().unwrap_or_default().to_owned();
                text.push_str(&format!("{}\n", begin.replace(">>>", ">>> (mine)")));
                let spans = generated_lines(&text, Owner::Project(file));
                assert_eq!(spans.len(), 1, "{}:\n{text}", d.id);
                let lines: Vec<&str> = text.split('\n').collect();
                let (first, last) = spans[0];
                assert_eq!(first, 0, "{}:\n{text}", d.id);
                assert!(
                    lines[last].contains("<<< GENERATED END >>>"),
                    "{}:\n{text}",
                    d.id
                );
                assert!(last + 2 < lines.len(), "{}: the tail is the user's", d.id);
                seen += 1;
            }
        }
        assert!(seen > 0);
    }

    /// Every built-in chip's main.rs and `src/pins/` files, as the IDE writes
    /// them, with the flash store switched on where the chip has one: a
    /// config file a bare chip does not write.
    #[test]
    fn every_builtin_project_is_shaded_on_its_markers() {
        use crate::panels::mcu_module::flash_store::{FlashStoreConfig, platform_of};
        let mut configs = 0;
        for d in crate::panels::mcu_module::builtins::builtin_definitions() {
            let mut mcu = d.build_mcu();
            if let Ok(p) = platform_of(&mcu) {
                mcu.flash_store = Some(FlashStoreConfig::default_on(&p, &mcu.family));
            }
            let code = mcu.fresh_main_rs();
            let lines: Vec<&str> = code.split('\n').collect();
            let spans = generated_lines(&code, MAIN);
            assert!(!spans.is_empty(), "{}: no block", d.id);
            for (first, last) in spans {
                assert!(lines[first].contains(GEN_BEGIN), "{}:\n{code}", d.id);
                assert!(lines[last].contains(GEN_END), "{}:\n{code}", d.id);
            }
            for (name, body) in mcu.config_files() {
                let spans = generated_lines(&body, Owner::Pins);
                assert_eq!(spans.len(), 1, "{}/{name}:\n{body}", d.id);
                let lines: Vec<&str> = body.split('\n').collect();
                assert!(lines[spans[0].0].contains(PB), "{}/{name}:\n{body}", d.id);
                assert!(lines[spans[0].1].contains(PE), "{}/{name}:\n{body}", d.id);
                configs += 1;
            }
        }
        assert!(configs > 0, "no built-in chip wrote a config file");
    }

    /// "Write the clock by hand", as the IDE writes main.rs for STM32 families
    /// that offer it, on both runtimes: the clock region's lines are the
    /// user's, its markers and the rest of the block stay shaded. The bundled
    /// F103 has no such switch, so it stands in for the parts, the way
    /// `family::dual_core_tests` builds them.
    #[test]
    fn a_hand_written_clock_region_stays_clear() {
        use crate::panels::mcu_module::clock::model::ClockConfig;
        use crate::panels::mcu_module::codegen::rcc::supports_manual_clock;
        use crate::panels::mcu_module::mcu::Runtime;
        let line_of = |code: &str, byte: usize| code[..byte].matches('\n').count();
        for (id, family) in [
            ("stm32f411ceux", "stm32f4"),
            ("stm32g431cbux", "stm32g4"),
            ("stm32h743zitx", "stm32h7"),
            ("stm32wl55jcix", "stm32wl"),
        ] {
            assert!(supports_manual_clock(family), "{family}");
            for runtime in [Runtime::Blocking, Runtime::Async] {
                let mut def = crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6")
                    .expect("the bundled F103");
                def.id = id.into();
                def.family = family.into();
                let mut mcu = def.build_mcu();
                mcu.runtime = runtime;
                mcu.clock = ClockConfig::None;
                mcu.clock_manual = true;
                let code = mcu.fresh_main_rs();
                let cb = code.find(CLOCK_BEGIN).expect("CLOCK BEGIN");
                let ce = cb + code[cb..].find(CLOCK_END).expect("CLOCK END");
                let [(b, e)] = crate::app::generated_byte_ranges(&code)[..] else {
                    panic!("{id}: one block");
                };
                let (gb, ge) = (line_of(&code, b), line_of(&code, e));
                let (cb, ce) = (line_of(&code, cb), line_of(&code, ce));
                assert!(gb < cb && cb + 1 < ce && ce < ge, "{id}:\n{code}");
                assert_eq!(
                    generated_lines(&code, MAIN),
                    [(gb, cb), (ce, ge)],
                    "{id} {runtime:?}:\n{code}"
                );
            }
        }
        // Without its END the region is not kept, so it is shaded.
        let broken = format!("{GEN_BEGIN}\n{CLOCK_BEGIN}\n    let c = 1;\n{GEN_END}\nloop {{}}\n");
        assert_eq!(generated_lines(&broken, MAIN), [(0, 3)]);
    }

    /// The header is shaded exactly where an update loses a line typed into
    /// it: every built-in chip and a few renamed STM32 families (the embassy
    /// backends), on every runtime. A line above the block goes on those, the
    /// rest keep it.
    #[test]
    fn the_header_is_shaded_where_an_update_rewrites_it() {
        use crate::panels::mcu_module::mcu::Runtime;
        let mut defs = crate::panels::mcu_module::builtins::builtin_definitions();
        for (id, family) in [("stm32g431cbux", "stm32g4"), ("stm32wba52cgux", "stm32wba")] {
            let mut def = crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6")
                .expect("the bundled F103");
            def.id = id.into();
            def.family = family.into();
            defs.push(def);
        }
        let (mut rebuilt, mut kept) = (0, 0);
        for d in defs {
            for runtime in [
                Runtime::Blocking,
                Runtime::Native,
                Runtime::Async,
                Runtime::Rtic,
            ] {
                let mut mcu = d.build_mcu();
                mcu.runtime = runtime;
                let fresh = mcu.fresh_main_rs();
                let Some(at) = fresh.find(GEN_BEGIN) else {
                    continue; // a runtime this family has no backend for
                };
                let mine = "// mine, above the block\n";
                let typed = format!("{}{mine}{}", &fresh[..at], &fresh[at..]);
                let lost = !mcu.update_main_rs(&typed).contains(mine);
                let header = mcu.rebuilds_main_header();
                assert_eq!(lost, header, "{} {runtime:?}", d.id);
                let spans = generated_lines(&typed, Owner::MainRs { header });
                let mine_line = typed[..typed.find(mine).unwrap()].matches('\n').count();
                assert_eq!(
                    spans[0].0 <= mine_line,
                    header,
                    "{} {runtime:?}: {spans:?}",
                    d.id
                );
                if header {
                    assert_eq!(spans[0].0, 0);
                    rebuilt += 1;
                } else {
                    kept += 1;
                }
            }
        }
        assert!(rebuilt > 0 && kept > 0, "{rebuilt} rebuilt, {kept} kept");
    }

    #[test]
    fn the_partition_table_is_shaded_where_its_splice_finds_the_block() {
        use crate::panels::mcu_module::project_gen::splice_partitions_csv;
        let rows = "nvs, data, nvs, 0x9000, 0x6000,\n";
        let mut text = splice_partitions_csv("", Some(rows));
        text.push_str("mine, data, undefined, 0x300000, 0x1000,\n");
        let spans = generated_lines(&text, Owner::Partitions);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(spans.len(), 1, "{text}");
        assert_eq!(spans[0].0, 0, "{text}");
        assert!(
            lines[spans[0].1].contains("<<< GENERATED END >>>"),
            "{text}"
        );
        assert!(!lines[..=spans[0].1].iter().any(|l| l.starts_with("mine")));
        // A hand-written table: nothing of the IDE's.
        assert!(generated_lines("nvs, data, nvs, 0x9000, 0x6000,\n", Owner::Partitions).is_empty());
    }

    /// A block inside `fn main`, whose header is generated and whose closing
    /// brace is the user's - the shape the ESP / RP / nRF main.rs has.
    fn main_with_block() -> String {
        format!(
            "use x;\n{GEN_BEGIN}\nfn main() {{\n    let a = 1;\n    let b = 2;\n{GEN_END}\n    \
             loop {{\n        a;\n    }}\n}}\n"
        )
    }

    #[test]
    fn unfolded_the_runs_are_the_blocks() {
        let main = main_with_block();
        let spans = generated_lines(&main, MAIN);
        assert_eq!(spans, [(1, 5)]);
        assert_eq!(display_runs(&spans, &FoldMap::identity(&main)), spans);
    }

    #[test]
    fn a_folded_main_keeps_its_generated_header_shaded() {
        let main = main_with_block();
        let map = FoldMap::new(&main, &BTreeSet::from([2]));
        assert!(!map.is_identity());
        // Shown: `use x;`, the BEGIN marker, `fn main() {`, the user's `}`.
        assert_eq!(display_runs(&generated_lines(&main, MAIN), &map), [(1, 2)]);
    }

    #[test]
    fn a_block_a_fold_hides_whole_has_no_run() {
        let text =
            format!("fn main() {{\n{GEN_BEGIN}\n    let a = 1;\n{GEN_END}\n    loop {{}}\n}}\n");
        let map = FoldMap::new(&text, &BTreeSet::from([0]));
        assert!(display_runs(&generated_lines(&text, MAIN), &map).is_empty());
    }
}
